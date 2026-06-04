//! End-to-end autoscaler integration test.
//!
//! Drives a real [`Controller`] with REAL collaborators — the loopback bridge
//! supervisor, the resource ledger, the replica registry, and an in-memory
//! SQLite store — and fakes only the system boundaries (runtime, health,
//! metrics/usage, catalog). Because this is a separate test crate it can only
//! touch `pub` items, so the usage source and catalog fakes are defined here
//! against the public traits rather than reusing the `#[cfg(test)]` fakes that
//! live inside `controller.rs`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use denia::access_log::AccessLogStore;
use denia::artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource};
use denia::autoscale::controller::{
    AutoscaleEvent, Controller, ManagedService, ServiceCatalog, UsageSource,
};
use denia::autoscale::ledger::{Headroom, HostCapacity, ResourceLedger};
use denia::autoscale::registry::{Replica, ReplicaRegistry};
use denia::autoscale::usage::ServiceUsage;
use denia::domain::{AutoscalePolicy, HealthCheck, ResourceLimits};
use denia::health::FakeHealthChecker;
use denia::ingress::pingora::IngressState;
use denia::logs::LogStore;
use denia::runtime::FakeRuntime;
use denia::state::SqliteStore;
use uuid::Uuid;

/// Scripted [`UsageSource`]: returns a fixed per-replica `(cpu%, mem%)` for the
/// whole service, regardless of how many replicas exist. `usage` takes
/// `&mut self`, so a plain field is enough — no interior mutability needed.
struct ScriptedUsage {
    cpu_pct: u32,
    mem_pct: u32,
}

impl UsageSource for ScriptedUsage {
    fn usage(
        &mut self,
        _service_name: &str,
        replicas: &[Replica],
        _limits: &ResourceLimits,
    ) -> ServiceUsage {
        ServiceUsage {
            avg_cpu_pct: self.cpu_pct,
            avg_mem_pct: self.mem_pct,
            max_mem_pct: self.mem_pct,
            replica_count: replicas.len() as u32,
            live_readings: replicas.len() as u32,
        }
    }
}

/// In-memory [`ServiceCatalog`] over a shared, mutable list of managed services.
/// The shared `Arc<Mutex<Vec<..>>>` lets a test swap a service's active
/// `deployment_id` mid-run (the controller holds its own `Arc` clone, so the
/// mutation is observed on the next tick) to exercise rolling replace.
#[derive(Clone)]
struct StaticCatalog {
    services: Arc<Mutex<Vec<ManagedService>>>,
}

impl StaticCatalog {
    fn new(services: Vec<ManagedService>) -> Self {
        Self {
            services: Arc::new(Mutex::new(services)),
        }
    }

    /// Replace the active deployment for a service by name (rolling replace).
    fn set_deployment(&self, service_name: &str, deployment_id: Uuid) {
        let mut guard = self.services.lock().expect("catalog lock");
        if let Some(ms) = guard.iter_mut().find(|m| m.service_name == service_name) {
            ms.deployment_id = deployment_id;
        }
    }
}

impl ServiceCatalog for StaticCatalog {
    fn resolve(&self, service_key: &str) -> Option<ManagedService> {
        // Bridge/activation identity is the service_id string (F-3).
        let service_id = Uuid::parse_str(service_key).ok()?;
        self.services
            .lock()
            .expect("catalog lock")
            .iter()
            .find(|m| m.service_id == service_id)
            .cloned()
    }

    fn all(&self) -> Vec<ManagedService> {
        self.services.lock().expect("catalog lock").clone()
    }
}

fn artifact() -> ArtifactRecord {
    ArtifactRecord::new(
        "sha256:deadbeef",
        ArtifactKind::OciImage,
        ArtifactSource::ExternalRegistry {
            image: "ghcr.io/acme/web:1".to_string(),
        },
    )
    .expect("artifact")
}

/// scale-from-zero policy: min=0, max=3, target cpu 80%, cooldown 300s, idle 600s.
fn policy() -> AutoscalePolicy {
    AutoscalePolicy {
        min_replicas: 0,
        max_replicas: 3,
        target_cpu_pct: 80,
        target_mem_pct: None,
        scale_down_cooldown_s: 300,
        idle_timeout_s: 600,
    }
}

fn managed(service_id: Uuid, deployment_id: Uuid) -> ManagedService {
    ManagedService {
        service_name: "web".to_string(),
        service_id,
        deployment_id,
        policy: policy(),
        artifact: artifact(),
        internal_port: 8080,
        limits: ResourceLimits::default(),
        env: Vec::new(),
        health_check: HealthCheck::new("/healthz", 5),
    }
}

/// Generous ledger: room for 3 replicas of the default limits and then some.
fn roomy_ledger() -> ResourceLedger {
    ResourceLedger::new(
        HostCapacity {
            cpu_millis: 4000,
            mem_bytes: 4 << 30,
        },
        Headroom {
            cpu_millis: 0,
            mem_bytes: 0,
        },
    )
}

fn build_controller(
    ledger: ResourceLedger,
    usage: ScriptedUsage,
    catalog: StaticCatalog,
) -> Controller {
    let store = SqliteStore::open_in_memory().expect("open in-memory store");
    store.migrate().expect("migrate");
    Controller::new(
        ReplicaRegistry::default(),
        ledger,
        Arc::new(FakeRuntime::default()),
        Arc::new(IngressState::with_access_log(AccessLogStore::new())),
        Arc::new(FakeHealthChecker::healthy()),
        store,
        Box::new(usage),
        Arc::new(catalog),
        Duration::ZERO,
        LogStore::new(std::env::temp_dir()),
    )
}

fn deployment_ids(ctrl: &Controller, svc: Uuid) -> Vec<Uuid> {
    ctrl.registry
        .replicas(svc)
        .iter()
        .map(|r| r.deployment_id)
        .collect()
}

/// Cold start → scale up on CPU → scale down after cooldown → idle to zero.
#[tokio::test]
async fn full_autoscale_lifecycle() {
    let svc = Uuid::now_v7();
    let dep = Uuid::now_v7();
    let ms = managed(svc, dep);
    let catalog = StaticCatalog::new(vec![ms.clone()]);
    // Start idle/low CPU; raised per-step via the controller's usage handle.
    let mut ctrl = build_controller(
        roomy_ledger(),
        ScriptedUsage {
            cpu_pct: 0,
            mem_pct: 0,
        },
        catalog,
    );

    // 1. Cold start via the activator: zero replicas → exactly one healthy.
    assert_eq!(ctrl.replica_count(svc), 0);
    ctrl.activate_one(&svc.to_string())
        .await
        .expect("activation ok");
    assert_eq!(ctrl.replica_count(svc), 1);
    assert_eq!(ctrl.healthy_replicas(svc), 1);
    assert_eq!(ctrl.ingress.healthy_count(&svc.to_string()).await, 1);

    // 2. Scale up on high CPU: 100% over an 80% target → ceil(1*100/80)=2.
    set_usage(&mut ctrl, 100, 0);
    let up = ctrl.tick_all(0).await;
    assert_eq!(ctrl.replica_count(svc), 2);
    assert!(
        up.contains(&AutoscaleEvent::ScaledUp {
            service: "web".to_string(),
            from: 1,
            to: 2,
        }),
        "expected ScaledUp 1->2, got {up:?}"
    );

    // A second high-CPU tick keeps growing toward, and caps at, max_replicas=3.
    let up2 = ctrl.tick_all(1).await;
    assert_eq!(ctrl.replica_count(svc), 3);
    assert!(
        up2.contains(&AutoscaleEvent::ScaledUp {
            service: "web".to_string(),
            from: 2,
            to: 3,
        }),
        "expected ScaledUp 2->3, got {up2:?}"
    );
    // Already at max: a further high-CPU tick is a no-op (clamped at 3).
    let capped = ctrl.tick_all(2).await;
    assert_eq!(ctrl.replica_count(svc), 3);
    assert!(
        capped.is_empty(),
        "expected no scaling at max, got {capped:?}"
    );

    // 4. Scale down only after the cooldown window elapses. At 5% CPU the
    // CPU-derived desired is ceil(3*5/80)=1 (clamped to the loop floor of 1),
    // so once the cooldown clears the fleet drops straight from 3 to 1.
    set_usage(&mut ctrl, 5, 0);
    // First low tick at now=100 arms the cooldown; nothing shrinks.
    let hold = ctrl.tick_all(100).await;
    assert_eq!(ctrl.replica_count(svc), 3);
    assert!(
        hold.is_empty(),
        "no scale-down before cooldown, got {hold:?}"
    );
    // After cooldown (now=100+300), the fleet drains to the floor of 1.
    let down = ctrl.tick_all(400).await;
    assert_eq!(ctrl.replica_count(svc), 1);
    assert!(
        down.contains(&AutoscaleEvent::ScaledDown {
            service: "web".to_string(),
            from: 3,
            to: 1,
        }),
        "expected ScaledDown 3->1, got {down:?}"
    );

    // 5. Idle to zero: backdate ingress activity past idle_timeout_s (600s) with
    // low CPU → all replicas drain and the service reports ScaledToZero.
    ctrl.ingress
        .set_last_activity(&svc.to_string(), Instant::now() - Duration::from_secs(700))
        .await;
    let zero = ctrl.tick_all(1000).await;
    assert_eq!(ctrl.replica_count(svc), 0);
    assert!(
        zero.contains(&AutoscaleEvent::ScaledToZero {
            service: "web".to_string(),
        }),
        "expected ScaledToZero, got {zero:?}"
    );
    assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(0));
    assert_eq!(ctrl.ingress.healthy_count(&svc.to_string()).await, 0);
}

/// Swap the active deployment mid-run and confirm the live replica rolls onto
/// the new deployment via a `RolloutStep` (single-replica launch-then-drain).
#[tokio::test]
async fn rolling_replace_swaps_deployment() {
    let svc = Uuid::now_v7();
    let d1 = Uuid::now_v7();
    let d2 = Uuid::now_v7();
    let ms = managed(svc, d1);
    let catalog = StaticCatalog::new(vec![ms.clone()]);
    let catalog_handle = catalog.clone();
    let mut ctrl = build_controller(
        roomy_ledger(),
        ScriptedUsage {
            cpu_pct: 0,
            mem_pct: 0,
        },
        catalog,
    );

    // Seed one replica on deployment d1 via the activator.
    ctrl.activate_one(&svc.to_string())
        .await
        .expect("activation ok");
    assert_eq!(ctrl.replica_count(svc), 1);
    assert_eq!(deployment_ids(&ctrl, svc), vec![d1]);

    // Swap the catalog's active deployment to d2; the controller holds the same
    // Arc, so the next tick sees the new target.
    catalog_handle.set_deployment("web", d2);

    let events = ctrl.tick_all(0).await;
    assert_eq!(ctrl.replica_count(svc), 1);
    assert_eq!(
        deployment_ids(&ctrl, svc),
        vec![d2],
        "the live replica must end on d2"
    );
    assert!(
        events.contains(&AutoscaleEvent::RolloutStep {
            service: "web".to_string(),
            to_deployment: d2,
        }),
        "expected RolloutStep to d2, got {events:?}"
    );
}

/// Under a ledger that fits exactly one replica, a high-CPU scale-up is denied
/// and the fleet stays at one replica.
#[tokio::test]
async fn capacity_denied_under_pressure() {
    let svc = Uuid::now_v7();
    let dep = Uuid::now_v7();
    let ms = managed(svc, dep);
    let catalog = StaticCatalog::new(vec![ms.clone()]);
    let lim = ResourceLimits::default();
    // Capacity for exactly one replica's worth.
    let tight = ResourceLedger::new(
        HostCapacity {
            cpu_millis: lim.cpu_millis,
            mem_bytes: lim.memory_bytes,
        },
        Headroom {
            cpu_millis: 0,
            mem_bytes: 0,
        },
    );
    let mut ctrl = build_controller(
        tight,
        ScriptedUsage {
            cpu_pct: 100,
            mem_pct: 0,
        },
        catalog,
    );

    // Cold start fits exactly one replica.
    ctrl.activate_one(&svc.to_string())
        .await
        .expect("activation ok");
    assert_eq!(ctrl.replica_count(svc), 1);

    // High CPU wants a second replica, but the ledger is full → denied, stays 1.
    let events = ctrl.tick_all(0).await;
    assert_eq!(ctrl.replica_count(svc), 1);
    assert!(
        events.contains(&AutoscaleEvent::ScaleUpDenied {
            service: "web".to_string(),
            reason: "insufficient_capacity".to_string(),
        }),
        "expected ScaleUpDenied(insufficient_capacity), got {events:?}"
    );
    assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(1));
}

/// Mutate the controller's scripted usage source between ticks. The controller
/// owns the `Box<dyn UsageSource>`, so we downcast through a fresh box: simplest
/// is to keep a typed handle. Here we just replace the box.
fn set_usage(ctrl: &mut Controller, cpu_pct: u32, mem_pct: u32) {
    ctrl.usage = Box::new(ScriptedUsage { cpu_pct, mem_pct });
}

/// ADR-028 deploy hand-off, min==0: a runtime deploy of an autoscaled service
/// hands ownership to the controller via `reconcile_service`. With min==0 it
/// launches nothing (the activator owns the 0->1 wake), so `/v1/workloads`
/// reports 0 — but unlike the pre-fix deploy path there is NO shadow workload.
/// The first request cold-starts to 1, and idle drains back to 0 (the cycle the
/// user expected but never saw because the deploy path shadowed the autoscaler).
#[tokio::test]
async fn deploy_handoff_min_zero_wakes_then_scales_to_zero() {
    let svc = Uuid::now_v7();
    let dep = Uuid::now_v7();
    let ms = managed(svc, dep); // policy() is min=0, max=3
    let catalog = StaticCatalog::new(vec![ms.clone()]);
    let mut ctrl = build_controller(
        roomy_ledger(),
        ScriptedUsage {
            cpu_pct: 0,
            mem_pct: 0,
        },
        catalog,
    );

    // Deploy hand-off: min==0 launches nothing.
    let handoff = ctrl.reconcile_service(svc).await;
    assert!(
        handoff.is_empty(),
        "min==0 deploy launches nothing, got {handoff:?}"
    );
    assert_eq!(ctrl.replica_count(svc), 0);
    assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(0));

    // First request cold-starts to one healthy replica (now tracked → workloads=1).
    ctrl.activate_one(&svc.to_string()).await.expect("wake ok");
    assert_eq!(ctrl.replica_count(svc), 1);
    assert_eq!(ctrl.healthy_replicas(svc), 1);

    // Idle past idle_timeout_s with low CPU → scale to zero.
    ctrl.ingress
        .set_last_activity(&svc.to_string(), Instant::now() - Duration::from_secs(700))
        .await;
    let zero = ctrl.tick_all(1000).await;
    assert_eq!(ctrl.replica_count(svc), 0);
    assert!(
        zero.contains(&AutoscaleEvent::ScaledToZero {
            service: "web".to_string(),
        }),
        "expected ScaledToZero, got {zero:?}"
    );
}

/// ADR-028 deploy hand-off, min>=1: `reconcile_service` launches `min` replicas
/// and the controller tracks them, so `/v1/workloads` reports the real count.
/// (The bug: a deploy-launched workload was never in the registry, so workloads
/// reported 0 while the service was actually serving traffic.)
#[tokio::test]
async fn deploy_handoff_min_one_is_tracked() {
    let svc = Uuid::now_v7();
    let dep = Uuid::now_v7();
    let mut ms = managed(svc, dep);
    ms.policy.min_replicas = 1; // min=1, max=3
    let catalog = StaticCatalog::new(vec![ms.clone()]);
    let mut ctrl = build_controller(
        roomy_ledger(),
        ScriptedUsage {
            cpu_pct: 0,
            mem_pct: 0,
        },
        catalog,
    );

    let events = ctrl.reconcile_service(svc).await;

    assert_eq!(ctrl.replica_count(svc), 1);
    assert_eq!(ctrl.healthy_replicas(svc), 1);
    assert_eq!(ctrl.ingress.healthy_count(&svc.to_string()).await, 1);
    assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(1));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AutoscaleEvent::ScaledUp { from: 0, to: 1, .. })),
        "expected ScaledUp 0->1, got {events:?}"
    );
}

//! Autoscaler control loop.
//!
//! The controller drives each managed service toward its metric-derived desired
//! replica count once per tick. It owns the runtime authority for replicas (the
//! [`ReplicaRegistry`]) and reconciles by launching/draining replicas via the
//! lifecycle primitives. The persisted `desired_replicas` is set to the actual
//! count achieved this tick, not the target, so a capacity-denied partial
//! scale-up reports the real fleet size.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::artifacts::ArtifactRecord;
use crate::autoscale::ledger::ResourceLedger;
use crate::autoscale::lifecycle::{LaunchSpec, LifecycleError, drain_replica, launch_replica};
use crate::autoscale::registry::{Replica, ReplicaRegistry};
use crate::autoscale::scaler::{CooldownState, clamp_loop, desired_down, desired_up};
use crate::autoscale::usage::{ServiceUsage, UsageSampler};
use crate::domain::{AutoscalePolicy, HealthCheck, ResourceLimits, RuntimeInstanceId};
use crate::health::HealthChecker;
use crate::ingress::bridge::LoopbackBridgeSupervisor;
use crate::observability::metrics::CgroupMetricsReader;
use crate::runtime::Runtime;
use crate::state::SqliteStore;

/// Events emitted by a controller tick (and, for `ScaledToZero`, a later idle task).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoscaleEvent {
    ScaledUp { service: String, from: u32, to: u32 },
    ScaledDown { service: String, from: u32, to: u32 },
    ScaleUpDenied { service: String, reason: String },
    ScaledToZero { service: String },
}

/// Per-tick launch context for one autoscaled service, assembled by the caller
/// (wiring) from repos.
#[derive(Clone)]
pub struct ManagedService {
    pub service_name: String,
    pub service_id: Uuid,
    pub deployment_id: Uuid,
    pub policy: AutoscalePolicy,
    pub artifact: ArtifactRecord,
    pub internal_port: u16,
    pub limits: ResourceLimits,
    pub env: Vec<(String, String)>,
    pub health_check: HealthCheck,
}

/// Abstracts metric sampling so the controller is testable without real cgroups.
pub trait UsageSource: Send {
    fn usage(
        &mut self,
        service_name: &str,
        replicas: &[Replica],
        limits: &ResourceLimits,
    ) -> ServiceUsage;
}

/// Real [`UsageSource`] backed by [`UsageSampler`] over a [`CgroupMetricsReader`].
pub struct CgroupUsageSource {
    pub sampler: UsageSampler,
    pub reader: CgroupMetricsReader,
}

impl CgroupUsageSource {
    pub fn new(reader: CgroupMetricsReader) -> Self {
        Self {
            sampler: UsageSampler::default(),
            reader,
        }
    }
}

impl UsageSource for CgroupUsageSource {
    fn usage(
        &mut self,
        service_name: &str,
        replicas: &[Replica],
        limits: &ResourceLimits,
    ) -> ServiceUsage {
        self.sampler
            .sample(service_name, replicas, &self.reader, limits)
    }
}

pub struct Controller {
    pub registry: ReplicaRegistry,
    pub ledger: ResourceLedger,
    pub runtime: Arc<dyn Runtime>,
    pub bridge: Arc<LoopbackBridgeSupervisor>,
    pub health: Arc<dyn HealthChecker>,
    pub store: SqliteStore,
    pub usage: Box<dyn UsageSource>,
    pub cooldowns: HashMap<Uuid, CooldownState>,
    pub drain_grace: Duration,
}

impl Controller {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        registry: ReplicaRegistry,
        ledger: ResourceLedger,
        runtime: Arc<dyn Runtime>,
        bridge: Arc<LoopbackBridgeSupervisor>,
        health: Arc<dyn HealthChecker>,
        store: SqliteStore,
        usage: Box<dyn UsageSource>,
        drain_grace: Duration,
    ) -> Self {
        Self {
            registry,
            ledger,
            runtime,
            bridge,
            health,
            store,
            usage,
            cooldowns: HashMap::new(),
            drain_grace,
        }
    }

    /// Drive each service one step toward its desired replica count.
    pub async fn tick(&mut self, services: &[ManagedService], now_s: u64) -> Vec<AutoscaleEvent> {
        use std::time::Instant;
        let mut events = Vec::new();

        'service: for ms in services {
            let start = self.registry.replica_count(ms.service_id) as u32;

            // 1/2. Determine the desired replica count.
            let desired = if start == 0 {
                if ms.policy.min_replicas >= 1 {
                    // Cold start / floor enforcement to min.
                    ms.policy.min_replicas
                } else {
                    // min==0: the 0->1 transition is owned by the activator.
                    // Nothing to sample at zero; persist 0 and move on.
                    self.store.set_desired_replicas(ms.service_id, 0).ok();
                    continue;
                }
            } else {
                let u = self.usage.usage(
                    &ms.service_name,
                    self.registry.replicas(ms.service_id),
                    &ms.limits,
                );

                // Idle scale-to-zero: a `min_replicas==0` service whose bridge
                // has been idle past `idle_timeout_s` AND whose CPU is below
                // target drains ALL replicas. This is distinct from the metric
                // cooldown scale-down: it keys off bridge activity, and the
                // activator wakes it on the next request. Memory is scale-up
                // only, so it must not block scale-to-zero.
                if ms.policy.min_replicas == 0 {
                    let idle_secs = match self.bridge.last_activity(&ms.service_name).await {
                        Some(t) => Instant::now().saturating_duration_since(t).as_secs(),
                        None => u64::MAX,
                    };
                    let metrics_low = u.avg_cpu_pct < ms.policy.target_cpu_pct as u32;
                    if idle_secs > ms.policy.idle_timeout_s as u64 && metrics_low {
                        let replicas: Vec<(Uuid, u32)> = self
                            .registry
                            .replicas(ms.service_id)
                            .iter()
                            .map(|r| (r.id, r.index))
                            .collect();
                        for (replica_id, index) in replicas {
                            let instance = RuntimeInstanceId {
                                service_name: ms.service_name.clone(),
                                replica_index: index,
                            };
                            let _ = drain_replica(
                                &ms.service_name,
                                replica_id,
                                &instance,
                                &ms.limits,
                                self.drain_grace,
                                &mut self.registry,
                                &mut self.ledger,
                                self.runtime.as_ref(),
                                self.bridge.as_ref(),
                            )
                            .await;
                        }
                        self.store.set_desired_replicas(ms.service_id, 0).ok();
                        events.push(AutoscaleEvent::ScaledToZero {
                            service: ms.service_name.clone(),
                        });
                        continue 'service;
                    }
                }

                let up = desired_up(
                    start,
                    u.avg_cpu_pct,
                    ms.policy.target_cpu_pct,
                    Some(u.avg_mem_pct),
                    ms.policy.target_mem_pct,
                );
                let cd = self.cooldowns.entry(ms.service_id).or_default();
                if up > start {
                    cd.note_above_target(now_s);
                    clamp_loop(up, ms.policy.min_replicas, ms.policy.max_replicas)
                } else {
                    let down = desired_down(start, u.avg_cpu_pct, ms.policy.target_cpu_pct);
                    if down < start
                        && cd.scale_down_allowed(now_s, ms.policy.scale_down_cooldown_s as u64)
                    {
                        clamp_loop(down, ms.policy.min_replicas, ms.policy.max_replicas)
                    } else {
                        start
                    }
                }
            };

            // 3. Reconcile toward `desired`. `current` tracks the actual count.
            let mut current = start;

            while current < desired {
                let replica_index = self.next_replica_index(ms.service_id);
                let spec = LaunchSpec {
                    service_name: ms.service_name.clone(),
                    service_id: ms.service_id,
                    deployment_id: ms.deployment_id,
                    replica_index,
                    artifact: ms.artifact.clone(),
                    internal_port: ms.internal_port,
                    limits: ms.limits.clone(),
                    env: ms.env.clone(),
                    health_check: ms.health_check.clone(),
                };
                match launch_replica(
                    &spec,
                    &mut self.registry,
                    &mut self.ledger,
                    self.runtime.as_ref(),
                    self.bridge.as_ref(),
                    self.health.as_ref(),
                )
                .await
                {
                    Ok(_) => current += 1,
                    Err(LifecycleError::Capacity) => {
                        events.push(AutoscaleEvent::ScaleUpDenied {
                            service: ms.service_name.clone(),
                            reason: "insufficient_capacity".to_string(),
                        });
                        break;
                    }
                    Err(e) => {
                        events.push(AutoscaleEvent::ScaleUpDenied {
                            service: ms.service_name.clone(),
                            reason: e.to_string(),
                        });
                        break;
                    }
                }
            }

            while current > desired {
                // Remove the highest-index replica.
                let victim = self
                    .registry
                    .replicas(ms.service_id)
                    .iter()
                    .max_by_key(|r| r.index)
                    .cloned();
                let Some(replica) = victim else {
                    break;
                };
                let instance = RuntimeInstanceId {
                    service_name: ms.service_name.clone(),
                    replica_index: replica.index,
                };
                match drain_replica(
                    &ms.service_name,
                    replica.id,
                    &instance,
                    &ms.limits,
                    self.drain_grace,
                    &mut self.registry,
                    &mut self.ledger,
                    self.runtime.as_ref(),
                    self.bridge.as_ref(),
                )
                .await
                {
                    Ok(()) => current -= 1,
                    Err(e) => {
                        events.push(AutoscaleEvent::ScaleUpDenied {
                            service: ms.service_name.clone(),
                            reason: e.to_string(),
                        });
                        break;
                    }
                }
            }

            // Emit a scale event reflecting the ACTUAL resulting count.
            if current > start {
                events.push(AutoscaleEvent::ScaledUp {
                    service: ms.service_name.clone(),
                    from: start,
                    to: current,
                });
            } else if current < start {
                events.push(AutoscaleEvent::ScaledDown {
                    service: ms.service_name.clone(),
                    from: start,
                    to: current,
                });
            }

            // 4. Persist the actual achieved count.
            self.store.set_desired_replicas(ms.service_id, current).ok();
        }

        events
    }

    /// Next replica index: max existing index + 1, or 0 if the service has none.
    fn next_replica_index(&self, service_id: Uuid) -> u32 {
        self.registry
            .replicas(service_id)
            .iter()
            .map(|r| r.index)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{ArtifactKind, ArtifactSource};
    use crate::autoscale::ledger::{Headroom, HostCapacity};
    use crate::health::FakeHealthChecker;
    use crate::runtime::FakeRuntime;

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

    fn policy() -> AutoscalePolicy {
        AutoscalePolicy {
            min_replicas: 1,
            max_replicas: 3,
            target_cpu_pct: 80,
            target_mem_pct: None,
            scale_down_cooldown_s: 300,
            idle_timeout_s: 600,
        }
    }

    fn managed(service_id: Uuid) -> ManagedService {
        ManagedService {
            service_name: "web".to_string(),
            service_id,
            deployment_id: Uuid::now_v7(),
            policy: policy(),
            artifact: artifact(),
            internal_port: 8080,
            limits: ResourceLimits::default(),
            env: Vec::new(),
            health_check: HealthCheck::new("/healthz", 5),
        }
    }

    /// Fake UsageSource that returns a scripted (cpu%, mem%) regardless of input,
    /// scaling avg_cpu by the live replica count so desired_up/down see the
    /// per-replica average the controller expects.
    struct FakeUsage {
        cpu_pct: u32,
        mem_pct: u32,
    }

    impl UsageSource for FakeUsage {
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
            }
        }
    }

    fn ledger(cpu_millis: u32, mem_bytes: u64) -> ResourceLedger {
        ResourceLedger::new(
            HostCapacity {
                cpu_millis,
                mem_bytes,
            },
            Headroom {
                cpu_millis: 0,
                mem_bytes: 0,
            },
        )
    }

    fn controller(ledger: ResourceLedger, usage: Box<dyn UsageSource>) -> Controller {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        Controller::new(
            ReplicaRegistry::default(),
            ledger,
            Arc::new(FakeRuntime::default()),
            Arc::new(LoopbackBridgeSupervisor::default()),
            Arc::new(FakeHealthChecker::healthy()),
            store,
            usage,
            Duration::ZERO,
        )
    }

    #[tokio::test]
    async fn tick_scales_up_on_high_cpu() {
        let svc = Uuid::now_v7();
        let ms = managed(svc);
        // Plenty of capacity for 3 replicas.
        let mut ctrl = controller(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 100,
                mem_pct: 0,
            }),
        );

        // First tick brings the service to min=1 from zero.
        let e0 = ctrl.tick(std::slice::from_ref(&ms), 0).await;
        assert_eq!(ctrl.registry.replica_count(svc), 1);
        assert_eq!(
            e0,
            vec![AutoscaleEvent::ScaledUp {
                service: "web".to_string(),
                from: 0,
                to: 1
            }]
        );
        assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(1));

        // Next tick: cpu 100% over target 80% => ceil(1*100/80)=2.
        let e1 = ctrl.tick(std::slice::from_ref(&ms), 1).await;
        assert_eq!(ctrl.registry.replica_count(svc), 2);
        assert_eq!(
            e1,
            vec![AutoscaleEvent::ScaledUp {
                service: "web".to_string(),
                from: 1,
                to: 2
            }]
        );
        assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(2));
    }

    #[tokio::test]
    async fn tick_scales_down_after_cooldown() {
        let svc = Uuid::now_v7();
        let ms = managed(svc);
        let mut ctrl = controller(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 100,
                mem_pct: 0,
            }),
        );

        // Drive up to 2 replicas: tick0 -> 1 (min), tick1 -> 2 (high cpu).
        ctrl.tick(std::slice::from_ref(&ms), 0).await;
        ctrl.tick(std::slice::from_ref(&ms), 0).await;
        assert_eq!(ctrl.registry.replica_count(svc), 2);

        // Now switch to low cpu so down = ceil(2*10/80)=1 < 2.
        ctrl.usage = Box::new(FakeUsage {
            cpu_pct: 10,
            mem_pct: 0,
        });

        // First low tick at now_s=0 starts the cooldown window, no scale down.
        let e_hold = ctrl.tick(std::slice::from_ref(&ms), 0).await;
        assert_eq!(ctrl.registry.replica_count(svc), 2);
        assert!(e_hold.is_empty());
        assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(2));

        // After the cooldown elapses (now_s=300), scale down by one.
        let e_down = ctrl.tick(std::slice::from_ref(&ms), 300).await;
        assert_eq!(ctrl.registry.replica_count(svc), 1);
        assert_eq!(
            e_down,
            vec![AutoscaleEvent::ScaledDown {
                service: "web".to_string(),
                from: 2,
                to: 1
            }]
        );
        assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(1));
    }

    fn scale_to_zero_policy() -> AutoscalePolicy {
        AutoscalePolicy {
            min_replicas: 0,
            max_replicas: 3,
            target_cpu_pct: 80,
            target_mem_pct: None,
            scale_down_cooldown_s: 300,
            idle_timeout_s: 600,
        }
    }

    /// Launch one healthy replica directly against the controller's fields so an
    /// idle test has something to scale to zero (a `min_replicas==0` service is
    /// never auto-launched by `tick`).
    async fn seed_replica(ctrl: &mut Controller, ms: &ManagedService) -> Uuid {
        let spec = LaunchSpec {
            service_name: ms.service_name.clone(),
            service_id: ms.service_id,
            deployment_id: ms.deployment_id,
            replica_index: 0,
            artifact: ms.artifact.clone(),
            internal_port: ms.internal_port,
            limits: ms.limits.clone(),
            env: ms.env.clone(),
            health_check: ms.health_check.clone(),
        };
        launch_replica(
            &spec,
            &mut ctrl.registry,
            &mut ctrl.ledger,
            ctrl.runtime.as_ref(),
            ctrl.bridge.as_ref(),
            ctrl.health.as_ref(),
        )
        .await
        .expect("seed launch ok")
    }

    #[tokio::test]
    async fn idle_scales_to_zero() {
        let svc = Uuid::now_v7();
        let mut ms = managed(svc);
        ms.policy = scale_to_zero_policy();
        let mut ctrl = controller(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 5,
                mem_pct: 0,
            }),
        );

        seed_replica(&mut ctrl, &ms).await;
        assert_eq!(ctrl.registry.replica_count(svc), 1);

        // Backdate activity past idle_timeout_s (600s).
        let idle = std::time::Instant::now() - Duration::from_secs(700);
        ctrl.bridge.set_last_activity(&ms.service_name, idle).await;

        let events = ctrl.tick(std::slice::from_ref(&ms), 1000).await;

        assert_eq!(ctrl.registry.replica_count(svc), 0);
        assert!(events.contains(&AutoscaleEvent::ScaledToZero {
            service: "web".to_string()
        }));
        assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(0));
    }

    #[tokio::test]
    async fn not_idle_does_not_zero() {
        let svc = Uuid::now_v7();
        let mut ms = managed(svc);
        ms.policy = scale_to_zero_policy();
        let mut ctrl = controller(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 5,
                mem_pct: 0,
            }),
        );

        seed_replica(&mut ctrl, &ms).await;
        assert_eq!(ctrl.registry.replica_count(svc), 1);

        // Recent activity: not idle.
        ctrl.bridge
            .set_last_activity(&ms.service_name, std::time::Instant::now())
            .await;

        let events = ctrl.tick(std::slice::from_ref(&ms), 1000).await;

        assert!(ctrl.registry.replica_count(svc) >= 1);
        assert!(!events.contains(&AutoscaleEvent::ScaledToZero {
            service: "web".to_string()
        }));
    }

    #[tokio::test]
    async fn idle_but_busy_does_not_zero() {
        let svc = Uuid::now_v7();
        let mut ms = managed(svc);
        ms.policy = scale_to_zero_policy();
        let mut ctrl = controller(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 95,
                mem_pct: 0,
            }),
        );

        seed_replica(&mut ctrl, &ms).await;
        assert_eq!(ctrl.registry.replica_count(svc), 1);

        // Idle by activity, but metrics are high → must not scale to zero.
        let idle = std::time::Instant::now() - Duration::from_secs(700);
        ctrl.bridge.set_last_activity(&ms.service_name, idle).await;

        let events = ctrl.tick(std::slice::from_ref(&ms), 1000).await;

        assert!(ctrl.registry.replica_count(svc) >= 1);
        assert!(!events.contains(&AutoscaleEvent::ScaledToZero {
            service: "web".to_string()
        }));
    }

    #[tokio::test]
    async fn tick_capacity_denied() {
        let svc = Uuid::now_v7();
        let ms = managed(svc);
        // Capacity for exactly one replica's worth.
        let lim = ResourceLimits::default();
        let mut ctrl = controller(
            ledger(lim.cpu_millis, lim.memory_bytes),
            Box::new(FakeUsage {
                cpu_pct: 100,
                mem_pct: 0,
            }),
        );

        // First tick reaches min=1 (fits exactly).
        ctrl.tick(std::slice::from_ref(&ms), 0).await;
        assert_eq!(ctrl.registry.replica_count(svc), 1);

        // Second tick wants 2 but the ledger is full -> capacity denied, stays 1.
        let e = ctrl.tick(std::slice::from_ref(&ms), 1).await;
        assert_eq!(ctrl.registry.replica_count(svc), 1);
        assert_eq!(
            e,
            vec![AutoscaleEvent::ScaleUpDenied {
                service: "web".to_string(),
                reason: "insufficient_capacity".to_string()
            }]
        );
        assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(1));
    }
}

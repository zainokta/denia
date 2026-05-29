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
use crate::autoscale::registry::{Replica, ReplicaRegistry, ReplicaState};
use crate::autoscale::scaler::{CooldownState, clamp_loop, desired_down, desired_up};
use crate::autoscale::usage::{ServiceUsage, UsageSampler};
use crate::domain::{AutoscalePolicy, HealthCheck, ResourceLimits, RuntimeInstanceId};
use crate::health::HealthChecker;
use crate::ingress::pingora::{ActivationError, ActivationHook, IngressState};
use crate::observability::logs::LogStore;
use crate::observability::metrics::CgroupMetricsReader;
use crate::runtime::Runtime;
use crate::state::SqliteStore;

/// Events emitted by a controller tick (and, for `ScaledToZero`, a later idle task).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoscaleEvent {
    ScaledUp {
        service: String,
        from: u32,
        to: u32,
    },
    ScaledDown {
        service: String,
        from: u32,
        to: u32,
    },
    ScaleUpDenied {
        service: String,
        reason: String,
    },
    ScaledToZero {
        service: String,
    },
    RolloutStep {
        service: String,
        to_deployment: Uuid,
    },
    /// A pre-existing running replica (survived a restart) was re-adopted into
    /// the in-memory registry during boot reconcile.
    Adopted {
        service: String,
        replica_index: u32,
    },
    /// A running workload with no managing service (unknown service or stale
    /// deployment) was stopped during boot reconcile.
    OrphanRemoved {
        service: String,
        replica_index: u32,
    },
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

/// Resolves a service name to its full [`ManagedService`] launch context. The
/// activator only receives a service NAME, but launching a replica needs the
/// artifact, limits, env, ports, and policy — this lookup supplies them.
pub trait ServiceCatalog: Send + Sync {
    fn resolve(&self, service_name: &str) -> Option<ManagedService>;
    /// All currently managed (autoscaled, promoted, artifact-linked) services.
    /// Drives the periodic tick loop and boot reconcile.
    fn all(&self) -> Vec<ManagedService>;
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
    pub ingress: Arc<IngressState>,
    pub health: Arc<dyn HealthChecker>,
    pub store: SqliteStore,
    pub usage: Box<dyn UsageSource>,
    pub catalog: Arc<dyn ServiceCatalog>,
    pub cooldowns: HashMap<Uuid, CooldownState>,
    pub drain_grace: Duration,
    /// Sink for control-plane marker lines (scale-to-zero / wake) written into
    /// the per-service log so the lifecycle is visible in the console.
    pub log_store: LogStore,
}

impl Controller {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        registry: ReplicaRegistry,
        ledger: ResourceLedger,
        runtime: Arc<dyn Runtime>,
        ingress: Arc<IngressState>,
        health: Arc<dyn HealthChecker>,
        store: SqliteStore,
        usage: Box<dyn UsageSource>,
        catalog: Arc<dyn ServiceCatalog>,
        drain_grace: Duration,
        log_store: LogStore,
    ) -> Self {
        Self {
            registry,
            ledger,
            runtime,
            ingress,
            health,
            store,
            usage,
            catalog,
            cooldowns: HashMap::new(),
            drain_grace,
            log_store,
        }
    }

    /// Append a control-plane marker line to the service's log. Best-effort:
    /// the key is the service id string (the log file the API reader tails) and
    /// the format mirrors the deployment log writer (`{rfc3339} {PHASE} {msg}`).
    fn append_service_log(&self, service_id: Uuid, msg: &str) {
        let line = format!("{} AUTOSCALE {msg}\n", chrono::Utc::now().to_rfc3339());
        let _ = self.log_store.append(&service_id.to_string(), &line);
    }

    /// Cold-start activation: launch exactly ONE replica for a scaled-to-zero
    /// service and return `Ok` only once it is Healthy. Single-flight is handled
    /// by the bridge; this just brings up the first replica. Returns `Ok` (no-op)
    /// if a replica already exists (lost the race) or the policy forbids any.
    pub async fn activate_one(&mut self, service: &str) -> Result<(), ActivationError> {
        let ms = self
            .catalog
            .resolve(service)
            .ok_or_else(|| ActivationError::Failed("unknown service".into()))?;
        let current = self.registry.replica_count(ms.service_id) as u32;
        if current >= 1 {
            return Ok(()); // already woke (race) — nothing to do
        }
        if ms.policy.max_replicas == 0 {
            return Ok(()); // defensive; policy.validate forbids, but be safe
        }
        let index = 0; // from zero, first replica is index 0
        let spec = launch_spec(&ms, index);
        match launch_replica(
            &spec,
            &mut self.registry,
            &mut self.ledger,
            self.runtime.as_ref(),
            self.ingress.as_ref(),
            self.health.as_ref(),
        )
        .await
        {
            Ok(_) => {
                self.append_service_log(ms.service_id, "scaling up from zero");
                Ok(())
            }
            Err(LifecycleError::Capacity) => {
                Err(ActivationError::Failed("insufficient_capacity".into()))
            }
            Err(LifecycleError::Health) => Err(ActivationError::Failed("health".into())),
            Err(LifecycleError::Runtime(e)) => Err(ActivationError::Failed(e)),
        }
    }

    /// Boot reconcile (ADR-016). On control-plane startup the in-memory
    /// [`ReplicaRegistry`] is empty, but workloads may still be running from
    /// before a crash or restart. This:
    ///
    /// 1. enumerates running workloads via the runtime,
    /// 2. ADOPTS any whose service is still managed AND whose `deployment_id`
    ///    matches the service's active deployment (registers them Healthy, adds
    ///    them to the bridge, reserves their budget in the ledger),
    /// 3. KILLs + cleans the rest (orphans: unknown service, or stale
    ///    deployment), and
    /// 4. TOPS UP each managed service to its persisted desired count.
    pub async fn reconcile_boot(&mut self, services: &[ManagedService]) -> Vec<AutoscaleEvent> {
        let mut events = Vec::new();
        let running = self.runtime.list_running().await.unwrap_or_default();

        for status in &running {
            let managed = services.iter().find(|m| m.service_id == status.service_id);
            // ADOPT only when the service is still managed, its deployment
            // matches, AND the ledger has budget. Otherwise (unknown service,
            // stale deployment, or no budget) the workload is an orphan.
            let adopt = match managed {
                Some(ms)
                    if status.deployment_id == ms.deployment_id
                        && self.ledger.try_reserve(&ms.limits).is_ok() =>
                {
                    let id = self.registry.add(
                        ms.service_id,
                        status.deployment_id,
                        status.replica_index,
                        status.socket_path.clone(),
                    );
                    self.registry.set_state(id, ReplicaState::Healthy);
                    self.ingress
                        .add_replica(&ms.service_id.to_string(), id, status.socket_path.clone())
                        .await;
                    self.ingress
                        .set_replica_healthy(&ms.service_id.to_string(), id, true)
                        .await;
                    events.push(AutoscaleEvent::Adopted {
                        service: ms.service_name.clone(),
                        replica_index: status.replica_index,
                    });
                    true
                }
                _ => false,
            };

            if !adopt {
                let instance = RuntimeInstanceId {
                    service_id: status.service_id,
                    service_name: status.service_name.clone(),
                    replica_index: status.replica_index,
                };
                let _ = self.runtime.stop(&instance).await;
                events.push(AutoscaleEvent::OrphanRemoved {
                    service: status.service_name.clone(),
                    replica_index: status.replica_index,
                });
            }
        }

        // Top up each managed service to its persisted desired count, clamped to
        // the policy bounds. A `min_replicas==0` service with no persisted
        // desired stays at 0 (the activator wakes it on the first request).
        for ms in services {
            events.extend(self.top_up_to_desired(ms).await);
            // Per-session logs are wiped on boot, so a scale-to-zero service that
            // is never woken this session shows a blank log panel. Append a marker
            // (this runs after `clean_session_logs`, so it survives) for services
            // left genuinely idle at zero — not a capacity-denied launch.
            if ms.policy.min_replicas == 0 && self.registry.replica_count(ms.service_id) == 0 {
                self.append_service_log(
                    ms.service_id,
                    "idle (scaled to zero) — waiting for first request to wake",
                );
            }
        }

        events
    }

    /// Bring `ms` up to its persisted desired count, clamped to the policy
    /// bounds. Start-bounded: it launches only `target - current`, so it is
    /// idempotent across repeated calls and redeploys (existing replicas are
    /// never double-started; a redeploy's rollover is handled by `tick`). A
    /// `min_replicas==0` service with no persisted desired stays at 0 (the
    /// activator wakes it on the first request). Shared by `reconcile_boot` and
    /// the single-service `reconcile_service` deploy hand-off.
    async fn top_up_to_desired(&mut self, ms: &ManagedService) -> Vec<AutoscaleEvent> {
        let mut events = Vec::new();
        let desired = self
            .store
            .get_desired_replicas(ms.service_id)
            .ok()
            .flatten()
            .unwrap_or(ms.policy.min_replicas);
        let target = desired.clamp(ms.policy.min_replicas, ms.policy.max_replicas);
        let start = self.registry.replica_count(ms.service_id) as u32;
        let mut current = start;

        while current < target {
            let index = self.next_replica_index(ms.service_id);
            let spec = launch_spec(ms, index);
            match launch_replica(
                &spec,
                &mut self.registry,
                &mut self.ledger,
                self.runtime.as_ref(),
                self.ingress.as_ref(),
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

        if current > start {
            events.push(AutoscaleEvent::ScaledUp {
                service: ms.service_name.clone(),
                from: start,
                to: current,
            });
        }

        self.store.set_desired_replicas(ms.service_id, current).ok();
        events
    }

    /// Drain every replica of `ms`: stop new traffic, run out the grace window,
    /// stop the runtime, release the ledger, and remove from the registry +
    /// ingress pool. Shared by the idle scale-to-zero branch and the `drain_all`
    /// stop path.
    async fn drain_all_replicas(&mut self, ms: &ManagedService) {
        let replicas: Vec<(Uuid, u32)> = self
            .registry
            .replicas(ms.service_id)
            .iter()
            .map(|r| (r.id, r.index))
            .collect();
        for (replica_id, index) in replicas {
            let instance = RuntimeInstanceId {
                service_id: ms.service_id,
                service_name: ms.service_name.clone(),
                replica_index: index,
            };
            let _ = drain_replica(
                &ms.service_id.to_string(),
                replica_id,
                &instance,
                &ms.limits,
                self.drain_grace,
                &mut self.registry,
                &mut self.ledger,
                self.runtime.as_ref(),
                self.ingress.as_ref(),
            )
            .await;
        }
    }

    /// Single-service reconcile, invoked after a runtime deploy of an autoscaled
    /// service hands ownership of its replicas to the controller. Brings the
    /// service up to its persisted desired count via the same start-bounded
    /// top-up loop as `reconcile_boot`, but WITHOUT the boot adopt/orphan pass.
    /// Returns an empty Vec if the service is not currently managed (no
    /// autoscale policy / no promoted deployment / no linked artifact).
    pub async fn reconcile_service(&mut self, service_id: Uuid) -> Vec<AutoscaleEvent> {
        let Some(ms) = self.catalog.resolve(&service_id.to_string()) else {
            return Vec::new();
        };
        self.top_up_to_desired(&ms).await
    }

    /// Drain and forget every replica of `service_id` (the stop path for an
    /// autoscaled service). Releases the ledger, removes ingress + registry
    /// entries, and persists `desired=0`. Resolve the catalog here BEFORE the
    /// caller clears the promoted row — once it is cleared the service is no
    /// longer resolvable and this becomes a no-op.
    pub async fn drain_all(&mut self, service_id: Uuid) -> Vec<AutoscaleEvent> {
        let Some(ms) = self.catalog.resolve(&service_id.to_string()) else {
            return Vec::new();
        };
        let start = self.registry.replica_count(service_id) as u32;
        self.drain_all_replicas(&ms).await;
        self.store.set_desired_replicas(service_id, 0).ok();
        let mut events = Vec::new();
        if start > 0 {
            events.push(AutoscaleEvent::ScaledDown {
                service: ms.service_name.clone(),
                from: start,
                to: 0,
            });
        }
        events
    }

    /// Drive each service one step toward its desired replica count.
    pub async fn tick(&mut self, services: &[ManagedService], now_s: u64) -> Vec<AutoscaleEvent> {
        use std::time::Instant;
        let mut events = Vec::new();

        'service: for ms in services {
            let start = self.registry.replica_count(ms.service_id) as u32;

            // 0. Rolling replace (ADR-016). If any live replica runs an older
            // deployment than the desired `ms.deployment_id`, roll exactly one
            // replica to the new deployment this tick. Normal scaling is deferred
            // while old replicas remain; each subsequent tick re-evaluates `old`,
            // so the rollout completes one replica at a time over several ticks.
            let to_dep = ms.deployment_id;
            let old: Vec<(Uuid, u32)> = self
                .registry
                .replicas(ms.service_id)
                .iter()
                .filter(|r| r.deployment_id != to_dep)
                .map(|r| (r.id, r.index))
                .collect();
            if !old.is_empty() {
                let next_index = self.next_replica_index(ms.service_id);
                let new_spec = launch_spec(ms, next_index);
                let total = self.registry.replica_count(ms.service_id) as u32;
                let (old_id, old_index) = old[0];
                let instance = RuntimeInstanceId {
                    service_id: ms.service_id,
                    service_name: ms.service_name.clone(),
                    replica_index: old_index,
                };

                if total <= 1 {
                    // Single replica: launch-then-drain (brief +1 surge) so the
                    // service never drops to zero capacity mid-rollout.
                    match launch_replica(
                        &new_spec,
                        &mut self.registry,
                        &mut self.ledger,
                        self.runtime.as_ref(),
                        self.ingress.as_ref(),
                        self.health.as_ref(),
                    )
                    .await
                    {
                        Ok(_) => {
                            let _ = drain_replica(
                                &ms.service_id.to_string(),
                                old_id,
                                &instance,
                                &ms.limits,
                                self.drain_grace,
                                &mut self.registry,
                                &mut self.ledger,
                                self.runtime.as_ref(),
                                self.ingress.as_ref(),
                            )
                            .await;
                            events.push(AutoscaleEvent::RolloutStep {
                                service: ms.service_name.clone(),
                                to_deployment: to_dep,
                            });
                        }
                        Err(LifecycleError::Capacity) => {
                            events.push(AutoscaleEvent::ScaleUpDenied {
                                service: ms.service_name.clone(),
                                reason: "insufficient_capacity".to_string(),
                            });
                        }
                        Err(e) => {
                            events.push(AutoscaleEvent::ScaleUpDenied {
                                service: ms.service_name.clone(),
                                reason: e.to_string(),
                            });
                        }
                    }
                } else {
                    // Multiple replicas: drain-then-launch (maxUnavailable=1, no
                    // surge) so the rollout stays within the resource budget.
                    let _ = drain_replica(
                        &ms.service_id.to_string(),
                        old_id,
                        &instance,
                        &ms.limits,
                        self.drain_grace,
                        &mut self.registry,
                        &mut self.ledger,
                        self.runtime.as_ref(),
                        self.ingress.as_ref(),
                    )
                    .await;
                    match launch_replica(
                        &new_spec,
                        &mut self.registry,
                        &mut self.ledger,
                        self.runtime.as_ref(),
                        self.ingress.as_ref(),
                        self.health.as_ref(),
                    )
                    .await
                    {
                        Ok(_) => {
                            events.push(AutoscaleEvent::RolloutStep {
                                service: ms.service_name.clone(),
                                to_deployment: to_dep,
                            });
                        }
                        Err(LifecycleError::Capacity) => {
                            events.push(AutoscaleEvent::ScaleUpDenied {
                                service: ms.service_name.clone(),
                                reason: "insufficient_capacity".to_string(),
                            });
                        }
                        Err(e) => {
                            events.push(AutoscaleEvent::ScaleUpDenied {
                                service: ms.service_name.clone(),
                                reason: e.to_string(),
                            });
                        }
                    }
                }

                self.store
                    .set_desired_replicas(
                        ms.service_id,
                        self.registry.replica_count(ms.service_id) as u32,
                    )
                    .ok();
                continue 'service; // defer normal scaling this tick
            }

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
                    let idle_secs =
                        match self.ingress.last_activity(&ms.service_id.to_string()).await {
                            Some(t) => Instant::now().saturating_duration_since(t).as_secs(),
                            None => u64::MAX,
                        };
                    let metrics_low = u.avg_cpu_pct < ms.policy.target_cpu_pct as u32;
                    if idle_secs > ms.policy.idle_timeout_s as u64 && metrics_low {
                        self.drain_all_replicas(ms).await;
                        self.store.set_desired_replicas(ms.service_id, 0).ok();
                        self.append_service_log(ms.service_id, "scaled to zero (idle)");
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
                let spec = launch_spec(ms, replica_index);
                match launch_replica(
                    &spec,
                    &mut self.registry,
                    &mut self.ledger,
                    self.runtime.as_ref(),
                    self.ingress.as_ref(),
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
                    service_id: ms.service_id,
                    service_name: ms.service_name.clone(),
                    replica_index: replica.index,
                };
                match drain_replica(
                    &ms.service_id.to_string(),
                    replica.id,
                    &instance,
                    &ms.limits,
                    self.drain_grace,
                    &mut self.registry,
                    &mut self.ledger,
                    self.runtime.as_ref(),
                    self.ingress.as_ref(),
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
                if current == 0 {
                    self.append_service_log(ms.service_id, "scaled to zero");
                }
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

    /// Total replica count for a service (running + launching + draining).
    pub fn replica_count(&self, service_id: uuid::Uuid) -> u32 {
        self.registry.replica_count(service_id) as u32
    }

    /// Number of replicas in the Healthy state for a service.
    pub fn healthy_replicas(&self, service_id: uuid::Uuid) -> u32 {
        self.registry.healthy_count(service_id) as u32
    }

    /// Catalog-driven tick: fetches all managed services from the catalog and
    /// drives one scaling tick for each.
    pub async fn tick_all(&mut self, now_s: u64) -> Vec<AutoscaleEvent> {
        let services = self.catalog.all();
        self.tick(&services, now_s).await
    }

    /// Catalog-driven boot reconcile: fetches all managed services from the
    /// catalog and runs [`reconcile_boot`] over them.
    pub async fn reconcile_boot_all(&mut self) -> Vec<AutoscaleEvent> {
        let services = self.catalog.all();
        self.reconcile_boot(&services).await
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

/// Build a [`LaunchSpec`] for one replica of `ms` at `replica_index`. Shared by
/// cold-start activation, the reconcile loop, and the rollout branch so spec
/// construction stays in one place.
fn launch_spec(ms: &ManagedService, replica_index: u32) -> LaunchSpec {
    LaunchSpec {
        service_name: ms.service_name.clone(),
        service_id: ms.service_id,
        deployment_id: ms.deployment_id,
        replica_index,
        artifact: ms.artifact.clone(),
        internal_port: ms.internal_port,
        limits: ms.limits.clone(),
        env: ms.env.clone(),
        health_check: ms.health_check.clone(),
    }
}

/// Shared handle over a [`Controller`] that satisfies the bridge's
/// [`ActivationHook`]. The wiring task stores the same `Arc<Mutex<Controller>>`
/// to drive `tick`, so cold-start activation and the periodic loop serialize on
/// one lock.
#[derive(Clone)]
pub struct SharedController(pub Arc<tokio::sync::Mutex<Controller>>);

#[async_trait::async_trait]
impl ActivationHook for SharedController {
    async fn activate(&self, service: &str) -> Result<(), ActivationError> {
        let mut guard = self.0.lock().await;
        guard.activate_one(service).await
    }
}

/// Background run loop for the autoscaler, mirroring `scheduler::run_until_shutdown`.
///
/// Ticks the controller at the given `interval` until a shutdown signal is
/// received on `shutdown`. The controller is held behind an `Arc<Mutex>` so
/// it can be shared with [`SharedController`] for cold-start activation.
pub async fn run_until_shutdown(
    controller: std::sync::Arc<tokio::sync::Mutex<Controller>>,
    interval: std::time::Duration,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let mut ticker = tokio::time::interval(interval);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = ticker.tick() => {
                let now_s = chrono::Utc::now().timestamp().max(0) as u64;
                let mut guard = controller.lock().await;
                let _ = guard.tick_all(now_s).await;
            }
        }
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

    /// In-memory [`ServiceCatalog`] backed by an id→[`ManagedService`] map
    /// (keyed by `service_id.to_string()`, matching production identity, F-3).
    #[derive(Default)]
    struct FakeCatalog {
        services: HashMap<String, ManagedService>,
    }

    impl FakeCatalog {
        fn with(ms: ManagedService) -> Self {
            let mut services = HashMap::new();
            services.insert(ms.service_id.to_string(), ms);
            Self { services }
        }
    }

    impl ServiceCatalog for FakeCatalog {
        fn resolve(&self, service_key: &str) -> Option<ManagedService> {
            self.services.get(service_key).cloned()
        }

        fn all(&self) -> Vec<ManagedService> {
            self.services.values().cloned().collect()
        }
    }

    fn controller(ledger: ResourceLedger, usage: Box<dyn UsageSource>) -> Controller {
        controller_with_catalog(ledger, usage, Arc::new(FakeCatalog::default()))
    }

    fn controller_with_catalog(
        ledger: ResourceLedger,
        usage: Box<dyn UsageSource>,
        catalog: Arc<dyn ServiceCatalog>,
    ) -> Controller {
        controller_full(ledger, usage, catalog, Arc::new(FakeRuntime::default()))
    }

    fn controller_full(
        ledger: ResourceLedger,
        usage: Box<dyn UsageSource>,
        catalog: Arc<dyn ServiceCatalog>,
        runtime: Arc<FakeRuntime>,
    ) -> Controller {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        Controller::new(
            ReplicaRegistry::default(),
            ledger,
            runtime,
            Arc::new(IngressState::default()),
            Arc::new(FakeHealthChecker::healthy()),
            store,
            usage,
            catalog,
            Duration::ZERO,
            LogStore::new(std::env::temp_dir()),
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
            ctrl.ingress.as_ref(),
            ctrl.health.as_ref(),
        )
        .await
        .expect("seed launch ok")
    }

    /// Seed one healthy replica at a specific deployment and index, so a rollout
    /// test can stage replicas on an OLD deployment before ticking with a new one.
    async fn seed_replica_at(
        ctrl: &mut Controller,
        ms: &ManagedService,
        deployment_id: Uuid,
        index: u32,
    ) -> Uuid {
        let mut spec = launch_spec(ms, index);
        spec.deployment_id = deployment_id;
        launch_replica(
            &spec,
            &mut ctrl.registry,
            &mut ctrl.ledger,
            ctrl.runtime.as_ref(),
            ctrl.ingress.as_ref(),
            ctrl.health.as_ref(),
        )
        .await
        .expect("seed launch ok")
    }

    fn deployment_ids_for(ctrl: &Controller, svc: Uuid) -> Vec<Uuid> {
        ctrl.registry
            .replicas(svc)
            .iter()
            .map(|r| r.deployment_id)
            .collect()
    }

    #[tokio::test]
    async fn rollout_replaces_old_with_new() {
        let svc = Uuid::now_v7();
        let d1 = Uuid::now_v7();
        let d2 = Uuid::now_v7();
        let mut ms = managed(svc);
        ms.deployment_id = d2; // desired deployment
        let mut ctrl = controller(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
        );

        // Two replicas already running the OLD deployment d1.
        seed_replica_at(&mut ctrl, &ms, d1, 0).await;
        seed_replica_at(&mut ctrl, &ms, d1, 1).await;
        assert_eq!(ctrl.registry.replica_count(svc), 2);

        // First tick: drain-then-launch one. Still 2 replicas, exactly one at d2.
        let e0 = ctrl.tick(std::slice::from_ref(&ms), 0).await;
        assert_eq!(ctrl.registry.replica_count(svc), 2);
        assert!(e0.contains(&AutoscaleEvent::RolloutStep {
            service: "web".to_string(),
            to_deployment: d2,
        }));
        let deps = deployment_ids_for(&ctrl, svc);
        assert_eq!(deps.iter().filter(|d| **d == d2).count(), 1);
        assert_eq!(deps.iter().filter(|d| **d == d1).count(), 1);

        // Second tick: the last old replica rolls to d2.
        let e1 = ctrl.tick(std::slice::from_ref(&ms), 1).await;
        assert_eq!(ctrl.registry.replica_count(svc), 2);
        assert!(e1.contains(&AutoscaleEvent::RolloutStep {
            service: "web".to_string(),
            to_deployment: d2,
        }));
        assert!(deployment_ids_for(&ctrl, svc).iter().all(|d| *d == d2));

        // Third tick: nothing to roll, no further RolloutStep.
        let e2 = ctrl.tick(std::slice::from_ref(&ms), 2).await;
        assert!(!e2.contains(&AutoscaleEvent::RolloutStep {
            service: "web".to_string(),
            to_deployment: d2,
        }));
    }

    #[tokio::test]
    async fn rollout_defers_scaling() {
        let svc = Uuid::now_v7();
        let d1 = Uuid::now_v7();
        let d2 = Uuid::now_v7();
        let mut ms = managed(svc);
        ms.deployment_id = d2;
        // Very high cpu would normally scale up, but rollout takes priority.
        let mut ctrl = controller(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 100,
                mem_pct: 0,
            }),
        );

        seed_replica_at(&mut ctrl, &ms, d1, 0).await;
        seed_replica_at(&mut ctrl, &ms, d1, 1).await;
        assert_eq!(ctrl.registry.replica_count(svc), 2);

        let events = ctrl.tick(std::slice::from_ref(&ms), 0).await;

        // Replaced one replica, did not scale up: count stays 2, no ScaledUp.
        assert_eq!(ctrl.registry.replica_count(svc), 2);
        assert!(events.contains(&AutoscaleEvent::RolloutStep {
            service: "web".to_string(),
            to_deployment: d2,
        }));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AutoscaleEvent::ScaledUp { .. }))
        );
    }

    #[tokio::test]
    async fn single_replica_launch_then_drain() {
        let svc = Uuid::now_v7();
        let d1 = Uuid::now_v7();
        let d2 = Uuid::now_v7();
        let mut ms = managed(svc);
        ms.deployment_id = d2;
        let mut ctrl = controller(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
        );

        seed_replica_at(&mut ctrl, &ms, d1, 0).await;
        assert_eq!(ctrl.registry.replica_count(svc), 1);

        let events = ctrl.tick(std::slice::from_ref(&ms), 0).await;

        // Ends with exactly one replica, now on d2 (launched before old drained).
        assert_eq!(ctrl.registry.replica_count(svc), 1);
        assert_eq!(deployment_ids_for(&ctrl, svc), vec![d2]);
        assert!(events.contains(&AutoscaleEvent::RolloutStep {
            service: "web".to_string(),
            to_deployment: d2,
        }));
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
        ctrl.ingress
            .set_last_activity(&ms.service_id.to_string(), idle)
            .await;

        let events = ctrl.tick(std::slice::from_ref(&ms), 1000).await;

        assert_eq!(ctrl.registry.replica_count(svc), 0);
        assert!(events.contains(&AutoscaleEvent::ScaledToZero {
            service: "web".to_string()
        }));
        assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(0));

        // A control-plane marker line lands in the service log on the >0->0 drain.
        let logs = ctrl.log_store.read_recent(&svc.to_string(), 10).unwrap();
        assert!(logs.iter().any(|l| l.contains("AUTOSCALE scaled to zero")));
    }

    #[tokio::test]
    async fn activate_logs_wake_from_zero() {
        let svc = Uuid::now_v7();
        let ms = managed(svc);
        let mut ctrl = controller_with_catalog(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            Arc::new(FakeCatalog::with(ms.clone())),
        );
        assert_eq!(ctrl.registry.replica_count(svc), 0);

        ctrl.activate_one(&svc.to_string()).await.unwrap();
        assert_eq!(ctrl.registry.replica_count(svc), 1);

        let logs = ctrl.log_store.read_recent(&svc.to_string(), 10).unwrap();
        assert!(
            logs.iter()
                .any(|l| l.contains("AUTOSCALE scaling up from zero"))
        );
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
        ctrl.ingress
            .set_last_activity(&ms.service_id.to_string(), std::time::Instant::now())
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
        ctrl.ingress
            .set_last_activity(&ms.service_id.to_string(), idle)
            .await;

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

    /// Build a scale-from-zero managed service (min=0, max=3) named `web`.
    fn zero_scaled(svc: Uuid) -> ManagedService {
        let mut ms = managed(svc);
        ms.policy = scale_to_zero_policy();
        ms
    }

    #[tokio::test]
    async fn activate_launches_one_replica() {
        let svc = Uuid::now_v7();
        let ms = zero_scaled(svc);
        let mut ctrl = controller_with_catalog(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            Arc::new(FakeCatalog::with(ms.clone())),
        );

        ctrl.activate_one(&svc.to_string())
            .await
            .expect("activation ok");

        assert_eq!(ctrl.registry.replica_count(svc), 1);
        assert_eq!(ctrl.registry.healthy_count(svc), 1);
        assert_eq!(ctrl.ingress.healthy_count(&svc.to_string()).await, 1);
    }

    #[tokio::test]
    async fn activate_capacity_denied() {
        let svc = Uuid::now_v7();
        let ms = zero_scaled(svc);
        let lim = ResourceLimits::default();
        // Capacity for exactly one replica, pre-filled so nothing fits.
        let mut led = ledger(lim.cpu_millis, lim.memory_bytes);
        led.try_reserve(&lim).expect("prefill");
        let mut ctrl = controller_with_catalog(
            led,
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            Arc::new(FakeCatalog::with(ms.clone())),
        );

        let err = ctrl
            .activate_one(&svc.to_string())
            .await
            .expect_err("capacity should deny");
        match err {
            ActivationError::Failed(reason) => assert!(
                reason.contains("insufficient_capacity"),
                "reason was {reason}"
            ),
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(ctrl.registry.replica_count(svc), 0);
    }

    #[tokio::test]
    async fn activate_noop_when_already_running() {
        let svc = Uuid::now_v7();
        let ms = zero_scaled(svc);
        let runtime = Arc::new(FakeRuntime::default());
        let mut ctrl = controller_full(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            Arc::new(FakeCatalog::with(ms.clone())),
            runtime.clone(),
        );

        // Pre-seed one replica via the lifecycle primitive on the controller fields.
        seed_replica(&mut ctrl, &ms).await;
        assert_eq!(ctrl.registry.replica_count(svc), 1);
        assert_eq!(runtime.started_requests().len(), 1);

        ctrl.activate_one(&svc.to_string())
            .await
            .expect("activation ok");

        // No second replica launched: count and runtime start count both unchanged.
        assert_eq!(ctrl.registry.replica_count(svc), 1);
        assert_eq!(runtime.started_requests().len(), 1);
    }

    #[tokio::test]
    async fn activate_unknown_service_errs() {
        let mut ctrl = controller_with_catalog(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            Arc::new(FakeCatalog::default()),
        );

        let err = ctrl
            .activate_one("nope")
            .await
            .expect_err("unknown service");
        match err {
            ActivationError::Failed(reason) => assert_eq!(reason, "unknown service"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// Pre-start a workload directly in a [`FakeRuntime`] so its `list_running`
    /// reports it, simulating a replica that survived a control-plane restart.
    async fn prestart(
        runtime: &FakeRuntime,
        service_id: Uuid,
        service_name: &str,
        deployment_id: Uuid,
        replica_index: u32,
    ) {
        use crate::domain::RuntimeStartRequest;
        runtime
            .start(RuntimeStartRequest {
                service_name: service_name.to_string(),
                service_id,
                deployment_id,
                artifact: artifact(),
                internal_port: 8080,
                socket_path: format!("/run/denia/{service_name}-{replica_index}.sock").into(),
                cpu_millis: 0,
                memory_bytes: 0,
                env: Vec::new(),
                pids_max: None,
                memory_swap_max: None,
                io_weight: None,
                replica_index,
            })
            .await
            .expect("prestart");
    }

    #[tokio::test]
    async fn reconcile_adopts_matching_and_kills_stale() {
        let svc = Uuid::now_v7();
        let d_active = Uuid::now_v7();
        let d_old = Uuid::now_v7();
        let mut ms = managed(svc);
        ms.deployment_id = d_active;

        let runtime = Arc::new(FakeRuntime::default());
        // One replica at the active deployment -> ADOPT.
        prestart(&runtime, svc, "web", d_active, 0).await;
        // One replica at a stale deployment -> ORPHAN (stop).
        prestart(&runtime, svc, "web", d_old, 1).await;
        // One replica for an unknown service -> ORPHAN (stop).
        prestart(&runtime, Uuid::now_v7(), "ghost", d_active, 0).await;

        let mut ctrl = controller_full(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            Arc::new(FakeCatalog::with(ms.clone())),
            runtime.clone(),
        );

        let events = ctrl.reconcile_boot(std::slice::from_ref(&ms)).await;

        // Adopted replica is registered Healthy at index 0.
        let replicas = ctrl.registry.replicas(svc);
        let adopted = replicas
            .iter()
            .find(|r| r.index == 0 && r.deployment_id == d_active)
            .expect("adopted replica present");
        assert_eq!(adopted.state, ReplicaState::Healthy);
        assert_eq!(ctrl.ingress.healthy_count(&svc.to_string()).await, 1);
        assert!(events.contains(&AutoscaleEvent::Adopted {
            service: "web".to_string(),
            replica_index: 0,
        }));

        // The stale and unknown replicas were stopped and reported as orphans.
        let stopped = runtime.stopped_instances();
        assert!(
            stopped
                .iter()
                .any(|i| i.service_name == "web" && i.replica_index == 1),
            "stale web replica stopped"
        );
        assert!(
            stopped
                .iter()
                .any(|i| i.service_name == "ghost" && i.replica_index == 0),
            "unknown ghost replica stopped"
        );
        assert!(events.contains(&AutoscaleEvent::OrphanRemoved {
            service: "web".to_string(),
            replica_index: 1,
        }));
        assert!(events.contains(&AutoscaleEvent::OrphanRemoved {
            service: "ghost".to_string(),
            replica_index: 0,
        }));

        // The stale replica index 1 must not survive in the registry.
        assert!(replicas.iter().all(|r| r.index != 1));
    }

    #[tokio::test]
    async fn reconcile_tops_up_to_persisted_desired() {
        let svc = Uuid::now_v7();
        let ms = managed(svc);
        let runtime = Arc::new(FakeRuntime::default());
        let mut ctrl = controller_full(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            Arc::new(FakeCatalog::with(ms.clone())),
            runtime.clone(),
        );
        // Persisted desired of 2, no orphans running.
        ctrl.store.set_desired_replicas(svc, 2).unwrap();

        let events = ctrl.reconcile_boot(std::slice::from_ref(&ms)).await;

        assert_eq!(ctrl.registry.replica_count(svc), 2);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AutoscaleEvent::Adopted { .. }))
        );
        assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(2));
    }

    #[tokio::test]
    async fn reconcile_min_zero_stays_zero() {
        let svc = Uuid::now_v7();
        let mut ms = managed(svc);
        ms.policy = scale_to_zero_policy();
        let runtime = Arc::new(FakeRuntime::default());
        let mut ctrl = controller_full(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            Arc::new(FakeCatalog::with(ms.clone())),
            runtime.clone(),
        );

        // No persisted desired, min_replicas == 0, no orphans.
        let _events = ctrl.reconcile_boot(std::slice::from_ref(&ms)).await;

        assert_eq!(ctrl.registry.replica_count(svc), 0);
        assert!(runtime.started_requests().is_empty());
    }

    // ---- deploy hand-off: reconcile_service / drain_all (ADR-028) ----

    #[tokio::test]
    async fn reconcile_service_min1_launches_min() {
        let svc = Uuid::now_v7();
        let ms = managed(svc); // policy() has min_replicas == 1
        let mut ctrl = controller_with_catalog(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            Arc::new(FakeCatalog::with(ms.clone())),
        );

        let events = ctrl.reconcile_service(svc).await;

        assert_eq!(ctrl.registry.replica_count(svc), 1);
        assert_eq!(ctrl.registry.healthy_count(svc), 1);
        assert_eq!(ctrl.ingress.healthy_count(&svc.to_string()).await, 1);
        assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(1));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AutoscaleEvent::ScaledUp { from: 0, to: 1, .. }))
        );
    }

    #[tokio::test]
    async fn reconcile_service_min0_stays_zero() {
        let svc = Uuid::now_v7();
        let ms = zero_scaled(svc); // min_replicas == 0
        let runtime = Arc::new(FakeRuntime::default());
        let mut ctrl = controller_full(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            Arc::new(FakeCatalog::with(ms.clone())),
            runtime.clone(),
        );

        let events = ctrl.reconcile_service(svc).await;

        // min==0 with no persisted desired: the activator owns the 0->1 wake, so
        // the deploy hand-off launches nothing.
        assert_eq!(ctrl.registry.replica_count(svc), 0);
        assert!(runtime.started_requests().is_empty());
        assert!(events.is_empty());
        assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(0));
    }

    #[tokio::test]
    async fn reconcile_service_idempotent_on_redeploy() {
        let svc = Uuid::now_v7();
        let ms = managed(svc);
        let runtime = Arc::new(FakeRuntime::default());
        let mut ctrl = controller_full(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            Arc::new(FakeCatalog::with(ms.clone())),
            runtime.clone(),
        );
        // Two replicas already running at the promoted deployment, desired=2.
        seed_replica_at(&mut ctrl, &ms, ms.deployment_id, 0).await;
        seed_replica_at(&mut ctrl, &ms, ms.deployment_id, 1).await;
        ctrl.store.set_desired_replicas(svc, 2).unwrap();
        assert_eq!(runtime.started_requests().len(), 2);

        let events = ctrl.reconcile_service(svc).await;

        // Start-bounded: nothing new launched, the rollout is `tick`'s job.
        assert_eq!(ctrl.registry.replica_count(svc), 2);
        assert_eq!(runtime.started_requests().len(), 2);
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn reconcile_service_unknown_noop() {
        let svc = Uuid::now_v7();
        let runtime = Arc::new(FakeRuntime::default());
        // Empty catalog: the service is not managed.
        let mut ctrl = controller_full(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            Arc::new(FakeCatalog::default()),
            runtime.clone(),
        );

        let events = ctrl.reconcile_service(svc).await;

        assert!(events.is_empty());
        assert_eq!(ctrl.registry.replica_count(svc), 0);
        assert!(runtime.started_requests().is_empty());
    }

    #[tokio::test]
    async fn drain_all_drains_and_releases() {
        let svc = Uuid::now_v7();
        let ms = managed(svc);
        let mut ctrl = controller_with_catalog(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            Arc::new(FakeCatalog::with(ms.clone())),
        );
        seed_replica_at(&mut ctrl, &ms, ms.deployment_id, 0).await;
        seed_replica_at(&mut ctrl, &ms, ms.deployment_id, 1).await;
        assert_eq!(ctrl.registry.replica_count(svc), 2);
        assert!(ctrl.ledger.committed_cpu() > 0);

        let events = ctrl.drain_all(svc).await;

        assert_eq!(ctrl.registry.replica_count(svc), 0);
        assert_eq!(ctrl.ledger.committed_cpu(), 0);
        assert_eq!(ctrl.ledger.committed_mem(), 0);
        assert_eq!(ctrl.ingress.healthy_count(&svc.to_string()).await, 0);
        assert_eq!(ctrl.store.get_desired_replicas(svc).unwrap(), Some(0));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AutoscaleEvent::ScaledDown { from: 2, to: 0, .. }))
        );
    }

    // ---- catalog-driven method tests (TDD: written before implementation) ----

    #[tokio::test]
    async fn tick_all_uses_catalog() {
        let svc = Uuid::now_v7();
        let ms = managed(svc);
        let catalog = Arc::new(FakeCatalog::with(ms.clone()));
        let mut ctrl = controller_with_catalog(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            catalog,
        );

        // tick_all should fetch the service from the catalog and bring it to
        // min_replicas=1 (same as calling tick(&[ms], 0)).
        let events = ctrl.tick_all(0).await;

        assert_eq!(ctrl.registry.replica_count(svc), 1);
        assert!(
            events.contains(&AutoscaleEvent::ScaledUp {
                service: "web".to_string(),
                from: 0,
                to: 1,
            }),
            "expected ScaledUp event, got: {events:?}"
        );
    }

    #[tokio::test]
    async fn reconcile_boot_all_uses_catalog() {
        let svc = Uuid::now_v7();
        let ms = managed(svc);
        let catalog = Arc::new(FakeCatalog::with(ms.clone()));
        let runtime = Arc::new(FakeRuntime::default());
        let mut ctrl = controller_full(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            catalog,
            runtime.clone(),
        );
        // Persisted desired = 2 so reconcile_boot tops up without any orphans.
        ctrl.store.set_desired_replicas(svc, 2).unwrap();

        let events = ctrl.reconcile_boot_all().await;

        assert_eq!(ctrl.registry.replica_count(svc), 2);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AutoscaleEvent::ScaledUp { .. })),
            "expected ScaledUp event, got: {events:?}"
        );
    }

    #[tokio::test]
    async fn shared_controller_delegates() {
        use crate::ingress::pingora::ActivationHook;

        let svc = Uuid::now_v7();
        let ms = zero_scaled(svc);
        let ctrl = controller_with_catalog(
            ledger(4000, 4 << 30),
            Box::new(FakeUsage {
                cpu_pct: 0,
                mem_pct: 0,
            }),
            Arc::new(FakeCatalog::with(ms.clone())),
        );

        let shared = SharedController(Arc::new(tokio::sync::Mutex::new(ctrl)));
        shared
            .activate(&svc.to_string())
            .await
            .expect("activation ok");

        let guard = shared.0.lock().await;
        assert_eq!(guard.registry.replica_count(svc), 1);
        assert_eq!(guard.ingress.healthy_count(&svc.to_string()).await, 1);
    }
}

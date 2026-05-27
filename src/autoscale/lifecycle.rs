use std::time::Duration;

use thiserror::Error;
use uuid::Uuid;

use crate::artifacts::ArtifactRecord;
use crate::autoscale::ledger::ResourceLedger;
use crate::autoscale::registry::{ReplicaRegistry, ReplicaState};
use crate::domain::{HealthCheck, ResourceLimits, RuntimeInstanceId, RuntimeStartRequest};
use crate::health::HealthChecker;
use crate::ingress::bridge::LoopbackBridgeSupervisor;
use crate::runtime::Runtime;

/// Everything needed to bring up a single replica of a service.
pub struct LaunchSpec {
    pub service_name: String,
    pub service_id: Uuid,
    pub deployment_id: Uuid,
    pub replica_index: u32,
    pub artifact: ArtifactRecord,
    pub internal_port: u16,
    pub limits: ResourceLimits,
    pub env: Vec<(String, String)>,
    pub health_check: HealthCheck,
}

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("insufficient capacity")]
    Capacity,
    #[error("runtime error: {0}")]
    Runtime(String),
    #[error("health check failed")]
    Health,
}

/// Reserve budget, start the runtime, register the replica, wire the bridge, and
/// gate on a health check. On any failure after the reservation, all partial
/// state (ledger, runtime, registry, bridge) is rolled back so nothing leaks.
pub async fn launch_replica(
    spec: &LaunchSpec,
    registry: &mut ReplicaRegistry,
    ledger: &mut ResourceLedger,
    runtime: &dyn Runtime,
    bridge: &LoopbackBridgeSupervisor,
    health: &dyn HealthChecker,
) -> Result<Uuid, LifecycleError> {
    // 1. Reserve before spawning so concurrent scale-ups can't double-spend.
    ledger
        .try_reserve(&spec.limits)
        .map_err(|_| LifecycleError::Capacity)?;

    // 2. Start the runtime. The overlay runtime computes its real socket path;
    //    the placeholder mirrors the deploy coordinator.
    let request = RuntimeStartRequest {
        service_name: spec.service_name.clone(),
        service_id: spec.service_id,
        deployment_id: spec.deployment_id,
        artifact: spec.artifact.clone(),
        internal_port: spec.internal_port,
        socket_path: format!("/var/lib/denia/runtime/{}/current.sock", spec.service_id).into(),
        cpu_millis: spec.limits.cpu_millis,
        memory_bytes: spec.limits.memory_bytes,
        env: spec.env.clone(),
        pids_max: None,
        memory_swap_max: None,
        io_weight: None,
        replica_index: spec.replica_index,
    };
    let status = match runtime.start(request).await {
        Ok(status) => status,
        Err(e) => {
            ledger.release(&spec.limits);
            return Err(LifecycleError::Runtime(e.to_string()));
        }
    };

    // 3. Register the replica (Pending) using the runtime's reported socket.
    let replica_id = registry.add(
        spec.service_id,
        spec.deployment_id,
        spec.replica_index,
        status.socket_path.clone(),
    );

    // Bridge/route/runtime state is keyed by service_id (globally unique), not
    // service.name (project-scoped), to prevent cross-project collisions (F-3).
    let service_key = spec.service_id.to_string();

    // 4. Register the endpoint (unhealthy) in the bridge pool.
    bridge
        .add_replica(&service_key, replica_id, status.socket_path.clone())
        .await;

    // 5. Gate on the health check; roll back everything on failure.
    let url = format!("http://127.0.0.1:{}", spec.internal_port);
    if health.check(&url, &spec.health_check).await.is_err() {
        let instance = RuntimeInstanceId {
            service_id: spec.service_id,
            service_name: spec.service_name.clone(),
            replica_index: spec.replica_index,
        };
        let _ = runtime.stop(&instance).await;
        ledger.release(&spec.limits);
        registry.remove(replica_id);
        bridge.remove_replica(&service_key, replica_id).await;
        return Err(LifecycleError::Health);
    }

    // 6. Promote to Healthy and start taking traffic.
    registry.set_state(replica_id, ReplicaState::Healthy);
    bridge
        .set_replica_healthy(&service_key, replica_id, true)
        .await;
    Ok(replica_id)
}

/// Gracefully drain and tear down a replica: stop new connections, wait out a
/// bounded grace window, stop the runtime, then always release the ledger and
/// remove from the registry/bridge so resources never leak even if `stop` errs.
#[allow(clippy::too_many_arguments)]
pub async fn drain_replica(
    service_key: &str,
    replica_id: Uuid,
    instance: &RuntimeInstanceId,
    limits: &ResourceLimits,
    grace: Duration,
    registry: &mut ReplicaRegistry,
    ledger: &mut ResourceLedger,
    runtime: &dyn Runtime,
    bridge: &LoopbackBridgeSupervisor,
) -> Result<(), LifecycleError> {
    // 1. Stop directing new connections at this replica.
    registry.set_state(replica_id, ReplicaState::Draining);
    bridge
        .set_replica_healthy(service_key, replica_id, false)
        .await;

    // 2. Bounded drain window for in-flight connections.
    tokio::time::sleep(grace).await;

    // 3. Stop the runtime; deliberately ignore the error and still
    //    release/remove below so the reservation and pool entry are never
    //    leaked even when the runtime fails to stop.
    let _ = runtime.stop(instance).await;

    // 4. Always release and remove.
    ledger.release(limits);
    registry.remove(replica_id);
    bridge.remove_replica(service_key, replica_id).await;
    Ok(())
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

    fn spec() -> LaunchSpec {
        LaunchSpec {
            service_name: "web".to_string(),
            service_id: Uuid::now_v7(),
            deployment_id: Uuid::now_v7(),
            replica_index: 0,
            artifact: artifact(),
            internal_port: 8080,
            limits: ResourceLimits::default(),
            env: Vec::new(),
            health_check: HealthCheck::new("/healthz", 5),
        }
    }

    fn ledger() -> ResourceLedger {
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

    struct FailingHealthChecker;

    #[async_trait::async_trait]
    impl HealthChecker for FailingHealthChecker {
        async fn check(
            &self,
            _url: &str,
            _health: &HealthCheck,
        ) -> Result<(), crate::health::HealthError> {
            Err(crate::health::HealthError::Failed)
        }
    }

    #[tokio::test]
    async fn launch_marks_healthy_and_reserves() {
        let spec = spec();
        let mut registry = ReplicaRegistry::default();
        let mut ledger = ledger();
        let runtime = FakeRuntime::default();
        let bridge = LoopbackBridgeSupervisor::default();
        let health = FakeHealthChecker::healthy();

        let id = launch_replica(
            &spec,
            &mut registry,
            &mut ledger,
            &runtime,
            &bridge,
            &health,
        )
        .await
        .expect("launch ok");

        assert_eq!(registry.healthy_count(spec.service_id), 1);
        let replica = registry
            .replicas(spec.service_id)
            .iter()
            .find(|r| r.id == id)
            .expect("replica present");
        assert_eq!(replica.state, ReplicaState::Healthy);
        assert!(ledger.committed_cpu() > 0);
        assert_eq!(bridge.healthy_count(&spec.service_id.to_string()).await, 1);
        assert_eq!(runtime.started_requests().len(), 1);
    }

    #[tokio::test]
    async fn launch_health_fail_cleans_up() {
        let spec = spec();
        let mut registry = ReplicaRegistry::default();
        let mut ledger = ledger();
        let runtime = FakeRuntime::default();
        let bridge = LoopbackBridgeSupervisor::default();
        let health = FailingHealthChecker;

        let err = launch_replica(
            &spec,
            &mut registry,
            &mut ledger,
            &runtime,
            &bridge,
            &health,
        )
        .await
        .expect_err("health should fail");
        assert!(matches!(err, LifecycleError::Health));

        assert_eq!(ledger.committed_cpu(), 0);
        assert_eq!(ledger.committed_mem(), 0);
        assert_eq!(registry.replica_count(spec.service_id), 0);
        assert_eq!(bridge.healthy_count(&spec.service_name).await, 0);
        let stopped = runtime.stopped_instances();
        assert_eq!(stopped.len(), 1);
        assert_eq!(stopped[0].service_name, spec.service_name);
        assert_eq!(stopped[0].replica_index, spec.replica_index);
    }

    #[tokio::test]
    async fn launch_capacity_denied_does_not_start() {
        let spec = spec();
        let mut registry = ReplicaRegistry::default();
        // Allocatable exactly one replica's worth; pre-fill to capacity.
        let mut ledger = ResourceLedger::new(
            HostCapacity {
                cpu_millis: spec.limits.cpu_millis,
                mem_bytes: spec.limits.memory_bytes,
            },
            Headroom {
                cpu_millis: 0,
                mem_bytes: 0,
            },
        );
        ledger.try_reserve(&spec.limits).expect("prefill");

        let runtime = FakeRuntime::default();
        let bridge = LoopbackBridgeSupervisor::default();
        let health = FakeHealthChecker::healthy();

        let err = launch_replica(
            &spec,
            &mut registry,
            &mut ledger,
            &runtime,
            &bridge,
            &health,
        )
        .await
        .expect_err("capacity should be denied");
        assert!(matches!(err, LifecycleError::Capacity));

        assert_eq!(runtime.started_requests().len(), 0);
        assert_eq!(registry.replica_count(spec.service_id), 0);
    }

    #[tokio::test]
    async fn drain_removes_and_releases() {
        let spec = spec();
        let mut registry = ReplicaRegistry::default();
        let mut ledger = ledger();
        let runtime = FakeRuntime::default();
        let bridge = LoopbackBridgeSupervisor::default();
        let health = FakeHealthChecker::healthy();

        let id = launch_replica(
            &spec,
            &mut registry,
            &mut ledger,
            &runtime,
            &bridge,
            &health,
        )
        .await
        .expect("launch ok");
        assert!(ledger.committed_cpu() > 0);

        let instance = RuntimeInstanceId {
            service_id: spec.service_id,
            service_name: spec.service_name.clone(),
            replica_index: spec.replica_index,
        };
        drain_replica(
            &spec.service_id.to_string(),
            id,
            &instance,
            &spec.limits,
            Duration::ZERO,
            &mut registry,
            &mut ledger,
            &runtime,
            &bridge,
        )
        .await
        .expect("drain ok");

        assert_eq!(ledger.committed_cpu(), 0);
        assert_eq!(ledger.committed_mem(), 0);
        assert_eq!(registry.replica_count(spec.service_id), 0);
        assert_eq!(bridge.healthy_count(&spec.service_name).await, 0);
        let stopped = runtime.stopped_instances();
        assert!(stopped.iter().any(|i| i == &instance));
    }
}

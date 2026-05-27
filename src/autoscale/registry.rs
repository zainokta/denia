use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaState {
    Pending,
    Healthy,
    Draining,
    Stopped,
}

#[derive(Debug, Clone)]
pub struct Replica {
    pub id: Uuid,
    pub service_id: Uuid,
    pub deployment_id: Uuid,
    pub index: u32,
    pub socket_path: PathBuf,
    pub state: ReplicaState,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct ReplicaRegistry {
    by_service: BTreeMap<Uuid, Vec<Replica>>,
    cursor: BTreeMap<Uuid, usize>,
}

impl ReplicaRegistry {
    pub fn add(
        &mut self,
        service_id: Uuid,
        deployment_id: Uuid,
        index: u32,
        socket_path: PathBuf,
    ) -> Uuid {
        let id = Uuid::now_v7();
        self.by_service
            .entry(service_id)
            .or_default()
            .push(Replica {
                id,
                service_id,
                deployment_id,
                index,
                socket_path,
                state: ReplicaState::Pending,
                started_at: Utc::now(),
            });
        id
    }

    pub fn set_state(&mut self, replica_id: Uuid, state: ReplicaState) {
        for replicas in self.by_service.values_mut() {
            if let Some(r) = replicas.iter_mut().find(|r| r.id == replica_id) {
                r.state = state;
                return;
            }
        }
    }

    pub fn remove(&mut self, replica_id: Uuid) {
        for replicas in self.by_service.values_mut() {
            replicas.retain(|r| r.id != replica_id);
        }
    }

    pub fn healthy_count(&self, service_id: Uuid) -> usize {
        self.by_service
            .get(&service_id)
            .map(|rs| {
                rs.iter()
                    .filter(|r| r.state == ReplicaState::Healthy)
                    .count()
            })
            .unwrap_or(0)
    }

    pub fn replica_count(&self, service_id: Uuid) -> usize {
        self.by_service
            .get(&service_id)
            .map(|rs| rs.len())
            .unwrap_or(0)
    }

    pub fn replicas(&self, service_id: Uuid) -> &[Replica] {
        self.by_service
            .get(&service_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Round-robin over Healthy replicas of a service.
    pub fn next_healthy(&mut self, service_id: Uuid) -> Option<Replica> {
        let replicas = self.by_service.get(&service_id)?;
        let healthy: Vec<&Replica> = replicas
            .iter()
            .filter(|r| r.state == ReplicaState::Healthy)
            .collect();
        if healthy.is_empty() {
            return None;
        }
        let cur = self.cursor.entry(service_id).or_insert(0);
        let pick = healthy[*cur % healthy.len()].clone();
        *cur = cur.wrapping_add(1);
        Some(pick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn registry_round_robin_over_healthy_only() {
        let mut reg = ReplicaRegistry::default();
        let svc = Uuid::now_v7();
        let r1 = reg.add(svc, Uuid::now_v7(), 0, "/run/denia/s-0.sock".into());
        let r2 = reg.add(svc, Uuid::now_v7(), 1, "/run/denia/s-1.sock".into());
        reg.set_state(r1, ReplicaState::Healthy);
        reg.set_state(r2, ReplicaState::Draining);
        // only r1 is selectable
        assert_eq!(reg.next_healthy(svc).map(|r| r.id), Some(r1));
        assert_eq!(reg.next_healthy(svc).map(|r| r.id), Some(r1));
        assert_eq!(reg.healthy_count(svc), 1);
        // make both healthy -> round robin alternates
        reg.set_state(r2, ReplicaState::Healthy);
        let a = reg.next_healthy(svc).map(|r| r.id);
        let b = reg.next_healthy(svc).map(|r| r.id);
        assert_ne!(a, b);
        assert_eq!(reg.healthy_count(svc), 2);
    }
}

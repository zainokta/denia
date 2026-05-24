use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeTarget {
    pub service_name: String,
    pub port: u16,
    pub socket_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct BridgeAllocator {
    next_port: u16,
    targets: BTreeMap<String, BridgeTarget>,
}

impl BridgeAllocator {
    pub fn new(start_port: u16) -> Self {
        Self {
            next_port: start_port,
            targets: BTreeMap::new(),
        }
    }

    pub fn assign(&mut self, service_name: &str, socket_path: PathBuf) -> BridgeTarget {
        if let Some(existing) = self.targets.get(service_name) {
            return existing.clone();
        }
        let target = BridgeTarget {
            service_name: service_name.to_string(),
            port: self.next_port,
            socket_path,
        };
        self.next_port += 1;
        self.targets
            .insert(service_name.to_string(), target.clone());
        target
    }
}

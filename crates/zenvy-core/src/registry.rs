use std::collections::BTreeMap;
use std::sync::RwLock;
use tokio::sync::broadcast;
use zenvy_proto::{CapabilityRecord, RegistrySnapshot};

#[derive(Debug, Clone)]
pub struct CapabilityChange {
    pub id: String,
    pub record: CapabilityRecord,
}

/// Single source of truth for what works (PLAN.md §7.3). Changes are pushed, never polled.
pub struct Registry {
    inner: RwLock<BTreeMap<String, CapabilityRecord>>,
    tx: broadcast::Sender<CapabilityChange>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(BTreeMap::new()),
            tx: broadcast::channel(64).0,
        }
    }

    /// Stores a record; returns true (and notifies subscribers) only if it actually changed.
    pub fn set(&self, id: impl Into<String>, record: CapabilityRecord) -> bool {
        let id = id.into();
        {
            let mut map = self.inner.write().unwrap();
            if map.get(&id) == Some(&record) {
                return false;
            }
            map.insert(id.clone(), record.clone());
        }
        let _ = self.tx.send(CapabilityChange { id, record });
        true
    }

    pub fn get(&self, id: &str) -> Option<CapabilityRecord> {
        self.inner.read().unwrap().get(id).cloned()
    }

    pub fn snapshot(&self) -> RegistrySnapshot {
        RegistrySnapshot {
            schema_version: 1,
            capabilities: self.inner.read().unwrap().clone(),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CapabilityChange> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenvy_proto::CapState;

    #[test]
    fn only_real_changes_notify() {
        let r = Registry::new();
        let mut rx = r.subscribe();
        assert!(r.set("a", CapabilityRecord::available("x")));
        assert!(!r.set("a", CapabilityRecord::available("x")));
        assert!(r.set("a", CapabilityRecord::not(CapState::Error, "x", "boom")));
        assert_eq!(rx.try_recv().unwrap().record.state, CapState::Available);
        assert_eq!(rx.try_recv().unwrap().record.state, CapState::Error);
        assert!(rx.try_recv().is_err());
    }
}

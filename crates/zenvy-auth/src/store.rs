use crate::{b64, random_bytes, unb64};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

/// Scopes a newly paired device gets. `power`, `exec`, `files.read` must be granted explicitly.
pub const DEFAULT_SCOPES: &[&str] = &["view", "input", "windows", "system"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// base64url raw Ed25519 public key (32 bytes). A *public* key: safe to store.
    pub pubkey: String,
    pub scopes: Vec<String>,
    pub created_at: u64,
    pub last_seen: Option<u64>,
}

impl Device {
    pub fn pubkey_bytes(&self) -> Option<Vec<u8>> {
        unb64(&self.pubkey)
    }
    pub fn has_scope(&self, s: &str) -> bool {
        self.scopes.iter().any(|x| x == s)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("device limit reached ({0})")]
    Full(usize),
    #[error("invalid public key")]
    BadKey,
    #[error("that key is already paired")]
    Duplicate,
    #[error("storage error: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt device file: {0}")]
    Corrupt(#[from] serde_json::Error),
}

/// JSON file of paired devices, written atomically with mode 0600. Holds public keys only.
pub struct DeviceStore {
    path: PathBuf,
    devices: Vec<Device>,
    max: usize,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl DeviceStore {
    pub fn open(path: PathBuf, max: usize) -> Result<Self, StoreError> {
        let devices = match std::fs::read_to_string(&path) {
            Ok(t) => serde_json::from_str(&t)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e.into()),
        };
        Ok(Self { path, devices, max })
    }

    fn save(&self) -> Result<(), StoreError> {
        let tmp = self.path.with_extension("tmp");
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(serde_json::to_string_pretty(&self.devices)?.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(tmp, &self.path)?;
        Ok(())
    }

    pub fn add(
        &mut self,
        name: &str,
        pubkey: &[u8],
        scopes: &[&str],
    ) -> Result<Device, StoreError> {
        if pubkey.len() != 32 {
            return Err(StoreError::BadKey);
        }
        if self.devices.len() >= self.max {
            return Err(StoreError::Full(self.max));
        }
        let pk = b64(pubkey);
        if self.devices.iter().any(|d| d.pubkey == pk) {
            return Err(StoreError::Duplicate);
        }
        let name: String = name.chars().filter(|c| !c.is_control()).take(40).collect();
        let d = Device {
            id: b64(&random_bytes::<9>()), // 12 url-safe chars
            name: if name.trim().is_empty() {
                "phone".into()
            } else {
                name
            },
            pubkey: pk,
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            created_at: now_secs(),
            last_seen: None,
        };
        self.devices.push(d.clone());
        self.save()?;
        Ok(d)
    }

    pub fn get(&self, id: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    pub fn list(&self) -> &[Device] {
        &self.devices
    }

    pub fn touch(&mut self, id: &str) {
        if let Some(d) = self.devices.iter_mut().find(|d| d.id == id) {
            d.last_seen = Some(now_secs());
            let _ = self.save();
        }
    }

    /// Removes a device by id or unique id-prefix. Returns the removed device.
    pub fn revoke(&mut self, id_or_prefix: &str) -> Result<Option<Device>, StoreError> {
        let matches: Vec<usize> = self
            .devices
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                d.id == id_or_prefix || (id_or_prefix.len() >= 4 && d.id.starts_with(id_or_prefix))
            })
            .map(|(i, _)| i)
            .collect();
        if matches.len() != 1 {
            return Ok(None);
        }
        let d = self.devices.remove(matches[0]);
        self.save()?;
        Ok(Some(d))
    }

    /// Adds (`grant = true`) or removes a scope on one device (id or unique id prefix). Idempotent.
    /// Returns the updated device, or `None` if the id matches no single device.
    pub fn set_scope(
        &mut self,
        id_or_prefix: &str,
        scope: &str,
        grant: bool,
    ) -> Result<Option<Device>, StoreError> {
        let matches: Vec<usize> = self
            .devices
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                d.id == id_or_prefix || (id_or_prefix.len() >= 4 && d.id.starts_with(id_or_prefix))
            })
            .map(|(i, _)| i)
            .collect();
        if matches.len() != 1 {
            return Ok(None);
        }
        let d = &mut self.devices[matches[0]];
        if grant {
            if !d.scopes.iter().any(|s| s == scope) {
                d.scopes.push(scope.to_string());
            }
        } else {
            d.scopes.retain(|s| s != scope);
        }
        let out = d.clone();
        self.save()?;
        Ok(Some(out))
    }

    pub fn revoke_all(&mut self) -> Result<Vec<Device>, StoreError> {
        let all = std::mem::take(&mut self.devices);
        self.save()?;
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn store(max: usize) -> (tempfile::TempDir, DeviceStore) {
        let d = tempfile::tempdir().unwrap();
        let s = DeviceStore::open(d.path().join("devices.json"), max).unwrap();
        (d, s)
    }

    #[test]
    fn add_persist_reload_revoke() {
        let (d, mut s) = store(5);
        let dev = s.add("Pixel", &[1u8; 32], DEFAULT_SCOPES).unwrap();
        assert!(dev.has_scope("input") && !dev.has_scope("power"));
        let mode = std::fs::metadata(d.path().join("devices.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let s2 = DeviceStore::open(d.path().join("devices.json"), 5).unwrap();
        assert_eq!(s2.list().len(), 1);
        let mut s2 = s2;
        assert!(s2.revoke(&dev.id).unwrap().is_some());
        assert!(DeviceStore::open(d.path().join("devices.json"), 5)
            .unwrap()
            .list()
            .is_empty());
    }

    #[test]
    fn limits_and_validation() {
        let (_d, mut s) = store(1);
        assert!(matches!(
            s.add("x", &[1u8; 31], &[]),
            Err(StoreError::BadKey)
        ));
        s.add("a", &[1u8; 32], &[]).unwrap();
        assert!(matches!(
            s.add("b", &[2u8; 32], &[]),
            Err(StoreError::Full(1))
        ));
        let (_d, mut s) = store(5);
        s.add("a", &[1u8; 32], &[]).unwrap();
        assert!(matches!(
            s.add("a again", &[1u8; 32], &[]),
            Err(StoreError::Duplicate)
        ));
    }

    #[test]
    fn names_are_sanitised() {
        let (_d, mut s) = store(5);
        let d = s.add("ev\u{7}il\nname", &[3u8; 32], &[]).unwrap();
        assert_eq!(d.name, "evilname");
        assert_eq!(
            s.add(&"x".repeat(200), &[4u8; 32], &[]).unwrap().name.len(),
            40
        );
        assert_eq!(s.add("  ", &[5u8; 32], &[]).unwrap().name, "phone");
    }

    #[test]
    fn scopes_can_be_granted_and_removed_and_persist() {
        let (d, mut s) = store(5);
        let dev = s.add("a", &[1u8; 32], DEFAULT_SCOPES).unwrap();
        assert!(
            !dev.has_scope("listen"),
            "listening is never granted by default"
        );
        let g = s.set_scope(&dev.id, "listen", true).unwrap().unwrap();
        assert!(g.has_scope("listen") && g.scopes.len() == DEFAULT_SCOPES.len() + 1);
        assert_eq!(
            s.set_scope(&dev.id, "listen", true)
                .unwrap()
                .unwrap()
                .scopes
                .len(),
            g.scopes.len(),
            "granting twice changes nothing"
        );
        assert!(
            DeviceStore::open(d.path().join("devices.json"), 5)
                .unwrap()
                .get(&dev.id)
                .unwrap()
                .has_scope("listen"),
            "persisted"
        );
        assert!(
            !s.set_scope(&dev.id[..6], "listen", false)
                .unwrap()
                .unwrap()
                .has_scope("listen"),
            "prefix works, scope removed"
        );
        assert!(
            s.set_scope("zz", "listen", true).unwrap().is_none()
                && s.set_scope("nosuchdevice", "listen", true)
                    .unwrap()
                    .is_none()
        );
    }

    #[test]
    fn short_or_ambiguous_prefix_does_not_revoke() {
        let (_d, mut s) = store(5);
        let dev = s.add("a", &[1u8; 32], &[]).unwrap();
        assert!(s.revoke(&dev.id[..2]).unwrap().is_none());
        assert!(s.revoke(&dev.id[..6]).unwrap().is_some());
    }

    #[test]
    fn stores_no_private_material() {
        let (d, mut s) = store(5);
        s.add("a", &[9u8; 32], &[]).unwrap();
        let text = std::fs::read_to_string(d.path().join("devices.json")).unwrap();
        assert!(text.contains("pubkey") && !text.contains("secret") && !text.contains("private"));
    }
}

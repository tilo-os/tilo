//! Shell-owned priority-contact registry.
//!
//! Contacts publishes only its favourites through the notification service.
//! That service accepts replacement only from the launcher-verified Contacts
//! package. Raw endpoints are normalised and immediately converted to salted
//! BLAKE3 digests; the durable 0600 record never contains an email address or
//! phone number. Notification senders can only query a handle internally—the
//! registry is not mounted into application sandboxes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use serde::{Deserialize, Serialize};

const MAX_CONTACTS: usize = 2048;
const MAX_ENDPOINTS_PER_CONTACT: usize = 16;
const MAX_ENDPOINT_LEN: usize = 320;
const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactProjection {
    pub id: String,
    pub name: String,
    pub endpoints: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ContactRecord {
    id: String,
    name: String,
    digests: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DiskRecord {
    version: u8,
    salt: String,
    contacts: Vec<ContactRecord>,
}

impl DiskRecord {
    fn empty() -> Self {
        let mut salt = [0_u8; 32];
        UnwrapErr(SysRng).fill_bytes(&mut salt);
        Self {
            version: 1,
            salt: hex::encode(salt),
            contacts: Vec::new(),
        }
    }

    fn salt(&self) -> Option<[u8; 32]> {
        let bytes = hex::decode(&self.salt).ok()?;
        bytes.try_into().ok()
    }
}

fn normalise_endpoint(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() || value.len() > MAX_ENDPOINT_LEN || value.chars().any(char::is_control) {
        return None;
    }
    if value.contains('@') {
        Some(format!("mail:{}", value.to_lowercase()))
    } else {
        let phone: String = value
            .chars()
            .filter(|ch| ch.is_ascii_digit() || *ch == '+')
            .collect();
        (phone.chars().filter(char::is_ascii_digit).count() >= 5).then(|| format!("tel:{phone}"))
    }
}

fn digest(salt: &[u8; 32], endpoint: &str) -> String {
    let mut hasher = blake3::Hasher::new_keyed(salt);
    hasher.update(endpoint.as_bytes());
    hasher.finalize().to_hex().to_string()
}

#[derive(Clone, Debug)]
pub struct PriorityContactRegistry {
    inner: Arc<Mutex<DiskRecord>>,
    path: Option<PathBuf>,
}

impl PriorityContactRegistry {
    pub fn open_default() -> Self {
        Self::open(settings_store::config_path(std::path::Path::new(
            "priority-contacts.json",
        )))
    }

    pub fn in_memory() -> Self {
        Self::open(None)
    }

    fn open(path: Option<PathBuf>) -> Self {
        let record = path
            .as_deref()
            .and_then(|path| read_record(path).ok())
            .unwrap_or_else(DiskRecord::empty);
        Self {
            inner: Arc::new(Mutex::new(record)),
            path,
        }
    }

    /// Replace the whole projection. This is called only after the D-Bus
    /// service verifies `so.tilo.contacts`; keeping the check at that boundary
    /// means this storage type never tries to infer authority from a string.
    ///
    /// # Errors
    ///
    /// Returns an error if `contacts` exceeds the maximum count, if the
    /// registry's lock is poisoned, or if persisting the updated record
    /// fails.
    pub fn replace_verified(&self, contacts: Vec<ContactProjection>) -> Result<usize, String> {
        if contacts.len() > MAX_CONTACTS {
            return Err("too many priority contacts".into());
        }
        let mut guard = self.inner.lock().map_err(|_| "registry unavailable")?;
        let salt = guard.salt().unwrap_or_else(|| {
            let mut salt = [0_u8; 32];
            UnwrapErr(SysRng).fill_bytes(&mut salt);
            salt
        });
        let mut dedup = BTreeMap::<String, ContactRecord>::new();
        for contact in contacts {
            let id = contact.id.trim();
            if id.is_empty()
                || id.len() > 64
                || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                || contact.endpoints.len() > MAX_ENDPOINTS_PER_CONTACT
            {
                continue;
            }
            let mut digests: Vec<String> = contact
                .endpoints
                .iter()
                .filter_map(|value| normalise_endpoint(value))
                .map(|value| digest(&salt, &value))
                .collect();
            digests.sort();
            digests.dedup();
            if digests.is_empty() {
                continue;
            }
            let name: String = contact
                .name
                .trim()
                .chars()
                .filter(|ch| !ch.is_control())
                .take(80)
                .collect();
            dedup.insert(
                id.to_string(),
                ContactRecord {
                    id: id.to_string(),
                    name,
                    digests,
                },
            );
        }
        guard.version = 1;
        guard.salt = hex::encode(salt);
        guard.contacts = dedup.into_values().collect();
        if let Some(path) = &self.path {
            write_record(path, &guard).map_err(|error| error.to_string())?;
        }
        Ok(guard.contacts.len())
    }

    pub fn matches(&self, endpoint: &str) -> bool {
        let Some(endpoint) = normalise_endpoint(endpoint) else {
            return false;
        };
        let Ok(guard) = self.inner.lock() else {
            return false;
        };
        let Some(salt) = guard.salt() else {
            return false;
        };
        let want = digest(&salt, &endpoint);
        guard
            .contacts
            .iter()
            .any(|contact| contact.digests.iter().any(|candidate| candidate == &want))
    }

    pub fn count(&self) -> usize {
        self.inner.lock().map_or(0, |record| record.contacts.len())
    }
}

fn read_record(path: &Path) -> std::io::Result<DiskRecord> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.len() > MAX_RECORD_BYTES
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "priority-contact registry has unsafe ownership or mode",
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "priority-contact registry is too large",
        ));
    }
    let record: DiskRecord = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
    if record.version != 1
        || record.salt().is_none()
        || record.contacts.len() > MAX_CONTACTS
        || record.contacts.iter().any(|contact| {
            contact.id.len() > 64
                || contact.name.chars().count() > 80
                || contact.digests.len() > MAX_ENDPOINTS_PER_CONTACT
                || contact.digests.iter().any(|digest| {
                    digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit())
                })
        })
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "priority-contact registry has invalid bounds",
        ));
    }
    Ok(record)
}

fn write_record(path: &Path, record: &DiskRecord) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_contact_claims_are_normalised_bounded_and_never_persist_endpoints() {
        // Phase `raw_endpoint_is_never_persisted_and_matching_is_normalised`.
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("priority.json");
            let registry = PriorityContactRegistry::open(Some(path.clone()));
            registry
                .replace_verified(vec![ContactProjection {
                    id: "ca761232-ed42-11ce-bacd-00aa0057b223".into(),
                    name: "Ada".into(),
                    endpoints: vec![" Ada@Example.COM ".into(), "+49 123 456789".into()],
                }])
                .unwrap();
            let raw = std::fs::read_to_string(path).unwrap();
            assert!(!raw.contains("Ada@Example.COM"));
            assert!(!raw.contains("123 456789"));
            assert!(registry.matches("ada@example.com"));
            assert!(registry.matches("+49 (123) 456789"));
        }
        // Phase `malformed_and_oversized_claims_are_not_registered`.
        {
            let registry = PriorityContactRegistry::in_memory();
            let count = registry
                .replace_verified(vec![ContactProjection {
                    id: "../escape".into(),
                    name: "x".repeat(1000),
                    endpoints: vec!["a@example.com".into()],
                }])
                .unwrap();
            assert_eq!(count, 0);
        }
    }
}

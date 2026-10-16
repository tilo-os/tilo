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
        let salt = crate::random::bytes::<32>();
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
    /// Serialises replacements and their disk writes, so `inner` never waits
    /// for the disk.
    writer: Arc<Mutex<()>>,
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
            writer: Arc::new(Mutex::new(())),
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
        // Replacements take turns on `writer`, held across the disk write.
        // `inner` is what notifications match against on the compositor's
        // loop, so it is held only to read the salt and to install the new
        // record, never across the write's fsyncs.
        let turn = self.writer.lock().map_err(|_| "registry unavailable")?;
        let salt = self
            .inner
            .lock()
            .map_err(|_| "registry unavailable")?
            .salt()
            .unwrap_or_else(crate::random::bytes::<32>);
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
        let record = DiskRecord {
            version: 1,
            salt: hex::encode(salt),
            contacts: dedup.into_values().collect(),
        };
        let written = self
            .path
            .as_ref()
            .map_or(Ok(()), |path| write_record(path, &record));
        let count = record.contacts.len();
        // Installed even when the write failed, as before: the caller learns
        // that the projection did not persist.
        *self.inner.lock().map_err(|_| "registry unavailable")? = record;
        drop(turn);
        written.map_err(|error| error.to_string())?;
        Ok(count)
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
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.len() > MAX_RECORD_BYTES
        || !tilo_files::native::private::private_to_me(&metadata)
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

    /// Notifications match senders on the compositor's loop while Contacts
    /// may be replacing the projection; the replacement's disk write (whose
    /// fsyncs take seconds under I/O pressure) must not hold the lock that
    /// matching takes.
    #[test]
    fn matching_never_waits_for_a_replacement_being_written() {
        use std::sync::mpsc;
        use std::time::Duration;
        let contact = |id: &str, endpoint: &str| ContactProjection {
            id: id.into(),
            name: id.into(),
            endpoints: vec![endpoint.into()],
        };
        let temp = tempfile::tempdir().unwrap();
        let registry = PriorityContactRegistry::open(Some(temp.path().join("priority.json")));
        registry
            .replace_verified(vec![contact("ada", "ada@example.com")])
            .unwrap();

        let (writing_tx, writing) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let replacing = registry.clone();
        let replacement = std::thread::spawn(move || {
            let paused = std::cell::Cell::new(false);
            tilo_files::native::atomic::test_support::with_observer(
                move |_| {
                    // Hold the write at its first checkpoint, before any sync.
                    if !paused.replace(true) {
                        writing_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    }
                },
                || replacing.replace_verified(vec![contact("grace", "grace@example.com")]),
            )
        });
        writing.recv().unwrap();
        let (matched_tx, matched) = mpsc::channel();
        let matching = registry.clone();
        std::thread::spawn(move || {
            let _ = matched_tx.send(matching.matches("ada@example.com"));
        });
        let matched = matched.recv_timeout(Duration::from_secs(10));
        release.send(()).unwrap();
        assert!(
            matched.expect("matching waited for the replacement's disk write"),
            "the previous projection answers until the new one is installed"
        );
        assert_eq!(replacement.join().unwrap().unwrap(), 1);
        assert!(registry.matches("grace@example.com"));
        assert!(!registry.matches("ada@example.com"));
    }
}

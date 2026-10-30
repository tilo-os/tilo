//! The person's priority contacts: the people whose messages may break
//! through focus and Do Not Disturb.
//!
//! Contacts hands its favourites over through the daemon
//! (`XTiloReplacePriorityContacts`), accepted only from the verified
//! Contacts app, and the person marks and unmarks a contact through the
//! published resources. Endpoints are normalised and at once turned into
//! salted BLAKE3 digests: the private record (mode 0600) never holds an
//! email address or phone number. A notification's sender can only be
//! matched against it inside the daemon ([`PriorityContacts::matches`]);
//! nothing of it is handed to apps.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;

const MAX_CONTACTS: usize = 2048;
const MAX_ENDPOINTS_PER_CONTACT: usize = 16;
const MAX_ENDPOINT_LEN: usize = 320;
const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;

/// The app whose favourites are the priority contacts.
pub const CONTACTS_APP: &str = "so.tilo.contacts";

/// A contact as Contacts (or the person) names it: its id, its name and the
/// endpoints (email addresses, phone numbers) its messages come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactProjection {
    pub id: String,
    pub name: String,
    pub endpoints: Vec<String>,
}

/// One priority contact as the daemon publishes it: never its endpoints.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorityContact {
    pub id: String,
    pub name: String,
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
        Self {
            version: 1,
            salt: hex::encode(fresh_salt()),
            contacts: Vec::new(),
        }
    }

    fn salt(&self) -> Option<[u8; 32]> {
        let bytes = hex::decode(&self.salt).ok()?;
        bytes.try_into().ok()
    }
}

fn fresh_salt() -> [u8; 32] {
    let mut salt = [0; 32];
    if let Err(error) = getrandom::fill(&mut salt) {
        // Without the system's generator there is nothing better to salt
        // with; the digests still never hold an endpoint.
        tracing::warn!(target: "notifications", %error, "no randomness for the priority contacts' salt");
    }
    salt
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

/// `contact` as a record under `salt`; `None` when its id is malformed or
/// none of its endpoints is one.
fn record(salt: &[u8; 32], contact: &ContactProjection) -> Option<ContactRecord> {
    let id = contact.id.trim();
    if id.is_empty()
        || id.len() > 64
        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || contact.endpoints.len() > MAX_ENDPOINTS_PER_CONTACT
    {
        return None;
    }
    let mut digests: Vec<String> = contact
        .endpoints
        .iter()
        .filter_map(|value| normalise_endpoint(value))
        .map(|value| digest(salt, &value))
        .collect();
    digests.sort();
    digests.dedup();
    if digests.is_empty() {
        return None;
    }
    let name: String = contact
        .name
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .take(80)
        .collect();
    Some(ContactRecord {
        id: id.to_string(),
        name,
        digests,
    })
}

/// The priority contacts, shared by the daemon's D-Bus face and its
/// resources. Cloning shares it.
#[derive(Clone, Debug)]
pub struct PriorityContacts {
    inner: Arc<Mutex<DiskRecord>>,
    /// Serialises changes and their disk writes, so `inner`, which every
    /// arriving notification is matched against, never waits for the disk.
    writer: Arc<Mutex<()>>,
    path: Option<PathBuf>,
}

impl PriorityContacts {
    /// The person's priority contacts, kept in their configuration.
    #[must_use]
    pub fn open_default() -> Self {
        Self::open(
            tilo_paths::xdg::config_home().map(|root| root.join("tilo/priority-contacts.json")),
        )
    }

    /// Priority contacts kept in memory only.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::open(None)
    }

    /// Priority contacts kept at `path`, when there is one. A record that
    /// is unsafe (not the person's own, too large, malformed) is not
    /// trusted: the contacts start empty.
    #[must_use]
    pub fn open(path: Option<PathBuf>) -> Self {
        let record = path
            .as_deref()
            .and_then(|path| {
                read_record(path)
                    .inspect_err(|error| {
                        if error.kind() != std::io::ErrorKind::NotFound {
                            tracing::warn!(target: "notifications", %error, "priority contacts unreadable");
                        }
                    })
                    .ok()
            })
            .unwrap_or_else(DiskRecord::empty);
        Self {
            inner: Arc::new(Mutex::new(record)),
            writer: Arc::new(Mutex::new(())),
            path,
        }
    }

    /// Replace every contact with `contacts`: what Contacts hands over.
    /// Only call once the caller is verified as [`CONTACTS_APP`]; this
    /// store never infers authority from a string. How many it keeps.
    ///
    /// # Errors
    /// Too many contacts, or the record could not be written (the new
    /// contacts are in force all the same).
    pub fn replace_verified(&self, contacts: &[ContactProjection]) -> Result<usize, String> {
        if contacts.len() > MAX_CONTACTS {
            return Err("too many priority contacts".into());
        }
        self.change(|salt, _| {
            let mut dedup = BTreeMap::<String, ContactRecord>::new();
            for contact in contacts {
                if let Some(record) = record(salt, contact) {
                    dedup.insert(record.id.clone(), record);
                }
            }
            Ok(dedup.into_values().collect())
        })
    }

    /// Mark `contact` a priority contact, or replace the one with its id.
    /// How many there are now.
    ///
    /// # Errors
    /// The contact names no valid id or endpoint, there are too many, or
    /// the record could not be written.
    pub fn mark(&self, contact: &ContactProjection) -> Result<usize, String> {
        self.change(|salt, current| {
            let record = record(salt, contact).ok_or_else(|| {
                "a priority contact needs an id and an email address or phone number".to_owned()
            })?;
            let mut contacts: Vec<ContactRecord> = current
                .iter()
                .filter(|existing| existing.id != record.id)
                .cloned()
                .collect();
            if contacts.len() >= MAX_CONTACTS {
                return Err("too many priority contacts".into());
            }
            contacts.push(record);
            contacts.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(contacts)
        })
    }

    /// Unmark contact `id`. Whether it was one.
    ///
    /// # Errors
    /// The record could not be written.
    pub fn unmark(&self, id: &str) -> Result<bool, String> {
        let before = self.count();
        let after = self.change(|_, current| {
            Ok(current
                .iter()
                .filter(|existing| existing.id != id)
                .cloned()
                .collect())
        })?;
        Ok(after < before)
    }

    /// Install what `next` makes of the current contacts, under the
    /// record's salt, and write it.
    fn change(
        &self,
        next: impl FnOnce(&[u8; 32], &[ContactRecord]) -> Result<Vec<ContactRecord>, String>,
    ) -> Result<usize, String> {
        // Changes take turns on `writer`, held across the disk write;
        // `inner` is held only to read and to install, never across the
        // write's fsyncs.
        let turn = self.writer.lock_recover();
        let (salt, current) = {
            let inner = self.inner.lock_recover();
            (
                inner.salt().unwrap_or_else(fresh_salt),
                inner.contacts.clone(),
            )
        };
        let record = DiskRecord {
            version: 1,
            salt: hex::encode(salt),
            contacts: next(&salt, &current)?,
        };
        let written = self
            .path
            .as_ref()
            .map_or(Ok(()), |path| write_record(path, &record));
        let count = record.contacts.len();
        // Installed even when the write failed: the caller learns that it
        // did not persist.
        *self.inner.lock_recover() = record;
        drop(turn);
        written.map_err(|error| error.to_string())?;
        Ok(count)
    }

    /// Whether `endpoint` is one of a priority contact's.
    #[must_use]
    pub fn matches(&self, endpoint: &str) -> bool {
        let Some(endpoint) = normalise_endpoint(endpoint) else {
            return false;
        };
        let guard = self.inner.lock_recover();
        let Some(salt) = guard.salt() else {
            return false;
        };
        let want = digest(&salt, &endpoint);
        guard
            .contacts
            .iter()
            .any(|contact| contact.digests.iter().any(|candidate| candidate == &want))
    }

    /// The priority contacts, by id.
    #[must_use]
    pub fn contacts(&self) -> Vec<PriorityContact> {
        self.inner
            .lock_recover()
            .contacts
            .iter()
            .map(|contact| PriorityContact {
                id: contact.id.clone(),
                name: contact.name.clone(),
            })
            .collect()
    }

    /// How many there are.
    #[must_use]
    pub fn count(&self) -> usize {
        self.inner.lock_recover().contacts.len()
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
#[allow(clippy::unwrap_used, reason = "a fixture that fails fails the test")]
mod tests {
    use super::*;

    fn contact(id: &str, endpoint: &str) -> ContactProjection {
        ContactProjection {
            id: id.into(),
            name: id.into(),
            endpoints: vec![endpoint.into()],
        }
    }

    #[test]
    fn priority_contact_claims_are_normalised_bounded_and_never_persist_endpoints() {
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("priority.json");
            let registry = PriorityContacts::open(Some(path.clone()));
            registry
                .replace_verified(&[ContactProjection {
                    id: "ca761232-ed42-11ce-bacd-00aa0057b223".into(),
                    name: "Ada".into(),
                    endpoints: vec![" Ada@Example.COM ".into(), "+49 123 456789".into()],
                }])
                .unwrap();
            let raw = std::fs::read_to_string(&path).unwrap();
            assert!(!raw.contains("Ada@Example.COM"));
            assert!(!raw.contains("123 456789"));
            assert!(registry.matches("ada@example.com"));
            assert!(registry.matches("+49 (123) 456789"));
            // And the record is the person's own, read back on restart.
            let reopened = PriorityContacts::open(Some(path));
            assert!(reopened.matches("ada@example.com"));
        }
        {
            let registry = PriorityContacts::in_memory();
            let count = registry
                .replace_verified(&[ContactProjection {
                    id: "../escape".into(),
                    name: "x".repeat(1000),
                    endpoints: vec!["a@example.com".into()],
                }])
                .unwrap();
            assert_eq!(count, 0);
        }
    }

    #[test]
    fn a_contact_is_marked_and_unmarked_by_id() {
        let registry = PriorityContacts::in_memory();
        assert_eq!(registry.mark(&contact("ada", "ada@example.com")), Ok(1));
        assert_eq!(registry.mark(&contact("grace", "grace@example.com")), Ok(2));
        // Marking again replaces it.
        assert_eq!(registry.mark(&contact("ada", "+49 30 1234567")), Ok(2));
        assert!(!registry.matches("ada@example.com"));
        assert!(registry.matches("+49301234567"));
        assert_eq!(
            registry.contacts(),
            [
                PriorityContact {
                    id: "ada".into(),
                    name: "ada".into()
                },
                PriorityContact {
                    id: "grace".into(),
                    name: "grace".into()
                }
            ]
        );
        assert_eq!(registry.unmark("ada"), Ok(true));
        assert_eq!(registry.unmark("ada"), Ok(false));
        assert!(!registry.matches("+49301234567"));
        assert!(
            registry
                .mark(&contact("nobody", "not an endpoint"))
                .is_err()
        );
    }

    /// Notifications are matched as they arrive while Contacts may be
    /// replacing the projection; the replacement's disk write (whose fsyncs
    /// take seconds under I/O pressure) must not hold the lock that
    /// matching takes.
    #[test]
    fn matching_never_waits_for_a_replacement_being_written() {
        use std::sync::mpsc;
        let temp = tempfile::tempdir().unwrap();
        let registry = PriorityContacts::open(Some(temp.path().join("priority.json")));
        registry
            .replace_verified(&[contact("ada", "ada@example.com")])
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
                || replacing.replace_verified(&[contact("grace", "grace@example.com")]),
            )
        });
        writing.recv().unwrap();
        let (matched_tx, matched) = mpsc::channel();
        let matching = registry.clone();
        std::thread::spawn(move || {
            let _ = matched_tx.send(matching.matches("ada@example.com"));
        });
        let matched = matched.recv_timeout(tilo_test_env::hang_bound());
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

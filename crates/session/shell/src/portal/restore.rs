//! Standard `restore_data` for the portals that persist a user's consent
//! (`ScreenCast`, `RemoteDesktop`): the `(suv)` wire value and the private
//! store its opaque key resolves through.
//!
//! Durable mode-2 records deliberately live outside the replicated workspace
//! document: an opaque restore payload is an authority secret for one local
//! application installation, never a collaboration record.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;
use zbus::zvariant::{OwnedValue, Value};

const MAX_RESTORE_RECORDS: usize = 256;
const RESTORE_VENDOR: &str = "tilo";
const RESTORE_DATA_VERSION: u32 = 1;

/// What a portal's restore key maps back to.
pub(super) trait RestoreRecord: Clone + Serialize + DeserializeOwned {
    fn app_id(&self) -> &str;
    /// 1 while the app runs, 2 until revoked (and so durable).
    fn persist_mode(&self) -> u32;
    fn issued_unix_seconds(&self) -> u64;
    /// Whether a durable record read back from disk grants only what the
    /// portal can grant.
    fn grant_in_range(&self) -> bool;
}

pub(super) struct RestoreStore<R> {
    memory: HashMap<String, R>,
    durable: HashMap<String, R>,
    path: Option<PathBuf>,
}

impl<R> Default for RestoreStore<R> {
    fn default() -> Self {
        Self {
            memory: HashMap::new(),
            durable: HashMap::new(),
            path: None,
        }
    }
}

impl<R: RestoreRecord> RestoreStore<R> {
    /// The store in the private portal state directory under `file`.
    pub(super) fn load(file: &str) -> Self {
        Self::load_at(tilo_paths::state_dir().map(|root| root.join("tilo/portal").join(file)))
    }

    pub(super) fn load_at(path: Option<PathBuf>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        let durable = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<HashMap<String, R>>(&bytes)
                .ok()
                .filter(valid_durable)
                .unwrap_or_else(|| {
                    // A corrupt restore record must never become a grant. Keep
                    // forensic evidence rather than overwriting it in place.
                    let quarantine =
                        path.with_extension(format!("corrupt-{}", uuid::Uuid::new_v4()));
                    let _ = std::fs::rename(&path, quarantine);
                    HashMap::new()
                }),
            Err(_) => HashMap::new(),
        };
        Self {
            memory: HashMap::new(),
            durable,
            path: Some(path),
        }
    }

    pub(super) fn get(&self, key: &str) -> Option<R> {
        self.memory
            .get(key)
            .or_else(|| self.durable.get(key))
            .cloned()
    }

    pub(super) fn insert(&mut self, key: String, record: R) -> Result<(), String> {
        let target = if record.persist_mode() == 2 {
            &mut self.durable
        } else {
            &mut self.memory
        };
        target.insert(key, record);
        while target.len() > MAX_RESTORE_RECORDS {
            let Some(oldest) = target
                .iter()
                .min_by_key(|(_, record)| record.issued_unix_seconds())
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            target.remove(&oldest);
        }
        self.persist_durable()
    }

    #[cfg(test)]
    pub(super) fn durable_is_empty(&self) -> bool {
        self.durable.is_empty()
    }

    fn persist_durable(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            // Mode 1 is still useful in an ephemeral/CI session.  Mode 2
            // cannot truthfully survive a restart without private state.
            return if self.durable.is_empty() {
                Ok(())
            } else {
                Err("private portal state directory is unavailable".into())
            };
        };
        let bytes = serde_json::to_vec(&self.durable)
            .map_err(|error| format!("serialise restore records: {error}"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create restore directory: {error}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                    .map_err(|error| format!("secure restore directory: {error}"))?;
            }
        }
        tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
            .map_err(|error| format!("write restore records: {error}"))
    }
}

fn valid_durable<R: RestoreRecord>(records: &HashMap<String, R>) -> bool {
    records.len() <= MAX_RESTORE_RECORDS
        && records.iter().all(|(key, record)| {
            uuid::Uuid::parse_str(key).is_ok()
                && !record.app_id().is_empty()
                && record.app_id().len() <= 256
                && record.persist_mode() == 2
                && record.grant_in_range()
        })
}

/// Decode Tilo's opaque key from standard `restore_data` `(suv)`. Unknown
/// vendors, versions, malformed variants, and non-UUID payloads are not an
/// error: the portal spec says a backend may simply prompt normally when it
/// cannot restore a record.
pub(super) fn restore_key(options: &HashMap<String, OwnedValue>) -> Option<String> {
    let raw = options.get("restore_data")?.try_clone().ok()?;
    let (vendor, version, payload): (String, u32, OwnedValue) = raw.try_into().ok()?;
    if vendor != RESTORE_VENDOR || version != RESTORE_DATA_VERSION {
        return None;
    }
    let key = Value::from(payload).downcast::<String>().ok()?;
    uuid::Uuid::parse_str(&key).ok().map(|_| key)
}

/// Build a standard `(suv)` `restore_data` value. The variant payload is the
/// random key understood only by this backend's private store.
pub(super) fn restore_data(key: String) -> Option<OwnedValue> {
    // `OwnedValue`'s D-Bus type is `v`, so the tuple is exactly `(suv)`;
    // the opaque payload itself is a normal string inside that variant.
    let payload = OwnedValue::from(zbus::zvariant::Str::from(key));
    OwnedValue::try_from(Value::new((RESTORE_VENDOR, RESTORE_DATA_VERSION, payload))).ok()
}

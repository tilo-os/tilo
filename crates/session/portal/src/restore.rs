//! Remembered shares: the standard `restore_data` of `ScreenCast` and
//! `RemoteDesktop`, the `(suv)` value an app keeps to share the same
//! sources again without asking, and the private store its opaque key
//! resolves through.
//!
//! The key is random: an app cannot forge a grant by making up its
//! fields. A grant kept until revoked (persist mode 2) lives in a private
//! file on this device only, never in a workspace: it is one installed
//! app's authority, not a collaboration record. Each grant is published
//! so the person sees what is remembered and may forget it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::{ActionOutput, DoorError, ResourcePresentation, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_resource::Effect;
use tilo_service::{Started, decode, offer};
use zbus::zvariant::{OwnedValue, Value as Variant};

use crate::streams::{CaptureSource, cursor_mode};

const MAX_GRANTS: usize = 256;
const MAX_SOURCES: usize = 16;
const VENDOR: &str = "tilo";
const VERSION: u32 = 1;
const GRANT_PREFIX: &str = "portal:restore:";

/// What a remembered share offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.portal.restore")]
pub enum GrantEffect {
    /// Forget the remembered share
    #[effect(own_device, idempotent)]
    Forget,
}

/// What a restore key grants again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// Names its published resource; never the key itself.
    pub id: String,
    pub app_id: String,
    /// 1 while the app runs, 2 until revoked.
    pub persist_mode: u32,
    pub issued_unix_seconds: u64,
    /// The outputs and windows it shares.
    pub sources: Vec<CaptureSource>,
    pub cursor_mode: u32,
    /// The input devices it may drive (`RemoteDesktop`), else 0.
    pub devices: u32,
}

impl Grant {
    fn in_range(&self) -> bool {
        !self.app_id.is_empty()
            && self.app_id.len() <= 256
            && self.sources.len() <= MAX_SOURCES
            && self.cursor_mode & !cursor_mode::ALL == 0
            && self.devices & !crate::remote_desktop::device_type::ALL == 0
            && (!self.sources.is_empty() || self.devices != 0)
    }
}

#[derive(Default)]
struct State {
    memory: HashMap<String, Grant>,
    durable: HashMap<String, Grant>,
}

/// The remembered shares, by restore key.
#[derive(Clone, Default)]
pub struct Grants {
    state: Arc<Mutex<State>>,
    path: Option<Arc<PathBuf>>,
}

impl Grants {
    /// Grants kept until revoked at `path` (only for this session without
    /// one).
    #[must_use]
    pub fn load(path: Option<PathBuf>) -> Self {
        let durable = path.as_deref().map(load_durable).unwrap_or_default();
        Self {
            state: Arc::new(Mutex::new(State {
                memory: HashMap::new(),
                durable,
            })),
            path: path.map(Arc::new),
        }
    }

    /// The grant restore key `key` names, for `app_id` only.
    #[must_use]
    pub fn get(&self, key: &str, app_id: &str) -> Option<Grant> {
        let state = self.state.lock_recover();
        state
            .memory
            .get(key)
            .or_else(|| state.durable.get(key))
            .filter(|grant| grant.app_id == app_id)
            .cloned()
    }

    /// Remember `grant`; the restore key it is handed back under.
    ///
    /// # Errors
    /// A grant until revoked could not be written.
    pub fn insert(&self, mut grant: Grant) -> Result<String, String> {
        let key = uuid::Uuid::new_v4().to_string();
        grant.id = uuid::Uuid::new_v4().simple().to_string();
        let durable = grant.persist_mode == 2;
        let mut state = self.state.lock_recover();
        let target = if durable {
            &mut state.durable
        } else {
            &mut state.memory
        };
        target.insert(key.clone(), grant);
        while target.len() > MAX_GRANTS {
            let Some(oldest) = target
                .iter()
                .min_by_key(|(_, grant)| grant.issued_unix_seconds)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            target.remove(&oldest);
        }
        let kept = durable.then(|| state.durable.clone());
        drop(state);
        if let Some(kept) = kept
            && let Err(error) = self.persist(&kept)
        {
            self.state.lock_recover().durable.remove(&key);
            return Err(error);
        }
        Ok(key)
    }

    /// Forget grant `id`; `false` when there is none.
    pub fn forget(&self, id: &str) -> bool {
        let mut state = self.state.lock_recover();
        let before = (state.memory.len(), state.durable.len());
        state.memory.retain(|_, grant| grant.id != id);
        state.durable.retain(|_, grant| grant.id != id);
        let durable_changed = before.1 != state.durable.len();
        let forgotten = before != (state.memory.len(), state.durable.len());
        let kept = durable_changed.then(|| state.durable.clone());
        drop(state);
        if let Some(kept) = kept
            && let Err(error) = self.persist(&kept)
        {
            tracing::warn!(%error, "a forgotten share is still on disk");
        }
        forgotten
    }

    /// Forget what an app kept while it ran: it stopped running.
    pub fn app_exited(&self, app_id: &str) {
        self.state
            .lock_recover()
            .memory
            .retain(|_, grant| grant.app_id != app_id);
    }

    fn all(&self) -> Vec<Grant> {
        let state = self.state.lock_recover();
        let mut grants: Vec<Grant> = state
            .memory
            .values()
            .chain(state.durable.values())
            .cloned()
            .collect();
        drop(state);
        grants.sort_by(|a, b| a.id.cmp(&b.id));
        grants
    }

    fn persist(&self, durable: &HashMap<String, Grant>) -> Result<(), String> {
        let Some(path) = &self.path else {
            return if durable.is_empty() {
                Ok(())
            } else {
                Err("no private state directory to keep the share in".into())
            };
        };
        tilo_files::native::atomic::json::write_private(path, durable)
            .map_err(|error| format!("remembered shares not kept: {error}"))
    }
}

impl crate::Published for Grants {
    fn resources(&self) -> Vec<ResourceSpec> {
        self.all()
            .into_iter()
            .map(|grant| {
                let detail = grant
                    .sources
                    .iter()
                    .map(|source| source.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                offer::<GrantEffect>(ResourceSpec::new(
                    format!("{GRANT_PREFIX}{}", grant.id),
                    "portal.restore",
                    grant.app_id,
                ))
                .with_presentation(ResourcePresentation {
                    detail: (!detail.is_empty()).then_some(detail),
                    badge: None,
                    preview: None,
                })
            })
            .collect()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        let id = resource.strip_prefix(GRANT_PREFIX)?;
        self.all()
            .iter()
            .find(|grant| grant.id == id)
            .map(reading_of)
    }

    fn invoke(
        &self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Option<Result<Started, DoorError>> {
        let id = resource.strip_prefix(GRANT_PREFIX)?;
        Some(
            decode::<GrantEffect>(published, resource, action, arguments).and_then(|_| {
                self.forget(id)
                    .then(|| Started::Done(ActionOutput::default()))
                    .ok_or_else(|| {
                        DoorError::Unknown(tilo_actions::ActionAddress::new(resource, action))
                    })
            }),
        )
    }
}

fn reading_of(grant: &Grant) -> Value {
    serde_json::json!({
        "app_id": grant.app_id,
        "until_revoked": grant.persist_mode == 2,
        "sources": grant.sources,
        "devices": grant.devices,
    })
}

/// The grants kept at `path`. A file that does not hold only valid grants
/// never becomes one: it is moved aside for diagnosis and nothing is
/// granted.
fn load_durable(path: &std::path::Path) -> HashMap<String, Grant> {
    let Ok(bytes) = std::fs::read(path) else {
        return HashMap::new();
    };
    serde_json::from_slice::<HashMap<String, Grant>>(&bytes)
        .ok()
        .filter(valid_durable)
        .unwrap_or_else(|| {
            let aside = path.with_extension(format!("corrupt-{}", uuid::Uuid::new_v4().simple()));
            let _ = std::fs::rename(path, aside);
            HashMap::new()
        })
}

fn valid_durable(grants: &HashMap<String, Grant>) -> bool {
    grants.len() <= MAX_GRANTS
        && grants.iter().all(|(key, grant)| {
            uuid::Uuid::parse_str(key).is_ok() && grant.persist_mode == 2 && grant.in_range()
        })
}

/// The restore key standard `restore_data` carries, when it is Tilo's.
/// Anything else is no error: the backend then simply asks again.
#[must_use]
pub fn restore_key<S: std::hash::BuildHasher>(
    options: &HashMap<String, OwnedValue, S>,
) -> Option<String> {
    let raw = options.get("restore_data")?.try_clone().ok()?;
    let (vendor, version, payload): (String, u32, OwnedValue) = raw.try_into().ok()?;
    if vendor != VENDOR || version != VERSION {
        return None;
    }
    let key = Variant::from(payload).downcast::<String>().ok()?;
    uuid::Uuid::parse_str(&key).ok().map(|_| key)
}

/// Standard `(suv)` `restore_data` carrying restore key `key`.
#[must_use]
pub fn restore_data(key: String) -> Option<OwnedValue> {
    let payload = OwnedValue::from(zbus::zvariant::Str::from(key));
    OwnedValue::try_from(Variant::new((VENDOR, VERSION, payload))).ok()
}

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Published as _;
    use crate::streams::SourceKind;

    fn grant(persist_mode: u32) -> Grant {
        Grant {
            id: String::new(),
            app_id: "org.example.Share".into(),
            persist_mode,
            issued_unix_seconds: 42,
            sources: vec![CaptureSource {
                kind: SourceKind::Output,
                id: "DP-1".into(),
                app_id: None,
                label: "Desk".into(),
            }],
            cursor_mode: cursor_mode::EMBEDDED,
            devices: 0,
        }
    }

    #[test]
    fn restore_data_is_a_standard_vendor_tuple() {
        let key = uuid::Uuid::new_v4().to_string();
        let mut options = HashMap::new();
        options.insert(
            "restore_data".into(),
            restore_data(key.clone()).expect("data"),
        );
        let (vendor, version, payload): (String, u32, OwnedValue) = options["restore_data"]
            .try_clone()
            .expect("clone")
            .try_into()
            .expect("(suv)");
        assert_eq!((vendor.as_str(), version), ("tilo", 1));
        assert_eq!(Variant::from(payload).downcast::<String>().expect("s"), key);
        assert_eq!(restore_key(&options).as_deref(), Some(key.as_str()));
        options.insert(
            "restore_data".into(),
            OwnedValue::from(zbus::zvariant::Str::from("not-a-tuple")),
        );
        assert_eq!(restore_key(&options), None);
    }

    #[test]
    fn grants_are_app_bound_durable_and_forgettable() {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("restore.json");
        let grants = Grants::load(Some(path.clone()));
        let key = grants.insert(grant(2)).expect("kept");
        let transient = grants.insert(grant(1)).expect("kept");
        assert!(grants.get(&key, "org.example.Other").is_none(), "app-bound");
        let restarted = Grants::load(Some(path.clone()));
        let kept = restarted.get(&key, "org.example.Share").expect("survives");
        assert_eq!(kept.sources[0].id, "DP-1");
        assert!(restarted.get(&transient, "org.example.Share").is_none());
        assert_eq!(restarted.resources().len(), 1);
        assert!(restarted.forget(&kept.id));
        assert!(
            Grants::load(Some(path))
                .get(&key, "org.example.Share")
                .is_none()
        );
        grants.app_exited("org.example.Share");
        assert!(grants.get(&transient, "org.example.Share").is_none());
    }

    #[test]
    fn a_corrupt_store_grants_nothing_and_is_kept_aside() {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("restore.json");
        std::fs::write(&path, b"not json").expect("fixture");
        assert!(Grants::load(Some(path.clone())).all().is_empty());
        assert!(!path.exists());
        assert!(
            std::fs::read_dir(dir.path())
                .expect("entries")
                .any(|entry| entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with("restore.corrupt-"))
        );
    }

    #[test]
    fn without_private_state_only_transient_grants_are_given() {
        let grants = Grants::load(None);
        assert!(grants.insert(grant(1)).is_ok());
        assert!(grants.insert(grant(2)).is_err());
    }
}

//! Remembered shares: the standard `restore_data` of `ScreenCast` and
//! `RemoteDesktop`, the `(suv)` value an app keeps to share the same
//! sources again without asking, and the store its opaque key resolves
//! through.
//!
//! The key is random: an app cannot forge a grant by making up its
//! fields. A grant lasts only until the app quits or the session ends
//! (an app asking to be remembered until revoked gets no more) and holds
//! only in the workspace it was given in: after a restart or from
//! another workspace the person is asked again. Each grant is published
//! so the person sees what is remembered and may forget it, and it stands
//! only while the app may still use what it shares: taking back the app's
//! screen or input answer forgets it too ([`crate::Portal::end_captures`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tilo_actions::{ActionOutput, ApiError, ResourcePresentation, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_resource::Effect;
use tilo_service::{Started, decode, offer};
use zbus::zvariant::{OwnedValue, Value as Variant};

use crate::PortalCaller;
use crate::streams::CaptureSource;

const MAX_GRANTS: usize = 256;
const VENDOR: &str = "tilo";
const VERSION: u32 = 1;
const GRANT_PREFIX: &str = "portal:restore:";

/// One remembered share, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "portal.restore", label = "Remembered share")]
pub struct GrantRow;

/// What a remembered share offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.portal.restore")]
pub enum GrantEffect {
    /// Forget the remembered share
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Forget,
}

/// What a restore key grants again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    /// The restore key the app holds; never published.
    pub(crate) key: String,
    /// Names its published resource.
    pub id: String,
    pub app_id: String,
    /// The workspace the app shared from.
    pub workspace: Option<String>,
    /// The outputs and windows it shares.
    pub sources: Vec<CaptureSource>,
    pub cursor_mode: u32,
    /// The input devices it may drive (`RemoteDesktop`), else 0.
    pub devices: u32,
}

/// The remembered shares of open apps, oldest first.
#[derive(Clone, Default)]
pub struct Grants {
    grants: Arc<Mutex<Vec<Grant>>>,
}

impl Grants {
    /// The grant restore key `key` names, for `caller`'s app in its
    /// workspace only.
    #[must_use]
    pub fn get(&self, key: &str, caller: &PortalCaller) -> Option<Grant> {
        self.grants
            .lock_recover()
            .iter()
            .find(|grant| {
                grant.key == key
                    && grant.app_id == caller.app_id
                    && grant.workspace == caller.workspace
            })
            .cloned()
    }

    /// Remember `grant`; the restore key it is handed back under.
    pub fn insert(&self, mut grant: Grant) -> String {
        grant.key = uuid::Uuid::new_v4().to_string();
        grant.id = uuid::Uuid::new_v4().simple().to_string();
        let key = grant.key.clone();
        let mut grants = self.grants.lock_recover();
        grants.push(grant);
        let over = grants.len().saturating_sub(MAX_GRANTS);
        grants.drain(..over);
        key
    }

    /// Forget grant `id`; `false` when there is none.
    pub fn forget(&self, id: &str) -> bool {
        let mut grants = self.grants.lock_recover();
        let before = grants.len();
        grants.retain(|grant| grant.id != id);
        before != grants.len()
    }

    /// Forget every grant `ended` says no longer stands; whether any was.
    pub fn forget_ended(&self, ended: impl Fn(&Grant) -> bool) -> bool {
        let mut grants = self.grants.lock_recover();
        let before = grants.len();
        grants.retain(|grant| !ended(grant));
        before != grants.len()
    }

    /// Forget what an app kept: it quit.
    pub fn app_exited(&self, app_id: &str) {
        self.grants
            .lock_recover()
            .retain(|grant| grant.app_id != app_id);
    }

    fn all(&self) -> Vec<Grant> {
        let mut grants = self.grants.lock_recover().clone();
        grants.sort_by(|a, b| a.id.cmp(&b.id));
        grants
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
                offer::<GrantEffect>(tilo_resource::typed_resource::<GrantRow>(
                    format!("{GRANT_PREFIX}{}", grant.id),
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
    ) -> Option<Result<Started, ApiError>> {
        let id = resource.strip_prefix(GRANT_PREFIX)?;
        Some(
            decode::<GrantEffect>(published, resource, action, arguments).and_then(|_| {
                self.forget(id)
                    .then(|| Started::Done(ActionOutput::default()))
                    .ok_or_else(|| {
                        ApiError::Unknown(tilo_actions::ActionAddress::new(resource, action))
                    })
            }),
        )
    }
}

fn reading_of(grant: &Grant) -> Value {
    serde_json::json!({
        "app_id": grant.app_id,
        "sources": grant.sources,
        "devices": grant.devices,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Published as _;
    use crate::streams::SourceKind;

    fn grant() -> Grant {
        Grant {
            key: String::new(),
            id: String::new(),
            app_id: "org.example.Share".into(),
            workspace: Some("work".into()),
            sources: vec![CaptureSource {
                kind: SourceKind::Output,
                id: "DP-1".into(),
                app_id: None,
                label: "Tower".into(),
            }],
            cursor_mode: crate::streams::cursor_mode::EMBEDDED,
            devices: 0,
        }
    }

    fn caller(app: &str, workspace: Option<&str>) -> PortalCaller {
        let mut caller = PortalCaller::new(app);
        caller.workspace = workspace.map(str::to_owned);
        caller
    }

    tilo_test_env::scenario! {
        fn restore_grants_hold_for_their_app_in_its_workspace_until_it_quits;

        phase restore_data_is_a_standard_vendor_tuple;
        {
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

        phase a_grant_restores_only_for_its_app_in_its_workspace;
        {
            let grants = Grants::default();
            let key = grants.insert(grant());
            let share = caller("org.example.Share", Some("work"));
            let kept = grants.get(&key, &share).expect("restores");
            assert_eq!(kept.sources[0].id, "DP-1");
            assert_ne!(kept.id, key, "the published id is not the key");
            assert!(grants.get(&key, &caller("org.example.Other", Some("work"))).is_none());
            assert!(grants.get(&key, &caller("org.example.Share", Some("home"))).is_none());
            assert!(grants.get(&key, &caller("org.example.Share", None)).is_none());
            assert!(
                Grants::default().get(&key, &share).is_none(),
                "nothing outlives the session"
            );
        }

        phase grants_end_with_their_app_or_when_forgotten;
        {
            let grants = Grants::default();
            let first = grants.insert(grant());
            let second = grants.insert(grant());
            let share = caller("org.example.Share", Some("work"));
            assert_eq!(grants.resources().len(), 2);
            let id = grants.get(&first, &share).expect("kept").id;
            assert!(grants.forget(&id));
            assert!(!grants.forget(&id));
            assert!(grants.get(&first, &share).is_none());
            grants.app_exited("org.example.Share");
            assert!(grants.get(&second, &share).is_none());
        }
    }
}

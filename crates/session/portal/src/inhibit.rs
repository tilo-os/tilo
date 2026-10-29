//! `org.freedesktop.impl.portal.Inhibit`: an app keeping the session from
//! logging out, switching users, suspending or idling while it works.
//!
//! The inhibition lives as long as its request object: the frontend closes
//! the request (or leaves the bus) and the hold is gone. Each hold is a
//! token of the session's [`InhibitRegistry`], which the session's idle
//! and power handling already consults, and is published so the person
//! sees who keeps the machine awake and may end it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tilo_actions::{ActionOutput, DoorError, ResourcePresentation, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_power_mgmt::{InhibitRegistry, InhibitToken};
use tilo_resource::Effect;
use tilo_service::{Started, decode, offer};
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue};

use crate::Portal;

/// Logging out.
pub const LOGOUT: u32 = 1;
/// Switching users.
pub const USER_SWITCH: u32 = 2;
/// Suspending.
pub const SUSPEND: u32 = 4;
/// Idling: blanking the screen and locking.
pub const IDLE: u32 = 8;
const SUPPORTED: u32 = LOGOUT | USER_SWITCH | SUSPEND | IDLE;

const INHIBITION_PREFIX: &str = "portal:inhibit:";

/// What an app's hold offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.portal.inhibition")]
pub enum InhibitionEffect {
    /// End the app's hold
    #[effect(own_device, idempotent)]
    End,
}

/// One app's hold.
pub struct Inhibition {
    pub app_id: String,
    /// What it holds off, as the portal's flags.
    pub flags: u32,
    /// Why, in the app's words, when it said.
    pub reason: Option<String>,
    _token: InhibitToken,
}

/// The holds in force, by request path.
#[derive(Clone, Default)]
pub struct Inhibitions {
    registry: InhibitRegistry,
    held: Arc<Mutex<BTreeMap<String, Inhibition>>>,
}

impl Inhibitions {
    /// Holds taken in `registry`.
    #[must_use]
    pub fn in_registry(registry: InhibitRegistry) -> Self {
        Self {
            registry,
            held: Arc::default(),
        }
    }

    /// The registry the holds are taken in.
    #[must_use]
    pub const fn registry(&self) -> &InhibitRegistry {
        &self.registry
    }

    /// `(request, app, flags)` of every hold in force.
    #[must_use]
    pub fn held(&self) -> Vec<(String, String, u32)> {
        self.held
            .lock_recover()
            .iter()
            .map(|(path, held)| (path.clone(), held.app_id.clone(), held.flags))
            .collect()
    }

    fn hold(&self, path: &str, app_id: &str, flags: u32, reason: Option<String>) -> bool {
        let mut held = self.held.lock_recover();
        if held.contains_key(path) {
            return false;
        }
        let token = self
            .registry
            .acquire_for(reason.clone().unwrap_or_else(|| app_id.to_owned()), flags);
        held.insert(
            path.to_owned(),
            Inhibition {
                app_id: app_id.to_owned(),
                flags,
                reason,
                _token: token,
            },
        );
        true
    }

    /// End hold `path`; `false` when there is none.
    pub fn release(&self, path: &str) -> bool {
        self.held.lock_recover().remove(path).is_some()
    }

    pub(crate) fn resources(&self) -> Vec<ResourceSpec> {
        self.held
            .lock_recover()
            .iter()
            .map(|(path, held)| {
                offer::<InhibitionEffect>(ResourceSpec::new(
                    format!("{INHIBITION_PREFIX}{path}"),
                    "portal.inhibition",
                    held.app_id.clone(),
                ))
                .with_presentation(ResourcePresentation {
                    detail: held.reason.clone(),
                    badge: None,
                    preview: None,
                })
            })
            .collect()
    }

    fn reading_of(held: &Inhibition) -> Value {
        serde_json::json!({
            "app_id": held.app_id,
            "flags": held.flags,
            "reason": held.reason,
        })
    }

    /// Every hold in force, as JSON.
    pub(crate) fn reading(&self) -> Value {
        Value::Array(
            self.held
                .lock_recover()
                .values()
                .map(Self::reading_of)
                .collect(),
        )
    }

    /// The reading of an inhibition's resource.
    pub(crate) fn read(&self, resource: &str) -> Option<Value> {
        let path = resource.strip_prefix(INHIBITION_PREFIX)?;
        self.held.lock_recover().get(path).map(Self::reading_of)
    }

    /// Carry out an action on an inhibition's resource; `None` when the
    /// resource is no inhibition.
    pub(crate) fn invoke(
        &self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Option<Result<Started, DoorError>> {
        let path = resource.strip_prefix(INHIBITION_PREFIX)?;
        Some(
            decode::<InhibitionEffect>(published, resource, action, arguments).and_then(|_| {
                self.release(path)
                    .then(|| Started::Done(ActionOutput::default()))
                    .ok_or_else(|| {
                        DoorError::Unknown(tilo_actions::ActionAddress::new(resource, action))
                    })
            }),
        )
    }
}

fn valid_reason(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}

/// The `org.freedesktop.impl.portal.Inhibit` interface.
pub struct Inhibit {
    portal: Portal,
}

impl Inhibit {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Inhibit")]
impl Inhibit {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        3
    }

    /// The request path owns the hold: closing that request (or the
    /// frontend leaving) releases it.
    async fn inhibit(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        _window: String,
        flags: u32,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        let auth = self
            .portal
            .authenticate(&header, handle.as_str(), &app_id, None)
            .await
            .map_err(|_| zbus::fdo::Error::AccessDenied("untrusted portal caller".into()))?;
        if flags == 0 || flags & !SUPPORTED != 0 {
            return Err(zbus::fdo::Error::NotSupported(
                "unknown inhibit flags".into(),
            ));
        }
        let reason =
            crate::option::<String>(&options, "reason").filter(|value| valid_reason(value));
        let path = handle.to_string();
        let inhibitions = self.portal.inhibitions.clone();
        if !inhibitions.hold(&path, &auth.caller.app_id, flags, reason) {
            return Err(zbus::fdo::Error::InvalidArgs(
                "inhibit request already exists".into(),
            ));
        }
        let released = inhibitions.clone();
        let release_path = path.clone();
        let exported = self
            .portal
            .requests
            .export(connection, &path, auth.owner.as_str(), move || {
                released.release(&release_path);
            })
            .await;
        if !matches!(exported, Ok(true)) {
            inhibitions.release(&path);
            return Err(zbus::fdo::Error::Failed(
                "could not create inhibit request object".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hold_counts_until_released_and_only_for_its_flags() {
        let inhibitions = Inhibitions::default();
        assert!(inhibitions.hold("/r/1", "org.example.Player", IDLE, Some("Playing".into())));
        assert!(!inhibitions.hold("/r/1", "org.example.Player", IDLE, None));
        assert!(inhibitions.registry().is_inhibited_for(IDLE));
        assert!(!inhibitions.registry().is_inhibited_for(LOGOUT));
        let published = inhibitions.resources();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].label, "org.example.Player");
        let ended = inhibitions.invoke(
            &published,
            &format!("{INHIBITION_PREFIX}/r/1"),
            "end",
            &Map::new(),
        );
        assert!(matches!(ended, Some(Ok(_))), "{ended:?}");
        assert!(!inhibitions.registry().is_inhibited_for(IDLE));
        assert!(inhibitions.held().is_empty());
    }

    #[test]
    fn reasons_are_bounded_plain_text() {
        assert!(valid_reason("Playing a video"));
        assert!(!valid_reason(""));
        assert!(!valid_reason("a\nb"));
        assert!(!valid_reason(&"a".repeat(513)));
    }
}

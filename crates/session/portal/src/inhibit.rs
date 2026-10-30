//! `org.freedesktop.impl.portal.Inhibit`: an app keeping the session from
//! logging out, switching users, suspending or idling while it works.
//!
//! The inhibition lives as long as its request object: the frontend closes
//! the request (or leaves the bus) and the hold is gone. Each hold is a
//! token of the session's [`InhibitRegistry`], which the session's idle
//! and power handling already consults, and is published so the person
//! sees who keeps the machine awake and may end it.
//!
//! An app may also watch the session (`CreateMonitor`): whether the screen
//! saver is on and whether the session is running, about to end (asking
//! its apps to wrap up, `QueryEndResponse`) or ending. The session says so
//! with [`Portal::set_session_state`]; each monitor hears it as
//! `StateChanged`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tilo_actions::{ActionOutput, DoorError, ResourcePresentation, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_power_mgmt::{InhibitRegistry, InhibitToken};
use tilo_resource::Effect;
use tilo_service::{Started, decode, offer};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue};

use crate::reply::Ended;
use crate::session::SessionKind;
use crate::{OBJECT_PATH, Portal};

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

/// What a monitor hears of the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Running,
    /// About to end: apps should wrap up and answer `QueryEndResponse`.
    QueryEnd,
    Ending,
}

impl SessionState {
    /// The portal's number for it.
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::Running => 1,
            Self::QueryEnd => 2,
            Self::Ending => 3,
        }
    }
}

/// The session as monitors see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Watched {
    screensaver_active: bool,
    state: SessionState,
}

impl Default for Watched {
    fn default() -> Self {
        Self {
            screensaver_active: false,
            state: SessionState::Running,
        }
    }
}

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
    watched: Arc<Mutex<Watched>>,
    /// The monitors that answered since the session last asked to end.
    answered: Arc<Mutex<BTreeSet<String>>>,
}

impl Inhibitions {
    /// Holds taken in `registry`.
    #[must_use]
    pub fn in_registry(registry: InhibitRegistry) -> Self {
        Self {
            registry,
            held: Arc::default(),
            watched: Arc::default(),
            answered: Arc::default(),
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

    pub(crate) fn hold(
        &self,
        path: &str,
        app_id: &str,
        flags: u32,
        reason: Option<String>,
    ) -> bool {
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

    /// Take the session's state; `false` when nothing changed.
    fn watch(&self, screensaver_active: bool, state: SessionState) -> bool {
        let next = Watched {
            screensaver_active,
            state,
        };
        let previous = std::mem::replace(&mut *self.watched.lock_recover(), next);
        if state == SessionState::QueryEnd && previous.state != SessionState::QueryEnd {
            self.answered.lock_recover().clear();
        }
        previous != next
    }

    /// The `StateChanged` state a monitor hears.
    fn state_details(&self) -> HashMap<&'static str, OwnedValue> {
        let watched = *self.watched.lock_recover();
        HashMap::from([
            (
                "screensaver-active",
                OwnedValue::from(watched.screensaver_active),
            ),
            ("session-state", OwnedValue::from(watched.state.code())),
        ])
    }

    /// End hold `path`; `false` when there is none.
    pub fn release(&self, path: &str) -> bool {
        self.held.lock_recover().remove(path).is_some()
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
}

impl crate::Published for Inhibitions {
    fn resources(&self) -> Vec<ResourceSpec> {
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

    /// The reading of an inhibition's resource.
    fn read(&self, resource: &str) -> Option<Value> {
        let path = resource.strip_prefix(INHIBITION_PREFIX)?;
        self.held.lock_recover().get(path).map(Self::reading_of)
    }

    /// Carry out an action on an inhibition's resource; `None` when the
    /// resource is no inhibition.
    fn invoke(
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

    /// Watch the session as session `session_handle`; it hears the state
    /// at once and on every change.
    async fn create_monitor(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        window: String,
    ) -> u32 {
        let created = async {
            let auth = self
                .portal
                .authenticate(&header, handle.as_str(), &app_id, Some(&window))
                .await?;
            self.portal
                .sessions
                .create(
                    connection,
                    session_handle.as_str(),
                    auth.owner.as_str(),
                    auth.caller,
                    SessionKind::InhibitMonitor,
                )
                .await
        };
        match created.await {
            Ok(()) => {
                if let Ok(emitter) = SignalEmitter::new(connection, OBJECT_PATH) {
                    let details = self.portal.inhibitions.state_details();
                    let _ = Self::state_changed(&emitter, session_handle, details).await;
                }
                0
            }
            Err(Ended::Cancelled) => 1,
            Err(Ended::Failed) => 2,
        }
    }

    /// The monitor's app is ready for the session to end.
    async fn query_end_response(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
    ) -> zbus::fdo::Result<()> {
        let path = session_handle.as_str();
        let (_, session) = self
            .portal
            .authenticate_session(&header, path, "")
            .await
            .map_err(|_| zbus::fdo::Error::AccessDenied("no such monitor".into()))?;
        if session.kind != SessionKind::InhibitMonitor {
            return Err(zbus::fdo::Error::AccessDenied("no such monitor".into()));
        }
        if self.portal.inhibitions.watched.lock_recover().state == SessionState::QueryEnd {
            self.portal
                .inhibitions
                .answered
                .lock_recover()
                .insert(path.to_owned());
        }
        Ok(())
    }

    #[zbus(signal)]
    async fn state_changed(
        emitter: &SignalEmitter<'_>,
        session_handle: ObjectPath<'_>,
        state: HashMap<&str, OwnedValue>,
    ) -> zbus::Result<()>;
}

impl Portal {
    /// The session's state changed: tell every monitor.
    pub async fn set_session_state(
        &self,
        connection: &zbus::Connection,
        screensaver_active: bool,
        state: SessionState,
    ) {
        if !self.inhibitions.watch(screensaver_active, state) {
            return;
        }
        let Ok(emitter) = SignalEmitter::new(connection, OBJECT_PATH) else {
            return;
        };
        for (path, _) in self.sessions.of_kind(SessionKind::InhibitMonitor) {
            let Ok(path) = ObjectPath::try_from(path.as_str()) else {
                continue;
            };
            let _ = Inhibit::state_changed(&emitter, path, self.inhibitions.state_details()).await;
        }
    }

    /// The monitors yet to answer since the session asked to end.
    #[must_use]
    pub fn awaiting_query_end(&self) -> usize {
        let answered = self.inhibitions.answered.lock_recover().clone();
        self.sessions
            .of_kind(SessionKind::InhibitMonitor)
            .into_iter()
            .filter(|(path, _)| !answered.contains(path))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Published as _;

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

//! `org.freedesktop.impl.portal.Inhibit` backed by Tilo's logind mirror.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tilo_power_mgmt::{InhibitRegistry, InhibitToken};
#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::names::OwnedUniqueName;
#[cfg(target_os = "linux")]
use zbus::zvariant::{ObjectPath, OwnedValue};

use super::lifecycle::PortalLifecycle;
#[cfg(target_os = "linux")]
use super::lifecycle::{LifecycleEntry, LifecycleResource};
#[cfg(target_os = "linux")]
use super::{PortalContext, auth::authenticate_call};

/// Portal inhibit flags.
pub const LOGOUT: u32 = 1;
pub const USER_SWITCH: u32 = 2;
pub const SUSPEND: u32 = 4;
pub const IDLE: u32 = 8;
const SUPPORTED: u32 = LOGOUT | USER_SWITCH | SUSPEND | IDLE;

static SCREENSAVER_ACTIVE: AtomicBool = AtomicBool::new(false);
static SESSION_STATE_REVISION: AtomicU64 = AtomicU64::new(1);
static SESSION_WAKE: std::sync::LazyLock<crate::idle_wake::IdleWake> =
    std::sync::LazyLock::new(crate::idle_wake::IdleWake::default);

/// Publish the compositor's authoritative lock state to Inhibit monitors.
/// This contains no application-controlled data and is safe to call from the
/// native render loop only when the state changes.
pub fn publish_screensaver_state(active: bool) {
    if SCREENSAVER_ACTIVE.swap(active, Ordering::AcqRel) != active {
        SESSION_STATE_REVISION.fetch_add(1, Ordering::AcqRel);
        SESSION_WAKE.poke();
    }
}

#[derive(Clone, Default)]
pub struct InhibitMonitorHandle(Arc<Mutex<HashSet<String>>>);

pub struct InhibitBackend {
    #[cfg(target_os = "linux")]
    ctx: PortalContext,
    registry: InhibitRegistry,
    holds: Arc<Mutex<HashMap<String, InhibitToken>>>,
    monitors: InhibitMonitorHandle,
    lifecycle: PortalLifecycle,
}
impl InhibitBackend {
    #[cfg(target_os = "linux")]
    pub fn new(ctx: PortalContext, registry: InhibitRegistry) -> Self {
        Self::with_lifecycle(ctx, registry, PortalLifecycle::new())
    }
    #[cfg(target_os = "linux")]
    pub fn with_lifecycle(
        #[cfg(target_os = "linux")] ctx: PortalContext,
        registry: InhibitRegistry,
        lifecycle: PortalLifecycle,
    ) -> Self {
        Self {
            ctx,
            registry,
            holds: Arc::new(Mutex::new(HashMap::new())),
            monitors: InhibitMonitorHandle::default(),
            lifecycle,
        }
    }
    /// Called by the dynamically exported Request.Close implementation.
    pub fn release_handle(&self, handle: &str) -> bool {
        self.holds
            .lock()
            .is_ok_and(|mut holds| holds.remove(handle).is_some())
    }
    pub fn active_count(&self) -> usize {
        self.holds
            .lock()
            .map(|holds| holds.len())
            .unwrap_or_default()
    }
    pub fn monitor_handle(&self) -> InhibitMonitorHandle {
        self.monitors.clone()
    }
}

#[cfg(target_os = "linux")]
#[interface(name = "org.freedesktop.impl.portal.Inhibit")]
impl InhibitBackend {
    /// The request path owns the RAII token.  Closing that request (or the
    /// frontend disconnecting) drops the token and releases logind's block.
    async fn inhibit(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        _window: String,
        flags: u32,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        let auth = authenticate_call(&self.ctx, &hdr, &app_id, None)
            .await
            .map_err(|_| zbus::fdo::Error::AccessDenied("untrusted portal caller".into()))?;
        if flags == 0 || flags & !SUPPORTED != 0 {
            return Err(zbus::fdo::Error::NotSupported(
                "unknown inhibit flags".into(),
            ));
        }
        let reason = crate::portal::option::<String>(&options, "reason")
            .filter(|value| valid_reason(value))
            .unwrap_or_else(|| format!("{} is active", auth.caller.app_id));
        let key = handle.to_string();
        {
            let mut holds = self
                .holds
                .lock()
                .map_err(|_| zbus::fdo::Error::Failed("inhibit state unavailable".into()))?;
            if holds.contains_key(&key) {
                return Err(zbus::fdo::Error::InvalidArgs(
                    "inhibit request already exists".into(),
                ));
            }
            holds.insert(key.clone(), self.registry.acquire_for(reason, flags));
        }
        let holds = self.holds.clone();
        let cleanup_path = key.clone();
        let entry = LifecycleEntry {
            owner: auth.owner.to_string(),
            app_id: auth.caller.app_id,
            resource: LifecycleResource::Inhibitor,
        };
        let registered = self
            .lifecycle
            .register_request(connection, &key, entry, move |_| {
                if let Ok(mut holds) = holds.lock() {
                    holds.remove(&cleanup_path);
                }
            })
            .await;
        if !matches!(registered, Ok(true)) {
            let _ = self.release_handle(&key);
            return Err(zbus::fdo::Error::Failed(
                "could not create inhibit request object".into(),
            ));
        }
        Ok(())
    }
    async fn create_monitor(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        _handle: ObjectPath<'_>,
        session: ObjectPath<'_>,
        app_id: String,
        _window: String,
    ) -> u32 {
        let Ok(auth) = authenticate_call(&self.ctx, &hdr, &app_id, None).await else {
            return 2;
        };
        let path = session.to_string();
        let monitors = self.monitors.clone();
        let cleanup_path = path.clone();
        let entry = LifecycleEntry {
            owner: auth.owner.to_string(),
            app_id: auth.caller.app_id,
            resource: LifecycleResource::Other("inhibit-monitor".into()),
        };
        if !matches!(
            self.lifecycle
                .register_session(connection, &path, entry, move |_| {
                    if let Ok(mut sessions) = monitors.0.lock() {
                        sessions.remove(&cleanup_path);
                    }
                })
                .await,
            Ok(true)
        ) {
            return 2;
        }
        if let Ok(mut sessions) = self.monitors.0.lock() {
            sessions.insert(path.clone());
        }
        // Send the initial state as required; later Tilo session transitions
        // use the same signal helper before logout/session replacement.
        let mut state = HashMap::new();
        state.insert(
            "screensaver-active".into(),
            OwnedValue::from(SCREENSAVER_ACTIVE.load(Ordering::Acquire)),
        );
        state.insert("session-state".into(), OwnedValue::from(1_u32));
        if let Ok(context) = zbus::object_server::SignalEmitter::new(connection, path.as_str()) {
            let _ = Self::state_changed(&context, session, state).await;
        }
        0
    }
    async fn query_end_response(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session: ObjectPath<'_>,
    ) -> zbus::fdo::Result<()> {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let owner = super::auth::authenticate_frontend_call(&self.ctx, sender.as_ref())
            .await
            .map_err(|_| zbus::fdo::Error::AccessDenied("untrusted portal caller".into()))?;
        self.lifecycle
            .has_session_owner(session.as_str(), owner.as_str())
            .then_some(())
            .ok_or_else(|| {
                zbus::fdo::Error::AccessDenied("monitor session is not owned by caller".into())
            })
    }
    #[zbus(signal)]
    async fn state_changed(
        ctxt: &zbus::object_server::SignalEmitter<'_>,
        session: ObjectPath<'_>,
        state: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;
}

#[cfg(target_os = "linux")]
pub fn spawn_state_emitter(connection: zbus::Connection, monitors: InhibitMonitorHandle) {
    let task_connection = connection.clone();
    connection
        .executor()
        .spawn(
            async move {
                let mut seen = SESSION_STATE_REVISION.load(Ordering::Acquire);
                loop {
                    SESSION_WAKE.wait().await;
                    let revision = SESSION_STATE_REVISION.load(Ordering::Acquire);
                    if revision == seen {
                        continue;
                    }
                    seen = revision;
                    let paths = monitors
                        .0
                        .lock()
                        .map(|sessions| sessions.iter().cloned().collect::<Vec<_>>())
                        .unwrap_or_default();
                    for path in paths {
                        let Ok(session) = ObjectPath::try_from(path.clone()) else {
                            continue;
                        };
                        let Ok(context) = zbus::object_server::SignalEmitter::new(
                            &task_connection,
                            path.as_str(),
                        ) else {
                            continue;
                        };
                        let mut state = HashMap::new();
                        state.insert(
                            "screensaver-active".into(),
                            OwnedValue::from(SCREENSAVER_ACTIVE.load(Ordering::Acquire)),
                        );
                        state.insert("session-state".into(), OwnedValue::from(1_u32));
                        let _ = InhibitBackend::state_changed(&context, session, state).await;
                    }
                }
            },
            "portal inhibit state emitter",
        )
        .detach();
}
#[cfg(target_os = "linux")]
fn valid_reason(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    fn context() -> PortalContext {
        let (grants, _) = crate::portal::GrantStoreHandle::test_with_workspace(
            crate::shell::state::Workspace::new(crate::shell::state::WorkspaceId(1), "test"),
        );
        PortalContext::new_with_null_spawners(
            grants,
            crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::AutoDeny),
            crate::portal::CallerMap::new(),
        )
    }
    #[test]
    fn inhibit_advertises_standard_scopes_and_close_releases_the_hold() {
        {
            let registry = InhibitRegistry::new();
            let backend = InhibitBackend {
                #[cfg(target_os = "linux")]
                ctx: context(),
                registry: registry.clone(),
                holds: Arc::new(Mutex::new(HashMap::new())),
                monitors: InhibitMonitorHandle::default(),
                lifecycle: PortalLifecycle::new(),
            };
            backend.holds.lock().unwrap().insert(
                "/org/freedesktop/portal/desktop/request/x".into(),
                registry.acquire("test"),
            );
            assert!(registry.is_inhibited());
            assert!(backend.release_handle("/org/freedesktop/portal/desktop/request/x"));
            assert!(!registry.is_inhibited());
        }
        assert_eq!(SUPPORTED, LOGOUT | USER_SWITCH | SUSPEND | IDLE);
    }
}

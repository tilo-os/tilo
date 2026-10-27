//! `org.freedesktop.impl.portal.GlobalShortcuts` with compositor-owned grabs.
//!
//! This module owns only authenticated session/binding state.  The compositor
//! receives a [`GlobalShortcutsHandle`] and is the sole party that can turn a
//! physical key event into a queued portal activation.  The shell UI receives
//! a [`GlobalShortcutsService`] request to ask for consent and install the
//! actual key grabs.  There is deliberately no generic D-Bus key-grab path.

use super::lifecycle::LifecycleEntry;
#[cfg(target_os = "linux")]
use super::lifecycle::LifecycleResource;
use super::reply::{Ended, OrFail, Reply, Results, answer, ensure};
#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::object_server::SignalEmitter;
#[cfg(target_os = "linux")]
use zbus::zvariant::ObjectPath;
#[cfg(target_os = "linux")]
use zbus::zvariant::Value;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;
use winit::event::Modifiers;
use winit::keyboard::PhysicalKey;
#[cfg(any(test, target_os = "linux"))]
use zbus::zvariant::OwnedValue;
#[cfg(target_os = "linux")]
use zbus::zvariant::Str;

use super::PortalCaller;
use super::desktop_services::PortalResult;
use super::lifecycle::PortalLifecycle;
#[cfg(target_os = "linux")]
use super::{PortalContext, auth::authenticate_call};
use crate::shell::portal_dialog::{
    PortalDialogQueue, ShortcutBindingDecision, ShortcutBindingRequest, ShortcutBindingRow,
};

#[cfg(target_os = "linux")]
const MAX_SESSIONS: usize = 128;
#[cfg(any(test, target_os = "linux"))]
const MAX_SHORTCUTS: usize = 64;
#[cfg(any(test, target_os = "linux"))]
const MAX_ID_BYTES: usize = 128;
#[cfg(any(test, target_os = "linux"))]
const MAX_TEXT_BYTES: usize = 1024;
const MAX_QUEUED_ACTIVATIONS: usize = 1024;

#[cfg(any(test, target_os = "linux"))]
type ShortcutWire = (String, HashMap<String, OwnedValue>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalShortcutBindingRequest {
    /// Dynamic implementation-side Request object path, used to withdraw the
    /// shell sheet when the frontend calls Request.Close.
    pub handle: String,
    pub session: String,
    pub app_id: String,
    pub parent_window: String,
    pub shortcuts: Vec<GlobalShortcut>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalShortcut {
    pub id: String,
    pub description: Option<String>,
    pub preferred_trigger: Option<String>,
}

impl GlobalShortcut {
    #[cfg(target_os = "linux")]
    fn wire(&self) -> ShortcutWire {
        let mut options = HashMap::new();
        if let Some(description) = &self.description {
            options.insert(
                "description".into(),
                OwnedValue::from(Str::from(description.as_str())),
            );
        }
        if let Some(trigger) = &self.preferred_trigger {
            options.insert(
                "preferred_trigger".into(),
                OwnedValue::from(Str::from(trigger.as_str())),
            );
        }
        (self.id.clone(), options)
    }
}

/// The response to Tilo's binding sheet.  `Allowed` is intentionally a list:
/// users may approve some requested shortcuts and reject conflicting ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlobalShortcutConsent {
    /// The shell returns the exact user-selected physical bindings.  The
    /// backend validates the IDs again before it exposes them to the portal.
    Allowed(Vec<GlobalShortcut>),
    Denied,
}

/// Shell-owned UI/key-grab service.  A production implementation must release
/// all compositor grabs in `release_session`, including when it sees a portal
/// frontend disconnect.  The backend validates a returned id against the
/// original request before recording it.
pub trait GlobalShortcutsService: Send + Sync {
    fn request_bind(
        &self,
        request: GlobalShortcutBindingRequest,
        reply: oneshot::Sender<PortalResult<GlobalShortcutConsent>>,
    ) -> PortalResult<()>;
    fn release_session(&self, session: &str);
    fn cancel_bind(&self, _handle: &str) {}
}

pub type GlobalShortcutsServiceHandle = Arc<dyn GlobalShortcutsService>;

pub struct UnavailableGlobalShortcuts;
impl GlobalShortcutsService for UnavailableGlobalShortcuts {
    fn request_bind(
        &self,
        _: GlobalShortcutBindingRequest,
        _: oneshot::Sender<PortalResult<GlobalShortcutConsent>>,
    ) -> PortalResult<()> {
        PortalResult::Unavailable("Tilo global-shortcut chrome is not attached".into())
    }
    fn release_session(&self, _: &str) {}
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InstalledShortcut {
    session: String,
    app_id: String,
    id: String,
    accelerator: String,
}

#[derive(Default)]
struct ShellShortcutRegistry {
    installed: Vec<InstalledShortcut>,
}

/// Concrete Tilo shortcut service.  It uses the same compositor-owned review
/// sheet as other sensitive portal actions, then keeps only canonical physical
/// accelerators.  There is no executable/action data in this registry: a hit
/// can only queue `Activated` for the already-authenticated portal session.
#[derive(Clone)]
pub struct ShellGlobalShortcutsService {
    dialogs: PortalDialogQueue,
    registry: Arc<Mutex<ShellShortcutRegistry>>,
    events: Arc<Mutex<Option<GlobalShortcutsHandle>>>,
    /// Grants whose key is down, by physical key, so the release can send
    /// `Deactivated` to exactly the bindings the press activated.
    held: Arc<Mutex<HashMap<String, Vec<(String, String)>>>>,
    /// The effective editable shortcut table. Grants may not take a chord
    /// the user has assigned to a desktop action.
    shortcuts: Arc<Mutex<settings_store::hardware::ShortcutConfig>>,
}

impl ShellGlobalShortcutsService {
    pub fn global() -> Self {
        static SERVICE: std::sync::LazyLock<ShellGlobalShortcutsService> =
            std::sync::LazyLock::new(|| {
                ShellGlobalShortcutsService::with_parts(
                    PortalDialogQueue::global(),
                    settings_store::hardware::ShortcutConfig::load(),
                )
            });
        SERVICE.clone()
    }

    pub fn with_dialogs(dialogs: PortalDialogQueue) -> Self {
        Self::with_parts(dialogs, settings_store::hardware::ShortcutConfig::default())
    }

    fn with_parts(
        dialogs: PortalDialogQueue,
        shortcuts: settings_store::hardware::ShortcutConfig,
    ) -> Self {
        Self {
            dialogs,
            registry: Arc::new(Mutex::new(ShellShortcutRegistry::default())),
            events: Arc::new(Mutex::new(None)),
            held: Arc::new(Mutex::new(HashMap::new())),
            shortcuts: Arc::new(Mutex::new(shortcuts)),
        }
    }

    /// Attach the backend's authenticated activation queue.  `serve` calls
    /// this exactly once before accepting portal requests.
    pub fn attach_event_bridge(&self, events: GlobalShortcutsHandle) {
        if let Ok(mut slot) = self.events.lock() {
            *slot = Some(events);
        }
    }

    /// Replace the effective editable shortcut table the reserved check
    /// consults. The compositor calls this whenever Settings change.
    pub fn set_shortcut_config(&self, config: settings_store::hardware::ShortcutConfig) {
        if let Ok(mut slot) = self.shortcuts.lock() {
            *slot = config;
        }
    }

    /// Whether `accelerator` is withheld from apps under the current
    /// effective shortcut table.
    pub fn is_reserved(&self, accelerator: &str) -> bool {
        self.shortcuts
            .lock()
            .map(|shortcuts| is_reserved_accelerator(accelerator, &shortcuts))
            .unwrap_or(true)
    }

    /// Does an approved grant own this press? The compositor asks before
    /// offering the key to a focused client, so a granted chord fires over
    /// whatever app holds focus. The physical `KeyCode` is canonicalised
    /// without consulting the keyboard layout, so a layout switch cannot
    /// silently change a grant.
    pub fn claims(&self, physical: &PhysicalKey, modifiers: Modifiers) -> bool {
        let Some(code) = physical_code(physical) else {
            return false;
        };
        if self.held.lock().is_ok_and(|held| held.contains_key(&code)) {
            return true;
        }
        !self.matching_bindings(&code, modifiers).is_empty()
    }

    /// Queue `Activated` for every grant this press matches and remember
    /// them until the key is released. A press whose key is already held
    /// (the same event offered twice) is claimed without a second
    /// activation. Returns whether a grant claimed the press.
    pub fn press(&self, physical: &PhysicalKey, modifiers: Modifiers) -> bool {
        let Some(code) = physical_code(physical) else {
            return false;
        };
        let Ok(mut held) = self.held.lock() else {
            return false;
        };
        if held.contains_key(&code) {
            return true;
        }
        let hits = self.matching_bindings(&code, modifiers);
        if hits.is_empty() {
            return false;
        }
        let events = self.events.lock().ok().and_then(|slot| slot.clone());
        let timestamp = tilo_clock::unix_now_ms();
        let activated: Vec<_> = hits
            .into_iter()
            .filter(|binding| {
                events
                    .as_ref()
                    .is_some_and(|events| events.activate(&binding.session, &binding.id, timestamp))
            })
            .map(|binding| (binding.session, binding.id))
            .collect();
        // The press is the grant's even if its session could not take the
        // signal right now; handing it to the focused app instead would
        // make a granted chord type into whatever has focus.
        held.insert(code, activated);
        true
    }

    /// Queue `Deactivated` for the grants the matching press activated.
    /// Returns whether the key was held by a grant, in which case its
    /// release must not reach a client either.
    pub fn release(&self, physical: &PhysicalKey) -> bool {
        let Some(code) = physical_code(physical) else {
            return false;
        };
        let Some(activated) = self
            .held
            .lock()
            .ok()
            .and_then(|mut held| held.remove(&code))
        else {
            return false;
        };
        if let Some(events) = self.events.lock().ok().and_then(|slot| slot.clone()) {
            let timestamp = tilo_clock::unix_now_ms();
            for (session, id) in activated {
                events.deactivate(&session, &id, timestamp);
            }
        }
        true
    }

    fn matching_bindings(&self, code: &str, modifiers: Modifiers) -> Vec<InstalledShortcut> {
        let Some(candidate) = accelerator_for_code(code, modifiers) else {
            return Vec::new();
        };
        self.registry
            .lock()
            .map(|registry| {
                registry
                    .installed
                    .iter()
                    .filter(|binding| binding.accelerator == candidate)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn install_approved(&self, request: &GlobalShortcutBindingRequest) -> Vec<String> {
        let candidates: Vec<_> = request
            .shortcuts
            .iter()
            .filter_map(|shortcut| {
                canonical_accelerator(shortcut.preferred_trigger.as_deref()?).map(|accelerator| {
                    InstalledShortcut {
                        session: request.session.clone(),
                        app_id: request.app_id.clone(),
                        id: shortcut.id.clone(),
                        accelerator,
                    }
                })
            })
            .collect();
        let Ok(mut registry) = self.registry.lock() else {
            return Vec::new();
        };
        // Reconfiguration atomically replaces this session's previous grabs.
        registry
            .installed
            .retain(|binding| binding.session != request.session);
        let mut accepted = Vec::new();
        let mut proposed = HashSet::new();
        for candidate in candidates {
            if self.is_reserved(&candidate.accelerator)
                || !proposed.insert(candidate.accelerator.clone())
                || registry
                    .installed
                    .iter()
                    .any(|binding| binding.accelerator == candidate.accelerator)
            {
                continue;
            }
            accepted.push(candidate.id.clone());
            registry.installed.push(candidate);
        }
        accepted
    }
}

impl GlobalShortcutsService for ShellGlobalShortcutsService {
    fn request_bind(
        &self,
        request: GlobalShortcutBindingRequest,
        reply: oneshot::Sender<PortalResult<GlobalShortcutConsent>>,
    ) -> PortalResult<()> {
        let installed = self
            .registry
            .lock()
            .map(|registry| {
                registry
                    .installed
                    .iter()
                    .filter(|binding| binding.session != request.session)
                    .map(|binding| (binding.accelerator.clone(), binding.app_id.clone()))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let rows = request
            .shortcuts
            .iter()
            .map(|shortcut| {
                let accelerator = shortcut
                    .preferred_trigger
                    .as_deref()
                    .and_then(canonical_accelerator);
                let status = accelerator.as_deref().and_then(|accelerator| {
                    if self.is_reserved(accelerator) {
                        Some(crate::shell::i18n::t("portal-shortcut-reserved"))
                    } else if let Some(app_id) = installed.get(accelerator) {
                        Some(crate::shell::i18n::t_args(
                            "portal-shortcut-taken",
                            &[("app", safe_app_name(app_id).into())],
                        ))
                    } else {
                        None
                    }
                });
                ShortcutBindingRow {
                    id: shortcut.id.clone(),
                    description: shortcut
                        .description
                        .clone()
                        .unwrap_or_else(|| shortcut.id.clone()),
                    accelerator,
                    status,
                }
            })
            .collect();
        let receiver = match self
            .dialogs
            .enqueue_shortcut_bindings(ShortcutBindingRequest {
                handle: request.handle.clone(),
                app_id: request.app_id.clone(),
                rows,
            }) {
            Ok(receiver) => receiver,
            Err(error) => return PortalResult::Unavailable(error),
        };
        let service = self.clone();
        tokio::spawn(async move {
            let Some(ShortcutBindingDecision { rows }) = receiver.await.ok() else {
                let _ = reply.send(PortalResult::Cancelled);
                return;
            };
            let selected: HashMap<_, _> = rows
                .into_iter()
                .filter(|row| row.status.is_none())
                .filter_map(|row| row.accelerator.map(|accelerator| (row.id, accelerator)))
                .collect();
            let mut approved_request = request.clone();
            approved_request.shortcuts = request
                .shortcuts
                .iter()
                .filter_map(|shortcut| {
                    let accelerator = selected.get(&shortcut.id)?;
                    Some(GlobalShortcut {
                        id: shortcut.id.clone(),
                        description: shortcut.description.clone(),
                        preferred_trigger: Some(portal_trigger_for_accelerator(accelerator)),
                    })
                })
                .collect();
            let accepted = service.install_approved(&approved_request);
            let bindings = approved_request
                .shortcuts
                .into_iter()
                .filter(|shortcut| accepted.contains(&shortcut.id))
                .collect();
            let outcome = PortalResult::Success(GlobalShortcutConsent::Allowed(bindings));
            let _ = reply.send(outcome);
        });
        PortalResult::Success(())
    }

    fn release_session(&self, session: &str) {
        if let Ok(mut registry) = self.registry.lock() {
            registry
                .installed
                .retain(|binding| binding.session != session);
        }
    }

    fn cancel_bind(&self, handle: &str) {
        let _ = self.dialogs.cancel(handle);
    }
}

#[derive(Clone)]
struct Session {
    owner: String,
    caller: PortalCaller,
    shortcuts: Vec<GlobalShortcut>,
    #[cfg(target_os = "linux")]
    binding: bool,
}

#[derive(Default)]
struct ShortcutState {
    sessions: HashMap<String, Session>,
    activations: VecDeque<PortalShortcutActivation>,
}

/// Which portal signal a queued edge becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutEdge {
    /// The granted chord was pressed (`Activated`).
    Activated,
    /// The granted chord was released (`Deactivated`); push-to-talk and
    /// hold-to-show clients stop on this.
    Deactivated,
}

/// An authenticated shortcut edge queued by the compositor for D-Bus
/// delivery, in the order the keys moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortalShortcutActivation {
    pub session: String,
    pub shortcut_id: String,
    pub timestamp: u64,
    pub edge: ShortcutEdge,
}

/// Cloneable compositor-side event bridge.  It deliberately exposes no way to
/// alter binding ownership: `activate` succeeds only for an id which the
/// portal stored after a consent decision.
#[derive(Clone)]
pub struct GlobalShortcutsHandle {
    state: Arc<Mutex<ShortcutState>>,
    lifecycle: PortalLifecycle,
    service: GlobalShortcutsServiceHandle,
    wake: crate::idle_wake::IdleWake,
}

impl GlobalShortcutsHandle {
    /// Queue a D-Bus `Activated` signal for a live approved binding.
    pub fn activate(&self, session: &str, shortcut_id: &str, timestamp: u64) -> bool {
        self.queue(session, shortcut_id, timestamp, ShortcutEdge::Activated)
    }

    /// Queue the matching `Deactivated` signal when the key is released.
    pub fn deactivate(&self, session: &str, shortcut_id: &str, timestamp: u64) -> bool {
        self.queue(session, shortcut_id, timestamp, ShortcutEdge::Deactivated)
    }

    fn queue(&self, session: &str, shortcut_id: &str, timestamp: u64, edge: ShortcutEdge) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let valid = state.sessions.get(session).is_some_and(|entry| {
            entry
                .shortcuts
                .iter()
                .any(|shortcut| shortcut.id == shortcut_id)
        });
        if !valid {
            return false;
        }
        if state.activations.len() >= MAX_QUEUED_ACTIVATIONS {
            state.activations.pop_front();
        }
        state.activations.push_back(PortalShortcutActivation {
            session: session.into(),
            shortcut_id: shortcut_id.into(),
            timestamp,
            edge,
        });
        drop(state);
        self.wake.poke();
        true
    }

    pub fn drain_activations(&self) -> Vec<PortalShortcutActivation> {
        self.state
            .lock()
            .map(|mut state| state.activations.drain(..).collect())
            .unwrap_or_default()
    }

    /// A bridge holding one already-bound portal session, standing in for
    /// the D-Bus `CreateSession`/`BindShortcuts` exchange in headless
    /// compositor tests. Production handles come only from
    /// `GlobalShortcutsBackend::handle`.
    #[doc(hidden)]
    pub fn with_bound_session_for_tests(
        session: &str,
        app_id: &str,
        shortcut_ids: &[&str],
    ) -> Self {
        let mut state = ShortcutState::default();
        state.sessions.insert(
            session.into(),
            Session {
                owner: String::new(),
                caller: PortalCaller::new(crate::shell::state::WorkspaceId(1), app_id),
                shortcuts: shortcut_ids
                    .iter()
                    .map(|id| GlobalShortcut {
                        id: (*id).into(),
                        description: None,
                        preferred_trigger: None,
                    })
                    .collect(),
                #[cfg(target_os = "linux")]
                binding: false,
            },
        );
        Self {
            state: Arc::new(Mutex::new(state)),
            lifecycle: PortalLifecycle::new(),
            service: Arc::new(UnavailableGlobalShortcuts),
            wake: crate::idle_wake::IdleWake::default(),
        }
    }

    /// Called by the portal session-close/disconnect bridge. This is a
    /// compositor-only handle, so it does not accept user supplied ownership.
    pub fn close_session(&self, session: &str) -> Option<PortalCaller> {
        let record = self.state.lock().ok()?.sessions.remove(session)?;
        let _ = self.lifecycle.close_session(session, &record.owner);
        self.service.release_session(session);
        Some(record.caller)
    }

    /// Release every session belonging to a previous frontend generation.
    /// This prevents old broker names from consuming the bounded registry.
    pub fn close_owner(&self, owner: &str) -> Vec<PortalCaller> {
        let removed = self
            .state
            .lock()
            .map(|mut state| {
                let paths: Vec<_> = state
                    .sessions
                    .iter()
                    .filter(|(_, session)| session.owner == owner)
                    .map(|(path, _)| path.clone())
                    .collect();
                paths
                    .into_iter()
                    .filter_map(|path| state.sessions.remove(&path).map(|session| (path, session)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (path, session) in &removed {
            let _ = self.lifecycle.close_session(path, &session.owner);
            self.service.release_session(path);
        }
        removed
            .into_iter()
            .map(|(_, session)| session.caller)
            .collect()
    }
}

#[derive(Clone)]
#[cfg(target_os = "linux")]
pub struct GlobalShortcutsBackend {
    ctx: PortalContext,
    state: Arc<Mutex<ShortcutState>>,
    service: GlobalShortcutsServiceHandle,
    lifecycle: PortalLifecycle,
    wake: crate::idle_wake::IdleWake,
}

#[cfg(target_os = "linux")]
impl GlobalShortcutsBackend {
    /// A backend without chrome reports unavailable on `BindShortcuts`; it
    /// never records a pretend system-wide key grab.
    pub fn new(ctx: PortalContext) -> Self {
        Self::with_lifecycle(ctx, PortalLifecycle::new())
    }

    pub fn with_lifecycle(ctx: PortalContext, lifecycle: PortalLifecycle) -> Self {
        Self::with_service(ctx, Arc::new(UnavailableGlobalShortcuts), lifecycle)
    }

    pub fn with_service(
        ctx: PortalContext,
        service: GlobalShortcutsServiceHandle,
        lifecycle: PortalLifecycle,
    ) -> Self {
        Self {
            ctx,
            state: Arc::new(Mutex::new(ShortcutState::default())),
            service,
            lifecycle,
            wake: crate::idle_wake::IdleWake::default(),
        }
    }

    pub fn handle(&self) -> GlobalShortcutsHandle {
        GlobalShortcutsHandle {
            state: self.state.clone(),
            lifecycle: self.lifecycle.clone(),
            service: self.service.clone(),
            wake: self.wake.clone(),
        }
    }

    async fn authenticate_session(
        &self,
        hdr: zbus::message::Header<'_>,
        session: &ObjectPath<'_>,
    ) -> Option<(super::auth::AuthenticatedCall, Session)> {
        let record = self
            .state
            .lock()
            .ok()?
            .sessions
            .get(session.as_str())
            .cloned()?;
        let auth = authenticate_call(&self.ctx, &hdr, &record.caller.app_id, None)
            .await
            .ok()?;
        (record.owner == auth.owner.as_str() && record.caller == auth.caller)
            .then_some((auth, record))
    }
}

#[interface(name = "org.freedesktop.impl.portal.GlobalShortcuts")]
#[cfg(target_os = "linux")]
impl GlobalShortcutsBackend {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        2
    }

    async fn create_session(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        _handle: ObjectPath<'_>,
        session: ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async move {
            let parent_window =
                crate::portal::option::<String>(&options, "parent_window").unwrap_or_default();
            let auth = authenticate_call(
                &self.ctx,
                &hdr,
                &app_id,
                (!parent_window.is_empty()).then_some(parent_window.as_str()),
            )
            .await
            .or_fail()?;
            let key = session.to_string();
            let entry_app_id = auth.caller.app_id.clone();
            let inserted = self.state.lock().is_ok_and(|mut state| {
                if state.sessions.len() >= MAX_SESSIONS || state.sessions.contains_key(&key) {
                    return false;
                }
                state.sessions.insert(
                    key.clone(),
                    Session {
                        owner: auth.owner.to_string(),
                        caller: auth.caller,
                        shortcuts: Vec::new(),
                        binding: false,
                    },
                );
                true
            });
            ensure(inserted)?;
            let state = self.state.clone();
            let service = self.service.clone();
            let cleanup_path = key.clone();
            let entry = LifecycleEntry {
                owner: auth.owner.to_string(),
                app_id: entry_app_id,
                resource: LifecycleResource::GlobalShortcuts,
            };
            let registered = self
                .lifecycle
                .register_session(connection, &key, entry, move |_| {
                    if let Ok(mut state) = state.lock() {
                        state.sessions.remove(&cleanup_path);
                    }
                    service.release_session(&cleanup_path);
                })
                .await;
            if !matches!(registered, Ok(true)) {
                if let Ok(mut state) = self.state.lock() {
                    state.sessions.remove(&key);
                }
                return Err(Ended::Failed);
            }
            Ok(Results::new())
        })
        .await
    }

    async fn bind_shortcuts(
        &self,
        #[zbus(signal_emitter)] sig: SignalEmitter<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        session: ObjectPath<'_>,
        shortcuts: Vec<ShortcutWire>,
        parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async move {
            let connection = sig.connection();
            let Some((auth, current)) = self.authenticate_session(hdr, &session).await else {
                return Err(Ended::Failed);
            };
            let shortcuts = decode_shortcuts(shortcuts).or_fail()?;
            ensure(valid_parent(&parent_window))?;
            let key = session.to_string();
            let marked = self.state.lock().is_ok_and(|mut state| {
                state.sessions.get_mut(&key).is_some_and(|record| {
                    if record.owner != auth.owner.as_str()
                        || record.caller != auth.caller
                        || record.binding
                    {
                        false
                    } else {
                        record.binding = true;
                        true
                    }
                })
            });
            ensure(marked)?;
            let request_path = handle.to_string();
            let state = self.state.clone();
            let service = self.service.clone();
            let cleanup_session = key.clone();
            let cleanup_request = request_path.clone();
            let entry = LifecycleEntry {
                owner: auth.owner.to_string(),
                app_id: auth.caller.app_id.clone(),
                resource: LifecycleResource::Prompt,
            };
            let cancel = move |_: &LifecycleEntry| {
                if let Ok(mut state) = state.lock() {
                    if let Some(record) = state.sessions.get_mut(&cleanup_session) {
                        record.binding = false;
                    }
                }
                service.cancel_bind(&cleanup_request);
            };
            let outcome = self
                .lifecycle
                .with_request(connection, &request_path, entry, cancel, async {
                    let request = GlobalShortcutBindingRequest {
                        handle: request_path.clone(),
                        session: key.clone(),
                        app_id: current.caller.app_id.clone(),
                        parent_window,
                        shortcuts: shortcuts.clone(),
                    };
                    let (reply, rx) = oneshot::channel();
                    let outcome = match self.service.request_bind(request, reply) {
                        PortalResult::Success(()) => rx.await.unwrap_or(PortalResult::Cancelled),
                        PortalResult::Cancelled => PortalResult::Cancelled,
                        PortalResult::Unavailable(error) => PortalResult::Unavailable(error),
                        PortalResult::Failed(error) => PortalResult::Failed(error),
                    };
                    let approved = match outcome {
                        PortalResult::Success(GlobalShortcutConsent::Allowed(bindings)) => {
                            let requested: HashSet<_> = shortcuts
                                .iter()
                                .map(|shortcut| shortcut.id.as_str())
                                .collect();
                            let mut seen = HashSet::new();
                            if bindings.iter().all(|shortcut| {
                                requested.contains(shortcut.id.as_str())
                                    && seen.insert(shortcut.id.clone())
                                    && shortcut
                                        .preferred_trigger
                                        .as_deref()
                                        .and_then(canonical_accelerator)
                                        .is_some()
                            }) {
                                Some(bindings)
                            } else {
                                None
                            }
                        }
                        PortalResult::Success(GlobalShortcutConsent::Denied) => Some(Vec::new()),
                        PortalResult::Cancelled
                        | PortalResult::Unavailable(_)
                        | PortalResult::Failed(_) => None,
                    };
                    let approved = approved.or_fail()?;
                    let approved: HashMap<_, _> = approved
                        .into_iter()
                        .map(|shortcut| (shortcut.id, shortcut.preferred_trigger))
                        .collect();
                    let selected: Vec<_> = shortcuts
                        .into_iter()
                        .filter_map(|mut shortcut| {
                            let trigger = approved.get(&shortcut.id)?.clone()?;
                            shortcut.preferred_trigger = Some(trigger);
                            Some(shortcut)
                        })
                        .collect();
                    let wire = selected
                        .iter()
                        .map(GlobalShortcut::wire)
                        .collect::<Vec<_>>();
                    let stored = self.state.lock().is_ok_and(|mut state| {
                        state.sessions.get_mut(&key).is_some_and(|record| {
                            if record.owner != auth.owner.as_str() || record.caller != auth.caller {
                                false
                            } else {
                                record.shortcuts = selected;
                                record.binding = false;
                                true
                            }
                        })
                    });
                    if !stored {
                        self.service.release_session(&key);
                        return Err(Ended::Failed);
                    }
                    let _ = Self::shortcuts_changed(&sig, session, wire).await;
                    Ok(Results::new())
                })
                .await;
            // A bind that did not complete frees the session for the next one.
            if outcome.is_err()
                && let Ok(mut state) = self.state.lock()
                && let Some(record) = state.sessions.get_mut(&key)
            {
                record.binding = false;
            }
            outcome
        })
        .await
    }

    async fn list_shortcuts(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        session: ObjectPath<'_>,
    ) -> Reply {
        answer(async move {
            let Some((_, record)) = self.authenticate_session(hdr, &session).await else {
                return Err(Ended::Failed);
            };
            let mut results = HashMap::new();
            let wire = record
                .shortcuts
                .iter()
                .map(GlobalShortcut::wire)
                .collect::<Vec<_>>();
            let value = OwnedValue::try_from(Value::new(wire)).or_fail()?;
            results.insert("shortcuts".into(), value);
            Ok(results)
        })
        .await
    }

    async fn configure_shortcuts(
        &self,
        #[zbus(signal_emitter)] sig: SignalEmitter<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session: ObjectPath<'_>,
        parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        let Some((auth, record)) = self.authenticate_session(hdr, &session).await else {
            return Err(zbus::fdo::Error::AccessDenied(
                "untrusted portal caller".into(),
            ));
        };
        if !valid_parent(&parent_window) {
            return Err(zbus::fdo::Error::InvalidArgs(
                "invalid parent window".into(),
            ));
        }
        let key = session.to_string();
        let (reply, rx) = oneshot::channel();
        let request = GlobalShortcutBindingRequest {
            handle: format!("{key}:configure"),
            session: key,
            app_id: record.caller.app_id,
            parent_window,
            shortcuts: record.shortcuts.clone(),
        };
        match self.service.request_bind(request, reply) {
            PortalResult::Success(()) => match rx.await.unwrap_or(PortalResult::Cancelled) {
                PortalResult::Success(GlobalShortcutConsent::Allowed(bindings)) => {
                    let approved: HashMap<_, _> = bindings
                        .into_iter()
                        .map(|shortcut| (shortcut.id, shortcut.preferred_trigger))
                        .collect();
                    if approved.iter().any(|(id, trigger)| {
                        trigger.as_deref().and_then(canonical_accelerator).is_none()
                            || !record.shortcuts.iter().any(|shortcut| &shortcut.id == id)
                    }) {
                        self.service.release_session(session.as_str());
                        return Err(zbus::fdo::Error::Failed(
                            "shortcut service returned an unknown binding".into(),
                        ));
                    }
                    let selected: Vec<_> = record
                        .shortcuts
                        .into_iter()
                        .filter_map(|mut shortcut| {
                            let trigger = approved.get(&shortcut.id)?.clone()?;
                            shortcut.preferred_trigger = Some(trigger);
                            Some(shortcut)
                        })
                        .collect();
                    let wire = selected
                        .iter()
                        .map(GlobalShortcut::wire)
                        .collect::<Vec<_>>();
                    let changed = self.state.lock().is_ok_and(|mut state| {
                        state
                            .sessions
                            .get_mut(session.as_str())
                            .is_some_and(|live| {
                                if live.owner != auth.owner.as_str()
                                    || live.caller != auth.caller
                                    || live.binding
                                {
                                    false
                                } else {
                                    live.shortcuts = selected;
                                    true
                                }
                            })
                    });
                    if !changed {
                        self.service.release_session(session.as_str());
                        return Err(zbus::fdo::Error::AccessDenied(
                            "session is no longer owned by caller".into(),
                        ));
                    }
                    let _ = Self::shortcuts_changed(&sig, session, wire).await;
                    Ok(())
                }
                PortalResult::Success(GlobalShortcutConsent::Denied) => {
                    self.service.release_session(session.as_str());
                    if let Ok(mut state) = self.state.lock() {
                        if let Some(live) = state.sessions.get_mut(session.as_str()) {
                            if live.owner == auth.owner.as_str() && live.caller == auth.caller {
                                live.shortcuts.clear();
                            }
                        }
                    }
                    Ok(())
                }
                PortalResult::Cancelled => {
                    Err(zbus::fdo::Error::Failed("shortcut setup cancelled".into()))
                }
                PortalResult::Unavailable(error) | PortalResult::Failed(error) => {
                    Err(zbus::fdo::Error::Failed(error))
                }
            },
            PortalResult::Cancelled => {
                Err(zbus::fdo::Error::Failed("shortcut setup cancelled".into()))
            }
            PortalResult::Unavailable(error) | PortalResult::Failed(error) => {
                Err(zbus::fdo::Error::Failed(error))
            }
        }
    }

    #[zbus(signal)]
    async fn activated(
        ctxt: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        shortcut_id: String,
        timestamp: u64,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn deactivated(
        ctxt: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        shortcut_id: String,
        timestamp: u64,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn shortcuts_changed(
        ctxt: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        shortcuts: Vec<ShortcutWire>,
    ) -> zbus::Result<()>;
}

/// Deliver activation events accumulated by the compositor handle. `serve`
/// should spawn this after registering the backend; keeping it independent
/// makes the bridge usable in shell tests without a D-Bus connection.
#[cfg(target_os = "linux")]
pub fn spawn_signal_emitter(
    connection: zbus::Connection,
    handle: GlobalShortcutsHandle,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Ok(context) = SignalEmitter::new(&connection, super::OBJECT_PATH) else {
            return;
        };
        loop {
            let activations = handle.drain_activations();
            if activations.is_empty() {
                handle.wake.wait().await;
                continue;
            }
            for activation in activations {
                let Ok(session) = ObjectPath::try_from(activation.session.as_str()) else {
                    continue;
                };
                let _ = match activation.edge {
                    ShortcutEdge::Activated => {
                        GlobalShortcutsBackend::activated(
                            &context,
                            session,
                            activation.shortcut_id,
                            activation.timestamp,
                            HashMap::new(),
                        )
                        .await
                    }
                    ShortcutEdge::Deactivated => {
                        GlobalShortcutsBackend::deactivated(
                            &context,
                            session,
                            activation.shortcut_id,
                            activation.timestamp,
                            HashMap::new(),
                        )
                        .await
                    }
                };
            }
        }
    })
}

#[cfg(any(test, target_os = "linux"))]
fn decode_shortcuts(wire: Vec<ShortcutWire>) -> Option<Vec<GlobalShortcut>> {
    if wire.len() > MAX_SHORTCUTS {
        return None;
    }
    let mut ids = HashSet::new();
    wire.into_iter()
        .map(|(id, options)| {
            if !valid_id(&id) || !ids.insert(id.clone()) {
                return None;
            }
            let description = crate::portal::option::<String>(&options, "description");
            let preferred_trigger = crate::portal::option::<String>(&options, "preferred_trigger");
            if description.as_deref().is_some_and(|text| !valid_text(text))
                || preferred_trigger
                    .as_deref()
                    .is_some_and(|text| !valid_text(text))
            {
                return None;
            }
            Some(GlobalShortcut {
                id,
                description,
                preferred_trigger,
            })
        })
        .collect()
}
#[cfg(any(test, target_os = "linux"))]
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}
#[cfg(any(test, target_os = "linux"))]
fn valid_text(value: &str) -> bool {
    value.len() <= MAX_TEXT_BYTES && !value.chars().any(char::is_control)
}
#[cfg(target_os = "linux")]
fn valid_parent(value: &str) -> bool {
    value.len() <= MAX_TEXT_BYTES && !value.chars().any(char::is_control)
}

/// Parse xdg portal-style triggers such as `Ctrl+Shift+K` into a stable
/// physical-key representation (`CTRL+SHIFT+KEYK`).  Requiring a modifier
/// avoids allowing an application to claim ordinary typing keys globally.
fn canonical_accelerator(value: &str) -> Option<String> {
    let mut modifiers = Vec::new();
    let mut key = None;
    for piece in value.split('+') {
        let piece = piece.trim().to_ascii_uppercase();
        if piece.is_empty() {
            return None;
        }
        let modifier = match piece.as_str() {
            "CTRL" | "CONTROL" => Some("CTRL"),
            "ALT" => Some("ALT"),
            "SHIFT" => Some("SHIFT"),
            "SUPER" | "META" | "WIN" => Some("SUPER"),
            _ => None,
        };
        if let Some(modifier) = modifier {
            if modifiers.contains(&modifier) {
                return None;
            }
            modifiers.push(modifier);
        } else if key.replace(canonical_key_piece(&piece)?).is_some() {
            return None;
        }
    }
    let key = key?;
    if modifiers.is_empty() {
        return None;
    }
    modifiers.sort_unstable();
    Some(format!("{}+{key}", modifiers.join("+")))
}

fn canonical_key_piece(value: &str) -> Option<String> {
    if value.len() == 1 {
        let byte = value.as_bytes()[0];
        if byte.is_ascii_alphabetic() {
            return Some(format!("KEY{}", char::from(byte)));
        }
        if byte.is_ascii_digit() {
            return Some(format!("DIGIT{}", char::from(byte)));
        }
    }
    let normalized = match value {
        "ESC" => "ESCAPE",
        "RETURN" => "ENTER",
        " " => "SPACE",
        value => value,
    };
    if normalized.len() == 4
        && normalized.starts_with("KEY")
        && normalized.as_bytes()[3].is_ascii_alphabetic()
    {
        return Some(normalized.into());
    }
    if normalized.len() == 6
        && normalized.starts_with("DIGIT")
        && normalized.as_bytes()[5].is_ascii_digit()
    {
        return Some(normalized.into());
    }
    matches!(
        normalized,
        "SPACE"
            | "ENTER"
            | "ESCAPE"
            | "TAB"
            | "BACKSPACE"
            | "DELETE"
            | "INSERT"
            | "HOME"
            | "END"
            | "PAGEUP"
            | "PAGEDOWN"
            | "ARROWUP"
            | "ARROWDOWN"
            | "ARROWLEFT"
            | "ARROWRIGHT"
            | "F1"
            | "F2"
            | "F3"
            | "F4"
            | "F5"
            | "F6"
            | "F7"
            | "F8"
            | "F9"
            | "F10"
            | "F11"
            | "F12"
    )
    .then(|| normalized.into())
}

/// A readable spelling for a canonical physical accelerator in a portal
/// response. This is cosmetic only; matching always uses the canonical form.
pub fn display_accelerator(accelerator: &str) -> String {
    accelerator
        .split('+')
        .map(|piece| match piece {
            "CTRL" => "Ctrl".into(),
            "ALT" => "Alt".into(),
            "SHIFT" => "Shift".into(),
            "SUPER" => "Super".into(),
            key if key.strip_prefix("KEY").is_some() => key[3..].into(),
            key if key.strip_prefix("DIGIT").is_some() => key[5..].into(),
            "ARROWUP" => "Up".into(),
            "ARROWDOWN" => "Down".into(),
            "ARROWLEFT" => "Left".into(),
            "ARROWRIGHT" => "Right".into(),
            other => other.to_ascii_lowercase(),
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

fn portal_trigger_for_accelerator(accelerator: &str) -> String {
    display_accelerator(accelerator)
}

fn safe_app_name(app_id: &str) -> String {
    crate::shell::launcher::display_name_for_app_id(app_id).unwrap_or_else(|| {
        app_id.rsplit('.').next().map_or_else(
            || crate::shell::i18n::t("portal-shortcut-another-app"),
            |name| name.replace(['-', '_'], " "),
        )
    })
}

/// Canonicalise an actual compositor key event for both matching and the
/// trusted shortcut editor.  This deliberately takes a physical key, rather
/// than `logical_key`, so a change of keyboard layout cannot alter a grant.
pub fn physical_accelerator(physical: &PhysicalKey, modifiers: Modifiers) -> Option<String> {
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    accelerator_for_code(&format!("{code:?}"), modifiers)
}

fn accelerator_for_code(code: &str, modifiers: Modifiers) -> Option<String> {
    let code = code.to_ascii_uppercase();
    let state = modifiers.state();
    let mut parts = Vec::new();
    if state.control_key() {
        parts.push("CTRL");
    }
    if state.alt_key() {
        parts.push("ALT");
    }
    if state.shift_key() {
        parts.push("SHIFT");
    }
    if state.super_key() {
        parts.push("SUPER");
    }
    if parts.is_empty() || canonical_key_piece(&code).is_none() {
        return None;
    }
    parts.sort_unstable();
    Some(format!("{}+{code}", parts.join("+")))
}

/// Session conventions outside the shell's own chord tables that an app
/// must still not be granted: the X server kill chord, in-app search, the
/// menu key some toolkits bind to Alt+Space, and the tab chords every app
/// answers for its own tabs or hands to the desktop's (`ui::shell_tabs`). A
/// grant would take those from whatever app holds focus.
const SESSION_RESERVED: &[&str] = &[
    "CTRL+ALT+BACKSPACE",
    "CTRL+KEYK",
    "ALT+SPACE",
    "CTRL+TAB",
    "CTRL+SHIFT+TAB",
    "CTRL+KEYT",
    "CTRL+SHIFT+KEYT",
    "CTRL+KEYW",
];

/// Tilo-owned accelerators always win.  Exposed to the trusted dialog so it
/// can explain a rejected capture before any registry mutation occurs.
///
/// Derived from the shell's fixed-chord table, the chords client routing
/// withholds from every app, and the effective editable `shortcuts`, so a
/// grant can never be approved for a chord the compositor would take
/// before the grant is consulted.
pub fn is_reserved_accelerator(
    accelerator: &str,
    shortcuts: &settings_store::hardware::ShortcutConfig,
) -> bool {
    SESSION_RESERVED.contains(&accelerator)
        || crate::shell::fixed_chords::all_chords()
            .any(|chord| fixed_chord_accelerator(chord).as_deref() == Some(accelerator))
        || chord_for_accelerator(accelerator).is_some_and(|(key, mods)| {
            crate::shell::input_policy::shell_owns_chord(key, mods, false)
        })
        || shortcuts
            .bindings
            .values()
            .any(|chord| key_chord_accelerator(chord).as_deref() == Some(accelerator))
}

/// The client-routing chord a canonical accelerator describes.
fn chord_for_accelerator(
    accelerator: &str,
) -> Option<(
    crate::shell::input_policy::ChordKey,
    crate::shell::input_policy::ChordMods,
)> {
    use crate::shell::input_policy::{ChordKey, ChordMods};
    let mut mods = ChordMods::default();
    let mut key = None;
    for piece in accelerator.split('+') {
        match piece {
            "CTRL" => mods.ctrl = true,
            "ALT" => mods.alt = true,
            "SHIFT" => mods.shift = true,
            "SUPER" => mods.logo = true,
            other => {
                let letter = other
                    .strip_prefix("KEY")
                    .or_else(|| other.strip_prefix("DIGIT"))
                    .filter(|rest| rest.len() == 1)
                    .and_then(|rest| rest.chars().next());
                key = Some(match (letter, other) {
                    (Some(c), _) => ChordKey::Char(c.to_ascii_lowercase()),
                    (None, "SPACE") => ChordKey::Space,
                    (None, "TAB") => ChordKey::Tab,
                    (None, "ENTER") => ChordKey::Enter,
                    (None, "ESCAPE") => ChordKey::Escape,
                    (None, "DELETE") => ChordKey::Delete,
                    (None, "NUMLOCK") => ChordKey::NumLock,
                    (None, "PAGEUP") => ChordKey::PageUp,
                    (None, "PAGEDOWN") => ChordKey::PageDown,
                    (None, "ARROWLEFT") => ChordKey::ArrowLeft,
                    (None, "ARROWRIGHT") => ChordKey::ArrowRight,
                    (None, "ARROWUP") => ChordKey::ArrowUp,
                    (None, "ARROWDOWN") => ChordKey::ArrowDown,
                    (None, function) => function
                        .strip_prefix('F')
                        .and_then(|n| n.parse::<u8>().ok())
                        .map_or(ChordKey::Other, ChordKey::Function),
                });
            }
        }
    }
    Some((key?, mods))
}

/// The canonical accelerator for a fixed shell chord, or `None` for a
/// chord the portal cannot express (no modifier, or a key without a
/// canonical physical name such as Print Screen).
fn fixed_chord_accelerator(chord: crate::shell::fixed_chords::FixedChord) -> Option<String> {
    use crate::shell::input_policy::ChordKey;
    let key = match chord.key {
        ChordKey::Char(c) if c.is_ascii_alphanumeric() => c.to_ascii_uppercase().to_string(),
        ChordKey::Space => "SPACE".into(),
        ChordKey::Tab => "TAB".into(),
        ChordKey::Enter => "ENTER".into(),
        ChordKey::Escape => "ESCAPE".into(),
        ChordKey::Delete => "DELETE".into(),
        ChordKey::NumLock => "NUMLOCK".into(),
        ChordKey::PageUp => "PAGEUP".into(),
        ChordKey::PageDown => "PAGEDOWN".into(),
        ChordKey::ArrowLeft => "ARROWLEFT".into(),
        ChordKey::ArrowRight => "ARROWRIGHT".into(),
        ChordKey::ArrowUp => "ARROWUP".into(),
        ChordKey::ArrowDown => "ARROWDOWN".into(),
        ChordKey::Function(n) => format!("F{n}"),
        ChordKey::Char(_) | ChordKey::PrintScreen | ChordKey::Modifier | ChordKey::Other => {
            return None;
        }
    };
    canonical_parts(
        chord.mods.ctrl,
        chord.mods.alt,
        chord.mods.shift,
        chord.mods.logo,
        &key,
    )
}

/// The canonical accelerator for an editable shortcut binding.
fn key_chord_accelerator(chord: &settings_store::hardware::KeyChord) -> Option<String> {
    let key = match chord.key.as_str() {
        "left" => "ARROWLEFT".to_string(),
        "right" => "ARROWRIGHT".to_string(),
        "up" => "ARROWUP".to_string(),
        "down" => "ARROWDOWN".to_string(),
        other => other.to_ascii_uppercase(),
    };
    canonical_parts(
        chord.modifiers.contains("Ctrl"),
        chord.modifiers.contains("Alt"),
        chord.modifiers.contains("Shift"),
        chord.modifiers.contains("Super"),
        &key,
    )
}

fn canonical_parts(ctrl: bool, alt: bool, shift: bool, logo: bool, key: &str) -> Option<String> {
    let key = canonical_key_piece(key)?;
    let mut parts = Vec::new();
    for (held, name) in [
        (ctrl, "CTRL"),
        (alt, "ALT"),
        (shift, "SHIFT"),
        (logo, "SUPER"),
    ] {
        if held {
            parts.push(name);
        }
    }
    if parts.is_empty() {
        return None;
    }
    parts.sort_unstable();
    Some(format!("{}+{key}", parts.join("+")))
}

/// The physical key's stable name (`KeyK`), independent of layout.
fn physical_code(physical: &PhysicalKey) -> Option<String> {
    match physical {
        PhysicalKey::Code(code) => Some(format!("{code:?}")),
        PhysicalKey::Unidentified(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{KeyCode, ModifiersState};

    #[test]
    fn shortcut_bindings_are_bounded_and_deliver_only_bound_ids() {
        {
            let good = vec![("toggle".into(), HashMap::new())];
            assert_eq!(decode_shortcuts(good).unwrap().len(), 1);
            let duplicate = vec![
                ("toggle".into(), HashMap::new()),
                ("toggle".into(), HashMap::new()),
            ];
            assert!(decode_shortcuts(duplicate).is_none());
            assert!(decode_shortcuts(vec![("bad space".into(), HashMap::new())]).is_none());
        }
        {
            let handle = test_handle();
            assert!(!handle.activate("/org/freedesktop/portal/desktop/session/x", "unknown", 1));
        }
        {
            let handle = test_handle();
            let session = "/org/freedesktop/portal/desktop/session/x";
            handle.state.lock().unwrap().sessions.insert(
                session.into(),
                Session {
                    owner: ":1.7".into(),
                    caller: PortalCaller::new(
                        crate::shell::state::WorkspaceId(1),
                        "org.example.App",
                    ),
                    shortcuts: vec![GlobalShortcut {
                        id: "toggle".into(),
                        description: None,
                        preferred_trigger: None,
                    }],
                    #[cfg(target_os = "linux")]
                    binding: false,
                },
            );
            assert!(handle.activate(session, "toggle", 9));
            assert!(!handle.activate(session, "other", 10));
            assert_eq!(
                handle.drain_activations(),
                vec![PortalShortcutActivation {
                    session: session.into(),
                    shortcut_id: "toggle".into(),
                    timestamp: 9,
                    edge: ShortcutEdge::Activated,
                }]
            );
        }
    }

    #[test]
    fn accelerators_are_canonical_and_reserved_chords_win() {
        {
            assert_eq!(
                canonical_accelerator("Shift + Ctrl + k").as_deref(),
                Some("CTRL+SHIFT+KEYK")
            );
            assert!(canonical_accelerator("k").is_none());
            assert!(is_reserved_accelerator("SUPER+SPACE", &defaults()));
            assert!(!is_reserved_accelerator("CTRL+SHIFT+KEYK", &defaults()));
            let modifiers = Modifiers::default();
            assert!(accelerator_for_code("KeyK", modifiers).is_none());
        }
        {
            let service = ShellGlobalShortcutsService::with_dialogs(PortalDialogQueue::default());
            let request = GlobalShortcutBindingRequest {
                handle: "/request/a".into(),
                session: "/session/a".into(),
                app_id: "org.example.A".into(),
                parent_window: String::new(),
                shortcuts: vec![
                    GlobalShortcut {
                        id: "shell".into(),
                        description: None,
                        preferred_trigger: Some("Super+Space".into()),
                    },
                    GlobalShortcut {
                        id: "ok".into(),
                        description: None,
                        preferred_trigger: Some("Ctrl+Shift+K".into()),
                    },
                ],
            };
            assert_eq!(service.install_approved(&request), vec!["ok"]);
            let competing = GlobalShortcutBindingRequest {
                handle: "/request/b".into(),
                session: "/session/b".into(),
                app_id: "org.example.B".into(),
                parent_window: String::new(),
                shortcuts: vec![GlobalShortcut {
                    id: "conflict".into(),
                    description: None,
                    preferred_trigger: Some("Ctrl+Shift+K".into()),
                }],
            };
            assert!(service.install_approved(&competing).is_empty());
            service.release_session("/session/a");
            assert_eq!(service.install_approved(&competing), vec!["conflict"]);
        }
    }

    fn defaults() -> settings_store::hardware::ShortcutConfig {
        settings_store::hardware::ShortcutConfig::default()
    }

    #[test]
    fn reservations_follow_the_live_shortcut_table_and_client_routing() {
        use settings_store::hardware::{KeyChord, ShortcutAction};
        let mut rebound = defaults();
        rebound
            .set(
                ShortcutAction::Search,
                KeyChord::parse("Alt+G").expect("valid chord"),
            )
            .expect("unassigned chord");
        assert!(is_reserved_accelerator("ALT+KEYG", &rebound));
        assert!(!is_reserved_accelerator("ALT+KEYG", &defaults()));
        // Any Super chord is withheld from apps by client routing, so a
        // grant for it could never fire.
        assert!(is_reserved_accelerator("SUPER+KEYX", &defaults()));
    }

    #[test]
    fn a_held_grant_activates_once_and_deactivates_on_release() {
        let service = ShellGlobalShortcutsService::with_dialogs(PortalDialogQueue::default());
        let session = "/session/talk";
        let handle = GlobalShortcutsHandle::with_bound_session_for_tests(
            session,
            "org.example.Talk",
            &["talk"],
        );
        service.attach_event_bridge(handle.clone());
        let request = GlobalShortcutBindingRequest {
            handle: "/request/talk".into(),
            session: session.into(),
            app_id: "org.example.Talk".into(),
            parent_window: String::new(),
            shortcuts: vec![GlobalShortcut {
                id: "talk".into(),
                description: None,
                preferred_trigger: Some("Ctrl+Shift+M".into()),
            }],
        };
        assert!(
            service.is_reserved("CTRL+SHIFT+KEYT"),
            "reopen is every app's tab chord, never grantable"
        );
        assert!(
            !service.is_reserved("CTRL+SHIFT+KEYM"),
            "the grant fixture is unreserved"
        );
        assert_eq!(service.install_approved(&request), vec!["talk"]);
        let key = PhysicalKey::Code(KeyCode::KeyM);
        let chord = Modifiers::from(ModifiersState::CONTROL | ModifiersState::SHIFT);
        assert!(service.claims(&key, chord));
        assert!(!service.claims(&key, Modifiers::from(ModifiersState::CONTROL)));
        assert!(service.press(&key, chord));
        assert!(
            service.press(&key, chord),
            "a re-offered press stays claimed"
        );
        assert!(service.release(&key));
        assert!(!service.release(&key), "only one release per press");
        let edges: Vec<_> = handle
            .drain_activations()
            .into_iter()
            .map(|event| (event.shortcut_id, event.edge))
            .collect();
        assert_eq!(
            edges,
            vec![
                ("talk".to_string(), ShortcutEdge::Activated),
                ("talk".to_string(), ShortcutEdge::Deactivated),
            ]
        );
    }

    #[test]
    fn every_fixed_shell_chord_and_editable_default_is_reserved() {
        for accelerator in [
            "SUPER+KEYL",
            "SUPER+KEYW",
            "SUPER+KEYQ",
            "ALT+F4",
            "ALT+TAB",
            "ALT+SHIFT+TAB",
            "SHIFT+SUPER+KEYS",
            "SHIFT+SUPER+KEYR",
            "SUPER+KEYH",
            "SUPER+ENTER",
            "SUPER+PAGEUP",
            "SUPER+ESCAPE",
            "SUPER+SPACE",
            "CTRL+TAB",
            "SUPER+TAB",
            "SUPER+ARROWLEFT",
            "CTRL+ALT+BACKSPACE",
        ] {
            assert!(
                is_reserved_accelerator(accelerator, &defaults()),
                "{accelerator} must not be grantable to an app"
            );
        }
        for chord in crate::shell::fixed_chords::all_chords() {
            if let Some(accelerator) = fixed_chord_accelerator(chord) {
                assert_eq!(
                    canonical_accelerator(&accelerator).as_deref(),
                    Some(accelerator.as_str()),
                    "the derived form of {} must be canonical to ever match",
                    chord.display()
                );
            }
        }
        assert!(!is_reserved_accelerator("CTRL+SHIFT+KEYM", &defaults()));
    }

    #[tokio::test]
    async fn shell_editor_assigns_a_shortcut_without_a_preferred_trigger() {
        let dialogs = PortalDialogQueue::default();
        let service = ShellGlobalShortcutsService::with_dialogs(dialogs.clone());
        let (reply, receiver) = oneshot::channel();
        assert!(matches!(
            service.request_bind(
                GlobalShortcutBindingRequest {
                    handle: "/request/editor".into(),
                    session: "/session/editor".into(),
                    app_id: "org.example.Recorder".into(),
                    parent_window: String::new(),
                    shortcuts: vec![GlobalShortcut {
                        id: "toggle".into(),
                        description: Some("Toggle recording".into()),
                        preferred_trigger: None,
                    }],
                },
                reply,
            ),
            PortalResult::Success(())
        ));
        let mut controller = crate::shell::portal_dialog::PortalDialogController::new(dialogs);
        assert!(controller.pump());
        assert!(controller.key(
            &PhysicalKey::Code(KeyCode::KeyR),
            Modifiers::from(ModifiersState::CONTROL | ModifiersState::SHIFT),
        ));
        assert!(controller.confirm());
        let outcome = receiver.await.expect("the shell resolves the review");
        assert_eq!(
            outcome,
            PortalResult::Success(GlobalShortcutConsent::Allowed(vec![GlobalShortcut {
                id: "toggle".into(),
                description: Some("Toggle recording".into()),
                preferred_trigger: Some("Ctrl + Shift + R".into()),
            }]))
        );
        assert!(
            service.claims(
                &PhysicalKey::Code(KeyCode::KeyR),
                Modifiers::from(ModifiersState::CONTROL | ModifiersState::SHIFT),
            ),
            "the approved grant owns its chord ahead of a focused app"
        );
    }

    fn test_handle() -> GlobalShortcutsHandle {
        GlobalShortcutsHandle {
            state: Arc::new(Mutex::new(ShortcutState::default())),
            lifecycle: PortalLifecycle::new(),
            service: Arc::new(UnavailableGlobalShortcuts),
            wake: crate::idle_wake::IdleWake::default(),
        }
    }
}

//! `org.freedesktop.impl.portal.RemoteDesktop` backend.
//!
//! Spec: `/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.RemoteDesktop.xml`.
//!
//! `RemoteDesktop` lets an app (a remote-control / collaboration client)
//! drive the seat — inject pointer + keyboard events — after the user
//! consents. The handshake mirrors `ScreenCast`:
//!
//! 1. `CreateSession` — register the session.
//! 2. `SelectDevices` — the app asks for `pointer | keyboard | touchscreen`
//!    (a `types` bitmask). We surface the consent prompt here
//!    (`CapabilityKind::RemoteDesktop`) and record the granted devices.
//! 3. `Start` — commit; returns the `devices` bitmask the user allowed.
//! 4. `Notify*` — per-event injection, gated on the session being started
//!    **and** holding the relevant device grant. Accepted events decode
//!    into a [`RemoteAction`]
//!    and go to the [`RemoteInputInjector`]; the compositor drains that
//!    into [`State::apply_remote_desktop_action`], driving the
//!    smithay `Seat` at whatever surface holds focus.
//!
//! [`State::apply_remote_desktop_action`]: crate::compositor::state::State::apply_remote_desktop_action

#![allow(
    clippy::used_underscore_binding,
    reason = "the D-Bus method signatures below require unused positional parameters (handle, \
              options, ...); zbus's #[interface] macro deserialises them into these named \
              parameters and forwards them by identifier into its generated dispatcher, so the \
              lint fires on macro-generated code a per-function #[allow] cannot reach"
)]

use super::reply::{Ended, OrFail, Reply, Results, answer, ensure};
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd as StdOwnedFd;
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::object_server::SignalEmitter;
#[cfg(target_os = "linux")]
use zbus::zvariant::OwnedFd;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::network::streaming::RemoteAction;
#[cfg(target_os = "linux")]
use crate::shell::state::{Capability, Expiry};

use super::PortalCaller;
use super::lifecycle::PortalLifecycle;
#[cfg(target_os = "linux")]
use super::lifecycle::{LifecycleEntry, LifecycleResource};
use super::restore::{RestoreRecord, RestoreStore, restore_data, restore_key};
#[cfg(target_os = "linux")]
use super::{CapabilityKind, PortalContext, auth::authenticate_call, flow};

#[cfg(target_os = "linux")]
const MAX_LIVE_SESSIONS: usize = 64;

/// Device-type bitmask (`RemoteDesktop.AvailableDeviceTypes`).
pub mod device_type {
    pub const KEYBOARD: u32 = 1;
    pub const POINTER: u32 = 2;
    /// Device classes with real compositor delivery. Touch is deliberately
    /// omitted until a dedicated touch protocol path exists.
    pub const ALL: u32 = KEYBOARD | POINTER;
}

/// Version 2 includes both the original `Notify*` methods and the
/// authenticated, session-bound sender-side `ConnectToEIS` transport.
pub const REMOTE_DESKTOP_VERSION: u32 = 2;

/// Freedesktop key/button state: `1` pressed, `0` released.
fn state_pressed(state: u32) -> bool {
    state == 1
}

/// Sink for decoded `RemoteDesktop` `Notify*` actions.
///
/// The portal backend runs on the D-Bus executor; the seat lives on the
/// compositor thread. Production wires a [`ChannelInjector`] whose
/// receiver the compositor loop drains into
/// [`State::apply_remote_desktop_action`](crate::compositor::state::State::apply_remote_desktop_action).
/// Tests use a [`RecordingInjector`] or a [`ChannelInjector`] drained straight into a real `State`.
pub trait RemoteInputInjector: Send + Sync {
    /// Inject one action. Returns whether it was accepted downstream.
    fn inject(&self, action: RemoteAction) -> bool;
    fn available(&self) -> bool {
        true
    }
}

/// Cheap-clone handle around `Arc<dyn RemoteInputInjector>`.
#[derive(Clone)]
pub struct RemoteInputInjectorHandle {
    inner: Arc<dyn RemoteInputInjector>,
}

impl RemoteInputInjectorHandle {
    /// Create a new handle wrapping the given injector.
    pub fn new(inner: Arc<dyn RemoteInputInjector>) -> Self {
        Self { inner }
    }

    /// A handle that drops every action — default for portal contexts that don't host a live seat (e.g. tests).
    pub fn null() -> Self {
        Self {
            inner: Arc::new(NullInjector),
        }
    }

    /// Inject a remote action into the wrapped injector.
    pub fn inject(&self, action: RemoteAction) -> bool {
        self.inner.inject(action)
    }
    pub fn available(&self) -> bool {
        self.inner.available()
    }
}

struct NullInjector;
impl RemoteInputInjector for NullInjector {
    fn inject(&self, _action: RemoteAction) -> bool {
        false
    }
    fn available(&self) -> bool {
        false
    }
}

/// Records every injected action — for tests.
pub struct RecordingInjector {
    actions: Mutex<Vec<RemoteAction>>,
}

impl RecordingInjector {
    /// Create a new recording injector.
    pub fn new() -> Self {
        Self {
            actions: Mutex::new(Vec::new()),
        }
    }

    /// Return a clone of all recorded actions.
    pub fn actions(&self) -> Vec<RemoteAction> {
        self.actions.lock().map(|a| a.clone()).unwrap_or_default()
    }
}

impl Default for RecordingInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteInputInjector for RecordingInjector {
    fn inject(&self, action: RemoteAction) -> bool {
        if let Ok(mut a) = self.actions.lock() {
            a.push(action);
        }
        true
    }
}

/// Forwards actions over a channel to the compositor loop — the
/// production wiring: the compositor inserts the receiver as a calloop source and applies each action to its `State`.
pub struct ChannelInjector {
    tx: std::sync::mpsc::Sender<RemoteAction>,
}

impl ChannelInjector {
    /// Create a new channel-based injector that sends actions to the given channel.
    pub fn new(tx: std::sync::mpsc::Sender<RemoteAction>) -> Self {
        Self { tx }
    }
}

impl RemoteInputInjector for ChannelInjector {
    fn inject(&self, action: RemoteAction) -> bool {
        self.tx.send(action).is_ok()
    }
}

/// Per-session state, keyed by the frontend's `session_handle` path.
#[derive(Clone, Debug, Default)]
struct RemoteDesktopSession {
    owner: String,
    app_id: String,
    caller: Option<PortalCaller>,
    /// Devices the user granted (bitmask). `Notify*` is gated on this.
    devices: u32,
    /// The user consented at `SelectDevices` (so `Start` needn't re-ask).
    consented: bool,
    /// Serializes concurrent SelectDevices calls across the async prompt.
    selecting: bool,
    /// `Start` has committed the session; `Notify*` only injects after.
    started: bool,
    /// Clipboard is opt-in before Start, as required by Clipboard.  Keeping
    /// it session-local prevents a later remote-control session from inheriting
    /// access merely because the same app previously used the clipboard.
    clipboard_requested: bool,
    /// Serializes concurrent Start calls.
    starting: bool,
    /// The sender-context EIS server reads this before every event. Closing a
    /// session flips it before dropping the map entry, so a retained client FD
    /// can never inject after the authenticated session ended.
    eis_live: Arc<AtomicBool>,
    eis_connected: bool,
    /// The effective standard v2 persistence request selected by the user.
    persist_mode: u32,
    /// A validated restore record skips the duplicate consent prompt, but is
    /// still bound to the same authenticated application id.
    restored: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct RemoteRestoreData {
    app_id: String,
    devices: u32,
    persist_mode: u32,
    issued_unix_seconds: u64,
}

impl RestoreRecord for RemoteRestoreData {
    fn app_id(&self) -> &str {
        &self.app_id
    }
    fn persist_mode(&self) -> u32 {
        self.persist_mode
    }
    fn issued_unix_seconds(&self) -> u64 {
        self.issued_unix_seconds
    }
    fn grant_in_range(&self) -> bool {
        self.devices != 0 && self.devices & !device_type::ALL == 0
    }
}

pub struct RemoteDesktopBackend {
    #[cfg(target_os = "linux")]
    ctx: PortalContext,
    injector: RemoteInputInjectorHandle,
    lifecycle: PortalLifecycle,
    sessions: Arc<Mutex<HashMap<String, RemoteDesktopSession>>>,
    restore: Arc<Mutex<RestoreStore<RemoteRestoreData>>>,
}

/// Lifecycle hook retained by `serve` for Session.Close and frontend loss.
#[derive(Clone)]
pub struct RemoteDesktopSessionHandle {
    sessions: Arc<Mutex<HashMap<String, RemoteDesktopSession>>>,
}

impl RemoteDesktopSessionHandle {
    pub fn close(&self, session_handle: &str) -> bool {
        self.sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(session_handle))
            .inspect(|session| session.eis_live.store(false, Ordering::Release))
            .is_some()
    }

    pub fn clear(&self) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.clear();
        }
    }
}

impl super::clipboard::ClipboardSessionProvider for RemoteDesktopSessionHandle {
    fn request_clipboard(&self, session: &str, owner: &str) -> bool {
        self.sessions.lock().ok().is_some_and(|mut sessions| {
            sessions.get_mut(session).is_some_and(|session| {
                if session.owner != owner || session.started {
                    return false;
                }
                session.clipboard_requested = true;
                true
            })
        })
    }
    fn clipboard_owner(&self, session: &str) -> Option<String> {
        let sessions = self.sessions.lock().ok()?;
        let session = sessions.get(session)?;
        (session.started && session.clipboard_requested).then(|| session.owner.clone())
    }
    fn clipboard_sessions(&self) -> Vec<String> {
        self.sessions.lock().map_or_else(
            |_| Vec::new(),
            |sessions| {
                sessions
                    .iter()
                    .filter(|(_, session)| session.started && session.clipboard_requested)
                    .map(|(path, _)| path.clone())
                    .collect()
            },
        )
    }
}

impl RemoteDesktopBackend {
    /// Create a new `RemoteDesktop` portal backend.
    #[cfg(target_os = "linux")]
    pub fn new(ctx: PortalContext) -> Self {
        Self::with_lifecycle(ctx, PortalLifecycle::new())
    }

    #[cfg(target_os = "linux")]
    pub fn with_lifecycle(ctx: PortalContext, lifecycle: PortalLifecycle) -> Self {
        Self {
            injector: ctx.remote_input_injector.clone(),
            ctx,
            lifecycle,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            restore: Arc::new(Mutex::new(RestoreStore::load(
                "remote-desktop-restore.json",
            ))),
        }
    }

    pub fn session_handle(&self) -> RemoteDesktopSessionHandle {
        RemoteDesktopSessionHandle {
            sessions: self.sessions.clone(),
        }
    }

    /// True if the session is started and holds the `need` device grant —
    /// every `Notify*` checks this gate; a request for an ungranted device is silently dropped.
    fn gate(&self, sender: &str, session_handle: &ObjectPath<'_>, need: u32) -> bool {
        self.sessions
            .lock()
            .ok()
            .and_then(|s| s.get(&session_handle.to_string()).cloned())
            .is_some_and(|sess| sess.owner == sender && sess.started && (sess.devices & need != 0))
    }

    /// Decode + route one action if the gate allows it.
    fn route(
        &self,
        sender: &str,
        session_handle: &ObjectPath<'_>,
        need: u32,
        action: RemoteAction,
    ) {
        if !self.gate(sender, session_handle, need) {
            tracing::debug!(?action, "RemoteDesktop: dropped un-granted Notify*");
            return;
        }
        self.injector.inject(action);
    }

    /// [`Self::route`] for the sender of a `Notify*` call.
    fn notify(
        &self,
        hdr: &zbus::message::Header<'_>,
        session_handle: &ObjectPath<'_>,
        need: u32,
        action: RemoteAction,
    ) {
        if let Some(sender) = hdr.sender() {
            self.route(sender.as_str(), session_handle, need, action);
        }
    }
}

#[cfg(target_os = "linux")]
#[interface(name = "org.freedesktop.impl.portal.RemoteDesktop")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "argument types are fixed by the org.freedesktop.impl.portal.RemoteDesktop D-Bus signatures; zbus deserializes owned values here rather than by reference"
)]
impl RemoteDesktopBackend {
    /// Device types the seat can be driven with.
    #[zbus(property)]
    #[allow(
        clippy::unused_self,
        reason = "zbus's #[interface] macro dispatches D-Bus property getters as instance methods, even though this constant value doesn't depend on self"
    )]
    fn available_device_types(&self) -> u32 {
        self.ctx
            .remote_input_injector
            .available()
            .then_some(device_type::ALL)
            .unwrap_or(0)
    }

    /// Interface version. Named lowercase to match the spec's `version` property (zbus would otherwise `PascalCase` it to `Version`).
    #[zbus(property, name = "version")]
    #[allow(
        clippy::unused_self,
        reason = "zbus's #[interface] macro dispatches D-Bus property getters as instance methods, even though this constant value doesn't depend on self"
    )]
    fn version(&self) -> u32 {
        REMOTE_DESKTOP_VERSION
    }

    /// `CreateSession` — register the session.
    async fn create_session(
        &self,
        #[zbus(signal_emitter)] _ctx: SignalEmitter<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async move {
            tracing::debug!("RemoteDesktop.CreateSession: app={app_id}");
            ensure(self.ctx.remote_input_injector.available())?;
            let auth = authenticate_call(&self.ctx, &hdr, &app_id, None)
                .await
                .or_fail()?;
            let key = session_handle.to_string();
            {
                let mut sessions = self.sessions.lock().or_fail()?;
                if sessions.contains_key(&key) || sessions.len() >= MAX_LIVE_SESSIONS {
                    return Err(Ended::Failed);
                }
                sessions.insert(
                    key.clone(),
                    RemoteDesktopSession {
                        owner: auth.owner.to_string(),
                        app_id: app_id.clone(),
                        caller: Some(auth.caller.clone()),
                        ..Default::default()
                    },
                );
            }
            let cleanup = self.session_handle();
            let cleanup_path = key.clone();
            let entry = LifecycleEntry {
                owner: auth.owner.to_string(),
                app_id,
                resource: LifecycleResource::RemoteDesktop,
            };
            let registered = self
                .lifecycle
                .register_session(connection, &key, entry, move |_| {
                    cleanup.close(&cleanup_path);
                })
                .await;
            if !matches!(registered, Ok(true)) {
                let _ = self.session_handle().close(&key);
                return Err(Ended::Failed);
            }
            // The frontend-created session handle is the sole CreateSession
            // result; the current implementation interface defines no result
            // fields for this method.
            Ok(Results::new())
        })
        .await
    }

    /// `SelectDevices` — request pointer/keyboard access; surfaces the consent prompt and records the granted devices on the session.
    async fn select_devices(
        &self,
        #[zbus(signal_emitter)] _ctx: SignalEmitter<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async move {
            tracing::debug!("RemoteDesktop.SelectDevices: app={app_id}");
            let key = session_handle.to_string();
            let requested_persist =
                crate::portal::option::<u32>(&options, "persist_mode").unwrap_or(0);
            if requested_persist > 2 {
                return Err(Ended::Failed);
            }
            let restored = restore_key(&options).and_then(|key| {
                self.restore
                    .lock()
                    .ok()
                    .and_then(|store| store.get(&key))
                    .filter(|record| record.app_id == app_id)
            });
            let mut types = restored.as_ref().map_or_else(
                || crate::portal::option::<u32>(&options, "types").unwrap_or(device_type::ALL),
                |record| record.devices,
            ) & device_type::ALL;
            if types == 0 {
                types = device_type::ALL;
            }

            let auth = authenticate_call(&self.ctx, &hdr, &app_id, None)
                .await
                .or_fail()?;
            let cancelled = Arc::new(AtomicBool::new(false));
            let cancellation = cancelled.clone();
            let request_path = handle.to_string();
            let request_entry = LifecycleEntry {
                owner: auth.owner.to_string(),
                app_id: auth.caller.app_id.clone(),
                resource: LifecycleResource::Prompt,
            };
            let cancel = move |_: &LifecycleEntry| {
                cancellation.store(true, Ordering::Release);
            };
            self.lifecycle
                .with_request(connection, &request_path, request_entry, cancel, async {
                    let Some(session) = self
                        .sessions
                        .lock()
                        .ok()
                        .and_then(|sessions| sessions.get(&key).cloned())
                    else {
                        return Err(Ended::Failed);
                    };
                    if session.owner != auth.owner.as_str()
                        || session.app_id != app_id
                        || session.caller.as_ref() != Some(&auth.caller)
                        || session.consented
                        || session.started
                    {
                        return Err(Ended::Failed);
                    }
                    let marked_selecting = self.sessions.lock().ok().is_some_and(|mut sessions| {
                        sessions.get_mut(&key).is_some_and(|entry| {
                            if entry.selecting || entry.consented || entry.started {
                                false
                            } else {
                                entry.selecting = true;
                                true
                            }
                        })
                    });
                    if !marked_selecting {
                        return Err(Ended::Failed);
                    }
                    let cap_template = Capability::RemoteDesktop {
                        scope: Expiry::AfterUse,
                    };
                    let allowed = restored.is_some()
                        || flow::resolve(
                            &self.ctx,
                            auth.caller,
                            CapabilityKind::RemoteDesktop,
                            cap_template,
                        )
                        .await
                        .is_allowed();

                    if cancelled.load(Ordering::Acquire) || !allowed {
                        if let Ok(mut sessions) = self.sessions.lock()
                            && let Some(entry) = sessions.get_mut(&key)
                        {
                            entry.selecting = false;
                        }
                        return Err(Ended::Cancelled);
                    }

                    let updated = self.sessions.lock().ok().is_some_and(|mut sessions| {
                        let Some(entry) = sessions.get_mut(&key) else {
                            return false;
                        };
                        entry.devices = types;
                        entry.consented = true;
                        entry.selecting = false;
                        entry.persist_mode = restored
                            .as_ref()
                            .map_or(requested_persist, |record| record.persist_mode);
                        entry.restored = restored.is_some();
                        true
                    });
                    if !updated {
                        return Err(Ended::Failed);
                    }
                    Ok(Results::new())
                })
                .await
        })
        .await
    }

    /// `Start` — commit the session. Returns the granted `devices` bitmask.
    async fn start(
        &self,
        #[zbus(signal_emitter)] _ctx: SignalEmitter<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async move {
            tracing::debug!("RemoteDesktop.Start: app={app_id}");
            ensure(self.ctx.remote_input_injector.available())?;
            let key = session_handle.to_string();
            let auth = authenticate_call(&self.ctx, &hdr, &app_id, Some(&parent_window))
                .await
                .or_fail()?;
            let session = self
                .sessions
                .lock()
                .ok()
                .and_then(|s| s.get(&key).cloned())
                .or_fail()?;
            if session.owner != auth.owner.as_str()
                || session.app_id != app_id
                || session.caller.as_ref() != Some(&auth.caller)
                || !session.consented
                || session.selecting
                || session.starting
                || session.started
            {
                return Err(Ended::Failed);
            }
            let marked_starting = self.sessions.lock().ok().is_some_and(|mut sessions| {
                sessions.get_mut(&key).is_some_and(|entry| {
                    if entry.starting || entry.started {
                        false
                    } else {
                        entry.starting = true;
                        true
                    }
                })
            });
            ensure(marked_starting)?;

            let devices = if session.devices == 0 {
                device_type::ALL
            } else {
                session.devices
            };
            let persist_mode = session.persist_mode;
            let session_key = key.clone();
            let mut sessions = self.sessions.lock().or_fail()?;
            let entry = sessions.get_mut(&key).or_fail()?;
            entry.starting = false;
            entry.started = true;
            entry.devices = devices;
            drop(sessions);

            let restore_key = (persist_mode > 0).then(|| uuid::Uuid::new_v4().to_string());
            if let Some(key) = &restore_key {
                let record = RemoteRestoreData {
                    app_id: app_id.clone(),
                    devices,
                    persist_mode,
                    issued_unix_seconds: tilo_clock::unix_now(),
                };
                let stored = self
                    .restore
                    .lock()
                    .map_err(|_| "remote-desktop restore store is unavailable".to_string())
                    .and_then(|mut store| store.insert(key.clone(), record));
                if stored.is_err() {
                    if let Ok(mut sessions) = self.sessions.lock()
                        && let Some(entry) = sessions.get_mut(&session_key)
                    {
                        entry.starting = false;
                        entry.started = false;
                    }
                    return Err(Ended::Failed);
                }
            }

            let mut results: HashMap<String, OwnedValue> = HashMap::new();
            if let Ok(v) = OwnedValue::try_from(Value::U32(devices)) {
                results.insert("devices".to_string(), v);
            }
            if let Ok(v) = OwnedValue::try_from(Value::Bool(false)) {
                results.insert("clipboard_enabled".to_string(), v);
            }
            if let Some(key) = restore_key
                && let Some(data) = restore_data(key)
            {
                results.insert("restore_data".to_string(), data);
                if let Ok(mode) = OwnedValue::try_from(Value::U32(persist_mode)) {
                    results.insert("persist_mode".to_string(), mode);
                }
            }
            Ok(results)
        })
        .await
    }

    /// v2 EIS transport.  Unlike InputCapture this is a sender-context EIS
    /// server: messages received from the app are decoded to `RemoteAction`
    /// and pass through the same compositor injector as `Notify*`.
    async fn connect_to_eis(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<OwnedFd> {
        let auth = authenticate_call(&self.ctx, &hdr, &app_id, None)
            .await
            .map_err(|_| zbus::fdo::Error::AccessDenied("untrusted RemoteDesktop caller".into()))?;
        let key = session_handle.to_string();
        let (live, devices) = self
            .sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.get_mut(&key).and_then(|session| {
                if session.owner != auth.owner.as_str()
                    || session.app_id != app_id
                    || session.caller.as_ref() != Some(&auth.caller)
                    || !session.started
                    || session.devices == 0
                    || session.eis_connected
                { return None; }
                session.eis_connected = true;
                Some((session.eis_live.clone(), session.devices))
            }))
            .ok_or_else(|| {
                zbus::fdo::Error::AccessDenied(
                    "RemoteDesktop session is not started, already connected, or owned by this caller".into(),
                )
            })?;
        live.store(true, Ordering::Release);
        let fd = spawn_remote_eis(live, devices, self.ctx.remote_input_injector.clone())
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        Ok(OwnedFd::from(fd))
    }

    /// Relative pointer motion.
    fn notify_pointer_motion(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        dx: f64,
        dy: f64,
    ) {
        self.notify(
            &hdr,
            &session_handle,
            device_type::POINTER,
            RemoteAction::PointerMotionRelative { dx, dy },
        );
    }

    /// Absolute pointer motion, relative to a stream node.
    fn notify_pointer_motion_absolute(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        stream: u32,
        x: f64,
        y: f64,
    ) {
        self.notify(
            &hdr,
            &session_handle,
            device_type::POINTER,
            RemoteAction::PointerMotionAbsolute { stream, x, y },
        );
    }

    /// Pointer button (evdev code + state).
    fn notify_pointer_button(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        button: i32,
        state: u32,
    ) {
        self.notify(
            &hdr,
            &session_handle,
            device_type::POINTER,
            RemoteAction::PointerButton {
                button,
                pressed: state_pressed(state),
            },
        );
    }

    /// Smooth scroll.
    fn notify_pointer_axis(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        dx: f64,
        dy: f64,
    ) {
        self.notify(
            &hdr,
            &session_handle,
            device_type::POINTER,
            RemoteAction::PointerAxis { dx, dy },
        );
    }

    /// Discrete scroll (`axis` 0 vertical, 1 horizontal).
    fn notify_pointer_axis_discrete(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        axis: u32,
        steps: i32,
    ) {
        self.notify(
            &hdr,
            &session_handle,
            device_type::POINTER,
            RemoteAction::PointerAxisDiscrete { axis, steps },
        );
    }

    /// Key press/release by evdev keycode.
    fn notify_keyboard_keycode(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        keycode: i32,
        state: u32,
    ) {
        self.notify(
            &hdr,
            &session_handle,
            device_type::KEYBOARD,
            RemoteAction::KeyboardKeycode {
                keycode,
                pressed: state_pressed(state),
            },
        );
    }

    /// Key press/release by keysym.
    fn notify_keyboard_keysym(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        keysym: i32,
        state: u32,
    ) {
        self.notify(
            &hdr,
            &session_handle,
            device_type::KEYBOARD,
            RemoteAction::KeyboardKeysym {
                keysym,
                pressed: state_pressed(state),
            },
        );
    }
}

/// Start a sender-context EIS server for one RemoteDesktop session.  The
/// server owns only its socket; permission remains the shared atomic owned by
/// the portal session and is checked for every decoded event.
#[cfg(target_os = "linux")]
fn spawn_remote_eis(
    live: Arc<AtomicBool>,
    devices: u32,
    injector: RemoteInputInjectorHandle,
) -> std::io::Result<StdOwnedFd> {
    use reis::enumflags2::BitFlag as _;
    let (server, client) = UnixStream::pair()?;
    std::thread::Builder::new()
        .name("tilo-remote-desktop-eis".into())
        .spawn(move || {
            let Ok(context) = reis::eis::Context::new(server) else {
                return;
            };
            let Ok(mut event_loop) = smithay::reexports::calloop::EventLoop::<()>::try_new() else {
                return;
            };
            let handle = event_loop.handle();
            let closure_live = live.clone();
            let installed = handle.insert_source(
                reis::calloop::EisRequestSource::new(context, 1),
                move |event, connection, _| {
                    match event {
                        Ok(reis::calloop::EisRequestSourceEvent::Connected) => {
                            let mut advertised = reis::request::DeviceCapability::empty();
                            if devices & device_type::KEYBOARD != 0 {
                                advertised |= reis::request::DeviceCapability::Keyboard;
                            }
                            if devices & device_type::POINTER != 0 {
                                advertised |= reis::request::DeviceCapability::Pointer
                                    | reis::request::DeviceCapability::Button
                                    | reis::request::DeviceCapability::Scroll;
                            }
                            let _seat = connection.add_seat(Some("tilo"), advertised);
                            let _ = connection.flush();
                        }
                        Ok(reis::calloop::EisRequestSourceEvent::Request(request)) => {
                            if !closure_live.load(Ordering::Acquire) {
                                return Ok(smithay::reexports::calloop::PostAction::Remove);
                            }
                            match request {
                                reis::request::EisRequest::Bind(bind) => {
                                    let capabilities = bind.capabilities
                                        & (reis::request::DeviceCapability::Keyboard
                                            | reis::request::DeviceCapability::Pointer
                                            | reis::request::DeviceCapability::Button
                                            | reis::request::DeviceCapability::Scroll);
                                    let device = bind.seat.add_device(
                                        Some("tilo remote input"),
                                        reis::eis::device::DeviceType::Virtual,
                                        capabilities,
                                        |_| {},
                                    );
                                    device.resumed();
                                    let _ = connection.flush();
                                }
                                request => {
                                    if let Some(action) = remote_action_from_eis(request)
                                        && action_allowed_by_devices(&action, devices)
                                    {
                                        let _ = injector.inject(action);
                                    }
                                }
                            }
                        }
                        Err(_) => return Ok(smithay::reexports::calloop::PostAction::Remove),
                    }
                    Ok(smithay::reexports::calloop::PostAction::Continue)
                },
            );
            if installed.is_err() {
                return;
            }
            let mut state = ();
            while live.load(Ordering::Acquire)
                && event_loop
                    .dispatch(std::time::Duration::from_secs(1), &mut state)
                    .is_ok()
            {}
        })?;
    Ok(StdOwnedFd::from(client))
}

fn action_allowed_by_devices(action: &RemoteAction, devices: u32) -> bool {
    let required = match action {
        RemoteAction::KeyboardKeycode { .. } | RemoteAction::KeyboardKeysym { .. } => {
            device_type::KEYBOARD
        }
        RemoteAction::PointerMotionRelative { .. }
        | RemoteAction::PointerMotionAbsolute { .. }
        | RemoteAction::PointerButton { .. }
        | RemoteAction::PointerAxis { .. }
        | RemoteAction::PointerAxisDiscrete { .. } => device_type::POINTER,
    };
    devices & required != 0
}

#[cfg(target_os = "linux")]
fn remote_action_from_eis(request: reis::request::EisRequest) -> Option<RemoteAction> {
    use reis::request::EisRequest;
    match request {
        EisRequest::PointerMotion(event) => Some(RemoteAction::PointerMotionRelative {
            dx: f64::from(event.dx),
            dy: f64::from(event.dy),
        }),
        EisRequest::PointerMotionAbsolute(event) => Some(RemoteAction::PointerMotionAbsolute {
            stream: 0,
            x: f64::from(event.dx_absolute),
            y: f64::from(event.dy_absolute),
        }),
        EisRequest::Button(event) => Some(RemoteAction::PointerButton {
            button: i32::try_from(event.button).ok()?,
            pressed: matches!(event.state, reis::eis::button::ButtonState::Press),
        }),
        EisRequest::ScrollDelta(event) => Some(RemoteAction::PointerAxis {
            dx: f64::from(event.dx),
            dy: f64::from(event.dy),
        }),
        EisRequest::ScrollDiscrete(event) => Some(RemoteAction::PointerAxisDiscrete {
            axis: 0,
            steps: event.discrete_dy,
        }),
        EisRequest::KeyboardKey(event) => Some(RemoteAction::KeyboardKeycode {
            keycode: i32::try_from(event.key).ok()?,
            pressed: matches!(event.state, reis::eis::keyboard::KeyState::Press),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::portal::{CallerMap, GrantStoreHandle, PromptQueueHandle, PromptResolver};
    #[cfg(target_os = "linux")]
    use crate::shell::state::{Workspace, WorkspaceId};

    #[cfg(target_os = "linux")]
    fn empty_ws(id: u64) -> Workspace {
        // `Workspace` grew a durable room id, an owner and a capability list; the builder keeps this fixture from having to track them.
        Workspace::new(WorkspaceId(id), "rd")
    }

    fn backend_with(injector: RemoteInputInjectorHandle) -> RemoteDesktopBackend {
        RemoteDesktopBackend {
            #[cfg(target_os = "linux")]
            ctx: {
                let (store, _) = GrantStoreHandle::test_with_workspace(empty_ws(70));
                PortalContext::new_with_null_spawners(
                    store,
                    PromptQueueHandle::new(PromptResolver::AutoDeny),
                    CallerMap::new(),
                )
            },
            injector,
            lifecycle: PortalLifecycle::new(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            restore: Arc::new(Mutex::new(RestoreStore::default())),
        }
    }

    #[test]
    fn remote_desktop_injectors_record_forward_or_drop() {
        // Phase `recording_injector_captures_actions`.
        {
            let rec = Arc::new(RecordingInjector::new());
            let h = RemoteInputInjectorHandle::new(rec.clone());
            assert!(h.inject(RemoteAction::PointerButton {
                button: 272,
                pressed: true
            }));
            assert_eq!(rec.actions().len(), 1);
        }
        // Phase `channel_injector_forwards`.
        {
            let (tx, rx) = std::sync::mpsc::channel();
            let inj = ChannelInjector::new(tx);
            assert!(inj.inject(RemoteAction::PointerMotionRelative { dx: 1.0, dy: 2.0 }));
            let got = rx.recv().expect("action forwarded");
            assert_eq!(
                got,
                RemoteAction::PointerMotionRelative { dx: 1.0, dy: 2.0 }
            );
        }
        // Phase `null_injector_drops`.
        {
            let h = RemoteInputInjectorHandle::null();
            assert!(!h.inject(RemoteAction::PointerMotionRelative { dx: 0.0, dy: 0.0 }));
        }
    }

    #[test]
    fn remote_desktop_gate_admits_only_started_granted_devices() {
        // Phase `notify_dropped_when_session_not_started`.
        // Un-started / un-granted sessions drop `Notify*` — proves the permission gate blocks injection until the user consents + Start.
        {
            let rec = Arc::new(RecordingInjector::new());
            let backend = backend_with(RemoteInputInjectorHandle::new(rec.clone()));
            let path: ObjectPath<'_> = "/s/never".try_into().unwrap();
            backend.route(
                "",
                &path,
                device_type::POINTER,
                RemoteAction::PointerButton {
                    button: 272,
                    pressed: true,
                },
            );
            assert_eq!(rec.actions().len(), 0, "no session => no injection");
        }
        // Phase `gate_scopes_to_granted_devices`.
        // A started session with the POINTER grant routes pointer actions but still drops keyboard actions it wasn't granted.
        {
            let rec = Arc::new(RecordingInjector::new());
            let backend = backend_with(RemoteInputInjectorHandle::new(rec.clone()));
            let key = "/s/1".to_string();
            // Hand-stage a started, pointer-only session.
            backend.sessions.lock().unwrap().insert(
                key,
                RemoteDesktopSession {
                    owner: ":1.7".into(),
                    app_id: "org.remote".into(),
                    caller: None,
                    devices: device_type::POINTER,
                    consented: true,
                    selecting: false,
                    started: true,
                    clipboard_requested: false,
                    starting: false,
                    ..Default::default()
                },
            );
            let path: ObjectPath<'_> = "/s/1".try_into().unwrap();
            // Pointer is granted → routed.
            backend.route(
                ":1.7",
                &path,
                device_type::POINTER,
                RemoteAction::PointerButton {
                    button: 272,
                    pressed: true,
                },
            );
            // Keyboard is NOT granted → dropped.
            backend.route(
                ":1.7",
                &path,
                device_type::KEYBOARD,
                RemoteAction::KeyboardKeycode {
                    keycode: 30,
                    pressed: true,
                },
            );
            let actions = rec.actions();
            assert_eq!(actions.len(), 1, "only the granted pointer action routes");
            assert!(matches!(actions[0], RemoteAction::PointerButton { .. }));
        }
        // Phase `eis_actions_are_limited_to_the_granted_device_classes`.
        {
            let keyboard = RemoteAction::KeyboardKeycode {
                keycode: 30,
                pressed: true,
            };
            let pointer = RemoteAction::PointerMotionRelative { dx: 1.0, dy: 2.0 };
            assert!(action_allowed_by_devices(&keyboard, device_type::KEYBOARD));
            assert!(!action_allowed_by_devices(&keyboard, device_type::POINTER));
            assert!(action_allowed_by_devices(&pointer, device_type::POINTER));
            assert!(!action_allowed_by_devices(&pointer, device_type::KEYBOARD));
        }
    }

    #[test]
    fn remote_desktop_wire_values_follow_the_standard() {
        // Phase `state_pressed_maps_freedesktop_convention`.
        assert!(state_pressed(1));
        assert!(!state_pressed(0));
        // Phase `v2_restore_data_is_a_standard_vendor_tuple`.
        {
            let key = uuid::Uuid::new_v4().to_string();
            let mut options = HashMap::new();
            options.insert("restore_data".into(), restore_data(key.clone()).unwrap());
            assert_eq!(restore_key(&options).as_deref(), Some(key.as_str()));
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn v2_is_only_advertised_with_the_eis_transport() {
        // Keep the protocol-version promise coupled to the compiled sender
        // context implementation above: xdg-desktop-portal will only call
        // ConnectToEIS when this is two or newer.
        assert_eq!(REMOTE_DESKTOP_VERSION, 2);
    }
}

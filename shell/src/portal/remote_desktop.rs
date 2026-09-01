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
//!    into a [`RemoteAction`](crate::network::streaming::RemoteAction)
//!    and go to the [`RemoteInputInjector`]; the compositor drains that
//!    into [`State::apply_remote_desktop_action`], driving the real
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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::object_server::SignalContext;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::network::streaming::RemoteAction;
use crate::shell::state::{Capability, Expiry};

use super::{CapabilityKind, PortalContext, file_chooser::FileChooserBackend, flow};

/// Device-type bitmask (`RemoteDesktop.AvailableDeviceTypes`).
pub mod device_type {
    pub const KEYBOARD: u32 = 1;
    pub const POINTER: u32 = 2;
    pub const TOUCHSCREEN: u32 = 4;
    /// Everything the seat can be driven with. Touchscreen injection is
    /// advertised but currently maps onto pointer at the seat, so v1 gates it the same way.
    pub const ALL: u32 = KEYBOARD | POINTER | TOUCHSCREEN;
}

/// Version we advertise. v1 covers `Notify*` (keycode + keysym); the v2
/// `ConnectToEIS` libei path is a documented follow-up, left unadvertised so the frontend doesn't route EIS to us.
pub const REMOTE_DESKTOP_VERSION: u32 = 1;

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

    /// A handle that drops every action — default for portal contexts that don't host a live seat (e.g. `--portal-server-only`).
    pub fn null() -> Self {
        Self {
            inner: Arc::new(NullInjector),
        }
    }

    /// Inject a remote action into the wrapped injector.
    pub fn inject(&self, action: RemoteAction) -> bool {
        self.inner.inject(action)
    }
}

struct NullInjector;
impl RemoteInputInjector for NullInjector {
    fn inject(&self, _action: RemoteAction) -> bool {
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
    #[allow(dead_code)]
    app_id: String,
    /// Devices the user granted (bitmask). `Notify*` is gated on this.
    devices: u32,
    /// The user consented at `SelectDevices` (so `Start` needn't re-ask).
    consented: bool,
    /// `Start` has committed the session; `Notify*` only injects after.
    started: bool,
}

pub struct RemoteDesktopBackend {
    ctx: PortalContext,
    sessions: Arc<Mutex<HashMap<String, RemoteDesktopSession>>>,
}

impl RemoteDesktopBackend {
    /// Create a new `RemoteDesktop` portal backend.
    pub fn new(ctx: PortalContext) -> Self {
        Self {
            ctx,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// True if the session is started and holds the `need` device grant —
    /// every `Notify*` checks this gate; a request for an ungranted device is silently dropped.
    fn gate(&self, session_handle: &ObjectPath<'_>, need: u32) -> bool {
        self.sessions
            .lock()
            .ok()
            .and_then(|s| s.get(&session_handle.to_string()).cloned())
            .is_some_and(|sess| sess.started && (sess.devices & need != 0))
    }

    /// Decode + route one action if the gate allows it.
    fn route(&self, session_handle: &ObjectPath<'_>, need: u32, action: RemoteAction) {
        if !self.gate(session_handle, need) {
            tracing::debug!(?action, "RemoteDesktop: dropped un-granted Notify*");
            return;
        }
        self.ctx.remote_input_injector.inject(action);
    }
}

#[interface(name = "org.freedesktop.impl.portal.RemoteDesktop")]
impl RemoteDesktopBackend {
    /// Device types the seat can be driven with.
    #[zbus(property)]
    #[allow(
        clippy::unused_self,
        reason = "zbus's #[interface] macro dispatches D-Bus property getters as instance methods, even though this constant value doesn't depend on self"
    )]
    fn available_device_types(&self) -> u32 {
        device_type::ALL
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
    #[allow(
        clippy::needless_pass_by_value,
        reason = "argument types and count are fixed by the org.freedesktop.impl.portal.RemoteDesktop.CreateSession D-Bus signature; zbus deserializes owned values here rather than by reference"
    )]
    fn create_session(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!("RemoteDesktop.CreateSession: app={app_id}");
        if let Ok(mut s) = self.sessions.lock() {
            s.insert(
                session_handle.to_string(),
                RemoteDesktopSession {
                    app_id,
                    ..Default::default()
                },
            );
        }
        (0, HashMap::new())
    }

    /// `SelectDevices` — request pointer/keyboard/touch access; surfaces the consent prompt and records the granted devices on the session.
    async fn select_devices(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!("RemoteDesktop.SelectDevices: app={app_id}");
        let mut types = opt_u32(&options, "types").unwrap_or(device_type::ALL) & device_type::ALL;
        if types == 0 {
            types = device_type::ALL;
        }

        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = FileChooserBackend::resolve_caller(&self.ctx, sender.as_ref(), &app_id);
        let cap_template = Capability::RemoteDesktop {
            scope: Expiry::AfterUse,
        };
        let allowed = flow::resolve(
            &self.ctx,
            caller,
            CapabilityKind::RemoteDesktop,
            cap_template,
        )
        .await
        .is_allowed();

        if !allowed {
            return (1, HashMap::new());
        }

        if let Ok(mut s) = self.sessions.lock() {
            let entry =
                s.entry(session_handle.to_string())
                    .or_insert_with(|| RemoteDesktopSession {
                        app_id: app_id.clone(),
                        ..Default::default()
                    });
            entry.devices = types;
            entry.consented = true;
        }
        (0, HashMap::new())
    }

    /// `Start` — commit the session. Returns the granted `devices` bitmask.
    #[allow(
        clippy::too_many_arguments,
        reason = "argument list mirrors the fixed org.freedesktop.impl.portal.RemoteDesktop.Start D-Bus signature"
    )]
    async fn start(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!("RemoteDesktop.Start: app={app_id}");
        let key = session_handle.to_string();
        let session = self
            .sessions
            .lock()
            .ok()
            .and_then(|s| s.get(&key).cloned())
            .unwrap_or_default();

        // Re-check consent unless SelectDevices already secured it.
        if !session.consented {
            let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
            let caller = FileChooserBackend::resolve_caller(&self.ctx, sender.as_ref(), &app_id);
            let cap_template = Capability::RemoteDesktop {
                scope: Expiry::AfterUse,
            };
            let allowed = flow::resolve(
                &self.ctx,
                caller,
                CapabilityKind::RemoteDesktop,
                cap_template,
            )
            .await
            .is_allowed();
            if !allowed {
                return (1, HashMap::new());
            }
        }

        let devices = if session.devices == 0 {
            device_type::ALL
        } else {
            session.devices
        };
        if let Ok(mut s) = self.sessions.lock() {
            let entry = s.entry(key).or_default();
            entry.started = true;
            entry.devices = devices;
        }

        let mut results: HashMap<String, OwnedValue> = HashMap::new();
        if let Ok(v) = OwnedValue::try_from(Value::U32(devices)) {
            results.insert("devices".to_string(), v);
        }
        if let Ok(v) = OwnedValue::try_from(Value::Bool(false)) {
            results.insert("clipboard_enabled".to_string(), v);
        }
        (0, results)
    }

    /// Relative pointer motion.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "argument types are fixed by the org.freedesktop.impl.portal.RemoteDesktop.NotifyPointerMotion D-Bus signature; zbus deserializes owned values here rather than by reference"
    )]
    fn notify_pointer_motion(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        dx: f64,
        dy: f64,
    ) {
        self.route(
            &session_handle,
            device_type::POINTER,
            RemoteAction::PointerMotionRelative { dx, dy },
        );
    }

    /// Absolute pointer motion, relative to a stream node.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "argument types are fixed by the org.freedesktop.impl.portal.RemoteDesktop.NotifyPointerMotionAbsolute D-Bus signature; zbus deserializes owned values here rather than by reference"
    )]
    fn notify_pointer_motion_absolute(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        stream: u32,
        x: f64,
        y: f64,
    ) {
        self.route(
            &session_handle,
            device_type::POINTER,
            RemoteAction::PointerMotionAbsolute { stream, x, y },
        );
    }

    /// Pointer button (evdev code + state).
    #[allow(
        clippy::needless_pass_by_value,
        reason = "argument types are fixed by the org.freedesktop.impl.portal.RemoteDesktop.NotifyPointerButton D-Bus signature; zbus deserializes owned values here rather than by reference"
    )]
    fn notify_pointer_button(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        button: i32,
        state: u32,
    ) {
        self.route(
            &session_handle,
            device_type::POINTER,
            RemoteAction::PointerButton {
                button,
                pressed: state_pressed(state),
            },
        );
    }

    /// Smooth scroll.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "argument types are fixed by the org.freedesktop.impl.portal.RemoteDesktop.NotifyPointerAxis D-Bus signature; zbus deserializes owned values here rather than by reference"
    )]
    fn notify_pointer_axis(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        dx: f64,
        dy: f64,
    ) {
        self.route(
            &session_handle,
            device_type::POINTER,
            RemoteAction::PointerAxis { dx, dy },
        );
    }

    /// Discrete scroll (`axis` 0 vertical, 1 horizontal).
    #[allow(
        clippy::needless_pass_by_value,
        reason = "argument types are fixed by the org.freedesktop.impl.portal.RemoteDesktop.NotifyPointerAxisDiscrete D-Bus signature; zbus deserializes owned values here rather than by reference"
    )]
    fn notify_pointer_axis_discrete(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        axis: u32,
        steps: i32,
    ) {
        self.route(
            &session_handle,
            device_type::POINTER,
            RemoteAction::PointerAxisDiscrete { axis, steps },
        );
    }

    /// Key press/release by evdev keycode.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "argument types are fixed by the org.freedesktop.impl.portal.RemoteDesktop.NotifyKeyboardKeycode D-Bus signature; zbus deserializes owned values here rather than by reference"
    )]
    fn notify_keyboard_keycode(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        keycode: i32,
        state: u32,
    ) {
        self.route(
            &session_handle,
            device_type::KEYBOARD,
            RemoteAction::KeyboardKeycode {
                keycode,
                pressed: state_pressed(state),
            },
        );
    }

    /// Key press/release by keysym.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "argument types are fixed by the org.freedesktop.impl.portal.RemoteDesktop.NotifyKeyboardKeysym D-Bus signature; zbus deserializes owned values here rather than by reference"
    )]
    fn notify_keyboard_keysym(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        keysym: i32,
        state: u32,
    ) {
        self.route(
            &session_handle,
            device_type::KEYBOARD,
            RemoteAction::KeyboardKeysym {
                keysym,
                pressed: state_pressed(state),
            },
        );
    }
}

fn opt_u32(options: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    let v = options.get(key)?.try_clone().ok()?;
    u32::try_from(v).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::{
        CallerMap, GrantStoreHandle, PortalCaller, PromptQueueHandle, PromptResolver,
    };
    use crate::shell::state::{Workspace, WorkspaceId};

    fn empty_ws(id: u64) -> Workspace {
        // `Workspace` grew a durable room id, an owner and a capability list; the builder keeps this fixture from having to track them.
        Workspace::test_default(WorkspaceId(id), "rd")
    }

    fn ctx_with(
        injector: RemoteInputInjectorHandle,
        resolver: PromptResolver,
    ) -> (PortalContext, WorkspaceId) {
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(70));
        let queue = PromptQueueHandle::new(resolver);
        let map = CallerMap::new();
        map.force_caller_for_tests(PortalCaller::new(ws_id, "org.remote"));
        let mut ctx = PortalContext::new_with_null_spawners(store, queue, map);
        ctx.remote_input_injector = injector;
        (ctx, ws_id)
    }

    #[test]
    fn recording_injector_captures_actions() {
        let rec = Arc::new(RecordingInjector::new());
        let h = RemoteInputInjectorHandle::new(rec.clone());
        assert!(h.inject(RemoteAction::PointerButton {
            button: 272,
            pressed: true
        }));
        assert_eq!(rec.actions().len(), 1);
    }

    #[test]
    fn channel_injector_forwards() {
        let (tx, rx) = std::sync::mpsc::channel();
        let inj = ChannelInjector::new(tx);
        assert!(inj.inject(RemoteAction::PointerMotionRelative { dx: 1.0, dy: 2.0 }));
        let got = rx.recv().expect("action forwarded");
        assert_eq!(
            got,
            RemoteAction::PointerMotionRelative { dx: 1.0, dy: 2.0 }
        );
    }

    #[test]
    fn null_injector_drops() {
        let h = RemoteInputInjectorHandle::null();
        assert!(!h.inject(RemoteAction::PointerMotionRelative { dx: 0.0, dy: 0.0 }));
    }

    /// Un-started / un-granted sessions drop `Notify*` — proves the permission gate blocks injection until the user consents + Start.
    #[test]
    fn notify_dropped_when_session_not_started() {
        let rec = Arc::new(RecordingInjector::new());
        let (ctx, _ws) = ctx_with(
            RemoteInputInjectorHandle::new(rec.clone()),
            PromptResolver::AutoAllow,
        );
        let backend = RemoteDesktopBackend::new(ctx);
        let path: ObjectPath<'_> = "/s/never".try_into().unwrap();
        backend.route(
            &path,
            device_type::POINTER,
            RemoteAction::PointerButton {
                button: 272,
                pressed: true,
            },
        );
        assert_eq!(rec.actions().len(), 0, "no session => no injection");
    }

    /// A started session with the POINTER grant routes pointer actions but still drops keyboard actions it wasn't granted.
    #[test]
    fn gate_scopes_to_granted_devices() {
        let rec = Arc::new(RecordingInjector::new());
        let (ctx, _ws) = ctx_with(
            RemoteInputInjectorHandle::new(rec.clone()),
            PromptResolver::AutoAllow,
        );
        let backend = RemoteDesktopBackend::new(ctx);
        let key = "/s/1".to_string();
        // Hand-stage a started, pointer-only session.
        backend.sessions.lock().unwrap().insert(
            key,
            RemoteDesktopSession {
                app_id: "org.remote".into(),
                devices: device_type::POINTER,
                consented: true,
                started: true,
            },
        );
        let path: ObjectPath<'_> = "/s/1".try_into().unwrap();
        // Pointer is granted → routed.
        backend.route(
            &path,
            device_type::POINTER,
            RemoteAction::PointerButton {
                button: 272,
                pressed: true,
            },
        );
        // Keyboard is NOT granted → dropped.
        backend.route(
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

    #[test]
    fn state_pressed_maps_freedesktop_convention() {
        assert!(state_pressed(1));
        assert!(!state_pressed(0));
    }
}

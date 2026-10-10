//! `org.freedesktop.impl.portal.InputCapture` backed by an in-process EIS server.
//!
//! The compositor handle is the authority that arms barriers and diverts input;
//! the D-Bus service can only authenticate and configure bounded sessions.

#![allow(
    clippy::used_underscore_binding,
    reason = "portal XML fixes these parameter names"
)]

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd as StdOwnedFd;
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use reis::calloop::{EisRequestSource, EisRequestSourceEvent};
#[cfg(target_os = "linux")]
use reis::enumflags2::BitFlags;
#[cfg(target_os = "linux")]
use reis::request::{Connection as EisConnection, Device, DeviceCapability, EisRequest, Seat};
#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::names::OwnedUniqueName;
#[cfg(target_os = "linux")]
use zbus::object_server::SignalEmitter;
#[cfg(target_os = "linux")]
use zbus::zvariant::{ObjectPath, OwnedFd};
use zbus::zvariant::{OwnedValue, Value};

use super::PortalCaller;
#[cfg(target_os = "linux")]
use super::auth::authenticate_call;
#[cfg(target_os = "linux")]
use super::lifecycle::{LifecycleEntry, LifecycleResource, PortalLifecycle};
#[cfg(target_os = "linux")]
use super::{CapabilityKind, PortalContext, flow};
#[cfg(target_os = "linux")]
use crate::shell::state::{Capability, Expiry};

#[cfg(target_os = "linux")]
const MAX_SESSIONS: usize = 32;
#[cfg(target_os = "linux")]
const MAX_BARRIERS: usize = 64;

pub mod capability {
    pub const KEYBOARD: u32 = 1;
    pub const POINTER: u32 = 2;
    pub const ALL: u32 = KEYBOARD | POINTER;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaptureZone {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CapturedInput {
    PointerMotion { x: f64, y: f64, dx: f64, dy: f64 },
    PointerButton { button: u32, pressed: bool },
    Scroll { dx: f32, dy: f32 },
    Keyboard { keycode: u32, pressed: bool },
}

#[derive(Clone, Debug)]
struct PointerBarrier {
    id: u32,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
}
impl PointerBarrier {
    fn crossed(&self, x: f64, y: f64, previous: (f64, f64)) -> bool {
        let (px, py) = previous;
        if self.x1 == self.x2 {
            let bx = f64::from(self.x1);
            (px - bx) * (x - bx) <= 0.
                && py.max(y) >= f64::from(self.y1.min(self.y2))
                && py.min(y) <= f64::from(self.y1.max(self.y2))
        } else {
            let by = f64::from(self.y1);
            (py - by) * (y - by) <= 0.
                && px.max(x) >= f64::from(self.x1.min(self.x2))
                && px.min(x) <= f64::from(self.x1.max(self.x2))
        }
    }
}

#[derive(Clone)]
#[cfg(target_os = "linux")]
struct EisTransport {
    tx: smithay::reexports::calloop::channel::Sender<EisCommand>,
}
#[derive(Clone, Copy)]
#[cfg(target_os = "linux")]
enum EisCommand {
    Activate(u32),
    Deactivate,
    Input(CapturedInput),
    Close,
}
#[cfg(target_os = "linux")]
impl EisTransport {
    fn send(&self, command: EisCommand) {
        let _ = self.tx.send(command);
    }
}

#[derive(Clone, Debug)]
enum PortalSignal {
    Activated {
        #[cfg_attr(
            not(target_os = "linux"),
            expect(
                dead_code,
                reason = "the authenticated Linux portal adapter consumes this shared model; retain its complete cross-host shape"
            )
        )]
        session: String,
        #[cfg_attr(
            not(target_os = "linux"),
            expect(
                dead_code,
                reason = "the authenticated Linux portal adapter consumes this shared model; retain its complete cross-host shape"
            )
        )]
        activation_id: u32,
        #[cfg_attr(
            not(target_os = "linux"),
            expect(
                dead_code,
                reason = "the authenticated Linux portal adapter consumes this shared model; retain its complete cross-host shape"
            )
        )]
        cursor: (f64, f64),
        barrier_id: u32,
    },
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            dead_code,
            reason = "the authenticated Linux portal adapter consumes this shared model; retain its complete cross-host shape"
        )
    )]
    Deactivated {
        session: String,
        activation_id: u32,
        cursor: (f64, f64),
    },
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            dead_code,
            reason = "the authenticated Linux portal adapter consumes this shared model; retain its complete cross-host shape"
        )
    )]
    Disabled { session: String },
    ZonesChanged {
        #[cfg_attr(
            not(target_os = "linux"),
            expect(
                dead_code,
                reason = "the authenticated Linux portal adapter consumes this shared model; retain its complete cross-host shape"
            )
        )]
        session: String,
    },
}
#[derive(Clone)]
struct CaptureSession {
    owner: String,
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            dead_code,
            reason = "the authenticated Linux portal adapter consumes this shared model; retain its complete cross-host shape"
        )
    )]
    app_id: String,
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            dead_code,
            reason = "the authenticated Linux portal adapter consumes this shared model; retain its complete cross-host shape"
        )
    )]
    caller: PortalCaller,
    started: bool,
    enabled: bool,
    active: bool,
    /// Requested through Clipboard before Start. Access itself is live only
    /// while `active` has been armed by a barrier.
    clipboard_requested: bool,
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            dead_code,
            reason = "the authenticated Linux portal adapter consumes this shared model; retain its complete cross-host shape"
        )
    )]
    capabilities: u32,
    zone_set: u32,
    barriers: Vec<PointerBarrier>,
    #[cfg(target_os = "linux")]
    transport: Option<EisTransport>,
    activation_id: u32,
    cursor: (f64, f64),
}
impl CaptureSession {
    fn new(owner: String, app_id: String, caller: PortalCaller) -> Self {
        Self {
            owner,
            app_id,
            caller,
            started: false,
            enabled: false,
            active: false,
            clipboard_requested: false,
            capabilities: 0,
            zone_set: 1,
            barriers: vec![],
            #[cfg(target_os = "linux")]
            transport: None,
            activation_id: 0,
            cursor: (0., 0.),
        }
    }
}
#[derive(Default)]
struct CaptureState {
    available: bool,
    zones: Vec<CaptureZone>,
    zone_set: u32,
    sessions: HashMap<String, CaptureSession>,
    signals: Vec<PortalSignal>,
}

/// Compositor-facing input capture authority.  Hosted sessions use
/// [`unavailable`](Self::unavailable), which prevents false capability claims.
#[derive(Clone)]
pub struct InputCaptureCompositorHandle {
    state: Arc<Mutex<CaptureState>>,
    /// Parked by the portal signal emitter until a signal is queued.
    wake: crate::idle_wake::IdleWake,
}
impl Default for InputCaptureCompositorHandle {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(CaptureState::default())),
            wake: crate::idle_wake::IdleWake::default(),
        }
    }
}
impl InputCaptureCompositorHandle {
    pub fn unavailable() -> Self {
        Self::default()
    }
    pub fn native(zones: Vec<CaptureZone>) -> Self {
        Self {
            state: Arc::new(Mutex::new(CaptureState {
                available: !zones.is_empty(),
                zones,
                zone_set: 1,
                sessions: HashMap::new(),
                signals: vec![],
            })),
            wake: crate::idle_wake::IdleWake::default(),
        }
    }
    pub fn available(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.available)
    }
    pub fn set_zones(&self, zones: Vec<CaptureZone>) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.available = !zones.is_empty();
        state.zones = zones;
        state.zone_set = state.zone_set.wrapping_add(1).max(1);
        let zone_set = state.zone_set;
        let paths = state.sessions.keys().cloned().collect::<Vec<_>>();
        for session in state.sessions.values_mut() {
            session.zone_set = zone_set;
            session.barriers.clear();
            if session.active {
                #[cfg(target_os = "linux")]
                if let Some(transport) = &session.transport {
                    transport.send(EisCommand::Deactivate);
                }
                session.active = false;
            }
        }
        state.signals.extend(
            paths
                .into_iter()
                .map(|session| PortalSignal::ZonesChanged { session }),
        );
    }
    /// Returns true iff Tilo must not deliver this event locally.
    pub fn observe(&self, input: CapturedInput) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if !state.available {
            return false;
        }
        // Input capture is exclusive. Once a session is active, no other
        // app may see its keystrokes or pointer motion, even if another app
        // registered an overlapping barrier.
        if let Some(path) = state
            .sessions
            .iter()
            .find(|(_, session)| session.started && session.enabled && session.active)
            .map(|(path, _)| path.clone())
        {
            if let Some(session) = state.sessions.get_mut(&path) {
                if let CapturedInput::PointerMotion { x, y, .. } = input {
                    session.cursor = (x, y);
                }
                #[cfg(target_os = "linux")]
                if let Some(transport) = &session.transport {
                    transport.send(EisCommand::Input(input));
                }
            }
            return true;
        }
        let CapturedInput::PointerMotion { x, y, .. } = input else {
            return false;
        };
        // Deterministic tie-break for overlapping barriers; a HashMap's
        // iteration order must never decide which app gets physical input.
        let candidate = state
            .sessions
            .iter_mut()
            .filter_map(|(path, session)| {
                if !session.started || !session.enabled {
                    return None;
                }
                let previous = session.cursor;
                session.cursor = (x, y);
                session
                    .barriers
                    .iter()
                    .find(|barrier| barrier.crossed(x, y, previous))
                    .cloned()
                    .map(|barrier| (path.clone(), barrier))
            })
            .min_by(|a, b| a.0.cmp(&b.0));
        let Some((path, barrier)) = candidate else {
            return false;
        };
        let Some(session) = state.sessions.get_mut(&path) else {
            return false;
        };
        session.active = true;
        session.activation_id = session.activation_id.wrapping_add(1).max(1);
        #[cfg(target_os = "linux")]
        if let Some(transport) = &session.transport {
            transport.send(EisCommand::Activate(session.activation_id));
        }
        let activation_id = session.activation_id;
        state.signals.push(PortalSignal::Activated {
            session: path,
            activation_id,
            cursor: (x, y),
            barrier_id: barrier.id,
        });
        self.wake.poke();
        true
    }
    fn drain_signals(&self) -> Vec<PortalSignal> {
        self.state
            .lock()
            .map(|mut state| std::mem::take(&mut state.signals))
            .unwrap_or_default()
    }
}

#[cfg(target_os = "linux")]
pub struct InputCaptureBackend {
    ctx: PortalContext,
    lifecycle: PortalLifecycle,
    compositor: InputCaptureCompositorHandle,
    connection: zbus::Connection,
}

/// Narrow bridge used by Clipboard; it never exposes input capture state to
/// apps and only answers ownership/liveness checks for existing session paths.
#[derive(Clone)]
pub struct InputCaptureSessionHandle {
    state: Arc<Mutex<CaptureState>>,
}
#[cfg(target_os = "linux")]
impl InputCaptureBackend {
    /// `connection` is the bus this backend is served on; its request and
    /// session objects are registered there.
    pub fn new(
        ctx: PortalContext,
        lifecycle: PortalLifecycle,
        connection: zbus::Connection,
    ) -> Self {
        Self {
            compositor: ctx.input_capture.clone(),
            ctx,
            lifecycle,
            connection,
        }
    }
    pub fn spawn_signal_emitter(&self) {
        spawn_compositor_emitter(self.compositor.clone(), self.connection.clone());
    }
    pub fn session_handle(&self) -> InputCaptureSessionHandle {
        InputCaptureSessionHandle {
            state: self.compositor.state.clone(),
        }
    }
    fn session_for(&self, key: &str, owner: &str, app_id: &str, caller: &PortalCaller) -> bool {
        self.compositor
            .state
            .lock()
            .ok()
            .and_then(|state| state.sessions.get(key).cloned())
            .is_some_and(|session| {
                session.owner == owner && session.app_id == app_id && &session.caller == caller
            })
    }
}

impl InputCaptureSessionHandle {
    fn request_clipboard(&self, key: &str, owner: &str) -> bool {
        self.state.lock().ok().is_some_and(|mut state| {
            state.sessions.get_mut(key).is_some_and(|session| {
                if session.owner != owner || session.started {
                    return false;
                }
                session.clipboard_requested = true;
                true
            })
        })
    }
    fn clipboard_active(&self, key: &str, owner: &str) -> bool {
        self.state.lock().ok().is_some_and(|state| {
            state.sessions.get(key).is_some_and(|session| {
                session.owner == owner
                    && session.started
                    && session.clipboard_requested
                    && session.active
            })
        })
    }
    fn clipboard_live(&self, key: &str) -> bool {
        self.state.lock().ok().is_some_and(|state| {
            state.sessions.get(key).is_some_and(|session| {
                session.started && session.clipboard_requested && session.active
            })
        })
    }
    fn clipboard_sessions(&self) -> Vec<String> {
        self.state.lock().map_or_else(
            |_| Vec::new(),
            |state| {
                state
                    .sessions
                    .iter()
                    .filter(|(_, session)| {
                        session.started && session.clipboard_requested && session.active
                    })
                    .map(|(path, _)| path.clone())
                    .collect()
            },
        )
    }
}

impl super::clipboard::ClipboardSessionProvider for InputCaptureSessionHandle {
    fn request_clipboard(&self, session: &str, owner: &str) -> bool {
        Self::request_clipboard(self, session, owner)
    }
    fn clipboard_active(&self, session: &str, owner: &str) -> bool {
        Self::clipboard_active(self, session, owner)
    }
    fn clipboard_live(&self, session: &str) -> bool {
        Self::clipboard_live(self, session)
    }
    fn clipboard_sessions(&self) -> Vec<String> {
        Self::clipboard_sessions(self)
    }
}

#[interface(name = "org.freedesktop.impl.portal.InputCapture")]
#[cfg(target_os = "linux")]
impl InputCaptureBackend {
    #[zbus(property, name = "SupportedCapabilities")]
    fn supported_capabilities(&self) -> u32 {
        if self.compositor.available() {
            capability::ALL
        } else {
            0
        }
    }
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        2
    }
    async fn create_session(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let requested = opt_u32(&options, "capabilities").unwrap_or(0) & capability::ALL;
        if requested == 0 {
            return (2, HashMap::new());
        }
        self.create_legacy(
            header,
            handle,
            session_handle,
            app_id,
            Some(parent_window),
            requested,
        )
        .await
    }
    async fn create_session2(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> HashMap<String, OwnedValue> {
        if !self.compositor.available() {
            return HashMap::new();
        }
        let sender = header.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) = authenticate_call(&self.ctx, sender.as_ref(), &app_id, None).await else {
            return HashMap::new();
        };
        let key = session_handle.to_string();
        let inserted = self.compositor.state.lock().ok().is_some_and(|mut state| {
            if state.sessions.len() >= MAX_SESSIONS || state.sessions.contains_key(&key) {
                return false;
            }
            state.sessions.insert(
                key.clone(),
                CaptureSession::new(auth.owner.to_string(), app_id.clone(), auth.caller.clone()),
            );
            true
        });
        if !inserted {
            return HashMap::new();
        }
        let state = self.compositor.state.clone();
        let cleanup_key = key.clone();
        let entry = LifecycleEntry {
            owner: auth.owner.to_string(),
            app_id: app_id.clone(),
            resource: LifecycleResource::InputCapture,
        };
        if !matches!(
            self.lifecycle
                .register_session(&self.connection, &key, entry, move |_| close_session(
                    &state,
                    &cleanup_key
                ))
                .await,
            Ok(true)
        ) {
            close_session(&self.compositor.state, &key);
            return HashMap::new();
        }
        let mut results = HashMap::new();
        insert_value(&mut results, "capabilities", Value::U32(capability::ALL));
        results
    }
    async fn start(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let requested = opt_u32(&options, "capabilities").unwrap_or(0) & capability::ALL;
        let requested_persist_mode = match requested_persist_mode(&options) {
            Ok(mode) => mode,
            Err(()) => return (2, HashMap::new()),
        };
        if requested == 0 || !self.compositor.available() {
            return (2, HashMap::new());
        }
        let sender = header.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) =
            authenticate_call(&self.ctx, sender.as_ref(), &app_id, Some(&parent_window)).await
        else {
            return (2, HashMap::new());
        };
        let key = session_handle.to_string();
        if !self.session_for(&key, auth.owner.as_str(), &app_id, &auth.caller) {
            return (2, HashMap::new());
        }
        let request = handle.to_string();
        let entry = LifecycleEntry {
            owner: auth.owner.to_string(),
            app_id: app_id.clone(),
            resource: LifecycleResource::Prompt,
        };
        if !matches!(
            self.lifecycle
                .register_request(&self.connection, &request, entry, |_| {})
                .await,
            Ok(true)
        ) {
            return (2, HashMap::new());
        }
        let allowed = flow::resolve_local(
            &self.ctx,
            auth.caller.clone(),
            CapabilityKind::InputCapture,
            Capability::RemoteDesktop {
                scope: Expiry::AfterUse,
            },
        )
        .await
        .is_allowed();
        self.lifecycle
            .complete_request_object(&self.connection, &request)
            .await;
        if !allowed {
            return (1, HashMap::new());
        }
        let started = self.compositor.state.lock().ok().is_some_and(|mut state| {
            let zone_set = state.zone_set;
            state.sessions.get_mut(&key).is_some_and(|session| {
                if session.started {
                    return false;
                }
                session.started = true;
                session.capabilities = requested;
                session.zone_set = zone_set;
                true
            })
        });
        if !started {
            return (2, HashMap::new());
        }
        let mut results = HashMap::new();
        insert_value(&mut results, "capabilities", Value::U32(requested));
        let clipboard_enabled = self.compositor.state.lock().ok().is_some_and(|state| {
            state
                .sessions
                .get(&key)
                .is_some_and(|session| session.clipboard_requested)
        });
        insert_value(
            &mut results,
            "clipboard_enabled",
            Value::Bool(clipboard_enabled),
        );
        // Tilo currently has no durable input-capture restore store. Explicitly
        // declining persistence is preferable to silently accepting it: the
        // caller can see that it must prompt again next time.
        if requested_persist_mode != 0 {
            insert_value(&mut results, "persist_mode", Value::U32(0));
        }
        (0, results)
    }
    async fn get_zones(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let sender = header.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) = authenticate_call(&self.ctx, sender.as_ref(), &app_id, None).await else {
            return (2, HashMap::new());
        };
        let key = session_handle.to_string();
        if !self.session_for(&key, auth.owner.as_str(), &app_id, &auth.caller) {
            return (2, HashMap::new());
        }
        let request = handle.to_string();
        let entry = LifecycleEntry {
            owner: auth.owner.to_string(),
            app_id,
            resource: LifecycleResource::Prompt,
        };
        if !matches!(
            self.lifecycle
                .register_request(&self.connection, &request, entry, |_| {})
                .await,
            Ok(true)
        ) {
            return (2, HashMap::new());
        }
        let (zones, zone_set) = self
            .compositor
            .state
            .lock()
            .map(|state| (state.zones.clone(), state.zone_set))
            .unwrap_or_default();
        self.lifecycle
            .complete_request_object(&self.connection, &request)
            .await;
        let mut results = HashMap::new();
        insert_value(
            &mut results,
            "zones",
            Value::from(
                zones
                    .into_iter()
                    .map(|z| (z.width, z.height, z.x, z.y))
                    .collect::<Vec<_>>(),
            ),
        );
        insert_value(&mut results, "zone_set", Value::U32(zone_set));
        (0, results)
    }
    async fn set_pointer_barriers(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
        barriers: Vec<HashMap<String, OwnedValue>>,
        zone_set: u32,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let sender = header.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) = authenticate_call(&self.ctx, sender.as_ref(), &app_id, None).await else {
            return (2, HashMap::new());
        };
        let key = session_handle.to_string();
        if !self.session_for(&key, auth.owner.as_str(), &app_id, &auth.caller)
            || barriers.len() > MAX_BARRIERS
        {
            return (2, HashMap::new());
        }
        let request = handle.to_string();
        let entry = LifecycleEntry {
            owner: auth.owner.to_string(),
            app_id,
            resource: LifecycleResource::Prompt,
        };
        if !matches!(
            self.lifecycle
                .register_request(&self.connection, &request, entry, |_| {})
                .await,
            Ok(true)
        ) {
            return (2, HashMap::new());
        }
        let (zones, current_zone_set) = self
            .compositor
            .state
            .lock()
            .map(|state| (state.zones.clone(), state.zone_set))
            .unwrap_or_default();
        let mut accepted = vec![];
        let mut failed = vec![];
        for raw in barriers {
            match parse_barrier(&raw)
                .filter(|barrier| zone_set == current_zone_set && valid_barrier(barrier, &zones))
            {
                Some(barrier) => accepted.push(barrier),
                None => failed.push(opt_u32(&raw, "barrier_id").unwrap_or(0)),
            }
        }
        if let Ok(mut state) = self.compositor.state.lock()
            && let Some(session) = state.sessions.get_mut(&key)
        {
            session.barriers = accepted;
            session.zone_set = current_zone_set;
        }
        self.lifecycle
            .complete_request_object(&self.connection, &request)
            .await;
        let mut results = HashMap::new();
        insert_value(&mut results, "failed_barriers", Value::from(failed));
        (0, results)
    }
    async fn enable(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        self.set_enabled(header, &session_handle, &app_id, true)
            .await
    }
    async fn disable(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        self.set_enabled(header, &session_handle, &app_id, false)
            .await
    }
    async fn release(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        self.deactivate(header, &session_handle, &app_id).await
    }
    async fn connect_to_eis(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<OwnedFd> {
        let sender = header.sender().cloned().map(OwnedUniqueName::from);
        let auth = authenticate_call(&self.ctx, sender.as_ref(), &app_id, None)
            .await
            .map_err(|_| zbus::fdo::Error::AccessDenied("untrusted InputCapture caller".into()))?;
        let key = session_handle.to_string();
        if !self.session_for(&key, auth.owner.as_str(), &app_id, &auth.caller) {
            return Err(zbus::fdo::Error::AccessDenied(
                "session is not owned by this caller".into(),
            ));
        }
        let (transport, fd) =
            make_eis_transport().map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        let installed = self.compositor.state.lock().ok().is_some_and(|mut state| {
            state.sessions.get_mut(&key).is_some_and(|session| {
                if !session.started || session.transport.is_some() {
                    return false;
                }
                session.transport = Some(transport);
                true
            })
        });
        if !installed {
            return Err(zbus::fdo::Error::Failed(
                "EIS requires a started session and is one-shot".into(),
            ));
        }
        Ok(OwnedFd::from(fd))
    }
    #[zbus(signal)]
    async fn disabled(
        ctxt: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn activated(
        ctxt: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn deactivated(
        ctxt: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn zones_changed(
        ctxt: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;
}

#[cfg(target_os = "linux")]
impl InputCaptureBackend {
    async fn create_legacy(
        &self,
        header: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        parent: Option<String>,
        requested: u32,
    ) -> (u32, HashMap<String, OwnedValue>) {
        if !self.compositor.available() {
            return (2, HashMap::new());
        }
        let sender = header.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) =
            authenticate_call(&self.ctx, sender.as_ref(), &app_id, parent.as_deref()).await
        else {
            return (2, HashMap::new());
        };
        let key = session_handle.to_string();
        let inserted = self.compositor.state.lock().ok().is_some_and(|mut state| {
            if state.sessions.len() >= MAX_SESSIONS || state.sessions.contains_key(&key) {
                return false;
            }
            let mut session =
                CaptureSession::new(auth.owner.to_string(), app_id.clone(), auth.caller.clone());
            session.started = true;
            session.capabilities = requested;
            session.zone_set = state.zone_set;
            state.sessions.insert(key.clone(), session);
            true
        });
        if !inserted {
            return (2, HashMap::new());
        }
        let state = self.compositor.state.clone();
        let cleanup_key = key.clone();
        let session_entry = LifecycleEntry {
            owner: auth.owner.to_string(),
            app_id: app_id.clone(),
            resource: LifecycleResource::InputCapture,
        };
        if !matches!(
            self.lifecycle
                .register_session(&self.connection, &key, session_entry, move |_| {
                    close_session(&state, &cleanup_key)
                })
                .await,
            Ok(true)
        ) {
            close_session(&self.compositor.state, &key);
            return (2, HashMap::new());
        }
        let request = handle.to_string();
        let request_entry = LifecycleEntry {
            owner: auth.owner.to_string(),
            app_id,
            resource: LifecycleResource::Prompt,
        };
        if !matches!(
            self.lifecycle
                .register_request(&self.connection, &request, request_entry, |_| {})
                .await,
            Ok(true)
        ) {
            return (2, HashMap::new());
        }
        self.lifecycle
            .complete_request_object(&self.connection, &request)
            .await;
        let mut results = HashMap::new();
        insert_value(&mut results, "capabilities", Value::U32(requested));
        (0, results)
    }
    async fn set_enabled(
        &self,
        header: zbus::message::Header<'_>,
        session_handle: &ObjectPath<'_>,
        app_id: &str,
        enabled: bool,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let sender = header.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) = authenticate_call(&self.ctx, sender.as_ref(), app_id, None).await else {
            return (2, HashMap::new());
        };
        let key = session_handle.to_string();
        let changed = self.compositor.state.lock().ok().is_some_and(|mut state| {
            let deactivated = {
                let Some(session) = state.sessions.get_mut(&key) else {
                    return false;
                };
                if session.owner != auth.owner.as_str()
                    || session.app_id != app_id
                    || session.caller != auth.caller
                    || !session.started
                    || (enabled && session.transport.is_none())
                {
                    return false;
                }
                let deactivated = (!enabled && session.active).then(|| {
                    #[cfg(target_os = "linux")]
                    if let Some(transport) = &session.transport {
                        transport.send(EisCommand::Deactivate);
                    }
                    session.active = false;
                    PortalSignal::Deactivated {
                        session: key.clone(),
                        activation_id: session.activation_id,
                        cursor: session.cursor,
                    }
                });
                session.enabled = enabled;
                deactivated
            };
            if let Some(signal) = deactivated {
                state.signals.push(signal);
            }
            if !enabled {
                state.signals.push(PortalSignal::Disabled {
                    session: key.clone(),
                });
            }
            self.compositor.wake.poke();
            true
        });
        (if changed { 0 } else { 2 }, HashMap::new())
    }
    async fn deactivate(
        &self,
        header: zbus::message::Header<'_>,
        session_handle: &ObjectPath<'_>,
        app_id: &str,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let sender = header.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) = authenticate_call(&self.ctx, sender.as_ref(), app_id, None).await else {
            return (2, HashMap::new());
        };
        let key = session_handle.to_string();
        let changed = self.compositor.state.lock().ok().is_some_and(|mut state| {
            let signal = {
                let Some(session) = state.sessions.get_mut(&key) else {
                    return false;
                };
                if session.owner != auth.owner.as_str()
                    || session.app_id != app_id
                    || session.caller != auth.caller
                    || !session.active
                {
                    return false;
                }
                #[cfg(target_os = "linux")]
                if let Some(transport) = &session.transport {
                    transport.send(EisCommand::Deactivate);
                }
                session.active = false;
                PortalSignal::Deactivated {
                    session: key.clone(),
                    activation_id: session.activation_id,
                    cursor: session.cursor,
                }
            };
            state.signals.push(signal);
            self.compositor.wake.poke();
            true
        });
        (if changed { 0 } else { 2 }, HashMap::new())
    }
}

#[cfg(target_os = "linux")]
fn close_session(state: &Arc<Mutex<CaptureState>>, key: &str) {
    if let Ok(mut state) = state.lock()
        && let Some(session) = state.sessions.remove(key)
        && let Some(transport) = session.transport
    {
        transport.send(EisCommand::Close);
    }
}
#[cfg(target_os = "linux")]
fn parse_barrier(raw: &HashMap<String, OwnedValue>) -> Option<PointerBarrier> {
    let id = opt_u32(raw, "barrier_id")?;
    if id == 0 {
        return None;
    }
    let (x1, y1, x2, y2) =
        <(i32, i32, i32, i32)>::try_from(raw.get("position")?.try_clone().ok()?).ok()?;
    ((x1 == x2) ^ (y1 == y2)).then_some(PointerBarrier { id, x1, y1, x2, y2 })
}
fn valid_barrier(barrier: &PointerBarrier, zones: &[CaptureZone]) -> bool {
    zones.iter().any(|zone| {
        let right = zone.x.saturating_add_unsigned(zone.width);
        let bottom = zone.y.saturating_add_unsigned(zone.height);
        let x1 = barrier.x1.min(barrier.x2);
        let x2 = barrier.x1.max(barrier.x2);
        let y1 = barrier.y1.min(barrier.y2);
        let y2 = barrier.y1.max(barrier.y2);
        let inside = x1 >= zone.x && x2 <= right && y1 >= zone.y && y2 <= bottom;
        let vertical_edge = barrier.x1 == barrier.x2
            && barrier.y1 != barrier.y2
            && (barrier.x1 == zone.x || barrier.x1 == right);
        let horizontal_edge = barrier.y1 == barrier.y2
            && barrier.x1 != barrier.x2
            && (barrier.y1 == zone.y || barrier.y1 == bottom);
        inside && (vertical_edge || horizontal_edge)
    })
}
fn opt_u32(values: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    u32::try_from(values.get(key)?.try_clone().ok()?).ok()
}

/// InputCapture v2 accepts the common persistence vocabulary. Until Tilo has
/// a durable, revocable restore store, a valid request is answered with the
/// effective mode zero in `Start` rather than a misleading restore record.
fn requested_persist_mode(values: &HashMap<String, OwnedValue>) -> Result<u32, ()> {
    let mode = opt_u32(values, "persist_mode").unwrap_or(0);
    (mode <= 2).then_some(mode).ok_or(())
}
fn insert_value(values: &mut HashMap<String, OwnedValue>, key: &str, value: Value<'_>) {
    if let Ok(value) = OwnedValue::try_from(value) {
        values.insert(key.into(), value);
    }
}

#[cfg(target_os = "linux")]
fn spawn_compositor_emitter(
    compositor: InputCaptureCompositorHandle,
    connection: zbus::Connection,
) {
    tokio::spawn(async move {
        loop {
            let signals = compositor.drain_signals();
            if signals.is_empty() {
                compositor.wake.wait().await;
                continue;
            }
            for signal in signals {
                emit_signal(&connection, signal).await;
            }
        }
    });
}

#[cfg(target_os = "linux")]
async fn emit_signal(connection: &zbus::Connection, signal: PortalSignal) {
    let context = match SignalEmitter::new(connection, super::OBJECT_PATH) {
        Ok(context) => context,
        Err(_) => return,
    };
    match signal {
        PortalSignal::Activated {
            session,
            activation_id,
            cursor,
            barrier_id,
        } => {
            let mut options = HashMap::new();
            insert_value(&mut options, "activation_id", Value::U32(activation_id));
            insert_value(
                &mut options,
                "cursor_position",
                Value::from((cursor.0, cursor.1)),
            );
            insert_value(&mut options, "barrier_id", Value::U32(barrier_id));
            if let Ok(path) = ObjectPath::try_from(session.as_str()) {
                let _ = InputCaptureBackend::activated(&context, path, options).await;
            }
        }
        PortalSignal::Deactivated {
            session,
            activation_id,
            cursor,
        } => {
            let mut options = HashMap::new();
            insert_value(&mut options, "activation_id", Value::U32(activation_id));
            insert_value(
                &mut options,
                "cursor_position",
                Value::from((cursor.0, cursor.1)),
            );
            if let Ok(path) = ObjectPath::try_from(session.as_str()) {
                let _ = InputCaptureBackend::deactivated(&context, path, options).await;
            }
        }
        PortalSignal::Disabled { session } => {
            if let Ok(path) = ObjectPath::try_from(session.as_str()) {
                let _ = InputCaptureBackend::disabled(&context, path, HashMap::new()).await;
            }
        }
        PortalSignal::ZonesChanged { session } => {
            if let Ok(path) = ObjectPath::try_from(session.as_str()) {
                let _ = InputCaptureBackend::zones_changed(&context, path, HashMap::new()).await;
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn make_eis_transport() -> io::Result<(EisTransport, StdOwnedFd)> {
    let (server, client) = UnixStream::pair()?;
    let (tx, commands) = smithay::reexports::calloop::channel::channel();
    std::thread::Builder::new()
        .name("tilo-input-capture-eis".into())
        .spawn(move || run_eis(server, commands))?;
    Ok((EisTransport { tx }, StdOwnedFd::from(client)))
}
#[derive(Default)]
#[cfg(target_os = "linux")]
struct EisState {
    connection: Option<EisServerConnection>,
    active: bool,
    running: bool,
    sequence: u32,
}
#[cfg(target_os = "linux")]
struct EisServerConnection {
    connection: EisConnection,
    keyboard: Option<Device>,
    pointer: Option<Device>,
}
#[cfg(target_os = "linux")]
fn run_eis(
    stream: UnixStream,
    commands: smithay::reexports::calloop::channel::Channel<EisCommand>,
) {
    let Ok(context) = reis::eis::Context::new(stream) else {
        return;
    };
    let Ok(mut event_loop) = smithay::reexports::calloop::EventLoop::<EisState>::try_new() else {
        return;
    };
    let handle = event_loop.handle();
    if handle
        .insert_source(
            EisRequestSource::new(context, 1),
            |event, connection, state| {
                match event {
                    Ok(EisRequestSourceEvent::Connected) => {
                        // A receiver client cannot bind devices until the EIS
                        // server has advertised a seat and its capabilities.
                        // The connection owns the seat object thereafter.
                        let _seat = connection.add_seat(
                            Some("tilo"),
                            DeviceCapability::Keyboard
                                | DeviceCapability::Pointer
                                | DeviceCapability::Button
                                | DeviceCapability::Scroll,
                        );
                        state.connection = Some(EisServerConnection {
                            connection: connection.clone(),
                            keyboard: None,
                            pointer: None,
                        });
                        let _ = connection.flush();
                    }
                    Ok(EisRequestSourceEvent::Request(EisRequest::Bind(bind))) => {
                        if let Some(current) = &mut state.connection {
                            if current.keyboard.is_none()
                                && bind.capabilities.contains(DeviceCapability::Keyboard)
                            {
                                current.keyboard = Some(add_device(
                                    &bind.seat,
                                    "tilo keyboard",
                                    DeviceCapability::Keyboard.into(),
                                    state.sequence,
                                ));
                            }
                            if current.pointer.is_none()
                                && bind.capabilities.contains(DeviceCapability::Pointer)
                            {
                                current.pointer = Some(add_device(
                                    &bind.seat,
                                    "tilo pointer",
                                    DeviceCapability::Pointer
                                        | DeviceCapability::Button
                                        | DeviceCapability::Scroll,
                                    state.sequence,
                                ));
                            }
                            let _ = current.connection.flush();
                        }
                    }
                    Ok(EisRequestSourceEvent::Request(EisRequest::Disconnect)) | Err(_) => {
                        return Ok(smithay::reexports::calloop::PostAction::Remove);
                    }
                    _ => {}
                }
                Ok(smithay::reexports::calloop::PostAction::Continue)
            },
        )
        .is_err()
    {
        return;
    }
    if handle
        .insert_source(commands, |event, _, state| {
            let smithay::reexports::calloop::channel::Event::Msg(command) = event else {
                state.running = false;
                return;
            };
            match command {
                EisCommand::Activate(sequence) => {
                    state.active = true;
                    state.sequence = sequence;
                    if let Some(current) = &state.connection {
                        for device in [&current.keyboard, &current.pointer].into_iter().flatten() {
                            device.start_emulating(sequence);
                        }
                        let _ = current.connection.flush();
                    }
                }
                EisCommand::Deactivate => {
                    state.active = false;
                    if let Some(current) = &state.connection {
                        for device in [&current.keyboard, &current.pointer].into_iter().flatten() {
                            device.stop_emulating();
                        }
                        let _ = current.connection.flush();
                    }
                }
                EisCommand::Input(input) if state.active => send_eis_input(state, input),
                EisCommand::Close => {
                    state.running = false;
                    return;
                }
                EisCommand::Input(_) => {}
            }
        })
        .is_err()
    {
        return;
    }
    let mut state = EisState {
        running: true,
        ..EisState::default()
    };
    while state.running
        && event_loop
            .dispatch(Duration::from_secs(1), &mut state)
            .is_ok()
    {}
}
#[cfg(target_os = "linux")]
fn add_device(seat: &Seat, name: &str, caps: BitFlags<DeviceCapability>, sequence: u32) -> Device {
    let device = seat.add_device(
        Some(name),
        reis::eis::device::DeviceType::Virtual,
        caps,
        |_| {},
    );
    device.resumed();
    if sequence != 0 {
        device.start_emulating(sequence);
    }
    device
}
#[cfg(target_os = "linux")]
fn send_eis_input(state: &mut EisState, input: CapturedInput) {
    let Some(current) = &state.connection else {
        return;
    };
    let time = u64::try_from(std::time::Instant::now().elapsed().as_micros()).unwrap_or(u64::MAX);
    match input {
        CapturedInput::PointerMotion { dx, dy, .. } => {
            if let Some(device) = &current.pointer
                && let Some(pointer) = device.interface::<reis::eis::Pointer>()
            {
                pointer.motion_relative(dx as f32, dy as f32);
                device.frame(time);
            }
        }
        CapturedInput::PointerButton { button, pressed } => {
            if let Some(device) = &current.pointer
                && let Some(buttons) = device.interface::<reis::eis::Button>()
            {
                buttons.button(
                    button,
                    if pressed {
                        reis::eis::button::ButtonState::Press
                    } else {
                        reis::eis::button::ButtonState::Released
                    },
                );
                device.frame(time);
            }
        }
        CapturedInput::Scroll { dx, dy } => {
            if let Some(device) = &current.pointer
                && let Some(scroll) = device.interface::<reis::eis::Scroll>()
            {
                scroll.scroll(dx, dy);
                device.frame(time);
            }
        }
        CapturedInput::Keyboard { keycode, pressed } => {
            if let Some(device) = &current.keyboard
                && let Some(keyboard) = device.interface::<reis::eis::Keyboard>()
            {
                keyboard.key(
                    keycode,
                    if pressed {
                        reis::eis::keyboard::KeyState::Press
                    } else {
                        reis::eis::keyboard::KeyState::Released
                    },
                );
                device.frame(time);
            }
        }
    };
    let _ = current.connection.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_capture_persistence_and_clipboard_are_bound_to_active_capture() {
        // Phase `v2_persistence_options_are_bounded_and_explicitly_declined`.
        {
            let mut options = HashMap::new();
            assert_eq!(requested_persist_mode(&options), Ok(0));
            insert_value(&mut options, "persist_mode", Value::U32(2));
            assert_eq!(requested_persist_mode(&options), Ok(2));
            insert_value(&mut options, "persist_mode", Value::U32(3));
            assert_eq!(requested_persist_mode(&options), Err(()));
        }
        // Phase `clipboard_request_is_bound_to_an_active_capture_only`.
        {
            let handle = InputCaptureCompositorHandle::native(Vec::new());
            let session = "/org/freedesktop/portal/desktop/session/tilo/clipboard".to_string();
            let owner = ":1.7";
            handle.state.lock().unwrap().sessions.insert(
                session.clone(),
                CaptureSession::new(
                    owner.into(),
                    "org.example.Capture".into(),
                    PortalCaller::new(crate::shell::state::WorkspaceId(1), "org.example.Capture"),
                ),
            );
            let provider = InputCaptureSessionHandle {
                state: handle.state.clone(),
            };
            assert!(provider.request_clipboard(&session, owner));
            assert!(!provider.clipboard_active(&session, owner));
            {
                let mut state = handle.state.lock().unwrap();
                let capture = state.sessions.get_mut(&session).unwrap();
                capture.started = true;
                capture.active = true;
            }
            assert!(provider.clipboard_active(&session, owner));
            assert!(provider.clipboard_live(&session));
            // v2 only permits the request before Start, so a capture cannot add
            // clipboard access after the user has already approved it.
            assert!(!provider.request_clipboard(&session, owner));
        }
    }
    #[test]
    fn only_edge_barriers_activate_capture_once() {
        // Phase `edge_barrier_activates_once`.
        {
            let handle = InputCaptureCompositorHandle::native(vec![CaptureZone {
                width: 100,
                height: 100,
                x: 0,
                y: 0,
            }]);
            let mut state = handle.state.lock().unwrap();
            let mut session = CaptureSession::new(
                ":1.1".into(),
                "org.example.App".into(),
                PortalCaller::new(crate::shell::state::WorkspaceId(1), "org.example.App"),
            );
            session.started = true;
            session.enabled = true;
            session.cursor = (90., 50.);
            session.barriers = vec![PointerBarrier {
                id: 7,
                x1: 100,
                y1: 0,
                x2: 100,
                y2: 100,
            }];
            state.sessions.insert(
                "/org/freedesktop/portal/desktop/session/tilo/one".into(),
                session,
            );
            drop(state);
            assert!(handle.observe(CapturedInput::PointerMotion {
                x: 101.,
                y: 50.,
                dx: 11.,
                dy: 0.
            }));
            assert!(matches!(
                handle.drain_signals().as_slice(),
                [PortalSignal::Activated { barrier_id: 7, .. }]
            ));
        }
        // Phase `interior_barrier_is_rejected`.
        assert!(!valid_barrier(
            &PointerBarrier {
                id: 1,
                x1: 5,
                y1: 0,
                x2: 5,
                y2: 10
            },
            &[CaptureZone {
                width: 10,
                height: 10,
                x: 0,
                y: 0
            }]
        ));
    }

    /// The session-runtime emitters used to wake on a timer with nothing to
    /// send. That CPU is not on the frame loop: the process still accumulates
    /// it while loop ticks and the main thread's context switches stay at zero.
    #[cfg(target_os = "linux")]
    #[test]
    fn idle_emitters_stay_asleep_until_a_signal_is_queued() {
        use std::time::Duration;
        use tokio::net::UnixStream;
        use zbus::connection::Builder;

        const COMM: &str = "tilo-idle-emit";
        let (runtime, tid) = tilo_test_env::idle::spawned_thread(COMM, || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .thread_name(COMM)
                .build()
                .expect("runtime")
        });
        let capture = super::InputCaptureCompositorHandle::unavailable();
        runtime.block_on(async {
            let (server_sock, client_sock) = UnixStream::pair().expect("socket pair");
            let server = Builder::unix_stream(server_sock)
                .p2p()
                .server("0123456789abcdef0123456789abcdef")
                .expect("server")
                .build();
            let client = Builder::unix_stream(client_sock).p2p().build();
            let (connection, _client) = tokio::try_join!(server, client).expect("p2p");
            super::spawn_compositor_emitter(capture.clone(), connection.clone());
            crate::portal::clipboard::spawn_signal_emitter(
                connection.clone(),
                crate::portal::clipboard::ClipboardBackend::new(),
            );
            crate::notifications::spawn_signal_emitter(
                connection.clone(),
                crate::notifications::NotificationBridge::new(),
            );
            crate::portal::notification::spawn_action_emitter(
                connection.clone(),
                crate::portal::notification::NotificationPortalHandle::default(),
            );
            crate::portal::inhibit::spawn_state_emitter(
                connection.clone(),
                crate::portal::inhibit::InhibitMonitorHandle::default(),
            );
            crate::portal::global_shortcuts::spawn_signal_emitter(
                connection,
                crate::portal::global_shortcuts::GlobalShortcutsHandle::with_bound_session_for_tests(
                    "/session",
                    "so.tilo.notes",
                    &[],
                ),
            );
        });
        tilo_test_env::idle::assert_stays_asleep(&tid, "portal and notification emitters");
        let parked = tilo_test_env::idle::context_switches(&tid);
        capture.wake.poke();
        std::thread::sleep(Duration::from_millis(50));
        let woke = tilo_test_env::idle::context_switches(&tid).saturating_sub(parked);
        assert!(
            woke > 0,
            "a queued input-capture signal must wake the emitter, woke {woke}"
        );
        drop(runtime);
    }
}

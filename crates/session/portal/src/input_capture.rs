//! `org.freedesktop.impl.portal.InputCapture`: an app (a software KVM, a
//! shared-keyboard tool) taking the keyboard and pointer once the pointer
//! crosses a barrier it set at a screen edge.
//!
//! The session feeds its input through [`InputCaptures::observe`] before
//! the desktop sees it and says where its screens are
//! ([`InputCaptures::set_zones`]); while a capture is active the input goes
//! to the app's EIS receiver ([`crate::eis`]) instead. Starting a session
//! is the person's access decision ([`crate::consent`]); a started session
//! is published so the person sees it and may end it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedFd, OwnedValue, Value};

use crate::consent::AccessQuestion;
use crate::eis::{Receiver, ReceiverCommand};
use crate::reply::{Ended, Outcome, Reply, Results, answer, ensure, reply};
use crate::session::{SessionKind, Sessions};
use crate::{OBJECT_PATH, Portal, option, question_handle};

/// The capability an input capture needs.
pub const INPUT_CAPTURE_CAPABILITY: &str = "input-capture";
const MAX_BARRIERS: usize = 64;

/// `SupportedCapabilities` bits.
pub mod capability {
    pub const KEYBOARD: u32 = 1;
    pub const POINTER: u32 = 2;
    pub const ALL: u32 = KEYBOARD | POINTER;
}

/// A screen's place in the layout, as the app sees zones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureZone {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
}

/// Input the session saw, before the desktop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CapturedInput {
    /// The pointer at (`x`, `y`) in the layout, having moved by
    /// (`dx`, `dy`).
    PointerMotion {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    PointerButton {
        button: u32,
        pressed: bool,
    },
    Scroll {
        dx: f32,
        dy: f32,
    },
    Keyboard {
        keycode: u32,
        pressed: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Barrier {
    id: u32,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
}

impl Barrier {
    /// Whether moving from `previous` to (`x`, `y`) crosses it.
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

#[derive(Clone, Debug, PartialEq)]
enum Signal {
    Activated {
        session: String,
        activation_id: u32,
        cursor: (f64, f64),
        barrier_id: u32,
    },
    Deactivated {
        session: String,
        activation_id: u32,
        cursor: (f64, f64),
    },
    Disabled {
        session: String,
    },
    ZonesChanged {
        session: String,
    },
}

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the portal's lifecycle flags"
)]
struct Capture {
    started: bool,
    enabled: bool,
    active: bool,
    clipboard: bool,
    capabilities: u32,
    barriers: Vec<Barrier>,
    receiver: Option<Receiver>,
    activation_id: u32,
    cursor: (f64, f64),
}

impl Capture {
    fn deactivate(&mut self, path: &str) -> Option<Signal> {
        if !self.active {
            return None;
        }
        self.active = false;
        if let Some(receiver) = &self.receiver {
            receiver.send(ReceiverCommand::Deactivate);
        }
        Some(Signal::Deactivated {
            session: path.to_owned(),
            activation_id: self.activation_id,
            cursor: self.cursor,
        })
    }
}

#[derive(Default)]
struct State {
    zones: Vec<CaptureZone>,
    zone_set: u32,
    captures: BTreeMap<String, Capture>,
    signals: Vec<Signal>,
}

/// The input captures, as the session feeds them its input.
#[derive(Clone, Default)]
pub struct InputCaptures {
    state: Arc<Mutex<State>>,
    signalled: Arc<tokio::sync::Notify>,
}

impl InputCaptures {
    pub(crate) fn follow(&self, sessions: &Sessions) {
        let captures = self.clone();
        sessions.on_end(move |path| {
            if let Some(capture) = captures.state.lock_recover().captures.remove(path)
                && let Some(receiver) = capture.receiver
            {
                receiver.send(ReceiverCommand::Close);
            }
        });
    }

    /// Whether the session said where its screens are.
    #[must_use]
    pub fn available(&self) -> bool {
        !self.state.lock_recover().zones.is_empty()
    }

    /// Where the session's screens are now: every capture's barriers are
    /// dropped and an active capture ends, since the edges moved.
    pub fn set_zones(&self, zones: Vec<CaptureZone>) {
        let mut state = self.state.lock_recover();
        if state.zones == zones {
            return;
        }
        state.zones = zones;
        state.zone_set = state.zone_set.wrapping_add(1).max(1);
        let mut signals = Vec::new();
        for (path, capture) in &mut state.captures {
            capture.barriers.clear();
            signals.extend(capture.deactivate(path));
            signals.push(Signal::ZonesChanged {
                session: path.clone(),
            });
        }
        state.signals.extend(signals);
        drop(state);
        self.signalled.notify_one();
    }

    /// Offer `input` to the captures; `true` when one took it and the
    /// desktop must not see it.
    pub fn observe(&self, input: CapturedInput) -> bool {
        let mut state = self.state.lock_recover();
        if state.zones.is_empty() {
            return false;
        }
        // A capture is exclusive: while one is active no other app sees
        // the input, whatever barriers it set.
        if let Some(capture) = state
            .captures
            .values_mut()
            .find(|capture| capture.started && capture.enabled && capture.active)
        {
            if let CapturedInput::PointerMotion { x, y, .. } = input {
                capture.cursor = (x, y);
            }
            if let Some(receiver) = &capture.receiver {
                receiver.send(ReceiverCommand::Input(input));
            }
            return true;
        }
        let CapturedInput::PointerMotion { x, y, .. } = input else {
            return false;
        };
        // The first session by path wins overlapping barriers: no map
        // order decides who gets the input.
        let mut crossed = None;
        for (path, capture) in &mut state.captures {
            if !capture.started || !capture.enabled {
                continue;
            }
            let previous = std::mem::replace(&mut capture.cursor, (x, y));
            if crossed.is_none()
                && let Some(barrier) = capture
                    .barriers
                    .iter()
                    .find(|barrier| barrier.crossed(x, y, previous))
            {
                crossed = Some((path.clone(), barrier.id));
            }
        }
        let Some((path, barrier_id)) = crossed else {
            return false;
        };
        let Some(capture) = state.captures.get_mut(&path) else {
            return false;
        };
        capture.active = true;
        capture.activation_id = capture.activation_id.wrapping_add(1).max(1);
        let activation_id = capture.activation_id;
        if let Some(receiver) = &capture.receiver {
            receiver.send(ReceiverCommand::Activate(activation_id));
        }
        state.signals.push(Signal::Activated {
            session: path,
            activation_id,
            cursor: (x, y),
            barrier_id,
        });
        drop(state);
        self.signalled.notify_one();
        true
    }

    fn take_signals(&self) -> Vec<Signal> {
        std::mem::take(&mut self.state.lock_recover().signals)
    }

    fn push(&self, signals: impl IntoIterator<Item = Signal>) {
        self.state.lock_recover().signals.extend(signals);
        self.signalled.notify_one();
    }

    /// Ask for the clipboard on session `path`, before it started.
    pub(crate) fn request_clipboard(&self, path: &str) -> bool {
        self.state
            .lock_recover()
            .captures
            .get_mut(path)
            .is_some_and(|capture| {
                if capture.started {
                    return false;
                }
                capture.clipboard = true;
                true
            })
    }

    /// Whether session `path` holds the clipboard: asked for before its
    /// start, and only while its capture is active.
    pub(crate) fn clipboard(&self, path: &str) -> bool {
        self.state
            .lock_recover()
            .captures
            .get(path)
            .is_some_and(|capture| capture.started && capture.clipboard && capture.active)
    }

    /// The sessions holding the clipboard now.
    pub(crate) fn clipboard_sessions(&self) -> Vec<String> {
        self.state
            .lock_recover()
            .captures
            .iter()
            .filter(|(_, capture)| capture.started && capture.clipboard && capture.active)
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// The started captures.
    pub(crate) fn started(&self) -> Vec<String> {
        self.state
            .lock_recover()
            .captures
            .iter()
            .filter(|(_, capture)| capture.started)
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// Emit the signals the captures queue on `connection`, as they come.
    fn emit_on(&self, connection: &zbus::Connection, sessions: &Sessions) {
        let captures = self.clone();
        let connection = connection.clone();
        sessions.spawn(async move {
            loop {
                let signals = captures.take_signals();
                if signals.is_empty() {
                    captures.signalled.notified().await;
                    continue;
                }
                for signal in signals {
                    emit(&connection, signal).await;
                }
            }
        });
    }
}

/// A barrier from its wire form: an id and a horizontal or vertical line.
fn parse_barrier(raw: &HashMap<String, OwnedValue>) -> Option<Barrier> {
    let id = option::<u32>(raw, "barrier_id").filter(|id| *id != 0)?;
    let (x1, y1, x2, y2) =
        <(i32, i32, i32, i32)>::try_from(raw.get("position")?.try_clone().ok()?).ok()?;
    ((x1 == x2) ^ (y1 == y2)).then_some(Barrier { id, x1, y1, x2, y2 })
}

/// Whether `barrier` lies along an edge of one of `zones`.
fn valid_barrier(barrier: &Barrier, zones: &[CaptureZone]) -> bool {
    zones.iter().any(|zone| {
        let right = zone.x.saturating_add_unsigned(zone.width);
        let bottom = zone.y.saturating_add_unsigned(zone.height);
        let inside = barrier.x1.min(barrier.x2) >= zone.x
            && barrier.x1.max(barrier.x2) <= right
            && barrier.y1.min(barrier.y2) >= zone.y
            && barrier.y1.max(barrier.y2) <= bottom;
        let vertical_edge = barrier.x1 == barrier.x2
            && barrier.y1 != barrier.y2
            && (barrier.x1 == zone.x || barrier.x1 == right);
        let horizontal_edge = barrier.y1 == barrier.y2
            && barrier.x1 != barrier.x2
            && (barrier.y1 == zone.y || barrier.y1 == bottom);
        inside && (vertical_edge || horizontal_edge)
    })
}

fn insert(values: &mut Results, key: &str, value: Value<'_>) {
    if let Ok(value) = OwnedValue::try_from(value) {
        values.insert(key.into(), value);
    }
}

async fn emit(connection: &zbus::Connection, signal: Signal) {
    let Ok(emitter) = SignalEmitter::new(connection, OBJECT_PATH) else {
        return;
    };
    let mut options = Results::new();
    let session = match signal {
        Signal::Activated {
            session,
            activation_id,
            cursor,
            barrier_id,
        } => {
            insert(&mut options, "activation_id", Value::U32(activation_id));
            insert(&mut options, "cursor_position", Value::from(cursor));
            insert(&mut options, "barrier_id", Value::U32(barrier_id));
            let Ok(path) = ObjectPath::try_from(session.as_str()) else {
                return;
            };
            let _ = InputCapture::activated(&emitter, path, options).await;
            return;
        }
        Signal::Deactivated {
            session,
            activation_id,
            cursor,
        } => {
            insert(&mut options, "activation_id", Value::U32(activation_id));
            insert(&mut options, "cursor_position", Value::from(cursor));
            let Ok(path) = ObjectPath::try_from(session.as_str()) else {
                return;
            };
            let _ = InputCapture::deactivated(&emitter, path, options).await;
            return;
        }
        Signal::Disabled { session } => (session, true),
        Signal::ZonesChanged { session } => (session, false),
    };
    let Ok(path) = ObjectPath::try_from(session.0.as_str()) else {
        return;
    };
    let _ = if session.1 {
        InputCapture::disabled(&emitter, path, options).await
    } else {
        InputCapture::zones_changed(&emitter, path, options).await
    };
}

/// The `org.freedesktop.impl.portal.InputCapture` interface.
pub struct InputCapture {
    portal: Portal,
}

impl InputCapture {
    /// The interface, emitting its signals on `connection`.
    #[must_use]
    pub fn new(portal: Portal, connection: &zbus::Connection) -> Self {
        portal.input_captures.emit_on(connection, &portal.sessions);
        Self { portal }
    }

    fn captures(&self) -> &InputCaptures {
        &self.portal.input_captures
    }

    /// Make capture session `path` for an authenticated call.
    async fn create(
        &self,
        connection: &zbus::Connection,
        path: &str,
        auth: crate::AuthenticatedCall,
    ) -> Result<(), Ended> {
        ensure(self.captures().available())?;
        self.portal
            .sessions
            .create(
                connection,
                path,
                auth.owner.as_str(),
                auth.caller,
                SessionKind::InputCapture,
            )
            .await?;
        self.captures()
            .state
            .lock_recover()
            .captures
            .insert(path.to_owned(), Capture::default());
        Ok(())
    }

    /// Change the capture of session `path`, sent by its frontend for
    /// `app_id`.
    async fn change(
        &self,
        header: &zbus::message::Header<'_>,
        path: &str,
        app_id: &str,
        change: impl FnOnce(&mut Capture) -> Option<Vec<Signal>>,
    ) -> Outcome {
        let (_, session) = self
            .portal
            .authenticate_session(header, path, app_id)
            .await?;
        ensure(session.kind == SessionKind::InputCapture)?;
        let signals = self
            .captures()
            .state
            .lock_recover()
            .captures
            .get_mut(path)
            .and_then(change)
            .ok_or(Ended::Failed)?;
        self.captures().push(signals);
        Ok(Results::new())
    }
}

#[interface(name = "org.freedesktop.impl.portal.InputCapture")]
impl InputCapture {
    #[zbus(property, name = "SupportedCapabilities")]
    fn supported_capabilities(&self) -> u32 {
        if self.captures().available() {
            capability::ALL
        } else {
            0
        }
    }

    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        2
    }

    /// Version 1's session: made and started at once, asking the person.
    async fn create_session(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let requested = option::<u32>(&options, "capabilities").unwrap_or(0) & capability::ALL;
            ensure(requested != 0)?;
            let auth = self
                .portal
                .authenticate(&header, handle.as_str(), &app_id, Some(&parent_window))
                .await?;
            let caller = auth.caller.clone();
            let owner = auth.owner.clone();
            let path = session_handle.as_str();
            self.create(connection, path, auth).await?;
            let request = handle.as_str();
            let decided = self
                .portal
                .requests
                .run(
                    connection,
                    request,
                    owner.as_str(),
                    self.portal.consents.decide(
                        &caller,
                        &question_handle(request),
                        AccessQuestion {
                            capability: INPUT_CAPTURE_CAPABILITY.into(),
                            reason: None,
                            may_remember: true,
                            text: None,
                        },
                    ),
                )
                .await;
            if let Err(ended) = decided {
                self.portal.sessions.end(path);
                return Err(ended);
            }
            let started = self
                .captures()
                .state
                .lock_recover()
                .captures
                .get_mut(path)
                .is_some_and(|capture| {
                    capture.started = true;
                    capture.capabilities = requested;
                    true
                });
            ensure(started)?;
            self.portal.sessions.changed();
            let mut results = Results::new();
            insert(&mut results, "capabilities", Value::U32(requested));
            Ok(results)
        })
        .await
    }

    /// Version 2's session: made now, started by `Start`.
    async fn create_session2(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> HashMap<String, OwnedValue> {
        let path = session_handle.as_str();
        let sender = header
            .sender()
            .cloned()
            .map(zbus::names::OwnedUniqueName::from);
        let Ok(auth) = self
            .portal
            .authenticator
            .authenticate_request(sender.as_ref(), path, &app_id, None)
            .await
        else {
            return HashMap::new();
        };
        if self.create(connection, path, auth).await.is_err() {
            return HashMap::new();
        }
        let mut results = Results::new();
        insert(&mut results, "capabilities", Value::U32(capability::ALL));
        results
    }

    async fn start(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let requested = option::<u32>(&options, "capabilities").unwrap_or(0) & capability::ALL;
            let persist_mode = option::<u32>(&options, "persist_mode").unwrap_or(0);
            ensure(requested != 0 && persist_mode <= 2 && self.captures().available())?;
            let auth = self
                .portal
                .authenticate(&header, handle.as_str(), &app_id, Some(&parent_window))
                .await?;
            let path = session_handle.as_str();
            ensure(
                self.portal
                    .sessions
                    .owned(path, auth.owner.as_str(), &auth.caller)
                    == Some(SessionKind::InputCapture),
            )?;
            let request = handle.as_str();
            self.portal
                .requests
                .run(
                    connection,
                    request,
                    auth.owner.as_str(),
                    self.portal.consents.decide(
                        &auth.caller,
                        &question_handle(request),
                        AccessQuestion {
                            capability: INPUT_CAPTURE_CAPABILITY.into(),
                            reason: None,
                            may_remember: true,
                            text: None,
                        },
                    ),
                )
                .await?;
            let clipboard = {
                let mut state = self.captures().state.lock_recover();
                let capture = state.captures.get_mut(path).ok_or(Ended::Failed)?;
                ensure(!capture.started)?;
                capture.started = true;
                capture.capabilities = requested;
                let clipboard = capture.clipboard;
                drop(state);
                clipboard
            };
            self.portal.sessions.changed();
            let mut results = Results::new();
            insert(&mut results, "capabilities", Value::U32(requested));
            insert(&mut results, "clipboard_enabled", Value::Bool(clipboard));
            // The access decision is what is remembered; no restore data.
            if persist_mode != 0 {
                insert(&mut results, "persist_mode", Value::U32(0));
            }
            Ok(results)
        })
        .await
    }

    async fn get_zones(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let auth = self
                .portal
                .authenticate(&header, handle.as_str(), &app_id, None)
                .await?;
            ensure(
                self.portal.sessions.owned(
                    session_handle.as_str(),
                    auth.owner.as_str(),
                    &auth.caller,
                ) == Some(SessionKind::InputCapture),
            )?;
            let (zones, zone_set) = {
                let state = self.captures().state.lock_recover();
                (state.zones.clone(), state.zone_set)
            };
            let mut results = Results::new();
            insert(
                &mut results,
                "zones",
                Value::from(
                    zones
                        .into_iter()
                        .map(|zone| (zone.width, zone.height, zone.x, zone.y))
                        .collect::<Vec<_>>(),
                ),
            );
            insert(&mut results, "zone_set", Value::U32(zone_set));
            Ok(results)
        })
        .await
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
    ) -> Reply {
        answer(async {
            ensure(barriers.len() <= MAX_BARRIERS)?;
            let auth = self
                .portal
                .authenticate(&header, handle.as_str(), &app_id, None)
                .await?;
            let path = session_handle.as_str();
            ensure(
                self.portal
                    .sessions
                    .owned(path, auth.owner.as_str(), &auth.caller)
                    == Some(SessionKind::InputCapture),
            )?;
            let mut state = self.captures().state.lock_recover();
            let current = state.zone_set;
            let zones = state.zones.clone();
            let mut accepted = Vec::new();
            let mut failed = Vec::new();
            for raw in &barriers {
                match parse_barrier(raw)
                    .filter(|barrier| zone_set == current && valid_barrier(barrier, &zones))
                {
                    Some(barrier) => accepted.push(barrier),
                    None => failed.push(option::<u32>(raw, "barrier_id").unwrap_or(0)),
                }
            }
            state.captures.get_mut(path).ok_or(Ended::Failed)?.barriers = accepted;
            drop(state);
            let mut results = Results::new();
            insert(&mut results, "failed_barriers", Value::from(failed));
            Ok(results)
        })
        .await
    }

    async fn enable(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        reply(
            self.change(&header, session_handle.as_str(), &app_id, |capture| {
                (capture.started && capture.receiver.is_some()).then(|| {
                    capture.enabled = true;
                    Vec::new()
                })
            })
            .await,
        )
    }

    async fn disable(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        let path = session_handle.as_str();
        reply(
            self.change(&header, path, &app_id, |capture| {
                capture.started.then(|| {
                    capture.enabled = false;
                    let mut signals: Vec<Signal> = capture.deactivate(path).into_iter().collect();
                    signals.push(Signal::Disabled {
                        session: path.to_owned(),
                    });
                    signals
                })
            })
            .await,
        )
    }

    async fn release(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        let path = session_handle.as_str();
        reply(
            self.change(&header, path, &app_id, |capture| {
                capture.deactivate(path).map(|signal| vec![signal])
            })
            .await,
        )
    }

    async fn connect_to_eis(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<OwnedFd> {
        let path = session_handle.as_str();
        let owned = self
            .portal
            .authenticate_session(&header, path, &app_id)
            .await
            .is_ok_and(|(_, session)| session.kind == SessionKind::InputCapture);
        if !owned {
            return Err(zbus::fdo::Error::AccessDenied(
                "the session is not this caller's".into(),
            ));
        }
        let (receiver, fd) = crate::eis::serve_receiver()
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        let installed = self
            .captures()
            .state
            .lock_recover()
            .captures
            .get_mut(path)
            .is_some_and(|capture| {
                if !capture.started || capture.receiver.is_some() {
                    return false;
                }
                capture.receiver = Some(receiver.clone());
                true
            });
        if !installed {
            receiver.send(ReceiverCommand::Close);
            return Err(zbus::fdo::Error::Failed(
                "EIS needs a started session and connects once".into(),
            ));
        }
        Ok(OwnedFd::from(fd))
    }

    #[zbus(signal)]
    async fn disabled(
        emitter: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn activated(
        emitter: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn deactivated(
        emitter: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn zones_changed(
        emitter: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone() -> CaptureZone {
        CaptureZone {
            width: 100,
            height: 100,
            x: 0,
            y: 0,
        }
    }

    fn armed(captures: &InputCaptures, path: &str, barrier: Barrier) {
        let mut state = captures.state.lock_recover();
        state.captures.insert(
            path.into(),
            Capture {
                started: true,
                enabled: true,
                cursor: (90., 50.),
                barriers: vec![barrier],
                ..Capture::default()
            },
        );
    }

    #[test]
    fn crossing_an_edge_barrier_activates_once_and_takes_the_input() {
        let captures = InputCaptures::default();
        captures.set_zones(vec![zone()]);
        let _ = captures.take_signals();
        let right_edge = Barrier {
            id: 7,
            x1: 100,
            y1: 0,
            x2: 100,
            y2: 100,
        };
        armed(&captures, "/s/b", right_edge.clone());
        armed(&captures, "/s/a", right_edge);
        let motion = CapturedInput::PointerMotion {
            x: 101.,
            y: 50.,
            dx: 11.,
            dy: 0.,
        };
        assert!(captures.observe(motion));
        assert!(matches!(
            captures.take_signals().as_slice(),
            [Signal::Activated { session, barrier_id: 7, .. }] if session == "/s/a"
        ));
        assert!(
            captures.observe(CapturedInput::Keyboard {
                keycode: 30,
                pressed: true
            }),
            "an active capture takes the keyboard too"
        );
        assert!(captures.take_signals().is_empty());
    }

    #[test]
    fn new_zones_drop_barriers_and_end_an_active_capture() {
        let captures = InputCaptures::default();
        captures.set_zones(vec![zone()]);
        armed(
            &captures,
            "/s/a",
            Barrier {
                id: 1,
                x1: 100,
                y1: 0,
                x2: 100,
                y2: 100,
            },
        );
        captures
            .state
            .lock_recover()
            .captures
            .get_mut("/s/a")
            .expect("armed")
            .active = true;
        let _ = captures.take_signals();
        captures.set_zones(vec![CaptureZone {
            width: 200,
            ..zone()
        }]);
        let signals = captures.take_signals();
        assert!(matches!(
            signals.as_slice(),
            [Signal::Deactivated { .. }, Signal::ZonesChanged { .. }]
        ));
        assert!(!captures.observe(CapturedInput::Keyboard {
            keycode: 30,
            pressed: true
        }));
    }

    #[test]
    fn barriers_lie_on_zone_edges() {
        let inside = Barrier {
            id: 1,
            x1: 5,
            y1: 0,
            x2: 5,
            y2: 10,
        };
        assert!(!valid_barrier(&inside, &[zone()]));
        let edge = Barrier {
            id: 1,
            x1: 0,
            y1: 0,
            x2: 0,
            y2: 100,
        };
        assert!(valid_barrier(&edge, &[zone()]));
        let mut raw = HashMap::new();
        raw.insert("barrier_id".into(), OwnedValue::from(3_u32));
        raw.insert(
            "position".into(),
            OwnedValue::try_from(Value::new((0_i32, 0_i32, 10_i32, 10_i32))).expect("tuple"),
        );
        assert!(parse_barrier(&raw).is_none(), "a diagonal is no barrier");
    }

    #[test]
    fn the_clipboard_is_asked_for_before_the_start_and_held_only_while_active() {
        let captures = InputCaptures::default();
        captures
            .state
            .lock_recover()
            .captures
            .insert("/s/a".into(), Capture::default());
        assert!(captures.request_clipboard("/s/a"));
        assert!(!captures.clipboard("/s/a"));
        {
            let mut state = captures.state.lock_recover();
            let capture = state.captures.get_mut("/s/a").expect("live");
            capture.started = true;
            capture.active = true;
            drop(state);
        }
        assert!(captures.clipboard("/s/a"));
        assert!(!captures.request_clipboard("/s/a"));
    }
}

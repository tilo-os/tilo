//! Root widget for the Remote app.
//!
//! Owns the connection [`Workspace`] (as the resource presents it, plus
//! what this device observed of each machine's reachability), the current
//! [`Screen`], the add / edit draft and the live session. The
//! window is a [`NavShell`]: a docked rail of destinations beside the
//! current screen; the screen's name and actions are the window bar's
//! (`window_bar`).
//! The list and the form are toolkit widgets rebuilt from this state
//! (`crate::view`); a connected session is `SessionPane`, which paints
//! the remote terminal grid or framebuffer.
//!
//! Controls report through one queue of `Action`s, drained after every
//! event, so the pointer, the keyboard, assistive technology and the
//! resource actions agents call share one path. Saved connections change
//! only through the resource's dispatcher; a connection's password or key
//! passphrase lives in this device's secret store.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};

use taffy::{NodeId, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Blob, ImageData as PenikoImage};
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};

use ui::LocalState;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_resource::Invoke;
use ui::widget::{
    AppModel, Collaborative, Column, Element, ModalDialog, ModelPath, NavMode, NavShell,
    TaffyTreeExt, Widget,
};

use crate::model::{
    AuthMethod, Connection, ConnectionDraft, ConnectionId, ConnectionKind, Remote, Screen,
    Workspace,
};
use crate::session::{SessionStatus, TransportReadiness, connection_readiness};
use crate::ssh::SshSession;
use crate::view::{
    self, Destination, DeviceModel, FormFields, FormModel, FormProblems, Requests, RowModel,
    StatusModel,
};
use crate::vnc::{VncInput, VncSession};
use ui::lock::MutexRecover as _;

/// A live transport backing an open session. SSH is a `russh` PTY channel
/// feeding the terminal grid; VNC is an RFB client
/// decoding the remote framebuffer. `Error` holds a pre-flight failure
/// (e.g. missing credentials) so the surface can show the reason without
/// opening a session.
enum LiveSession {
    Ssh(Box<SshSession>),
    Vnc(VncSession),
    Tilo(TiloSession),
    Error(String),
    /// A saved profile that cannot start in this process. This is a distinct
    /// state from a failed connection: no socket was opened and no credential
    /// was sent. Keeping it explicit lets the UI offer a recovery path instead
    /// of a blank row or a misleading retry button.
    Unavailable {
        readiness: TransportReadiness,
        kind: ConnectionKind,
    },
    /// A fixed status with no transport, for `--shots`/preview rendering of
    /// the connecting/failed/closed states. Never on the live path.
    Preview(SessionStatus),
    /// A connected session staged with a sample surface and no transport,
    /// for `--shots`. Never on the live path.
    Sample(SampleSurface),
}

/// What a staged connected session shows.
enum SampleSurface {
    /// A terminal grid, as an SSH session fills it.
    Terminal(Box<crate::vt::VtGrid>),
    /// A remote desktop; its frame is the session's framebuffer image.
    Desktop,
}

impl LiveSession {
    fn status(&self) -> SessionStatus {
        match self {
            LiveSession::Ssh(s) => s.status(),
            LiveSession::Vnc(s) => s.status(),
            LiveSession::Tilo(s) => s.status.clone(),
            LiveSession::Error(e) => SessionStatus::Failed(e.clone()),
            LiveSession::Unavailable { readiness, kind } => {
                SessionStatus::Unavailable(readiness.detail(*kind).into())
            }
            LiveSession::Preview(s) => s.clone(),
            LiveSession::Sample(_) => SessionStatus::Connected,
        }
    }
}

struct TiloSession {
    id: u64,
    status: SessionStatus,
    size: Option<(u32, u32)>,
    /// Tilo collaboration events arrive on the UI thread. Keep their H.264
    /// payloads bounded and hand decoding to this private worker so a hostile
    /// or merely busy peer cannot make event dispatch stutter.
    /// Encoded units in arrival order, plus a capacity-one wake channel.
    /// Every admitted unit is kept, since a delta references the ones before
    /// it; past [`TILO_UNIT_BACKLOG`] the backlog is dropped and the chain
    /// waits for a keyframe.
    encoded: Arc<Mutex<UnitBacklog>>,
    encoded_wake: SyncSender<()>,
    decoder_available: Arc<AtomicBool>,
    decoded: Arc<Mutex<Option<tilo_callstream::DecodedFrame>>>,
    /// The stream's reference chain, judged as each unit arrives rather than
    /// when the worker reaches it: a decoder still starting up, or busy with
    /// a slow unit, must not delay noticing a gap and asking for a keyframe.
    chain: tilo_callstream::unit_chain::UnitChain,
    /// When a keyframe was last asked for, while one is awaited.
    asked: Option<std::time::Instant>,
}

/// Encoded units waiting for the decode worker, in the order they continue
/// the chain.
type UnitBacklog = std::collections::VecDeque<Vec<u8>>;

/// Encoded units waiting for the decode worker before the backlog is
/// dropped: two seconds of a 30 fps stream.
/// How long a connection attempt is given before the page says it did not
/// answer: a Tilo case waits for the owner's consent, and an SSH or VNC
/// handshake waits on the network, but neither waits without end.
const CONNECT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(45);

/// Remote, as the ledger and the waiting stack name it.
fn owner() -> ui::attention::Owner {
    ui::attention::Owner::new("so.tilo.remote", "Remote").with_icon(Icon::Globe)
}

const TILO_UNIT_BACKLOG: usize = 60;

/// How long the session waits for an asked-for keyframe before asking
/// again, in case the request or the keyframe was lost too.
const TILO_KEYFRAME_RETRY: std::time::Duration = std::time::Duration::from_millis(300);

impl TiloSession {
    fn new(id: u64) -> Result<Self, String> {
        let (encoded_wake, encoded_rx) = sync_channel::<()>(1);
        let encoded: Arc<Mutex<UnitBacklog>> = Arc::default();
        let encoded_worker = Arc::clone(&encoded);
        let decoder_available = Arc::new(AtomicBool::new(true));
        let decoder_available_worker = Arc::clone(&decoder_available);
        // A replace-only latest slot bounds GPU-upload backlog and guarantees
        // the UI never paints an older decoded frame after a newer one exists.
        let decoded = Arc::new(Mutex::new(None));
        let decoded_worker = Arc::clone(&decoded);
        std::thread::Builder::new()
            .name("remote-tilo-decode".into())
            .spawn(move || {
                let mut h264_decoder = tilo_callstream::VideoDecoder::default();
                while encoded_rx.recv().is_ok() {
                    let units = std::mem::take(&mut *encoded_worker.lock_recover());
                    for encoded in units {
                        if let Some(frame) = h264_decoder.decode(&encoded) {
                            *decoded_worker.lock_recover() = Some(frame);
                        }
                    }
                }
                decoder_available_worker.store(false, Ordering::Release);
            })
            .map_err(|error| format!("could not start Tilo frame decoder: {error}"))?;
        Ok(Self {
            id,
            status: SessionStatus::Connecting,
            size: None,
            encoded,
            encoded_wake,
            decoder_available,
            decoded,
            chain: tilo_callstream::unit_chain::UnitChain::default(),
            asked: None,
        })
    }

    /// Hand unit `seq` to the decode worker if it continues the chain.
    /// `Ok(Some(seq))` names the unit after which the target is asked for a
    /// keyframe: a unit went missing, the worker fell behind, or an earlier
    /// request went unanswered.
    fn queue_frame(&mut self, seq: u64, encoded: Vec<u8>) -> Result<Option<u64>, String> {
        if !self.decoder_available.load(Ordering::Acquire) {
            return Err("Tilo frame decoder is unavailable".into());
        }
        let keyframe = tilo_callstream::is_keyframe(tilo_callstream::StreamCodec::H264, &encoded);
        let fell_behind = {
            let mut queued = self.encoded.lock_recover();
            let full = queued.len() >= TILO_UNIT_BACKLOG;
            if full {
                // The worker cannot keep up: the units it skips break the
                // chain, which waits for a keyframe.
                queued.clear();
            }
            drop(queued);
            full
        };
        if fell_behind {
            self.chain.lose();
        }
        match self.chain.admit(seq, keyframe) {
            tilo_callstream::unit_chain::Admission::Decode => {
                if keyframe {
                    self.asked = None;
                }
                self.encoded.lock_recover().push_back(encoded);
            }
            tilo_callstream::unit_chain::Admission::Stale => return Ok(None),
            tilo_callstream::unit_chain::Admission::AwaitingKeyframe { broke } => {
                let ask = broke
                    || fell_behind
                    || self
                        .asked
                        .is_none_or(|at| at.elapsed() >= TILO_KEYFRAME_RETRY);
                if ask {
                    self.asked = Some(std::time::Instant::now());
                }
                return Ok(ask.then_some(seq));
            }
        }
        match self.encoded_wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(None),
            Err(TrySendError::Disconnected(())) => {
                self.decoder_available.store(false, Ordering::Release);
                Err("Tilo frame decoder stopped".into())
            }
        }
    }

    fn take_latest_frame(&self) -> Option<tilo_callstream::DecodedFrame> {
        self.decoded.lock_recover().take()
    }
}

enum PendingResourceOperation {
    Connect {
        /// The caller to answer; a test drives the settlement without one.
        reply: Option<ui::framework::EffectReply>,
        connection_id: ConnectionId,
        session_generation: u64,
    },
    SshInput {
        reply: Option<ui::framework::EffectReply>,
        connection_id: ConnectionId,
        session_generation: u64,
        acknowledgement: std::sync::mpsc::Receiver<Result<(), String>>,
    },
}

/// Everything a control can ask for. Queued by the views
/// ([`crate::view`]) and applied by [`RemoteApp::apply`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Go to a rail destination.
    Show(Destination),
    /// Open the add-connection form (blank).
    NewConnection,
    /// Open the form to edit an existing connection.
    EditConnection(ConnectionId),
    /// Ask before opening a live session. Resource actions receive the same
    /// confirmation from the shell, so every path is equally explicit.
    RequestConnect(ConnectionId),
    /// Pin the connection to the top of the list, or unpin it.
    ToggleFavorite(ConnectionId),
    /// Ask before removing a shared saved profile.
    RequestDelete(ConnectionId),
    /// Ask before adding the freshly observed SSH host key to device-private trust.
    RequestTrustSshHostKey(ConnectionId),
    /// Ask before forgetting a device-private SSH host-key pin.
    RequestForgetSshHostKey(ConnectionId),
    /// Execute the open, revalidated confirmation.
    ConfirmPending,
    /// Dismiss the open confirmation without doing anything.
    CancelPending,
    SetKind(ConnectionKind),
    SetAuth(AuthMethod),
    /// Pick one of the discovered Tilo devices into the form.
    PickTiloDevice(usize),
    /// Save the form's draft.
    SaveForm,
    /// Leave the form.
    Back,
    /// Switch the remote desktop between fitting the window and its own size.
    ToggleScale,
    /// Show the session alone in the window, or bring the window back.
    ToggleFullscreen,
    /// End the open session.
    Disconnect,
}

/// A discoverable Tilo peer, surfaced in the form's device picker.
#[derive(Clone, Debug)]
struct TiloDevice {
    name: String,
    node: String,
    online: bool,
}

/// A local, deliberate confirmation for actions whose resource contract is
/// privileged or destructive.  Only an id and a non-secret target snapshot
/// are retained: secrets are neither rendered nor copied into UI state.
#[derive(Clone, Debug)]
enum PendingConfirmation {
    Connect {
        id: ConnectionId,
        target: String,
    },
    /// End the session open on `id`: its scrollback goes with it.
    Disconnect {
        id: ConnectionId,
        target: String,
    },
    Delete {
        id: ConnectionId,
        target: String,
    },
    TrustSshHostKey {
        id: ConnectionId,
        target: String,
        observed: crate::ssh::HostKeyIdentity,
    },
    ForgetSshHostKey {
        id: ConnectionId,
        target: String,
    },
}

impl PendingConfirmation {
    fn id(&self) -> ConnectionId {
        match self {
            Self::Connect { id, .. }
            | Self::Disconnect { id, .. }
            | Self::Delete { id, .. }
            | Self::TrustSshHostKey { id, .. }
            | Self::ForgetSshHostKey { id, .. } => *id,
        }
    }

    fn target(&self) -> &str {
        match self {
            Self::Connect { target, .. }
            | Self::Disconnect { target, .. }
            | Self::Delete { target, .. }
            | Self::TrustSshHostKey { target, .. }
            | Self::ForgetSshHostKey { target, .. } => target,
        }
    }
}

/// The open session's transport and what its surface last showed, shared
/// between the app (which opens, closes and reports on it) and the
/// [`SessionPane`] that paints it and forwards input to it.
struct SessionCore {
    live: Option<LiveSession>,
    /// The most recent remote framebuffer, rebuilt only when a new frame
    /// arrives.
    fb_image: Option<PenikoImage>,
    /// Where the framebuffer was last painted, so pointer events map back
    /// into remote-desktop coordinates.
    fb_canvas: Option<Rect>,
    /// Current VNC pointer button mask (bit 0 = left, 1 = middle, 2 = right).
    vnc_buttons: u8,
    /// The keys this pane pressed on the remote machine and has not released
    /// there yet, by the physical key: the evdev keycode for a Tilo session,
    /// the keysym for VNC. Released together when the pane loses the
    /// keyboard, so no key stays held on the remote side.
    held_keys: Vec<(PhysicalKey, u32)>,
    /// Wheel movement not yet sent to a VNC desktop, in wheel clicks: a
    /// trackpad scrolls in pixels, a desktop in whole clicks.
    vnc_wheel: (f64, f64),
    /// Whether the remote desktop is scaled to fit (vs. its own size).
    scale_to_fit: bool,
    /// Last pointer position over the pane.
    pointer: Point,
    /// Requests for the shell's collaboration broker: opening and closing
    /// Tilo workspace control, and the input sent through it.
    collab_outbox: Vec<ui::collaboration::CollabRequest>,
    /// The surface changed since it was last painted.
    changed: bool,
}

impl Default for SessionCore {
    fn default() -> Self {
        Self {
            live: None,
            fb_image: None,
            fb_canvas: None,
            vnc_buttons: 0,
            held_keys: Vec::new(),
            vnc_wheel: (0.0, 0.0),
            scale_to_fit: true,
            pointer: Point::ZERO,
            collab_outbox: Vec::new(),
            changed: false,
        }
    }
}

impl SessionCore {
    fn status(&self) -> Option<SessionStatus> {
        self.live.as_ref().map(LiveSession::status)
    }

    fn live_active(&self) -> bool {
        self.status().is_some_and(|s| s.is_live())
    }

    /// Drain live-session I/O: SSH output into the grid, new VNC and Tilo
    /// frames into the image. Returns whether the surface changed.
    fn pump(&mut self) -> bool {
        let mut changed = false;
        match self.live.as_mut() {
            Some(LiveSession::Ssh(s)) => changed = s.pump(),
            Some(LiveSession::Vnc(s)) => {
                if let Some(fb) = s.take_frame() {
                    self.fb_image = Some(ui::raster::rgba8(
                        Blob::new(fb.data),
                        u32::from(fb.width),
                        u32::from(fb.height),
                    ));
                    changed = true;
                }
            }
            Some(LiveSession::Tilo(session)) => {
                if let Some(frame) = session.take_latest_frame() {
                    session.size = Some((frame.width, frame.height));
                    self.fb_image = Some(ui::raster::rgba8(frame.rgba, frame.width, frame.height));
                    changed = true;
                }
            }
            _ => {}
        }
        changed
    }

    /// The connected VNC session, if that's the current live transport.
    fn vnc_session(&self) -> Option<&VncSession> {
        match &self.live {
            Some(LiveSession::Vnc(s)) if s.status() == SessionStatus::Connected => Some(s),
            _ => None,
        }
    }

    /// Map a logical point to remote-desktop pixel coordinates, if it falls
    /// on the live VNC framebuffer.
    fn vnc_pointer_target(&self, p: Point) -> Option<(u16, u16)> {
        let s = self.vnc_session()?;
        let canvas = self.fb_canvas?;
        if !canvas.contains(p) || canvas.width() <= 0.0 || canvas.height() <= 0.0 {
            return None;
        }
        let (w, h) = s.size()?;
        if w == 0 || h == 0 {
            return None;
        }
        let fx =
            ((p.x - canvas.x0) / canvas.width() * f64::from(w)).clamp(0.0, f64::from(w - 1)) as u16;
        let fy = ((p.y - canvas.y0) / canvas.height() * f64::from(h)).clamp(0.0, f64::from(h - 1))
            as u16;
        Some((fx, fy))
    }

    /// Release on the remote machine every key this pane still holds there:
    /// the pane lost the keyboard, and the release will not reach it.
    fn release_held_keys(&mut self) {
        for (_, key) in std::mem::take(&mut self.held_keys) {
            self.send_release(key);
        }
    }

    /// Release on the remote machine the key this pane pressed there as
    /// `physical`, if it still holds it: the key came up, whether or not its
    /// release reaches the pane.
    fn release_key(&mut self, physical: PhysicalKey) {
        if let Some(at) = self.held_keys.iter().position(|(key, _)| *key == physical) {
            let (_, key) = self.held_keys.remove(at);
            self.send_release(key);
        }
    }

    /// Send the release of `key`, as its press sent it: a keysym to a VNC
    /// desktop, an evdev keycode to a Tilo session.
    fn send_release(&mut self, key: u32) {
        if let Some(s) = self.vnc_session() {
            let _ = s.send_input(VncInput::Key { down: false, key });
        } else {
            self.queue_tilo_input(runtime_collab::WorkspaceControlInput::Key {
                keycode: key,
                pressed: false,
                modifiers: 0,
            });
        }
    }

    /// Note that `key` was pressed (`Some`) or released on the remote
    /// machine; a release answers with what its press sent.
    fn track_key(&mut self, physical: PhysicalKey, pressed: Option<u32>) -> Option<u32> {
        let at = self.held_keys.iter().position(|(key, _)| *key == physical);
        let released = at.map(|at| self.held_keys.remove(at).1);
        if let Some(sent) = pressed {
            self.held_keys.push((physical, sent));
            return Some(sent);
        }
        released
    }

    /// Scroll the live remote desktop under `p` by `delta`: each whole wheel
    /// click is a press and release of the wheel's button (RFB 6143 §7.5.5),
    /// at most [`VNC_WHEEL_BURST`] of them per event. `false` when the
    /// pointer is not over the desktop. A trackpad clicks every
    /// `wheel_line` logical pixels.
    fn forward_vnc_wheel(
        &mut self,
        p: Point,
        delta: &MouseScrollDelta,
        scale_factor: f64,
        wheel_line: f64,
    ) -> bool {
        let Some((x, y)) = self.vnc_pointer_target(p) else {
            return false;
        };
        let clicks = vnc_wheel_clicks(&mut self.vnc_wheel, delta, scale_factor, wheel_line);
        if let Some(s) = self.vnc_session() {
            for button in clicks {
                for button_mask in [self.vnc_buttons | button, self.vnc_buttons] {
                    let _ = s.send_input(VncInput::Pointer { button_mask, x, y });
                }
            }
        }
        true
    }

    /// Send the current button mask at `p` to the live remote desktop, if
    /// the pointer is over it.
    fn forward_vnc_pointer(&self, p: Point) {
        if let (Some(s), Some((x, y))) = (self.vnc_session(), self.vnc_pointer_target(p)) {
            let _ = s.send_input(VncInput::Pointer {
                button_mask: self.vnc_buttons,
                x,
                y,
            });
        }
    }

    fn tilo_pointer_target(&self, p: Point) -> Option<(f64, f64)> {
        let Some(LiveSession::Tilo(session)) = self.live.as_ref() else {
            return None;
        };
        if session.status != SessionStatus::Connected {
            return None;
        }
        let (width, height) = session.size?;
        let canvas = self.fb_canvas?;
        if !canvas.contains(p) || canvas.width() <= 0.0 || canvas.height() <= 0.0 {
            return None;
        }
        Some((
            ((p.x - canvas.x0) / canvas.width() * f64::from(width))
                .clamp(0.0, f64::from(width.saturating_sub(1))),
            ((p.y - canvas.y0) / canvas.height() * f64::from(height))
                .clamp(0.0, f64::from(height.saturating_sub(1))),
        ))
    }

    fn queue_tilo_input(&mut self, event: runtime_collab::WorkspaceControlInput) -> bool {
        let Some(LiveSession::Tilo(session)) = self.live.as_ref() else {
            return false;
        };
        if session.status != SessionStatus::Connected {
            return false;
        }
        let session_id = session.id;
        self.collab_outbox
            .push(ui::collaboration::CollabRequest::WorkspaceControlInput { session_id, event });
        true
    }
}

type SharedSession = Rc<RefCell<SessionCore>>;

/// What the window currently shows, so it is rebuilt only when that changes.
fn fingerprint(value: &impl Hash) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// What the content column shows.
#[derive(Hash)]
enum Content {
    List {
        destination: Destination,
        rows: Vec<RowModel>,
        has_connections: bool,
    },
    Form(FormModel),
    Status {
        id: ConnectionId,
        status: Option<String>,
    },
    Surface(ConnectionId),
    Missing,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent flag (fullscreen, layout and paint gates, \
              capture); grouping them would express no invariant between them"
)]
pub struct RemoteApp {
    /// The saved connections as the resource last presented them, plus what
    /// this device observed of their reachability.
    workspace: Workspace,
    /// Which screen is showing.
    screen: Screen,
    /// Which list the rail points at.
    destination: Destination,
    /// The connection last opened, saved or found by search, marked in the
    /// list.
    selected: Option<ConnectionId>,
    /// In-progress add / edit draft (only meaningful on the form).
    draft: ConnectionDraft,
    /// The text behind the form's fields.
    fields: FormFields,
    /// What stops the draft from being saved; empty until Save is tried,
    /// then kept current as the person edits.
    problems: FormProblems,
    /// Save was tried on this draft, so its problems follow every edit.
    validating: bool,
    /// The open session's transport, shared with its pane.
    session: SharedSession,
    /// Connection id the open session belongs to.
    session_for: Option<ConnectionId>,
    /// Monotonic identity for a live session. A saved profile can be reopened
    /// many times; resource-operation completion must never settle an old
    /// invocation against a newer session for the same connection id.
    session_generation: u64,
    /// The connect attempt in flight: when it started, and the job the
    /// whole system sees while it runs.
    connect_started: Option<ui::time::Instant>,
    connect_job: Option<(ConnectionId, ui::jobs::Running)>,
    /// The session fills the window, without the rail.
    fullscreen: bool,
    /// Discoverable Tilo peers for the form's device picker.
    tilo_devices: Vec<TiloDevice>,
    next_tilo_session_id: u64,

    /// The resource's door: the single path by which a saved connection is
    /// added, edited, renamed, pinned, or deleted, shared with the palette,
    /// agents and scripts. `None` until the framework attaches the surface.
    dispatch: Option<ui::framework::Dispatcher<crate::resource::Remote>>,
    /// This device's store for the connections' passwords and passphrases.
    secrets: Option<ui::secrets::SecretStore>,
    /// This device's SSH host-key trust, in the app's device-local settings.
    known_hosts: crate::ssh::KnownHostStore,
    /// Built for a screenshot: no transport starts.
    capture: bool,
    /// The time, in milliseconds since the Unix epoch, "last used" reads
    /// against in a screenshot.
    now_override: Option<i64>,
    /// What this window submitted, for tests of a bare widget.
    #[cfg(test)]
    submitted: RefCell<Vec<Invoke<crate::model::Remote>>>,
    pending_resource_operation: Option<PendingResourceOperation>,
    /// Confirmation mirroring resource-action policy. Private view state,
    /// never replicated with the connection profile.
    pending_confirmation: Option<PendingConfirmation>,
    dialog: Option<ModalDialog<Column>>,

    /// The window: rail and current screen.
    shell: NavShell,
    /// The session alone, while it fills the window.
    immersive: Option<SessionPane>,
    /// When the session went full screen, while the way out is said over
    /// it ([`FULLSCREEN_HINT`]).
    fullscreen_hint: Option<ui::time::Instant>,
    /// The queue every control reports to.
    requests: Requests,
    /// Search hits picked in the platform search, waiting to be shown.
    opened: Rc<RefCell<Vec<String>>>,
    /// The fingerprint of what the window was last built to show.
    built: Option<u64>,
    rect: Rect,
    layout_dirty: bool,
    dirty: bool,
}

impl RemoteApp {
    /// The live app starts empty. Only the user's saved connections (held in
    /// the shared document and persisted across sessions) appear.
    pub fn new() -> Self {
        Self::from_parts(Workspace::new(), Vec::new())
    }

    fn from_parts(workspace: Workspace, tilo_devices: Vec<TiloDevice>) -> Self {
        Self {
            workspace,
            screen: Screen::List,
            destination: Destination::Connections,
            selected: None,
            draft: ConnectionDraft::default(),
            fields: FormFields::default(),
            problems: FormProblems::default(),
            validating: false,
            session: SharedSession::default(),
            session_for: None,
            session_generation: 0,
            connect_started: None,
            connect_job: None,
            fullscreen: false,
            tilo_devices,
            next_tilo_session_id: 1,
            dispatch: None,
            known_hosts: crate::ssh::KnownHostStore::of_session(),
            secrets: None,
            capture: false,
            now_override: None,
            #[cfg(test)]
            submitted: RefCell::default(),
            pending_resource_operation: None,
            pending_confirmation: None,
            dialog: None,
            shell: NavShell::new()
                .content_kind(ui::widget::ContentKind::Document)
                .with_id("remote.shell")
                .with_window_toolbar()
                .mode(NavMode::Docked),
            immersive: None,
            fullscreen_hint: None,
            requests: Requests::default(),
            opened: Rc::default(),
            built: None,
            rect: Rect::ZERO,
            layout_dirty: true,
            dirty: true,
        }
    }

    /// A bare widget holding the sample roster and what a capture observes
    /// of it, for tests.
    #[cfg(test)]
    fn sample() -> Self {
        let sample = Sample::new();
        let mut workspace = Workspace::new();
        workspace.connections = sample_connections();
        workspace.connections.push(sample_legacy_rdp());
        workspace.presence = sample.presence;
        let mut app = Self::from_parts(workspace, sample.devices);
        app.now_override = Some(sample.now_ms);
        app
    }

    /// Where the platform search hands the hits a person picks: the view
    /// registers an opener that pushes each hit's anchor here.
    pub(crate) fn open_requests(&self) -> Rc<RefCell<Vec<String>>> {
        Rc::clone(&self.opened)
    }

    /// Stage the view state of a named screenshot scenario. The connections
    /// themselves come from the scenario's resource actions
    /// ([`crate::resource::RemoteShell::scenario`]); what they do not hold
    /// (reachability, nearby devices) from the capture's [`Sample`].
    pub(crate) fn stage(&mut self, scenario: Option<&str>) {
        match scenario.unwrap_or_default() {
            "pinned" => self.apply(Action::Show(Destination::Pinned)),
            "form" => self.apply(Action::NewConnection),
            "form-tilo" => {
                self.apply(Action::NewConnection);
                self.apply(Action::SetKind(ConnectionKind::Tilo));
                self.apply(Action::PickTiloDevice(2));
            }
            "form-invalid" => {
                self.apply(Action::NewConnection);
                self.fields.host.set("build.lab.internal".into());
                self.fields.port.set(String::new());
                self.apply(Action::SaveForm);
            }
            "edit" => {
                if let Some(id) = self.first_of_kind(ConnectionKind::Ssh) {
                    self.apply(Action::EditConnection(id));
                }
            }
            "session-ssh" => self.sample_session(ConnectionKind::Ssh),
            "session-vnc" => self.sample_session(ConnectionKind::Vnc),
            "session-rdp" => {
                if let Some(id) = self.first_of_kind(ConnectionKind::Rdp) {
                    self.open_session(id);
                }
            }
            "session-tilo" => {
                self.preview_session(ConnectionKind::Tilo, SessionStatus::Connecting);
            }
            "session-connecting" => {
                self.preview_session(ConnectionKind::Ssh, SessionStatus::Connecting);
            }
            "session-failed" => {
                if let Some(address) = self
                    .first_of_kind(ConnectionKind::Vnc)
                    .and_then(|id| self.workspace.get(id))
                    .map(Connection::address)
                {
                    self.preview_session(
                        ConnectionKind::Vnc,
                        SessionStatus::Failed(format!("{address} refused the connection.")),
                    );
                }
            }
            "session-closed" => {
                self.preview_session(ConnectionKind::Ssh, SessionStatus::Closed);
            }
            "confirm-connect" => {
                if let Some(id) = self.first_of_kind(ConnectionKind::Tilo) {
                    self.request_connect(id);
                }
            }
            "confirm-delete" => {
                if let Some(id) = self.first_of_kind(ConnectionKind::Ssh) {
                    self.request_delete(id);
                }
            }
            "confirm-ssh-trust" => {
                if let Some(id) = self.first_of_kind(ConnectionKind::Ssh)
                    && let Some(connection) = self.workspace.get(id)
                {
                    self.pending_confirmation = Some(PendingConfirmation::TrustSshHostKey {
                        id,
                        target: Self::confirmation_target(connection),
                        observed: crate::ssh::HostKeyIdentity {
                            algorithm: "ssh-ed25519".into(),
                            fingerprint: "SHA256:k3nN0Z9mF2a7QxRwYp4Lc8uVb1HsTe6JdGq5oWiXyKA"
                                .into(),
                        },
                    });
                    self.show_confirmation();
                }
            }
            _ => {}
        }
        self.dirty = true;
        self.check_views();
    }

    /// Stage a session in a fixed status, with no transport (shots and
    /// tests only).
    fn preview_session(&mut self, kind: ConnectionKind, status: SessionStatus) {
        if let Some(id) = self.first_of_kind(kind) {
            self.session_for = Some(id);
            self.session.borrow_mut().live = Some(LiveSession::Preview(status));
            self.screen = Screen::Session { id };
            self.destination = Destination::Session;
        }
    }

    /// Stage a connected session of `kind` showing its sample surface, with
    /// no transport (shots and tests only).
    fn sample_session(&mut self, kind: ConnectionKind) {
        let Some(id) = self.first_of_kind(kind) else {
            return;
        };
        let mut session = self.session.borrow_mut();
        if kind.is_terminal() {
            session.live = Some(LiveSession::Sample(SampleSurface::Terminal(Box::new(
                crate::sample::terminal(),
            ))));
        } else {
            let (width, height) = crate::sample::DESKTOP_SIZE;
            session.fb_image = Some(ui::raster::rgba8(crate::sample::desktop(), width, height));
            session.live = Some(LiveSession::Sample(SampleSurface::Desktop));
        }
        drop(session);
        self.session_for = Some(id);
        self.selected = Some(id);
        self.screen = Screen::Session { id };
        self.destination = Destination::Session;
    }

    /// Id of the first saved connection of a given kind, if any.
    fn first_of_kind(&self, kind: ConnectionKind) -> Option<ConnectionId> {
        self.ordered()
            .into_iter()
            .find(|c| c.kind == kind)
            .map(|c| c.id)
    }

    /// The connections in list order: favourites first, then by name.
    fn ordered(&self) -> Vec<&Connection> {
        self.workspace.filtered("")
    }

    /// Apply a queued [`Action`] to the app state.
    fn apply(&mut self, action: Action) {
        self.dirty = true;
        match action {
            Action::Show(destination) => {
                // Leaving a session that has ended or never opened closes it.
                if destination != Destination::Session
                    && self.session_for.is_some()
                    && !self.session.borrow().live_active()
                {
                    self.disconnect_session();
                }
                self.destination = destination;
                self.screen = match (destination, self.session_for) {
                    (Destination::Session, Some(id)) => Screen::Session { id },
                    _ => Screen::List,
                };
            }
            Action::Back => {
                self.screen = Screen::List;
                if self.destination == Destination::Session {
                    self.destination = Destination::Connections;
                }
            }
            Action::NewConnection => {
                self.draft = ConnectionDraft::default();
                self.problems = FormProblems::default();
                self.validating = false;
                self.screen = Screen::Form { editing: None };
                self.load_fields();
                self.fields.secret.set(String::new());
            }
            Action::EditConnection(id) => {
                if let Some(c) = self.workspace.get(id) {
                    self.draft = ConnectionDraft::from_connection(c);
                    self.problems = FormProblems::default();
                    self.validating = false;
                    self.screen = Screen::Form { editing: Some(id) };
                    self.load_fields();
                    // A saved credential is never read back into the form;
                    // typing one replaces it.
                    self.fields.secret.set(String::new());
                }
            }
            Action::RequestConnect(id) => self.request_connect(id),
            Action::ToggleFavorite(id) => self.toggle_favorite(id),
            Action::RequestDelete(id) => self.request_delete(id),
            Action::RequestTrustSshHostKey(id) => self.request_trust_ssh_host_key(id),
            Action::RequestForgetSshHostKey(id) => self.request_forget_ssh_host_key(id),
            Action::ConfirmPending => self.confirm_pending(),
            Action::CancelPending => self.pending_confirmation = None,
            Action::SetKind(k) => {
                self.read_fields();
                self.draft.kind = k;
                // Keep the port in step with the kind's default unless the
                // user already typed a custom one for this kind.
                if k == ConnectionKind::Tilo {
                    self.draft.port = String::new();
                    self.draft.auth = AuthMethod::DeviceIdentity;
                } else if self.draft.port.trim().is_empty() || self.draft.port.trim() == "0" {
                    self.draft.port = k.default_port().to_string();
                }
                if !self.draft.auth.is_supported_by(k)
                    && let Some(auth) = AuthMethod::available_for(k).first().copied()
                {
                    self.draft.auth = auth;
                }
                self.load_fields();
            }
            Action::SetAuth(a) => self.draft.auth = a,
            Action::PickTiloDevice(i) => {
                if let Some(d) = self.tilo_devices.get(i).cloned() {
                    self.read_fields();
                    self.draft.name = d.name;
                    self.draft.host = d.node;
                    self.draft.kind = ConnectionKind::Tilo;
                    self.draft.auth = AuthMethod::DeviceIdentity;
                    self.draft.port = String::new();
                    self.load_fields();
                }
            }
            Action::SaveForm => self.save_form(),
            Action::ToggleScale => {
                let mut session = self.session.borrow_mut();
                session.scale_to_fit = !session.scale_to_fit;
                session.changed = true;
            }
            Action::ToggleFullscreen => self.set_fullscreen(!self.fullscreen),
            Action::Disconnect => self.request_disconnect(),
        }
        if self.pending_confirmation.is_none() {
            self.dialog = None;
        }
        self.check_views();
    }

    fn set_fullscreen(&mut self, fullscreen: bool) {
        let fullscreen = fullscreen && matches!(self.screen, Screen::Session { .. });
        if fullscreen == self.fullscreen {
            return;
        }
        self.fullscreen = fullscreen;
        self.immersive = fullscreen.then(|| SessionPane::new(Rc::clone(&self.session)));
        // Full screen hides the window's bar: the way out is said over the
        // session for a moment.
        self.fullscreen_hint = fullscreen.then(ui::time::now);
        self.layout_dirty = true;
        self.dirty = true;
    }

    /// Run every queued action.
    fn apply_requests(&mut self) {
        loop {
            let queued = std::mem::take(&mut *self.requests.borrow_mut());
            if queued.is_empty() {
                break;
            }
            for action in queued {
                self.apply(action);
            }
        }
    }

    /// Show the connection a picked search hit points at.
    fn open_found(&mut self) {
        let anchors = std::mem::take(&mut *self.opened.borrow_mut());
        for anchor in anchors {
            let Some(id) = self
                .workspace
                .connections()
                .iter()
                .map(|connection| connection.id)
                .find(|id| crate::resource::connection_resource_id(*id) == anchor)
            else {
                continue;
            };
            self.selected = Some(id);
            self.destination = Destination::Connections;
            self.screen = Screen::List;
            self.dirty = true;
        }
    }

    fn toggle_favorite(&mut self, id: ConnectionId) {
        // Compute the absolute target before the in-memory flip so replay does
        // not depend on the connection's current state.
        let Some(target) = self
            .workspace
            .get(id)
            .map(|connection| !connection.favorite)
        else {
            return;
        };
        self.workspace.toggle_favorite(id);
        let item = crate::model::Remote::item(id);
        let pinned = crate::model::Remote::paths()
            .connections
            .item(&item)
            .favorite;
        self.submit(Invoke::edit(&pinned.set(target)));
    }

    fn delete_connection(&mut self, id: ConnectionId) {
        self.workspace.remove(id);
        // Asked already, in this window's own dialog.
        let saved = crate::model::Remote::paths().connections;
        self.submit(Invoke::edit(&saved.remove(&crate::model::Remote::item(id))));
        self.forget_credentials(id);
        if self.selected == Some(id) {
            self.selected = None;
        }
        if matches!(self.screen, Screen::Form { editing: Some(editing) } if editing == id) {
            self.screen = Screen::List;
        }
    }

    /// Forget the passwords and passphrase this device saved for a
    /// connection that is gone, whichever door or device deleted it. A
    /// capture never forgets one.
    fn forget_credentials(&self, id: ConnectionId) {
        if self.capture {
            return;
        }
        if let Some(secrets) = &self.secrets {
            for purpose in [SSH_PASSWORD, KEY_PASSPHRASE, VNC_PASSWORD] {
                let _ = secrets.item(purpose, &id.to_string()).delete_blocking();
            }
        }
    }

    /// Submit a field edit or verb of the saved connections through the
    /// resource's door — the one way this window changes them, shared with
    /// the palette, agents and scripts. The rows it changes are presented
    /// back; the caller updates the workspace for immediate feedback.
    /// Without a dispatcher (a bare widget in a test) nothing is saved.
    fn submit(&self, invoke: Result<Invoke<crate::model::Remote>, serde_json::Error>) {
        // A connection always encodes; nothing to submit otherwise.
        let Ok(invoke) = invoke else {
            return;
        };
        #[cfg(test)]
        self.submitted.borrow_mut().push(invoke.clone());
        if let Some(dispatch) = &self.dispatch {
            dispatch.submit(invoke);
        }
    }

    /// The credential this device saved for a connection, for `purpose`.
    fn saved_secret(&self, purpose: &'static str, id: ConnectionId) -> Option<String> {
        let secret = self
            .secrets
            .as_ref()?
            .item(purpose, &id.to_string())
            .get_blocking()
            .ok()??;
        String::from_utf8(secret.expose().to_vec()).ok()
    }

    /// The time "last used" reads against, in milliseconds since the Unix
    /// epoch.
    fn now_ms(&self) -> i64 {
        self.now_override.unwrap_or_else(ui::time::unix_now_ms_i64)
    }

    /// Stable, non-secret identity used to revalidate a confirmation after a
    /// collaborator may have edited the shared profile.
    fn confirmation_target(connection: &Connection) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            connection.kind.tag(),
            connection.host.0,
            connection.port,
            connection.user.0,
            connection.auth.label()
        )
    }

    /// An outcome worth a moment's notice (a revalidation that cancelled an
    /// action), in the window's bar. It holds no credential material.
    /// Record what happened in the ledger: Remote's outcomes do not expire
    /// and nothing of them takes the keyboard.
    fn notify(&mut self, message: impl Into<String>) {
        ui::attention::publish(ui::attention::Notice::did(owner(), message));
    }

    /// Commit the form's draft through the resource, and its credential into
    /// this device's secret store.
    fn save_form(&mut self) {
        self.read_fields();
        let problems = self.draft.problems();
        if !problems.is_empty() {
            self.problems = sort_problems(problems);
            self.validating = true;
            return;
        }
        self.problems = FormProblems::default();
        self.validating = false;
        let editing = match &self.screen {
            Screen::Form { editing } => *editing,
            _ => None,
        };
        let id = editing.unwrap_or_else(Uuid::new_v4);
        let mut conn = self.draft.build(id);
        // Preserve favorite + last used across an edit.
        if let Some(existing) = self.workspace.get(id) {
            conn.favorite = existing.favorite;
            conn.last_used_ms = existing.last_used_ms;
        }
        // Only the fields the form changed, as one step: a collaborator's
        // pin or a use recorded meanwhile stays.
        let invoke = save_invoke(self.workspace.connections(), &conn);
        let purpose = secret_purpose(conn.kind, conn.auth);
        self.workspace.upsert(conn);
        if let Some(invoke) = invoke {
            self.submit(invoke);
        }
        let secret = self.fields.secret.with(Clone::clone);
        self.fields.secret.set(String::new());
        if !secret.is_empty()
            && let Some(secrets) = &self.secrets
            && let Err(error) = secrets
                .item(purpose, &id.to_string())
                .put_blocking(secret.as_bytes())
        {
            self.notify(format!(
                "Couldn't save the password on this device: {error}"
            ));
        }
        self.selected = Some(id);
        self.screen = Screen::List;
    }

    /// Once Save has been tried, keep the form's problems in step with what
    /// the person types, so a fixed field loses its error at once.
    fn revalidate(&mut self) {
        if !self.validating || !matches!(self.screen, Screen::Form { .. }) {
            return;
        }
        self.read_fields();
        let problems = sort_problems(self.draft.problems());
        if problems != self.problems {
            self.problems = problems;
            self.dirty = true;
        }
    }

    /// Copy the draft into the form's fields.
    fn load_fields(&self) {
        let fields = &self.fields;
        fields.name.set(self.draft.name.clone());
        fields.host.set(self.draft.host.clone());
        fields.port.set(self.draft.port.clone());
        fields.user.set(self.draft.user.clone());
        fields.group.set(self.draft.group.clone());
    }

    /// Copy the form's fields into the draft.
    fn read_fields(&mut self) {
        let read = |state: &LocalState<String>| state.with(Clone::clone);
        self.draft.name = read(&self.fields.name);
        self.draft.host = read(&self.fields.host);
        self.draft.port = read(&self.fields.port)
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        self.draft.user = read(&self.fields.user);
        self.draft.group = read(&self.fields.group);
    }

    /// Show the saved connections the resource holds: this window's edits,
    /// a collaborator's, an undo. A session to a connection that is gone
    /// ends.
    pub(crate) fn adopt_resource_state(&mut self, state: &[crate::model::Connection]) {
        if self.workspace.connections() == state {
            return;
        }
        // A connection deleted elsewhere (the palette, an agent, another
        // device) takes this device's credentials with it, as deleting it
        // here does.
        let gone: Vec<ConnectionId> = self
            .workspace
            .connections()
            .iter()
            .filter(|old| state.iter().all(|now| now.id != old.id))
            .map(|old| old.id)
            .collect();
        for id in gone {
            self.forget_credentials(id);
        }
        self.workspace.connections = state.to_vec();
        self.forget_vanished();
        self.dirty = true;
        self.check_views();
    }

    /// Drop references to connections a collaborator deleted.
    fn forget_vanished(&mut self) {
        if self
            .selected
            .is_some_and(|id| self.workspace.get(id).is_none())
        {
            self.selected = None;
        }
        if self
            .session_for
            .is_some_and(|id| self.workspace.get(id).is_none())
        {
            self.disconnect_session();
        }
        if let Screen::Form { editing: Some(id) } = self.screen
            && self.workspace.get(id).is_none()
        {
            self.screen = Screen::List;
        }
    }

    /// Take the resource's door and this device's secret store when hosted
    /// as a [`ResourceSurface`]; a capture shows its sample's reachability
    /// and nearby devices.
    ///
    /// [`ResourceSurface`]: ui::framework::ResourceSurface
    pub(crate) fn attach_framework(
        &mut self,
        cx: &ui::framework::SurfaceCx<crate::resource::Remote>,
    ) {
        self.dispatch = Some(cx.dispatcher());
        self.secrets = Some(cx.secrets());
        self.known_hosts = crate::ssh::KnownHostStore::in_settings(
            cx.storage().settings::<crate::ssh::KnownHostsSetting>(),
        );
        self.capture = cx.mode() == ui::framework::RunMode::Capture;
        if let Some(sample) = cx.fixture::<Sample>() {
            self.workspace.presence.clone_from(&sample.presence);
            self.tilo_devices.clone_from(&sample.devices);
            self.now_override = Some(sample.now_ms);
        }
    }

    /// One list row per connection the destination shows. The meta says
    /// when the machine was last used, or what about the saved connection
    /// stops it from opening; what this device lacks is said when a session
    /// is opened, where it can be acted on.
    fn rows(&self) -> Vec<RowModel> {
        let now_ms = self.now_ms();
        self.ordered()
            .into_iter()
            .filter(|c| self.destination != Destination::Pinned || c.favorite)
            .map(|c| {
                let meta = match connection_readiness(c) {
                    readiness @ (TransportReadiness::BackendUnavailable
                    | TransportReadiness::InvalidProfile) => readiness.label().to_owned(),
                    TransportReadiness::Ready | TransportReadiness::NeedsLiveSessionCapability => {
                        // Another device's clock may run ahead of this one.
                        c.last_used_ms.map_or_else(
                            || "Never connected".to_owned(),
                            |at_ms| {
                                let secs_ago = now_ms.saturating_sub(at_ms).max(0) / 1000;
                                ui::format::capitalize_first(&ui::format::relative(secs_ago))
                            },
                        )
                    }
                };
                RowModel {
                    id: c.id,
                    name: c.name.clone(),
                    kind: c.kind,
                    address: if c.kind == ConnectionKind::Tilo {
                        c.kind.label().to_owned()
                    } else {
                        c.address()
                    },
                    meta,
                    presence: self.workspace.presence_of(c.id),
                    favourite: c.favorite,
                    selected: self.selected == Some(c.id),
                }
            })
            .collect()
    }

    fn form_model(&self, editing: Option<ConnectionId>) -> FormModel {
        FormModel {
            editing,
            kind: self.draft.kind,
            auth: self.draft.auth,
            devices: if self.draft.kind == ConnectionKind::Tilo {
                self.tilo_devices
                    .iter()
                    .map(|d| DeviceModel {
                        name: d.name.clone(),
                        online: d.online,
                        picked: self.draft.host == d.node,
                    })
                    .collect()
            } else {
                Vec::new()
            },
            problems: self.problems.clone(),
        }
    }

    fn content(&self) -> Content {
        match self.screen {
            Screen::List => Content::List {
                destination: self.destination,
                rows: self.rows(),
                has_connections: !self.workspace.connections().is_empty(),
            },
            Screen::Form { editing } => Content::Form(self.form_model(editing)),
            Screen::Session { id } => {
                if self.workspace.get(id).is_none() {
                    return Content::Missing;
                }
                let session = self.session.borrow();
                let surface = matches!(session.status(), Some(SessionStatus::Connected))
                    && !matches!(session.live, Some(LiveSession::Preview(_)));
                if surface {
                    Content::Surface(id)
                } else {
                    Content::Status {
                        id,
                        status: session.status().map(|s| format!("{s:?}")),
                    }
                }
            }
        }
    }

    /// Everything the window's views are built from.
    fn view_key(&self) -> u64 {
        let session = self.session.borrow();
        fingerprint(&(
            self.content(),
            self.session_for
                .and_then(|id| self.workspace.get(id))
                .map(|c| (c.name.clone(), c.kind.tag())),
            session.scale_to_fit,
            self.destination,
            self.fullscreen,
        ))
    }

    /// Ask for a rebuild when what the views show changed.
    fn check_views(&mut self) {
        if self.built != Some(self.view_key()) {
            self.layout_dirty = true;
            self.dirty = true;
        }
    }

    /// Put the current screen and the rail into the shell.
    fn rebuild_views(&mut self) {
        let key = self.view_key();
        if self.built == Some(key) {
            return;
        }
        self.built = Some(key);
        let session_kind = self
            .session_for
            .and_then(|id| self.workspace.get(id))
            .map(|c| c.kind);
        let rail = view::rail(&self.requests, self.destination, session_kind);
        let content: Element = match self.content() {
            Content::List {
                destination,
                rows,
                has_connections,
            } => view::connection_list(&self.requests, destination, &rows, has_connections),
            Content::Form(model) => view::connection_form(&self.requests, &self.fields, &model),
            Content::Surface(_) => Box::new(SessionPane::new(Rc::clone(&self.session))),
            Content::Status { id, .. } => {
                view::session_status(&self.requests, self.status_model(id))
            }
            Content::Missing => Box::new(
                ui::widget::EmptyState::new("This connection was deleted")
                    .with_icon(Icon::Monitor)
                    .with_message("Someone you share your connections with removed it."),
            ),
        };
        self.shell.set_sidebar(Some(Box::new(rail)));
        self.shell.set_content(content);
    }

    /// What a session that shows no remote surface says, and the one way
    /// forward it offers.
    fn status_model(&self, id: ConnectionId) -> StatusModel {
        let connection = self.workspace.get(id);
        let name = connection.map_or("the machine", |c| c.name.as_str());
        let kind = connection.map_or(ConnectionKind::Ssh, |c| c.kind);
        let icon = view::kind_icon(kind);
        let status = self.session.borrow().status();
        match status {
            None => StatusModel {
                icon,
                title: "Not connected".to_owned(),
                message: format!("No session to {name} is open."),
                error: false,
                action: Some(("Connect", Action::RequestConnect(id))),
            },
            Some(SessionStatus::Connecting) => {
                if kind == ConnectionKind::Tilo {
                    StatusModel {
                        icon,
                        title: format!("Waiting for {name}"),
                        message: "Its owner is asked before the device shares a workspace."
                            .to_owned(),
                        error: false,
                        action: Some(("Stop", Action::Disconnect)),
                    }
                } else {
                    StatusModel {
                        icon,
                        title: format!("Connecting to {name}"),
                        message: "Remote is reaching the machine and signing in. This stops on its own after 45 seconds.".to_owned(),
                        error: false,
                        action: Some(("Stop", Action::Disconnect)),
                    }
                }
            }
            Some(SessionStatus::Connected) => StatusModel {
                icon,
                title: format!("Connected to {name}"),
                message: "The session is open.".to_owned(),
                error: false,
                action: None,
            },
            Some(SessionStatus::Failed(error)) => {
                let recovery = match self.ssh_host_key_issue(id) {
                    Some(crate::ssh::HostKeyIssue::FirstContact { .. }) => {
                        ("Trust key", Action::RequestTrustSshHostKey(id))
                    }
                    Some(crate::ssh::HostKeyIssue::Changed { .. }) => {
                        ("Forget key", Action::RequestForgetSshHostKey(id))
                    }
                    _ => ("Try again", Action::RequestConnect(id)),
                };
                StatusModel {
                    icon,
                    title: format!("Couldn't connect to {name}"),
                    message: error,
                    error: true,
                    action: Some(recovery),
                }
            }
            Some(SessionStatus::Unavailable(detail)) => StatusModel {
                icon: Icon::Ban,
                title: format!("{name} can't be opened here"),
                message: detail,
                error: false,
                // Editing helps a legacy or malformed profile; it never
                // pretends to grant a capability this build lacks.
                action: (kind == ConnectionKind::Rdp
                    || connection.is_some_and(|c| c.profile_issue().is_some()))
                .then_some(("Edit connection", Action::EditConnection(id))),
            },
            Some(SessionStatus::Closed) => StatusModel {
                icon,
                title: "Session ended".to_owned(),
                message: format!("{name} closed the session."),
                error: false,
                action: Some(("Reconnect", Action::RequestConnect(id))),
            },
        }
    }

    /// The child that fills the window: the shell, or the session alone.
    fn body_mut(&mut self) -> &mut dyn Widget {
        match self.immersive.as_mut() {
            Some(pane) => pane,
            None => &mut self.shell,
        }
    }

    fn body(&self) -> &dyn Widget {
        match self.immersive.as_ref() {
            Some(pane) => pane,
            None => &self.shell,
        }
    }

    /// Keys the app answers itself, before the window's controls.
    fn handle_shortcut(&self, event: &WindowEvent, mods: ModifiersState) -> Option<Action> {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return None;
        };
        let command = mods.control_key() || mods.super_key();
        // A connected session's keys belong to the remote machine.
        let surface = matches!(self.content(), Content::Surface(_));
        match logical_key {
            Key::Character(c) if command && !surface && c.eq_ignore_ascii_case("n") => {
                Some(Action::NewConnection)
            }
            Key::Named(NamedKey::Escape) if self.fullscreen => Some(Action::ToggleFullscreen),
            _ => None,
        }
    }

    /// Escape, when nothing in the window wanted it, leaves the form.
    fn handle_unclaimed(event: &WindowEvent, screen: &Screen) -> Option<Action> {
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if matches!(screen, Screen::Form { .. }) => Some(Action::Back),
            _ => None,
        }
    }
}

impl Default for RemoteApp {
    fn default() -> Self {
        Self::new()
    }
}

/// Sort Save's problems by the field that repairs each.
/// What saving `conn` from the form submits: the new connection, or the
/// fields an edit of one of the `saved` connections changed, as one step;
/// `None` when an edit changed nothing.
pub(crate) fn save_invoke(
    saved: &[Connection],
    conn: &Connection,
) -> Option<Result<Invoke<Remote>, serde_json::Error>> {
    let connections = Remote::paths().connections;
    let item = Remote::item(conn.id);
    let Some(before) = saved.iter().find(|saved| saved.id == conn.id) else {
        return Some(Invoke::edit(&connections.insert_as(
            &item,
            saved.len(),
            conn,
        )));
    };
    let edits = connections.item(&item).path().edits(before, conn);
    if edits.is_empty() {
        return None;
    }
    Some(
        edits
            .iter()
            .map(Invoke::edit)
            .collect::<Result<Vec<_>, _>>()
            .map(Invoke::group),
    )
}

fn sort_problems(problems: Vec<String>) -> FormProblems {
    let mut sorted = FormProblems::default();
    for problem in problems {
        if problem.starts_with("Name") {
            sorted.name.get_or_insert(problem);
        } else if problem.starts_with("Host") || problem.starts_with("Tilo device node") {
            sorted.host.get_or_insert(problem);
        } else if problem.starts_with("Port") {
            sorted.port.get_or_insert(problem);
        } else {
            sorted.other.push(problem);
        }
    }
    sorted
}

fn child_node(taffy: &TaffyTree<()>, node: NodeId) -> Option<NodeId> {
    taffy.children(node).ok()?.first().copied()
}

// ===========================================================================
// Widget impl
// ===========================================================================

impl Collaborative for RemoteApp {
    fn take_collab_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        std::mem::take(&mut self.session.borrow_mut().collab_outbox)
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        let ui::collaboration::CollabEvent::WorkspaceControl { session_id, event } = event else {
            return;
        };
        let mut core = self.session.borrow_mut();
        let Some(LiveSession::Tilo(session)) = core.live.as_mut() else {
            return;
        };
        if session.id != *session_id {
            return;
        }
        let mut keyframe_request = None;
        match event {
            runtime_collab::WorkspaceControlEvent::WaitingForConsent { .. } => {
                session.status = SessionStatus::Connecting;
            }
            runtime_collab::WorkspaceControlEvent::StreamStarted { width, height, .. } => {
                session.size = Some((*width, *height));
                session.status = SessionStatus::Connected;
            }
            runtime_collab::WorkspaceControlEvent::Frame {
                encoded_h264, seq, ..
            } => match session.queue_frame(*seq, encoded_h264.clone()) {
                Ok(request) => keyframe_request = request,
                Err(error) => session.status = SessionStatus::Failed(error),
            },
            runtime_collab::WorkspaceControlEvent::Closed(reason) => {
                session.status = SessionStatus::Failed(match reason {
                    runtime_collab::WorkspaceControlCloseReason::Denied(message) => message.clone(),
                    runtime_collab::WorkspaceControlCloseReason::Revoked => {
                        "The device stopped sharing its workspace.".into()
                    }
                    runtime_collab::WorkspaceControlCloseReason::TargetDisconnected => {
                        "The device disconnected.".into()
                    }
                    runtime_collab::WorkspaceControlCloseReason::RelayDisconnected => {
                        "The encrypted relay disconnected.".into()
                    }
                    runtime_collab::WorkspaceControlCloseReason::LocalDisconnect => {
                        "The session was closed.".into()
                    }
                });
            }
        }
        if let Some(seq) = keyframe_request {
            core.queue_tilo_input(runtime_collab::WorkspaceControlInput::RequestKeyframe { seq });
        }
        core.changed = true;
        drop(core);
        self.dirty = true;
        self.check_views();
    }
}

impl Widget for RemoteApp {
    ui::provides!(
        dyn ui::widget::Collaborative,
        dyn ui::widget::controls::ActionControls
    );

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        // The saved connections arrive through the resource
        // ([`Self::adopt_resource_state`]); here the window catches up on
        // picked search hits and queued controls.
        self.settle_pending_effect();
        self.open_found();
        self.take_window_toolbar_events();
        self.apply_requests();
        self.revalidate();
        self.check_views();
        self.declare_window_toolbar();
        self.body_mut().sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.declare_window_toolbar();
        self.body_mut().update(state, path)
    }

    fn needs_idle_sync(&self) -> bool {
        self.pending_resource_operation.is_some() || !self.opened.borrow().is_empty()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.rebuild_views();
        self.layout_dirty = false;
        let child = self.body_mut().build_layout(taffy, theme, node_map);
        let node = taffy.fill_node(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let origin = Point::new(self.rect.x0, self.rect.y0);
        if let Some(child) = child_node(taffy, node) {
            self.body_mut().apply_layout(taffy, child, origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.pump_session();
        self.check_views();
        self.body_mut()
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.body_mut()
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        self.draw_fullscreen_hint(scene, transform, text_renderer, theme);
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.children()
            .into_iter()
            .filter_map(Widget::next_wake_deadline)
            .chain(self.fullscreen_hint_deadline())
            .min()
    }

    // Seen ahead of the framework's shortcuts, the action palette and a
    // dialog, which keep keys from the session pane: a key the pane pressed
    // on the remote machine is released there when it comes up, even when
    // its release goes to the palette, so Ctrl+K leaves no Ctrl held there.
    fn observe_physical_input(&mut self, input: ui::widget::PhysicalInput) {
        match input {
            ui::widget::PhysicalInput::Key {
                key,
                state: ElementState::Released,
            } => self.session.borrow_mut().release_key(key),
            ui::widget::PhysicalInput::Focused(false) => {
                self.session.borrow_mut().release_held_keys();
            }
            _ => {}
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // A confirmation is modal: nothing reaches the window behind it.
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.handle_detached(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                &mut ctx.state,
                ctx.path,
            );
            *ctx.handled = true;
            self.apply_requests();
            self.dirty = true;
            return;
        }
        if let Some(action) = self.handle_shortcut(event, ctx.modifiers.state()) {
            self.apply(action);
            *ctx.handled = true;
            return;
        }
        if !*ctx.handled
            && let Some(child) = child_node(ctx.taffy, ctx.node_id)
        {
            let path = ctx.path;
            self.body_mut()
                .handle_event(event, &mut ctx.reborrow(path, child));
        }
        if !*ctx.handled
            && let Some(action) = Self::handle_unclaimed(event, &self.screen)
        {
            self.apply(action);
            *ctx.handled = true;
        }
        self.open_found();
        self.apply_requests();
        self.revalidate();
        self.check_views();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children = vec![self.body()];
        if let Some(dialog) = self.dialog.as_ref() {
            children.push(dialog);
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.body());
        if let Some(dialog) = self.dialog.as_ref() {
            visitor(dialog);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::with_capacity(2);
        match self.immersive.as_mut() {
            Some(pane) => children.push(pane),
            None => children.push(&mut self.shell),
        }
        if let Some(dialog) = self.dialog.as_mut() {
            children.push(dialog);
        }
        children
    }

    fn wants_pointer_capture(&self) -> bool {
        self.body().wants_pointer_capture()
    }

    fn name(&self) -> &'static str {
        "RemoteApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("remote_app")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty || self.body().is_layout_dirty()
    }

    // Rebuilding the views (the connections or the screen changed, full
    // screen swapped the body) replaces this widget's children. Say so, or
    // the host re-places the old tree and the change shows only once
    // something else asks for a rebuild: a collaborator's deletion left
    // its row on screen.
    fn take_structure_dirty(&mut self) -> bool {
        let own = self.layout_dirty;
        self.children_mut()
            .into_iter()
            .fold(own, |dirty, child| child.take_structure_dirty() || dirty)
    }

    fn layout_needs_measure(&self) -> bool {
        self.layout_dirty || self.body().layout_needs_measure()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.body().needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn needs_animation_frame(&self) -> bool {
        // A live session needs continuous frames so incoming remote data
        // (PTY output, framebuffer updates) is drained and painted.
        self.session.borrow().live_active() || self.body().needs_animation_frame()
    }
}

impl ui::widget::controls::ActionControls for RemoteApp {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        ui::widget::controls::take_control_invocation(self.body_mut())
    }
}

mod connect;
mod effects;
mod fullscreen_hint;
mod keys;
mod live_session;
mod secrets;
mod seeds;
mod session_pane;
mod window_bar;

pub(crate) use seeds::{Sample, sample_connections, sample_legacy_rdp};
pub(crate) use session_pane::SessionPane;

use keys::vnc_wheel_clicks;
use secrets::{KEY_PASSPHRASE, SSH_PASSWORD, VNC_PASSWORD, secret_purpose};

#[cfg(test)]
mod tests;

impl RemoteApp {
    pub(crate) fn can_send_text(&self) -> bool {
        matches!(
            &self.session.borrow().live,
            Some(LiveSession::Ssh(session)) if session.status() == SessionStatus::Connected
        )
    }
}

//! Root widget for the Remote app.
//!
//! Owns the connection [`Workspace`] (mirrored from the shared document),
//! the current [`Screen`], the add / edit draft and the live session. The
//! window is a [`NavShell`]: a docked rail of destinations beside the
//! current screen, with the screen's title and actions in the header row.
//! The list and the form are toolkit widgets rebuilt from this state
//! ([`crate::view`]); a connected session is [`SessionPane`], which paints
//! the remote terminal grid or framebuffer.
//!
//! Controls report through one queue of [`Action`]s, drained after every
//! event, so the pointer, the keyboard, assistive technology and the
//! resource actions agents call share one path.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Mutex, PoisonError};

use automerge::{AutoCommit, ObjId};
use iroh_base::{EndpointId, SecretKey};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{
    Blob, Fill, ImageAlphaType, ImageBrush, ImageData as PenikoImage, ImageFormat,
};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toast::{ToastKind, ToastStack};
use ui::widget::{
    AppModel, Collaborative, Column, Element, ModalDialog, ModelPath, NavMode, NavShell,
    TaffyTreeExt, Text, Widget,
};
use ui::{LocalState, TextRole};

use crate::doc::RemoteDoc;
use crate::model::{
    AuthMethod, Connection, ConnectionDraft, ConnectionId, ConnectionKind, Presence, Screen,
    Workspace,
};
use crate::session::{SessionStatus, TransportReadiness, connection_readiness};
use crate::ssh::{SshConfig, SshSession};
use crate::view::{
    self, Destination, DeviceModel, FormFields, FormModel, FormProblems, Requests, RowModel,
    StatusModel,
};
use crate::vnc::{VncConfig, VncInput, VncSession};

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
    /// A replace-latest encoded frame slot plus a capacity-one wake channel.
    /// It bounds backlog to one frame while ensuring overload retains the
    /// newest, rather than the oldest, H.264 payload.
    encoded: Arc<Mutex<Option<Vec<u8>>>>,
    encoded_wake: SyncSender<()>,
    decoder_available: Arc<AtomicBool>,
    decoded: Arc<Mutex<Option<tilo_callstream::DecodedFrame>>>,
}

impl TiloSession {
    fn new(id: u64) -> Result<Self, String> {
        let (encoded_wake, encoded_rx) = sync_channel::<()>(1);
        let encoded: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
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
                    let Some(encoded) = lock_recover(&encoded_worker).take() else {
                        continue;
                    };
                    let Some(frame) = h264_decoder.decode(&encoded) else {
                        continue;
                    };
                    *lock_recover(&decoded_worker) = Some(frame);
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
        })
    }

    fn queue_frame(&self, encoded: Vec<u8>) -> Result<(), String> {
        if !self.decoder_available.load(Ordering::Acquire) {
            return Err("Tilo frame decoder is unavailable".into());
        }
        // Store first, then wake. If a wake is already pending, that worker
        // pass observes this replacement; at most one stale decode can be
        // in progress and no queue of old frames accumulates.
        *lock_recover(&self.encoded) = Some(encoded);
        match self.encoded_wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(()),
            Err(TrySendError::Disconnected(())) => {
                self.decoder_available.store(false, Ordering::Release);
                Err("Tilo frame decoder stopped".into())
            }
        }
    }

    fn take_latest_frame(&self) -> Option<tilo_callstream::DecodedFrame> {
        lock_recover(&self.decoded).take()
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

enum PendingResourceOperation {
    Connect {
        invocation: tilo_actions::InvocationId,
        connection_id: ConnectionId,
        session_generation: u64,
    },
    SshInput {
        invocation: tilo_actions::InvocationId,
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
    /// Pick a protocol in the form.
    SetKind(ConnectionKind),
    /// Pick an authentication method in the form.
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
            | Self::Delete { id, .. }
            | Self::TrustSshHostKey { id, .. }
            | Self::ForgetSshHostKey { id, .. } => *id,
        }
    }

    fn target(&self) -> &str {
        match self {
            Self::Connect { target, .. }
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
                    self.fb_image = Some(PenikoImage {
                        data: Blob::new(fb.data),
                        format: ImageFormat::Rgba8,
                        alpha_type: ImageAlphaType::Alpha,
                        width: u32::from(fb.width),
                        height: u32::from(fb.height),
                    });
                    changed = true;
                }
            }
            Some(LiveSession::Tilo(session)) => {
                if let Some(frame) = session.take_latest_frame() {
                    session.size = Some((frame.width, frame.height));
                    self.fb_image = Some(PenikoImage {
                        data: Blob::new(Arc::new(frame.rgba)),
                        format: ImageFormat::Rgba8,
                        alpha_type: ImageAlphaType::Alpha,
                        width: frame.width,
                        height: frame.height,
                    });
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
              seeding); grouping them would express no invariant between them"
)]
pub struct RemoteApp {
    /// The connection store. Source of truth for the list; mirrored from
    /// the shared document.
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
    /// The generation and outcome last recorded in the document.
    recorded: Option<(u64, Presence)>,
    /// Session outcomes waiting to be written to the document.
    pending_records: Vec<crate::actions::RemoteAction>,
    /// The session fills the window, without the rail and the header.
    fullscreen: bool,
    /// Discoverable Tilo peers for the form's device picker.
    tilo_devices: Vec<TiloDevice>,
    next_tilo_session_id: u64,

    /// Framework-owned action dispatch: the single path by which a saved
    /// connection is added, edited, renamed, favourited, or deleted. It
    /// brackets the undo boundary and applies each action through the
    /// executor, so the on-screen controls and the palette share one
    /// replayable path.
    actions: crate::resource::ActionSink,
    pending_resource_operation: Option<PendingResourceOperation>,
    /// Confirmation mirroring resource-action policy. Private view state,
    /// never replicated with the connection profile.
    pending_confirmation: Option<PendingConfirmation>,
    /// The dialog that asks it.
    dialog: Option<ModalDialog<Column>>,
    /// Outcomes worth a moment's notice (a revalidation that cancelled an
    /// action). They contain no credential material.
    toasts: ToastStack,
    toast_clock: Option<ui::time::Instant>,

    /// The window: rail, header row and current screen.
    shell: NavShell,
    /// The session alone, while it fills the window.
    immersive: Option<SessionPane>,
    /// The queue every control reports to.
    requests: Requests,
    /// Search hits picked in the platform search, waiting to be shown.
    opened: Rc<RefCell<Vec<String>>>,
    /// The fingerprint of what the window was last built to show.
    built: Option<u64>,
    rect: Rect,
    layout_dirty: bool,
    dirty: bool,
    /// True once the in-memory workspace has been mirrored into the document.
    seeded: bool,
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
            recorded: None,
            pending_records: Vec::new(),
            fullscreen: false,
            tilo_devices,
            next_tilo_session_id: 1,
            actions: crate::resource::ActionSink::default(),
            pending_resource_operation: None,
            pending_confirmation: None,
            dialog: None,
            toasts: ToastStack::new(),
            toast_clock: None,
            shell: NavShell::new()
                .with_id("remote.shell")
                .mode(NavMode::Docked),
            immersive: None,
            requests: Requests::default(),
            opened: Rc::default(),
            built: None,
            rect: Rect::ZERO,
            layout_dirty: true,
            dirty: true,
            seeded: false,
        }
    }

    /// Where the platform search hands the hits a person picks: the view
    /// registers an opener that pushes each hit's anchor here.
    pub(crate) fn open_requests(&self) -> Rc<RefCell<Vec<String>>> {
        Rc::clone(&self.opened)
    }

    /// Stage the view state of a named screenshot scenario. The connections
    /// themselves come from the scenario's resource actions
    /// ([`crate::resource::RemoteShell::scenario`]).
    pub(crate) fn stage(&mut self, scenario: Option<&str>) {
        let mut scratch = AutoCommit::new();
        self.tilo_devices = seed_tilo_devices();
        match scenario.unwrap_or_default() {
            "pinned" => self.apply(Action::Show(Destination::Pinned), &mut scratch),
            "form" => self.apply(Action::NewConnection, &mut scratch),
            "form-tilo" => {
                self.apply(Action::NewConnection, &mut scratch);
                self.apply(Action::SetKind(ConnectionKind::Tilo), &mut scratch);
                self.apply(Action::PickTiloDevice(2), &mut scratch);
            }
            "form-invalid" => {
                self.apply(Action::NewConnection, &mut scratch);
                self.fields.host.set("build.lab.internal".into());
                self.fields.port.set(String::new());
                self.apply(Action::SaveForm, &mut scratch);
            }
            "edit" => {
                if let Some(id) = self.first_of_kind(ConnectionKind::Ssh) {
                    self.apply(Action::EditConnection(id), &mut scratch);
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
            session.fb_image = Some(PenikoImage {
                data: Blob::new(Arc::new(crate::sample::desktop())),
                format: ImageFormat::Rgba8,
                alpha_type: ImageAlphaType::Alpha,
                width,
                height,
            });
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
    fn apply(&mut self, action: Action, state: &mut AutoCommit) {
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
            }
            Action::EditConnection(id) => {
                if let Some(c) = self.workspace.get(id) {
                    self.draft = ConnectionDraft::from_connection(c);
                    self.problems = FormProblems::default();
                    self.validating = false;
                    self.screen = Screen::Form { editing: Some(id) };
                    self.load_fields();
                }
            }
            Action::RequestConnect(id) => self.request_connect(id),
            Action::ToggleFavorite(id) => self.toggle_favorite(id, state),
            Action::RequestDelete(id) => self.request_delete(id),
            Action::RequestTrustSshHostKey(id) => self.request_trust_ssh_host_key(id),
            Action::RequestForgetSshHostKey(id) => self.request_forget_ssh_host_key(id),
            Action::ConfirmPending => self.confirm_pending(state),
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
            Action::SaveForm => self.save_form(state),
            Action::ToggleScale => {
                let mut session = self.session.borrow_mut();
                session.scale_to_fit = !session.scale_to_fit;
                session.changed = true;
            }
            Action::ToggleFullscreen => self.set_fullscreen(!self.fullscreen),
            Action::Disconnect => self.disconnect_session(),
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
        if fullscreen {
            self.notify("Press Esc to leave full screen.");
        }
        self.layout_dirty = true;
        self.dirty = true;
    }

    /// Run every queued action.
    fn apply_requests(&mut self, state: &mut AutoCommit) {
        loop {
            let queued = std::mem::take(&mut *self.requests.borrow_mut());
            if queued.is_empty() {
                break;
            }
            for action in queued {
                self.apply(action, state);
            }
        }
    }

    /// Show the connection a picked search hit points at.
    fn open_found(&mut self) {
        let anchors = std::mem::take(&mut *self.opened.borrow_mut());
        for anchor in anchors {
            let Some(id) = anchor
                .strip_prefix("remote.connection:")
                .and_then(|id| id.parse::<ConnectionId>().ok())
            else {
                continue;
            };
            if self.workspace.get(id).is_none() {
                continue;
            }
            self.selected = Some(id);
            self.destination = Destination::Connections;
            self.screen = Screen::List;
            self.dirty = true;
        }
    }

    fn toggle_favorite(&mut self, id: ConnectionId, state: &mut AutoCommit) {
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
        self.dispatch(state, &crate::actions::set_favorite_action(id, target));
    }

    fn delete_connection(&mut self, id: ConnectionId, state: &mut AutoCommit) {
        self.workspace.remove(id);
        self.dispatch(state, &crate::actions::delete_action(id));
        if self.selected == Some(id) {
            self.selected = None;
        }
        if matches!(self.screen, Screen::Form { editing: Some(editing) } if editing == id) {
            self.screen = Screen::List;
        }
    }

    fn dispatch(&mut self, state: &mut AutoCommit, action: &crate::actions::RemoteAction) {
        self.actions.dispatch_with_context(
            state,
            crate::actions::REMOTE_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.remote", "Remote", action),
        );
    }

    /// Write the session outcomes recorded since the last event.
    fn flush_records(&mut self, state: &mut AutoCommit) {
        for action in std::mem::take(&mut self.pending_records) {
            self.dispatch(state, &action);
        }
    }

    /// Stable, non-secret identity used to revalidate a confirmation after a
    /// collaborator may have edited the shared profile.
    fn confirmation_target(connection: &Connection) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            connection.kind.tag(),
            connection.host,
            connection.port,
            connection.user,
            connection.auth.label()
        )
    }

    fn notify(&mut self, message: impl Into<String>) {
        self.toasts.push(message.into(), ToastKind::Info);
        self.dirty = true;
    }

    fn request_connect(&mut self, id: ConnectionId) {
        let Some(connection) = self.workspace.get(id) else {
            self.notify("That saved connection no longer exists.");
            return;
        };
        if !connection_readiness(connection).can_connect() {
            self.open_session(id);
            return;
        }
        self.pending_confirmation = Some(PendingConfirmation::Connect {
            id,
            target: Self::confirmation_target(connection),
        });
        self.show_confirmation();
    }

    fn request_delete(&mut self, id: ConnectionId) {
        let Some(connection) = self.workspace.get(id) else {
            self.notify("That saved connection no longer exists.");
            return;
        };
        self.pending_confirmation = Some(PendingConfirmation::Delete {
            id,
            target: Self::confirmation_target(connection),
        });
        self.show_confirmation();
    }

    fn ssh_host_key_issue(&self, id: ConnectionId) -> Option<crate::ssh::HostKeyIssue> {
        match (&self.session.borrow().live, self.session_for) {
            (Some(LiveSession::Ssh(session)), Some(session_id)) if session_id == id => {
                session.host_key_issue()
            }
            _ => None,
        }
    }

    fn request_trust_ssh_host_key(&mut self, id: ConnectionId) {
        let Some(connection) = self.workspace.get(id) else {
            return;
        };
        let target = Self::confirmation_target(connection);
        let Some(crate::ssh::HostKeyIssue::FirstContact { observed }) = self.ssh_host_key_issue(id)
        else {
            self.notify("No new SSH host key is waiting for trust. Forget a changed key before you review it again.");
            return;
        };
        self.pending_confirmation = Some(PendingConfirmation::TrustSshHostKey {
            id,
            target,
            observed,
        });
        self.show_confirmation();
    }

    fn request_forget_ssh_host_key(&mut self, id: ConnectionId) {
        let Some(connection) = self.workspace.get(id) else {
            return;
        };
        let target = Self::confirmation_target(connection);
        if !matches!(
            self.ssh_host_key_issue(id),
            Some(crate::ssh::HostKeyIssue::Changed { .. })
        ) {
            self.notify("Only a changed SSH host key can be forgotten. Reconnect to check this machine again.");
            return;
        }
        self.pending_confirmation = Some(PendingConfirmation::ForgetSshHostKey { id, target });
        self.show_confirmation();
    }

    /// Build the dialog that asks the pending confirmation. It states the
    /// scope in plain language; cancel is the safe default.
    fn show_confirmation(&mut self) {
        let Some(pending) = self.pending_confirmation.clone() else {
            self.dialog = None;
            return;
        };
        let name = self
            .workspace
            .get(pending.id())
            .map_or_else(|| "this connection".to_owned(), |c| c.name.clone());
        let mut key = None;
        let (title, detail, primary, destructive) = match &pending {
            PendingConfirmation::Delete { .. } => (
                format!("Delete {name}?"),
                "The saved connection is removed for everyone you share it with. The machine itself is not changed.".to_owned(),
                "Delete connection",
                true,
            ),
            PendingConfirmation::Connect { .. } => (
                format!("Connect to {name}?"),
                "This opens a live session. Remote checks the saved address and access again before anything is sent.".to_owned(),
                "Connect",
                false,
            ),
            PendingConfirmation::TrustSshHostKey { observed, .. } => {
                key = Some(format!("{} {}", observed.algorithm, observed.fingerprint));
                (
                    format!("Trust the SSH key of {name}?"),
                    "Check this key with the machine's owner before you trust it. Only this device remembers it.".to_owned(),
                    "Trust key",
                    false,
                )
            }
            PendingConfirmation::ForgetSshHostKey { .. } => (
                format!("Forget the SSH key of {name}?"),
                "Only this device's saved key is removed. Reconnect to check and trust the new key.".to_owned(),
                "Forget key",
                true,
            ),
        };
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(Text::new(&detail).with_role(TextRole::Body)));
        if let Some(key) = key {
            body = body.push(Box::new(
                Text::new(&key)
                    .with_role(TextRole::Code)
                    .with_kind(ui::TextKind::Secret),
            ));
        }
        let confirm = Rc::clone(&self.requests);
        let cancel = Rc::clone(&self.requests);
        let dismiss = Rc::clone(&self.requests);
        self.dialog = Some(
            ModalDialog::new(title, body)
                .with_test_id("remote.confirmation")
                .primary_action(primary, move || {
                    confirm.borrow_mut().push(Action::ConfirmPending);
                })
                .secondary_action("Cancel", move || {
                    cancel.borrow_mut().push(Action::CancelPending);
                })
                .on_dismiss(move || dismiss.borrow_mut().push(Action::CancelPending))
                .with_destructive(destructive)
                .open(true),
        );
        self.dirty = true;
    }

    /// Revalidate against the current shared projection immediately before a
    /// network attempt or deletion. A late collaborator edit changes neither
    /// the target nor the document unexpectedly; the user must review it.
    fn confirm_pending(&mut self, state: &mut AutoCommit) {
        let Some(pending) = self.pending_confirmation.take() else {
            return;
        };
        let id = pending.id();
        let Some(connection) = self.workspace.get(id).cloned() else {
            self.notify("The saved connection was removed before you confirmed.");
            return;
        };
        if Self::confirmation_target(&connection) != pending.target() {
            self.notify("This connection changed while you were asked. Review it, then try again.");
            return;
        }
        match pending {
            PendingConfirmation::Connect { .. } => {
                // `open_session` repeats transport/profile readiness at the I/O
                // boundary, so an expired capability cannot race confirmation.
                self.open_session(id);
            }
            PendingConfirmation::Delete { .. } => self.delete_connection(id, state),
            PendingConfirmation::TrustSshHostKey { observed, .. } => {
                let current = self.ssh_host_key_issue(id);
                if !matches!(current, Some(crate::ssh::HostKeyIssue::FirstContact { observed: ref current }) if current == &observed)
                {
                    self.notify("The SSH host key changed before it was trusted. Review it again.");
                    return;
                }
                let path = match crate::ssh::default_known_hosts_path() {
                    Ok(path) => path,
                    Err(error) => {
                        self.notify(format!(
                            "Couldn't open this device's SSH key store: {error}"
                        ));
                        return;
                    }
                };
                match crate::ssh::trust_host_key(
                    &path,
                    &connection.host,
                    connection.port,
                    &observed,
                ) {
                    Ok(()) => self.open_session(id),
                    Err(error) => self.notify(format!("Couldn't save the SSH key: {error}")),
                }
            }
            PendingConfirmation::ForgetSshHostKey { .. } => {
                if !matches!(
                    self.ssh_host_key_issue(id),
                    Some(crate::ssh::HostKeyIssue::Changed { .. })
                ) {
                    self.notify("The SSH host key is no longer the one you chose to forget. Reconnect to review it again.");
                    return;
                }
                let path = match crate::ssh::default_known_hosts_path() {
                    Ok(path) => path,
                    Err(error) => {
                        self.notify(format!(
                            "Couldn't open this device's SSH key store: {error}"
                        ));
                        return;
                    }
                };
                match crate::ssh::forget_host_key(&path, &connection.host, connection.port) {
                    Ok(true) => {
                        self.notify("Forgot the saved SSH key. Reconnect to check the new one before you trust it.");
                        self.disconnect_session();
                    }
                    Ok(false) => self.notify("This device had no saved SSH key for the machine."),
                    Err(error) => self.notify(format!("Couldn't forget the SSH key: {error}")),
                }
            }
        }
    }

    /// Open the session surface for a connection. Tilo always uses its real
    /// shell-broker transport; SSH/VNC require the explicit live-session
    /// capability. A missing capability or RDP backend gets a first-class
    /// recovery surface — no socket is opened, and it is not misreported as a
    /// connection failure.
    fn open_session(&mut self, id: ConnectionId) {
        let Some(c) = self.workspace.get(id).cloned() else {
            return;
        };
        self.selected = Some(id);
        self.destination = Destination::Session;
        let Some(next_generation) = self.session_generation.checked_add(1) else {
            // Never recycle a generation: pending actions use it as a
            // capability to a particular transport attempt. A process that
            // exhausted u64 attempts must restart rather than aliasing one.
            self.session_for = Some(id);
            self.session.borrow_mut().live = Some(LiveSession::Error(
                "Remote has run out of session identities. Restart it to connect again.".into(),
            ));
            self.screen = Screen::Session { id };
            return;
        };
        self.session_generation = next_generation;
        self.session_for = Some(id);
        self.set_fullscreen(false);
        {
            let mut session = self.session.borrow_mut();
            session.fb_image = None;
            session.fb_canvas = None;
            session.vnc_buttons = 0;
            session.live = None;
        }
        let readiness = connection_readiness(&c);
        let live = if readiness.can_connect() {
            self.start_live(&c)
        } else {
            LiveSession::Unavailable {
                readiness,
                kind: c.kind,
            }
        };
        self.session.borrow_mut().live = Some(live);
        self.screen = Screen::Session { id };
    }

    fn disconnect_session(&mut self) {
        // Dropping the live session closes the transport (the worker thread
        // sees its channel close and disconnects).
        {
            let mut session = self.session.borrow_mut();
            if let Some(LiveSession::Tilo(tilo)) = session.live.as_ref() {
                let session_id = tilo.id;
                session
                    .collab_outbox
                    .push(ui::collaboration::CollabRequest::CloseWorkspaceControl { session_id });
            }
            session.live = None;
            session.fb_image = None;
            session.fb_canvas = None;
        }
        self.session_for = None;
        // At the identity boundary opening another transport is rejected, so
        // retaining MAX here cannot allow a stale operation to alias a new
        // session.
        if let Some(next_generation) = self.session_generation.checked_add(1) {
            self.session_generation = next_generation;
        }
        self.set_fullscreen(false);
        self.screen = Screen::List;
        if self.destination == Destination::Session {
            self.destination = Destination::Connections;
        }
    }

    /// Build the real transport for a connection from its kind + auth and
    /// the process environment (secrets are never stored in the document).
    fn start_live(&mut self, c: &Connection) -> LiveSession {
        match c.kind {
            ConnectionKind::Ssh => {
                let known_hosts_path = match crate::ssh::default_known_hosts_path() {
                    Ok(path) => path,
                    Err(error) => {
                        return LiveSession::Error(format!(
                            "SSH host-key trust is unavailable: {error}"
                        ));
                    }
                };
                let prefer_key = matches!(c.auth, AuthMethod::PublicKey);
                let auth = match crate::ssh::resolve_auth(
                    prefer_key,
                    |k| std::env::var(k).ok(),
                    &crate::ssh::default_key_paths(),
                ) {
                    Ok(a) => a,
                    Err(e) => return LiveSession::Error(e.to_string()),
                };
                let user = if c.user.is_empty() {
                    tilo_paths::session::user_name().unwrap_or_else(|| "root".into())
                } else {
                    c.user.clone()
                };
                match SshSession::connect(SshConfig {
                    host: c.host.clone(),
                    port: c.port,
                    user,
                    auth,
                    cols: 80,
                    rows: 24,
                    known_hosts_path,
                }) {
                    Ok(s) => LiveSession::Ssh(Box::new(s)),
                    Err(e) => LiveSession::Error(e.to_string()),
                }
            }
            ConnectionKind::Vnc => match VncSession::connect(VncConfig {
                host: c.host.clone(),
                port: c.port,
                password: std::env::var("REMOTE_VNC_PASSWORD").ok(),
            }) {
                Ok(s) => LiveSession::Vnc(s),
                Err(e) => LiveSession::Error(e.to_string()),
            },
            ConnectionKind::Tilo => {
                let target = match c.host.parse::<EndpointId>() {
                    Ok(target) => *target.as_bytes(),
                    Err(_) => {
                        return LiveSession::Error(
                            "Tilo host must be an exact certified device endpoint id".into(),
                        );
                    }
                };
                let id = self.next_tilo_session_id;
                self.next_tilo_session_id = self.next_tilo_session_id.saturating_add(1);
                self.session.borrow_mut().collab_outbox.push(
                    ui::collaboration::CollabRequest::OpenWorkspaceControl {
                        session_id: id,
                        target_device: target,
                    },
                );
                match TiloSession::new(id) {
                    Ok(session) => LiveSession::Tilo(session),
                    Err(error) => LiveSession::Error(error),
                }
            }
            // `open_session` preflights RDP as `BackendUnavailable` before
            // reaching this constructor. Keep the defensive branch explicit
            // anyway, so a future caller cannot turn a persisted legacy RDP
            // profile into a generic, misleading transport failure.
            ConnectionKind::Rdp => LiveSession::Unavailable {
                readiness: TransportReadiness::BackendUnavailable,
                kind: ConnectionKind::Rdp,
            },
        }
    }

    /// Drain live-session I/O and note a session's outcome once: a
    /// connection that came up marks the machine reachable, one that failed
    /// marks it unreachable.
    fn pump_session(&mut self) {
        let (changed, status) = {
            let mut session = self.session.borrow_mut();
            let changed = session.pump() || std::mem::take(&mut session.changed);
            (changed, session.status())
        };
        if changed {
            self.dirty = true;
        }
        let Some(id) = self.session_for else {
            return;
        };
        let presence = match status {
            Some(SessionStatus::Connected) => Presence::Online,
            Some(SessionStatus::Failed(_)) => Presence::Offline,
            _ => return,
        };
        // A preview has no machine behind it to observe.
        if matches!(
            self.session.borrow().live,
            Some(LiveSession::Preview(_) | LiveSession::Sample(_))
        ) {
            return;
        }
        if self.recorded == Some((self.session_generation, presence)) {
            return;
        }
        self.recorded = Some((self.session_generation, presence));
        let at = ui::clock::local_now().timestamp();
        if let Some(connection) = self.workspace.get_mut(id) {
            connection.presence = presence;
            connection.last_used = Some(at);
        }
        self.pending_records
            .push(crate::actions::record_session_action(id, at, presence));
        self.dirty = true;
    }

    /// Commit the form's draft into the workspace and the document.
    fn save_form(&mut self, state: &mut AutoCommit) {
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
        // Preserve favorite + presence + last_used across an edit.
        if let Some(existing) = self.workspace.get(id) {
            conn.favorite = existing.favorite;
            conn.presence = existing.presence;
            conn.last_used = existing.last_used;
        }
        // One focused action for the edited fields — not a whole-workspace
        // re-mirror. favourite/presence/last_used are preserved by the action's
        // deterministic read of the current document (see [`crate::actions`]).
        let action = crate::actions::upsert_action(&conn);
        self.workspace.upsert(conn);
        self.dispatch(state, &action);
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

    /// Show the saved connections the framework resource already holds.
    pub(crate) fn adopt_resource_state(&mut self, state: &[crate::model::Connection]) {
        if self.workspace.connections() == state {
            return;
        }
        self.workspace.connections = state.to_vec();
        self.forget_vanished();
        self.seeded = true;
        self.dirty = true;
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

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    ///
    /// [`ResourceSurface`]: ui::framework::ResourceSurface
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Remote>,
    ) {
        self.actions.attach(dispatcher);
    }

    /// The last action an on-screen control dispatched.
    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    // -- the window ---------------------------------------------------------

    /// One list row per connection the destination shows. The meta says
    /// when the machine was last used, or what about the saved connection
    /// stops it from opening; what this device lacks is said when a session
    /// is opened, where it can be acted on.
    fn rows(&self) -> Vec<RowModel> {
        let now = ui::clock::local_now().timestamp();
        self.ordered()
            .into_iter()
            .filter(|c| self.destination != Destination::Pinned || c.favorite)
            .map(|c| {
                let meta = match connection_readiness(c) {
                    readiness @ (TransportReadiness::BackendUnavailable
                    | TransportReadiness::InvalidProfile) => readiness.label().to_owned(),
                    TransportReadiness::Ready | TransportReadiness::NeedsLiveSessionCapability => {
                        c.last_used.map_or_else(
                            || "Never connected".to_owned(),
                            |at| ui::format::capitalize_first(&ui::format::relative(now - at)),
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
                    presence: c.presence,
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

    /// Put the current screen, its header and the rail into the shell.
    fn rebuild_views(&mut self, theme: &Theme) {
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
        let (title, actions, content): (String, Vec<Element>, Element) = match self.content() {
            Content::List {
                destination,
                rows,
                has_connections,
            } => (
                match destination {
                    Destination::Pinned => "Pinned connections".to_owned(),
                    _ => "Connections".to_owned(),
                },
                view::list_actions(&self.requests, has_connections),
                view::connection_list(theme, &self.requests, destination, &rows, has_connections),
            ),
            Content::Form(model) => (
                String::new(),
                view::form_actions(&self.requests),
                view::connection_form(&self.requests, &self.fields, &model),
            ),
            Content::Surface(id) => {
                let connection = self.workspace.get(id);
                let framebuffer = connection.is_some_and(|c| c.kind.streams_framebuffer());
                (
                    connection.map(|c| c.name.clone()).unwrap_or_default(),
                    view::session_actions(
                        &self.requests,
                        framebuffer,
                        self.session.borrow().scale_to_fit,
                    ),
                    Box::new(SessionPane::new(Rc::clone(&self.session))),
                )
            }
            Content::Status { id, .. } => {
                let name = self
                    .workspace
                    .get(id)
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                let actions = if self.session.borrow().live_active() {
                    view::session_actions(&self.requests, false, true)
                } else {
                    Vec::new()
                };
                (
                    name,
                    actions,
                    view::session_status(&self.requests, self.status_model(id)),
                )
            }
            Content::Missing => (
                String::new(),
                Vec::new(),
                Box::new(
                    ui::widget::EmptyState::new("This connection was deleted")
                        .with_icon(Icon::Monitor)
                        .with_message("Someone you share your connections with removed it."),
                ),
            ),
        };
        let mut shell = std::mem::take(&mut self.shell)
            .with_id("remote.shell")
            .mode(NavMode::Docked)
            .content_header(title, actions);
        shell.set_sidebar(Some(Box::new(rail)));
        shell.set_content(content);
        self.shell = shell;
    }

    /// What a session that shows no remote surface says, and the one way
    /// forward it offers.
    fn status_model(&self, id: ConnectionId) -> StatusModel {
        let connection = self.workspace.get(id);
        let name = connection.map_or("the machine", |c| c.name.as_str());
        let kind = connection.map_or(ConnectionKind::Ssh, |c| c.kind);
        let icon = if kind.is_terminal() {
            Icon::Terminal
        } else {
            Icon::Monitor
        };
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
                        action: None,
                    }
                } else {
                    StatusModel {
                        icon,
                        title: format!("Connecting to {name}"),
                        message: "Remote is reaching the machine and signing in.".to_owned(),
                        error: false,
                        action: None,
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

    /// Advance the toasts' timers.
    fn tick_toasts(&mut self) {
        let now = ui::time::now();
        if let Some(last) = self.toast_clock {
            self.toasts.tick(now.saturating_duration_since(last));
        }
        self.toast_clock = (!self.toasts.is_empty()).then_some(now);
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

/// The single child node of the app's node.
fn child_node(taffy: &TaffyTree<()>, node: NodeId) -> Option<NodeId> {
    taffy.children(node).ok()?.first().copied()
}

/// A parent node that fills the window around one child.
fn fill_node(taffy: &mut TaffyTree<()>, child: NodeId) -> NodeId {
    taffy.parent_node(
        Style {
            display: taffy::Display::Flex,
            flex_grow: 1.0,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        },
        &[child],
    )
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
        match event {
            runtime_collab::WorkspaceControlEvent::WaitingForConsent { .. } => {
                session.status = SessionStatus::Connecting;
            }
            runtime_collab::WorkspaceControlEvent::StreamStarted { width, height, .. } => {
                session.size = Some((*width, *height));
                session.status = SessionStatus::Connected;
            }
            runtime_collab::WorkspaceControlEvent::Frame { encoded_h264, .. } => {
                if let Err(error) = session.queue_frame(encoded_h264.clone()) {
                    session.status = SessionStatus::Failed(error);
                }
            }
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
        core.changed = true;
        drop(core);
        self.dirty = true;
        self.check_views();
    }
}

impl Widget for RemoteApp {
    ui::provides!(
        dyn ui::widget::Collaborative,
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources,
        dyn ui::widget::controls::ActionControls
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::REMOTE_OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let started = if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            Some(started)
        } else {
            self.answer_resource_operation(
                request.kind,
                request.type_id,
                request.payload,
                doc,
                path,
            )?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            })
        };
        self.check_views();
        Ok(started)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "each pending operation's exact terminal settlement is kept beside cancellation and generation validation"
    )]
    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let pending = self.pending_resource_operation.as_ref()?;
        if cancellation_requested
            && let PendingResourceOperation::Connect {
                invocation: pending_invocation,
                connection_id,
                session_generation,
            } = pending
            && *pending_invocation == invocation
        {
            // Connect has not completed yet, so dropping its exact generation
            // cancels the socket/relay attempt. Do not let a cancellation for
            // an old invocation tear down a newer reconnect of the same saved
            // profile.
            if self.session_for == Some(*connection_id)
                && self.session_generation == *session_generation
            {
                self.disconnect_session();
                self.check_views();
            }
            self.pending_resource_operation = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("remote connection attempt cancelled".into()),
            });
        }
        let update = match pending {
            PendingResourceOperation::Connect {
                invocation: pending,
                connection_id,
                session_generation,
            } if *pending == invocation => {
                if self.session_for != Some(*connection_id)
                    || self.session_generation != *session_generation
                {
                    tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "remote.session_replaced".into(),
                            message: "the connection attempt was superseded by a newer session"
                                .into(),
                            details: None,
                            retryable: false,
                        },
                    }
                } else {
                    match self.session.borrow().status() {
                        Some(SessionStatus::Connecting) => return None,
                        Some(SessionStatus::Connected) => {
                            tilo_actions::InvocationUpdate::Succeeded {
                                invocation,
                                output: tilo_actions::ActionOutput::default(),
                            }
                        }
                        Some(SessionStatus::Failed(message)) => {
                            tilo_actions::InvocationUpdate::Failed {
                                invocation,
                                error: tilo_actions::ActionFailure {
                                    code: "remote.connect_failed".into(),
                                    message,
                                    details: None,
                                    retryable: false,
                                },
                            }
                        }
                        Some(SessionStatus::Unavailable(message)) => {
                            tilo_actions::InvocationUpdate::Failed {
                                invocation,
                                error: tilo_actions::ActionFailure {
                                    code: "remote.transport_unavailable".into(),
                                    message,
                                    details: None,
                                    retryable: false,
                                },
                            }
                        }
                        Some(SessionStatus::Closed) | None => {
                            tilo_actions::InvocationUpdate::Failed {
                                invocation,
                                error: tilo_actions::ActionFailure {
                                    code: "remote.connection_closed".into(),
                                    message: "remote connection closed before it became ready"
                                        .into(),
                                    details: None,
                                    retryable: false,
                                },
                            }
                        }
                    }
                }
            }
            PendingResourceOperation::SshInput {
                invocation: pending,
                connection_id,
                session_generation,
                acknowledgement,
            } if *pending == invocation => {
                if self.session_for != Some(*connection_id)
                    || self.session_generation != *session_generation
                {
                    tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "remote.session_replaced".into(),
                            message: "the SSH session ended before input delivery was acknowledged"
                                .into(),
                            details: None,
                            retryable: false,
                        },
                    }
                } else {
                    match acknowledgement.try_recv() {
                        Ok(Ok(())) => tilo_actions::InvocationUpdate::Succeeded {
                            invocation,
                            output: tilo_actions::ActionOutput::default(),
                        },
                        Ok(Err(message)) => tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "remote.input_failed".into(),
                                message,
                                details: None,
                                retryable: false,
                            },
                        },
                        Err(std::sync::mpsc::TryRecvError::Empty) => return None,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            tilo_actions::InvocationUpdate::Failed {
                                invocation,
                                error: tilo_actions::ActionFailure {
                                    code: "remote.input_worker_lost".into(),
                                    message: "remote input worker stopped before acknowledgement"
                                        .into(),
                                    details: None,
                                    retryable: false,
                                },
                            }
                        }
                    }
                }
            }
            _ => return None,
        };
        self.pending_resource_operation = None;
        Some(update)
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let (doc, _) = ui::doc::document(state, path);
            let doc_conns = RemoteDoc::list(doc);
            if self.seeded {
                // The document is authoritative once seeded. Adopt the complete
                // merged projection, not merely additions: collaborator edits,
                // favourite changes, and deletions must all become visible.
                if self.workspace.connections() != doc_conns.as_slice() {
                    self.workspace.connections = doc_conns;
                    self.forget_vanished();
                    self.dirty = true;
                }
            } else {
                // A live window starts empty and adopts whatever the document
                // holds; nothing fabricated is ever written into it.
                self.workspace.connections = doc_conns;
                self.forget_vanished();
                self.seeded = true;
                self.dirty = true;
            }
            self.flush_records(doc);
            self.open_found();
            self.apply_requests(doc);
        }
        self.revalidate();
        self.check_views();
        self.body_mut().sync(state, path);
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        self.body_mut().update(state, path)
    }

    fn needs_idle_sync(&self) -> bool {
        !self.seeded || !self.pending_records.is_empty() || !self.opened.borrow().is_empty()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.rebuild_views(theme);
        self.layout_dirty = false;
        let child = self.body_mut().build_layout(taffy, theme, node_map);
        let node = fill_node(taffy, child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
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

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.pump_session();
        self.check_views();
        self.body_mut().draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
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
        self.tick_toasts();
        if !self.toasts.is_empty() {
            self.toasts
                .place_bottom_right(self.rect, f64::from(theme.tokens.spacing.s4));
            self.toasts
                .draw(scene, transform, text_renderer, theme, presence);
        }
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // A confirmation is modal: nothing reaches the window behind it.
        if let Some(dialog) = self.dialog.as_mut() {
            let (doc, path) = ui::doc::document(ctx.state, ctx.path);
            dialog.handle_detached(event, ctx.scale_factor, *ctx.modifiers, doc, path);
            *ctx.handled = true;
            self.apply_requests(doc);
            self.dirty = true;
            return;
        }
        if let Some(action) = self.handle_shortcut(event, ctx.modifiers.state()) {
            let (doc, _) = ui::doc::document(ctx.state, ctx.path);
            self.apply(action, doc);
            *ctx.handled = true;
            return;
        }
        if !self.toasts.is_empty() {
            self.toasts.handle_event(event, ctx);
        }
        if !*ctx.handled
            && let Some(child) = child_node(ctx.taffy, ctx.node_id)
        {
            let path = ctx.path;
            self.body_mut()
                .handle_event(event, &mut ctx.reborrow(path, child));
        }
        let (doc, _) = ui::doc::document(ctx.state, ctx.path);
        if !*ctx.handled
            && let Some(action) = Self::handle_unclaimed(event, &self.screen)
        {
            self.apply(action, doc);
            *ctx.handled = true;
        }
        self.flush_records(doc);
        self.open_found();
        self.apply_requests(doc);
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
        self.session.borrow().live_active()
            || !self.toasts.is_empty()
            || self.body().needs_animation_frame()
    }
}

impl ui::resources::PublishesResources for RemoteApp {
    /// Publish each saved connection as a resource (open, connect, rename,
    /// favourite, delete) so the shell's action palette and agents reach
    /// them.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        // The framework root publishes saved connections. Keep only the live
        // session resource when hosted — it is not in Resource::children.
        if self.actions.is_hosted() {
            return self.live_session_resources();
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.actions.is_hosted() && id == "remote.connections" {
            return None;
        }
        find_resource(&self.publish_resources(doc), id).or_else(|| {
            self.actions.is_hosted().then(|| {
                self.live_session_resources()
                    .into_iter()
                    .find(|resource| resource.id.as_str() == id)
            })?
        })
    }
}

impl ui::undo::CommittedActions for RemoteApp {
    /// Hand the host the heads captured before an on-screen control
    /// dispatched its Remote action, so it records one undo boundary
    /// bracketing it. Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl ui::widget::controls::ActionControls for RemoteApp {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        ui::widget::controls::take_control_invocation(self.body_mut())
    }
}

impl RemoteApp {
    /// Live session affordances are not in [`crate::resource::Remote`]'s
    /// children — publish them beside the framework tree when hosted.
    pub(crate) fn live_session_resources(&self) -> Vec<tilo_actions::Resource> {
        let Some(id) = self.session_for else {
            return Vec::new();
        };
        let Some(connection) = self.workspace.get(id) else {
            return Vec::new();
        };
        let can_send_text = matches!(
            &self.session.borrow().live,
            Some(LiveSession::Ssh(session))
                if session.status() == SessionStatus::Connected
        );
        let host_key_issue = self.ssh_host_key_issue(id);
        vec![crate::actions::session_resource(
            connection,
            can_send_text,
            host_key_issue.as_ref(),
        )]
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per published Remote operation keeps the effect boundary auditable"
    )]
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        if kind != crate::actions::operation_kind(&operation) {
            return Err("remote operation kind does not match its declared contract".into());
        }
        match operation {
            crate::actions::RemoteOperation::ShowConnections => {
                self.apply(
                    Action::Show(Destination::Connections),
                    &mut AutoCommit::new(),
                );
            }
            crate::actions::RemoteOperation::NewConnectionForm => {
                self.apply(Action::NewConnection, &mut AutoCommit::new());
            }
            crate::actions::RemoteOperation::EditConnection { connection_id } => {
                if self.workspace.get(connection_id).is_none() {
                    return Err("saved connection no longer exists".into());
                }
                self.apply(
                    Action::EditConnection(connection_id),
                    &mut AutoCommit::new(),
                );
            }
            crate::actions::RemoteOperation::Connect { connection_id } => {
                if self.workspace.get(connection_id).is_none() {
                    return Err("saved connection no longer exists".to_string());
                }
                self.open_session(connection_id);
            }
            crate::actions::RemoteOperation::Disconnect { connection_id } => {
                if self.session_for != Some(connection_id) {
                    return Err("that remote session is no longer active".to_string());
                }
                self.disconnect_session();
            }
            crate::actions::RemoteOperation::SendText {
                connection_id,
                text,
            } => {
                if self.session_for != Some(connection_id) {
                    return Err("that remote session is no longer active".to_string());
                }
                match &self.session.borrow().live {
                    Some(LiveSession::Ssh(session))
                        if session.status() == SessionStatus::Connected =>
                    {
                        session.send_input(text.into_bytes())?;
                    }
                    _ => return Err("text input requires a connected SSH terminal".to_string()),
                }
                self.dirty = true;
            }
            crate::actions::RemoteOperation::TrustSshHostKey {
                connection_id,
                observed,
            } => {
                let connection = self
                    .workspace
                    .get(connection_id)
                    .ok_or_else(|| "saved connection no longer exists".to_string())?
                    .clone();
                if !matches!(self.ssh_host_key_issue(connection_id), Some(crate::ssh::HostKeyIssue::FirstContact { observed: ref current }) if current == &observed)
                {
                    return Err(
                        "the observed SSH host key is stale; reconnect to inspect the current key"
                            .into(),
                    );
                }
                let path = crate::ssh::default_known_hosts_path().map_err(|error| {
                    format!("private SSH host-key storage is unavailable: {error}")
                })?;
                crate::ssh::trust_host_key(&path, &connection.host, connection.port, &observed)
                    .map_err(|error| format!("could not save SSH host-key trust: {error}"))?;
                self.open_session(connection_id);
            }
            crate::actions::RemoteOperation::ForgetSshHostKey { connection_id } => {
                let connection = self
                    .workspace
                    .get(connection_id)
                    .ok_or_else(|| "saved connection no longer exists".to_string())?
                    .clone();
                if !matches!(
                    self.ssh_host_key_issue(connection_id),
                    Some(crate::ssh::HostKeyIssue::Changed { .. })
                ) {
                    return Err(
                        "only the currently observed changed SSH host key can be forgotten".into(),
                    );
                }
                let path = crate::ssh::default_known_hosts_path().map_err(|error| {
                    format!("private SSH host-key storage is unavailable: {error}")
                })?;
                crate::ssh::forget_host_key(&path, &connection.host, connection.port)
                    .map_err(|error| format!("could not forget SSH host-key trust: {error}"))?;
                self.disconnect_session();
            }
        }
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != tilo_actions::OperationKind::Effect {
            return Ok(None);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        if self.pending_resource_operation.is_some() {
            return Err("another remote operation is still running".into());
        }
        match operation {
            crate::actions::RemoteOperation::ShowConnections
            | crate::actions::RemoteOperation::NewConnectionForm
            | crate::actions::RemoteOperation::EditConnection { .. } => {
                Err("view operations do not start background work".into())
            }
            crate::actions::RemoteOperation::Connect { connection_id } => {
                let Some(connection) = self.workspace.get(connection_id) else {
                    return Err("saved connection no longer exists".into());
                };
                let readiness = connection_readiness(connection);
                if !readiness.can_connect() {
                    let reason = if readiness == TransportReadiness::InvalidProfile {
                        connection
                            .profile_issue()
                            .unwrap_or_else(|| readiness.detail(connection.kind))
                    } else {
                        readiness.detail(connection.kind)
                    };
                    return Err(reason.to_string());
                }
                self.open_session(connection_id);
                let status = self.session.borrow().status();
                match status {
                    Some(SessionStatus::Connecting) => {
                        self.pending_resource_operation = Some(PendingResourceOperation::Connect {
                            invocation,
                            connection_id,
                            session_generation: self.session_generation,
                        });
                        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
                    }
                    Some(SessionStatus::Connected) => {
                        Ok(Some(ui::widget::ResourceOperationStart::Completed(
                            tilo_actions::ActionOutput::default(),
                        )))
                    }
                    Some(SessionStatus::Failed(message) | SessionStatus::Unavailable(message)) => {
                        Err(message)
                    }
                    Some(SessionStatus::Closed) | None => {
                        Err("remote transport did not start".into())
                    }
                }
            }
            crate::actions::RemoteOperation::SendText {
                connection_id,
                text,
            } => {
                if self.session_for != Some(connection_id) {
                    return Err("that remote session is no longer active".into());
                }
                let acknowledgement = match &self.session.borrow().live {
                    Some(LiveSession::Ssh(session))
                        if session.status() == SessionStatus::Connected =>
                    {
                        session.send_input_ack(text.into_bytes())?
                    }
                    _ => return Err("text input requires a connected SSH terminal".into()),
                };
                self.pending_resource_operation = Some(PendingResourceOperation::SshInput {
                    invocation,
                    connection_id,
                    session_generation: self.session_generation,
                    acknowledgement,
                });
                Ok(Some(ui::widget::ResourceOperationStart::Accepted))
            }
            crate::actions::RemoteOperation::Disconnect { .. }
            | crate::actions::RemoteOperation::TrustSshHostKey { .. }
            | crate::actions::RemoteOperation::ForgetSshHostKey { .. } => {
                if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
                    return Ok(None);
                }
                Ok(Some(ui::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                )))
            }
        }
    }
}

fn find_resource(resources: &[tilo_actions::Resource], id: &str) -> Option<tilo_actions::Resource> {
    for resource in resources {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        if let Some(found) = find_resource(&resource.children, id) {
            return Some(found);
        }
    }
    None
}

// ===========================================================================
// The session surface
// ===========================================================================

/// A connected session: the remote terminal grid or desktop, painted from
/// the transport's own state, with the pointer and keyboard forwarded to the
/// remote machine while the pane has focus.
pub(crate) struct SessionPane {
    session: SharedSession,
    rect: Rect,
    focused: bool,
}

impl SessionPane {
    fn new(session: SharedSession) -> Self {
        Self {
            session,
            rect: Rect::ZERO,
            focused: true,
        }
    }

    /// Paint the remote framebuffer, letterboxed to fit or at its own size.
    fn draw_framebuffer(
        core: &mut SessionCore,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        surface: Rect,
    ) {
        let media = &theme.tokens.colors.media;
        scene.fill(Fill::NonZero, transform, media.letterbox, None, &surface);
        let Some(img) = core.fb_image.clone() else {
            // Connected, but no frame decoded yet.
            let style = theme.text(TextRole::Body);
            let label = "Waiting for the first screen update";
            let (w, h) = text_renderer.measure_style(label, None, &style);
            text_renderer.draw_style(
                scene,
                label,
                media.ink_dim,
                transform
                    * Affine::translate((
                        surface.center().x - f64::from(w) / 2.0,
                        surface.center().y - f64::from(h) / 2.0,
                    )),
                None,
                &style,
            );
            return;
        };
        let iw = f64::from(img.width.max(1));
        let ih = f64::from(img.height.max(1));
        let (w, h) = if core.scale_to_fit {
            let scale = (surface.width() / iw).min(surface.height() / ih);
            (iw * scale, ih * scale)
        } else {
            (iw.min(surface.width()), ih.min(surface.height()))
        };
        let canvas = Rect::from_center_size(surface.center(), (w, h));
        core.fb_canvas = Some(canvas);
        let sx = canvas.width() / iw;
        let sy = canvas.height() / ih;
        let brush_transform =
            Affine::translate((canvas.x0, canvas.y0)) * Affine::scale_non_uniform(sx, sy);
        scene.fill(
            Fill::NonZero,
            transform,
            &ImageBrush::new(img),
            Some(brush_transform),
            &canvas,
        );
    }

    /// The terminal grid the session shows, if it is a terminal.
    fn terminal_grid(core: &SessionCore) -> Option<&crate::vt::VtGrid> {
        match &core.live {
            Some(LiveSession::Ssh(s)) => Some(s.grid()),
            Some(LiveSession::Sample(SampleSurface::Terminal(grid))) => Some(grid),
            _ => None,
        }
    }

    /// Paint the SSH terminal grid, straight from the remote PTY output held
    /// in the [`crate::vt::VtGrid`], on the window's content ground in the
    /// theme's terminal palette.
    fn draw_terminal(
        grid: &crate::vt::VtGrid,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        surface: Rect,
    ) {
        let palette = &theme.tokens.terminal_palette;
        let (lines, cursor) = (grid.lines(), grid.cursor());
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &surface,
        );
        let regular = theme.text(TextRole::Code);
        let bold = regular.clone().strong();
        let (cell_w, line_h) = text_renderer.measure_style("M", None, &regular);
        let (cell_w, line_h) = (f64::from(cell_w), f64::from(line_h));
        let pad = f64::from(theme.tokens.spacing.s4);
        for (row, line) in lines.iter().enumerate() {
            let y = surface.y0 + pad + row as f64 * line_h;
            // Draw cells run by run (consecutive cells of one style).
            let mut x = surface.x0 + pad;
            let mut i = 0;
            while let Some(cell) = line.cells.get(i) {
                let style = (cell.color, cell.bold);
                let start = i;
                while line
                    .cells
                    .get(i)
                    .is_some_and(|c| (c.color, c.bold) == style)
                {
                    i += 1;
                }
                let run: String = line
                    .cells
                    .get(start..i)
                    .unwrap_or_default()
                    .iter()
                    .map(|c| c.ch)
                    .collect();
                let color = style.0.map_or(palette.fg_default, |idx| {
                    palette
                        .ansi
                        .get(idx.min(15))
                        .copied()
                        .unwrap_or(palette.fg_default)
                });
                text_renderer.draw_style(
                    scene,
                    &run,
                    color,
                    transform * Affine::translate((x, y)),
                    None,
                    if style.1 { &bold } else { &regular },
                );
                x += run.chars().count() as f64 * cell_w;
            }
        }
        // Block caret at the remote cursor, in the person's own colour.
        let (cx, cy) = cursor;
        let caret = Rect::from_origin_size(
            (
                surface.x0 + pad + cx as f64 * cell_w,
                surface.y0 + pad + cy as f64 * line_h,
            ),
            (cell_w, line_h),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.intent.selection_tint,
            None,
            &caret,
        );
        scene.pop_layer();
    }

    fn handle_pointer(&self, event: &WindowEvent, scale_factor: f64) -> bool {
        let mut core = self.session.borrow_mut();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                if p == core.pointer {
                    return false;
                }
                core.pointer = p;
                core.forward_vnc_pointer(p);
                if let Some((x, y)) = core.tilo_pointer_target(p) {
                    core.queue_tilo_input(runtime_collab::WorkspaceControlInput::PointerMotion {
                        x,
                        y,
                    });
                }
                false
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let p = core.pointer;
                if core.tilo_pointer_target(p).is_some() {
                    let evdev_button = match button {
                        MouseButton::Left => Some(272),
                        MouseButton::Right => Some(273),
                        MouseButton::Middle => Some(274),
                        _ => None,
                    };
                    if let Some(evdev_button) = evdev_button {
                        if *state == ElementState::Pressed && evdev_button == 272 {
                            core.queue_tilo_input(
                                runtime_collab::WorkspaceControlInput::KeyboardFocus {
                                    focused: true,
                                },
                            );
                        }
                        core.queue_tilo_input(
                            runtime_collab::WorkspaceControlInput::PointerButton {
                                button: evdev_button,
                                pressed: *state == ElementState::Pressed,
                            },
                        );
                    }
                    return true;
                }
                if core.vnc_pointer_target(p).is_some() {
                    let bit = match button {
                        MouseButton::Left => 1,
                        MouseButton::Middle => 2,
                        MouseButton::Right => 4,
                        _ => 0,
                    };
                    match state {
                        ElementState::Pressed => core.vnc_buttons |= bit,
                        ElementState::Released => core.vnc_buttons &= !bit,
                    }
                    core.forward_vnc_pointer(p);
                    return true;
                }
                self.rect.contains(p)
            }
            _ => false,
        }
    }

    fn handle_key(&self, event: &KeyEvent, mods: ModifiersState) -> bool {
        let mut core = self.session.borrow_mut();
        let tilo_connected = matches!(
            core.live.as_ref(),
            Some(LiveSession::Tilo(session)) if session.status == SessionStatus::Connected
        );
        if tilo_connected {
            if let PhysicalKey::Code(code) = event.physical_key
                && let Some(keycode) = tilo_keycode(code)
            {
                core.queue_tilo_input(runtime_collab::WorkspaceControlInput::Key {
                    keycode,
                    pressed: event.state == ElementState::Pressed,
                    modifiers: 0,
                });
                return true;
            }
            return false;
        }
        if event.state != ElementState::Pressed {
            return false;
        }
        let text = event.text.as_ref().map(winit::keyboard::SmolStr::as_str);
        match &core.live {
            Some(LiveSession::Ssh(s)) if s.status() == SessionStatus::Connected => {
                if let Some(bytes) = ssh_key_bytes(&event.logical_key, text, mods) {
                    // Interactive key events are best-effort when the
                    // bounded queue is full; the resource action path
                    // reports that condition exactly to agents.
                    let _ = s.send_input(bytes);
                    core.changed = true;
                    return true;
                }
                false
            }
            Some(LiveSession::Vnc(s)) if s.status() == SessionStatus::Connected => {
                if let Some(sym) = vnc_keysym(&event.logical_key, text) {
                    let _ = s.send_input(VncInput::Key {
                        down: true,
                        key: sym,
                    });
                    let _ = s.send_input(VncInput::Key {
                        down: false,
                        key: sym,
                    });
                    core.changed = true;
                    return true;
                }
                false
            }
            _ => false,
        }
    }
}

impl Widget for SessionPane {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            flex_grow: 1.0,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let mut core = self.session.borrow_mut();
        if let Some(grid) = Self::terminal_grid(&core) {
            Self::draw_terminal(grid, scene, transform, text_renderer, theme, self.rect);
        } else {
            Self::draw_framebuffer(&mut core, scene, transform, text_renderer, theme, self.rect);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let handled = match event {
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. } => {
                if matches!(
                    event,
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        ..
                    }
                ) {
                    self.focused = self.rect.contains(self.session.borrow().pointer);
                }
                self.handle_pointer(event, ctx.scale_factor)
            }
            WindowEvent::KeyboardInput { event, .. } if self.focused => {
                self.handle_key(event, ctx.modifiers.state())
            }
            _ => false,
        };
        if handled {
            *ctx.handled = true;
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn is_default_keyboard_focus(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn wants_tab_input(&self) -> bool {
        true
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let core = self.session.borrow();
        let name = if Self::terminal_grid(&core).is_some() {
            "Remote terminal"
        } else {
            "Remote desktop"
        };
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Group)
                .with_name(name)
                .with_bounds(self.rect)
                .with_state(
                    ui::a11y::AccessState::new()
                        .focusable(true)
                        .focused(self.focused),
                ),
        )
    }

    fn needs_redraw(&self) -> bool {
        self.session.borrow().changed
    }

    fn name(&self) -> &'static str {
        "SessionPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn tilo_keycode(code: KeyCode) -> Option<u32> {
    Some(match code {
        KeyCode::KeyA => 30,
        KeyCode::KeyB => 48,
        KeyCode::KeyC => 46,
        KeyCode::KeyD => 32,
        KeyCode::KeyE => 18,
        KeyCode::KeyF => 33,
        KeyCode::KeyG => 34,
        KeyCode::KeyH => 35,
        KeyCode::KeyI => 23,
        KeyCode::KeyJ => 36,
        KeyCode::KeyK => 37,
        KeyCode::KeyL => 38,
        KeyCode::KeyM => 50,
        KeyCode::KeyN => 49,
        KeyCode::KeyO => 24,
        KeyCode::KeyP => 25,
        KeyCode::KeyQ => 16,
        KeyCode::KeyR => 19,
        KeyCode::KeyS => 31,
        KeyCode::KeyT => 20,
        KeyCode::KeyU => 22,
        KeyCode::KeyV => 47,
        KeyCode::KeyW => 17,
        KeyCode::KeyX => 45,
        KeyCode::KeyY => 21,
        KeyCode::KeyZ => 44,
        KeyCode::Digit1 => 2,
        KeyCode::Digit2 => 3,
        KeyCode::Digit3 => 4,
        KeyCode::Digit4 => 5,
        KeyCode::Digit5 => 6,
        KeyCode::Digit6 => 7,
        KeyCode::Digit7 => 8,
        KeyCode::Digit8 => 9,
        KeyCode::Digit9 => 10,
        KeyCode::Digit0 => 11,
        KeyCode::Enter => 28,
        KeyCode::Escape => 1,
        KeyCode::Backspace => 14,
        KeyCode::Tab => 15,
        KeyCode::Space => 57,
        KeyCode::ArrowUp => 103,
        KeyCode::ArrowDown => 108,
        KeyCode::ArrowLeft => 105,
        KeyCode::ArrowRight => 106,
        KeyCode::ShiftLeft => 42,
        KeyCode::ShiftRight => 54,
        KeyCode::ControlLeft => 29,
        KeyCode::ControlRight => 97,
        KeyCode::AltLeft => 56,
        KeyCode::AltRight => 100,
        _ => return None,
    })
}

/// Translate a key press into the bytes an interactive PTY expects.
fn ssh_key_bytes(key: &Key, text: Option<&str>, mods: ModifiersState) -> Option<Vec<u8>> {
    match key {
        Key::Named(NamedKey::Enter) => Some(vec![b'\r']),
        Key::Named(NamedKey::Backspace) => Some(vec![0x7f]),
        Key::Named(NamedKey::Tab) => Some(vec![b'\t']),
        Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
        Key::Named(NamedKey::Space) => Some(vec![b' ']),
        Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B".to_vec()),
        Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C".to_vec()),
        Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D".to_vec()),
        Key::Named(NamedKey::Home) => Some(b"\x1b[H".to_vec()),
        Key::Named(NamedKey::End) => Some(b"\x1b[F".to_vec()),
        Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
        Key::Character(s) => {
            if mods.control_key() {
                // Ctrl+letter → the corresponding control byte (^A=1…^Z=26).
                let c = s.chars().next()?;
                let up = c.to_ascii_uppercase();
                if up.is_ascii_alphabetic() {
                    return Some(vec![(up as u8) - 0x40]);
                }
            }
            text.map(|t| t.as_bytes().to_vec())
                .or_else(|| Some(s.as_bytes().to_vec()))
        }
        _ => text.map(|t| t.as_bytes().to_vec()),
    }
}

/// Map a key press to an X11 keysym for VNC `KeyEvent`s (ASCII + a few
/// named keys — enough for basic remote typing).
fn vnc_keysym(key: &Key, text: Option<&str>) -> Option<u32> {
    match key {
        Key::Named(NamedKey::Enter) => Some(0xff0d),
        Key::Named(NamedKey::Backspace) => Some(0xff08),
        Key::Named(NamedKey::Tab) => Some(0xff09),
        Key::Named(NamedKey::Escape) => Some(0xff1b),
        Key::Named(NamedKey::Space) => Some(0x20),
        Key::Named(NamedKey::ArrowUp) => Some(0xff52),
        Key::Named(NamedKey::ArrowDown) => Some(0xff54),
        Key::Named(NamedKey::ArrowLeft) => Some(0xff51),
        Key::Named(NamedKey::ArrowRight) => Some(0xff53),
        Key::Named(NamedKey::Delete) => Some(0xffff),
        _ => text.and_then(|t| t.chars().next()).map(|c| c as u32),
    }
}

// ===========================================================================
// Sample data: the connections and nearby devices a screenshot shows.
// ===========================================================================

/// Readable fixture input for a sample connection. Named fields keep adjacent
/// strings, enums, and booleans from being silently transposed.
struct ConnectionSeed<'a> {
    name: &'a str,
    host: &'a str,
    port: u16,
    user: &'a str,
    kind: ConnectionKind,
    auth: AuthMethod,
    /// How long ago the last session was, in minutes; `None` for never.
    used_minutes_ago: Option<i64>,
    presence: Presence,
    group: &'a str,
    favorite: bool,
}

impl ConnectionSeed<'_> {
    fn build(self, index: u128, now: i64) -> Connection {
        Connection {
            // Stable ids keep a scenario's frame identical on every run.
            id: Uuid::from_u128(0x7e40_7e00_0000_0000_0000_0000_0000_0000 | index),
            name: self.name.into(),
            host: self.host.into(),
            port: self.port,
            user: self.user.into(),
            kind: self.kind,
            auth: self.auth,
            last_used: self.used_minutes_ago.map(|minutes| now - minutes * 60),
            presence: self.presence,
            group: self.group.into(),
            favorite: self.favorite,
        }
    }
}

/// A lived-in list of saved machines, last used relative to `now` (Unix
/// seconds).
#[allow(
    clippy::too_many_lines,
    reason = "fixture data: one named record per sample machine"
)]
pub(crate) fn sample_connections(now: i64) -> Vec<Connection> {
    let studio = SecretKey::from_bytes(&[17; 32]).public().to_string();
    let laptop = SecretKey::from_bytes(&[29; 32]).public().to_string();
    let seeds = [
        ConnectionSeed {
            name: "Build server",
            host: "build.lab.internal",
            port: 22,
            user: "admin",
            kind: ConnectionKind::Ssh,
            auth: AuthMethod::PublicKey,
            used_minutes_ago: Some(12),
            presence: Presence::Online,
            group: "Lab",
            favorite: true,
        },
        ConnectionSeed {
            name: "Studio desktop",
            host: &studio,
            port: 0,
            user: "",
            kind: ConnectionKind::Tilo,
            auth: AuthMethod::DeviceIdentity,
            used_minutes_ago: Some(1),
            presence: Presence::Online,
            group: "Tilo",
            favorite: true,
        },
        ConnectionSeed {
            name: "Design iMac",
            host: "10.0.0.24",
            port: 5900,
            user: "",
            kind: ConnectionKind::Vnc,
            auth: AuthMethod::Password,
            used_minutes_ago: Some(130),
            presence: Presence::Online,
            group: "Studio",
            favorite: false,
        },
        ConnectionSeed {
            name: "Media PC",
            host: "192.168.1.40",
            port: 5900,
            user: "",
            kind: ConnectionKind::Vnc,
            auth: AuthMethod::Password,
            used_minutes_ago: Some(26 * 60),
            presence: Presence::Online,
            group: "Home",
            favorite: false,
        },
        ConnectionSeed {
            name: "Edge gateway",
            host: "edge.example.com",
            port: 2222,
            user: "root",
            kind: ConnectionKind::Ssh,
            auth: AuthMethod::PublicKey,
            used_minutes_ago: Some(3 * 24 * 60),
            presence: Presence::Online,
            group: "Production",
            favorite: false,
        },
        ConnectionSeed {
            name: "Raspberry Pi cluster",
            host: "192.168.4.2",
            port: 22,
            user: "pi",
            kind: ConnectionKind::Ssh,
            auth: AuthMethod::Password,
            used_minutes_ago: Some(8 * 24 * 60),
            presence: Presence::Offline,
            group: "Lab",
            favorite: false,
        },
        ConnectionSeed {
            name: "Workshop laptop",
            host: &laptop,
            port: 0,
            user: "",
            kind: ConnectionKind::Tilo,
            auth: AuthMethod::DeviceIdentity,
            used_minutes_ago: Some(5 * 60),
            presence: Presence::Online,
            group: "Tilo",
            favorite: false,
        },
        ConnectionSeed {
            name: "Backup vault",
            host: "vault.example.com",
            port: 22,
            user: "backup",
            kind: ConnectionKind::Ssh,
            auth: AuthMethod::PublicKey,
            used_minutes_ago: Some(12 * 24 * 60),
            presence: Presence::Online,
            group: "Production",
            favorite: false,
        },
    ];
    seeds
        .into_iter()
        .zip(1_u128..)
        .map(|(seed, index)| seed.build(index, now))
        .collect()
}

/// A remote desktop profile saved before Remote dropped RDP: kept so its
/// owner can switch it to a protocol Remote opens.
pub(crate) fn sample_legacy_rdp(now: i64) -> Connection {
    ConnectionSeed {
        name: "Windows workstation",
        host: "10.0.0.31",
        port: 3389,
        user: "quinten",
        kind: ConnectionKind::Rdp,
        auth: AuthMethod::Password,
        used_minutes_ago: Some(40 * 24 * 60),
        presence: Presence::Offline,
        group: "Work",
        favorite: false,
    }
    .build(100, now)
}

fn seed_tilo_devices() -> Vec<TiloDevice> {
    let device = |name: &str, seed: u8, online: bool| TiloDevice {
        name: name.into(),
        node: SecretKey::from_bytes(&[seed; 32]).public().to_string(),
        online,
    };
    vec![
        device("Studio desktop", 17, true),
        device("Workshop laptop", 29, true),
        device("Living room TV", 41, true),
        device("Garage Pi", 53, false),
    ]
}

#[cfg(test)]
mod tests;

//! Root widget for the Remote app.
//!
//! Owns the connection [`Workspace`] (mirrored into the shared CRDT), the
//! current [`Screen`], and the per-screen interaction state. The whole UI
//! is custom-drawn into the Vello scene: this gives precise control over
//! the connection list, the add/edit form, and the two session surfaces
//! (an SSH terminal grid and a VNC/RDP/qOS framebuffer canvas).
//!
//! Interaction is region-based. Each frame `draw` registers clickable
//! [`Hit`] regions; `handle_event` hit-tests pointer presses against the
//! regions captured on the previous frame and dispatches an [`Action`].
//! Keyboard input drives the focused text field on the form and the
//! search box on the list, plus global shortcuts (Escape to go back,
//! Ctrl-N for a new connection, `/` to focus search).

use std::collections::HashMap;
use std::sync::Arc;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{
    Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageData as PenikoImage, ImageFormat,
};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::Widget;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, FlapState, SplitLayout, TextInput};

use crate::doc::RemoteDoc;
use crate::model::{
    AuthMethod, Connection, ConnectionDraft, ConnectionId, ConnectionKind, Presence, Screen,
    Workspace,
};
use crate::session::{SessionStatus, live_sessions_enabled};
use crate::ssh::{SshConfig, SshSession};
use crate::theme_ext::ColorExt;
use crate::vnc::{VncConfig, VncInput, VncSession};

/// A live transport backing an open session. Real, not simulated: SSH is a
/// `russh` PTY channel feeding the terminal grid; VNC is an RFB client
/// decoding the remote framebuffer. `Error` holds a pre-flight failure
/// (e.g. missing credentials) so the surface can show the honest reason
/// without ever fabricating a session.
enum LiveSession {
    Ssh(Box<SshSession>),
    Vnc(VncSession),
    Error(String),
    /// A fixed status with no transport, for `--shots`/preview rendering of
    /// the composed connecting/failed/closed surfaces. Never on the live path.
    Preview(SessionStatus),
}

impl LiveSession {
    fn status(&self) -> SessionStatus {
        match self {
            LiveSession::Ssh(s) => s.status(),
            LiveSession::Vnc(s) => s.status(),
            LiveSession::Error(e) => SessionStatus::Failed(e.clone()),
            LiveSession::Preview(s) => s.clone(),
        }
    }
}

enum PendingResourceOperation {
    Connect {
        invocation: qos_actions::InvocationId,
    },
    SshInput {
        invocation: qos_actions::InvocationId,
        acknowledgement: std::sync::mpsc::Receiver<Result<(), String>>,
    },
}

/// Accent of a non-live status surface (drives the toolbar dot colour).
#[derive(Clone, Copy)]
enum StatusTone {
    Neutral,
    Pending,
    Error,
}

/// Width of the left navigation rail.
const RAIL_W: f64 = 64.0;
/// Header height across the top of every screen.
const HEADER_H: f64 = 64.0;
/// Connection-row height in the list.
const ROW_H: f64 = 72.0;
/// Session toolbar height.
const TOOLBAR_H: f64 = 48.0;

/// Below this allocated width (logical px) the navigation rail stops being a
/// permanent docked column and becomes an overlay flap summoned by the
/// hamburger toggle. Chosen so a connection row's name + address + last-used +
/// Edit/Connect buttons (or the form card) keep breathing room once the 64px
/// rail is reclaimed for content.
const RAIL_BREAKPOINT: f64 = 720.0;
/// Size of the square hamburger toggle that summons the rail in overlay mode.
const HAMBURGER: f64 = 40.0;

/// A clickable region captured during `draw`, hit-tested on the next
/// pointer press. Keeping the action with the rect means `handle_event`
/// never re-derives geometry — it just looks up which region was hit.
#[derive(Clone, Debug)]
struct Hit {
    rect: Rect,
    action: Action,
}

/// Everything a click can do. Dispatched from `handle_event` after a
/// hit-test; applied by [`RemoteApp::apply`].
#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    /// Open the list (home).
    GoList,
    /// Open the add-connection form (blank).
    NewConnection,
    /// Open the form to edit an existing connection.
    EditConnection(ConnectionId),
    /// Connect to a saved machine (opens its session surface).
    Connect(ConnectionId),
    /// Select a row without connecting (shows the detail rail).
    SelectRow(ConnectionId),
    /// Toggle the favorite star on a row.
    ToggleFavorite(ConnectionId),
    /// Delete a saved connection.
    Delete(ConnectionId),
    /// Focus a named text field on the form (or the search box).
    FocusField(Field),
    /// Pick a protocol kind in the form.
    SetKind(ConnectionKind),
    /// Pick an auth method in the form.
    SetAuth(AuthMethod),
    /// Save the current form draft.
    SaveForm,
    /// Cancel the form / leave the session.
    Back,
    /// Toggle the session's scale-to-fit mode.
    ToggleScale,
    /// Toggle fullscreen for the session surface.
    ToggleFullscreen,
    /// Copy the session's clipboard sample (no-op offline, but live).
    CopyClipboard,
    /// Disconnect from the active session.
    Disconnect,
    /// Pick one of the discovered qOS devices into the form.
    PickQosDevice(usize),
    /// Open/close the navigation rail (hamburger toggle in overlay mode).
    ToggleRail,
}

/// Editable text fields. The search box lives on the list screen; the
/// rest live on the form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Field {
    Search,
    Name,
    Host,
    Port,
    User,
    Group,
}

impl Field {
    const ALL: [Field; 6] = [
        Field::Search,
        Field::Name,
        Field::Host,
        Field::Port,
        Field::User,
        Field::Group,
    ];

    /// The hint shown while the field is empty.
    fn placeholder(self) -> &'static str {
        match self {
            Field::Search => "Search machines…  ( / )",
            Field::Name => "My machine",
            Field::Host => "hostname or IP",
            Field::Port => "Port",
            Field::User => "username",
            Field::Group => "group (optional)",
        }
    }
}

/// The six editable fields, each a real [`TextInput`] — caret, selection,
/// clipboard, IME, keyboard navigation and a11y come from the shared widget
/// rather than being re-earned per field (HIG §8). The app keeps the plain
/// `String`s these mirror (`search`, and the draft) because the rest of it
/// filters, validates and saves from them; a focused input is the source of
/// truth and writes back on every edit.
struct FieldInputs {
    search: TextInput,
    name: TextInput,
    host: TextInput,
    port: TextInput,
    user: TextInput,
    group: TextInput,
}

impl FieldInputs {
    fn new() -> Self {
        let field = |f: Field| {
            let mut t = TextInput::local()
                .with_placeholder(f.placeholder())
                .with_font_size(14.0);
            // The form paints the field box + focus ring itself; the widget
            // paints the text, selection and caret inside it.
            t.set_plain_chrome(true);
            t
        };
        Self {
            search: field(Field::Search),
            name: field(Field::Name),
            host: field(Field::Host),
            port: field(Field::Port),
            user: field(Field::User),
            group: field(Field::Group),
        }
    }

    fn get(&mut self, f: Field) -> &mut TextInput {
        match f {
            Field::Search => &mut self.search,
            Field::Name => &mut self.name,
            Field::Host => &mut self.host,
            Field::Port => &mut self.port,
            Field::User => &mut self.user,
            Field::Group => &mut self.group,
        }
    }
}

/// A discoverable qOS peer, surfaced in the form's device picker. Offline
/// we seed a representative set so the picker is populated.
#[derive(Clone, Debug)]
struct QosDevice {
    name: String,
    node: String,
    online: bool,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag (session scale/fullscreen, \
              repaint gate, seed status); grouping them into enums would not express \
              any real invariant between them and would touch every read/write site \
              across this file"
)]
pub struct RemoteApp {
    /// The connection store. Source of truth for the list; mirrored into
    /// the shared CRDT in `sync`.
    workspace: Workspace,
    /// Which screen is showing.
    screen: Screen,
    /// Selected row on the list (for the detail rail / keyboard nav).
    selected: Option<ConnectionId>,
    /// Search query over the connection list.
    search: String,
    /// In-progress add/edit draft (only meaningful on the form screen).
    draft: ConnectionDraft,
    /// Which text field currently has keyboard focus.
    focus: Option<Field>,
    /// The shared [`TextInput`] behind each editable field.
    inputs: FieldInputs,
    /// The live transport for the open session (SSH terminal or VNC
    /// framebuffer). `None` until a session is opened with live sessions
    /// enabled; the default build leaves it `None` and shows the honest
    /// "not connected" surface.
    live: Option<LiveSession>,
    /// Cached GPU image of the most recent VNC frame, rebuilt only when a
    /// new framebuffer generation arrives.
    fb_image: Option<PenikoImage>,
    /// The framebuffer canvas rect from the last draw, so pointer events can
    /// be mapped back into remote-desktop coordinates.
    fb_canvas: Option<Rect>,
    /// Current VNC pointer button mask (bit 0 = left, 1 = middle, 2 = right).
    vnc_buttons: u8,
    /// Connection id the current session belongs to.
    session_for: Option<ConnectionId>,
    /// Whether the session surface is scaled to fit (vs. 1:1).
    scale_to_fit: bool,
    /// Whether the session surface is in fullscreen (hides chrome).
    fullscreen: bool,
    /// Discoverable qOS peers for the form's device picker.
    qos_devices: Vec<QosDevice>,

    /// Root rect, from `apply_layout`.
    rect: Rect,
    /// Clickable regions captured last `draw`, hit-tested on press.
    hits: Vec<Hit>,
    /// Last pointer position in logical coords (for hover + hit-tests).
    pointer: Point,
    /// Repaint gate.
    dirty: bool,
    /// True once the in-memory workspace has been mirrored into the CRDT.
    seeded: bool,

    /// Framework-owned action dispatch: the single path by which a saved
    /// connection is added, edited, renamed, favourited, or deleted. It brackets
    /// the undo boundary and applies each action through the executor, so the
    /// on-screen affordances and the Ctrl+K palette share one replayable path.
    actions: ui::ActionHost,
    pending_resource_operation: Option<PendingResourceOperation>,

    /// Eased open/closed state of the primary navigation rail. On a wide
    /// window the rail is docked and open; on a narrow window it collapses
    /// into an overlay flap (closed by default, summoned by the hamburger).
    /// This is the shared toolkit kernel — the same easing weather + chat use.
    rail: FlapState,
    /// Resolved docked-vs-overlay layout for the rail, recomputed each frame
    /// from the allocated width against [`RAIL_BREAKPOINT`].
    rail_layout: SplitLayout,
    /// When set, the next layout snaps the rail to this open state with no
    /// animation. Used by the single-frame `--shots` renderer so a narrow
    /// capture shows a settled (collapsed) flap rather than one mid-slide.
    rail_snap: Option<bool>,
    /// Wall-clock of the last rail animation advance, so the eased slide is
    /// frame-rate independent regardless of how often the host repaints.
    last_tick: std::time::Instant,
}

impl RemoteApp {
    /// The live app: starts empty. Only the user's real saved connections
    /// (held in the shared document, persisted across sessions) appear — no
    /// fabricated machines are ever written into the CRDT.
    pub fn new() -> Self {
        Self::from_parts(Workspace::new(), Vec::new())
    }

    /// The hermetic `--shots` / demo build: a representative connection list
    /// plus a discovered-device roster so the screenshot reads as lived-in.
    /// This is the ONLY place sample machines come from — the live app never
    /// fabricates them.
    pub fn new_demo() -> Self {
        let mut workspace = Workspace::new();
        for c in seed_connections() {
            workspace.upsert(c);
        }
        Self::from_parts(workspace, seed_qos_devices())
    }

    fn from_parts(workspace: Workspace, qos_devices: Vec<QosDevice>) -> Self {
        let selected = workspace.filtered("").first().map(|c| c.id);
        Self {
            workspace,
            screen: Screen::List,
            selected,
            search: String::new(),
            draft: ConnectionDraft::default(),
            focus: None,
            inputs: FieldInputs::new(),
            live: None,
            fb_image: None,
            fb_canvas: None,
            vnc_buttons: 0,
            session_for: None,
            scale_to_fit: true,
            fullscreen: false,
            qos_devices,
            rect: Rect::ZERO,
            hits: Vec::new(),
            pointer: Point::ZERO,
            dirty: true,
            seeded: false,
            actions: ui::ActionHost::new(),
            pending_resource_operation: None,
            rail: FlapState::open_when_docked(),
            rail_layout: SplitLayout::Docked,
            rail_snap: None,
            last_tick: std::time::Instant::now(),
        }
    }

    /// Stage a deterministic state for the single-frame `--shots` renderer.
    /// `"narrow"` collapses the rail into its closed overlay flap so a small
    /// viewport captures the responsive layout without animating in.
    #[doc(hidden)]
    pub fn dev_shot_scenario(&mut self, scenario: &str) {
        match scenario {
            "narrow" => self.rail_snap = Some(false),
            // The summoned overlay rail (flap open over the dimming scrim) —
            // proves the narrow open state for visual verification.
            "narrow-open" => self.rail_snap = Some(true),
            _ => {}
        }
    }

    /// Preview hook: jump straight to a named screen with representative
    /// state, so tests and offline render previews can exercise the form
    /// and session surfaces without driving the full event sequence.
    #[doc(hidden)]
    pub fn dev_set_screen(&mut self, which: &str) {
        let mut doc = AutoCommit::new();
        match which {
            "form" => self.apply(Action::NewConnection, &mut doc),
            "form-qos" => {
                self.apply(Action::NewConnection, &mut doc);
                self.apply(Action::SetKind(ConnectionKind::Qos), &mut doc);
            }
            "session-ssh" => {
                if let Some(id) = self.first_of_kind(ConnectionKind::Ssh) {
                    self.apply(Action::Connect(id), &mut doc);
                }
            }
            "session-vnc" => {
                if let Some(id) = self.first_of_kind(ConnectionKind::Vnc) {
                    self.apply(Action::Connect(id), &mut doc);
                }
            }
            "session-rdp" => {
                self.dev_preview_session(
                    ConnectionKind::Rdp,
                    SessionStatus::Failed("RDP is not available in qOS yet".into()),
                );
            }
            "session-qos" => {
                self.dev_preview_session(
                    ConnectionKind::Qos,
                    SessionStatus::Failed("qOS Relay is not available in qOS yet".into()),
                );
            }
            // Preview-only session states (no transport) for `--shots` grading
            // of the composed status surfaces.
            "session-connecting" => {
                self.dev_preview_session(ConnectionKind::Ssh, SessionStatus::Connecting);
            }
            "session-failed" => {
                self.dev_preview_session(
                    ConnectionKind::Vnc,
                    SessionStatus::Failed("Connection refused".into()),
                );
            }
            _ => {}
        }
    }

    /// Stage a preview session in a fixed status (shots/tests only).
    #[doc(hidden)]
    fn dev_preview_session(&mut self, kind: ConnectionKind, status: SessionStatus) {
        if let Some(id) = self.first_of_kind(kind) {
            self.session_for = Some(id);
            self.live = Some(LiveSession::Preview(status));
            self.screen = Screen::Session { id };
        }
    }

    /// Id of the first saved connection of a given kind, if any.
    fn first_of_kind(&self, kind: ConnectionKind) -> Option<ConnectionId> {
        self.workspace
            .connections()
            .iter()
            .find(|c| c.kind == kind)
            .map(|c| c.id)
    }

    /// Apply a dispatched [`Action`] to the app state.
    fn apply(&mut self, action: Action, state: &mut AutoCommit) {
        self.dirty = true;
        match action {
            Action::GoList | Action::Back => {
                self.screen = Screen::List;
                self.blur_field();
                self.dismiss_overlay_rail();
            }
            Action::NewConnection => {
                self.draft = ConnectionDraft::default();
                self.screen = Screen::Form { editing: None };
                self.reload_inputs();
                self.focus_field(Field::Name);
                self.dismiss_overlay_rail();
            }
            Action::EditConnection(id) => {
                if let Some(c) = self.workspace.get(id) {
                    self.draft = ConnectionDraft::from_connection(c);
                    self.screen = Screen::Form { editing: Some(id) };
                    self.reload_inputs();
                    self.focus_field(Field::Name);
                }
            }
            Action::Connect(id) => {
                self.open_session(id);
            }
            Action::SelectRow(id) => {
                self.selected = Some(id);
            }
            Action::ToggleFavorite(id) => {
                self.toggle_favorite(id, state);
            }
            Action::Delete(id) => {
                self.delete_connection(id, state);
            }
            Action::FocusField(f) => {
                self.focus_field(f);
            }
            Action::SetKind(k) => {
                self.draft.kind = k;
                // Keep the port in step with the kind's default unless the
                // user already typed a custom one for this kind.
                if k == ConnectionKind::Qos {
                    self.draft.port = String::new();
                    self.draft.auth = AuthMethod::DeviceIdentity;
                } else if self.draft.port.trim().is_empty() || self.draft.port.trim() == "0" {
                    self.draft.port = k.default_port().to_string();
                }
                self.reload_inputs();
            }
            Action::SetAuth(a) => {
                self.draft.auth = a;
            }
            Action::SaveForm => {
                self.save_form(state);
            }
            Action::ToggleScale => {
                self.scale_to_fit = !self.scale_to_fit;
            }
            Action::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
            }
            Action::CopyClipboard => {
                // Offline: clipboard sync is a no-op placeholder. Live, this
                // would push the remote selection into the local clipboard.
            }
            Action::Disconnect => {
                self.disconnect_session();
            }
            Action::PickQosDevice(i) => {
                if let Some(d) = self.qos_devices.get(i) {
                    self.draft.name = d.name.clone();
                    self.draft.host = d.node.clone();
                    self.draft.kind = ConnectionKind::Qos;
                    self.draft.auth = AuthMethod::DeviceIdentity;
                    self.draft.port = String::new();
                    self.reload_inputs();
                    self.focus_field(Field::Name);
                }
            }
            Action::ToggleRail => {
                self.rail.toggle(self.rail_layout);
            }
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
        let action = crate::actions::set_favorite_action(id, target);
        self.actions.dispatch_with_context(
            state,
            crate::actions::REMOTE_ACTION_TYPE,
            &action,
            qos_actions::ActionContext::for_action("org.qos.Remote", "Remote", &action),
        );
    }

    fn delete_connection(&mut self, id: ConnectionId, state: &mut AutoCommit) {
        self.workspace.remove(id);
        let action = crate::actions::delete_action(id);
        self.actions.dispatch_with_context(
            state,
            crate::actions::REMOTE_ACTION_TYPE,
            &action,
            qos_actions::ActionContext::for_action("org.qos.Remote", "Remote", &action),
        );
        if self.selected == Some(id) {
            self.selected = self.workspace.filtered(&self.search).first().map(|c| c.id);
        }
    }

    /// `true` while the rail is (resolved) open at the current layout.
    fn rail_open(&self) -> bool {
        self.rail.is_open(self.rail_layout)
    }

    /// Collapse the rail if it's an open overlay — a nav action chosen from
    /// the flap should hand the screen back to the content beneath it.
    fn dismiss_overlay_rail(&mut self) {
        if self.rail_layout == SplitLayout::Overlay && self.rail_open() {
            self.rail.close();
        }
    }

    /// Open the session surface for a connection. When `REMOTE_LIVE_SESSIONS`
    /// is unset (the default) this does **not** touch the network or
    /// fabricate a terminal/desktop: the surface shows the honest "not
    /// connected" card. When it is set, this opens a **real** transport —
    /// `russh` for SSH, an RFB client for VNC — and the surface tracks the
    /// real connection state (connecting → connected, or the honest error).
    fn open_session(&mut self, id: ConnectionId) {
        let Some(c) = self.workspace.get(id).cloned() else {
            return;
        };
        // Older documents may contain RDP or qOS Relay profiles. Keep them
        // editable, but never make an unavailable transport look connectable.
        if !c.kind.is_supported() {
            self.selected = Some(id);
            return;
        }
        self.session_for = Some(id);
        self.fullscreen = false;
        self.fb_image = None;
        self.fb_canvas = None;
        self.vnc_buttons = 0;
        self.live = None;
        if live_sessions_enabled() {
            self.live = Some(Self::start_live(&c));
            // Record the attempt in "last used"; presence is only marked
            // Online once the transport actually reports Connected (in draw).
            if let Some(m) = self.workspace.get_mut(id) {
                m.last_used = "just now".into();
            }
        }
        self.screen = Screen::Session { id };
    }

    fn disconnect_session(&mut self) {
        // Dropping the live session closes the transport (the worker thread
        // sees its channel close and disconnects).
        self.live = None;
        self.fb_image = None;
        self.fb_canvas = None;
        self.session_for = None;
        self.fullscreen = false;
        self.screen = Screen::List;
    }

    /// Build the real transport for a connection from its kind + auth and
    /// the process environment (secrets are never stored in the document).
    fn start_live(c: &Connection) -> LiveSession {
        match c.kind {
            ConnectionKind::Ssh => {
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
                    std::env::var("USER").unwrap_or_else(|_| "root".into())
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
            other => LiveSession::Error(format!(
                "{} transport is not implemented yet",
                other.label()
            )),
        }
    }

    /// Drain live-session I/O for this frame. Pulls SSH output into the grid
    /// and picks up new VNC frames. Returns true if the surface changed and
    /// should repaint.
    fn pump_live(&mut self) -> bool {
        let mut changed = false;
        match self.live.as_mut() {
            Some(LiveSession::Ssh(s)) => {
                if s.pump() {
                    changed = true;
                }
            }
            Some(LiveSession::Vnc(s)) => {
                if let Some(fb) = s.take_frame() {
                    self.fb_image = Some(PenikoImage {
                        data: Blob::new(Arc::new(fb.data)),
                        format: ImageFormat::Rgba8,
                        alpha_type: ImageAlphaType::Alpha,
                        width: u32::from(fb.width),
                        height: u32::from(fb.height),
                    });
                    changed = true;
                }
            }
            _ => {}
        }
        // Reflect a freshly-established connection in the row's presence.
        if let (Some(id), Some(live)) = (self.session_for, self.live.as_ref())
            && live.status() == SessionStatus::Connected
            && let Some(m) = self.workspace.get_mut(id)
            && m.presence != Presence::Online
        {
            m.presence = Presence::Online;
            changed = true;
        }
        changed
    }

    /// Whether a live transport is currently connecting or connected — used
    /// to keep the host pumping animation frames so incoming data renders.
    fn live_active(&self) -> bool {
        self.live.as_ref().is_some_and(|l| l.status().is_live())
    }

    /// The connected VNC session, if that's the current live transport.
    fn vnc_session(&self) -> Option<&VncSession> {
        match &self.live {
            Some(LiveSession::Vnc(s)) if s.status() == SessionStatus::Connected => Some(s),
            _ => None,
        }
    }

    /// Map a logical point to remote-desktop pixel coordinates, if it falls
    /// on the live VNC framebuffer canvas.
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

    /// Send a pointer event (current button mask + mapped position) to the
    /// live remote desktop, if the pointer is over its canvas.
    fn forward_vnc_pointer(&self, p: Point) {
        if let (Some(s), Some((x, y))) = (self.vnc_session(), self.vnc_pointer_target(p)) {
            s.send_input(VncInput::Pointer {
                button_mask: self.vnc_buttons,
                x,
                y,
            });
        }
    }

    /// Commit the form draft into the workspace + CRDT.
    fn save_form(&mut self, state: &mut AutoCommit) {
        if !self.draft.problems().is_empty() {
            return; // validation gate; the form shows the problems
        }
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
            existing.last_used.clone_into(&mut conn.last_used);
        }
        // One focused action for the edited fields — not a whole-workspace
        // re-mirror. favourite/presence/last_used are preserved by the action's
        // deterministic read of the current document (see [`crate::actions`]).
        let action = crate::actions::upsert_action(&conn);
        self.workspace.upsert(conn);
        self.actions.dispatch_with_context(
            state,
            crate::actions::REMOTE_ACTION_TYPE,
            &action,
            qos_actions::ActionContext::for_action("org.qos.Remote", "Remote", &action),
        );
        self.selected = Some(id);
        self.screen = Screen::List;
        self.blur_field();
    }

    /// Read-only handle to a field's current text.
    fn field_value(&self, field: Field) -> &str {
        match field {
            Field::Search => &self.search,
            Field::Name => &self.draft.name,
            Field::Host => &self.draft.host,
            Field::Port => &self.draft.port,
            Field::User => &self.draft.user,
            Field::Group => &self.draft.group,
        }
    }

    /// Mirror each **unfocused** field's input from the string behind it, so a
    /// draft loaded for an edit, a reset form, or a device picked from the
    /// discovery list shows up in the widget. A focused field is left alone —
    /// there it is the input that is authoritative, and it writes back through
    /// [`Self::commit_field`].
    fn sync_inputs(&mut self) {
        for f in Field::ALL {
            if self.focus == Some(f) {
                continue;
            }
            let value = self.field_value(f).to_string();
            self.inputs.get(f).set_text(&value);
        }
    }

    /// Mirror **every** field, focused one included. For the moments the app
    /// rewrites the draft out from under the user — loading one to edit,
    /// switching the connection kind (which resets the port), or filling the
    /// form from a discovered device — where the string is the newer of the two.
    fn reload_inputs(&mut self) {
        for f in Field::ALL {
            let value = self.field_value(f).to_string();
            let input = self.inputs.get(f);
            input.set_text(&value);
            input.set_cursor_pos(value.len());
        }
    }

    /// Copy a field's input back into the string the rest of the app reads.
    /// The port stays numeric here rather than per keystroke, so a pasted port
    /// arrives whole and is cleaned once instead of being dropped character by
    /// character on the way in.
    fn commit_field(&mut self, f: Field) {
        let mut value = self.inputs.get(f).value().to_string();
        if f == Field::Port {
            let digits: String = value.chars().filter(char::is_ascii_digit).collect();
            if digits != value {
                // Push the cleaned text back so what is stored is what is
                // shown; the caret follows to the end of it.
                self.inputs.get(f).set_text(&digits);
                self.inputs.get(f).set_cursor_pos(digits.len());
                value = digits;
            }
        }
        match f {
            Field::Search => self.search = value,
            Field::Name => self.draft.name = value,
            Field::Host => self.draft.host = value,
            Field::Port => self.draft.port = value,
            Field::User => self.draft.user = value,
            Field::Group => self.draft.group = value,
        }
    }

    /// Move keyboard focus to `f`, telling the widgets about it.
    fn focus_field(&mut self, f: Field) {
        self.blur_field();
        self.focus = Some(f);
        self.inputs.get(f).set_focus(true);
    }

    /// Drop keyboard focus from whichever field holds it.
    fn blur_field(&mut self) {
        if let Some(f) = self.focus.take() {
            self.inputs.get(f).set_focus(false);
            self.dirty = true;
        }
    }

    /// Route one event into the focused field's input and write the result
    /// back. Returns whether the input consumed it.
    #[allow(clippy::too_many_arguments)]
    fn route_to_focused_field(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        let Some(f) = self.focus else { return false };
        let before = self.inputs.get(f).value().to_string();
        let input = self.inputs.get(f);
        input.set_focus(true);
        let (mut handled, mut focus_target) = (false, None);
        input.handle_event(
            event,
            scale,
            &modifiers,
            state,
            path,
            &mut handled,
            &mut focus_target,
            node_id,
            taffy,
        );
        if self.inputs.get(f).value() != before {
            self.commit_field(f);
        }
        self.dirty = true;
        handled
    }

    /// Push a clickable region for this frame.
    fn push_hit(&mut self, rect: Rect, action: Action) {
        self.hits.push(Hit { rect, action });
    }

    /// The last action an on-screen affordance dispatched, for the
    /// affordance == [`ActionSpec`](qos_actions::ActionSpec) covenant test.
    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }
}

impl Default for RemoteApp {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// Seed data — a populated, lived-in connection list + qOS device roster so
// the offline screenshot and first launch are immediately meaningful.
// ===========================================================================

/// Build one seed [`Connection`]. Broken out of [`seed_connections`] purely to
/// keep that function's line count down — the field list is identical to
/// [`Connection`] itself, just positional.
#[allow(
    clippy::too_many_arguments,
    reason = "one positional field per Connection column; a builder struct would \
              just move the same ten values into named fields for a private \
              seed-data helper used only here"
)]
fn seeded_connection(
    name: &str,
    host: &str,
    port: u16,
    user: &str,
    kind: ConnectionKind,
    auth: AuthMethod,
    last: &str,
    presence: Presence,
    group: &str,
    fav: bool,
) -> Connection {
    Connection {
        id: Uuid::new_v4(),
        name: name.into(),
        host: host.into(),
        port,
        user: user.into(),
        kind,
        auth,
        last_used: last.into(),
        presence,
        group: group.into(),
        favorite: fav,
    }
}

fn seed_connections() -> Vec<Connection> {
    let mut list = seed_connections_lab_and_office();
    list.extend(seed_connections_remote_and_backup());
    list
}

/// First half of [`seed_connections`]' roster — split out only to stay under
/// the line-count limit; order matters (it's the on-screen list order).
fn seed_connections_lab_and_office() -> Vec<Connection> {
    vec![
        seeded_connection(
            "Build Box",
            "build.lab.internal",
            22,
            "admin",
            ConnectionKind::Ssh,
            AuthMethod::PublicKey,
            "12m ago",
            Presence::Online,
            "lab",
            true,
        ),
        seeded_connection(
            "Studio Node",
            "studio-node",
            0,
            "",
            ConnectionKind::Qos,
            AuthMethod::DeviceIdentity,
            "just now",
            Presence::Online,
            "qOS",
            true,
        ),
        seeded_connection(
            "Office NAS",
            "10.0.0.20",
            5900,
            "media",
            ConnectionKind::Vnc,
            AuthMethod::Password,
            "2h ago",
            Presence::Online,
            "home",
            false,
        ),
        seeded_connection(
            "Win Workstation",
            "10.0.0.31",
            3389,
            "quinten",
            ConnectionKind::Rdp,
            AuthMethod::Password,
            "yesterday",
            Presence::Offline,
            "work",
            false,
        ),
    ]
}

/// Second half of [`seed_connections`]' roster — see
/// [`seed_connections_lab_and_office`].
fn seed_connections_remote_and_backup() -> Vec<Connection> {
    vec![
        seeded_connection(
            "Edge Gateway",
            "edge.example.com",
            2222,
            "root",
            ConnectionKind::Ssh,
            AuthMethod::PublicKey,
            "3d ago",
            Presence::Online,
            "prod",
            false,
        ),
        seeded_connection(
            "Pi Cluster Head",
            "192.168.4.2",
            22,
            "pi",
            ConnectionKind::Ssh,
            AuthMethod::Password,
            "1w ago",
            Presence::Offline,
            "lab",
            false,
        ),
        seeded_connection(
            "Workshop Laptop",
            "workshop-laptop",
            0,
            "",
            ConnectionKind::Qos,
            AuthMethod::DeviceIdentity,
            "5h ago",
            Presence::Online,
            "qOS",
            false,
        ),
        seeded_connection(
            "Backup Vault",
            "vault.example.com",
            5901,
            "ops",
            ConnectionKind::Vnc,
            AuthMethod::Password,
            "Never",
            Presence::Unknown,
            "prod",
            false,
        ),
    ]
}

fn seed_qos_devices() -> Vec<QosDevice> {
    vec![
        QosDevice {
            name: "Studio Node".into(),
            node: "studio-node".into(),
            online: true,
        },
        QosDevice {
            name: "Workshop Laptop".into(),
            node: "workshop-laptop".into(),
            online: true,
        },
        QosDevice {
            name: "Living Room TV".into(),
            node: "lr-display".into(),
            online: true,
        },
        QosDevice {
            name: "Garage Pi".into(),
            node: "garage-pi".into(),
            online: false,
        },
    ]
}

/// Read-only "how to route this input" bundle threaded through
/// `handle_event`'s per-arm helpers, so extracting them doesn't balloon each
/// one's parameter list.
struct EventRouteCtx<'a> {
    scale_factor: f64,
    modifiers: winit::event::Modifiers,
    path: &'a ObjId,
    node_id: NodeId,
    taffy: &'a TaffyTree<()>,
}

// ===========================================================================
// Widget impl
// ===========================================================================

impl Widget for RemoteApp {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::REMOTE_OPERATION_TYPE]
    }

    /// Publish each saved connection as a Ctrl+K resource (rename / favourite /
    /// delete) so the shell's one action palette scopes to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        let mut resources = <Self as qos_actions::ResourceProvider>::resources(self, doc);
        if let Some(id) = self.session_for
            && let Some(connection) = self.workspace.get(id)
        {
            let can_send_text = matches!(
                &self.live,
                Some(LiveSession::Ssh(session))
                    if session.status() == SessionStatus::Connected
            );
            resources.push(crate::actions::session_resource(connection, can_send_text));
        }
        resources
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its Remote action, so it records one undo boundary bracketing it.
    /// Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind != qos_actions::OperationKind::Effect {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        match operation {
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
                match &self.live {
                    Some(LiveSession::Ssh(session))
                        if session.status() == SessionStatus::Connected =>
                    {
                        session.send_input(text.into_bytes());
                        self.dirty = true;
                    }
                    _ => return Err("text input requires a connected SSH terminal".to_string()),
                }
            }
        }
        Ok(true)
    }

    fn start_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != qos_actions::OperationKind::Effect {
            return Ok(None);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        if self.pending_resource_operation.is_some() {
            return Err("another remote operation is still running".into());
        }
        match operation {
            crate::actions::RemoteOperation::Connect { connection_id } => {
                if !live_sessions_enabled() {
                    return Err("live remote sessions are disabled".into());
                }
                if self.workspace.get(connection_id).is_none() {
                    return Err("saved connection no longer exists".into());
                }
                self.open_session(connection_id);
                match self.live.as_ref().map(LiveSession::status) {
                    Some(SessionStatus::Connecting) => {
                        self.pending_resource_operation =
                            Some(PendingResourceOperation::Connect { invocation });
                        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
                    }
                    Some(SessionStatus::Connected) => {
                        Ok(Some(ui::widget::ResourceOperationStart::Completed(
                            qos_actions::ActionOutput::default(),
                        )))
                    }
                    Some(SessionStatus::Failed(message)) => Err(message),
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
                let acknowledgement = match &self.live {
                    Some(LiveSession::Ssh(session))
                        if session.status() == SessionStatus::Connected =>
                    {
                        session.send_input_ack(text.into_bytes())?
                    }
                    _ => return Err("text input requires a connected SSH terminal".into()),
                };
                self.pending_resource_operation = Some(PendingResourceOperation::SshInput {
                    invocation,
                    acknowledgement,
                });
                Ok(Some(ui::widget::ResourceOperationStart::Accepted))
            }
            crate::actions::RemoteOperation::Disconnect { .. } => {
                if !self.invoke_resource_operation(kind, type_id, payload, doc, path)? {
                    return Ok(None);
                }
                Ok(Some(ui::widget::ResourceOperationStart::Completed(
                    qos_actions::ActionOutput::default(),
                )))
            }
        }
    }

    fn poll_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<qos_actions::InvocationUpdate> {
        let update = match self.pending_resource_operation.as_ref()? {
            PendingResourceOperation::Connect {
                invocation: pending,
            } if *pending == invocation => match self.live.as_ref().map(LiveSession::status) {
                Some(SessionStatus::Connecting) => return None,
                Some(SessionStatus::Connected) => qos_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: qos_actions::ActionOutput::default(),
                },
                Some(SessionStatus::Failed(message)) => qos_actions::InvocationUpdate::Failed {
                    invocation,
                    error: qos_actions::ActionFailure {
                        code: "remote.connect_failed".into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
                Some(SessionStatus::Closed) | None => qos_actions::InvocationUpdate::Failed {
                    invocation,
                    error: qos_actions::ActionFailure {
                        code: "remote.connection_closed".into(),
                        message: "remote connection closed before it became ready".into(),
                        details: None,
                        retryable: false,
                    },
                },
            },
            PendingResourceOperation::SshInput {
                invocation: pending,
                acknowledgement,
            } if *pending == invocation => match acknowledgement.try_recv() {
                Ok(Ok(())) => qos_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: qos_actions::ActionOutput::default(),
                },
                Ok(Err(message)) => qos_actions::InvocationUpdate::Failed {
                    invocation,
                    error: qos_actions::ActionFailure {
                        code: "remote.input_failed".into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
                Err(std::sync::mpsc::TryRecvError::Empty) => return None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    qos_actions::InvocationUpdate::Failed {
                        invocation,
                        error: qos_actions::ActionFailure {
                            code: "remote.input_worker_lost".into(),
                            message: "remote input worker stopped before acknowledgement".into(),
                            details: None,
                            retryable: false,
                        },
                    }
                }
            },
            _ => return None,
        };
        self.pending_resource_operation = None;
        Some(update)
    }

    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        // Publish our own connection list into the shared document the first
        // time we run, then adopt any remote additions. The live app's list
        // is empty on a fresh machine, so nothing fabricated is ever written
        // into the CRDT — only the demo build (or the user's real, persisted
        // connections) has anything to publish here.
        let doc_conns = RemoteDoc::list(state);
        if self.seeded {
            // Pull any connections a peer added that we don't have yet.
            let local: std::collections::HashSet<ConnectionId> =
                self.workspace.connections().iter().map(|c| c.id).collect();
            for dc in doc_conns {
                if !local.contains(&dc.id) {
                    self.workspace.connections.push(dc);
                    self.dirty = true;
                }
            }
        } else {
            if doc_conns.is_empty() {
                // We're the origin — publish our list (empty when live).
                for c in self.workspace.connections().to_vec() {
                    RemoteDoc::upsert(state, &c);
                }
            } else {
                // Adopt a peer's list wholesale.
                self.workspace.connections = doc_conns;
                if self.selected.and_then(|s| self.workspace.get(s)).is_none() {
                    self.selected = self.workspace.filtered(&self.search).first().map(|c| c.id);
                }
            }
            self.seeded = true;
            self.dirty = true;
        }
    }

    fn needs_idle_sync(&self) -> bool {
        !self.seeded
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The app draws itself; we just need a single full-bleed node so
        // the host can size us. No child widgets participate in taffy.
        // `new_leaf` on a freshly created tree never fails.
        #[allow(clippy::unwrap_used, reason = "new_leaf on a fresh tree cannot fail")]
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // Drive the rail's docked-vs-overlay choice from our *allocated*
        // width, then reconcile so a window resize across the breakpoint
        // returns the rail to that mode's default. A pending `--shots` snap
        // settles the flap deterministically for a single offscreen frame.
        self.rail_layout = SplitLayout::from_width(self.rect.width(), RAIL_BREAKPOINT);
        if let Some(open) = self.rail_snap.take() {
            self.rail.snap(open);
        }
        self.rail.reconcile(self.rail_layout);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.hits.clear();
        let tokens = &theme.tokens;
        // Mirror the strings behind the fields into their inputs before the
        // frame reads them; a focused input stays authoritative.
        self.sync_inputs();

        // Advance the rail's eased slide for this frame. While it's still
        // settling, keep the repaint gate open so the host pumps the next
        // frame; the dt is real elapsed time so the slide is frame-rate
        // independent.
        let now = std::time::Instant::now();
        let dt = (now - self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        if self.rail.tick(self.rail_layout, dt) {
            self.dirty = true;
        }

        // Pull any live-session I/O for this frame (remote PTY output, new
        // VNC frames) so the surface reflects the real connection.
        if matches!(self.screen, Screen::Session { .. }) && self.pump_live() {
            self.dirty = true;
        }

        // Whole-window background.
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        // Fullscreen session hides the rail + header for an immersive view.
        let immersive = matches!(self.screen, Screen::Session { .. }) && self.fullscreen;

        let overlay = self.rail_layout == SplitLayout::Overlay;
        let reveal = self.rail.reveal();

        // Content area. Docked: inset by the eased rail width so the rail
        // reveals/collapses without overlapping. Overlay: content spans full
        // width and the rail floats over it. Immersive sessions are full-bleed.
        let content = if immersive {
            self.rect
        } else if overlay {
            // Reserve room at the content's top-left for the floating
            // hamburger so the header title / search box don't sit under it.
            let inset = 12.0 + HAMBURGER + 12.0;
            Rect::new(
                self.rect.x0 + inset,
                self.rect.y0,
                self.rect.x1,
                self.rect.y1,
            )
        } else {
            let inset = RAIL_W * reveal;
            Rect::new(
                self.rect.x0 + inset,
                self.rect.y0,
                self.rect.x1,
                self.rect.y1,
            )
        };

        match self.screen.clone() {
            Screen::List => {
                self.draw_list(scene, transform, text_renderer, theme, presence, content);
            }
            Screen::Form { editing } => self.draw_form(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                content,
                editing,
            ),
            Screen::Session { id } => self.draw_session(
                scene,
                transform,
                text_renderer,
                theme,
                content,
                id,
                immersive,
            ),
        }

        if immersive {
            return;
        }

        // In overlay mode a dimming scrim sits between the content and the
        // sliding rail; tapping it dismisses the rail. A hamburger floats at
        // the top-left of the content to summon the rail.
        if overlay {
            if reveal > 0.001 {
                let scrim = Color::from_rgba8(0, 0, 0, 255).at_alpha((reveal * 0.5) as f32);
                scene.fill(Fill::NonZero, transform, scrim, None, &self.rect);
            }
            self.draw_hamburger(scene, transform, text_renderer, theme);
        }

        // The rail itself, drawn last so it sits above the content + scrim.
        // Docked: slides in from the left edge as it reveals. Overlay: slides
        // over the content. Hidden entirely once fully collapsed.
        if reveal > 0.001 {
            let slide = self.rail.slide();
            let rail_x0 = self.rect.x0 - RAIL_W * (1.0 - slide);
            self.draw_rail(scene, transform, text_renderer, theme, rail_x0);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let ctx = EventRouteCtx {
            scale_factor,
            modifiers: *modifiers,
            path,
            node_id,
            taffy,
        };
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.handle_cursor_moved_pos(p, event, state, &ctx);
            }
            // A press/release over the live VNC canvas is injected into the
            // remote desktop rather than hit-testing the local chrome.
            WindowEvent::MouseInput { state, button, .. }
                if self.vnc_pointer_target(self.pointer).is_some() =>
            {
                self.handle_vnc_mouse_input(*state, *button);
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_press_event(event, state, handled, &ctx);
            }
            // Ending a drag has to reach the field that started it, or the
            // selection keeps extending after the button is up.
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } if self.focus.is_some() => {
                self.route_to_focused_field(
                    event,
                    ctx.scale_factor,
                    ctx.modifiers,
                    state,
                    ctx.path,
                    ctx.node_id,
                    ctx.taffy,
                );
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        text,
                        ..
                    },
                ..
            } => {
                self.handle_keyboard_input_event(
                    event,
                    logical_key,
                    text.as_ref().map(winit::keyboard::SmolStr::as_str),
                    state,
                    handled,
                    &ctx,
                );
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "RemoteApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn needs_animation_frame(&self) -> bool {
        // While the rail's overlay/dock slide is mid-flight, keep the host
        // pumping frames so the eased transition plays smoothly. A live
        // session likewise needs continuous frames so incoming remote data
        // (PTY output, framebuffer updates) is drained and rendered.
        self.rail.is_animating(self.rail_layout) || self.live_active()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

impl RemoteApp {
    /// `WindowEvent::CursorMoved`'s share of `handle_event`: track hover,
    /// stream motion to a live remote desktop, and let a focused field's drag
    /// follow the pointer.
    fn handle_cursor_moved_pos(
        &mut self,
        p: Point,
        event: &WindowEvent,
        state: &mut AutoCommit,
        ctx: &EventRouteCtx<'_>,
    ) {
        if p != self.pointer {
            self.pointer = p;
            self.dirty = true; // hover feedback
            // Stream pointer motion to a live remote desktop.
            self.forward_vnc_pointer(p);
        }
        // A focused field tracks the pointer so a drag extends its
        // selection past the edge of the box.
        if self.focus.is_some() {
            self.route_to_focused_field(
                event,
                ctx.scale_factor,
                ctx.modifiers,
                state,
                ctx.path,
                ctx.node_id,
                ctx.taffy,
            );
        }
    }

    /// A press/release over the live VNC canvas: update the button mask and
    /// forward the pointer, rather than hit-testing the local chrome.
    fn handle_vnc_mouse_input(&mut self, state: ElementState, button: MouseButton) {
        let bit = match button {
            MouseButton::Left => 1,
            MouseButton::Middle => 2,
            MouseButton::Right => 4,
            _ => 0,
        };
        match state {
            ElementState::Pressed => self.vnc_buttons |= bit,
            ElementState::Released => self.vnc_buttons &= !bit,
        }
        self.forward_vnc_pointer(self.pointer);
    }

    /// A left press: dismiss an open overlay rail's scrim, else hit-test the
    /// regions captured last frame.
    fn handle_left_press_event(
        &mut self,
        event: &WindowEvent,
        state: &mut AutoCommit,
        handled: &mut bool,
        ctx: &EventRouteCtx<'_>,
    ) {
        let p = self.pointer;

        // Scrim dismissal: when the rail is an open overlay, a press
        // on the dimmed content (outside the rail itself, and not on
        // the hamburger) closes the rail and is consumed — matching the
        // toolkit flap's scrim-click behaviour.
        if self.rail_layout == SplitLayout::Overlay && self.rail_open() {
            let rail_rect = Rect::new(
                self.rect.x0,
                self.rect.y0,
                self.rect.x0 + RAIL_W,
                self.rect.y1,
            );
            let hamburger = Rect::new(
                self.rect.x0 + 12.0,
                self.rect.y0 + 12.0,
                self.rect.x0 + 12.0 + HAMBURGER,
                self.rect.y0 + 12.0 + HAMBURGER,
            );
            if !rail_rect.contains(p) && !hamburger.contains(p) {
                self.rail.close();
                self.dirty = true;
                *handled = true;
                return;
            }
        }

        // Hit-test the regions captured on the previous frame.
        if let Some(action) = self
            .hits
            .iter()
            .rev()
            .find(|h| h.rect.contains(p))
            .map(|h| h.action)
        {
            // A press on a field focuses it *and* goes on to the input,
            // so the caret lands where the pointer did and a drag from
            // here selects — rather than the click only meaning "focus".
            if let Action::FocusField(f) = action {
                self.focus_field(f);
                self.route_to_focused_field(
                    event,
                    ctx.scale_factor,
                    ctx.modifiers,
                    state,
                    ctx.path,
                    ctx.node_id,
                    ctx.taffy,
                );
            } else {
                self.apply(action, state);
            }
            *handled = true;
        } else {
            // A click on empty space drops field focus.
            self.blur_field();
        }
    }

    /// A pressed key: run the app's own shortcuts/navigation first; anything
    /// unclaimed falls through to the focused field's input (typing, caret
    /// movement, selection, clipboard, IME).
    fn handle_keyboard_input_event(
        &mut self,
        event: &WindowEvent,
        logical_key: &Key,
        text: Option<&str>,
        state: &mut AutoCommit,
        handled: &mut bool,
        ctx: &EventRouteCtx<'_>,
    ) {
        self.handle_key(logical_key, text, ctx.modifiers.state(), state, handled);
        if !*handled && self.focus.is_some() {
            *handled = self.route_to_focused_field(
                event,
                ctx.scale_factor,
                ctx.modifiers,
                state,
                ctx.path,
                ctx.node_id,
                ctx.taffy,
            );
        }
    }

    /// Process a key press. Returns via `handled`.
    fn handle_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        if self.handle_key_global_shortcut(key, mods, state, handled) {
            return;
        }
        if self.handle_key_live_session(key, text, mods, handled) {
            return;
        }
        self.handle_key_navigation(key, text, state, handled);
    }

    /// Ctrl/Cmd shortcuts that work regardless of focus. Returns whether the
    /// key was fully handled (caller should stop dispatching further).
    fn handle_key_global_shortcut(
        &mut self,
        key: &Key,
        mods: ModifiersState,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        let ctrl = mods.control_key() || mods.super_key();
        if ctrl && let Key::Character(c) = key {
            match c.to_lowercase().as_str() {
                "n" => {
                    self.apply(Action::NewConnection, state);
                    *handled = true;
                    return true;
                }
                "f" => {
                    self.apply(Action::FocusField(Field::Search), state);
                    if matches!(self.screen, Screen::Form { .. } | Screen::Session { .. }) {
                        self.apply(Action::GoList, state);
                        self.apply(Action::FocusField(Field::Search), state);
                    }
                    *handled = true;
                    return true;
                }
                "w" => {
                    if matches!(self.screen, Screen::Session { .. }) {
                        self.apply(Action::Disconnect, state);
                        *handled = true;
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// A connected session captures keyboard input for the remote — except
    /// Escape while fullscreen, which exits fullscreen locally. Returns
    /// whether the key was fully handled (caller should stop dispatching
    /// further).
    fn handle_key_live_session(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
        handled: &mut bool,
    ) -> bool {
        let esc_exits_fullscreen = self.fullscreen && matches!(key, Key::Named(NamedKey::Escape));
        if !esc_exits_fullscreen && matches!(self.screen, Screen::Session { .. }) {
            match &self.live {
                Some(LiveSession::Ssh(s)) if s.status() == SessionStatus::Connected => {
                    if let Some(bytes) = Self::ssh_key_bytes(key, text, mods) {
                        s.send_input(bytes);
                        self.dirty = true;
                        *handled = true;
                        return true;
                    }
                }
                Some(LiveSession::Vnc(s)) if s.status() == SessionStatus::Connected => {
                    if let Some(sym) = Self::vnc_keysym(key, text) {
                        s.send_input(VncInput::Key {
                            down: true,
                            key: sym,
                        });
                        s.send_input(VncInput::Key {
                            down: false,
                            key: sym,
                        });
                        self.dirty = true;
                        *handled = true;
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Screen navigation / local editing keys, once neither a global shortcut
    /// nor a connected live session claimed the key.
    fn handle_key_navigation(
        &mut self,
        key: &Key,
        text: Option<&str>,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        match key {
            Key::Named(NamedKey::Escape) => {
                // An open overlay rail is dismissed first (toolkit flap parity).
                if self.rail_layout == SplitLayout::Overlay && self.rail_open() {
                    self.rail.close();
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                match self.screen {
                    Screen::List => {
                        if self.focus == Some(Field::Search) {
                            self.search.clear();
                            self.blur_field();
                            self.sync_inputs();
                        }
                    }
                    Screen::Form { .. } => self.apply(Action::Back, state),
                    Screen::Session { .. } => {
                        if self.fullscreen {
                            self.fullscreen = false;
                            self.dirty = true;
                        } else {
                            self.apply(Action::Back, state);
                        }
                    }
                }
                *handled = true;
            }
            Key::Named(NamedKey::Enter) => {
                match self.screen.clone() {
                    Screen::Form { .. } => self.apply(Action::SaveForm, state),
                    Screen::List => {
                        if let Some(id) = self.selected {
                            self.apply(Action::Connect(id), state);
                        }
                    }
                    Screen::Session { .. } => {}
                }
                *handled = true;
            }
            Key::Named(NamedKey::Tab) => {
                if matches!(self.screen, Screen::Form { .. }) {
                    self.focus_field(next_field(self.focus, self.draft.kind));
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp) => {
                if matches!(self.screen, Screen::List) && self.focus.is_none() {
                    let down = matches!(key, Key::Named(NamedKey::ArrowDown));
                    self.move_selection(down);
                    *handled = true;
                }
            }
            Key::Character(_) => {
                // `/` focuses search on the list when nothing else is focused.
                // Everything else falls through to the focused field's input.
                if let Some(t) = text
                    && t == "/"
                    && matches!(self.screen, Screen::List)
                    && self.focus.is_none()
                {
                    self.focus_field(Field::Search);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    /// Move list selection up/down within the filtered view.
    fn move_selection(&mut self, down: bool) {
        let filtered: Vec<ConnectionId> = self
            .workspace
            .filtered(&self.search)
            .iter()
            .map(|c| c.id)
            .collect();
        if filtered.is_empty() {
            return;
        }
        let cur = self
            .selected
            .and_then(|s| filtered.iter().position(|id| *id == s))
            .unwrap_or(0);
        let next = if down {
            (cur + 1).min(filtered.len() - 1)
        } else {
            cur.saturating_sub(1)
        };
        // `next` is derived from `cur` (in bounds, or 0) via `min`/`saturating_sub`.
        if let Some(&id) = filtered.get(next) {
            self.selected = Some(id);
        }
        self.dirty = true;
    }

    // -- Drawing helpers ----------------------------------------------------

    /// Left navigation rail: app glyph + new-connection + back/home. `x0` is
    /// the rail's left edge, which the caller slides off-screen while the rail
    /// is collapsing (docked) or animating in over the content (overlay).
    fn draw_rail(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        x0: f64,
    ) {
        let tokens = &theme.tokens;
        let rail = Rect::new(x0, self.rect.y0, x0 + RAIL_W, self.rect.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.neutral[1],
            None,
            &rail,
        );
        // Right hairline.
        let seam = Rect::new(rail.x1 - 1.0, rail.y0, rail.x1, rail.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.border_standard,
            None,
            &seam,
        );

        // App glyph at the top — a stylized monitor in the accent colour.
        let glyph = Rect::new(
            rail.x0 + 16.0,
            rail.y0 + 18.0,
            rail.x0 + 48.0,
            rail.y0 + 44.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.accent[5],
            None,
            &RoundedRect::from_rect(glyph, 5.0),
        );
        let stand = Rect::new(
            glyph.center().x - 6.0,
            glyph.y1,
            glyph.center().x + 6.0,
            glyph.y1 + 5.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.accent[5],
            None,
            &stand,
        );

        // Rail buttons.
        let on_list = matches!(self.screen, Screen::List);
        let home_btn = self.draw_rail_button(
            scene,
            transform,
            text_renderer,
            theme,
            rail,
            0,
            "≡",
            "Connections",
            on_list,
        );
        self.push_hit(home_btn, Action::GoList);

        let new_btn = self.draw_rail_button(
            scene,
            transform,
            text_renderer,
            theme,
            rail,
            1,
            "+",
            "Add",
            false,
        );
        self.push_hit(new_btn, Action::NewConnection);
    }

    /// Draw the hamburger toggle that summons the rail in overlay mode. It
    /// floats at the top-left of the content; clicking it opens or closes the
    /// overlay rail. The glyph flips to a back-chevron once the rail is open.
    fn draw_hamburger(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let open = self.rail_open();
        // Anchored to the root's top-left (the content is inset to clear it).
        let btn = Rect::new(
            self.rect.x0 + 12.0,
            self.rect.y0 + 12.0,
            self.rect.x0 + 12.0 + HAMBURGER,
            self.rect.y0 + 12.0 + HAMBURGER,
        );
        let hovered = btn.contains(self.pointer);
        let rr = RoundedRect::from_rect(btn, 8.0);
        let bg = if hovered {
            tokens.colors.surfaces.raised
        } else {
            tokens.colors.neutral[1]
        };
        scene.fill(Fill::NonZero, transform, bg, None, &rr);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.border_standard,
            None,
            &rr,
        );

        // Three stacked bars (or a chevron when open) in the dim text colour.
        let color = tokens.colors.text_primary;
        if open {
            // Back chevron «.
            let glyph = "‹";
            let (gw, _) = text_renderer.measure_weighted(glyph, 22.0, None, TextWeight::Bold);
            text_renderer.draw_text_weighted(
                scene,
                glyph,
                22.0,
                color,
                transform * Affine::translate((btn.center().x - f64::from(gw) / 2.0, btn.y0 + 6.0)),
                None,
                TextWeight::Bold,
            );
        } else {
            let bar_w = 18.0;
            let bar_h = 2.0;
            let cx = btn.center().x - bar_w / 2.0;
            for i in 0..3 {
                let by = btn.center().y - 6.0 + f64::from(i) * 6.0;
                let bar = Rect::new(cx, by, cx + bar_w, by + bar_h);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    color,
                    None,
                    &RoundedRect::from_rect(bar, 1.0),
                );
            }
        }
        self.push_hit(btn, Action::ToggleRail);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_rail_button(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rail: Rect,
        index: usize,
        glyph: &str,
        _label: &str,
        active: bool,
    ) -> Rect {
        let tokens = &theme.tokens;
        let top = rail.y0 + 72.0 + index as f64 * 56.0;
        let btn = Rect::new(rail.x0 + 12.0, top, rail.x1 - 12.0, top + 40.0);
        let hovered = btn.contains(self.pointer);
        let bg = if active {
            tokens.colors.accent[5].at_alpha(0.20)
        } else if hovered {
            tokens.colors.surface_white_10
        } else {
            Color::TRANSPARENT
        };
        if bg != Color::TRANSPARENT {
            scene.fill(
                Fill::NonZero,
                transform,
                bg,
                None,
                &RoundedRect::from_rect(btn, 8.0),
            );
        }
        let color = if active {
            tokens.colors.accent[6]
        } else {
            tokens.colors.text_dim
        };
        let (gw, _) = text_renderer.measure(glyph, 22.0, None);
        text_renderer.draw_text(
            scene,
            glyph,
            22.0,
            color,
            transform * Affine::translate((btn.center().x - f64::from(gw) / 2.0, btn.y0 + 6.0)),
            None,
        );
        btn
    }

    /// Draw a connection-list header (title + stats + new button + search).
    fn draw_list(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        content: Rect,
    ) {
        let tokens = &theme.tokens;
        let pad = 24.0;

        // --- Header ---
        let header = Rect::new(content.x0, content.y0, content.x1, content.y0 + HEADER_H);
        text_renderer.draw_text_weighted(
            scene,
            "Remote Machines",
            22.0,
            tokens.colors.text_primary,
            transform * Affine::translate((header.x0 + pad, header.y0 + 14.0)),
            None,
            TextWeight::Bold,
        );
        let total = self.workspace.connections().len();
        let online = self.workspace.online_count();
        let stat = format!("{total} saved · {online} online");
        text_renderer.draw_text(
            scene,
            &stat,
            13.0,
            tokens.colors.text_dim,
            transform * Affine::translate((header.x0 + pad, header.y0 + 40.0)),
            None,
        );

        // New-connection button (top-right).
        let add = Rect::new(
            content.x1 - pad - 150.0,
            header.y0 + 16.0,
            content.x1 - pad,
            header.y0 + 48.0,
        );
        self.draw_button(
            scene,
            transform,
            text_renderer,
            theme,
            add,
            "+ New connection",
            true,
        );
        self.push_hit(add, Action::NewConnection);

        // --- Search box ---
        let search = Rect::new(
            content.x0 + pad,
            header.y1 + 4.0,
            content.x1 - pad,
            header.y1 + 40.0,
        );
        self.draw_field(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            search,
            Field::Search,
        );
        self.push_hit(search, Action::FocusField(Field::Search));

        // --- Rows ---
        let list_top = search.y1 + 16.0;
        let mut y = list_top;
        let rows: Vec<Connection> = self
            .workspace
            .filtered(&self.search)
            .into_iter()
            .cloned()
            .collect();

        if rows.is_empty() {
            let region = Rect::new(content.x0, list_top, content.x1, content.y1);
            if self.workspace.connections().is_empty() {
                self.draw_empty_state(scene, transform, text_renderer, theme, region);
            } else {
                // A transient search miss — a quiet centred note, not the
                // full onboarding zero-state.
                let msg = "No machines match your search.";
                let (mw, _) = text_renderer.measure(msg, 14.0, None);
                text_renderer.draw_text(
                    scene,
                    msg,
                    14.0,
                    tokens.colors.text_dim,
                    transform
                        * Affine::translate((
                            region.center().x - f64::from(mw) / 2.0,
                            region.y0 + 48.0,
                        )),
                    None,
                );
            }
            return;
        }

        // Clip rows to the content area so a long list doesn't paint over
        // the header.
        let list_clip = Rect::new(content.x0, list_top, content.x1, content.y1);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &list_clip,
        );
        for c in &rows {
            let row = Rect::new(content.x0 + pad, y, content.x1 - pad, y + ROW_H);
            self.draw_connection_row(scene, transform, text_renderer, theme, row, c);
            y += ROW_H + 10.0;
        }
        scene.pop_layer();
    }

    /// The composed onboarding zero-state for an empty machine list: an
    /// emblem, a heading, an inviting subtitle, and the primary action.
    fn draw_empty_state(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        region: Rect,
    ) {
        let tokens = &theme.tokens;
        let accent = tokens.colors.accent[5];
        let mid = region.center().x;
        // Nudge the group above the exact centre so it reads as anchored.
        let mut y = region.y0 + (region.height() * 0.30).max(24.0);

        // Emblem: an accent-ringed disc with a plus glyph — "add a machine".
        let r = 36.0;
        let c = Point::new(mid, y + r);
        scene.fill(
            Fill::NonZero,
            transform,
            accent.at_alpha(0.12),
            None,
            &Circle::new(c, r),
        );
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            accent.at_alpha(0.8),
            None,
            &Circle::new(c, r),
        );
        let arm = 12.0;
        let thick = Stroke::new(2.6);
        scene.stroke(
            &thick,
            transform,
            accent,
            None,
            &vello::kurbo::Line::new(Point::new(c.x - arm, c.y), Point::new(c.x + arm, c.y)),
        );
        scene.stroke(
            &thick,
            transform,
            accent,
            None,
            &vello::kurbo::Line::new(Point::new(c.x, c.y - arm), Point::new(c.x, c.y + arm)),
        );
        y += 2.0 * r + 24.0;

        let title = "No machines yet";
        let (tw, _) = text_renderer.measure_weighted(title, 19.0, None, TextWeight::Bold);
        text_renderer.draw_text_weighted(
            scene,
            title,
            19.0,
            tokens.colors.text_primary,
            transform * Affine::translate((mid - f64::from(tw) / 2.0, y)),
            None,
            TextWeight::Bold,
        );
        y += 30.0;

        let sub = "Add one to reach it over SSH, VNC, or the qOS relay.";
        let (sw, _) = text_renderer.measure(sub, 13.5, None);
        text_renderer.draw_text(
            scene,
            sub,
            13.5,
            tokens.colors.text_dim,
            transform * Affine::translate((mid - f64::from(sw) / 2.0, y)),
            None,
        );
        y += 34.0;

        let bw = 168.0;
        let btn = Rect::new(mid - bw / 2.0, y, mid + bw / 2.0, y + 40.0);
        self.draw_button(
            scene,
            transform,
            text_renderer,
            theme,
            btn,
            "+ New connection",
            true,
        );
        self.push_hit(btn, Action::NewConnection);
    }

    /// Draw one connection row: status dot, name, address, kind badge,
    /// last-used, favorite star, edit + connect buttons.
    fn draw_connection_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        c: &Connection,
    ) {
        self.draw_connection_row_header(scene, transform, text_renderer, theme, row, c);
        self.draw_connection_row_meta_and_actions(scene, transform, text_renderer, theme, row, c);
    }

    /// `draw_connection_row`'s background/border/status dot and the first
    /// line (name + favorite star).
    fn draw_connection_row_header(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        c: &Connection,
    ) {
        let tokens = &theme.tokens;
        let selected = self.selected == Some(c.id);
        let hovered = row.contains(self.pointer);
        let rr = RoundedRect::from_rect(row, 12.0);
        let bg = if selected {
            tokens.colors.accent[5].at_alpha(0.14)
        } else if hovered {
            tokens.colors.surfaces.raised
        } else {
            tokens.colors.neutral[1]
        };
        scene.fill(Fill::NonZero, transform, bg, None, &rr);
        let border = if selected {
            tokens.colors.accent[5]
        } else {
            tokens.colors.border_standard
        };
        scene.stroke(
            &Stroke::new(if selected { 1.5 } else { 1.0 }),
            transform,
            border,
            None,
            &rr,
        );

        // Clicking the body (not a button) selects the row.
        self.push_hit(row, Action::SelectRow(c.id));

        // Status dot.
        let dot_c = match c.presence {
            Presence::Online => tokens.colors.semantic.success,
            Presence::Offline => tokens.colors.semantic.danger,
            Presence::Unknown => tokens.colors.neutral[4],
        };
        let dot = Point::new(row.x0 + 22.0, row.center().y);
        scene.fill(
            Fill::NonZero,
            transform,
            dot_c,
            None,
            &Circle::new(dot, 5.0),
        );
        if c.presence == Presence::Online {
            // Soft halo.
            scene.fill(
                Fill::NonZero,
                transform,
                dot_c.at_alpha(0.25),
                None,
                &Circle::new(dot, 9.0),
            );
        }

        // --- First line: name + favorite star ---
        let name_x = row.x0 + 44.0;
        text_renderer.draw_text_weighted(
            scene,
            &c.name,
            16.0,
            tokens.colors.text_primary,
            transform * Affine::translate((name_x, row.y0 + 13.0)),
            None,
            TextWeight::Semibold,
        );
        let (name_w, _) = text_renderer.measure_weighted(&c.name, 16.0, None, TextWeight::Semibold);
        let star_center = Point::new(name_x + f64::from(name_w) + 16.0, row.y0 + 21.0);
        let star_color = if c.favorite {
            tokens.colors.semantic.warning
        } else {
            tokens.colors.neutral[4]
        };
        draw_star(scene, transform, star_center, 7.0, star_color, c.favorite);
        let star_rect = Rect::new(
            star_center.x - 11.0,
            star_center.y - 11.0,
            star_center.x + 11.0,
            star_center.y + 11.0,
        );
        self.push_hit(star_rect, Action::ToggleFavorite(c.id));
    }

    /// `draw_connection_row`'s second line (kind badge · address · group)
    /// and its right-aligned action buttons + last-used stamp.
    fn draw_connection_row_meta_and_actions(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        c: &Connection,
    ) {
        let tokens = &theme.tokens;
        let name_x = row.x0 + 44.0;
        // --- Second line: kind badge · address · group ---
        let line2_y = row.y0 + 38.0;
        // Kind badge first (leftmost), vertically centered on line2.
        let badge_w = Self::draw_kind_badge_at(
            scene,
            transform,
            text_renderer,
            theme,
            Point::new(name_x, line2_y),
            c.kind,
        );
        let mut x = name_x + badge_w + 12.0;
        // Address.
        text_renderer.draw_text(
            scene,
            &c.address(),
            13.0,
            tokens.colors.text_dim,
            transform * Affine::translate((x, line2_y)),
            None,
        );
        let (aw, _) = text_renderer.measure(&c.address(), 13.0, None);
        x += f64::from(aw) + 14.0;
        // Group chip.
        if !c.group.is_empty() {
            let g = format!("· {}", c.group);
            text_renderer.draw_text(
                scene,
                &g,
                12.0,
                tokens.colors.neutral[5],
                transform * Affine::translate((x, line2_y + 1.0)),
                None,
            );
        }

        // --- Right-aligned action buttons ---
        let connect = Rect::new(
            row.x1 - 116.0,
            row.center().y - 16.0,
            row.x1 - 16.0,
            row.center().y + 16.0,
        );
        if c.kind.is_supported() {
            self.draw_button(
                scene,
                transform,
                text_renderer,
                theme,
                connect,
                "Connect",
                true,
            );
            self.push_hit(connect, Action::Connect(c.id));
        } else {
            Self::draw_unavailable_button(scene, transform, text_renderer, theme, connect);
        }

        let edit = Rect::new(connect.x0 - 86.0, connect.y0, connect.x0 - 10.0, connect.y1);
        self.draw_button(scene, transform, text_renderer, theme, edit, "Edit", false);
        self.push_hit(edit, Action::EditConnection(c.id));

        // last_used, right-aligned just left of the Edit button.
        let (uw, _) = text_renderer.measure(&c.last_used, 12.0, None);
        text_renderer.draw_text(
            scene,
            &c.last_used,
            12.0,
            tokens.colors.neutral[5],
            transform * Affine::translate((edit.x0 - 20.0 - f64::from(uw), row.center().y - 7.0)),
            None,
        );
    }

    /// Draw a kind badge whose top-left sits at `top_left`; returns its
    /// drawn width so the caller can lay subsequent text out beside it.
    fn draw_kind_badge_at(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        top_left: Point,
        kind: ConnectionKind,
    ) -> f64 {
        let (label, accent) = (kind.tag(), kind_color(kind, theme));
        let size = 11.0f32;
        let (tw, _) = text_renderer.measure(label, size, None);
        let w = f64::from(tw) + 16.0;
        let h = 18.0;
        let badge = Rect::new(top_left.x, top_left.y, top_left.x + w, top_left.y + h);
        scene.fill(
            Fill::NonZero,
            transform,
            accent.at_alpha(0.18),
            None,
            &RoundedRect::from_rect(badge, h / 2.0),
        );
        text_renderer.draw_text_weighted(
            scene,
            label,
            size,
            accent,
            transform
                * Affine::translate((badge.x0 + 8.0, badge.y0 + (h - f64::from(size)) / 2.0 - 1.0)),
            None,
            TextWeight::Semibold,
        );
        w
    }

    /// Generic button using the shared immediate-mode face while this canvas
    /// retains ownership of pointer routing.
    #[allow(clippy::too_many_arguments)]
    fn draw_button(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        primary: bool,
    ) {
        let hovered = rect.contains(self.pointer);
        let face = ButtonFace::new(rect, label)
            .hovered(hovered)
            .text_size(13.0)
            .weight(TextWeight::Semibold);
        let face = if primary { face.primary() } else { face };
        Button::paint(scene, transform, text_renderer, theme, &face);
    }

    /// A non-interactive status, deliberately not styled like a secondary
    /// action. It makes old profiles truthful without turning them into a
    /// dead "Connect" button.
    fn draw_unavailable_button(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        let rr = RoundedRect::from_rect(rect, 8.0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.neutral[2],
            None,
            &rr,
        );
        let label = "Unavailable";
        let (width, _) = text_renderer.measure_weighted(label, 12.0, None, TextWeight::Semibold);
        text_renderer.draw_text_weighted(
            scene,
            label,
            12.0,
            theme.tokens.colors.text_dim,
            transform
                * Affine::translate((
                    rect.center().x - f64::from(width) / 2.0,
                    rect.center().y - 7.0,
                )),
            None,
            TextWeight::Semibold,
        );
    }

    /// The text band inside a field box — where the shared input lives, and
    /// the rect a click is mapped through. One function so drawing and
    /// hit-testing cannot drift.
    fn field_inner(rect: Rect) -> Rect {
        Rect::new(rect.x0 + 12.0, rect.y0, rect.x1 - 12.0, rect.y1)
    }

    /// Draw a text field: the app paints the box and the focus ring, the
    /// shared [`TextInput`] paints the text, placeholder, selection and caret
    /// inside it — and owns every editing behaviour that goes with them.
    #[allow(clippy::too_many_arguments)]
    fn draw_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        rect: Rect,
        field: Field,
    ) {
        let tokens = &theme.tokens;
        let focused = self.focus == Some(field);
        let rr = RoundedRect::from_rect(rect, 8.0);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.neutral[0],
            None,
            &rr,
        );
        let border = if focused {
            tokens.colors.border_focus
        } else {
            tokens.colors.border_standard
        };
        scene.stroke(
            &Stroke::new(if focused { 2.0 } else { 1.0 }),
            transform,
            border,
            None,
            &rr,
        );

        let inner = Self::field_inner(rect);
        let input = self.inputs.get(field);
        input.apply_layout_external(inner);
        Widget::draw(input, scene, transform, text_renderer, theme, presence);
    }

    /// Draw the add/edit form. Split into one helper per card section — each
    /// threads the running layout cursor `y` through and hands back where the
    /// next section starts, exactly as the inline version did.
    #[allow(
        clippy::too_many_arguments,
        reason = "matches ui's draw dispatch shape (scene/transform/text_renderer/theme) \
                  plus this screen's own presence/content/editing state"
    )]
    fn draw_form(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        content: Rect,
        editing: Option<ConnectionId>,
    ) {
        let pad = 24.0;
        Self::draw_form_header(
            scene,
            transform,
            text_renderer,
            theme,
            content,
            pad,
            editing,
        );

        // The form is a centered card.
        let card_w = (content.width() - 2.0 * pad).min(560.0);
        let card_x = content.x0 + (content.width() - card_w) / 2.0;
        let y = content.y0 + HEADER_H + 8.0;

        let y =
            self.draw_form_kind_selector(scene, transform, text_renderer, theme, card_x, card_w, y);
        let y =
            self.draw_form_qos_picker(scene, transform, text_renderer, theme, card_x, card_w, y);
        let y = self.draw_form_name_field(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            card_x,
            card_w,
            y,
        );
        let y = self.draw_form_host_port_row(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            card_x,
            card_w,
            y,
        );
        let y = self.draw_form_user_auth_row(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            card_x,
            card_w,
            y,
        );
        self.draw_form_footer(
            scene,
            transform,
            text_renderer,
            theme,
            card_x,
            card_w,
            y,
            editing,
        );
    }

    /// `draw_form`'s title, centered above the card.
    fn draw_form_header(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        pad: f64,
        editing: Option<ConnectionId>,
    ) {
        let title = if editing.is_some() {
            "Edit connection"
        } else {
            "New connection"
        };
        text_renderer.draw_text_weighted(
            scene,
            title,
            22.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((content.x0 + pad, content.y0 + 18.0)),
            None,
            TextWeight::Bold,
        );
    }

    /// `draw_form`'s protocol kind selector (segmented control).
    #[allow(
        clippy::too_many_arguments,
        reason = "one draw_form section threading the shared card geometry \
                  (x/width) and running layout cursor `y` through the shaped \
                  protocol-kind row"
    )]
    fn draw_form_kind_selector(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        card_x: f64,
        card_w: f64,
        y: f64,
    ) -> f64 {
        let mut y = Self::draw_form_label(
            scene,
            transform,
            text_renderer,
            theme,
            card_x,
            y,
            "Protocol",
        );
        let kinds = ConnectionKind::available_to_create();
        let seg_w = card_w / kinds.len() as f64;
        for (i, k) in kinds.into_iter().enumerate() {
            let seg = Rect::new(
                card_x + i as f64 * seg_w,
                y,
                card_x + (i + 1) as f64 * seg_w,
                y + 40.0,
            );
            let active = self.draft.kind == k;
            self.draw_segment(
                scene,
                transform,
                text_renderer,
                theme,
                seg,
                k.tag(),
                active,
                i == 0,
                i.saturating_add(1) == kinds.len(),
            );
            self.push_hit(seg, Action::SetKind(k));
        }
        y += 56.0;
        y
    }

    /// `draw_form`'s qOS device picker — a no-op (returns `y` unchanged) for
    /// any other protocol kind.
    #[allow(
        clippy::too_many_arguments,
        reason = "one draw_form section threading the shared card geometry \
                  (x/width) and running layout cursor `y` through the \
                  discovered-devices list"
    )]
    fn draw_form_qos_picker(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        card_x: f64,
        card_w: f64,
        y: f64,
    ) -> f64 {
        let tokens = &theme.tokens;
        let mut y = y;
        if self.draft.kind == ConnectionKind::Qos {
            y = Self::draw_form_label(
                scene,
                transform,
                text_renderer,
                theme,
                card_x,
                y,
                "Discovered qOS devices",
            );
            for (i, d) in self.qos_devices.clone().into_iter().enumerate() {
                let chip = Rect::new(card_x, y, card_x + card_w, y + 36.0);
                let hovered = chip.contains(self.pointer);
                let rr = RoundedRect::from_rect(chip, 8.0);
                let picked = self.draft.host == d.node;
                let bg = if picked {
                    tokens.colors.accent[5].at_alpha(0.16)
                } else if hovered {
                    tokens.colors.surface_white_10
                } else {
                    tokens.colors.neutral[0]
                };
                scene.fill(Fill::NonZero, transform, bg, None, &rr);
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    if picked {
                        tokens.colors.accent[5]
                    } else {
                        tokens.colors.border_standard
                    },
                    None,
                    &rr,
                );
                let dot_c = if d.online {
                    tokens.colors.semantic.success
                } else {
                    tokens.colors.neutral[4]
                };
                scene.fill(
                    Fill::NonZero,
                    transform,
                    dot_c,
                    None,
                    &Circle::new(Point::new(chip.x0 + 16.0, chip.center().y), 4.0),
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &d.name,
                    13.0,
                    tokens.colors.text_primary,
                    transform * Affine::translate((chip.x0 + 30.0, chip.center().y - 8.0)),
                    None,
                    TextWeight::Medium,
                );
                text_renderer.draw_text(
                    scene,
                    &d.node,
                    12.0,
                    tokens.colors.text_dim,
                    transform * Affine::translate((chip.x1 - 160.0, chip.center().y - 7.0)),
                    None,
                );
                self.push_hit(chip, Action::PickQosDevice(i));
                y += 44.0;
            }
            y += 4.0;
        }
        y
    }

    /// `draw_form`'s Name field.
    #[allow(
        clippy::too_many_arguments,
        reason = "one draw_form section threading the shared card geometry \
                  (x/width) and running layout cursor `y` through the Name field"
    )]
    fn draw_form_name_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        card_x: f64,
        card_w: f64,
        y: f64,
    ) -> f64 {
        let mut y =
            Self::draw_form_label(scene, transform, text_renderer, theme, card_x, y, "Name");
        let name = Rect::new(card_x, y, card_x + card_w, y + 40.0);
        self.draw_field(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            name,
            Field::Name,
        );
        self.push_hit(name, Action::FocusField(Field::Name));
        y += 56.0;
        y
    }

    /// `draw_form`'s Host + Port row (Port hidden for qOS).
    #[allow(
        clippy::too_many_arguments,
        reason = "one draw_form section threading the shared card geometry \
                  (x/width) and running layout cursor `y` through the Host/Port row"
    )]
    fn draw_form_host_port_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        card_x: f64,
        card_w: f64,
        y: f64,
    ) -> f64 {
        let host_label = if self.draft.kind == ConnectionKind::Qos {
            "Device node"
        } else {
            "Host"
        };
        let mut y = Self::draw_form_label(
            scene,
            transform,
            text_renderer,
            theme,
            card_x,
            y,
            host_label,
        );
        let qos = self.draft.kind == ConnectionKind::Qos;
        let host_w = if qos { card_w } else { card_w - 130.0 };
        let host = Rect::new(card_x, y, card_x + host_w, y + 40.0);
        self.draw_field(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            host,
            Field::Host,
        );
        self.push_hit(host, Action::FocusField(Field::Host));
        if !qos {
            let port = Rect::new(card_x + host_w + 12.0, y, card_x + card_w, y + 40.0);
            self.draw_field(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                port,
                Field::Port,
            );
            self.push_hit(port, Action::FocusField(Field::Port));
        }
        y += 56.0;
        y
    }

    /// `draw_form`'s User + Group row for non-qOS kinds, or the implicit-auth
    /// info card for qOS.
    #[allow(
        clippy::too_many_arguments,
        reason = "one draw_form section threading the shared card geometry \
                  (x/width) and running layout cursor `y` through the User/Group \
                  row and auth method selector"
    )]
    fn draw_form_user_auth_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        card_x: f64,
        card_w: f64,
        y: f64,
    ) -> f64 {
        let tokens = &theme.tokens;
        let qos = self.draft.kind == ConnectionKind::Qos;
        let mut y = y;
        if qos {
            // qOS: authentication is implicit via device identity.
            y = Self::draw_form_label(
                scene,
                transform,
                text_renderer,
                theme,
                card_x,
                y,
                "Authentication",
            );
            let info = Rect::new(card_x, y, card_x + card_w, y + 40.0);
            scene.fill(
                Fill::NonZero,
                transform,
                tokens.colors.accent[5].at_alpha(0.10),
                None,
                &RoundedRect::from_rect(info, 8.0),
            );
            text_renderer.draw_text(
                scene,
                "Authenticated by qOS device identity over the P2P fabric — no password needed.",
                12.5,
                tokens.colors.text_dim,
                transform * Affine::translate((info.x0 + 12.0, info.center().y - 7.0)),
                Some(card_w as f32 - 24.0),
            );
            y += 56.0;
        } else {
            y = Self::draw_form_label(scene, transform, text_renderer, theme, card_x, y, "User");
            let half = (card_w - 12.0) / 2.0;
            let user = Rect::new(card_x, y, card_x + half, y + 40.0);
            self.draw_field(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                user,
                Field::User,
            );
            self.push_hit(user, Action::FocusField(Field::User));
            let group = Rect::new(card_x + half + 12.0, y, card_x + card_w, y + 40.0);
            self.draw_field(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                group,
                Field::Group,
            );
            self.push_hit(group, Action::FocusField(Field::Group));
            y += 56.0;

            // -- Auth method selector --
            y = Self::draw_form_label(
                scene,
                transform,
                text_renderer,
                theme,
                card_x,
                y,
                "Authentication",
            );
            let aseg = card_w / 3.0;
            for (i, a) in AuthMethod::all().into_iter().enumerate() {
                let seg = Rect::new(
                    card_x + i as f64 * aseg,
                    y,
                    card_x + (i + 1) as f64 * aseg,
                    y + 40.0,
                );
                let active = self.draft.auth == a;
                self.draw_segment(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    seg,
                    a.label(),
                    active,
                    i == 0,
                    i == 2,
                );
                self.push_hit(seg, Action::SetAuth(a));
            }
            y += 56.0;
        }
        y
    }

    /// `draw_form`'s tail: validation messages, Save + Cancel, and (edit mode
    /// only) the Delete button.
    #[allow(
        clippy::too_many_arguments,
        reason = "one draw_form section threading the shared card geometry \
                  (x/width), the settled layout cursor `y`, and the editing id \
                  through the closing validation/action row"
    )]
    fn draw_form_footer(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        card_x: f64,
        card_w: f64,
        y: f64,
        editing: Option<ConnectionId>,
    ) {
        let tokens = &theme.tokens;

        // -- Validation messages --
        let problems = self.draft.problems();
        if !problems.is_empty() {
            let msg = problems.join(" · ");
            text_renderer.draw_text(
                scene,
                &msg,
                12.5,
                tokens.colors.semantic.danger,
                transform * Affine::translate((card_x, y)),
                Some(card_w as f32),
            );
        }
        let y = y + 28.0;

        // -- Save + Cancel --
        let save = Rect::new(card_x + card_w - 120.0, y, card_x + card_w, y + 40.0);
        let can_save = problems.is_empty();
        self.draw_button(
            scene,
            transform,
            text_renderer,
            theme,
            save,
            "Save",
            can_save,
        );
        if can_save {
            self.push_hit(save, Action::SaveForm);
        }
        let cancel = Rect::new(save.x0 - 110.0, y, save.x0 - 12.0, y + 40.0);
        self.draw_button(
            scene,
            transform,
            text_renderer,
            theme,
            cancel,
            "Cancel",
            false,
        );
        self.push_hit(cancel, Action::Back);

        // Delete (edit mode only), bottom-left.
        if let Some(id) = editing {
            let del = Rect::new(card_x, y, card_x + 120.0, y + 40.0);
            let hovered = del.contains(self.pointer);
            let rr = RoundedRect::from_rect(del, 8.0);
            if hovered {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    tokens.colors.semantic.danger.at_alpha(0.15),
                    None,
                    &rr,
                );
            }
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                tokens.colors.semantic.danger,
                None,
                &rr,
            );
            let (tw, _) =
                text_renderer.measure_weighted("Delete", 13.0, None, TextWeight::Semibold);
            text_renderer.draw_text_weighted(
                scene,
                "Delete",
                13.0,
                tokens.colors.semantic.danger,
                transform
                    * Affine::translate((
                        del.center().x - f64::from(tw) / 2.0,
                        del.center().y - 8.0,
                    )),
                None,
                TextWeight::Semibold,
            );
            self.push_hit(del, Action::Delete(id));
        }
    }

    /// Draw a small section label and return the y just below it.
    fn draw_form_label(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        label: &str,
    ) -> f64 {
        text_renderer.draw_text_weighted(
            scene,
            label,
            12.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((x, y)),
            None,
            TextWeight::Semibold,
        );
        y + 22.0
    }

    /// Draw one segment of a segmented control.
    #[allow(clippy::too_many_arguments)]
    fn draw_segment(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        active: bool,
        _first: bool,
        _last: bool,
    ) {
        let tokens = &theme.tokens;
        let hovered = rect.contains(self.pointer);
        let rr = RoundedRect::from_rect(rect.inset(-0.0), 8.0);
        let bg = if active {
            tokens.colors.accent[5]
        } else if hovered {
            tokens.colors.surface_white_10
        } else {
            tokens.colors.neutral[0]
        };
        scene.fill(Fill::NonZero, transform, bg, None, &rr);
        if !active {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                tokens.colors.border_standard,
                None,
                &rr,
            );
        }
        let color = if active {
            Color::WHITE
        } else {
            tokens.colors.text_primary
        };
        let (tw, _) = text_renderer.measure_weighted(label, 13.0, None, TextWeight::Medium);
        text_renderer.draw_text_weighted(
            scene,
            label,
            13.0,
            color,
            transform
                * Affine::translate((rect.center().x - f64::from(tw) / 2.0, rect.center().y - 8.0)),
            None,
            TextWeight::Medium,
        );
    }

    /// Draw the session surface for a connection.
    #[allow(clippy::too_many_arguments)]
    fn draw_session(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        id: ConnectionId,
        immersive: bool,
    ) {
        let tokens = &theme.tokens;
        let Some(c) = self.workspace.get(id).cloned() else {
            // Connection vanished — bounce home.
            text_renderer.draw_text(
                scene,
                "Connection no longer exists.",
                14.0,
                tokens.colors.text_dim,
                transform * Affine::translate((content.x0 + 24.0, content.y0 + 24.0)),
                None,
            );
            return;
        };

        // No live transport object: the honest "not connected" card. The
        // env gate is enforced at `open_session` (it only creates a live
        // session when REMOTE_LIVE_SESSIONS is set), so a `None` here means
        // either the default gated build or a not-yet-opened session — never
        // a fabricated terminal or desktop.
        let Some(status) = self.live.as_ref().map(LiveSession::status) else {
            self.draw_status_session(
                scene,
                transform,
                text_renderer,
                theme,
                content,
                &c,
                "Not connected",
                &[
                    "Live remote sessions are off in this build.".to_string(),
                    "Set REMOTE_LIVE_SESSIONS to connect for real —".to_string(),
                    "nothing here is simulated.".to_string(),
                ],
                StatusTone::Neutral,
            );
            return;
        };

        // Live path: reflect the real transport state.
        match status {
            SessionStatus::Connecting => {
                self.draw_status_session(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    content,
                    &c,
                    "Connecting…",
                    &["Negotiating the transport and authenticating…".to_string()],
                    StatusTone::Pending,
                );
            }
            SessionStatus::Failed(err) => {
                self.draw_status_session(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    content,
                    &c,
                    "Connection failed",
                    &[
                        err,
                        "This is the real error — nothing is simulated.".to_string(),
                    ],
                    StatusTone::Error,
                );
            }
            SessionStatus::Closed => {
                self.draw_status_session(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    content,
                    &c,
                    "Session ended",
                    &["The remote closed the session.".to_string()],
                    StatusTone::Neutral,
                );
            }
            SessionStatus::Connected => {
                // Real surface. Toolbar hidden in immersive/fullscreen mode.
                let surface = if immersive {
                    content
                } else {
                    self.draw_session_toolbar(scene, transform, text_renderer, theme, content, &c);
                    Rect::new(content.x0, content.y0 + TOOLBAR_H, content.x1, content.y1)
                };
                let is_ssh = matches!(self.live, Some(LiveSession::Ssh(_)));
                if is_ssh {
                    self.draw_terminal(scene, transform, text_renderer, theme, surface, &c);
                } else {
                    self.draw_framebuffer(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        surface,
                        immersive,
                    );
                }
            }
        }
    }

    /// A session surface that is *not* a live pixel/terminal view: the
    /// honest states (not connected, connecting, failed, ended), composed as
    /// a centred card — a state emblem carrying the protocol, the machine
    /// identity, a status pill, an explanation, and real actions. The state
    /// has a visible form (the emblem ring + pill colour), not just a label.
    /// Never draws a fabricated terminal or desktop.
    #[allow(clippy::too_many_arguments)]
    fn draw_status_session(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        c: &Connection,
        title: &str,
        body: &[String],
        tone: StatusTone,
    ) {
        let tokens = &theme.tokens;
        let accent = match tone {
            StatusTone::Neutral => tokens.colors.neutral[4],
            StatusTone::Pending => tokens.colors.semantic.warning,
            StatusTone::Error => tokens.colors.semantic.danger,
        };
        let card = Self::draw_status_session_card(scene, transform, theme, content, body);
        let y = Self::draw_status_session_identity(
            scene,
            transform,
            text_renderer,
            theme,
            card,
            accent,
            c,
        );
        let y = Self::draw_status_session_pill_and_body(
            scene,
            transform,
            text_renderer,
            theme,
            card,
            accent,
            title,
            body,
            y,
        );
        self.draw_status_session_actions(scene, transform, text_renderer, theme, card, tone, c, y);
    }

    /// `draw_status_session`'s full-surface backdrop and centred card
    /// background/border, sized to fit `body`. Returns the card's [`Rect`].
    fn draw_status_session_card(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        content: Rect,
        body: &[String],
    ) -> Rect {
        let tokens = &theme.tokens;

        // Full-surface backdrop.
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &content,
        );

        // Card geometry, sized to its content so it stays balanced.
        let cw = 460.0_f64.min(content.width() - 40.0);
        let ch = 336.0 + body.len() as f64 * 22.0;
        let cx = content.x0 + (content.width() - cw) / 2.0;
        let cy = content.y0 + ((content.height() - ch) / 2.0).max(16.0);
        let card = Rect::new(cx, cy, cx + cw, cy + ch);
        let rr = RoundedRect::from_rect(card, 16.0);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &rr,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.border_white_20,
            None,
            &rr,
        );
        card
    }

    /// `draw_status_session`'s state emblem, machine name, and address line.
    /// Returns the `y` the status pill should start at.
    #[allow(
        clippy::too_many_arguments,
        reason = "one draw_status_session section threading the card geometry \
                  and tone accent through the emblem + identity lines"
    )]
    fn draw_status_session_identity(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        accent: Color,
        c: &Connection,
    ) -> f64 {
        let tokens = &theme.tokens;
        let mid = card.center().x;
        let mut y = card.y0 + 34.0;

        // State emblem: a tone-ringed disc carrying the protocol tag — state
        // (ring colour) and identity (protocol) in one visible form.
        let r = 34.0;
        let emblem = Point::new(mid, y + r);
        scene.fill(
            Fill::NonZero,
            transform,
            accent.at_alpha(0.14),
            None,
            &Circle::new(emblem, r),
        );
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            accent.at_alpha(0.9),
            None,
            &Circle::new(emblem, r),
        );
        let tag = c.kind.tag();
        let (tagw, _) = text_renderer.measure_weighted(tag, 15.0, None, TextWeight::Bold);
        text_renderer.draw_text_weighted(
            scene,
            tag,
            15.0,
            tokens.colors.text_primary,
            transform * Affine::translate((mid - f64::from(tagw) / 2.0, emblem.y - 10.0)),
            None,
            TextWeight::Bold,
        );
        y += 2.0 * r + 22.0;

        // Machine name — the title of the card.
        let (nw, _) = text_renderer.measure_weighted(&c.name, 20.0, None, TextWeight::Bold);
        text_renderer.draw_text_weighted(
            scene,
            &c.name,
            20.0,
            tokens.colors.text_primary,
            transform * Affine::translate((mid - f64::from(nw) / 2.0, y)),
            None,
            TextWeight::Bold,
        );
        y += 30.0;

        // Address + protocol, one dim caption line.
        let addr = format!("{} · {}", c.address(), c.kind.label());
        let (aw, _) = text_renderer.measure(&addr, 13.0, None);
        text_renderer.draw_text(
            scene,
            &addr,
            13.0,
            tokens.colors.text_dim,
            transform * Affine::translate((mid - f64::from(aw) / 2.0, y)),
            None,
        );
        y += 30.0;
        y
    }

    /// `draw_status_session`'s status pill and explanation body. Returns the
    /// `y` the action row should start at.
    #[allow(
        clippy::too_many_arguments,
        reason = "one draw_status_session section threading the card geometry, \
                  tone accent, and running layout cursor `y` through the pill \
                  and body lines"
    )]
    fn draw_status_session_pill_and_body(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        accent: Color,
        title: &str,
        body: &[String],
        y: f64,
    ) -> f64 {
        let tokens = &theme.tokens;
        let mid = card.center().x;
        let mut y = y;

        // Status pill — the state as a labelled, tone-coloured form.
        let (pw, _) = text_renderer.measure_weighted(title, 13.0, None, TextWeight::Semibold);
        let pill_w = f64::from(pw) + 44.0;
        let pill = Rect::new(mid - pill_w / 2.0, y, mid + pill_w / 2.0, y + 30.0);
        scene.fill(
            Fill::NonZero,
            transform,
            accent.at_alpha(0.16),
            None,
            &RoundedRect::from_rect(pill, 15.0),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            accent,
            None,
            &Circle::new(Point::new(pill.x0 + 16.0, pill.center().y), 4.0),
        );
        text_renderer.draw_text_weighted(
            scene,
            title,
            13.0,
            tokens.colors.text_primary,
            transform * Affine::translate((pill.x0 + 28.0, pill.center().y - 8.0)),
            None,
            TextWeight::Semibold,
        );
        y += 46.0;

        // Explanation body.
        for line in body {
            let (lw, _) = text_renderer.measure(line, 13.0, None);
            text_renderer.draw_text(
                scene,
                line,
                13.0,
                tokens.colors.text_dim,
                transform * Affine::translate((mid - f64::from(lw) / 2.0, y)),
                None,
            );
            y += 22.0;
        }
        y += 14.0;
        y
    }

    /// `draw_status_session`'s action row: a primary "Back to list", plus
    /// "Retry" where the tone is an actual failure.
    #[allow(
        clippy::too_many_arguments,
        reason = "one draw_status_session section threading the card geometry, \
                  tone, and settled layout cursor `y` through the action row"
    )]
    fn draw_status_session_actions(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        tone: StatusTone,
        c: &Connection,
        y: f64,
    ) {
        let mid = card.center().x;
        // Retry only makes sense after a real failure; the gated/ended states
        // just return to the list.
        let can_retry = matches!(tone, StatusTone::Error);
        let btn_h = 38.0;
        let back_w = 132.0;
        let retry_w = 104.0;
        let gap = 12.0;
        let group_w = if can_retry {
            back_w + gap + retry_w
        } else {
            back_w
        };
        let mut bx = mid - group_w / 2.0;
        if can_retry {
            let retry = Rect::new(bx, y, bx + retry_w, y + btn_h);
            self.draw_button(
                scene,
                transform,
                text_renderer,
                theme,
                retry,
                "Retry",
                false,
            );
            self.push_hit(retry, Action::Connect(c.id));
            bx += retry_w + gap;
        }
        let back = Rect::new(bx, y, bx + back_w, y + btn_h);
        self.draw_button(
            scene,
            transform,
            text_renderer,
            theme,
            back,
            "Back to list",
            true,
        );
        self.push_hit(back, Action::Disconnect);
    }

    /// Draw the live VNC framebuffer, scaled to fit (letterboxed) or 1:1.
    fn draw_framebuffer(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        surface: Rect,
        immersive: bool,
    ) {
        let tokens = &theme.tokens;
        // Letterbox backdrop.
        scene.fill(Fill::NonZero, transform, Color::BLACK, None, &surface);

        let Some(img) = self.fb_image.clone() else {
            // Connected, but no framebuffer decoded yet.
            text_renderer.draw_text(
                scene,
                "Connected — waiting for the first framebuffer update…",
                13.0,
                tokens.colors.text_dim,
                transform * Affine::translate((surface.x0 + 24.0, surface.y0 + 24.0)),
                None,
            );
            return;
        };

        let iw = f64::from(img.width.max(1));
        let ih = f64::from(img.height.max(1));
        let canvas = if self.scale_to_fit {
            let scale = (surface.width() / iw).min(surface.height() / ih);
            let (w, h) = (iw * scale, ih * scale);
            let (cx, cy) = (surface.center().x, surface.center().y);
            Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
        } else {
            let w = iw.min(surface.width());
            let h = ih.min(surface.height());
            let (cx, cy) = (surface.center().x, surface.center().y);
            Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
        };
        self.fb_canvas = Some(canvas);

        let sx = canvas.width() / iw;
        let sy = canvas.height() / ih;
        let brush_transform =
            Affine::translate((canvas.x0, canvas.y0)) * Affine::scale_non_uniform(sx, sy);
        let brush = ImageBrush::new(img);
        scene.fill(
            Fill::NonZero,
            transform,
            &brush,
            Some(brush_transform),
            &canvas,
        );

        if immersive {
            text_renderer.draw_text(
                scene,
                "Esc to exit fullscreen",
                12.0,
                tokens.colors.text_dim,
                transform * Affine::translate((surface.x1 - 170.0, surface.y0 + 12.0)),
                None,
            );
        }
    }

    /// Draw the session toolbar and return the surface area below it. Also
    /// registers the toolbar's clickable controls.
    fn draw_session_toolbar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        c: &Connection,
    ) {
        let tokens = &theme.tokens;
        let bar = Rect::new(content.x0, content.y0, content.x1, content.y0 + TOOLBAR_H);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.neutral[1],
            None,
            &bar,
        );
        let seam = Rect::new(bar.x0, bar.y1 - 1.0, bar.x1, bar.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.border_standard,
            None,
            &seam,
        );

        // Live status dot + name + address.
        let dot = Point::new(bar.x0 + 18.0, bar.center().y);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.semantic.success,
            None,
            &Circle::new(dot, 5.0),
        );
        text_renderer.draw_text_weighted(
            scene,
            &c.name,
            14.0,
            tokens.colors.text_primary,
            transform * Affine::translate((bar.x0 + 32.0, bar.center().y - 9.0)),
            None,
            TextWeight::Semibold,
        );
        let (nw, _) = text_renderer.measure_weighted(&c.name, 14.0, None, TextWeight::Semibold);
        // Kind chip + connection label after the name.
        let mode = if c.kind == ConnectionKind::Qos {
            format!("· {} · local render", c.address())
        } else if c.kind.is_terminal() {
            format!("· {} · shell", c.address())
        } else {
            format!("· {} · framebuffer", c.address())
        };
        text_renderer.draw_text(
            scene,
            &mode,
            12.0,
            tokens.colors.text_dim,
            transform * Affine::translate((bar.x0 + 40.0 + f64::from(nw), bar.center().y - 7.0)),
            None,
        );

        // Right-aligned tool buttons. Only the relevant ones per kind.
        let mut x = bar.x1 - 12.0;
        let disconnect = Rect::new(x - 110.0, bar.center().y - 16.0, x, bar.center().y + 16.0);
        self.draw_button(
            scene,
            transform,
            text_renderer,
            theme,
            disconnect,
            "Disconnect",
            false,
        );
        // Override the outlined style with a danger tint for disconnect.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.semantic.danger,
            None,
            &RoundedRect::from_rect(disconnect, 8.0),
        );
        self.push_hit(disconnect, Action::Disconnect);
        x = disconnect.x0 - 10.0;

        // Framebuffer-only controls: fullscreen, scale, clipboard.
        if c.kind.streams_framebuffer() || c.kind == ConnectionKind::Qos {
            let fs = self.draw_tool(scene, transform, text_renderer, theme, x, bar, "Fullscreen");
            self.push_hit(fs, Action::ToggleFullscreen);
            x = fs.x0 - 8.0;

            let scale_label = if self.scale_to_fit {
                "Scale: Fit"
            } else {
                "Scale: 1:1"
            };
            let sc = self.draw_tool(scene, transform, text_renderer, theme, x, bar, scale_label);
            self.push_hit(sc, Action::ToggleScale);
            x = sc.x0 - 8.0;

            let clip = self.draw_tool(scene, transform, text_renderer, theme, x, bar, "Clipboard");
            self.push_hit(clip, Action::CopyClipboard);
        } else {
            // Terminal: clipboard only.
            let clip = self.draw_tool(scene, transform, text_renderer, theme, x, bar, "Copy");
            self.push_hit(clip, Action::CopyClipboard);
        }
    }

    /// Draw a compact toolbar tool button ending at `right_x`; returns its rect.
    #[allow(clippy::too_many_arguments)]
    fn draw_tool(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        right_x: f64,
        bar: Rect,
        label: &str,
    ) -> Rect {
        let tokens = &theme.tokens;
        let (tw, _) = text_renderer.measure(label, 12.5, None);
        let w = f64::from(tw) + 24.0;
        let rect = Rect::new(
            right_x - w,
            bar.center().y - 16.0,
            right_x,
            bar.center().y + 16.0,
        );
        let hovered = rect.contains(self.pointer);
        let rr = RoundedRect::from_rect(rect, 8.0);
        let bg = if hovered {
            tokens.colors.surface_white_10
        } else {
            tokens.colors.neutral[0]
        };
        scene.fill(Fill::NonZero, transform, bg, None, &rr);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.border_standard,
            None,
            &rr,
        );
        text_renderer.draw_text(
            scene,
            label,
            12.5,
            tokens.colors.text_primary,
            transform * Affine::translate((rect.x0 + 12.0, rect.center().y - 8.0)),
            None,
        );
        rect
    }

    /// Draw the terminal grid for an SSH session.
    /// Draw the live SSH terminal grid, straight from the real PTY output
    /// held in the [`crate::vt::VtGrid`].
    fn draw_terminal(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        _theme: &Theme,
        surface: Rect,
        _c: &Connection,
    ) {
        let palette = ui::tokens::terminal_palette::TerminalPalette::dark();
        // Terminal background.
        scene.fill(Fill::NonZero, transform, palette.bg_default, None, &surface);

        // Snapshot the live grid (cheap clone of the visible cells + cursor).
        let (lines, cursor) = match &self.live {
            Some(LiveSession::Ssh(s)) => (s.grid().lines(), s.grid().cursor()),
            _ => return,
        };

        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &surface,
        );
        let font_size = 14.0f32;
        let char_w = f64::from(font_size) * 0.6; // monospace advance approximation
        let line_h = f64::from(font_size) * 1.5;
        let pad = 16.0;

        for (row, line) in lines.iter().enumerate() {
            let y = surface.y0 + pad + row as f64 * line_h;
            // Draw cells run-by-run (group consecutive same-style cells).
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
                let color = match style.0 {
                    // `ansi` is a fixed `[Color; 16]`; `idx.min(15)` is always in range.
                    Some(idx) => palette
                        .ansi
                        .get(idx.min(15))
                        .copied()
                        .unwrap_or(palette.fg_default),
                    None => palette.fg_default,
                };
                let weight = if style.1 {
                    TextWeight::Bold
                } else {
                    TextWeight::Regular
                };
                text_renderer.draw_text_weighted(
                    scene,
                    &run,
                    font_size,
                    color,
                    transform * Affine::translate((x, y)),
                    None,
                    weight,
                );
                x += run.chars().count() as f64 * char_w;
            }
        }

        // Block caret at the real cursor position.
        let (cx, cy) = cursor;
        let caret_x = surface.x0 + pad + cx as f64 * char_w;
        let caret_y = surface.y0 + pad + cy as f64 * line_h;
        let caret = Rect::new(
            caret_x,
            caret_y,
            caret_x + char_w,
            caret_y + f64::from(font_size) * 1.2,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            palette.fg_default.at_alpha(0.6),
            None,
            &caret,
        );

        scene.pop_layer();
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
}

/// Draw a five-pointed star centered at `center`. Filled when `filled`,
/// otherwise just an outline — used for the favorite toggle so the marker
/// renders as a vector shape (independent of font glyph coverage).
fn draw_star(
    scene: &mut Scene,
    transform: Affine,
    center: Point,
    r: f64,
    color: Color,
    filled: bool,
) {
    use vello::kurbo::BezPath;
    let inner = r * 0.42;
    let mut path = BezPath::new();
    for i in 0..10 {
        let radius = if i % 2 == 0 { r } else { inner };
        // Start at the top point (-90°) and step every 36°.
        let angle = -std::f64::consts::FRAC_PI_2 + f64::from(i) * std::f64::consts::PI / 5.0;
        let p = Point::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        );
        if i == 0 {
            path.move_to(p);
        } else {
            path.line_to(p);
        }
    }
    path.close_path();
    if filled {
        scene.fill(Fill::NonZero, transform, color, None, &path);
    } else {
        scene.stroke(&Stroke::new(1.2), transform, color, None, &path);
    }
}

/// Color for a kind's badge — derived from theme tokens so it tracks the
/// active theme.
fn kind_color(kind: ConnectionKind, theme: &Theme) -> Color {
    let c = &theme.tokens.colors;
    match kind {
        ConnectionKind::Ssh => c.semantic.success,
        ConnectionKind::Vnc => c.semantic.info,
        ConnectionKind::Rdp => c.semantic.warning,
        ConnectionKind::Qos => c.accent[6],
    }
}

/// Tab order across the form fields, respecting which fields the current
/// kind shows.
fn next_field(cur: Option<Field>, kind: ConnectionKind) -> Field {
    let order: Vec<Field> = if kind == ConnectionKind::Qos {
        vec![Field::Name, Field::Host]
    } else {
        vec![
            Field::Name,
            Field::Host,
            Field::Port,
            Field::User,
            Field::Group,
        ]
    };
    match cur {
        Some(f) => {
            let idx = order.iter().position(|x| *x == f).unwrap_or(0);
            // `order` is never empty, and the modulo keeps the index in range.
            order
                .get((idx + 1) % order.len())
                .copied()
                .unwrap_or(Field::Name)
        }
        None => order.first().copied().unwrap_or(Field::Name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> AutoCommit {
        AutoCommit::new()
    }

    /// A synthetic character key press, as winit delivers it.
    fn char_ev(s: &str) -> WindowEvent {
        use winit::keyboard::SmolStr;
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(SmolStr::new(s));
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(SmolStr::new(s));
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    /// Feed one event through the real `Widget::handle_event` path, with the
    /// modifiers the host would carry.
    fn feed(app: &mut RemoteApp, state: &mut AutoCommit, ev: &WindowEvent, mods: ModifiersState) {
        let m = winit::event::Modifiers::from(mods);
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let (mut h, mut ft) = (false, None);
        app.handle_event(
            ev,
            1.0,
            &m,
            state,
            &automerge::ROOT,
            &mut h,
            &mut ft,
            node,
            &taffy,
        );
    }

    /// Drive the real layout passes (`build_layout` → taffy → `apply_layout`)
    /// at a given allocation, exactly as the host does, so responsive tests
    /// assert on the production path rather than a tautological helper.
    fn lay_out(app: &mut RemoteApp, w: f64, h: f64) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = app.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(w as f32),
                    height: taffy::AvailableSpace::Definite(h as f32),
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, Point::ZERO);
    }

    #[test]
    fn wide_window_docks_rail_open() {
        let mut app = RemoteApp::new();
        lay_out(&mut app, 1280.0, 800.0);
        assert_eq!(app.rail_layout, SplitLayout::Docked);
        assert!(
            app.rail_open(),
            "a wide window keeps the rail docked + open"
        );
    }

    #[test]
    fn narrow_window_collapses_rail_to_closed_overlay() {
        let mut app = RemoteApp::new();
        lay_out(&mut app, 640.0, 900.0);
        assert_eq!(app.rail_layout, SplitLayout::Overlay);
        assert!(
            !app.rail_open(),
            "a narrow window collapses the rail into a closed overlay flap"
        );
    }

    #[test]
    fn breakpoint_boundary_is_inclusive_docked() {
        let mut app = RemoteApp::new();
        lay_out(&mut app, RAIL_BREAKPOINT, 800.0);
        assert_eq!(
            app.rail_layout,
            SplitLayout::Docked,
            "at the breakpoint we dock"
        );
        let mut app2 = RemoteApp::new();
        lay_out(&mut app2, RAIL_BREAKPOINT - 1.0, 800.0);
        assert_eq!(
            app2.rail_layout,
            SplitLayout::Overlay,
            "just below we overlay"
        );
    }

    #[test]
    fn toggle_opens_overlay_rail_then_esc_dismisses() {
        let mut app = RemoteApp::new();
        lay_out(&mut app, 640.0, 900.0);
        assert!(!app.rail_open());
        // Hamburger summons the overlay rail.
        app.apply(Action::ToggleRail, &mut doc());
        assert!(app.rail_open(), "toggle opens the overlay flap");
        // Esc dismisses it (production key path).
        let mut handled = false;
        app.handle_key(
            &Key::Named(NamedKey::Escape),
            None,
            ModifiersState::empty(),
            &mut doc(),
            &mut handled,
        );
        assert!(handled, "Esc on an open overlay flap is consumed");
        assert!(!app.rail_open(), "Esc closes the overlay flap");
    }

    #[test]
    fn crossing_breakpoint_returns_rail_to_mode_default() {
        let mut app = RemoteApp::new();
        // Start docked, user hides the rail.
        lay_out(&mut app, 1280.0, 800.0);
        app.apply(Action::ToggleRail, &mut doc());
        assert!(!app.rail_open(), "user hid the docked rail");
        // Narrow past the breakpoint, then widen again: the stale preference
        // is dropped and the rail returns to the docked default (open).
        lay_out(&mut app, 640.0, 900.0);
        lay_out(&mut app, 1280.0, 800.0);
        assert!(
            app.rail_open(),
            "crossing the breakpoint clears the stale preference"
        );
    }

    #[test]
    fn live_new_starts_empty_and_writes_nothing() {
        // The honesty guard: the live constructor fabricates no machines, and
        // `sync` on a fresh document must not seed a fake fleet into the CRDT.
        let mut app = RemoteApp::new();
        assert!(
            app.workspace.connections().is_empty(),
            "live app starts empty"
        );
        assert!(
            app.qos_devices.is_empty(),
            "no fabricated discovered devices"
        );
        assert!(app.selected.is_none());
        assert_eq!(app.screen, Screen::List);
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        assert!(
            RemoteDoc::list(&state).is_empty(),
            "an empty live app must not write fabricated machines into the document"
        );
    }

    #[test]
    fn demo_build_is_populated_for_shots() {
        // `--shots` still needs a lived-in frame.
        let app = RemoteApp::new_demo();
        assert!(app.workspace.connections().len() >= 6);
        assert_eq!(app.screen, Screen::List);
        assert!(app.selected.is_some());
    }

    /// A hostname or a key is exactly the sort of string nobody types by
    /// hand, so the connection form has to take a paste — and, having taken
    /// one, has to let you select what arrived and replace it.
    #[test]
    #[serial_test::serial(clipboard)]
    fn the_connection_form_takes_a_paste_and_a_selection() {
        let ctrl = ModifiersState::CONTROL;
        let mut app = RemoteApp::new();
        lay_out(&mut app, 1280.0, 800.0);
        let mut state = doc();
        app.apply(Action::NewConnection, &mut state);
        app.apply(Action::FocusField(Field::Host), &mut state);

        ui::text_selection::clipboard_copy("build-07.fleet.internal");
        feed(&mut app, &mut state, &char_ev("v"), ctrl);
        assert_eq!(
            app.draft.host, "build-07.fleet.internal",
            "Ctrl+V puts the clipboard into the focused field",
        );

        // Select-all then type replaces what is there — the other half of a
        // real field, and impossible for a push/pop buffer.
        feed(&mut app, &mut state, &char_ev("a"), ctrl);
        feed(&mut app, &mut state, &char_ev("x"), ModifiersState::empty());
        assert_eq!(app.draft.host, "x", "typing over a selection replaces it");
    }

    /// The port field stays numeric, but numeric is not a reason to lose the
    /// clipboard: a pasted port arrives, and pasted junk is still filtered.
    #[test]
    #[serial_test::serial(clipboard)]
    fn the_port_field_takes_a_paste_and_stays_numeric() {
        let ctrl = ModifiersState::CONTROL;
        let mut app = RemoteApp::new();
        lay_out(&mut app, 1280.0, 800.0);
        let mut state = doc();
        app.apply(Action::NewConnection, &mut state);
        app.apply(Action::FocusField(Field::Port), &mut state);
        feed(&mut app, &mut state, &char_ev("a"), ctrl); // replace the default
        ui::text_selection::clipboard_copy("2222");
        feed(&mut app, &mut state, &char_ev("v"), ctrl);
        assert_eq!(app.draft.port, "2222", "a pasted port arrives");

        feed(&mut app, &mut state, &char_ev("a"), ctrl);
        ui::text_selection::clipboard_copy("22ssh");
        feed(&mut app, &mut state, &char_ev("v"), ctrl);
        assert_eq!(app.draft.port, "22", "non-digits are still refused");
    }

    /// The search box is the sixth of these fields and gets the same deal.
    #[test]
    #[serial_test::serial(clipboard)]
    fn the_search_box_takes_a_paste() {
        let ctrl = ModifiersState::CONTROL;
        let mut app = RemoteApp::new_demo();
        lay_out(&mut app, 1280.0, 800.0);
        let mut state = doc();
        app.apply(Action::FocusField(Field::Search), &mut state);
        ui::text_selection::clipboard_copy("build");
        feed(&mut app, &mut state, &char_ev("v"), ctrl);
        assert_eq!(app.search, "build");
    }

    #[test]
    fn new_connection_opens_blank_form_focused() {
        let mut app = RemoteApp::new();
        app.apply(Action::NewConnection, &mut doc());
        assert_eq!(app.screen, Screen::Form { editing: None });
        assert_eq!(app.focus, Some(Field::Name));
        assert!(app.draft.name.is_empty());
    }

    #[test]
    fn save_form_adds_connection_and_returns_to_list() {
        let mut app = RemoteApp::new();
        let before = app.workspace.connections().len();
        app.apply(Action::NewConnection, &mut doc());
        app.draft.name = "Test".into();
        app.draft.host = "1.2.3.4".into();
        app.draft.port = "22".into();
        app.apply(Action::SaveForm, &mut doc());
        assert_eq!(app.screen, Screen::List);
        assert_eq!(app.workspace.connections().len(), before + 1);
        assert!(app.workspace.connections().iter().any(|c| c.name == "Test"));
    }

    #[test]
    fn save_form_blocked_on_validation() {
        let mut app = RemoteApp::new();
        app.apply(Action::NewConnection, &mut doc());
        // Empty name + host → blocked, stays on the form.
        app.apply(Action::SaveForm, &mut doc());
        assert!(matches!(app.screen, Screen::Form { .. }));
    }

    #[test]
    fn edit_then_save_updates_in_place() {
        let mut app = RemoteApp::new_demo();
        let id = app.workspace.connections()[0].id;
        let count = app.workspace.connections().len();
        app.apply(Action::EditConnection(id), &mut doc());
        app.draft.name = "Renamed Host".into();
        app.apply(Action::SaveForm, &mut doc());
        assert_eq!(
            app.workspace.connections().len(),
            count,
            "edit must not add"
        );
        assert_eq!(app.workspace.get(id).unwrap().name, "Renamed Host");
    }

    #[test]
    fn delete_removes_connection() {
        let mut app = RemoteApp::new_demo();
        let id = app.workspace.connections()[0].id;
        let count = app.workspace.connections().len();
        app.apply(Action::Delete(id), &mut doc());
        assert_eq!(app.workspace.connections().len(), count - 1);
        assert!(app.workspace.get(id).is_none());
    }

    #[test]
    fn connect_ssh_shows_honest_not_connected_session() {
        // Default build has no live transport: Connect opens the session
        // screen but does NOT fabricate a terminal or open a transport — an
        // honest "not connected" state, never a faked shell.
        let mut app = RemoteApp::new_demo();
        let id = app
            .workspace
            .connections()
            .iter()
            .find(|c| c.kind == ConnectionKind::Ssh)
            .unwrap()
            .id;
        app.apply(Action::Connect(id), &mut doc());
        assert_eq!(app.screen, Screen::Session { id });
        assert!(
            app.live.is_none(),
            "no live transport → no session object, no fabricated terminal"
        );
    }

    #[test]
    fn connect_vnc_has_no_live_session_when_gated() {
        let mut app = RemoteApp::new_demo();
        let id = app
            .workspace
            .connections()
            .iter()
            .find(|c| c.kind == ConnectionKind::Vnc)
            .unwrap()
            .id;
        app.apply(Action::Connect(id), &mut doc());
        assert!(
            app.live.is_none(),
            "gated build opens no framebuffer transport"
        );
    }

    #[test]
    fn pick_qos_device_fills_draft() {
        let mut app = RemoteApp::new_demo();
        app.apply(Action::NewConnection, &mut doc());
        app.apply(Action::SetKind(ConnectionKind::Qos), &mut doc());
        app.apply(Action::PickQosDevice(0), &mut doc());
        assert_eq!(app.draft.kind, ConnectionKind::Qos);
        assert!(!app.draft.host.is_empty());
        assert_eq!(app.draft.auth, AuthMethod::DeviceIdentity);
    }

    #[test]
    fn set_kind_qos_clears_port_and_sets_device_auth() {
        let mut app = RemoteApp::new();
        app.apply(Action::NewConnection, &mut doc());
        app.apply(Action::SetKind(ConnectionKind::Qos), &mut doc());
        assert!(app.draft.port.is_empty());
        assert_eq!(app.draft.auth, AuthMethod::DeviceIdentity);
    }

    #[test]
    fn search_filters_rows() {
        let mut app = RemoteApp::new();
        app.search = "build".into();
        let rows = app.workspace.filtered(&app.search);
        assert!(rows.iter().all(|c| {
            c.name.to_lowercase().contains("build") || c.host.to_lowercase().contains("build")
        }));
    }

    /// Loading a connection to edit has to reach the widgets, not just the
    /// draft behind them — otherwise the form shows placeholders over values
    /// that are really there, and saving writes back whatever was on screen.
    #[test]
    fn editing_a_connection_loads_it_into_the_fields() {
        let mut app = RemoteApp::new_demo();
        let c = app.workspace.connections()[0].clone();
        app.apply(Action::EditConnection(c.id), &mut doc());
        assert_eq!(app.inputs.get(Field::Name).value(), c.name);
        assert_eq!(app.inputs.get(Field::Host).value(), c.host);
        assert_eq!(app.inputs.get(Field::Port).value(), c.port.to_string());
    }

    #[test]
    fn typing_into_focused_field_appends() {
        let none = ModifiersState::empty();
        let mut app = RemoteApp::new();
        let mut state = doc();
        app.apply(Action::NewConnection, &mut state);
        app.apply(Action::FocusField(Field::Name), &mut state);
        feed(&mut app, &mut state, &char_ev("a"), none);
        feed(&mut app, &mut state, &char_ev("b"), none);
        assert_eq!(app.draft.name, "ab");
    }

    #[test]
    fn port_field_rejects_non_digits() {
        let none = ModifiersState::empty();
        let mut app = RemoteApp::new();
        let mut state = doc();
        app.apply(Action::NewConnection, &mut state);
        app.draft.port.clear();
        app.reload_inputs();
        app.apply(Action::FocusField(Field::Port), &mut state);
        feed(&mut app, &mut state, &char_ev("2"), none);
        feed(&mut app, &mut state, &char_ev("x"), none);
        feed(&mut app, &mut state, &char_ev("2"), none);
        assert_eq!(app.draft.port, "22");
    }

    #[test]
    fn toggle_favorite_flips() {
        let mut app = RemoteApp::new_demo();
        let id = app.workspace.connections()[0].id;
        let before = app.workspace.get(id).unwrap().favorite;
        app.apply(Action::ToggleFavorite(id), &mut doc());
        assert_ne!(app.workspace.get(id).unwrap().favorite, before);
    }

    #[test]
    fn favorite_affordance_dispatches_the_ctrl_k_action() {
        // Affordance == ActionSpec: the on-screen star dispatches the exact same
        // SetFavorite envelope the Ctrl+K row builds — one source of truth on the
        // real dispatch path (through the app's embedded `ActionHost`).
        let mut app = RemoteApp::new_demo();
        let mut state = AutoCommit::new();
        // Publish the demo list into the CRDT so the connection exists there too.
        app.sync(&mut state, &automerge::ROOT);
        let id = app.workspace.connections()[0].id;
        let target = !app.workspace.get(id).unwrap().favorite;

        app.apply(Action::ToggleFavorite(id), &mut state);

        let dispatched = app
            .last_dispatched()
            .expect("the star dispatched an action");
        assert_eq!(
            dispatched,
            &crate::actions::encode(&crate::actions::set_favorite_action(id, target)),
            "the star and the Ctrl+K row build the byte-identical action",
        );
        assert!(
            app.take_committed_action().is_some(),
            "the dispatch armed exactly one undo boundary for the host to record",
        );
    }

    #[test]
    fn delete_affordance_dispatches_the_delete_action() {
        let mut app = RemoteApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let id = app.workspace.connections()[0].id;

        app.apply(Action::Delete(id), &mut state);

        assert!(
            RemoteDoc::list(&state).iter().all(|c| c.id != id),
            "removed from the CRDT"
        );
        let dispatched = app.last_dispatched().expect("delete dispatched an action");
        assert_eq!(
            dispatched,
            &crate::actions::encode(&crate::actions::delete_action(id)),
        );
    }

    #[test]
    fn save_form_dispatches_upsert_into_the_crdt() {
        let mut app = RemoteApp::new();
        let mut state = AutoCommit::new();
        app.apply(Action::NewConnection, &mut state);
        app.draft.name = "Test".into();
        app.draft.host = "1.2.3.4".into();
        app.draft.port = "22".into();
        app.apply(Action::SaveForm, &mut state);

        let doc_list = RemoteDoc::list(&state);
        assert_eq!(
            doc_list.len(),
            1,
            "the saved connection reached the shared document"
        );
        assert_eq!(doc_list[0].name, "Test");
        assert!(
            app.last_dispatched().is_some(),
            "save routed through an action"
        );
    }

    #[test]
    fn dev_set_screen_jumps_to_form() {
        let mut app = RemoteApp::new();
        app.dev_set_screen("form");
        assert_eq!(app.screen, Screen::Form { editing: None });
    }

    #[test]
    fn dev_set_screen_qos_form_selects_qos_kind() {
        let mut app = RemoteApp::new();
        app.dev_set_screen("form-qos");
        assert!(matches!(app.screen, Screen::Form { .. }));
        assert_eq!(app.draft.kind, ConnectionKind::Qos);
    }

    #[test]
    fn unavailable_transport_cannot_open_a_session() {
        let mut app = RemoteApp::new_demo();
        let mut state = AutoCommit::new();
        let id = app
            .first_of_kind(ConnectionKind::Rdp)
            .expect("demo contains a legacy RDP profile");

        app.apply(Action::Connect(id), &mut state);

        assert_eq!(app.screen, Screen::List);
        assert_eq!(app.selected, Some(id));
        assert!(app.live.is_none());
    }

    #[test]
    fn dev_set_screen_opens_kind_specific_session() {
        for (which, kind) in [
            ("session-ssh", ConnectionKind::Ssh),
            ("session-vnc", ConnectionKind::Vnc),
            ("session-rdp", ConnectionKind::Rdp),
            ("session-qos", ConnectionKind::Qos),
        ] {
            let mut app = RemoteApp::new_demo();
            app.dev_set_screen(which);
            match app.screen {
                Screen::Session { id } => {
                    assert_eq!(app.workspace.get(id).unwrap().kind, kind);
                }
                other => panic!("{which} did not open a session: {other:?}"),
            }
        }
    }

    #[test]
    fn sync_seeds_then_pulls_remote() {
        let mut state = AutoCommit::new();
        // A peer pre-populates the doc with one extra connection.
        let extra = Connection {
            id: Uuid::new_v4(),
            name: "Peer Box".into(),
            host: "peer".into(),
            port: 22,
            user: "p".into(),
            kind: ConnectionKind::Ssh,
            auth: AuthMethod::Password,
            last_used: "now".into(),
            presence: Presence::Online,
            group: String::new(),
            favorite: false,
        };
        RemoteDoc::upsert(&mut state, &extra);

        let mut app = RemoteApp::new();
        app.sync(&mut state, &automerge::ROOT);
        // Adopted the peer's list (non-empty doc path).
        assert!(app.workspace.get(extra.id).is_some());
    }
}

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
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

use ui::Widget;
use ui::a11y::{AccessNode, AccessState, Politeness, Role};
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::tooltip::draw_hover_tooltip;
use ui::widget::{Button, ButtonFace, FlapState, SplitLayout, TextInput, TooltipHost};

use crate::doc::RemoteDoc;
use crate::model::{
    AuthMethod, Connection, ConnectionDraft, ConnectionId, ConnectionKind, Presence, Screen,
    Workspace,
};
use crate::session::{SessionStatus, TransportReadiness, connection_readiness};
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
    Qos(QosSession),
    Error(String),
    /// A saved profile that cannot start in this process. This is a distinct
    /// state from a failed connection: no socket was opened and no credential
    /// was sent. Keeping it explicit lets the UI offer an honest recovery path
    /// instead of a blank row or a misleading retry button.
    Unavailable {
        readiness: TransportReadiness,
        kind: ConnectionKind,
    },
    /// A fixed status with no transport, for `--shots`/preview rendering of
    /// the composed connecting/failed/closed surfaces. Never on the live path.
    Preview(SessionStatus),
}

impl LiveSession {
    fn status(&self) -> SessionStatus {
        match self {
            LiveSession::Ssh(s) => s.status(),
            LiveSession::Vnc(s) => s.status(),
            LiveSession::Qos(s) => s.status.clone(),
            LiveSession::Error(e) => SessionStatus::Failed(e.clone()),
            LiveSession::Unavailable { readiness, kind } => {
                SessionStatus::Unavailable(readiness.detail(*kind).into())
            }
            LiveSession::Preview(s) => s.clone(),
        }
    }
}

struct QosSession {
    id: u64,
    status: SessionStatus,
    size: Option<(u32, u32)>,
    decoder: qos_callstream::H264Decoder,
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
/// Rows gain a second action line when the content column can no longer keep
/// identity, metadata, and two 44px-minimum targets side by side.
const COMPACT_ROW_H: f64 = 116.0;
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
    /// Ask before opening a live session from the canvas UI. Resource actions
    /// receive the same confirmation from the shell; this keeps mouse,
    /// keyboard, and assistive-technology paths equally explicit.
    RequestConnect(ConnectionId),
    /// Select a row without connecting (shows the detail rail).
    SelectRow(ConnectionId),
    /// Toggle the favorite star on a row.
    ToggleFavorite(ConnectionId),
    /// Ask before removing a shared saved profile from the canvas UI.
    RequestDelete(ConnectionId),
    /// Ask before adding the freshly observed SSH host key to device-private trust.
    RequestTrustSshHostKey(ConnectionId),
    /// Ask before forgetting a device-private SSH host-key pin.
    RequestForgetSshHostKey(ConnectionId),
    /// Execute the currently visible, revalidated confirmation.
    ConfirmPending,
    /// Dismiss the currently visible confirmation without doing anything.
    CancelPending,
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

    fn target_name(self) -> &'static str {
        match self {
            Field::Search => "search",
            Field::Name => "name",
            Field::Host => "host",
            Field::Port => "port",
            Field::User => "user",
            Field::Group => "group",
        }
    }

    fn from_target_name(name: &str) -> Option<Self> {
        match name {
            "search" => Some(Field::Search),
            "name" => Some(Field::Name),
            "host" => Some(Field::Host),
            "port" => Some(Field::Port),
            "user" => Some(Field::User),
            "group" => Some(Field::Group),
            _ => None,
        }
    }
}

/// Validation visibility belongs to this editing interaction, rather than to
/// the draft itself. A fresh draft is allowed to be incomplete without being
/// announced as an error; a field shows its own feedback once it has lost
/// focus, and Save reveals every remaining problem.
#[derive(Default)]
struct FormValidation {
    submitted: bool,
    /// Name, host, and port in that order; these are the only fields with
    /// field-local validation feedback.
    touched: (bool, bool, bool),
}

impl FormValidation {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn touch(&mut self, field: Field) {
        match field {
            Field::Name => self.touched.0 = true,
            Field::Host => self.touched.1 = true,
            Field::Port => self.touched.2 = true,
            Field::Search | Field::User | Field::Group => {}
        }
    }

    fn shows_problem(&self, problem: &str) -> bool {
        if self.submitted {
            return true;
        }
        match problem {
            p if p.starts_with("Name is required") => self.touched.0,
            p if p.starts_with("Host is required")
                || p.starts_with("qOS device node must be an exact certified endpoint id") =>
            {
                self.touched.1
            }
            p if p.starts_with("Port") => self.touched.2,
            // Cross-field/protocol errors have no single field to repair, so
            // reserve them for an explicit Save attempt.
            _ => false,
        }
    }
}

impl AuthMethod {
    fn target_name(self) -> &'static str {
        match self {
            AuthMethod::Password => "password",
            AuthMethod::PublicKey => "public-key",
            AuthMethod::DeviceIdentity => "device-identity",
        }
    }

    fn from_target_name(name: &str) -> Self {
        match name {
            "public-key" => AuthMethod::PublicKey,
            "device-identity" => AuthMethod::DeviceIdentity,
            _ => AuthMethod::Password,
        }
    }
}

trait OptionalAccessBounds {
    fn with_optional_bounds(self, bounds: Option<Rect>) -> Self;
}

impl OptionalAccessBounds for AccessNode {
    fn with_optional_bounds(self, bounds: Option<Rect>) -> Self {
        match bounds {
            Some(bounds) => self.with_bounds(bounds),
            None => self,
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
    /// Interaction-local validation state for the current add/edit form.
    form_validation: FormValidation,
    /// Which text field currently has keyboard focus.
    focus: Option<Field>,
    /// The shared [`TextInput`] behind each editable field.
    inputs: FieldInputs,
    /// The live transport for the open session. Production qOS control always
    /// creates one; gated SSH/VNC leave it `None` and show the honest "not
    /// connected" surface until explicitly enabled.
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
    collab_outbox: Vec<ui::collaboration::CollabRequest>,
    next_qos_session_id: u64,

    /// Root rect, from `apply_layout`.
    rect: Rect,
    /// Clickable regions captured last `draw`, hit-tested on press.
    hits: Vec<Hit>,
    /// Last pointer position in logical coords (for hover + hit-tests).
    pointer: Point,
    /// Vertical offset and most recently measured limit for the saved-machine
    /// list. Kept as view state: collaborators can browse independently.
    list_scroll: f64,
    list_scroll_max: f64,
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
    /// Canvas-side confirmation mirroring resource-action policy. This is
    /// private view state and never replicated with the connection profile.
    pending_confirmation: Option<PendingConfirmation>,
    /// Short local status after a confirmation is cancelled by revalidation.
    /// It deliberately contains no credential material.
    notice: Option<String>,

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
    /// System motion preference. The rail keeps the same spatial model, but
    /// snaps between endpoints when motion is reduced.
    motion_reduced: bool,
    /// Shared delayed tooltip state for icon-only canvas controls.
    tooltips: TooltipHost,
}

impl RemoteApp {
    /// A short local outcome message for cancelled/revalidated actions. This
    /// intentionally stays out of the document and contains no secret data.
    fn draw_notice(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        message: &str,
    ) {
        let tokens = &theme.tokens;
        let max_w = (self.rect.width() - 32.0).min(520.0);
        let rect = Rect::new(
            self.rect.x1 - max_w - 16.0,
            self.rect.y1 - 70.0,
            self.rect.x1 - 16.0,
            self.rect.y1 - 16.0,
        );
        let card = RoundedRect::from_rect(rect, 10.0);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.semantic.warning.at_alpha(0.18),
            None,
            &card,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.semantic.warning,
            None,
            &card,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Action needs review",
            12.5,
            tokens.colors.text_primary,
            transform * Affine::translate((rect.x0 + 12.0, rect.y0 + 9.0)),
            None,
            TextWeight::Semibold,
        );
        text_renderer.draw_text(
            scene,
            message,
            12.0,
            tokens.colors.text_dim,
            transform * Affine::translate((rect.x0 + 12.0, rect.y0 + 27.0)),
            Some((rect.width() - 24.0) as f32),
        );
    }

    /// Canvas counterpart to the shell's resource-action confirmation. It
    /// states the scope in plain language and makes cancel the safe default.
    #[allow(
        clippy::too_many_lines,
        reason = "a compact modal keeps its visual copy, geometry, and action targets together"
    )]
    fn draw_confirmation(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(pending) = self.pending_confirmation.clone() else {
            return;
        };
        let tokens = &theme.tokens;
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(0, 0, 0, 255).at_alpha(0.62),
            None,
            &self.rect,
        );
        let connection = self.workspace.get(pending.id());
        let name = connection.map_or("this saved connection", |c| c.name.as_str());
        let is_delete = matches!(pending, PendingConfirmation::Delete { .. });
        let (title, detail, primary) = match &pending {
            PendingConfirmation::Delete { .. } => (
                format!("Delete {name}?"),
                "This removes the shared saved profile from Remote. It does not modify the remote machine.".to_string(),
                "Delete",
            ),
            PendingConfirmation::Connect { .. } => (
                format!("Connect to {name}?"),
                "This opens a live remote session. Remote will recheck the saved target and access before anything is sent.".to_string(),
                "Connect",
            ),
            PendingConfirmation::TrustSshHostKey { observed, .. } => (
                format!("Trust SSH key for {name}?"),
                format!("Verify this out of band before trusting: {} {}. This pin is private to this device.", observed.algorithm, observed.fingerprint),
                "Trust key",
            ),
            PendingConfirmation::ForgetSshHostKey { .. } => (
                format!("Forget SSH key for {name}?"),
                "This removes only this device's saved host-key pin. Reconnect to inspect and explicitly trust a replacement key.".to_string(),
                "Forget key",
            ),
        };
        let w = 460.0_f64.min((self.rect.width() - 32.0).max(280.0));
        let card_rect = Rect::new(
            self.rect.center().x - w / 2.0,
            self.rect.center().y - 112.0,
            self.rect.center().x + w / 2.0,
            self.rect.center().y + 112.0,
        );
        let card = RoundedRect::from_rect(card_rect, 14.0);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &card,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.border_focus,
            None,
            &card,
        );
        text_renderer.draw_text_weighted(
            scene,
            &title,
            18.0,
            tokens.colors.text_primary,
            transform * Affine::translate((card_rect.x0 + 24.0, card_rect.y0 + 28.0)),
            Some((card_rect.width() - 48.0) as f32),
            TextWeight::Bold,
        );
        if let Some(connection) = connection {
            text_renderer.draw_text(
                scene,
                &connection.address(),
                12.5,
                tokens.colors.text_dim,
                transform * Affine::translate((card_rect.x0 + 24.0, card_rect.y0 + 58.0)),
                Some((card_rect.width() - 48.0) as f32),
            );
        }
        text_renderer.draw_text(
            scene,
            &detail,
            13.0,
            tokens.colors.text_dim,
            transform * Affine::translate((card_rect.x0 + 24.0, card_rect.y0 + 92.0)),
            Some((card_rect.width() - 48.0) as f32),
        );
        let cancel = Rect::new(
            card_rect.x1 - 244.0,
            card_rect.y1 - 58.0,
            card_rect.x1 - 132.0,
            card_rect.y1 - 18.0,
        );
        let confirm = Rect::new(
            card_rect.x1 - 120.0,
            card_rect.y1 - 58.0,
            card_rect.x1 - 18.0,
            card_rect.y1 - 18.0,
        );
        self.draw_button(
            scene,
            transform,
            text_renderer,
            theme,
            cancel,
            "Cancel",
            false,
        );
        if is_delete {
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(confirm, "Delete")
                    .hovered(confirm.contains(self.pointer))
                    .text_size(13.0)
                    .weight(TextWeight::Semibold)
                    .danger(),
            );
        } else {
            self.draw_button(
                scene,
                transform,
                text_renderer,
                theme,
                confirm,
                primary,
                true,
            );
        }
        self.push_hit(cancel, Action::CancelPending);
        self.push_hit(confirm, Action::ConfirmPending);
    }

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
            form_validation: FormValidation::default(),
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
            collab_outbox: Vec::new(),
            next_qos_session_id: 1,
            rect: Rect::ZERO,
            hits: Vec::new(),
            pointer: Point::ZERO,
            list_scroll: 0.0,
            list_scroll_max: 0.0,
            dirty: true,
            seeded: false,
            actions: ui::ActionHost::new(),
            pending_resource_operation: None,
            pending_confirmation: None,
            notice: None,
            rail: FlapState::open_when_docked(),
            rail_layout: SplitLayout::Docked,
            rail_snap: None,
            last_tick: std::time::Instant::now(),
            motion_reduced: false,
            tooltips: TooltipHost::new(),
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
                    SessionStatus::Unavailable(
                        TransportReadiness::BackendUnavailable
                            .detail(ConnectionKind::Rdp)
                            .into(),
                    ),
                );
            }
            "session-qos" => {
                self.dev_preview_session(ConnectionKind::Qos, SessionStatus::Connecting);
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
            "confirm-connect" => {
                if let Some(id) = self.first_of_kind(ConnectionKind::Qos) {
                    self.request_connect(id);
                }
            }
            "confirm-delete" => {
                if let Some(id) = self
                    .workspace
                    .connections()
                    .first()
                    .map(|connection| connection.id)
                {
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
                            fingerprint: "SHA256:DemoFingerprintForVisualReviewOnly".into(),
                        },
                    });
                }
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
                self.form_validation.reset();
                self.screen = Screen::Form { editing: None };
                self.reload_inputs();
                self.focus_field(Field::Name);
                self.dismiss_overlay_rail();
            }
            Action::EditConnection(id) => {
                if let Some(c) = self.workspace.get(id) {
                    self.draft = ConnectionDraft::from_connection(c);
                    self.form_validation.reset();
                    self.screen = Screen::Form { editing: Some(id) };
                    self.reload_inputs();
                    self.focus_field(Field::Name);
                }
            }
            Action::Connect(id) => {
                self.open_session(id);
            }
            Action::RequestConnect(id) => self.request_connect(id),
            Action::SelectRow(id) => {
                self.selected = Some(id);
            }
            Action::ToggleFavorite(id) => {
                self.toggle_favorite(id, state);
            }
            Action::RequestDelete(id) => self.request_delete(id),
            Action::RequestTrustSshHostKey(id) => self.request_trust_ssh_host_key(id),
            Action::RequestForgetSshHostKey(id) => self.request_forget_ssh_host_key(id),
            Action::ConfirmPending => self.confirm_pending(state),
            Action::CancelPending => {
                self.pending_confirmation = None;
                self.notice =
                    Some("Action cancelled. No connection or profile was changed.".into());
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
                if !self.draft.auth.is_supported_by(k)
                    && let Some(auth) = AuthMethod::available_for(k).first().copied()
                {
                    self.draft.auth = auth;
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

    /// The most recently painted bounds for a custom-canvas affordance. Hit
    /// regions and accessibility geometry deliberately share this source so a
    /// resized or responsive control cannot become visually/semantically split.
    fn action_bounds(&self, action: Action) -> Option<Rect> {
        self.hits
            .iter()
            .rev()
            .find_map(|hit| (hit.action == action).then_some(hit.rect))
    }

    /// Accessibility edits take the same validation path as keyboard/paste:
    /// notably, the port remains numeric instead of accepting a value the Save
    /// action would later reject for a different reason.
    fn set_field_value(&mut self, field: Field, value: &str) {
        let value = if field == Field::Port {
            value.chars().filter(char::is_ascii_digit).collect()
        } else {
            value.to_string()
        };
        match field {
            Field::Search => self.search.clone_from(&value),
            Field::Name => self.draft.name.clone_from(&value),
            Field::Host => self.draft.host.clone_from(&value),
            Field::Port => self.draft.port.clone_from(&value),
            Field::User => self.draft.user.clone_from(&value),
            Field::Group => self.draft.group.clone_from(&value),
        }
        self.inputs.get(field).set_text(&value);
        self.focus_field(field);
        if field == Field::Search {
            self.selected = self.workspace.filtered(&self.search).first().map(|c| c.id);
            self.list_scroll = 0.0;
        }
        self.dirty = true;
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

    /// Stable, non-secret identity used to revalidate an on-screen confirmation
    /// after a collaborator may have edited the shared profile.
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

    fn request_connect(&mut self, id: ConnectionId) {
        let Some(connection) = self.workspace.get(id) else {
            self.notice = Some("That saved connection no longer exists.".into());
            return;
        };
        if !connection_readiness(connection).can_connect() {
            self.open_session(id);
            return;
        }
        self.notice = None;
        self.pending_confirmation = Some(PendingConfirmation::Connect {
            id,
            target: Self::confirmation_target(connection),
        });
        self.blur_field();
    }

    fn request_delete(&mut self, id: ConnectionId) {
        let Some(connection) = self.workspace.get(id) else {
            self.notice = Some("That saved connection no longer exists.".into());
            return;
        };
        self.notice = None;
        self.pending_confirmation = Some(PendingConfirmation::Delete {
            id,
            target: Self::confirmation_target(connection),
        });
        self.blur_field();
    }

    fn ssh_host_key_issue(&self, id: ConnectionId) -> Option<crate::ssh::HostKeyIssue> {
        match (&self.live, self.session_for) {
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
        let Some(crate::ssh::HostKeyIssue::FirstContact { observed }) = self.ssh_host_key_issue(id)
        else {
            self.notice = Some("No new SSH host key is waiting for trust. Changed keys must be forgotten before they can be reviewed again.".into());
            return;
        };
        self.pending_confirmation = Some(PendingConfirmation::TrustSshHostKey {
            id,
            target: Self::confirmation_target(connection),
            observed,
        });
    }

    fn request_forget_ssh_host_key(&mut self, id: ConnectionId) {
        let Some(connection) = self.workspace.get(id) else {
            return;
        };
        if !matches!(
            self.ssh_host_key_issue(id),
            Some(crate::ssh::HostKeyIssue::Changed { .. })
        ) {
            self.notice = Some("Only a currently changed SSH host key can be forgotten. Reconnect to revalidate this target first.".into());
            return;
        }
        self.pending_confirmation = Some(PendingConfirmation::ForgetSshHostKey {
            id,
            target: Self::confirmation_target(connection),
        });
    }

    /// Revalidate against the current shared projection immediately before a
    /// network attempt or deletion. A late collaborator edit changes neither
    /// the target nor the document unexpectedly; the user must review it.
    fn confirm_pending(&mut self, state: &mut AutoCommit) {
        let Some(pending) = self.pending_confirmation.take() else {
            return;
        };
        let id = pending.id();
        let Some(connection) = self.workspace.get(id) else {
            self.notice = Some("The saved connection was removed before confirmation.".into());
            return;
        };
        if Self::confirmation_target(connection) != pending.target() {
            self.notice = Some(
                "This connection changed while confirmation was open. Review the current profile, then try again."
                    .into(),
            );
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
                    self.notice = Some(
                        "The observed SSH host key changed before it was trusted. Review it again."
                            .into(),
                    );
                    return;
                }
                let path = match crate::ssh::default_known_hosts_path() {
                    Ok(path) => path,
                    Err(error) => {
                        self.notice = Some(format!(
                            "Could not use private SSH host-key storage: {error}"
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
                    Err(error) => {
                        self.notice = Some(format!("Could not save SSH host-key trust: {error}"));
                    }
                }
            }
            PendingConfirmation::ForgetSshHostKey { .. } => {
                if !matches!(
                    self.ssh_host_key_issue(id),
                    Some(crate::ssh::HostKeyIssue::Changed { .. })
                ) {
                    self.notice = Some("The SSH host key is no longer the changed key you approved for removal. Reconnect to review it again.".into());
                    return;
                }
                let path = match crate::ssh::default_known_hosts_path() {
                    Ok(path) => path,
                    Err(error) => {
                        self.notice = Some(format!(
                            "Could not use private SSH host-key storage: {error}"
                        ));
                        return;
                    }
                };
                match crate::ssh::forget_host_key(&path, &connection.host, connection.port) {
                    Ok(true) => {
                        self.notice = Some("Forgot the local SSH host-key pin. Reconnect to review the next key before trusting it.".into());
                        self.disconnect_session();
                    }
                    Ok(false) => {
                        self.notice =
                            Some("No local SSH host-key pin was found for this target.".into());
                    }
                    Err(error) => {
                        self.notice = Some(format!("Could not forget SSH host-key trust: {error}"));
                    }
                }
            }
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

    /// Open the session surface for a connection. qOS always uses its real
    /// shell-broker transport; SSH/VNC require the explicit live-session
    /// capability. A missing capability or RDP backend gets a first-class
    /// recovery surface — no socket is opened, and it is not misreported as a
    /// connection failure.
    fn open_session(&mut self, id: ConnectionId) {
        let Some(c) = self.workspace.get(id).cloned() else {
            return;
        };
        self.session_for = Some(id);
        self.fullscreen = false;
        self.fb_image = None;
        self.fb_canvas = None;
        self.vnc_buttons = 0;
        self.live = None;
        let readiness = connection_readiness(&c);
        if readiness.can_connect() {
            let live = self.start_live(&c);
            self.live = Some(live);
            // Record the attempt in "last used"; presence is only marked
            // Online once the transport actually reports Connected (in draw).
            if let Some(m) = self.workspace.get_mut(id) {
                m.last_used = "just now".into();
            }
        } else {
            self.live = Some(LiveSession::Unavailable {
                readiness,
                kind: c.kind,
            });
        }
        self.screen = Screen::Session { id };
    }

    fn disconnect_session(&mut self) {
        // Dropping the live session closes the transport (the worker thread
        // sees its channel close and disconnects).
        if let Some(LiveSession::Qos(session)) = self.live.as_ref() {
            self.collab_outbox
                .push(ui::collaboration::CollabRequest::CloseWorkspaceControl {
                    session_id: session.id,
                });
        }
        self.live = None;
        self.fb_image = None;
        self.fb_canvas = None;
        self.session_for = None;
        self.fullscreen = false;
        self.screen = Screen::List;
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
            ConnectionKind::Qos => {
                let target = match c.host.parse::<iroh::EndpointId>() {
                    Ok(target) => *target.as_bytes(),
                    Err(_) => {
                        return LiveSession::Error(
                            "qOS host must be an exact certified device endpoint id".into(),
                        );
                    }
                };
                let id = self.next_qos_session_id;
                self.next_qos_session_id = self.next_qos_session_id.saturating_add(1);
                self.collab_outbox
                    .push(ui::collaboration::CollabRequest::OpenWorkspaceControl {
                        session_id: id,
                        target_device: target,
                    });
                LiveSession::Qos(QosSession {
                    id,
                    status: SessionStatus::Connecting,
                    size: None,
                    decoder: qos_callstream::H264Decoder::new(),
                })
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

    fn qos_pointer_target(&self, p: Point) -> Option<(f64, f64)> {
        let Some(LiveSession::Qos(session)) = self.live.as_ref() else {
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

    fn queue_qos_input(&mut self, event: runtime_collab::WorkspaceControlInput) -> bool {
        let Some(LiveSession::Qos(session)) = self.live.as_ref() else {
            return false;
        };
        if session.status != SessionStatus::Connected {
            return false;
        }
        self.collab_outbox
            .push(ui::collaboration::CollabRequest::WorkspaceControlInput {
                session_id: session.id,
                event,
            });
        true
    }

    fn qos_keycode(code: KeyCode) -> Option<u32> {
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

    /// Commit the form draft into the workspace + CRDT.
    fn save_form(&mut self, state: &mut AutoCommit) {
        if !self.draft.problems().is_empty() {
            self.form_validation.submitted = true;
            if let Some(field) = self.first_invalid_form_field() {
                self.focus_field(field);
            }
            return;
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

    /// The first field a person can directly repair for the current invalid
    /// draft. Keeping this beside the save gate ensures pointer, keyboard, and
    /// accessibility activation all land on the same recovery path.
    fn first_invalid_form_field(&self) -> Option<Field> {
        if self.draft.name.trim().is_empty() {
            Some(Field::Name)
        } else if self.draft.host.trim().is_empty()
            || (self.draft.kind == ConnectionKind::Qos
                && !self.draft.host.trim().is_empty()
                && !self.draft.problems().is_empty())
        {
            Some(Field::Host)
        } else if self.draft.port.trim().is_empty() {
            Some(Field::Port)
        } else {
            None
        }
    }

    fn visible_form_problems(&self) -> Vec<String> {
        self.draft
            .problems()
            .into_iter()
            .filter(|problem| self.form_validation.shows_problem(problem))
            .collect()
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
        self.inputs
            .get(Field::Host)
            .set_placeholder(if self.draft.kind == ConnectionKind::Qos {
                "certified qOS endpoint id"
            } else {
                Field::Host.placeholder()
            });
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
            if matches!(self.screen, Screen::Form { .. }) {
                self.form_validation.touch(f);
            }
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
    let studio_node = iroh::SecretKey::from_bytes(&[17; 32]).public().to_string();
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
            &studio_node,
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
            "ada",
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
    let workshop_laptop = iroh::SecretKey::from_bytes(&[29; 32]).public().to_string();
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
            &workshop_laptop,
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
    let studio_node = iroh::SecretKey::from_bytes(&[17; 32]).public().to_string();
    let workshop_laptop = iroh::SecretKey::from_bytes(&[29; 32]).public().to_string();
    let living_room_tv = iroh::SecretKey::from_bytes(&[41; 32]).public().to_string();
    let garage_pi = iroh::SecretKey::from_bytes(&[53; 32]).public().to_string();
    vec![
        QosDevice {
            name: "Studio Node".into(),
            node: studio_node,
            online: true,
        },
        QosDevice {
            name: "Workshop Laptop".into(),
            node: workshop_laptop,
            online: true,
        },
        QosDevice {
            name: "Living Room TV".into(),
            node: living_room_tv,
            online: true,
        },
        QosDevice {
            name: "Garage Pi".into(),
            node: garage_pi,
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
    fn take_collab_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        std::mem::take(&mut self.collab_outbox)
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        let ui::collaboration::CollabEvent::WorkspaceControl { session_id, event } = event else {
            return;
        };
        let Some(LiveSession::Qos(session)) = self.live.as_mut() else {
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
                if let Some(frame) = session.decoder.decode(encoded_h264) {
                    session.size = Some((frame.width, frame.height));
                    self.fb_image = Some(PenikoImage {
                        data: Blob::new(Arc::new(frame.rgba)),
                        format: ImageFormat::Rgba8,
                        alpha_type: ImageAlphaType::Alpha,
                        width: frame.width,
                        height: frame.height,
                    });
                }
            }
            runtime_collab::WorkspaceControlEvent::Closed(reason) => {
                session.status = SessionStatus::Failed(match reason {
                    runtime_collab::WorkspaceControlCloseReason::Denied(message) => message.clone(),
                    runtime_collab::WorkspaceControlCloseReason::Revoked => {
                        "Target revoked workspace control".into()
                    }
                    runtime_collab::WorkspaceControlCloseReason::TargetDisconnected => {
                        "Target disconnected".into()
                    }
                    runtime_collab::WorkspaceControlCloseReason::RelayDisconnected => {
                        "Encrypted relay disconnected".into()
                    }
                    runtime_collab::WorkspaceControlCloseReason::LocalDisconnect => {
                        "Workspace control closed".into()
                    }
                });
            }
        }
        self.dirty = true;
    }

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
            if let Some(workspace) = resources.first_mut() {
                let host_key_issue = self.ssh_host_key_issue(id);
                workspace.children.push(crate::actions::session_resource(
                    connection,
                    can_send_text,
                    host_key_issue.as_ref(),
                ));
            }
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
        if kind == qos_actions::OperationKind::Document {
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
                self.apply(Action::GoList, &mut AutoCommit::new());
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
            crate::actions::RemoteOperation::TrustSshHostKey {
                connection_id,
                observed,
            } => {
                let connection = self
                    .workspace
                    .get(connection_id)
                    .ok_or_else(|| "saved connection no longer exists".to_string())?;
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
                    .ok_or_else(|| "saved connection no longer exists".to_string())?;
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
            crate::actions::RemoteOperation::Disconnect { .. }
            | crate::actions::RemoteOperation::TrustSshHostKey { .. }
            | crate::actions::RemoteOperation::ForgetSshHostKey { .. } => {
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
                Some(SessionStatus::Unavailable(message)) => {
                    qos_actions::InvocationUpdate::Failed {
                        invocation,
                        error: qos_actions::ActionFailure {
                            code: "remote.transport_unavailable".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    }
                }
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
            // The CRDT is authoritative after seeding. Adopt the complete
            // merged projection, not merely additions: collaborator edits,
            // favourite changes, and deletions must all become visible.
            if self.workspace.connections() != doc_conns.as_slice() {
                self.workspace.connections = doc_conns;
                if self
                    .selected
                    .and_then(|id| self.workspace.get(id))
                    .is_none()
                {
                    self.selected = self.workspace.filtered(&self.search).first().map(|c| c.id);
                }
                if self
                    .session_for
                    .is_some_and(|id| self.workspace.get(id).is_none())
                {
                    self.disconnect_session();
                }
                self.dirty = true;
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

    #[allow(
        clippy::too_many_lines,
        reason = "the root compositor intentionally keeps screen, notice, rail, and modal stacking in one ordering contract"
    )]
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
        if self.motion_reduced {
            self.rail.settle(self.rail_layout);
        } else if self.rail.tick(self.rail_layout, dt) {
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

        if let Some(notice) = self.notice.clone() {
            self.draw_notice(scene, transform, text_renderer, theme, &notice);
        }

        // A confirmation is always top-most, including an immersive session.
        // It is drawn after content so its hit regions cannot be obscured by a
        // terminal/framebuffer that may be receiving continuous updates.
        if self.pending_confirmation.is_some() {
            self.draw_confirmation(scene, transform, text_renderer, theme);
            return;
        }

        if immersive {
            return;
        }

        // In overlay mode a dimming scrim sits between the content and the
        // sliding rail; tapping it dismisses the rail. A hamburger floats at
        // the top-left of the content to summon the rail.
        if overlay {
            if reveal > 0.001 {
                // The rail is only 64px wide: retain enough separation to
                // signal a temporary overlay without reducing the underlying
                // connection list to disabled-looking contrast.
                let scrim = Color::from_rgba8(0, 0, 0, 255).at_alpha((reveal * 0.26) as f32);
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

        if self.tooltips.tick(now) {
            self.dirty = true;
        }
        if let Some((label, anchor)) = self.tooltips.visible() {
            draw_hover_tooltip(
                scene,
                transform,
                text_renderer,
                theme,
                anchor,
                label,
                self.rect,
            );
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the widget trait requires one exhaustive event router so pointer, keyboard, IME, and accessibility routing share one ordering contract"
    )]
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
        if !matches!(event, WindowEvent::CursorMoved { .. }) && self.tooltips.dismiss() {
            self.dirty = true;
        }
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
            WindowEvent::MouseWheel { delta, .. }
                if self.pending_confirmation.is_none() && matches!(self.screen, Screen::List) =>
            {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 48.0,
                    MouseScrollDelta::PixelDelta(position) => -position.y / scale_factor,
                };
                let next = (self.list_scroll + dy).clamp(0.0, self.list_scroll_max);
                if (next - self.list_scroll).abs() > f64::EPSILON {
                    self.list_scroll = next;
                    self.dirty = true;
                }
                *handled = true;
            }
            WindowEvent::MouseInput { state, button, .. }
                if self.pending_confirmation.is_none()
                    && self.qos_pointer_target(self.pointer).is_some() =>
            {
                let button = match button {
                    MouseButton::Left => Some(272),
                    MouseButton::Right => Some(273),
                    MouseButton::Middle => Some(274),
                    _ => None,
                };
                if let Some(button) = button {
                    if *state == ElementState::Pressed && button == 272 {
                        self.queue_qos_input(
                            runtime_collab::WorkspaceControlInput::KeyboardFocus { focused: true },
                        );
                    }
                    self.queue_qos_input(runtime_collab::WorkspaceControlInput::PointerButton {
                        button,
                        pressed: *state == ElementState::Pressed,
                    });
                }
                *handled = true;
            }
            // A press/release over the live VNC canvas is injected into the
            // remote desktop rather than hit-testing the local chrome.
            WindowEvent::MouseInput { state, button, .. }
                if self.pending_confirmation.is_none()
                    && self.vnc_pointer_target(self.pointer).is_some() =>
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
                        physical_key: PhysicalKey::Code(code),
                        state: key_state,
                        ..
                    },
                ..
            } if self.pending_confirmation.is_none()
                && matches!(self.live.as_ref(), Some(LiveSession::Qos(session)) if session.status == SessionStatus::Connected)
                && !(self.fullscreen && *code == KeyCode::Escape) =>
            {
                if let Some(keycode) = Self::qos_keycode(*code) {
                    self.queue_qos_input(runtime_collab::WorkspaceControlInput::Key {
                        keycode,
                        pressed: *key_state == ElementState::Pressed,
                        modifiers: 0,
                    });
                    *handled = true;
                }
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

    #[allow(
        clippy::too_many_lines,
        reason = "the custom canvas has one explicit semantic mirror for its three screens"
    )]
    fn accessibility(&self) -> Option<AccessNode> {
        let focusable = || AccessState::new().focusable(true);
        let button = |name: &str, target: String, bounds: Option<Rect>| {
            let mut node = AccessNode::new(Role::Button)
                .with_name(name)
                .with_action_target(target)
                .with_state(focusable());
            if let Some(bounds) = bounds {
                node = node.with_bounds(bounds);
            }
            node
        };

        let mut nav = Vec::new();
        if self.rail_layout == SplitLayout::Overlay {
            nav.push(
                AccessNode::new(Role::Button)
                    .with_name(if self.rail_open() {
                        "Close navigation"
                    } else {
                        "Open navigation"
                    })
                    .with_description("Show Connections and Add connection")
                    .with_action_target("remote.nav.toggle")
                    .with_optional_bounds(self.action_bounds(Action::ToggleRail))
                    .with_state(focusable().expanded(self.rail_open())),
            );
        }
        if self.rail_layout == SplitLayout::Docked || self.rail_open() {
            nav.push(button(
                "Connections",
                "remote.nav.list".into(),
                self.action_bounds(Action::GoList),
            ));
            nav.push(button(
                "Add connection",
                "remote.connection.new".into(),
                self.action_bounds(Action::NewConnection),
            ));
        }
        let nav = AccessNode::new(Role::Toolbar)
            .with_name("Remote navigation")
            .with_children(nav);

        let content = match self.screen {
            Screen::List => {
                let visible = self.workspace.filtered(&self.search);
                let search = AccessNode::new(Role::TextInput)
                    .with_name("Search saved connections")
                    .with_description("Shortcut: slash or Control F")
                    .with_value(self.search.clone())
                    .with_action_target("remote.field:search")
                    .with_optional_bounds(self.action_bounds(Action::FocusField(Field::Search)))
                    .with_state(focusable().focused(self.focus == Some(Field::Search)));
                let mut children = Vec::new();
                for (index, connection) in visible.iter().enumerate() {
                    let id = connection.id;
                    let presence = match connection.presence {
                        Presence::Online => "online",
                        Presence::Offline => "offline",
                        Presence::Unknown => "status unknown",
                    };
                    let mut actions = vec![
                        AccessNode::new(Role::ToggleButton)
                            .with_name(if connection.favorite {
                                "Remove from favourites"
                            } else {
                                "Add to favourites"
                            })
                            .with_action_target(format!("remote.connection:{id}:favorite"))
                            .with_optional_bounds(self.action_bounds(Action::ToggleFavorite(id)))
                            .with_state(focusable().checked(connection.favorite)),
                        button(
                            "Edit connection",
                            format!("remote.connection:{id}:edit"),
                            self.action_bounds(Action::EditConnection(id)),
                        ),
                    ];
                    if connection_readiness(connection).can_connect() {
                        actions.push(button(
                            "Connect",
                            format!("remote.connection:{id}:connect"),
                            self.action_bounds(Action::RequestConnect(id)),
                        ));
                    }
                    let readiness = connection_readiness(connection);
                    let availability = if readiness.can_connect() {
                        "ready to connect".to_string()
                    } else if readiness == TransportReadiness::InvalidProfile {
                        connection
                            .profile_issue()
                            .unwrap_or_else(|| readiness.detail(connection.kind))
                            .to_string()
                    } else {
                        readiness.detail(connection.kind).to_string()
                    };
                    let row_name = format!(
                        "{}, {}, {}, {}, last used {}",
                        connection.name,
                        connection.kind.label(),
                        presence,
                        availability,
                        connection.last_used
                    );
                    let mut row = AccessNode::new(Role::ListItem)
                        .with_name(row_name)
                        .with_value(connection.address())
                        .with_action_target(format!("remote.connection:{id}:select"))
                        .with_position(index + 1, visible.len())
                        .with_state(focusable().selected(self.selected == Some(id)))
                        .with_children(actions);
                    if let Some(bounds) = self.action_bounds(Action::SelectRow(id)) {
                        row = row.with_bounds(bounds);
                    }
                    children.push(row);
                }
                let list = AccessNode::new(Role::List)
                    .with_name("Saved connections")
                    .with_hashed_key("remote.list")
                    .with_children(children);
                let mut screen = vec![
                    AccessNode::new(Role::Heading { level: 1 })
                        .with_name("Remote Machines")
                        .with_hashed_key("remote.list.heading"),
                    button(
                        "New connection",
                        "remote.connection.new".into(),
                        self.action_bounds(Action::NewConnection),
                    ),
                    search,
                    list,
                ];
                if visible.is_empty() {
                    screen.push(
                        AccessNode::new(Role::StaticText)
                            .with_name(if self.workspace.connections().is_empty() {
                                "No machines yet. Add a connection to reach it over SSH, VNC, or the qOS relay."
                            } else {
                                "No machines match your search."
                            })
                            .with_live(Politeness::Polite)
                            .with_hashed_key("remote.list.empty"),
                    );
                }
                AccessNode::new(Role::Group)
                    .with_name("Connections")
                    .with_children(screen)
            }
            Screen::Form { editing } => {
                let title = if editing.is_some() {
                    "Edit connection"
                } else {
                    "New connection"
                };
                let mut children = vec![
                    AccessNode::new(Role::Heading { level: 1 })
                        .with_name(title)
                        .with_hashed_key("remote.form.heading"),
                ];
                let protocol = ConnectionKind::available_to_create()
                    .into_iter()
                    .map(|kind| {
                        AccessNode::new(Role::RadioButton)
                            .with_name(kind.label())
                            .with_action_target(format!("remote.kind:{}", kind.tag()))
                            .with_optional_bounds(self.action_bounds(Action::SetKind(kind)))
                            .with_state(focusable().checked(self.draft.kind == kind))
                    })
                    .collect();
                children.push(
                    AccessNode::new(Role::Group)
                        .with_name("Protocol")
                        .with_hashed_key("remote.form.protocol")
                        .with_children(protocol),
                );
                if self.draft.kind == ConnectionKind::Rdp {
                    children.push(
                        AccessNode::new(Role::StaticText)
                            .with_name("Legacy RDP profile. RDP has no client backend in this build; select SSH, VNC, or qOS to migrate this profile.")
                            .with_live(Politeness::Polite)
                            .with_hashed_key("remote.form.legacy-rdp"),
                    );
                }
                if self.draft.kind == ConnectionKind::Qos {
                    children.push(
                        AccessNode::new(Role::List)
                            .with_name("Discovered qOS devices")
                            .with_hashed_key("remote.form.devices")
                            .with_children(
                                self.qos_devices
                                    .iter()
                                    .enumerate()
                                    .map(|(index, device)| {
                                        AccessNode::new(Role::ListItem)
                                            .with_name(device.name.clone())
                                            .with_value(device.node.clone())
                                            .with_description(if device.online {
                                                "Online"
                                            } else {
                                                "Offline"
                                            })
                                            .with_action_target(format!("remote.device:{index}"))
                                            .with_optional_bounds(
                                                self.action_bounds(Action::PickQosDevice(index)),
                                            )
                                            .with_state(
                                                focusable()
                                                    .selected(self.draft.host == device.node),
                                            )
                                    })
                                    .collect(),
                            ),
                    );
                    children.push(
                        AccessNode::new(Role::StaticText)
                            .with_name(
                                "Authentication: qOS device identity. No password is stored or requested.",
                            )
                            .with_hashed_key("remote.form.qos-authentication"),
                    );
                }
                for (field, name) in [
                    (Field::Name, "Connection name"),
                    (
                        Field::Host,
                        if self.draft.kind == ConnectionKind::Qos {
                            "Device node"
                        } else {
                            "Host name or IP address"
                        },
                    ),
                    (Field::Port, "Port"),
                    (Field::User, "User name"),
                    (Field::Group, "Group"),
                ] {
                    if (self.draft.kind == ConnectionKind::Qos
                        && matches!(field, Field::Port | Field::User | Field::Group))
                        || (self.draft.kind == ConnectionKind::Vnc && field == Field::User)
                    {
                        continue;
                    }
                    children.push(
                        AccessNode::new(Role::TextInput)
                            .with_name(name)
                            .with_value(self.field_value(field))
                            .with_action_target(format!("remote.field:{}", field.target_name()))
                            .with_optional_bounds(self.action_bounds(Action::FocusField(field)))
                            .with_state(focusable().focused(self.focus == Some(field))),
                    );
                }
                if self.draft.kind != ConnectionKind::Qos {
                    children.push(
                        AccessNode::new(Role::Group)
                            .with_name("Authentication")
                            .with_hashed_key("remote.form.authentication")
                            .with_children(
                                AuthMethod::available_for(self.draft.kind)
                                    .iter()
                                    .copied()
                                    .map(|auth| {
                                        AccessNode::new(Role::RadioButton)
                                            .with_name(auth.label())
                                            .with_action_target(format!(
                                                "remote.auth:{}",
                                                auth.target_name()
                                            ))
                                            .with_optional_bounds(
                                                self.action_bounds(Action::SetAuth(auth)),
                                            )
                                            .with_state(
                                                focusable().checked(self.draft.auth == auth),
                                            )
                                    })
                                    .collect(),
                            ),
                    );
                }
                let problems = self.visible_form_problems();
                if !problems.is_empty() {
                    children.push(
                        AccessNode::new(Role::StaticText)
                            .with_name(problems.join(". "))
                            .with_live(Politeness::Polite)
                            .with_hashed_key("remote.form.problems"),
                    );
                }
                let mut form_actions = vec![
                    button(
                        "Cancel",
                        "remote.form.cancel".into(),
                        self.action_bounds(Action::Back),
                    ),
                    AccessNode::new(Role::Button)
                        .with_name("Save connection")
                        .with_action_target("remote.form.save")
                        .with_optional_bounds(self.action_bounds(Action::SaveForm))
                        .with_state(focusable()),
                ];
                if let Some(id) = editing {
                    form_actions.push(button(
                        "Delete connection",
                        format!("remote.connection:{id}:delete"),
                        self.action_bounds(Action::RequestDelete(id)),
                    ));
                }
                children.push(
                    AccessNode::new(Role::Toolbar)
                        .with_name("Form actions")
                        .with_hashed_key("remote.form.actions")
                        .with_children(form_actions),
                );
                AccessNode::new(Role::Group)
                    .with_name(title)
                    .with_children(children)
            }
            Screen::Session { id } => {
                let connection = self.workspace.get(id);
                let status = self.live.as_ref().map(LiveSession::status);
                let status_label = match &status {
                    None => "Not connected".to_string(),
                    Some(SessionStatus::Connecting) => "Connecting".to_string(),
                    Some(SessionStatus::Connected) => "Connected".to_string(),
                    Some(SessionStatus::Closed) => "Session ended".to_string(),
                    Some(SessionStatus::Failed(error)) => format!("Connection failed: {error}"),
                    Some(SessionStatus::Unavailable(detail)) => {
                        format!("Not available: {detail}")
                    }
                };
                let mut children = vec![
                    AccessNode::new(Role::Heading { level: 1 })
                        .with_name(
                            connection
                                .map_or("Remote session", |connection| connection.name.as_str()),
                        )
                        .with_hashed_key("remote.session.heading"),
                    AccessNode::new(Role::StaticText)
                        .with_name(status_label)
                        .with_live(Politeness::Polite)
                        .with_hashed_key("remote.session.status"),
                ];
                if status == Some(SessionStatus::Connected) {
                    children.push(
                        AccessNode::new(if connection.is_some_and(|c| c.kind.is_terminal()) {
                            Role::Document
                        } else {
                            Role::Canvas
                        })
                        .with_name(if connection.is_some_and(|c| c.kind.is_terminal()) {
                            "Interactive SSH terminal"
                        } else {
                            "Interactive remote desktop"
                        })
                        .with_hashed_key("remote.session.surface")
                        .with_description(
                            "Keyboard and pointer input are sent to the remote machine",
                        ),
                    );
                    if connection.is_some_and(|c| c.kind.streams_framebuffer()) {
                        children.push(
                            AccessNode::new(Role::ToggleButton)
                                .with_name("Scale remote desktop to fit")
                                .with_action_target("remote.session.scale")
                                .with_optional_bounds(self.action_bounds(Action::ToggleScale))
                                .with_state(focusable().checked(self.scale_to_fit)),
                        );
                        children.push(button(
                            if self.fullscreen {
                                "Exit fullscreen"
                            } else {
                                "Enter fullscreen"
                            },
                            "remote.session.fullscreen".into(),
                            self.action_bounds(Action::ToggleFullscreen),
                        ));
                    }
                }
                if matches!(status, Some(SessionStatus::Failed(_))) {
                    children.push(button(
                        "Retry connection",
                        format!("remote.connection:{id}:connect"),
                        self.action_bounds(Action::RequestConnect(id)),
                    ));
                }
                if matches!(status, Some(SessionStatus::Unavailable(_))) {
                    children.push(button(
                        "Edit connection",
                        format!("remote.connection:{id}:edit"),
                        self.action_bounds(Action::EditConnection(id)),
                    ));
                }
                children.push(button(
                    if status == Some(SessionStatus::Connected) {
                        "Disconnect"
                    } else {
                        "Back to list"
                    },
                    "remote.session.disconnect".into(),
                    self.action_bounds(Action::Disconnect),
                ));
                AccessNode::new(Role::Group)
                    .with_name("Remote session")
                    .with_children(children)
            }
        };

        let mut root_children = vec![nav, content];
        if let Some(pending) = self.pending_confirmation.as_ref() {
            let name = self
                .workspace
                .get(pending.id())
                .map_or("this saved connection", |connection| {
                    connection.name.as_str()
                });
            let (title, detail, primary) = match pending {
                PendingConfirmation::Delete { .. } => (
                    format!("Delete {name}?"),
                    "This removes the shared saved profile, not the remote machine.".to_string(),
                    "Delete",
                ),
                PendingConfirmation::Connect { .. } => (
                    format!("Connect to {name}?"),
                    "The target and access are rechecked before a remote session opens.".to_string(),
                    "Connect",
                ),
                PendingConfirmation::TrustSshHostKey { observed, .. } => (
                    format!("Trust SSH key for {name}?"),
                    format!(
                        "Verify {} {} out of band. This device-private trust is checked again before connecting.",
                        observed.algorithm, observed.fingerprint
                    ),
                    "Trust key",
                ),
                PendingConfirmation::ForgetSshHostKey { .. } => (
                    format!("Forget SSH key for {name}?"),
                    "Remove this device-private pin, then reconnect to inspect a replacement key before trusting it.".to_string(),
                    "Forget key",
                ),
            };
            root_children.push(
                AccessNode::new(Role::Dialog)
                    .with_name(title)
                    .with_description(detail)
                    .with_bounds(self.rect)
                    .with_state(AccessState::new().modal(true).focusable(true))
                    .with_children(vec![
                        button(
                            "Cancel",
                            "remote.confirmation.cancel".into(),
                            self.action_bounds(Action::CancelPending),
                        ),
                        button(
                            primary,
                            "remote.confirmation.confirm".into(),
                            self.action_bounds(Action::ConfirmPending),
                        ),
                    ]),
            );
        }
        if let Some(notice) = self.notice.as_ref() {
            root_children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(notice.clone())
                    .with_live(Politeness::Polite)
                    .with_hashed_key("remote.action.notice"),
            );
        }

        Some(
            AccessNode::new(Role::Window)
                .with_name("Remote")
                .with_bounds(self.rect)
                .with_children(root_children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        use ui::a11y::Action as A11yAction;
        if self.pending_confirmation.is_some() {
            let routed = match (target, action) {
                ("remote.confirmation.cancel", A11yAction::Activate) => Action::CancelPending,
                ("remote.confirmation.confirm", A11yAction::Activate) => Action::ConfirmPending,
                _ => return false,
            };
            self.apply(routed, state);
            return true;
        }
        if let Some(name) = target.strip_prefix("remote.field:") {
            let Some(field) = Field::from_target_name(name) else {
                return false;
            };
            match action {
                A11yAction::Activate | A11yAction::Focus => self.focus_field(field),
                A11yAction::SetValue(value) => self.set_field_value(field, value),
                _ => return false,
            }
            self.dirty = true;
            return true;
        }
        if let Some(tag) = target.strip_prefix("remote.kind:")
            && matches!(action, A11yAction::Activate | A11yAction::ToggleCheck)
        {
            self.apply(Action::SetKind(ConnectionKind::from_tag(tag)), state);
            return true;
        }
        if let Some(name) = target.strip_prefix("remote.auth:")
            && matches!(action, A11yAction::Activate | A11yAction::ToggleCheck)
        {
            let auth = AuthMethod::from_target_name(name);
            if auth.is_supported_by(self.draft.kind) {
                self.apply(Action::SetAuth(auth), state);
                return true;
            }
            return false;
        }
        if let Some(index) = target
            .strip_prefix("remote.device:")
            .and_then(|value| value.parse::<usize>().ok())
            && matches!(action, A11yAction::Activate | A11yAction::Focus)
        {
            self.apply(Action::PickQosDevice(index), state);
            return true;
        }
        if let Some(rest) = target.strip_prefix("remote.connection:") {
            let Some((raw_id, verb)) = rest.rsplit_once(':') else {
                return false;
            };
            let Ok(id) = raw_id.parse::<ConnectionId>() else {
                return false;
            };
            if verb == "select" && matches!(action, A11yAction::ScrollIntoView) {
                let visible = self.workspace.filtered(&self.search);
                let Some(index) = visible.iter().position(|connection| connection.id == id) else {
                    return false;
                };
                self.selected = Some(id);
                self.scroll_connection_into_view(index, visible.len());
                self.dirty = true;
                return true;
            }
            let routed = match (verb, action) {
                ("select", A11yAction::Activate | A11yAction::Focus) => Action::SelectRow(id),
                ("favorite", A11yAction::Activate | A11yAction::ToggleCheck) => {
                    Action::ToggleFavorite(id)
                }
                ("edit", A11yAction::Activate) => Action::EditConnection(id),
                ("connect", A11yAction::Activate) => Action::RequestConnect(id),
                ("delete", A11yAction::Activate) => Action::RequestDelete(id),
                _ => return false,
            };
            self.apply(routed, state);
            return true;
        }
        let routed = match (target, action) {
            ("remote.nav.toggle", A11yAction::Activate) => Action::ToggleRail,
            ("remote.nav.list", A11yAction::Activate) => Action::GoList,
            ("remote.connection.new", A11yAction::Activate) => Action::NewConnection,
            ("remote.form.cancel", A11yAction::Activate) => Action::Back,
            ("remote.form.save", A11yAction::Activate) => Action::SaveForm,
            ("remote.session.disconnect", A11yAction::Activate) => Action::Disconnect,
            ("remote.session.scale", A11yAction::Activate | A11yAction::ToggleCheck) => {
                Action::ToggleScale
            }
            ("remote.session.fullscreen", A11yAction::Activate) => Action::ToggleFullscreen,
            _ => return false,
        };
        self.apply(routed, state);
        true
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn needs_animation_frame(&self) -> bool {
        // While the rail's overlay/dock slide is mid-flight, keep the host
        // pumping frames so the eased transition plays smoothly. A live
        // session likewise needs continuous frames so incoming remote data
        // (PTY output, framebuffer updates) is drained and rendered.
        (!self.motion_reduced && self.rail.is_animating(self.rail_layout))
            || self.tooltips.needs_frame()
            || self.live_active()
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
        if reduced {
            self.rail.settle(self.rail_layout);
        }
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
            if self.pending_confirmation.is_none() {
                self.forward_vnc_pointer(p);
                if let Some((x, y)) = self.qos_pointer_target(p) {
                    self.queue_qos_input(runtime_collab::WorkspaceControlInput::PointerMotion {
                        x,
                        y,
                    });
                }
            }
        }
        let tip = self
            .hits
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(p))
            .and_then(|hit| match hit.action {
                Action::GoList => Some((hit.rect, "Connections".to_string())),
                Action::NewConnection => Some((hit.rect, "Add connection (Ctrl+N)".to_string())),
                Action::ToggleRail => Some((
                    hit.rect,
                    if self.rail_open() {
                        "Close navigation"
                    } else {
                        "Open navigation"
                    }
                    .to_string(),
                )),
                Action::ToggleFavorite(id) => self.workspace.get(id).map(|connection| {
                    (
                        hit.rect,
                        if connection.favorite {
                            "Remove from favourites"
                        } else {
                            "Add to favourites"
                        }
                        .to_string(),
                    )
                }),
                _ => None,
            });
        if self.tooltips.on_pointer(tip, std::time::Instant::now()) {
            self.dirty = true;
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

        // A pending privileged/destructive action is modal. Never allow a
        // click through it to connect, send input, or mutate a profile.
        if self.pending_confirmation.is_some() {
            if let Some(action) = self
                .hits
                .iter()
                .rev()
                .find(|hit| hit.rect.contains(p))
                .map(|hit| hit.action)
                && matches!(action, Action::ConfirmPending | Action::CancelPending)
            {
                self.apply(action, state);
            }
            *handled = true;
            return;
        }

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
        if self.pending_confirmation.is_some() {
            match key {
                Key::Named(NamedKey::Escape) => self.apply(Action::CancelPending, state),
                Key::Named(NamedKey::Enter) => self.apply(Action::ConfirmPending, state),
                _ => return,
            }
            *handled = true;
            return;
        }
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
                            self.apply(Action::RequestConnect(id), state);
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
            self.scroll_connection_into_view(next, filtered.len());
        }
        self.dirty = true;
    }

    fn scroll_connection_into_view(&mut self, index: usize, count: usize) {
        let content_width = (self.rect.width() - RAIL_W).max(0.0);
        let row_h = if content_width < 620.0 {
            COMPACT_ROW_H
        } else {
            ROW_H
        };
        let stride = row_h + 10.0;
        let viewport = (self.rect.height() - (HEADER_H + 56.0)).max(row_h);
        self.list_scroll_max = (count as f64 * stride - 10.0 - viewport).max(0.0);
        let top = index as f64 * stride;
        let bottom = top + row_h;
        if top < self.list_scroll {
            self.list_scroll = top;
        } else if bottom > self.list_scroll + viewport {
            self.list_scroll = bottom - viewport;
        }
        self.list_scroll = self.list_scroll.clamp(0.0, self.list_scroll_max);
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
            tokens.colors.lift(0.72)
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
    #[allow(
        clippy::too_many_lines,
        reason = "the custom list keeps responsive header, empty state, scrolling, and rows in one geometry pass so hit regions match what is drawn"
    )]
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
        let compact_header = content.width() < 520.0;
        text_renderer.draw_text_weighted(
            scene,
            if compact_header {
                "Remote"
            } else {
                "Remote Machines"
            },
            22.0,
            tokens.colors.text_primary,
            transform * Affine::translate((header.x0 + pad, header.y0 + 14.0)),
            None,
            TextWeight::Bold,
        );
        let total = self.workspace.connections().len();
        let online = self.workspace.online_count();
        let connectable = self
            .workspace
            .connections()
            .iter()
            .filter(|connection| connection_readiness(connection).can_connect())
            .count();
        let stat = if connectable < total {
            format!("{total} saved · {online} online · {connectable} connectable here")
        } else {
            format!("{total} saved · {online} online")
        };
        text_renderer.draw_text(
            scene,
            &stat,
            13.0,
            tokens.colors.text_dim,
            transform * Affine::translate((header.x0 + pad, header.y0 + 40.0)),
            None,
        );

        // New-connection button (top-right).
        let add_w = if compact_header { 88.0 } else { 150.0 };
        let add = Rect::new(
            content.x1 - pad - add_w,
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
            if compact_header {
                "+ New"
            } else {
                "+ New connection"
            },
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
        let rows: Vec<Connection> = self
            .workspace
            .filtered(&self.search)
            .into_iter()
            .cloned()
            .collect();

        if rows.is_empty() {
            self.list_scroll = 0.0;
            self.list_scroll_max = 0.0;
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
        let row_h = if content.width() < 620.0 {
            COMPACT_ROW_H
        } else {
            ROW_H
        };
        let stride = row_h + 10.0;
        let content_height = rows.len() as f64 * stride - 10.0;
        self.list_scroll_max = (content_height - list_clip.height()).max(0.0);
        self.list_scroll = self.list_scroll.clamp(0.0, self.list_scroll_max);
        let mut y = list_top - self.list_scroll;
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &list_clip,
        );
        for c in &rows {
            let row = Rect::new(content.x0 + pad, y, content.x1 - pad, y + row_h);
            if row.y1 >= list_clip.y0 && row.y0 <= list_clip.y1 {
                self.draw_connection_row(scene, transform, text_renderer, theme, row, c);
            }
            y += stride;
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
        let compact = row.height() > ROW_H;
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
        if !compact && !c.group.is_empty() {
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
        let connect = if compact {
            Rect::new(row.x1 - 108.0, row.y0 + 72.0, row.x1 - 12.0, row.y0 + 104.0)
        } else {
            Rect::new(
                row.x1 - 116.0,
                row.center().y - 16.0,
                row.x1 - 16.0,
                row.center().y + 16.0,
            )
        };
        let readiness = connection_readiness(c);
        let can_connect = readiness.can_connect();
        if can_connect {
            self.draw_button(
                scene,
                transform,
                text_renderer,
                theme,
                connect,
                "Connect",
                true,
            );
            self.push_hit(connect, Action::RequestConnect(c.id));
        } else {
            Self::draw_readiness_badge(scene, transform, text_renderer, theme, connect, readiness);
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
            transform
                * Affine::translate(if compact {
                    (row.x0 + 44.0, row.y0 + 81.0)
                } else {
                    (edit.x0 - 20.0 - f64::from(uw), row.center().y - 7.0)
                }),
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

    /// Small, non-interactive readiness marker for profiles that cannot be
    /// connected from this process. It keeps the restriction visible at the
    /// point of decision instead of hiding it in a disabled palette row.
    fn draw_readiness_badge(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        readiness: TransportReadiness,
    ) {
        let tokens = &theme.tokens;
        let (color, label) = match readiness {
            TransportReadiness::Ready => return,
            TransportReadiness::NeedsLiveSessionCapability => {
                (tokens.colors.semantic.warning, "Needs access")
            }
            TransportReadiness::BackendUnavailable => (tokens.colors.neutral[5], "Not supported"),
            TransportReadiness::InvalidProfile => (tokens.colors.semantic.warning, "Fix profile"),
        };
        let badge = RoundedRect::from_rect(rect, 8.0);
        scene.fill(Fill::NonZero, transform, color.at_alpha(0.12), None, &badge);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            color.at_alpha(0.75),
            None,
            &badge,
        );
        let (tw, _) = text_renderer.measure_weighted(label, 11.5, None, TextWeight::Semibold);
        text_renderer.draw_text_weighted(
            scene,
            label,
            11.5,
            color,
            transform
                * Affine::translate((rect.center().x - f64::from(tw) / 2.0, rect.center().y - 7.0)),
            None,
            TextWeight::Semibold,
        );
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
        clippy::too_many_lines,
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
        let y = self.draw_form_legacy_transport_notice(
            scene,
            transform,
            text_renderer,
            theme,
            card_x,
            card_w,
            y,
        );
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
        clippy::too_many_lines,
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

    /// Legacy RDP profiles are deliberately editable rather than discarded.
    /// Make their migration path explicit: the selector offers only real
    /// backends, so choosing SSH, VNC, or qOS replaces the unavailable kind.
    #[allow(
        clippy::too_many_arguments,
        reason = "this small drawing section receives the same scene/render context and geometry cursor as the adjacent form sections"
    )]
    fn draw_form_legacy_transport_notice(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        card_x: f64,
        card_w: f64,
        y: f64,
    ) -> f64 {
        if self.draft.kind != ConnectionKind::Rdp {
            return y;
        }
        let tokens = &theme.tokens;
        let note = Rect::new(card_x, y, card_x + card_w, y + 54.0);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.semantic.warning.at_alpha(0.12),
            None,
            &RoundedRect::from_rect(note, 8.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.semantic.warning.at_alpha(0.75),
            None,
            &RoundedRect::from_rect(note, 8.0),
        );
        text_renderer.draw_text_weighted(
            scene,
            "Legacy RDP profile",
            12.0,
            tokens.colors.semantic.warning,
            transform * Affine::translate((note.x0 + 12.0, note.y0 + 9.0)),
            None,
            TextWeight::Semibold,
        );
        text_renderer.draw_text(
            scene,
            "RDP has no client backend here. Select SSH, VNC, or qOS to migrate this profile.",
            11.5,
            tokens.colors.text_dim,
            transform * Affine::translate((note.x0 + 12.0, note.y0 + 29.0)),
            Some((card_w - 24.0) as f32),
        );
        y + 66.0
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
                    tokens.colors.lift(0.72)
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
                let compact_node = crate::model::compact_qos_endpoint(&d.node);
                text_renderer.draw_text(
                    scene,
                    &compact_node,
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
        clippy::too_many_lines,
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
            let has_user = self.draft.kind != ConnectionKind::Vnc;
            y = Self::draw_form_label(
                scene,
                transform,
                text_renderer,
                theme,
                card_x,
                y,
                if has_user { "User" } else { "Group" },
            );
            if has_user {
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
            }
            let group = if has_user {
                Rect::new(card_x + card_w.midpoint(12.0), y, card_x + card_w, y + 40.0)
            } else {
                Rect::new(card_x, y, card_x + card_w, y + 40.0)
            };
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
            let auth_methods = AuthMethod::available_for(self.draft.kind);
            let aseg = card_w / auth_methods.len() as f64;
            for (i, a) in auth_methods.iter().copied().enumerate() {
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
                    i.saturating_add(1) == auth_methods.len(),
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
        let problems = self.visible_form_problems();
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
        let y = if problems.is_empty() {
            text_renderer.draw_text(
                scene,
                "Enter a name and target to save this connection.",
                12.5,
                tokens.colors.text_dim,
                transform * Affine::translate((card_x, y)),
                Some(card_w as f32),
            );
            y + 28.0
        } else {
            y + 28.0
        };

        // -- Save + Cancel --
        let save = Rect::new(card_x + card_w - 120.0, y, card_x + card_w, y + 40.0);
        self.draw_button(scene, transform, text_renderer, theme, save, "Save", true);
        self.push_hit(save, Action::SaveForm);
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
            self.push_hit(del, Action::RequestDelete(id));
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
            tokens.colors.lift(0.72)
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
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "the live-state match deliberately composes all session status surfaces beside the shared toolbar and canvas geometry"
    )]
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

        // No live transport object: the honest "not connected" card. Only
        // app-owned SSH/VNC transports use the developer gate; production qOS
        // control always creates a real broker session or an honest error.
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
                    "This transport is unavailable in this build.".to_string(),
                    "Your saved connection is unchanged.".to_string(),
                ],
                StatusTone::Neutral,
                false,
            );
            return;
        };

        // Live path: reflect the real transport state.
        match status {
            SessionStatus::Connecting => {
                let (title, detail) = if c.kind == ConnectionKind::Qos {
                    (
                        "Waiting for consent…",
                        "Opening an authenticated route to the exact target, then waiting for it to share a workspace surface…",
                    )
                } else {
                    (
                        "Connecting…",
                        "Negotiating the transport and authenticating…",
                    )
                };
                self.draw_status_session(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    content,
                    &c,
                    title,
                    &[detail.to_string()],
                    StatusTone::Pending,
                    false,
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
                    false,
                );
            }
            SessionStatus::Unavailable(detail) => {
                let recovery = match c.kind {
                    ConnectionKind::Rdp => {
                        "Edit this saved profile to use SSH, VNC, or qOS device control."
                    }
                    ConnectionKind::Ssh | ConnectionKind::Vnc => {
                        "Use qOS device control now, or install Remote with live network and host-key trust access."
                    }
                    ConnectionKind::Qos => "No connection was attempted.",
                };
                self.draw_status_session(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    content,
                    &c,
                    "Connection not available",
                    &[detail, recovery.to_string()],
                    StatusTone::Neutral,
                    true,
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
                    false,
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
        show_edit: bool,
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
        self.draw_status_session_actions(
            scene,
            transform,
            text_renderer,
            theme,
            card,
            tone,
            c,
            y,
            show_edit,
        );
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
            ui::tokens::colors::Colors::hairline_on(tokens.colors.surfaces.raised, 51),
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
        show_edit: bool,
    ) {
        let mid = card.center().x;
        // Retry only makes sense after a real failure. An unavailable saved
        // profile gets a direct edit recovery instead; it must never look as
        // though retrying would somehow install an RDP backend or grant a
        // denied sandbox capability.
        let host_key_recovery = if c.kind == ConnectionKind::Ssh {
            match self.ssh_host_key_issue(c.id) {
                Some(crate::ssh::HostKeyIssue::FirstContact { .. }) => {
                    Some(("Trust key", Action::RequestTrustSshHostKey(c.id)))
                }
                Some(crate::ssh::HostKeyIssue::Changed { .. }) => {
                    Some(("Forget key", Action::RequestForgetSshHostKey(c.id)))
                }
                _ => None,
            }
        } else {
            None
        };
        let can_retry = matches!(tone, StatusTone::Error) && host_key_recovery.is_none();
        let btn_h = 38.0;
        let back_w = 132.0;
        let retry_w = 104.0;
        let gap = 12.0;
        let group_w = if can_retry || show_edit || host_key_recovery.is_some() {
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
            self.push_hit(retry, Action::RequestConnect(c.id));
            bx += retry_w + gap;
        }
        if let Some((label, action)) = host_key_recovery {
            let recover = Rect::new(bx, y, bx + retry_w, y + btn_h);
            self.draw_button(
                scene,
                transform,
                text_renderer,
                theme,
                recover,
                label,
                false,
            );
            self.push_hit(recover, action);
            bx += retry_w + gap;
        }
        if show_edit {
            let edit = Rect::new(bx, y, bx + retry_w, y + btn_h);
            self.draw_button(
                scene,
                transform,
                text_renderer,
                theme,
                edit,
                "Edit profile",
                false,
            );
            self.push_hit(edit, Action::EditConnection(c.id));
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
            format!("· {} · workspace stream", c.address())
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

        // Framebuffer-only controls. Clipboard is intentionally absent until
        // the transports implement a real, consent-aware synchronization
        // channel; a clickable no-op would lie about a security-sensitive
        // capability.
        if c.kind.streams_framebuffer() {
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
            tokens.colors.lift(0.72)
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
    let order: Vec<Field> = match kind {
        ConnectionKind::Qos => vec![Field::Name, Field::Host],
        ConnectionKind::Vnc => vec![Field::Name, Field::Host, Field::Port, Field::Group],
        ConnectionKind::Ssh | ConnectionKind::Rdp => {
            vec![
                Field::Name,
                Field::Host,
                Field::Port,
                Field::User,
                Field::Group,
            ]
        }
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
    fn collaborator_edits_and_deletes_replace_the_local_projection() {
        let mut app = RemoteApp::new_demo();
        let id = app.workspace.connections()[0].id;
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);

        RemoteDoc::rename(&mut state, id, "Renamed by collaborator");
        app.sync(&mut state, &automerge::ROOT);
        assert_eq!(
            app.workspace
                .get(id)
                .expect("remote edit remains present")
                .name,
            "Renamed by collaborator"
        );

        RemoteDoc::remove(&mut state, id);
        app.sync(&mut state, &automerge::ROOT);
        assert!(
            app.workspace.get(id).is_none(),
            "a collaborator's deletion cannot leave a local ghost row"
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

    fn find_semantic_target<'a>(node: &'a AccessNode, target: &str) -> Option<&'a AccessNode> {
        if node.action_target.as_deref() == Some(target) {
            return Some(node);
        }
        node.children
            .iter()
            .find_map(|child| find_semantic_target(child, target))
    }

    fn find_live_region(node: &AccessNode) -> Option<&AccessNode> {
        if node.live.is_some() {
            return Some(node);
        }
        node.children.iter().find_map(find_live_region)
    }

    #[test]
    fn accessibility_exposes_and_routes_every_connection_control() {
        let mut app = RemoteApp::new_demo();
        lay_out(&mut app, 640.0, 900.0);
        let id = app
            .workspace
            .connections()
            .iter()
            .find(|connection| connection_readiness(connection).can_connect())
            .expect("demo has a production-available qOS connection")
            .id;
        let tree = app.accessibility().expect("Remote window semantics");
        for target in [
            "remote.connection.new".to_string(),
            "remote.field:search".to_string(),
            format!("remote.connection:{id}:select"),
            format!("remote.connection:{id}:favorite"),
            format!("remote.connection:{id}:edit"),
            format!("remote.connection:{id}:connect"),
        ] {
            assert!(
                find_semantic_target(&tree, &target).is_some(),
                "missing semantic control {target}"
            );
        }

        let before = app.workspace.get(id).expect("connection").favorite;
        assert!(app.perform_semantic_action(
            &format!("remote.connection:{id}:favorite"),
            &ui::a11y::Action::ToggleCheck,
            &mut doc(),
            &automerge::ROOT,
        ));
        assert_eq!(app.workspace.get(id).expect("connection").favorite, !before);
    }

    #[test]
    fn accessibility_can_edit_form_fields_without_bypassing_validation() {
        let mut app = RemoteApp::new();
        app.apply(Action::NewConnection, &mut doc());
        assert!(app.perform_semantic_action(
            "remote.field:host",
            &ui::a11y::Action::SetValue("build.internal".into()),
            &mut doc(),
            &automerge::ROOT,
        ));
        assert!(app.perform_semantic_action(
            "remote.field:port",
            &ui::a11y::Action::SetValue("22ssh".into()),
            &mut doc(),
            &automerge::ROOT,
        ));
        assert_eq!(app.draft.host, "build.internal");
        assert_eq!(app.draft.port, "22");
        assert_eq!(app.focus, Some(Field::Port));
        let tree = app.accessibility().expect("form semantics");
        for target in [
            "remote.kind:SSH",
            "remote.field:name",
            "remote.field:host",
            "remote.field:port",
            "remote.auth:password",
            "remote.form.cancel",
            "remote.form.save",
        ] {
            assert!(
                find_semantic_target(&tree, target).is_some(),
                "missing form control {target}"
            );
        }
    }

    #[test]
    fn view_resource_operations_open_the_exact_requested_ui() {
        let mut app = RemoteApp::new_demo();
        let id = app.workspace.connections()[0].id;
        let edit = qos_actions::EncodedAction::encode_operation(
            crate::actions::REMOTE_OPERATION_TYPE,
            &crate::actions::RemoteOperation::EditConnection { connection_id: id },
        )
        .expect("serializable view operation");
        assert!(
            app.invoke_resource_operation(
                qos_actions::OperationKind::View,
                &edit.type_id,
                &edit.payload,
                &doc(),
                &automerge::ROOT,
            )
            .expect("view routed")
        );
        assert_eq!(app.screen, Screen::Form { editing: Some(id) });

        assert!(
            app.invoke_resource_operation(
                qos_actions::OperationKind::Effect,
                &edit.type_id,
                &edit.payload,
                &doc(),
                &automerge::ROOT,
            )
            .is_err()
        );
    }

    #[test]
    fn reduced_motion_settles_the_navigation_flap() {
        let mut app = RemoteApp::new();
        lay_out(&mut app, 640.0, 900.0);
        app.apply(Action::ToggleRail, &mut doc());
        assert!(app.needs_animation_frame());
        app.set_motion_reduced(true);
        assert!(!app.needs_animation_frame());
        assert!((app.rail.reveal() - 1.0).abs() <= f64::EPSILON);
    }

    #[test]
    fn long_narrow_lists_scroll_and_keep_selection_visible() {
        let mut app = RemoteApp::new_demo();
        lay_out(&mut app, 640.0, 500.0);
        let count = app.workspace.connections().len();
        app.scroll_connection_into_view(count - 1, count);
        assert!(app.list_scroll > 0.0);
        assert!(app.list_scroll <= app.list_scroll_max);
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
        assert!(app.form_validation.submitted);
        assert_eq!(app.focus, Some(Field::Name));
        assert_eq!(
            app.visible_form_problems(),
            vec!["Name is required", "Host is required"],
            "a Save attempt reveals every problem, including the unfocused host"
        );
    }

    #[test]
    fn new_form_stays_quiet_until_a_field_is_touched_or_save_is_attempted() {
        let mut app = RemoteApp::new();
        app.apply(Action::NewConnection, &mut doc());

        assert!(app.visible_form_problems().is_empty());
        let tree = app.accessibility().expect("form semantics");
        assert!(
            find_live_region(&tree).is_none(),
            "a fresh blank draft is guidance, not an accessibility error announcement"
        );

        app.apply(Action::FocusField(Field::Host), &mut doc());
        assert_eq!(
            app.visible_form_problems(),
            vec!["Name is required"],
            "blurring a blank field reveals only feedback for that touched field"
        );
    }

    #[test]
    fn accessibility_save_attempt_reveals_validation_and_focuses_first_problem() {
        let mut app = RemoteApp::new();
        app.apply(Action::NewConnection, &mut doc());
        assert!(app.perform_semantic_action(
            "remote.form.save",
            &ui::a11y::Action::Activate,
            &mut doc(),
            &automerge::ROOT,
        ));
        assert!(app.form_validation.submitted);
        assert_eq!(app.focus, Some(Field::Name));
        let tree = app.accessibility().expect("validated form semantics");
        assert!(
            find_live_region(&tree).is_some(),
            "validation feedback is announced after the explicit Save attempt"
        );
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
        app.apply(Action::RequestDelete(id), &mut doc());
        app.apply(Action::ConfirmPending, &mut doc());
        assert_eq!(app.workspace.connections().len(), count - 1);
        assert!(app.workspace.get(id).is_none());
    }

    #[test]
    fn canvas_connect_requires_confirmation_before_opening_a_broker_route() {
        let mut app = RemoteApp::new_demo();
        let id = app
            .workspace
            .connections()
            .iter()
            .find(|connection| connection.kind == ConnectionKind::Qos)
            .expect("demo qOS connection")
            .id;

        app.apply(Action::RequestConnect(id), &mut doc());

        assert!(matches!(
            app.pending_confirmation,
            Some(PendingConfirmation::Connect { id: pending, .. }) if pending == id
        ));
        assert!(app.live.is_none(), "no route is opened before confirmation");
        assert!(
            app.collab_outbox.is_empty(),
            "no control request is queued before confirmation"
        );
    }

    #[test]
    fn confirmation_revalidates_a_collaboratively_changed_target() {
        let mut app = RemoteApp::new_demo();
        let id = app
            .workspace
            .connections()
            .iter()
            .find(|connection| connection.kind == ConnectionKind::Qos)
            .expect("demo qOS connection")
            .id;
        app.apply(Action::RequestConnect(id), &mut doc());
        app.workspace.get_mut(id).expect("profile").host =
            iroh::SecretKey::generate().public().to_string();

        app.apply(Action::ConfirmPending, &mut doc());

        assert!(
            app.live.is_none(),
            "changed target must not be used after confirmation"
        );
        assert!(
            app.collab_outbox.is_empty(),
            "no route is opened for stale confirmation"
        );
        assert!(
            app.notice
                .as_deref()
                .is_some_and(|notice| notice.contains("changed"))
        );
    }

    #[test]
    fn confirmation_is_a_modal_accessibility_surface() {
        let mut app = RemoteApp::new_demo();
        lay_out(&mut app, 960.0, 700.0);
        let id = app.workspace.connections()[0].id;
        app.apply(Action::RequestDelete(id), &mut doc());
        let tree = app.accessibility().expect("confirmation semantics");
        let confirm =
            find_semantic_target(&tree, "remote.confirmation.confirm").expect("confirm action");
        assert_eq!(confirm.name.as_deref(), Some("Delete"));
        assert!(
            tree.children
                .iter()
                .any(|node| node.role == Role::Dialog && node.state.modal)
        );
    }

    #[test]
    fn connect_ssh_explains_the_missing_live_capability_without_opening_a_socket() {
        // Default build has no live transport: Connect opens an explicit
        // unavailable surface, not a fabricated terminal or an ambiguous
        // "not connected" screen.
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
        assert!(matches!(
            app.live,
            Some(LiveSession::Unavailable {
                readiness: TransportReadiness::NeedsLiveSessionCapability,
                kind: ConnectionKind::Ssh,
            })
        ));
    }

    #[test]
    fn connect_vnc_explains_the_missing_live_capability_without_opening_a_socket() {
        let mut app = RemoteApp::new_demo();
        let id = app
            .workspace
            .connections()
            .iter()
            .find(|c| c.kind == ConnectionKind::Vnc)
            .unwrap()
            .id;
        app.apply(Action::Connect(id), &mut doc());
        assert!(matches!(
            app.live,
            Some(LiveSession::Unavailable {
                readiness: TransportReadiness::NeedsLiveSessionCapability,
                kind: ConnectionKind::Vnc,
            })
        ));
    }

    #[test]
    fn connect_qos_starts_the_production_broker_transport_without_dev_gate() {
        let mut app = RemoteApp::new_demo();
        let target = iroh::SecretKey::generate().public();
        let id = app
            .workspace
            .connections()
            .iter()
            .find(|connection| connection.kind == ConnectionKind::Qos)
            .expect("demo qOS connection")
            .id;
        app.workspace.get_mut(id).expect("qOS connection").host = target.to_string();

        app.apply(Action::Connect(id), &mut doc());

        assert!(matches!(app.live, Some(LiveSession::Qos(_))));
        assert!(matches!(
            app.collab_outbox.as_slice(),
            [ui::collaboration::CollabRequest::OpenWorkspaceControl { target_device, .. }]
                if target_device == target.as_bytes()
        ));
    }

    #[test]
    fn qos_session_setup_uses_authenticated_broker_transport() {
        let mut app = RemoteApp::new_demo();
        let mut connection = app
            .workspace
            .connections()
            .iter()
            .find(|connection| connection.kind == ConnectionKind::Qos)
            .expect("demo qOS connection")
            .clone();
        let target = iroh::SecretKey::generate().public();
        connection.host = target.to_string();
        let session = app.start_live(&connection);
        assert!(matches!(
            session,
            LiveSession::Qos(QosSession {
                status: SessionStatus::Connecting,
                ..
            })
        ));
        assert!(matches!(
            app.collab_outbox.as_slice(),
            [ui::collaboration::CollabRequest::OpenWorkspaceControl { target_device, .. }]
                if target_device == target.as_bytes()
        ));
    }

    #[test]
    fn qos_widget_never_queues_input_before_consent_or_after_revocation() {
        let mut app = RemoteApp::new_demo();
        let mut connection = app
            .workspace
            .connections()
            .iter()
            .find(|connection| connection.kind == ConnectionKind::Qos)
            .expect("demo qOS connection")
            .clone();
        connection.host = iroh::SecretKey::generate().public().to_string();
        let live = app.start_live(&connection);
        app.live = Some(live);
        let session_id = match app.live.as_ref() {
            Some(LiveSession::Qos(session)) => session.id,
            _ => panic!("qOS session"),
        };
        app.collab_outbox.clear();
        assert!(
            !app.queue_qos_input(runtime_collab::WorkspaceControlInput::PointerMotion {
                x: 1.0,
                y: 2.0
            })
        );
        assert!(app.collab_outbox.is_empty());

        app.handle_collab_event(&ui::collaboration::CollabEvent::WorkspaceControl {
            session_id,
            event: runtime_collab::WorkspaceControlEvent::StreamStarted {
                surface: 4,
                width: 1280,
                height: 720,
            },
        });
        assert!(
            app.queue_qos_input(runtime_collab::WorkspaceControlInput::PointerMotion {
                x: 1.0,
                y: 2.0
            })
        );
        app.collab_outbox.clear();

        app.handle_collab_event(&ui::collaboration::CollabEvent::WorkspaceControl {
            session_id,
            event: runtime_collab::WorkspaceControlEvent::Closed(
                runtime_collab::WorkspaceControlCloseReason::Revoked,
            ),
        });
        assert!(
            !app.queue_qos_input(runtime_collab::WorkspaceControlInput::PointerMotion {
                x: 1.0,
                y: 2.0
            })
        );
        assert!(app.collab_outbox.is_empty());
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

        app.apply(Action::RequestDelete(id), &mut state);
        app.apply(Action::ConfirmPending, &mut state);

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
    fn unavailable_transport_opens_an_editable_recovery_surface() {
        let mut app = RemoteApp::new_demo();
        let mut state = AutoCommit::new();
        let id = app
            .first_of_kind(ConnectionKind::Rdp)
            .expect("demo contains a legacy RDP profile");

        app.apply(Action::Connect(id), &mut state);

        assert_eq!(app.screen, Screen::Session { id });
        assert!(matches!(
            app.live,
            Some(LiveSession::Unavailable {
                readiness: TransportReadiness::BackendUnavailable,
                kind: ConnectionKind::Rdp,
            })
        ));
    }

    #[test]
    fn unavailable_connect_operation_reports_a_distinct_non_retryable_result() {
        let mut app = RemoteApp::new_demo();
        let invocation = qos_actions::InvocationId::new();
        app.live = Some(LiveSession::Unavailable {
            readiness: TransportReadiness::BackendUnavailable,
            kind: ConnectionKind::Rdp,
        });
        app.pending_resource_operation = Some(PendingResourceOperation::Connect { invocation });

        let update = app
            .poll_resource_operation(invocation, false, &doc(), &automerge::ROOT)
            .expect("unavailable transport reaches a terminal operation result");
        assert!(matches!(
            update,
            qos_actions::InvocationUpdate::Failed { error, .. }
                if error.code == "remote.transport_unavailable" && !error.retryable
        ));
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

        let mut app = RemoteApp::new_demo();
        app.dev_set_screen("session-qos");
        assert!(matches!(
            app.live,
            Some(LiveSession::Preview(SessionStatus::Connecting))
        ));
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

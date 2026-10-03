//! [`GreeterApp`] — the root login widget.
//!
//! A composed welcome and credential surface: wide displays place identity
//! beside the form, while compact or large-text displays use a single stack.
//! The shared toolkit owns every input and action. Its App/FrameHarness
//! lifecycle settles viewport-dependent composition before the first paint.
//!
//! IMPORTANT (focus): every layout node has a matching widget in `children()`.
//! This keeps the toolkit's focus, accessibility and layout traversals aligned
//! through every grouping and across responsive recomposition.
//!
//! Non-secret presentation state lives in the CRDT (the toolkit convention):
//!   * `greeter.username` — `Text`
//!   * `greeter.submit`   — `bool` flag (Log-in / Enter rising edge)
//!   * `greeter.fingerprint` — `bool` flag (explicit passwordless PAM attempt)
//!   * `greeter.session`  — `Text` (the chosen session's label)
//!
//! The passcode is deliberately different: it is held only in a process-local,
//! zeroized buffer shared with a local-mode [`TextInput`]. It never enters an
//! Automerge object, change history, persistence, presence, or peer sync.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use ui::doc::{read_bool, read_text};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, transaction::Transactable};
use taffy::style_helpers::{length, percent};
use taffy::{
    AlignItems, Dimension, Display, FlexDirection, JustifyContent, NodeId, Size, Style, TaffyTree,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, NamedKey};
use zeroize::{Zeroize, Zeroizing};

use ui::a11y::Politeness;
use ui::app::UserEvent;
use ui::caps_lock::CapsLockEstimate;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    Button, Dropdown, IconWidget, Row, Scrollable, Spacer, Spinner, Text, TextInput, Widget,
};

mod console;

pub use console::{CONSOLE_WAKE, PROVIDER as CONSOLE_PROVIDER};

use crate::auth::{AuthUpdate, spawn_auth};
use crate::local_users::{LoginUser, discover_default_login_user};
use crate::sessions::{DEFAULT_SESSIONS_DIR, Session, read_sessions};

const USERNAME_PATH: &str = "greeter.username";
const SUBMIT_PATH: &str = "greeter.submit";
const FINGERPRINT_PATH: &str = "greeter.fingerprint";
/// The session picker's chosen label, as a CRDT `Text`. The picker writes
/// its selection here; [`GreeterApp::sync`] mirrors it back onto the
/// selected index.
const SESSION_PATH: &str = "greeter.session";
/// Rising-edge boolean flags raised by the secondary power-action buttons.
/// Consumed in [`GreeterApp::sync`], which routes them through logind via
/// `systemctl reboot` / `systemctl poweroff` (authorised for the `greetd`
/// user by os/etc/polkit-1/rules.d/49-tilo-greeter-power.rules).
const REBOOT_PATH: &str = "greeter.reboot";
const POWEROFF_PATH: &str = "greeter.poweroff";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthMethod {
    Password,
    Fingerprint,
}

/// Card geometry (logical px).
const CARD_WIDTH: f32 = 440.0;
const CARD_PADDING: f32 = 24.0;
/// Gap between a field's label and the field itself. Tight enough that the
/// pair reads as one unit rather than two stacked rows.
const LABEL_GAP: f32 = 7.0;
/// Height of the primary action.
const ACTION_HEIGHT: f32 = 44.0;
/// Full-size power targets remain secondary through their ghost treatment.
const POWER_HEIGHT: f32 = 44.0;
/// The status row is reserved at this height in *every* state, so a message
/// appearing — or a spinner replacing it — never resizes the card. Without
/// the reservation the whole form jumps vertically the moment you get your
/// password wrong, which is exactly when you are least able to absorb it.
const STATUS_HEIGHT: f32 = 20.0;
/// Space between the primary action and its status line. Tighter than
/// the section gap, so feedback reads as belonging to the button you just
/// pressed rather than floating loose in the card.
const ACTION_STATUS_GAP: f32 = 10.0;
/// Space between lines in the compact welcome lockup.
const BRAND_GAP: f32 = 5.0;
/// A split is useful only when both regions have room to breathe. Short or
/// large-text displays keep the compact form and its persistent power footer.
const WIDE_VIEWPORT_WIDTH: f64 = 1100.0;
const WIDE_VIEWPORT_HEIGHT: f64 = 560.0;
const COMPACT_VIEWPORT_WIDTH: f64 = 600.0;
/// How long the decorative backdrop keeps moving after the greeter appears or
/// after the last input, before it holds its frame. A greeter mostly waits
/// unattended, and without a GPU every backdrop frame is a full software
/// render: at 5K that was over four cores and the compositor behind it, for
/// as long as the login screen stayed up.
const BACKDROP_ACTIVE: Duration = Duration::from_secs(20);
/// Frame interval of the moving backdrop.
const BACKDROP_INTERVAL: Duration = Duration::from_millis(42);

fn backdrop_idle_after(activity: Instant) -> Instant {
    activity.checked_add(BACKDROP_ACTIVE).unwrap_or(activity)
}

/// A stack with an explicit gap and optional padding, fixed width,
/// minimum height, and opaque card chrome. Only the wide welcome stage is horizontal.
///
/// Every grouping in the greeter is one of these. It exists as a real
/// `Widget` rather than a bare taffy node because the App discovers
/// focusable widgets by walking the widget tree (`Widget::children`) in
/// lock-step with the taffy tree — see the module docs. Because each stack
/// contributes exactly one taffy node whose taffy children are its widget
/// children, the two trees mirror each other by construction at every depth,
/// however deeply the card nests.
struct VStack {
    children: Vec<Box<dyn Widget>>,
    gap: f32,
    padding: f32,
    /// `None` fills the parent's content width.
    width: Option<f32>,
    min_height: Option<f32>,
    max_width: Option<f32>,
    fill_height: bool,
    center_children: bool,
    direction: FlexDirection,
    /// Paint the opaque credential surface and its theme-derived edge.
    chrome: bool,
    rect: Rect,
}

impl VStack {
    /// A bare grouping: no padding, no chrome, fills the parent's width.
    fn new(gap: f32, children: Vec<Box<dyn Widget>>) -> Self {
        Self {
            children,
            gap,
            padding: 0.0,
            width: None,
            min_height: None,
            max_width: None,
            fill_height: false,
            center_children: false,
            direction: FlexDirection::Column,
            chrome: false,
            rect: Rect::ZERO,
        }
    }

    /// The login card itself — capped at a comfortable reading width, but
    /// allowed to narrow with the viewport.
    fn card(gap: f32, children: Vec<Box<dyn Widget>>) -> Self {
        Self {
            children,
            gap,
            padding: CARD_PADDING,
            width: None,
            min_height: None,
            max_width: Some(CARD_WIDTH),
            fill_height: false,
            center_children: false,
            direction: FlexDirection::Column,
            chrome: true,
            rect: Rect::ZERO,
        }
    }

    /// A viewport-height stage with flexible spacers above and below the
    /// card. The spacers center a short card, then collapse to zero before
    /// a tall card overflows into the surrounding [`Scrollable`].
    fn viewport(child: Box<dyn Widget>) -> Self {
        Self {
            children: vec![Box::new(Spacer::new()), child, Box::new(Spacer::new())],
            gap: 0.0,
            padding: 16.0,
            width: None,
            min_height: None,
            max_width: None,
            fill_height: true,
            center_children: true,
            direction: FlexDirection::Column,
            chrome: false,
            rect: Rect::ZERO,
        }
    }

    /// Full-height screen shell for the scrollable card and the persistent
    /// power footer.
    fn shell(children: Vec<Box<dyn Widget>>) -> Self {
        Self {
            children,
            gap: 10.0,
            padding: 16.0,
            width: None,
            min_height: None,
            max_width: None,
            fill_height: true,
            center_children: false,
            direction: FlexDirection::Column,
            chrome: false,
            rect: Rect::ZERO,
        }
    }

    /// Two real widget children preserve the toolkit's focus/layout topology.
    fn split(brand: Box<dyn Widget>, card: Box<dyn Widget>, theme: &Theme) -> Self {
        let mut stage = Self::new(theme.tokens.spacing.s16, vec![brand, card]);
        stage.direction = FlexDirection::Row;
        stage.center_children = true;
        stage.max_width = Some(CARD_WIDTH * 2.0 + theme.tokens.spacing.s16);
        stage
    }

    fn with_min_height(mut self, px: f32) -> Self {
        self.min_height = Some(px);
        self
    }
}

impl Widget for VStack {
    ui::provides!(dyn ui::undo::UndoPoints);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child_nodes: Vec<NodeId> = self
            .children
            .iter_mut()
            .map(|c| c.build_layout(taffy, theme, node_map))
            .collect();
        // `new_with_children` only fails when a child belongs to a different
        // tree; every entry in `child_nodes` was just built in this one above.
        #[allow(
            clippy::unwrap_used,
            reason = "every child node was just created in this same tree"
        )]
        let node = taffy
            .new_with_children(
                Style {
                    display: Display::Flex,
                    flex_direction: self.direction,
                    justify_content: (self.direction == FlexDirection::Row)
                        .then_some(JustifyContent::Center),
                    // Stretch, not Center: every control spans the card's
                    // content width and the copy sits on one left margin, so
                    // the form reads as a single column instead of a stack of
                    // separately-centred pills.
                    align_items: Some(if self.center_children {
                        AlignItems::Center
                    } else {
                        AlignItems::Stretch
                    }),
                    gap: Size {
                        width: length(if self.direction == FlexDirection::Row {
                            self.gap
                        } else {
                            0.0
                        }),
                        height: length(if self.direction == FlexDirection::Row {
                            0.0
                        } else {
                            self.gap
                        }),
                    },
                    padding: taffy::Rect {
                        left: length(self.padding),
                        right: length(self.padding),
                        top: length(self.padding),
                        bottom: length(self.padding),
                    },
                    size: Size {
                        width: match self.width {
                            Some(w) => length(w),
                            None => percent(1.0_f32),
                        },
                        height: if self.fill_height {
                            percent(1.0_f32)
                        } else {
                            Dimension::auto()
                        },
                    },
                    max_size: Size {
                        width: self.max_width.map_or(Dimension::auto(), length),
                        height: Dimension::auto(),
                    },
                    min_size: Size {
                        width: Dimension::auto(),
                        height: match self.min_height {
                            Some(h) => length(h),
                            None => Dimension::auto(),
                        },
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                &child_nodes,
            )
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let own_origin = Point::new(self.rect.x0, self.rect.y0);
        if let Ok(child_nodes) = taffy.children(node) {
            for (child, &child_node) in self.children.iter_mut().zip(child_nodes.iter()) {
                child.apply_layout(taffy, child_node, own_origin);
            }
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
        if self.chrome {
            let tokens = &theme.tokens;
            let shape = RoundedRect::from_rect(self.rect, f64::from(tokens.radii.xl));
            let ground = ui::chrome::shell::panel_ground(theme);
            // Credentials sit on one opaque, quiet surface. Its derived edge
            // separates it from the moving backdrop without a heavy shadow.
            scene.fill(Fill::NonZero, transform, ground, None, &shape);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                ui::tokens::Colors::hairline_on(
                    ground,
                    if theme.name == "High Contrast" {
                        180
                    } else {
                        26
                    },
                ),
                None,
                &shape,
            );
        }
        for child in &mut self.children {
            child.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let Ok(child_nodes) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        if let Some(idx) = self.children.iter().position(|c| c.wants_pointer_capture()) {
            if let Some(&cnode) = child_nodes.get(idx)
                && let Some(child) = self.children.get_mut(idx)
            {
                child.handle_event(event, &mut ctx.reborrow(ctx.path, cnode));
            }
            return;
        }
        for (child, &child_node) in self.children.iter_mut().zip(child_nodes.iter()) {
            child.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
            if *ctx.handled {
                break;
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.children
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.children
            .iter_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.children.iter().any(|c| c.wants_pointer_capture())
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Not `.any()`: `take_structure_dirty` drains each child's flag as a
        // side effect, and `.any()` short-circuits on the first `true`,
        // leaving later children's flags un-drained.
        let mut any = false;
        for child in &mut self.children {
            any |= child.take_structure_dirty();
        }
        any
    }

    fn name(&self) -> &'static str {
        "GreeterStack"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::undo::UndoPoints for VStack {
    fn needs_commit(&self) -> bool {
        self.children
            .iter()
            .any(|c| ui::undo::needs_commit(c.as_ref()))
    }
    fn on_commit(&mut self) {
        for child in &mut self.children {
            ui::undo::on_commit(child.as_mut());
        }
    }
}

/// Copy for the moment between "your password was accepted" and the
/// greeter handing the VT to the session. Named so the success wording
/// lives in exactly one place.
const STARTING_MESSAGE: &str = "Starting your session…";

/// Where the greeter is in the authentication lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthPhase {
    /// Showing the login form, waiting for the user.
    Idle,
    /// A greetd conversation is running on the worker thread. Inputs are
    /// disabled and submit is ignored (no double-submit). On failure we
    /// return to [`AuthPhase::Idle`] with an error.
    Authenticating,
    /// greetd accepted the credentials and is starting the session. The
    /// process exits moments later, but the user gets an explicit success
    /// frame rather than the spinner simply vanishing — the attempt always
    /// resolves into something visible.
    Starting,
}

impl AuthPhase {
    /// Whether an attempt currently owns the form: inputs disabled and
    /// submit ignored. The primary action stays enabled — it is inert
    /// because `GreeterApp::attempt_login` refuses a second attempt, not
    /// because it is greyed out — so it remains the card's keyboard home
    /// instead of the focus ring falling through to the power actions.
    pub fn is_busy(self) -> bool {
        matches!(self, AuthPhase::Authenticating | AuthPhase::Starting)
    }
}

/// What the card's single status line is currently saying, and therefore
/// how it should look. Severity is carried in the state rather than
/// inferred from the wording, so an error reads as an error at a glance
/// instead of as one more line of grey caption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// Nothing to say. The row is still reserved, so the card keeps its
    /// size and the form does not jump when something appears here.
    None,
    /// A correctable note about the form itself ("Enter your passcode").
    Hint,
    /// The attempt was rejected.
    Error,
}

/// A machine power action the greeter can request before login. Routed to
/// logind via `systemctl` — see `GreeterApp::run_power`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Reboot,
    PowerOff,
}

/// Pre-canned visual states for the headless `--shots` render path so each
/// distinct screen can be snapshot-verified. Live runs never use these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScenario {
    /// The empty login form (default first frame).
    Form,
    /// The form after a failed attempt — error status shown, password
    /// cleared, username retained.
    Error,
    /// Mid-authentication: spinner + "Authenticating…" with the username +
    /// password inputs disabled (dimmed, inert).
    Authenticating,
    /// Mid-authentication with a fingerprint prompt surfaced.
    Fingerprint,
    /// The form with the Caps-Lock warning shown under the password field.
    CapsLock,
    /// The form after submitting with an empty password — a correctable
    /// hint rather than an error.
    Hint,
    /// Credentials accepted; the session is coming up.
    Starting,
    /// A username long enough to test the field's overflow behaviour.
    LongName,
    /// The normal personal-device form: one known local account and an
    /// enrolled fingerprint, so only the passcode is editable.
    Personal,
    /// Personal-device form after an incorrect passcode.
    PersonalError,
    /// Personal-device form while password authentication is in flight.
    PersonalAuthenticating,
    /// Personal-device form with its Caps-Lock warning visible.
    PersonalCapsLock,
    /// Personal-device form after an empty passcode submission.
    PersonalHint,
    /// Personal-device form while the accepted session is starting.
    PersonalStarting,
    /// One known local account but no enrolled fingerprint. The unavailable
    /// capability must disappear rather than becoming a dead action.
    PersonalNoFingerprint,
}

/// Recognise the deliberately small, machine-readable-looking portion of
/// `fprintd-list`'s output. A successful command alone is insufficient: some
/// fprintd versions exit successfully while reporting no enrolled prints.
fn fingerprint_list_has_enrollment(output: &str) -> bool {
    output.lines().any(|line| {
        let line = line.trim();
        line.starts_with("- #")
            || (line.starts_with("- ") && line.trim_end_matches('.').ends_with("-finger"))
    })
}

/// Ask fprintd whether `username` has an enrolled factor. Any missing command,
/// service, reader, permission, non-zero status, malformed output, or empty
/// enrollment fails closed to `false` so the login screen never offers a
/// button which can only fail.
fn enrolled_fingerprint(username: &str) -> bool {
    let Ok(output) = bounded_command::run_with_env(
        "fprintd-list",
        &[username],
        &[("LC_ALL", "C")],
        None,
        64 * 1024,
        std::time::Duration::from_secs(3),
    ) else {
        return false;
    };
    output.status.success()
        && fingerprint_list_has_enrollment(&String::from_utf8_lossy(&output.stdout))
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent lifecycle flags: bootstrap, rebuild, focus reset, keyboard focus"
)]
pub struct GreeterApp {
    sessions: Vec<Session>,
    selected_session: usize,
    /// The sole eligible interactive local account, when discovery was
    /// unambiguous. Its username is fixed and the form asks only for the
    /// passcode; zero/multiple candidates retain the editable username field.
    login_user: Option<LoginUser>,
    /// Whether fingerprint login is unavailable, already confirmed for the
    /// fixed local account, or must be resolved from the typed username.
    fingerprint: FingerprintAvailability,
    /// Account-specific enrollment resolver. Production calls `fprintd-list`;
    /// a function pointer keeps multi-account routing deterministic in tests.
    fingerprint_probe: fn(&str) -> bool,
    /// Process-local only. Passcode text never enters Automerge, persistence,
    /// presence, diagnostics, or peer sync and is zeroized after every attempt.
    password_buffer: Arc<Mutex<String>>,
    status: String,
    /// How [`GreeterApp::status`] should read — see [`StatusKind`].
    status_kind: StatusKind,
    /// Optional secondary, small/raw detail under the status (e.g. a
    /// non-auth greetd error description). Empty = none shown.
    detail: String,
    /// Current PAM conversation prompt during [`AuthPhase::Authenticating`]
    /// (e.g. "Place your finger on the reader"). Empty falls back to the
    /// generic "Authenticating…" label.
    prompt: String,
    phase: AuthPhase,
    bootstrap_done: bool,
    /// Set when the composition must be rebuilt (session label, status
    /// text, or auth phase changed). Drained by `take_structure_dirty`,
    /// which the App polls to trigger a fresh `build_layout`.
    dirty_structure: bool,
    /// Heuristic Caps-Lock state inferred from typed letters (winit can't
    /// report the lock directly). Drives the warning under the password
    /// field. See [`ui::caps_lock`].
    caps_lock: CapsLockEstimate,
    /// One-shot request that the App re-seat keyboard focus on the first
    /// focusable widget after the next rebuild (drained by
    /// [`Widget::take_focus_reset`]). Raised on auth-phase transitions,
    /// which enable/disable the inputs and therefore change *which* widgets
    /// are focusable — preserving the old ordinal would land focus on the
    /// wrong control. Plain status/caps rebuilds leave it `false` so focus +
    /// caret are preserved in place.
    focus_reset_pending: bool,
    /// The most recent power action requested, recorded for test
    /// observability (the actual `systemctl` invocation is a side effect).
    last_power_action: Option<PowerAction>,
    rect: Rect,
    /// The composed card subtree, rebuilt each `build_layout`.
    composed: Option<Box<dyn Widget>>,
    /// Slot holding the winit event-loop proxy, populated by `main.rs`'s
    /// `run_with_setup` once the loop starts. The submit handler reads it
    /// to spawn the background auth worker (which pumps progress back via
    /// [`UserEvent::External`]). Empty in tests / headless renders, where
    /// auth falls back to a synchronous exchange.
    proxy: Arc<OnceLock<EventLoopProxy<UserEvent>>>,
    /// The compact-viewport scroll position survives status/caps/auth
    /// rebuilds, which recreate the composed tree.
    viewport_offset: Rc<Cell<(f32, f32)>>,
    /// Pure presentation caches, never authentication or replicated state.
    layout_text_scale: f32,
    /// Backdrop animation time at `backdrop_resumed`; it advances only until
    /// `backdrop_idle_at`, then holds until input resumes it from there.
    backdrop_phase: f64,
    backdrop_resumed: Instant,
    backdrop_idle_at: Instant,
    backdrop_deadline: Option<Instant>,
    shot_time: Option<f64>,
    /// Queued console requests (see [`console`]); `None` in shots and tests
    /// that do not exercise the console.
    console: Option<console_control::ConsoleServer>,
    /// Whether the compositor has given this window the keyboard. Until it
    /// has, keys typed at the greeter go nowhere, so the console reports
    /// sign-in as accepting input only once this is set.
    keyboard_focus: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FingerprintAvailability {
    Unavailable,
    Enrolled,
    CheckNamedAccount,
}

impl From<bool> for FingerprintAvailability {
    fn from(enrolled: bool) -> Self {
        if enrolled {
            Self::Enrolled
        } else {
            Self::Unavailable
        }
    }
}

impl Default for GreeterApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for GreeterApp {
    fn drop(&mut self) {
        self.password_buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .zeroize();
    }
}

impl GreeterApp {
    /// Construct a greeter reading sessions from the standard
    /// `/usr/share/wayland-sessions` directory.
    pub fn new() -> Self {
        let login_user = discover_default_login_user();
        let fingerprint = login_user
            .as_ref()
            .map_or(FingerprintAvailability::CheckNamedAccount, |user| {
                enrolled_fingerprint(&user.username).into()
            });
        Self::with_sessions_login_user_and_fingerprint(
            read_sessions(std::path::Path::new(DEFAULT_SESSIONS_DIR)),
            login_user,
            fingerprint,
        )
    }

    /// Construct with an explicit session list (used by tests / headless
    /// rendering where the system directory may be absent).
    pub fn with_sessions(sessions: Vec<Session>) -> Self {
        Self::with_sessions_and_login_user(sessions, None)
    }

    /// Construct with an already validated local-account discovery result.
    /// Public for the production entry point and focused UI tests.
    pub fn with_sessions_and_login_user(
        sessions: Vec<Session>,
        login_user: Option<LoginUser>,
    ) -> Self {
        // Deterministic constructor for shots and focused UI tests. Production
        // uses `new`, which probes the fixed account and fails closed.
        let fingerprint = if login_user.is_some() {
            FingerprintAvailability::Enrolled
        } else {
            FingerprintAvailability::CheckNamedAccount
        };
        Self::with_sessions_login_user_and_fingerprint(sessions, login_user, fingerprint)
    }

    fn with_sessions_login_user_and_fingerprint(
        sessions: Vec<Session>,
        login_user: Option<LoginUser>,
        fingerprint: FingerprintAvailability,
    ) -> Self {
        Self {
            sessions,
            selected_session: 0,
            login_user,
            fingerprint,
            fingerprint_probe: enrolled_fingerprint,
            password_buffer: Arc::new(Mutex::new(String::new())),
            status: String::new(),
            status_kind: StatusKind::None,
            detail: String::new(),
            prompt: String::new(),
            phase: AuthPhase::Idle,
            bootstrap_done: false,
            dirty_structure: false,
            caps_lock: CapsLockEstimate::new(),
            focus_reset_pending: false,
            last_power_action: None,
            rect: Rect::ZERO,
            composed: None,
            proxy: Arc::new(OnceLock::new()),
            viewport_offset: Rc::new(Cell::new((0.0, 0.0))),
            layout_text_scale: 1.0,
            backdrop_phase: 0.0,
            backdrop_resumed: Instant::now(),
            backdrop_idle_at: backdrop_idle_after(Instant::now()),
            backdrop_deadline: None,
            shot_time: None,
            console: None,
            keyboard_focus: false,
        }
    }

    /// Wire the shared event-loop proxy slot. `main.rs` constructs the slot,
    /// hands a clone here, and populates it inside `App::run_with_setup`.
    /// The submit handler reads it to spawn the background auth worker.
    #[must_use]
    pub fn with_proxy_slot(mut self, slot: Arc<OnceLock<EventLoopProxy<UserEvent>>>) -> Self {
        self.proxy = slot;
        self
    }

    /// Build a greeter pre-seeded for a headless `--shots` render of
    /// `scenario`. Live runs never use this; it just stages enough state
    /// that each distinct screen renders something representative. The
    /// caller is expected to seed the username CRDT object separately
    /// (see `main.rs`), mirroring how the live App syncs fields.
    pub fn for_shot(scenario: AuthScenario) -> Self {
        // A snapshot may point at a fixture directory via
        // `TILO_GREETER_SESSIONS_DIR`, so the session picker can be captured
        // on a machine whose real sessions directory holds a single entry.
        // Deliberately confined to this snapshot constructor:
        // `GreeterApp::new` — the live path — always reads the system
        // location, so nothing in the environment can redirect what a real
        // login is offered.
        let dir = std::env::var("TILO_GREETER_SESSIONS_DIR")
            .unwrap_or_else(|_| DEFAULT_SESSIONS_DIR.to_string());
        let sessions = crate::sessions::read_sessions(std::path::Path::new(&dir));
        let personal = matches!(
            scenario,
            AuthScenario::Fingerprint
                | AuthScenario::Personal
                | AuthScenario::PersonalError
                | AuthScenario::PersonalAuthenticating
                | AuthScenario::PersonalCapsLock
                | AuthScenario::PersonalHint
                | AuthScenario::PersonalStarting
                | AuthScenario::PersonalNoFingerprint
        )
        .then(|| LoginUser {
            username: "ada".to_string(),
            display_name: "ada Example".to_string(),
        });
        // A fixed personal account has a known enrollment result. The
        // multi-account fixture exposes the action only as a route: its
        // enrollment is resolved for the username entered before PAM starts.
        let fingerprint = if personal.is_none() {
            FingerprintAvailability::CheckNamedAccount
        } else if matches!(scenario, AuthScenario::PersonalNoFingerprint) {
            FingerprintAvailability::Unavailable
        } else {
            FingerprintAvailability::Enrolled
        };
        let mut app =
            Self::with_sessions_login_user_and_fingerprint(sessions, personal, fingerprint);
        app.shot_time = Some(0.0);
        match scenario {
            AuthScenario::Form
            | AuthScenario::LongName
            | AuthScenario::Personal
            | AuthScenario::PersonalNoFingerprint => {}
            AuthScenario::Error | AuthScenario::PersonalError => {
                app.status = "Incorrect username or passcode".to_string();
                app.status_kind = StatusKind::Error;
            }
            AuthScenario::Authenticating | AuthScenario::PersonalAuthenticating => {
                app.phase = AuthPhase::Authenticating;
            }
            AuthScenario::Fingerprint => {
                app.phase = AuthPhase::Authenticating;
                app.prompt = "Place your finger on the reader".to_string();
            }
            AuthScenario::CapsLock | AuthScenario::PersonalCapsLock => {
                // Force the heuristic "on" so the warning renders.
                app.caps_lock.observe('A', false);
            }
            AuthScenario::Hint | AuthScenario::PersonalHint => {
                app.status = "Enter your passcode".to_string();
                app.status_kind = StatusKind::Hint;
            }
            AuthScenario::Starting | AuthScenario::PersonalStarting => {
                app.phase = AuthPhase::Starting;
            }
        }
        app
    }

    /// Freeze decorative motion at a reproducible phase for offscreen review.
    /// This changes presentation only and is used exclusively by `--shots`.
    pub fn set_shot_time(&mut self, seconds: f64) {
        self.shot_time = Some(if seconds.is_finite() {
            seconds.max(0.0)
        } else {
            0.0
        });
    }

    fn backdrop_elapsed(&self, now: Instant) -> f64 {
        self.backdrop_phase
            + now
                .min(self.backdrop_idle_at)
                .saturating_duration_since(self.backdrop_resumed)
                .as_secs_f64()
    }

    /// Input keeps the backdrop moving; after it had settled, it resumes from
    /// the frame it held and the next wake redraws it.
    fn note_backdrop_activity(&mut self, now: Instant) {
        if now >= self.backdrop_idle_at {
            self.backdrop_phase = self.backdrop_elapsed(now);
            self.backdrop_resumed = now;
            // One wake to redraw; draw drops it again for a static theme.
            if self.shot_time.is_none() {
                self.backdrop_deadline = Some(now);
            }
        }
        self.backdrop_idle_at = backdrop_idle_after(now);
    }

    fn uses_wide_layout(&self, text_scale: f32) -> bool {
        self.rect.width() >= WIDE_VIEWPORT_WIDTH
            && self.rect.height() >= WIDE_VIEWPORT_HEIGHT
            && text_scale < 1.75
    }

    /// Short or narrow displays need less decorative space as text grows.
    /// Controls keep their normal target sizes; identity and spacing reflow.
    fn uses_compact_layout(&self, text_scale: f32) -> bool {
        self.rect.width() > 0.0
            && self.rect.height() > 0.0
            && (self.rect.width() < COMPACT_VIEWPORT_WIDTH
                || self.rect.height() < WIDE_VIEWPORT_HEIGHT)
            && text_scale >= 1.25
    }

    /// Seed representative local-only passcode text for a headless snapshot.
    /// The live path never calls this, and even shots keep the value out of the
    /// Automerge document used to drive the rest of the frame.
    pub fn seed_shot_password(&mut self, password: &str) {
        let mut value = self
            .password_buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        value.zeroize();
        value.push_str(password);
    }

    /// Label for the session picker — the session's own human-readable
    /// name, never the internal desktop id it is launched under. The id
    /// (`tilo`) is a `DesktopNames=` contract with the portal; it is not
    /// something to show the person logging in.
    #[cfg(test)]
    fn session_label(&self) -> String {
        match self.sessions.get(self.selected_session) {
            Some(s) => s.name.clone(),
            None => "No session available".to_string(),
        }
    }

    /// The primary action's label. This is the *only* place the greeter
    /// says it is signing you in, so the screen can never show two copies
    /// of that sentence at once.
    fn login_label(&self) -> &'static str {
        match self.phase {
            AuthPhase::Idle => "Log in",
            AuthPhase::Authenticating => "Signing you in…",
            AuthPhase::Starting => "Welcome back",
        }
    }

    /// What the single status line says, as plain text.
    ///
    /// While a login is in flight this deliberately returns the live PAM
    /// prompt and *nothing else* — the "signing you in" wording belongs to
    /// the button ([`GreeterApp::login_label`]). Returning it here too is
    /// what used to render the sentence twice with a spinner wedged
    /// between the copies.
    fn feedback_text(&self) -> String {
        match self.phase {
            AuthPhase::Authenticating => self.prompt.clone(),
            AuthPhase::Starting => {
                if self.prompt.is_empty() {
                    STARTING_MESSAGE.to_string()
                } else {
                    self.prompt.clone()
                }
            }
            AuthPhase::Idle => self.status.clone(),
        }
    }

    /// A field label: the quiet tier of the type hierarchy, so the field it
    /// names never competes with the value inside it.
    fn field_label(text: &str, theme: &Theme) -> Text {
        Text::new(text)
            .with_size(12.0)
            .with_color(theme.tokens.colors.text_dim)
    }

    /// The card's single feedback voice, reserved at a constant height in
    /// every state so the form never jumps.
    ///
    /// Exactly one thing speaks at a time:
    ///   * idle with nothing to say — an empty, reserved row;
    ///   * idle after a correctable mistake — a quiet hint;
    ///   * idle after a rejected attempt — the message in the danger
    ///     colour behind a leading glyph, so it reads as a *state* rather
    ///     than one more line of caption;
    ///   * in flight — a spinner, plus the live PAM prompt when there is
    ///     one (a fingerprint instruction is new information, not a second
    ///     copy of the button).
    fn build_status(&self, theme: &Theme) -> VStack {
        let c = &theme.tokens.colors;
        let mut rows: Vec<Box<dyn Widget>> = Vec::new();

        match self.phase {
            AuthPhase::Authenticating | AuthPhase::Starting => {
                // A spinner only when there is something for it to be
                // spinning *about*.
                //
                // While the exchange is in flight the button already
                // reads "Signing you in…", and the status line is
                // deliberately silent unless PAM says something new. A
                // spinner on its own under a full-width button is then a
                // dot floating in empty space — it reads as a rendering
                // fault rather than as progress, and it says nothing the
                // button has not already said. The moment there *is* a
                // message (a fingerprint prompt, or the session
                // starting) the pair carries real information and both
                // belong.
                let live = self.feedback_text();
                let live = live.trim();
                if !live.is_empty() {
                    let tint = if self.phase == AuthPhase::Starting {
                        c.semantic.success
                    } else {
                        c.text_dim
                    };
                    let politeness = if self.phase == AuthPhase::Authenticating {
                        // PAM prompts can require immediate physical action
                        // (for example, touching the fingerprint reader).
                        Politeness::Assertive
                    } else {
                        Politeness::Polite
                    };
                    rows.push(Box::new(
                        Row::new()
                            .push(Box::new(Spinner::with_size(16.0)))
                            .push(Box::new(
                                Text::new(live)
                                    .with_size(13.0)
                                    .with_color(tint)
                                    .with_live(politeness),
                            )),
                    ));
                }
            }
            AuthPhase::Idle => match self.status_kind {
                StatusKind::None => {}
                StatusKind::Hint => rows.push(Box::new(
                    Text::new(&self.status)
                        .with_size(13.0)
                        .with_color(c.text_dim)
                        .with_live(Politeness::Assertive),
                )),
                StatusKind::Error => {
                    rows.push(Box::new(
                        Row::new()
                            .push(Box::new(
                                IconWidget::new(Icon::Ban, 13.0).with_color(c.semantic.danger),
                            ))
                            .push(Box::new(
                                Text::new(&self.status)
                                    .with_size(13.0)
                                    .with_color(c.semantic.danger)
                                    .with_live(Politeness::Assertive),
                            )),
                    ));
                    if !self.detail.is_empty() {
                        rows.push(Box::new(
                            Text::new(&self.detail)
                                .with_size(11.0)
                                .with_color(c.text_tertiary),
                        ));
                    }
                }
            },
        }

        VStack::new(4.0, rows).with_min_height(STATUS_HEIGHT)
    }

    fn build_auth_actions(&self, busy: bool, theme: &Theme) -> VStack {
        let mut actions: Vec<Box<dyn Widget>> = vec![Box::new(
            Button::new("greeter.login", self.login_label(), Some(SUBMIT_PATH))
                .primary()
                .fill_width(ACTION_HEIGHT),
        )];
        // Once an attempt is running this action is inert and the live PAM
        // prompt is the information the person needs. Removing the disabled
        // duplicate keeps that assertive instruction visible at large text
        // sizes instead of pushing it below the scroll viewport.
        if self.fingerprint != FingerprintAvailability::Unavailable && !busy {
            actions.push(Box::new(
                Button::new(
                    "greeter.fingerprint",
                    "Use fingerprint",
                    Some(FINGERPRINT_PATH),
                )
                .fill_width(ACTION_HEIGHT),
            ));
        }
        actions.push(Box::new(self.build_status(theme)));
        VStack::new(
            if self.uses_compact_layout(theme.text_scale) {
                theme.tokens.spacing.s1
            } else {
                ACTION_STATUS_GAP
            },
            actions,
        )
    }

    fn build_account_field(&self, busy: bool, theme: &Theme) -> Box<dyn Widget> {
        if let Some(user) = &self.login_user {
            Box::new(VStack::new(
                LABEL_GAP,
                vec![
                    Box::new(Self::field_label("Local account", theme)),
                    Box::new(
                        Text::new(&user.username)
                            .with_id("greeter.account")
                            .with_size(15.0),
                    ),
                ],
            ))
        } else {
            Box::new(VStack::new(
                LABEL_GAP,
                vec![
                    Box::new(Self::field_label("Username", theme)),
                    Box::new(
                        TextInput::new(Vec::new())
                            .with_id("greeter.username")
                            .with_on_change_path(USERNAME_PATH)
                            .with_accessible_name("Username")
                            .with_placeholder("Your username")
                            .disabled(busy),
                    ),
                ],
            ))
        }
    }

    /// The greeter's visual introduction. On a personal device the account
    /// is already known, so the person—not the product wordmark—gets the
    /// strongest type tier. The ambiguous-account fallback keeps the compact
    /// product-first lockup because it cannot truthfully greet anyone yet.
    fn build_brand(&self, theme: &Theme, wide: bool) -> Box<dyn Widget> {
        let c = &theme.tokens.colors;
        if !wide && self.uses_compact_layout(theme.text_scale) {
            // Identity remains explicit while decorative welcome copy yields
            // its space to the credentials and live authentication feedback.
            let mut identity: Vec<Box<dyn Widget>> = Vec::new();
            if let Some(user) = &self.login_user {
                identity.push(Box::new(
                    Text::new(&user.display_name)
                        .with_id("greeter.welcome")
                        .with_heading_level(1)
                        .with_role(ui::tokens::TextRole::Heading),
                ));
                identity.push(Box::new(
                    Text::new(&user.username)
                        .with_id("greeter.account")
                        .with_role(ui::tokens::TextRole::Label)
                        .with_color(c.text_dim),
                ));
            } else {
                identity.push(Box::new(
                    Text::new("Tilo")
                        .with_heading_level(1)
                        .with_fixed_size(theme.tokens.typography.scale.x2l),
                ));
            }
            return Box::new(VStack::new(theme.tokens.spacing.s1, identity));
        }
        if let Some(user) = &self.login_user {
            let compact_for_large_text = theme.text_scale >= 1.75;
            let caption = if compact_for_large_text {
                Text::new(&user.username)
                    .with_id("greeter.account")
                    .with_size(13.0)
                    .with_color(c.text_dim)
            } else {
                Text::new("greeter-passcode-intro")
                    .with_size(13.0)
                    .with_color(c.text_dim)
            };
            // Keep the person's name as one intentional headline. Prefixing it
            // with "Welcome back," made an ordinary first + last name wrap
            // after the first name in the production-width card, which looked
            // like accidental clipping rather than a considered lockup.
            let welcome = Text::new(&user.display_name)
                .with_id("greeter.welcome")
                .with_heading_level(1);
            let welcome = if wide {
                welcome.with_fixed_size(theme.tokens.typography.scale.x4l)
            } else {
                welcome.with_size(28.0)
            };
            Box::new(VStack::new(
                BRAND_GAP,
                vec![
                    Box::new(
                        Text::new("greeter-welcome-back")
                            .with_role(ui::tokens::typography::TextRole::Label)
                            .with_color(c.text_dim),
                    ),
                    Box::new(welcome),
                    Box::new(caption),
                ],
            ))
        } else {
            let wordmark = Text::new("Tilo").with_heading_level(1);
            let wordmark = if wide {
                wordmark.with_fixed_size(theme.tokens.typography.scale.x4l * 2.0)
            } else {
                wordmark.with_size(38.0)
            };
            Box::new(VStack::new(
                BRAND_GAP,
                vec![
                    Box::new(wordmark),
                    Box::new(
                        Text::new("greeter-sign-in")
                            .with_size(13.0)
                            .with_color(c.text_dim),
                    ),
                ],
            ))
        }
    }

    /// Build the composed card subtree from current state.
    ///
    /// Focus continuity contract: the focusable set is, in tree order, the
    /// optional username field, passcode field, primary action, fingerprint
    /// action, session picker (present only when there is more than one
    /// session to choose between), and the two power actions. On an
    /// unambiguous personal machine the username becomes a static account
    /// label, making the passcode the first focusable control. Labels, the
    /// Caps-Lock note, the spinner, and the status line are all non-focusable, so an incidental
    /// rebuild (a status change, a Caps-Lock flip, cycling the session)
    /// keeps keyboard focus AND the caret glued in place — see `main.rs`'s
    /// `autofocus_on_structure_change` and the App's `rebuild_structure`.
    ///
    /// While an attempt is in flight the inputs are **disabled** (dimmed and
    /// dropped from the focus order), which deliberately changes the
    /// focusable set; the phase transition raises `take_focus_reset` so the
    /// App re-seats focus rather than preserving a now-invalid ordinal. The
    /// primary action remains enabled while its handler rejects a second
    /// submit, ensuring that reset cannot land on a power action.
    fn build_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let busy = self.phase.is_busy();
        let c = &theme.tokens.colors;

        let mut children: Vec<Box<dyn Widget>> = Vec::new();

        let wide = self.uses_wide_layout(theme.text_scale);
        let compact = self.uses_compact_layout(theme.text_scale);
        let field_gap = if compact {
            theme.tokens.spacing.s1
        } else {
            LABEL_GAP
        };
        // On compact displays the welcome belongs to the same short column.
        // Wide displays give identity its own region without duplicating copy.
        if !wide {
            children.push(self.build_brand(theme, false));
        }

        // --- credentials -------------------------------------------------
        // Persistent labels above each field. Two unlabelled rounded boxes
        // leave the user guessing which one is which; a placeholder alone
        // disappears the moment they start typing.
        // At the maximum supported text scale, fold the already-known local
        // username into the welcome lockup (see `build_brand`). This preserves
        // account context while keeping both authentication actions above the
        // fixed power footer on a 640x480 logical display.
        if self.login_user.is_none() || (!compact && theme.text_scale < 1.75) {
            children.push(self.build_account_field(busy, theme));
        }

        let password_input = {
            let value = self
                .password_buffer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            TextInput::local()
                .with_initial_value(&value)
                .with_id("greeter.password")
                .with_external_buffer(Arc::clone(&self.password_buffer))
                .with_accessible_name("Passcode")
                .with_secret(true)
                .with_placeholder("Your passcode")
                .disabled(busy)
        };
        let mut password_group: Vec<Box<dyn Widget>> = vec![
            Box::new(Self::field_label("Passcode", theme)),
            Box::new(password_input),
        ];
        // The Caps-Lock note lives *inside* the password group so it reads
        // as attached to that field, and so toggling it can never shift the
        // card's top-level ordinals.
        if !busy && self.caps_lock.on() {
            password_group.push(Box::new(
                Row::new()
                    .push(Box::new(
                        IconWidget::new(Icon::ArrowBigUp, 13.0).with_color(c.semantic.warning),
                    ))
                    .push(Box::new(
                        Text::new("Caps Lock is on")
                            .with_size(12.0)
                            .with_color(c.semantic.warning),
                    )),
            ));
        }
        children.push(Box::new(VStack::new(field_gap, password_group)));

        // --- authentication actions + status ------------------------------
        // The one accented, full-width control on the card — everything else
        // is quieter than this by construction — paired with the single
        // status voice beneath the explicit password and fingerprint actions,
        // so feedback lands where the eye already is after a click.
        children.push(Box::new(self.build_auth_actions(busy, theme)));

        // --- session picker ----------------------------------------------
        // Only when there is an actual choice to make. It follows the
        // immediately-needed authentication actions so a scaled/short
        // viewport never hides Log in or Use fingerprint merely because an
        // optional alternate session is installed.
        if self.sessions.len() > 1 {
            children.push(self.build_session_picker(busy, field_gap, theme));
        }

        let mut credentials = VStack::card(
            if compact {
                theme.tokens.spacing.s1
            } else {
                theme.tokens.spacing.s3
            },
            children,
        );
        credentials.padding = if wide {
            theme.tokens.spacing.s8
        } else if compact {
            // Text-scaled fields grow with large type; the compact card gives
            // that height back from its own inset rather than scrolling.
            theme.tokens.spacing.s2
        } else {
            theme.tokens.spacing.s6
        };
        if compact {
            credentials.max_width = Some(CARD_WIDTH + theme.tokens.spacing.s16 * 2.0);
        }
        let card: Box<dyn Widget> = Box::new(credentials);
        let stage = if wide {
            let mut identity =
                VStack::new(theme.tokens.spacing.s6, vec![self.build_brand(theme, true)]);
            identity.width = Some(CARD_WIDTH);
            Box::new(VStack::split(Box::new(identity), card, theme)) as Box<dyn Widget>
        } else {
            card
        };
        let mut viewport = VStack::viewport(stage);
        if compact {
            viewport.padding = theme.tokens.spacing.s2;
        }
        let card_view: Box<dyn Widget> = Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(Box::new(viewport))
                .with_persisted_offset(self.viewport_offset.clone())
                .always_visible(),
        );
        Box::new(VStack::shell(vec![card_view, Self::build_power_footer()]))
    }

    fn build_session_picker(&self, busy: bool, label_gap: f32, theme: &Theme) -> Box<dyn Widget> {
        let options = self
            .sessions
            .iter()
            .map(|session| session.name.clone())
            .collect();
        Box::new(VStack::new(
            label_gap,
            vec![
                Box::new(Self::field_label("Session", theme)),
                Box::new(
                    Dropdown::new(options, SESSION_PATH)
                        .with_id("greeter.session")
                        .with_enabled(!busy),
                ),
            ],
        ))
    }

    fn build_power_footer() -> Box<dyn Widget> {
        Box::new(
            Row::new()
                .push(Box::new(Spacer::new()))
                .push(Box::new(
                    Button::new("greeter.reboot", "Restart", Some(REBOOT_PATH))
                        .ghost()
                        .with_icon(Icon::Refresh)
                        .content_sized()
                        .with_min_height(POWER_HEIGHT),
                ))
                .push(Box::new(
                    Button::new("greeter.poweroff", "Shut down", Some(POWEROFF_PATH))
                        .ghost()
                        .with_icon(Icon::Power)
                        .content_sized()
                        .with_min_height(POWER_HEIGHT),
                )),
        )
    }

    fn bootstrap_crdt(&self, state: &mut AutoCommit, root: &ObjId) {
        if state.get(root, USERNAME_PATH).ok().flatten().is_none() {
            let username = state.put_object(root, USERNAME_PATH, ObjType::Text).ok();
            if let (Some(username), Some(user)) = (username, self.login_user.as_ref()) {
                let _ = state.splice_text(&username, 0, 0, &user.username);
            }
        }
        if state.get(root, SUBMIT_PATH).ok().flatten().is_none() {
            let _ = state.put(root, SUBMIT_PATH, false);
        }
        if state.get(root, FINGERPRINT_PATH).ok().flatten().is_none() {
            let _ = state.put(root, FINGERPRINT_PATH, false);
        }
        if state.get(root, SESSION_PATH).ok().flatten().is_none()
            && let Ok(obj) = state.put_object(root, SESSION_PATH, ObjType::Text)
            && let Some(s) = self.sessions.get(self.selected_session)
        {
            let _ = state.splice_text(&obj, 0, 0, &s.name);
        }
        if state.get(root, REBOOT_PATH).ok().flatten().is_none() {
            let _ = state.put(root, REBOOT_PATH, false);
        }
        if state.get(root, POWEROFF_PATH).ok().flatten().is_none() {
            let _ = state.put(root, POWEROFF_PATH, false);
        }
    }

    /// Record + execute a power action. Splitting the record (always) from
    /// the side-effecting `systemctl` invocation keeps the rising-edge
    /// wiring unit-testable: tests observe [`last_power_action`] without ever
    /// powering off the host.
    ///
    /// [`last_power_action`]: GreeterApp::last_power_action
    fn request_power(&mut self, action: PowerAction) {
        self.last_power_action = Some(action);
        Self::run_power(action);
    }

    /// Invoke logind for `action` via `systemctl`. `systemctl reboot` /
    /// `systemctl poweroff` issue `org.freedesktop.login1.Manager.{Reboot,
    /// PowerOff}` over D-Bus; polkit authorises these for the `greetd` user
    /// through os/etc/polkit-1/rules.d/49-tilo-greeter-power.rules (logind
    /// also permits an *active* local session by default, but the greeter's
    /// session class is "greeter" and may not register as active, so the
    /// explicit rule makes it deterministic). Compiled out under `cfg(test)`
    /// so the test suite can never reboot the build host.
    fn run_power(action: PowerAction) {
        let arg = match action {
            PowerAction::Reboot => "reboot",
            PowerAction::PowerOff => "poweroff",
        };
        #[cfg(not(test))]
        {
            match std::process::Command::new("systemctl").arg(arg).spawn() {
                Ok(_) => {}
                Err(e) => tracing::error!("greeter: `systemctl {arg}` failed: {e}"),
            }
        }
        #[cfg(test)]
        {
            let _ = arg; // never power off the test host
        }
    }

    fn take_auth_secret(&self, method: AuthMethod) -> Result<Zeroizing<String>, &'static str> {
        let mut stored = self
            .password_buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut password = Zeroizing::new(std::mem::take(&mut *stored));
        drop(stored);
        match method {
            AuthMethod::Password if password.is_empty() => Err("Enter your passcode"),
            AuthMethod::Password => Ok(password),
            AuthMethod::Fingerprint => {
                // A passcode may have been typed before the alternate factor
                // was selected. Scrub it now; it must not ride the fingerprint
                // PAM conversation or remain recoverable in the form model.
                password.zeroize();
                Ok(Zeroizing::new(String::new()))
            }
        }
    }

    fn fingerprint_available_for(&self, username: &str) -> bool {
        match self.fingerprint {
            FingerprintAvailability::Unavailable => false,
            FingerprintAvailability::Enrolled => true,
            FingerprintAvailability::CheckNamedAccount => (self.fingerprint_probe)(username),
        }
    }

    /// Whether keyboard focus currently belongs to a button in the composed
    /// card. Buttons are activated by [`ui::App`] after widget dispatch, so the
    /// form-level Enter fallback must leave their event unhandled.
    fn focused_button_present(&self) -> bool {
        fn walk(widget: &dyn Widget) -> bool {
            (widget.name() == "Button" && widget.is_focused())
                || widget.children().into_iter().any(walk)
        }

        self.composed.as_deref().is_some_and(walk)
    }

    /// Environment handed to greetd's `StartSession`. Mirrors the
    /// `DesktopNames=tilo` advertised by the session `.desktop` files.
    fn session_env() -> Vec<String> {
        vec![
            "XDG_SESSION_TYPE=wayland".to_string(),
            "XDG_SESSION_DESKTOP=tilo".to_string(),
            "XDG_CURRENT_DESKTOP=tilo".to_string(),
        ]
    }

    /// Read credentials from the document and kick off authentication.
    ///
    /// Validation failures land an inline hint in the status line. A valid
    /// submit transitions to [`AuthPhase::Authenticating`] and spawns the
    /// background worker (which pumps prompts/result back via
    /// [`UserEvent::External`]). Without an event-loop proxy (tests /
    /// headless), it falls back to a synchronous exchange so the path is
    /// still exercised.
    fn attempt_login(&mut self, state: &mut AutoCommit, root: &ObjId, method: AuthMethod) {
        // Guard: never start a second attempt while one is in flight.
        if self.phase != AuthPhase::Idle {
            return;
        }
        if method == AuthMethod::Fingerprint
            && self.fingerprint == FingerprintAvailability::Unavailable
        {
            self.set_status("No enrolled fingerprint is available for this account");
            return;
        }

        // A fixed account's identity comes from conservative host discovery,
        // not mutable document state. Multi-account mode accepts the visible
        // field and normalizes incidental surrounding whitespace before both
        // the enrollment check and greetd's account-specific PAM session.
        let username = self.login_user.as_ref().map_or_else(
            || read_text(state, root, USERNAME_PATH).unwrap_or_default(),
            |user| user.username.clone(),
        );
        let username = username.trim().to_owned();

        if username.is_empty() {
            self.set_status("Enter your username");
            return;
        }
        if method == AuthMethod::Fingerprint && !self.fingerprint_available_for(&username) {
            self.set_status("No enrolled fingerprint is available for this account");
            return;
        }
        // An explicit fingerprint attempt must not copy or submit a password
        // that happens to remain in the field. PAM/fprintd owns the factor
        // conversation and receives an empty response for secret prompts.
        let password = match self.take_auth_secret(method) {
            Ok(secret) => secret,
            Err(message) => {
                self.set_status(message);
                return;
            }
        };
        let Some(session) = self.sessions.get(self.selected_session) else {
            self.set_status("No session available");
            return;
        };
        let sock = match std::env::var("GREETD_SOCK") {
            Ok(s) if !s.is_empty() => s,
            _ => {
                self.set_status("GREETD_SOCK is not set");
                return;
            }
        };

        let argv = session.argv.clone();
        let env = Self::session_env();

        self.begin_authenticating();

        if let Some(proxy) = self.proxy.get() {
            // Live path: run on a worker thread so the UI can paint the
            // "Authenticating…" frame and live fingerprint prompts.
            spawn_auth(proxy.clone(), sock, username, password, argv, env);
        } else {
            // Fallback (no proxy wired): synchronous exchange. Blocks the
            // UI for the request's duration but still authenticates and
            // starts the session; on success the process exits inside
            // `authenticate_and_start`.
            match crate::greetd_client::authenticate_and_start(
                &sock,
                &username,
                password.as_str(),
                &argv,
                &env,
                |_| {},
                || {},
            ) {
                Ok(()) => {} // never reached — success exits the process
                Err(e) => self.finish_failure(&e.user_message(), e.detail(), state, root),
            }
        }
    }

    /// Enter the authenticating phase: clear any stale status/detail and
    /// show the spinner. Triggers a rebuild. The inputs become disabled
    /// (dropped from the focus order), so re-seat focus rather than keep a
    /// now-invalid ordinal.
    fn begin_authenticating(&mut self) {
        self.phase = AuthPhase::Authenticating;
        self.prompt.clear();
        self.status.clear();
        self.status_kind = StatusKind::None;
        self.detail.clear();
        self.dirty_structure = true;
        self.focus_reset_pending = true;
    }

    /// Return to the form after a failed attempt: surface the (already
    /// user-facing) `message` + optional `detail`, clear the password,
    /// keep the username. Focus is kept on the same field across the
    /// rebuild by the App's `autofocus_on_structure_change`.
    fn finish_failure(
        &mut self,
        message: &str,
        detail: Option<String>,
        _state: &mut AutoCommit,
        _root: &ObjId,
    ) {
        self.phase = AuthPhase::Idle;
        self.prompt.clear();
        self.status = message.to_string();
        self.status_kind = StatusKind::Error;
        self.detail = detail.unwrap_or_default();
        self.clear_password();
        self.dirty_structure = true;
        // Inputs are re-enabled (back in the focus order) — re-seat focus on
        // the rebuilt form rather than a stale auth-phase ordinal.
        self.focus_reset_pending = true;
    }

    /// Empty the process-local passcode buffer. Rebuilding the form drops and
    /// zeroizes the old input widget, so the next field starts genuinely blank.
    fn clear_password(&self) {
        self.password_buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .zeroize();
    }

    /// Surface a correctable note about the form itself. Distinct from a
    /// rejected attempt, which goes through [`GreeterApp::finish_failure`]
    /// and reads as an error.
    fn set_status(&mut self, msg: &str) {
        if self.status != msg || self.status_kind != StatusKind::Hint || !self.detail.is_empty() {
            self.status = msg.to_string();
            self.status_kind = StatusKind::Hint;
            self.detail.clear();
            self.dirty_structure = true;
        }
    }

    /// greetd accepted the credentials and is bringing the session up. The
    /// process exits moments later, but the attempt resolves into a visible
    /// success frame rather than the spinner just disappearing.
    fn begin_starting(&mut self) {
        if self.phase == AuthPhase::Starting {
            return;
        }
        self.phase = AuthPhase::Starting;
        self.prompt.clear();
        self.status.clear();
        self.status_kind = StatusKind::None;
        self.detail.clear();
        self.dirty_structure = true;
    }
}

impl ui::widget::FullscreenProvider for GreeterApp {
    fn fullscreen_intent(&self) -> Option<bool> {
        // greetd owns the output; this is a system surface, not an app window.
        Some(true)
    }
}

impl Widget for GreeterApp {
    ui::provides!(dyn ui::widget::FullscreenProvider, dyn ui::undo::UndoPoints);

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        if !self.bootstrap_done {
            self.bootstrap_crdt(state, root);
            self.bootstrap_done = true;
        }

        // The session picker writes its chosen label into the document;
        // mirror that back onto the selected index. An unrecognised label
        // (a session that vanished between renders) is ignored rather than
        // resetting the choice out from under the user.
        if let Some(name) = read_text(state, root, SESSION_PATH)
            && let Some(idx) = self.sessions.iter().position(|s| s.name == name)
            && idx != self.selected_session
        {
            self.selected_session = idx;
            self.dirty_structure = true;
        }

        // Pull local-only passcode edits into the zeroized buffer before
        // consuming a submit edge raised by Enter or the Log in button in the
        // same event cycle. No secret is written to `state` by this sync.
        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }

        // Submit rising edge — reset the flag *before* attempting so a
        // single click fires exactly one attempt.
        if read_bool(state, root, SUBMIT_PATH).unwrap_or(false) {
            let _ = state.put(root, SUBMIT_PATH, false);
            self.attempt_login(state, root, AuthMethod::Password);
        }

        if read_bool(state, root, FINGERPRINT_PATH).unwrap_or(false) {
            let _ = state.put(root, FINGERPRINT_PATH, false);
            self.attempt_login(state, root, AuthMethod::Fingerprint);
        }

        // Power-action rising edges. Reset the flag first so a single click
        // fires exactly one request.
        if read_bool(state, root, REBOOT_PATH).unwrap_or(false) {
            let _ = state.put(root, REBOOT_PATH, false);
            self.request_power(PowerAction::Reboot);
        }
        if read_bool(state, root, POWEROFF_PATH).unwrap_or(false) {
            let _ = state.put(root, POWEROFF_PATH, false);
            self.request_power(PowerAction::PowerOff);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_text_scale = theme.text_scale;
        let mut composed = self.build_composition(theme);
        let card_node = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        // Root fills the screen and centers the card both axes.
        // `new_with_children` only fails when a child belongs to a different
        // tree; `card_node` was just built in this one above.
        #[allow(
            clippy::unwrap_used,
            reason = "card_node was just created in this same tree"
        )]
        let root = taffy
            .new_with_children(
                Style {
                    display: Display::Flex,
                    flex_direction: FlexDirection::Column,
                    justify_content: Some(JustifyContent::Center),
                    align_items: Some(AlignItems::Center),
                    size: Size {
                        width: percent(1.0_f32),
                        height: percent(1.0_f32),
                    },
                    ..Default::default()
                },
                &[card_node],
            )
            .unwrap();

        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        let was_wide = self.uses_wide_layout(self.layout_text_scale);
        let was_compact = self.uses_compact_layout(self.layout_text_scale);
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // App and FrameHarness settle this rebuild before painting, including
        // the very first frame and crossings caused by display/text scaling.
        if was_wide != self.uses_wide_layout(self.layout_text_scale)
            || was_compact != self.uses_compact_layout(self.layout_text_scale)
        {
            self.dirty_structure = true;
        }
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&card_node) = children.first()
        {
            c.apply_layout(taffy, card_node, Point::new(self.rect.x0, self.rect.y0));
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
        let now = Instant::now();
        let elapsed = self.shot_time.unwrap_or_else(|| self.backdrop_elapsed(now));
        let mut backdrop = Scene::new();
        ui::auth_backdrop::draw(&mut backdrop, theme, self.rect, elapsed);
        scene.append(&backdrop, Some(transform));
        self.backdrop_deadline = if self.shot_time.is_none()
            && ui::auth_backdrop::needs_animation(theme)
            && now < self.backdrop_idle_at
        {
            now.checked_add(BACKDROP_INTERVAL)
                .map(|next| next.min(self.backdrop_idle_at))
        } else {
            None
        };
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::Focused(focused) => self.keyboard_focus = *focused,
            WindowEvent::KeyboardInput { .. } => self.keyboard_focus = true,
            _ => {}
        }
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::Touch(_)
        ) {
            self.note_backdrop_activity(Instant::now());
        }
        // Track Caps Lock from typed letters (winit's Modifiers can't report
        // the lock directly — see `ui::caps_lock`). A flip in the estimate
        // triggers a rebuild so the warning under the password field
        // appears/disappears. Focus + caret survive that rebuild via the
        // App's `rebuild_structure` continuity path.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    text: Some(t),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            let before = self.caps_lock.on();
            self.caps_lock
                .observe_text(t.as_str(), ctx.modifiers.state().shift_key());
            if self.caps_lock.on() != before {
                self.dirty_structure = true;
            }
        }

        let enter_pressed = matches!(
            event,
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key: Key::Named(NamedKey::Enter),
                    state: ElementState::Pressed,
                    ..
                },
                ..
            }
        );

        // Delegate to the composed card.
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&card_node) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, card_node));
        }

        // A focused control owns Enter. Dropdown consumes it above; buttons
        // deliberately leave it for App's standard focused-control activation.
        // Only an otherwise-unhandled Enter (normally from either text field)
        // acts as the form's default Log-in action.
        if enter_pressed && !*ctx.handled && !self.focused_button_present() {
            let _ = ctx.state.put(ctx.path, SUBMIT_PATH, true);
            *ctx.handled = true;
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mine = self.dirty_structure;
        self.dirty_structure = false;
        let child = self
            .composed
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty());
        mine || child
    }

    /// Rising-edge one-shot consumed by the App's `rebuild_structure`. We
    /// raise the flag only on auth-phase transitions (which enable/disable
    /// the inputs and so change the focusable set); plain status / caps /
    /// session-cycle rebuilds leave it `false` so focus + caret are preserved
    /// in place.
    fn take_focus_reset(&mut self) -> bool {
        let mine = std::mem::take(&mut self.focus_reset_pending);
        let child = self.composed.as_mut().is_some_and(|c| c.take_focus_reset());
        mine || child
    }

    fn on_external_event(&mut self, payload: &str, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        if payload == CONSOLE_WAKE {
            self.serve_console(state);
            return;
        }
        // Progress from the background auth worker (see `crate::auth`).
        match AuthUpdate::decode(payload) {
            Some(AuthUpdate::Prompt { text, is_error: _ }) => {
                // Reflect the live PAM prompt (e.g. fingerprint
                // instructions) while authenticating. We keep the warning
                // flag out of the rendered colour to respect OS theming —
                // the text itself carries the meaning.
                if self.phase == AuthPhase::Authenticating && self.prompt != text {
                    self.prompt = text;
                    self.dirty_structure = true;
                }
            }
            Some(AuthUpdate::Accepted) => {
                self.begin_starting();
            }
            Some(AuthUpdate::Failed { message, detail }) => {
                self.finish_failure(&message, detail, state, root);
            }
            // Unrelated External payloads are ignored.
            None => {}
        }
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let child = self.composed.as_ref().and_then(|c| c.next_wake_deadline());
        match (self.backdrop_deadline, child) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.composed
            .as_ref()
            .is_some_and(|c| c.wants_pointer_capture())
    }

    fn name(&self) -> &'static str {
        "GreeterApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::undo::UndoPoints for GreeterApp {
    fn needs_commit(&self) -> bool {
        self.composed
            .as_ref()
            .is_some_and(|c| ui::undo::needs_commit(c.as_ref()))
    }
    fn on_commit(&mut self) {
        if let Some(c) = &mut self.composed {
            ui::undo::on_commit(c.as_mut());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two entries shipped in os/usr/share/wayland-sessions/ — human
    /// names, not the `DesktopNames=` id they launch under.
    fn test_sessions() -> Vec<Session> {
        vec![
            Session {
                name: "Tilo".to_string(),
                argv: vec!["/usr/bin/shell".into(), "--backend".into(), "drm".into()],
            },
            Session {
                name: "Tilo with peer sync".to_string(),
                argv: vec![
                    "/usr/bin/shell".into(),
                    "--backend".into(),
                    "drm".into(),
                    "--enable-peer-sync".into(),
                ],
            },
        ]
    }

    fn card_of(app: &GreeterApp) -> Box<dyn Widget> {
        app.build_composition(&Theme::dark())
    }

    fn set_focus_by_id(widget: &mut dyn Widget, id: &str) {
        widget.set_focus(widget.test_id() == Some(id));
        for child in widget.children_mut() {
            set_focus_by_id(child, id);
        }
    }

    fn named_key_event(key: NamedKey) -> WindowEvent {
        use winit::keyboard::{KeyLocation, NativeKeyCode, PhysicalKey};

        // winit intentionally keeps the platform-specific key field private;
        // these are the same inert placeholders used by ui::App's key tests.
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = Key::Named(key);
        event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        event.location = KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        event.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event,
            is_synthetic: false,
        }
    }

    fn enter_event() -> WindowEvent {
        named_key_event(NamedKey::Enter)
    }

    fn dispatch_enter(app: &mut GreeterApp, state: &mut AutoCommit, focused: &str) -> bool {
        dispatch(app, state, focused, &enter_event())
    }

    fn dispatch(
        app: &mut GreeterApp,
        state: &mut AutoCommit,
        focused: &str,
        event: &WindowEvent,
    ) -> bool {
        use std::collections::HashMap;
        use taffy::AvailableSpace;

        app.sync(state, &automerge::ROOT);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = app.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: AvailableSpace::Definite(1280.0),
                    height: AvailableSpace::Definite(800.0),
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, Point::ZERO);
        set_focus_by_id(app, focused);

        let mut handled = false;
        let mut focus_target = None;
        app.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    #[test]
    fn console_reports_input_only_once_the_compositor_focuses_the_window() {
        let accepts = |app: &GreeterApp| {
            app.console_state()["accepts_input"]
                .as_bool()
                .expect("sign-in state reports whether it accepts input")
        };
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut state = AutoCommit::new();
        assert!(!accepts(&app), "a window nobody focused receives no keys");
        dispatch(&mut app, &mut state, "", &WindowEvent::Focused(true));
        assert!(accepts(&app));
        dispatch(&mut app, &mut state, "", &WindowEvent::Focused(false));
        assert!(!accepts(&app));
        // A key can only arrive at the focused window.
        dispatch(&mut app, &mut state, "", &named_key_event(NamedKey::Tab));
        assert!(accepts(&app));
    }

    /// How a widget identifies itself in a tree dump: its `test_id` when it
    /// has one, otherwise its type name.
    ///
    /// Controls expose stable semantic ids here. Visible labels remain in
    /// their accessibility nodes, but are deliberately not the automation
    /// address: labels may be localized or change with application state.
    fn ident(w: &dyn Widget) -> String {
        w.test_id()
            .map_or_else(|| w.name().to_string(), str::to_string)
    }

    /// Every focusable widget under `w`, in the depth-first order the App
    /// builds `focusable_order` in.
    fn collect_focusables(w: &dyn Widget, out: &mut Vec<String>) {
        for child in w.children() {
            if child.is_focusable() && child.is_enabled() {
                out.push(ident(child));
            }
            collect_focusables(child, out);
        }
    }

    fn accessible_name_for(w: &dyn Widget, id: &str) -> Option<String> {
        if w.test_id() == Some(id) {
            return w.accessibility().and_then(|node| node.name);
        }
        w.children()
            .into_iter()
            .find_map(|child| accessible_name_for(child, id))
    }

    fn rect_for(w: &dyn Widget, id: &str) -> Option<Rect> {
        if w.test_id() == Some(id) {
            return Some(w.get_rect());
        }
        w.children()
            .into_iter()
            .find_map(|child| rect_for(child, id))
    }

    fn rect_for_accessible_name(w: &dyn Widget, name: &str) -> Option<Rect> {
        if w.accessibility().and_then(|node| node.name).as_deref() == Some(name) {
            return Some(w.get_rect());
        }
        w.children()
            .into_iter()
            .find_map(|child| rect_for_accessible_name(child, name))
    }

    fn subtree_min_y(w: &dyn Widget) -> f64 {
        w.children()
            .into_iter()
            .fold(w.get_rect().y0, |min_y, child| {
                min_y.min(subtree_min_y(child))
            })
    }

    fn layout_at_with_theme(
        app: &mut GreeterApp,
        viewport: (f32, f32),
        theme: &Theme,
    ) -> (TaffyTree<()>, NodeId) {
        use std::collections::HashMap;
        use taffy::AvailableSpace;

        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        for _ in 0..8 {
            let mut taffy = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = app.build_layout(&mut taffy, theme, &mut node_map);
            app.sync(&mut doc, &automerge::ROOT);
            let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
            // SAFETY: this helper exclusively owns the matching tree and the
            // app stays in place through admission and the prepaint solve.
            unsafe {
                bindings.rebuild(app, &taffy, node, &node_map);
                ui::widget::compute_widget_layout(
                    &mut taffy,
                    node,
                    Size {
                        width: AvailableSpace::Definite(viewport.0),
                        height: AvailableSpace::Definite(viewport.1),
                    },
                    &node_map,
                    &bindings,
                    &mut text,
                )
            }
            .unwrap();
            app.apply_layout(&taffy, node, Point::ZERO);
            if !app.take_structure_dirty() {
                return (taffy, node);
            }
        }
        panic!("greeter composition did not settle before paint");
    }

    fn layout_at(app: &mut GreeterApp, viewport: (f32, f32)) -> (TaffyTree<()>, NodeId) {
        layout_at_with_theme(app, viewport, &Theme::dark())
    }

    /// The card's focusable widgets, identified by stable id, in tab order.
    fn focusables(app: &GreeterApp) -> Vec<String> {
        let card = card_of(app);
        let mut v = Vec::new();
        collect_focusables(card.as_ref(), &mut v);
        v
    }

    /// The *shape* of the focus order — one widget type per focusable, in
    /// tab order.
    ///
    /// This is what focus continuity actually depends on: the App restores
    /// focus by **ordinal** after a rebuild, so what must not change is
    /// which kind of control sits at each position. Labels are free to
    /// change (cycling the session rewrites that button's text) without
    /// disturbing it.
    fn focus_shape(app: &GreeterApp) -> Vec<String> {
        let card = card_of(app);
        let mut v = Vec::new();
        collect_focus_shape(card.as_ref(), &mut v);
        v
    }

    fn collect_focus_shape(w: &dyn Widget, out: &mut Vec<String>) {
        for child in w.children() {
            if child.is_focusable() {
                out.push(child.name().to_string());
            }
            collect_focus_shape(child, out);
        }
    }

    /// Every widget under `w` whose `name()` matches, at any depth.
    fn count_named(w: &dyn Widget, name: &str) -> usize {
        w.children()
            .iter()
            .map(|c| usize::from(c.name() == name).saturating_add(count_named(*c, name)))
            .sum()
    }

    /// Everything the card announces, in tree order: the accessible name of
    /// every node that has one. This is what a screen reader would read
    /// out, so asserting on it is asserting on what the screen says rather
    /// than on some parallel bookkeeping.
    fn spoken(w: &dyn Widget) -> Vec<String> {
        fn walk(w: &dyn Widget, out: &mut Vec<String>) {
            for c in w.children() {
                if let Some(name) = Widget::accessibility(c).and_then(|n| n.name) {
                    out.push(name);
                }
                walk(c, out);
            }
        }
        let mut out = Vec::new();
        walk(w, &mut out);
        out
    }

    fn live_politeness_for(w: &dyn Widget, name: &str) -> Option<Politeness> {
        if let Some(node) = w.accessibility()
            && node.name.as_deref() == Some(name)
        {
            return node.live;
        }
        w.children()
            .into_iter()
            .find_map(|child| live_politeness_for(child, name))
    }

    #[test]
    fn session_picker_labels_and_drives_the_started_session() {
        // Phase `session_label_reflects_selection`.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            assert_eq!(app.session_label(), "Tilo");
            app.selected_session = 1;
            assert_eq!(app.session_label(), "Tilo with peer sync");
        }
        // Phase `session_label_is_the_human_name_not_the_desktop_id`.
        // The greeter must never put the internal desktop id in front of the
        // user. `tilo` is a `DesktopNames=`/`UseIn=` contract with the
        // portal, not a name anyone logging in should have to read.
        {
            let app = GreeterApp::with_sessions(test_sessions());
            let label = app.session_label();
            assert!(
                !label.contains("tilo"),
                "internal desktop id leaked into the greeter: {label:?}"
            );
            assert!(
                !label.starts_with("Session:"),
                "the picker is labelled by a field label, not by a prefix: {label:?}"
            );
        }
        // Phase `empty_sessions_label_is_graceful`.
        {
            let app = GreeterApp::with_sessions(Vec::new());
            assert_eq!(app.session_label(), "No session available");
        }
        // Phase `single_session_hides_the_picker`.
        // With one session there is no choice to offer, so the picker is not
        // rendered at all — and the focusable set shrinks accordingly.
        {
            let one = vec![test_sessions().remove(0)];
            let app = GreeterApp::with_sessions(one);
            assert_eq!(
                focusables(&app),
                vec![
                    "greeter.username",
                    "greeter.password",
                    "greeter.login",
                    "greeter.fingerprint",
                    "greeter.reboot",
                    "greeter.poweroff",
                ],
                "a lone session must not render a picker"
            );

            let two = GreeterApp::with_sessions(test_sessions());
            assert!(
                focusables(&two).contains(&"greeter.session".to_string()),
                "a real choice must render a picker, got {:?}",
                focusables(&two)
            );
        }
        // Phase `picker_selection_drives_the_started_session`.
        // The picker owns the choice: it writes a label into the document and
        // the app mirrors that onto the selected index (which is what decides
        // the argv greetd is asked to start).
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;

            // Bootstrap seeds the picker with the current session, so the
            // control shows a value rather than an empty placeholder.
            app.sync(&mut doc, &root);
            assert_eq!(app.selected_session, 0);
            assert_eq!(
                read_text(&doc, &root, SESSION_PATH).as_deref(),
                Some("Tilo"),
            );

            choose_session(&mut doc, &root, "Tilo with peer sync");
            app.sync(&mut doc, &root);
            assert_eq!(app.selected_session, 1);
            assert_eq!(app.session_label(), "Tilo with peer sync");
            assert!(
                app.sessions[app.selected_session]
                    .argv
                    .contains(&"--enable-peer-sync".to_string()),
                "the choice must reach the argv handed to greetd"
            );

            choose_session(&mut doc, &root, "Tilo");
            app.sync(&mut doc, &root);
            assert_eq!(app.selected_session, 0);
        }
        // Phase `unknown_session_label_is_ignored`.
        // A label that names no known session leaves the choice alone rather
        // than silently resetting it.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            choose_session(&mut doc, &root, "Tilo with peer sync");
            app.sync(&mut doc, &root);
            assert_eq!(app.selected_session, 1);

            choose_session(&mut doc, &root, "a session that went away");
            app.sync(&mut doc, &root);
            assert_eq!(app.selected_session, 1, "choice survives a stale label");
        }
    }

    #[test]
    fn fingerprint_login_is_offered_only_for_enrolled_accounts_and_sends_no_password() {
        // Phase `fingerprint_enrollment_output_fails_closed`.
        {
            assert!(fingerprint_list_has_enrollment(
                "Fingerprints for user tester:\n - #0: right-index-finger\n"
            ));
            assert!(fingerprint_list_has_enrollment(
                "Fingerprints for user tester:\n - right-index-finger\n"
            ));
            assert!(!fingerprint_list_has_enrollment(
                "Fingerprints for user tester:\nnone\n"
            ));
            assert!(!fingerprint_list_has_enrollment("right-index-finger"));
        }
        // Phase `fingerprint_action_is_hidden_and_rejected_without_enrollment`.
        {
            let user = LoginUser {
                username: "tester".into(),
                display_name: "ada Quick".into(),
            };
            let mut app = GreeterApp::with_sessions_login_user_and_fingerprint(
                test_sessions(),
                Some(user),
                FingerprintAvailability::Unavailable,
            );
            assert!(
                !focusables(&app).contains(&"greeter.fingerprint".to_string()),
                "an unavailable factor must not appear in the form or focus order"
            );

            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            app.attempt_login(&mut doc, &automerge::ROOT, AuthMethod::Fingerprint);
            assert_eq!(
                app.status,
                "No enrolled fingerprint is available for this account"
            );
            assert_eq!(app.phase, AuthPhase::Idle);
        }
        // Phase `multi_account_fingerprint_is_resolved_for_the_typed_username`.
        {
            let mut app = GreeterApp::with_sessions_login_user_and_fingerprint(
                test_sessions(),
                None,
                FingerprintAvailability::CheckNamedAccount,
            );
            app.fingerprint_probe = only_bob_has_a_fingerprint;

            assert!(
                focusables(&app).contains(&"greeter.fingerprint".to_string()),
                "multi-account login must expose the alternate factor after asking who is signing in"
            );
            assert!(app.fingerprint_available_for("bob"));
            assert!(!app.fingerprint_available_for("alice"));
            assert!(!app.fingerprint_available_for(""));
        }
        // Phase `fingerprint_auth_explicitly_uses_no_password`.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            assert_eq!(
                app.take_auth_secret(AuthMethod::Password),
                Err("Enter your passcode")
            );
            assert_eq!(
                app.take_auth_secret(AuthMethod::Fingerprint)
                    .map(|secret| secret.to_string()),
                Ok(String::new()),
                "fingerprint PAM conversation must begin without a dummy password"
            );
            assert!(
                focusables(&app).contains(&"greeter.fingerprint".to_string()),
                "the passwordless factor must be an explicit reachable action"
            );
        }
        // Phase `fingerprint_prompt_renders_as_feedback`.
        // A live PAM prompt IS new information, so it may appear alongside the
        // button — but it must never be a restatement of it.
        {
            let app = GreeterApp::for_shot(AuthScenario::Fingerprint);
            assert_eq!(app.phase, AuthPhase::Authenticating);
            assert_eq!(app.feedback_text(), "Place your finger on the reader");
            assert_ne!(app.feedback_text(), app.login_label());

            let said = spoken(card_of(&app).as_ref());
            assert!(
                said.iter().any(|s| s == "Place your finger on the reader"),
                "the prompt must reach the card, got {said:?}"
            );
            assert_eq!(
                live_politeness_for(card_of(&app).as_ref(), "Place your finger on the reader"),
                Some(Politeness::Assertive),
                "a time-sensitive PAM instruction must be announced without moving focus"
            );
        }
    }

    fn only_bob_has_a_fingerprint(username: &str) -> bool {
        username == "bob"
    }

    /// Replace the session picker's CRDT value, as the `Dropdown` does on
    /// selection.
    fn choose_session(doc: &mut AutoCommit, root: &ObjId, name: &str) {
        let (_, obj) = doc.get(root, SESSION_PATH).unwrap().unwrap();
        let old = doc.text(&obj).unwrap_or_default().chars().count();
        let old = isize::try_from(old).expect("count fits isize");
        let _ = doc.splice_text(&obj, 0, old, name);
    }

    #[test]
    fn submit_validates_fields_keeps_the_passcode_private_and_ignores_repeats() {
        // Phase `submit_without_username_sets_status`.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            // Username left empty → submit should surface a status, not panic
            // or attempt a connection.
            let _ = doc.put(&root, SUBMIT_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.status, "Enter your username");
            assert_eq!(app.status_kind, StatusKind::Hint);
            assert!(!read_bool(&doc, &root, SUBMIT_PATH).unwrap_or(false));
        }
        // Phase `submit_without_password_sets_status`.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            // Username present, password empty → inline hint, stays Idle.
            let (_, uobj) = doc.get(&root, USERNAME_PATH).unwrap().unwrap();
            let _ = doc.splice_text(&uobj, 0, 0, "alice");
            let _ = doc.put(&root, SUBMIT_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.status, "Enter your passcode");
            assert_eq!(app.status_kind, StatusKind::Hint);
            assert_eq!(app.phase, AuthPhase::Idle);
        }
        // Phase `status_severity_distinguishes_a_rejection_from_a_blank_field`.
        // A rejected attempt and a blank field are different things and must
        // not look alike: the rejection carries error severity (and therefore
        // a leading glyph and the danger colour), the blank field does not.
        {
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;

            let mut hint = GreeterApp::with_sessions(test_sessions());
            hint.sync(&mut doc, &root);
            hint.set_status("Enter your passcode");
            assert_eq!(hint.status_kind, StatusKind::Hint);
            assert_eq!(
                count_named(card_of(&hint).as_ref(), "IconWidget"),
                0,
                "a hint is quiet — no severity glyph"
            );

            let mut err = GreeterApp::with_sessions(test_sessions());
            err.sync(&mut doc, &root);
            err.finish_failure("Incorrect username or passcode", None, &mut doc, &root);
            assert_eq!(err.status_kind, StatusKind::Error);
            assert_eq!(
                count_named(card_of(&err).as_ref(), "IconWidget"),
                1,
                "an error reads as a state, not a caption"
            );
        }
        // Phase `passcode_never_enters_automerge_or_its_saved_history`.
        {
            let app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.bootstrap_crdt(&mut doc, &root);
            seed_password(&app, "correct horse battery staple");

            assert!(doc.get(&root, "greeter.password").unwrap().is_none());
            assert!(
                !doc.save()
                    .windows("correct horse battery staple".len())
                    .any(|bytes| bytes == b"correct horse battery staple"),
                "the serialized Automerge history must not contain the passcode"
            );

            let secret = app
                .take_auth_secret(AuthMethod::Password)
                .expect("local passcode is available exactly for authentication");
            assert_eq!(secret.as_str(), "correct horse battery staple");
            assert!(app.password_buffer.lock().unwrap().is_empty());
        }
        // Phase `submit_while_authenticating_is_ignored`.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            // Pretend a request is already in flight.
            app.phase = AuthPhase::Authenticating;
            let (_, uobj) = doc.get(&root, USERNAME_PATH).unwrap().unwrap();
            let _ = doc.splice_text(&uobj, 0, 0, "alice");
            seed_password(&app, "pw");

            // A second submit must be consumed but NOT start a new attempt or
            // reset the phase.
            let _ = doc.put(&root, SUBMIT_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.phase, AuthPhase::Authenticating);
            assert!(!read_bool(&doc, &root, SUBMIT_PATH).unwrap_or(false));
            // Password is still intact (no failure was processed).
            assert_eq!(&*app.password_buffer.lock().unwrap(), "pw");
            assert!(doc.get(&root, "greeter.password").unwrap().is_none());
        }
        // Phase `submit_while_starting_is_ignored`.
        // A submit that lands while the session is already starting must not
        // restart the conversation either.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            app.begin_authenticating();
            app.begin_starting();
            let (_, uobj) = doc.get(&root, USERNAME_PATH).unwrap().unwrap();
            let _ = doc.splice_text(&uobj, 0, 0, "alice");
            seed_password(&app, "pw");

            let _ = doc.put(&root, SUBMIT_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.phase, AuthPhase::Starting);
            assert_eq!(&*app.password_buffer.lock().unwrap(), "pw");
            assert!(doc.get(&root, "greeter.password").unwrap().is_none());
        }
    }

    /// The widget tree must mirror the taffy tree **at every depth** — the
    /// App recurses both in lock-step to build `focusable_order`, so a node
    /// present in one but not the other derails the walk and silently
    /// strands every widget below it. The card nests several levels now
    /// (field groups, the status stack, the power row), so this is checked
    /// recursively rather than one level down.
    fn assert_trees_mirror(w: &dyn Widget, taffy: &TaffyTree<()>, node: NodeId, path: &str) {
        let widget_kids = w.children();
        let taffy_kids = taffy.children(node).unwrap();
        assert_eq!(
            widget_kids.len(),
            taffy_kids.len(),
            "{path} has {} widget children but {} taffy children",
            widget_kids.len(),
            taffy_kids.len()
        );
        for (i, (child, &child_node)) in widget_kids.iter().zip(taffy_kids.iter()).enumerate() {
            assert_trees_mirror(
                *child,
                taffy,
                child_node,
                &format!("{path}/{}[{i}]", ident(*child)),
            );
        }
    }

    #[test]
    fn widget_tree_and_focus_order_are_complete_and_stable() {
        // Phase `widget_tree_mirrors_taffy_tree`.
        {
            use std::collections::HashMap;
            let mut app = GreeterApp::with_sessions(test_sessions());
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

            let root = app.build_layout(&mut taffy, &theme, &mut node_map);

            // GreeterApp exposes exactly one child (the screen shell), matching the
            // single taffy child of the root node.
            assert_eq!(app.children().len(), 1);
            assert_eq!(taffy.children(root).unwrap().len(), 1);

            assert_trees_mirror(&app, &taffy, root, "GreeterApp");
        }
        // Phase `focus_order_reaches_every_control`.
        // Tab order across the whole card, however deeply the controls nest.
        {
            let app = GreeterApp::with_sessions(test_sessions());
            assert_eq!(
                focusables(&app),
                vec![
                    "greeter.username",
                    "greeter.password",
                    "greeter.login",
                    "greeter.fingerprint",
                    "greeter.session",
                    "greeter.reboot",
                    "greeter.poweroff",
                ]
            );
        }
        // Phase `focusable_order_is_stable_across_idle_state_changes`.
        // On the *form* (Idle), the focusable order must NOT shift when the
        // session is cycled or an error/detail is shown — that is the
        // precondition for the App's `autofocus_on_structure_change` to keep
        // keyboard focus + caret glued to the same field across the rebuild.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let base = focus_shape(&app);
            assert_eq!(
                base,
                vec![
                    "TextInput", // username
                    "TextInput", // password
                    "Button",    // log in
                    "Button",    // fingerprint
                    "Dropdown",  // session picker
                    "Button",    // restart
                    "Button",    // shut down
                ]
            );

            app.selected_session = 1; // pick the other session
            assert_eq!(focus_shape(&app), base, "changing session keeps order");

            app.status = "Incorrect username or passcode".to_string();
            app.status_kind = StatusKind::Error;
            app.detail = "boom".to_string();
            assert_eq!(focus_shape(&app), base, "error + detail keep order");

            app.status_kind = StatusKind::Hint;
            app.detail.clear();
            assert_eq!(focus_shape(&app), base, "a hint keeps order too");
        }
        // Phase `caps_lock_warning_does_not_shift_focusable_order`.
        // The Caps-Lock warning is a non-focusable child inserted under the
        // password field, so it must NOT shift the focusable order (caret +
        // focus continuity depend on that).
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let before = focus_shape(&app);
            let texts_off = count_named(card_of(&app).as_ref(), "Text");

            app.caps_lock.observe('A', false); // force "on"

            assert_eq!(
                focus_shape(&app),
                before,
                "the warning must not disturb tab order"
            );
            assert_eq!(
                count_named(card_of(&app).as_ref(), "Text"),
                texts_off + 1,
                "caps-lock warning adds exactly one Text"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn local_account_discovery_picks_the_passcode_form_only_when_unambiguous() {
        // Phase `sole_local_user_gets_a_one_passcode_form_with_passcode_focus_first`.
        {
            let user = LoginUser {
                username: "tester".into(),
                display_name: "ada Quick".into(),
            };
            let mut app = GreeterApp::with_sessions_and_login_user(test_sessions(), Some(user));
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);

            assert_eq!(
                read_text(&doc, &automerge::ROOT, USERNAME_PATH).as_deref(),
                Some("tester"),
                "the authenticated username is seeded without asking the user to type it"
            );
            assert_eq!(
                focusables(&app),
                vec![
                    "greeter.password",
                    "greeter.login",
                    "greeter.fingerprint",
                    "greeter.session",
                    "greeter.reboot",
                    "greeter.poweroff",
                ],
                "autofocus therefore lands on the passcode field"
            );
            let card = card_of(&app);
            assert_eq!(
                accessible_name_for(card.as_ref(), "greeter.account").as_deref(),
                Some("tester")
            );
            assert_eq!(
                accessible_name_for(card.as_ref(), "greeter.password").as_deref(),
                Some("Passcode")
            );
            assert!(
                accessible_name_for(card.as_ref(), "greeter.username").is_none(),
                "an editable username must not remain hidden in the accessibility tree"
            );
        }
        // Phase `one_passcode_autofocus_is_exposed_to_assistive_technology`.
        {
            let greeter = GreeterApp::with_sessions_and_login_user(
                test_sessions(),
                Some(LoginUser {
                    username: "tester".into(),
                    display_name: "ada Quick".into(),
                }),
            );
            let mut app = ui::App::new(ui::State::new(), Box::new(greeter));
            app.autofocus_first = true;
            app.autofocus_on_structure_change = true;
            app.init_layout();
            app.refresh_semantic_tree();

            let focused = app
                .semantic_tree()
                .iter()
                .find(|node| node.state.focused)
                .and_then(|node| node.name.as_deref());
            assert_eq!(focused, Some("Passcode"));
        }
        // Phase `ambiguous_account_discovery_keeps_named_two_field_fallback`.
        {
            let app = GreeterApp::with_sessions_and_login_user(test_sessions(), None);
            let card = card_of(&app);
            assert_eq!(
                accessible_name_for(card.as_ref(), "greeter.username").as_deref(),
                Some("Username")
            );
            assert_eq!(
                focusables(&app).first().map(String::as_str),
                Some("greeter.username")
            );
        }
        // Phase `personal_device_greets_the_known_person_before_requesting_passcode`.
        {
            let app = GreeterApp::with_sessions_and_login_user(
                test_sessions(),
                Some(LoginUser {
                    username: "tester".into(),
                    display_name: "ada Quick".into(),
                }),
            );
            let card = card_of(&app);
            let spoken = spoken(card.as_ref());

            let welcome = spoken
                .iter()
                .position(|text| text == "Welcome back to Tilo")
                .expect("personal greeter has a named welcome");
            let person = spoken
                .iter()
                .position(|text| text == "ada Quick")
                .expect("personal greeter gives the known person the headline");
            let passcode = spoken
                .iter()
                .position(|text| text == "Passcode")
                .expect("personal greeter retains its passcode field");
            assert!(
                welcome < person && person < passcode,
                "the welcome should establish context before the form: {spoken:?}"
            );
            assert_eq!(
                accessible_name_for(card.as_ref(), "greeter.welcome").as_deref(),
                Some("ada Quick")
            );
            assert_eq!(
                focusables(&app).first().map(String::as_str),
                Some("greeter.password"),
                "visual warmth must not slow keyboard login"
            );
        }
        // Phase `personal_shot_variants_match_discovered_capabilities`.
        {
            let with_fingerprint = card_of(&GreeterApp::for_shot(AuthScenario::Personal));
            let fingerprint_prompt = card_of(&GreeterApp::for_shot(AuthScenario::Fingerprint));
            let passcode_only = card_of(&GreeterApp::for_shot(AuthScenario::PersonalNoFingerprint));
            let ambiguous = card_of(&GreeterApp::for_shot(AuthScenario::Form));
            let mut with_ids = Vec::new();
            let mut passcode_ids = Vec::new();
            collect_focusables(with_fingerprint.as_ref(), &mut with_ids);
            collect_focusables(passcode_only.as_ref(), &mut passcode_ids);
            assert!(!with_ids.contains(&"greeter.username".to_string()));
            assert!(!passcode_ids.contains(&"greeter.username".to_string()));
            assert!(with_ids.contains(&"greeter.fingerprint".to_string()));
            assert!(!passcode_ids.contains(&"greeter.fingerprint".to_string()));
            assert!(
                accessible_name_for(fingerprint_prompt.as_ref(), "greeter.username").is_none(),
                "the fingerprint prompt must model the known account required for enrollment"
            );
            assert!(
                accessible_name_for(ambiguous.as_ref(), "greeter.fingerprint").is_some(),
                "multi-account login must retain a username-specific fingerprint path"
            );
        }
    }

    #[test]
    fn enter_logs_in_only_from_text_fields() {
        // Phase `enter_defaults_to_login_only_from_text_fields`.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();

            assert!(dispatch_enter(&mut app, &mut doc, "greeter.username"));
            assert!(read_bool(&doc, &automerge::ROOT, SUBMIT_PATH).unwrap_or(false));
        }
        // Phase `focused_alternate_controls_own_enter_instead_of_login`.
        {
            for control in [
                "greeter.session",
                "greeter.fingerprint",
                "greeter.reboot",
                "greeter.poweroff",
            ] {
                let mut app = GreeterApp::with_sessions(test_sessions());
                let mut doc = AutoCommit::new();
                let handled = dispatch_enter(&mut app, &mut doc, control);

                assert!(
                    !read_bool(&doc, &automerge::ROOT, SUBMIT_PATH).unwrap_or(false),
                    "Enter on {control} must not submit the password form"
                );
                if control == "greeter.session" {
                    assert!(handled, "the focused dropdown consumes Enter itself");
                } else {
                    assert!(
                        !handled,
                        "the focused {control} button must remain available to App activation"
                    );
                }
            }
        }
    }

    // --- auth lifecycle ------------------------------------------------

    /// Seed the process-local passcode buffer without touching the document.
    fn seed_password(app: &GreeterApp, pw: &str) {
        let mut value = app
            .password_buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        value.zeroize();
        value.push_str(pw);
    }

    #[test]
    fn in_flight_feedback_is_announced_once_with_appropriate_urgency() {
        // Phase `authenticating_relabels_the_button_and_draws_nothing_else`.
        // In flight, the button carries the sentence and nothing else is
        // drawn. A spinner underneath it would have nothing beside it —
        // the status line is deliberately silent unless PAM says something
        // new — and a lone dot under a full-width button reads as a
        // rendering fault rather than as progress.
        {
            let app = GreeterApp::for_shot(AuthScenario::Authenticating);
            assert_eq!(app.phase, AuthPhase::Authenticating);
            assert_eq!(app.login_label(), "Signing you in…");
            assert_eq!(
                count_named(card_of(&app).as_ref(), "Spinner"),
                0,
                "a spinner with no message beside it is a dot in empty space",
            );
        }
        // Phase `a_spinner_appears_once_there_is_something_to_say`.
        // And the moment there *is* something to say, the pair appears —
        // so this is "no spinner without a message", not "no spinner".
        {
            for scenario in [AuthScenario::Starting, AuthScenario::Fingerprint] {
                let app = GreeterApp::for_shot(scenario);
                assert!(
                    !app.feedback_text().trim().is_empty(),
                    "{scenario:?} is expected to carry a message",
                );
                assert_eq!(
                    count_named(card_of(&app).as_ref(), "Spinner"),
                    1,
                    "{scenario:?} says something, so it spins beside it",
                );
            }
        }
        // Phase `in_flight_state_is_said_exactly_once`.
        // The regression this screen was carrying: "Authenticating…" was
        // rendered by BOTH the primary button and the status line, with the
        // spinner wedged between the two copies, so the card said the same
        // sentence twice and read as a rendering fault.
        //
        // The in-flight sentence now lives on the button and nowhere else.
        {
            let app = GreeterApp::for_shot(AuthScenario::Authenticating);
            let said = spoken(card_of(&app).as_ref());

            let label = app.login_label();
            let copies = said.iter().filter(|s| s.as_str() == label).count();
            assert_eq!(
                copies, 1,
                "the in-flight sentence must appear exactly once, got {said:?}"
            );

            // And the status line is not a second copy of the button.
            assert_ne!(app.feedback_text(), label);
            assert!(
                app.feedback_text().is_empty(),
                "with no PAM prompt the status line stays silent, got {:?}",
                app.feedback_text()
            );

            // And nothing is drawn under the button at all, because there is
            // nothing to draw: no message means no spinner.
            assert_eq!(count_named(card_of(&app).as_ref(), "Spinner"), 0);
        }
        // Phase `login_feedback_is_a_live_region_with_appropriate_urgency`.
        {
            for (scenario, message, expected) in [
                (
                    AuthScenario::Hint,
                    "Enter your passcode",
                    Politeness::Assertive,
                ),
                (
                    AuthScenario::Error,
                    "Incorrect username or passcode",
                    Politeness::Assertive,
                ),
                (AuthScenario::Starting, STARTING_MESSAGE, Politeness::Polite),
            ] {
                let app = GreeterApp::for_shot(scenario);
                assert_eq!(
                    live_politeness_for(card_of(&app).as_ref(), message),
                    Some(expected),
                    "{scenario:?} feedback must be announced at the right urgency"
                );
            }
        }
        // Phase `worker_prompt_update_reflects_in_feedback`.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            app.begin_authenticating();
            // No prompt yet: the status line says nothing, because the button
            // is already saying it.
            assert_eq!(app.feedback_text(), "");

            // A fingerprint Info prompt arrives from the worker.
            let payload = crate::auth::AuthUpdate::Prompt {
                text: "Place your finger on the reader".to_string(),
                is_error: false,
            }
            .encode();
            app.on_external_event(&payload, &mut doc, &root);
            assert_eq!(app.phase, AuthPhase::Authenticating);
            assert_eq!(app.feedback_text(), "Place your finger on the reader");
        }
    }

    #[test]
    fn worker_outcomes_resolve_into_success_or_a_recoverable_form() {
        // Phase `accepted_update_resolves_into_a_visible_success_state`.
        // An attempt must always resolve into something the user can see. On
        // success greetd hands over the VT and the process exits — without an
        // explicit accepted state the spinner would simply vanish, which is
        // indistinguishable from a greeter that has hung.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            app.begin_authenticating();

            let payload = crate::auth::AuthUpdate::Accepted.encode();
            app.on_external_event(&payload, &mut doc, &root);

            assert_eq!(app.phase, AuthPhase::Starting);
            assert!(app.phase.is_busy(), "inputs stay inert while handing over");
            assert_eq!(app.feedback_text(), STARTING_MESSAGE);
            assert_eq!(app.login_label(), "Welcome back");

            let said = spoken(card_of(&app).as_ref());
            assert!(
                said.iter().any(|s| s == STARTING_MESSAGE),
                "the success frame must say so, got {said:?}"
            );
            // Still one voice: the button and the status line differ.
            assert_ne!(app.feedback_text(), app.login_label());
        }
        // Phase `worker_failure_returns_to_form_clears_password_keeps_username`.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            // Seed both fields and enter the authenticating phase.
            let (_, uobj) = doc.get(&root, USERNAME_PATH).unwrap().unwrap();
            let _ = doc.splice_text(&uobj, 0, 0, "alice");
            seed_password(&app, "wrongpw");
            app.begin_authenticating();
            assert_eq!(app.phase, AuthPhase::Authenticating);

            // Worker reports a bad-credentials failure (already user-facing).
            let payload = crate::auth::AuthUpdate::Failed {
                message: "Incorrect username or passcode".to_string(),
                detail: None,
            }
            .encode();
            app.on_external_event(&payload, &mut doc, &root);

            assert_eq!(app.phase, AuthPhase::Idle, "back to the form on failure");
            assert_eq!(app.status, "Incorrect username or passcode");
            // Password cleared, username preserved.
            assert!(app.password_buffer.lock().unwrap().is_empty());
            assert!(doc.get(&root, "greeter.password").unwrap().is_none());
            assert_eq!(
                read_text(&doc, &root, USERNAME_PATH).as_deref(),
                Some("alice")
            );
        }
        // Phase `worker_failure_with_detail_surfaces_secondary`.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            app.begin_authenticating();

            let payload = crate::auth::AuthUpdate::Failed {
                message: "Could not start the session".to_string(),
                detail: Some("session slot busy".to_string()),
            }
            .encode();
            app.on_external_event(&payload, &mut doc, &root);

            assert_eq!(app.status, "Could not start the session");
            assert_eq!(app.detail, "session slot busy");

            let said = spoken(card_of(&app).as_ref());
            assert!(said.iter().any(|s| s == "Could not start the session"));
            assert!(
                said.iter().any(|s| s == "session slot busy"),
                "the raw detail must still reach the card, got {said:?}"
            );
        }
        // Phase `failure_requests_focus_reset`.
        // A failed attempt returns to the form (inputs re-enabled) and also
        // raises a focus reset, since the focusable set changes back.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            app.begin_authenticating();
            let _ = Widget::take_focus_reset(&mut app); // drain the enter-auth reset

            app.finish_failure("nope", None, &mut doc, &root);
            assert!(
                Widget::take_focus_reset(&mut app),
                "returning to the form must request a focus reset"
            );
        }
    }

    #[test]
    fn authentication_and_power_actions_lock_input_and_reset_focus() {
        // Phase `authenticating_disables_inputs_and_requests_focus_reset`.
        // During an in-flight attempt the username + password inputs are
        // disabled, so they drop out of the focusable set — and entering the
        // phase raises a one-shot `take_focus_reset` so the App re-seats focus
        // instead of pasting a now-invalid ordinal.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            assert!(focusables(&app).contains(&"greeter.username".to_string()));

            app.begin_authenticating();

            let live = focusables(&app);
            assert!(
                !live.contains(&"greeter.username".to_string()),
                "username disabled during auth, got {live:?}"
            );
            assert!(
                !live.contains(&"greeter.password".to_string()),
                "password disabled during auth, got {live:?}"
            );
            assert_eq!(
                live,
                vec![
                    "greeter.login".to_string(),
                    "greeter.reboot".to_string(),
                    "greeter.poweroff".to_string(),
                ],
                "the primary action must remain the first focusable control during auth"
            );
            // The power actions stay reachable — you must always be able to
            // turn the machine off, even from a stuck login.
            assert!(
                live.contains(&"greeter.poweroff".to_string()),
                "power actions must survive an in-flight attempt, got {live:?}"
            );

            // The phase transition queued a focus reset (drained once).
            assert!(
                Widget::take_focus_reset(&mut app),
                "entering auth must request a focus reset"
            );
            assert!(
                !Widget::take_focus_reset(&mut app),
                "focus reset is one-shot"
            );
        }
        // Phase `authenticating_focus_reset_lands_on_primary_action`.
        // Exercise the real App focus reset, not just the greeter's widget-tree
        // inventory. When auth disables both fields, the reset must land on the
        // inert primary action rather than on Restart or Shut down.
        {
            let mut greeter = GreeterApp::with_sessions(test_sessions());
            greeter.begin_authenticating();

            let mut app = ui::App::new(ui::State::new(), Box::new(greeter));
            app.autofocus_first = true;
            app.autofocus_on_structure_change = true;
            app.init_layout();
            app.rebuild_structure();
            app.refresh_semantic_tree();

            let focused = app
                .semantic_tree()
                .iter()
                .find(|node| node.state.focused)
                .and_then(|node| node.name.as_deref());
            assert_eq!(
                focused,
                Some("Signing you in…"),
                "auth focus reset must not land on a power action"
            );
        }
        // Phase `power_flag_rising_edge_resets_and_records_action`.
        // Power-action buttons raise a boolean rising edge that `sync` consumes:
        // the flag is reset and the action is recorded (the real `systemctl`
        // call is compiled out under `cfg(test)`).
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root); // bootstrap flags

            // Reboot.
            let _ = doc.put(&root, REBOOT_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.last_power_action, Some(PowerAction::Reboot));
            assert!(
                !read_bool(&doc, &root, REBOOT_PATH).unwrap_or(false),
                "reboot flag reset"
            );

            // Power off.
            let _ = doc.put(&root, POWEROFF_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.last_power_action, Some(PowerAction::PowerOff));
            assert!(
                !read_bool(&doc, &root, POWEROFF_PATH).unwrap_or(false),
                "poweroff flag reset"
            );
        }
    }

    #[test]
    fn shot_scenarios_compose_and_the_backdrop_stays_bounded() {
        // Phase `every_shot_scenario_composes`.
        // Every scenario the `--shots` path can render must actually compose,
        // so a snapshot can never silently become a blank frame.
        {
            for scenario in [
                AuthScenario::Form,
                AuthScenario::Hint,
                AuthScenario::Error,
                AuthScenario::Authenticating,
                AuthScenario::Fingerprint,
                AuthScenario::Starting,
                AuthScenario::CapsLock,
                AuthScenario::LongName,
                AuthScenario::Personal,
                AuthScenario::PersonalError,
                AuthScenario::PersonalAuthenticating,
                AuthScenario::PersonalCapsLock,
                AuthScenario::PersonalHint,
                AuthScenario::PersonalStarting,
                AuthScenario::PersonalNoFingerprint,
            ] {
                let app = GreeterApp::for_shot(scenario);
                let card = card_of(&app);
                assert!(
                    !card.children().is_empty(),
                    "{scenario:?} composed an empty card"
                );
                // The primary action is present and reachable in every state.
                let mut f = Vec::new();
                collect_focusables(card.as_ref(), &mut f);
                assert!(
                    f.contains(&"greeter.login".to_string()),
                    "{scenario:?} lost the primary action, got {f:?}"
                );
                assert_eq!(
                    accessible_name_for(card.as_ref(), "greeter.login").as_deref(),
                    Some(app.login_label()),
                    "{scenario:?} must keep the state-specific visible label on the stable action"
                );
            }
        }
        // Phase `backdrop_schedules_bounded_frames_and_stops_for_accessibility`.
        {
            let mut reduced = Theme::dark();
            reduced.reduce_motion = true;
            for (theme, animated) in [
                (Theme::dark(), true),
                (Theme::light(), true),
                (reduced, false),
                (Theme::high_contrast(), false),
            ] {
                let mut app = GreeterApp::with_sessions_and_login_user(
                    test_sessions(),
                    Some(LoginUser {
                        username: "ada".into(),
                        display_name: "ada Example".into(),
                    }),
                );
                let _layout = layout_at_with_theme(&mut app, (1280.0, 800.0), &theme);
                let before = std::time::Instant::now();
                app.draw(
                    &mut Scene::new(),
                    Affine::IDENTITY,
                    &mut TextRenderer::new(),
                    &theme,
                    &PresenceCtx::empty(),
                );
                let deadline = app.next_wake_deadline();
                if animated {
                    let deadline = deadline.expect("moving backdrop needs a scheduled redraw");
                    assert!(deadline > before, "backdrop must let the event loop sleep");
                    assert!(
                        deadline
                            <= std::time::Instant::now() + std::time::Duration::from_millis(50)
                    );
                } else {
                    assert!(
                        deadline.is_none(),
                        "static accessibility presentation must sleep"
                    );
                }
                assert!(
                    !app.needs_animation_frame(),
                    "backdrop uses a bounded cadence, not vsync spin"
                );
            }
        }
        // Phase `backdrop_settles_when_unattended_and_input_resumes_it`.
        backdrop_settles_when_unattended_and_input_resumes_it();
    }

    /// Phase of `shot_scenarios_compose_and_the_backdrop_stays_bounded`.
    fn backdrop_settles_when_unattended_and_input_resumes_it() {
        let theme = Theme::dark();
        let mut app = GreeterApp::with_sessions_and_login_user(test_sessions(), None);
        let _layout = layout_at_with_theme(&mut app, (1280.0, 800.0), &theme);
        let draw = |app: &mut GreeterApp| {
            app.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                &PresenceCtx::empty(),
            );
        };
        draw(&mut app);
        assert!(app.next_wake_deadline().is_some(), "a fresh greeter moves");

        let now = std::time::Instant::now();
        let later = |by| {
            now.checked_add(by)
                .expect("the monotonic clock has room ahead")
        };
        app.backdrop_resumed = now
            .checked_sub(BACKDROP_ACTIVE.saturating_add(std::time::Duration::from_secs(5)))
            .expect("the monotonic clock is past 25 s");
        app.backdrop_idle_at = backdrop_idle_after(app.backdrop_resumed);
        draw(&mut app);
        assert!(
            app.next_wake_deadline().is_none(),
            "an unattended greeter must stop scheduling backdrop frames"
        );
        let held = app.backdrop_elapsed(now);
        assert!(
            (held - BACKDROP_ACTIVE.as_secs_f64()).abs() < 1e-6,
            "the settled backdrop holds the frame it reached, got {held}"
        );
        assert!(
            (app.backdrop_elapsed(later(std::time::Duration::from_secs(60))) - held).abs() < 1e-9,
            "the held frame does not move while nobody is there"
        );

        app.note_backdrop_activity(now);
        let resumed = app
            .next_wake_deadline()
            .expect("input must wake the loop to resume the backdrop");
        assert!(resumed <= std::time::Instant::now());
        assert!(
            (app.backdrop_elapsed(now) - held).abs() < 1e-9,
            "the backdrop resumes from the held frame instead of jumping"
        );
        draw(&mut app);
        assert!(
            app.next_wake_deadline().is_some(),
            "and keeps moving after input"
        );
        assert!(
            app.backdrop_elapsed(later(BACKDROP_ACTIVE.saturating_mul(2))) - held
                <= BACKDROP_ACTIVE.as_secs_f64() + 1e-6,
            "one input buys one active window, not an endless animation"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn wide_viewport_scales_headings_once_and_separates_welcome_from_credentials() {
        // Phase `wide_headings_apply_accessibility_scale_once`.
        // Theme typography is already scaled. Passing those resolved sizes to
        // renderer-relative Text would apply the accessibility factor twice.
        {
            fn heading_height(theme: &Theme, personal: bool) -> f64 {
                let user = personal.then(|| LoginUser {
                    username: "ada".into(),
                    display_name: "Ada".into(),
                });
                let app = GreeterApp::with_sessions_and_login_user(test_sessions(), user);
                let mut brand = app.build_brand(theme, true);
                let mut taffy = TaffyTree::new();
                let mut nodes = std::collections::HashMap::new();
                let node = brand.build_layout(&mut taffy, theme, &mut nodes);
                let mut text = TextRenderer::new();
                text.set_text_scale(theme.text_scale);
                let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
                // SAFETY: the matching tree and widgets remain exclusively owned
                // and stationary throughout admission and the production solve.
                unsafe {
                    bindings.rebuild(brand.as_ref(), &taffy, node, &nodes);
                    ui::widget::compute_widget_layout(
                        &mut taffy,
                        node,
                        Size {
                            // Enough width isolates scaling from line wrapping.
                            width: taffy::AvailableSpace::Definite(2000.0),
                            height: taffy::AvailableSpace::MaxContent,
                        },
                        &nodes,
                        &bindings,
                        &mut text,
                    )
                }
                .unwrap();
                brand.apply_layout(&taffy, node, Point::ZERO);
                if personal {
                    rect_for(brand.as_ref(), "greeter.welcome")
                        .unwrap()
                        .height()
                } else {
                    rect_for_accessible_name(brand.as_ref(), "Tilo")
                        .unwrap()
                        .height()
                }
            }
            for personal in [false, true] {
                let normal = heading_height(&Theme::dark(), personal);
                let enlarged = heading_height(&Theme::dark().with_text_scale(1.5), personal);
                assert!(
                    normal > 0.0 && enlarged > 0.0,
                    "production heading measurement must run: {normal} → {enlarged}"
                );
                let ratio = enlarged / normal;
                assert!(
                    (1.45..=1.55).contains(&ratio),
                    "heading should grow once at 1.5×, personal={personal}: {normal} → {enlarged} ({ratio}×)"
                );
            }
        }
        // Phase `wide_viewport_separates_welcome_from_credentials`.
        // Wide identity and credential regions must not overlap. Reusing a
        // centered narrow card after a resize would fail this production layout.
        {
            let mut app = GreeterApp::for_shot(AuthScenario::Personal);
            let _layout = layout_at(&mut app, (1440.0, 900.0));
            let welcome = rect_for(&app, "greeter.welcome").expect("welcome");
            let password = rect_for(&app, "greeter.password").expect("passcode");
            assert!(
                welcome.x1 + 48.0 <= password.x0,
                "wide welcome and credentials must occupy separate regions: {welcome:?}, {password:?}"
            );
            assert!(
                welcome.y0 < password.y1 && welcome.y1 > password.y0 - 180.0,
                "wide identity should sit alongside the form: {welcome:?}, {password:?}"
            );

            let _layout = layout_at(&mut app, (640.0, 480.0));
            let welcome = rect_for(&app, "greeter.welcome").expect("welcome");
            let password = rect_for(&app, "greeter.password").expect("passcode");
            assert!(
                welcome.y1 <= password.y0,
                "compact welcome precedes credentials"
            );
            assert!(
                welcome.x0 < password.x1 && welcome.x1 > password.x0,
                "compact welcome and credentials share one column"
            );

            // A height-only resize must settle the enlarged-text composition in
            // both directions, even though neither viewport qualifies as wide.
            let theme = Theme::dark().with_text_scale(1.3);
            let _layout = layout_at_with_theme(&mut app, (640.0, 720.0), &theme);
            let tall_password = rect_for(&app, "greeter.password").expect("tall passcode");
            let mut tall_actions = Vec::new();
            collect_focusables(&app, &mut tall_actions);
            let _layout = layout_at_with_theme(&mut app, (640.0, 480.0), &theme);
            let short_password = rect_for(&app, "greeter.password").expect("short passcode");
            assert!(
                short_password.width() > tall_password.width(),
                "short enlarged-text form should use available width before wrapping"
            );
            let mut short_actions = Vec::new();
            collect_focusables(&app, &mut short_actions);
            assert_eq!(
                short_actions, tall_actions,
                "height adaptation must retain every action and its focus order"
            );
            let footer_top = rect_for(&app, "greeter.reboot").expect("power footer").y0;
            assert!(
                rect_for(&app, "greeter.fingerprint")
                    .expect("fingerprint")
                    .y1
                    <= footer_top
            );
            let _layout = layout_at_with_theme(&mut app, (640.0, 720.0), &theme);
            let restored = rect_for(&app, "greeter.password").expect("restored passcode");
            assert!(
                (restored.width() - tall_password.width()).abs() < 1.0,
                "a taller viewport should restore the comfortable reading width"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn narrow_and_resized_viewports_keep_identity_and_actions_visible() {
        // Phase `narrow_enlarged_identity_fits_and_settles_on_width_only_resize`.
        {
            let theme = Theme::dark().with_text_scale(2.0);
            for scenario in [
                AuthScenario::Personal,
                AuthScenario::PersonalCapsLock,
                AuthScenario::PersonalError,
            ] {
                let mut app = GreeterApp::for_shot(scenario);
                // This regression requires a session picker. Snapshot discovery
                // follows the host, whose CI image may expose zero or one session.
                // Inject both choices before the first CRDT sync and layout.
                app.sessions = test_sessions();
                let _layout = layout_at_with_theme(&mut app, (640.0, 640.0), &theme);
                let ordinary = rect_for(&app, "greeter.welcome").expect("ordinary identity");
                let mut original_actions = Vec::new();
                collect_focusables(&app, &mut original_actions);

                let _layout = layout_at_with_theme(&mut app, (320.0, 640.0), &theme);
                let heading = rect_for(&app, "greeter.welcome").expect("compact identity");
                let label = accessible_name_for(&app, "greeter.welcome").expect("identity name");
                let mut text = TextRenderer::new();
                text.set_text_scale(2.0);
                // The compact identity uses the shared Heading role. Measure the
                // actual name with that role's base style and the live text scale.
                let style = Theme::dark()
                    .tokens
                    .typography
                    .style(ui::tokens::TextRole::Heading);
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "bounded logical test viewport"
                )]
                let width = heading.width() as f32;
                let (painted_width, painted_height) =
                    text.measure_style(&label, Some(width), &style);
                assert!(painted_width > 0.0 && painted_height > 0.0);
                assert!(
                    f64::from(painted_width) <= heading.width() + 0.5
                        && f64::from(painted_height) <= heading.height() + 0.5,
                    "{scenario:?} compact identity must contain actual scaled glyphs: {heading:?}, text={painted_width}×{painted_height}"
                );
                assert!(heading.x0 >= 16.0 && heading.x1 <= 304.0 && heading.y0 >= 0.0);
                assert!(
                    heading.height() < ordinary.height(),
                    "narrow presentation must release decorative space despite having less width"
                );
                let footer_top = rect_for(&app, "greeter.reboot").expect("footer").y0;
                for id in ["greeter.password", "greeter.login", "greeter.fingerprint"] {
                    let rect = rect_for(&app, id).expect("authentication control");
                    assert!(
                        rect.y0 >= heading.y1 && rect.y1 <= footer_top,
                        "{scenario:?} at narrow 2× must keep {id} visible above power: {rect:?}, footer={footer_top}"
                    );
                }
                let mut narrow_actions = Vec::new();
                collect_focusables(&app, &mut narrow_actions);
                // The full welcome needs overflow scrolling at this text scale;
                // the compact identity fits until a status adds content. Preserve
                // every credential and power target, plus overflow keyboard access.
                assert_eq!(
                    original_actions,
                    [
                        "Scrollable",
                        "greeter.password",
                        "greeter.login",
                        "greeter.fingerprint",
                        "greeter.session",
                        "greeter.reboot",
                        "greeter.poweroff",
                    ]
                );
                let mut expected_narrow = vec![
                    "greeter.password",
                    "greeter.login",
                    "greeter.fingerprint",
                    "greeter.session",
                    "greeter.reboot",
                    "greeter.poweroff",
                ];
                if matches!(scenario, AuthScenario::PersonalError) {
                    expected_narrow.insert(0, "Scrollable");
                }
                assert_eq!(narrow_actions, expected_narrow, "{scenario:?}");

                let _layout = layout_at_with_theme(&mut app, (640.0, 640.0), &theme);
                let restored = rect_for(&app, "greeter.welcome").expect("restored identity");
                assert!(
                    (restored.height() - ordinary.height()).abs() < 1.0,
                    "width-only expansion must restore the full welcome before paint"
                );
            }
        }
        // Phase `narrow_viewport_keeps_auth_and_power_actions_visible`.
        // A narrow portrait display must keep the complete primary workflow on
        // screen instead of clipping the fixed-width card at both edges.
        {
            let mut app = GreeterApp::for_shot(AuthScenario::LongName);
            let _layout = layout_at(&mut app, (320.0, 640.0));

            for id in ["greeter.login", "greeter.reboot", "greeter.poweroff"] {
                let rect = rect_for(&app, id).unwrap_or_else(|| panic!("missing {id}"));
                assert!(rect.x0 >= 15.5, "{id} escaped left: {rect:?}");
                assert!(rect.x1 <= 304.5, "{id} escaped right: {rect:?}");
                assert!(rect.y0 >= 0.0 && rect.y1 <= 640.0, "{id} clipped: {rect:?}");
            }
            assert!(
                rect_for(&app, "greeter.fingerprint").is_some(),
                "multi-account login must keep the username-specific fingerprint action reachable"
            );
        }
        // Phase `live_greeter_requests_full_output_and_survives_cage_resize`.
        {
            let mut app = GreeterApp::for_shot(AuthScenario::PersonalNoFingerprint);
            assert_eq!(
                ui::widget::FullscreenWidgetExt::fullscreen_intent(&app),
                Some(true)
            );
            // The live Wayland configure can arrive after the default first size.
            // Both height-only growth and a later shrink must repaint the output.
            for (width, height) in [(800.0, 600.0), (1024.0, 768.0), (1024.0, 600.0)] {
                let _layout = layout_at(&mut app, (width, height));
                assert_eq!(
                    app.get_rect(),
                    Rect::new(0.0, 0.0, f64::from(width), f64::from(height))
                );
                for id in ["greeter.login", "greeter.reboot", "greeter.poweroff"] {
                    let rect = rect_for(&app, id).unwrap_or_else(|| panic!("missing {id}"));
                    assert!(
                        rect.x0 >= 0.0 && rect.x1 <= f64::from(width),
                        "{id}: {rect:?}"
                    );
                    assert!(
                        rect.y0 >= 0.0 && rect.y1 <= f64::from(height),
                        "{id}: {rect:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn power_footer_reserves_space_for_scaled_labels_and_icons() {
        // Footer rectangles alone do not prove that enlarged labels fit: fixed
        // widths let the button painter draw glyphs outside their hit targets.
        {
            for (viewport, scale, allow_wrap) in [
                ((320.0, 640.0), 1.0, false),
                ((640.0, 480.0), 1.3, false),
                ((640.0, 480.0), 2.0, false),
                ((640.0, 480.0), 3.0, false),
                ((640.0, 480.0), 4.0, false),
                ((320.0, 640.0), 2.0, true),
            ] {
                let theme = Theme::dark().with_text_scale(scale);
                let mut app = GreeterApp::for_shot(AuthScenario::Personal);
                let _layout = layout_at_with_theme(&mut app, viewport, &theme);
                let mut text = TextRenderer::new();
                text.set_text_scale(scale);
                for id in ["greeter.reboot", "greeter.poweroff"] {
                    let rect = rect_for(&app, id).expect("power action");
                    let label = accessible_name_for(&app, id).expect("power label");
                    // The shared Button paints its label at a 14px base size and
                    // its 44px target uses a 22px icon with a 6px label gap. Check
                    // actual font measurement plus at least 8px breathing room on
                    // each side, independently of Button's intrinsic-width result.
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "bounded logical test viewport"
                    )]
                    let max_width = allow_wrap
                        .then(|| rect.width() as f32 - theme.padding_standard * 2.0 - 28.0);
                    let (label_width, label_height) = text.measure(&label, 14.0, max_width);
                    assert!(label_width > 0.0 && label_height > 0.0);
                    assert!(
                        rect.width() >= f64::from(label_width) + 22.0 + 6.0 + 16.0,
                        "{id} at {scale}× cannot contain its painted label and icon: {rect:?}, label={label_width}"
                    );
                    assert!(
                        rect.height() >= 44.0 && rect.height() >= f64::from(label_height),
                        "{id} at {scale}× needs a complete target and unclipped glyphs: {rect:?}"
                    );
                    assert!(
                        rect.height() + 0.5 >= f64::from(label_height + theme.padding_standard),
                        "{id} at {scale}× must reserve vertical label padding: {rect:?}, text={label_height}"
                    );
                    assert!(
                        rect.x0 >= 16.0 && rect.x1 <= f64::from(viewport.0) - 16.0,
                        "{id} escaped the footer at {scale}×: {rect:?}"
                    );
                    assert!(
                        rect.y0 >= 0.0 && rect.y1 <= f64::from(viewport.1) - 16.0,
                        "{id} must remain inside the viewport at {scale}×: {rect:?}"
                    );
                }
                let restart = rect_for(&app, "greeter.reboot").unwrap();
                let shutdown = rect_for(&app, "greeter.poweroff").unwrap();
                if allow_wrap {
                    assert!(
                        restart.x1 <= shutdown.x0
                            || shutdown.x1 <= restart.x0
                            || restart.y1 <= shutdown.y0
                            || shutdown.y1 <= restart.y0,
                        "wrapped footer targets must remain disjoint: {restart:?}, {shutdown:?}"
                    );
                } else {
                    assert!(
                        restart.x1 + f64::from(theme.gap_standard) <= shutdown.x0 + 0.5,
                        "power labels need distinct, separated targets at {scale}×"
                    );
                    assert!(
                        (restart.y0 - shutdown.y0).abs() < 0.5,
                        "normal narrow and enlarged desktop footers should keep both actions on one row"
                    );
                }
            }
        }
    }

    #[test]
    fn large_text_viewport_keeps_auth_visible_and_power_reachable() {
        // At the 640×480 logical viewport produced by common 2× display
        // scaling, the sign-in and power actions remain immediately available.
        // The content card may scroll, but machine controls deliberately live
        // in a persistent footer rather than disappearing with it.
        {
            for scale in [1.3, 2.0] {
                let theme = Theme::dark().with_text_scale(scale);
                for scenario in [
                    AuthScenario::Form,
                    AuthScenario::Error,
                    AuthScenario::Authenticating,
                    AuthScenario::CapsLock,
                    AuthScenario::LongName,
                    AuthScenario::Personal,
                    AuthScenario::PersonalError,
                    AuthScenario::PersonalAuthenticating,
                    AuthScenario::Fingerprint,
                    AuthScenario::PersonalCapsLock,
                    AuthScenario::PersonalHint,
                    AuthScenario::PersonalStarting,
                    AuthScenario::PersonalNoFingerprint,
                ] {
                    let mut app = GreeterApp::for_shot(scenario);
                    let _layout = layout_at_with_theme(&mut app, (640.0, 480.0), &theme);
                    let footer_top = rect_for(&app, "greeter.reboot").expect("power footer").y0;
                    assert!(
                        subtree_min_y(&app) >= 0.0,
                        "{scenario:?} at {scale}x starts with content above the viewport: {}",
                        subtree_min_y(&app)
                    );

                    for id in ["greeter.login", "greeter.reboot", "greeter.poweroff"] {
                        let rect = rect_for(&app, id).unwrap_or_else(|| panic!("missing {id}"));
                        assert!(
                            rect.y0 >= 0.0 && rect.y1 <= 480.0,
                            "{scenario:?} at {scale}x clips {id}: {rect:?}"
                        );
                        if id == "greeter.login" {
                            assert!(
                                rect.y1 <= footer_top,
                                "{scenario:?} at {scale}x lets the footer cover {id}: {rect:?}"
                            );
                        }
                    }
                    if app.fingerprint != FingerprintAvailability::Unavailable
                        && !app.phase.is_busy()
                    {
                        let rect = rect_for(&app, "greeter.fingerprint")
                            .expect("enrolled fingerprint action");
                        assert!(
                            rect.y0 >= 0.0 && rect.y1 <= footer_top,
                            "{scenario:?} at {scale}x clips fingerprint action: {rect:?}"
                        );
                    } else {
                        assert!(rect_for(&app, "greeter.fingerprint").is_none());
                    }
                    let required_live_message = match scenario {
                        AuthScenario::Fingerprint => Some("Place your finger on the reader"),
                        AuthScenario::PersonalStarting => Some(STARTING_MESSAGE),
                        _ => None,
                    };
                    if let Some(message) = required_live_message {
                        let rect = rect_for_accessible_name(&app, message)
                            .unwrap_or_else(|| panic!("missing live message {message:?}"));
                        assert!(
                            rect.y0 >= 0.0 && rect.y1 <= footer_top,
                            "{scenario:?} at {scale}x clips live message {message:?}: {rect:?}"
                        );
                    }
                }
            }

            // The installed image can offer the standard and peer-sync sessions;
            // the additional picker must not push authentication out of reach.
            let mut app = GreeterApp::with_sessions(test_sessions());
            let theme = Theme::dark().with_text_scale(2.0);
            let _layout = layout_at_with_theme(&mut app, (640.0, 480.0), &theme);
            let footer_top = rect_for(&app, "greeter.reboot").expect("power footer").y0;
            for id in [
                "greeter.login",
                "greeter.fingerprint",
                "greeter.reboot",
                "greeter.poweroff",
            ] {
                let rect = rect_for(&app, id).unwrap_or_else(|| panic!("missing {id}"));
                assert!(
                    rect.y0 >= 0.0 && rect.y1 <= 480.0,
                    "session picker pushes {id} out of the scaled viewport: {rect:?}"
                );
                if matches!(id, "greeter.login" | "greeter.fingerprint") {
                    assert!(
                        rect.y1 <= footer_top,
                        "power footer covers {id} with the session picker: {rect:?}"
                    );
                }
            }
        }
    }
}

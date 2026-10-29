//! [`GreeterApp`] — the root login widget.
//!
//! The sign-in screen reads top to bottom as time, who, passcode, and then
//! everything secondary. The time and date are the hero, set large and light
//! on the shared authentication backdrop; below them the person signing in
//! (their avatar and name, or a choice of accounts when the machine has
//! several), the passcode field with its show/hide control and sign-in
//! action, one status line, and the fingerprint action when a print is
//! enrolled. Along the bottom edge sit the accessibility controls on one
//! side and the machine's state and power on the other. Every control is a
//! toolkit widget.
//!
//! IMPORTANT (focus): every layout node has a matching widget in `children()`.
//! This keeps the toolkit's focus, accessibility and layout traversals aligned
//! through every grouping and across recomposition.
//!
//! IMPORTANT (frames): the backdrop is still, and every container forwards
//! its children's damage instead of claiming its whole box. A keystroke
//! therefore repaints the passcode field, not the screen; the clock rebuilds
//! the screen once a minute.
//!
//! The greeter keeps no document. A sign-in attempt is not data: it lives
//! for one process and ends in a session or in a message on this screen.
//! Every control therefore reports to the greeter directly:
//!   * the username field and the session picker edit [`ui::LocalState`]s
//!     the greeter reads when an attempt starts;
//!   * the sign-in, fingerprint, account, accessibility and power controls
//!     raise one-shot requests the greeter takes on its next sync.
//!
//! The passcode is held only in a process-local, zeroized buffer shared with
//! a local-mode [`TextInput`]. It never enters an Automerge object, change
//! history, persistence, presence, or peer sync.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use taffy::style_helpers::{length, percent};
use taffy::{
    AlignItems, Dimension, Display, FlexDirection, JustifyContent, NodeId, Size, Style, TaffyTree,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, NamedKey};
use zeroize::{Zeroize, Zeroizing};

use ui::a11y::AccessibilityRequest;
use ui::app::UserEvent;
use ui::caps_lock::CapsLockEstimate;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::{NumericStyle, OpenTypeTag, TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    Avatar, AvatarSize, Button, ButtonRole, ControlSize, Dropdown, FieldRole, IconWidget,
    Presentation, Row, Scrollable, SecretKind, SecretPrompt, Spacer, Text, TextInput, Widget,
};

mod boot_mode;
mod console;
mod recovery;
mod sign_in_comforts;
mod touch_keyboard;

pub use console::{CONSOLE_WAKE, PROVIDER as CONSOLE_PROVIDER};

use crate::auth::boot::{self, BootLink, Note, ToGreeter};
use crate::auth::{AuthSession, AuthUpdate, Failure, spawn_auth, spawn_handoff};
use crate::device_status::DeviceStatus;
use crate::i18n::{t, t_args};
use crate::local_users::{LoginUser, discover_login_users, remember_user, remembered_user};
use crate::sessions::{DEFAULT_SESSIONS_DIR, Session, read_sessions};
use tilo_lock::MutexRecover as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthMethod {
    Password,
    Fingerprint,
}

/// The credentials column's comfortable width (logical px). It narrows with
/// the viewport.
const FORM_WIDTH: f32 = 360.0;
/// The passcode field's band at normal text size; the sign-in action beside
/// it grows with the field.
const FIELD_HEIGHT: f32 = 40.0;
/// The footer's session picker, wide enough for a session's name.
const SESSION_PICKER_WIDTH: f32 = 200.0;
/// Below this logical height, or at large text, the screen tightens its
/// spacing and avatar so the form stays above the footer.
const COMPACT_VIEWPORT_HEIGHT: f64 = 600.0;
const COMPACT_TEXT_SCALE: f32 = 1.5;
/// The time is display type: as large as the screen comfortably allows,
/// between these bounds (logical px).
const CLOCK_MIN: f32 = 64.0;
const CLOCK_MAX: f32 = 200.0;
/// A light cut of the variable UI face for the display time.
const CLOCK_WEIGHT: f32 = 280.0;
/// The larger-text control's step.
const LARGER_TEXT_SCALE: f32 = 1.5;
/// The still backdrop's composition, the one the lock screen draws too.
const BACKDROP_PHASE_SECONDS: f64 = ui::auth_backdrop::STILL_SECONDS;

/// How the screen arranges the time and the credentials. Each is a complete
/// composition; see `GreeterApp::build_composition`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// The lock screen's arrangement: time and date on the left, the person
    /// and passcode on the right.
    Split,
    /// One centred column: the time above, the person and passcode below.
    Column,
    /// The time as a poster across the bottom left, the credentials to its
    /// right.
    Poster,
}

/// Whether assistive technology reads a [`Stack`] and its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StackAccess {
    Read,
    Hidden,
}

/// A flex group with an explicit gap and optional padding and width.
///
/// Every grouping in the greeter is one of these. It exists as a real
/// `Widget` rather than a bare taffy node because the App discovers
/// focusable widgets by walking the widget tree (`Widget::children`) in
/// lock-step with the taffy tree. Because each stack contributes exactly one
/// taffy node whose taffy children are its widget children, the two trees
/// mirror each other by construction at every depth.
struct Stack {
    children: Vec<Box<dyn Widget>>,
    gap: f32,
    padding: f32,
    direction: FlexDirection,
    align: AlignItems,
    justify: JustifyContent,
    /// `None` fills the parent's width.
    width: Option<f32>,
    max_width: Option<f32>,
    fill_height: bool,
    width_auto: bool,
    grow: bool,
    /// Pinned at this box of its parent instead of taking part in the
    /// parent's flow: the primary output's part of a window spanning several.
    place: Option<Rect>,
    /// Out of the accessibility tree when hidden: what another display
    /// repeats of the screen (its clock) is read once, where the card is.
    access: StackAccess,
    rect: Rect,
}

impl Stack {
    /// A column whose children span its width.
    fn column(gap: f32, children: Vec<Box<dyn Widget>>) -> Self {
        Self {
            children,
            gap,
            padding: 0.0,
            direction: FlexDirection::Column,
            align: AlignItems::Stretch,
            justify: JustifyContent::Start,
            width: None,
            max_width: None,
            fill_height: false,
            width_auto: false,
            grow: false,
            place: None,
            access: StackAccess::Read,
            rect: Rect::ZERO,
        }
    }

    /// A row of children at their own size.
    fn row(gap: f32, children: Vec<Box<dyn Widget>>) -> Self {
        let mut stack = Self::column(gap, children);
        stack.direction = FlexDirection::Row;
        stack.align = AlignItems::Center;
        stack
    }

    fn align(mut self, align: AlignItems) -> Self {
        self.align = align;
        self
    }

    fn justify(mut self, justify: JustifyContent) -> Self {
        self.justify = justify;
        self
    }

    fn padding(mut self, padding: f32) -> Self {
        self.padding = padding;
        self
    }

    fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    fn max_width(mut self, width: f32) -> Self {
        self.max_width = Some(width);
        self
    }

    fn fill_height(mut self) -> Self {
        self.fill_height = true;
        self
    }

    fn grow(mut self) -> Self {
        self.grow = true;
        self
    }

    /// Size to the children instead of filling the parent.
    fn width_auto(mut self) -> Self {
        self.width_auto = true;
        self
    }

    /// Occupy exactly `area` of the parent, out of its flow.
    fn place(mut self, area: Rect) -> Self {
        self.place = Some(area);
        self
    }

    /// Hide the stack and its children from assistive technology.
    fn decorative(mut self) -> Self {
        self.access = StackAccess::Hidden;
        self
    }
}

impl Widget for Stack {
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
        let row = self.direction == FlexDirection::Row;
        // `new_with_children` only fails when a child belongs to a different
        // tree; every entry in `child_nodes` was just built in this one above.
        let style = Style {
            display: Display::Flex,
            flex_direction: self.direction,
            align_items: Some(self.align),
            justify_content: Some(self.justify),
            gap: Size {
                width: length(if row { self.gap } else { 0.0 }),
                height: length(if row { 0.0 } else { self.gap }),
            },
            padding: taffy::Rect {
                left: length(self.padding),
                right: length(self.padding),
                top: length(self.padding),
                bottom: length(self.padding),
            },
            size: Size {
                width: match (self.width, self.width_auto) {
                    (Some(width), _) => length(width),
                    (None, true) => Dimension::auto(),
                    (None, false) => percent(1.0_f32),
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
            flex_grow: if self.grow { 1.0 } else { 0.0 },
            flex_shrink: if self.grow { 1.0 } else { 0.0 },
            ..Default::default()
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "taffy lays out in f32; output geometry is far inside its range"
        )]
        let style = match self.place {
            Some(area) => Style {
                position: taffy::Position::Absolute,
                inset: taffy::Rect {
                    left: length(area.x0 as f32),
                    top: length(area.y0 as f32),
                    right: taffy::LengthPercentageAuto::auto(),
                    bottom: taffy::LengthPercentageAuto::auto(),
                },
                size: Size {
                    width: length(area.width() as f32),
                    height: length(area.height() as f32),
                },
                ..style
            },
            None => style,
        };
        taffy.widget_node(style, &child_nodes, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
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
        for child in &mut self.children {
            child.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Ok(child_nodes) = ctx.taffy.children(ctx.node_id) {
            ui::widget::route_to_children(&mut self.children, &child_nodes, event, ctx);
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

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in &self.children {
            visitor(child.as_ref());
        }
    }

    fn needs_redraw(&self) -> bool {
        self.children.iter().any(|c| c.needs_redraw())
    }

    /// The stack's own paint changes only with a rebuild, which repaints
    /// everything. Between rebuilds its children are the whole change.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        for child in &self.children {
            child.collect_damage(regions);
        }
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

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        (self.access == StackAccess::Hidden).then(|| {
            ui::a11y::AccessNode::new(ui::a11y::Role::Group).with_state(ui::a11y::AccessState {
                hidden: true,
                ..Default::default()
            })
        })
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// Where the greeter is in the authentication lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthPhase {
    /// Showing the sign-in form, waiting for the user.
    Idle,
    /// A greetd conversation is running on the worker thread. Inputs are
    /// disabled and submit is ignored (no double-submit). On failure we
    /// return to [`AuthPhase::Idle`] with an error.
    Authenticating,
    /// greetd accepted the credentials and is starting the session. The
    /// process exits moments later, but the user gets an explicit success
    /// frame rather than the spinner simply vanishing.
    Starting,
}

impl AuthPhase {
    /// Whether an attempt currently owns the form: inputs disabled and
    /// submit ignored. The sign-in action stays enabled — it is inert
    /// because `GreeterApp::attempt_login` refuses a second attempt, not
    /// because it is greyed out — so it remains the form's keyboard home
    /// instead of the focus ring falling through to the footer.
    pub fn is_busy(self) -> bool {
        matches!(self, AuthPhase::Authenticating | AuthPhase::Starting)
    }
}

/// What the single status line is saying, as a catalogue key. Severity is
/// carried in the state rather than inferred from the wording, so an error
/// reads as an error at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// Nothing to say. The row is still reserved.
    None,
    /// A correctable note about the form itself ("Enter your passcode").
    Hint(&'static str),
    /// The attempt was refused or could not complete.
    Error(&'static str),
}

impl Notice {
    fn key(self) -> Option<&'static str> {
        match self {
            Notice::None => None,
            Notice::Hint(key) | Notice::Error(key) => Some(key),
        }
    }
}

/// A machine power action the greeter can request before login. Routed to
/// logind via `systemctl` — see `GreeterApp::run_power`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Suspend,
    Reboot,
    PowerOff,
}

impl PowerAction {
    const ALL: [Self; 3] = [Self::Suspend, Self::Reboot, Self::PowerOff];

    fn label(self) -> String {
        t(match self {
            Self::Suspend => "greeter-suspend",
            Self::Reboot => "greeter-restart",
            Self::PowerOff => "greeter-shut-down",
        })
    }
}

/// Pre-canned visual states for the headless `--shots` render path so each
/// distinct screen can be snapshot-verified. Live runs never use these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScenario {
    /// The empty sign-in form on a machine whose accounts are unknown.
    Form,
    /// That form after a refused attempt.
    Error,
    /// Mid-authentication: spinner and "Signing in…", inputs disabled.
    Authenticating,
    /// Mid-authentication with a fingerprint prompt surfaced.
    Fingerprint,
    /// The form with the Caps-Lock note shown.
    CapsLock,
    /// The form after submitting with an empty passcode.
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
    /// Personal-device form after the third incorrect passcode in a row.
    PersonalRepeatedError,
    /// Personal-device form while password authentication is in flight.
    PersonalAuthenticating,
    /// Personal-device form with its Caps-Lock note visible.
    PersonalCapsLock,
    /// Personal-device form after an empty passcode submission.
    PersonalHint,
    /// Personal-device form while the accepted session is starting.
    PersonalStarting,
    /// One known local account but no enrolled fingerprint. The unavailable
    /// capability must disappear rather than becoming a dead action.
    PersonalNoFingerprint,
    /// Personal-device form after too many failed attempts.
    PersonalLocked,
    /// Personal-device form after the desktop session failed to start.
    PersonalSessionFailed,
    /// Personal-device form when greetd cannot be reached.
    PersonalServiceDown,
    PersonalPasscodeExpired,
    /// Personal-device form on a machine with a second session installed,
    /// so the footer offers the choice.
    PersonalSessions,
    /// A machine with three accounts: the remembered one is chosen and the
    /// others are listed to switch to.
    Accounts,
    /// Personal-device form for a person whose secret is a PIN: the field
    /// asks for it by name, and the on-screen keyboard is a number pad.
    PersonalPin,
}

/// Two installed sessions, for a shot whose footer offers the choice.
fn shot_sessions_with_a_choice() -> Vec<Session> {
    vec![
        Session {
            name: "Tilo".to_string(),
            argv: vec!["/usr/libexec/tilo-session".to_string()],
        },
        Session {
            name: "GNOME".to_string(),
            argv: vec!["/usr/bin/gnome-session".to_string()],
        },
    ]
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

/// Whether this screen may offer `username`'s fingerprint: tilo-authd holds
/// their key for a session of theirs running now, and fprintd has a print
/// enrolled. At the first sign-in after boot, or once their sessions have
/// ended, it may not (docs/security/sign-in.md §4.3); the sign-in service
/// would not run the fingerprint step either.
fn fingerprint_offered(username: &str) -> bool {
    crate::handoff::key_held(
        std::path::Path::new(crate::handoff::DEFAULT_SOCKET),
        username,
    ) && enrolled_fingerprint(username)
}

/// The time and date the screen shows.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClockFace {
    time: String,
    date: String,
}

impl ClockFace {
    fn now() -> Self {
        Self::of(&ui::clock::local_now())
    }

    fn of(now: &chrono::DateTime<chrono::Local>) -> Self {
        Self {
            time: ui::clock::time_of_day(now),
            date: ui::format::day_heading(now.date_naive(), false),
        }
    }
}

/// The instant the minute shown at `wall` (read at the monotonic `now`)
/// ends. Fixed once read: a deadline that moves with the clock is never
/// due, and a host that serves due deadlines never redraws for it.
fn minute_ends(wall: &chrono::DateTime<chrono::Local>, now: Instant) -> Instant {
    use chrono::Timelike;
    // A leap second reads as a nanosecond count past one second.
    let into = Duration::from_secs(u64::from(wall.second())).saturating_add(Duration::from_nanos(
        u64::from(wall.nanosecond().min(999_999_999)),
    ));
    now.checked_add(Duration::from_secs(60).saturating_sub(into))
        .unwrap_or(now)
}

/// Controls whose callbacks report back to the greeter on its next sync.
#[derive(Default)]
struct Requests {
    /// Sign in with the passcode: the sign-in action, or Enter in a field.
    sign_in: Cell<bool>,
    cancel_authentication: Cell<bool>,
    /// Sign in with the enrolled fingerprint instead.
    fingerprint: Cell<bool>,
    power: Cell<Option<PowerAction>>,
    account: Cell<Option<usize>>,
    accessibility: Cell<Option<AccessibilityRequest>>,
    /// "Can't sign in": the other ways of opening the disk at boot, the
    /// recovery form after it.
    cant_sign_in: Cell<bool>,
    /// Set the new secret with the recovery key.
    recover: Cell<bool>,
    /// Leave the recovery form.
    cancel_recovery: Cell<bool>,
}

const SESSION_DIAGNOSTICS: [(&str, &str); 4] = [
    ("presentation", "TILO_PROFILE_PRESENTATION"),
    ("gpu_stages", "TILO_PROFILE_GPU_STAGES"),
    ("gpu_timestamps", "VELLO_GPU_TIMESTAMPS"),
    ("gpu_dispatch_timestamps", "VELLO_GPU_DISPATCH_TIMESTAMPS"),
];

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent lifecycle flags: rebuild, focus reset, keyboard focus, screen reader, pinned clock"
)]
pub struct GreeterApp {
    sessions: Vec<Session>,
    selected_session: usize,
    /// The session picker's chosen name. [`Widget::sync`] mirrors it onto
    /// `selected_session`.
    session_choice: ui::LocalState<String>,
    /// The local accounts this machine offers, in account-database order.
    /// With more than one, the screen lists them to choose from.
    users: Vec<LoginUser>,
    /// The account signing in, when known: the sole account, the chosen or
    /// remembered one of several. `None` asks for a typed username.
    login_user: Option<LoginUser>,
    /// The username field's value. Seeded with the known account, replaced
    /// when another account is chosen, and read when an attempt starts.
    username: ui::LocalState<String>,
    /// Whether fingerprint login is unavailable, already confirmed for the
    /// fixed local account, or must be resolved from the typed username.
    fingerprint: FingerprintAvailability,
    /// Account-specific enrollment resolver. Production calls `fprintd-list`;
    /// a function pointer keeps multi-account routing deterministic in tests.
    fingerprint_probe: fn(&str) -> bool,
    /// Whose secret is a PIN and whose a password, as `tilo-authd` keeps it
    /// ([`crate::handoff::secret_kind`]); a person not here is asked for a
    /// password. At boot the supervisor's list says instead.
    secret_kinds: std::collections::HashMap<String, boot::SecretKind>,
    /// Process-local only. Passcode text never enters Automerge, persistence,
    /// presence, diagnostics, or peer sync and is zeroized after every attempt.
    password_buffer: Arc<Mutex<String>>,
    auth_session: Option<AuthSession>,
    challenge_sequence: Arc<AtomicU64>,
    notice: Notice,
    /// Refused passcodes in a row, so a repeated failure says so.
    failures: u32,
    /// Current PAM conversation prompt during [`AuthPhase::Authenticating`]
    /// (e.g. "Place your finger on the reader"). Empty falls back to the
    /// generic "Signing in…" line.
    prompt: String,
    phase: AuthPhase,
    /// Set when the composition must be rebuilt (session, notice, auth phase
    /// or the minute changed). Drained by `take_structure_dirty`, which the
    /// App polls to trigger a fresh `build_layout`.
    dirty_structure: bool,
    /// Heuristic Caps-Lock state inferred from typed letters (winit can't
    /// report the lock directly). See [`ui::caps_lock`].
    caps_lock: CapsLockEstimate,
    /// One-shot request that the App re-seat keyboard focus on the first
    /// focusable widget after the next rebuild (drained by
    /// [`Widget::take_focus_reset`]). Raised on auth-phase and account
    /// changes; plain notice/caps rebuilds leave it `false` so focus + caret
    /// are preserved in place.
    focus_reset_pending: bool,
    /// The most recent power action requested, recorded for test
    /// observability (the actual `systemctl` invocation is a side effect).
    last_power_action: Option<PowerAction>,
    requests: Rc<Requests>,
    /// Whether this screen turned the screen reader on.
    screen_reader: bool,
    /// Where a comfort turned on or off here is kept for the login screens;
    /// `None` in shots and tests.
    sign_in_comforts: Option<crate::comforts::Changes>,
    /// The time and date the current composition shows. Shots pin it.
    clock: ClockFace,
    /// When the minute `clock` shows ends: the wake the clock asks for.
    clock_turns: Instant,
    clock_pinned: bool,
    /// Battery and Wi-Fi as the current composition shows them.
    device: DeviceStatus,
    direction: Direction,
    rect: Rect,
    /// Where the sign-in composition is laid out: the primary output's part
    /// of the window when the kiosk spans several outputs, else `rect`.
    content: Rect,
    /// The kiosk's outputs, when the greeter runs on one (`main.rs`).
    displays: Option<crate::output_scale::Displays>,
    /// The output geometry the current layout was built for, relative to
    /// the window: the primary output, and every output for the backdrop.
    /// Empty when the window is one output (or its geometry is unknown).
    layout_area: Option<Rect>,
    layout_outputs: Vec<Rect>,
    /// The composed subtree, rebuilt each `build_layout`.
    composed: ui::widget::Composed,
    /// Slot holding the winit event-loop proxy, populated by `main.rs`'s
    /// `run_with_setup` once the loop starts. The submit handler reads it
    /// to spawn the background auth worker. Empty in tests / headless
    /// renders, where auth falls back to a synchronous exchange.
    proxy: Arc<OnceLock<EventLoopProxy<UserEvent>>>,
    /// The scroll position survives notice/caps/auth rebuilds, which
    /// recreate the composed tree.
    viewport_offset: Rc<Cell<(f32, f32)>>,
    /// The text scale the current composition was built for.
    layout_text_scale: f32,
    /// The height the on-screen keyboard takes from the screen, when shown.
    layout_keyboard_height: f64,
    /// Queued console requests (see [`console`]); `None` in shots and tests
    /// that do not exercise the console.
    console: Option<console_control::ConsoleServer>,
    /// Transient fixed diagnostic options for this ordinary sign-in.
    next_sign_in_diagnostics: Option<[bool; 4]>,
    /// Whether the compositor has given this window the keyboard. Until it
    /// has, keys typed at the greeter go nowhere, so the console reports
    /// sign-in as accepting input only once this is set.
    keyboard_focus: bool,
    /// The person who signed in on the boot screen, whose session starts
    /// without this screen asking them anything.
    boot_sign_in: BootSignIn,
    /// The boot sign-in this screen is, when it runs in the initramfs as
    /// `tilo-boot-signin`'s screen ([`crate::auth::boot`]).
    boot: Option<Boot>,
    /// The on-screen keyboard's state and the keys pressed on it.
    touch_keyboard: touch_keyboard::Shared,
    /// The input devices and the one in use, as the host reports them.
    environment: ui::InteractionEnvironment,
    /// Whether a keyboard is attached, where the kiosk's own report cannot
    /// be trusted ([`crate::keyboards`]); `None` takes the host's.
    keyboards: Option<bool>,
    /// The recovery form, in the passcode form's place while it is open.
    recovery: Option<recovery::Recovery>,
    /// `tilo-authd`'s socket, which sets a new secret.
    auth_socket: std::path::PathBuf,
}

/// The greeter as the boot's sign-in screen: it shows the people and notes
/// the supervisor sends and answers with the person and secret.
struct Boot {
    link: BootLink,
    /// The people as the supervisor listed them; the screen answers with an
    /// index into this list.
    people: Vec<boot::Person>,
    note: Note,
}

/// The notice that stands for the supervisor's note ([`Boot::note`]), which
/// carries its own numbers.
const BOOT_NOTE: &str = "greeter-boot-note";

/// Where the boot sign-in handoff is (see [`crate::handoff`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum BootSignIn {
    /// Nobody signed in at boot, or the handoff is over.
    None,
    /// Someone did; their session starts once the event loop runs.
    Pending,
    /// greetd is starting their session.
    Running,
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
        self.password_buffer.lock_recover().zeroize();
    }
}

impl GreeterApp {
    /// Construct a greeter reading sessions from the standard
    /// `/usr/share/wayland-sessions` directory and accounts from the
    /// account database.
    pub fn new() -> Self {
        let users = discover_login_users();
        let chosen = match users.len() {
            0 => None,
            1 => Some(0),
            _ => Some(remembered_user(&users).unwrap_or(0)),
        };
        let fingerprint = match (users.len(), chosen) {
            (1, Some(_)) => users
                .first()
                .map_or(FingerprintAvailability::Unavailable, |user| {
                    fingerprint_offered(&user.username).into()
                }),
            _ => FingerprintAvailability::CheckNamedAccount,
        };
        let mut app = Self::with_sessions_login_user_and_fingerprint(
            read_sessions(std::path::Path::new(DEFAULT_SESSIONS_DIR)),
            chosen.and_then(|index| users.get(index).cloned()),
            fingerprint,
        );
        let socket = std::path::Path::new(crate::handoff::DEFAULT_SOCKET);
        app.secret_kinds = users
            .iter()
            .filter_map(|user| {
                crate::handoff::secret_kind(socket, &user.username)
                    .map(|kind| (user.username.clone(), kind))
            })
            .collect();
        app.users = users;
        app.device = DeviceStatus::read();
        if let Some(name) = crate::handoff::pending_sign_in(
            std::path::Path::new(crate::handoff::DEFAULT_SOCKET),
            std::path::Path::new(crate::handoff::FIRST_BOOT_AUTOLOGIN),
        ) {
            app.offer_boot_sign_in(&name);
        }
        app
    }

    /// The person `name` signed in on the boot screen: show the ordinary
    /// "Starting your session" frame for them, and start their session as
    /// soon as the event loop runs ([`Self::start_boot_sign_in`]). If it
    /// does not start, the screen is the one they would otherwise have seen,
    /// with them already chosen.
    fn offer_boot_sign_in(&mut self, name: &str) {
        // An account the screen does not list (discovery is conservative)
        // falls back to the typed-username form.
        self.login_user = self
            .users
            .iter()
            .find(|user| user.username == name)
            .cloned();
        self.username.set(name.to_owned());
        self.boot_sign_in = BootSignIn::Pending;
        self.phase = AuthPhase::Starting;
        self.notice = Notice::None;
        self.dirty_structure = true;
        self.focus_reset_pending = true;
    }

    /// Ask greetd to start the boot-signed-in person's session.
    fn start_boot_sign_in(&mut self) {
        if self.boot_sign_in != BootSignIn::Pending {
            return;
        }
        let Some(proxy) = self.proxy.get().cloned() else {
            return;
        };
        let sock = std::env::var("GREETD_SOCK").ok().filter(|s| !s.is_empty());
        let (Some(sock), Some(session)) = (sock, self.sessions.get(self.selected_session)) else {
            self.end_boot_sign_in();
            return;
        };
        self.boot_sign_in = BootSignIn::Running;
        self.auth_session = Some(spawn_handoff(
            proxy,
            sock,
            self.attempt_username(),
            session.argv.clone(),
            Self::session_env(None),
        ));
    }

    /// The boot sign-in did not start a session: show the ordinary screen,
    /// without an error. The person signs in there as they would have.
    fn end_boot_sign_in(&mut self) {
        self.boot_sign_in = BootSignIn::None;
        self.phase = AuthPhase::Idle;
        self.auth_session.take();
        self.prompt.clear();
        self.notice = Notice::None;
        self.dirty_structure = true;
        self.focus_reset_pending = true;
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
        crate::i18n::ensure();
        let session_choice = ui::LocalState::new(
            sessions
                .first()
                .map(|session| session.name.clone())
                .unwrap_or_default(),
        );
        Self {
            sessions,
            selected_session: 0,
            session_choice,
            users: login_user.iter().cloned().collect(),
            username: ui::LocalState::new(
                login_user
                    .as_ref()
                    .map(|user| user.username.clone())
                    .unwrap_or_default(),
            ),
            login_user,
            fingerprint,
            fingerprint_probe: fingerprint_offered,
            secret_kinds: std::collections::HashMap::new(),
            password_buffer: Arc::new(Mutex::new(String::new())),
            auth_session: None,
            challenge_sequence: Arc::new(AtomicU64::new(1)),
            notice: Notice::None,
            failures: 0,
            prompt: String::new(),
            phase: AuthPhase::Idle,
            dirty_structure: false,
            caps_lock: CapsLockEstimate::new(),
            focus_reset_pending: false,
            last_power_action: None,
            requests: Rc::new(Requests::default()),
            screen_reader: false,
            sign_in_comforts: None,
            clock: ClockFace::now(),
            clock_turns: minute_ends(&ui::clock::local_now(), ui::time::now()),
            clock_pinned: false,
            device: DeviceStatus::default(),
            direction: Direction::Column,
            rect: Rect::ZERO,
            content: Rect::ZERO,
            displays: None,
            layout_area: None,
            layout_outputs: Vec::new(),
            composed: ui::widget::Composed::default(),
            proxy: Arc::new(OnceLock::new()),
            viewport_offset: Rc::new(Cell::new((0.0, 0.0))),
            layout_text_scale: 1.0,
            layout_keyboard_height: 0.0,
            console: None,
            next_sign_in_diagnostics: None,
            keyboard_focus: false,
            boot_sign_in: BootSignIn::None,
            boot: None,
            touch_keyboard: touch_keyboard::Shared::default(),
            environment: ui::InteractionEnvironment::default(),
            keyboards: None,
            recovery: None,
            auth_socket: crate::handoff::DEFAULT_SOCKET.into(),
        }
    }

    /// Wire the shared event-loop proxy slot. `main.rs` constructs the slot,
    /// hands a clone here, and populates it inside `App::run_with_setup`.
    #[must_use]
    pub fn with_proxy_slot(mut self, slot: Arc<OnceLock<EventLoopProxy<UserEvent>>>) -> Self {
        self.proxy = slot;
        self
    }

    /// Follow the kiosk's outputs: the sign-in card goes on the primary
    /// output and the backdrop on each, instead of centring the card in a
    /// window that spans the seam (or a gap) between displays.
    #[must_use]
    pub fn with_displays(mut self, displays: crate::output_scale::Displays) -> Self {
        self.displays = Some(displays);
        self
    }

    /// The primary output and every output, relative to the window, when
    /// the window spans more than one and is already the layout's size.
    /// Before the kiosk resizes the window to a changed layout, the
    /// composition stays centred in the window as on a single display.
    fn output_layout(&self) -> (Option<Rect>, Vec<Rect>) {
        let Some(arrangement) = self
            .displays
            .as_ref()
            .map(crate::output_scale::Displays::arrangement)
        else {
            return (None, Vec::new());
        };
        let Some(extent) = arrangement.extent() else {
            return (None, Vec::new());
        };
        if arrangement.displays.len() < 2
            || (extent.width() - self.rect.width()).abs() > 1.0
            || (extent.height() - self.rect.height()).abs() > 1.0
        {
            return (None, Vec::new());
        }
        (arrangement.primary_in_window(), arrangement.in_window())
    }

    /// Build a greeter pre-seeded for a headless `--shots` render of
    /// `scenario`. Live runs never use this; it just stages enough state
    /// that each distinct screen renders something representative. The
    /// caller seeds representative credentials separately (see `main.rs`).
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
        let personal = !matches!(
            scenario,
            AuthScenario::Form
                | AuthScenario::Error
                | AuthScenario::Authenticating
                | AuthScenario::CapsLock
                | AuthScenario::Hint
                | AuthScenario::Starting
                | AuthScenario::LongName
        );
        let person = |username: &str, display_name: &str| LoginUser {
            username: username.to_string(),
            display_name: display_name.to_string(),
        };
        let personal = personal.then(|| person("kai", "Kai Example"));
        // A fixed personal account has a known enrollment result. The
        // multi-account fixture exposes the action only as a route: its
        // enrollment is resolved for the username entered before PAM starts.
        let fingerprint = if personal.is_none() || scenario == AuthScenario::Accounts {
            FingerprintAvailability::CheckNamedAccount
        } else if matches!(scenario, AuthScenario::PersonalNoFingerprint) {
            FingerprintAvailability::Unavailable
        } else {
            FingerprintAvailability::Enrolled
        };
        let sessions = if scenario == AuthScenario::PersonalSessions {
            shot_sessions_with_a_choice()
        } else {
            sessions
        };
        let mut app =
            Self::with_sessions_login_user_and_fingerprint(sessions, personal, fingerprint);
        app.clock = ClockFace {
            time: "09:41".to_string(),
            date: "Saturday, October 4".to_string(),
        };
        app.clock_pinned = true;
        app.device = DeviceStatus {
            battery: Some((76, false)),
            wifi: Some(true),
        };
        match scenario {
            AuthScenario::Form
            | AuthScenario::LongName
            | AuthScenario::Personal
            | AuthScenario::PersonalNoFingerprint
            | AuthScenario::PersonalSessions => {}
            AuthScenario::PersonalPin => {
                app.secret_kinds
                    .insert("kai".to_owned(), boot::SecretKind::Pin);
            }
            AuthScenario::Accounts => {
                app.users = vec![
                    person("ada", "Ada Lovelace"),
                    person("kai", "Kai Example"),
                    person("grace", "Grace Hopper"),
                ];
            }
            AuthScenario::Error => app.notice = Notice::Error("greeter-error-credentials"),
            AuthScenario::PersonalError => {
                app.notice = Notice::Error("greeter-error-passcode");
                app.failures = 1;
            }
            AuthScenario::PersonalRepeatedError => {
                app.notice = Notice::Error("greeter-error-passcode");
                app.failures = 3;
            }
            AuthScenario::PersonalLocked => app.notice = Notice::Error("greeter-error-locked"),
            AuthScenario::PersonalSessionFailed => {
                app.notice = Notice::Error("greeter-error-session");
            }
            AuthScenario::PersonalServiceDown => {
                app.notice = Notice::Error("greeter-error-service");
            }
            AuthScenario::PersonalPasscodeExpired => {
                app.notice = Notice::Error("greeter-error-passcode-expired");
            }
            AuthScenario::Authenticating | AuthScenario::PersonalAuthenticating => {
                app.phase = AuthPhase::Authenticating;
            }
            AuthScenario::Fingerprint => {
                app.phase = AuthPhase::Authenticating;
                app.prompt = "Place your finger on the reader".to_string();
            }
            AuthScenario::CapsLock | AuthScenario::PersonalCapsLock => {
                // Force the heuristic "on" so the note renders.
                app.caps_lock.observe('A', false);
            }
            AuthScenario::Hint | AuthScenario::PersonalHint => {
                app.notice = Notice::Hint("greeter-hint-passcode");
            }
            AuthScenario::Starting | AuthScenario::PersonalStarting => {
                app.phase = AuthPhase::Starting;
            }
        }
        app
    }

    /// Arrange a shot in `direction`.
    pub fn set_direction(&mut self, direction: Direction) {
        self.direction = direction;
    }

    /// Seed representative username text for a headless snapshot.
    pub fn seed_shot_username(&mut self, username: &str) {
        self.username.set(username.to_owned());
    }

    /// Seed representative local-only passcode text for a headless snapshot.
    /// The live path never calls this, and even shots keep the value out of the
    /// Automerge document used to drive the rest of the frame.
    pub fn seed_shot_password(&mut self, password: &str) {
        let mut value = self.password_buffer.lock_recover();
        value.zeroize();
        value.push_str(password);
    }

    /// Tight space or large text: a smaller avatar and closer spacing keep
    /// the whole form above the footer.
    fn compact(&self, text_scale: f32) -> bool {
        let height = self.content.height() - self.layout_keyboard_height;
        (self.content.height() > 0.0 && height < COMPACT_VIEWPORT_HEIGHT)
            || text_scale >= COMPACT_TEXT_SCALE
    }

    /// Whether the footer offers a session choice: only when there is more
    /// than one session to choose between. With a single session the
    /// greeter starts it directly, so a one-option dropdown never appears.
    pub(crate) fn offers_session_choice(&self) -> bool {
        self.sessions.len() > 1
    }

    /// Whether the screen lists accounts to choose from.
    fn offers_account_choice(&self) -> bool {
        self.users.len() > 1
    }

    /// Label for the session picker — the session's own human-readable
    /// name, never the internal desktop id it is launched under.
    #[cfg(test)]
    fn session_label(&self) -> String {
        self.sessions
            .get(self.selected_session)
            .map_or_else(String::new, |s| s.name.clone())
    }

    /// What the status line says, as plain text.
    ///
    /// While a login is in flight this is the live PAM prompt when there is
    /// one (a fingerprint instruction is new information), otherwise the
    /// in-flight wording.
    pub(crate) fn status_text(&self) -> String {
        match self.phase {
            AuthPhase::Authenticating
                if self
                    .auth_session
                    .as_ref()
                    .is_some_and(AuthSession::is_cancelling) =>
            {
                t("greeter-auth-cancelling")
            }
            AuthPhase::Authenticating if !self.prompt.trim().is_empty() => {
                self.prompt.trim().to_string()
            }
            AuthPhase::Authenticating => t("greeter-signing-in"),
            AuthPhase::Starting => t("greeter-starting"),
            AuthPhase::Idle => match self.notice {
                Notice::Error(BOOT_NOTE) => self
                    .boot
                    .as_ref()
                    .map(|boot| boot_mode::boot_note_text(boot.note))
                    .unwrap_or_default(),
                Notice::Error("greeter-error-passcode") if self.failures > 1 => t_args(
                    "greeter-error-passcode-again",
                    &[("count", self.failures.into())],
                ),
                notice => notice.key().map(t).unwrap_or_default(),
            },
        }
    }

    /// The display size of the time on this screen.
    fn clock_size(&self, compact: bool) -> f32 {
        let (width, height) = (self.content.width(), self.content.height());
        let fraction = match (self.direction, compact) {
            (_, true) => 0.16,
            (Direction::Poster, false) => 0.30,
            (Direction::Split, false) => 0.22,
            (Direction::Column, false) => 0.21,
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a viewport fraction of a few hundred logical px fits f32"
        )]
        let fitted = (height * fraction).min(width * 0.2) as f32;
        let max = if self.direction == Direction::Poster && !compact {
            CLOCK_MAX * 1.5
        } else {
            CLOCK_MAX
        };
        if fitted.is_finite() && fitted > 0.0 {
            fitted.clamp(CLOCK_MIN, max)
        } else {
            CLOCK_MAX * 0.6
        }
    }

    /// The time as display type and the date beneath it.
    fn build_clock(&self, theme: &Theme, compact: bool, align: AlignItems) -> Box<dyn Widget> {
        let size = self.clock_size(compact);
        let time = Text::new(&self.clock.time)
            .with_id("greeter.clock")
            .with_content_size(size)
            .with_font_variation(OpenTypeTag::WEIGHT, CLOCK_WEIGHT)
            .with_numeric_style(NumericStyle::TabularLining)
            .with_line_height(1.0)
            .with_color(theme.text_primary);
        let date = Text::new(&self.clock.date)
            .with_id("greeter.date")
            .with_role(if compact {
                TextRole::Body
            } else {
                TextRole::Subheading
            })
            .with_weight(TextWeight::Medium)
            .with_color(theme.tokens.colors.text_dim);
        Box::new(Stack::column(0.0, vec![Box::new(time), Box::new(date)]).align(align))
    }

    /// The time and date centred on a display without the sign-in card.
    fn build_display_clock(&self, theme: &Theme, output: Rect) -> Box<dyn Widget> {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a display fraction of a few hundred logical px fits f32"
        )]
        let fitted = (output.height() * 0.21).min(output.width() * 0.2) as f32;
        let size = if fitted.is_finite() {
            fitted.clamp(CLOCK_MIN, CLOCK_MAX)
        } else {
            CLOCK_MIN
        };
        let time = Text::new(&self.clock.time)
            .with_id("greeter.display-clock")
            .with_content_size(size)
            .with_font_variation(OpenTypeTag::WEIGHT, CLOCK_WEIGHT)
            .with_numeric_style(NumericStyle::TabularLining)
            .with_line_height(1.0)
            .with_color(theme.text_primary);
        let date = Text::new(&self.clock.date)
            .with_id("greeter.display-date")
            .with_role(TextRole::Subheading)
            .with_weight(TextWeight::Medium)
            .with_color(theme.tokens.colors.text_dim);
        Box::new(
            Stack::column(
                0.0,
                vec![Box::new(
                    Stack::column(0.0, vec![Box::new(time), Box::new(date)])
                        .align(AlignItems::Center)
                        .width_auto(),
                )],
            )
            .align(AlignItems::Center)
            .justify(JustifyContent::Center)
            .place(output)
            .decorative(),
        )
    }

    /// The passcode, asked with the one password prompt every part of Tilo
    /// uses: its label, its show/hide button, Caps Lock, and one line under
    /// it saying what the attempt is doing or why it was refused. The tree
    /// is built afresh as the form changes, so what is typed lives in the
    /// greeter's buffer and the prompt starts from it.
    fn build_passcode(&self, challenge: bool) -> SecretPrompt {
        let line = Some(self.status_text()).filter(|line| !line.trim().is_empty());
        let mut prompt = SecretPrompt::new(SecretKind::Secret)
            .with_presentation(Presentation::DialogBody)
            .with_id("greeter.password")
            .with_label(
                FieldRole::Secret,
                t(if challenge {
                    "greeter-auth-response"
                } else if self.secret_is_pin() {
                    "greeter-pin"
                } else {
                    "greeter-passcode"
                }),
            )
            .with_secret_buffer(Arc::clone(&self.password_buffer))
            .with_caps_lock(self.caps_lock);
        prompt = match (self.phase, self.notice) {
            // A question from PAM is answered in the field: it stays open.
            (AuthPhase::Authenticating, _) if challenge => prompt.with_notice(line),
            // What PAM says while it checks (touch the reader) can need
            // acting on now: it is said over "Signing in…".
            (AuthPhase::Authenticating, _) if !self.prompt.trim().is_empty() => prompt
                .with_busy(Some(t("greeter-signing-in")))
                .with_notice(line),
            (AuthPhase::Authenticating | AuthPhase::Starting, _) => prompt.with_busy(line),
            (AuthPhase::Idle, Notice::Error(_)) => prompt.with_error(line),
            (AuthPhase::Idle, Notice::Hint(_) | Notice::None) => prompt.with_notice(line),
        };
        prompt
    }

    /// Who is signing in: their avatar and name, or, when the accounts are
    /// unknown, what the screen is for.
    fn build_identity(&self, theme: &Theme, compact: bool, align: AlignItems) -> Box<dyn Widget> {
        let c = &theme.tokens.colors;
        let mut identity: Vec<Box<dyn Widget>> = Vec::new();
        if let Some(user) = &self.login_user {
            let name = if user.display_name.is_empty() {
                &user.username
            } else {
                &user.display_name
            };
            let avatar = Avatar::person(name, &user.username).size(if compact {
                AvatarSize::Large
            } else {
                AvatarSize::Hero
            });
            let mut names: Vec<Box<dyn Widget>> = vec![Box::new(
                Text::new(&user.display_name)
                    .with_id("greeter.welcome")
                    .with_heading_level(1)
                    .with_role(if compact {
                        TextRole::Title
                    } else {
                        TextRole::LargeTitle
                    })
                    .with_weight(TextWeight::Semibold),
            )];
            if user.display_name != user.username {
                names.push(Box::new(
                    Text::new(&user.username)
                        .with_id("greeter.account")
                        .with_role(TextRole::Label)
                        .with_color(c.text_dim),
                ));
            }
            let names = Stack::column(2.0, names).align(align);
            if align == AlignItems::Center {
                identity.push(Box::new(avatar));
                identity.push(Box::new(names));
            } else {
                // Beside the time, the avatar leads its name on one line.
                identity.push(Box::new(Stack::row(
                    theme.tokens.spacing.s4,
                    vec![Box::new(avatar), Box::new(names)],
                )));
            }
        } else {
            identity.push(Box::new(
                Text::new(&t("greeter-title"))
                    .with_id("greeter.welcome")
                    .with_heading_level(1)
                    .with_role(if compact {
                        TextRole::Title
                    } else {
                        TextRole::LargeTitle
                    })
                    .with_weight(TextWeight::Semibold),
            ));
        }
        Box::new(Stack::column(theme.tokens.spacing.s3, identity).align(align))
    }

    /// The credentials, the sign-in action beside the passcode, the status
    /// line, the alternate factor, and the other accounts to switch to.
    fn build_form(&self, busy: bool, theme: &Theme, align: AlignItems) -> Box<dyn Widget> {
        if let Some(recovery) = &self.recovery {
            return self.build_recovery(recovery, theme, align);
        }
        let s = &theme.tokens.spacing;
        let challenge = self.auth_session.as_ref().and_then(AuthSession::challenge);
        let mut form: Vec<Box<dyn Widget>> = Vec::new();
        if self.login_user.is_none() {
            // Labelled above, as the password is, and as wide as the
            // password with its sign-in action.
            form.push(Box::new(Stack::column(
                s.s1,
                vec![
                    Box::new(Text::new(&t("greeter-username")).with_role(TextRole::Label)),
                    Box::new(
                        TextInput::from_state(&self.username)
                            .with_id("greeter.username")
                            .with_accessible_name(t("greeter-username"))
                            .control_size(ControlSize::Regular)
                            .disabled(busy),
                    ),
                ],
            )));
        }
        let sign_in = t(if challenge.is_some() {
            "greeter-auth-respond"
        } else {
            "greeter-sign-in"
        });
        let requests = Rc::clone(&self.requests);
        // The sign-in action sits beside the field, as tall as it and past
        // the prompt's label row; the username field is as tall too.
        let field = SecretPrompt::field_height(theme);
        let action = ControlSize::Regular.height(theme);
        let label_row = (f64::from(theme.text(TextRole::Label).line_height_px() + s.s1)
            + (field - action) / 2.0)
            .max(0.0);
        let label_row = ui::num::f32_of_f64(label_row);
        form.push(Box::new(
            Row::new()
                .with_gap(s.s2)
                .no_wrap()
                .with_align_items(AlignItems::Start)
                .push(Box::new(self.build_passcode(challenge.is_some())))
                .push(Box::new(
                    Stack::column(
                        0.0,
                        vec![
                            Box::new(Spacer::fixed(label_row)),
                            Box::new(
                                Button::new("greeter.login", "")
                                    .role(ui::widget::ButtonRole::Primary)
                                    .with_icon(Icon::ArrowRight)
                                    .with_accessible_name(&sign_in)
                                    .tooltip(sign_in.clone())
                                    .control_size(ControlSize::Regular)
                                    .on_click(move || requests.sign_in.set(true)),
                            ),
                        ],
                    )
                    .width_auto(),
                )),
        ));
        // Directly under the field, at boot and after it alike, so the card
        // is the same on both.
        form.extend(self.build_cant_sign_in(busy, align));
        if self.phase == AuthPhase::Authenticating && self.auth_session.is_some() {
            let requests = Rc::clone(&self.requests);
            form.push(Box::new(
                Button::new("greeter.cancel-authentication", &t("greeter-auth-cancel"))
                    .role(ButtonRole::Plain)
                    .with_enabled(
                        self.auth_session
                            .as_ref()
                            .is_some_and(AuthSession::can_cancel),
                    )
                    .on_click(move || requests.cancel_authentication.set(true)),
            ));
        }
        // While an attempt runs, the alternate factor gives way to the live
        // prompt; its space stays reserved so the form does not change size.
        if self.fingerprint != FingerprintAvailability::Unavailable && busy {
            form.push(Box::new(Spacer::fixed(FIELD_HEIGHT)));
        } else if self.fingerprint != FingerprintAvailability::Unavailable {
            let requests = Rc::clone(&self.requests);
            form.push(Box::new(
                Stack::column(
                    0.0,
                    vec![Box::new(
                        Button::new("greeter.fingerprint", &t("greeter-use-fingerprint"))
                            .role(ui::widget::ButtonRole::Plain)
                            .with_icon(Icon::Fingerprint)
                            .control_size(ControlSize::Large)
                            .on_click(move || requests.fingerprint.set(true)),
                    )],
                )
                .align(align),
            ));
        }
        if self.offers_account_choice() {
            form.push(self.build_accounts(busy, theme, align));
        }
        Box::new(
            Stack::column(s.s3, form)
                .max_width(FORM_WIDTH)
                .align(AlignItems::Stretch),
        )
    }

    /// The other accounts on a machine with several, to switch to.
    fn build_accounts(&self, busy: bool, theme: &Theme, align: AlignItems) -> Box<dyn Widget> {
        let mut chips: Vec<Box<dyn Widget>> = Vec::new();
        for (index, user) in self.users.iter().enumerate() {
            if self
                .login_user
                .as_ref()
                .is_some_and(|chosen| chosen.username == user.username)
            {
                continue;
            }
            let requests = Rc::clone(&self.requests);
            chips.push(Box::new(
                Button::new(
                    &format!("greeter.account.{}", user.username),
                    &user.display_name,
                )
                .role(ButtonRole::Plain)
                .with_icon(Icon::User)
                .control_size(ControlSize::Large)
                .with_enabled(!busy)
                .on_click(move || requests.account.set(Some(index))),
            ));
        }
        let mut row = Row::new().with_gap(theme.tokens.spacing.s2);
        for chip in chips {
            row = row.push(chip);
        }
        Box::new(Stack::column(0.0, vec![Box::new(row.with_id("greeter.accounts"))]).align(align))
    }

    /// The bottom edge: accessibility on the leading side; the session
    /// choice, the machine's state and power on the trailing side.
    #[allow(
        clippy::too_many_lines,
        reason = "one footer tree; splitting it would scatter its two halves' shared spacing"
    )]
    fn build_footer(&self, busy: bool, theme: &Theme) -> Box<dyn Widget> {
        let s = &theme.tokens.spacing;
        let c = &theme.tokens.colors;
        let high_contrast = theme.name == "High Contrast";
        let larger_text = theme.text_scale > 1.01;
        let toggle = |id: &str, icon: Icon, key: &str, on: bool, request: AccessibilityRequest| {
            let label = t(key);
            let requests = Rc::clone(&self.requests);
            let button = Button::new(id, "")
                .with_icon(icon)
                .with_accessible_name(&label)
                .tooltip(label)
                .control_size(ControlSize::Large)
                .on_click(move || requests.accessibility.set(Some(request)));
            Box::new(if on {
                button
            } else {
                button.role(ButtonRole::Plain)
            }) as Box<dyn Widget>
        };
        let leading = Stack::row(
            s.s1,
            vec![
                toggle(
                    "greeter.larger-text",
                    Icon::ALargeSmall,
                    "greeter-larger-text",
                    larger_text,
                    AccessibilityRequest::TextScale(if larger_text {
                        1.0
                    } else {
                        LARGER_TEXT_SCALE
                    }),
                ),
                toggle(
                    "greeter.high-contrast",
                    Icon::Contrast,
                    "greeter-high-contrast",
                    high_contrast,
                    AccessibilityRequest::HighContrast(!high_contrast),
                ),
                toggle(
                    "greeter.screen-reader",
                    Icon::Volume2,
                    "greeter-screen-reader",
                    self.screen_reader,
                    AccessibilityRequest::ScreenReader(!self.screen_reader),
                ),
            ],
        );

        let mut trailing: Vec<Box<dyn Widget>> = Vec::new();
        if self.offers_session_choice() {
            let options = self
                .sessions
                .iter()
                .map(|session| session.name.clone())
                .collect();
            trailing.push(Box::new(
                Stack::column(
                    0.0,
                    vec![Box::new(
                        Dropdown::from_state(options, &self.session_choice)
                            .with_id("greeter.session")
                            .with_accessible_name(t("greeter-session"))
                            .with_enabled(!busy),
                    )],
                )
                .width(if self.content.width() < 640.0 {
                    SESSION_PICKER_WIDTH * 0.7
                } else {
                    SESSION_PICKER_WIDTH
                }),
            ));
        }
        if let Some((percent, charging)) = self.device.battery {
            let low = !charging && percent <= 15;
            trailing.push(Box::new(
                Stack::row(
                    s.s1,
                    vec![
                        Box::new(
                            IconWidget::new(if charging {
                                Icon::BatteryCharging
                            } else {
                                Icon::Battery
                            })
                            .with_color(if low {
                                c.semantic.danger
                            } else {
                                c.text_dim
                            }),
                        ),
                        Box::new(
                            Text::new(&format!("{percent}%"))
                                .with_id("greeter.battery")
                                .with_role(TextRole::Label)
                                .with_numeric_style(NumericStyle::TabularLining)
                                .with_color(c.text_dim),
                        ),
                    ],
                )
                .width_auto(),
            ));
        }
        if let Some(connected) = self.device.wifi {
            trailing.push(Box::new(
                IconWidget::new(if connected { Icon::Wifi } else { Icon::WifiOff })
                    .with_color(c.text_dim),
            ));
        }
        let requests = Rc::clone(&self.requests);
        // The initramfs cannot suspend: what is in memory there is the boot.
        let actions: &'static [PowerAction] = if self.boot.is_some() {
            &[PowerAction::Reboot, PowerAction::PowerOff]
        } else {
            &PowerAction::ALL
        };
        let labels: Vec<String> = actions.iter().map(|a| a.label()).collect();
        let chosen = labels.clone();
        trailing.push(Box::new(
            // A menu: the choice is a command, reported and never kept.
            Dropdown::from_state(labels, &ui::LocalState::new(String::new()))
                .as_menu(Icon::Power)
                .with_id("greeter.power")
                .with_accessible_name(t("greeter-power"))
                .on_change(move |label| {
                    let action = chosen
                        .iter()
                        .position(|candidate| candidate == label)
                        .and_then(|index| actions.get(index).copied());
                    requests.power.set(action);
                }),
        ));
        let trailing = Stack::row(s.s4, trailing).justify(JustifyContent::End);
        if self.content.width() > 0.0 && self.content.width() < 640.0 {
            // A phone-narrow screen stacks the two halves.
            return Box::new(Stack::column(
                s.s1,
                vec![
                    Box::new(leading.justify(JustifyContent::Start)),
                    Box::new(trailing),
                ],
            ));
        }
        Box::new(Stack::row(
            s.s4,
            vec![
                Box::new(leading.width(0.0).grow()),
                Box::new(trailing.width(0.0).grow()),
            ],
        ))
    }

    /// Build the composed subtree from current state.
    ///
    /// Focus continuity contract: the focusable set is, in tree order, the
    /// optional username field, passcode field (and its show/hide control),
    /// sign-in action, "Can't sign in" (at boot, and after it with a known
    /// account), fingerprint action, other accounts, then the footer's
    /// accessibility toggles, session picker (only with more than one
    /// session) and power menu. With a known account the passcode is the
    /// first focusable control. While the recovery form is open it stands
    /// in the passcode form's place: the recovery key, the new secret, its
    /// confirmation, Cancel and the action that sets it, the key first so
    /// it takes focus when the form opens. Notices, the Caps-Lock note and
    /// the spinner are non-focusable, so an incidental rebuild keeps keyboard
    /// focus AND the caret in place — see `main.rs`'s
    /// `autofocus_on_structure_change`.
    ///
    /// While an attempt is in flight the inputs are **disabled** (dimmed and
    /// dropped from the focus order); the phase transition raises
    /// `take_focus_reset` so the App re-seats focus rather than preserving a
    /// now-invalid ordinal. The sign-in action remains enabled while its
    /// handler rejects a second submit, so that reset lands on it.
    #[allow(
        clippy::too_many_lines,
        reason = "each arrangement is one declarative tree, kept side by side to compare"
    )]
    fn build_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let busy = self.phase.is_busy();
        let compact = self.compact(theme.text_scale);
        let s = &theme.tokens.spacing;
        let wide = self.content.width() >= 1000.0 && !compact;
        let direction = if wide {
            self.direction
        } else {
            Direction::Column
        };
        let stage: Box<dyn Widget> = match direction {
            Direction::Column => Box::new(
                Stack::column(
                    if compact { s.s6 } else { s.s12 },
                    vec![
                        self.build_clock(theme, compact, AlignItems::Center),
                        Box::new(
                            Stack::column(
                                if compact { s.s4 } else { s.s6 },
                                vec![
                                    self.build_identity(theme, compact, AlignItems::Center),
                                    self.build_form(busy, theme, AlignItems::Center),
                                ],
                            )
                            .align(AlignItems::Center),
                        ),
                    ],
                )
                .align(AlignItems::Center),
            ),
            Direction::Split => Box::new(
                Stack::row(
                    s.s16 * 2.0,
                    vec![
                        Box::new(
                            Stack::column(
                                s.s6,
                                vec![self.build_clock(theme, compact, AlignItems::Start)],
                            )
                            .width(0.0)
                            .grow()
                            .align(AlignItems::Start),
                        ),
                        Box::new(
                            Stack::column(
                                s.s6,
                                vec![
                                    self.build_identity(theme, compact, AlignItems::Start),
                                    self.build_form(busy, theme, AlignItems::Start),
                                ],
                            )
                            .width(FORM_WIDTH)
                            .align(AlignItems::Stretch),
                        ),
                    ],
                )
                .max_width(1080.0)
                .align(AlignItems::Center),
            ),
            Direction::Poster => Box::new(
                Stack::row(
                    s.s16,
                    vec![
                        Box::new(
                            Stack::column(
                                0.0,
                                vec![self.build_clock(theme, compact, AlignItems::Start)],
                            )
                            .width(0.0)
                            .grow()
                            .align(AlignItems::Start),
                        ),
                        Box::new(
                            Stack::column(
                                s.s6,
                                vec![
                                    self.build_identity(theme, compact, AlignItems::Start),
                                    self.build_form(busy, theme, AlignItems::Start),
                                ],
                            )
                            .width(FORM_WIDTH)
                            .align(AlignItems::Stretch),
                        ),
                    ],
                )
                .align(AlignItems::End),
            ),
        };
        // Spacers centre a short stage, or (the poster) settle it on the
        // bottom edge, and collapse before a tall one scrolls.
        let mut viewport_children: Vec<Box<dyn Widget>> = Vec::new();
        viewport_children.push(Box::new(Spacer::new()));
        viewport_children.push(stage);
        if direction != Direction::Poster {
            viewport_children.push(Box::new(Spacer::new()));
        }
        let viewport = Stack::column(0.0, viewport_children)
            .align(AlignItems::Center)
            .padding(if compact { s.s2 } else { s.s8 })
            .fill_height();
        let view: Box<dyn Widget> = Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(Box::new(viewport))
                .with_persisted_offset(self.viewport_offset.clone()),
        );
        Box::new(
            Stack::column(s.s2, {
                let mut column = vec![view, self.build_footer(busy, theme)];
                if self.shows_touch_keyboard() {
                    self.fit_touch_keyboard_to_secret();
                    column.push(Box::new(touch_keyboard::TouchKeyboard::new(
                        self.touch_keyboard.clone(),
                    )));
                }
                column
            })
            .padding(if compact { s.s2 } else { s.s6 })
            .fill_height(),
        )
    }

    /// Record + execute a power action. Splitting the record (always) from
    /// the side-effecting `systemctl` invocation keeps the wiring
    /// unit-testable: tests observe [`last_power_action`] without ever
    /// powering off the host.
    ///
    /// [`last_power_action`]: GreeterApp::last_power_action
    fn request_power(&mut self, action: PowerAction) {
        self.last_power_action = Some(action);
        if let Some(boot) = &self.boot {
            // The supervisor carries it out: this process may not.
            let power = match action {
                PowerAction::Reboot => boot::Power::Reboot,
                PowerAction::PowerOff => boot::Power::PowerOff,
                PowerAction::Suspend => return,
            };
            if boot.link.power(power).is_err() {
                self.show_error("greeter-error-service");
            }
            return;
        }
        Self::run_power(action);
    }

    /// Invoke logind for `action` via `systemctl`, which issues the matching
    /// `org.freedesktop.login1.Manager` call over D-Bus; polkit authorises
    /// these for the `greetd` user through
    /// os/etc/polkit-1/rules.d/49-tilo-greeter-power.rules. Compiled out
    /// under `cfg(test)` so the test suite can never power off the build
    /// host.
    fn run_power(action: PowerAction) {
        let arg = match action {
            PowerAction::Suspend => "suspend",
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
        let mut stored = self.password_buffer.lock_recover();
        let mut password = Zeroizing::new(std::mem::take(&mut *stored));
        drop(stored);
        match method {
            AuthMethod::Password if password.is_empty() => Err("greeter-hint-passcode"),
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
    /// tree. Buttons are activated by [`ui::App`] after widget dispatch, so
    /// the form-level Enter fallback must leave their event unhandled.
    fn focused_button_present(&self) -> bool {
        fn walk(widget: &dyn Widget) -> bool {
            (widget.name() == "Button" && widget.is_focused())
                || widget.children().into_iter().any(walk)
        }

        self.composed.element().is_some_and(walk)
    }

    /// Environment handed to greetd's `StartSession`. Mirrors the
    /// `DesktopNames=tilo` advertised by the session `.desktop` file.
    fn session_env(diagnostics: Option<[bool; 4]>) -> Vec<String> {
        let mut env = vec![
            "XDG_SESSION_TYPE=wayland".to_string(),
            "XDG_SESSION_DESKTOP=tilo".to_string(),
            "XDG_CURRENT_DESKTOP=tilo".to_string(),
        ];
        // Explicit host diagnostics must survive greetd's clean session
        // environment. Forward fixed booleans only; never forward scenarios,
        // input gates, arbitrary environment or caller-supplied session argv.
        let flags = diagnostics.unwrap_or_else(|| {
            SESSION_DIAGNOSTICS.map(|(_, name)| std::env::var(name).is_ok_and(|value| value == "1"))
        });
        for ((_, name), enabled) in SESSION_DIAGNOSTICS.iter().zip(flags) {
            if enabled {
                env.push(format!("{name}=1"));
            }
        }
        env
    }

    fn attempt_username(&self) -> String {
        // A known account's identity comes from conservative host discovery,
        // not the editable field. Otherwise the visible field counts, with
        // incidental surrounding whitespace removed.
        self.login_user.as_ref().map_or_else(
            || self.username.with(|name| name.trim().to_owned()),
            |user| user.username.trim().to_owned(),
        )
    }

    /// Read credentials and kick off authentication.
    ///
    /// Validation failures land an inline hint in the status line. A valid
    /// submit transitions to [`AuthPhase::Authenticating`] and spawns the
    /// background worker (which pumps prompts/result back via
    /// [`UserEvent::External`]). Without an event-loop proxy (tests /
    /// headless), it falls back to a synchronous exchange so the path is
    /// still exercised.
    fn attempt_login(&mut self, method: AuthMethod) {
        if self.boot.is_some() {
            if method == AuthMethod::Password {
                self.attempt_boot_sign_in();
            }
            return;
        }
        if self.phase == AuthPhase::Authenticating && method == AuthMethod::Password {
            if let Some(challenge) = self.auth_session.as_ref().and_then(AuthSession::challenge)
                && let Some(session) = &self.auth_session
            {
                let secret =
                    Zeroizing::new(std::mem::take(&mut *self.password_buffer.lock_recover()));
                if session.respond(challenge.id, secret).is_err() {
                    self.set_hint("greeter-error-unavailable");
                }
                self.dirty_structure = true;
            }
            return;
        }
        // Guard: never start a second attempt while one is in flight.
        if self.phase != AuthPhase::Idle {
            return;
        }
        if method == AuthMethod::Fingerprint
            && self.fingerprint == FingerprintAvailability::Unavailable
        {
            self.set_hint("greeter-hint-no-fingerprint");
            return;
        }
        let username = self.attempt_username();
        if username.is_empty() {
            self.set_hint("greeter-hint-username");
            return;
        }
        if method == AuthMethod::Fingerprint && !self.fingerprint_available_for(&username) {
            self.set_hint("greeter-hint-no-fingerprint");
            return;
        }
        // An explicit fingerprint attempt must not copy or submit a password
        // that happens to remain in the field. PAM/fprintd owns the factor
        // conversation and receives an empty response for secret prompts.
        let password = match self.take_auth_secret(method) {
            Ok(secret) => secret,
            Err(hint) => {
                self.set_hint(hint);
                return;
            }
        };
        let Some(session) = self.sessions.get(self.selected_session) else {
            self.show_error("greeter-error-no-session");
            return;
        };
        let sock = match std::env::var("GREETD_SOCK") {
            Ok(s) if !s.is_empty() => s,
            _ => {
                tracing::warn!("GREETD_SOCK is not set; the greeter was not started by greetd");
                self.show_error("greeter-error-service");
                return;
            }
        };

        let argv = session.argv.clone();
        let env = Self::session_env(self.next_sign_in_diagnostics);

        self.begin_authenticating();

        if let Some(proxy) = self.proxy.get() {
            // Live path: run on a worker thread so the UI can paint the
            // "Signing in…" frame and live fingerprint prompts.
            self.auth_session = Some(spawn_auth(
                proxy.clone(),
                sock,
                username,
                password,
                argv,
                env,
                Arc::clone(&self.challenge_sequence),
            ));
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
                || remember_user(&username),
            ) {
                Ok(()) => {} // never reached — success exits the process
                Err(e) => {
                    tracing::warn!(failure = ?Failure::from(&e), "sign-in did not start a session");
                    self.finish_failure(Failure::from(&e));
                }
            }
        }
    }

    /// Enter the authenticating phase: clear any stale notice and show the
    /// spinner. The inputs become disabled (dropped from the focus order),
    /// so re-seat focus rather than keep a now-invalid ordinal.
    fn begin_authenticating(&mut self) {
        self.phase = AuthPhase::Authenticating;
        self.prompt.clear();
        self.notice = Notice::None;
        self.dirty_structure = true;
        self.focus_reset_pending = true;
    }

    /// The catalogue key that explains `failure` on this form.
    fn failure_key(&self, failure: Failure) -> &'static str {
        match failure {
            Failure::Cancelled => "greeter-auth-cancelled",
            // With a known account there is no username to have mistyped.
            Failure::Credentials if self.login_user.is_some() => "greeter-error-passcode",
            Failure::Credentials => "greeter-error-credentials",
            Failure::Locked => "greeter-error-locked",
            Failure::AccountExpired => "greeter-error-account-expired",
            Failure::PasscodeExpired => "greeter-error-passcode-expired",
            Failure::NotPermitted => "greeter-error-not-permitted",
            Failure::Unavailable => "greeter-error-unavailable",
            Failure::SessionStart => "greeter-error-session",
            Failure::Service => "greeter-error-service",
        }
    }

    /// Return to the form after an attempt that did not start a session:
    /// explain it, clear the passcode, keep the username, and put the
    /// keyboard back on the first field so the next try is just typing.
    fn finish_failure(&mut self, failure: Failure) {
        if self.boot_sign_in != BootSignIn::None {
            self.end_boot_sign_in();
            return;
        }
        self.phase = AuthPhase::Idle;
        self.auth_session.take();
        self.prompt.clear();
        self.failures = if failure == Failure::Credentials {
            self.failures.saturating_add(1)
        } else {
            0
        };
        self.notice = if failure == Failure::Cancelled {
            Notice::Hint(self.failure_key(failure))
        } else {
            Notice::Error(self.failure_key(failure))
        };
        self.clear_password();
        self.dirty_structure = true;
        // Inputs are re-enabled (back in the focus order) — re-seat focus on
        // the rebuilt form rather than a stale in-flight ordinal.
        self.focus_reset_pending = true;
    }

    /// An error found before greetd was asked (no session, no greetd).
    fn show_error(&mut self, key: &'static str) {
        if self.notice != Notice::Error(key) {
            self.notice = Notice::Error(key);
            self.dirty_structure = true;
        }
    }

    /// Empty the process-local passcode buffer. Rebuilding the form drops and
    /// zeroizes the old input widget, so the next field starts genuinely blank.
    fn clear_password(&self) {
        self.password_buffer.lock_recover().zeroize();
    }

    /// Surface a correctable note about the form itself. Distinct from a
    /// refused attempt, which goes through [`GreeterApp::finish_failure`]
    /// and reads as an error.
    fn set_hint(&mut self, key: &'static str) {
        if self.notice != Notice::Hint(key) {
            self.notice = Notice::Hint(key);
            self.dirty_structure = true;
        }
    }

    /// greetd accepted the credentials and is bringing the session up. The
    /// process exits moments later, but the attempt resolves into a visible
    /// success frame rather than the spinner just disappearing.
    fn begin_starting(&mut self) {
        if self.boot_sign_in == BootSignIn::Running {
            remember_user(&self.attempt_username());
        }
        if self.phase == AuthPhase::Starting {
            return;
        }
        if let Some(user) = &self.login_user {
            remember_user(&user.username);
        }
        self.phase = AuthPhase::Starting;
        self.prompt.clear();
        self.notice = Notice::None;
        self.failures = 0;
        self.dirty_structure = true;
    }

    /// Switch to another of the listed accounts.
    fn choose_account(&mut self, index: usize) {
        let Some(user) = self.users.get(index).cloned() else {
            return;
        };
        if self.phase.is_busy()
            || self
                .login_user
                .as_ref()
                .is_some_and(|chosen| chosen.username == user.username)
        {
            return;
        }
        self.username.set(user.username.clone());
        self.login_user = Some(user);
        self.clear_password();
        self.notice = Notice::None;
        self.failures = 0;
        self.dirty_structure = true;
        self.focus_reset_pending = true;
    }

    /// Clear the passcode and any notice, as Esc does.
    fn clear_form(&mut self) {
        self.clear_password();
        self.notice = Notice::None;
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
    ui::provides!(
        dyn ui::widget::FullscreenProvider,
        dyn ui::resources::PublishesResources,
        dyn ui::host_operation::ResourceOperations
    );

    /// Take what the controls reported since the last pass. The document is
    /// only lent on to the composed controls; the greeter itself neither
    /// reads nor writes it.
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        // The minute turned over (the wake deadline brought us here), or the
        // machine's state changed: rebuild once to show it.
        if !self.clock_pinned {
            let (wall, now) = (ui::clock::local_now(), ui::time::now());
            let clock = ClockFace::of(&wall);
            let turned = clock != self.clock;
            if turned {
                self.clock = clock;
                self.device = DeviceStatus::read();
                self.dirty_structure = true;
            }
            // The next wake is the end of the minute now shown, also when
            // the wake came and the wall clock had been set back under it.
            if turned || self.clock_turns <= now {
                self.clock_turns = minute_ends(&wall, now);
            }
        }

        // Mirror the picker's chosen name onto the selected index. An
        // unrecognised name (a session that vanished between renders) is
        // ignored rather than resetting the choice out from under the user.
        let chosen = self
            .session_choice
            .with(|name| self.sessions.iter().position(|s| s.name == *name));
        if let Some(idx) = chosen
            && idx != self.selected_session
        {
            self.selected_session = idx;
            self.dirty_structure = true;
        }

        if let Some(index) = self.requests.account.take() {
            self.choose_account(index);
        }

        self.take_touch_keys();
        // Pull local-only passcode edits into the zeroized buffer before
        // taking a sign-in request raised by Enter or the sign-in action in
        // the same event cycle.
        self.composed.sync(state, root);

        self.start_boot_sign_in();

        // Each request is taken before it is acted on, so one activation
        // starts exactly one attempt.
        if self.requests.cancel_authentication.take()
            && let Some(session) = &self.auth_session
        {
            let _ = session.cancel();
            self.clear_password();
            self.dirty_structure = true;
        }
        // Enter in the recovery form sets the new secret.
        if self.requests.sign_in.take() {
            if self.recovery.is_some() {
                self.submit_recovery();
            } else {
                self.attempt_login(AuthMethod::Password);
            }
        }

        if self.requests.fingerprint.take() {
            self.attempt_login(AuthMethod::Fingerprint);
        }

        if let Some(action) = self.requests.power.take() {
            self.request_power(action);
        }

        if self.requests.cant_sign_in.take() && self.phase == AuthPhase::Idle {
            if let Some(boot) = &self.boot {
                self.clear_password();
                if boot.link.cant_sign_in().is_ok() {
                    self.begin_authenticating();
                } else {
                    self.show_error("greeter-error-service");
                }
            } else {
                self.open_recovery();
            }
        }
        if self.requests.recover.take() {
            self.submit_recovery();
        }
        if self.requests.cancel_recovery.take() {
            self.cancel_recovery();
        }
        self.take_recovery_result();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_text_scale = theme.text_scale;
        self.layout_keyboard_height = if self.shows_touch_keyboard() {
            touch_keyboard::height(theme)
        } else {
            0.0
        };
        (self.layout_area, self.layout_outputs) = self.output_layout();
        let mut composition = self.build_composition(theme);
        if let Some(area) = self.layout_area {
            // The composition fills the primary output's part of the window,
            // and every other output shows the clock over the backdrop, as
            // the lock screen's other displays do. The boxes are widgets, not
            // bare taffy nodes: the App walks the widget and taffy trees in
            // lock-step for the focus order, and a node without a widget
            // shifted every focusable onto its neighbour's node, so autofocus
            // missed the passcode field.
            let mut outputs: Vec<Box<dyn Widget>> = vec![Box::new(
                Stack::column(0.0, vec![composition])
                    .align(AlignItems::Center)
                    .justify(JustifyContent::Center)
                    .place(area),
            )];
            outputs.extend(
                self.layout_outputs
                    .iter()
                    .filter(|output| **output != area)
                    .map(|output| self.build_display_clock(theme, *output)),
            );
            composition = Box::new(Stack::column(0.0, outputs).fill_height());
        }
        let child = self
            .composed
            .build_layout(composition, taffy, theme, node_map);

        let style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            justify_content: Some(JustifyContent::Center),
            align_items: Some(AlignItems::Center),
            size: Size::from_percent(1.0_f32, 1.0_f32),
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let before = (
            self.compact(self.layout_text_scale),
            self.clock_size(self.compact(self.layout_text_scale))
                .to_bits(),
            self.content.width() >= 1000.0,
        );
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let child = taffy
            .children(node)
            .ok()
            .and_then(|children| children.first().copied());
        let child_origin = Point::new(self.rect.x0, self.rect.y0);
        self.content = self.rect;
        if self.layout_area.is_some()
            && let Some(outputs) = child
            && let Some(area) = taffy
                .children(outputs)
                .ok()
                .and_then(|children| children.first().copied())
        {
            // The primary-output box built above holds the composition.
            let outputs = ui::widget::layout_rect(taffy, outputs, child_origin);
            self.content = ui::widget::layout_rect(taffy, area, outputs.origin());
        }
        // The composition depends on the viewport. App and FrameHarness
        // settle this rebuild before painting, including the very first frame
        // and crossings caused by display/text scaling.
        let after = (
            self.compact(self.layout_text_scale),
            self.clock_size(self.compact(self.layout_text_scale))
                .to_bits(),
            self.content.width() >= 1000.0,
        );
        // The window reached (or left) the size of a multi-output layout:
        // lay the composition out again for the outputs it now spans.
        if before != after || self.output_layout().0 != self.layout_area {
            self.dirty_structure = true;
        }
        if let Some(c) = self.composed.element_mut()
            && let Some(child) = child
        {
            c.apply_layout(taffy, child, child_origin);
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
        // The backdrop holds one composition. A login screen mostly waits,
        // and a moving full-screen backdrop re-rasterized the whole display
        // behind every keystroke.
        // Each output receives its own complete composition, as on the lock
        // screen, rather than one stretched across displays of different
        // sizes.
        if self.layout_outputs.is_empty() {
            ui::auth_backdrop::draw(scene, theme, transform, self.rect, BACKDROP_PHASE_SECONDS);
        }
        for output in &self.layout_outputs {
            let output = Rect::from_origin_size(
                (output.x0 + self.rect.x0, output.y0 + self.rect.y0),
                output.size(),
            );
            ui::auth_backdrop::draw(scene, theme, transform, output, BACKDROP_PHASE_SECONDS);
        }
        if let Some(c) = self.composed.element_mut() {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::Focused(focused) => self.keyboard_focus = *focused,
            WindowEvent::KeyboardInput { .. } => self.keyboard_focus = true,
            _ => {}
        }
        // Track Caps Lock from typed letters (winit's Modifiers can't report
        // the lock directly — see `ui::caps_lock`). A flip in the estimate
        // triggers a rebuild so the note appears/disappears. Focus + caret
        // survive that rebuild via the App's `rebuild_structure` continuity.
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

        let pressed = |named: NamedKey| {
            matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: KeyEvent {
                        logical_key: Key::Named(key),
                        state: ElementState::Pressed,
                        ..
                    },
                    ..
                } if *key == named
            )
        };
        let enter_pressed = pressed(NamedKey::Enter);
        let escape_pressed = pressed(NamedKey::Escape);

        // Delegate to the composed tree.
        if let Some(c) = self.composed.element_mut()
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&child) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, child));
        }

        // A focused control owns Enter. Dropdown consumes it above; buttons
        // deliberately leave it for App's standard focused-control activation.
        // Only an otherwise-unhandled Enter (normally from either text field)
        // acts as the form's default sign-in action.
        if enter_pressed && !*ctx.handled && !self.focused_button_present() {
            self.requests.sign_in.set(true);
            *ctx.handled = true;
        }
        // Esc not taken by an open menu leaves the recovery form, or starts
        // the passcode over.
        if escape_pressed && !*ctx.handled && self.recovery.is_some() {
            self.requests.cancel_recovery.set(true);
            *ctx.handled = true;
        }
        if escape_pressed && !*ctx.handled && self.phase == AuthPhase::Idle {
            self.clear_form();
            *ctx.handled = true;
        }
        if escape_pressed && !*ctx.handled && self.phase == AuthPhase::Authenticating {
            self.requests.cancel_authentication.set(true);
            *ctx.handled = true;
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed.children()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed.children_mut()
    }

    /// The card is rebuilt from state: each build keeps the input in use,
    /// so a finger gets touch-sized fields and actions.
    fn set_interaction_environment(&mut self, environment: ui::InteractionEnvironment) {
        let shown = self.shows_touch_keyboard();
        self.environment = environment;
        if self.shows_touch_keyboard() != shown {
            self.dirty_structure = true;
        }
        self.composed.set_interaction_environment(environment);
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if let Some(c) = self.composed.element() {
            visitor(c);
        }
    }

    fn needs_redraw(&self) -> bool {
        self.composed.element().is_some_and(Widget::needs_redraw)
    }

    /// The still backdrop never changes between rebuilds, so what the
    /// controls report is the whole change — never the whole screen.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if let Some(c) = self.composed.element() {
            c.collect_damage(regions);
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mine = self.dirty_structure;
        self.dirty_structure = false;
        let child = self.composed.take_structure_dirty();
        mine || child
    }

    /// Rising-edge one-shot consumed by the App's `rebuild_structure`. We
    /// raise the flag on auth-phase and account changes (which change the
    /// focusable set or what the first field means); plain notice / caps /
    /// session rebuilds leave it `false` so focus + caret are preserved.
    fn take_focus_reset(&mut self) -> bool {
        let mine = std::mem::take(&mut self.focus_reset_pending);
        let child = self
            .composed
            .element_mut()
            .is_some_and(Widget::take_focus_reset);
        mine || child
    }

    fn take_accessibility_request(&mut self) -> Option<AccessibilityRequest> {
        let request = self.requests.accessibility.take()?;
        self.keep_accessibility_change(request);
        Some(request)
    }

    fn on_external_event(
        &mut self,
        payload: &str,
        state: &mut ui::widget::Edits<'_>,
        root: &ModelPath,
    ) {
        let (state, _root) = ui::doc::document_ref(state.doc(), root);
        if payload == CONSOLE_WAKE {
            self.serve_console(state);
            return;
        }
        if self.keep_screen_reader_chord(payload) {
            return;
        }
        if payload == recovery::RECOVERY_WAKE {
            self.take_recovery_result();
            return;
        }
        if payload == crate::output_scale::OUTPUTS_WAKE {
            // A display was plugged, removed or rescaled: place the card and
            // the backdrop for the new arrangement.
            self.dirty_structure = true;
            return;
        }
        if payload == crate::keyboards::KEYBOARDS_WAKE {
            let shown = self.shows_touch_keyboard();
            self.keyboards = Some(crate::keyboards::present(std::path::Path::new(
                crate::keyboards::INPUT_CLASS,
            )));
            if self.shows_touch_keyboard() != shown {
                self.dirty_structure = true;
            }
            return;
        }
        if let Some(message) = boot::decode(payload) {
            self.boot_message(message);
            return;
        }
        // Progress from the background auth worker (see `crate::auth`).
        match AuthUpdate::decode(payload) {
            Some(AuthUpdate::Challenge(challenge))
                if self.phase == AuthPhase::Authenticating
                    && self.auth_session.as_ref().and_then(AuthSession::challenge)
                        == Some(challenge) =>
            {
                self.clear_password();
                self.dirty_structure = true;
                self.focus_reset_pending = true;
            }
            Some(AuthUpdate::Prompt { text, is_error: _ }) => {
                // Reflect the live PAM prompt (e.g. fingerprint
                // instructions) while authenticating. The text itself carries
                // the meaning; its colour follows the theme.
                if self.phase == AuthPhase::Authenticating && self.prompt != text {
                    self.prompt = text;
                    self.dirty_structure = true;
                }
            }
            Some(AuthUpdate::Accepted) => {
                self.begin_starting();
            }
            Some(AuthUpdate::Failed(failure)) => {
                self.finish_failure(failure);
            }
            // Unrelated External payloads are ignored.
            Some(AuthUpdate::Challenge(_)) | None => {}
        }
    }

    /// The controls' own deadlines, and the next minute for the clock.
    fn next_wake_deadline(&self) -> Option<Instant> {
        let child = self.composed.element().and_then(Widget::next_wake_deadline);
        let clock = (!self.clock_pinned).then_some(self.clock_turns);
        match (child, clock) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.composed
            .element()
            .is_some_and(Widget::wants_pointer_capture)
    }

    fn name(&self) -> &'static str {
        "GreeterApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::host_operation::ResourceOperations for GreeterApp {
    /// The greeter answers its own published operations; without this the
    /// host refuses to publish any of them.
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[console::OPERATION]
    }

    /// The host's action surface admits a greeter operation here, after its
    /// own resolution, availability and argument binding. It runs exactly
    /// what the console runs.
    fn start_operation(
        &mut self,
        op: &ui::host_operation::ResourceOperation<'_>,
        doc: &mut ui::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped().filter(|r| r.type_id == console::OPERATION) else {
            return ui::host_operation::start_in_children(self, op, doc, path);
        };
        let mut encoded = ui::tilo_actions::EncodedAction {
            type_id: request.type_id.to_owned(),
            payload: request.payload.to_vec(),
        };
        let applied = self.apply_operation(&encoded);
        encoded.payload.zeroize();
        applied
            .map(|value| {
                Some(ui::host_operation::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                ))
            })
            .map_err(|(_, message)| message)
    }
}

#[cfg(test)]
mod tests;

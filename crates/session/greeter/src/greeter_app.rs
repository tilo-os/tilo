//! [`GreeterApp`] — the root login widget.
//!
//! One centred sign-in panel on the shared authentication backdrop: who is
//! signing in, the passcode field with its sign-in action beside it, one
//! status line, and the alternate factor when there is one. Power actions,
//! and the session choice when the machine offers more than one session,
//! sit quietly in the footer. Every control is a toolkit widget.
//!
//! IMPORTANT (focus): every layout node has a matching widget in `children()`.
//! This keeps the toolkit's focus, accessibility and layout traversals aligned
//! through every grouping and across recomposition.
//!
//! IMPORTANT (frames): the backdrop is still, and every container forwards
//! its children's damage instead of claiming its whole box. A keystroke
//! therefore repaints the passcode field, not the screen.
//!
//! Non-secret presentation state lives in the CRDT (the toolkit convention):
//!   * `greeter.username` — `Text`
//!   * `greeter.submit`   — `bool` flag (Sign in / Enter rising edge)
//!   * `greeter.fingerprint` — `bool` flag (explicit passwordless PAM attempt)
//!   * `greeter.session`  — `Text` (the chosen session's label)
//!
//! The passcode is deliberately different: it is held only in a process-local,
//! zeroized buffer shared with a local-mode [`TextInput`]. It never enters an
//! Automerge object, change history, persistence, presence, or peer sync.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
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
    Avatar, Button, Dropdown, IconWidget, Row, Scrollable, Spacer, Spinner, Text, TextInput, Widget,
};

mod console;

pub use console::{CONSOLE_WAKE, PROVIDER as CONSOLE_PROVIDER};

use crate::auth::{AuthUpdate, Failure, spawn_auth};
use crate::i18n::t;
use crate::local_users::{LoginUser, discover_default_login_user};
use crate::sessions::{DEFAULT_SESSIONS_DIR, Session, read_sessions};

const USERNAME_PATH: &str = "greeter.username";
const SUBMIT_PATH: &str = "greeter.submit";
const FINGERPRINT_PATH: &str = "greeter.fingerprint";
/// The session picker's chosen label, as a CRDT `Text`. The picker writes
/// its selection here; [`GreeterApp::sync`] mirrors it back onto the
/// selected index.
const SESSION_PATH: &str = "greeter.session";
/// Rising-edge boolean flags raised by the footer's power actions.
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

/// The sign-in panel's comfortable reading width (logical px). It narrows
/// with the viewport.
const PANEL_WIDTH: f32 = 400.0;
/// The passcode field's band at normal text size; the sign-in action beside
/// it grows with the field.
const FIELD_HEIGHT: f32 = 40.0;
/// The footer's session picker, wide enough for a session's name.
const SESSION_PICKER_WIDTH: f32 = 220.0;
/// Avatar diameter on a roomy display, and when space or large text is tight.
const AVATAR_SIZE: f32 = 88.0;
const COMPACT_AVATAR_SIZE: f32 = 56.0;
/// The status line is reserved at this height in *every* state, so a message
/// appearing — or a spinner replacing it — never resizes the panel. Without
/// the reservation the form jumps the moment a passcode is wrong, which is
/// exactly when the person is least able to absorb it.
const STATUS_HEIGHT: f32 = 20.0;
/// Below this logical height, or at large text, the panel tightens its
/// spacing and avatar so the form stays above the footer.
const COMPACT_VIEWPORT_HEIGHT: f64 = 600.0;
const COMPACT_TEXT_SCALE: f32 = 1.5;
/// The still backdrop's composition. Any phase is a valid frame; this one
/// keeps the band of light clear of the centred panel.
const BACKDROP_PHASE_SECONDS: f64 = 0.0;

/// A stack with an explicit gap and optional padding, fixed width,
/// minimum height, and opaque panel chrome.
///
/// Every grouping in the greeter is one of these. It exists as a real
/// `Widget` rather than a bare taffy node because the App discovers
/// focusable widgets by walking the widget tree (`Widget::children`) in
/// lock-step with the taffy tree. Because each stack contributes exactly one
/// taffy node whose taffy children are its widget children, the two trees
/// mirror each other by construction at every depth.
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
    /// Paint the opaque sign-in surface and its theme-derived edge.
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
            chrome: false,
            rect: Rect::ZERO,
        }
    }

    /// A grouping whose children keep their own width and sit on one axis.
    fn centered(gap: f32, children: Vec<Box<dyn Widget>>) -> Self {
        let mut stack = Self::new(gap, children);
        stack.center_children = true;
        stack
    }

    /// The sign-in panel itself.
    fn panel(gap: f32, padding: f32, children: Vec<Box<dyn Widget>>) -> Self {
        let mut stack = Self::new(gap, children);
        stack.padding = padding;
        stack.max_width = Some(PANEL_WIDTH);
        stack.chrome = true;
        stack
    }

    /// A viewport-height stage with flexible spacers above and below the
    /// panel. The spacers center a short panel, then collapse to zero before
    /// a tall one overflows into the surrounding [`Scrollable`].
    fn viewport(child: Box<dyn Widget>, padding: f32) -> Self {
        let mut stack = Self::centered(
            0.0,
            vec![Box::new(Spacer::new()), child, Box::new(Spacer::new())],
        );
        stack.padding = padding;
        stack.fill_height = true;
        stack
    }

    /// Full-height screen shell for the scrollable panel and the footer.
    fn shell(children: Vec<Box<dyn Widget>>, padding: f32) -> Self {
        let mut stack = Self::new(padding, children);
        stack.padding = padding;
        stack.fill_height = true;
        stack
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
                    flex_direction: FlexDirection::Column,
                    // Stretch, not Center, unless asked: controls span the
                    // panel's content width and copy sits on one margin.
                    align_items: Some(if self.center_children {
                        AlignItems::Center
                    } else {
                        AlignItems::Stretch
                    }),
                    gap: Size {
                        width: length(0.0_f32),
                        height: length(self.gap),
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
                        height: self.min_height.map_or(Dimension::auto(), length),
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
            // separates it from the backdrop without a heavy shadow.
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

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in &self.children {
            visitor(child.as_ref());
        }
    }

    fn needs_redraw(&self) -> bool {
        self.children.iter().any(|c| c.needs_redraw())
    }

    /// The stack's own paint (the panel surface) changes only with a
    /// rebuild, which repaints everything. Between rebuilds its children
    /// are the whole change.
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
    /// because it is greyed out — so it remains the panel's keyboard home
    /// instead of the focus ring falling through to the power actions.
    pub fn is_busy(self) -> bool {
        matches!(self, AuthPhase::Authenticating | AuthPhase::Starting)
    }
}

/// What the panel's single status line is saying, as a catalogue key.
/// Severity is carried in the state rather than inferred from the wording,
/// so an error reads as an error at a glance.
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
    Reboot,
    PowerOff,
}

/// Pre-canned visual states for the headless `--shots` render path so each
/// distinct screen can be snapshot-verified. Live runs never use these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScenario {
    /// The empty sign-in form on a machine with several accounts.
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
    /// Personal-device form with an expired passcode.
    PersonalPasscodeExpired,
    /// Personal-device form on a machine with a second session installed,
    /// so the footer offers the choice.
    PersonalSessions,
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
    notice: Notice,
    /// Current PAM conversation prompt during [`AuthPhase::Authenticating`]
    /// (e.g. "Place your finger on the reader"). Empty falls back to the
    /// generic "Signing in…" line.
    prompt: String,
    phase: AuthPhase,
    bootstrap_done: bool,
    /// Set when the composition must be rebuilt (session, notice, or auth
    /// phase changed). Drained by `take_structure_dirty`, which the App
    /// polls to trigger a fresh `build_layout`.
    dirty_structure: bool,
    /// Heuristic Caps-Lock state inferred from typed letters (winit can't
    /// report the lock directly). See [`ui::caps_lock`].
    caps_lock: CapsLockEstimate,
    /// One-shot request that the App re-seat keyboard focus on the first
    /// focusable widget after the next rebuild (drained by
    /// [`Widget::take_focus_reset`]). Raised on auth-phase transitions,
    /// which enable/disable the inputs and therefore change *which* widgets
    /// are focusable. Plain notice/caps rebuilds leave it `false` so focus +
    /// caret are preserved in place.
    focus_reset_pending: bool,
    /// The most recent power action requested, recorded for test
    /// observability (the actual `systemctl` invocation is a side effect).
    last_power_action: Option<PowerAction>,
    rect: Rect,
    /// The composed panel subtree, rebuilt each `build_layout`.
    composed: Option<Box<dyn Widget>>,
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
        crate::i18n::ensure();
        Self {
            sessions,
            selected_session: 0,
            login_user,
            fingerprint,
            fingerprint_probe: enrolled_fingerprint,
            password_buffer: Arc::new(Mutex::new(String::new())),
            notice: Notice::None,
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
            console: None,
            keyboard_focus: false,
        }
    }

    /// Wire the shared event-loop proxy slot. `main.rs` constructs the slot,
    /// hands a clone here, and populates it inside `App::run_with_setup`.
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
        let personal = personal.then(|| LoginUser {
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
        let sessions = if scenario == AuthScenario::PersonalSessions {
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
        } else {
            sessions
        };
        let mut app =
            Self::with_sessions_login_user_and_fingerprint(sessions, personal, fingerprint);
        match scenario {
            AuthScenario::Form
            | AuthScenario::LongName
            | AuthScenario::Personal
            | AuthScenario::PersonalNoFingerprint
            | AuthScenario::PersonalSessions => {}
            AuthScenario::Error => app.notice = Notice::Error("greeter-error-credentials"),
            AuthScenario::PersonalError => app.notice = Notice::Error("greeter-error-passcode"),
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

    /// Tight space or large text: a smaller avatar and closer spacing keep
    /// the whole form above the footer.
    fn compact(&self, text_scale: f32) -> bool {
        (self.rect.height() > 0.0 && self.rect.height() < COMPACT_VIEWPORT_HEIGHT)
            || text_scale >= COMPACT_TEXT_SCALE
    }

    /// Whether the footer offers a session choice: only when there is more
    /// than one session to choose between. With a single session the
    /// greeter starts it directly, so a one-option dropdown never appears.
    pub(crate) fn offers_session_choice(&self) -> bool {
        self.sessions.len() > 1
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
            AuthPhase::Authenticating if !self.prompt.trim().is_empty() => {
                self.prompt.trim().to_string()
            }
            AuthPhase::Authenticating => t("greeter-signing-in"),
            AuthPhase::Starting => t("greeter-starting"),
            AuthPhase::Idle => self.notice.key().map(t).unwrap_or_default(),
        }
    }

    /// The panel's single feedback voice, reserved at a constant height in
    /// every state so the form never jumps. Exactly one thing speaks at a
    /// time, with the Caps-Lock note beneath it while the form is editable.
    fn build_status(&self, theme: &Theme) -> VStack {
        let c = &theme.tokens.colors;
        let mut rows: Vec<Box<dyn Widget>> = Vec::new();
        let line = |lead: Box<dyn Widget>, text: String, color, live| -> Box<dyn Widget> {
            Box::new(
                Row::new()
                    .with_gap(theme.tokens.spacing.s2)
                    .no_wrap()
                    .push(lead)
                    .push(Box::new(
                        Text::new(&text)
                            .with_id("greeter.status")
                            .with_size(13.0)
                            .with_color(color)
                            .with_live(live),
                    )),
            )
        };
        match (self.phase, self.notice) {
            (AuthPhase::Authenticating, _) => rows.push(line(
                Box::new(Spinner::with_size(14.0)),
                self.status_text(),
                c.text_dim,
                // PAM prompts can require immediate physical action (for
                // example, touching the fingerprint reader).
                Politeness::Assertive,
            )),
            (AuthPhase::Starting, _) => rows.push(line(
                Box::new(Spinner::with_size(14.0)),
                self.status_text(),
                c.semantic.success,
                Politeness::Polite,
            )),
            (AuthPhase::Idle, Notice::Hint(_)) => rows.push(line(
                Box::new(IconWidget::new(Icon::Info, 14.0).with_color(c.text_dim)),
                self.status_text(),
                c.text_dim,
                Politeness::Assertive,
            )),
            (AuthPhase::Idle, Notice::Error(_)) => rows.push(line(
                Box::new(IconWidget::new(Icon::CircleAlert, 14.0).with_color(c.semantic.danger)),
                self.status_text(),
                c.semantic.danger,
                Politeness::Assertive,
            )),
            (AuthPhase::Idle, Notice::None) => {}
        }
        if self.phase == AuthPhase::Idle && self.caps_lock.on() {
            rows.push(Box::new(
                Row::new()
                    .with_gap(theme.tokens.spacing.s2)
                    .no_wrap()
                    .push(Box::new(
                        IconWidget::new(Icon::ArrowBigUp, 14.0).with_color(c.semantic.warning),
                    ))
                    .push(Box::new(
                        Text::new(&t("greeter-caps-lock"))
                            .with_size(13.0)
                            .with_color(c.semantic.warning),
                    )),
            ));
        }
        VStack::new(theme.tokens.spacing.s1, rows).with_min_height(STATUS_HEIGHT)
    }

    /// Who is signing in. On a personal device that is the person, by
    /// avatar and name; a machine with several accounts cannot greet anyone
    /// yet and names the action instead.
    fn build_identity(&self, theme: &Theme, compact: bool) -> Box<dyn Widget> {
        let c = &theme.tokens.colors;
        let mut identity: Vec<Box<dyn Widget>> = Vec::new();
        if let Some(user) = &self.login_user {
            let initials = Some(Avatar::initials_for_name(&user.display_name))
                .filter(|initials| !initials.is_empty())
                .unwrap_or_else(|| Avatar::initials_for_name(&user.username));
            identity.push(Box::new(Avatar::from_initials(
                &initials,
                if compact {
                    COMPACT_AVATAR_SIZE
                } else {
                    AVATAR_SIZE
                },
            )));
            identity.push(Box::new(
                Text::new(&user.display_name)
                    .with_id("greeter.welcome")
                    .with_heading_level(1)
                    .with_size(if compact { 20.0 } else { 24.0 }),
            ));
            if user.display_name != user.username {
                identity.push(Box::new(
                    Text::new(&user.username)
                        .with_id("greeter.account")
                        .with_size(13.0)
                        .with_color(c.text_dim),
                ));
            }
        } else {
            identity.push(Box::new(
                Text::new(&t("greeter-title"))
                    .with_id("greeter.welcome")
                    .with_heading_level(1)
                    .with_size(if compact { 20.0 } else { 24.0 }),
            ));
        }
        Box::new(VStack::centered(theme.tokens.spacing.s2, identity))
    }

    /// The credentials, the sign-in action beside the passcode, the status
    /// line, and the alternate factor.
    fn build_form(&self, busy: bool, theme: &Theme) -> Box<dyn Widget> {
        let s = &theme.tokens.spacing;
        let mut form: Vec<Box<dyn Widget>> = Vec::new();
        if self.login_user.is_none() {
            form.push(Box::new(
                TextInput::new(Vec::new())
                    .with_id("greeter.username")
                    .with_on_change_path(USERNAME_PATH)
                    .with_accessible_name(t("greeter-username"))
                    .with_placeholder(&t("greeter-username-placeholder"))
                    .disabled(busy),
            ));
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
                .with_accessible_name(t("greeter-passcode"))
                .with_secret(true)
                .with_placeholder(&t("greeter-passcode-placeholder"))
                .disabled(busy)
        };
        let sign_in = t("greeter-sign-in");
        form.push(Box::new(
            // The action takes the field's height, whatever the text scale
            // makes it, so the two read as one control.
            Row::new()
                .with_gap(s.s2)
                .no_wrap()
                .with_align_items(AlignItems::Stretch)
                .push(Box::new(password_input))
                .push(Box::new(
                    Button::new("greeter.login", "", Some(SUBMIT_PATH))
                        .primary()
                        .with_icon(Icon::ArrowRight)
                        .with_accessible_name(&sign_in)
                        .tooltip(sign_in.clone())
                        .content_sized()
                        .with_min_height(FIELD_HEIGHT),
                )),
        ));
        form.push(Box::new(self.build_status(theme)));
        // While an attempt runs the alternate factor stays in place, inert
        // and out of the focus order, so the panel does not change size.
        if self.fingerprint != FingerprintAvailability::Unavailable {
            form.push(Box::new(VStack::centered(
                0.0,
                vec![Box::new(
                    Button::new(
                        "greeter.fingerprint",
                        &t("greeter-use-fingerprint"),
                        Some(FINGERPRINT_PATH),
                    )
                    .ghost()
                    .with_icon(Icon::Fingerprint)
                    .content_sized()
                    .with_min_height(FIELD_HEIGHT)
                    .with_enabled(!busy),
                )],
            )));
        }
        Box::new(VStack::new(s.s3, form))
    }

    /// Build the composed subtree from current state.
    ///
    /// Focus continuity contract: the focusable set is, in tree order, the
    /// optional username field, passcode field, sign-in action, fingerprint
    /// action, then the footer's session picker (only with more than one
    /// session) and power actions. On an unambiguous personal machine the
    /// passcode is the first focusable control. Notices, the Caps-Lock note
    /// and the spinner are non-focusable, so an incidental rebuild keeps
    /// keyboard focus AND the caret in place — see `main.rs`'s
    /// `autofocus_on_structure_change`.
    ///
    /// While an attempt is in flight the inputs are **disabled** (dimmed and
    /// dropped from the focus order); the phase transition raises
    /// `take_focus_reset` so the App re-seats focus rather than preserving a
    /// now-invalid ordinal. The sign-in action remains enabled while its
    /// handler rejects a second submit, ensuring that reset cannot land on a
    /// power action.
    fn build_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let busy = self.phase.is_busy();
        let compact = self.compact(theme.text_scale);
        let s = &theme.tokens.spacing;
        let panel = VStack::panel(
            if compact { s.s4 } else { s.s6 },
            if compact { s.s4 } else { s.s8 },
            vec![
                self.build_identity(theme, compact),
                self.build_form(busy, theme),
            ],
        );
        let stage = VStack::viewport(Box::new(panel), if compact { s.s2 } else { s.s4 });
        let view: Box<dyn Widget> = Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(Box::new(stage))
                .with_persisted_offset(self.viewport_offset.clone()),
        );
        Box::new(VStack::shell(
            vec![view, self.build_footer(busy)],
            if compact { s.s2 } else { s.s4 },
        ))
    }

    /// Session choice (only when there is one to make) and power actions,
    /// quiet in the corner.
    fn build_footer(&self, busy: bool) -> Box<dyn Widget> {
        let mut footer = Row::new().no_wrap().push(Box::new(Spacer::new()));
        if self.offers_session_choice() {
            let options = self
                .sessions
                .iter()
                .map(|session| session.name.clone())
                .collect();
            footer = footer
                .push(Box::new(IconWidget::new(Icon::Monitor, 16.0).dim()))
                .push(Box::new({
                    // The picker keeps a modest width; the footer's spacer,
                    // not the choice, takes the rest of the row.
                    let mut picker = VStack::new(
                        0.0,
                        vec![Box::new(
                            Dropdown::new(options, SESSION_PATH)
                                .with_id("greeter.session")
                                .with_accessible_name(t("greeter-session"))
                                .with_enabled(!busy),
                        )],
                    );
                    picker.width = Some(SESSION_PICKER_WIDTH);
                    picker
                }));
        }
        Box::new(
            footer
                .push(Box::new(
                    Button::new("greeter.reboot", &t("greeter-restart"), Some(REBOOT_PATH))
                        .ghost()
                        .with_icon(Icon::RotateCw)
                        .content_sized()
                        .with_min_height(FIELD_HEIGHT),
                ))
                .push(Box::new(
                    Button::new(
                        "greeter.poweroff",
                        &t("greeter-shut-down"),
                        Some(POWEROFF_PATH),
                    )
                    .ghost()
                    .with_icon(Icon::Power)
                    .content_sized()
                    .with_min_height(FIELD_HEIGHT),
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
    /// through os/etc/polkit-1/rules.d/49-tilo-greeter-power.rules. Compiled
    /// out under `cfg(test)` so the test suite can never reboot the build
    /// host.
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
    /// panel. Buttons are activated by [`ui::App`] after widget dispatch, so
    /// the form-level Enter fallback must leave their event unhandled.
    fn focused_button_present(&self) -> bool {
        fn walk(widget: &dyn Widget) -> bool {
            (widget.name() == "Button" && widget.is_focused())
                || widget.children().into_iter().any(walk)
        }

        self.composed.as_deref().is_some_and(walk)
    }

    /// Environment handed to greetd's `StartSession`. Mirrors the
    /// `DesktopNames=tilo` advertised by the session `.desktop` file.
    fn session_env() -> Vec<String> {
        vec![
            "XDG_SESSION_TYPE=wayland".to_string(),
            "XDG_SESSION_DESKTOP=tilo".to_string(),
            "XDG_CURRENT_DESKTOP=tilo".to_string(),
        ]
    }

    /// Read credentials and kick off authentication.
    ///
    /// Validation failures land an inline hint in the status line. A valid
    /// submit transitions to [`AuthPhase::Authenticating`] and spawns the
    /// background worker (which pumps prompts/result back via
    /// [`UserEvent::External`]). Without an event-loop proxy (tests /
    /// headless), it falls back to a synchronous exchange so the path is
    /// still exercised.
    fn attempt_login(&mut self, state: &AutoCommit, root: &ObjId, method: AuthMethod) {
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
        let env = Self::session_env();

        self.begin_authenticating();

        if let Some(proxy) = self.proxy.get() {
            // Live path: run on a worker thread so the UI can paint the
            // "Signing in…" frame and live fingerprint prompts.
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
                Err(e) => {
                    tracing::warn!(detail = ?e.detail(), "sign-in did not start a session");
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
            // With a fixed account there is no username to have mistyped.
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
        self.phase = AuthPhase::Idle;
        self.prompt.clear();
        self.notice = Notice::Error(self.failure_key(failure));
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
        self.password_buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .zeroize();
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
        if self.phase == AuthPhase::Starting {
            return;
        }
        self.phase = AuthPhase::Starting;
        self.prompt.clear();
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
        // consuming a submit edge raised by Enter or the sign-in action in
        // the same event cycle. No secret is written to `state` by this sync.
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
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        // `new_with_children` only fails when a child belongs to a different
        // tree; `child` was just built in this one above.
        #[allow(
            clippy::unwrap_used,
            reason = "the child node was just created in this same tree"
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
                &[child],
            )
            .unwrap();

        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        let was_compact = self.compact(self.layout_text_scale);
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // App and FrameHarness settle this rebuild before painting, including
        // the very first frame and crossings caused by display/text scaling.
        if was_compact != self.compact(self.layout_text_scale) {
            self.dirty_structure = true;
        }
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&child) = children.first()
        {
            c.apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
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
        ui::auth_backdrop::draw(scene, theme, self.rect, BACKDROP_PHASE_SECONDS);
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

        // Delegate to the composed panel.
        if let Some(c) = &mut self.composed
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

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if let Some(c) = &self.composed {
            visitor(c.as_ref());
        }
    }

    fn needs_redraw(&self) -> bool {
        self.composed.as_ref().is_some_and(|c| c.needs_redraw())
    }

    /// The still backdrop never changes between rebuilds, so what the panel
    /// reports is the whole change — never the whole screen.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if let Some(c) = &self.composed {
            c.collect_damage(regions);
        }
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
    /// the inputs and so change the focusable set); plain notice / caps /
    /// session rebuilds leave it `false` so focus + caret are preserved.
    fn take_focus_reset(&mut self) -> bool {
        let mine = std::mem::take(&mut self.focus_reset_pending);
        let child = self.composed.as_mut().is_some_and(|c| c.take_focus_reset());
        mine || child
    }

    fn on_external_event(&mut self, payload: &str, state: &mut AppModel, root: &ModelPath) {
        let (state, _root) = ui::doc::document(state, root);
        if payload == CONSOLE_WAKE {
            self.serve_console(state);
            return;
        }
        // Progress from the background auth worker (see `crate::auth`).
        match AuthUpdate::decode(payload) {
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
            None => {}
        }
    }

    fn next_wake_deadline(&self) -> Option<std::time::Instant> {
        self.composed.as_ref().and_then(|c| c.next_wake_deadline())
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
    use std::collections::HashMap;
    use taffy::AvailableSpace;

    /// The shipped Tilo session plus a second, separately installed one, so
    /// there is a real choice and the picker is in the footer. Human names,
    /// not the `DesktopNames=` id they launch under.
    fn test_sessions() -> Vec<Session> {
        vec![
            Session {
                name: "Tilo".to_string(),
                argv: vec![
                    "/usr/libexec/tilo-session".into(),
                    "--backend".into(),
                    "drm".into(),
                ],
            },
            Session {
                name: "Weston".to_string(),
                argv: vec!["/usr/bin/weston".into()],
            },
        ]
    }

    fn one_session() -> Vec<Session> {
        vec![test_sessions().remove(0)]
    }

    fn quick_user() -> LoginUser {
        LoginUser {
            username: "tester".into(),
            display_name: "ada Quick".into(),
        }
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

    fn key_event(key: Key, text: Option<&str>) -> WindowEvent {
        use winit::keyboard::{KeyLocation, NativeKeyCode, PhysicalKey};

        // winit intentionally keeps the platform-specific key field private;
        // these are the same inert placeholders used by ui::App's key tests.
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = key;
        event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        event.location = KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        event.text = text.map(Into::into);
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event,
            is_synthetic: false,
        }
    }

    fn named_key_event(key: NamedKey) -> WindowEvent {
        key_event(Key::Named(key), None)
    }

    fn char_event(c: &str) -> WindowEvent {
        key_event(Key::Character(c.into()), Some(c))
    }

    fn dispatch_enter(app: &mut GreeterApp, state: &mut AutoCommit, focused: &str) -> bool {
        dispatch(app, state, focused, &named_key_event(NamedKey::Enter))
    }

    fn dispatch(
        app: &mut GreeterApp,
        state: &mut AutoCommit,
        focused: &str,
        event: &WindowEvent,
    ) -> bool {
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

    /// How a widget identifies itself in a tree dump: its `test_id` when it
    /// has one, otherwise its type name. Visible labels are localized, so
    /// they are never the automation address.
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

    fn find<'a>(w: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
        if w.test_id() == Some(id) {
            return Some(w);
        }
        w.children().into_iter().find_map(|child| find(child, id))
    }

    fn accessible_name_for(w: &dyn Widget, id: &str) -> Option<String> {
        find(w, id)
            .and_then(Widget::accessibility)
            .and_then(|n| n.name)
    }

    fn rect_for(w: &dyn Widget, id: &str) -> Option<Rect> {
        find(w, id).map(Widget::get_rect)
    }

    /// The panel's focusable widgets, identified by stable id, in tab order.
    fn focusables(app: &GreeterApp) -> Vec<String> {
        let card = card_of(app);
        let mut v = Vec::new();
        collect_focusables(card.as_ref(), &mut v);
        v
    }

    /// The *shape* of the focus order — one widget type per focusable. The
    /// App restores focus by ordinal after a rebuild, so this is what must
    /// not change across incidental rebuilds.
    fn focus_shape(app: &GreeterApp) -> Vec<String> {
        fn walk(w: &dyn Widget, out: &mut Vec<String>) {
            for child in w.children() {
                if child.is_focusable() {
                    out.push(child.name().to_string());
                }
                walk(child, out);
            }
        }
        let card = card_of(app);
        let mut v = Vec::new();
        walk(card.as_ref(), &mut v);
        v
    }

    /// Every widget under `w` whose `name()` matches, at any depth.
    fn count_named(w: &dyn Widget, name: &str) -> usize {
        w.children()
            .iter()
            .map(|c| usize::from(c.name() == name).saturating_add(count_named(*c, name)))
            .sum()
    }

    /// Everything the panel announces, in tree order: the accessible name of
    /// every node that has one — what a screen reader reads out.
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

    /// A settled greeter laid out the way App does it, kept alive so it can
    /// be relaid out in place after input — exactly the per-keystroke path.
    struct Laid {
        taffy: TaffyTree<()>,
        node: NodeId,
        node_map: HashMap<NodeId, *mut dyn Widget>,
        bindings: ui::bound_dispatch::BoundNodeMap,
        text: TextRenderer,
        viewport: (f32, f32),
    }

    impl Laid {
        fn new(
            app: &mut GreeterApp,
            doc: &mut AutoCommit,
            viewport: (f32, f32),
            theme: &Theme,
        ) -> Self {
            app.sync(doc, &automerge::ROOT);
            let mut text = TextRenderer::new();
            text.set_text_scale(theme.text_scale);
            for _ in 0..8 {
                let mut laid = Self {
                    taffy: TaffyTree::new(),
                    node: NodeId::from(0usize),
                    node_map: HashMap::new(),
                    bindings: ui::bound_dispatch::BoundNodeMap::new(),
                    text,
                    viewport,
                };
                laid.node = app.build_layout(&mut laid.taffy, theme, &mut laid.node_map);
                app.sync(doc, &automerge::ROOT);
                // SAFETY: this helper exclusively owns the matching tree and
                // the app stays in place through admission and the solve.
                unsafe {
                    laid.bindings
                        .rebuild(app, &laid.taffy, laid.node, &laid.node_map);
                }
                laid.relayout(app);
                if !app.take_structure_dirty() {
                    return laid;
                }
                text = laid.text;
            }
            panic!("greeter composition did not settle before paint");
        }

        fn relayout(&mut self, app: &mut GreeterApp) {
            // SAFETY: as above; the tree was built by this helper for `app`.
            unsafe {
                ui::widget::compute_widget_layout(
                    &mut self.taffy,
                    self.node,
                    Size {
                        width: AvailableSpace::Definite(self.viewport.0),
                        height: AvailableSpace::Definite(self.viewport.1),
                    },
                    &self.node_map,
                    &self.bindings,
                    &mut self.text,
                )
            }
            .unwrap();
            app.apply_layout(&self.taffy, self.node, Point::ZERO);
        }

        fn deliver(&self, app: &mut GreeterApp, doc: &mut AutoCommit, event: &WindowEvent) {
            let mut handled = false;
            let mut focus_target = None;
            app.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: doc,
                    path: &automerge::ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: self.node,
                    taffy: &self.taffy,
                },
            );
        }
    }

    fn layout_at_with_theme(app: &mut GreeterApp, viewport: (f32, f32), theme: &Theme) {
        let mut doc = AutoCommit::new();
        let _ = Laid::new(app, &mut doc, viewport, theme);
    }

    fn inside(outer: Rect, inner: Rect) -> bool {
        inner.x0 >= outer.x0 && inner.y0 >= outer.y0 && inner.x1 <= outer.x1 && inner.y1 <= outer.y1
    }

    fn geometry(w: &dyn Widget, out: &mut Vec<Rect>) {
        out.push(w.get_rect());
        for child in w.children() {
            geometry(child, out);
        }
    }

    fn paint(app: &mut GreeterApp, theme: &Theme) {
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        app.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            theme,
            &PresenceCtx::default(),
        );
    }

    fn scene_bytes(app: &mut GreeterApp) -> (Vec<u32>, Vec<u32>) {
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        app.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        let encoding = scene.encoding();
        (
            encoding
                .draw_tags
                .iter()
                .map(|tag| tag.0)
                .collect::<Vec<_>>(),
            encoding.path_data.clone(),
        )
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

    #[test]
    fn a_session_choice_appears_only_when_there_is_one_to_make() {
        // Phase `the_shipped_sessions_start_directly_without_a_picker`.
        // The image installs os/usr/share/wayland-sessions as-is, so this is
        // exactly what the greeter on a fresh install reads.
        {
            let shipped = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../os/usr/share/wayland-sessions");
            let sessions = read_sessions(&shipped);
            let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, ["Tilo"], "the image must ship exactly one session");

            let app = GreeterApp::with_sessions(sessions);
            assert!(!app.offers_session_choice());
            assert!(
                !focusables(&app).contains(&"greeter.session".to_string()),
                "a single session must not render a picker: {:?}",
                focusables(&app)
            );
            assert_eq!(count_named(card_of(&app).as_ref(), "Dropdown"), 0);
            assert_eq!(app.console_state()["session_choice"], false);
            assert_eq!(app.console_state()["sessions"], serde_json::json!(["Tilo"]));
            // Signing in starts that session; there is nothing left to pick.
            assert_eq!(
                app.sessions[app.selected_session].argv,
                ["/usr/libexec/tilo-session", "--backend", "drm"]
            );
        }
        // Phase `two_sessions_put_the_picker_in_the_footer`.
        {
            let app = GreeterApp::with_sessions(test_sessions());
            assert!(app.offers_session_choice());
            assert_eq!(app.console_state()["session_choice"], true);
            assert_eq!(
                focusables(&app),
                vec![
                    "greeter.username",
                    "greeter.password",
                    "greeter.password_reveal",
                    "greeter.login",
                    "greeter.fingerprint",
                    "greeter.session",
                    "greeter.reboot",
                    "greeter.poweroff",
                ],
                "the choice sits after sign-in, beside the power actions"
            );
            assert_eq!(app.session_label(), "Tilo", "the human name, not the id");
        }
        // Phase `picker_selection_drives_the_started_session`.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;

            // Bootstrap seeds the picker with the current session.
            app.sync(&mut doc, &root);
            assert_eq!(app.selected_session, 0);
            assert_eq!(
                read_text(&doc, &root, SESSION_PATH).as_deref(),
                Some("Tilo"),
            );

            choose_session(&mut doc, &root, "Weston");
            app.sync(&mut doc, &root);
            assert_eq!(app.selected_session, 1);
            assert_eq!(
                app.sessions[app.selected_session].argv,
                ["/usr/bin/weston"],
                "the choice must reach the argv handed to greetd"
            );

            // A label that names no known session leaves the choice alone.
            choose_session(&mut doc, &root, "a session that went away");
            app.sync(&mut doc, &root);
            assert_eq!(app.selected_session, 1, "choice survives a stale label");
        }
        // Phase `no_session_is_a_clear_error_not_a_crash`.
        {
            let mut app = GreeterApp::with_sessions_and_login_user(Vec::new(), Some(quick_user()));
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            seed_password(&app, "pw");
            app.attempt_login(&doc, &automerge::ROOT, AuthMethod::Password);
            assert_eq!(app.notice, Notice::Error("greeter-error-no-session"));
            assert_eq!(app.phase, AuthPhase::Idle);
        }
    }

    /// Replace the session picker's CRDT value, as the `Dropdown` does on
    /// selection.
    fn choose_session(doc: &mut AutoCommit, root: &ObjId, name: &str) {
        let (_, obj) = doc.get(root, SESSION_PATH).unwrap().unwrap();
        let old = doc.text(&obj).unwrap_or_default().chars().count();
        let old = isize::try_from(old).expect("count fits isize");
        let _ = doc.splice_text(&obj, 0, old, name);
    }

    fn only_bob_has_a_fingerprint(username: &str) -> bool {
        username == "bob"
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
            let mut app = GreeterApp::with_sessions_login_user_and_fingerprint(
                one_session(),
                Some(quick_user()),
                FingerprintAvailability::Unavailable,
            );
            assert!(
                !focusables(&app).contains(&"greeter.fingerprint".to_string()),
                "an unavailable factor must not appear in the form or focus order"
            );

            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            app.attempt_login(&doc, &automerge::ROOT, AuthMethod::Fingerprint);
            assert_eq!(app.notice, Notice::Hint("greeter-hint-no-fingerprint"));
            assert_eq!(app.phase, AuthPhase::Idle);
        }
        // Phase `multi_account_fingerprint_is_resolved_for_the_typed_username`.
        {
            let mut app = GreeterApp::with_sessions_login_user_and_fingerprint(
                one_session(),
                None,
                FingerprintAvailability::CheckNamedAccount,
            );
            app.fingerprint_probe = only_bob_has_a_fingerprint;

            assert!(focusables(&app).contains(&"greeter.fingerprint".to_string()));
            assert!(app.fingerprint_available_for("bob"));
            assert!(!app.fingerprint_available_for("alice"));
            assert!(!app.fingerprint_available_for(""));
        }
        // Phase `fingerprint_auth_explicitly_uses_no_password`.
        {
            let mut app = GreeterApp::with_sessions(one_session());
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);

            assert_eq!(
                app.take_auth_secret(AuthMethod::Password),
                Err("greeter-hint-passcode")
            );
            assert_eq!(
                app.take_auth_secret(AuthMethod::Fingerprint)
                    .map(|secret| secret.to_string()),
                Ok(String::new()),
                "fingerprint PAM conversation must begin without a dummy password"
            );
        }
        // Phase `fingerprint_prompt_renders_as_feedback`.
        {
            let app = GreeterApp::for_shot(AuthScenario::Fingerprint);
            assert_eq!(app.phase, AuthPhase::Authenticating);
            assert_eq!(app.status_text(), "Place your finger on the reader");
            assert_eq!(
                live_politeness_for(card_of(&app).as_ref(), "Place your finger on the reader"),
                Some(Politeness::Assertive),
                "a time-sensitive PAM instruction must be announced without moving focus"
            );
        }
    }

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
    fn submit_validates_fields_keeps_the_passcode_private_and_ignores_repeats() {
        // Phase `submit_without_username_is_a_hint`.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            let _ = doc.put(&root, SUBMIT_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.notice, Notice::Hint("greeter-hint-username"));
            assert_eq!(app.status_text(), "Enter your username.");
            assert!(!read_bool(&doc, &root, SUBMIT_PATH).unwrap_or(false));
        }
        // Phase `submit_without_password_is_a_hint`.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            let (_, uobj) = doc.get(&root, USERNAME_PATH).unwrap().unwrap();
            let _ = doc.splice_text(&uobj, 0, 0, "alice");
            let _ = doc.put(&root, SUBMIT_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.notice, Notice::Hint("greeter-hint-passcode"));
            assert_eq!(app.phase, AuthPhase::Idle);
        }
        // Phase `without_greetd_the_screen_says_so_plainly`.
        {
            let mut app =
                GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            seed_password(&app, "pw");
            // The test runner is not greetd.
            assert!(std::env::var_os("GREETD_SOCK").is_none());
            app.attempt_login(&doc, &automerge::ROOT, AuthMethod::Password);
            assert_eq!(app.notice, Notice::Error("greeter-error-service"));
            assert!(!app.status_text().contains("GREETD_SOCK"));
        }
        // Phase `a_hint_and_an_error_look_different`.
        {
            let mut hint = GreeterApp::with_sessions(one_session());
            hint.set_hint("greeter-hint-passcode");
            let mut err = GreeterApp::with_sessions(one_session());
            err.finish_failure(Failure::Credentials);
            let icon = |app: &GreeterApp| {
                fn walk(w: &dyn Widget, out: &mut Vec<String>) {
                    for c in w.children() {
                        if c.name() == "IconWidget" {
                            out.push(format!("{c:p}"));
                        }
                        walk(c, out);
                    }
                }
                let mut out = Vec::new();
                walk(card_of(app).as_ref(), &mut out);
                out.len()
            };
            assert_eq!(icon(&hint), 1, "a hint carries its quiet info glyph");
            assert_eq!(icon(&err), 1, "an error carries its alert glyph");
            assert_eq!(hint.status_text(), "Enter your passcode.");
            assert_eq!(
                err.status_text(),
                "Incorrect username or passcode. Try again."
            );
        }
        // Phase `passcode_never_enters_automerge_or_its_saved_history`.
        {
            let app = GreeterApp::with_sessions(one_session());
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
        // Phase `submit_while_busy_is_ignored`.
        for phase in [AuthPhase::Authenticating, AuthPhase::Starting] {
            let mut app = GreeterApp::with_sessions(one_session());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            app.phase = phase;
            let (_, uobj) = doc.get(&root, USERNAME_PATH).unwrap().unwrap();
            let _ = doc.splice_text(&uobj, 0, 0, "alice");
            seed_password(&app, "pw");

            let _ = doc.put(&root, SUBMIT_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.phase, phase);
            assert!(!read_bool(&doc, &root, SUBMIT_PATH).unwrap_or(false));
            assert_eq!(&*app.password_buffer.lock().unwrap(), "pw");
        }
    }

    /// The widget tree must mirror the taffy tree **at every depth** — the
    /// App recurses both in lock-step to build `focusable_order`.
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
        for app in [
            GreeterApp::with_sessions(test_sessions()),
            GreeterApp::for_shot(AuthScenario::PersonalError),
            GreeterApp::for_shot(AuthScenario::PersonalAuthenticating),
        ] {
            let mut app = app;
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let root = app.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
            assert_eq!(app.children().len(), 1);
            assert_trees_mirror(&app, &taffy, root, "GreeterApp");
        }
        // Phase `focusable_order_is_stable_across_idle_state_changes`.
        // On the form, the focusable order must NOT shift when the session
        // changes, a notice appears, or Caps Lock is detected — the
        // precondition for keeping focus and caret glued to the same field.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let base = focus_shape(&app);
            assert_eq!(
                base,
                vec![
                    "TextInput",      // username
                    "TextInput",      // passcode
                    "PasswordReveal", // show/hide passcode
                    "Button",         // sign in
                    "Button",         // fingerprint
                    "Dropdown",       // session
                    "Button",         // restart
                    "Button",         // shut down
                ]
            );
            app.selected_session = 1;
            assert_eq!(focus_shape(&app), base, "changing session keeps order");
            app.notice = Notice::Error("greeter-error-credentials");
            assert_eq!(focus_shape(&app), base, "an error keeps order");
            app.notice = Notice::Hint("greeter-hint-passcode");
            assert_eq!(focus_shape(&app), base, "a hint keeps order too");
            let texts = count_named(card_of(&app).as_ref(), "Text");
            app.caps_lock.observe('A', false);
            assert_eq!(focus_shape(&app), base, "the Caps-Lock note keeps order");
            assert_eq!(
                count_named(card_of(&app).as_ref(), "Text"),
                texts + 1,
                "the Caps-Lock note adds exactly one line"
            );
        }
    }

    #[test]
    fn a_personal_device_greets_the_person_and_asks_only_for_the_passcode() {
        // Phase `sole_local_user_gets_a_one_passcode_form_with_passcode_focus_first`.
        {
            let mut app =
                GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            assert_eq!(
                read_text(&doc, &automerge::ROOT, USERNAME_PATH).as_deref(),
                Some("tester"),
                "the username is seeded without asking the user to type it"
            );
            assert_eq!(
                focusables(&app),
                vec![
                    "greeter.password",
                    "greeter.password_reveal",
                    "greeter.login",
                    "greeter.fingerprint",
                    "greeter.reboot",
                    "greeter.poweroff",
                ],
                "autofocus therefore lands on the passcode field"
            );
            let card = card_of(&app);
            assert_eq!(
                accessible_name_for(card.as_ref(), "greeter.welcome").as_deref(),
                Some("ada Quick"),
                "the person, not the product, gets the headline"
            );
            assert_eq!(
                accessible_name_for(card.as_ref(), "greeter.account").as_deref(),
                Some("tester")
            );
            assert_eq!(
                accessible_name_for(card.as_ref(), "greeter.password").as_deref(),
                Some("Passcode")
            );
            assert_eq!(
                accessible_name_for(card.as_ref(), "greeter.login").as_deref(),
                Some("Sign in"),
                "the icon-only action still has a spoken name"
            );
            assert!(accessible_name_for(card.as_ref(), "greeter.username").is_none());
            assert_eq!(count_named(card.as_ref(), "Avatar"), 1);
        }
        // Phase `one_passcode_autofocus_is_exposed_to_assistive_technology`.
        {
            let greeter =
                GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
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
            let app = GreeterApp::with_sessions_and_login_user(one_session(), None);
            let card = card_of(&app);
            assert_eq!(
                accessible_name_for(card.as_ref(), "greeter.username").as_deref(),
                Some("Username")
            );
            assert_eq!(
                accessible_name_for(card.as_ref(), "greeter.welcome").as_deref(),
                Some("Sign in to Tilo")
            );
            assert_eq!(count_named(card.as_ref(), "Avatar"), 0);
            assert_eq!(
                focusables(&app).first().map(String::as_str),
                Some("greeter.username")
            );
        }
        // Phase `the_person_is_named_before_the_passcode_is_asked`.
        {
            let app = GreeterApp::for_shot(AuthScenario::Personal);
            let said = spoken(card_of(&app).as_ref());
            let person = said.iter().position(|s| s == "ada Example");
            let passcode = said.iter().position(|s| s == "Passcode");
            assert!(person.is_some() && person < passcode, "{said:?}");
        }
    }

    #[test]
    fn enter_signs_in_only_from_text_fields() {
        // Phase `enter_defaults_to_sign_in_from_text_fields`.
        for field in ["greeter.username", "greeter.password"] {
            let mut app = GreeterApp::with_sessions(test_sessions());
            let mut doc = AutoCommit::new();
            assert!(dispatch_enter(&mut app, &mut doc, field));
            assert!(read_bool(&doc, &automerge::ROOT, SUBMIT_PATH).unwrap_or(false));
        }
        // Phase `focused_alternate_controls_own_enter`.
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
                "Enter on {control} must not submit the passcode form"
            );
            if control == "greeter.session" {
                assert!(handled, "the focused dropdown consumes Enter itself");
            } else {
                assert!(!handled, "{control} remains available to App activation");
            }
        }
    }

    #[test]
    fn in_flight_feedback_is_said_once_with_appropriate_urgency() {
        // Phase `signing_in_says_so_beside_a_spinner`.
        {
            let app = GreeterApp::for_shot(AuthScenario::PersonalAuthenticating);
            assert_eq!(app.status_text(), "Signing in…");
            let card = card_of(&app);
            assert_eq!(count_named(card.as_ref(), "Spinner"), 1);
            let said = spoken(card.as_ref());
            assert_eq!(
                said.iter().filter(|s| s.as_str() == "Signing in…").count(),
                1,
                "the in-flight sentence appears exactly once: {said:?}"
            );
        }
        // Phase `feedback_is_a_live_region_with_appropriate_urgency`.
        for (scenario, message, expected) in [
            (
                AuthScenario::PersonalHint,
                "Enter your passcode.",
                Politeness::Assertive,
            ),
            (
                AuthScenario::PersonalError,
                "Incorrect passcode. Try again.",
                Politeness::Assertive,
            ),
            (
                AuthScenario::PersonalStarting,
                "Starting your session…",
                Politeness::Polite,
            ),
        ] {
            let app = GreeterApp::for_shot(scenario);
            assert_eq!(
                live_politeness_for(card_of(&app).as_ref(), message),
                Some(expected),
                "{scenario:?} feedback must be announced at the right urgency"
            );
        }
        // Phase `worker_prompt_update_reflects_in_feedback`.
        {
            let mut app = GreeterApp::with_sessions(one_session());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            app.begin_authenticating();
            assert_eq!(app.status_text(), "Signing in…");
            let payload = AuthUpdate::Prompt {
                text: "Place your finger on the reader".to_string(),
                is_error: false,
            }
            .encode();
            app.on_external_event(&payload, &mut doc, &root);
            assert_eq!(app.phase, AuthPhase::Authenticating);
            assert_eq!(app.status_text(), "Place your finger on the reader");
        }
        // Phase `accepted_resolves_into_a_visible_success_state`.
        {
            let mut app = GreeterApp::with_sessions(one_session());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            app.begin_authenticating();
            app.on_external_event(&AuthUpdate::Accepted.encode(), &mut doc, &root);
            assert_eq!(app.phase, AuthPhase::Starting);
            assert!(app.phase.is_busy(), "inputs stay inert while handing over");
            assert_eq!(app.status_text(), "Starting your session…");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one table row per failure, then the same ready-form checks for each"
    )]
    fn every_failure_reads_as_a_clear_sentence_and_returns_to_a_ready_form() {
        let cases = [
            (Failure::Credentials, true, "Incorrect passcode. Try again."),
            (
                Failure::Credentials,
                false,
                "Incorrect username or passcode. Try again.",
            ),
            (
                Failure::Locked,
                true,
                "Too many failed attempts. Wait a few minutes, then try again.",
            ),
            (
                Failure::AccountExpired,
                true,
                "This account has expired. Ask an administrator to renew it.",
            ),
            (
                Failure::PasscodeExpired,
                true,
                "Your passcode has expired. Ask an administrator to reset it.",
            ),
            (
                Failure::NotPermitted,
                true,
                "This account can't sign in on this computer.",
            ),
            (
                Failure::Unavailable,
                true,
                "Your passcode couldn't be checked. Try again.",
            ),
            (
                Failure::SessionStart,
                true,
                "Your desktop couldn't start. Try again, or restart the computer.",
            ),
            (
                Failure::Service,
                true,
                "The sign-in service isn't responding. Restart the computer to try again.",
            ),
        ];
        for (failure, personal, sentence) in cases {
            let mut app =
                GreeterApp::with_sessions_and_login_user(one_session(), personal.then(quick_user));
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            if !personal {
                let (_, uobj) = doc.get(&root, USERNAME_PATH).unwrap().unwrap();
                let _ = doc.splice_text(&uobj, 0, 0, "alice");
            }
            seed_password(&app, "wrongpw");
            app.begin_authenticating();
            let _ = Widget::take_focus_reset(&mut app);

            app.on_external_event(&AuthUpdate::Failed(failure).encode(), &mut doc, &root);

            assert_eq!(
                app.phase,
                AuthPhase::Idle,
                "{failure:?} returns to the form"
            );
            assert_eq!(app.status_text(), sentence, "{failure:?}");
            assert!(matches!(app.notice, Notice::Error(_)));
            assert!(
                app.password_buffer.lock().unwrap().is_empty(),
                "{failure:?} clears the passcode"
            );
            if !personal {
                assert_eq!(
                    read_text(&doc, &root, USERNAME_PATH).as_deref(),
                    Some("alice"),
                    "the username survives a failure"
                );
            }
            assert!(
                Widget::take_focus_reset(&mut app),
                "{failure:?} puts the keyboard back on the form"
            );
            let card = card_of(&app);
            assert_eq!(
                live_politeness_for(card.as_ref(), sentence),
                Some(Politeness::Assertive)
            );
            assert!(
                focusables(&app).contains(&"greeter.password".to_string()),
                "the passcode field is editable again"
            );
        }
        // Phase `focus_reset_after_failure_lands_on_the_passcode_field`.
        {
            let mut greeter =
                GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
            greeter.begin_authenticating();
            greeter.finish_failure(Failure::Credentials);
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
            assert_eq!(focused, Some("Passcode"));
        }
    }

    #[test]
    fn authentication_and_power_actions_lock_input_and_reset_focus() {
        // Phase `authenticating_disables_inputs_and_requests_focus_reset`.
        {
            let mut app = GreeterApp::with_sessions(test_sessions());
            app.begin_authenticating();
            assert_eq!(
                focusables(&app),
                vec!["greeter.login", "greeter.reboot", "greeter.poweroff"],
                "the sign-in action stays the first focusable during auth; power stays reachable"
            );
            assert!(Widget::take_focus_reset(&mut app));
            assert!(
                !Widget::take_focus_reset(&mut app),
                "focus reset is one-shot"
            );
        }
        // Phase `authenticating_focus_reset_lands_on_the_sign_in_action`.
        {
            let mut greeter = GreeterApp::with_sessions(one_session());
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
            assert_eq!(focused, Some("Sign in"));
        }
        // Phase `power_flag_rising_edge_resets_and_records_action`.
        {
            let mut app = GreeterApp::with_sessions(one_session());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            let _ = doc.put(&root, REBOOT_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.last_power_action, Some(PowerAction::Reboot));
            assert!(!read_bool(&doc, &root, REBOOT_PATH).unwrap_or(false));
            let _ = doc.put(&root, POWEROFF_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.last_power_action, Some(PowerAction::PowerOff));
            assert!(!read_bool(&doc, &root, POWEROFF_PATH).unwrap_or(false));
        }
    }

    #[test]
    fn the_backdrop_is_still_and_asks_for_no_frames() {
        for scenario in [
            AuthScenario::Personal,
            AuthScenario::PersonalError,
            AuthScenario::Form,
            AuthScenario::PersonalHint,
        ] {
            let mut app = GreeterApp::for_shot(scenario);
            layout_at_with_theme(&mut app, (1472.0, 920.0), &Theme::dark());
            assert_eq!(
                app.next_wake_deadline(),
                None,
                "{scenario:?} must let the loop sleep"
            );
            assert!(
                !app.needs_animation_frame(),
                "{scenario:?} must not animate"
            );
            let first = scene_bytes(&mut app);
            std::thread::sleep(std::time::Duration::from_millis(20));
            let later = scene_bytes(&mut app);
            assert!(
                first == later,
                "{scenario:?} painted a different frame later"
            );
        }
    }

    /// The native host repaints a widget's reported damage and nothing else
    /// unless geometry moved. Typing into the passcode field must therefore
    /// leave every layout box where it was and report damage confined to the
    /// field's row, whatever the display size.
    #[test]
    fn a_keystroke_repaints_the_passcode_row_not_the_screen() {
        for (viewport, theme) in [
            ((2944.0, 1840.0), Theme::dark()),
            ((1472.0, 920.0), Theme::dark()),
            ((1280.0, 800.0), Theme::light()),
            ((640.0, 480.0), Theme::dark().with_text_scale(2.0)),
        ] {
            let mut app =
                GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
            let mut doc = AutoCommit::new();
            let mut laid = Laid::new(&mut app, &mut doc, viewport, &theme);
            set_focus_by_id(&mut app, "greeter.password");
            // The first frame paints everything; what counts is every frame
            // after it.
            paint(&mut app, &theme);
            Widget::mark_drawn(&mut app);
            let card = app.composed.as_deref().expect("composed");
            let row = find(card, "greeter.password")
                .map(Widget::get_rect)
                .expect("passcode field")
                .union(rect_for(card, "greeter.login").expect("sign-in action"));
            let mut before = Vec::new();
            geometry(&app, &mut before);

            for c in ["h", "u", "n", "t", "e", "r", "2"] {
                laid.deliver(&mut app, &mut doc, &char_event(c));
                app.sync(&mut doc, &automerge::ROOT);
                assert!(
                    !app.take_structure_dirty(),
                    "a keystroke at {viewport:?} rebuilt the screen"
                );
                laid.relayout(&mut app);
                let mut after = Vec::new();
                geometry(&app, &mut after);
                assert!(before == after, "a keystroke moved layout at {viewport:?}");

                // As the native host does: paint, then collect what changed.
                paint(&mut app, &theme);
                let mut damage = Vec::new();
                app.collect_damage(&mut damage);
                assert!(!damage.is_empty(), "the typed character must be painted");
                let area: f64 = damage.iter().map(Rect::area).sum();
                let screen = f64::from(viewport.0) * f64::from(viewport.1);
                // On a laptop panel the field is a sliver of the screen; on a
                // tiny large-text display it is the field, and nothing more.
                let bound = if viewport.0 >= 1280.0 { 0.02 } else { 0.1 };
                assert!(
                    area <= screen * bound,
                    "a keystroke at {viewport:?} damaged {:.1}% of the screen: {damage:?}",
                    area / screen * 100.0
                );
                for rect in &damage {
                    assert!(
                        inside(row.inflate(2.0, 2.0), *rect),
                        "damage {rect:?} escapes the passcode row {row:?}"
                    );
                }
                Widget::mark_drawn(&mut app);
            }
            assert_eq!(&*app.password_buffer.lock().unwrap(), "hunter2");
            assert!(
                doc.get(&automerge::ROOT, "greeter.password")
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn the_form_stays_on_screen_at_every_size_and_text_scale() {
        for (viewport, scale) in [
            ((2944.0, 1840.0), 1.0),
            ((1472.0, 920.0), 1.0),
            ((1280.0, 800.0), 1.0),
            ((800.0, 600.0), 1.5),
            ((640.0, 480.0), 2.0),
            ((360.0, 640.0), 1.0),
        ] {
            for scenario in [AuthScenario::PersonalError, AuthScenario::Error] {
                let mut app = GreeterApp::for_shot(scenario);
                app.sessions = test_sessions();
                let theme = Theme::dark().with_text_scale(scale);
                layout_at_with_theme(&mut app, viewport, &theme);
                let screen = Rect::new(0.0, 0.0, f64::from(viewport.0), f64::from(viewport.1));
                let card = app.composed.as_deref().expect("composed");
                for id in [
                    "greeter.password",
                    "greeter.password_reveal",
                    "greeter.login",
                    "greeter.session",
                    "greeter.reboot",
                    "greeter.poweroff",
                ] {
                    let rect = rect_for(card, id)
                        .unwrap_or_else(|| panic!("{id} missing at {viewport:?}x{scale}"));
                    assert!(
                        rect.width() > 0.0 && inside(screen, rect),
                        "{id} {rect:?} is off screen at {viewport:?} x{scale} ({scenario:?})"
                    );
                }
                let field = rect_for(card, "greeter.password").unwrap();
                let action = rect_for(card, "greeter.login").unwrap();
                assert!(
                    (field.center().y - action.center().y).abs() < 1.0 && action.x0 >= field.x1,
                    "the sign-in action sits beside the field at {viewport:?}: {field:?} {action:?}"
                );
            }
        }
    }

    #[test]
    fn copy_uses_words_not_symbols() {
        // The screen says things in words and draws icons with the toolkit's
        // icon set; no emoji or pictographic characters stand in for either.
        for source in [
            include_str!("../locales/en-US.ftl"),
            include_str!("../locales/de-DE.ftl"),
        ] {
            for (key, text) in source
                .lines()
                .filter(|line| !line.starts_with('#'))
                .filter_map(|line| line.split_once(" = "))
            {
                assert!(
                    text.chars().all(|c| c.is_alphanumeric()
                        || c.is_whitespace()
                        || c.is_ascii_punctuation()
                        || c == '\u{2026}'
                        || c == '\u{2019}'),
                    "{key} draws a symbol: {text:?}"
                );
            }
        }
        for key in crate::i18n::KEYS {
            assert_ne!(t(key), *key, "{key} must resolve");
        }
    }

    #[test]
    fn shot_scenarios_compose() {
        for scenario in [
            AuthScenario::Form,
            AuthScenario::Error,
            AuthScenario::Authenticating,
            AuthScenario::Fingerprint,
            AuthScenario::CapsLock,
            AuthScenario::Hint,
            AuthScenario::Starting,
            AuthScenario::LongName,
            AuthScenario::Personal,
            AuthScenario::PersonalError,
            AuthScenario::PersonalAuthenticating,
            AuthScenario::PersonalCapsLock,
            AuthScenario::PersonalHint,
            AuthScenario::PersonalStarting,
            AuthScenario::PersonalNoFingerprint,
            AuthScenario::PersonalLocked,
            AuthScenario::PersonalSessionFailed,
            AuthScenario::PersonalServiceDown,
            AuthScenario::PersonalPasscodeExpired,
            AuthScenario::PersonalSessions,
        ] {
            let mut app = GreeterApp::for_shot(scenario);
            layout_at_with_theme(&mut app, (1280.0, 800.0), &Theme::dark());
            assert!(app.composed.is_some(), "{scenario:?} composes");
        }
    }
}

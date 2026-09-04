//! [`GreeterApp`] — the root login widget.
//!
//! Structured like the other `ui`-toolkit composed apps (file-explorer /
//! browser): a thin root widget that owns a single composed child (the
//! [`Card`]) and delegates the five `Widget` passes into it. The card is a
//! fixed-width column holding a title, username + password inputs, a
//! session-cycle button, a Log-in button, and a status line.
//!
//! IMPORTANT (focus): the App discovers focusable widgets by walking the
//! widget tree (`Widget::children`) in lock-step with the taffy tree
//! (`taffy.children`). Any intermediate taffy node MUST have a matching
//! widget in `children()`, or the recursion derails and inner widgets
//! (the `TextInputs`) never enter `focusable_order` — breaking Tab / arrow
//! navigation. That's why the card is a real [`Card`] widget rather than a
//! bare taffy node owned directly by `GreeterApp`: the structure stays
//! `GreeterApp -> Card -> [fields]`, mirroring the taffy tree exactly.
//!
//! State lives in the CRDT (the toolkit convention):
//!   * `greeter.username` — `Text`
//!   * `greeter.password` — `Text` (rendered masked; value stays real)
//!   * `greeter.submit`   — `bool` flag (Log-in / Enter rising edge)
//!   * `greeter.fingerprint` — `bool` flag (explicit passwordless PAM attempt)
//!   * `greeter.session`  — `Text` (the chosen session's label)

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use automerge::{
    AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value, transaction::Transactable,
};
use taffy::style_helpers::{length, percent};
use taffy::{
    AlignItems, Dimension, Display, FlexDirection, JustifyContent, NodeId, Size, Style, TaffyTree,
};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, ColorStop, Extend, Fill, Gradient};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, NamedKey};

use ui::a11y::Politeness;
use ui::app::UserEvent;
use ui::caps_lock::CapsLockEstimate;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    Button, Divider, Dropdown, IconWidget, Row, Scrollable, Spacer, Spinner, Text, TextInput,
    Widget,
};

use crate::auth::{AuthUpdate, spawn_auth};
use crate::local_users::{LoginUser, discover_default_login_user};
use crate::sessions::{DEFAULT_SESSIONS_DIR, Session, read_sessions};

const USERNAME_PATH: &str = "greeter.username";
const PASSWORD_PATH: &str = "greeter.password";
const SUBMIT_PATH: &str = "greeter.submit";
const FINGERPRINT_PATH: &str = "greeter.fingerprint";
/// The session picker's chosen label, as a CRDT `Text`. The picker writes
/// its selection here; [`GreeterApp::sync`] mirrors it back onto the
/// selected index.
const SESSION_PATH: &str = "greeter.session";
/// Rising-edge boolean flags raised by the secondary power-action buttons.
/// Consumed in [`GreeterApp::sync`], which routes them through logind via
/// `systemctl reboot` / `systemctl poweroff` (authorised for the `greetd`
/// user by os/etc/polkit-1/rules.d/49-qos-greeter-power.rules).
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
/// Space between the card's top-level groups (brand, each field, the
/// primary action, the status row, the power strip).
const CARD_GAP: f32 = 12.0;
/// Gap between a field's label and the field itself. Tight enough that the
/// pair reads as one unit rather than two stacked rows.
const LABEL_GAP: f32 = 7.0;
/// Height of the primary action.
const ACTION_HEIGHT: f32 = 44.0;
/// Height of a secondary power action. Deliberately smaller than
/// [`ACTION_HEIGHT`] so "Shut down" can never be mistaken for the thing you
/// came to this screen to press.
const POWER_HEIGHT: f32 = 30.0;
/// The status row is reserved at this height in *every* state, so a message
/// appearing — or a spinner replacing it — never resizes the card. Without
/// the reservation the whole form jumps vertically the moment you get your
/// password wrong, which is exactly when you are least able to absorb it.
const STATUS_HEIGHT: f32 = 20.0;
/// Space between the primary action and its status line. Tighter than
/// [`CARD_GAP`], so the feedback reads as belonging to the button you just
/// pressed rather than floating loose in the card.
const ACTION_STATUS_GAP: f32 = 10.0;
/// Space between lines in the compact welcome lockup.
const BRAND_GAP: f32 = 5.0;

/// A vertical stack with an explicit gap and optional padding, fixed width,
/// minimum height, and card chrome (surface + border + elevation shadow).
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
    /// Paint the card surface: elevation shadow, fill, and hairline border.
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
            chrome: false,
            rect: Rect::ZERO,
        }
    }

    fn with_min_height(mut self, px: f32) -> Self {
        self.min_height = Some(px);
        self
    }
}

impl Widget for VStack {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        for child in &mut self.children {
            child.sync(state, root);
        }
    }

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
            let shape = RoundedRect::from_rect(self.rect, f64::from(tokens.radii.lg) * 1.5);
            // Lift the card off the backdrop so it reads as a surface rather
            // than a lighter rectangle painted onto the wallpaper.
            ui::surface::elevation(scene, transform, self.rect, &tokens.elevation.e4);
            scene.fill(
                Fill::NonZero,
                transform,
                tokens.colors.surfaces.raised,
                None,
                &shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                tokens.colors.border_white_10,
                None,
                &shape,
            );
        }
        for child in &mut self.children {
            child.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let Ok(child_nodes) = taffy.children(node_id) else {
            return;
        };
        if let Some(idx) = self.children.iter().position(|c| c.wants_pointer_capture()) {
            if let Some(&cnode) = child_nodes.get(idx)
                && let Some(child) = self.children.get_mut(idx)
            {
                child.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    root,
                    handled,
                    focus_target,
                    cnode,
                    taffy,
                );
            }
            return;
        }
        for (child, &child_node) in self.children.iter_mut().zip(child_nodes.iter()) {
            child.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                root,
                handled,
                focus_target,
                child_node,
                taffy,
            );
            if *handled {
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

    fn needs_commit(&self) -> bool {
        self.children.iter().any(|c| c.needs_commit())
    }

    fn on_commit(&mut self) {
        for child in &mut self.children {
            child.on_commit();
        }
    }

    fn name(&self) -> &'static str {
        "GreeterStack"
    }

    fn get_rect(&self) -> Rect {
        self.rect
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
    /// because [`GreeterApp::attempt_login`] refuses a second attempt, not
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
/// logind via `systemctl` — see [`GreeterApp::run_power`].
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
    let Ok(output) = std::process::Command::new("fprintd-list")
        .env("LC_ALL", "C")
        .arg(username)
        .output()
    else {
        return false;
    };
    output.status.success()
        && fingerprint_list_has_enrollment(&String::from_utf8_lossy(&output.stdout))
}

pub struct GreeterApp {
    sessions: Vec<Session>,
    selected_session: usize,
    /// The sole eligible interactive local account, when discovery was
    /// unambiguous. Its username is fixed and the form asks only for the
    /// passcode; zero/multiple candidates retain the editable username field.
    login_user: Option<LoginUser>,
    /// Whether the fixed local account has at least one enrolled fingerprint.
    /// False on every discovery or probe error, and whenever the username is
    /// editable: the greeter must not advertise a factor it cannot identify.
    fingerprint: FingerprintAvailability,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FingerprintAvailability {
    Unavailable,
    Enrolled,
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

impl GreeterApp {
    /// Construct a greeter reading sessions from the standard
    /// `/usr/share/wayland-sessions` directory.
    pub fn new() -> Self {
        let login_user = discover_default_login_user();
        let fingerprint_available = login_user
            .as_ref()
            .is_some_and(|user| enrolled_fingerprint(&user.username));
        Self::with_sessions_login_user_and_fingerprint(
            read_sessions(std::path::Path::new(DEFAULT_SESSIONS_DIR)),
            login_user,
            fingerprint_available,
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
        Self::with_sessions_login_user_and_fingerprint(sessions, login_user, true)
    }

    fn with_sessions_login_user_and_fingerprint(
        sessions: Vec<Session>,
        login_user: Option<LoginUser>,
        fingerprint_available: bool,
    ) -> Self {
        Self {
            sessions,
            selected_session: 0,
            login_user,
            fingerprint: fingerprint_available.into(),
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
        // `QOS_GREETER_SESSIONS_DIR`, so the session picker can be captured
        // on a machine whose real sessions directory holds a single entry.
        // Deliberately confined to this snapshot constructor:
        // `GreeterApp::new` — the live path — always reads the system
        // location, so nothing in the environment can redirect what a real
        // login is offered.
        let dir = std::env::var("QOS_GREETER_SESSIONS_DIR")
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
            username: "quinten".to_string(),
            display_name: "Quinten Example".to_string(),
        });
        // Live discovery only probes enrollment after it has resolved one
        // unambiguous local account. Keep shot fixtures inside that same
        // capability contract: a fallback username form cannot truthfully
        // advertise an account-specific enrolled print.
        let fingerprint_available =
            personal.is_some() && !matches!(scenario, AuthScenario::PersonalNoFingerprint);
        let mut app = Self::with_sessions_login_user_and_fingerprint(
            sessions,
            personal,
            fingerprint_available,
        );
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

    /// Label for the session picker — the session's own human-readable
    /// name, never the internal desktop id it is launched under. The id
    /// (`nirios`) is a `DesktopNames=` contract with the portal; it is not
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
        if self.fingerprint == FingerprintAvailability::Enrolled && !busy {
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
        VStack::new(ACTION_STATUS_GAP, actions)
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
    fn build_brand(&self, theme: &Theme) -> Box<dyn Widget> {
        let c = &theme.tokens.colors;
        if let Some(user) = &self.login_user {
            let compact_for_large_text = theme.text_scale >= 1.75;
            let caption = if compact_for_large_text {
                Text::new(&user.username)
                    .with_id("greeter.account")
                    .with_size(13.0)
                    .with_color(c.text_dim)
            } else {
                Text::new("Enter your passcode to open your desktop.")
                    .with_size(13.0)
                    .with_color(c.text_dim)
            };
            Box::new(VStack::new(
                BRAND_GAP,
                vec![
                    Box::new(
                        Text::new("qOS desktop")
                            .with_role(ui::tokens::typography::TextRole::Label)
                            .with_color(c.text_dim),
                    ),
                    Box::new(
                        Text::new(&format!("Welcome back, {}", user.display_name))
                            .with_id("greeter.welcome")
                            .with_size(28.0),
                    ),
                    Box::new(caption),
                ],
            ))
        } else {
            Box::new(VStack::new(
                BRAND_GAP,
                vec![
                    Box::new(Text::new("qOS").with_size(38.0)),
                    Box::new(
                        Text::new("Sign in to continue")
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

        // --- brand -------------------------------------------------------
        children.push(self.build_brand(theme));

        // --- credentials -------------------------------------------------
        // Persistent labels above each field. Two unlabelled rounded boxes
        // leave the user guessing which one is which; a placeholder alone
        // disappears the moment they start typing.
        // At the maximum supported text scale, fold the already-known local
        // username into the welcome lockup (see `build_brand`). This preserves
        // account context while keeping both authentication actions above the
        // fixed power footer on a 640x480 logical display.
        if self.login_user.is_none() || theme.text_scale < 1.75 {
            children.push(self.build_account_field(busy, theme));
        }

        let mut password_group: Vec<Box<dyn Widget>> = vec![
            Box::new(Self::field_label("Passcode", theme)),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("greeter.password")
                    .with_on_change_path(PASSWORD_PATH)
                    .with_accessible_name("Passcode")
                    .with_secret(true)
                    .with_placeholder("Your passcode")
                    .disabled(busy),
            ),
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
        children.push(Box::new(VStack::new(LABEL_GAP, password_group)));

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
            let options: Vec<String> = self.sessions.iter().map(|s| s.name.clone()).collect();
            children.push(Box::new(VStack::new(
                LABEL_GAP,
                vec![
                    Box::new(Self::field_label("Session", theme)),
                    Box::new(
                        Dropdown::new(options, SESSION_PATH)
                            .with_id("greeter.session")
                            .with_enabled(!busy),
                    ),
                ],
            )));
        }

        let card: Box<dyn Widget> = Box::new(VStack::card(CARD_GAP, children));
        let card_view: Box<dyn Widget> = Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(Box::new(VStack::viewport(card)))
                .with_persisted_offset(self.viewport_offset.clone())
                .always_visible(),
        );
        let power: Box<dyn Widget> = Box::new(
            Row::new()
                .push(Box::new(Spacer::new()))
                .push(Box::new(
                    Button::new("greeter.reboot", "Restart", Some(REBOOT_PATH))
                        .ghost()
                        .with_icon(Icon::Refresh)
                        .with_size(100.0, POWER_HEIGHT),
                ))
                .push(Box::new(
                    Button::new("greeter.poweroff", "Shut down", Some(POWEROFF_PATH))
                        .ghost()
                        .with_icon(Icon::Power)
                        .with_size(116.0, POWER_HEIGHT),
                )),
        );
        Box::new(VStack::shell(vec![
            card_view,
            Box::new(Divider::horizontal().with_color(c.border_white_10)),
            power,
        ]))
    }

    fn bootstrap_crdt(&self, state: &mut AutoCommit, root: &ObjId) {
        if state.get(root, USERNAME_PATH).ok().flatten().is_none() {
            let username = state.put_object(root, USERNAME_PATH, ObjType::Text).ok();
            if let (Some(username), Some(user)) = (username, self.login_user.as_ref()) {
                let _ = state.splice_text(&username, 0, 0, &user.username);
            }
        }
        if state.get(root, PASSWORD_PATH).ok().flatten().is_none() {
            let _ = state.put_object(root, PASSWORD_PATH, ObjType::Text);
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
    /// through os/etc/polkit-1/rules.d/49-qos-greeter-power.rules (logind
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

    fn read_text(state: &AutoCommit, root: &ObjId, path: &str) -> Option<String> {
        match state.get(root, path) {
            Ok(Some((Value::Object(ObjType::Text), obj_id))) => state.text(&obj_id).ok(),
            _ => None,
        }
    }

    fn auth_secret(
        state: &AutoCommit,
        root: &ObjId,
        method: AuthMethod,
    ) -> Result<String, &'static str> {
        match method {
            AuthMethod::Password => {
                let password = Self::read_text(state, root, PASSWORD_PATH).unwrap_or_default();
                if password.is_empty() {
                    Err("Enter your passcode")
                } else {
                    Ok(password)
                }
            }
            AuthMethod::Fingerprint => Ok(String::new()),
        }
    }

    fn read_flag(state: &AutoCommit, root: &ObjId, path: &str) -> bool {
        matches!(
            state.get(root, path),
            Ok(Some((Value::Scalar(s), _)))
                if matches!(s.as_ref(), ScalarValue::Boolean(true))
        )
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
    /// `DesktopNames=nirios` advertised by the session `.desktop` files.
    fn session_env() -> Vec<String> {
        vec![
            "XDG_SESSION_TYPE=wayland".to_string(),
            "XDG_SESSION_DESKTOP=nirios".to_string(),
            "XDG_CURRENT_DESKTOP=nirios".to_string(),
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
            && self.fingerprint != FingerprintAvailability::Enrolled
        {
            self.set_status("No enrolled fingerprint is available for this account");
            return;
        }

        let username = Self::read_text(state, root, USERNAME_PATH).unwrap_or_default();

        if username.trim().is_empty() {
            self.set_status("Enter your username");
            return;
        }
        // An explicit fingerprint attempt must not copy or submit a password
        // that happens to remain in the field. PAM/fprintd owns the factor
        // conversation and receives an empty response for secret prompts.
        let password = match Self::auth_secret(state, root, method) {
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
                &password,
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
        state: &mut AutoCommit,
        root: &ObjId,
    ) {
        self.phase = AuthPhase::Idle;
        self.prompt.clear();
        self.status = message.to_string();
        self.status_kind = StatusKind::Error;
        self.detail = detail.unwrap_or_default();
        Self::clear_password(state, root);
        self.dirty_structure = true;
        // Inputs are re-enabled (back in the focus order) — re-seat focus on
        // the rebuilt form rather than a stale auth-phase ordinal.
        self.focus_reset_pending = true;
    }

    /// Empty the password CRDT Text object so the field clears on the next
    /// sync. The username is intentionally left untouched.
    fn clear_password(state: &mut AutoCommit, root: &ObjId) {
        if let Ok(Some((Value::Object(ObjType::Text), obj))) = state.get(root, PASSWORD_PATH)
            && let Ok(text) = state.text(&obj)
        {
            let chars = text.chars().count();
            if chars > 0 {
                let delete = isize::try_from(chars).unwrap_or(isize::MAX);
                let _ = state.splice_text(&obj, 0, delete, "");
            }
        }
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

impl Widget for GreeterApp {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !self.bootstrap_done {
            self.bootstrap_crdt(state, root);
            self.bootstrap_done = true;
        }

        // The session picker writes its chosen label into the document;
        // mirror that back onto the selected index. An unrecognised label
        // (a session that vanished between renders) is ignored rather than
        // resetting the choice out from under the user.
        if let Some(name) = Self::read_text(state, root, SESSION_PATH)
            && let Some(idx) = self.sessions.iter().position(|s| s.name == name)
            && idx != self.selected_session
        {
            self.selected_session = idx;
            self.dirty_structure = true;
        }

        // Submit rising edge — reset the flag *before* attempting so a
        // single click fires exactly one attempt.
        if Self::read_flag(state, root, SUBMIT_PATH) {
            let _ = state.put(root, SUBMIT_PATH, false);
            self.attempt_login(state, root, AuthMethod::Password);
        }

        if Self::read_flag(state, root, FINGERPRINT_PATH) {
            let _ = state.put(root, FINGERPRINT_PATH, false);
            self.attempt_login(state, root, AuthMethod::Fingerprint);
        }

        // Power-action rising edges. Reset the flag first so a single click
        // fires exactly one request.
        if Self::read_flag(state, root, REBOOT_PATH) {
            let _ = state.put(root, REBOOT_PATH, false);
            self.request_power(PowerAction::Reboot);
        }
        if Self::read_flag(state, root, POWEROFF_PATH) {
            let _ = state.put(root, POWEROFF_PATH, false);
            self.request_power(PowerAction::PowerOff);
        }

        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
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
        // Backdrop: the app ground, then a soft accent wash falling from
        // the top edge. Both are theme tokens, so the greeter inherits the
        // OS palette — and the high-contrast track — instead of pinning its
        // own colours the way a login screen usually does.
        scene.fill(Fill::NonZero, transform, theme.bg_app, None, &self.rect);
        if self.rect.height() > 0.0 {
            let a = theme.tokens.colors.accent[5].to_rgba8();
            let wash = Gradient::new_linear(
                Point::new(self.rect.x0, self.rect.y0),
                Point::new(self.rect.x0, self.rect.y0 + self.rect.height() * 0.7),
            )
            .with_extend(Extend::Pad)
            .with_stops(
                &[
                    ColorStop {
                        offset: 0.0,
                        color: Color::from_rgba8(a.r, a.g, a.b, 30).into(),
                    },
                    ColorStop {
                        offset: 1.0,
                        color: Color::from_rgba8(a.r, a.g, a.b, 0).into(),
                    },
                ][..],
            );
            scene.fill(Fill::NonZero, transform, &wash, None, &self.rect);

            // Echo the trusted installer/OOBE backdrop with two restrained
            // accent fields. Their low alpha keeps the passcode card dominant
            // and remains safe in high-contrast themes because the hue comes
            // from the active theme rather than a hard-coded brand colour.
            let accent_soft = Color::from_rgba8(a.r, a.g, a.b, 18);
            let radius = self.rect.width().min(self.rect.height()) * 0.34;
            let upper = Circle::new(
                Point::new(self.rect.x0 - radius * 0.25, self.rect.y0 - radius * 0.2),
                radius,
            );
            let lower = Circle::new(
                Point::new(self.rect.x1 + radius * 0.45, self.rect.y1 + radius * 0.25),
                radius * 1.15,
            );
            scene.fill(Fill::NonZero, transform, accent_soft, None, &upper);
            scene.fill(Fill::NonZero, transform, accent_soft, None, &lower);
        }
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
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
                .observe_text(t.as_str(), modifiers.state().shift_key());
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
            && let Ok(children) = taffy.children(node_id)
            && let Some(&card_node) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                root,
                handled,
                focus_target,
                card_node,
                taffy,
            );
        }

        // A focused control owns Enter. Dropdown consumes it above; buttons
        // deliberately leave it for App's standard focused-control activation.
        // Only an otherwise-unhandled Enter (normally from either text field)
        // acts as the form's default Log-in action.
        if enter_pressed && !*handled && !self.focused_button_present() {
            let _ = state.put(root, SUBMIT_PATH, true);
            *handled = true;
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

    fn on_external_event(&mut self, payload: &str, state: &mut AutoCommit, root: &ObjId) {
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

    fn needs_commit(&self) -> bool {
        self.composed.as_ref().is_some_and(|c| c.needs_commit())
    }

    fn on_commit(&mut self) {
        if let Some(c) = &mut self.composed {
            c.on_commit();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The two entries shipped in os/usr/share/wayland-sessions/ — human
    /// names, not the `DesktopNames=` id they launch under.
    fn test_sessions() -> Vec<Session> {
        vec![
            Session {
                name: "qOS".to_string(),
                argv: vec!["/usr/bin/shell".into(), "--backend".into(), "drm".into()],
            },
            Session {
                name: "qOS with peer sync".to_string(),
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
            &enter_event(),
            1.0,
            &winit::event::Modifiers::default(),
            state,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        handled
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
        let mut taffy = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = app.build_layout(&mut taffy, theme, &mut node_map);
        let mut text = TextRenderer::new();
        taffy
            .compute_layout_with_measure(
                node,
                Size {
                    width: AvailableSpace::Definite(viewport.0),
                    height: AvailableSpace::Definite(viewport.1),
                },
                |known, available, node_id, _, _| {
                    node_map.get(&node_id).map_or(Size::ZERO, |&ptr| unsafe {
                        (*ptr).measure(known, available, &mut text)
                    })
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, Point::ZERO);
        (taffy, node)
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
    fn session_label_reflects_selection() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        assert_eq!(app.session_label(), "qOS");
        app.selected_session = 1;
        assert_eq!(app.session_label(), "qOS with peer sync");
    }

    #[test]
    fn fingerprint_enrollment_output_fails_closed() {
        assert!(fingerprint_list_has_enrollment(
            "Fingerprints for user quickuser:\n - #0: right-index-finger\n"
        ));
        assert!(fingerprint_list_has_enrollment(
            "Fingerprints for user quickuser:\n - right-index-finger\n"
        ));
        assert!(!fingerprint_list_has_enrollment(
            "Fingerprints for user quickuser:\nnone\n"
        ));
        assert!(!fingerprint_list_has_enrollment("right-index-finger"));
    }

    #[test]
    fn fingerprint_action_is_hidden_and_rejected_without_enrollment() {
        let user = LoginUser {
            username: "quickuser".into(),
            display_name: "Quinten Quick".into(),
        };
        let mut app = GreeterApp::with_sessions_login_user_and_fingerprint(
            test_sessions(),
            Some(user),
            false,
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

    /// The greeter must never put the internal desktop id in front of the
    /// user. `nirios` is a `DesktopNames=`/`UseIn=` contract with the
    /// portal, not a name anyone logging in should have to read.
    #[test]
    fn session_label_is_the_human_name_not_the_desktop_id() {
        let app = GreeterApp::with_sessions(test_sessions());
        let label = app.session_label();
        assert!(
            !label.contains("nirios"),
            "internal desktop id leaked into the greeter: {label:?}"
        );
        assert!(
            !label.starts_with("Session:"),
            "the picker is labelled by a field label, not by a prefix: {label:?}"
        );
    }

    #[test]
    fn empty_sessions_label_is_graceful() {
        let app = GreeterApp::with_sessions(Vec::new());
        assert_eq!(app.session_label(), "No session available");
    }

    /// With one session there is no choice to offer, so the picker is not
    /// rendered at all — and the focusable set shrinks accordingly.
    #[test]
    fn single_session_hides_the_picker() {
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

    /// Replace the session picker's CRDT value, as the `Dropdown` does on
    /// selection.
    fn choose_session(doc: &mut AutoCommit, root: &ObjId, name: &str) {
        let (_, obj) = doc.get(root, SESSION_PATH).unwrap().unwrap();
        let old = doc.text(&obj).unwrap_or_default().chars().count();
        let old = isize::try_from(old).expect("count fits isize");
        let _ = doc.splice_text(&obj, 0, old, name);
    }

    /// The picker owns the choice: it writes a label into the document and
    /// the app mirrors that onto the selected index (which is what decides
    /// the argv greetd is asked to start).
    #[test]
    fn picker_selection_drives_the_started_session() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        // Bootstrap seeds the picker with the current session, so the
        // control shows a value rather than an empty placeholder.
        app.sync(&mut doc, &root);
        assert_eq!(app.selected_session, 0);
        assert_eq!(
            GreeterApp::read_text(&doc, &root, SESSION_PATH).as_deref(),
            Some("qOS"),
        );

        choose_session(&mut doc, &root, "qOS with peer sync");
        app.sync(&mut doc, &root);
        assert_eq!(app.selected_session, 1);
        assert_eq!(app.session_label(), "qOS with peer sync");
        assert!(
            app.sessions[app.selected_session]
                .argv
                .contains(&"--enable-peer-sync".to_string()),
            "the choice must reach the argv handed to greetd"
        );

        choose_session(&mut doc, &root, "qOS");
        app.sync(&mut doc, &root);
        assert_eq!(app.selected_session, 0);
    }

    /// A label that names no known session leaves the choice alone rather
    /// than silently resetting it.
    #[test]
    fn unknown_session_label_is_ignored() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        choose_session(&mut doc, &root, "qOS with peer sync");
        app.sync(&mut doc, &root);
        assert_eq!(app.selected_session, 1);

        choose_session(&mut doc, &root, "a session that went away");
        app.sync(&mut doc, &root);
        assert_eq!(app.selected_session, 1, "choice survives a stale label");
    }

    #[test]
    fn submit_without_username_sets_status() {
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
        assert!(!GreeterApp::read_flag(&doc, &root, SUBMIT_PATH));
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
    fn widget_tree_mirrors_taffy_tree() {
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

    /// Tab order across the whole card, however deeply the controls nest.
    #[test]
    fn focus_order_reaches_every_control() {
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

    #[test]
    fn sole_local_user_gets_a_one_passcode_form_with_passcode_focus_first() {
        let user = LoginUser {
            username: "quickuser".into(),
            display_name: "Quinten Quick".into(),
        };
        let mut app = GreeterApp::with_sessions_and_login_user(test_sessions(), Some(user));
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);

        assert_eq!(
            GreeterApp::read_text(&doc, &automerge::ROOT, USERNAME_PATH).as_deref(),
            Some("quickuser"),
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
            Some("quickuser")
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

    #[test]
    fn one_passcode_autofocus_is_exposed_to_assistive_technology() {
        let greeter = GreeterApp::with_sessions_and_login_user(
            test_sessions(),
            Some(LoginUser {
                username: "quickuser".into(),
                display_name: "Quinten Quick".into(),
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

    #[test]
    fn ambiguous_account_discovery_keeps_named_two_field_fallback() {
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

    #[test]
    fn enter_defaults_to_login_only_from_text_fields() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut doc = AutoCommit::new();

        assert!(dispatch_enter(&mut app, &mut doc, "greeter.username"));
        assert!(GreeterApp::read_flag(&doc, &automerge::ROOT, SUBMIT_PATH));
    }

    #[test]
    fn focused_alternate_controls_own_enter_instead_of_login() {
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
                !GreeterApp::read_flag(&doc, &automerge::ROOT, SUBMIT_PATH),
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

    // --- auth lifecycle ------------------------------------------------

    /// Seed the password CRDT Text object with `pw`.
    fn seed_password(doc: &mut AutoCommit, root: &ObjId, pw: &str) {
        let (_, obj) = doc.get(root, PASSWORD_PATH).unwrap().unwrap();
        let _ = doc.splice_text(&obj, 0, 0, pw);
    }

    #[test]
    fn submit_without_password_sets_status() {
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

    #[test]
    fn fingerprint_auth_explicitly_uses_no_password() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);

        assert_eq!(
            GreeterApp::auth_secret(&doc, &root, AuthMethod::Password),
            Err("Enter your passcode")
        );
        assert_eq!(
            GreeterApp::auth_secret(&doc, &root, AuthMethod::Fingerprint),
            Ok(String::new()),
            "fingerprint PAM conversation must begin without a dummy password"
        );
        assert!(
            focusables(&app).contains(&"greeter.fingerprint".to_string()),
            "the passwordless factor must be an explicit reachable action"
        );
    }

    /// In flight, the button carries the sentence and nothing else is
    /// drawn. A spinner underneath it would have nothing beside it —
    /// the status line is deliberately silent unless PAM says something
    /// new — and a lone dot under a full-width button reads as a
    /// rendering fault rather than as progress.
    #[test]
    fn authenticating_relabels_the_button_and_draws_nothing_else() {
        let app = GreeterApp::for_shot(AuthScenario::Authenticating);
        assert_eq!(app.phase, AuthPhase::Authenticating);
        assert_eq!(app.login_label(), "Signing you in…");
        assert_eq!(
            count_named(card_of(&app).as_ref(), "Spinner"),
            0,
            "a spinner with no message beside it is a dot in empty space",
        );
    }

    /// And the moment there *is* something to say, the pair appears —
    /// so this is "no spinner without a message", not "no spinner".
    #[test]
    fn a_spinner_appears_once_there_is_something_to_say() {
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

    /// The regression this screen was carrying: "Authenticating…" was
    /// rendered by BOTH the primary button and the status line, with the
    /// spinner wedged between the two copies, so the card said the same
    /// sentence twice and read as a rendering fault.
    ///
    /// The in-flight sentence now lives on the button and nowhere else.
    #[test]
    fn in_flight_state_is_said_exactly_once() {
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

    /// A live PAM prompt IS new information, so it may appear alongside the
    /// button — but it must never be a restatement of it.
    #[test]
    fn fingerprint_prompt_renders_as_feedback() {
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

    #[test]
    fn login_feedback_is_a_live_region_with_appropriate_urgency() {
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

    #[test]
    fn worker_prompt_update_reflects_in_feedback() {
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

    /// An attempt must always resolve into something the user can see. On
    /// success greetd hands over the VT and the process exits — without an
    /// explicit accepted state the spinner would simply vanish, which is
    /// indistinguishable from a greeter that has hung.
    #[test]
    fn accepted_update_resolves_into_a_visible_success_state() {
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

    #[test]
    fn worker_failure_returns_to_form_clears_password_keeps_username() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);

        // Seed both fields and enter the authenticating phase.
        let (_, uobj) = doc.get(&root, USERNAME_PATH).unwrap().unwrap();
        let _ = doc.splice_text(&uobj, 0, 0, "alice");
        seed_password(&mut doc, &root, "wrongpw");
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
        assert_eq!(
            GreeterApp::read_text(&doc, &root, PASSWORD_PATH).as_deref(),
            Some("")
        );
        assert_eq!(
            GreeterApp::read_text(&doc, &root, USERNAME_PATH).as_deref(),
            Some("alice")
        );
    }

    /// A rejected attempt and a blank field are different things and must
    /// not look alike: the rejection carries error severity (and therefore
    /// a leading glyph and the danger colour), the blank field does not.
    #[test]
    fn status_severity_distinguishes_a_rejection_from_a_blank_field() {
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

    #[test]
    fn worker_failure_with_detail_surfaces_secondary() {
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

    #[test]
    fn submit_while_authenticating_is_ignored() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);

        // Pretend a request is already in flight.
        app.phase = AuthPhase::Authenticating;
        let (_, uobj) = doc.get(&root, USERNAME_PATH).unwrap().unwrap();
        let _ = doc.splice_text(&uobj, 0, 0, "alice");
        seed_password(&mut doc, &root, "pw");

        // A second submit must be consumed but NOT start a new attempt or
        // reset the phase.
        let _ = doc.put(&root, SUBMIT_PATH, true);
        app.sync(&mut doc, &root);
        assert_eq!(app.phase, AuthPhase::Authenticating);
        assert!(!GreeterApp::read_flag(&doc, &root, SUBMIT_PATH));
        // Password is still intact (no failure was processed).
        assert_eq!(
            GreeterApp::read_text(&doc, &root, PASSWORD_PATH).as_deref(),
            Some("pw")
        );
    }

    /// A submit that lands while the session is already starting must not
    /// restart the conversation either.
    #[test]
    fn submit_while_starting_is_ignored() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);

        app.begin_authenticating();
        app.begin_starting();
        let (_, uobj) = doc.get(&root, USERNAME_PATH).unwrap().unwrap();
        let _ = doc.splice_text(&uobj, 0, 0, "alice");
        seed_password(&mut doc, &root, "pw");

        let _ = doc.put(&root, SUBMIT_PATH, true);
        app.sync(&mut doc, &root);
        assert_eq!(app.phase, AuthPhase::Starting);
        assert_eq!(
            GreeterApp::read_text(&doc, &root, PASSWORD_PATH).as_deref(),
            Some("pw")
        );
    }

    /// On the *form* (Idle), the focusable order must NOT shift when the
    /// session is cycled or an error/detail is shown — that is the
    /// precondition for the App's `autofocus_on_structure_change` to keep
    /// keyboard focus + caret glued to the same field across the rebuild.
    #[test]
    fn focusable_order_is_stable_across_idle_state_changes() {
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

    /// The Caps-Lock warning is a non-focusable child inserted under the
    /// password field, so it must NOT shift the focusable order (caret +
    /// focus continuity depend on that).
    #[test]
    fn caps_lock_warning_does_not_shift_focusable_order() {
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

    /// During an in-flight attempt the username + password inputs are
    /// disabled, so they drop out of the focusable set — and entering the
    /// phase raises a one-shot `take_focus_reset` so the App re-seats focus
    /// instead of pasting a now-invalid ordinal.
    #[test]
    fn authenticating_disables_inputs_and_requests_focus_reset() {
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

    /// Exercise the real App focus reset, not just the greeter's widget-tree
    /// inventory. When auth disables both fields, the reset must land on the
    /// inert primary action rather than on Restart or Shut down.
    #[test]
    fn authenticating_focus_reset_lands_on_primary_action() {
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

    /// Power-action buttons raise a boolean rising edge that `sync` consumes:
    /// the flag is reset and the action is recorded (the real `systemctl`
    /// call is compiled out under `cfg(test)`).
    #[test]
    fn power_flag_rising_edge_resets_and_records_action() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root); // bootstrap flags

        // Reboot.
        let _ = doc.put(&root, REBOOT_PATH, true);
        app.sync(&mut doc, &root);
        assert_eq!(app.last_power_action, Some(PowerAction::Reboot));
        assert!(
            !GreeterApp::read_flag(&doc, &root, REBOOT_PATH),
            "reboot flag reset"
        );

        // Power off.
        let _ = doc.put(&root, POWEROFF_PATH, true);
        app.sync(&mut doc, &root);
        assert_eq!(app.last_power_action, Some(PowerAction::PowerOff));
        assert!(
            !GreeterApp::read_flag(&doc, &root, POWEROFF_PATH),
            "poweroff flag reset"
        );
    }

    /// A failed attempt returns to the form (inputs re-enabled) and also
    /// raises a focus reset, since the focusable set changes back.
    #[test]
    fn failure_requests_focus_reset() {
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

    /// Every scenario the `--shots` path can render must actually compose,
    /// so a snapshot can never silently become a blank frame.
    #[test]
    fn every_shot_scenario_composes() {
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

    #[test]
    fn personal_shot_variants_match_discovered_capabilities() {
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
            accessible_name_for(ambiguous.as_ref(), "greeter.fingerprint").is_none(),
            "the ambiguous-account shot must not invent an enrolled fingerprint"
        );
    }

    #[test]
    fn personal_device_greets_the_known_person_before_requesting_passcode() {
        let app = GreeterApp::with_sessions_and_login_user(
            test_sessions(),
            Some(LoginUser {
                username: "quickuser".into(),
                display_name: "Quinten Quick".into(),
            }),
        );
        let card = card_of(&app);
        let spoken = spoken(card.as_ref());

        let welcome = spoken
            .iter()
            .position(|text| text == "Welcome back, Quinten Quick")
            .expect("personal greeter has a named welcome");
        let passcode = spoken
            .iter()
            .position(|text| text == "Passcode")
            .expect("personal greeter retains its passcode field");
        assert!(
            welcome < passcode,
            "the welcome should establish context before the form: {spoken:?}"
        );
        assert_eq!(
            accessible_name_for(card.as_ref(), "greeter.welcome").as_deref(),
            Some("Welcome back, Quinten Quick")
        );
        assert_eq!(
            focusables(&app).first().map(String::as_str),
            Some("greeter.password"),
            "visual warmth must not slow keyboard login"
        );
    }

    /// A narrow portrait display must keep the complete primary workflow on
    /// screen instead of clipping the fixed-width card at both edges.
    #[test]
    fn narrow_viewport_keeps_auth_and_power_actions_visible() {
        let mut app = GreeterApp::for_shot(AuthScenario::LongName);
        let _layout = layout_at(&mut app, (320.0, 640.0));

        for id in ["greeter.login", "greeter.reboot", "greeter.poweroff"] {
            let rect = rect_for(&app, id).unwrap_or_else(|| panic!("missing {id}"));
            assert!(rect.x0 >= 15.5, "{id} escaped left: {rect:?}");
            assert!(rect.x1 <= 304.5, "{id} escaped right: {rect:?}");
            assert!(rect.y0 >= 0.0 && rect.y1 <= 640.0, "{id} clipped: {rect:?}");
        }
        assert!(
            rect_for(&app, "greeter.fingerprint").is_none(),
            "an ambiguous-account fallback must not advertise an account-specific fingerprint"
        );
    }

    /// At the 640×480 logical viewport produced by common 2× display
    /// scaling, the sign-in and power actions remain immediately available.
    /// The content card may scroll, but machine controls deliberately live
    /// in a persistent footer rather than disappearing with it.
    #[test]
    fn large_text_viewport_keeps_auth_visible_and_power_reachable() {
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
                if app.fingerprint == FingerprintAvailability::Enrolled && !app.phase.is_busy() {
                    let rect =
                        rect_for(&app, "greeter.fingerprint").expect("enrolled fingerprint action");
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

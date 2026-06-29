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
//! (the TextInputs) never enter `focusable_order` — breaking Tab / arrow
//! navigation. That's why the card is a real [`Card`] widget rather than a
//! bare taffy node owned directly by `GreeterApp`: the structure stays
//! `GreeterApp -> Card -> [fields]`, mirroring the taffy tree exactly.
//!
//! State lives in the CRDT (the toolkit convention):
//!   * `greeter.username` — `Text`
//!   * `greeter.password` — `Text` (rendered masked; value stays real)
//!   * `greeter.submit`   — `bool` flag (Log-in / Enter rising edge)
//!   * `greeter.cycle`    — `bool` flag (session-cycle rising edge)

use std::sync::{Arc, OnceLock};

use automerge::{transaction::Transactable, AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use taffy::style_helpers::{length, percent};
use taffy::{
    AlignItems, Dimension, Display, FlexDirection, JustifyContent, NodeId, Size, Style, TaffyTree,
};
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use vello::Scene;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, NamedKey};

use ui::app::UserEvent;
use ui::caps_lock::CapsLockEstimate;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, Row, Spinner, Text, TextInput, Widget};

use crate::auth::{spawn_auth, AuthUpdate};
use crate::sessions::{read_sessions, Session, DEFAULT_SESSIONS_DIR};

const USERNAME_PATH: &str = "greeter.username";
const PASSWORD_PATH: &str = "greeter.password";
const SUBMIT_PATH: &str = "greeter.submit";
const CYCLE_PATH: &str = "greeter.cycle";
/// Rising-edge boolean flags raised by the secondary power-action buttons.
/// Consumed in [`GreeterApp::sync`], which routes them through logind via
/// `systemctl reboot` / `systemctl poweroff` (authorised for the `greetd`
/// user by os/etc/polkit-1/rules.d/49-qos-greeter-power.rules).
const REBOOT_PATH: &str = "greeter.reboot";
const POWEROFF_PATH: &str = "greeter.poweroff";

/// Card geometry (logical px).
const CARD_WIDTH: f32 = 360.0;
const CARD_PADDING: f32 = 28.0;
const CARD_GAP: f32 = 16.0;

/// A fixed-width, padded, vertically-stacked card with a rounded
/// background. Exists as its own `Widget` (rather than a bare taffy node)
/// so the widget tree mirrors the taffy tree for the App's focusable-order
/// traversal — see the module docs.
struct Card {
    children: Vec<Box<dyn Widget>>,
    rect: Rect,
}

impl Card {
    fn new(children: Vec<Box<dyn Widget>>) -> Self {
        Self {
            children,
            rect: Rect::ZERO,
        }
    }
}

impl Widget for Card {
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
        let node = taffy
            .new_with_children(
                Style {
                    display: Display::Flex,
                    flex_direction: FlexDirection::Column,
                    align_items: Some(AlignItems::Center),
                    gap: Size {
                        width: length(0.0),
                        height: length(CARD_GAP),
                    },
                    padding: taffy::Rect {
                        left: length(CARD_PADDING),
                        right: length(CARD_PADDING),
                        top: length(CARD_PADDING),
                        bottom: length(CARD_PADDING),
                    },
                    size: Size {
                        width: length(CARD_WIDTH),
                        height: Dimension::auto(),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                &child_nodes,
            )
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        let card_origin = Point::new(self.rect.x0, self.rect.y0);
        if let Ok(child_nodes) = taffy.children(node) {
            for (child, &child_node) in self.children.iter_mut().zip(child_nodes.iter()) {
                child.apply_layout(taffy, child_node, card_origin);
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
        let shape = RoundedRect::from_rect(self.rect, theme.radius_standard);
        scene.fill(Fill::NonZero, transform, theme.bg_primary, None, &shape);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &shape,
        );
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
            if let Some(&cnode) = child_nodes.get(idx) {
                self.children[idx].handle_event(
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
        self.children.iter().map(|c| c.as_ref()).collect()
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
        self.children
            .iter_mut()
            .map(|c| c.take_structure_dirty())
            .fold(false, |acc, d| acc || d)
    }

    fn needs_commit(&self) -> bool {
        self.children.iter().any(|c| c.needs_commit())
    }

    fn on_commit(&mut self) {
        for child in &mut self.children {
            child.on_commit();
        }
    }

    fn name(&self) -> &str {
        "GreeterCard"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// Where the greeter is in the authentication lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthPhase {
    /// Showing the login form, waiting for the user.
    Idle,
    /// A greetd conversation is running on the worker thread. Inputs are
    /// shown with a spinner + status and submit is ignored (no
    /// double-submit). On success the process exits; on failure we return
    /// to [`AuthPhase::Idle`] with an error.
    Authenticating,
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
}

pub struct GreeterApp {
    sessions: Vec<Session>,
    selected_session: usize,
    status: String,
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
        Self::with_sessions(read_sessions(std::path::Path::new(DEFAULT_SESSIONS_DIR)))
    }

    /// Construct with an explicit session list (used by tests / headless
    /// rendering where the system directory may be absent).
    pub fn with_sessions(sessions: Vec<Session>) -> Self {
        Self {
            sessions,
            selected_session: 0,
            status: String::new(),
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
        }
    }

    /// Wire the shared event-loop proxy slot. `main.rs` constructs the slot,
    /// hands a clone here, and populates it inside `App::run_with_setup`.
    /// The submit handler reads it to spawn the background auth worker.
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
        let mut app = Self::with_sessions(crate::sessions::read_sessions(std::path::Path::new(
            DEFAULT_SESSIONS_DIR,
        )));
        match scenario {
            AuthScenario::Form => {}
            AuthScenario::Error => {
                app.status = "Incorrect username or password".to_string();
            }
            AuthScenario::Authenticating => {
                app.phase = AuthPhase::Authenticating;
            }
            AuthScenario::Fingerprint => {
                app.phase = AuthPhase::Authenticating;
                app.prompt = "Place your finger on the reader".to_string();
            }
            AuthScenario::CapsLock => {
                // Force the heuristic "on" so the warning renders.
                app.caps_lock.observe('A', false);
            }
        }
        app
    }

    /// Label for the session-cycle button.
    fn session_label(&self) -> String {
        match self.sessions.get(self.selected_session) {
            Some(s) => format!("Session: {}", s.name),
            None => "No session available".to_string(),
        }
    }

    /// The login-button label reflects the auth phase.
    fn login_label(&self) -> &'static str {
        match self.phase {
            AuthPhase::Idle => "Log in",
            AuthPhase::Authenticating => "Authenticating…",
        }
    }

    /// The feedback line shown under the controls: the live PAM prompt
    /// while authenticating, otherwise the status/error string.
    fn feedback_text(&self) -> String {
        match self.phase {
            AuthPhase::Authenticating => {
                if self.prompt.is_empty() {
                    "Authenticating…".to_string()
                } else {
                    self.prompt.clone()
                }
            }
            AuthPhase::Idle => self.status.clone(),
        }
    }

    /// Build the composed card subtree from current state.
    ///
    /// Focus continuity contract: the two text inputs (username, password)
    /// and the two main buttons (session, login) are the *focusable* set on
    /// the form, in that order. Non-focusable widgets (title, the Caps-Lock
    /// warning, spinner, status/detail) never change the focusable ordinal,
    /// so an incidental rebuild (status text, caps toggle, session cycle)
    /// keeps keyboard focus AND the caret glued in place — see `main.rs`'s
    /// `autofocus_on_structure_change` and the App's `rebuild_structure`.
    ///
    /// While authenticating the inputs are **disabled** (dimmed + dropped
    /// from the focus order), which deliberately changes the focusable set;
    /// the phase transition raises `take_focus_reset` so the App re-seats
    /// focus rather than preserving a now-invalid ordinal.
    fn build_composition(&self) -> Box<dyn Widget> {
        let authenticating = self.phase == AuthPhase::Authenticating;

        let mut children: Vec<Box<dyn Widget>> = Vec::new();
        children.push(Box::new(Text::new("qOS").with_size(34.0)));
        children.push(Box::new(
            TextInput::new(Vec::new())
                .with_id("greeter.username")
                .with_on_change_path(USERNAME_PATH)
                .disabled(authenticating),
        ));
        children.push(Box::new(
            TextInput::new(Vec::new())
                .with_id("greeter.password")
                .with_on_change_path(PASSWORD_PATH)
                .with_secret(true)
                .disabled(authenticating),
        ));

        // Caps-Lock warning sits directly under the password field. It's a
        // non-focusable Text, so toggling it never shifts the focusable
        // ordinal. Only meaningful on the live form (not mid-auth).
        if !authenticating && self.caps_lock.on() {
            children.push(Box::new(Text::new("⇪ Caps Lock is on").with_size(13.0)));
        }

        children.push(Box::new(Button::new(
            "greeter.session",
            &self.session_label(),
            Some(CYCLE_PATH),
        )));
        children.push(Box::new(Button::new(
            "greeter.login",
            self.login_label(),
            Some(SUBMIT_PATH),
        )));

        // While authenticating, a spinner gives live feedback that the
        // request is in flight (and the "Authenticating…" button label +
        // the submit guard in `sync` prevent double-submits).
        if authenticating {
            children.push(Box::new(Spinner::new()));
        }

        // Feedback line: PAM prompt / "Authenticating…" / status / error.
        children.push(Box::new(Text::new(&self.feedback_text()).with_size(14.0)));

        // Optional small secondary detail (raw greetd description), only
        // meaningful on the form (not mid-auth).
        if self.phase == AuthPhase::Idle && !self.detail.is_empty() {
            children.push(Box::new(Text::new(&self.detail).with_size(11.0)));
        }

        // Secondary power-actions row at the bottom of the card. Quiet
        // neutral buttons (ui::Button), always present, so they sit at a
        // stable trailing focusable ordinal across rebuilds.
        children.push(Box::new(
            Row::new()
                .push(Box::new(Button::new(
                    "greeter.reboot",
                    "Reboot",
                    Some(REBOOT_PATH),
                )))
                .push(Box::new(Button::new(
                    "greeter.poweroff",
                    "Shut down",
                    Some(POWEROFF_PATH),
                ))),
        ));

        Box::new(Card::new(children))
    }

    fn bootstrap_crdt(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if state.get(root, USERNAME_PATH).ok().flatten().is_none() {
            let _ = state.put_object(root, USERNAME_PATH, ObjType::Text);
        }
        if state.get(root, PASSWORD_PATH).ok().flatten().is_none() {
            let _ = state.put_object(root, PASSWORD_PATH, ObjType::Text);
        }
        if state.get(root, SUBMIT_PATH).ok().flatten().is_none() {
            let _ = state.put(root, SUBMIT_PATH, false);
        }
        if state.get(root, CYCLE_PATH).ok().flatten().is_none() {
            let _ = state.put(root, CYCLE_PATH, false);
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

    fn read_flag(state: &AutoCommit, root: &ObjId, path: &str) -> bool {
        matches!(
            state.get(root, path),
            Ok(Some((Value::Scalar(s), _)))
                if matches!(s.as_ref(), ScalarValue::Boolean(true))
        )
    }

    /// Environment handed to greetd's `StartSession`. Mirrors the
    /// `DesktopNames=nirios` advertised by the session `.desktop` files.
    fn session_env(&self) -> Vec<String> {
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
    fn attempt_login(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Guard: never start a second attempt while one is in flight.
        if self.phase != AuthPhase::Idle {
            return;
        }

        let username = Self::read_text(state, root, USERNAME_PATH).unwrap_or_default();
        let password = Self::read_text(state, root, PASSWORD_PATH).unwrap_or_default();

        if username.trim().is_empty() {
            self.set_status("Enter a username");
            return;
        }
        if password.is_empty() {
            self.set_status("Enter a password");
            return;
        }
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
        let env = self.session_env();

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
                &sock, &username, &password, &argv, &env, |_| {},
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
        if let Ok(Some((Value::Object(ObjType::Text), obj))) = state.get(root, PASSWORD_PATH) {
            if let Ok(text) = state.text(&obj) {
                let chars = text.chars().count();
                if chars > 0 {
                    let _ = state.splice_text(&obj, 0, chars as isize, "");
                }
            }
        }
    }

    fn set_status(&mut self, msg: &str) {
        if self.status != msg || !self.detail.is_empty() {
            self.status = msg.to_string();
            self.detail.clear();
            self.dirty_structure = true;
        }
    }
}

impl Widget for GreeterApp {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !self.bootstrap_done {
            self.bootstrap_crdt(state, root);
            self.bootstrap_done = true;
        }

        // Session-cycle rising edge.
        if Self::read_flag(state, root, CYCLE_PATH) {
            if !self.sessions.is_empty() {
                self.selected_session = (self.selected_session + 1) % self.sessions.len();
                self.dirty_structure = true;
            }
            let _ = state.put(root, CYCLE_PATH, false);
        }

        // Submit rising edge — reset the flag *before* attempting so a
        // single click fires exactly one attempt.
        if Self::read_flag(state, root, SUBMIT_PATH) {
            let _ = state.put(root, SUBMIT_PATH, false);
            self.attempt_login(state, root);
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
        let mut composed = self.build_composition();
        let card_node = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        // Root fills the screen and centers the card both axes.
        let root = taffy
            .new_with_children(
                Style {
                    display: Display::Flex,
                    flex_direction: FlexDirection::Column,
                    justify_content: Some(JustifyContent::Center),
                    align_items: Some(AlignItems::Center),
                    size: Size {
                        width: percent(1.0),
                        height: percent(1.0),
                    },
                    ..Default::default()
                },
                &[card_node],
            )
            .unwrap();

        node_map.insert(root, self as *mut dyn Widget);
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node) {
                if let Some(&card_node) = children.first() {
                    c.apply_layout(taffy, card_node, Point::new(self.rect.x0, self.rect.y0));
                }
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
        // Full-screen backdrop, then the card draws itself + its fields.
        scene.fill(Fill::NonZero, transform, theme.bg_app, None, &self.rect);
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

        // Enter submits from anywhere in the form. We mark the event
        // handled so the App runs its post-event `sync` (which detects
        // the rising edge and runs auth).
        let mut enter_pressed = false;
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::Enter),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            let _ = state.put(root, SUBMIT_PATH, true);
            enter_pressed = true;
        }

        // Delegate to the composed card.
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node_id) {
                if let Some(&card_node) = children.first() {
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
            }
        }

        if enter_pressed {
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
            .map(|c| c.take_structure_dirty())
            .unwrap_or(false);
        mine || child
    }

    /// Rising-edge one-shot consumed by the App's `rebuild_structure`. We
    /// raise the flag only on auth-phase transitions (which enable/disable
    /// the inputs and so change the focusable set); plain status / caps /
    /// session-cycle rebuilds leave it `false` so focus + caret are preserved
    /// in place.
    fn take_focus_reset(&mut self) -> bool {
        let mine = std::mem::take(&mut self.focus_reset_pending);
        let child = self
            .composed
            .as_mut()
            .map(|c| c.take_focus_reset())
            .unwrap_or(false);
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

    fn name(&self) -> &str {
        "GreeterApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_sessions() -> Vec<Session> {
        vec![
            Session {
                name: "nirios".to_string(),
                argv: vec!["/usr/bin/shell".into(), "--backend".into(), "drm".into()],
            },
            Session {
                name: "nirios (peer-sync)".to_string(),
                argv: vec![
                    "/usr/bin/shell".into(),
                    "--backend".into(),
                    "drm".into(),
                    "--enable-peer-sync".into(),
                ],
            },
        ]
    }

    #[test]
    fn session_label_reflects_selection() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        assert_eq!(app.session_label(), "Session: nirios");
        app.selected_session = 1;
        assert_eq!(app.session_label(), "Session: nirios (peer-sync)");
    }

    #[test]
    fn empty_sessions_label_is_graceful() {
        let app = GreeterApp::with_sessions(Vec::new());
        assert_eq!(app.session_label(), "No session available");
    }

    #[test]
    fn cycle_flag_advances_selection_and_resets() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        // First sync bootstraps the flags.
        app.sync(&mut doc, &root);
        assert_eq!(app.selected_session, 0);

        // Raise the cycle flag and re-sync.
        let _ = doc.put(&root, CYCLE_PATH, true);
        app.sync(&mut doc, &root);
        assert_eq!(app.selected_session, 1);
        // Flag must be reset so a single click cycles exactly once.
        assert!(!GreeterApp::read_flag(&doc, &root, CYCLE_PATH));

        // Wraps around.
        let _ = doc.put(&root, CYCLE_PATH, true);
        app.sync(&mut doc, &root);
        assert_eq!(app.selected_session, 0);
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
        assert_eq!(app.status, "Enter a username");
        assert!(!GreeterApp::read_flag(&doc, &root, SUBMIT_PATH));
    }

    /// The widget tree must mirror the taffy tree so the App's
    /// focusable-order traversal reaches the inputs. After `build_layout`,
    /// `GreeterApp.children()` is the single card, and the card's children
    /// are the six fields — the structural invariant focus relies on.
    #[test]
    fn widget_tree_mirrors_taffy_tree() {
        use std::collections::HashMap;
        let mut app = GreeterApp::with_sessions(test_sessions());
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

        let root = app.build_layout(&mut taffy, &theme, &mut node_map);

        // GreeterApp exposes exactly one child (the card), matching the
        // single taffy child of the root node.
        assert_eq!(app.children().len(), 1);
        assert_eq!(taffy.children(root).unwrap().len(), 1);

        // The card exposes seven children in the default form: title,
        // username, password, session button, login button, feedback line,
        // and the power-actions row.
        let card_node = taffy.children(root).unwrap()[0];
        let card = app.children()[0];
        let fields = card.children();
        assert_eq!(fields.len(), 7);
        assert_eq!(taffy.children(card_node).unwrap().len(), 7);

        // The username (idx 1) and password (idx 2) text inputs must be
        // focusable — this is what `App::update_focusable_order` needs to
        // reach so Tab/arrow nav and startup auto-focus work.
        assert!(fields[1].is_focusable(), "username must be focusable");
        assert!(fields[2].is_focusable(), "password must be focusable");

        // Four DIRECT card children are focusable in tree order: username,
        // password, session button, login button. The power-actions row is a
        // non-focusable `Row` container (its two buttons are focusable but
        // nested one level deeper, reached by the App's recursive walk).
        let focusable = fields.iter().filter(|c| c.is_focusable()).count();
        assert_eq!(focusable, 4, "2 inputs + 2 buttons are direct focusable children");
    }

    // --- auth lifecycle ------------------------------------------------

    /// Seed the password CRDT Text object with `pw`.
    fn seed_password(doc: &mut AutoCommit, root: &ObjId, pw: &str) {
        let (_, obj) = doc.get(root, PASSWORD_PATH).unwrap().unwrap();
        let _ = doc.splice_text(&obj, 0, 0, pw);
    }

    /// The card's focusable child indices for the current state.
    fn focusable_indices(app: &GreeterApp) -> Vec<usize> {
        let card = app.build_composition();
        card.children()
            .iter()
            .enumerate()
            .filter(|(_, c)| c.is_focusable())
            .map(|(i, _)| i)
            .collect()
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
        assert_eq!(app.status, "Enter a password");
        assert_eq!(app.phase, AuthPhase::Idle);
    }

    #[test]
    fn authenticating_shows_spinner_and_relabels_button() {
        let app = GreeterApp::for_shot(AuthScenario::Authenticating);
        assert_eq!(app.phase, AuthPhase::Authenticating);
        assert_eq!(app.login_label(), "Authenticating…");
        let card = app.build_composition();
        let names: Vec<&str> = card.children().iter().map(|c| c.name()).collect();
        assert!(
            names.iter().any(|n| n.contains("Spinner")),
            "a spinner is shown while authenticating, got {names:?}"
        );
    }

    #[test]
    fn fingerprint_prompt_renders_as_feedback() {
        let app = GreeterApp::for_shot(AuthScenario::Fingerprint);
        assert_eq!(app.phase, AuthPhase::Authenticating);
        assert_eq!(app.feedback_text(), "Place your finger on the reader");
    }

    #[test]
    fn worker_prompt_update_reflects_in_feedback() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);

        app.begin_authenticating();
        assert_eq!(app.feedback_text(), "Authenticating…");

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
            message: "Incorrect username or password".to_string(),
            detail: None,
        }
        .encode();
        app.on_external_event(&payload, &mut doc, &root);

        assert_eq!(app.phase, AuthPhase::Idle, "back to the form on failure");
        assert_eq!(app.status, "Incorrect username or password");
        // Password cleared, username preserved.
        assert_eq!(GreeterApp::read_text(&doc, &root, PASSWORD_PATH).as_deref(), Some(""));
        assert_eq!(GreeterApp::read_text(&doc, &root, USERNAME_PATH).as_deref(), Some("alice"));
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
        // The detail renders as a small secondary line on the form: the
        // card now holds three plain Text widgets (title, feedback, detail)
        // — TextInput is a distinct widget name, so it isn't counted here.
        let card = app.build_composition();
        let texts = card
            .children()
            .iter()
            .filter(|c| c.name() == "Text")
            .count();
        assert_eq!(texts, 3, "title + feedback + detail Texts expected");
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
        assert_eq!(GreeterApp::read_text(&doc, &root, PASSWORD_PATH).as_deref(), Some("pw"));
    }

    /// Deliverable #4: on the *form* (Idle), the focusable order must NOT
    /// shift when the session is cycled or an error/detail is shown — that is
    /// the precondition for the App's `autofocus_on_structure_change` to keep
    /// keyboard focus + caret glued to the same field across the rebuild.
    #[test]
    fn focusable_order_is_stable_across_idle_state_changes() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let base = focusable_indices(&app);
        assert_eq!(base, vec![1, 2, 3, 4], "username, password, session, login");

        app.selected_session = 1; // cycle session
        assert_eq!(focusable_indices(&app), base, "session cycle keeps order");

        app.status = "Incorrect username or password".to_string();
        app.detail = "boom".to_string();
        assert_eq!(focusable_indices(&app), base, "error + detail keep order");
    }

    /// The Caps-Lock warning is a non-focusable child inserted under the
    /// password field, so it must NOT shift the focusable order (caret +
    /// focus continuity depend on that).
    #[test]
    fn caps_lock_warning_does_not_shift_focusable_order() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        assert_eq!(focusable_indices(&app), vec![1, 2, 3, 4]);
        app.caps_lock.observe('A', false); // force "on"
        // A non-focusable Text is inserted at idx 3, shifting later children
        // by one — but the *focusable* indices among them are unchanged.
        assert_eq!(
            focusable_indices(&app),
            vec![1, 2, 4, 5],
            "warning inserts a non-focusable child but keeps the same focusable widgets"
        );
        // Idle form has two Texts (title + feedback line); the warning adds a
        // third when caps is on.
        let off = GreeterApp::with_sessions(test_sessions()).build_composition();
        let texts_off = off.children().iter().filter(|c| c.name() == "Text").count();
        let on = app.build_composition();
        let texts_on = on.children().iter().filter(|c| c.name() == "Text").count();
        assert_eq!(texts_on, texts_off + 1, "caps-lock warning adds one Text");
    }

    /// During Authenticating the username + password inputs are disabled, so
    /// they drop out of the focusable set — and entering the phase raises a
    /// one-shot `take_focus_reset` so the App re-seats focus instead of
    /// pasting a now-invalid ordinal.
    #[test]
    fn authenticating_disables_inputs_and_requests_focus_reset() {
        let mut app = GreeterApp::with_sessions(test_sessions());
        // Idle: username + password focusable.
        assert_eq!(focusable_indices(&app), vec![1, 2, 3, 4]);

        app.begin_authenticating();

        // The two inputs are no longer focusable — only the session + login
        // buttons (and the nested power buttons) remain.
        let card = app.build_composition();
        let fields = card.children();
        assert!(!fields[1].is_focusable(), "username disabled during auth");
        assert!(!fields[2].is_focusable(), "password disabled during auth");

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
        assert!(!GreeterApp::read_flag(&doc, &root, REBOOT_PATH), "reboot flag reset");

        // Power off.
        let _ = doc.put(&root, POWEROFF_PATH, true);
        app.sync(&mut doc, &root);
        assert_eq!(app.last_power_action, Some(PowerAction::PowerOff));
        assert!(!GreeterApp::read_flag(&doc, &root, POWEROFF_PATH), "poweroff flag reset");
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
}

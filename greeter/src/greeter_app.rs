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

use automerge::{transaction::Transactable, AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use taffy::style_helpers::{length, percent};
use taffy::{
    AlignItems, Dimension, Display, FlexDirection, JustifyContent, NodeId, Size, Style, TaffyTree,
};
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use vello::Scene;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, Text, TextInput, Widget};

use crate::sessions::{read_sessions, Session, DEFAULT_SESSIONS_DIR};

const USERNAME_PATH: &str = "greeter.username";
const PASSWORD_PATH: &str = "greeter.password";
const SUBMIT_PATH: &str = "greeter.submit";
const CYCLE_PATH: &str = "greeter.cycle";

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

pub struct GreeterApp {
    sessions: Vec<Session>,
    selected_session: usize,
    status: String,
    bootstrap_done: bool,
    /// Set when the composition must be rebuilt (session label or status
    /// text changed). Drained by `take_structure_dirty`, which the App
    /// polls to trigger a fresh `build_layout`.
    dirty_structure: bool,
    rect: Rect,
    /// The composed card subtree, rebuilt each `build_layout`.
    composed: Option<Box<dyn Widget>>,
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
            bootstrap_done: false,
            dirty_structure: false,
            rect: Rect::ZERO,
            composed: None,
        }
    }

    /// Label for the session-cycle button.
    fn session_label(&self) -> String {
        match self.sessions.get(self.selected_session) {
            Some(s) => format!("Session: {}", s.name),
            None => "No session available".to_string(),
        }
    }

    /// Build the composed card subtree from current state.
    fn build_composition(&self) -> Box<dyn Widget> {
        let children: Vec<Box<dyn Widget>> = vec![
            Box::new(Text::new("qOS").with_size(34.0)),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("greeter.username")
                    .with_on_change_path(USERNAME_PATH),
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("greeter.password")
                    .with_on_change_path(PASSWORD_PATH)
                    .with_secret(true),
            ),
            Box::new(Button::new(
                "greeter.session",
                &self.session_label(),
                Some(CYCLE_PATH),
            )),
            Box::new(Button::new("greeter.login", "Log in", Some(SUBMIT_PATH))),
            Box::new(Text::new(&self.status).with_size(14.0)),
        ];
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

    /// Read credentials from the document and run the greetd exchange.
    /// On success [`crate::greetd_client::authenticate_and_start`] exits
    /// the process; on failure the description lands in the status line.
    fn attempt_login(&mut self, state: &AutoCommit, root: &ObjId) {
        let username = Self::read_text(state, root, USERNAME_PATH).unwrap_or_default();
        let password = Self::read_text(state, root, PASSWORD_PATH).unwrap_or_default();

        if username.trim().is_empty() {
            self.set_status("Enter a username");
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
        match crate::greetd_client::authenticate_and_start(&sock, &username, &password, &argv, &env)
        {
            Ok(()) => {} // never reached — success exits the process
            Err(e) => self.set_status(&e),
        }
    }

    fn set_status(&mut self, msg: &str) {
        if self.status != msg {
            self.status = msg.to_string();
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

        // The card exposes the six fields, matching its taffy children.
        let card_node = taffy.children(root).unwrap()[0];
        let card = app.children()[0];
        let fields = card.children();
        assert_eq!(fields.len(), 6);
        assert_eq!(taffy.children(card_node).unwrap().len(), 6);

        // The username (idx 1) and password (idx 2) text inputs must be
        // focusable — this is what `App::update_focusable_order` needs to
        // reach so Tab/arrow nav and startup auto-focus work.
        assert!(fields[1].is_focusable(), "username must be focusable");
        assert!(fields[2].is_focusable(), "password must be focusable");

        // Four widgets are focusable in tree order: username, password,
        // session button, login button (the two Texts are not).
        let focusable = fields.iter().filter(|c| c.is_focusable()).count();
        assert_eq!(focusable, 4, "2 inputs + 2 buttons are focusable");
    }
}

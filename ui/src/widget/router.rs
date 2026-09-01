//! Process-local screen routing with explicit, inspectable history.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::gesture::NavDirection;
use crate::nav_stack::NavStack;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

use super::{Element, TaffyTreeExt, Widget};

#[derive(Clone, Debug, PartialEq, Eq)]
struct RouteState {
    stack: NavStack<String>,
    revision: u64,
}

/// A cloneable controller for one router's process-local route stack.
///
/// Route changes are presentation state: they do not mutate the app document,
/// participate in collaboration, or create undo entries.
#[derive(Clone, Debug)]
pub struct RouteHandle {
    known: Rc<HashSet<String>>,
    state: Rc<RefCell<RouteState>>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    #[error("a router requires at least one route")]
    NoRoutes,
    #[error("route names must not be empty")]
    EmptyRoute,
    #[error("initial route {0:?} is not registered")]
    UnknownInitialRoute(String),
    #[error("route {0:?} is not registered")]
    UnknownRoute(String),
    #[error("cannot build route {route:?}: {message}")]
    Build { route: String, message: String },
}

impl RouteHandle {
    /// Create a navigation stack from a finite route registry.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry is empty, contains a blank name, or
    /// does not contain `initial`.
    pub fn new<S, I, R>(initial: S, routes: I) -> Result<Self, RouteError>
    where
        S: Into<String>,
        I: IntoIterator<Item = R>,
        R: Into<String>,
    {
        let known: HashSet<_> = routes.into_iter().map(Into::into).collect();
        if known.is_empty() {
            return Err(RouteError::NoRoutes);
        }
        if known.iter().any(|route| route.trim().is_empty()) {
            return Err(RouteError::EmptyRoute);
        }
        let initial = initial.into();
        if !known.contains(&initial) {
            return Err(RouteError::UnknownInitialRoute(initial));
        }
        Ok(Self {
            known: Rc::new(known),
            state: Rc::new(RefCell::new(RouteState {
                stack: NavStack::new(initial),
                revision: 0,
            })),
        })
    }

    #[must_use]
    pub fn current(&self) -> String {
        self.state.borrow().stack.current().clone()
    }

    #[must_use]
    pub fn knows(&self, route: &str) -> bool {
        self.known.contains(route)
    }

    #[must_use]
    pub fn can_back(&self) -> bool {
        self.state.borrow().stack.can_go_back()
    }

    #[must_use]
    pub fn can_forward(&self) -> bool {
        self.state.borrow().stack.can_go_forward()
    }

    #[must_use]
    pub fn history(&self) -> (Vec<String>, String, Vec<String>) {
        let state = self.state.borrow();
        (
            state.stack.past().to_vec(),
            state.stack.current().clone(),
            state.stack.future().to_vec(),
        )
    }

    /// Push a different route and clear forward history.
    ///
    /// # Errors
    ///
    /// Returns [`RouteError::UnknownRoute`] without changing history.
    pub fn push(&self, route: impl Into<String>) -> Result<bool, RouteError> {
        let route = route.into();
        self.require_known(&route)?;
        let mut state = self.state.borrow_mut();
        if state.stack.current() == &route {
            return Ok(false);
        }
        state.stack.push(route);
        state.revision = state.revision.wrapping_add(1);
        Ok(true)
    }

    /// Replace the current entry without extending back history.
    ///
    /// # Errors
    ///
    /// Returns [`RouteError::UnknownRoute`] without changing history.
    pub fn replace(&self, route: impl Into<String>) -> Result<bool, RouteError> {
        let route = route.into();
        self.require_known(&route)?;
        let mut state = self.state.borrow_mut();
        if state.stack.current() == &route {
            return Ok(false);
        }
        state.stack.replace_current(route);
        state.revision = state.revision.wrapping_add(1);
        Ok(true)
    }

    #[must_use]
    pub fn back(&self) -> bool {
        let mut state = self.state.borrow_mut();
        if state.stack.back().is_none() {
            return false;
        }
        state.revision = state.revision.wrapping_add(1);
        true
    }

    #[must_use]
    pub fn forward(&self) -> bool {
        let mut state = self.state.borrow_mut();
        if state.stack.forward().is_none() {
            return false;
        }
        state.revision = state.revision.wrapping_add(1);
        true
    }

    /// Replace the complete history with one registered route.
    ///
    /// # Errors
    ///
    /// Returns [`RouteError::UnknownRoute`] without changing history.
    pub fn reset(&self, route: impl Into<String>) -> Result<bool, RouteError> {
        let route = route.into();
        self.require_known(&route)?;
        let mut state = self.state.borrow_mut();
        if state.stack.current() == &route
            && !state.stack.can_go_back()
            && !state.stack.can_go_forward()
        {
            return Ok(false);
        }
        state.stack.reset(route);
        state.revision = state.revision.wrapping_add(1);
        Ok(true)
    }

    /// A directly usable [`super::Button::on_click`] callback.
    ///
    /// # Errors
    ///
    /// Returns an error immediately for an unknown route, rather than waiting
    /// for the user to click a dead control.
    pub fn push_action(&self, route: impl Into<String>) -> Result<Box<dyn FnMut()>, RouteError> {
        let route = route.into();
        self.require_known(&route)?;
        let handle = self.clone();
        Ok(Box::new(move || {
            let _ = handle.push(route.clone());
        }))
    }

    pub fn back_action(&self) -> impl FnMut() + use<> {
        let handle = self.clone();
        move || {
            let _ = handle.back();
        }
    }

    fn require_known(&self, route: &str) -> Result<(), RouteError> {
        if self.knows(route) {
            Ok(())
        } else {
            Err(RouteError::UnknownRoute(route.to_owned()))
        }
    }

    fn snapshot(&self) -> RouteState {
        self.state.borrow().clone()
    }

    fn restore(&self, mut state: RouteState) {
        state.revision = self.state.borrow().revision.wrapping_add(1);
        *self.state.borrow_mut() = state;
    }
}

type RouteFactory = dyn FnMut(&str, &Theme, &RouteHandle) -> Result<Element, String>;

/// A retained single-screen host driven by an explicit [`RouteHandle`].
pub struct Router {
    handle: RouteHandle,
    factory: Box<RouteFactory>,
    theme: Theme,
    seen: RouteState,
    child: Element,
    rect: Rect,
    structure_dirty: bool,
    focus_reset: bool,
    last_error: Option<RouteError>,
}

impl Router {
    /// Build the initial screen and retain the factory for later transitions.
    ///
    /// The factory receives the same handle that drives the host, allowing a
    /// screen to bind buttons without global state.
    ///
    /// # Errors
    ///
    /// Returns a route-registry error or the initial screen's build error.
    pub fn new<S, I, R>(
        initial: S,
        routes: I,
        theme: &Theme,
        mut factory: impl FnMut(&str, &Theme, &RouteHandle) -> Result<Element, String> + 'static,
    ) -> Result<Self, RouteError>
    where
        S: Into<String>,
        I: IntoIterator<Item = R>,
        R: Into<String>,
    {
        let handle = RouteHandle::new(initial, routes)?;
        let seen = handle.snapshot();
        let child =
            factory(seen.stack.current(), theme, &handle).map_err(|message| RouteError::Build {
                route: seen.stack.current().clone(),
                message,
            })?;
        Ok(Self {
            handle,
            factory: Box::new(factory),
            theme: theme.clone(),
            seen,
            child,
            rect: Rect::ZERO,
            structure_dirty: false,
            focus_reset: false,
            last_error: None,
        })
    }

    #[must_use]
    pub fn handle(&self) -> RouteHandle {
        self.handle.clone()
    }

    #[must_use]
    pub fn last_error(&self) -> Option<&RouteError> {
        self.last_error.as_ref()
    }

    fn rebuild_if_changed(&mut self) {
        let desired = self.handle.snapshot();
        if desired.revision == self.seen.revision {
            return;
        }
        match (self.factory)(desired.stack.current(), &self.theme, &self.handle) {
            Ok(child) => {
                self.child = child;
                self.seen = desired;
                self.structure_dirty = true;
                self.focus_reset = true;
                self.last_error = None;
            }
            Err(message) => {
                self.last_error = Some(RouteError::Build {
                    route: desired.stack.current().clone(),
                    message,
                });
                self.handle.restore(self.seen.clone());
                self.seen = self.handle.snapshot();
            }
        }
    }
}

impl Widget for Router {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.rebuild_if_changed();
        self.child.sync(state, path);
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        self.rebuild_if_changed();
        self.child.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = theme.clone();
        let child = self.child.build_layout(taffy, theme, node_map);
        let node = taffy.parent_node(
            Style {
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: percent(1.0_f32),
                },
                ..Default::default()
            },
            &[child],
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
        if let Ok(children) = taffy.children(node)
            && let Some(child) = children.first()
        {
            self.child
                .apply_layout(taffy, *child, Point::new(self.rect.x0, self.rect.y0));
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
        self.child
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Ok(children) = taffy.children(node_id)
            && let Some(child) = children.first()
        {
            self.child.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                *child,
                taffy,
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }

    fn name(&self) -> &'static str {
        "Router"
    }

    fn test_id(&self) -> Option<&str> {
        Some("router")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_commit(&self) -> bool {
        self.child.needs_commit()
    }

    fn on_commit(&mut self) {
        self.child.on_commit();
    }

    fn on_nav_gesture(
        &mut self,
        direction: NavDirection,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        if self.child.on_nav_gesture(direction, state, path) {
            return true;
        }
        match direction {
            NavDirection::Back => self.handle.back(),
            NavDirection::Forward => self.handle.forward(),
        }
    }

    fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.focus_reset) || self.child.take_focus_reset()
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty) | self.child.take_structure_dirty()
    }

    fn needs_idle_sync(&self) -> bool {
        self.handle.snapshot().revision != self.seen.revision || self.child.needs_idle_sync()
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        self.child.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_history_push_replace_back_forward_and_reset_are_explicit() {
        let handle = RouteHandle::new("home", ["home", "settings", "about"]).unwrap();
        assert_eq!(handle.current(), "home");
        assert!(!handle.can_back());
        assert!(handle.push("settings").unwrap());
        assert!(handle.push("about").unwrap());
        assert_eq!(
            handle.history(),
            (
                vec!["home".into(), "settings".into()],
                "about".into(),
                vec![]
            )
        );
        assert!(handle.back());
        assert_eq!(handle.current(), "settings");
        assert!(handle.can_forward());
        assert!(handle.forward());
        assert!(handle.replace("settings").unwrap());
        assert_eq!(handle.history().0, ["home"]);
        assert!(handle.reset("home").unwrap());
        assert_eq!(handle.history(), (vec![], "home".into(), vec![]));
    }

    #[test]
    fn unknown_routes_and_duplicate_current_pushes_do_not_mutate_history() {
        let handle = RouteHandle::new("home", ["home", "settings"]).unwrap();
        assert!(!handle.push("home").unwrap());
        let before = handle.history();
        assert_eq!(
            handle.push("missing"),
            Err(RouteError::UnknownRoute("missing".into()))
        );
        assert_eq!(handle.history(), before);
        assert!(handle.push_action("missing").is_err());
    }

    #[test]
    fn failed_screen_build_restores_the_last_valid_route() {
        let theme = Theme::default();
        let mut router = Router::new("home", ["home", "broken"], &theme, |route, theme, _| {
            if route == "broken" {
                Err("invalid screen".into())
            } else {
                Ok(Box::new(
                    super::super::Text::new(route).with_role(crate::tokens::TextRole::Body, theme),
                ))
            }
        })
        .unwrap();
        let handle = router.handle();
        handle.push("broken").unwrap();
        let mut state = crate::State::new();
        let root = state.root.clone();
        router.sync(&mut state.doc, &root);
        assert_eq!(handle.current(), "home");
        assert!(matches!(
            router.last_error(),
            Some(RouteError::Build { .. })
        ));
        assert!(!router.take_structure_dirty());
    }

    #[test]
    fn initial_deep_link_and_successful_transition_rebuild_and_reset_focus() {
        let theme = Theme::default();
        let mut router = Router::new("settings", ["home", "settings"], &theme, |route, _, _| {
            Ok(Box::new(super::super::Button::new(route, route, None)))
        })
        .unwrap();
        assert_eq!(router.handle().current(), "settings");

        router.handle().replace("home").unwrap();
        let mut state = crate::State::new();
        let root = state.root.clone();
        router.sync(&mut state.doc, &root);
        assert!(router.take_structure_dirty());
        assert!(router.take_focus_reset());
        assert!(!router.take_focus_reset(), "focus reset is edge-triggered");
    }
}

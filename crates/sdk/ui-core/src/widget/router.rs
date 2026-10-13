//! Process-local screen routing with explicit, inspectable history.

use crate::widget::{AppModel, ModelPath};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use vello::kurbo::Rect;

use crate::gesture::NavDirection;
use crate::nav_stack::NavStack;
use crate::theme::Theme;

use super::{Element, TaffyTreeExt, Widget};

#[derive(Clone, Debug, PartialEq, Eq)]
struct RouteState {
    stack: NavStack<String>,
    revision: u64,
}

/// Process-local route history, exported so a dev reload can put the same
/// screen back. Unknown names are rejected by [`RouteHandle::adopt_stack`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RouteSnapshot {
    /// Older screens, oldest first.
    pub past: Vec<String>,
    /// The screen that was showing.
    pub current: String,
    /// Screens reachable with Forward, nearest last.
    pub future: Vec<String>,
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
    #[error("router revision space is exhausted; refusing to reuse a prior route state")]
    RevisionExhausted,
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
        advance_revision(&mut state.revision)?;
        state.stack.push(route);
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
        advance_revision(&mut state.revision)?;
        state.stack.replace_current(route);
        Ok(true)
    }

    #[must_use]
    pub fn back(&self) -> bool {
        let mut state = self.state.borrow_mut();
        if !state.stack.can_go_back() || advance_revision(&mut state.revision).is_err() {
            return false;
        }
        let _ = state.stack.back();
        true
    }

    #[must_use]
    pub fn forward(&self) -> bool {
        let mut state = self.state.borrow_mut();
        if !state.stack.can_go_forward() || advance_revision(&mut state.revision).is_err() {
            return false;
        }
        let _ = state.stack.forward();
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
        advance_revision(&mut state.revision)?;
        state.stack.reset(route);
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

    fn require_known(&self, route: &str) -> Result<(), RouteError> {
        if self.knows(route) {
            Ok(())
        } else {
            Err(RouteError::UnknownRoute(route.to_owned()))
        }
    }

    /// Export the history for a same-process-session reload.
    #[must_use]
    pub fn export_stack(&self) -> RouteSnapshot {
        let state = self.state.borrow();
        RouteSnapshot {
            past: state.stack.past().to_vec(),
            current: state.stack.current().clone(),
            future: state.stack.future().to_vec(),
        }
    }

    /// Replace the history when every name is still registered.
    ///
    /// # Errors
    ///
    /// Returns [`RouteError::UnknownRoute`] or
    /// [`RouteError::RevisionExhausted`] without changing history.
    pub fn adopt_stack(&self, snapshot: &RouteSnapshot) -> Result<bool, RouteError> {
        for route in snapshot
            .past
            .iter()
            .chain(std::iter::once(&snapshot.current))
            .chain(snapshot.future.iter())
        {
            self.require_known(route)?;
        }
        let mut state = self.state.borrow_mut();
        if state.stack.past() == snapshot.past.as_slice()
            && state.stack.current() == &snapshot.current
            && state.stack.future() == snapshot.future.as_slice()
        {
            return Ok(false);
        }
        advance_revision(&mut state.revision)?;
        state.stack = NavStack::from_parts(
            snapshot.past.clone(),
            snapshot.current.clone(),
            snapshot.future.clone(),
        );
        Ok(true)
    }

    fn snapshot(&self) -> RouteState {
        self.state.borrow().clone()
    }

    fn restore(&self, mut state: RouteState) {
        let mut current = self.state.borrow_mut();
        // Rollback is part of the same owner-thread transition that already
        // consumed a revision. Reuse that *new* revision for the prior stack
        // instead of trying to allocate a second one: at `u64::MAX` the old
        // child and restored route remain coherent, while future mutations
        // still fail closed rather than wrapping.
        state.revision = current.revision;
        *current = state;
    }
}

fn advance_revision(revision: &mut u64) -> Result<(), RouteError> {
    *revision = revision
        .checked_add(1)
        .ok_or(RouteError::RevisionExhausted)?;
    Ok(())
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
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.rebuild_if_changed();
        self.child.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
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
        let style = Style {
            size: taffy::Size::from_percent(1.0_f32, 1.0_f32),
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    crate::forward_to_child!(child, rect);

    fn name(&self) -> &'static str {
        "Router"
    }

    fn test_id(&self) -> Option<&str> {
        Some("router")
    }

    fn on_nav_gesture(
        &mut self,
        direction: NavDirection,
        state: &AppModel,
        path: &ModelPath,
    ) -> bool {
        if self.child.on_nav_gesture(direction, state, path) {
            return true;
        }
        match direction {
            NavDirection::Back => self.handle.back(),
            NavDirection::Forward => self.handle.forward(),
        }
    }

    fn nav_availability(&self) -> crate::gesture::NavAvailability {
        self.child
            .nav_availability()
            .or(crate::gesture::NavAvailability {
                back: self.handle.can_back(),
                forward: self.handle.can_forward(),
            })
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

    fn dev_route(&self) -> Option<RouteSnapshot> {
        Some(self.handle.export_stack())
    }

    fn restore_dev_route(&mut self, route: &RouteSnapshot) -> bool {
        self.handle.adopt_stack(route).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_history_mutations_are_explicit_and_bounded() {
        // Case `route_history_push_replace_back_forward_and_reset_are_explicit`.
        {
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

        // Case `unknown_routes_and_duplicate_current_pushes_do_not_mutate_history`.
        {
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

        // Case `exhausted_revision_refuses_route_mutations_without_reusing_a_snapshot`.
        {
            let handle = RouteHandle::new("home", ["home", "settings"]).unwrap();
            handle.state.borrow_mut().revision = u64::MAX;
            let before = handle.history();
            assert_eq!(handle.push("settings"), Err(RouteError::RevisionExhausted));
            assert_eq!(handle.history(), before);
            assert!(!handle.back());
            assert!(!handle.forward());
        }
    }

    #[test]
    fn screen_builds_restore_the_last_valid_route_and_reset_focus_on_success() {
        // Case `failed_screen_build_restores_the_last_valid_route`.
        {
            let theme = Theme::default();
            let mut router = Router::new("home", ["home", "broken"], &theme, |route, _theme, _| {
                if route == "broken" {
                    Err("invalid screen".into())
                } else {
                    Ok(Box::new(
                        super::super::Text::new(route).with_role(crate::tokens::TextRole::Body),
                    ))
                }
            })
            .unwrap();
            let handle = router.handle();
            handle.push("broken").unwrap();
            let state = crate::State::new();
            let root = state.root.clone();
            router.sync(&state.doc, &root);
            assert_eq!(handle.current(), "home");
            assert!(matches!(
                router.last_error(),
                Some(RouteError::Build { .. })
            ));
            assert!(!router.take_structure_dirty());
        }

        // Case `failed_build_at_final_revision_keeps_route_and_retained_child_coherent`.
        {
            let theme = Theme::default();
            let mut router = Router::new("home", ["home", "broken"], &theme, |route, _theme, _| {
                if route == "broken" {
                    Err("invalid screen".into())
                } else {
                    Ok(Box::new(
                        super::super::Text::new(route).with_role(crate::tokens::TextRole::Body),
                    ))
                }
            })
            .unwrap();
            let handle = router.handle();
            handle.state.borrow_mut().revision = u64::MAX - 1;
            assert!(handle.push("broken").unwrap());
            let state = crate::State::new();
            let root = state.root.clone();
            router.sync(&state.doc, &root);
            assert_eq!(handle.current(), "home");
            assert_eq!(router.seen.revision, u64::MAX);
            assert_eq!(handle.snapshot().revision, u64::MAX);
            assert!(matches!(
                router.last_error(),
                Some(RouteError::Build { .. })
            ));
            assert!(!router.take_structure_dirty());
        }

        // Case `initial_deep_link_and_successful_transition_rebuild_and_reset_focus`.
        {
            let theme = Theme::default();
            let mut router =
                Router::new("settings", ["home", "settings"], &theme, |route, _, _| {
                    Ok(Box::new(super::super::Button::new(route, route)))
                })
                .unwrap();
            assert_eq!(router.handle().current(), "settings");

            router.handle().replace("home").unwrap();
            let state = crate::State::new();
            let root = state.root.clone();
            router.sync(&state.doc, &root);
            assert!(router.take_structure_dirty());
            assert!(router.take_focus_reset());
            assert!(!router.take_focus_reset(), "focus reset is edge-triggered");
        }
    }
}

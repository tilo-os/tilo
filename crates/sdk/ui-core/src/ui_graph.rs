//! Versioned, serializable UI object graphs compiled into ordinary widgets.
//!
//! The graph is a source format, not a second rendering system. [`UiNode`]
//! variants build the same [`crate::widget::Widget`] implementations an app
//! constructs in Rust, so accessibility, focus, model bindings, actions,
//! themes, inspection, and headless tests keep one implementation.

use crate::LocalState;
use crate::theme::Theme;
use crate::time::SystemTime;
use crate::tokens::TextRole;
use crate::widget::{AppModel, ModelPath};
use crate::widget::{
    Button, ButtonRole, Checkbox, Column, Container, Element, Row, Scrollable, Spacer, Text,
    TextInput, Toggle, Widget, labeled_check, labeled_field,
};
use automerge::{AutoCommit, ObjId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::kurbo::Rect;

/// Directory of live `*.json` screen graphs. When set, graph apps load and
/// hot-reload screens from disk instead of the embed baked by `include_str!`.
pub const LIVE_GRAPH_DIR_ENV: &str = "TILO_UI_GRAPH_DIR";

/// Operation type for live graph mutations published by [`LiveUi`].
pub const LIVE_GRAPH_OPERATION: &str = "tilo.ui.graph";

pub use tilo_ui_graph::{
    UI_GRAPH_VERSION, UiButtonRole, UiDocument, UiGraphEdit, UiGraphError, UiNavigation, UiNode,
    UiTextRole, UiTextSource, parse_ui_graph_edits,
};

/// Inputs passed to one app-owned serialized-graph component factory.
#[derive(Clone, Copy, Debug)]
pub struct UiComponentRequest<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub props: &'a serde_json::Map<String, serde_json::Value>,
    /// The graph's typed values, for a component's own text inputs.
    pub values: &'a UiValues,
}

/// A graph's text and checked values, by each control's `value_path`, and
/// which buttons were clicked, by each button's `on_click_path`.
///
/// The values are view state, never written to the document: every input
/// naming one path shows and edits the same value, and the app reads them
/// when it acts on them, submitting its own resource actions. A path the app
/// routes ([`Self::route`]) instead shows the app's own value, and each edit
/// of it is the app's action. A clicked button is reported the same way: the
/// app takes the click ([`Self::take_click`]) and acts on it.
#[derive(Clone, Default)]
pub struct UiValues {
    texts: Rc<RefCell<HashMap<String, LocalState<String>>>>,
    routes: Rc<RefCell<HashMap<String, tilo_actions::ActionBinding<String>>>>,
    booleans: Rc<RefCell<HashMap<String, LocalState<bool>>>>,
    bool_routes: Rc<RefCell<HashMap<String, tilo_actions::ActionBinding<bool>>>>,
    clicks: Rc<RefCell<HashMap<String, String>>>,
}

impl UiValues {
    /// The value inputs naming `path` edit, starting empty.
    #[must_use]
    pub fn text(&self, path: &str) -> LocalState<String> {
        self.texts
            .borrow_mut()
            .entry(path.to_owned())
            .or_insert_with(|| LocalState::new(String::new()))
            .clone()
    }

    /// What is typed at `path`, if an input names it.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<String> {
        self.texts.borrow().get(path).map(LocalState::get)
    }

    /// Route the inputs naming `path` through the app: they show `shown`,
    /// which the app keeps current, and each edit invokes `action` with the
    /// whole new text, so it is checked, attributed and undone like every
    /// other edit of the app.
    pub fn route(
        &self,
        path: &str,
        shown: LocalState<String>,
        action: tilo_actions::ActionBinding<String>,
    ) {
        self.texts.borrow_mut().insert(path.to_owned(), shown);
        self.routes.borrow_mut().insert(path.to_owned(), action);
    }

    /// The shared view value checked controls naming `path` edit, starting false.
    #[must_use]
    pub fn boolean(&self, path: &str) -> LocalState<bool> {
        self.booleans
            .borrow_mut()
            .entry(path.to_owned())
            .or_insert_with(|| LocalState::new(false))
            .clone()
    }

    /// The checked value at `path`, if a control names it.
    #[must_use]
    pub fn get_bool(&self, path: &str) -> Option<bool> {
        self.booleans.borrow().get(path).map(LocalState::get)
    }

    /// Route checked controls through the resource's typed action. The app keeps
    /// `shown` current, including refused edits, undo and remote changes.
    pub fn route_bool(
        &self,
        path: &str,
        shown: LocalState<bool>,
        action: tilo_actions::ActionBinding<bool>,
    ) {
        self.booleans.borrow_mut().insert(path.to_owned(), shown);
        self.bool_routes
            .borrow_mut()
            .insert(path.to_owned(), action);
    }

    /// A checkbox over the shared view value, invoking the app's action when routed.
    #[must_use]
    pub fn checkbox(&self, path: &str) -> Checkbox {
        let control = Checkbox::from_state(&self.boolean(path));
        match self.bool_routes.borrow().get(path) {
            Some(action) => control.on_action(action.clone()),
            None => control,
        }
    }

    /// A toggle over the shared view value, invoking the app's action when routed.
    #[must_use]
    pub fn toggle(&self, path: &str) -> Toggle {
        let control = Toggle::from_state(&self.boolean(path));
        match self.bool_routes.borrow().get(path) {
            Some(action) => control.on_action(action.clone()),
            None => control,
        }
    }

    /// Record a click of a button naming `path`, carrying `value`. Only the
    /// latest click per path is kept until the app takes it.
    pub fn click(&self, path: &str, value: &str) {
        self.clicks
            .borrow_mut()
            .insert(path.to_owned(), value.to_owned());
    }

    /// The latest unhandled click of a button naming `path`: its
    /// `click_value`, or `"true"` when it names none. Taking it clears it,
    /// so each click is acted on once.
    #[must_use]
    pub fn take_click(&self, path: &str) -> Option<String> {
        self.clicks.borrow_mut().remove(path)
    }

    /// An input over `path`: the app's value edited through its action when
    /// the path is routed, else the shared typed value, which starts as
    /// `initial`.
    #[must_use]
    pub fn input(&self, path: &str, initial: &str) -> TextInput {
        let value = self.text(path);
        if let Some(action) = self.routes.borrow().get(path) {
            return TextInput::from_state(&value).on_action(action.clone());
        }
        if value.with(String::is_empty) && !initial.is_empty() {
            value.set(initial.to_owned());
        }
        TextInput::from_state(&value)
    }
}

impl fmt::Debug for UiValues {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut paths: Vec<String> = self.texts.borrow().keys().cloned().collect();
        paths.sort_unstable();
        let mut routes: Vec<String> = self.routes.borrow().keys().cloned().collect();
        routes.sort_unstable();
        let mut clicks: Vec<String> = self.clicks.borrow().keys().cloned().collect();
        clicks.sort_unstable();
        let mut booleans: Vec<String> = self.booleans.borrow().keys().cloned().collect();
        booleans.sort_unstable();
        let mut bool_routes: Vec<String> = self.bool_routes.borrow().keys().cloned().collect();
        bool_routes.sort_unstable();
        formatter
            .debug_struct("UiValues")
            .field("paths", &paths)
            .field("routes", &routes)
            .field("booleans", &booleans)
            .field("bool_routes", &bool_routes)
            .field("clicks", &clicks)
            .finish()
    }
}

type UiComponentFactory =
    dyn for<'a> Fn(UiComponentRequest<'a>) -> Result<Element, String> + 'static;

/// Process-local services available while compiling a serialized graph.
#[derive(Clone, Default)]
pub struct UiBuildContext {
    router: Option<crate::widget::RouteHandle>,
    components: Rc<HashMap<String, Rc<UiComponentFactory>>>,
    component_placeholders: bool,
    values: UiValues,
}

impl fmt::Debug for UiBuildContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut components: Vec<_> = self.components.keys().collect();
        components.sort_unstable();
        formatter
            .debug_struct("UiBuildContext")
            .field("router", &self.router)
            .field("components", &components)
            .field("component_placeholders", &self.component_placeholders)
            .field("values", &self.values)
            .finish()
    }
}

impl UiBuildContext {
    #[must_use]
    pub fn with_router(mut self, router: crate::widget::RouteHandle) -> Self {
        self.router = Some(router);
        self
    }

    /// Register one app-owned component factory by its stable graph name.
    ///
    /// The factory receives the node id and its serialized props. Return a
    /// normal retained widget with that id so inspection, scenarios, and
    /// accessibility address the component consistently.
    #[must_use]
    pub fn with_component(
        mut self,
        name: impl Into<String>,
        factory: impl for<'a> Fn(UiComponentRequest<'a>) -> Result<Element, String> + 'static,
    ) -> Self {
        Rc::make_mut(&mut self.components).insert(name.into(), Rc::new(factory));
        self
    }

    /// Keep what the graph's inputs type in `values`, which the app reads.
    #[must_use]
    pub fn with_values(mut self, values: UiValues) -> Self {
        self.values = values;
        self
    }

    /// What the graph's inputs type, by their `value_path`.
    #[must_use]
    pub fn values(&self) -> &UiValues {
        &self.values
    }

    /// Render unresolved components as named text placeholders.
    ///
    /// Intended for graph editors that cannot load application code. Normal
    /// app builds reject an unregistered component instead.
    #[must_use]
    pub fn with_component_placeholders(mut self) -> Self {
        self.component_placeholders = true;
        self
    }
}

impl From<UiButtonRole> for ButtonRole {
    fn from(value: UiButtonRole) -> Self {
        match value {
            UiButtonRole::Primary => Self::Primary,
            UiButtonRole::Secondary => Self::Secondary,
            UiButtonRole::Destructive => Self::Destructive,
            UiButtonRole::Plain => Self::Plain,
        }
    }
}

impl From<UiTextRole> for TextRole {
    fn from(value: UiTextRole) -> Self {
        match value {
            UiTextRole::Display => Self::Display,
            UiTextRole::LargeTitle => Self::LargeTitle,
            UiTextRole::Title => Self::Title,
            UiTextRole::Heading => Self::Heading,
            UiTextRole::Subheading => Self::Subheading,
            UiTextRole::Body => Self::Body,
            UiTextRole::Label => Self::Label,
            UiTextRole::Caption => Self::Caption,
            UiTextRole::Reading => Self::Reading,
            UiTextRole::Code => Self::Code,
        }
    }
}

/// Compile a validated serialized graph into ordinary Tilo widgets.
pub trait UiDocumentExt {
    /// Build the retained widget subtree represented by this graph.
    ///
    /// # Errors
    ///
    /// Returns a schema validation error before any widget is constructed.
    fn build(&self, theme: &Theme) -> Result<Element, UiGraphError>;

    /// Build with process-local services such as screen navigation.
    ///
    /// # Errors
    ///
    /// Returns a schema validation error before any widget is constructed.
    fn build_with(&self, theme: &Theme, context: &UiBuildContext) -> Result<Element, UiGraphError>;
}

impl UiDocumentExt for UiDocument {
    fn build(&self, theme: &Theme) -> Result<Element, UiGraphError> {
        self.build_with(theme, &UiBuildContext::default())
    }

    fn build_with(
        &self,
        _theme: &Theme,
        context: &UiBuildContext,
    ) -> Result<Element, UiGraphError> {
        self.validate()?;
        build_node(&self.root, context)
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive source-node-to-widget match keeps graph construction reviewable and compiler-checked"
)]
fn build_node(node: &UiNode, context: &UiBuildContext) -> Result<Element, UiGraphError> {
    Ok(match node {
        UiNode::Column {
            id,
            children,
            gap,
            no_padding,
            intrinsic_height,
            fill_remaining_height,
        } => {
            let mut column = Column::new().with_id(id);
            if let Some(gap) = gap {
                column = column.with_gap(*gap);
            }
            if *no_padding {
                column = column.with_no_padding();
            }
            if *intrinsic_height {
                column = column.with_intrinsic_height();
            }
            if *fill_remaining_height {
                column = column.with_fill_remaining_height();
            }
            for child in children {
                column = column.push(build_node(child, context)?);
            }
            Box::new(column)
        }
        UiNode::Row {
            id,
            children,
            no_wrap,
            fill_remaining_height,
        } => {
            let mut row = Row::new().with_id(id);
            if *no_wrap {
                row = row.no_wrap();
            }
            if *fill_remaining_height {
                row = row.with_fill_remaining_height();
            }
            for child in children {
                row = row.push(build_node(child, context)?);
            }
            Box::new(row)
        }
        UiNode::Container {
            id,
            child,
            background,
            no_padding,
            fill_height,
            width,
            height,
        } => {
            let mut container = Container::new()
                .with_id(id)
                .with_child(build_node(child, context)?);
            if *background {
                container = container.with_background();
            }
            if *no_padding {
                container = container.with_no_padding();
            }
            if *fill_height {
                container = container.filling();
            }
            if let Some(width) = width {
                container = container.with_width(*width);
            }
            if let Some(height) = height {
                container = container.with_height(*height);
            }
            Box::new(container)
        }
        UiNode::Text { id, text, role } => {
            let text = match text {
                UiTextSource::Static { value } => Text::new(value),
                UiTextSource::Document { path } => Text::from_document_string(path),
            };
            Box::new(text.with_role((*role).into()).with_id(id))
        }
        UiNode::Button {
            id,
            label,
            accessible_name,
            tooltip,
            on_click_path,
            click_value,
            navigate,
            enabled,
            role,
        } => {
            let navigation_bound = navigate.as_ref().is_none_or(|navigation| {
                context
                    .router
                    .as_ref()
                    .is_some_and(|router| match navigation {
                        UiNavigation::Push { route } | UiNavigation::Replace { route } => {
                            router.knows(route)
                        }
                        UiNavigation::Back | UiNavigation::Forward => true,
                    })
            });
            let mut button = Button::new(id, label)
                .with_enabled(*enabled && (navigation_bound || on_click_path.is_some()))
                .role((*role).into());
            if let Some(name) = accessible_name {
                button = button.with_accessible_name(name);
            }
            if let Some(tooltip) = tooltip {
                button = button.tooltip(tooltip);
            }
            // A click path reports the click to the app through its values;
            // the button never writes the document.
            let record: Option<Box<dyn Fn()>> = on_click_path.as_ref().map(|path| {
                let values = context.values.clone();
                let path = path.clone();
                let value = click_value.clone().unwrap_or_else(|| "true".to_owned());
                Box::new(move || values.click(&path, &value)) as Box<dyn Fn()>
            });
            let navigation: Option<Box<dyn Fn()>> = match (navigate, context.router.as_ref()) {
                (Some(UiNavigation::Push { route }), Some(router)) if router.knows(route) => {
                    let (router, route) = (router.clone(), route.clone());
                    Some(Box::new(move || {
                        let _ = router.push(route.clone());
                    }))
                }
                (Some(UiNavigation::Replace { route }), Some(router)) if router.knows(route) => {
                    let (router, route) = (router.clone(), route.clone());
                    Some(Box::new(move || {
                        let _ = router.replace(route.clone());
                    }))
                }
                (Some(UiNavigation::Back), Some(router)) => {
                    let router = router.clone();
                    Some(Box::new(move || {
                        let _ = router.back();
                    }))
                }
                (Some(UiNavigation::Forward), Some(router)) => {
                    let router = router.clone();
                    Some(Box::new(move || {
                        let _ = router.forward();
                    }))
                }
                _ => None,
            };
            // A published view action, so the palette, agents and the native
            // audit reach the button as they reach any control.
            if record.is_some() || navigation.is_some() {
                button = button.on_view(move || {
                    if let Some(record) = &record {
                        record();
                    }
                    if let Some(navigate) = &navigation {
                        navigate();
                    }
                });
            }
            Box::new(button)
        }
        UiNode::TextInput {
            id,
            label,
            accessible_name,
            placeholder,
            value_path,
            initial_value,
            multiline,
            disabled,
        } => {
            let name = label.as_deref().unwrap_or(accessible_name);
            let input = match value_path {
                Some(path) => context.values.input(path, initial_value),
                None => TextInput::local().with_initial_value(initial_value),
            }
            .with_id(id)
            .with_accessible_name(name)
            .with_placeholder(placeholder)
            .multiline(*multiline)
            .disabled(*disabled);
            if let Some(label) = label {
                Box::new(labeled_field(label, input))
            } else {
                Box::new(input)
            }
        }
        UiNode::Checkbox {
            id,
            label,
            accessible_name,
            value_path,
            enabled,
        } => {
            let name = label.as_deref().unwrap_or(accessible_name);
            let control = context
                .values
                .checkbox(value_path)
                .with_id(id)
                .with_accessible_name(name)
                .with_enabled(*enabled);
            if let Some(label) = label {
                Box::new(labeled_check(label, control))
            } else {
                Box::new(control)
            }
        }
        UiNode::Toggle {
            id,
            label,
            accessible_name,
            value_path,
            enabled,
        } => {
            let name = label.as_deref().unwrap_or(accessible_name);
            let control = context
                .values
                .toggle(value_path)
                .with_id(id)
                .with_accessible_name(name)
                .with_enabled(*enabled);
            if let Some(label) = label {
                Box::new(labeled_check(label, control))
            } else {
                Box::new(control)
            }
        }
        UiNode::Component { id, name, props } => {
            if let Some(factory) = context.components.get(name) {
                factory(UiComponentRequest {
                    id,
                    name,
                    props,
                    values: &context.values,
                })
                .map_err(|message| UiGraphError::ComponentBuild {
                    id: id.clone(),
                    name: name.clone(),
                    message,
                })?
            } else if context.component_placeholders {
                Box::new(
                    Text::new(&format!("Component: {name}"))
                        .with_role(TextRole::Label)
                        .with_id(id),
                )
            } else {
                return Err(UiGraphError::UnknownComponent {
                    id: id.clone(),
                    name: name.clone(),
                });
            }
        }
        UiNode::Spacer { id } => Box::new(Spacer::new().with_id(id)),
    })
}

/// Reconcile a retained tree with a new source graph, reusing widgets whose
/// source nodes are unchanged and transferring ephemeral state across leaf
/// rebuilds that keep the same id.
fn reconcile_node(
    old: Element,
    old_node: &UiNode,
    new_node: &UiNode,
    context: &UiBuildContext,
) -> Result<(Element, bool), UiGraphError> {
    if old_node == new_node {
        return Ok((old, false));
    }
    if std::mem::discriminant(old_node) != std::mem::discriminant(new_node)
        || old_node.id() != new_node.id()
    {
        return Ok((build_node(new_node, context)?, true));
    }
    match (old_node, new_node) {
        (
            UiNode::Column {
                children: old_children,
                ..
            },
            UiNode::Column { .. },
        ) => reconcile_column(old, old_children, new_node, context),
        (
            UiNode::Row {
                children: old_children,
                ..
            },
            UiNode::Row { .. },
        ) => reconcile_row(old, old_children, new_node, context),
        (
            UiNode::Container {
                child: old_child_node,
                ..
            },
            UiNode::Container { .. },
        ) => reconcile_container(old, old_child_node, new_node, context),
        _ => {
            let snapshot = widget_state_snapshot(old.as_ref());
            let focused = focused_widget_snapshot(old.as_ref());
            let mut fresh = build_node(new_node, context)?;
            restore_widget_states(fresh.as_mut(), &snapshot);
            if let Some((id, state)) = focused {
                restore_widget_focus(fresh.as_mut(), &id, state.as_deref());
            }
            Ok((fresh, false))
        }
    }
}

/// Rebuilding `new_node` from scratch is the fallback whenever the retained
/// widget is not the matching kind.
fn reconcile_column(
    mut old: Element,
    old_children: &[UiNode],
    new_node: &UiNode,
    context: &UiBuildContext,
) -> Result<(Element, bool), UiGraphError> {
    let (
        UiNode::Column {
            id,
            children: new_children,
            gap,
            no_padding,
            intrinsic_height,
            fill_remaining_height,
        },
        Some(column),
    ) = (
        new_node,
        old.as_any_mut()
            .and_then(|any| any.downcast_mut::<Column>()),
    )
    else {
        return Ok((build_node(new_node, context)?, true));
    };
    let previous = column.take_children();
    let (next, topology) = reconcile_child_list(previous, old_children, new_children, context)?;
    column.set_children(next);
    column.apply_graph_props(
        id,
        *gap,
        *no_padding,
        *intrinsic_height,
        *fill_remaining_height,
    );
    Ok((old, topology))
}

fn reconcile_row(
    mut old: Element,
    old_children: &[UiNode],
    new_node: &UiNode,
    context: &UiBuildContext,
) -> Result<(Element, bool), UiGraphError> {
    let (
        UiNode::Row {
            id,
            children: new_children,
            no_wrap,
            fill_remaining_height,
        },
        Some(row),
    ) = (
        new_node,
        old.as_any_mut().and_then(|any| any.downcast_mut::<Row>()),
    )
    else {
        return Ok((build_node(new_node, context)?, true));
    };
    let previous = row.take_children();
    let (next, topology) = reconcile_child_list(previous, old_children, new_children, context)?;
    row.set_children(next);
    row.apply_graph_props(id, *no_wrap, *fill_remaining_height);
    Ok((old, topology))
}

fn reconcile_container(
    mut old: Element,
    old_child_node: &UiNode,
    new_node: &UiNode,
    context: &UiBuildContext,
) -> Result<(Element, bool), UiGraphError> {
    let (
        UiNode::Container {
            id,
            child: new_child_node,
            background,
            no_padding,
            fill_height,
            width,
            height,
        },
        Some(container),
    ) = (
        new_node,
        old.as_any_mut()
            .and_then(|any| any.downcast_mut::<Container>()),
    )
    else {
        return Ok((build_node(new_node, context)?, true));
    };
    let previous = container
        .take_child()
        .unwrap_or_else(|| Box::new(Spacer::new()));
    let (child, topology) = reconcile_node(previous, old_child_node, new_child_node, context)?;
    container.set_child(child);
    container.apply_graph_props(id, *background, *no_padding, *fill_height, *width, *height);
    Ok((old, topology))
}

fn reconcile_child_list(
    previous: Vec<Element>,
    old_children: &[UiNode],
    new_children: &[UiNode],
    context: &UiBuildContext,
) -> Result<(Vec<Element>, bool), UiGraphError> {
    let mut by_id = HashMap::with_capacity(old_children.len());
    for (node, element) in old_children.iter().cloned().zip(previous) {
        by_id.insert(node.id().to_owned(), (node, element));
    }
    let old_ids: Vec<&str> = old_children.iter().map(UiNode::id).collect();
    let new_ids: Vec<&str> = new_children.iter().map(UiNode::id).collect();
    let mut topology = old_ids != new_ids;
    let mut next = Vec::with_capacity(new_children.len());
    for child_node in new_children {
        if let Some((prev_node, prev_element)) = by_id.remove(child_node.id()) {
            let (element, child_topology) =
                reconcile_node(prev_element, &prev_node, child_node, context)?;
            topology |= child_topology;
            next.push(element);
        } else {
            topology = true;
            next.push(build_node(child_node, context)?);
        }
    }
    if !by_id.is_empty() {
        topology = true;
    }
    Ok((next, topology))
}

fn widget_state_snapshot(widget: &dyn Widget) -> HashMap<String, Option<String>> {
    let mut states = HashMap::new();
    fn walk(widget: &dyn Widget, states: &mut HashMap<String, Option<String>>) {
        if let Some(id) = widget.test_id() {
            states.insert(id.to_owned(), crate::undo::get_state(widget));
        }
        widget.for_each_child(&mut |child| walk(child, states));
    }
    walk(widget, &mut states);
    states
}

fn restore_widget_states(widget: &mut dyn Widget, states: &HashMap<String, Option<String>>) {
    if let Some(id) = widget.test_id().map(str::to_owned)
        && let Some(Some(state)) = states.get(&id)
    {
        crate::undo::set_state(widget, state);
    }
    for child in widget.children_mut() {
        restore_widget_states(child, states);
    }
}

#[derive(Clone)]
pub struct UiGraphHandle {
    inner: Arc<RwLock<UiGraphState>>,
}

#[derive(Clone)]
struct UiGraphState {
    document: UiDocument,
    revision: u64,
    undo: Vec<UiDocument>,
    redo: Vec<UiDocument>,
    active_edit_group: Option<String>,
    live_path: Option<PathBuf>,
    live_mtime: Option<SystemTime>,
}

const UI_GRAPH_HISTORY_LIMIT: usize = 100;

impl UiGraphHandle {
    /// Create a live handle after validating its complete source graph.
    ///
    /// # Errors
    ///
    /// Returns the graph's validation error.
    pub fn new(document: UiDocument) -> Result<Self, UiGraphError> {
        document.validate()?;
        Ok(Self {
            inner: Arc::new(RwLock::new(UiGraphState {
                document,
                revision: 0,
                undo: Vec::new(),
                redo: Vec::new(),
                active_edit_group: None,
                live_path: None,
                live_mtime: None,
            })),
        })
    }

    /// Parse and validate a live graph handle from JSON.
    ///
    /// # Errors
    ///
    /// Returns a JSON or graph validation error.
    pub fn from_json(source: &str) -> Result<Self, UiGraphError> {
        Self::new(UiDocument::from_json(source)?)
    }

    /// Load a screen from the host's live graph directory when one is set,
    /// otherwise from the embedded source. The native runner fills that
    /// directory from `TILO_UI_GRAPH_DIR`.
    ///
    /// # Errors
    ///
    /// Returns a JSON, I/O, or graph validation error.
    pub fn from_embedded_or_live(screen: &str, embedded: &str) -> Result<Self, UiGraphError> {
        if let Some(dir) = crate::host::environment().ui_graph_dir.clone() {
            let path = dir.join(format!("{screen}.json"));
            return Self::open_live(path);
        }
        Self::from_json(embedded)
    }

    /// Open a graph file and keep reloading it when its mtime advances.
    ///
    /// Successful in-process edits are written back to the same path so the
    /// builder, `tilo-app ui apply`, and the running app share one document.
    ///
    /// # Errors
    ///
    /// Returns an I/O, JSON, or graph validation error.
    pub fn open_live(path: impl Into<PathBuf>) -> Result<Self, UiGraphError> {
        let path = path.into();
        let source = std::fs::read_to_string(&path)
            .map_err(|error| UiGraphError::Io(format!("reading {}: {error}", path.display())))?;
        let document = UiDocument::from_json(&source)?;
        let live_mtime = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(crate::time::from_file_time);
        Ok(Self {
            inner: Arc::new(RwLock::new(UiGraphState {
                document,
                revision: 0,
                undo: Vec::new(),
                redo: Vec::new(),
                active_edit_group: None,
                live_path: Some(path),
                live_mtime,
            })),
        })
    }

    /// Path of the live on-disk graph, when this handle was opened with
    /// [`Self::open_live`].
    #[must_use]
    pub fn live_path(&self) -> Option<PathBuf> {
        self.inner
            .read()
            .ok()
            .and_then(|state| state.live_path.clone())
    }

    /// Whether the on-disk live graph is newer than the last load/persist.
    #[must_use]
    pub fn live_source_stale(&self) -> bool {
        let Ok(state) = self.inner.read() else {
            return false;
        };
        let Some(path) = state.live_path.as_ref() else {
            return false;
        };
        let Ok(mtime) = std::fs::metadata(path).and_then(|metadata| metadata.modified()) else {
            return false;
        };
        state.live_mtime != Some(crate::time::from_file_time(mtime))
    }

    /// Reload from disk when the live file's mtime is newer than the last load.
    ///
    /// External edits become the new history baseline. Returns `true` when the
    /// in-memory document changed.
    ///
    /// # Errors
    ///
    /// Returns an I/O, JSON, lock, or validation error.
    pub fn poll_live_source(&self) -> Result<bool, UiGraphError> {
        let path = {
            let state = self.inner.read().map_err(|_| UiGraphError::Poisoned)?;
            let Some(path) = state.live_path.clone() else {
                return Ok(false);
            };
            let Ok(metadata) = std::fs::metadata(&path) else {
                return Ok(false);
            };
            let Ok(mtime) = metadata.modified() else {
                return Ok(false);
            };
            if state.live_mtime == Some(crate::time::from_file_time(mtime)) {
                return Ok(false);
            }
            path
        };
        let source = std::fs::read_to_string(&path)
            .map_err(|error| UiGraphError::Io(format!("reading {}: {error}", path.display())))?;
        let document = UiDocument::from_json(&source)?;
        let mtime = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(crate::time::from_file_time);
        let mut state = self.inner.write().map_err(|_| UiGraphError::Poisoned)?;
        if state.live_mtime == mtime && state.document == document {
            return Ok(false);
        }
        if state.document == document {
            state.live_mtime = mtime;
            return Ok(false);
        }
        advance_graph_revision(&mut state.revision)?;
        state.document = document;
        state.live_mtime = mtime;
        state.undo.clear();
        state.redo.clear();
        state.active_edit_group = None;
        drop(state);
        Ok(true)
    }

    /// Apply a batch of tagged edits and persist when this handle is live.
    ///
    /// # Errors
    ///
    /// Returns a lock, edit, validation, or I/O error.
    pub fn apply_edits(&self, edits: Vec<UiGraphEdit>) -> Result<(), UiGraphError> {
        self.edit(|candidate| candidate.apply_edits(edits))
    }

    /// Snapshot the current source document.
    ///
    /// # Errors
    ///
    /// Returns [`UiGraphError::Poisoned`] if another thread panicked while
    /// holding the graph lock.
    pub fn document(&self) -> Result<UiDocument, UiGraphError> {
        self.inner
            .read()
            .map(|state| state.document.clone())
            .map_err(|_| UiGraphError::Poisoned)
    }

    /// Serialize the current source document as formatted JSON.
    ///
    /// # Errors
    ///
    /// Returns a graph lock or serialization error.
    pub fn to_json_pretty(&self) -> Result<String, UiGraphError> {
        self.document()?.to_json_pretty()
    }

    /// Inspect one source node without exposing the handle's lock or mutable
    /// live state. The returned value is an editable snapshot.
    ///
    /// # Errors
    ///
    /// Returns a graph lock error or [`UiGraphError::UnknownNode`].
    pub fn node(&self, id: &str) -> Result<UiNode, UiGraphError> {
        self.document()?
            .node(id)
            .cloned()
            .ok_or_else(|| UiGraphError::UnknownNode(id.to_owned()))
    }

    /// Validate and atomically replace the complete source graph.
    ///
    /// # Errors
    ///
    /// Returns a validation or graph lock error.
    pub fn replace(&self, document: UiDocument) -> Result<(), UiGraphError> {
        document.validate()?;
        let mut state = self.inner.write().map_err(|_| UiGraphError::Poisoned)?;
        if state.document == document {
            return Ok(());
        }
        advance_graph_revision(&mut state.revision)?;
        let previous = std::mem::replace(&mut state.document, document);
        push_graph_history(&mut state.undo, previous);
        state.redo.clear();
        state.active_edit_group = None;
        drop(state);
        self.persist_live()
    }

    /// Whether the live graph has a successful edit that can be undone.
    ///
    /// Invalid and no-op edits never enter history.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        self.inner.read().is_ok_and(|state| !state.undo.is_empty())
    }

    /// Whether an undone edit can be reapplied.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        self.inner.read().is_ok_and(|state| !state.redo.is_empty())
    }

    /// Restore the graph before its most recent successful edit.
    ///
    /// The restored document is published atomically and observed by every
    /// [`LiveUi`] and inspector sharing this handle. Returns `false` when the
    /// history is already at its oldest retained document.
    ///
    /// # Errors
    ///
    /// Returns [`UiGraphError::Poisoned`] if another thread panicked while
    /// holding the graph lock.
    pub fn undo(&self) -> Result<bool, UiGraphError> {
        self.step_history(crate::undo::UndoRequest::Undo)
    }

    /// Reapply the most recently undone graph edit.
    ///
    /// Returns `false` when there is no newer retained document.
    ///
    /// # Errors
    ///
    /// Returns [`UiGraphError::Poisoned`] if another thread panicked while
    /// holding the graph lock.
    pub fn redo(&self) -> Result<bool, UiGraphError> {
        self.step_history(crate::undo::UndoRequest::Redo)
    }

    /// Move the graph one retained document back (undo) or forward (redo),
    /// keeping the replaced document on the opposite stack.
    fn step_history(&self, request: crate::undo::UndoRequest) -> Result<bool, UiGraphError> {
        let mut guard = self.inner.write().map_err(|_| UiGraphError::Poisoned)?;
        let state = &mut *guard;
        let (from, to) = match request {
            crate::undo::UndoRequest::Undo => (&mut state.undo, &mut state.redo),
            crate::undo::UndoRequest::Redo => (&mut state.redo, &mut state.undo),
        };
        if from.is_empty() {
            return Ok(false);
        }
        advance_graph_revision(&mut state.revision)?;
        let Some(document) = from.pop() else {
            return Ok(false);
        };
        let current = std::mem::replace(&mut state.document, document);
        push_graph_history(to, current);
        state.active_edit_group = None;
        drop(guard);
        self.persist_live()?;
        Ok(true)
    }

    /// Forget retained undo and redo documents without changing the graph.
    ///
    /// A builder can call this after establishing a newly loaded or saved
    /// document as its history boundary.
    ///
    /// # Errors
    ///
    /// Returns [`UiGraphError::Poisoned`] if another thread panicked while
    /// holding the graph lock.
    pub fn clear_history(&self) -> Result<(), UiGraphError> {
        let mut state = self.inner.write().map_err(|_| UiGraphError::Poisoned)?;
        state.undo.clear();
        state.redo.clear();
        state.active_edit_group = None;
        drop(state);
        Ok(())
    }

    /// End a continuous edit without changing the document or its history.
    ///
    /// Call this when a text field loses focus or a drag gesture ends so a
    /// later edit using the same group key starts a new undo step.
    ///
    /// # Errors
    ///
    /// Returns [`UiGraphError::Poisoned`] if another thread panicked while
    /// holding the graph lock.
    pub fn finish_edit_group(&self) -> Result<(), UiGraphError> {
        let mut state = self.inner.write().map_err(|_| UiGraphError::Poisoned)?;
        state.active_edit_group = None;
        drop(state);
        Ok(())
    }

    /// Mutate one source node, validate the complete candidate graph, then
    /// publish it atomically. A failed edit leaves the live graph unchanged.
    ///
    /// # Errors
    ///
    /// Returns a graph lock, unknown-node, concurrent-edit, or validation
    /// error.
    pub fn update_node(
        &self,
        id: &str,
        update: impl FnOnce(&mut UiNode),
    ) -> Result<(), UiGraphError> {
        self.edit(|candidate| {
            let node = candidate
                .node_mut(id)
                .ok_or_else(|| UiGraphError::UnknownNode(id.to_owned()))?;
            update(node);
            Ok(())
        })
    }

    /// Mutate one node as part of a continuous undo group.
    ///
    /// Consecutive successful edits with the same `group` publish their live
    /// results normally but share one undo baseline. A different grouped edit,
    /// any ordinary graph mutation, undo/redo, or [`Self::finish_edit_group`]
    /// ends the group. Failed and no-op candidates never affect history.
    ///
    /// This is intended for property typing and drag gestures; use
    /// [`Self::update_node`] for discrete operations.
    ///
    /// # Errors
    ///
    /// Returns a graph lock, unknown-node, concurrent-edit, or validation
    /// error.
    pub fn update_node_grouped(
        &self,
        id: &str,
        group: impl Into<String>,
        update: impl FnOnce(&mut UiNode),
    ) -> Result<(), UiGraphError> {
        let group = group.into();
        self.edit_with_group(Some(&group), |candidate| {
            let node = candidate
                .node_mut(id)
                .ok_or_else(|| UiGraphError::UnknownNode(id.to_owned()))?;
            update(node);
            Ok(())
        })
    }

    /// Atomically insert a subtree into a live row or column.
    ///
    /// # Errors
    ///
    /// Returns the corresponding [`UiDocument::insert_child`] error, a lock
    /// error, or [`UiGraphError::ConcurrentEdit`].
    pub fn insert_child(
        &self,
        parent_id: &str,
        index: usize,
        node: UiNode,
    ) -> Result<(), UiGraphError> {
        self.edit(|candidate| candidate.insert_child(parent_id, index, node))
    }

    /// Atomically remove and return a live subtree.
    ///
    /// # Errors
    ///
    /// Returns the corresponding [`UiDocument::remove_node`] error, a lock
    /// error, or [`UiGraphError::ConcurrentEdit`].
    pub fn remove_node(&self, id: &str) -> Result<UiNode, UiGraphError> {
        self.edit(|candidate| candidate.remove_node(id))
    }

    /// Atomically replace a live subtree, including the root.
    ///
    /// # Errors
    ///
    /// Returns the corresponding [`UiDocument::replace_node`] error, a lock
    /// error, or [`UiGraphError::ConcurrentEdit`].
    pub fn replace_node(&self, id: &str, replacement: UiNode) -> Result<(), UiGraphError> {
        self.edit(|candidate| candidate.replace_node(id, replacement))
    }

    /// Atomically reparent or reorder a live subtree.
    ///
    /// # Errors
    ///
    /// Returns the corresponding [`UiDocument::move_node`] error, a lock
    /// error, or [`UiGraphError::ConcurrentEdit`].
    pub fn move_node(
        &self,
        id: &str,
        new_parent_id: &str,
        index: usize,
    ) -> Result<(), UiGraphError> {
        self.edit(|candidate| candidate.move_node(id, new_parent_id, index))
    }

    /// Compile the live graph into ordinary retained widgets.
    ///
    /// # Errors
    ///
    /// Returns a graph validation or widget-construction error.
    pub fn widget(&self, theme: &Theme) -> Result<LiveUi, UiGraphError> {
        self.widget_with(theme, UiBuildContext::default())
    }

    /// Build a live graph with process-local services such as routing.
    ///
    /// # Errors
    ///
    /// Returns a graph validation or widget-construction error.
    pub fn widget_with(
        &self,
        theme: &Theme,
        context: UiBuildContext,
    ) -> Result<LiveUi, UiGraphError> {
        LiveUi::new(self.clone(), theme.clone(), context)
    }

    fn snapshot(&self) -> Result<(u64, UiDocument), UiGraphError> {
        self.inner
            .read()
            .map(|state| (state.revision, state.document.clone()))
            .map_err(|_| UiGraphError::Poisoned)
    }

    fn edit<T>(
        &self,
        edit: impl FnOnce(&mut UiDocument) -> Result<T, UiGraphError>,
    ) -> Result<T, UiGraphError> {
        self.edit_with_group(None, edit)
    }

    fn edit_with_group<T>(
        &self,
        group: Option<&str>,
        edit: impl FnOnce(&mut UiDocument) -> Result<T, UiGraphError>,
    ) -> Result<T, UiGraphError> {
        let (base_revision, mut candidate) = self.snapshot()?;
        let output = edit(&mut candidate)?;
        candidate.validate()?;
        let mut state = self.inner.write().map_err(|_| UiGraphError::Poisoned)?;
        if state.revision != base_revision {
            return Err(UiGraphError::ConcurrentEdit);
        }
        if state.document == candidate {
            return Ok(output);
        }
        advance_graph_revision(&mut state.revision)?;
        let previous = std::mem::replace(&mut state.document, candidate);
        let continues_group = group.is_some() && state.active_edit_group.as_deref() == group;
        if !continues_group {
            push_graph_history(&mut state.undo, previous);
        }
        state.redo.clear();
        state.active_edit_group = group.map(str::to_owned);
        drop(state);
        self.persist_live()?;
        Ok(output)
    }

    fn revision(&self) -> Option<u64> {
        self.inner.read().ok().map(|state| state.revision)
    }

    fn persist_live(&self) -> Result<(), UiGraphError> {
        let (path, json) = {
            let state = self.inner.read().map_err(|_| UiGraphError::Poisoned)?;
            let Some(path) = state.live_path.clone() else {
                return Ok(());
            };
            (path, state.document.to_json_pretty()?)
        };
        write_live_graph_atomic(&path, &json)?;
        let mtime = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(crate::time::from_file_time);
        let mut state = self.inner.write().map_err(|_| UiGraphError::Poisoned)?;
        state.live_mtime = mtime;
        drop(state);
        Ok(())
    }
}

fn write_live_graph_atomic(path: &Path, json: &str) -> Result<(), UiGraphError> {
    tilo_files::native::atomic::write(path, json.as_bytes())
        .map_err(|error| UiGraphError::Io(format!("publishing {}: {error}", path.display())))
}

fn advance_graph_revision(revision: &mut u64) -> Result<(), UiGraphError> {
    *revision = revision
        .checked_add(1)
        .ok_or(UiGraphError::RevisionExhausted)?;
    Ok(())
}

fn push_graph_history(history: &mut Vec<UiDocument>, document: UiDocument) {
    if history.len() == UI_GRAPH_HISTORY_LIMIT {
        history.remove(0);
    }
    history.push(document);
}

/// A retained host whose child is reconciled after a validated graph edit.
pub struct LiveUi {
    handle: UiGraphHandle,
    theme: Theme,
    context: UiBuildContext,
    seen_revision: u64,
    seen_document: UiDocument,
    child: Element,
    rect: Rect,
    structure_dirty: bool,
}

impl LiveUi {
    fn new(
        handle: UiGraphHandle,
        theme: Theme,
        context: UiBuildContext,
    ) -> Result<Self, UiGraphError> {
        let (seen_revision, document) = handle.snapshot()?;
        let child = document.build_with(&theme, &context)?;
        Ok(Self {
            handle,
            theme,
            context,
            seen_revision,
            seen_document: document,
            child,
            rect: Rect::ZERO,
            structure_dirty: false,
        })
    }

    #[must_use]
    pub fn handle(&self) -> UiGraphHandle {
        self.handle.clone()
    }

    fn rebuild_if_changed(&mut self) {
        let _ = self.handle.poll_live_source();
        let Ok((revision, document)) = self.handle.snapshot() else {
            return;
        };
        if revision == self.seen_revision {
            return;
        }
        // Handle updates validate before publication. Reconcile may still fail
        // after a toolkit/schema bug (and may have already taken children off
        // containers). Never leave a spacer or a half-built tree on screen.
        let old_child = std::mem::replace(&mut self.child, Box::new(Spacer::new()));
        match reconcile_node(
            old_child,
            &self.seen_document.root,
            &document.root,
            &self.context,
        ) {
            Ok((child, _topology_changed)) => {
                self.child = child;
                self.seen_revision = revision;
                self.seen_document = document;
                // Any published revision may have replaced Element boxes or
                // cleared container child NodeIds. App composition refreshes
                // the map; CompositionFocus restores caret by test id.
                self.structure_dirty = true;
            }
            Err(_) => match document.build_with(&self.theme, &self.context) {
                Ok(child) => {
                    self.child = child;
                    self.seen_revision = revision;
                    self.seen_document = document;
                    self.structure_dirty = true;
                }
                Err(_) => {
                    // Keep the last known-good tree and revision. A later
                    // sync will retry if the handle still exposes this
                    // published document.
                    if let Ok(good) = self.seen_document.build_with(&self.theme, &self.context) {
                        self.child = good;
                    }
                }
            },
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "graph action payloads are unit or already-validated JSON maps"
    )]
    fn graph_resource(&self) -> crate::tilo_actions::Resource {
        use crate::tilo_actions::{
            ActionContract, ActionParam, ActionSpec, DiscoveryScope, EncodedAction, OperationKind,
            ParamType, Resource, ResultSpec,
        };
        let id = self.seen_document.root.id();
        let mut resource = Resource::new(
            format!("tilo.ui-graph:{id}"),
            "tilo.ui-graph",
            format!("UI graph {id}"),
        );
        let dump = ActionSpec::operation(
            "dump",
            "Dump UI graph",
            OperationKind::View,
            format!("{LIVE_GRAPH_OPERATION}.dump"),
            &(),
        )
        .expect("unit dump payload is serializable");
        let mut apply = ActionSpec::typed_operation(
            "apply",
            "Apply UI graph edits",
            "JSON array of graph edits",
            OperationKind::Effect,
            format!("{LIVE_GRAPH_OPERATION}.apply"),
            vec![ActionParam::required_owned(
                "edits",
                ParamType::Text,
                "Graph edit batch JSON",
            )],
            |args| {
                EncodedAction::encode_operation(
                    format!("{LIVE_GRAPH_OPERATION}.apply"),
                    &args.to_json(),
                )
                .expect("apply arguments are serializable")
            },
        );
        apply.contract = ActionContract {
            discovery: DiscoveryScope::Launchable,
            result: ResultSpec::None,
            ..ActionContract::default()
        };
        let undo = ActionSpec::operation(
            "undo",
            "Undo UI graph edit",
            OperationKind::View,
            format!("{LIVE_GRAPH_OPERATION}.undo"),
            &(),
        )
        .expect("unit undo payload is serializable");
        let redo = ActionSpec::operation(
            "redo",
            "Redo UI graph edit",
            OperationKind::View,
            format!("{LIVE_GRAPH_OPERATION}.redo"),
            &(),
        )
        .expect("unit redo payload is serializable");
        resource.actions.extend([dump, apply, undo, redo]);
        resource
    }

    fn invoke_graph_operation(
        &mut self,
        type_id: &str,
        payload: &[u8],
    ) -> Result<crate::tilo_actions::ActionOutput, String> {
        use crate::tilo_actions::ActionOutput;
        let dump = format!("{LIVE_GRAPH_OPERATION}.dump");
        let apply = format!("{LIVE_GRAPH_OPERATION}.apply");
        let undo = format!("{LIVE_GRAPH_OPERATION}.undo");
        let redo = format!("{LIVE_GRAPH_OPERATION}.redo");
        if type_id == dump {
            let json = self
                .handle
                .to_json_pretty()
                .map_err(|error| error.to_string())?;
            return Ok(ActionOutput {
                value: Some(serde_json::Value::String(json)),
                resources: vec![format!("tilo.ui-graph:{}", self.seen_document.root.id())],
            });
        }
        if type_id == apply {
            let args: serde_json::Map<String, serde_json::Value> =
                serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            let edits_json = args
                .get("edits")
                .and_then(serde_json::Value::as_str)
                .ok_or("missing edits")?;
            let edits = parse_ui_graph_edits(edits_json).map_err(|error| error.to_string())?;
            self.handle
                .apply_edits(edits)
                .map_err(|error| error.to_string())?;
            self.rebuild_if_changed();
            return Ok(ActionOutput::default());
        }
        if type_id == undo {
            let changed = self.handle.undo().map_err(|error| error.to_string())?;
            if changed {
                self.rebuild_if_changed();
            }
            return Ok(ActionOutput {
                value: Some(serde_json::json!({ "changed": changed })),
                resources: Vec::new(),
            });
        }
        if type_id == redo {
            let changed = self.handle.redo().map_err(|error| error.to_string())?;
            if changed {
                self.rebuild_if_changed();
            }
            return Ok(ActionOutput {
                value: Some(serde_json::json!({ "changed": changed })),
                resources: Vec::new(),
            });
        }
        Err(format!("unknown UI graph operation {type_id:?}"))
    }
}

/// The style of a widget that fills the space its parent gives it.
fn filling() -> Style {
    Style {
        size: taffy::Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        ..Default::default()
    }
}

impl Widget for LiveUi {
    crate::provides!(dyn crate::resources::PublishesResources);

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
        crate::widget::TaffyTreeExt::widget_node(taffy, filling(), &[child], node_map, self)
    }

    crate::forward_to_child!(child, rect);

    fn name(&self) -> &'static str {
        "LiveUi"
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        own | self.child.take_structure_dirty()
    }

    fn needs_idle_sync(&self) -> bool {
        self.handle.live_source_stale()
            || self
                .handle
                .revision()
                .is_some_and(|revision| revision != self.seen_revision)
            || self.child.needs_idle_sync()
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            "tilo.ui.graph.dump",
            "tilo.ui.graph.apply",
            "tilo.ui.graph.undo",
            "tilo.ui.graph.redo",
        ]
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        if let Some(started) = crate::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op
            .unscoped()
            .filter(|request| request.type_id.starts_with(LIVE_GRAPH_OPERATION))
            .filter(|request| request.kind != crate::tilo_actions::OperationKind::Document)
        else {
            return Ok(None);
        };
        self.invoke_graph_operation(request.type_id, request.payload)
            .map(|output| Some(crate::widget::ResourceOperationStart::Completed(output)))
    }
}

impl crate::resources::PublishesResources for LiveUi {
    fn resources(&self, doc: &AutoCommit) -> Vec<crate::tilo_actions::Resource> {
        let mut resources = crate::resources::resources(self.child.as_ref(), doc);
        resources.push(self.graph_resource());
        resources
    }
}

type SaveGraph = Box<dyn FnMut(&str) -> Result<(), String>>;

struct UiGraphEditorState {
    selected: String,
    chrome_dirty: bool,
    suppress_graph_revision: Option<u64>,
    status: LocalState<String>,
    save: Option<SaveGraph>,
}

/// A native, accessible object-graph editor with a live production-widget
/// preview. It is also the root component used by the standalone UI builder.
pub struct UiGraphEditor {
    handle: UiGraphHandle,
    theme: Theme,
    shared: Rc<RefCell<UiGraphEditorState>>,
    seen_graph_revision: u64,
    child: Element,
    rect: Rect,
    structure_dirty: bool,
}

impl UiGraphEditor {
    /// Build an editor around an existing live graph.
    ///
    /// # Errors
    ///
    /// Returns a graph lock, validation, or widget-construction error.
    pub fn new(handle: UiGraphHandle, theme: Theme) -> Result<Self, UiGraphError> {
        let document = handle.document()?;
        let graph_revision = handle.revision().unwrap_or_default();
        let shared = Rc::new(RefCell::new(UiGraphEditorState {
            selected: document.root.id().to_owned(),
            chrome_dirty: false,
            suppress_graph_revision: None,
            status: LocalState::new("Ready".to_owned()),
            save: None,
        }));
        let child = build_editor_child(&handle, &shared, &theme)?;
        Ok(Self {
            handle,
            theme,
            shared,
            seen_graph_revision: graph_revision,
            child,
            rect: Rect::ZERO,
            structure_dirty: false,
        })
    }

    /// Install the persistence callback used by the editor's Save button.
    #[must_use]
    pub fn on_save(self, save: impl FnMut(&str) -> Result<(), String> + 'static) -> Self {
        self.shared.borrow_mut().save = Some(Box::new(save));
        self
    }

    #[must_use]
    pub fn handle(&self) -> UiGraphHandle {
        self.handle.clone()
    }

    fn rebuild_if_changed(&mut self) {
        let graph_revision = self.handle.revision().unwrap_or(self.seen_graph_revision);
        let (chrome_dirty, suppressed) = {
            let state = self.shared.borrow();
            (state.chrome_dirty, state.suppress_graph_revision)
        };
        if graph_revision != self.seen_graph_revision && suppressed == Some(graph_revision) {
            self.seen_graph_revision = graph_revision;
            self.shared.borrow_mut().suppress_graph_revision = None;
        }
        let graph_changed = graph_revision != self.seen_graph_revision;
        if !graph_changed && !chrome_dirty {
            return;
        }
        let focused = focused_widget_snapshot(self.child.as_ref());
        match build_editor_child(&self.handle, &self.shared, &self.theme) {
            Ok(mut child) => {
                if let Some((id, state)) = focused {
                    restore_widget_focus(child.as_mut(), &id, state.as_deref());
                }
                self.child = child;
                self.seen_graph_revision = graph_revision;
                self.shared.borrow_mut().chrome_dirty = false;
                self.structure_dirty = true;
            }
            Err(error) => self
                .shared
                .borrow()
                .status
                .set(format!("Cannot rebuild editor: {error}")),
        }
    }
}

fn widget_tree_has_focus(widget: &dyn Widget) -> bool {
    widget.is_focused() || widget.children().into_iter().any(widget_tree_has_focus)
}

fn focused_widget_snapshot(widget: &dyn Widget) -> Option<(String, Option<String>)> {
    if widget.is_focused()
        && let Some(id) = widget.test_id()
    {
        return Some((id.to_owned(), crate::undo::get_state(widget)));
    }
    widget
        .children()
        .into_iter()
        .find_map(focused_widget_snapshot)
}

fn restore_widget_focus(widget: &mut dyn Widget, id: &str, state: Option<&str>) -> bool {
    let Some(widget) = crate::widget::find_test_id_mut(widget, id) else {
        return false;
    };
    widget.set_focus(true);
    if let Some(state) = state {
        crate::undo::set_state(widget, state);
    }
    true
}

#[derive(Clone, Copy)]
enum InsertKind {
    Text,
    Button,
    Row,
    Column,
    Container,
    TextInput,
    Checkbox,
    Toggle,
    Component,
    Spacer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditableProperty {
    NodeId,
    ColumnGap,
    ContainerWidth,
    ContainerHeight,
    TextDocumentPath,
    ButtonLabel,
    ButtonAccessibleName,
    ButtonTooltip,
    ButtonClickPath,
    ButtonClickValue,
    ButtonNavigationRoute,
    StaticText,
    InputLabel,
    InputAccessibleName,
    InputPlaceholder,
    InputValuePath,
    InputInitialValue,
    CheckAccessibleName,
    CheckLabel,
    CheckValuePath,
    ComponentName,
    ComponentProps,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditableBool {
    ColumnNoPadding,
    ColumnIntrinsicHeight,
    ColumnFillRemainingHeight,
    RowNoWrap,
    RowFillRemainingHeight,
    ContainerBackground,
    ContainerNoPadding,
    ContainerFillHeight,
    ButtonEnabled,
    InputMultiline,
    InputDisabled,
    CheckEnabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditableChoice {
    TextSource,
    TextRole,
    ButtonRole,
    ButtonNavigation,
}

#[allow(
    clippy::too_many_lines,
    reason = "the builder intentionally keeps the inspector's complete declarative control tree together"
)]
fn build_editor_child(
    handle: &UiGraphHandle,
    shared: &Rc<RefCell<UiGraphEditorState>>,
    theme: &Theme,
) -> Result<Element, UiGraphError> {
    let document = handle.document()?;
    let selected = shared.borrow().selected.clone();
    let selected_node = document.node(&selected).unwrap_or(&document.root);
    let selected = selected_node.id().to_owned();
    if shared.borrow().selected != selected {
        shared.borrow_mut().selected.clone_from(&selected);
    }
    let mut flattened = Vec::new();
    flatten_nodes(&document.root, 0, &mut flattened);

    let mut hierarchy = Column::new().with_intrinsic_height();
    hierarchy = hierarchy.push(Box::new(Text::new("Hierarchy").with_role(TextRole::Title)));
    for (id, kind, depth) in flattened {
        let button_id = format!("builder-node-{id}");
        let label = format!("{}{}  {id}", "  ".repeat(depth), kind);
        let selected_row = id == selected;
        let state = Rc::clone(shared);
        let graph = handle.clone();
        let id_for_click = id.clone();
        hierarchy = hierarchy.push(Box::new(
            Button::new(&button_id, &label)
                .role(if selected_row {
                    ButtonRole::Primary
                } else {
                    ButtonRole::Plain
                })
                .on_click(move || {
                    if let Err(error) = graph.finish_edit_group() {
                        state
                            .borrow()
                            .status
                            .set(format!("Cannot finish property edit: {error}"));
                        return;
                    }
                    let mut state = state.borrow_mut();
                    state.selected.clone_from(&id_for_click);
                    state.chrome_dirty = true;
                }),
        ));
    }

    let can_add = matches!(selected_node, UiNode::Column { .. } | UiNode::Row { .. });
    let parent = parent_relation(&document.root, &selected);
    let can_delete = selected != document.root.id();
    let can_move_up = parent.as_ref().is_some_and(|(_, index, _)| *index > 0);
    let can_move_down = parent
        .as_ref()
        .is_some_and(|(_, index, len)| index.saturating_add(1) < *len);

    let mut toolbar = Row::new().no_wrap();
    toolbar = toolbar
        .push(Box::new(save_button(handle, shared)))
        .push(Box::new(undo_button(handle, shared)))
        .push(Box::new(redo_button(handle, shared)));
    for (id, label, kind) in [
        ("builder-add-text", "+ Text", InsertKind::Text),
        ("builder-add-button", "+ Button", InsertKind::Button),
        ("builder-add-row", "+ Row", InsertKind::Row),
        ("builder-add-column", "+ Column", InsertKind::Column),
        (
            "builder-add-container",
            "+ Container",
            InsertKind::Container,
        ),
        ("builder-add-input", "+ Input", InsertKind::TextInput),
        ("builder-add-checkbox", "+ Checkbox", InsertKind::Checkbox),
        ("builder-add-toggle", "+ Toggle", InsertKind::Toggle),
        (
            "builder-add-component",
            "+ Component",
            InsertKind::Component,
        ),
        ("builder-add-spacer", "+ Spacer", InsertKind::Spacer),
    ] {
        toolbar = toolbar.push(Box::new(add_button(
            id, label, kind, can_add, &selected, handle, shared,
        )));
    }
    toolbar = toolbar
        .push(Box::new(move_button(
            "builder-move-up",
            "Move up",
            can_move_up,
            -1,
            &selected,
            handle,
            shared,
        )))
        .push(Box::new(move_button(
            "builder-move-down",
            "Move down",
            can_move_down,
            1,
            &selected,
            handle,
            shared,
        )))
        .push(Box::new(delete_button(
            can_delete, &selected, handle, shared,
        )));

    let mut inspector = Column::new().with_intrinsic_height();
    inspector = inspector
        .push(Box::new(Text::new("Inspector").with_role(TextRole::Title)))
        .push(Box::new(Text::new(&format!(
            "{} · {}",
            node_kind(selected_node),
            selected_node.id()
        ))));
    let properties = editable_properties(selected_node);
    if properties.is_empty() {
        inspector = inspector.push(Box::new(Text::new(
            "This node has no inline text property. Structural controls remain available.",
        )));
    }
    for (property, value, label, input_id) in properties {
        let graph = handle.clone();
        let state = Rc::clone(shared);
        let history_group = format!("builder-property:{property:?}");
        inspector = inspector.push(Box::new(Text::new(label))).push(Box::new(
            TextInput::local()
                .with_id(input_id)
                .with_accessible_name(label)
                .with_initial_value(&value)
                .on_change(move |value| {
                    let parsed = parse_editable_property(property, value);
                    let status = state.borrow().status.clone();
                    let result = parsed.and_then(|parsed| {
                        let selected = state.borrow().selected.clone();
                        graph
                            .update_node_grouped(&selected, &history_group, |node| {
                                set_editable_property(node, property, parsed);
                            })
                            .map_err(|error| error.to_string())
                    });
                    match result {
                        Ok(()) => {
                            let mut editor = state.borrow_mut();
                            if property == EditableProperty::NodeId {
                                value.trim().clone_into(&mut editor.selected);
                                editor.chrome_dirty = true;
                            } else {
                                editor.suppress_graph_revision = graph.revision();
                            }
                            status.set("Preview updated".to_owned());
                        }
                        Err(error) => status.set(format!("Edit rejected: {error}")),
                    }
                }),
        ));
    }
    for (property, value, label, button_id) in editable_bools(selected_node) {
        inspector = inspector.push(Box::new(boolean_property_button(
            property, value, label, button_id, &selected, handle, shared,
        )));
    }
    for (property, value, label, button_id) in editable_choices(selected_node) {
        inspector = inspector.push(Box::new(choice_property_button(
            property, value, label, button_id, &selected, handle, shared,
        )));
    }
    inspector = inspector.push(Box::new(Text::from_state(
        &shared.borrow().status,
        Clone::clone,
    )));

    let hierarchy = Scrollable::new_vertical()
        .fill_parent_height()
        .with_child(Box::new(hierarchy));
    let inspector = Scrollable::new_vertical()
        .fill_parent_height()
        .with_child(Box::new(inspector));
    let preview = handle.widget_with(
        theme,
        UiBuildContext::default().with_component_placeholders(),
    )?;
    let body = Row::new()
        .with_fill_remaining_height()
        .no_wrap()
        .push(Box::new(
            Container::new()
                .with_width(300.0)
                .filling()
                .with_background()
                .with_child(Box::new(hierarchy)),
        ))
        .push(Box::new(
            Container::new()
                .filling()
                .with_background()
                .with_child(Box::new(preview)),
        ))
        .push(Box::new(
            Container::new()
                .with_width(320.0)
                .filling()
                .with_background()
                .with_child(Box::new(inspector)),
        ));
    Ok(Box::new(
        Column::new()
            .with_no_padding()
            .push(Box::new(toolbar))
            .push(Box::new(body)),
    ))
}

fn flatten_nodes(node: &UiNode, depth: usize, output: &mut Vec<(String, &'static str, usize)>) {
    output.push((node.id().to_owned(), node_kind(node), depth));
    for child in node.children() {
        flatten_nodes(child, depth.saturating_add(1), output);
    }
}

fn node_kind(node: &UiNode) -> &'static str {
    match node {
        UiNode::Column { .. } => "Column",
        UiNode::Row { .. } => "Row",
        UiNode::Container { .. } => "Container",
        UiNode::Text { .. } => "Text",
        UiNode::Button { .. } => "Button",
        UiNode::TextInput { .. } => "Text input",
        UiNode::Checkbox { .. } => "Checkbox",
        UiNode::Toggle { .. } => "Toggle",
        UiNode::Component { .. } => "Component",
        UiNode::Spacer { .. } => "Spacer",
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive property catalog makes every serializable node variant auditable"
)]
fn editable_properties(
    node: &UiNode,
) -> Vec<(EditableProperty, String, &'static str, &'static str)> {
    let mut properties = vec![(
        EditableProperty::NodeId,
        node.id().to_owned(),
        "Stable id",
        "builder-node-id",
    )];
    match node {
        UiNode::Column { gap, .. } => properties.push((
            EditableProperty::ColumnGap,
            optional_number(*gap),
            "Gap (optional px)",
            "builder-column-gap",
        )),
        UiNode::Container { width, height, .. } => {
            properties.push((
                EditableProperty::ContainerWidth,
                optional_number(*width),
                "Width (optional px)",
                "builder-container-width",
            ));
            properties.push((
                EditableProperty::ContainerHeight,
                optional_number(*height),
                "Height (optional px)",
                "builder-container-height",
            ));
        }
        UiNode::Text { text, .. } => {
            let (property, value, label, id) = match text {
                UiTextSource::Static { value } => (
                    EditableProperty::StaticText,
                    value.clone(),
                    "Text",
                    "builder-property",
                ),
                UiTextSource::Document { path } => (
                    EditableProperty::TextDocumentPath,
                    path.clone(),
                    "Document path",
                    "builder-text-path",
                ),
            };
            properties.push((property, value, label, id));
        }
        UiNode::Button {
            label,
            accessible_name,
            tooltip,
            on_click_path,
            click_value,
            navigate,
            ..
        } => {
            properties.extend([
                (
                    EditableProperty::ButtonLabel,
                    label.clone(),
                    "Label",
                    "builder-property",
                ),
                (
                    EditableProperty::ButtonAccessibleName,
                    accessible_name.clone().unwrap_or_default(),
                    "Accessible name (optional)",
                    "builder-button-accessible-name",
                ),
                (
                    EditableProperty::ButtonTooltip,
                    tooltip.clone().unwrap_or_default(),
                    "Tooltip (optional)",
                    "builder-button-tooltip",
                ),
                (
                    EditableProperty::ButtonClickPath,
                    on_click_path.clone().unwrap_or_default(),
                    "Click reported to the app as (optional)",
                    "builder-button-click-path",
                ),
                (
                    EditableProperty::ButtonClickValue,
                    click_value.clone().unwrap_or_default(),
                    "Click value (optional)",
                    "builder-button-click-value",
                ),
            ]);
            if let Some(UiNavigation::Push { route } | UiNavigation::Replace { route }) = navigate {
                properties.push((
                    EditableProperty::ButtonNavigationRoute,
                    route.clone(),
                    "Destination route",
                    "builder-button-route",
                ));
            }
        }
        UiNode::TextInput {
            label,
            accessible_name,
            placeholder,
            value_path,
            initial_value,
            ..
        } => properties.extend([
            (
                EditableProperty::InputLabel,
                label.clone().unwrap_or_default(),
                "Visible label (also the accessible name)",
                "builder-input-label",
            ),
            (
                EditableProperty::InputAccessibleName,
                accessible_name.clone(),
                "Accessible name (only without a visible label)",
                "builder-input-accessible-name",
            ),
            (
                EditableProperty::InputPlaceholder,
                placeholder.clone(),
                "Placeholder",
                "builder-property",
            ),
            (
                EditableProperty::InputValuePath,
                value_path.clone().unwrap_or_default(),
                "Document value path (optional)",
                "builder-input-value-path",
            ),
            (
                EditableProperty::InputInitialValue,
                initial_value.clone(),
                "Initial local value",
                "builder-input-initial-value",
            ),
        ]),
        UiNode::Checkbox {
            label,
            accessible_name,
            value_path,
            ..
        }
        | UiNode::Toggle {
            label,
            accessible_name,
            value_path,
            ..
        } => properties.extend([
            (
                EditableProperty::CheckLabel,
                label.clone().unwrap_or_default(),
                "Visible label (also the accessible name)",
                "builder-check-label",
            ),
            (
                EditableProperty::CheckAccessibleName,
                accessible_name.clone(),
                "Accessible name (only without a visible label)",
                "builder-check-accessible-name",
            ),
            (
                EditableProperty::CheckValuePath,
                value_path.clone(),
                "Document value path",
                "builder-check-value-path",
            ),
        ]),
        UiNode::Component { name, props, .. } => properties.extend([
            (
                EditableProperty::ComponentName,
                name.clone(),
                "Component name",
                "builder-component-name",
            ),
            (
                EditableProperty::ComponentProps,
                serde_json::to_string(props).unwrap_or_else(|_| "{}".to_owned()),
                "Props (JSON object)",
                "builder-component-props",
            ),
        ]),
        UiNode::Row { .. } | UiNode::Spacer { .. } => {}
    }
    properties
}

fn optional_number(value: Option<f32>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

enum ParsedProperty {
    Text(String),
    OptionalText(Option<String>),
    OptionalNumber(Option<f32>),
    JsonObject(serde_json::Map<String, serde_json::Value>),
}

fn parse_editable_property(
    property: EditableProperty,
    raw: &str,
) -> Result<ParsedProperty, String> {
    if property == EditableProperty::ComponentProps {
        return serde_json::from_str(raw)
            .map(ParsedProperty::JsonObject)
            .map_err(|error| format!("props must be a JSON object: {error}"));
    }
    if matches!(
        property,
        EditableProperty::ColumnGap
            | EditableProperty::ContainerWidth
            | EditableProperty::ContainerHeight
    ) {
        return if raw.trim().is_empty() {
            Ok(ParsedProperty::OptionalNumber(None))
        } else {
            raw.trim()
                .parse::<f32>()
                .map(|value| ParsedProperty::OptionalNumber(Some(value)))
                .map_err(|_| format!("{raw:?} is not a number"))
        };
    }
    if matches!(
        property,
        EditableProperty::ButtonAccessibleName
            | EditableProperty::ButtonTooltip
            | EditableProperty::ButtonClickPath
            | EditableProperty::ButtonClickValue
            | EditableProperty::InputLabel
            | EditableProperty::CheckLabel
            | EditableProperty::InputValuePath
    ) {
        return Ok(ParsedProperty::OptionalText(
            (!raw.trim().is_empty()).then(|| raw.trim().to_owned()),
        ));
    }
    let value = if matches!(
        property,
        EditableProperty::NodeId
            | EditableProperty::TextDocumentPath
            | EditableProperty::ButtonNavigationRoute
            | EditableProperty::CheckValuePath
            | EditableProperty::ComponentName
    ) {
        raw.trim().to_owned()
    } else {
        raw.to_owned()
    };
    Ok(ParsedProperty::Text(value))
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive property setter mirrors the property catalog above"
)]
fn set_editable_property(node: &mut UiNode, property: EditableProperty, value: ParsedProperty) {
    match (node, property, value) {
        (node, EditableProperty::NodeId, ParsedProperty::Text(value)) => set_node_id(node, value),
        (
            UiNode::Column { gap, .. },
            EditableProperty::ColumnGap,
            ParsedProperty::OptionalNumber(value),
        ) => *gap = value,
        (
            UiNode::Container { width, .. },
            EditableProperty::ContainerWidth,
            ParsedProperty::OptionalNumber(value),
        ) => *width = value,
        (
            UiNode::Container { height, .. },
            EditableProperty::ContainerHeight,
            ParsedProperty::OptionalNumber(value),
        ) => *height = value,
        (
            UiNode::Text { text, .. },
            EditableProperty::TextDocumentPath,
            ParsedProperty::Text(value),
        ) => *text = UiTextSource::Document { path: value },
        (
            UiNode::Button { label, .. },
            EditableProperty::ButtonLabel,
            ParsedProperty::Text(value),
        ) => *label = value,
        (
            UiNode::Button {
                accessible_name, ..
            },
            EditableProperty::ButtonAccessibleName,
            ParsedProperty::OptionalText(value),
        ) => *accessible_name = value,
        (
            UiNode::Button { tooltip, .. },
            EditableProperty::ButtonTooltip,
            ParsedProperty::OptionalText(value),
        ) => *tooltip = value,
        (
            UiNode::Button { on_click_path, .. },
            EditableProperty::ButtonClickPath,
            ParsedProperty::OptionalText(value),
        ) => *on_click_path = value,
        (
            UiNode::Button { click_value, .. },
            EditableProperty::ButtonClickValue,
            ParsedProperty::OptionalText(value),
        ) => *click_value = value,
        (
            UiNode::Button {
                navigate: Some(UiNavigation::Push { route } | UiNavigation::Replace { route }),
                ..
            },
            EditableProperty::ButtonNavigationRoute,
            ParsedProperty::Text(value),
        ) => *route = value,
        (
            UiNode::Text {
                text: UiTextSource::Static { value: text },
                ..
            },
            EditableProperty::StaticText,
            ParsedProperty::Text(value),
        ) => *text = value,
        (
            UiNode::TextInput {
                label,
                accessible_name,
                ..
            },
            EditableProperty::InputLabel,
            ParsedProperty::OptionalText(value),
        )
        | (
            UiNode::Checkbox {
                label,
                accessible_name,
                ..
            }
            | UiNode::Toggle {
                label,
                accessible_name,
                ..
            },
            EditableProperty::CheckLabel,
            ParsedProperty::OptionalText(value),
        ) => {
            *label = value;
            if label.is_some() {
                accessible_name.clear();
            }
        }
        (
            UiNode::TextInput {
                label,
                accessible_name,
                ..
            },
            EditableProperty::InputAccessibleName,
            ParsedProperty::Text(value),
        )
        | (
            UiNode::Checkbox {
                label,
                accessible_name,
                ..
            }
            | UiNode::Toggle {
                label,
                accessible_name,
                ..
            },
            EditableProperty::CheckAccessibleName,
            ParsedProperty::Text(value),
        ) => {
            *accessible_name = value;
            if !accessible_name.trim().is_empty() {
                *label = None;
            }
        }
        (
            UiNode::TextInput { placeholder, .. },
            EditableProperty::InputPlaceholder,
            ParsedProperty::Text(value),
        ) => *placeholder = value,
        (
            UiNode::TextInput { value_path, .. },
            EditableProperty::InputValuePath,
            ParsedProperty::OptionalText(value),
        ) => *value_path = value,
        (
            UiNode::TextInput { initial_value, .. },
            EditableProperty::InputInitialValue,
            ParsedProperty::Text(value),
        ) => *initial_value = value,
        (
            UiNode::Checkbox { value_path, .. } | UiNode::Toggle { value_path, .. },
            EditableProperty::CheckValuePath,
            ParsedProperty::Text(value),
        ) => *value_path = value,
        (
            UiNode::Component { name, .. },
            EditableProperty::ComponentName,
            ParsedProperty::Text(value),
        ) => *name = value,
        (
            UiNode::Component { props, .. },
            EditableProperty::ComponentProps,
            ParsedProperty::JsonObject(value),
        ) => *props = value,
        _ => {}
    }
}

fn set_node_id(node: &mut UiNode, value: String) {
    match node {
        UiNode::Column { id, .. }
        | UiNode::Row { id, .. }
        | UiNode::Container { id, .. }
        | UiNode::Text { id, .. }
        | UiNode::Button { id, .. }
        | UiNode::TextInput { id, .. }
        | UiNode::Checkbox { id, .. }
        | UiNode::Toggle { id, .. }
        | UiNode::Component { id, .. }
        | UiNode::Spacer { id } => *id = value,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive boolean-property catalog keeps graph editing schema-complete"
)]
fn editable_bools(node: &UiNode) -> Vec<(EditableBool, bool, &'static str, &'static str)> {
    match node {
        UiNode::Column {
            no_padding,
            intrinsic_height,
            fill_remaining_height,
            ..
        } => vec![
            (
                EditableBool::ColumnNoPadding,
                *no_padding,
                "No padding",
                "builder-column-no-padding",
            ),
            (
                EditableBool::ColumnIntrinsicHeight,
                *intrinsic_height,
                "Intrinsic height",
                "builder-column-intrinsic-height",
            ),
            (
                EditableBool::ColumnFillRemainingHeight,
                *fill_remaining_height,
                "Fill remaining height",
                "builder-column-fill-height",
            ),
        ],
        UiNode::Row {
            no_wrap,
            fill_remaining_height,
            ..
        } => vec![
            (
                EditableBool::RowNoWrap,
                *no_wrap,
                "No wrap",
                "builder-row-no-wrap",
            ),
            (
                EditableBool::RowFillRemainingHeight,
                *fill_remaining_height,
                "Fill remaining height",
                "builder-row-fill-height",
            ),
        ],
        UiNode::Container {
            background,
            no_padding,
            fill_height,
            ..
        } => vec![
            (
                EditableBool::ContainerBackground,
                *background,
                "Background",
                "builder-container-background",
            ),
            (
                EditableBool::ContainerNoPadding,
                *no_padding,
                "No padding",
                "builder-container-no-padding",
            ),
            (
                EditableBool::ContainerFillHeight,
                *fill_height,
                "Fill height",
                "builder-container-fill-height",
            ),
        ],
        UiNode::Button { enabled, .. } => vec![(
            EditableBool::ButtonEnabled,
            *enabled,
            "Enabled",
            "builder-button-enabled",
        )],
        UiNode::TextInput {
            multiline,
            disabled,
            ..
        } => vec![
            (
                EditableBool::InputMultiline,
                *multiline,
                "Multiline",
                "builder-input-multiline",
            ),
            (
                EditableBool::InputDisabled,
                *disabled,
                "Disabled",
                "builder-input-disabled",
            ),
        ],
        UiNode::Checkbox { enabled, .. } | UiNode::Toggle { enabled, .. } => vec![(
            EditableBool::CheckEnabled,
            *enabled,
            "Enabled",
            "builder-check-enabled",
        )],
        UiNode::Text { .. } | UiNode::Component { .. } | UiNode::Spacer { .. } => Vec::new(),
    }
}

fn set_editable_bool(node: &mut UiNode, property: EditableBool, value: bool) {
    match (node, property) {
        (UiNode::Column { no_padding, .. }, EditableBool::ColumnNoPadding)
        | (UiNode::Container { no_padding, .. }, EditableBool::ContainerNoPadding) => {
            *no_padding = value;
        }
        (
            UiNode::Column {
                intrinsic_height, ..
            },
            EditableBool::ColumnIntrinsicHeight,
        ) => *intrinsic_height = value,
        (
            UiNode::Column {
                fill_remaining_height,
                ..
            },
            EditableBool::ColumnFillRemainingHeight,
        )
        | (
            UiNode::Row {
                fill_remaining_height,
                ..
            },
            EditableBool::RowFillRemainingHeight,
        ) => *fill_remaining_height = value,
        (UiNode::Row { no_wrap, .. }, EditableBool::RowNoWrap) => *no_wrap = value,
        (UiNode::Container { background, .. }, EditableBool::ContainerBackground) => {
            *background = value;
        }
        (UiNode::Container { fill_height, .. }, EditableBool::ContainerFillHeight) => {
            *fill_height = value;
        }
        (UiNode::Button { enabled, .. }, EditableBool::ButtonEnabled)
        | (
            UiNode::Checkbox { enabled, .. } | UiNode::Toggle { enabled, .. },
            EditableBool::CheckEnabled,
        ) => *enabled = value,
        (UiNode::TextInput { multiline, .. }, EditableBool::InputMultiline) => *multiline = value,
        (UiNode::TextInput { disabled, .. }, EditableBool::InputDisabled) => *disabled = value,
        _ => {}
    }
}

fn boolean_property_button(
    property: EditableBool,
    value: bool,
    label: &'static str,
    id: &'static str,
    selected: &str,
    handle: &UiGraphHandle,
    shared: &Rc<RefCell<UiGraphEditorState>>,
) -> Button {
    let graph = handle.clone();
    let state = Rc::clone(shared);
    let selected = selected.to_owned();
    Button::new(
        id,
        &format!("{label}: {}", if value { "On" } else { "Off" }),
    )
    .on_click(move || {
        let result = graph.update_node(&selected, |node| set_editable_bool(node, property, !value));
        let status = state.borrow().status.clone();
        match result {
            Ok(()) => {
                let mut editor = state.borrow_mut();
                editor.chrome_dirty = true;
                status.set("Preview updated".to_owned());
            }
            Err(error) => status.set(format!("Edit rejected: {error}")),
        }
    })
}

fn editable_choices(
    node: &UiNode,
) -> Vec<(EditableChoice, &'static str, &'static str, &'static str)> {
    match node {
        UiNode::Text { text, role, .. } => vec![
            (
                EditableChoice::TextSource,
                match text {
                    UiTextSource::Static { .. } => "Static",
                    UiTextSource::Document { .. } => "Document",
                },
                "Text source",
                "builder-text-source",
            ),
            (
                EditableChoice::TextRole,
                text_role_name(*role),
                "Text role",
                "builder-text-role",
            ),
        ],
        UiNode::Button { role, navigate, .. } => vec![
            (
                EditableChoice::ButtonRole,
                button_role_name(*role),
                "Role",
                "builder-button-role",
            ),
            (
                EditableChoice::ButtonNavigation,
                navigation_name(navigate.as_ref()),
                "Navigation",
                "builder-button-navigation",
            ),
        ],
        _ => Vec::new(),
    }
}

fn choice_property_button(
    property: EditableChoice,
    value: &'static str,
    label: &'static str,
    id: &'static str,
    selected: &str,
    handle: &UiGraphHandle,
    shared: &Rc<RefCell<UiGraphEditorState>>,
) -> Button {
    let graph = handle.clone();
    let state = Rc::clone(shared);
    let selected = selected.to_owned();
    Button::new(id, &format!("{label}: {value}")).on_click(move || {
        let result = graph.update_node(&selected, |node| cycle_editable_choice(node, property));
        let status = state.borrow().status.clone();
        match result {
            Ok(()) => {
                let mut editor = state.borrow_mut();
                editor.chrome_dirty = true;
                status.set("Preview updated".to_owned());
            }
            Err(error) => status.set(format!("Edit rejected: {error}")),
        }
    })
}

fn cycle_editable_choice(node: &mut UiNode, property: EditableChoice) {
    match (node, property) {
        (UiNode::Text { text, .. }, EditableChoice::TextSource) => {
            *text = match text {
                UiTextSource::Static { .. } => UiTextSource::Document {
                    path: "value".to_owned(),
                },
                UiTextSource::Document { path } => UiTextSource::Static {
                    value: path.clone(),
                },
            };
        }
        (UiNode::Text { role, .. }, EditableChoice::TextRole) => {
            *role = next_text_role(*role);
        }
        (UiNode::Button { role, .. }, EditableChoice::ButtonRole) => {
            *role = match role {
                UiButtonRole::Secondary => UiButtonRole::Primary,
                UiButtonRole::Primary => UiButtonRole::Destructive,
                UiButtonRole::Destructive => UiButtonRole::Plain,
                UiButtonRole::Plain => UiButtonRole::Secondary,
            };
        }
        (UiNode::Button { navigate, .. }, EditableChoice::ButtonNavigation) => {
            *navigate = match navigate.take() {
                None => Some(UiNavigation::Push {
                    route: "screen".to_owned(),
                }),
                Some(UiNavigation::Push { route }) => Some(UiNavigation::Replace { route }),
                Some(UiNavigation::Replace { .. }) => Some(UiNavigation::Back),
                Some(UiNavigation::Back) => Some(UiNavigation::Forward),
                Some(UiNavigation::Forward) => None,
            };
        }
        _ => {}
    }
}

const fn text_role_name(role: UiTextRole) -> &'static str {
    match role {
        UiTextRole::Display => "Display",
        UiTextRole::LargeTitle => "Large title",
        UiTextRole::Title => "Title",
        UiTextRole::Heading => "Heading",
        UiTextRole::Subheading => "Subheading",
        UiTextRole::Body => "Body",
        UiTextRole::Label => "Label",
        UiTextRole::Caption => "Caption",
        UiTextRole::Reading => "Reading",
        UiTextRole::Code => "Code",
    }
}

const fn next_text_role(role: UiTextRole) -> UiTextRole {
    match role {
        UiTextRole::Display => UiTextRole::LargeTitle,
        UiTextRole::LargeTitle => UiTextRole::Title,
        UiTextRole::Title => UiTextRole::Heading,
        UiTextRole::Heading => UiTextRole::Subheading,
        UiTextRole::Subheading => UiTextRole::Body,
        UiTextRole::Body => UiTextRole::Label,
        UiTextRole::Label => UiTextRole::Caption,
        UiTextRole::Caption => UiTextRole::Reading,
        UiTextRole::Reading => UiTextRole::Code,
        UiTextRole::Code => UiTextRole::Display,
    }
}

const fn button_role_name(role: UiButtonRole) -> &'static str {
    match role {
        UiButtonRole::Primary => "Primary",
        UiButtonRole::Secondary => "Secondary",
        UiButtonRole::Destructive => "Destructive",
        UiButtonRole::Plain => "Plain",
    }
}

const fn navigation_name(navigation: Option<&UiNavigation>) -> &'static str {
    match navigation {
        None => "None",
        Some(UiNavigation::Push { .. }) => "Push",
        Some(UiNavigation::Replace { .. }) => "Replace",
        Some(UiNavigation::Back) => "Back",
        Some(UiNavigation::Forward) => "Forward",
    }
}

fn parent_relation<'a>(node: &'a UiNode, id: &str) -> Option<(&'a str, usize, usize)> {
    let children = node.children();
    if let Some(index) = children.iter().position(|child| child.id() == id) {
        return Some((node.id(), index, children.len()));
    }
    children
        .into_iter()
        .find_map(|child| parent_relation(child, id))
}

fn save_button(handle: &UiGraphHandle, shared: &Rc<RefCell<UiGraphEditorState>>) -> Button {
    let graph = handle.clone();
    let state = Rc::clone(shared);
    Button::new("builder-save", "Save")
        .role(ButtonRole::Primary)
        .on_click(move || {
            let status = state.borrow().status.clone();
            if let Err(error) = graph.finish_edit_group() {
                status.set(format!("Cannot finish property edit: {error}"));
                return;
            }
            let source = match graph.to_json_pretty() {
                Ok(source) => source,
                Err(error) => {
                    status.set(format!("Cannot serialize: {error}"));
                    return;
                }
            };
            let result = state.borrow_mut().save.as_mut().map_or_else(
                || Err("no save destination configured".to_owned()),
                |save| save(&source),
            );
            match result {
                Ok(()) => status.set("Saved".to_owned()),
                Err(error) => status.set(format!("Save failed: {error}")),
            }
        })
}

fn undo_button(handle: &UiGraphHandle, shared: &Rc<RefCell<UiGraphEditorState>>) -> Button {
    let graph = handle.clone();
    let state = Rc::clone(shared);
    Button::new("builder-undo", "Undo")
        .with_enabled(handle.can_undo())
        .on_click(move || {
            let status = state.borrow().status.clone();
            match graph.undo() {
                Ok(true) => status.set("Undid graph edit".to_owned()),
                Ok(false) => status.set("Nothing to undo".to_owned()),
                Err(error) => status.set(format!("Undo failed: {error}")),
            }
        })
}

fn redo_button(handle: &UiGraphHandle, shared: &Rc<RefCell<UiGraphEditorState>>) -> Button {
    let graph = handle.clone();
    let state = Rc::clone(shared);
    Button::new("builder-redo", "Redo")
        .with_enabled(handle.can_redo())
        .on_click(move || {
            let status = state.borrow().status.clone();
            match graph.redo() {
                Ok(true) => status.set("Redid graph edit".to_owned()),
                Ok(false) => status.set("Nothing to redo".to_owned()),
                Err(error) => status.set(format!("Redo failed: {error}")),
            }
        })
}

fn add_button(
    id: &str,
    label: &str,
    kind: InsertKind,
    enabled: bool,
    selected: &str,
    handle: &UiGraphHandle,
    shared: &Rc<RefCell<UiGraphEditorState>>,
) -> Button {
    let parent = selected.to_owned();
    let graph = handle.clone();
    let state = Rc::clone(shared);
    Button::new(id, label)
        .with_enabled(enabled)
        .on_click(move || {
            let status = state.borrow().status.clone();
            let result = (|| {
                let document = graph.document()?;
                let index = document
                    .node(&parent)
                    .map(|node| node.children().len())
                    .ok_or_else(|| UiGraphError::UnknownNode(parent.clone()))?;
                let node = new_editor_node(&document, kind);
                let id = node.id().to_owned();
                graph.insert_child(&parent, index, node)?;
                Ok::<_, UiGraphError>(id)
            })();
            match result {
                Ok(id) => {
                    let mut editor = state.borrow_mut();
                    editor.selected = id;
                    editor.chrome_dirty = true;
                    status.set("Node added".to_owned());
                }
                Err(error) => status.set(format!("Add rejected: {error}")),
            }
        })
}

fn new_editor_node(document: &UiDocument, kind: InsertKind) -> UiNode {
    let base = match kind {
        InsertKind::Text => "text",
        InsertKind::Button => "button",
        InsertKind::Row => "row",
        InsertKind::Column => "column",
        InsertKind::Container => "container",
        InsertKind::TextInput => "input",
        InsertKind::Checkbox => "checkbox",
        InsertKind::Toggle => "toggle",
        InsertKind::Component => "component",
        InsertKind::Spacer => "spacer",
    };
    let id = unique_node_id(document, base);
    match kind {
        InsertKind::Text => UiNode::Text {
            id,
            text: UiTextSource::Static {
                value: "New text".to_owned(),
            },
            role: UiTextRole::Body,
        },
        InsertKind::Button => UiNode::Button {
            id,
            label: "Button".to_owned(),
            accessible_name: None,
            tooltip: None,
            on_click_path: None,
            click_value: None,
            navigate: None,
            enabled: true,
            role: UiButtonRole::Secondary,
        },
        InsertKind::Row => UiNode::Row {
            id,
            children: Vec::new(),
            no_wrap: false,
            fill_remaining_height: false,
        },
        InsertKind::Column => UiNode::Column {
            id,
            children: Vec::new(),
            gap: None,
            no_padding: false,
            intrinsic_height: false,
            fill_remaining_height: false,
        },
        InsertKind::Container => UiNode::Container {
            id,
            child: Box::new(UiNode::Text {
                id: unique_node_id(document, "content"),
                text: UiTextSource::Static {
                    value: "Content".to_owned(),
                },
                role: UiTextRole::Body,
            }),
            background: true,
            no_padding: false,
            fill_height: false,
            width: None,
            height: None,
        },
        InsertKind::TextInput => UiNode::TextInput {
            id,
            label: Some("Text input".to_owned()),
            accessible_name: String::new(),
            placeholder: "Enter text".to_owned(),
            value_path: None,
            initial_value: String::new(),
            multiline: false,
            disabled: false,
        },
        InsertKind::Checkbox => UiNode::Checkbox {
            value_path: format!("{id}-checked"),
            id,
            label: Some("Checkbox".to_owned()),
            accessible_name: String::new(),
            enabled: true,
        },
        InsertKind::Toggle => UiNode::Toggle {
            value_path: format!("{id}-enabled"),
            id,
            label: Some("Toggle".to_owned()),
            accessible_name: String::new(),
            enabled: true,
        },
        InsertKind::Component => UiNode::Component {
            id,
            name: "app-component".to_owned(),
            props: serde_json::Map::new(),
        },
        InsertKind::Spacer => UiNode::Spacer { id },
    }
}

fn unique_node_id(document: &UiDocument, base: &str) -> String {
    if document.node(base).is_none() {
        return base.to_owned();
    }
    (2_u32..=u32::MAX)
        .map(|suffix| format!("{base}-{suffix}"))
        .find(|candidate| document.node(candidate).is_none())
        .unwrap_or_else(|| format!("{base}-new"))
}

fn move_button(
    id: &str,
    label: &str,
    enabled: bool,
    direction: i8,
    selected: &str,
    handle: &UiGraphHandle,
    shared: &Rc<RefCell<UiGraphEditorState>>,
) -> Button {
    let selected = selected.to_owned();
    let graph = handle.clone();
    let state = Rc::clone(shared);
    Button::new(id, label)
        .with_enabled(enabled)
        .on_click(move || {
            let result = (|| {
                let document = graph.document()?;
                let (parent, index, _) = parent_relation(&document.root, &selected)
                    .ok_or_else(|| UiGraphError::UnknownNode(selected.clone()))?;
                let target = if direction < 0 {
                    index.saturating_sub(1)
                } else {
                    index.saturating_add(1)
                };
                graph.move_node(&selected, parent, target)
            })();
            let status = state.borrow().status.clone();
            match result {
                Ok(()) => {
                    let mut editor = state.borrow_mut();
                    editor.chrome_dirty = true;
                    status.set("Node moved".to_owned());
                }
                Err(error) => status.set(format!("Move rejected: {error}")),
            }
        })
}

fn delete_button(
    enabled: bool,
    selected: &str,
    handle: &UiGraphHandle,
    shared: &Rc<RefCell<UiGraphEditorState>>,
) -> Button {
    let selected = selected.to_owned();
    let graph = handle.clone();
    let state = Rc::clone(shared);
    Button::new("builder-delete", "Delete")
        .role(ButtonRole::Destructive)
        .with_enabled(enabled)
        .on_click(move || {
            let result = (|| {
                let document = graph.document()?;
                let parent = parent_relation(&document.root, &selected)
                    .map(|(parent, _, _)| parent.to_owned())
                    .ok_or_else(|| UiGraphError::UnknownNode(selected.clone()))?;
                graph.remove_node(&selected)?;
                Ok::<_, UiGraphError>(parent)
            })();
            let status = state.borrow().status.clone();
            match result {
                Ok(parent) => {
                    let mut editor = state.borrow_mut();
                    editor.selected = parent;
                    editor.chrome_dirty = true;
                    status.set("Node deleted".to_owned());
                }
                Err(error) => status.set(format!("Delete rejected: {error}")),
            }
        })
}

impl Widget for UiGraphEditor {
    crate::provides!(dyn crate::undo::LocalUndo);

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
        crate::widget::TaffyTreeExt::widget_node(taffy, filling(), &[child], node_map, self)
    }

    crate::forward_to_child!(child, rect);

    fn name(&self) -> &'static str {
        "UiGraphEditor"
    }

    fn test_id(&self) -> Option<&str> {
        Some("ui-graph-editor")
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty) | self.child.take_structure_dirty()
    }

    fn needs_idle_sync(&self) -> bool {
        let state = self.shared.borrow();
        state.chrome_dirty
            || self.handle.revision() != Some(self.seen_graph_revision)
            || self.child.needs_idle_sync()
    }
}

impl crate::undo::LocalUndo for UiGraphEditor {
    fn handle_local_undo_request(&mut self, request: crate::undo::UndoRequest) -> bool {
        if !widget_tree_has_focus(self.child.as_ref()) {
            return false;
        }
        let status = self.shared.borrow().status.clone();
        let result = match request {
            crate::undo::UndoRequest::Undo => self.handle.undo(),
            crate::undo::UndoRequest::Redo => self.handle.redo(),
        };
        match (request, result) {
            (crate::undo::UndoRequest::Undo, Ok(true)) => {
                status.set("Undid graph edit".to_owned());
            }
            (crate::undo::UndoRequest::Redo, Ok(true)) => {
                status.set("Redid graph edit".to_owned());
            }
            (request, Ok(false)) => status.set(format!("Nothing to {}", request.verb())),
            (_, Err(error)) => status.set(format!("History action failed: {error}")),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::{Action, Role};
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ROOT};

    #[test]
    fn graph_checked_controls_share_view_values_without_document_writes() {
        let document = UiDocument::from_json(
            r#"{"version":1,"root":{"type":"column","id":"root","children":[{"type":"checkbox","id":"check","value_path":"shared","accessible_name":"Check"},{"type":"toggle","id":"toggle","value_path":"shared","accessible_name":"Toggle"}]}}"#,
        ).unwrap();
        let context = UiBuildContext::default();
        let widget = document.build_with(&Theme::dark(), &context).unwrap();
        let mut driver =
            crate::headless::FrameHarness::new(crate::State::new(), widget, (360, 200));
        let before = driver.dump_json();
        driver.perform_named("check", &Action::ToggleCheck).unwrap();
        driver
            .expect(&crate::headless::SemanticExpectation::named("toggle").checked(true))
            .unwrap();
        assert_eq!(context.values().get_bool("shared"), Some(true));
        driver
            .perform_named("toggle", &Action::ToggleCheck)
            .unwrap();
        driver
            .expect(&crate::headless::SemanticExpectation::named("check").checked(false))
            .unwrap();
        assert_eq!(context.values().get_bool("shared"), Some(false));
        assert_eq!(
            driver.dump_json(),
            before,
            "unrouted graph controls edit view state only"
        );
    }

    fn graph_field(id: &str, value_path: &str) -> UiNode {
        UiNode::TextInput {
            id: id.into(),
            label: None,
            accessible_name: id.into(),
            placeholder: String::new(),
            value_path: Some(value_path.into()),
            initial_value: String::new(),
            multiline: false,
            disabled: false,
        }
    }

    #[test]
    fn a_routed_path_edits_through_the_apps_action_and_shows_its_value() {
        use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType};
        let spec = ActionSpec::typed_operation(
            "rename",
            "Rename",
            "The new title",
            OperationKind::Effect,
            "graph.rename",
            vec![ActionParam::required("title", ParamType::Text, "Title")],
            |args| EncodedAction::encode_operation("graph.rename", &args.to_json()).unwrap(),
        );
        let resource =
            tilo_actions::Resource::new("ordinary:graph", "graph", "Graph").with_action(spec);
        let action = tilo_actions::ActionBinding::new(&resource, &resource.actions[0], |title| {
            let mut arguments = serde_json::Map::new();
            arguments.insert("title".into(), serde_json::Value::String(title));
            arguments
        })
        .unwrap();
        let title = LocalState::new("Atlas".to_owned());
        let values = UiValues::default();
        values.route("title", title.clone(), action);
        let context = UiBuildContext::default().with_values(values.clone());
        let document = UiDocument::new(UiNode::Column {
            id: "root".into(),
            children: vec![
                graph_field("title-a", "title"),
                graph_field("title-b", "title"),
                graph_field("draft", "draft"),
            ],
            gap: None,
            no_padding: false,
            intrinsic_height: false,
            fill_remaining_height: false,
        });
        let mut root = document.build_with(&Theme::dark(), &context).unwrap();
        crate::widget::controls::refresh_control_action_availability(
            root.as_mut(),
            &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                "ordinary:graph",
                "rename",
            )]),
        );
        let mut doc = AutoCommit::new();
        Widget::sync(root.as_mut(), &doc, &ROOT);
        assert_eq!(
            named_value(root.as_ref(), "title-b").as_deref(),
            Some("Atlas")
        );

        let set = |root: &mut Element, doc: &mut AutoCommit, id: &str, value: &str| {
            assert!(perform_named(
                root.as_mut(),
                id,
                &Action::SetValue(value.into()),
                doc
            ));
        };
        set(&mut root, &mut doc, "title-a", "Atlas II");
        let invocation = crate::widget::controls::take_control_invocation(root.as_mut())
            .expect("the edit is the app's action");
        assert_eq!(invocation.address.action, "rename");
        assert_eq!(invocation.arguments["title"], "Atlas II");
        assert!(
            automerge::ReadDoc::get(&doc, &ROOT, "title")
                .unwrap()
                .is_none(),
            "the input never writes the document"
        );
        // What every input over the path shows is the app's value.
        title.set("Atlas II".to_owned());
        Widget::sync(root.as_mut(), &doc, &ROOT);
        assert_eq!(
            named_value(root.as_ref(), "title-b").as_deref(),
            Some("Atlas II")
        );

        set(&mut root, &mut doc, "draft", "Notes");
        assert!(
            crate::widget::controls::take_control_invocation(root.as_mut()).is_none(),
            "an unrouted path invokes nothing"
        );
        assert_eq!(values.get("draft").as_deref(), Some("Notes"));
        assert!(
            automerge::ReadDoc::get(&doc, &ROOT, "draft")
                .unwrap()
                .is_none()
        );
    }

    fn perform_named(
        widget: &mut dyn Widget,
        id: &str,
        action: &Action,
        doc: &mut AutoCommit,
    ) -> bool {
        crate::widget::find_test_id_mut(widget, id).is_some_and(|widget| {
            Widget::perform_action(
                widget,
                action,
                &[],
                &mut crate::widget::Edits::lend(doc),
                &ROOT,
            )
        })
    }

    fn named_value(widget: &dyn Widget, id: &str) -> Option<String> {
        crate::widget::find_test_id(widget, id)?
            .accessibility()?
            .value
    }

    fn run_operation(
        live: &mut impl Widget,
        kind: crate::tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
    ) -> Result<Option<crate::tilo_actions::ActionOutput>, String> {
        crate::widget::ResourceOperation::local(
            crate::tilo_actions::InvocationId::new(),
            kind,
            type_id,
            payload,
        )
        .start(
            live,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &ROOT,
        )
        .map(|started| started.and_then(crate::widget::ResourceOperationStart::completed))
    }

    fn sample() -> UiDocument {
        UiDocument::new(UiNode::Column {
            id: "root".into(),
            children: vec![
                UiNode::Text {
                    id: "title-view".into(),
                    text: UiTextSource::Document {
                        path: "title".into(),
                    },
                    role: UiTextRole::Title,
                },
                UiNode::TextInput {
                    id: "title-input".into(),
                    label: Some("Title".into()),
                    accessible_name: String::new(),
                    placeholder: "Untitled".into(),
                    value_path: Some("title".into()),
                    initial_value: String::new(),
                    multiline: false,
                    disabled: false,
                },
                UiNode::Button {
                    id: "save".into(),
                    label: "Save".into(),
                    accessible_name: None,
                    tooltip: Some("Save document".into()),
                    on_click_path: Some("save-requested".into()),
                    click_value: Some("yes".into()),
                    navigate: None,
                    enabled: true,
                    role: UiButtonRole::Primary,
                },
            ],
            gap: Some(8.0),
            no_padding: false,
            intrinsic_height: false,
            fill_remaining_height: false,
        })
    }

    #[test]
    fn app_component_factories_receive_props_and_keep_graph_lifecycle() {
        let document = UiDocument::from_json(
            r#"{"version":1,"root":{"type":"component","id":"profile","name":"profile-card","props":{"title":"Ada"}}}"#,
        )
        .unwrap();
        assert!(matches!(
            document.build(&Theme::dark()),
            Err(UiGraphError::UnknownComponent { id, name })
                if id == "profile" && name == "profile-card"
        ));

        let seen = Rc::new(RefCell::new(Vec::new()));
        let seen_by_factory = Rc::clone(&seen);
        let context = UiBuildContext::default().with_component("profile-card", move |request| {
            seen_by_factory.borrow_mut().push((
                request.id.to_owned(),
                request.name.to_owned(),
                request.props.get("title").cloned(),
            ));
            let title = request
                .props
                .get("title")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "title must be a string".to_owned())?;
            Ok(Box::new(Text::new(title).with_id(request.id)))
        });
        let widget = document.build_with(&Theme::dark(), &context).unwrap();
        let mut app = crate::headless::FrameHarness::new(crate::State::new(), widget, (320, 120));
        app.expect(
            &crate::headless::SemanticExpectation::named("profile")
                .role(Role::StaticText)
                .name("Ada"),
        )
        .expect("factory result remains a normal graph widget");
        assert_eq!(
            seen.borrow().as_slice(),
            [(
                "profile".to_owned(),
                "profile-card".to_owned(),
                Some(serde_json::json!("Ada"))
            )]
        );

        let failing = UiBuildContext::default()
            .with_component("profile-card", |_| Err("missing account".to_owned()));
        assert!(matches!(
            document.build_with(&Theme::dark(), &failing),
            Err(UiGraphError::ComponentBuild { message, .. }) if message == "missing account"
        ));
    }

    #[test]
    fn builder_previews_and_edits_unloaded_app_components() {
        let document = UiDocument::from_json(
            r#"{"version":1,"root":{"type":"component","id":"profile","name":"profile-card","props":{"title":"Ada"}}}"#,
        )
        .unwrap();
        let handle = UiGraphHandle::new(document).unwrap();
        let editor = UiGraphEditor::new(handle.clone(), Theme::dark()).unwrap();
        let mut app =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(editor), (1200, 720));
        app.expect(
            &crate::headless::SemanticExpectation::named("profile")
                .role(Role::StaticText)
                .name("Component: profile-card"),
        )
        .expect("builder uses a named placeholder without app code");

        let mut component = handle.node("profile").unwrap();
        set_editable_property(
            &mut component,
            EditableProperty::ComponentProps,
            parse_editable_property(EditableProperty::ComponentProps, r#"{"title":"Grace"}"#)
                .unwrap(),
        );
        handle.replace_node("profile", component).unwrap();
        assert!(matches!(
            handle.node("profile"),
            Ok(UiNode::Component { props, .. })
                if props.get("title") == Some(&serde_json::json!("Grace"))
        ));
    }

    #[test]
    fn graph_json_round_trips_and_builds_real_accessible_widgets() {
        fn has_title(node: &crate::headless::InspectionNode) -> bool {
            node.semantics.as_ref().is_some_and(|semantics| {
                semantics.role == (Role::Heading { level: 1 })
                    && semantics.name.as_deref() == Some("Roadmap")
            }) || node.children.iter().any(has_title)
        }

        let document = sample();
        let json = document.to_json_pretty().unwrap();
        let reparsed = UiDocument::from_json(&json).unwrap();
        assert_eq!(reparsed, document);
        let mut title_doc = AutoCommit::new();
        let text = title_doc.put_object(&ROOT, "title", ObjType::Text).unwrap();
        title_doc.splice_text(&text, 0, 0, "Roadmap").unwrap();
        let values = UiValues::default();
        let context = UiBuildContext::default().with_values(values.clone());
        let mut driver = crate::headless::FrameHarness::new(
            crate::State::from_doc(title_doc),
            reparsed.build_with(&Theme::dark(), &context).unwrap(),
            (640, 360),
        );
        let inspection = driver.inspection();
        assert!(has_title(&inspection.root));
        driver
            .expect(
                &crate::headless::SemanticExpectation::named("title-input")
                    .role(Role::TextInput)
                    .name("Title"),
            )
            .expect("serialized label names the real input");
        assert!(
            driver.targets().contains_key("title-view"),
            "serialized text ids remain addressable in scenarios"
        );
        driver
            .perform_named("save", &Action::Activate)
            .expect("serialized button is a real button");
        assert_eq!(values.take_click("save-requested").as_deref(), Some("yes"));
        assert_eq!(
            values.take_click("save-requested"),
            None,
            "a click is taken once"
        );
        assert!(
            !driver.dump_json().contains("save-requested"),
            "the button never writes the document"
        );
    }

    #[test]
    fn exhausted_graph_revision_rejects_a_mutation_without_reusing_live_state() {
        let handle = UiGraphHandle::new(sample()).unwrap();
        let before = handle.document().unwrap();
        handle.inner.write().unwrap().revision = u64::MAX;
        let replacement = UiDocument::new(UiNode::Text {
            id: "replacement".into(),
            text: UiTextSource::Static {
                value: "Replacement".into(),
            },
            role: UiTextRole::Body,
        });
        assert!(matches!(
            handle.replace(replacement),
            Err(UiGraphError::RevisionExhausted)
        ));
        assert_eq!(handle.document().unwrap(), before);
    }

    #[test]
    fn serialized_check_label_builds_visible_and_spoken_semantics() {
        fn has_visible_label(node: &crate::headless::InspectionNode) -> bool {
            node.semantics.as_ref().is_some_and(|semantics| {
                semantics.role == Role::StaticText
                    && semantics.name.as_deref() == Some("Remember me")
            }) || node.children.iter().any(has_visible_label)
        }

        let document = UiDocument::new(UiNode::Checkbox {
            id: "remember".into(),
            label: Some("Remember me".into()),
            accessible_name: String::new(),
            value_path: "remember".into(),
            enabled: true,
        });
        let mut driver = crate::headless::FrameHarness::new(
            crate::State::new(),
            document.build(&Theme::dark()).unwrap(),
            (360, 120),
        );

        driver
            .expect(
                &crate::headless::SemanticExpectation::named("remember")
                    .role(Role::Checkbox)
                    .name("Remember me"),
            )
            .expect("serialized label names the checkbox");
        assert!(has_visible_label(&driver.inspection().root));
    }

    #[test]
    fn editor_label_fields_clear_the_fallback_name_and_vice_versa() {
        let mut node = UiNode::Toggle {
            id: "sync".into(),
            label: None,
            accessible_name: "Unlabeled sync".into(),
            value_path: "sync".into(),
            enabled: true,
        };

        set_editable_property(
            &mut node,
            EditableProperty::CheckLabel,
            ParsedProperty::OptionalText(Some("Sync changes".into())),
        );
        assert!(matches!(
            &node,
            UiNode::Toggle {
                label: Some(label),
                accessible_name,
                ..
            } if label == "Sync changes" && accessible_name.is_empty()
        ));

        set_editable_property(
            &mut node,
            EditableProperty::CheckAccessibleName,
            ParsedProperty::Text("Background synchronization".into()),
        );
        assert!(matches!(
            &node,
            UiNode::Toggle {
                label: None,
                accessible_name,
                ..
            } if accessible_name == "Background synchronization"
        ));
    }

    #[test]
    fn routed_graph_buttons_publish_navigation_and_leave_the_document_alone() {
        fn screen(id: &str, title: &str, button: UiNode) -> UiDocument {
            UiDocument::new(UiNode::Column {
                id: id.into(),
                children: vec![
                    UiNode::Text {
                        id: format!("{id}-title"),
                        text: UiTextSource::Static {
                            value: title.into(),
                        },
                        role: UiTextRole::Title,
                    },
                    button,
                ],
                gap: None,
                no_padding: false,
                intrinsic_height: false,
                fill_remaining_height: false,
            })
        }

        let home = screen(
            "home-root",
            "Home",
            UiNode::Button {
                id: "open-settings".into(),
                label: "Settings".into(),
                accessible_name: None,
                tooltip: None,
                on_click_path: None,
                click_value: None,
                navigate: Some(UiNavigation::Push {
                    route: "settings".into(),
                }),
                enabled: true,
                role: UiButtonRole::Primary,
            },
        );
        let settings = screen(
            "settings-root",
            "Settings screen",
            UiNode::Button {
                id: "back-home".into(),
                label: "Back".into(),
                accessible_name: None,
                tooltip: None,
                on_click_path: None,
                click_value: None,
                navigate: Some(UiNavigation::Back),
                enabled: true,
                role: UiButtonRole::Secondary,
            },
        );
        let router = crate::widget::Router::new(
            "home",
            ["home", "settings"],
            &Theme::dark(),
            move |route, theme, handle| {
                let document = if route == "home" { &home } else { &settings };
                document
                    .build_with(
                        theme,
                        &UiBuildContext::default().with_router(handle.clone()),
                    )
                    .map_err(|error| error.to_string())
            },
        )
        .unwrap();
        let handle = router.handle();
        let mut driver =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(router), (640, 360));
        let document_before = driver.dump_json();

        driver
            .perform_named("open-settings", &Action::Activate)
            .unwrap();
        assert_eq!(handle.current(), "settings");
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Settings screen")
        );
        driver
            .perform_named("back-home", &Action::Activate)
            .unwrap();
        assert_eq!(handle.current(), "home");
        assert_eq!(driver.dump_json(), document_before);

        let audit = driver.native_app_audit();
        assert!(audit.is_certified(), "{:?}", audit.issues);
        driver
            .invoke_action(
                &tilo_actions::ActionAddress::new("tilo.control:open-settings", "invoke"),
                &serde_json::Map::new(),
                false,
                None,
            )
            .unwrap();
        assert_eq!(handle.current(), "settings");
        assert!(driver.native_app_audit().is_certified());
        assert_eq!(driver.dump_json(), document_before);
    }

    #[test]
    fn navigation_to_an_unknown_registered_screen_is_disabled() {
        fn find_missing(
            node: &crate::headless::InspectionNode,
        ) -> Option<&crate::a11y::AccessNode> {
            node.semantics
                .as_ref()
                .filter(|semantics| semantics.name.as_deref() == Some("Missing"))
                .or_else(|| node.children.iter().find_map(find_missing))
        }

        let mut document = sample();
        if let UiNode::Column { children, .. } = &mut document.root {
            children.push(UiNode::Button {
                id: "missing".into(),
                label: "Missing".into(),
                accessible_name: None,
                tooltip: None,
                on_click_path: None,
                click_value: None,
                navigate: Some(UiNavigation::Push {
                    route: "missing".into(),
                }),
                enabled: true,
                role: UiButtonRole::Secondary,
            });
        }
        let route = crate::widget::RouteHandle::new("home", ["home"]).unwrap();
        let root = document
            .build_with(
                &Theme::dark(),
                &UiBuildContext::default().with_router(route.clone()),
            )
            .unwrap();
        let mut driver = crate::headless::FrameHarness::new(crate::State::new(), root, (640, 360));
        let inspection = driver.inspection();
        let missing = find_missing(&inspection.root).expect("missing-route button remains visible");
        assert!(missing.state.disabled);
        driver
            .perform_named("missing", &Action::Activate)
            .expect("a disabled button ignores activation without crashing");
        assert_eq!(route.current(), "home");
    }

    #[test]
    fn validation_is_atomic_and_accessibility_is_required() {
        let handle = UiGraphHandle::new(sample()).unwrap();
        let before = handle.to_json_pretty().unwrap();
        let error = handle
            .update_node("save", |node| {
                if let UiNode::Button {
                    label,
                    accessible_name,
                    ..
                } = node
                {
                    label.clear();
                    *accessible_name = None;
                }
            })
            .unwrap_err();
        assert!(matches!(error, UiGraphError::MissingAccessibleName(_)));
        assert_eq!(handle.to_json_pretty().unwrap(), before);
    }

    #[test]
    fn duplicate_ids_and_unknown_fields_are_rejected() {
        let mut duplicate = sample();
        if let UiNode::Column { children, .. } = &mut duplicate.root {
            *children.last_mut().unwrap() = UiNode::Spacer {
                id: "title-input".into(),
            };
        }
        assert!(matches!(
            duplicate.validate(),
            Err(UiGraphError::DuplicateId(_))
        ));
        let json = r#"{"version":1,"root":{"type":"spacer","id":"root","mystery":1}}"#;
        assert!(matches!(
            UiDocument::from_json(json),
            Err(UiGraphError::Json(_))
        ));
    }

    #[test]
    fn live_edit_rebuilds_the_real_subtree_and_round_trips() {
        let handle = UiGraphHandle::new(sample()).unwrap();
        assert_eq!(handle.document().unwrap().nodes().len(), 4);
        assert!(matches!(handle.node("save"), Ok(UiNode::Button { .. })));
        let live = handle.widget(&Theme::dark()).unwrap();
        let mut driver =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(live), (640, 360));
        assert!(driver.inspection_json().unwrap().contains("Save"));

        handle
            .update_node("save", |node| {
                if let UiNode::Button { label, role, .. } = node {
                    *label = "Publish".into();
                    *role = UiButtonRole::Destructive;
                }
            })
            .unwrap();
        driver.relayout();
        assert!(driver.inspection_json().unwrap().contains("Publish"));
        assert!(
            !driver
                .inspection_json()
                .unwrap()
                .contains("\"name\": \"Save\"")
        );
        let saved = handle.to_json_pretty().unwrap();
        let reopened = UiDocument::from_json(&saved).unwrap();
        assert!(matches!(
            reopened.node("save"),
            Some(UiNode::Button { label, .. }) if label == "Publish"
        ));
    }

    fn set_named_state(widget: &mut dyn Widget, id: &str, state: &str) -> bool {
        let Some(widget) = crate::widget::find_test_id_mut(widget, id) else {
            return false;
        };
        widget.set_focus(true);
        crate::undo::set_state(widget, state);
        true
    }

    fn named_state(widget: &dyn Widget, id: &str) -> Option<String> {
        crate::undo::get_state(crate::widget::find_test_id(widget, id)?)
    }

    fn named_widget_ptr(widget: &dyn Widget, id: &str) -> Option<usize> {
        crate::widget::find_test_id(widget, id)
            .map(|widget| (widget as *const dyn Widget).cast::<()>().addr())
    }

    #[test]
    fn live_reconcile_preserves_text_input_ephemeral_state() {
        let document = UiDocument::new(UiNode::Column {
            id: "root".into(),
            children: vec![
                UiNode::Text {
                    id: "title".into(),
                    text: UiTextSource::Static {
                        value: "Hello".into(),
                    },
                    role: UiTextRole::Title,
                },
                UiNode::TextInput {
                    id: "name".into(),
                    label: None,
                    accessible_name: "Name".into(),
                    placeholder: "Type here".into(),
                    value_path: None,
                    initial_value: "Ada".into(),
                    multiline: false,
                    disabled: false,
                },
            ],
            gap: Some(8.0),
            no_padding: false,
            intrinsic_height: false,
            fill_remaining_height: false,
        });
        let handle = UiGraphHandle::new(document).unwrap();
        let mut live = handle.widget(&Theme::dark()).unwrap();
        assert!(set_named_state(&mut live, "name", "2"));
        let before = named_state(&live, "name");
        assert_eq!(before.as_deref(), Some("2"));
        let before_ptr = named_widget_ptr(&live, "name").expect("name widget");

        handle
            .update_node("title", |node| {
                if let UiNode::Text {
                    text: UiTextSource::Static { value },
                    ..
                } = node
                {
                    *value = "Projects".into();
                }
            })
            .unwrap();
        let doc = AutoCommit::new();
        Widget::sync(&mut live, &doc, &automerge::ROOT);
        assert_eq!(named_state(&live, "name"), before);
        assert_eq!(
            named_widget_ptr(&live, "name"),
            Some(before_ptr),
            "unchanged TextInput must keep the same widget instance"
        );
        assert!(live.take_structure_dirty());
    }

    #[test]
    fn live_reconcile_transfers_ephemeral_state_when_leaf_props_change() {
        let document = UiDocument::new(UiNode::Column {
            id: "root".into(),
            children: vec![UiNode::TextInput {
                id: "name".into(),
                label: None,
                accessible_name: "Name".into(),
                placeholder: "Before".into(),
                value_path: None,
                initial_value: "Ada".into(),
                multiline: false,
                disabled: false,
            }],
            gap: None,
            no_padding: false,
            intrinsic_height: false,
            fill_remaining_height: false,
        });
        let handle = UiGraphHandle::new(document).unwrap();
        let mut live = handle.widget(&Theme::dark()).unwrap();
        assert!(set_named_state(&mut live, "name", "4"));
        let before = named_state(&live, "name");
        assert_eq!(before.as_deref(), Some("4"));
        let before_ptr = named_widget_ptr(&live, "name").expect("name widget");

        handle
            .apply_edits(vec![UiGraphEdit::SetProperty {
                id: "name".into(),
                property: "placeholder".into(),
                value: serde_json::json!("After"),
            }])
            .unwrap();
        let doc = AutoCommit::new();
        Widget::sync(&mut live, &doc, &automerge::ROOT);
        assert_eq!(named_state(&live, "name"), before);
        assert_ne!(
            named_widget_ptr(&live, "name"),
            Some(before_ptr),
            "leaf property changes rebuild the widget"
        );
        assert!(matches!(
            handle.node("name"),
            Ok(UiNode::TextInput { placeholder, .. }) if placeholder == "After"
        ));
    }

    #[test]
    fn live_graph_reject_bad_apply_keeps_last_good_tree() {
        let handle = UiGraphHandle::new(sample()).unwrap();
        let mut live = handle.widget(&Theme::dark()).unwrap();
        let before = handle.to_json_pretty().unwrap();
        let edits = r#"[{"op":"set-property","id":"save","property":"label","value":""}]"#;
        let payload = serde_json::to_vec(&serde_json::json!({ "edits": edits })).unwrap();
        let error = run_operation(
            &mut live,
            crate::tilo_actions::OperationKind::Effect,
            "tilo.ui.graph.apply",
            &payload,
        )
        .unwrap_err();
        assert!(
            error.contains("edit") || error.contains("Unknown") || error.contains("missing"),
            "rejected apply must surface an error, got {error}"
        );
        assert_eq!(handle.to_json_pretty().unwrap(), before);
        let doc = AutoCommit::new();
        Widget::sync(&mut live, &doc, &automerge::ROOT);
        assert!(
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(live), (640, 360))
                .inspection_json()
                .unwrap()
                .contains("Save"),
            "last good tree must stay on screen after a rejected apply"
        );
    }

    #[test]
    fn poll_live_source_rejects_invalid_json_without_replacing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("main.json");
        std::fs::write(
            &path,
            r#"{"version":1,"root":{"type":"text","id":"title","text":{"source":"static","value":"Good"},"role":"title"}}"#,
        )
        .unwrap();
        let handle = UiGraphHandle::open_live(&path).unwrap();
        let before = handle.to_json_pretty().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "{not-valid-graph").unwrap();
        assert!(handle.poll_live_source().is_err());
        assert_eq!(handle.to_json_pretty().unwrap(), before);
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            &path,
            r#"{"version":1,"root":{"type":"button","id":"title","label":"","accessibleName":null}}"#,
        )
        .unwrap();
        assert!(handle.poll_live_source().is_err());
        assert_eq!(handle.to_json_pretty().unwrap(), before);
    }

    #[test]
    fn live_graph_actions_apply_and_dump() {
        let handle = UiGraphHandle::new(sample()).unwrap();
        let mut live = handle.widget(&Theme::dark()).unwrap();
        let resources = crate::resources::resources(&live, &AutoCommit::new());
        assert!(
            resources
                .iter()
                .any(|resource| resource.kind == "tilo.ui-graph")
        );
        let edits = r#"[{"op":"set-property","id":"save","property":"label","value":"Publish"}]"#;
        let payload = serde_json::to_vec(&serde_json::json!({ "edits": edits })).unwrap();
        let output = run_operation(
            &mut live,
            crate::tilo_actions::OperationKind::Effect,
            "tilo.ui.graph.apply",
            &payload,
        )
        .unwrap()
        .unwrap();
        assert!(output.value.is_none());
        assert!(matches!(
            handle.node("save"),
            Ok(UiNode::Button { label, .. }) if label == "Publish"
        ));
        let dumped = run_operation(
            &mut live,
            crate::tilo_actions::OperationKind::View,
            "tilo.ui.graph.dump",
            b"null",
        )
        .unwrap()
        .unwrap();
        assert!(
            dumped
                .value
                .as_ref()
                .and_then(serde_json::Value::as_str)
                .is_some_and(|json| json.contains("Publish"))
        );
    }

    #[test]
    fn open_live_reloads_when_the_file_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("main.json");
        std::fs::write(
            &path,
            r#"{"version":1,"root":{"type":"text","id":"title","text":{"source":"static","value":"Before"},"role":"title"}}"#,
        )
        .unwrap();
        let handle = UiGraphHandle::open_live(&path).unwrap();
        assert!(matches!(
            handle.node("title"),
            Ok(UiNode::Text {
                text: UiTextSource::Static { value },
                ..
            }) if value == "Before"
        ));
        // Ensure the filesystem mtime advances on coarse clocks.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            &path,
            r#"{"version":1,"root":{"type":"text","id":"title","text":{"source":"static","value":"After"},"role":"title"}}"#,
        )
        .unwrap();
        assert!(handle.poll_live_source().unwrap());
        assert!(matches!(
            handle.node("title"),
            Ok(UiNode::Text {
                text: UiTextSource::Static { value },
                ..
            }) if value == "After"
        ));
    }

    #[test]
    fn structural_insert_is_validated_and_atomic() {
        let mut document = sample();
        document
            .insert_child(
                "root",
                1,
                UiNode::Text {
                    id: "subtitle".into(),
                    text: UiTextSource::Static {
                        value: "A small project".into(),
                    },
                    role: UiTextRole::Caption,
                },
            )
            .unwrap();
        let ids: Vec<_> = document.nodes().iter().map(|node| node.id()).collect();
        assert_eq!(
            ids,
            ["root", "title-view", "subtitle", "title-input", "save"]
        );

        let before = document.clone();
        let error = document
            .insert_child("root", 0, UiNode::Spacer { id: "save".into() })
            .unwrap_err();
        assert!(matches!(error, UiGraphError::DuplicateId(id) if id == "save"));
        assert_eq!(document, before, "a failed structural edit is atomic");
    }

    #[test]
    fn move_reparents_and_reorders_without_losing_the_subtree() {
        let mut document = sample();
        document
            .insert_child(
                "root",
                1,
                UiNode::Row {
                    id: "actions".into(),
                    children: vec![UiNode::Spacer {
                        id: "action-gap".into(),
                    }],
                    no_wrap: true,
                    fill_remaining_height: false,
                },
            )
            .unwrap();
        document.move_node("save", "actions", 0).unwrap();
        let actions = document.node("actions").unwrap();
        assert_eq!(
            actions
                .children()
                .iter()
                .map(|child| child.id())
                .collect::<Vec<_>>(),
            ["save", "action-gap"]
        );
        assert!(matches!(
            document.move_node("actions", "save", 0),
            Err(UiGraphError::MoveIntoDescendant { .. })
        ));
        assert!(document.node("save").is_some());
    }

    #[test]
    fn remove_and_replace_obey_root_and_container_invariants() {
        let mut document = UiDocument::new(UiNode::Container {
            id: "root".into(),
            child: Box::new(UiNode::Text {
                id: "body".into(),
                text: UiTextSource::Static {
                    value: "Before".into(),
                },
                role: UiTextRole::Body,
            }),
            background: true,
            no_padding: false,
            fill_height: false,
            width: None,
            height: None,
        });
        assert!(matches!(
            document.remove_node("body"),
            Err(UiGraphError::RequiredContainerChild { .. })
        ));
        document
            .replace_node(
                "body",
                UiNode::Button {
                    id: "continue".into(),
                    label: "Continue".into(),
                    accessible_name: None,
                    tooltip: None,
                    on_click_path: None,
                    click_value: None,
                    navigate: None,
                    enabled: true,
                    role: UiButtonRole::Primary,
                },
            )
            .unwrap();
        assert!(document.node("body").is_none());
        assert!(document.node("continue").is_some());
        assert!(matches!(
            document.remove_node("root"),
            Err(UiGraphError::CannotRemoveRoot)
        ));
    }

    #[test]
    fn live_graph_history_is_atomic_bounded_and_invalidated_by_new_edits() {
        let handle = UiGraphHandle::new(sample()).unwrap();
        assert!(!handle.can_undo());
        assert!(!handle.can_redo());

        handle
            .update_node("save", |node| {
                if let UiNode::Button { label, .. } = node {
                    *label = "Publish".into();
                }
            })
            .unwrap();
        assert!(handle.can_undo());
        assert!(!handle.can_redo());

        let invalid = handle.update_node("save", |node| {
            if let UiNode::Button { id, .. } = node {
                *id = "root".into();
            }
        });
        assert!(matches!(invalid, Err(UiGraphError::DuplicateId(_))));
        assert!(handle.undo().unwrap());
        assert!(matches!(
            handle.node("save"),
            Ok(UiNode::Button { label, .. }) if label == "Save"
        ));
        assert!(
            !handle.can_undo(),
            "the rejected edit did not enter history"
        );
        assert!(handle.can_redo());

        assert!(handle.redo().unwrap());
        assert!(matches!(
            handle.node("save"),
            Ok(UiNode::Button { label, .. }) if label == "Publish"
        ));
        assert!(handle.undo().unwrap());
        handle
            .update_node("save", |node| {
                if let UiNode::Button { label, .. } = node {
                    *label = "Share".into();
                }
            })
            .unwrap();
        assert!(!handle.can_redo(), "a divergent edit starts a new future");

        for index in 0..UI_GRAPH_HISTORY_LIMIT.saturating_add(5) {
            handle
                .update_node("save", |node| {
                    if let UiNode::Button { label, .. } = node {
                        *label = format!("Share {index}");
                    }
                })
                .unwrap();
        }
        let mut undo_count = 0;
        while handle.undo().unwrap() {
            undo_count += 1;
        }
        assert_eq!(undo_count, UI_GRAPH_HISTORY_LIMIT);
        handle.clear_history().unwrap();
        assert!(!handle.can_undo());
        assert!(!handle.can_redo());
    }

    #[test]
    fn grouped_live_edits_share_one_undo_baseline_until_finished() {
        let handle = UiGraphHandle::new(sample()).unwrap();
        for label in ["Publ", "Publish"] {
            handle
                .update_node_grouped("save", "button-label", |node| {
                    if let UiNode::Button { label: current, .. } = node {
                        *current = label.into();
                    }
                })
                .unwrap();
        }
        assert!(handle.undo().unwrap());
        assert!(matches!(
            handle.node("save"),
            Ok(UiNode::Button { label, .. }) if label == "Save"
        ));
        assert!(
            !handle.can_undo(),
            "the two keystroke updates formed one step"
        );
        assert!(handle.redo().unwrap());
        assert!(matches!(
            handle.node("save"),
            Ok(UiNode::Button { label, .. }) if label == "Publish"
        ));

        handle.finish_edit_group().unwrap();
        handle
            .update_node_grouped("save", "button-label", |node| {
                if let UiNode::Button { label, .. } = node {
                    *label = "Export".into();
                }
            })
            .unwrap();
        assert!(handle.undo().unwrap());
        assert!(matches!(
            handle.node("save"),
            Ok(UiNode::Button { label, .. }) if label == "Publish"
        ));
    }

    #[test]
    fn live_structural_edits_rebuild_and_save_the_same_graph() {
        let handle = UiGraphHandle::new(sample()).unwrap();
        let values = UiValues::default();
        let live = handle
            .widget_with(
                &Theme::dark(),
                UiBuildContext::default().with_values(values.clone()),
            )
            .unwrap();
        let mut driver =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(live), (640, 360));
        handle
            .insert_child(
                "root",
                0,
                UiNode::Button {
                    id: "new-project".into(),
                    label: "New project".into(),
                    accessible_name: None,
                    tooltip: None,
                    on_click_path: Some("new-project".into()),
                    click_value: Some("requested".into()),
                    navigate: None,
                    enabled: true,
                    role: UiButtonRole::Primary,
                },
            )
            .unwrap();
        driver.relayout();
        driver
            .perform_named("new-project", &Action::Activate)
            .unwrap();
        assert_eq!(
            values.take_click("new-project").as_deref(),
            Some("requested")
        );

        let removed = handle.remove_node("new-project").unwrap();
        assert_eq!(removed.id(), "new-project");
        driver.relayout();
        assert!(
            driver
                .perform_named("new-project", &Action::Activate)
                .is_err()
        );
        assert!(UiDocument::from_json(&handle.to_json_pretty().unwrap()).is_ok());
    }

    fn edit_builder_button_properties(driver: &mut crate::headless::FrameHarness) {
        driver
            .perform_named("builder-property", &Action::SetValue("Publish".into()))
            .unwrap();
        driver
            .perform_named(
                "builder-button-accessible-name",
                &Action::SetValue("Publish changes".into()),
            )
            .unwrap();
        driver
            .perform_named(
                "builder-button-tooltip",
                &Action::SetValue("Publish this document".into()),
            )
            .unwrap();
        driver
            .perform_named(
                "builder-button-click-path",
                &Action::SetValue("publish-requested".into()),
            )
            .unwrap();
        driver
            .perform_named(
                "builder-button-click-value",
                &Action::SetValue("yes".into()),
            )
            .unwrap();
        driver
            .perform_named("builder-button-role", &Action::Activate)
            .unwrap();
        driver.relayout();
        driver
            .perform_named("builder-button-navigation", &Action::Activate)
            .unwrap();
        driver.relayout();
        driver
            .perform_named("builder-button-route", &Action::SetValue("settings".into()))
            .unwrap();
        driver
            .perform_named("builder-button-enabled", &Action::Activate)
            .unwrap();
        driver.relayout();
    }

    #[test]
    fn graphical_editor_changes_and_saves_the_live_preview_graph() {
        let handle = UiGraphHandle::new(sample()).unwrap();
        let saved = Rc::new(RefCell::new(None));
        let saved_from_callback = Rc::clone(&saved);
        let editor = UiGraphEditor::new(handle.clone(), Theme::dark())
            .unwrap()
            .on_save(move |source| {
                *saved_from_callback.borrow_mut() = Some(source.to_owned());
                Ok(())
            });
        let mut driver =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(editor), (1_400, 800));

        let initial = driver.inspection_json().unwrap();
        assert!(initial.contains("builder-add-button"));
        assert!(initial.contains("builder-add-checkbox"));
        assert!(initial.contains("title-input"));

        driver
            .perform_named("builder-add-button", &Action::Activate)
            .unwrap();
        driver.relayout();
        assert!(matches!(
            handle.node("button"),
            Ok(UiNode::Button { label, .. }) if label == "Button"
        ));

        driver
            .perform_named("builder-undo", &Action::Activate)
            .unwrap();
        driver.relayout();
        assert!(handle.node("button").is_err());
        driver
            .perform_named("builder-redo", &Action::Activate)
            .unwrap();
        driver.relayout();
        assert!(handle.node("button").is_ok());
        driver
            .perform_named("builder-node-button", &Action::Activate)
            .unwrap();
        driver.relayout();

        edit_builder_button_properties(&mut driver);
        assert!(matches!(
            handle.node("button"),
            Ok(UiNode::Button {
                label,
                accessible_name: Some(accessible_name),
                tooltip: Some(tooltip),
                on_click_path: Some(on_click_path),
                click_value: Some(click_value),
                navigate: Some(UiNavigation::Push { route }),
                enabled: false,
                role: UiButtonRole::Primary,
                ..
            }) if label == "Publish"
                && accessible_name == "Publish changes"
                && tooltip == "Publish this document"
                && on_click_path == "publish-requested"
                && click_value == "yes"
                && route == "settings"
        ));
        assert!(driver.inspection_json().unwrap().contains("Publish"));

        driver
            .perform_named("builder-save", &Action::Activate)
            .unwrap();
        let saved = saved.borrow().clone().expect("Save callback ran");
        let reopened = UiDocument::from_json(&saved).unwrap();
        assert!(matches!(
            reopened.node("button"),
            Some(UiNode::Button {
                label,
                accessible_name: Some(accessible_name),
                on_click_path: Some(on_click_path),
                navigate: Some(UiNavigation::Push { route }),
                enabled: false,
                role: UiButtonRole::Primary,
                ..
            }) if label == "Publish"
                && accessible_name == "Publish changes"
                && on_click_path == "publish-requested"
                && route == "settings"
        ));
    }

    #[test]
    fn graphical_editor_validates_property_edits_atomically_and_tracks_renamed_ids() {
        let handle = UiGraphHandle::new(sample()).unwrap();
        let editor = UiGraphEditor::new(handle.clone(), Theme::dark()).unwrap();
        let mut driver =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(editor), (1_400, 800));

        driver
            .perform_named("builder-add-container", &Action::Activate)
            .unwrap();
        driver.relayout();
        let before_invalid_number = handle.to_json_pretty().unwrap();
        driver
            .perform_named("builder-container-width", &Action::SetValue("wide".into()))
            .unwrap();
        assert_eq!(handle.to_json_pretty().unwrap(), before_invalid_number);

        driver
            .perform_named("builder-node-id", &Action::SetValue("panel".into()))
            .unwrap();
        driver.relayout();
        assert!(handle.node("container").is_err());
        assert!(handle.node("panel").is_ok());

        let before_duplicate_id = handle.to_json_pretty().unwrap();
        driver
            .perform_named("builder-node-id", &Action::SetValue("root".into()))
            .unwrap();
        assert_eq!(handle.to_json_pretty().unwrap(), before_duplicate_id);
        assert!(driver.inspection_json().unwrap().contains("panel"));

        driver
            .perform_named("builder-node-root", &Action::Activate)
            .unwrap();
        driver.relayout();
        driver
            .perform_named("builder-add-checkbox", &Action::Activate)
            .unwrap();
        driver.relayout();
        let before_invalid_binding = handle.to_json_pretty().unwrap();
        driver
            .perform_named("builder-check-value-path", &Action::SetValue(String::new()))
            .unwrap();
        assert_eq!(handle.to_json_pretty().unwrap(), before_invalid_binding);
    }

    #[test]
    fn graphical_editor_keeps_renames_live_and_routes_shortcuts_to_local_history() {
        let handle = UiGraphHandle::new(sample()).unwrap();
        let editor = UiGraphEditor::new(handle.clone(), Theme::dark()).unwrap();
        let mut driver =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(editor), (1_400, 800));

        driver
            .perform_named("builder-add-container", &Action::Activate)
            .unwrap();
        driver.relayout();
        driver
            .perform_named("builder-node-id", &Action::SetValue("panel".into()))
            .unwrap();
        driver.relayout();
        driver
            .perform_named("builder-node-id", &Action::SetValue("panel-final".into()))
            .unwrap();
        driver.relayout();
        assert!(handle.node("container").is_err());
        assert!(handle.node("panel").is_err());
        assert!(handle.node("panel-final").is_ok());

        driver
            .perform_named("builder-node-id", &Action::Focus)
            .unwrap();
        driver.set_modifiers(&["ctrl".to_owned()]);
        driver.key("z");
        assert!(handle.node("container").is_ok());
        assert!(handle.node("panel-final").is_err());

        driver.set_modifiers(&["ctrl".to_owned(), "shift".to_owned()]);
        driver.key("z");
        assert!(handle.node("panel-final").is_ok());
        driver.set_modifiers(&[]);
    }

    #[test]
    fn graphical_editor_exposes_layout_and_text_source_choices() {
        let handle = UiGraphHandle::new(sample()).unwrap();
        let editor = UiGraphEditor::new(handle.clone(), Theme::dark()).unwrap();
        let mut driver =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(editor), (1_400, 800));

        driver
            .perform_named("builder-column-gap", &Action::SetValue("12.5".into()))
            .unwrap();
        driver
            .perform_named("builder-column-no-padding", &Action::Activate)
            .unwrap();
        driver.relayout();
        assert!(matches!(
            handle.node("root"),
            Ok(UiNode::Column {
                gap: Some(12.5),
                no_padding: true,
                ..
            })
        ));

        driver
            .perform_named("builder-node-title-view", &Action::Activate)
            .unwrap();
        driver.relayout();
        driver
            .perform_named("builder-text-role", &Action::Activate)
            .unwrap();
        driver.relayout();
        driver
            .perform_named("builder-text-source", &Action::Activate)
            .unwrap();
        driver.relayout();
        assert!(matches!(
            handle.node("title-view"),
            Ok(UiNode::Text {
                text: UiTextSource::Static { value },
                role: UiTextRole::Heading,
                ..
            }) if value == "title"
        ));
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Text source: Static")
        );
    }
}

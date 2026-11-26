//! Versioned, serializable UI object graphs compiled into ordinary widgets. The graph is
//! a source format, not a second rendering system.

use tilo_ui_base::LocalState;
use tilo_ui_base::theme::Theme;
use crate::time::SystemTime;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::{widget::AppModel, widget::ModelPath};
use crate::{widget::Button, widget::ButtonRole, widget::Checkbox, widget::Column, widget::Container, widget::Row, widget::Scrollable, widget::Text, widget::TextInput, widget::Toggle, widget::labeled_check, widget::labeled_field};
use tilo_ui_base::{widget::Element, widget::Spacer, widget::Widget};
use automerge::{AutoCommit, ObjId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use taffy::{NodeId, Style, TaffyTree};
use tilo_sort::CompactSort as _;
use vello::kurbo::Rect;

/// Directory of live `*.json` screen graphs.
pub const LIVE_GRAPH_DIR_ENV: &str = "TILO_UI_GRAPH_DIR";

/// Operation type for live graph mutations published by [`LiveUi`].
pub const LIVE_GRAPH_OPERATION: &str = "tilo.ui.graph";

pub use tilo_ui_graph::{
    GraphPropKind, UI_GRAPH_VERSION, UiButtonRole, UiDocument, UiGraphEdit, UiGraphError,
    UiNavigation, UiNode, UiTextRole, UiTextSource, parse_ui_graph_edits,
};

/// Inputs passed to one app-owned serialized-graph component factory.
#[derive(Clone, Copy, Debug)]
pub struct UiComponentRequest<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub props: &'a serde_json::Map<String, serde_json::Value>,
    pub values: &'a UiValues,
}

/// A graph's text and checked values, by each control's `value_path`, and which buttons
/// were clicked, by each button's `on_click_path`. The values are view state, never
/// written to the document: every input naming one path shows and edits the same value,
/// and the app reads them when it acts on them, submitting its own resource actions.
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

    /// Route the inputs naming `path` through the app: they show `shown`, which the app
    /// keeps current, and each edit invokes `action` with the whole new text, so it is
    /// checked, attributed and undone like every other edit of the app.
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

    /// Route checked controls through the resource's typed action.
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

    /// Record a click of a button naming `path`, carrying `value`.
    pub fn click(&self, path: &str, value: &str) {
        self.clicks
            .borrow_mut()
            .insert(path.to_owned(), value.to_owned());
    }

    /// The latest unhandled click of a button naming `path`: its `click_value`, or
    /// `"true"` when it names none.
    #[must_use]
    pub fn take_click(&self, path: &str) -> Option<String> {
        self.clicks.borrow_mut().remove(path)
    }

    /// An input over `path`: the app's value edited through its action when the path is
    /// routed, else the shared typed value, which starts as `initial`.
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
        paths.compact_sort();
        let mut routes: Vec<String> = self.routes.borrow().keys().cloned().collect();
        routes.compact_sort();
        let mut clicks: Vec<String> = self.clicks.borrow().keys().cloned().collect();
        clicks.compact_sort();
        let mut booleans: Vec<String> = self.booleans.borrow().keys().cloned().collect();
        booleans.compact_sort();
        let mut bool_routes: Vec<String> = self.bool_routes.borrow().keys().cloned().collect();
        bool_routes.compact_sort();
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
    router: Option<tilo_ui_base::widget::RouteHandle>,
    components: Rc<HashMap<String, Rc<UiComponentFactory>>>,
    component_placeholders: bool,
    values: UiValues,
}

impl fmt::Debug for UiBuildContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut components: Vec<_> = self.components.keys().collect();
        components.compact_sort();
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
    pub fn with_router(mut self, router: tilo_ui_base::widget::RouteHandle) -> Self {
        self.router = Some(router);
        self
    }

    /// Register one app-owned component factory by its stable graph name.
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

    /// Render unresolved components as named text placeholders. Intended for graph
    /// editors that cannot load application code.
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

/// Compile a validated serialized graph into ordinary Tilo widgets.
pub trait UiDocumentExt {
    /// Build the retained widget subtree represented by this graph.
    fn build(&self, theme: &Theme) -> Result<Element, UiGraphError>;

    /// Build with process-local services such as screen navigation.
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
            // A click path reports the click to the app through its values; the button
            // never writes the document.
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
            if record.is_some() || navigation.is_some() {
                button = button.on_open(move || {
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
            states.insert(id.to_owned(), tilo_ui_base::widget::undo_points::get_state(widget));
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
        tilo_ui_base::widget::undo_points::set_state(widget, state);
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
    pub fn from_json(source: &str) -> Result<Self, UiGraphError> {
        Self::new(UiDocument::from_json(source)?)
    }

    /// Load a screen from the host's live graph directory when one is set, otherwise from
    /// the embedded source.
    pub fn from_embedded_or_live(screen: &str, embedded: &str) -> Result<Self, UiGraphError> {
        if let Some(dir) = crate::process::environment().ui_graph_dir.clone() {
            let path = dir.join(format!("{screen}.json"));
            return Self::open_live(path);
        }
        Self::from_json(embedded)
    }

    /// Open a graph file and keep reloading it when its mtime advances.
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
    pub fn apply_edits(&self, edits: Vec<UiGraphEdit>) -> Result<(), UiGraphError> {
        self.edit(|candidate| candidate.apply_edits(edits))
    }

    /// Snapshot the current source document. Returns [`UiGraphError::Poisoned`] if
    /// another thread panicked while holding the graph lock.
    pub fn document(&self) -> Result<UiDocument, UiGraphError> {
        self.inner
            .read()
            .map(|state| state.document.clone())
            .map_err(|_| UiGraphError::Poisoned)
    }

    /// Serialize the current source document as formatted JSON.
    pub fn to_json_pretty(&self) -> Result<String, UiGraphError> {
        self.document()?.to_json_pretty()
    }

    /// Inspect one source node without exposing the handle's lock or mutable live state.
    /// Returns a graph lock error or [`UiGraphError::UnknownNode`].
    pub fn node(&self, id: &str) -> Result<UiNode, UiGraphError> {
        self.document()?
            .node(id)
            .cloned()
            .ok_or_else(|| UiGraphError::UnknownNode(id.to_owned()))
    }

    /// Validate and atomically replace the complete source graph.
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

    /// Whether the live graph has a successful edit that can be undone. Invalid and no-op
    /// edits never enter history.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        self.inner.read().is_ok_and(|state| !state.undo.is_empty())
    }

    /// Whether an undone edit can be reapplied.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        self.inner.read().is_ok_and(|state| !state.redo.is_empty())
    }

    /// Restore the graph before its most recent successful edit. Returns
    /// [`UiGraphError::Poisoned`] if another thread panicked while holding the graph
    /// lock.
    pub fn undo(&self) -> Result<bool, UiGraphError> {
        self.step_history(tilo_ui_base::widget::undo_access::UndoRequest::Undo)
    }

    /// Reapply the most recently undone graph edit. Returns [`UiGraphError::Poisoned`] if
    /// another thread panicked while holding the graph lock.
    pub fn redo(&self) -> Result<bool, UiGraphError> {
        self.step_history(tilo_ui_base::widget::undo_access::UndoRequest::Redo)
    }

    fn step_history(
        &self,
        request: tilo_ui_base::widget::undo_access::UndoRequest,
    ) -> Result<bool, UiGraphError> {
        let mut guard = self.inner.write().map_err(|_| UiGraphError::Poisoned)?;
        let state = &mut *guard;
        let (from, to) = match request {
            tilo_ui_base::widget::undo_access::UndoRequest::Undo => (&mut state.undo, &mut state.redo),
            tilo_ui_base::widget::undo_access::UndoRequest::Redo => (&mut state.redo, &mut state.undo),
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

    /// Forget retained undo and redo documents without changing the graph. Returns
    /// [`UiGraphError::Poisoned`] if another thread panicked while holding the graph
    /// lock.
    pub fn clear_history(&self) -> Result<(), UiGraphError> {
        let mut state = self.inner.write().map_err(|_| UiGraphError::Poisoned)?;
        state.undo.clear();
        state.redo.clear();
        state.active_edit_group = None;
        drop(state);
        Ok(())
    }

    /// End a continuous edit without changing the document or its history. Returns
    /// [`UiGraphError::Poisoned`] if another thread panicked while holding the graph
    /// lock.
    pub fn finish_edit_group(&self) -> Result<(), UiGraphError> {
        let mut state = self.inner.write().map_err(|_| UiGraphError::Poisoned)?;
        state.active_edit_group = None;
        drop(state);
        Ok(())
    }

    /// Mutate one source node, validate the complete candidate graph, then publish it
    /// atomically.
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

    /// Mutate one node as part of a continuous undo group. Failed and no-op candidates
    /// never affect history.
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

    /// Atomically insert a subtree into a live row or column. Returns the corresponding
    /// [`UiDocument::insert_child`] error, a lock error, or
    /// [`UiGraphError::ConcurrentEdit`].
    pub fn insert_child(
        &self,
        parent_id: &str,
        index: usize,
        node: UiNode,
    ) -> Result<(), UiGraphError> {
        self.edit(|candidate| candidate.insert_child(parent_id, index, node))
    }

    /// Atomically remove and return a live subtree. Returns the corresponding
    /// [`UiDocument::remove_node`] error, a lock error, or
    /// [`UiGraphError::ConcurrentEdit`].
    pub fn remove_node(&self, id: &str) -> Result<UiNode, UiGraphError> {
        self.edit(|candidate| candidate.remove_node(id))
    }

    /// Atomically replace a live subtree, including the root. Returns the corresponding
    /// [`UiDocument::replace_node`] error, a lock error, or
    /// [`UiGraphError::ConcurrentEdit`].
    pub fn replace_node(&self, id: &str, replacement: UiNode) -> Result<(), UiGraphError> {
        self.edit(|candidate| candidate.replace_node(id, replacement))
    }

    /// Atomically reparent or reorder a live subtree. Returns the corresponding
    /// [`UiDocument::move_node`] error, a lock error, or
    /// [`UiGraphError::ConcurrentEdit`].
    pub fn move_node(
        &self,
        id: &str,
        new_parent_id: &str,
        index: usize,
    ) -> Result<(), UiGraphError> {
        self.edit(|candidate| candidate.move_node(id, new_parent_id, index))
    }

    /// Compile the live graph into ordinary retained widgets.
    pub fn widget(&self, theme: &Theme) -> Result<LiveUi, UiGraphError> {
        self.widget_with(theme, UiBuildContext::default())
    }

    /// Build a live graph with process-local services such as routing.
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
        tilo_files::native::atomic::write(&path, json.as_bytes())
            .map_err(|error| UiGraphError::Io(format!("publishing {}: {error}", path.display())))?;
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
        // Handle updates validate before publication.
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
    fn graph_resource(&self) -> crate::tilo_actions::ResourceSpec {
        use crate::tilo_actions::{
            ActionContract, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType,
            ResourceSpec, ResultSpec,
        };
        let id = self.seen_document.root.id();
        let mut resource = ResourceSpec::new(
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
        apply.type_mut().contract = ActionContract {
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

fn filling() -> Style {
    Style {
        size: taffy::Size::from_percent(1.0_f32, 1.0_f32),
        ..Default::default()
    }
}

impl Widget for LiveUi {
    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::resources::PublishesResources,
        dyn tilo_ui_base::widget::ResourceOperations
    );

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
        tilo_ui_base::widget::TaffyTreeExt::widget_node(taffy, filling(), &[child], node_map, self)
    }

    tilo_ui_base::forward_to_child!(child, rect);

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
}

impl tilo_ui_base::widget::ResourceOperations for LiveUi {
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
        op: &tilo_ui_base::widget::ResourceOperation<'_>,
        doc: &mut tilo_ui_base::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<tilo_ui_base::widget::ResourceOperationStart>, String> {
        if let Some(started) = tilo_ui_base::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let request = op.request();
        if !(request.type_id.starts_with(LIVE_GRAPH_OPERATION)
            && request.kind != crate::tilo_actions::OperationKind::Document)
        {
            return Ok(None);
        }
        self.invoke_graph_operation(request.type_id, request.payload)
            .map(|output| Some(tilo_ui_base::widget::ResourceOperationStart::Completed(output)))
    }
}

impl tilo_ui_base::widget::resources::PublishesResources for LiveUi {
    fn resources(&self, doc: &AutoCommit) -> Vec<crate::tilo_actions::ResourceSpec> {
        let mut resources = tilo_ui_base::widget::resources::resources(self.child.as_ref(), doc);
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

/// A native, accessible object-graph editor with a live production-widget preview.
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
        return Some((id.to_owned(), tilo_ui_base::widget::undo_points::get_state(widget)));
    }
    widget
        .children()
        .into_iter()
        .find_map(focused_widget_snapshot)
}

fn restore_widget_focus(widget: &mut dyn Widget, id: &str, state: Option<&str>) -> bool {
    let Some(widget) = tilo_ui_base::widget::find_test_id_mut(widget, id) else {
        return false;
    };
    widget.set_focus(true);
    if let Some(state) = state {
        tilo_ui_base::widget::undo_points::set_state(widget, state);
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
                .on_open(move || {
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
                // A save that landed says nothing; only a failure is news.
                Ok(()) => status.set(String::new()),
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
    tilo_ui_base::provides!(dyn tilo_ui_base::widget::undo_access::LocalUndo);

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
        tilo_ui_base::widget::TaffyTreeExt::widget_node(taffy, filling(), &[child], node_map, self)
    }

    tilo_ui_base::forward_to_child!(child, rect);

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

impl tilo_ui_base::widget::undo_access::LocalUndo for UiGraphEditor {
    fn handle_local_undo_request(
        &mut self,
        request: tilo_ui_base::widget::undo_access::UndoRequest,
    ) -> bool {
        if !widget_tree_has_focus(self.child.as_ref()) {
            return false;
        }
        let status = self.shared.borrow().status.clone();
        let result = match request {
            tilo_ui_base::widget::undo_access::UndoRequest::Undo => self.handle.undo(),
            tilo_ui_base::widget::undo_access::UndoRequest::Redo => self.handle.redo(),
        };
        match (request, result) {
            (tilo_ui_base::widget::undo_access::UndoRequest::Undo, Ok(true)) => {
                status.set("Undid graph edit".to_owned());
            }
            (tilo_ui_base::widget::undo_access::UndoRequest::Redo, Ok(true)) => {
                status.set("Redid graph edit".to_owned());
            }
            (request, Ok(false)) => status.set(format!("Nothing to {}", request.verb())),
            (_, Err(error)) => status.set(format!("History action failed: {error}")),
        }
        true
    }
}

#[cfg(test)]
#[path = "ui_graph_tests.rs"]
mod tests;

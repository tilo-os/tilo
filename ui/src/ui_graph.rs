//! Versioned, serializable UI object graphs compiled into ordinary widgets.
//!
//! The graph is a source format, not a second rendering system. [`UiNode`]
//! variants build the same [`crate::widget::Widget`] implementations an app
//! constructs in Rust, so accessibility, focus, model bindings, actions,
//! themes, inspection, and headless tests keep one implementation.

use crate::LocalState;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{
    Button, ButtonStyle, Checkbox, Column, Container, Element, Row, Scrollable, Spacer, Text,
    TextInput, Toggle, Widget, labeled_field,
};
use automerge::{AutoCommit, ObjId};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

pub use qos_ui_graph::{
    UI_GRAPH_VERSION, UiButtonStyle, UiDocument, UiGraphError, UiNavigation, UiNode, UiTextRole,
    UiTextSource,
};

/// Process-local services available while compiling a serialized graph.
#[derive(Clone, Debug, Default)]
pub struct UiBuildContext {
    router: Option<crate::widget::RouteHandle>,
}

impl UiBuildContext {
    #[must_use]
    pub fn with_router(mut self, router: crate::widget::RouteHandle) -> Self {
        self.router = Some(router);
        self
    }
}

impl From<UiButtonStyle> for ButtonStyle {
    fn from(value: UiButtonStyle) -> Self {
        match value {
            UiButtonStyle::Neutral => Self::Neutral,
            UiButtonStyle::Primary => Self::Primary,
            UiButtonStyle::Danger => Self::Danger,
            UiButtonStyle::Ghost => Self::Ghost,
        }
    }
}

impl From<UiTextRole> for TextRole {
    fn from(value: UiTextRole) -> Self {
        match value {
            UiTextRole::Caption => Self::Caption,
            UiTextRole::Label => Self::Label,
            UiTextRole::Body => Self::Body,
            UiTextRole::Reading => Self::Reading,
            UiTextRole::Title => Self::Title,
            UiTextRole::Heading => Self::Heading,
            UiTextRole::Display => Self::Display,
            UiTextRole::Code => Self::Code,
        }
    }
}

/// Compile a validated serialized graph into ordinary qOS widgets.
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
        Ok(build_node(&self.root, context))
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive source-node-to-widget match keeps graph construction reviewable and compiler-checked"
)]
fn build_node(node: &UiNode, context: &UiBuildContext) -> Element {
    match node {
        UiNode::Column {
            children,
            gap,
            no_padding,
            intrinsic_height,
            fill_remaining_height,
            ..
        } => {
            let mut column = Column::new();
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
            Box::new(children.iter().fold(column, |column, child| {
                column.push(build_node(child, context))
            }))
        }
        UiNode::Row {
            children,
            no_wrap,
            fill_remaining_height,
            ..
        } => {
            let mut row = Row::new();
            if *no_wrap {
                row = row.no_wrap();
            }
            if *fill_remaining_height {
                row = row.with_fill_remaining_height();
            }
            Box::new(
                children
                    .iter()
                    .fold(row, |row, child| row.push(build_node(child, context))),
            )
        }
        UiNode::Container {
            child,
            background,
            no_padding,
            fill_height,
            width,
            height,
            ..
        } => {
            let mut container = Container::new().with_child(build_node(child, context));
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
        UiNode::Text {
            id,
            text,
            role,
            size,
        } => {
            let text = match text {
                UiTextSource::Static { value } => Text::new(value),
                UiTextSource::Document { path } => Text::from_document_string(path),
            };
            let mut text = text.with_role((*role).into()).with_id(id);
            if let Some(size) = size {
                text = text.with_size(*size);
            }
            Box::new(text)
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
            style,
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
            let mut button = Button::new(id, label, on_click_path.as_deref())
                .with_enabled(*enabled && (navigation_bound || on_click_path.is_some()))
                .with_style((*style).into());
            if let Some(name) = accessible_name {
                button = button.with_accessible_name(name);
            }
            if let Some(tooltip) = tooltip {
                button = button.tooltip(tooltip);
            }
            if let Some(value) = click_value {
                button = button.with_click_value(value);
            }
            if let (Some(navigation), Some(router)) = (navigate, context.router.as_ref()) {
                let router = router.clone();
                button = match navigation {
                    UiNavigation::Push { route } if router.knows(route) => {
                        let route = route.clone();
                        button.on_click(move || {
                            let _ = router.push(route.clone());
                        })
                    }
                    UiNavigation::Replace { route } if router.knows(route) => {
                        let route = route.clone();
                        button.on_click(move || {
                            let _ = router.replace(route.clone());
                        })
                    }
                    UiNavigation::Back => button.on_click(move || {
                        let _ = router.back();
                    }),
                    UiNavigation::Forward => button.on_click(move || {
                        let _ = router.forward();
                    }),
                    UiNavigation::Push { .. } | UiNavigation::Replace { .. } => button,
                };
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
            let mut input = TextInput::local()
                .with_id(id)
                .with_accessible_name(name)
                .with_placeholder(placeholder)
                .with_initial_value(initial_value)
                .multiline(*multiline)
                .disabled(*disabled);
            if let Some(path) = value_path {
                input = input.with_on_change_path(path);
            }
            if let Some(label) = label {
                Box::new(labeled_field(label, input))
            } else {
                Box::new(input)
            }
        }
        UiNode::Checkbox {
            id,
            accessible_name,
            value_path,
            enabled,
        } => Box::new(
            Checkbox::new(id, value_path)
                .with_id(id)
                .with_accessible_name(accessible_name)
                .with_enabled(*enabled),
        ),
        UiNode::Toggle {
            id,
            accessible_name,
            value_path,
            enabled,
        } => Box::new(
            Toggle::new(id, value_path)
                .with_id(id)
                .with_accessible_name(accessible_name)
                .with_enabled(*enabled),
        ),
        UiNode::Spacer { .. } => Box::new(Spacer::new()),
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
        let previous = std::mem::replace(&mut state.document, document);
        push_graph_history(&mut state.undo, previous);
        state.redo.clear();
        state.active_edit_group = None;
        state.revision = state.revision.wrapping_add(1);
        drop(state);
        Ok(())
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
        let mut state = self.inner.write().map_err(|_| UiGraphError::Poisoned)?;
        let Some(previous) = state.undo.pop() else {
            return Ok(false);
        };
        let current = std::mem::replace(&mut state.document, previous);
        push_graph_history(&mut state.redo, current);
        state.active_edit_group = None;
        state.revision = state.revision.wrapping_add(1);
        drop(state);
        Ok(true)
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
        let mut state = self.inner.write().map_err(|_| UiGraphError::Poisoned)?;
        let Some(next) = state.redo.pop() else {
            return Ok(false);
        };
        let current = std::mem::replace(&mut state.document, next);
        push_graph_history(&mut state.undo, current);
        state.active_edit_group = None;
        state.revision = state.revision.wrapping_add(1);
        drop(state);
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
        let previous = std::mem::replace(&mut state.document, candidate);
        let continues_group = group.is_some() && state.active_edit_group.as_deref() == group;
        if !continues_group {
            push_graph_history(&mut state.undo, previous);
        }
        state.redo.clear();
        state.active_edit_group = group.map(str::to_owned);
        state.revision = state.revision.wrapping_add(1);
        drop(state);
        Ok(output)
    }

    fn revision(&self) -> Option<u64> {
        self.inner.read().ok().map(|state| state.revision)
    }
}

fn push_graph_history(history: &mut Vec<UiDocument>, document: UiDocument) {
    if history.len() == UI_GRAPH_HISTORY_LIMIT {
        history.remove(0);
    }
    history.push(document);
}

/// A retained host whose child is rebuilt after a validated graph edit.
pub struct LiveUi {
    handle: UiGraphHandle,
    theme: Theme,
    context: UiBuildContext,
    seen_revision: u64,
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
        let Ok((revision, document)) = self.handle.snapshot() else {
            return;
        };
        if revision == self.seen_revision {
            return;
        }
        // Handle updates validate before publication, so this can only fail
        // after a toolkit/schema bug. Keep the last known-good tree in that
        // case instead of replacing the app with an error placeholder.
        if let Ok(child) = document.build_with(&self.theme, &self.context) {
            self.child = child;
            self.seen_revision = revision;
            self.structure_dirty = true;
        }
    }
}

impl Widget for LiveUi {
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
        let node = crate::widget::TaffyTreeExt::parent_node(
            taffy,
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
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
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
        "LiveUi"
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

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        own | self.child.take_structure_dirty()
    }

    fn needs_idle_sync(&self) -> bool {
        self.handle
            .revision()
            .is_some_and(|revision| revision != self.seen_revision)
            || self.child.needs_idle_sync()
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
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

type SaveGraph = Box<dyn FnMut(&str) -> Result<(), String>>;

struct UiGraphEditorState {
    selected: String,
    chrome_revision: u64,
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
    seen_chrome_revision: u64,
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
            chrome_revision: 0,
            suppress_graph_revision: None,
            status: LocalState::new("Ready".to_owned()),
            save: None,
        }));
        let child = build_editor_child(&handle, &shared, &theme)?;
        Ok(Self {
            handle,
            theme,
            shared,
            seen_chrome_revision: 0,
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
        let (chrome_revision, suppressed) = {
            let state = self.shared.borrow();
            (state.chrome_revision, state.suppress_graph_revision)
        };
        if graph_revision != self.seen_graph_revision && suppressed == Some(graph_revision) {
            self.seen_graph_revision = graph_revision;
            self.shared.borrow_mut().suppress_graph_revision = None;
        }
        let graph_changed = graph_revision != self.seen_graph_revision;
        if !graph_changed && chrome_revision == self.seen_chrome_revision {
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
                self.seen_chrome_revision = chrome_revision;
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
        return Some((id.to_owned(), widget.get_state()));
    }
    widget
        .children()
        .into_iter()
        .find_map(focused_widget_snapshot)
}

fn restore_widget_focus(widget: &mut dyn Widget, id: &str, state: Option<&str>) -> bool {
    if widget.test_id() == Some(id) {
        widget.set_focus(true);
        if let Some(state) = state {
            widget.set_state(state);
        }
        return true;
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| restore_widget_focus(child, id, state))
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
    Spacer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditableProperty {
    NodeId,
    ColumnGap,
    ContainerWidth,
    ContainerHeight,
    TextDocumentPath,
    TextSize,
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
    CheckValuePath,
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
    ButtonStyle,
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
    hierarchy = hierarchy.push(Box::new(Text::new("Hierarchy").with_size(20.0)));
    for (id, kind, depth) in flattened {
        let button_id = format!("builder-node-{id}");
        let label = format!("{}{}  {id}", "  ".repeat(depth), kind);
        let selected_row = id == selected;
        let state = Rc::clone(shared);
        let graph = handle.clone();
        let id_for_click = id.clone();
        hierarchy = hierarchy.push(Box::new(
            Button::new(&button_id, &label, None)
                .fit_content(34.0)
                .with_style(if selected_row {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Ghost
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
                    state.chrome_revision = state.chrome_revision.wrapping_add(1);
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
        .push(Box::new(Text::new("Inspector").with_size(20.0)))
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
                                editor.chrome_revision = editor.chrome_revision.wrapping_add(1);
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
    let preview = handle.widget(theme)?;
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
        UiNode::Text { text, size, .. } => {
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
            properties.push((
                EditableProperty::TextSize,
                optional_number(*size),
                "Size (optional px)",
                "builder-text-size",
            ));
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
                    "Click document path (optional)",
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
            accessible_name,
            value_path,
            ..
        }
        | UiNode::Toggle {
            accessible_name,
            value_path,
            ..
        } => properties.extend([
            (
                EditableProperty::CheckAccessibleName,
                accessible_name.clone(),
                "Accessible name",
                "builder-check-accessible-name",
            ),
            (
                EditableProperty::CheckValuePath,
                value_path.clone(),
                "Document value path",
                "builder-check-value-path",
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
}

fn parse_editable_property(
    property: EditableProperty,
    raw: &str,
) -> Result<ParsedProperty, String> {
    if matches!(
        property,
        EditableProperty::ColumnGap
            | EditableProperty::ContainerWidth
            | EditableProperty::ContainerHeight
            | EditableProperty::TextSize
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
            UiNode::Text { size, .. },
            EditableProperty::TextSize,
            ParsedProperty::OptionalNumber(value),
        ) => *size = value,
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
        ) => {
            *accessible_name = value;
            if !accessible_name.trim().is_empty() {
                *label = None;
            }
        }
        (
            UiNode::Checkbox {
                accessible_name, ..
            }
            | UiNode::Toggle {
                accessible_name, ..
            },
            EditableProperty::CheckAccessibleName,
            ParsedProperty::Text(value),
        ) => *accessible_name = value,
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
        UiNode::Text { .. } | UiNode::Spacer { .. } => Vec::new(),
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
        None,
    )
    .fit_content(36.0)
    .on_click(move || {
        let result = graph.update_node(&selected, |node| set_editable_bool(node, property, !value));
        let status = state.borrow().status.clone();
        match result {
            Ok(()) => {
                let mut editor = state.borrow_mut();
                editor.chrome_revision = editor.chrome_revision.wrapping_add(1);
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
        UiNode::Button {
            style, navigate, ..
        } => vec![
            (
                EditableChoice::ButtonStyle,
                button_style_name(*style),
                "Style",
                "builder-button-style",
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
    Button::new(id, &format!("{label}: {value}"), None)
        .fit_content(36.0)
        .on_click(move || {
            let result = graph.update_node(&selected, |node| cycle_editable_choice(node, property));
            let status = state.borrow().status.clone();
            match result {
                Ok(()) => {
                    let mut editor = state.borrow_mut();
                    editor.chrome_revision = editor.chrome_revision.wrapping_add(1);
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
        (UiNode::Button { style, .. }, EditableChoice::ButtonStyle) => {
            *style = match style {
                UiButtonStyle::Neutral => UiButtonStyle::Primary,
                UiButtonStyle::Primary => UiButtonStyle::Danger,
                UiButtonStyle::Danger => UiButtonStyle::Ghost,
                UiButtonStyle::Ghost => UiButtonStyle::Neutral,
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
        UiTextRole::Caption => "Caption",
        UiTextRole::Label => "Label",
        UiTextRole::Body => "Body",
        UiTextRole::Reading => "Reading",
        UiTextRole::Title => "Title",
        UiTextRole::Heading => "Heading",
        UiTextRole::Display => "Display",
        UiTextRole::Code => "Code",
    }
}

const fn next_text_role(role: UiTextRole) -> UiTextRole {
    match role {
        UiTextRole::Caption => UiTextRole::Label,
        UiTextRole::Label => UiTextRole::Body,
        UiTextRole::Body => UiTextRole::Reading,
        UiTextRole::Reading => UiTextRole::Title,
        UiTextRole::Title => UiTextRole::Heading,
        UiTextRole::Heading => UiTextRole::Display,
        UiTextRole::Display => UiTextRole::Code,
        UiTextRole::Code => UiTextRole::Caption,
    }
}

const fn button_style_name(style: UiButtonStyle) -> &'static str {
    match style {
        UiButtonStyle::Neutral => "Neutral",
        UiButtonStyle::Primary => "Primary",
        UiButtonStyle::Danger => "Danger",
        UiButtonStyle::Ghost => "Ghost",
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
    Button::new("builder-save", "Save", None)
        .primary()
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
    Button::new("builder-undo", "Undo", None)
        .fit_content(36.0)
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
    Button::new("builder-redo", "Redo", None)
        .fit_content(36.0)
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
    Button::new(id, label, None)
        .fit_content(36.0)
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
                    editor.chrome_revision = editor.chrome_revision.wrapping_add(1);
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
            size: None,
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
            style: UiButtonStyle::Neutral,
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
                size: None,
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
            accessible_name: "Checkbox".to_owned(),
            enabled: true,
        },
        InsertKind::Toggle => UiNode::Toggle {
            value_path: format!("{id}-enabled"),
            id,
            accessible_name: "Toggle".to_owned(),
            enabled: true,
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
    Button::new(id, label, None)
        .fit_content(36.0)
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
                    editor.chrome_revision = editor.chrome_revision.wrapping_add(1);
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
    Button::new("builder-delete", "Delete", None)
        .danger()
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
                    editor.chrome_revision = editor.chrome_revision.wrapping_add(1);
                    status.set("Node deleted".to_owned());
                }
                Err(error) => status.set(format!("Delete rejected: {error}")),
            }
        })
}

impl Widget for UiGraphEditor {
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
        let node = crate::widget::TaffyTreeExt::parent_node(
            taffy,
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
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
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
        "UiGraphEditor"
    }

    fn test_id(&self) -> Option<&str> {
        Some("ui-graph-editor")
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

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty) | self.child.take_structure_dirty()
    }

    fn needs_idle_sync(&self) -> bool {
        let state = self.shared.borrow();
        state.chrome_revision != self.seen_chrome_revision
            || self.handle.revision() != Some(self.seen_graph_revision)
            || self.child.needs_idle_sync()
    }

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

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
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
    use crate::a11y::{Action, Role};
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ROOT};

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
                    size: None,
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
                    style: UiButtonStyle::Primary,
                },
            ],
            gap: Some(8.0),
            no_padding: false,
            intrinsic_height: false,
            fill_remaining_height: false,
        })
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
        let mut driver = crate::headless::Driver::new(
            crate::State::from_doc(title_doc),
            reparsed.build(&Theme::dark()).unwrap(),
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
        assert!(driver.dump_json().contains("save-requested"));
    }

    #[test]
    fn routed_graph_buttons_drive_history_and_back_without_document_mutations() {
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
                        size: None,
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
                style: UiButtonStyle::Primary,
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
                style: UiButtonStyle::Neutral,
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
            crate::headless::Driver::new(crate::State::new(), Box::new(router), (640, 360));
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
                style: UiButtonStyle::Neutral,
            });
        }
        let route = crate::widget::RouteHandle::new("home", ["home"]).unwrap();
        let root = document
            .build_with(
                &Theme::dark(),
                &UiBuildContext::default().with_router(route.clone()),
            )
            .unwrap();
        let mut driver = crate::headless::Driver::new(crate::State::new(), root, (640, 360));
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
            crate::headless::Driver::new(crate::State::new(), Box::new(live), (640, 360));
        assert!(driver.inspection_json().unwrap().contains("Save"));

        handle
            .update_node("save", |node| {
                if let UiNode::Button { label, style, .. } = node {
                    *label = "Publish".into();
                    *style = UiButtonStyle::Danger;
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
                    size: None,
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
                size: None,
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
                    style: UiButtonStyle::Primary,
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
        let live = handle.widget(&Theme::dark()).unwrap();
        let mut driver =
            crate::headless::Driver::new(crate::State::new(), Box::new(live), (640, 360));
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
                    style: UiButtonStyle::Primary,
                },
            )
            .unwrap();
        driver.relayout();
        driver
            .perform_named("new-project", &Action::Activate)
            .unwrap();
        assert!(driver.dump_json().contains("new-project"));

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

    fn edit_builder_button_properties(driver: &mut crate::headless::Driver) {
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
            .perform_named("builder-button-style", &Action::Activate)
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
            crate::headless::Driver::new(crate::State::new(), Box::new(editor), (1_400, 800));

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
                style: UiButtonStyle::Primary,
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
                style: UiButtonStyle::Primary,
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
            crate::headless::Driver::new(crate::State::new(), Box::new(editor), (1_400, 800));

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
            crate::headless::Driver::new(crate::State::new(), Box::new(editor), (1_400, 800));

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
            crate::headless::Driver::new(crate::State::new(), Box::new(editor), (1_400, 800));

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

//! `ListRow` — one selectable row of a list whose content is other widgets:
//! a thumbnail, a name, a badge, a toggle or two.
//!
//! A [`DataTable`](super::DataTable) or [`VirtualList`](super::VirtualList)
//! paints its rows itself; this is the row for a short list whose items
//! carry real controls (a layers panel, a list of versions). The row paints
//! the shared selection lift behind its content, a press on the row
//! outside its controls chooses it, and assistive technology meets it as a
//! selectable list item.
//!
//! The choice lives in a [`LocalState`](crate::LocalState) the row compares
//! its own value with ([`ListRow::choosing`]), so every row of the list
//! follows one selection without being rebuilt.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::chrome::{SelectionState, selection_tier, selection_tier_on};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{AppModel, EventCtx, ModelPath, Row, Widget};

/// Whether a row is the chosen one, and how to choose it.
struct Choice {
    is_chosen: Box<dyn Fn() -> bool>,
    choose: Box<dyn FnMut()>,
}

/// A selectable row over a [`Row`] of content.
pub struct ListRow {
    content: Row,
    id: Option<String>,
    name: Option<String>,
    choice: Option<Choice>,
    selected: bool,
    hovered: bool,
    cursor: Point,
    /// The lift changed since the last frame.
    dirty: bool,
    /// The row sits on a raised panel rather than the window's ground.
    raised: bool,
}

impl ListRow {
    pub fn new(content: Row) -> Self {
        Self {
            content,
            id: None,
            name: None,
            choice: None,
            selected: false,
            hovered: false,
            cursor: Point::ZERO,
            dirty: true,
            raised: false,
        }
    }

    /// The row stands for `value`: it is selected while `state` holds it,
    /// and choosing the row stores it.
    #[must_use]
    pub fn choosing<T: Clone + PartialEq + Send + Sync + 'static>(
        mut self,
        state: &crate::LocalState<T>,
        value: T,
    ) -> Self {
        let read = state.clone();
        let write = state.clone();
        let mine = value.clone();
        self.choice = Some(Choice {
            is_chosen: Box::new(move || read.with(|current| *current == mine)),
            choose: Box::new(move || {
                if write.with(|current| *current != value) {
                    write.set(value.clone());
                }
            }),
        });
        self.selected = self
            .choice
            .as_ref()
            .is_some_and(|choice| (choice.is_chosen)());
        self
    }

    /// The row sits on a raised panel (a sidebar, a card): its lift is
    /// taken from that ground, where the window's would not show.
    #[must_use]
    pub fn on_raised(mut self) -> Self {
        self.raised = true;
        self
    }

    /// A stable name to find the row by in tests and tools.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// What a screen reader announces for the row.
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn is_selected(&self) -> bool {
        self.selected
    }

    fn choose(&mut self) -> bool {
        let Some(choice) = self.choice.as_mut() else {
            return false;
        };
        (choice.choose)();
        self.selected = true;
        self.dirty = true;
        true
    }

    fn paint_lift(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let state =
            SelectionState::new(self.selected, self.hovered && self.choice.is_some(), false);
        let tier = if self.raised {
            selection_tier_on(theme, state, theme.tokens.colors.surfaces.raised)
        } else {
            selection_tier(theme, state)
        };
        if tier.fill.components[3] <= 0.0 {
            return;
        }
        let rect: Rect = Widget::get_rect(&self.content);
        let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.md));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tier.fill,
            None,
            &shape,
        );
    }
}

impl crate::widget::Decorator for ListRow {
    type Inner = Row;

    fn inner(&self) -> &Row {
        &self.content
    }

    fn inner_mut(&mut self) -> &mut Row {
        &mut self.content
    }

    fn concrete_type_name(&self) -> &'static str {
        "ListRow"
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        if let Some(choice) = &self.choice {
            let selected = (choice.is_chosen)();
            self.dirty |= selected != self.selected;
            self.selected = selected;
        }
        self.content.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.content.build_layout(taffy, theme, node_map);
        // Accessibility actions on the row reach it through the node map.
        crate::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.paint_lift(scene, transform, theme);
        self.content
            .draw(scene, transform, text_renderer, theme, presence);
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
        if presence.should_cull(Widget::get_rect(&self.content)) {
            return;
        }
        self.paint_lift(scene, transform, theme);
        self.content.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            let hovered = Widget::get_rect(&self.content).contains(self.cursor);
            self.dirty |= hovered != self.hovered;
            self.hovered = hovered;
        }
        if let WindowEvent::CursorLeft { .. } = event {
            self.dirty |= self.hovered;
            self.hovered = false;
        }
        self.content.handle_event(event, ctx);
        // A press the row's own controls did not take chooses the row.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && !*ctx.handled
            && Widget::get_rect(&self.content).contains(self.cursor)
            && self.choose()
        {
            *ctx.handled = true;
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.content.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.content.mark_drawn();
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // A row of a list is addressed by its position: the item it shows
        // is addressed by the resource's own actions.
        Some(
            AccessNode::new(Role::ListItem)
                .with_optional_name(self.name.clone())
                .with_state(AccessState {
                    selected: self.selected,
                    ..Default::default()
                }),
        )
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        match action {
            crate::a11y::Action::Activate | crate::a11y::Action::SetSelection { .. } => {
                self.choose()
            }
            _ => self.content.perform_action(action, state, path),
        }
    }
}

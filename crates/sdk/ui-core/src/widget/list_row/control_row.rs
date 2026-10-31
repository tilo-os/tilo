//! [`ControlRow`]: a selectable row of real controls standing in the list
//! row's selection lift.

use super::{RowMetrics, row_fill};
use crate::a11y::{AccessNode, AccessState, Role};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{AppModel, ModelPath, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::WindowEvent;

/// Whether a [`ControlRow`] is the chosen one, and how to choose it.
struct RowChoice {
    is_chosen: Box<dyn Fn() -> bool>,
    choose: Box<dyn FnMut()>,
}

/// A selectable row of a short list whose items carry real controls (a
/// layers panel with each layer's thumbnail, name and switches; a list of
/// versions with a restore button): a [`crate::widget::Row`] of widgets
/// standing in the list row's selection lift.
///
/// It is selected while the [`crate::LocalState`] it was bound to with
/// [`ControlRow::choosing`] holds its value, so every row of the list
/// follows one selection without being rebuilt. A press on the row that
/// none of its controls took chooses it; assistive technology meets it as a
/// selectable list item. Its look is a [`ListRowFace`]'s: the lift of the
/// ground it stands on, hovered or chosen, at the row radius.
pub struct ControlRow {
    content: crate::widget::Row,
    id: Option<String>,
    name: Option<String>,
    choice: Option<RowChoice>,
    selected: bool,
    hovered: bool,
    cursor: Point,
    /// The lift changed since the last frame.
    dirty: bool,
}

impl ControlRow {
    pub fn new(content: crate::widget::Row) -> Self {
        Self {
            content,
            id: None,
            name: None,
            choice: None,
            selected: false,
            hovered: false,
            cursor: Point::ZERO,
            dirty: true,
        }
    }

    /// The row stands for `value`: it is selected while `state` holds it,
    /// and choosing the row stores it.
    #[must_use]
    pub fn choosing<T: Clone + PartialEq + Send + Sync + 'static>(
        self,
        state: &crate::LocalState<T>,
        value: T,
    ) -> Self {
        self.choosing_any(state, vec![value])
    }

    /// The row stands for several values, the first its own and the rest
    /// parts of it (a layer and its mask): it is selected while `state`
    /// holds any of them, and choosing the row stores the first unless a
    /// part of it is already chosen.
    #[must_use]
    pub fn choosing_any<T: Clone + PartialEq + Send + Sync + 'static>(
        mut self,
        state: &crate::LocalState<T>,
        values: Vec<T>,
    ) -> Self {
        let Some(first) = values.first().cloned() else {
            return self;
        };
        let read = state.clone();
        let write = state.clone();
        let mine = values.clone();
        self.choice = Some(RowChoice {
            is_chosen: Box::new(move || read.with(|current| mine.contains(current))),
            choose: Box::new(move || {
                if write.with(|current| !values.contains(current)) {
                    write.set(first.clone());
                }
            }),
        });
        self.selected = self
            .choice
            .as_ref()
            .is_some_and(|choice| (choice.is_chosen)());
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
        let Some(fill) = row_fill(theme, self.selected, self.hovered && self.choice.is_some())
        else {
            return;
        };
        let rect: Rect = Widget::get_rect(&self.content);
        let radius = RowMetrics::new(theme).radius;
        scene.fill(
            Fill::NonZero,
            transform,
            fill,
            None,
            &RoundedRect::from_rect(rect, radius),
        );
    }
}

impl crate::widget::Decorator for ControlRow {
    type Inner = crate::widget::Row;

    fn inner(&self) -> &crate::widget::Row {
        &self.content
    }

    fn inner_mut(&mut self) -> &mut crate::widget::Row {
        &mut self.content
    }

    fn concrete_type_name(&self) -> &'static str {
        "ControlRow"
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if let Some(choice) = &self.choice {
            let selected = (choice.is_chosen)();
            self.dirty |= selected != self.selected;
            self.selected = selected;
        }
        self.content.sync(state, path);
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = ctx.logical(position);
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
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
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

    fn accessibility(&self) -> Option<AccessNode> {
        // A row of a list is addressed by its position: the item it shows
        // is addressed by its own controls' actions.
        Some(
            AccessNode::new(Role::ListItem)
                .with_optional_name(self.name.clone())
                .with_state(AccessState::new().selected(self.selected)),
        )
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        match action {
            crate::a11y::Action::Activate | crate::a11y::Action::SetSelection { .. } => {
                self.choose()
            }
            _ => self.content.perform_action(action, &[], state, path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_rows_follow_one_shared_choice() {
        let chosen = crate::LocalState::new("ink".to_owned());
        let mut ink = ControlRow::new(crate::widget::Row::new())
            .choosing(&chosen, "ink".to_owned())
            .with_accessible_name("Ink");
        let mut oil = ControlRow::new(crate::widget::Row::new())
            .choosing_any(&chosen, vec!["oil".to_owned(), "oil-mask".to_owned()]);
        assert!(ink.is_selected() && !oil.is_selected());

        // Activating a row chooses it; the other follows on sync.
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        assert!(Widget::perform_action(
            &mut oil,
            &crate::a11y::Action::Activate,
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &root
        ));
        assert_eq!(chosen.with(Clone::clone), "oil");
        Widget::sync(&mut ink, &doc, &root);
        assert!(!ink.is_selected() && oil.is_selected());

        // A part of the row (its mask) keeps the row chosen, and choosing
        // the row again does not take the part's place.
        chosen.set("oil-mask".to_owned());
        Widget::sync(&mut oil, &doc, &root);
        assert!(oil.is_selected());
        Widget::perform_action(
            &mut oil,
            &crate::a11y::Action::Activate,
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &root,
        );
        assert_eq!(chosen.with(Clone::clone), "oil-mask");

        let node = Widget::accessibility(&ink).expect("a list item");
        assert_eq!(node.role, Role::ListItem);
        assert_eq!(node.name.as_deref(), Some("Ink"));
    }
}

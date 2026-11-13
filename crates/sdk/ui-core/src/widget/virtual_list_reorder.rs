//! Reordering a list's rows: the drop a pointer offers, the moves the keyboard asks for, and
//! the one commit both go through.

use vello::kurbo::Point;
use winit::keyboard::{Key, NamedKey};

use super::{ListModel, VirtualList};
use crate::reorder::{DropPlacement, DropTarget, ReorderDrop};
use crate::time::Instant;

impl<M: ListModel> VirtualList<M> {
    pub(super) fn takes_row_press(&self, event: &winit::event::WindowEvent) -> bool {
        if self.reorder_validator.is_none() || self.reorder_commit.is_none() {
            return false;
        }
        if !matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) {
            return false;
        }
        let Some(cursor) = self.last_cursor.filter(|point| self.rect.contains(*point)) else {
            return false;
        };
        if self.pill_rect().is_some_and(|rect| rect.contains(cursor)) {
            return false;
        }
        let Some((index, _)) = self.row_at_cursor() else {
            return false;
        };
        let Some(row) = self
            .mounted
            .iter()
            .find(|row| row.index == index && row.state == super::MountedRowState::Ready)
        else {
            return false;
        };
        row.widget
            .as_any()
            .and_then(|widget| widget.downcast_ref::<crate::widget::ListRow>())
            .is_some_and(|row| !row.claims_pointer(cursor))
    }

    /// Show `source` being dragged to just before `before`, for a capture: the row lifts under
    /// a pointer resting on the seam above `before` and the drop line shows. Nothing moves
    /// unless a real release follows. Whether both rows stand placed, so it showed.
    pub fn stage_reorder(&mut self, source: &M::Key, before: &M::Key) -> bool {
        if self.rect.is_zero_area() {
            return false;
        }
        let (Some(from), Some(to)) = (self.row_rect(source), self.row_rect(before)) else {
            return false;
        };
        let now = crate::time::now();
        self.reorder.begin(source.clone(), from.center());
        let at = Point::new(to.center().x, to.y0 + to.height() * 0.1);
        self.last_cursor = Some(at);
        self.last_tick = Some(now);
        self.update_reorder_pointer(at, now);
        true
    }

    pub(super) fn update_reorder_pointer(&mut self, cursor: Point, now: Instant) {
        let candidate = self.reorder_candidate(cursor);
        let model = &self.model;
        let validator = &mut self.reorder_validator;
        self.reorder
            .update(cursor, candidate, now, |source, target| {
                validator
                    .as_mut()
                    .is_some_and(|validate| validate(model, source, target))
            });
    }

    pub(super) fn reorder_chord(
        &self,
        key: &Key,
        modifiers: winit::keyboard::ModifiersState,
    ) -> Option<bool> {
        if self.reorder_validator.is_none() || self.reorder_commit.is_none() {
            return None;
        }
        let chord = modifiers.control_key() != modifiers.alt_key()
            && !modifiers.shift_key()
            && !modifiers.super_key();
        match key {
            Key::Named(NamedKey::ArrowUp) if chord => Some(true),
            Key::Named(NamedKey::ArrowDown) if chord => Some(false),
            _ => None,
        }
    }

    pub(super) fn move_focused(&mut self, up: bool) -> bool {
        let Some(source) = self.focused_row_key() else {
            return false;
        };
        let Some(index) = self.model.index_of(&source) else {
            return false;
        };
        let neighbour = if up {
            index.checked_sub(1)
        } else {
            index.checked_add(1)
        };
        let Some(neighbour) = neighbour.and_then(|at| self.model.key_at(at)) else {
            return false;
        };
        let target = DropTarget::new(
            neighbour,
            if up {
                DropPlacement::Before
            } else {
                DropPlacement::After
            },
        );
        let model = &self.model;
        let accepted = self
            .reorder_validator
            .as_mut()
            .is_some_and(|validate| validate(model, &source, &target));
        if !accepted
            || !self.commit_reorder(ReorderDrop {
                source: source.clone(),
                target,
            })
        {
            return false;
        }
        if let Some(at) = self.model.index_of(&source) {
            self.scroll_to_index(at);
        }
        self.focus_row(source);
        true
    }

    pub(super) fn commit_reorder(&mut self, dropped: ReorderDrop<M::Key>) -> bool {
        let anchor = self.capture_anchor();
        let changed = self
            .reorder_commit
            .as_mut()
            .is_some_and(|commit| commit(&mut self.model, dropped));
        if changed {
            self.forget_tree();
            self.reconcile_model(anchor.as_ref());
        }
        changed
    }
}

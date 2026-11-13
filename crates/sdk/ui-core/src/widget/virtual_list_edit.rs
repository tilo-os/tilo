//! Editing a list's rows in place: the title a person types over a row, and the rows they
//! ask to delete.

use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::{ListModel, VirtualList};
use crate::widget::Widget;

pub(super) type RenameCallback<K> = Box<dyn FnMut(K, String)>;

pub(super) struct RenameField<K> {
    pub(super) key: K,
    pub(super) input: crate::widget::TextInput,
}

#[derive(PartialEq)]
pub(super) struct RenameStamp {
    text: String,
    bounds: Rect,
    state: crate::widget::text_input::TextAccessibilityState,
}

impl<K> RenameField<K> {
    pub(super) fn stamp(&self) -> RenameStamp {
        RenameStamp {
            text: self.input.value().to_owned(),
            bounds: self.input.get_rect(),
            state: self.input.accessibility_state(),
        }
    }
}

pub trait RowRenameField {
    fn rename_field(&self) -> Option<(Rect, crate::tokens::TextRole)>;
}

impl<M: ListModel> VirtualList<M> {
    #[must_use]
    pub fn on_rename(mut self, f: impl FnMut(M::Key, String) + 'static) -> Self {
        self.on_rename = Some(Box::new(f));
        self
    }

    /// Make a double click on a row rename it instead of opening it, as an outliner does.
    /// Requires [`Self::on_rename`].
    #[must_use]
    pub fn rename_on_double_click(mut self) -> Self {
        self.rename_on_double_click = true;
        self
    }

    /// Receive `Delete` on the focused row. The list never removes a row itself; the host
    /// decides.
    #[must_use]
    pub fn on_delete(mut self, f: impl FnMut(M::Key) + 'static) -> Self {
        self.on_delete = Some(Box::new(f));
        self
    }

    pub fn begin_rename(&mut self, key: &M::Key) -> bool {
        if self.on_rename.is_none() {
            return false;
        }
        let Some(index) = self
            .model
            .index_of(key)
            .filter(|index| self.model.renames(*index))
        else {
            return false;
        };
        let Some(label) = self.model.label(index) else {
            return false;
        };
        let placed = self.rename_rect(index, key);
        let role = placed.map_or(crate::tokens::TextRole::Body, |(_, role)| role);
        let mut input = crate::widget::TextInput::local()
            .with_plain_chrome()
            .with_text_role(role)
            .with_initial_value(&label);
        input.set_focus(true);
        self.focus_row(key.clone());
        if let Some((rect, _)) = placed {
            input.apply_layout_external(rect);
        }
        self.rename = Some(Box::new(RenameField {
            key: key.clone(),
            input,
        }));
        true
    }

    #[must_use]
    pub fn renaming(&self) -> Option<&M::Key> {
        self.rename.as_ref().map(|field| &field.key)
    }

    #[must_use]
    pub fn rename_field(&self) -> Option<&crate::widget::TextInput> {
        self.rename.as_ref().map(|field| &field.input)
    }

    pub fn commit_rename(&mut self) {
        let Some(field) = self.rename.take() else {
            return;
        };
        let title = field.input.value().trim().to_owned();
        if title.is_empty() {
            return;
        }
        if let Some(callback) = &mut self.on_rename {
            callback(field.key, title);
        }
    }

    pub fn cancel_rename(&mut self) {
        self.rename = None;
    }

    fn rename_rect(&self, index: usize, key: &M::Key) -> Option<(Rect, crate::tokens::TextRole)> {
        if let Some(placed) = self
            .mounted
            .iter()
            .find(|row| row.key == *key)
            .and_then(|row| {
                crate::widget::capability::capability::<dyn RowRenameField>(&*row.widget)
            })
            .and_then(RowRenameField::rename_field)
        {
            return Some(placed);
        }
        let row = self.row_box(index)?;
        let depth = f64::from(self.tree.as_ref().map_or(0, |marks| marks.depth(key))) + 1.0;
        let x0 = row.x0 + self.lead + self.chevron_column + depth * self.disclosure;
        Some((
            Rect::new(
                x0,
                row.y0,
                (self.lift_rect(0.0, 0.0).x1 - self.bleed).max(x0 + 1.0),
                row.y1,
            ),
            crate::tokens::TextRole::Body,
        ))
    }

    pub(super) fn place_rename(&mut self) {
        let Some(field) = self.rename.as_ref() else {
            return;
        };
        let Some(index) = self.model.index_of(&field.key) else {
            self.rename = None;
            return;
        };
        let Some((rect, _)) = self.rename_rect(index, &field.key) else {
            return;
        };
        if let Some(field) = self.rename.as_mut()
            && field.input.get_rect() != rect
        {
            field.input.apply_layout_external(rect);
        }
    }

    pub(super) fn route_rename(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        if self.rename.is_none() {
            return false;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left | MouseButton::Right,
            ..
        } = event
        {
            let inside = self.rename.as_ref().is_some_and(|field| {
                field
                    .input
                    .get_rect()
                    .contains(self.last_cursor.unwrap_or(Point::new(f64::NAN, f64::NAN)))
            });
            if !inside {
                self.commit_rename();
                return false;
            }
        }
        let keyboard = matches!(event, WindowEvent::KeyboardInput { .. });
        if let WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            match logical_key {
                Key::Named(NamedKey::Enter) => {
                    self.commit_rename();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.cancel_rename();
                    *ctx.handled = true;
                    return true;
                }
                _ => {}
            }
        }
        let Some(field) = self.rename.as_mut() else {
            return false;
        };
        field.input.handle_event(event, ctx);
        if keyboard {
            // Keys typed into the field never reach the list's navigation.
            *ctx.handled = true;
        }
        *ctx.handled
    }

    pub(super) fn rename_animating(&self) -> bool {
        self.rename
            .as_ref()
            .is_some_and(|field| field.input.needs_animation_frame())
    }

    pub(super) fn mark_rename_drawn(&mut self) {
        if let Some(field) = self.rename.as_mut() {
            field.input.mark_drawn();
        }
    }

    pub(super) fn rename_needs_redraw(&self) -> bool {
        self.rename
            .as_ref()
            .is_some_and(|field| field.input.needs_redraw())
    }
}

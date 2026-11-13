//! Editing a list's rows in place: the title a person types over a row, and the rows they
//! ask to delete.

use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::{ListModel, VirtualList};
use crate::widget::Widget;

pub(super) type RenameCallback<K> = Box<dyn FnMut(K, String)>;

/// The inline editor over a row being renamed ([`VirtualList::on_rename`]).
pub(super) struct RenameField<K> {
    pub(super) key: K,
    pub(super) input: crate::widget::TextInput,
}

/// What a row's inline rename field says in the accessibility tree, short of
/// building its node: its text, where it stands, and its own state.
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

/// A row that says where its title stands, for the field renaming it in place to stand over
/// it, in the role the title reads in: asked for through
/// [`capability`](crate::widget::capability::capability).
pub trait RowRenameField {
    /// Where the rename field stands over the row's title, and the role it reads in. `None`
    /// leaves it where the list puts it.
    fn rename_field(&self) -> Option<(Rect, crate::tokens::TextRole)>;
}

impl<M: ListModel> VirtualList<M> {
    /// Rename a row in place. `F2`, [`Self::begin_rename`] and (with
    /// [`Self::rename_on_double_click`]) a double click open a field over the row's label;
    /// `Enter`, a click elsewhere or the list losing focus commit it, `Escape` cancels.
    /// The callback receives the trimmed title; an empty title is dropped. The host applies the
    /// rename to its model.
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

    /// Open the inline rename field on the row of `key`. Whether it opened: the list renames
    /// ([`Self::on_rename`]), the row stands in it, and the model names it.
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
        // A field draws over the label the row's own widget paints, so its chrome takes the
        // title's place rather than doubly printing it.
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

    /// The row being renamed, if any.
    #[must_use]
    pub fn renaming(&self) -> Option<&M::Key> {
        self.rename.as_ref().map(|field| &field.key)
    }

    /// The open rename field, for hosts and tests inspecting its state.
    #[must_use]
    pub fn rename_field(&self) -> Option<&crate::widget::TextInput> {
        self.rename.as_ref().map(|field| &field.input)
    }

    /// Close the rename field, reporting a non-empty trimmed title.
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

    /// Close the rename field without reporting.
    pub fn cancel_rename(&mut self) {
        self.rename = None;
    }

    /// Where the rename field sits over the row of `key`, in the role its title reads in: where
    /// the mounted row says its label stands ([`RowRenameField`]), else over its label from the
    /// row's content column at its depth to its trailing edge.
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
        // The field stands where the row's content does: past the step its chevron takes.
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

    /// Put the open rename field back over its row where the rows moved since it opened.
    pub(super) fn place_rename(&mut self) {
        let Some(field) = self.rename.as_ref() else {
            return;
        };
        let Some(index) = self.model.index_of(&field.key) else {
            // The row it renamed left the list: the field closes with it.
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

    /// Route input to an open rename field. A press outside it commits and falls through to the
    /// list; `Enter` commits and `Escape` cancels; every other key belongs to the field.
    /// Returns whether the event was consumed.
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

    /// Whether the open rename field asks for a frame of its own: its caret blinking, or a
    /// correction floating past it.
    pub(super) fn rename_animating(&self) -> bool {
        self.rename
            .as_ref()
            .is_some_and(|field| field.input.needs_animation_frame())
    }

    /// Mark the open rename field drawn, so it asks for no frame until it changes again.
    pub(super) fn mark_rename_drawn(&mut self) {
        if let Some(field) = self.rename.as_mut() {
            field.input.mark_drawn();
        }
    }

    /// Whether the open rename field has something new to paint.
    pub(super) fn rename_needs_redraw(&self) -> bool {
        self.rename
            .as_ref()
            .is_some_and(|field| field.input.needs_redraw())
    }
}

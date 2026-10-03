//! Editing a tree in place: inline rename, and the keyboard and
//! assistive-technology counterparts of dragging a row (move up / down,
//! indent, outdent). Every structural edit goes through the same validated
//! [`TreeView::with_reorder`] commit a pointer drop uses, so a host writes its
//! move logic once.

use super::{
    DropPlacement, DropTarget, ElementState, Key, KeyEvent, LABEL_ROLE, MouseButton, NamedKey,
    NodeId, Point, Rect, ReorderDrop, TreeModel, TreeRowKey, TreeView, WindowEvent, idx_to_f64,
};
use crate::widget::{TextInput, Widget};

/// The inline editor over a row being renamed.
pub(super) struct RenameField {
    pub(super) id: NodeId,
    pub(super) input: TextInput,
}

/// A structural move of one row — what a drag does, as a single step for the
/// keyboard (`Alt` + arrows, or `Tab` / `Shift+Tab` with
/// [`TreeView::tab_indents`]) and for assistive technology, which sees one
/// button per move on every row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeMove {
    /// Before the previous sibling.
    Up,
    /// After the next sibling.
    Down,
    /// Into the previous sibling, as its last child.
    Indent,
    /// Out of the parent, directly after it.
    Outdent,
}

impl TreeMove {
    pub(super) const ALL: [Self; 4] = [Self::Up, Self::Down, Self::Indent, Self::Outdent];

    pub(super) fn token(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Indent => "indent",
            Self::Outdent => "outdent",
        }
    }

    pub(super) fn from_token(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|movement| movement.token() == token)
    }

    /// The FTL key naming this move for assistive technology.
    pub(super) fn label_key(self) -> &'static str {
        match self {
            Self::Up => "tree-move-up",
            Self::Down => "tree-move-down",
            Self::Indent => "tree-move-indent",
            Self::Outdent => "tree-move-outdent",
        }
    }
}

impl<T, M: TreeModel> TreeView<T, M> {
    /// Enable inline rename. `F2`, [`Self::begin_rename`] and (with
    /// [`Self::rename_on_double_click`]) a double click open a plain text
    /// field over the row's label; `Enter`, a click elsewhere or the tree
    /// losing focus commit it, `Escape` cancels. The callback receives the
    /// trimmed title; an empty title is dropped. The host applies the rename
    /// to its model.
    #[must_use]
    pub fn on_rename<F: FnMut(NodeId, String) + 'static>(mut self, f: F) -> Self {
        self.on_rename = Some(Box::new(f));
        self
    }

    /// Make a double click on a row rename it instead of activating it, as
    /// an outliner does. Requires [`Self::on_rename`].
    #[must_use]
    pub fn rename_on_double_click(mut self) -> Self {
        self.rename_on_double_click = true;
        self
    }

    /// Bind `Tab` / `Shift+Tab` to indent / outdent the focused row, as an
    /// outliner does. `Alt+→` / `Alt+←` indent and outdent regardless. Needs
    /// [`Self::with_reorder`]; a `Tab` that cannot move the row is left to
    /// focus traversal.
    #[must_use]
    pub fn tab_indents(mut self, enabled: bool) -> Self {
        self.tab_indents = enabled;
        self
    }

    /// Receive `Delete` on the focused row. The tree never removes a row
    /// itself; the host decides.
    #[must_use]
    pub fn on_delete<F: FnMut(NodeId) + 'static>(mut self, f: F) -> Self {
        self.on_delete = Some(Box::new(f));
        self
    }

    /// Open the inline rename field on `id`. Returns `false` when rename is
    /// not enabled or the node is unknown.
    pub fn begin_rename(&mut self, id: NodeId) -> bool {
        if self.on_rename.is_none() || !self.model.is_renamable(id) {
            return false;
        }
        let Some(label) = self.model.label(id) else {
            return false;
        };
        let mut input = TextInput::local()
            .with_plain_chrome()
            .with_text_role(LABEL_ROLE)
            .with_initial_value(label);
        input.set_focus(true);
        if let Some(rect) = self.rename_field_rect(id, 0.0) {
            input.apply_layout_external(rect);
        }
        self.focused = Some(TreeRowKey::Node(id));
        self.rename = Some(RenameField { id, input });
        true
    }

    /// The row being renamed, if any.
    #[must_use]
    pub fn renaming(&self) -> Option<NodeId> {
        self.rename.as_ref().map(|field| field.id)
    }

    /// The open rename field, for hosts and tests inspecting its state.
    #[must_use]
    pub fn rename_field(&self) -> Option<&TextInput> {
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
            callback(field.id, title);
        }
    }

    /// Close the rename field without reporting.
    pub fn cancel_rename(&mut self) {
        self.rename = None;
    }

    /// Where the rename field sits on `id`'s row: over the label, from its
    /// leading edge (past the disclosure column and `lead`, a decoration
    /// icon's width) to the row's trailing padding.
    pub(super) fn rename_field_rect(&self, id: NodeId, lead: f64) -> Option<Rect> {
        let index = self.visible_index_of(id)?;
        let row = self.visible_row_rect(index)?;
        let (_, depth) = self.index.row_at(&self.model, index)?;
        let x0 = row.x0 + self.row_inset + idx_to_f64(depth) * self.indent_px + self.disclosure_w;
        Some(Rect::new(
            x0 + lead,
            row.y0,
            (row.x1 - self.row_gap).max(x0 + lead),
            row.y1,
        ))
    }

    /// Route input to an open rename field. A press outside it commits and
    /// falls through to the tree; `Enter` commits and `Escape` cancels; every
    /// other key belongs to the field. Returns whether the event was
    /// consumed.
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
                KeyEvent {
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
        let _ = field.input.take_dirty();
        if keyboard {
            // Keys typed into the field never reach tree navigation.
            *ctx.handled = true;
        }
        *ctx.handled
    }

    /// The drop target that performs `movement` on `id`, if the hierarchy
    /// has room for it. Validation is left to the commit path.
    pub(super) fn move_target(&self, id: NodeId, movement: TreeMove) -> Option<DropTarget<NodeId>> {
        let parent = self.model.parent_id(id)?;
        let index = self.model.child_index(parent, id)?;
        match movement {
            TreeMove::Up => {
                let previous = self.model.child_id(parent, index.checked_sub(1)?)?;
                Some(DropTarget::new(previous, DropPlacement::Before))
            }
            TreeMove::Down => {
                let next = self.model.child_id(parent, index.checked_add(1)?)?;
                Some(DropTarget::new(next, DropPlacement::After))
            }
            TreeMove::Indent => {
                let previous = self.model.child_id(parent, index.checked_sub(1)?)?;
                self.model
                    .accepts_children(previous)
                    .then(|| DropTarget::new(previous, DropPlacement::Inside))
            }
            TreeMove::Outdent => {
                // A top-level row (child of the root) has nowhere to go.
                self.model.parent_id(parent)?;
                Some(DropTarget::new(parent, DropPlacement::After))
            }
        }
    }

    /// Perform `movement` on `id` through the reorder validator and commit.
    /// Returns whether the move was accepted. The row keeps focus and stays
    /// in view.
    pub fn move_node(&mut self, id: NodeId, movement: TreeMove) -> bool {
        if self.reorder_validator.is_none() || self.reorder_commit.is_none() {
            return false;
        }
        let Some(target) = self.move_target(id, movement) else {
            return false;
        };
        if self.reorder_would_cycle(id, &target) {
            return false;
        }
        let model = &self.model;
        let accepted = self
            .reorder_validator
            .as_mut()
            .is_some_and(|validate| validate(model, id, &target));
        if !accepted {
            return false;
        }
        self.focused = Some(TreeRowKey::Node(id));
        self.commit_reorder(ReorderDrop { source: id, target });
        self.reveal_focus();
        true
    }

    /// The move a key chord asks for, when the tree is reorderable.
    pub(super) fn key_move(
        &self,
        logical_key: &Key,
        modifiers: winit::keyboard::ModifiersState,
    ) -> Option<TreeMove> {
        if self.reorder_validator.is_none() || self.reorder_commit.is_none() {
            return None;
        }
        let plain = !modifiers.control_key() && !modifiers.super_key();
        match logical_key {
            Key::Named(NamedKey::ArrowUp) if modifiers.alt_key() && plain => Some(TreeMove::Up),
            Key::Named(NamedKey::ArrowDown) if modifiers.alt_key() && plain => Some(TreeMove::Down),
            Key::Named(NamedKey::ArrowRight) if modifiers.alt_key() && plain => {
                Some(TreeMove::Indent)
            }
            Key::Named(NamedKey::ArrowLeft) if modifiers.alt_key() && plain => {
                Some(TreeMove::Outdent)
            }
            Key::Named(NamedKey::Tab) if self.tab_indents && plain && !modifiers.alt_key() => {
                Some(if modifiers.shift_key() {
                    TreeMove::Outdent
                } else {
                    TreeMove::Indent
                })
            }
            _ => None,
        }
    }

    /// The node keyboard commands act on: the focused row, else the
    /// selection.
    pub(super) fn command_node(&self) -> Option<NodeId> {
        self.focused()
            .or_else(|| self.selected())
            .filter(|id| self.model.contains(*id))
    }
}

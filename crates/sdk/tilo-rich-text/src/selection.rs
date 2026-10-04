//! Whole-block commands applied through the owning canonical source.
use super::{
    AppModel, AutoCommit, EventCtx, ModelPath, RichTextAction, RichTextSource, RichTextSurface,
    WindowEvent,
};

enum SelectionCommand {
    Toggle,
    Navigate { up: bool, extend: bool },
    Edit,
    SelectAll,
    Copy,
    Delete { cut: bool },
}

impl<S: RichTextSource> RichTextSurface<S> {
    fn selection_key(
        &self,
        event: &WindowEvent,
        modifiers: ui::winit::event::Modifiers,
    ) -> Option<SelectionCommand> {
        use ui::winit::{
            event::ElementState,
            keyboard::{Key, NamedKey},
        };
        if !self.editor.is_focused() || self.editor.has_open_popup() || self.is_composing() {
            return None;
        }
        let WindowEvent::KeyboardInput { event, .. } = event else {
            return None;
        };
        if event.state != ElementState::Pressed {
            return None;
        }
        let held = modifiers.state();
        let command = held.control_key() || held.super_key();
        if event.logical_key == Key::Named(NamedKey::Escape) && !command && !held.alt_key() {
            return Some(SelectionCommand::Toggle);
        }
        if !self.editor.is_block_selection_mode() {
            return None;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Backspace | NamedKey::Delete) => {
                Some(SelectionCommand::Delete { cut: false })
            }
            Key::Named(NamedKey::Enter) => Some(SelectionCommand::Edit),
            Key::Named(key @ (NamedKey::ArrowUp | NamedKey::ArrowDown))
                if !command && !held.alt_key() =>
            {
                Some(SelectionCommand::Navigate {
                    up: *key == NamedKey::ArrowUp,
                    extend: held.shift_key(),
                })
            }
            Key::Character(key) if command && !held.alt_key() => {
                match key.to_lowercase().as_str() {
                    "a" => Some(SelectionCommand::SelectAll),
                    "c" => Some(SelectionCommand::Copy),
                    "x" => Some(SelectionCommand::Delete { cut: true }),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub(super) fn handle_selection_key(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
    ) -> bool {
        let Some(command) = self.selection_key(event, *ctx.modifiers) else {
            return false;
        };
        match command {
            SelectionCommand::Toggle => {
                self.editor.toggle_block_selection();
            }
            SelectionCommand::Navigate { up, extend } => {
                self.editor.navigate_block_selection(up, extend);
            }
            SelectionCommand::Edit => {
                self.editor.leave_block_selection();
            }
            SelectionCommand::SelectAll => {
                self.editor.select_all_blocks();
            }
            SelectionCommand::Copy => {
                self.editor.copy_selected_blocks();
            }
            SelectionCommand::Delete { cut } => {
                self.delete_blocks(ctx.state, None, cut);
            }
        }
        *ctx.handled = true;
        true
    }

    pub(super) fn delete_blocks(
        &mut self,
        doc: &mut AutoCommit,
        target: Option<&str>,
        cut: bool,
    ) -> bool {
        if self.source.read_only() || self.is_composing() {
            return false;
        }
        self.commit(doc);
        self.source.sync(doc);
        if self.source.revision() != self.revision {
            self.present_state(None);
        }
        let range = match target {
            Some(id) => self
                .editor
                .blocks()
                .iter()
                .position(|block| block.id == id)
                .and_then(|index| self.editor.block_group_range(index)),
            None => self.editor.selected_block_range(),
        };
        let Some(range) = range else {
            return false;
        };
        let blocks = self.editor.blocks();
        let Some(selected) = blocks.get(range.clone()) else {
            return false;
        };
        let ids: Vec<String> = selected.iter().map(|block| block.id.clone()).collect();
        // A never-written empty editor row is only a visual placeholder.
        let stored: std::collections::HashSet<&str> =
            self.sources.iter().map(|view| view.id.as_str()).collect();
        if ids.is_empty() || ids.iter().any(|id| !stored.contains(id.as_str())) {
            return false;
        }
        let next = blocks
            .get(range.end)
            .or_else(|| {
                range
                    .start
                    .checked_sub(1)
                    .and_then(|index| blocks.get(index))
            })
            .map(|block| block.id.clone());
        let empty = ids.len() == blocks.len();
        if cut && !self.editor.copy_selected_blocks() {
            return false;
        }
        if let Err(error) = self
            .source
            .apply(doc, &RichTextAction::RemoveBlocks { blocks: ids })
        {
            tracing::debug!(%error, "block deletion refused");
            return false;
        }
        if empty
            && let Err(error) = self.source.apply(
                doc,
                &RichTextAction::EnsureRegionBlock {
                    region: tilo_document::action::RegionKind::Body,
                },
            )
        {
            tracing::debug!(%error, "empty paragraph creation refused");
        }
        self.editor.clear_document_selection();
        self.source.sync(doc);
        self.present_state(None);
        let focus = next
            .and_then(|id| self.editor.blocks().iter().position(|block| block.id == id))
            .unwrap_or(0);
        self.editor
            .focus_block(focus, ui::widget::CursorPlacement::Start);
        true
    }

    pub(super) fn selection_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> Option<bool> {
        let target = if self.editor.is_block_selection_mode() {
            match target {
                "notes.editor:copy" => "notes.editor:copy-blocks",
                "notes.editor:cut" => "notes.editor:cut-blocks",
                "notes.editor:select-all" => "notes.editor:select-all-blocks",
                "notes.editor:paste" => {
                    if !matches!(action, ui::a11y::Action::Activate) || self.is_composing() {
                        return Some(false);
                    }
                    self.editor.leave_block_selection();
                    return None;
                }
                _ => target,
            }
        } else {
            target
        };
        let selection = target.starts_with("notes.block-select:")
            || matches!(
                target,
                "notes.editor:select-block"
                    | "notes.editor:select-all-blocks"
                    | "notes.editor:extend-up"
                    | "notes.editor:extend-down"
                    | "notes.editor:edit-block"
                    | "notes.editor:copy-blocks"
            );
        let deletion = target.starts_with("notes.block-delete:")
            || matches!(target, "notes.editor:delete" | "notes.editor:cut-blocks");
        if !selection && !deletion {
            return None;
        }
        if !matches!(action, ui::a11y::Action::Activate) || self.is_composing() {
            return Some(false);
        }
        if deletion {
            let (doc, _) = ui::doc::document(state, path);
            return Some(self.delete_blocks(
                doc,
                target.strip_prefix("notes.block-delete:"),
                target == "notes.editor:cut-blocks",
            ));
        }
        Some(match target {
            "notes.editor:select-all-blocks" => self.editor.select_all_blocks(),
            "notes.editor:extend-up" => self.editor.navigate_block_selection(true, true),
            "notes.editor:extend-down" => self.editor.navigate_block_selection(false, true),
            "notes.editor:edit-block" => self.editor.leave_block_selection(),
            "notes.editor:copy-blocks" => self.editor.copy_selected_blocks(),
            _ => {
                let index = target
                    .strip_prefix("notes.block-select:")
                    .and_then(|id| self.editor.blocks().iter().position(|block| block.id == id))
                    .or_else(|| {
                        (target == "notes.editor:select-block")
                            .then(|| self.editor.focused_index())
                            .flatten()
                    });
                index.is_some_and(|index| self.editor.select_block_group(index))
            }
        })
    }

    pub(super) fn selection_accessibility(&self, tree: &mut ui::a11y::AccessNode) {
        use ui::a11y::{AccessNode, AccessState, Role};
        let composing = self.is_composing();
        let disabled = self.source.read_only() || composing || self.sources.is_empty();
        let button = |target: String, name: String, disabled: bool| {
            AccessNode::new(Role::Button)
                .with_name(name)
                .with_action_target(target)
                .with_state(AccessState::new().focusable(!disabled).disabled(disabled))
        };
        for row in &mut tree.children {
            let Some(id) = row
                .semantic_key
                .as_deref()
                .and_then(|key| key.strip_prefix("notes.block:"))
            else {
                continue;
            };
            let mut select = button(
                format!("notes.block-select:{id}"),
                ui::i18n!("widget-block-select"),
                composing,
            );
            let mut delete = button(
                format!("notes.block-delete:{id}"),
                ui::i18n!("widget-block-delete"),
                disabled,
            );
            select.bounds = row.bounds;
            delete.bounds = row.bounds;
            row.children.extend([select, delete]);
        }
        let missing = self.editor.selected_block_range().is_none();
        let mode = self.editor.is_block_selection_mode();
        for (target, label, disabled) in [
            (
                "select-block",
                ui::i18n!("widget-block-select"),
                composing || self.editor.focused_index().is_none(),
            ),
            (
                "select-all-blocks",
                ui::i18n!("widget-block-select-all"),
                composing,
            ),
            (
                "extend-up",
                ui::i18n!("widget-block-select-up"),
                !self.editor.can_navigate_block_selection(true),
            ),
            (
                "extend-down",
                ui::i18n!("widget-block-select-down"),
                !self.editor.can_navigate_block_selection(false),
            ),
            (
                "edit-block",
                ui::i18n!("widget-block-edit"),
                composing || !mode,
            ),
            (
                "copy-blocks",
                ui::i18n!("widget-block-copy"),
                composing || missing,
            ),
            (
                "cut-blocks",
                ui::i18n!("widget-block-cut"),
                disabled || missing,
            ),
            (
                "delete",
                ui::i18n!("widget-block-delete-selection"),
                disabled || missing,
            ),
        ] {
            let mut command = button(format!("notes.editor:{target}"), label, disabled);
            command.bounds = tree.bounds;
            tree.children.push(command);
        }
    }
}

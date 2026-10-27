//! The menu a secondary click, a long press or the menu key opens on the
//! words: cut, copy and paste, select all, and the formatting a toolbar
//! applies, each through the command its shortcut or button runs.

use vello::kurbo::Point;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::{BlockEditor, BlockKind, CursorPlacement, DocumentPosition};
use crate::i18n::I18nManager;
use crate::platform::Shortcut;
use crate::text_selection::word_boundaries;
use crate::widget::context_menu::{ContextMenu, MenuItem};
use crate::widget::marks::Mark;
use crate::widget::{EventCtx, Widget};

/// The prefix of the text menu's rows' targets in the accessibility tree.
const TEXT_MENU_TARGET: &str = "editor.text-menu:";

/// What a row of the text menu does.
#[derive(Clone, Debug, PartialEq)]
enum TextMenuAction {
    /// `cut`, `copy`, `paste`, `paste-plain` or `select-all`
    /// ([`BlockEditor::clipboard_command`]).
    Clipboard(&'static str),
    Mark(Mark),
    List {
        ordered: bool,
    },
}

/// The text menu and what each of its rows does.
#[derive(Default)]
pub(super) struct TextMenu {
    menu: ContextMenu,
    /// Parallel to the menu's rows; separators are `None`.
    actions: Vec<Option<TextMenuAction>>,
}

impl TextMenu {
    pub(super) fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    pub(super) fn needs_animation_frame(&self) -> bool {
        self.menu.needs_animation_frame()
    }

    /// The open menu's box: what is modal while it is open.
    pub(super) fn modal_rect(&self) -> Option<vello::kurbo::Rect> {
        self.menu.modal_capture()
    }
}

impl BlockEditor {
    /// The text menu, while it is open: where a host places it or finds
    /// its rows.
    #[must_use]
    pub fn text_menu(&self) -> Option<&ContextMenu> {
        self.text_menu
            .menu
            .is_open()
            .then_some(&self.text_menu.menu)
    }

    /// Open the text menu at `at` (the editor's coordinates) on the words
    /// there: over the selection when `at` is in it, else over the word
    /// under it, selected. `false` when no editable words are at `at`.
    pub fn open_text_menu(&mut self, at: Point) -> bool {
        if self.read_only {
            return false;
        }
        let Some(index) = self
            .blocks
            .iter()
            .position(|block| block.kind.is_editable() && block.line.get_rect().contains(at))
        else {
            return false;
        };
        let byte = self
            .blocks
            .get(index)
            .map_or(0, |block| block.line.inner().byte_at_point(at));
        if !self.selection_holds(DocumentPosition { block: index, byte }) {
            self.clear_document_selection();
            self.focus_block(index, CursorPlacement::Byte(byte));
            if let Some(line) = self.blocks.get_mut(index).map(|block| &mut block.line) {
                let (start, end) = word_boundaries(line.value(), byte);
                if start < end {
                    line.inner_mut().set_selection(start, end);
                }
            }
        }
        self.show_text_menu(at);
        true
    }

    /// Route `event` to the text menu while it is open (it is modal: every
    /// pointer and key event is its), or open it on a secondary press over
    /// the words or on the menu key. `true` when the event was the menu's.
    pub(super) fn handle_text_menu_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
    ) -> bool {
        let secondary_press = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            }
        );
        if self.text_menu.is_open() {
            if !matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::KeyboardInput { .. }
                    | WindowEvent::Ime(_)
                    | WindowEvent::Touch(_)
            ) {
                return false;
            }
            self.handle_cursor_moved(event, ctx.scale_factor);
            if secondary_press {
                // A secondary press elsewhere opens the menu there instead.
                self.text_menu.menu.close_at_once();
                self.open_text_menu(self.last_mouse_pos);
            } else {
                Widget::handle_event(&mut self.text_menu.menu, event, ctx);
                if let Some(row) = self.text_menu.menu.take_selection() {
                    self.run_text_menu_row(row);
                }
            }
            *ctx.handled = true;
            self.dirty = true;
            return true;
        }
        let menu_key = matches!(
            event,
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    state: ElementState::Pressed,
                    logical_key,
                    ..
                },
                ..
            } if *logical_key == Key::Named(NamedKey::ContextMenu)
                || (ctx.modifiers.state().shift_key() && *logical_key == Key::Named(NamedKey::F10))
        );
        let opened = if secondary_press {
            self.open_text_menu(self.last_mouse_pos)
        } else if menu_key && self.is_focused {
            self.open_text_menu_at_caret()
        } else {
            false
        };
        if opened {
            *ctx.focus_target = Some(ctx.node_id);
            *ctx.handled = true;
            self.dirty = true;
        }
        opened
    }

    /// Paint the text menu, open or leaving, where it was opened, kept
    /// inside the editor's popover bounds.
    pub(super) fn paint_text_menu(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
    ) {
        if let Some(bounds) = self.popover_bounds.or(self.overlay_viewport) {
            self.text_menu.menu.set_viewport(Some(bounds));
        }
        Widget::draw_overlay(
            &mut self.text_menu.menu,
            scene,
            transform,
            text,
            theme,
            &crate::presence::PresenceCtx::empty(),
        );
    }

    /// The open text menu's node of the accessibility tree: its rows, each
    /// an action an assistive technology runs as a click does.
    pub(super) fn text_menu_access_node(&self) -> Option<crate::a11y::AccessNode> {
        fn namespace_targets(node: &mut crate::a11y::AccessNode) {
            if let Some(target) = node.action_target.take() {
                node.action_target = Some(format!("{TEXT_MENU_TARGET}{target}"));
            }
            for child in &mut node.children {
                namespace_targets(child);
            }
        }
        if !self.text_menu.is_open() {
            return None;
        }
        let mut menu = Widget::accessibility(&self.text_menu.menu)?;
        namespace_targets(&mut menu);
        menu.semantic_key = Some("editor.text-menu".into());
        menu.bounds = self.text_menu.modal_rect().map(Into::into);
        Some(menu)
    }

    /// Run the text menu's row an assistive technology activated
    /// (`target` from [`Self::text_menu_access_node`]). `None` when `target`
    /// is not the menu's.
    pub(super) fn perform_text_menu_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> Option<bool> {
        let target = target.strip_prefix(TEXT_MENU_TARGET)?;
        let handled = self
            .text_menu
            .menu
            .perform_semantic_action(target, action, state, path);
        if let Some(row) = self.text_menu.menu.take_selection() {
            self.run_text_menu_row(row);
        }
        if handled {
            self.dirty = true;
        }
        Some(handled)
    }

    /// The menu key's text menu: at the caret, over the selection.
    fn open_text_menu_at_caret(&mut self) -> bool {
        let Some(line) = self
            .focused
            .and_then(|index| self.blocks.get(index))
            .map(|block| &block.line)
        else {
            return false;
        };
        let caret = line.caret_rect(&self.layout_theme, line.cursor_pos());
        self.show_text_menu(Point::new(caret.x0, caret.y1));
        true
    }

    /// Whether `at` lies in what is selected: the selection across
    /// paragraphs, or the focused paragraph's.
    fn selection_holds(&self, at: DocumentPosition) -> bool {
        if let Some(selection) = self.document_selection {
            let (start, end) = selection.ordered();
            return start != end && start <= at && at <= end;
        }
        self.focused == Some(at.block)
            && self
                .blocks
                .get(at.block)
                .and_then(|block| block.line.selection_range())
                .is_some_and(|(start, end)| start < end && start <= at.byte && at.byte <= end)
    }

    /// Whether anything is selected to cut or copy.
    fn has_text_selection(&self) -> bool {
        self.document_selection
            .is_some_and(|selection| selection.anchor != selection.cursor)
            || self
                .focused
                .and_then(|index| self.blocks.get(index))
                .and_then(|block| block.line.selection_range())
                .is_some_and(|(start, end)| start < end)
    }

    /// Build the menu's rows for what is selected and open it at `at`.
    fn show_text_menu(&mut self, at: Point) {
        let label = |key: &str| I18nManager::lookup_plain(key);
        let selected = self.has_text_selection();
        // Paste stays offered: reading the clipboard to find out would wait
        // on the compositor while the menu opens, and use up a browser's
        // one read of a paste. A paste of nothing changes nothing.
        let active = self.active_marks();
        let focused_kind = self
            .focused
            .and_then(|index| self.blocks.get(index))
            .map(|block| block.kind.clone());
        let mut rows: Vec<(MenuItem, Option<TextMenuAction>)> = Vec::new();
        let mut row = |item: MenuItem, action: TextMenuAction, enabled: bool| {
            rows.push((item.with_enabled(enabled), Some(action)));
        };
        row(
            MenuItem::action(label("text-menu-cut"), || {}).with_shortcut(Shortcut::command("X")),
            TextMenuAction::Clipboard("cut"),
            selected,
        );
        row(
            MenuItem::action(label("text-menu-copy"), || {}).with_shortcut(Shortcut::command("C")),
            TextMenuAction::Clipboard("copy"),
            selected,
        );
        row(
            MenuItem::action(label("text-menu-paste"), || {}).with_shortcut(Shortcut::command("V")),
            TextMenuAction::Clipboard("paste"),
            true,
        );
        row(
            MenuItem::action(label("text-menu-paste-plain"), || {})
                .with_shortcut(Shortcut::command("V").shift()),
            TextMenuAction::Clipboard("paste-plain"),
            true,
        );
        row(
            MenuItem::action(label("text-menu-select-all"), || {})
                .with_shortcut(Shortcut::command("A")),
            TextMenuAction::Clipboard("select-all"),
            true,
        );
        rows.push((MenuItem::separator(), None));
        for (key, mark, shortcut) in [
            ("text-menu-bold", Mark::Bold, Some("B")),
            ("text-menu-italic", Mark::Italic, Some("I")),
            ("text-menu-underline", Mark::Underline, Some("U")),
            ("text-menu-strikethrough", Mark::Strikethrough, None),
        ] {
            let on = active.iter().any(|held| held.same_kind(&mark));
            let mut item = MenuItem::action(label(key), || {}).with_checked(on);
            if let Some(letter) = shortcut {
                item = item.with_shortcut(Shortcut::command(letter));
            }
            rows.push((item, Some(TextMenuAction::Mark(mark))));
        }
        rows.push((MenuItem::separator(), None));
        for (key, ordered) in [
            ("text-menu-bulleted-list", false),
            ("text-menu-numbered-list", true),
        ] {
            let on = focused_kind.as_ref().is_some_and(|kind| {
                if ordered {
                    matches!(kind, BlockKind::Numbered { .. })
                } else {
                    matches!(kind, BlockKind::Bullet { .. })
                }
            });
            rows.push((
                MenuItem::action(label(key), || {}).with_checked(on),
                Some(TextMenuAction::List { ordered }),
            ));
        }
        let (items, actions): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
        self.text_menu.menu.set_items(items);
        self.text_menu.actions = actions;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "pointer coordinates are bounded on-screen positions, never near f32's range"
        )]
        self.text_menu.menu.open_at([at.x as f32, at.y as f32]);
    }

    /// Do what row `row` of the text menu does, as its shortcut or the
    /// toolbar's button does.
    fn run_text_menu_row(&mut self, row: usize) {
        let Some(action) = self.text_menu.actions.get(row).cloned().flatten() else {
            return;
        };
        match action {
            TextMenuAction::Clipboard(command) => {
                self.clipboard_command(command);
            }
            TextMenuAction::Mark(mark) => {
                self.toggle_mark(mark);
            }
            TextMenuAction::List { ordered } => {
                self.toggle_list(ordered);
            }
        }
        self.dirty = true;
    }
}

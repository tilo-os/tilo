//! Code-editor adapter for the shared command palette.
//!
//! The editor supplies domain entries (project node ids and typed commands),
//! while `ui::CommandPalette` owns query editing, IME composition, selection,
//! fuzzy matching, navigation, layout and painting.

use taffy::{Style, TaffyTree};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::{CommandEntry, CommandPalette as SharedPalette, PresenceCtx, icons::Icon};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use crate::model::NodeId as TreeId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteKind {
    File(TreeId),
    Command(Command),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    NewFile,
    OpenFolder,
    OpenFile,
    Save,
    SaveAs,
    SaveAll,
    RevertFile,
    CloseTab,
    Find,
    Replace,
    SearchProject,
    GoToLine,
    DuplicateLine,
    MoveLineUp,
    MoveLineDown,
    DeleteLine,
    ToggleComment,
    ToggleBottomPanel,
    NextTheme,
    GoToProblems,
    GoToSearch,
    GoToTerminal,
    ToggleSidebar,
}

impl Command {
    pub const fn id(self) -> &'static str {
        match self {
            Self::NewFile => "new-file",
            Self::OpenFolder => "open-folder",
            Self::OpenFile => "open-file",
            Self::Save => "save",
            Self::SaveAs => "save-as",
            Self::SaveAll => "save-all",
            Self::RevertFile => "revert-file",
            Self::CloseTab => "close-tab",
            Self::Find => "find",
            Self::Replace => "replace",
            Self::SearchProject => "search-project",
            Self::GoToLine => "go-to-line",
            Self::DuplicateLine => "duplicate-line",
            Self::MoveLineUp => "move-line-up",
            Self::MoveLineDown => "move-line-down",
            Self::DeleteLine => "delete-line",
            Self::ToggleComment => "toggle-comment",
            Self::ToggleBottomPanel => "toggle-bottom-panel",
            Self::NextTheme => "next-theme",
            Self::GoToProblems => "go-to-problems",
            Self::GoToSearch => "go-to-search",
            Self::GoToTerminal => "go-to-terminal",
            Self::ToggleSidebar => "toggle-sidebar",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::catalogue()
            .into_iter()
            .find_map(|(command, _, _)| (command.id() == id).then_some(command))
    }

    pub fn all() -> Vec<(Command, &'static str)> {
        Self::catalogue()
            .into_iter()
            .map(|(command, label, _)| (command, label))
            .collect()
    }

    pub fn catalogue() -> Vec<(Command, &'static str, &'static str)> {
        vec![
            (
                Command::OpenFolder,
                "File: Open Folder\u{2026}",
                "Ctrl+Shift+O",
            ),
            (Command::OpenFile, "File: Open File\u{2026}", "Ctrl+O"),
            (Command::NewFile, "File: New File", ""),
            (Command::Save, "File: Save", "Ctrl+S"),
            (Command::SaveAs, "File: Save As\u{2026}", "Ctrl+Shift+S"),
            (Command::SaveAll, "File: Save All", "Ctrl+Alt+S"),
            (Command::RevertFile, "File: Revert to Saved", ""),
            (Command::CloseTab, "View: Close Active Tab", "Ctrl+W"),
            (Command::Find, "Edit: Find in File", "Ctrl+F"),
            (Command::Replace, "Edit: Replace in File", "Ctrl+H"),
            (
                Command::SearchProject,
                "Edit: Search Project",
                "Ctrl+Shift+F",
            ),
            (Command::GoToLine, "Go: Go to Line\u{2026}", "Ctrl+G"),
            (
                Command::DuplicateLine,
                "Selection: Duplicate Line",
                "Shift+Alt+Down",
            ),
            (Command::MoveLineUp, "Selection: Move Line Up", "Alt+Up"),
            (
                Command::MoveLineDown,
                "Selection: Move Line Down",
                "Alt+Down",
            ),
            (
                Command::DeleteLine,
                "Selection: Delete Line",
                "Ctrl+Shift+K",
            ),
            (
                Command::ToggleComment,
                "Edit: Toggle Line Comment",
                "Ctrl+/",
            ),
            (
                Command::ToggleBottomPanel,
                "View: Toggle Bottom Panel",
                "Ctrl+J",
            ),
            (Command::ToggleSidebar, "View: Toggle Sidebar", "Ctrl+B"),
            (Command::NextTheme, "Preferences: Cycle Color Theme", ""),
            (Command::GoToProblems, "View: Focus Problems", ""),
            (Command::GoToSearch, "View: Focus Search Results", ""),
            (Command::GoToTerminal, "View: Focus Terminal", ""),
        ]
    }

    fn keybinding(self) -> &'static str {
        Self::catalogue()
            .into_iter()
            .find_map(|(candidate, _, key)| (candidate == self).then_some(key))
            .unwrap_or("")
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub detail: String,
    pub kind: PaletteKind,
    /// The resource-backed row that executes this entry. Keeping it beside
    /// the presentation metadata makes it impossible for the adapter to
    /// accidentally invent a private callback command.
    pub action: CommandEntry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Files,
    Commands,
    Goto,
}

pub struct CommandPalette {
    shared: SharedPalette,
    entries: Vec<Entry>,
    active: Vec<PaletteKind>,
    viewport: Rect,
    mode: Mode,
}

impl CommandPalette {
    pub fn new() -> Self {
        Self {
            shared: SharedPalette::commands(),
            entries: Vec::new(),
            active: Vec::new(),
            viewport: Rect::ZERO,
            mode: Mode::Files,
        }
    }

    pub fn set_viewport(&mut self, rect: Rect) {
        self.viewport = rect;
    }

    pub fn is_visible(&self) -> bool {
        self.shared.is_open()
    }

    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        if self.is_visible() && self.mode != Mode::Goto {
            self.load_mode(self.mode);
        }
    }

    fn load_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.active.clear();
        let mut rows = Vec::new();
        for entry in &self.entries {
            let belongs = matches!(
                (mode, &entry.kind),
                (Mode::Files, PaletteKind::File(_)) | (Mode::Commands, PaletteKind::Command(_))
            );
            if !belongs {
                continue;
            }
            self.active.push(entry.kind.clone());
            let mut row = entry.action.clone().icon(match entry.kind {
                PaletteKind::File(_) => Icon::File,
                PaletteKind::Command(_) => Icon::Settings,
            });
            match entry.kind {
                PaletteKind::File(_) if !entry.detail.is_empty() => {
                    row = row.description(&entry.detail);
                }
                PaletteKind::Command(command) => {
                    let key = command.keybinding();
                    if !key.is_empty() {
                        row = row.keybinding(key);
                    }
                }
                PaletteKind::File(_) => {}
            }
            rows.push(row);
        }
        self.shared.set_entries(rows);
        self.shared.set_query_placeholder(match mode {
            Mode::Files => "Go to file\u{2026}",
            Mode::Commands => "Type a command\u{2026}",
            Mode::Goto => "Line number\u{2026}",
        });
        self.shared.open();
    }

    pub fn open_files(&mut self) {
        self.load_mode(Mode::Files);
    }

    pub fn open_commands(&mut self) {
        self.load_mode(Mode::Commands);
    }

    pub fn open_goto(&mut self) {
        self.mode = Mode::Goto;
        self.active.clear();
        if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.kind == PaletteKind::Command(Command::GoToLine))
        {
            self.shared.open_action_argument(entry.action.clone(), None);
        }
    }

    pub fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        ui::widget::controls::take_control_invocation(&mut self.shared)
    }

    pub fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        ui::widget::controls::control_action_bindings(&self.shared)
    }

    pub fn seed_query(&mut self, query: &str) {
        if self.mode == Mode::Goto {
            if let Some(entry) = self
                .entries
                .iter()
                .find(|entry| entry.kind == PaletteKind::Command(Command::GoToLine))
            {
                self.shared
                    .open_action_argument(entry.action.clone(), Some(query.to_string()));
            }
        } else {
            self.shared.set_query(query);
        }
    }

    pub fn handle_key(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
    ) -> bool {
        if !self.shared.is_open() {
            return false;
        }
        let mut handled = false;
        let mut focus = None;
        let mut taffy = TaffyTree::new();
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return false;
        };
        Widget::handle_event(
            &mut self.shared,
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    pub fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.shared.is_open() {
            return;
        }
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &self.viewport,
        );
        let (width, height) = self.shared.desired_size();
        let x0 = self.viewport.x0 + (self.viewport.width() - width) / 2.0;
        let y0 = self.viewport.y0 + 80.0;
        self.shared
            .set_rect(Rect::new(x0, y0, x0 + width, y0 + height));
        Widget::draw(
            &mut self.shared,
            scene,
            transform,
            text_renderer,
            theme,
            &PresenceCtx::default(),
        );
    }
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native(resource_id: &str, action_id: &str, label: &str) -> CommandEntry {
        let resource = ui::tilo_actions::Resource::new(resource_id, "test", "Test").with_action(
            ui::tilo_actions::ActionSpec::operation(
                action_id,
                label,
                ui::tilo_actions::OperationKind::View,
                "test.view",
                &action_id,
            )
            .unwrap(),
        );
        CommandEntry::from_flat_action(&ui::tilo_actions::flatten(&[resource])[0])
    }

    fn entries() -> Vec<Entry> {
        vec![
            Entry {
                detail: "src".into(),
                kind: PaletteKind::File(42),
                action: native("code_editor.file:42", "open", "Open main.rs"),
            },
            Entry {
                detail: String::new(),
                kind: PaletteKind::Command(Command::NewFile),
                action: native("code_editor.project", "command-new-file", "New file"),
            },
        ]
    }

    #[test]
    fn palette_modes_filter_entries_and_file_selection_emits_its_action() {
        // Modes map only their domain entries.
        {
            let mut palette = CommandPalette::new();
            palette.set_entries(entries());
            palette.open_commands();
            assert_eq!(palette.active, vec![PaletteKind::Command(Command::NewFile)]);
            palette.open_files();
            assert_eq!(palette.active, vec![PaletteKind::File(42)]);
        }

        // Shared selection emits the file resource action (fresh palette).
        {
            let mut palette = CommandPalette::new();
            palette.set_entries(entries());
            palette.open_files();
            let mut state = automerge::AutoCommit::new();
            palette
                .shared
                .activate_selected(&mut state, &automerge::ROOT);
            let invocation = palette
                .take_control_invocation()
                .expect("native invocation");
            assert_eq!(invocation.address.resource, "code_editor.file:42");
            assert_eq!(invocation.address.action, "open");
        }
    }
}

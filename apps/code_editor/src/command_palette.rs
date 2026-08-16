//! The command palette: a centered overlay invoked with Ctrl/Cmd+P (file
//! quick-open) and Ctrl/Cmd+Shift+P (command mode). It fuzzy-filters a list of
//! entries and emits the chosen one. Drawn in the overlay pass so it floats
//! above the editor.

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::model::NodeId as TreeId;

/// What kind of entry the palette is offering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteKind {
    File(TreeId),
    Command(Command),
}

/// Commands the palette can dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    ToggleBottomPanel,
    NextTheme,
    GoToProblems,
    GoToSearch,
    GoToTerminal,
    ToggleSidebar,
}

impl Command {
    /// Every command, with its palette label and the keybinding to advertise
    /// beside it. A command reachable only by a shortcut nobody told you about
    /// is not reachable.
    pub fn all() -> Vec<(Command, &'static str)> {
        Self::catalogue()
            .into_iter()
            .map(|(c, label, _)| (c, label))
            .collect()
    }

    pub fn catalogue() -> Vec<(Command, &'static str, &'static str)> {
        vec![
            (Command::OpenFolder, "File: Open Folder\u{2026}", "Ctrl+Shift+O"),
            (Command::OpenFile, "File: Open File\u{2026}", "Ctrl+O"),
            (Command::NewFile, "File: New File", ""),
            (Command::Save, "File: Save", "Ctrl+S"),
            (Command::SaveAs, "File: Save As\u{2026}", "Ctrl+Shift+S"),
            (Command::SaveAll, "File: Save All", "Ctrl+Alt+S"),
            (Command::RevertFile, "File: Revert to Saved", ""),
            (Command::CloseTab, "View: Close Active Tab", "Ctrl+W"),
            (Command::Find, "Edit: Find in File", "Ctrl+F"),
            (Command::Replace, "Edit: Replace in File", "Ctrl+H"),
            (Command::SearchProject, "Edit: Search Project", "Ctrl+Shift+F"),
            (Command::ToggleBottomPanel, "View: Toggle Bottom Panel", "Ctrl+J"),
            (Command::ToggleSidebar, "View: Toggle Sidebar", "Ctrl+B"),
            (Command::NextTheme, "Preferences: Cycle Color Theme", ""),
            (Command::GoToProblems, "View: Focus Problems", ""),
            (Command::GoToSearch, "View: Focus Search Results", ""),
            (Command::GoToTerminal, "View: Focus Terminal", ""),
        ]
    }
}

/// One filterable entry.
#[derive(Clone, Debug)]
pub struct Entry {
    pub label: String,
    /// Secondary, dimmed text (e.g. a file's folder path).
    pub detail: String,
    pub kind: PaletteKind,
}

pub struct CommandPalette {
    visible: bool,
    /// Whether we're in command mode (`>` prefix) vs file mode.
    command_mode: bool,
    query: String,
    all_entries: Vec<Entry>,
    filtered: Vec<usize>,
    selected: usize,
    viewport: Rect,
    /// Set when the user accepts an entry; the app drains it.
    chosen: Option<PaletteKind>,
}

const WIDTH: f64 = 560.0;
const INPUT_H: f64 = 44.0;
const ROW_H: f64 = 36.0;
const MAX_ROWS: usize = 9;

impl CommandPalette {
    pub fn new() -> Self {
        CommandPalette {
            visible: false,
            command_mode: false,
            query: String::new(),
            all_entries: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            viewport: Rect::ZERO,
            chosen: None,
        }
    }

    pub fn set_viewport(&mut self, r: Rect) {
        self.viewport = r;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Feed the current set of entries (files + commands). Re-filters.
    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        self.all_entries = entries;
        self.refilter();
    }

    /// Open in file-quick-open mode.
    pub fn open_files(&mut self) {
        self.visible = true;
        self.command_mode = false;
        self.query.clear();
        self.selected = 0;
        self.refilter();
    }

    /// Open in command mode.
    pub fn open_commands(&mut self) {
        self.visible = true;
        self.command_mode = true;
        self.query.clear();
        self.selected = 0;
        self.refilter();
    }

    pub fn close(&mut self) {
        self.visible = false;
    }

    pub fn take_chosen(&mut self) -> Option<PaletteKind> {
        self.chosen.take()
    }

    /// Simple subsequence fuzzy match: every query char appears in order.
    fn fuzzy_match(query: &str, text: &str) -> Option<i32> {
        if query.is_empty() {
            return Some(0);
        }
        let q: Vec<char> = query.to_lowercase().chars().collect();
        let mut qi = 0;
        let mut score = 0i32;
        let mut prev_match = false;
        for ch in text.to_lowercase().chars() {
            if qi < q.len() && ch == q[qi] {
                qi += 1;
                // Reward consecutive matches.
                score += if prev_match { 3 } else { 1 };
                prev_match = true;
            } else {
                prev_match = false;
            }
        }
        if qi == q.len() {
            // Shorter targets rank higher for equal matches.
            Some(score - (text.len() as i32) / 20)
        } else {
            None
        }
    }

    fn refilter(&mut self) {
        let want_command = self.command_mode;
        let mut scored: Vec<(usize, i32)> = self
            .all_entries
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e.kind, PaletteKind::Command(_)) == want_command)
            .filter_map(|(i, e)| {
                Self::fuzzy_match(&self.query, &e.label).map(|s| (i, s))
            })
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        self.filtered = scored.into_iter().map(|(i, _)| i).collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }

    /// Handle a key while visible. Returns true if consumed.
    pub fn handle_key(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    text,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        match logical_key {
            Key::Named(NamedKey::Escape) => {
                self.close();
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                if !self.filtered.is_empty() {
                    self.selected = (self.selected + 1) % self.filtered.len();
                }
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                if !self.filtered.is_empty() {
                    self.selected =
                        (self.selected + self.filtered.len() - 1) % self.filtered.len();
                }
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(&idx) = self.filtered.get(self.selected) {
                    self.chosen = Some(self.all_entries[idx].kind.clone());
                }
                self.close();
                true
            }
            Key::Named(NamedKey::Backspace) => {
                self.query.pop();
                self.refilter();
                true
            }
            _ => {
                if let Some(t) = text {
                    let s = t.as_str();
                    if !s.is_empty() && !s.chars().any(|c| c.is_control()) {
                        self.query.push_str(s);
                        self.refilter();
                        return true;
                    }
                }
                // Swallow everything while open so keystrokes don't fall to
                // the editor underneath.
                true
            }
        }
    }

    /// Draw the palette as an overlay. No-op when hidden.
    pub fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.visible {
            return;
        }
        let c = &theme.tokens.colors;
        // Scrim over the whole viewport.
        scene.fill(Fill::NonZero, transform, c.surfaces.overlay, None, &self.viewport);

        let rows = self.filtered.len().min(MAX_ROWS);
        let panel_h = INPUT_H + rows as f64 * ROW_H + 8.0;
        let x0 = self.viewport.x0 + (self.viewport.width() - WIDTH) / 2.0;
        let y0 = self.viewport.y0 + 80.0;
        let panel = RoundedRect::new(x0, y0, x0 + WIDTH, y0 + panel_h, 10.0);
        scene.fill(Fill::NonZero, transform, c.surfaces.glass, None, &panel);
        scene.stroke(&Stroke::new(1.0), transform, c.border_white_20, None, &panel);

        // Input row.
        let prompt = if self.command_mode { "> " } else { "" };
        let shown = format!("{}{}", prompt, self.query);
        let placeholder = if self.command_mode {
            "Type a command..."
        } else {
            "Go to file..."
        };
        let input_color = if self.query.is_empty() {
            c.text_dim
        } else {
            c.text_primary
        };
        let input_text = if self.query.is_empty() {
            format!("{}{}", prompt, placeholder)
        } else {
            shown.clone()
        };
        text_renderer.draw_text(
            scene,
            &input_text,
            16.0,
            input_color,
            transform * Affine::translate((x0 + 16.0, y0 + 13.0)),
            Some((WIDTH - 32.0) as f32),
        );
        // Caret after the query text.
        let (qw, _) = text_renderer.measure(&shown, 16.0, None);
        if !self.query.is_empty() || self.command_mode {
            let cx = x0 + 16.0 + qw as f64 + 1.0;
            scene.fill(
                Fill::NonZero,
                transform,
                c.border_focus,
                None,
                &Rect::new(cx, y0 + 12.0, cx + 2.0, y0 + 32.0),
            );
        }
        // Divider under the input.
        scene.fill(
            Fill::NonZero,
            transform,
            c.border_white_10,
            None,
            &Rect::new(x0, y0 + INPUT_H, x0 + WIDTH, y0 + INPUT_H + 1.0),
        );

        // Result rows.
        for (vi, &idx) in self.filtered.iter().take(MAX_ROWS).enumerate() {
            let entry = &self.all_entries[idx];
            let ry = y0 + INPUT_H + vi as f64 * ROW_H + 4.0;
            let row_rect = Rect::new(x0 + 4.0, ry, x0 + WIDTH - 4.0, ry + ROW_H);
            if vi == self.selected {
                let sel = RoundedRect::from_rect(row_rect, 6.0);
                scene.fill(Fill::NonZero, transform, c.accent[5].multiply_alpha(0.35), None, &sel);
            }
            // Icon glyph: command vs file.
            let glyph = match entry.kind {
                PaletteKind::Command(_) => "\u{2699}",
                PaletteKind::File(_) => "\u{1F5CB}",
            };
            text_renderer.draw_text(
                scene,
                glyph,
                14.0,
                c.text_dim,
                transform * Affine::translate((x0 + 16.0, ry + 9.0)),
                None,
            );
            text_renderer.draw_text(
                scene,
                &entry.label,
                14.0,
                c.text_primary,
                transform * Affine::translate((x0 + 40.0, ry + 9.0)),
                Some(360.0),
            );
            if !entry.detail.is_empty() {
                // Right-aligned dimmed detail.
                let (dw, _) = text_renderer.measure(&entry.detail, 12.0, None);
                text_renderer.draw_text(
                    scene,
                    &entry.detail,
                    12.0,
                    c.text_dim,
                    transform
                        * Affine::translate((x0 + WIDTH - 16.0 - dw as f64, ry + 10.0)),
                    None,
                );
            }
        }

        if self.filtered.is_empty() {
            text_renderer.draw_text(
                scene,
                "No matching results",
                14.0,
                c.text_dim,
                transform * Affine::translate((x0 + 16.0, y0 + INPUT_H + 12.0)),
                None,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matches_subsequence() {
        assert!(CommandPalette::fuzzy_match("mn", "main.rs").is_some());
        assert!(CommandPalette::fuzzy_match("xyz", "main.rs").is_none());
        // Empty query matches anything.
        assert!(CommandPalette::fuzzy_match("", "anything").is_some());
    }

    #[test]
    fn consecutive_matches_score_higher() {
        let consec = CommandPalette::fuzzy_match("mai", "main.rs").unwrap();
        let split = CommandPalette::fuzzy_match("mai", "m_a_i_x.rs").unwrap();
        assert!(consec > split);
    }

    #[test]
    fn command_mode_filters_to_commands() {
        let mut p = CommandPalette::new();
        p.set_entries(vec![
            Entry {
                label: "main.rs".into(),
                detail: "src".into(),
                kind: PaletteKind::File(0),
            },
            Entry {
                label: "File: New File".into(),
                detail: String::new(),
                kind: PaletteKind::Command(Command::NewFile),
            },
        ]);
        p.open_commands();
        assert_eq!(p.filtered.len(), 1);
        assert!(matches!(
            p.all_entries[p.filtered[0]].kind,
            PaletteKind::Command(_)
        ));
    }

    #[test]
    fn enter_chooses_selected() {
        let mut p = CommandPalette::new();
        p.set_entries(vec![Entry {
            label: "main.rs".into(),
            detail: "src".into(),
            kind: PaletteKind::File(42),
        }]);
        p.open_files();
        // Simulate Enter by calling the internal accept path.
        if let Some(&idx) = p.filtered.get(p.selected) {
            p.chosen = Some(p.all_entries[idx].kind.clone());
        }
        assert_eq!(p.take_chosen(), Some(PaletteKind::File(42)));
    }
}

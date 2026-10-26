//! The spreadsheet's commands, as slots in the window's bar
//! ([`ui::window_toolbar`]).
//!
//! The bar shows the File and Format menus and Bold in its row, and `AutoSum`,
//! showing formulas and the document's settings in its menu. Each slot runs
//! the same command the bar the sheet used to draw ran, with the document in
//! hand. The location names the workbook by its title or file and the sheet
//! in view as its next level; a workbook never named is called by the sheet
//! in view. Search is the bar's own; the formula bar stays the sheet's.

use ui::window_toolbar::{
    LocationSegment, ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar,
};

use super::chrome::{MenuRow, post};
use super::{Edits, MenuItem, SheetsApp};

impl SheetsApp {
    /// The window bar's slots, and the command each slot id runs.
    fn window_bar_commands(&self) -> (WindowToolbar, Vec<(String, MenuItem)>) {
        let mut commands = Vec::new();
        let mut menu = |prefix: &str, rows: Vec<MenuRow>, icon: &str| {
            rows.into_iter()
                .enumerate()
                .filter_map(|(index, (label, item, checked))| {
                    let item = item?;
                    let id = format!("{prefix}:{index}");
                    commands.push((id.clone(), item));
                    Some(ToolbarAction::new(id, icon, label).on(checked == Some(true)))
                })
                .collect::<Vec<_>>()
        };
        let file = menu("file", Self::file_menu_rows(), "file");
        let format = menu("format", self.format_menu_rows(), "type");
        let formulas = self.grid.formula_view();
        let mut actions = vec![
            ToolbarAction::new("file", "file", "File").with_items(file),
            ToolbarAction::new("format", "type", "Format").with_items(format),
            ToolbarAction::new("bold", "bold", "Bold").on(self.active_cell_bold),
            ToolbarAction::new("autosum", "plus", "AutoSum"),
            ToolbarAction::new(
                "formulas",
                "code",
                if formulas {
                    "Show calculated results"
                } else {
                    "Show formulas"
                },
            )
            .on(formulas),
            ToolbarAction::new("settings", "settings", "Document settings").in_menu(),
        ];
        // AutoSum and the formula view are occasional: the menu's.
        for action in actions.iter_mut().skip(3) {
            action.menu = true;
        }
        commands.push(("bold".into(), MenuItem::ToggleBold));
        commands.push(("autosum".into(), MenuItem::AutoSum));
        commands.push(("formulas".into(), MenuItem::ToggleFormulaView));
        commands.push(("settings".into(), MenuItem::OpenDocSettings));
        // A workbook with a name of its own stands first; one never named
        // is called by the sheet in view, as the model names it.
        let mut segments: Vec<LocationSegment> = self
            .workbook_name()
            .map(|label| LocationSegment {
                label,
                value: String::new(),
            })
            .into_iter()
            .collect();
        if let Some(id) = self.active_sheet_id()
            && let Some(sheet) = self.workbook.sheet(&id)
        {
            segments.push(LocationSegment {
                label: sheet.name.0.clone(),
                value: id.to_string(),
            });
        }
        let label = segments
            .first()
            .map(|segment| segment.label.clone())
            .unwrap_or_default();
        let toolbar = WindowToolbar {
            location: Some(ToolbarLocation {
                label,
                segments,
                ..ToolbarLocation::default()
            }),
            actions: std::mem::take(&mut actions),
            ..WindowToolbar::default()
        };
        (toolbar, commands)
    }

    /// The workbook's own name: its title, else its file's name without
    /// the extension; none while it was never named or saved.
    pub(super) fn workbook_name(&self) -> Option<String> {
        self.workbook
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                self.file
                    .as_deref()
                    .and_then(std::path::Path::file_stem)
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .filter(|stem| !stem.trim().is_empty())
            })
    }

    /// The window bar's slots, as declared.
    #[cfg(test)]
    pub(super) fn window_bar(&self) -> WindowToolbar {
        self.window_bar_commands().0
    }

    /// Run what the person did in the window's bar as the command it names,
    /// then declare the bar's slots for the sheet as it is now.
    pub(super) fn apply_window_toolbar(&mut self, doc: &mut Edits<'_>) {
        for event in ui::window_toolbar::take_events() {
            let ToolbarEvent::Action { id } = event else {
                continue;
            };
            let Some(item) = self
                .bar_commands
                .iter()
                .find(|(slot, _)| *slot == id)
                .map(|(_, item)| item.clone())
            else {
                continue;
            };
            post(&self.command_slot, item);
            self.drain_command_bar(doc);
        }
        let (toolbar, commands) = self.window_bar_commands();
        self.bar_commands = commands;
        ui::window_toolbar::declare(Some(toolbar));
    }
}

//! The spreadsheet's commands, as slots in the window's bar
//! ([`ui::window_toolbar`]).
//!
//! The bar shows the File and Format menus and Bold in its row, and `AutoSum`,
//! showing formulas and the document's settings in its menu. Each slot runs
//! the same command the bar the sheet used to draw ran, with the document in
//! hand. The location names the workbook by its title or file and the sheet
//! in view as its next level; a workbook never named is called by the sheet
//! in view. Search is the bar's own; the formula bar stays the sheet's.

use ui::window_toolbar::{LocationSegment, ToolbarAction, ToolbarLocation, WindowToolbar};

use super::chrome::{MenuRow, post};
use super::{Edits, MenuItem, SheetsApp};

impl SheetsApp {
    /// The window bar's slots, each carrying the command it runs.
    pub(super) fn window_bar(&self) -> WindowToolbar {
        let bar = &self.bar_commands;
        let menu = |rows: Vec<MenuRow>, icon: &str| {
            rows.into_iter()
                .filter_map(|(label, item, checked)| {
                    Some(bar.entry(icon, label, item?).on(checked == Some(true)))
                })
                .collect::<Vec<_>>()
        };
        let file = menu(Self::file_menu_rows(), "file");
        let format = menu(self.format_menu_rows(), "type");
        let formulas = self.grid.formula_view();
        let formulas_name = if formulas {
            "Show calculated results"
        } else {
            "Show formulas"
        };
        // AutoSum, the formula view and the settings are occasional: the
        // menu's.
        let actions = vec![
            ToolbarAction::new("file", "file", "File").with_items(file),
            ToolbarAction::new("format", "type", "Format").with_items(format),
            bar.entry("bold", "Bold", MenuItem::ToggleBold)
                .on(self.active_cell_bold),
            bar.entry("plus", "AutoSum", MenuItem::AutoSum).in_menu(),
            bar.entry("code", formulas_name, MenuItem::ToggleFormulaView)
                .on(formulas)
                .in_menu(),
            bar.entry("settings", "Document settings", MenuItem::OpenDocSettings)
                .in_menu(),
        ];
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
        WindowToolbar {
            location: Some(ToolbarLocation {
                label,
                segments,
                ..ToolbarLocation::default()
            }),
            actions,
            ..WindowToolbar::default()
        }
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

    /// Run what the person did in the window's bar as the command it names,
    /// then declare the bar's slots for the sheet as it is now.
    pub(super) fn apply_window_toolbar(&mut self, doc: &mut Edits<'_>) {
        for event in ui::window_toolbar::take_events() {
            if let Some(item) = self.bar_commands.pressed(&event) {
                post(&self.command_slot, item);
                self.drain_command_bar(doc);
            }
        }
        ui::window_toolbar::declare(Some(self.window_bar()));
    }
}

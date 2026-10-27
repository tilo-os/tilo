//! The hex editor's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): the file's name as the location, Overwrite and
//! Insert as the views, and the inspector's toggle as an action. The window's
//! search finds and replaces in the bytes ([`super::find`]) and its History
//! control opens the record's History; the view draws no header row.

use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar,
};

use super::HexView;
use crate::model::EditMode;

/// The bar action that shows and hides the inspector.
pub(crate) const INSPECTOR: &str = "hex.inspector";
/// The bar's views: how a typed byte lands.
pub(crate) const OVERWRITE: &str = "hex.mode:overwrite";
pub(crate) const INSERT: &str = "hex.mode:insert";

fn mode_id(mode: EditMode) -> &'static str {
    match mode {
        EditMode::Overwrite => OVERWRITE,
        EditMode::Insert => INSERT,
    }
}

impl HexView {
    /// The window bar's slots for what the view shows now; none of its own
    /// while no file is open.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        if !self.has_file {
            return WindowToolbar::default();
        }
        let open = self.drawer.is_open();
        WindowToolbar {
            location: Some(ToolbarLocation {
                label: self.file_name.clone(),
                ..ToolbarLocation::default()
            }),
            views: Some(ToolbarViews {
                name: "Edit mode".into(),
                choices: [EditMode::Overwrite, EditMode::Insert]
                    .into_iter()
                    .map(|mode| ToolbarView {
                        id: mode_id(mode).into(),
                        label: mode.label().into(),
                    })
                    .collect(),
                chosen: mode_id(self.model.mode()).into(),
            }),
            actions: vec![
                ToolbarAction::new(
                    INSPECTOR,
                    "panel-right",
                    if open {
                        "Hide inspector"
                    } else {
                        "Show inspector"
                    },
                )
                .on(open),
            ],
            ..WindowToolbar::default()
        }
    }

    /// Run what the person did in the window's bar, then declare the slots
    /// for what the view shows now.
    pub(super) fn run_window_toolbar(&mut self) {
        for event in ui::window_toolbar::take_events() {
            self.run_window_bar_event(&event);
        }
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run one thing the person did in the window's bar.
    pub(crate) fn run_window_bar_event(&mut self, event: &ToolbarEvent) {
        let ToolbarEvent::Action { id } = event else {
            return;
        };
        if !self.has_file {
            return;
        }
        match id.as_str() {
            INSPECTOR => self.toggle_inspector(),
            OVERWRITE | INSERT => {
                let want = if id == OVERWRITE {
                    EditMode::Overwrite
                } else {
                    EditMode::Insert
                };
                if self.model.mode() != want {
                    self.model.toggle_mode();
                    self.moved();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "window_bar_tests.rs"]
mod tests;

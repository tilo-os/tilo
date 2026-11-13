//! The editor's controls as slots in the window's bar: the file's name as the
//! location, Overwrite and Insert as the views, and the inspector's toggle as an
//! action. The bar's search finds and replaces in the bytes.

use ui::window_toolbar::{ToolbarEvent, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar};

use super::Editor;
use crate::model::EditMode;

pub(crate) const OVERWRITE: &str = "hex.mode:overwrite";
pub(crate) const INSERT: &str = "hex.mode:insert";

fn mode_id(mode: EditMode) -> &'static str {
    match mode {
        EditMode::Overwrite => OVERWRITE,
        EditMode::Insert => INSERT,
    }
}

impl Editor {
    /// The bar's slots for what the view shows now; none while no file is open.
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
                name: crate::resource::CATALOG.t("hex-editor-edit-mode"),
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
                self.inspector_entry
                    .entry(
                        "panel-right",
                        if open {
                            crate::resource::CATALOG.t("hex-editor-hide-inspector")
                        } else {
                            crate::resource::CATALOG.t("hex-editor-show-inspector")
                        },
                        (),
                    )
                    .on(open),
            ],
            ..WindowToolbar::default()
        }
    }

    /// Run what the person did in the bar, then declare the slots for what the view
    /// shows now.
    pub(super) fn run_window_toolbar(&mut self) {
        for event in ui::window_toolbar::take_events() {
            self.run_window_bar_event(&event);
        }
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    pub(crate) fn run_window_bar_event(&mut self, event: &ToolbarEvent) {
        if !self.has_file {
            return;
        }
        if self.inspector_entry.pressed(event).is_some() {
            self.toggle_inspector();
            return;
        }
        let ToolbarEvent::Action { id } = event else {
            return;
        };
        let want = match id.as_str() {
            OVERWRITE => EditMode::Overwrite,
            INSERT => EditMode::Insert,
            _ => return,
        };
        if self.model.mode() != want {
            self.model.toggle_mode();
            self.moved();
        }
    }
}

#[cfg(test)]
#[path = "window_bar_tests.rs"]
mod tests;

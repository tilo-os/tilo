//! The editor's controls as slots in the window's bar ([`ui::window_toolbar`]): the
//! bin's toggle, the project with Open and Export in its picker, and Split, Export,
//! the inspector's toggle and Import as actions.
//!
//! Every slot runs the handler of the editor's published action, so a press in the
//! bar, an assistive technology and an agent take one path.

use ui::window_toolbar::{
    LocationChoice, ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar,
};

#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;
use crate::actions::VideoEditorEffect;

/// What an entry of the window's bar runs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BarCommand {
    /// The handler of the editor's published action.
    Run(VideoEditorEffect),
    /// Two angles in one frame: the project's own edit, submitted the way
    /// an agent or the palette submits it; the bar is the gesture.
    PlaceSideBySide,
    /// The editor's own, asked of the host, so they have no published action; the bar's
    /// menu carries them while the transport strip is too narrow to.
    History(ui::UndoRequest),
}

/// What a row of the project picker commits.
const OPEN: &str = "project:open-file";
const EXPORT_FILE: &str = "project:export-file";

impl VideoEditorApp {
    /// The window bar's slots for what the editor shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let inspector_open = self.inspector_flap.is_open(self.inspector_split());
        let bar = &self.bar_entries;
        WindowToolbar {
            sidebar: Some(self.bin_flap.is_open(self.bin_layout())),
            location: Some(ToolbarLocation {
                label: self.project_label(),
                name: self.project_label(),
                choices: Self::project_choices(),
                ..ToolbarLocation::default()
            }),
            actions: {
                // In the order the published actions sit in; Undo and Redo join it only
                // while the strip has no room for its own pair.
                let mut actions = vec![
                    bar.entry(
                        "scissors",
                        crate::resource::CATALOG.t("video-editor-shortcut-split"),
                        BarCommand::Run(VideoEditorEffect::SplitSelection),
                    )
                    .enabled(self.can_split_selection()),
                    bar.entry(
                        "columns-2",
                        crate::resource::CATALOG.t("video-editor-side-by-side"),
                        BarCommand::PlaceSideBySide,
                    )
                    .enabled(self.can_place_side_by_side()),
                    self.export_action(),
                    bar.entry(
                        "panel-right",
                        if inspector_open {
                            crate::resource::CATALOG.t("video-editor-hide-inspector")
                        } else {
                            crate::resource::CATALOG.t("video-editor-show-inspector")
                        },
                        BarCommand::Run(VideoEditorEffect::ToggleInspector),
                    )
                    .on(inspector_open),
                    bar.entry(
                        "folder-open",
                        crate::resource::CATALOG.t("video-editor-shortcut-import-media"),
                        BarCommand::Run(VideoEditorEffect::Import { path: None }),
                    )
                    .in_menu(),
                ];
                actions.extend(self.history_off_the_strip());
                actions
            },
            ..WindowToolbar::default()
        }
    }

    /// No capability is lost to a width: off the strip they stand in the bar's overflow
    /// menu, named, and disabled exactly when the strip's pair would have been. They are
    /// menu-only, so a resize does not change the row.
    fn history_off_the_strip(&self) -> Vec<ToolbarAction> {
        if !self.history_buttons(&self.regions()).is_empty() {
            return Vec::new();
        }
        [
            (
                ui::UndoRequest::Undo,
                "undo-2",
                crate::resource::CATALOG.t("video-editor-shortcut-undo"),
            ),
            (
                ui::UndoRequest::Redo,
                "redo-2",
                crate::resource::CATALOG.t("video-editor-shortcut-redo"),
            ),
        ]
        .into_iter()
        .map(|(which, icon, label)| {
            self.bar_entries
                .entry(icon, label, BarCommand::History(which))
                .in_menu()
                .enabled(self.undo.can(which))
        })
        .collect()
    }

    /// The destination, a running render's progress and Cancel, and why a render failed. It
    /// reads as on while a render runs.
    fn export_action(&self) -> ToolbarAction {
        let export = BarCommand::Run(VideoEditorEffect::ShowExport);
        self.bar_entries
            .entry(
                "upload",
                crate::resource::CATALOG.t("video-editor-start-export"),
                export,
            )
            .on(matches!(self.export_status, ExportStatus::Rendering { .. }))
    }

    /// The project picker's rows: a project file in, and one out.
    fn project_choices() -> Vec<LocationChoice> {
        [
            (
                crate::resource::CATALOG.t("video-editor-open-project-file"),
                OPEN,
            ),
            (
                crate::resource::CATALOG.t("video-editor-export-project-file"),
                EXPORT_FILE,
            ),
        ]
        .into_iter()
        .map(|(label, value)| LocationChoice {
            label,
            value: value.to_owned(),
            current: false,
            separated: false,
        })
        .collect()
    }

    /// Whether Split would cut anything: an editable selected clip, or one
    /// linked to it, covers the playhead.
    fn can_split_selection(&self) -> bool {
        self.edit_targets().iter().any(|id| {
            self.project
                .clip(*id)
                .is_some_and(|clip| clip.covers(self.playhead))
        })
    }

    /// Two pictures meeting at the playhead, on tracks that may be edited, neither already
    /// beside the other.
    fn can_place_side_by_side(&self) -> bool {
        self.project.side_by_side_available(self.playhead)
    }

    /// Run what the person did in the window's bar, then declare the slots
    /// for what the editor shows now.
    pub(super) fn run_window_toolbar(&mut self) {
        for event in ui::window_toolbar::take_events() {
            self.run_window_bar_event(&event);
        }
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Anything but the media toggle first closes the bin's overlay, so nothing changes
    /// behind its scrim.
    pub(crate) fn run_window_bar_event(&mut self, event: &ToolbarEvent) {
        let ours = matches!(
            event,
            ToolbarEvent::Action { .. } | ToolbarEvent::Commit { .. }
        );
        if ours && self.bin_overlay_is_open() {
            self.close_bin_overlay();
        }
        let effect = match event {
            ToolbarEvent::ToggleSidebar => VideoEditorEffect::ToggleMedia,
            ToolbarEvent::Action { .. } => match self.bar_entries.pressed(event) {
                Some(BarCommand::Run(effect)) => effect,
                Some(BarCommand::PlaceSideBySide) => {
                    self.place_side_by_side();
                    return;
                }
                // The strip's own pair, when the strip is too narrow to
                // draw it: one request to the host, as a press there makes.
                Some(BarCommand::History(which)) => {
                    self.undo.request(which);
                    return;
                }
                None => return,
            },
            ToolbarEvent::Commit { text } => {
                // A project file is a job of the effect's own handler.
                let effect = match text.as_str() {
                    OPEN => VideoEditorEffect::OpenProjectFile { path: None },
                    EXPORT_FILE => VideoEditorEffect::ExportProjectFile { path: None },
                    _ => return,
                };
                self.invoke_effect(&effect);
                return;
            }
            _ => return,
        };
        // Everything one press edits is one undo step, as for a key.
        let _gesture = self
            .actions
            .as_ref()
            .map(ui::framework::Dispatcher::begin_group);
        if let Err(error) = self.perform_resource_operation(effect) {
            tracing::warn!(%error, "the window bar's action did not run");
        }
    }
}

#[cfg(test)]
impl VideoEditorApp {
    /// The bar's action named `name`, or pictured by the icon `name` (a
    /// toggle named for what a press does).
    pub(crate) fn bar_entry(&self, name: &str) -> Option<ToolbarAction> {
        self.window_toolbar()
            .actions
            .into_iter()
            .find(|action| action.name == name || action.icon == name)
    }

    /// Press the bar's action [`Self::bar_entry`] finds by `name`.
    pub(crate) fn press_bar(&mut self, name: &str) {
        let id = self
            .bar_entry(name)
            .unwrap_or_else(|| panic!("the bar has {name}"))
            .id;
        self.run_window_bar_event(&ToolbarEvent::Action { id });
    }
}

#[cfg(test)]
#[path = "window_bar_tests.rs"]
mod tests;

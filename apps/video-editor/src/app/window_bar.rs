//! The editor's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): the media bin's toggle as the sidebar, the
//! project as the location with Open, Save, Save As and the recent projects
//! in its picker, and Split, Export, the inspector's toggle and Import as
//! actions. The location says when the project has unsaved edits; a
//! running render's progress is in the export dialog, which Export opens.
//! The bar's History control is the window's; the editor draws no header
//! row.
//!
//! Every slot runs the handler of the editor's published action for it
//! ([`crate::actions::VideoEditorEffect`]), so a press in the bar, an
//! assistive technology and an agent take one path.

use ui::window_toolbar::{
    LocationChoice, ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar,
};

#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;
use crate::actions::VideoEditorEffect;

/// The bar actions, by the published action each runs.
pub(crate) const SPLIT: &str = "video.split";
/// Undo and Redo, as the bar carries them while the transport strip has no
/// room for its own pair. They are the window's history verbs, not the
/// editor's (`ui::UndoAccess` asks the host), so they have no published
/// action of their own to run.
pub(crate) const UNDO: &str = "video.undo";
pub(crate) const REDO: &str = "video.redo";
pub(crate) const EXPORT: &str = "video.export";
pub(crate) const INSPECTOR: &str = "video.inspector";
pub(crate) const IMPORT: &str = "video.import";

/// What a row of the project picker commits.
const OPEN: &str = "project:open";
const SAVE: &str = "project:save";
const SAVE_AS: &str = "project:save-as";
const RECENT: &str = "project:recent:";

impl VideoEditorApp {
    /// The window bar's slots for what the editor shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let inspector_open = self.inspector_flap.is_open(self.inspector_split());
        WindowToolbar {
            sidebar: Some(self.bin_flap.is_open(self.bin_layout())),
            location: Some(ToolbarLocation {
                label: self.project_label(),
                name: self.project_status_line(),
                edited: self.project_is_dirty(),
                choices: self.project_choices(),
                ..ToolbarLocation::default()
            }),
            actions: self.history_off_the_strip().into_iter().fold(
                vec![
                    ToolbarAction::new(SPLIT, "scissors", "Split at playhead")
                        .enabled(self.can_split_selection()),
                    self.export_action(),
                    ToolbarAction::new(
                        INSPECTOR,
                        "panel-right",
                        if inspector_open {
                            "Hide inspector"
                        } else {
                            "Show inspector"
                        },
                    )
                    .on(inspector_open),
                    ToolbarAction::new(IMPORT, "folder-open", "Import media").in_menu(),
                ],
                |mut actions, action| {
                    actions.push(action);
                    actions
                },
            ),
            ..WindowToolbar::default()
        }
    }

    /// Undo and Redo while the transport strip has no room to draw them.
    ///
    /// A narrow strip gives the buttons up before the playhead time, which
    /// nothing else on the strip carries — but no capability is lost to a
    /// width: off the strip they stand in the bar's overflow menu, named, and
    /// disabled exactly when the strip's pair would have been. They are
    /// menu-only, so the row of the bar is unchanged by a resize.
    fn history_off_the_strip(&self) -> Vec<ToolbarAction> {
        if !self.history_buttons(&self.regions()).is_empty() {
            return Vec::new();
        }
        [
            (UNDO, ui::UndoRequest::Undo, "undo-2", "Undo"),
            (REDO, ui::UndoRequest::Redo, "redo-2", "Redo"),
        ]
        .into_iter()
        .map(|(id, which, icon, label)| {
            ToolbarAction::new(id, icon, label)
                .in_menu()
                .enabled(self.undo.can(which))
        })
        .collect()
    }

    /// Export: it opens the export settings, where the destination, a
    /// running render's progress and Cancel, and the whole reason a render
    /// failed are. It reads as on while a render runs.
    fn export_action(&self) -> ToolbarAction {
        ToolbarAction::new(EXPORT, "upload", "Export")
            .on(matches!(self.export_status, ExportStatus::Rendering { .. }))
    }

    /// The project picker's rows: Open, Save and Save As, then the recent
    /// projects in a group of their own.
    fn project_choices(&self) -> Vec<LocationChoice> {
        let mut first_recent = true;
        self.project_menu_rows()
            .into_iter()
            .map(|(item, label)| {
                let (value, separated) = match item {
                    ProjectMenuItem::Open => (OPEN.to_owned(), false),
                    ProjectMenuItem::Save => (SAVE.to_owned(), false),
                    ProjectMenuItem::SaveAs => (SAVE_AS.to_owned(), false),
                    ProjectMenuItem::Recent(path) => (
                        format!("{RECENT}{}", path.display()),
                        std::mem::take(&mut first_recent),
                    ),
                };
                LocationChoice {
                    label,
                    value,
                    current: false,
                    separated,
                }
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

    /// Run what the person did in the window's bar, then declare the slots
    /// for what the editor shows now.
    pub(super) fn run_window_toolbar(&mut self) {
        for event in ui::window_toolbar::take_events() {
            self.run_window_bar_event(&event);
        }
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run one thing the person did in the window's bar, through the
    /// handler of the published action for it. Anything but the media
    /// toggle first closes the bin's overlay, so nothing changes behind its
    /// scrim.
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
            ToolbarEvent::Action { id } => match id.as_str() {
                // The strip's own pair, when the strip is too narrow to
                // draw it: one request to the host, as a press there makes.
                UNDO => {
                    self.undo.request(ui::UndoRequest::Undo);
                    return;
                }
                REDO => {
                    self.undo.request(ui::UndoRequest::Redo);
                    return;
                }
                SPLIT => VideoEditorEffect::SplitSelection,
                EXPORT => VideoEditorEffect::ShowExport,
                INSPECTOR => VideoEditorEffect::ToggleInspector,
                IMPORT => VideoEditorEffect::Import { path: None },
                _ => return,
            },
            ToolbarEvent::Commit { text } => match text.as_str() {
                OPEN => VideoEditorEffect::OpenProject { path: None },
                SAVE => VideoEditorEffect::Save { path: None },
                SAVE_AS => VideoEditorEffect::SaveAs { path: None },
                other => {
                    // A recent project opens as the published Open with its
                    // path does.
                    if let Some(path) = other.strip_prefix(RECENT)
                        && self.recents.iter().any(|recent| recent == Path::new(path))
                    {
                        self.begin_open(PathBuf::from(path), None);
                    }
                    return;
                }
            },
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
#[path = "window_bar_tests.rs"]
mod tests;

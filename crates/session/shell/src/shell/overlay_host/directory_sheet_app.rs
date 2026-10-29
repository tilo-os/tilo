//! "Join a workspace…" ([`tilo_sheets::directory`]) as the shell hosts it:
//! the shell reads the directories and joins, and keeps the app's list
//! current; the person's choices come back as requests it carries out.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

use tilo_sheets::directory::DirectorySeat;

use super::sheet_apps::{SheetHost, mount_sheet};

impl Default for SheetHost<DirectorySeat> {
    fn default() -> Self {
        Self::new(
            LayerId::Directory,
            LayerId::Directory.surface_kind(),
            tilo_sheets::directory::definition,
        )
    }
}

impl ShellOverlayHost {
    /// Open "Join a workspace…" and ask the directories for what they list.
    pub(in crate::shell) fn open_directory(&mut self) {
        let request = self.directory_sheet.open();
        tracing::info!("directory sheet opened: a link or code typed there joins");
        let room = self.sheet_room();
        let apps = self.sheet_apps();
        self.layers
            .directory
            .mount(&room, |definition, room, instance, seat| {
                mount_sheet(apps.as_ref(), definition, room, instance, seat)
            });
        self.layers.directory.seat().clear_query();
        self.layers.directory.open();
        self.run_directory_request(request);
        self.follow_directory();
    }

    /// Carry out what the sheet asked, and keep what it lists current;
    /// whether anything changed on screen.
    pub(super) fn pump_directory(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        let shown = self.layers.directory.update(doc, path);
        let asked = self.layers.directory.seat().take_asked();
        let acted = !asked.is_empty();
        for effect in asked {
            if let Some(request) = self.directory_sheet.answer(effect) {
                self.run_directory_request(request);
            }
        }
        self.follow_directory();
        shown | acted
    }

    /// Tell the app what to list now, or take it down once the sheet closed.
    pub(super) fn follow_directory(&self) {
        if self.directory_sheet.is_open() {
            let view = self.directory_sheet.view(tilo_clock::unix_now_ms());
            self.layers.directory.seat().show(view);
        } else if self.layers.directory.is_drawn() {
            self.layers.directory.close_now();
        }
    }

    /// While the sheet is up every event is its own: a link pasted into its
    /// search shares characters with chords.
    pub(super) fn route_directory(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.directory_sheet.is_open() {
            return false;
        }
        if self.layers.directory.is_drawn() {
            self.layers.directory.route(event, ctx);
        }
        *ctx.handled = true;
        true
    }
}

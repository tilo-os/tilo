//! A folder's History, and a file's: the tree's changes in the shared timeline, with Undo and
//! Restore on the row a person selects.
use super::{Column, Command, HistoryHost, HistorySourceRegistration, Row, Text};

impl HistoryHost {
    pub(super) fn close_folder_presentation(&mut self) {
        self.review = None;
        self.review_request = None;
        self.folder_selected = None;
        self.folder_status = None;
        self.selected.clear();
        self.before.preview = None;
        self.after.preview = None;
        self.timeline.set_selection(None);
    }

    /// The one row of the timeline a person selected, when they selected one.
    pub(super) fn timeline_folder_selection(&self) -> Option<crate::HistoryEntryId> {
        let (start, end) = self.timeline.selection()?;
        (start == end)
            .then(|| self.timeline.entries().get(start).map(|entry| entry.id))
            .flatten()
    }

    /// Read the folder's changes again when they moved, and follow the row a person selected.
    pub(super) fn refresh_folder(&mut self, mut changed: bool) {
        let HistorySourceRegistration::FolderFiles { source, .. } = self.source().clone() else {
            return;
        };
        if source.take_open() && !self.open {
            self.open = true;
            changed = true;
        }
        let revision = source.revision();
        if self.observed_folder_revision != Some(revision) {
            self.observed_folder_revision = Some(revision);
            self.timeline.set_entries(source.entries());
            changed = true;
        }
        let selected = self.timeline_folder_selection();
        if selected != self.folder_selected {
            self.folder_selected = selected;
            self.folder_status = None;
            changed = true;
        }
        if changed {
            self.rebuild_toolbar();
        }
    }

    /// Take back the selected row's change (`restore` false), or put its file back as it was
    /// right after it (`restore` true).
    pub(super) fn apply_folder_verb(&mut self, restore: bool) {
        let HistorySourceRegistration::FolderFiles { source, .. } = self.source().clone() else {
            return;
        };
        let Some(id) = self.folder_selected else {
            return;
        };
        let applied = if restore {
            source.restore(&id)
        } else {
            source.undo(&id)
        };
        self.folder_status = applied.err();
        // The change applied is a row of its own: the folder is read again.
        self.observed_folder_revision = None;
    }

    pub(super) fn folder_toolbar(&self, mut toolbar: Column) -> Column {
        let HistorySourceRegistration::FolderFiles { source, .. } = self.source() else {
            return toolbar;
        };
        let Some(id) = self.folder_selected.filter(|_| self.open) else {
            return toolbar;
        };
        let mut actions = Row::new().with_gap(8.0).push(Box::new(self.command_button(
            "history-folder-undo",
            "history-mode-restore-change",
            Command::FolderUndo,
        )));
        if source.restorable(&id) {
            actions = actions.push(Box::new(self.command_button(
                "history-folder-restore",
                "history-folder-restore",
                Command::FolderRestore,
            )));
        }
        toolbar = toolbar.push(Box::new(actions));
        if let Some(status) = &self.folder_status {
            toolbar = toolbar.push(Box::new(
                Text::new(status).with_live(crate::a11y::Politeness::Polite),
            ));
        }
        toolbar
    }
}

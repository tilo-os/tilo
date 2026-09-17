//! Checked Files uses the same retained chrome, timeline, canvases and dialog.
use super::*;
use crate::history_mode::{checked_files::FilePreviewInput, files_session::FileController};
use collab_types::{FileVersionId, files::CheckedHistoryOperationV1 as Operation};

impl HistoryHost {
    pub(super) fn close_files_presentation(&mut self) {
        if let Some(files) = &mut self.files {
            files.suspend();
        }
        self.review = None;
        self.review_request = None;
        self.requested_file_version = None;
        self.selected.clear();
        self.before.preview = None;
        self.after.preview = None;
        self.timeline.set_selection(None);
    }
    pub(super) fn refresh_files(&mut self, mut changed: bool) {
        let HistorySourceRegistration::CheckedFiles { source, .. } = self.source() else {
            return;
        };
        let source = source.clone();
        let request = source.take_open();
        if let Some(version) = request {
            self.open = true;
            self.requested_file_version = version;
            changed = true;
        }
        if self.open {
            if let Some(target) = source.target() {
                // Once open, the selected owner stays pinned until an explicit
                // new History request or a return-to-live/reopen transition.
                let switch = self.files.as_ref().is_none_or(|files| {
                    files.target.binding != target.binding
                        && (request.is_some() || self.selected.is_empty())
                });
                if switch {
                    self.close_files_presentation();
                    self.requested_file_version = request.flatten();
                    if let Some(old) = self.files.take() {
                        self.retired_files.push(old);
                    }
                    if let Some(index) = self
                        .retired_files
                        .iter()
                        .position(|old| old.target.binding == target.binding)
                    {
                        self.files = Some(self.retired_files.remove(index));
                    } else {
                        self.files = Some(FileController::new(target));
                    }
                    if let Some(files) = &mut self.files {
                        files.activate();
                    }
                    changed = true;
                } else if request.is_some()
                    || self.files.as_ref().is_some_and(|files| !files.is_active())
                {
                    if let Some(files) = &mut self.files {
                        files.activate();
                    }
                }
            } else if self.files.is_none() {
                self.unavailable = Text::new("Select a file with versioned history");
            }
        }
        for old in &mut self.retired_files {
            old.drain();
        }
        // Files selects one immutable version. A drag/shift range in the shared
        // timeline must not imply a synthetic grouped file snapshot.
        if let Some((start, end)) = self.timeline.selection() {
            if start != end {
                self.timeline.set_selection(Some((start, start)));
                changed = true;
            }
        }
        let selected = self
            .timeline
            .selection()
            .and_then(|(start, _)| self.timeline.entries().get(start))
            .map(|entry| FileVersionId(uuid::Uuid::from_bytes(entry.id.0)));
        let Some(files) = &mut self.files else {
            if changed {
                self.rebuild_toolbar();
            }
            return;
        };
        changed |= files.drain();
        if let Some(requested) = self.requested_file_version.take().or(selected) {
            if Some(requested) != files.selected() {
                files.select(requested, None);
                self.selected = vec![crate::HistoryEntryId(*requested.0.as_bytes())];
                self.activity_page = false;
                self.side = PreviewSide::After;
                self.review = None;
                self.review_request = None;
                changed = true;
            }
        }
        self.timeline.set_entries(
            files
                .versions
                .iter()
                .map(|version| {
                    let person = version.actor.person.as_bytes();
                    let identity: String =
                        person.iter().take(6).map(|b| format!("{b:02x}")).collect();
                    let author = if person.iter().all(|b| *b == 0) {
                        "Unknown author".to_owned()
                    } else {
                        format!("Author {identity}")
                    };
                    crate::HistoryEntry {
                        id: crate::HistoryEntryId(*version.id.0.as_bytes()),
                        author: crate::HistoryAuthor::new(
                            author,
                            crate::history_source::stable_hue(&identity),
                            identity,
                        ),
                        op_class: collab_types::OpClass::Compaction,
                        artifact_kind: collab_types::ArtifactKind::File,
                        timestamp: std::time::UNIX_EPOCH
                            .checked_add(std::time::Duration::from_millis(version.timestamp_ms))
                            .unwrap_or(std::time::UNIX_EPOCH),
                        summary: format!(
                            "{} · {}",
                            version_label(&version.provenance),
                            files.target.label
                        ),
                        text_delta: None,
                        diff_payload: None,
                        redaction: None,
                        context: None,
                    }
                })
                .collect(),
        );
        if files.preview_changed {
            files.preview_changed = false;
            changed = true;
            let create = match self
                .registration
                .definition
                .scopes
                .get(self.active_scope)
                .map(|scope| &scope.presentation)
            {
                Some(HistoryPresentation::CheckedFiles { create }) => *create,
                _ => return,
            };
            self.after.install(files.after.clone().map_or_else(
                || Err(files.message.clone()),
                |snapshot| {
                    create(FilePreviewInput {
                        snapshot,
                        initial_view: PreviewView::default(),
                    })
                },
            ));
            self.before.install(files.before.clone().map_or_else(
                || {
                    Err(
                        if files
                            .after
                            .as_ref()
                            .is_some_and(|s| s.version.parents.len() > 1)
                        {
                            "Choose an actual parent revision for Before".into()
                        } else {
                            "No earlier version".into()
                        },
                    )
                },
                |snapshot| {
                    create(FilePreviewInput {
                        snapshot,
                        initial_view: PreviewView::default(),
                    })
                },
            ));
        }
        let dialog = if files.review_changed && self.review.is_some() {
            files.review_changed = false;
            Some(format!(
                "File: {}\nVersion: {}\n\n{}",
                files.target.label,
                files
                    .selected()
                    .map_or_else(|| "No selection".into(), |id| id.0.to_string()),
                files.message
            ))
        } else {
            None
        };
        if let Some(message) = dialog {
            self.show_review(None, message);
        }
        if changed {
            self.rebuild_toolbar();
        }
    }
    pub(super) fn service_file_review(&mut self, command: review::ReviewCommand) {
        let Some(files) = &mut self.files else {
            return;
        };
        match command {
            review::ReviewCommand::Open | review::ReviewCommand::Refresh => {
                let Some(version_id) = files.selected() else {
                    return;
                };
                let operation = if matches!(command, review::ReviewCommand::Refresh) {
                    files
                        .review_operation
                        .unwrap_or(Operation::Restore { version_id })
                } else {
                    Operation::Restore { version_id }
                };
                files.request_review(operation);
            }
            review::ReviewCommand::FileConfirm(token) => {
                if token != files.confirmation_token() || files.review.is_none() {
                    return;
                }
                files.confirm(token);
            }
            review::ReviewCommand::Confirm => return,
            review::ReviewCommand::Cancel => {
                files.cancel_review();
                self.review = None;
                return;
            }
        }
        let message = format!("File: {}\n\n{}", files.target.label, files.message);
        self.show_review(None, message);
    }
    pub(super) fn review_file_compensation(&mut self, redo: bool) {
        let Some(files) = &mut self.files else {
            return;
        };
        let Some(receipt) = files.restore.receipts.last() else {
            return;
        };
        let operation = if redo {
            Operation::Redo {
                undo_intent: receipt.intent_id,
            }
        } else {
            Operation::Undo {
                restore_intent: receipt.intent_id,
            }
        };
        files.request_review(operation);
        let message = format!(
            "File: {}\n\nChecking a fresh compensation review",
            files.target.label
        );
        self.show_review(None, message);
    }
    pub(super) fn files_toolbar(&self, mut toolbar: Column) -> Column {
        let Some(files) = &self.files else {
            return toolbar;
        };
        if !self.open {
            return toolbar;
        }
        let mut actions = Row::new().push(Box::new(self.command_button(
            "history-files-refresh",
            "Refresh activity",
            Command::FileRefresh,
        )));
        if files.restore.request.is_some() {
            actions = actions
                .push(Box::new(self.command_button(
                    "history-files-result",
                    "Check restore result",
                    Command::FileCheck,
                )))
                .push(Box::new(self.command_button(
                    "history-files-retry",
                    "Retry same restore",
                    Command::FileRetry,
                )));
        }
        if !files.restore.receipts.is_empty() {
            let redo = matches!(files.restore.last_operation, Some(Operation::Undo { .. }));
            actions = actions.push(Box::new(self.command_button(
                if redo {
                    "history-files-redo"
                } else {
                    "history-files-undo"
                },
                if redo {
                    "Redo file restore…"
                } else {
                    "Undo file restore…"
                },
                if redo {
                    Command::FileRedo
                } else {
                    Command::FileUndo
                },
            )));
        }
        toolbar = toolbar.push(Box::new(actions)).push(Box::new(
            Text::new(&files.message).with_live(crate::a11y::Politeness::Polite),
        ));
        toolbar = toolbar.push(Box::new(Text::new(&files.current_revision.map_or_else(
            || "Current file revision pointer unavailable".into(),
            |revision| format!("Current file revision: {}", revision.version_id.0),
        ))));
        if files.live_updates {
            toolbar = toolbar.push(Box::new(Text::new(
                "Live file has updates; selected immutable version remains pinned",
            )));
        }
        if let Some(snapshot) = &files.after {
            if snapshot.version.parents.len() > 1 {
                let mut parents = Row::new();
                for id in &snapshot.version.parents {
                    parents = parents.push(Box::new(self.command_button(
                        &format!("history-parent-{}", id.0),
                        &format!("Before parent {}", id.0),
                        Command::FileParent(*id),
                    )));
                }
                toolbar = toolbar.push(Box::new(parents));
            }
            if let Some(before) = &files.before {
                toolbar = toolbar.push(Box::new(Text::new(&format!(
                    "Before parent: {}",
                    before.version.id.0
                ))));
            }
        }
        toolbar
    }
}
fn version_label(provenance: &collab_types::FileProvenance) -> &'static str {
    match provenance {
        collab_types::FileProvenance::Create => "Created",
        collab_types::FileProvenance::Edit => "Edited",
        collab_types::FileProvenance::Rename => "Renamed",
        collab_types::FileProvenance::Move => "Moved",
        collab_types::FileProvenance::Delete => "Deleted",
        collab_types::FileProvenance::Restore { .. } => "Restored",
        collab_types::FileProvenance::ConflictResolution => "Conflict resolved",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_host() -> HistoryHost {
        fn preview(
            _: FilePreviewInput,
        ) -> Result<Box<dyn crate::history_mode::HistoryPreview>, String> {
            Err("no bytes requested by this selection test".into())
        }
        static DEFINITION: crate::history_mode::HistoryDefinition =
            crate::history_mode::HistoryDefinition {
                app_id: "test.files",
                scopes: &[crate::history_mode::HistoryScopeDefinition {
                    key: "file",
                    title: "File",
                    content_schema: None,
                    presentation: HistoryPresentation::CheckedFiles { create: preview },
                }],
            };
        let registration = HistoryRegistration::new(
            &DEFINITION,
            vec![HistorySourceRegistration::CheckedFiles {
                scope: "file",
                source: Default::default(),
            }],
        )
        .unwrap();
        HistoryHost::new(registration, &AutoCommit::new()).unwrap()
    }
    #[test]
    fn files_range_selection_is_one_immutable_version() {
        let mut host = test_host();
        host.timeline.set_entries(
            (1..=2)
                .map(|id| crate::HistoryEntry {
                    id: crate::HistoryEntryId(*uuid::Uuid::from_u128(id).as_bytes()),
                    author: crate::HistoryAuthor::new("Unknown author", 0.0, "unknown"),
                    op_class: collab_types::OpClass::Compaction,
                    artifact_kind: collab_types::ArtifactKind::File,
                    timestamp: std::time::UNIX_EPOCH,
                    summary: "File revision".into(),
                    text_delta: None,
                    diff_payload: None,
                    redaction: None,
                    context: None,
                })
                .collect(),
        );
        host.timeline.set_selection(Some((0, 1)));
        host.refresh_files(false);
        assert_eq!(
            host.timeline.selection(),
            Some((0, 0)),
            "shared range interaction must never advertise a grouped file snapshot"
        );
    }
    #[test]
    fn files_confirmation_queued_after_same_frame_cancel_cannot_submit() {
        use crate::history_mode::checked_files::{FileHistoryTarget, FileHistoryTransport};
        use collab_types::files::*;
        struct Transport;
        impl FileHistoryTransport for Transport {
            fn read(
                &self,
                _: FileHistoryReadQueryV1,
            ) -> Result<FileHistoryReadValueV1, FileHistoryReadErrorV1> {
                Err(FileHistoryReadErrorV1::Unavailable)
            }
            fn preview(
                &self,
                _: FileHistoryReadScopeV1,
                _: FileVersionId,
            ) -> Result<Vec<u8>, FileHistoryReadErrorV1> {
                Err(FileHistoryReadErrorV1::Unavailable)
            }
            fn review(&self) -> Result<FileRestoreReviewV1, CheckedHistoryOutcomeV1> {
                Err(CheckedHistoryOutcomeV1::Unavailable)
            }
            fn submit(&self, _: CheckedHistoryRequestV1) -> CheckedHistoryOutcomeV1 {
                panic!("cancelled confirmation must never submit")
            }
            fn result(&self, _: uuid::Uuid) -> CheckedHistoryOutcomeV1 {
                CheckedHistoryOutcomeV1::Unavailable
            }
        }
        let mut host = test_host();
        let record = collab_types::FileRecordId(uuid::Uuid::from_u128(3));
        let mut files = FileController::new(FileHistoryTarget {
            binding: uuid::Uuid::new_v4(),
            record,
            label: "file".into(),
            transport: std::sync::Arc::new(Transport),
        });
        files.suspend();
        files.review = Some((
            FileRestoreReviewV1 {
                schema_version: 1,
                workspace_id: uuid::Uuid::from_u128(1),
                mount_lifetime: uuid::Uuid::from_u128(2),
                record_id: record,
                frontier: [4; 32],
            },
            Operation::Undo {
                restore_intent: uuid::Uuid::from_u128(5),
            },
        ));
        let token = files.confirmation_token();
        host.files = Some(files);
        host.commands
            .borrow_mut()
            .push_back(Command::Review(review::ReviewCommand::Cancel));
        host.commands
            .borrow_mut()
            .push_back(Command::Review(review::ReviewCommand::FileConfirm(token)));
        host.refresh(&AutoCommit::new());
        if let Some(command) = host.review_request.take() {
            host.service_file_review(command);
        }
        assert!(host.files.as_ref().unwrap().restore.request.is_none());
        assert!(host.files.as_ref().unwrap().review.is_none());
        assert!(
            host.review.is_none(),
            "a stale confirmation cannot reopen the cancelled dialog"
        );
    }
}

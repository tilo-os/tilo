//! Checked Files uses the same retained chrome, timeline, canvases and dialog.
use super::{
    Column, Command, HistoryHost, HistoryPresentation, HistorySourceRegistration, PreviewSide,
    PreviewView, Row, Text, review,
};
use crate::history_mode::{checked_files::FilePreviewInput, files_session::FileController};
use crate::i18n::I18nManager;
use fs_protocol::{FileVersionId, files::CheckedHistoryOperationV1 as Operation};

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
    pub(super) fn reconcile_file_target(&mut self) -> bool {
        let HistorySourceRegistration::CheckedFiles { source, .. } = self.source() else {
            return false;
        };
        let target = source.target();
        let binding = target.as_ref().map(|target| target.binding);
        if binding == self.observed_file_binding {
            return false;
        }
        self.close_files_presentation();
        if let Some(old) = self.files.take() {
            self.retired_files.push(old);
        }
        self.observed_file_binding = binding;
        self.file_target_resolved = false;
        self.timeline.set_entries(vec![]);
        self.unavailable =
            crate::widget::EmptyState::new(I18nManager::lookup_plain("history-files-select-file"))
                .with_icon(crate::icons::Icon::History);
        if self.open
            && let Some(target) = target
        {
            let mut files = FileController::new(target);
            files.activate();
            self.files = Some(files);
        }
        true
    }
    pub(super) fn refresh_files(&mut self, mut changed: bool) {
        let HistorySourceRegistration::CheckedFiles { source, .. } = self.source() else {
            return;
        };
        let source = source.clone();
        changed |= self.reconcile_file_target();
        let request = source.take_open();
        if let Some(request) = request {
            self.open = true;
            self.requested_file_version = request.version;
            changed = true;
        }
        if self.open {
            if let Some(target) = source.target() {
                if self.files.is_none() {
                    self.files = Some(FileController::new(target));
                    if let Some(files) = &mut self.files {
                        files.activate();
                    }
                    changed = true;
                } else if self.files.as_ref().is_some_and(|files| !files.is_active()) {
                    // Reopening an already active owner is idempotent. Its exact
                    // immutable selection stays installed; only a different
                    // requested version below starts a new selection lifetime.
                    if let Some(files) = &mut self.files {
                        files.activate();
                    }
                }
            } else if self.files.is_none() {
                self.unavailable = crate::widget::EmptyState::new(I18nManager::lookup_plain(
                    "history-files-select-file",
                ))
                .with_icon(crate::icons::Icon::History);
            }
        }
        for old in &mut self.retired_files {
            old.drain();
        }
        // Files selects one immutable version. A drag/shift range in the shared
        // timeline must not imply a synthetic grouped file snapshot.
        if let Some((start, end)) = self.timeline.selection()
            && start != end
        {
            self.timeline.set_selection(Some((start, start)));
            changed = true;
        }
        let mut selected = self
            .timeline
            .selection()
            .and_then(|(start, _)| self.timeline.entries().get(start))
            .map(|entry| FileVersionId(uuid::Uuid::from_bytes(entry.id.0)));
        if let Some(files) = &mut self.files {
            changed |= files.drain();
        }
        if !self.file_target_resolved
            && let Some(identity) = self
                .files
                .as_ref()
                .and_then(FileController::resource_identity)
        {
            // The new descriptor first establishes current authorized identity.
            // Reuse the old session, never transplant its request/receipts onto
            // this fresh descriptor. Paths and transient binding IDs do not match.
            if let Some(index) = self
                .retired_files
                .iter()
                .position(|old| old.resource_identity() == Some(identity))
            {
                let mut retained = self.retired_files.remove(index);
                let requested = self
                    .requested_file_version
                    .or_else(|| self.files.as_ref().and_then(FileController::selected));
                if let Some(fresh) = &self.files {
                    retained.target.label.clone_from(&fresh.target.label);
                    retained.target.authors.clone_from(&fresh.target.authors);
                }
                self.close_files_presentation();
                retained.activate();
                self.files = Some(retained);
                self.requested_file_version = requested;
                selected = None;
                changed = true;
            }
            self.file_target_resolved = true;
        }
        self.refresh_file_selection(selected, changed);
    }

    fn refresh_file_selection(&mut self, mut selected: Option<FileVersionId>, mut changed: bool) {
        let Some(files) = &mut self.files else {
            if changed {
                self.rebuild_toolbar();
            }
            return;
        };
        if files.presentation_invalidated {
            files.presentation_invalidated = false;
            self.selected.clear();
            self.timeline.set_selection(None);
            selected = None;
            self.requested_file_version = None;
            self.review = None;
            self.review_request = None;
        }
        if let Some(requested) = self.requested_file_version.take().or(selected)
            && Some(requested) != files.selected()
        {
            files.select(requested, None);
            self.selected = vec![crate::HistoryEntryId(*requested.0.as_bytes())];
            self.activity_page = false;
            self.side = PreviewSide::After;
            self.review = None;
            self.review_request = None;
            changed = true;
        }
        self.timeline.set_entries(file_timeline_entries(files));
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
                            I18nManager::lookup_plain("history-files-choose-parent")
                        } else {
                            I18nManager::lookup_plain("history-files-no-earlier")
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
        // Root callbacks can change selection after refresh but before the
        // runner services a queued dialog command. Recheck at this final door.
        if self.reconcile_file_target() {
            self.rebuild_toolbar();
            return;
        }
        if !self.file_target_resolved {
            return;
        }
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
        if !self.file_target_resolved {
            return;
        }
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
        let message = I18nManager::format(
            "history-files-checking-compensation",
            &[("file", files.target.label.as_str().into())],
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
            "history-files-refresh-activity",
            Command::FileRefresh,
        )));
        if files.restore.request.is_some() {
            actions = actions
                .push(Box::new(self.command_button(
                    "history-files-result",
                    "history-files-check-restore",
                    Command::FileCheck,
                )))
                .push(Box::new(self.command_button(
                    "history-files-retry",
                    "history-files-retry-restore",
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
                    "history-files-redo-restore"
                } else {
                    "history-files-undo-restore"
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
            || I18nManager::lookup_plain("history-files-current-unavailable"),
            |revision| {
                I18nManager::format(
                    "history-files-current-revision",
                    &[("revision", revision.version_id.0.to_string().into())],
                )
            },
        ))));
        if files.live_updates {
            toolbar = toolbar.push(Box::new(Text::new("history-files-live-updates")));
        }
        if let Some(snapshot) = &files.after {
            if snapshot.version.parents.len() > 1 {
                let mut parents = Row::new();
                for id in &snapshot.version.parents {
                    parents = parents.push(Box::new(self.command_button(
                        &format!("history-parent-{}", id.0),
                        &I18nManager::format(
                            "history-files-before-parent",
                            &[("revision", id.0.to_string().into())],
                        ),
                        Command::FileParent(*id),
                    )));
                }
                toolbar = toolbar.push(Box::new(parents));
            }
            if let Some(before) = &files.before {
                toolbar = toolbar.push(Box::new(Text::new(&I18nManager::format(
                    "history-files-showing-parent",
                    &[("revision", before.version.id.0.to_string().into())],
                ))));
            }
        }
        toolbar
    }
}
fn version_label(provenance: &fs_protocol::FileProvenance) -> String {
    I18nManager::lookup_plain(match provenance {
        fs_protocol::FileProvenance::Create => "history-files-created",
        fs_protocol::FileProvenance::Edit => "history-files-edited",
        fs_protocol::FileProvenance::Rename => "history-files-renamed",
        fs_protocol::FileProvenance::Move => "history-files-moved",
        fs_protocol::FileProvenance::Delete => "history-files-deleted",
        fs_protocol::FileProvenance::Restore { .. } => "history-files-restored",
        fs_protocol::FileProvenance::ConflictResolution => "history-files-conflict-resolved",
    })
}

fn file_timeline_entries(files: &FileController) -> Vec<crate::HistoryEntry> {
    files
        .versions
        .iter()
        .map(|version| {
            let person = version.actor.person.as_bytes();
            // The device that signed the version names who wrote it: an
            // agent's own device reads as the agent, on behalf of its person.
            let known = files
                .target
                .authors
                .resolve_file_actor(person, version.actor.device.as_bytes());
            let author = known.unwrap_or_else(|| {
                let identity = hex::encode(person.get(..6).unwrap_or(person));
                let name = if person.iter().all(|b| *b == 0) {
                    I18nManager::lookup_plain("history-files-unknown-author")
                } else {
                    I18nManager::format(
                        "history-files-author",
                        &[("identity", identity.as_str().into())],
                    )
                };
                crate::history_source::author_coloured(
                    name,
                    identity.clone(),
                    crate::presence::user_color(&identity),
                )
            });
            crate::HistoryEntry {
                id: crate::HistoryEntryId(*version.id.0.as_bytes()),
                author,
                entry_kind: crate::widget::EntryKind::Compaction,
                artifact_kind: collab_types::ArtifactKind::File,
                timestamp: crate::time::UNIX_EPOCH
                    .checked_add(crate::time::Duration::from_millis(version.timestamp_ms))
                    .unwrap_or(crate::time::UNIX_EPOCH),
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
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HANG_BOUND;
    use crate::history_mode::HistoryRegistration;
    use automerge::AutoCommit;

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
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn files_selection_and_queued_confirmation_follow_the_current_target() {
        // phase: files_range_selection_is_one_immutable_version
        {
            let mut host = test_host();
            host.timeline.set_entries(
                (1..=2)
                    .map(|id| crate::HistoryEntry {
                        id: crate::HistoryEntryId(*uuid::Uuid::from_u128(id).as_bytes()),
                        author: crate::HistoryAuthor::new("Unknown author", 0.0, "unknown"),
                        entry_kind: crate::widget::EntryKind::Compaction,
                        artifact_kind: collab_types::ArtifactKind::File,
                        timestamp: crate::time::UNIX_EPOCH,
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
        // phase: files_confirmation_queued_after_same_frame_cancel_cannot_submit
        {
            use crate::history_mode::checked_files::{FileHistoryTarget, FileHistoryTransport};
            use fs_protocol::files::*;
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
            let record = fs_protocol::FileRecordId(uuid::Uuid::from_u128(3));
            let mut files = FileController::new(FileHistoryTarget {
                binding: uuid::Uuid::new_v4(),
                record,
                label: "file".into(),
                transport: std::sync::Arc::new(Transport),
                authors: crate::history_source::AuthorDirectory::new(),
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
        // phase: round1_root_target_replacement_or_removal_invalidates_queued_confirmation
        {
            use crate::history_mode::checked_files::{FileHistoryTarget, FileHistoryTransport};
            use fs_protocol::files::*;
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
                    CheckedHistoryOutcomeV1::Unavailable
                }
                fn result(&self, _: uuid::Uuid) -> CheckedHistoryOutcomeV1 {
                    CheckedHistoryOutcomeV1::Unavailable
                }
            }
            for (remove, late) in [(false, false), (true, false), (false, true), (true, true)] {
                let mut host = test_host();
                let HistorySourceRegistration::CheckedFiles { source, .. } = host.source() else {
                    panic!()
                };
                let source = source.clone();
                let record = fs_protocol::FileRecordId(uuid::Uuid::from_u128(3));
                let target = FileHistoryTarget {
                    binding: uuid::Uuid::new_v4(),
                    record,
                    label: "A".into(),
                    transport: std::sync::Arc::new(Transport),
                    authors: crate::history_source::AuthorDirectory::new(),
                };
                source.set_target(Some(target.clone()));
                host.open = true;
                host.refresh(&AutoCommit::new());
                host.file_target_resolved = true;
                let files = host.files.as_mut().unwrap();
                files.review = Some((
                    FileRestoreReviewV1 {
                        schema_version: 1,
                        workspace_id: uuid::Uuid::from_u128(1),
                        mount_lifetime: uuid::Uuid::from_u128(2),
                        record_id: record,
                        frontier: [4; 32],
                    },
                    Operation::Restore {
                        version_id: FileVersionId(uuid::Uuid::from_u128(8)),
                    },
                ));
                let nonce = files.confirmation_token();
                host.selected = vec![crate::HistoryEntryId(*uuid::Uuid::from_u128(8).as_bytes())];
                let replacement = if remove {
                    None
                } else {
                    Some(FileHistoryTarget {
                        binding: uuid::Uuid::new_v4(),
                        label: "B".into(),
                        ..target.clone()
                    })
                };
                if !late {
                    source.set_target(replacement.clone());
                }
                host.commands
                    .borrow_mut()
                    .push_back(Command::Review(review::ReviewCommand::FileConfirm(nonce)));
                host.refresh(&AutoCommit::new());
                if late {
                    source.set_target(replacement);
                }
                if let Some(command) = host.review_request.take() {
                    host.service_file_review(command);
                }
                assert!(
                    host.selected.is_empty(),
                    "resource replacement/removal must clear the pinned presentation"
                );
                assert!(host.review.is_none());
                assert!(
                    host.files
                        .as_ref()
                        .is_none_or(|files| files.restore.request.is_none())
                );
                let old = host
                    .retired_files
                    .iter()
                    .find(|files| files.target.binding == target.binding)
                    .unwrap();
                assert!(old.restore.request.is_none());
                assert!(old.review.is_none());
                assert_ne!(old.confirmation_token(), nonce);
            }
        }
    }

    #[test]
    fn round1_resource_recovery_keeps_original_transport_and_rejects_wrong_lifetime() {
        use crate::history_mode::checked_files::{FileHistoryTarget, FileHistoryTransport};
        use fs_protocol::files::*;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct Transport {
            scope: FileHistoryReadScopeV1,
            results: AtomicUsize,
            reviews: AtomicUsize,
        }
        impl FileHistoryTransport for Transport {
            fn read(
                &self,
                _: FileHistoryReadQueryV1,
            ) -> Result<FileHistoryReadValueV1, FileHistoryReadErrorV1> {
                Ok(FileHistoryReadValueV1::Page(FileHistoryPageV1 {
                    scope: self.scope,
                    versions: vec![],
                    total_versions: 0,
                    heads: vec![],
                    current_revision: None,
                    next_after: None,
                }))
            }
            fn preview(
                &self,
                _: FileHistoryReadScopeV1,
                _: FileVersionId,
            ) -> Result<Vec<u8>, FileHistoryReadErrorV1> {
                Err(FileHistoryReadErrorV1::Unavailable)
            }
            fn review(&self) -> Result<FileRestoreReviewV1, CheckedHistoryOutcomeV1> {
                self.reviews.fetch_add(1, Ordering::SeqCst);
                Err(CheckedHistoryOutcomeV1::ReadOnly)
            }
            fn submit(&self, _: CheckedHistoryRequestV1) -> CheckedHistoryOutcomeV1 {
                CheckedHistoryOutcomeV1::Unavailable
            }
            fn result(&self, intent: uuid::Uuid) -> CheckedHistoryOutcomeV1 {
                self.results.fetch_add(1, Ordering::SeqCst);
                CheckedHistoryOutcomeV1::RecoveryPending { intent_id: intent }
            }
        }
        fn settle(host: &mut HistoryHost) {
            let end = crate::time::now() + HANG_BOUND;
            while !host.file_target_resolved && crate::time::now() < end {
                host.refresh(&AutoCommit::new());
                std::thread::sleep(crate::time::Duration::from_millis(1));
            }
            assert!(host.file_target_resolved);
        }
        let scope = FileHistoryReadScopeV1 {
            workspace_id: uuid::Uuid::from_u128(1),
            mount_lifetime: uuid::Uuid::from_u128(2),
            record_id: fs_protocol::FileRecordId(uuid::Uuid::from_u128(3)),
            frontier: [4; 32],
        };
        let original = Arc::new(Transport {
            scope,
            results: AtomicUsize::new(0),
            reviews: AtomicUsize::new(0),
        });
        let original_handle: Arc<dyn FileHistoryTransport> = original.clone();
        let target = FileHistoryTarget {
            binding: uuid::Uuid::new_v4(),
            record: scope.record_id,
            label: "same display path".into(),
            transport: original_handle.clone(),
            authors: crate::history_source::AuthorDirectory::new(),
        };
        let mut host = test_host();
        let HistorySourceRegistration::CheckedFiles { source, .. } = host.source() else {
            panic!()
        };
        let source = source.clone();
        source.set_target(Some(target.clone()));
        host.open = true;
        host.refresh(&AutoCommit::new());
        settle(&mut host);
        let selected = FileVersionId(uuid::Uuid::from_u128(8));
        host.files.as_mut().unwrap().select(selected, None);
        let active_nonce = host.files.as_ref().unwrap().confirmation_token();
        source.open(Some(selected));
        host.refresh(&AutoCommit::new());
        assert_eq!(host.files.as_ref().unwrap().selected(), Some(selected));
        assert_eq!(
            host.files.as_ref().unwrap().confirmation_token(),
            active_nonce,
            "repeated open of an active exact selection must not restart its presentation lifetime"
        );
        let review = FileRestoreReviewV1 {
            schema_version: 1,
            workspace_id: scope.workspace_id,
            mount_lifetime: scope.mount_lifetime,
            record_id: scope.record_id,
            frontier: scope.frontier,
        };
        let request = host
            .files
            .as_mut()
            .unwrap()
            .restore
            .prepare(
                review,
                Operation::Restore {
                    version_id: FileVersionId(uuid::Uuid::from_u128(8)),
                },
            )
            .unwrap();
        assert!(host.files.as_mut().unwrap().restore.receive(
            request,
            CheckedHistoryOutcomeV1::RecoveryPending {
                intent_id: request.intent_id
            }
        ));
        source.set_target(None);
        host.refresh(&AutoCommit::new());
        assert!(host.files.is_none());
        let fresh = Arc::new(Transport {
            scope,
            results: AtomicUsize::new(0),
            reviews: AtomicUsize::new(0),
        });
        let fresh_binding = uuid::Uuid::new_v4();
        source.set_target(Some(FileHistoryTarget {
            binding: fresh_binding,
            record: scope.record_id,
            label: "same display path".into(),
            transport: fresh.clone(),
            authors: crate::history_source::AuthorDirectory::new(),
        }));
        host.refresh(&AutoCommit::new());
        settle(&mut host);
        assert_eq!(host.observed_file_binding, Some(fresh_binding));
        for _ in 0..3 {
            host.refresh(&AutoCommit::new());
            assert_eq!(host.files.as_ref().unwrap().target.binding, target.binding);
        }
        let files = host.files.as_mut().unwrap();
        assert!(Arc::ptr_eq(&files.target.transport, &original_handle));
        assert_eq!(files.restore.request, Some(request));
        assert!(files.restore.prepare(review, request.operation).is_err());
        files.check_result();
        let end = crate::time::now() + HANG_BOUND;
        while original.results.load(Ordering::SeqCst) == 0 && crate::time::now() < end {
            std::thread::sleep(crate::time::Duration::from_millis(1));
        }
        assert_eq!(original.results.load(Ordering::SeqCst), 1);
        assert_eq!(fresh.results.load(Ordering::SeqCst), 0);
        assert_eq!(
            original.reviews.load(Ordering::SeqCst) + fresh.reviews.load(Ordering::SeqCst),
            0,
            "authorized read identity recovery must not require writable review"
        );
        let mut remounted = scope;
        remounted.mount_lifetime = uuid::Uuid::new_v4();
        let replacement = FileHistoryTarget {
            binding: uuid::Uuid::new_v4(),
            record: scope.record_id,
            label: "same display path".into(),
            transport: Arc::new(Transport {
                scope: remounted,
                results: AtomicUsize::new(0),
                reviews: AtomicUsize::new(0),
            }),
            authors: crate::history_source::AuthorDirectory::new(),
        };
        source.set_target(Some(replacement.clone()));
        host.refresh(&AutoCommit::new());
        settle(&mut host);
        assert_eq!(
            host.files.as_ref().unwrap().target.binding,
            replacement.binding
        );
        assert!(host.files.as_ref().unwrap().restore.request.is_none());
        assert!(
            host.retired_files
                .iter()
                .any(|old| old.restore.request == Some(request)
                    && Arc::ptr_eq(&old.target.transport, &original_handle))
        );
    }

    /// A file version an agent's own device signed reads as the agent's, on
    /// behalf of its person, in the file history rows; its person's own
    /// devices read as the person, and a stranger still reads by key.
    #[test]
    fn file_rows_attribute_an_agents_version_to_the_agent_for_its_person() {
        use crate::history_mode::checked_files::{FileHistoryTarget, FileHistoryTransport};
        use crate::history_source::{AuthorDirectory, HistoryAgent};
        use crate::widget::history::{AuthorRelation, HistoryAttribution};
        use fs_protocol::files::*;
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
                CheckedHistoryOutcomeV1::Unavailable
            }
            fn result(&self, _: uuid::Uuid) -> CheckedHistoryOutcomeV1 {
                CheckedHistoryOutcomeV1::Unavailable
            }
        }
        let (ada, ada_laptop, scribe, stranger) = ([1; 32], [2; 32], [3; 32], [4; 32]);
        let mut authors = AuthorDirectory::new();
        authors.register_person(
            &hex::encode(ada),
            crate::HistoryAuthor::new("Ada", 0.0, "ada00000"),
        );
        authors.register_agent(
            &hex::encode(scribe),
            HistoryAgent {
                principal: "agent:scribe".into(),
                label: "Scribe".into(),
                rgb: (10, 20, 30),
                person: hex::encode(ada),
            },
        );
        let record = fs_protocol::FileRecordId(uuid::Uuid::from_u128(3));
        let mut files = FileController::new(FileHistoryTarget {
            binding: uuid::Uuid::new_v4(),
            record,
            label: "Report.txt".into(),
            transport: std::sync::Arc::new(Transport),
            authors,
        });
        let version = |id: u128, person: [u8; 32], device: [u8; 32]| FileVersionV1 {
            id: FileVersionId(uuid::Uuid::from_u128(id)),
            record_id: record,
            parents: vec![],
            actor: fs_protocol::FileActor {
                person: collab_types::Ed25519PublicKey::from_bytes(person),
                device: collab_types::Ed25519PublicKey::from_bytes(device),
            },
            timestamp_ms: u64::try_from(id).unwrap(),
            content_hash: None,
            metadata_hash: fs_protocol::ContentHash([0; 32]),
            provenance: fs_protocol::FileProvenance::Edit,
        };
        files.versions = vec![
            version(1, ada, ada_laptop),
            version(2, ada, scribe),
            version(3, stranger, scribe),
        ];
        let rows = file_timeline_entries(&files);
        assert_eq!(rows[0].author.name, "Ada");
        assert_eq!(rows[0].author.attribution, None);
        assert_eq!(rows[1].author.name, "Scribe · Agent, for Ada");
        assert_eq!(rows[1].author.relation, AuthorRelation::Software);
        assert_eq!(rows[1].author.rgb(), (10, 20, 30));
        assert_eq!(
            rows[1].author.attribution,
            Some(HistoryAttribution {
                actor: "agent:scribe".into(),
                kind: tilo_actions::ActorKind::Agent,
                actor_name: "Scribe".into(),
                on_behalf_of: Some(hex::encode(ada)),
                on_behalf_of_name: Some("Ada".into()),
            })
        );
        // A device is an agent only of the person who delegated it.
        assert_ne!(rows[2].author.name, rows[1].author.name);
        assert_eq!(rows[2].author.attribution, None);
    }
}

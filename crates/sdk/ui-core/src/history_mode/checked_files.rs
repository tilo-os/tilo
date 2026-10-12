//! Typed checked-file state. No Automerge identity or document fallback exists here.
use fs_protocol::files::{
    CheckedHistoryOperationV1 as Operation, CheckedHistoryOutcomeV1 as Outcome,
    CheckedHistoryRequestV1 as Request, FileHistoryReadErrorV1 as ReadError,
    FileHistoryReadQueryV1 as Query, FileHistoryReadScopeV1 as ReadScope,
    FileHistoryReadValueV1 as ReadValue, FileRestoreReceiptV1 as Receipt,
    FileRestoreReviewV1 as Review,
};
use fs_protocol::{FileRecordId, FileVersionId};
use std::{cell::RefCell, rc::Rc, sync::Arc};

/// Trusted owner-side transport. This handle is never included in preview input.
pub trait FileHistoryTransport: Send + Sync {
    /// # Errors
    /// Returns the transport or checked-read failure for the requested scope.
    fn read(&self, query: Query) -> Result<ReadValue, ReadError>;
    /// # Errors
    /// Returns a checked-read failure when the requested version cannot be retrieved.
    fn preview(&self, scope: ReadScope, version: FileVersionId) -> Result<Vec<u8>, ReadError>;
    /// # Errors
    /// Returns the checked operation outcome when a restore review cannot be produced.
    fn review(&self) -> Result<Review, Outcome>;
    fn submit(&self, request: Request) -> Outcome;
    fn result(&self, intent: uuid::Uuid) -> Outcome;
}
#[derive(Clone)]
pub struct FileHistoryTarget {
    pub binding: uuid::Uuid,
    pub record: FileRecordId,
    pub label: String,
    pub transport: Arc<dyn FileHistoryTransport>,
    /// Who the people and agents writing versions are, as the app knows
    /// them: a version an agent's device signed reads as the agent's, on
    /// behalf of its person. Presentation only; never authority.
    pub authors: crate::history_source::AuthorDirectory,
}
/// A root retains its own handle; the runner validates this exact source witness.
#[derive(Clone, Default)]
pub struct FileHistorySource(Rc<RefCell<FileHistorySourceState>>);
/// An admitted open request; `None` does not request a specific version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileHistoryOpenRequest {
    pub version: Option<FileVersionId>,
}
#[derive(Default)]
struct FileHistorySourceState {
    target: Option<FileHistoryTarget>,
    open: Option<FileHistoryOpenRequest>,
}
impl FileHistorySource {
    pub fn set_target(&self, target: Option<FileHistoryTarget>) {
        self.0.borrow_mut().target = target;
    }
    pub fn open(&self, version: Option<FileVersionId>) {
        self.0.borrow_mut().open = Some(FileHistoryOpenRequest { version });
    }
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn target(&self) -> Option<FileHistoryTarget> {
        self.0.borrow().target.clone()
    }
    pub(crate) fn take_open(&self) -> Option<FileHistoryOpenRequest> {
        self.0.borrow_mut().open.take()
    }
    pub(crate) fn wants_open(&self) -> bool {
        self.0.borrow().open.is_some()
    }
}

#[derive(Clone, Debug)]
pub enum FileHistoricalContent {
    Bytes(Arc<[u8]>),
    NoContent,
    Unavailable(ReadError),
}
/// Immutable metadata and exact bytes only. No path, service, callback or live State.
#[derive(Clone, Debug)]
pub struct FileHistorySnapshot {
    pub scope: ReadScope,
    pub version: fs_protocol::FileVersionV1,
    pub content: FileHistoricalContent,
}
pub struct FilePreviewInput {
    pub snapshot: FileHistorySnapshot,
    pub initial_view: super::PreviewView,
}
pub type FilePreviewFactory =
    fn(FilePreviewInput) -> Result<Box<dyn super::HistoryPreview>, String>;

#[derive(Default)]
pub(crate) struct FileRestoreSession {
    pub request: Option<Request>,
    pub outcome: Option<Outcome>,
    pub receipts: Vec<Receipt>,
    uncertain: bool,
    pub last_operation: Option<Operation>,
    /// Requests this session already resolved. Their submit and result calls
    /// run concurrently, so a slower call for the same exact request can
    /// still complete after another one settled it.
    settled: Vec<Request>,
}
impl FileRestoreSession {
    pub(crate) fn prepare(
        &mut self,
        review: Review,
        operation: Operation,
    ) -> Result<Request, &'static str> {
        if self.request.is_some() {
            return Err("Resolve the existing restore intent first");
        }
        review.validate().map_err(|_| "Invalid backend review")?;
        let request = Request {
            schema_version: 1,
            intent_id: uuid::Uuid::new_v4(),
            review,
            operation,
        };
        request.validate().map_err(|_| "Invalid backend request")?;
        self.request = Some(request);
        self.outcome = None;
        Ok(request)
    }
    /// Whether `request` is no longer pending because this session already
    /// resolved it. A late completion for it is stale, not a backend fault.
    pub(crate) fn has_settled(&self, request: &Request) -> bool {
        self.request.as_ref() != Some(request) && self.settled.contains(request)
    }
    pub(crate) fn receive(&mut self, request: Request, outcome: Outcome) -> bool {
        if self.request != Some(request) {
            return false;
        }
        match &outcome {
            Outcome::Applied { receipt } => {
                if receipt.schema_version != 1
                    || receipt.intent_id != request.intent_id
                    || receipt.workspace_id != request.review.workspace_id
                    || receipt.mount_lifetime != request.review.mount_lifetime
                    || receipt.record_id != request.review.record_id
                    || receipt.prior_revision.record_id != request.review.record_id
                    || receipt.restored_revision.record_id != request.review.record_id
                    || receipt.prior_revision.validate().is_err()
                    || receipt.restored_revision.validate().is_err()
                    || match request.operation {
                        Operation::Restore { version_id } => {
                            receipt.selected_version != version_id || receipt.compensates.is_some()
                        }
                        Operation::Undo { restore_intent } => {
                            receipt.compensates != Some(restore_intent)
                        }
                        Operation::Redo { undo_intent } => receipt.compensates != Some(undo_intent),
                    }
                {
                    return false;
                }
                if !self
                    .receipts
                    .iter()
                    .any(|old| old.intent_id == receipt.intent_id)
                {
                    self.receipts.push(receipt.clone());
                }
                self.last_operation = Some(request.operation);
                self.request = None;
                self.uncertain = false;
            }
            Outcome::Unchanged { intent_id, review } => {
                if *intent_id != request.intent_id || *review != request.review {
                    return false;
                }
                self.request = None;
                self.uncertain = false;
            }
            Outcome::RecoveryPending { intent_id } => {
                if *intent_id != request.intent_id {
                    return false;
                }
                self.uncertain = true;
            }
            Outcome::Unavailable => self.uncertain = true,
            _ => {
                if !self.uncertain {
                    self.request = None;
                }
            }
        }
        if self.request.is_none() {
            self.settled.push(request);
        }
        self.outcome = Some(outcome);
        true
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn checked_file_requests_are_exact_consumed_once_and_undo_only_real_changes() {
        // phase: open_request_distinguishes_idle_unspecified_and_exact_version_and_is_consumed_once
        {
            let source = super::FileHistorySource::default();
            assert!(!source.wants_open());
            assert!(source.take_open().is_none());
            source.open(None);
            assert!(source.wants_open());
            assert_eq!(source.take_open().unwrap().version, None);
            assert!(!source.wants_open());
            assert!(source.take_open().is_none());
            let version = fs_protocol::FileVersionId(uuid::Uuid::new_v4());
            source.open(Some(version));
            assert_eq!(source.take_open().unwrap().version, Some(version));
            assert!(source.take_open().is_none());
            source.open(Some(version));
            source.open(None);
            assert_eq!(source.take_open().unwrap().version, None);
        }
        // phase: files_uncertain_result_keeps_exact_request_and_blocks_replacement
        {
            let mut session = FileRestoreSession::default();
            let request = session
                .prepare(
                    review(),
                    Operation::Restore {
                        version_id: FileVersionId(uuid::Uuid::from_u128(5)),
                    },
                )
                .unwrap();
            assert!(session.receive(
                request,
                Outcome::RecoveryPending {
                    intent_id: request.intent_id
                }
            ));
            assert_eq!(session.request, Some(request));
            assert!(session.prepare(review(), request.operation).is_err());
            assert!(session.receive(request, Outcome::Unavailable));
            assert_eq!(session.request, Some(request));
            let mut foreign = request;
            foreign.review.record_id = FileRecordId(uuid::Uuid::from_u128(6));
            assert!(!session.receive(foreign, Outcome::NotVisible));
            assert_eq!(session.request, Some(request));
        }
        // phase: files_late_completion_of_a_settled_request_is_recognised_as_stale
        {
            let mut session = FileRestoreSession::default();
            let request = session
                .prepare(
                    review(),
                    Operation::Restore {
                        version_id: FileVersionId(uuid::Uuid::from_u128(5)),
                    },
                )
                .unwrap();
            assert!(
                !session.has_settled(&request),
                "a pending request is not stale"
            );
            assert!(session.receive(request, Outcome::ReadOnly));
            assert!(session.has_settled(&request));
            let mut foreign = request;
            foreign.intent_id = uuid::Uuid::from_u128(9);
            assert!(!session.has_settled(&foreign));
            let next = session
                .prepare(
                    review(),
                    Operation::Restore {
                        version_id: FileVersionId(uuid::Uuid::from_u128(6)),
                    },
                )
                .unwrap();
            assert!(session.has_settled(&request));
            assert!(!session.has_settled(&next));
        }
        // phase: files_unchanged_and_definite_rejections_do_not_create_undo_entries
        {
            for outcome in [
                Outcome::ReviewRequired,
                Outcome::ReadOnly,
                Outcome::PendingWrites,
                Outcome::UnsupportedOwner {
                    mapping: "tilo-doc".into(),
                },
                Outcome::InvalidRequest,
            ] {
                let mut session = FileRestoreSession::default();
                let request = session
                    .prepare(
                        review(),
                        Operation::Restore {
                            version_id: FileVersionId(uuid::Uuid::from_u128(5)),
                        },
                    )
                    .unwrap();
                assert!(session.receive(request, outcome));
                assert!(session.request.is_none());
                assert!(session.receipts.is_empty());
            }
            let mut session = FileRestoreSession::default();
            let request = session
                .prepare(
                    review(),
                    Operation::Restore {
                        version_id: FileVersionId(uuid::Uuid::from_u128(5)),
                    },
                )
                .unwrap();
            assert!(session.receive(
                request,
                Outcome::Unchanged {
                    intent_id: request.intent_id,
                    review: request.review
                }
            ));
            assert!(session.request.is_none());
            assert!(session.receipts.is_empty());
        }
    }

    use super::*;
    fn review() -> Review {
        Review {
            schema_version: 1,
            workspace_id: uuid::Uuid::from_u128(1),
            mount_lifetime: uuid::Uuid::from_u128(2),
            record_id: FileRecordId(uuid::Uuid::from_u128(3)),
            frontier: [4; 32],
        }
    }
}

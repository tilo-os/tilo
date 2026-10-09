//! Host-owned asynchronous file sessions. Only frozen values cross into renderers.
use super::checked_files::{
    FileHistoricalContent, FileHistorySnapshot, FileHistoryTarget, FileHistoryTransport,
    FileRestoreSession,
};
use fs_protocol::files::{
    CheckedHistoryOperationV1 as Operation, CheckedHistoryOutcomeV1 as Outcome,
    CheckedHistoryRequestV1 as Request, FileHistoryReadErrorV1 as Error,
    FileHistoryReadQueryV1 as Query, FileHistoryReadScopeV1 as Scope,
    FileHistoryReadValueV1 as Value, FileRestoreReviewV1 as Review,
};
use fs_protocol::{FileVersionId, FileVersionV1};
use std::{
    cell::RefCell,
    sync::{Arc, mpsc},
    time::Duration,
};

use crate::background::Background;
use crate::i18n::I18nManager;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    host: uuid::Uuid,
    generation: u64,
    binding: uuid::Uuid,
    selection: Option<FileVersionId>,
}
enum Reply {
    Activity(
        Result<
            (
                Scope,
                Vec<FileVersionV1>,
                Option<fs_protocol::ContentRevisionV1>,
            ),
            Error,
        >,
    ),
    Selection(Result<(FileHistorySnapshot, Option<FileHistorySnapshot>), Error>),
    Review(Operation, Result<Review, Outcome>),
    /// The host refused to run the transport call in the background.
    Unavailable,
    Outcome(Request, Outcome),
}
const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a snapshot of the controller's independent presentation flags"
)]
struct Presented {
    versions: Vec<FileVersionV1>,
    scope: Option<Scope>,
    current_revision: Option<fs_protocol::ContentRevisionV1>,
    message: String,
    live_updates: bool,
    preview_changed: bool,
    presentation_invalidated: bool,
    review_changed: bool,
}

struct Completion {
    key: Key,
    reply: Reply,
}
#[allow(
    clippy::struct_excessive_bools,
    reason = "preview/review notifications, live-update policy and asynchronous lifecycle flags can coexist"
)]
pub(super) struct FileController {
    pub target: FileHistoryTarget,
    key: Key,
    tx: mpsc::Sender<Completion>,
    rx: mpsc::Receiver<Completion>,
    inbox: RefCell<Vec<Completion>>,
    wake: crate::wake::WakeHandle,
    background: Background,
    pub versions: Vec<FileVersionV1>,
    pub scope: Option<Scope>,
    pub current_revision: Option<fs_protocol::ContentRevisionV1>,
    pub after: Option<FileHistorySnapshot>,
    pub before: Option<FileHistorySnapshot>,
    pub parent: Option<FileVersionId>,
    pub review: Option<(Review, Operation)>,
    pub review_operation: Option<Operation>,
    confirmation: uuid::Uuid,
    pub restore: FileRestoreSession,
    pub message: String,
    activity_recovery_message: Option<String>,
    pub preview_changed: bool,
    pub presentation_invalidated: bool,
    pub review_changed: bool,
    pub live_updates: bool,
    active: bool,
    refreshing: bool,
    /// `message` reports a restore outcome. The activity summary must not
    /// replace it until the user acts or the presentation is reactivated.
    reporting: bool,
    next_poll: crate::time::Instant,
}
impl FileController {
    pub fn new(target: FileHistoryTarget) -> Self {
        let (tx, rx) = mpsc::channel();
        let background = Background::current();
        let now = background.now();
        Self {
            key: Key {
                host: uuid::Uuid::new_v4(),
                generation: 0,
                binding: target.binding,
                selection: None,
            },
            target,
            tx,
            rx,
            inbox: RefCell::new(vec![]),
            wake: crate::wake::WakeHandle::new(),
            background,
            versions: vec![],
            scope: None,
            current_revision: None,
            after: None,
            before: None,
            parent: None,
            review: None,
            review_operation: None,
            confirmation: uuid::Uuid::new_v4(),
            restore: FileRestoreSession::default(),
            message: I18nManager::lookup_plain("history-files-loading"),
            activity_recovery_message: None,
            preview_changed: false,
            presentation_invalidated: false,
            review_changed: false,
            live_updates: false,
            active: true,
            refreshing: false,
            reporting: false,
            next_poll: now,
        }
    }
    fn run(&self, work: impl FnOnce(Arc<dyn FileHistoryTransport>) -> Reply + Send + 'static) {
        let transport = self.target.transport.clone();
        let key = self.key;
        let tx = self.tx.clone();
        let wake = self.wake.clone();
        let spawned = self.background.spawn("ui-file-history", move || {
            let reply = work(transport);
            let _ = tx.send(Completion { key, reply });
            wake.wake();
        });
        if spawned.is_err() {
            self.inbox.borrow_mut().push(Completion {
                key,
                reply: Reply::Unavailable,
            });
        }
    }
    pub fn has_results(&self) -> bool {
        self.inbox.borrow_mut().extend(self.rx.try_iter());
        !self.inbox.borrow().is_empty()
            || self.active && !self.refreshing && self.background.now() >= self.next_poll
    }
    pub fn load(&mut self) {
        if self.refreshing {
            return;
        }
        self.refreshing = true;
        self.run(|transport| Reply::Activity(load_all(transport.as_ref())));
    }
    fn advance(&mut self) {
        if let Some(next) = self.key.generation.checked_add(1) {
            self.key.generation = next;
        } else {
            self.key.host = uuid::Uuid::new_v4();
            self.key.generation = 0;
        }
        self.review = None;
        self.confirmation = uuid::Uuid::new_v4();
        self.review_changed = true;
        self.refreshing = false;
        self.reporting = false;
    }
    pub fn suspend(&mut self) {
        self.active = false;
        self.advance();
        self.after = None;
        self.before = None;
        self.key.selection = None;
        self.preview_changed = true;
    }
    pub fn is_active(&self) -> bool {
        self.active
    }
    pub fn activate(&mut self) {
        self.advance();
        self.key.selection = None;
        self.parent = None;
        self.live_updates = false;
        self.versions.clear();
        self.current_revision = None;
        self.after = None;
        self.before = None;
        self.preview_changed = true;
        self.active = true;
        self.refreshing = false;
        self.message = I18nManager::lookup_plain("history-files-loading");
        self.next_poll = self.background.now();
        self.load();
    }
    /// Learned only from a successful authorized immutable metadata read.
    pub fn resource_identity(&self) -> Option<(uuid::Uuid, uuid::Uuid, fs_protocol::FileRecordId)> {
        self.scope
            .map(|scope| (scope.workspace_id, scope.mount_lifetime, scope.record_id))
    }
    pub fn selected(&self) -> Option<FileVersionId> {
        self.key.selection
    }
    pub fn select(&mut self, version: FileVersionId, parent: Option<FileVersionId>) {
        self.advance();
        self.key.selection = Some(version);
        self.parent = parent;
        self.after = None;
        self.before = None;
        self.preview_changed = true;
        self.activity_recovery_message = None;
        self.message = I18nManager::lookup_plain("history-files-loading-version");
        self.run(move |transport| {
            Reply::Selection(read_selection(transport.as_ref(), version, parent))
        });
    }
    pub fn request_review(&mut self, operation: Operation) {
        self.advance();
        self.review_operation = Some(operation);
        self.review = None;
        self.review_changed = true;
        self.activity_recovery_message = None;
        self.message = I18nManager::lookup_plain("history-files-checking-permissions");
        self.run(move |transport| Reply::Review(operation, transport.review()));
    }
    pub fn cancel_review(&mut self) {
        self.advance();
        self.review_operation = None;
    }
    pub fn confirmation_token(&self) -> uuid::Uuid {
        self.confirmation
    }
    pub fn confirm(&mut self, token: uuid::Uuid) {
        if token != self.confirmation {
            return;
        }
        let Some((review, operation)) = self.review.take() else {
            return;
        };
        self.activity_recovery_message = None;
        self.confirmation = uuid::Uuid::new_v4();
        self.review_changed = true;
        self.reporting = false;
        let Ok(request) = self.restore.prepare(review, operation) else {
            self.message = I18nManager::lookup_plain("history-files-earlier-pending");
            return;
        };
        self.message = I18nManager::lookup_plain("history-files-submitting");
        self.run(move |transport| Reply::Outcome(request, transport.submit(request)));
    }
    pub fn retry(&self) {
        let Some(request) = self.restore.request else {
            return;
        };
        self.run(move |transport| Reply::Outcome(request, transport.submit(request)));
    }
    pub fn check_result(&self) {
        let Some(request) = self.restore.request else {
            return;
        };
        self.run(move |transport| Reply::Outcome(request, transport.result(request.intent_id)));
    }
    fn visible_identity(
        &self,
        workspace: uuid::Uuid,
        mount: uuid::Uuid,
        record: fs_protocol::FileRecordId,
    ) -> bool {
        record == self.target.record
            && self.scope.is_none_or(|s| {
                s.workspace_id == workspace && s.mount_lifetime == mount && s.record_id == record
            })
    }
    fn hide(&mut self) {
        // A denial is a barrier even for successful reads that already returned
        // but have not delivered their completion. Exact operation settlement
        // remains bound to host/binding/request independently of this epoch.
        self.advance();
        self.presentation_invalidated = true;
        self.key.selection = None;
        self.parent = None;
        self.live_updates = false;
        self.review_operation = None;
        self.after = None;
        self.before = None;
        self.versions.clear();
        self.current_revision = None;
        self.review = None;
        self.preview_changed = true;
        self.review_changed = true;
    }
    fn apply_activity(
        &mut self,
        result: Result<
            (
                Scope,
                Vec<FileVersionV1>,
                Option<fs_protocol::ContentRevisionV1>,
            ),
            Error,
        >,
    ) {
        self.refreshing = false;
        self.schedule_poll();
        match result {
            Ok((scope, versions, current_revision))
                if self.visible_identity(
                    scope.workspace_id,
                    scope.mount_lifetime,
                    scope.record_id,
                ) =>
            {
                self.live_updates = self
                    .after
                    .as_ref()
                    .is_some_and(|after| after.scope.frontier != scope.frontier);
                self.scope = Some(scope);
                self.versions = versions;
                self.current_revision = current_revision;
                // Activity polling is ambient status. Keep an admitted restore
                // outcome visible, and after a transient read failure restore the
                // exact operational message that failure temporarily displaced.
                if let Some(message) = self.activity_recovery_message.take() {
                    self.message = message;
                } else if self.after.is_none() && self.restore.outcome.is_none() && !self.reporting
                {
                    self.message = I18nManager::format(
                        "history-files-version-count",
                        &[("count", self.versions.len().into())],
                    );
                }
            }
            Ok(_) => {
                self.activity_recovery_message = None;
                self.hide();
                self.message = I18nManager::lookup_plain("history-files-binding-changed");
            }
            Err(error) => {
                if error == Error::NotVisible {
                    self.activity_recovery_message = None;
                    self.hide();
                } else if self.activity_recovery_message.is_none()
                    && (self.after.is_some() || self.restore.outcome.is_some())
                {
                    self.activity_recovery_message = Some(self.message.clone());
                }
                self.message = read_message(error);
            }
        }
    }

    pub fn drain(&mut self) -> bool {
        self.has_results();
        let values = self.inbox.get_mut().drain(..).collect::<Vec<_>>();
        // Only what the presentation shows counts as a change: an idle poll
        // that finds the same activity must not rebuild the chrome under a
        // pointer that is mid-click.
        let mut changed = false;
        for completion in values {
            // Exact admitted outcomes survive a closed presentation generation,
            // but can only settle this controller's original request and target.
            if let Reply::Outcome(request, outcome) = completion.reply {
                if completion.key.host != self.key.host
                    || completion.key.binding != self.key.binding
                {
                    continue;
                }
                if self.restore.has_settled(&request) {
                    // The submit and a result lookup for one exact intent run
                    // concurrently; whichever settled it first already
                    // reported. The slower one must not replace that report.
                    continue;
                }
                changed = true;
                if self.restore.receive(request, outcome.clone()) {
                    self.activity_recovery_message = None;
                    self.message = outcome_message(&outcome);
                    self.reporting = true;
                    if matches!(outcome, Outcome::NotVisible) {
                        self.hide();
                    }
                    self.review_changed = true;
                    self.refreshing = false;
                    self.next_poll = self.background.now();
                } else {
                    self.activity_recovery_message = None;
                    self.message = I18nManager::lookup_plain("history-files-invalid-result");
                }
                continue;
            }
            if completion.key != self.key {
                continue;
            }
            let before = if matches!(completion.reply, Reply::Activity(_)) {
                Some(self.presented())
            } else {
                changed = true;
                None
            };
            match completion.reply {
                Reply::Unavailable => {
                    self.refreshing = false;
                    self.schedule_poll();
                    self.message = I18nManager::lookup_plain("history-files-unavailable-host");
                }
                Reply::Activity(result) => self.apply_activity(result),
                Reply::Selection(result) => match result {
                    Ok((after, before))
                        if self.visible_identity(
                            after.scope.workspace_id,
                            after.scope.mount_lifetime,
                            after.scope.record_id,
                        ) && Some(after.version.id) == self.key.selection =>
                    {
                        self.activity_recovery_message = None;
                        self.scope = Some(after.scope);
                        self.after = Some(after);
                        self.before = before;
                        self.preview_changed = true;
                        self.message =
                            I18nManager::lookup_plain("history-files-historical-version");
                    }
                    Ok(_) => {
                        self.activity_recovery_message = None;
                        self.hide();
                        self.message = I18nManager::lookup_plain("history-files-foreign-response");
                    }
                    Err(error) => {
                        self.activity_recovery_message = None;
                        self.after = None;
                        self.before = None;
                        if error == Error::NotVisible {
                            self.hide();
                        }
                        self.preview_changed = true;
                        self.message = read_message(error);
                    }
                },
                Reply::Review(operation, result) => {
                    self.review_changed = true;
                    match result {
                        Ok(review)
                            if review.validate().is_ok()
                                && self.visible_identity(
                                    review.workspace_id,
                                    review.mount_lifetime,
                                    review.record_id,
                                ) =>
                        {
                            if matches!(operation,Operation::Restore { version_id } if Some(version_id)!=self.key.selection)
                            {
                                continue;
                            }
                            self.activity_recovery_message = None;
                            self.review = Some((review, operation));
                            self.message =
                                I18nManager::lookup_plain("history-files-restore-explainer");
                        }
                        Ok(_) => {
                            self.activity_recovery_message = None;
                            self.hide();
                            self.message =
                                I18nManager::lookup_plain("history-files-foreign-review");
                        }
                        Err(outcome) => {
                            self.activity_recovery_message = None;
                            if matches!(outcome, Outcome::NotVisible) {
                                self.hide();
                            }
                            self.message = outcome_message(&outcome);
                        }
                    }
                }
                Reply::Outcome(..) => {}
            }
            if let Some(before) = before {
                changed |= before != self.presented();
            }
        }
        if self.active && !self.refreshing && self.background.now() >= self.next_poll {
            self.load();
        }
        changed
    }
    /// Everything an activity reply can change that the host presents.
    fn presented(&self) -> Presented {
        Presented {
            versions: self.versions.clone(),
            scope: self.scope,
            current_revision: self.current_revision,
            message: self.message.clone(),
            live_updates: self.live_updates,
            preview_changed: self.preview_changed,
            presentation_invalidated: self.presentation_invalidated,
            review_changed: self.review_changed,
        }
    }
    /// Refresh the activity list again after the poll interval.
    fn schedule_poll(&mut self) {
        let now = self.background.now();
        self.next_poll = now.checked_add(POLL_INTERVAL).unwrap_or(now);
        if self.active {
            self.background.wake_at(self.next_poll, self.wake.clone());
        }
    }
}
fn load_all(
    transport: &dyn FileHistoryTransport,
) -> Result<
    (
        Scope,
        Vec<FileVersionV1>,
        Option<fs_protocol::ContentRevisionV1>,
    ),
    Error,
> {
    let mut cursor = None;
    let mut expected = None;
    let mut versions = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut previous = None;
    loop {
        let Value::Page(page) = transport.read(Query::Page { cursor })? else {
            return Err(Error::InvalidRequest);
        };
        let metadata = (
            page.scope,
            page.total_versions,
            page.heads.clone(),
            page.current_revision,
        );
        if expected.as_ref().is_some_and(|value| value != &metadata)
            || page.versions.len() > fs_protocol::files::MAX_HISTORY_PAGE_VERSIONS
            || page.scope.workspace_id.is_nil()
            || page.scope.mount_lifetime.is_nil()
            || page.scope.record_id.0.is_nil()
            || page.current_revision.is_some_and(|revision| {
                revision.validate().is_err() || revision.record_id != page.scope.record_id
            })
        {
            return Err(Error::InvalidRequest);
        }
        expected = Some(metadata);
        for version in &page.versions {
            if version.record_id != page.scope.record_id
                || version.validate().is_err()
                || !seen.insert(version.id)
            {
                return Err(Error::InvalidRequest);
            }
            let order = (version.timestamp_ms, version.id);
            if previous.is_some_and(|last| last >= order) {
                return Err(Error::InvalidRequest);
            }
            previous = Some(order);
        }
        if page.next_after.is_some() && page.next_after != page.versions.last().map(|v| v.id) {
            return Err(Error::InvalidRequest);
        }
        versions.extend(page.versions);
        if versions.len() as u64 > page.total_versions {
            return Err(Error::InvalidRequest);
        }
        match page.next_after {
            Some(id) => cursor = Some((page.scope.frontier, id)),
            None if versions.len() as u64 == page.total_versions => {
                if page.heads.iter().any(|id| !seen.contains(id))
                    || page.current_revision.is_some_and(|revision| {
                        #[allow(clippy::suspicious_operation_groupings, reason = "FileVersionV1.id corresponds to ContentRevisionV1.version_id, not the revision identity")]
                        let present = versions.iter().any(|version| {
                            version.id == revision.version_id
                                && version.content_hash == revision.content_hash
                                && version.metadata_hash == revision.metadata_hash
                        });
                        !present
                    })
                {
                    return Err(Error::InvalidRequest);
                }
                return Ok((page.scope, versions, page.current_revision));
            }
            None => return Err(Error::InvalidRequest),
        }
    }
}
fn read_selection(
    transport: &dyn FileHistoryTransport,
    version_id: FileVersionId,
    parent: Option<FileVersionId>,
) -> Result<(FileHistorySnapshot, Option<FileHistorySnapshot>), Error> {
    let Value::Page(page) = transport.read(Query::Page { cursor: None })? else {
        return Err(Error::InvalidRequest);
    };
    let after = read_snapshot(transport, page.scope, version_id)?;
    let parent = match parent {
        Some(parent) if after.version.parents.contains(&parent) => Some(parent),
        Some(_) => return Err(Error::InvalidRequest),
        None if after.version.parents.len() == 1 => after.version.parents.first().copied(),
        None => None,
    };
    let before = parent
        .map(|id| read_snapshot(transport, page.scope, id))
        .transpose()?;
    Ok((after, before))
}
fn read_snapshot(
    transport: &dyn FileHistoryTransport,
    scope: Scope,
    version_id: FileVersionId,
) -> Result<FileHistorySnapshot, Error> {
    let Value::Version {
        scope: actual,
        version,
    } = transport.read(Query::Version {
        frontier: scope.frontier,
        version_id,
    })?
    else {
        return Err(Error::InvalidRequest);
    };
    if actual != scope
        || version.id != version_id
        || version.record_id != scope.record_id
        || version.validate().is_err()
    {
        return Err(Error::InvalidRequest);
    }
    let content = if version.content_hash.is_none() {
        FileHistoricalContent::NoContent
    } else {
        match transport.preview(scope, version_id) {
            Ok(bytes) if bytes.len() <= fs_protocol::MAX_HISTORY_PREVIEW_BYTES => {
                FileHistoricalContent::Bytes(bytes.into())
            }
            Ok(_) => FileHistoricalContent::Unavailable(Error::TooLarge),
            Err(Error::NotVisible) => return Err(Error::NotVisible),
            Err(error) => FileHistoricalContent::Unavailable(error),
        }
    };
    Ok(FileHistorySnapshot {
        scope,
        version,
        content,
    })
}
pub(super) fn read_message(error: Error) -> String {
    I18nManager::lookup_plain(match error {
        Error::Stale => "history-files-error-stale",
        Error::NotVisible => "history-files-not-visible",
        Error::MissingVersion => "history-files-error-missing-version",
        Error::InvalidRequest => "history-files-error-invalid",
        Error::Unavailable => "history-files-error-unavailable",
        Error::TooLarge => "history-files-error-too-large",
        Error::NoContent => "history-files-error-no-content",
    })
}
pub(super) fn outcome_message(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Applied { receipt } => I18nManager::format(
            match receipt.prior_reference {
                fs_protocol::files::FileRestorePriorReferenceV1::ObservedRevision => {
                    "history-files-outcome-applied"
                }
                fs_protocol::files::FileRestorePriorReferenceV1::LegacyContentRepresentative => {
                    "history-files-outcome-applied-legacy"
                }
            },
            &[
                (
                    "revision",
                    receipt.restored_revision.version_id.0.to_string().into(),
                ),
                ("intent", receipt.intent_id.to_string().into()),
            ],
        ),
        Outcome::Unchanged { .. } => I18nManager::lookup_plain("history-files-outcome-unchanged"),
        Outcome::RecoveryPending { .. } => {
            I18nManager::lookup_plain("history-files-outcome-pending")
        }
        Outcome::ReviewRequired => I18nManager::lookup_plain("history-files-outcome-review"),
        Outcome::NotVisible => I18nManager::lookup_plain("history-files-not-visible"),
        Outcome::ReadOnly => I18nManager::lookup_plain("history-files-outcome-read-only"),
        Outcome::PendingWrites => I18nManager::lookup_plain("history-files-outcome-pending-writes"),
        Outcome::UnsupportedOwner { mapping } => I18nManager::format(
            "history-files-outcome-unsupported-owner",
            &[("mapping", mapping.clone().into())],
        ),
        Outcome::Unavailable => I18nManager::lookup_plain("history-files-outcome-unavailable"),
        Outcome::InvalidRequest => I18nManager::lookup_plain("history-files-outcome-invalid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Transport;
    impl FileHistoryTransport for Transport {
        fn read(&self, _: Query) -> Result<Value, Error> {
            Err(Error::Unavailable)
        }
        fn preview(&self, _: Scope, _: FileVersionId) -> Result<Vec<u8>, Error> {
            Err(Error::Unavailable)
        }
        fn review(&self) -> Result<Review, Outcome> {
            Err(Outcome::Unavailable)
        }
        fn submit(&self, _: Request) -> Outcome {
            Outcome::Unavailable
        }
        fn result(&self, _: uuid::Uuid) -> Outcome {
            Outcome::Unavailable
        }
    }
    fn controller() -> FileController {
        FileController::new(FileHistoryTarget {
            binding: uuid::Uuid::from_u128(9),
            record: fs_protocol::FileRecordId(uuid::Uuid::from_u128(3)),
            label: "Report.txt".into(),
            transport: Arc::new(Transport),
        })
    }
    #[test]
    fn files_activity_follows_the_clock_and_rejects_incoherent_pages() {
        // phase: activity_refresh_follows_the_background_clock
        {
            let pumped = crate::background::Pumped::new();
            let mut controller = {
                let _background = pumped.background().enter();
                controller()
            };
            controller.load();
            assert!(!controller.has_results());
            assert_eq!(pumped.run_pending(), 1);
            assert!(controller.drain());
            assert_eq!(controller.message, read_message(Error::Unavailable));
            let refresh = pumped.next_deadline().expect("the next refresh is armed");
            assert!(!controller.has_results());
            pumped.advance(POLL_INTERVAL);
            assert_eq!(pumped.now(), refresh);
            assert!(controller.has_results());
            controller.drain();
            assert!(!pumped.is_idle(), "the due refresh queued the next load");
        }
        // phase: a_host_without_background_work_reports_history_unavailable
        {
            let mut controller = {
                let _background = Background::new(crate::background::NoBackground).enter();
                controller()
            };
            controller.load();
            assert!(controller.drain());
            assert_eq!(
                controller.message,
                "File history is unavailable on this host"
            );
            assert!(!controller.has_results(), "no immediate retry loop");
        }
        // phase: files_activity_rejects_incoherent_page_metadata_and_order
        {
            struct Pages(std::sync::Mutex<std::collections::VecDeque<Value>>);
            impl FileHistoryTransport for Pages {
                fn read(&self, _: Query) -> Result<Value, Error> {
                    self.0.lock().unwrap().pop_front().ok_or(Error::Unavailable)
                }
                fn preview(&self, _: Scope, _: FileVersionId) -> Result<Vec<u8>, Error> {
                    Err(Error::Unavailable)
                }
                fn review(&self) -> Result<Review, Outcome> {
                    Err(Outcome::Unavailable)
                }
                fn submit(&self, _: Request) -> Outcome {
                    Outcome::Unavailable
                }
                fn result(&self, _: uuid::Uuid) -> Outcome {
                    Outcome::Unavailable
                }
            }
            let version = |id| FileVersionV1 {
                id: FileVersionId(uuid::Uuid::from_u128(id)),
                record_id: scope().record_id,
                parents: vec![],
                actor: fs_protocol::FileActor {
                    person: collab_types::Ed25519PublicKey::from_bytes([1; 32]),
                    device: collab_types::Ed25519PublicKey::from_bytes([2; 32]),
                },
                timestamp_ms: id as u64,
                content_hash: None,
                metadata_hash: fs_protocol::ContentHash([0; 32]),
                provenance: fs_protocol::FileProvenance::Create,
            };
            let page = |versions: Vec<FileVersionV1>, total, next| {
                Value::Page(fs_protocol::files::FileHistoryPageV1 {
                    scope: scope(),
                    versions,
                    total_versions: total,
                    heads: vec![],
                    current_revision: None,
                    next_after: next,
                })
            };
            let transport = Pages(std::sync::Mutex::new(
                [page(vec![version(2), version(1)], 2, None)].into(),
            ));
            assert_eq!(
                load_all(&transport),
                Err(Error::InvalidRequest),
                "within-page order must be deterministic"
            );
            let transport = Pages(std::sync::Mutex::new(
                [
                    page(vec![version(1)], 3, Some(version(1).id)),
                    page(vec![version(2)], 2, None),
                ]
                .into(),
            ));
            assert_eq!(
                load_all(&transport),
                Err(Error::InvalidRequest),
                "a pinned frontier cannot change its total across pages"
            );
        }
    }

    #[test]
    fn activity_refresh_does_not_hide_an_uncertain_restore_outcome() {
        let mut initial = controller();
        initial.active = false;
        let initial_key = initial.key;
        initial
            .tx
            .send(Completion {
                key: initial_key,
                reply: Reply::Activity(Err(Error::Unavailable)),
            })
            .unwrap();
        initial.drain();
        initial
            .tx
            .send(Completion {
                key: initial_key,
                reply: Reply::Activity(Ok((scope(), vec![], None))),
            })
            .unwrap();
        initial.drain();
        assert_eq!(initial.message, "0 immutable versions");

        let mut controller = controller();
        controller.active = false;
        let request = controller
            .restore
            .prepare(
                Review {
                    schema_version: 1,
                    workspace_id: scope().workspace_id,
                    mount_lifetime: scope().mount_lifetime,
                    record_id: scope().record_id,
                    frontier: scope().frontier,
                },
                Operation::Restore {
                    version_id: FileVersionId(uuid::Uuid::from_u128(8)),
                },
            )
            .unwrap();
        let key = controller.key;
        controller
            .tx
            .send(Completion {
                key,
                reply: Reply::Outcome(request, Outcome::Unavailable),
            })
            .unwrap();
        controller
            .tx
            .send(Completion {
                key,
                reply: Reply::Activity(Ok((scope(), vec![], None))),
            })
            .unwrap();

        controller.drain();

        assert_eq!(controller.restore.request, Some(request));
        assert_eq!(controller.restore.outcome, Some(Outcome::Unavailable));
        assert_eq!(controller.message, outcome_message(&Outcome::Unavailable));

        controller
            .tx
            .send(Completion {
                key,
                reply: Reply::Activity(Err(Error::Unavailable)),
            })
            .unwrap();
        controller.drain();
        assert_eq!(controller.message, read_message(Error::Unavailable));

        controller
            .tx
            .send(Completion {
                key,
                reply: Reply::Activity(Ok((scope(), vec![], None))),
            })
            .unwrap();
        controller.drain();
        assert_eq!(controller.message, outcome_message(&Outcome::Unavailable));

        controller
            .tx
            .send(Completion {
                key,
                reply: Reply::Activity(Err(Error::Unavailable)),
            })
            .unwrap();
        controller.drain();
        let mut foreign = request;
        foreign.intent_id = uuid::Uuid::new_v4();
        controller
            .tx
            .send(Completion {
                key,
                reply: Reply::Outcome(foreign, Outcome::Unavailable),
            })
            .unwrap();
        controller.drain();
        assert_eq!(
            controller.message,
            "Invalid backend result; the exact request remains unresolved"
        );
        controller
            .tx
            .send(Completion {
                key,
                reply: Reply::Activity(Ok((scope(), vec![], None))),
            })
            .unwrap();
        controller.drain();
        assert_eq!(
            controller.message,
            "Invalid backend result; the exact request remains unresolved"
        );
    }

    fn scope() -> Scope {
        Scope {
            workspace_id: uuid::Uuid::from_u128(1),
            mount_lifetime: uuid::Uuid::from_u128(2),
            record_id: fs_protocol::FileRecordId(uuid::Uuid::from_u128(3)),
            frontier: [4; 32],
        }
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn files_reviews_are_fenced_against_denial_cancellation_and_delay() {
        // phase: round1_denial_fences_delayed_preview_and_review_but_keeps_exact_settlement
        {
            for changed_binding in [false, true] {
                let mut controller = controller();
                controller.active = false;
                let version = FileVersionV1 {
                    id: FileVersionId(uuid::Uuid::from_u128(8)),
                    record_id: scope().record_id,
                    parents: vec![],
                    actor: fs_protocol::FileActor {
                        person: collab_types::Ed25519PublicKey::from_bytes([1; 32]),
                        device: collab_types::Ed25519PublicKey::from_bytes([2; 32]),
                    },
                    timestamp_ms: 1,
                    content_hash: Some(fs_protocol::ContentHash([3; 32])),
                    metadata_hash: fs_protocol::ContentHash([4; 32]),
                    provenance: fs_protocol::FileProvenance::Create,
                };
                controller.key.selection = Some(version.id);
                let key = controller.key;
                let nonce = controller.confirmation_token();
                let review = Review {
                    schema_version: 1,
                    workspace_id: scope().workspace_id,
                    mount_lifetime: scope().mount_lifetime,
                    record_id: scope().record_id,
                    frontier: scope().frontier,
                };
                let operation = Operation::Restore {
                    version_id: version.id,
                };
                let request = controller.restore.prepare(review, operation).unwrap();
                let denial = if changed_binding {
                    let mut foreign = scope();
                    foreign.mount_lifetime = uuid::Uuid::new_v4();
                    controller.scope = Some(scope());
                    Reply::Activity(Ok((foreign, vec![], None)))
                } else {
                    Reply::Activity(Err(Error::NotVisible))
                };
                // The reads succeeded before revocation, but their completions arrive
                // after the newer access denial. The channel order is deterministic.
                for reply in [
                    denial,
                    Reply::Selection(Ok((
                        FileHistorySnapshot {
                            scope: scope(),
                            version,
                            content: FileHistoricalContent::Bytes(Arc::from(&b"protected"[..])),
                        },
                        None,
                    ))),
                    Reply::Review(operation, Ok(review)),
                    Reply::Outcome(
                        request,
                        Outcome::RecoveryPending {
                            intent_id: request.intent_id,
                        },
                    ),
                ] {
                    controller.tx.send(Completion { key, reply }).unwrap();
                }
                controller.drain();
                assert!(
                    controller.after.is_none(),
                    "denial must fence already-returned but delayed protected bytes"
                );
                assert!(
                    controller.review.is_none(),
                    "denial must fence delayed authorized review"
                );
                assert_ne!(controller.confirmation_token(), nonce);
                assert_eq!(controller.selected(), None);
                assert_eq!(controller.restore.request, Some(request));
                assert_eq!(
                    controller.restore.outcome,
                    Some(Outcome::RecoveryPending {
                        intent_id: request.intent_id
                    })
                );
            }
        }
        // phase: files_confirmation_from_cancelled_review_cannot_confirm_new_review
        {
            let mut controller = controller();
            controller.active = false;
            let review = Review {
                schema_version: 1,
                workspace_id: scope().workspace_id,
                mount_lifetime: scope().mount_lifetime,
                record_id: scope().record_id,
                frontier: scope().frontier,
            };
            let operation = Operation::Restore {
                version_id: FileVersionId(uuid::Uuid::from_u128(8)),
            };
            controller.review = Some((review, operation));
            let old = controller.confirmation_token();
            controller.cancel_review();
            controller.review = Some((review, operation));
            controller.confirm(old);
            assert!(
                controller.restore.request.is_none(),
                "retained old dialog confirmation cannot authorize a later review"
            );
            let current = controller.confirmation_token();
            controller.confirm(current);
            let admitted = controller.restore.request;
            assert!(admitted.is_some());
            controller.confirm(current);
            assert_eq!(
                controller.restore.request, admitted,
                "a valid confirmation admits exactly one intent"
            );
        }
        // phase: files_cancel_and_new_review_reject_delayed_old_review
        {
            let mut controller = controller();
            let old = controller.key;
            controller.cancel_review();
            controller.request_review(Operation::Undo {
                restore_intent: uuid::Uuid::from_u128(20),
            });
            let review = Review {
                schema_version: 1,
                workspace_id: scope().workspace_id,
                mount_lifetime: scope().mount_lifetime,
                record_id: scope().record_id,
                frontier: [5; 32],
            };
            controller
                .tx
                .send(Completion {
                    key: old,
                    reply: Reply::Review(
                        Operation::Undo {
                            restore_intent: uuid::Uuid::from_u128(19),
                        },
                        Ok(review),
                    ),
                })
                .unwrap();
            controller.next_poll = crate::time::now() + Duration::from_secs(60);
            controller.drain();
            assert!(
                controller.review.is_none(),
                "a cancelled request cannot populate a later compensation dialog"
            );
        }
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn files_generations_keep_their_intent_and_access_checks() {
        // phase: files_closed_generation_cannot_install_old_review_or_foreign_preview
        {
            let mut controller = controller();
            let old = controller.key;
            controller.suspend();
            let review = Review {
                schema_version: 1,
                workspace_id: scope().workspace_id,
                mount_lifetime: scope().mount_lifetime,
                record_id: scope().record_id,
                frontier: [5; 32],
            };
            controller
                .tx
                .send(Completion {
                    key: old,
                    reply: Reply::Review(
                        Operation::Restore {
                            version_id: FileVersionId(uuid::Uuid::from_u128(8)),
                        },
                        Ok(review),
                    ),
                })
                .unwrap();
            controller.drain();
            assert!(controller.review.is_none());
            let mut foreign = scope();
            foreign.record_id = fs_protocol::FileRecordId(uuid::Uuid::from_u128(7));
            controller
                .tx
                .send(Completion {
                    key: controller.key,
                    reply: Reply::Activity(Ok((foreign, vec![], None))),
                })
                .unwrap();
            controller.drain();
            assert!(controller.scope.is_none());
            assert!(controller.message.contains("binding changed"));
        }
        // phase: files_closed_generation_retains_original_intent_result_without_retargeting
        {
            let mut controller = controller();
            let review = Review {
                schema_version: 1,
                workspace_id: scope().workspace_id,
                mount_lifetime: scope().mount_lifetime,
                record_id: scope().record_id,
                frontier: [5; 32],
            };
            let request = controller
                .restore
                .prepare(
                    review,
                    Operation::Restore {
                        version_id: FileVersionId(uuid::Uuid::from_u128(8)),
                    },
                )
                .unwrap();
            let old = controller.key;
            controller.suspend();
            controller
                .tx
                .send(Completion {
                    key: old,
                    reply: Reply::Outcome(
                        request,
                        Outcome::RecoveryPending {
                            intent_id: request.intent_id,
                        },
                    ),
                })
                .unwrap();
            controller.drain();
            assert_eq!(controller.restore.request, Some(request));
            assert_eq!(
                controller.restore.outcome,
                Some(Outcome::RecoveryPending {
                    intent_id: request.intent_id
                })
            );
            assert!(controller.review.is_none());
            assert!(controller.after.is_none());
        }
        // phase: files_selection_during_activity_load_resumes_real_transport_access_checks
        {
            fn phase_files_selection_during_activity_load_resumes_real_transport_access_checks() {
                use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
                struct Delayed {
                    calls: AtomicUsize,
                    revoked: AtomicBool,
                    started: mpsc::Sender<()>,
                    release: std::sync::Barrier,
                    version: FileVersionV1,
                }
                impl FileHistoryTransport for Delayed {
                    fn read(&self, query: Query) -> Result<Value, Error> {
                        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                            self.started.send(()).unwrap();
                            self.release.wait();
                        }
                        if self.revoked.load(Ordering::SeqCst) {
                            return Err(Error::NotVisible);
                        }
                        match query {
                            Query::Page { .. } => {
                                Ok(Value::Page(fs_protocol::files::FileHistoryPageV1 {
                                    scope: scope(),
                                    versions: vec![self.version.clone()],
                                    total_versions: 1,
                                    heads: vec![self.version.id],
                                    current_revision: None,
                                    next_after: None,
                                }))
                            }
                            Query::Version { .. } => Ok(Value::Version {
                                scope: scope(),
                                version: self.version.clone(),
                            }),
                            _ => Err(Error::InvalidRequest),
                        }
                    }
                    fn preview(&self, _: Scope, _: FileVersionId) -> Result<Vec<u8>, Error> {
                        Ok(b"protected historical content".to_vec())
                    }
                    fn review(&self) -> Result<Review, Outcome> {
                        Err(Outcome::Unavailable)
                    }
                    fn submit(&self, _: Request) -> Outcome {
                        Outcome::Unavailable
                    }
                    fn result(&self, _: uuid::Uuid) -> Outcome {
                        Outcome::Unavailable
                    }
                }
                let (started, rx) = mpsc::channel();
                let version = FileVersionV1 {
                    id: FileVersionId(uuid::Uuid::from_u128(8)),
                    record_id: scope().record_id,
                    parents: vec![],
                    actor: fs_protocol::FileActor {
                        person: collab_types::Ed25519PublicKey::from_bytes([1; 32]),
                        device: collab_types::Ed25519PublicKey::from_bytes([2; 32]),
                    },
                    timestamp_ms: 1,
                    content_hash: Some(fs_protocol::ContentHash([3; 32])),
                    metadata_hash: fs_protocol::ContentHash([4; 32]),
                    provenance: fs_protocol::FileProvenance::Create,
                };
                let transport = Arc::new(Delayed {
                    calls: AtomicUsize::new(0),
                    revoked: AtomicBool::new(false),
                    started,
                    release: std::sync::Barrier::new(2),
                    version: version.clone(),
                });
                let mut controller = controller();
                controller.target.transport = transport.clone();
                controller.next_poll = crate::time::now() + Duration::from_secs(60);
                controller.load();
                tilo_test_env::recv("the activity load to start", &rx);
                controller.select(version.id, None);
                tilo_test_env::wait_until(
                    "the selected immutable preview while older activity is delayed",
                    || {
                        controller.drain();
                        controller.after.is_some()
                    },
                );
                transport.release.wait();
                // Wait for the delayed activity reply itself, so the assertion below
                // sees it applied rather than racing its arrival.
                tilo_test_env::wait_until("the delayed activity reply", || {
                    controller.has_results()
                });
                controller.drain();
                assert!(
                    controller.after.is_some(),
                    "stale activity must leave the selected preview pinned"
                );
                let calls = transport.calls.load(Ordering::SeqCst);
                transport.revoked.store(true, Ordering::SeqCst);
                controller.next_poll = crate::time::now();
                tilo_test_env::wait_until("the post-selection access refresh", || {
                    controller.drain();
                    controller.after.is_none()
                });
                assert!(
                    transport.calls.load(Ordering::SeqCst) > calls,
                    "actual post-selection transport access refresh must run"
                );
                assert!(controller.after.is_none());
                assert!(controller.before.is_none());
                assert!(controller.versions.is_empty());
                assert_eq!(controller.message, "This file is no longer visible");
            }
            phase_files_selection_during_activity_load_resumes_real_transport_access_checks();
        }
    }
}

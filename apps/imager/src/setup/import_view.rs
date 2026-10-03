//! Standalone later-folder import UI.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::a11y::Politeness;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::typography::TextRole;
use ui::widget::{Button, Card, Column, Container, Row, Scrollable, Text, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use super::desktop_continuation::{self, ChoiceMode, Continuation};
use super::folders;
use super::import_actions::{
    self, ContinuationProjection, FolderProjection, ImportOperation, ImportProjection,
    ImportStatusPhase, IssueProjection, OperationProjection, SelectionProjection,
};
use super::migration_store::{
    IssuePage, MigrationStore, OperationRecord, OperationState, StoreWriter,
};
use super::migration_worker::{
    MigrationJob, MigrationWorker, WorkerEvent, WorkerOutcome, durable_copy_available,
};
use super::view::system_folder_reason;
// Page rhythm (HIG §7b): the crate's steps and the shared page sections.
use super::{page, stack};
use crate::rhythm::px::{GROUP, PAGE_GUTTER, RELATED, SCROLLBAR_GUTTER, SIBLING};

static NEXT_OPERATION: AtomicU64 = AtomicU64::new(0);

/// How long a screenshot scenario waits for its migration worker. Durable
/// copies and journal fsyncs can take many seconds on a loaded disk, so this
/// only catches a worker that never finishes.
const SHOT_WAIT_BOUND: std::time::Duration = std::time::Duration::from_secs(60);

type CommandSink = Rc<RefCell<Vec<ImportCommand>>>;
type SaveReply = mpsc::Receiver<Result<Vec<OperationRecord>, String>>;

#[derive(Clone, Copy, Debug)]
enum PickTarget {
    Source,
    DestinationParent,
    ContinuedDestination(usize),
}

#[derive(Clone, Copy, Debug)]
enum ImportCommand {
    PickSource,
    CheckIdentity,
    ResolveContinued(usize),
    PickContinuedDestination(usize),
    ContinuedMode(usize, ChoiceMode),
    StartBatch,
    LaterAll,
    PickDestination,
    PrepareReview,
    Start(usize),
    Retry(usize),
    Cancel,
    ClearSelection,
    ToggleDetails(usize),
    NextIssues(usize),
    FirstIssues(usize),
}

struct PendingCopyInvocation {
    invocation: tilo_actions::InvocationId,
    operation_ids: Vec<String>,
    terminal_states: HashMap<String, OperationState>,
    cancellation_requested: bool,
    failure: Option<String>,
}

struct PendingReviewInvocation {
    invocation: tilo_actions::InvocationId,
    folder_id: String,
    cancelled: bool,
    cancellation_requested: bool,
    finished: bool,
    failure: Option<String>,
    save_reply: Option<SaveReply>,
}

struct PendingPrepareInvocation {
    invocation: tilo_actions::InvocationId,
    save_reply: SaveReply,
    operation_id: String,
    source: PathBuf,
    destination_parent: PathBuf,
}

/// A later-import surface backed by the same real journaled worker as setup.
pub struct ImportApp {
    continuation: Option<Continuation>,
    identity_ready: bool,
    review_request: Option<(usize, mpsc::Receiver<Result<Continuation, String>>)>,
    gate_request: Option<(ImportCommand, mpsc::Receiver<Result<(), String>>)>,
    batch: Vec<usize>,
    store: Option<Arc<MigrationStore>>,
    writer: Option<StoreWriter>,
    pending_saves: Vec<mpsc::Receiver<Result<Vec<OperationRecord>, String>>>,
    detail_request: Option<(usize, mpsc::Receiver<Result<IssuePage, String>>)>,
    detail_pages: HashMap<usize, IssuePage>,
    operations: Vec<OperationRecord>,
    source: Option<PathBuf>,
    destination_parent: Option<PathBuf>,
    worker: Option<(usize, MigrationWorker)>,
    pending_copy_invocation: Option<PendingCopyInvocation>,
    pending_review_invocation: Option<PendingReviewInvocation>,
    pending_prepare_invocation: Option<PendingPrepareInvocation>,
    picker: Option<(PickTarget, ui::PortalChannel)>,
    picker_launcher: fn() -> ui::PortalChannel,
    identity_checker: fn() -> Result<(), String>,
    commands: CommandSink,
    status: String,
    status_is_error: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
    structure_dirty: bool,
    expanded_details: HashSet<usize>,
    shot_temp: Option<tempfile::TempDir>,
}

impl Default for ImportApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ImportApp {
    #[must_use]
    pub fn new() -> Self {
        match MigrationStore::open_default().and_then(Self::with_store) {
            Ok(mut app) => {
                if app.continuation.is_some()
                    && let Err(error) = app.request_identity_gate(ImportCommand::CheckIdentity)
                {
                    app.status = error;
                    app.status_is_error = true;
                }
                app
            }
            Err(error) => Self::unavailable(&error),
        }
    }

    fn unavailable(error: &str) -> Self {
        Self {
            continuation: None,
            identity_ready: false,
            gate_request: None,
            review_request: None,
            batch: Vec::new(),
            store: None,
            writer: None,
            pending_saves: Vec::new(),
            detail_request: None,
            detail_pages: HashMap::new(),
            operations: Vec::new(),
            source: None,
            destination_parent: None,
            worker: None,
            pending_copy_invocation: None,
            pending_review_invocation: None,
            pending_prepare_invocation: None,
            picker: None,
            picker_launcher: folders::pick_folder,
            identity_checker: desktop_continuation::trusted_identity_complete,
            commands: Rc::new(RefCell::new(Vec::new())),
            status: format!("Folder imports are unavailable: {error}"),
            status_is_error: true,
            composed: None,
            rect: Rect::ZERO,
            structure_dirty: true,
            expanded_details: HashSet::new(),
            shot_temp: None,
        }
    }

    /// Deterministic screenshot state derived from real worker events and
    /// temporary folders. A preparation failure remains visibly unavailable.
    #[must_use]
    pub fn for_shot(scenario: &str) -> Self {
        match Self::try_for_shot(scenario) {
            Ok(app) => app,
            Err(error) => Self::unavailable(&error),
        }
    }

    fn try_for_shot(scenario: &str) -> Result<Self, String> {
        let temp = disposable_fixture_dir().map_err(|error| error.to_string())?;
        let base = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let source = base.join("Documents");
        let destination_parent = base.join("Tilo files");
        std::fs::create_dir(&source).map_err(|error| error.to_string())?;
        std::fs::create_dir(&destination_parent).map_err(|error| error.to_string())?;
        let store = MigrationStore::open_at(&base.join("state"))?;
        let mut app = Self::with_store(store)?;
        app.shot_temp = Some(temp);

        match scenario {
            "import-first-login" | "import-first-login-waiting" | "import-first-login-later" => {
                let choices = vec![super::controller::FolderChoice {
                    scan: super::folders::FolderScan {
                        source: source.clone(),
                        file_count: 1,
                        bytes: 8,
                        empty_directory_count: 0,
                        empty_directories: vec![],
                        skipped_count: 0,
                        skipped: vec![],
                    },
                    destination: super::controller::FolderDestination::Unresolved,
                    mode: super::model::FolderMode::Copy,
                    mode_overridden: false,
                }];
                std::fs::write(source.join("Welcome.txt"), b"original")
                    .map_err(|e| e.to_string())?;
                let mut c = Continuation::capture(&choices)?;
                if scenario == "import-first-login" {
                    c.resolve(0, &destination_parent, true)?;
                    app.identity_ready = true;
                } else if scenario == "import-first-login-later" {
                    c.set_mode(0, ChoiceMode::Later)?;
                    c.show_on_login = false;
                }
                c.save(app.store.as_ref().ok_or("shot store unavailable")?)?;
                app.continuation = Some(c);
                if scenario == "import-first-login" {
                    app.prepare_continued_folder(0)?;
                }
                app.status = if app.identity_ready {
                    "Review the resolved source and destination, then choose Copy."
                } else if scenario == "import-first-login-later" {
                    "Your choices are saved. Return to Add folders to Tilo whenever you are ready."
                } else {
                    "Finish setting up your Tilo identity to continue. Your choices remain pending."
                }
                .into();
            }
            "import-progress" => app.seed_progress_shot(&source, &destination_parent)?,
            "import-retry-progress" => {
                app.seed_retry_progress_shot(&source, &destination_parent)?;
            }
            "import-partial" => app.seed_partial_shot(&source, &destination_parent)?,
            "import-conflict" => app.seed_conflict_shot(&source, &destination_parent)?,
            "import-issue-pages" => {
                std::fs::write(source.join("Welcome.txt"), b"verified bytes")
                    .map_err(|e| e.to_string())?;
                #[cfg(unix)]
                for index in 0..40 {
                    std::os::unix::fs::symlink(
                        source.join("Welcome.txt"),
                        source.join(format!("Shortcut {index:02}")),
                    )
                    .map_err(|e| e.to_string())?;
                }
                app.prepare_and_start_shot(&source, &destination_parent)?;
                let deadline = ui::time::now() + SHOT_WAIT_BOUND;
                app.wait_for_shot_terminal(deadline)?;
                app.expanded_details.insert(0);
                app.load_issue_page(0, 0)?;
                while app.detail_request.is_some() {
                    app.poll_background();
                    if ui::time::now() > deadline {
                        return Err("issue-page screenshot timed out".into());
                    }
                    std::thread::yield_now();
                }
            }
            _ => {
                std::fs::write(source.join("Welcome.txt"), b"verified import")
                    .map_err(|error| error.to_string())?;
                app.prepare_and_start_shot(&source, &destination_parent)?;
                app.wait_for_shot_terminal(ui::time::now() + SHOT_WAIT_BOUND)?;
            }
        }
        app.structure_dirty = true;
        Ok(app)
    }

    fn seed_progress_shot(
        &mut self,
        source: &Path,
        destination_parent: &Path,
    ) -> Result<(), String> {
        for index in 0..24 {
            std::fs::write(
                source.join(format!("Archive {index:02}.bin")),
                vec![index as u8; 1024 * 1024],
            )
            .map_err(|error| error.to_string())?;
        }
        self.prepare_and_start_shot(source, destination_parent)?;
        let deadline = ui::time::now() + SHOT_WAIT_BOUND;
        loop {
            self.poll_background();
            if self
                .operations
                .first()
                .is_some_and(|operation| operation.verified_file_count() > 0)
            {
                let snapshot = self.operations.first().cloned();
                self.cancel_active();
                self.wait_for_shot_terminal(deadline)?;
                if let Some(snapshot) = snapshot
                    && let Some(operation) = self.operations.first_mut()
                {
                    *operation = snapshot;
                }
                self.status = "Copying and checking files…".into();
                self.status_is_error = false;
                return Ok(());
            }
            if ui::time::now() >= deadline {
                return Err("progress screenshot worker timed out".into());
            }
            std::thread::yield_now();
        }
    }

    fn seed_partial_shot(
        &mut self,
        source: &Path,
        destination_parent: &Path,
    ) -> Result<(), String> {
        std::fs::write(source.join("Welcome.txt"), b"kept and verified")
            .map_err(|error| error.to_string())?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(source.join("Welcome.txt"), source.join("Old shortcut"))
            .map_err(|error| error.to_string())?;
        self.prepare_and_start_shot(source, destination_parent)?;
        self.wait_for_shot_terminal(ui::time::now() + SHOT_WAIT_BOUND)
    }

    fn seed_retry_progress_shot(
        &mut self,
        source: &Path,
        destination_parent: &Path,
    ) -> Result<(), String> {
        for index in 0..24 {
            std::fs::write(
                source.join(format!("Archive {index:02}.bin")),
                vec![index as u8; 1024 * 1024],
            )
            .map_err(|error| error.to_string())?;
        }
        self.prepare_and_start_shot(source, destination_parent)?;
        let deadline = ui::time::now() + SHOT_WAIT_BOUND;
        loop {
            self.poll_background();
            if self
                .operations
                .first()
                .is_some_and(|operation| operation.verified_file_count() > 0)
            {
                self.cancel_active();
                self.wait_for_shot_terminal(deadline)?;
                break;
            }
            if ui::time::now() >= deadline {
                return Err("retry screenshot setup timed out".into());
            }
            std::thread::yield_now();
        }
        if self.operations.first().map(OperationRecord::state) != Some(OperationState::Cancelled) {
            return Err("retry screenshot did not retain a stopped attempt".into());
        }
        self.retry_operation(0)?;
        loop {
            self.poll_background();
            if self.operations.first().is_some_and(|operation| {
                operation.state() == OperationState::Running
                    && operation.verified_file_count() > 0
                    && operation.issues().is_ok_and(|issues| issues.is_empty())
            }) {
                let snapshot = self.operations.first().cloned();
                self.cancel_active();
                self.wait_for_shot_terminal(deadline)?;
                if let Some(snapshot) = snapshot
                    && let Some(operation) = self.operations.first_mut()
                {
                    *operation = snapshot;
                }
                self.status = "Continuing copy from checked progress…".into();
                self.status_is_error = false;
                return Ok(());
            }
            if ui::time::now() >= deadline || self.worker.is_none() {
                return Err("retry screenshot worker finished before progress was captured".into());
            }
            std::thread::yield_now();
        }
    }

    fn seed_conflict_shot(
        &mut self,
        source: &Path,
        destination_parent: &Path,
    ) -> Result<(), String> {
        std::fs::write(source.join("Budget.txt"), b"source version")
            .map_err(|error| error.to_string())?;
        self.prepare_and_start_shot(source, destination_parent)?;
        self.wait_for_shot_terminal(ui::time::now() + SHOT_WAIT_BOUND)?;
        let completed = self
            .operations
            .first()
            .ok_or_else(|| "conflict screenshot lost its operation".to_string())?;
        let destination = completed.destination()?;
        let journal = completed.journal()?;
        std::fs::write(destination.join("Budget.txt"), b"destination edit")
            .map_err(|error| error.to_string())?;
        let fresh = MigrationJob::fresh(
            "shot-conflict-resume",
            source.to_path_buf(),
            destination,
            journal,
        )?;
        let retry = fresh.retry()?;
        let mut conflict = OperationRecord::new(&fresh);
        conflict.approve();
        conflict.mark_running()?;
        Self::run_shot_worker(&mut conflict, retry)?;
        self.operations = vec![conflict];
        self.status = "Destination changes were preserved for review.".into();
        self.status_is_error = true;
        Ok(())
    }

    fn prepare_and_start_shot(
        &mut self,
        source: &Path,
        destination_parent: &Path,
    ) -> Result<(), String> {
        self.source = Some(source.to_path_buf());
        self.destination_parent = Some(destination_parent.to_path_buf());
        self.prepare_review()?;
        self.start_copy(0)
    }

    fn wait_for_shot_terminal(&mut self, deadline: ui::time::Instant) -> Result<(), String> {
        while self.worker.is_some() {
            self.poll_background();
            if ui::time::now() >= deadline {
                return Err("screenshot worker timed out".into());
            }
            std::thread::yield_now();
        }
        Ok(())
    }

    fn run_shot_worker(operation: &mut OperationRecord, job: MigrationJob) -> Result<(), String> {
        let mut worker = MigrationWorker::start(job)?;
        let deadline = ui::time::now() + SHOT_WAIT_BOUND;
        loop {
            if let Some(event) = worker.try_next() {
                match event {
                    WorkerEvent::Stored(saved) => {
                        *operation = *saved;
                        return Ok(());
                    }
                    WorkerEvent::Progress(progress) => operation.apply_progress(&progress),
                    WorkerEvent::Finished(outcome) => {
                        operation.apply_outcome(outcome);
                        return Ok(());
                    }
                }
            }
            if ui::time::now() >= deadline {
                operation.apply_outcome(WorkerOutcome::Interrupted);
                return Err("screenshot worker timed out".into());
            }
            std::thread::yield_now();
        }
    }

    fn with_store(store: MigrationStore) -> Result<Self, String> {
        let continuation = Continuation::load(&store)?;
        let mut operations = store.load()?;
        // Persist the load-time Running -> Interrupted normalization. This is
        // an index update only; retry still goes through journal revalidation.
        let store = Arc::new(store);
        let writer = StoreWriter::new(Arc::clone(&store))?;
        let pending_saves = vec![writer.save(operations.clone())?];
        for operation in &mut operations {
            operation.compact();
        }
        let status = if operations.is_empty() {
            "Choose a source folder and destination to prepare a one-time copy.".into()
        } else {
            "Previous imports reopened. Interrupted work requires an explicit retry.".into()
        };
        Ok(Self {
            continuation,
            identity_ready: false,
            gate_request: None,
            review_request: None,
            batch: Vec::new(),
            store: Some(store),
            writer: Some(writer),
            pending_saves,
            detail_request: None,
            detail_pages: HashMap::new(),
            operations,
            source: None,
            destination_parent: None,
            worker: None,
            pending_copy_invocation: None,
            pending_review_invocation: None,
            pending_prepare_invocation: None,
            picker: None,
            picker_launcher: folders::pick_folder,
            identity_checker: desktop_continuation::trusted_identity_complete,
            commands: Rc::new(RefCell::new(Vec::new())),
            status,
            status_is_error: false,
            composed: None,
            rect: Rect::ZERO,
            structure_dirty: true,
            expanded_details: HashSet::new(),
            shot_temp: None,
        })
    }

    fn operation_index(&self, operation_id: &str) -> Option<usize> {
        self.operations
            .iter()
            .position(|operation| operation.id() == operation_id)
    }

    fn folder_index(&self, folder_id: &str) -> Option<usize> {
        let continuation = self.continuation.as_ref()?;
        (0..continuation.folders.len()).find(|index| continuation.operation_id(*index) == folder_id)
    }

    fn can_retry_operation(&self, index: usize) -> bool {
        if self.worker.is_some()
            || self.review_request.is_some()
            || self.pending_copy_invocation.is_some()
            || self.pending_prepare_invocation.is_some()
            || self.pending_review_invocation.is_some()
        {
            return false;
        }
        let Some(operation) = self.operations.get(index) else {
            return false;
        };
        if self.validate_continued_operation(index).is_err() {
            return false;
        }
        if operation.retry_job().is_ok() {
            return true;
        }
        let Some(folder) = self.continued_index(index) else {
            return false;
        };
        self.continuation.as_ref().is_some_and(|continuation| {
            continuation
                .unpublished_retry(folder, operation)
                .is_ok_and(|job| job.is_some())
        })
    }

    fn can_start_copy(&self, index: usize) -> bool {
        self.operations
            .get(index)
            .is_some_and(|record| record.state() == OperationState::AwaitingReview)
            && self.worker.is_none()
            && self.review_request.is_none()
            && self.gate_request.is_none()
            && self.pending_copy_invocation.is_none()
            && self.pending_review_invocation.is_none()
            && self.pending_prepare_invocation.is_none()
            && self.validate_continued_operation(index).is_ok()
            && self.continued_index(index).is_none_or(|folder| {
                self.continuation
                    .as_ref()
                    .and_then(|continuation| continuation.folders.get(folder))
                    .is_some_and(|choice| choice.mode == ChoiceMode::Copy)
            })
            && durable_copy_available().is_ok()
    }

    fn can_prepare_review(&self) -> bool {
        self.source.is_some()
            && self.destination_parent.is_some()
            && self.store.is_some()
            && self.worker.is_none()
            && self.review_request.is_none()
            && self.gate_request.is_none()
            && self.picker.is_none()
            && self.pending_copy_invocation.is_none()
            && self.pending_review_invocation.is_none()
            && self.pending_prepare_invocation.is_none()
            && self.pending_saves.len() < 2
    }

    fn import_phase(&self) -> ImportStatusPhase {
        if self.store.is_none() {
            ImportStatusPhase::Unavailable
        } else if self.worker.is_some() {
            ImportStatusPhase::Copying
        } else if self.review_request.is_some() {
            ImportStatusPhase::Reviewing
        } else if self.gate_request.is_some() {
            ImportStatusPhase::CheckingIdentity
        } else if self.picker.is_some() {
            ImportStatusPhase::ChoosingFolder
        } else if self.detail_request.is_some() {
            ImportStatusPhase::LoadingDetails
        } else if !self.pending_saves.is_empty()
            || self.pending_prepare_invocation.is_some()
            || self
                .pending_review_invocation
                .as_ref()
                .is_some_and(|pending| pending.save_reply.is_some())
        {
            ImportStatusPhase::Saving
        } else if self.status_is_error {
            ImportStatusPhase::Error
        } else {
            ImportStatusPhase::Ready
        }
    }

    fn issue_projection(&self, index: usize) -> Vec<IssueProjection> {
        if !self.expanded_details.contains(&index) {
            return Vec::new();
        }
        let loaded = self.detail_pages.get(&index).map(|page| &page.issues);
        let preview = self
            .operations
            .get(index)
            .and_then(|operation| operation.issues().ok());
        loaded
            .map_or_else(|| preview.as_deref().unwrap_or(&[]), Vec::as_slice)
            .iter()
            .take(32)
            .enumerate()
            .map(|(number, (_, message))| IssueProjection {
                number: number + 1,
                message: message.chars().take(512).collect(),
            })
            .collect()
    }

    fn can_load_next_issues(&self, index: usize) -> bool {
        self.expanded_details.contains(&index)
            && self.detail_request.is_none()
            && self
                .detail_pages
                .get(&index)
                .is_some_and(|page| page.next.is_some())
    }

    fn can_load_first_issues(&self, index: usize) -> bool {
        self.expanded_details.contains(&index)
            && self.detail_request.is_none()
            && self
                .detail_pages
                .get(&index)
                .is_some_and(|page| page.offset > 0 && page.next.is_none())
    }

    fn import_projection(&self) -> ImportProjection {
        let active_operation = self
            .worker
            .as_ref()
            .and_then(|(index, _)| self.operations.get(*index))
            .map(|operation| operation.id().to_string());
        let operations = self
            .operations
            .iter()
            .enumerate()
            .map(|(index, operation)| {
                let details_available = operation.issue_count() > 0
                    || !operation.metadata_limitations().is_empty()
                    || operation.failure().is_some();
                let start_available = self.can_start_copy(index);
                OperationProjection {
                    identity: operation.id().to_string(),
                    state: operation_state_key(operation.state()),
                    verified_files: operation.verified_file_count(),
                    verified_bytes: operation.verified_bytes(),
                    issue_count: operation.issue_count(),
                    review_needed: operation.state() == OperationState::AwaitingReview,
                    details_open: self.expanded_details.contains(&index),
                    details_loading: self
                        .detail_request
                        .as_ref()
                        .is_some_and(|(loading, _)| *loading == index),
                    issues: self.issue_projection(index),
                    next_issues_available: self.can_load_next_issues(index),
                    first_issues_available: self.can_load_first_issues(index),
                    start_available,
                    retry_available: self.can_retry_operation(index)
                        && durable_copy_available().is_ok(),
                    cancel_available: self
                        .worker
                        .as_ref()
                        .is_some_and(|(active, _)| *active == index),
                    details_available,
                }
            })
            .collect();
        let continuation = self.continuation.as_ref().map(|continuation| {
            let folders = continuation
                .folders
                .iter()
                .enumerate()
                .map(|(index, folder)| {
                    let review_available = self.check_continued_review_allowed(index).is_ok()
                        && folder.mode == ChoiceMode::Copy;
                    FolderProjection {
                        identity: continuation.operation_id(index),
                        number: index + 1,
                        mode: choice_mode_key(folder.mode),
                        destination_reviewed: folder.destination().is_ok_and(|path| path.is_some()),
                        review_available,
                        picker_available: review_available && self.picker.is_none(),
                        mode_available: self.worker.is_none()
                            && self.review_request.is_none()
                            && self.gate_request.is_none(),
                    }
                })
                .collect();
            ContinuationProjection {
                identity_ready: self.identity_ready,
                identity_check_available: !self.identity_ready && self.gate_request.is_none(),
                start_batch_available: self.can_start_continued_batch(),
                later_all_available: self.worker.is_none()
                    && self.review_request.is_none()
                    && self.gate_request.is_none(),
                folders,
            }
        });
        ImportProjection {
            phase: self.import_phase(),
            status_is_error: self.status_is_error,
            selection: SelectionProjection {
                source_selected: self.source.is_some(),
                destination_selected: self.destination_parent.is_some(),
                review_ready: self.can_prepare_review(),
            },
            active_operation,
            operations,
            continuation,
            picker_available: self.picker.is_none() && self.store.is_some(),
            clear_available: self.source.is_some() || self.destination_parent.is_some(),
            prepare_review_available: self.can_prepare_review(),
        }
    }

    /// A private status response for the local owner. The public resource
    /// projection and Effect receipts remain path-free. The host refuses
    /// directed View actions from remote actors (`private_view` in
    /// `ui-core/src/app/actions.rs`), covered by its directed-view test.
    fn local_status_value(&self) -> serde_json::Value {
        let mut value = self.import_projection().value();
        let source = self.source.as_ref().map(|path| path.display().to_string());
        let destination = self.destination_parent.as_ref().map(|parent| {
            self.source
                .as_ref()
                .and_then(|source| source.file_name())
                .map_or_else(
                    || parent.display().to_string(),
                    |name| parent.join(name).display().to_string(),
                )
        });
        value["local"] = serde_json::json!({
            "source": source,
            "destination": destination,
            "status": self.status,
        });
        for (index, operation) in self.operations.iter().enumerate() {
            value["operations"][index]["source"] = serde_json::json!(
                operation
                    .source()
                    .ok()
                    .map(|path| path.display().to_string())
            );
            value["operations"][index]["destination"] = serde_json::json!(
                operation
                    .destination()
                    .ok()
                    .map(|path| path.display().to_string())
            );
            if self.expanded_details.contains(&index) {
                let preview = operation.issues().ok();
                let issues = self
                    .detail_pages
                    .get(&index)
                    .map_or_else(|| preview.as_deref().unwrap_or(&[]), |page| &page.issues);
                for (number, (path, _)) in issues.iter().take(32).enumerate() {
                    value["operations"][index]["issues"][number]["path"] =
                        serde_json::json!(path.display().to_string());
                }
            }
        }
        if let Some(continuation) = &self.continuation {
            for (index, folder) in continuation.folders.iter().enumerate() {
                value["continuation"]["folders"][index]["source"] =
                    serde_json::json!(folder.source().ok().map(|path| path.display().to_string()));
                value["continuation"]["folders"][index]["destination"] = serde_json::json!(
                    folder
                        .destination()
                        .ok()
                        .flatten()
                        .map(|path| path.display().to_string())
                );
            }
        }
        value
    }

    fn refuse_semantic<T>(&mut self, message: String) -> Result<T, String> {
        self.status.clone_from(&message);
        self.status_is_error = true;
        self.structure_dirty = true;
        Err(message)
    }

    fn apply_semantic_command(&mut self, command: ImportCommand) -> Result<(), String> {
        self.status_is_error = false;
        self.apply_command(command);
        if self.status_is_error {
            Err(super::strings::t("imager-import-error-command-refused"))
        } else {
            Ok(())
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive admission map keeps each semantic operation beside its fresh-state validation"
    )]
    fn invoke_import_operation(
        &mut self,
        operation: ImportOperation,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let command = match operation {
            ImportOperation::Observe => {
                return Ok(tilo_actions::ActionOutput {
                    value: Some(self.local_status_value()),
                    resources: Vec::new(),
                });
            }
            ImportOperation::PickSource => {
                if self.picker.is_some() || self.store.is_none() {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-picker-unavailable",
                    ));
                }
                ImportCommand::PickSource
            }
            ImportOperation::PickDestination => {
                if self.picker.is_some() || self.store.is_none() {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-picker-unavailable",
                    ));
                }
                ImportCommand::PickDestination
            }
            ImportOperation::ClearSelection => {
                if self.source.is_none() && self.destination_parent.is_none() {
                    return self
                        .refuse_semantic(super::strings::t("imager-import-error-selection-empty"));
                }
                ImportCommand::ClearSelection
            }
            ImportOperation::PrepareReview => ImportCommand::PrepareReview,
            ImportOperation::CheckIdentity => {
                if self.continuation.is_none() || self.identity_ready || self.gate_request.is_some()
                {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-identity-check-unavailable",
                    ));
                }
                ImportCommand::CheckIdentity
            }
            ImportOperation::StartReviewedBatch => {
                if !self.can_start_continued_batch() {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-batch-unavailable",
                    ));
                }
                ImportCommand::StartBatch
            }
            ImportOperation::LaterAll => {
                if self.continuation.is_none()
                    || self.worker.is_some()
                    || self.review_request.is_some()
                    || self.gate_request.is_some()
                {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-later-all-unavailable",
                    ));
                }
                ImportCommand::LaterAll
            }
            ImportOperation::Start { operation_id } => {
                let Some(index) = self.operation_index(&operation_id) else {
                    return self
                        .refuse_semantic(super::strings::t("imager-import-error-stale-operation"));
                };
                if !self.can_start_copy(index) {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-start-unavailable",
                    ));
                }
                ImportCommand::Start(index)
            }
            ImportOperation::Retry { operation_id } => {
                let Some(index) = self.operation_index(&operation_id) else {
                    return self
                        .refuse_semantic(super::strings::t("imager-import-error-stale-operation"));
                };
                if !self.can_retry_operation(index) {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-retry-unavailable",
                    ));
                }
                ImportCommand::Retry(index)
            }
            ImportOperation::Cancel { operation_id } => {
                let Some(index) = self.operation_index(&operation_id) else {
                    return self
                        .refuse_semantic(super::strings::t("imager-import-error-stale-operation"));
                };
                if self
                    .worker
                    .as_ref()
                    .is_none_or(|(active, _)| *active != index)
                {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-cancel-unavailable",
                    ));
                }
                ImportCommand::Cancel
            }
            ImportOperation::ToggleDetails { operation_id } => {
                let Some(index) = self.operation_index(&operation_id) else {
                    return self
                        .refuse_semantic(super::strings::t("imager-import-error-stale-operation"));
                };
                let available = self.operations.get(index).is_some_and(|record| {
                    record.issue_count() > 0
                        || !record.metadata_limitations().is_empty()
                        || record.failure().is_some()
                });
                if !available {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-details-unavailable",
                    ));
                }
                ImportCommand::ToggleDetails(index)
            }
            ImportOperation::NextIssues { operation_id } => {
                let Some(index) = self.operation_index(&operation_id) else {
                    return self
                        .refuse_semantic(super::strings::t("imager-import-error-stale-operation"));
                };
                if !self.can_load_next_issues(index) {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-next-issues-unavailable",
                    ));
                }
                ImportCommand::NextIssues(index)
            }
            ImportOperation::FirstIssues { operation_id } => {
                let Some(index) = self.operation_index(&operation_id) else {
                    return self
                        .refuse_semantic(super::strings::t("imager-import-error-stale-operation"));
                };
                if !self.can_load_first_issues(index) {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-first-issues-unavailable",
                    ));
                }
                ImportCommand::FirstIssues(index)
            }
            ImportOperation::ResolveFolder { folder_id } => {
                let Some(index) = self.folder_index(&folder_id) else {
                    return self
                        .refuse_semantic(super::strings::t("imager-import-error-stale-folder"));
                };
                if self.check_continued_review_allowed(index).is_err()
                    || !self
                        .continuation
                        .as_ref()
                        .and_then(|continuation| continuation.folders.get(index))
                        .is_some_and(|folder| folder.mode == ChoiceMode::Copy)
                {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-folder-review-unavailable",
                    ));
                }
                ImportCommand::ResolveContinued(index)
            }
            ImportOperation::PickFolderDestination { folder_id } => {
                let Some(index) = self.folder_index(&folder_id) else {
                    return self
                        .refuse_semantic(super::strings::t("imager-import-error-stale-folder"));
                };
                if self.picker.is_some()
                    || self.check_continued_review_allowed(index).is_err()
                    || !self
                        .continuation
                        .as_ref()
                        .and_then(|continuation| continuation.folders.get(index))
                        .is_some_and(|folder| folder.mode == ChoiceMode::Copy)
                {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-picker-unavailable",
                    ));
                }
                ImportCommand::PickContinuedDestination(index)
            }
            ImportOperation::SetFolderCopy { folder_id } => {
                let Some(index) = self.folder_index(&folder_id) else {
                    return self
                        .refuse_semantic(super::strings::t("imager-import-error-stale-folder"));
                };
                if self.worker.is_some()
                    || self.review_request.is_some()
                    || self.gate_request.is_some()
                {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-folder-mode-unavailable",
                    ));
                }
                ImportCommand::ContinuedMode(index, ChoiceMode::Copy)
            }
            ImportOperation::SetFolderLater { folder_id } => {
                let Some(index) = self.folder_index(&folder_id) else {
                    return self
                        .refuse_semantic(super::strings::t("imager-import-error-stale-folder"));
                };
                if self.worker.is_some()
                    || self.review_request.is_some()
                    || self.gate_request.is_some()
                {
                    return self.refuse_semantic(super::strings::t(
                        "imager-import-error-folder-mode-unavailable",
                    ));
                }
                ImportCommand::ContinuedMode(index, ChoiceMode::Later)
            }
            ImportOperation::SetFolderConnected { folder_id } => {
                if self.folder_index(&folder_id).is_none() {
                    return self
                        .refuse_semantic(super::strings::t("imager-import-error-stale-folder"));
                }
                return self.refuse_semantic(super::strings::t(
                    "imager-import-error-connected-unavailable",
                ));
            }
        };
        self.apply_semantic_command(command)?;
        Ok(tilo_actions::ActionOutput {
            value: Some(self.import_projection().value()),
            resources: Vec::new(),
        })
    }

    fn persist(&mut self) -> Result<(), String> {
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| "private migration state is unavailable".to_string())?;
        self.pending_saves
            .push(writer.save(self.operations.clone())?);
        Ok(())
    }

    fn load_issue_page(&mut self, index: usize, offset: u64) -> Result<(), String> {
        if self.detail_request.is_some() {
            return Err("Copy details are still loading.".into());
        }
        let store = Arc::clone(
            self.store
                .as_ref()
                .ok_or_else(|| "private migration state is unavailable".to_string())?,
        );
        let operation = self
            .operations
            .get(index)
            .cloned()
            .ok_or_else(|| "copy operation is unavailable".to_string())?;
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("imager-copy-details".into())
            .spawn(move || {
                let _ = tx.send(store.issue_page(&operation, offset));
            })
            .map_err(|e| e.to_string())?;
        self.detail_request = Some((index, rx));
        Ok(())
    }

    fn load_next_issue_page(&mut self, index: usize) -> Result<(), String> {
        if !self.can_load_next_issues(index) {
            return Err(super::strings::t(
                "imager-import-error-next-issues-unavailable",
            ));
        }
        let offset = self
            .detail_pages
            .get(&index)
            .and_then(|page| page.next)
            .ok_or_else(|| super::strings::t("imager-import-error-next-issues-unavailable"))?;
        self.load_issue_page(index, offset)
    }

    fn load_first_issue_page(&mut self, index: usize) -> Result<(), String> {
        if !self.can_load_first_issues(index) {
            return Err(super::strings::t(
                "imager-import-error-first-issues-unavailable",
            ));
        }
        self.load_issue_page(index, 0)
    }

    fn prepare_review(&mut self) -> Result<(), String> {
        if self.pending_prepare_invocation.is_some()
            || self.pending_review_invocation.is_some()
            || self.pending_copy_invocation.is_some()
            || self.gate_request.is_some()
            || self.picker.is_some()
        {
            return Err("Wait for the previous folder action to finish.".into());
        }
        if self.worker.is_some() {
            return Err("Wait for the active copy before preparing another import.".into());
        }
        if self.pending_saves.len() >= 2 {
            return Err("Wait for local copy state to finish saving.".into());
        }
        let source = self
            .source
            .clone()
            .ok_or_else(|| "choose a source folder first".to_string())?;
        let destination_parent = self
            .destination_parent
            .clone()
            .ok_or_else(|| "choose a destination parent first".to_string())?;
        if let Some(reason) = system_folder_reason(&source) {
            return Err(format!(
                "{} is a {reason} and cannot be imported",
                source.display()
            ));
        }
        if let Some(reason) = system_folder_reason(&destination_parent) {
            return Err(format!(
                "{} is a {reason} and cannot receive an import",
                destination_parent.display()
            ));
        }
        let name = source
            .file_name()
            .ok_or_else(|| "the selected source has no folder name".to_string())?;
        let destination = destination_parent.join(name);
        if destination.exists() {
            return Err(format!(
                "{} already exists; choose a destination without a folder of this name",
                destination.display()
            ));
        }
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| "private migration state is unavailable".to_string())?;
        let id = format!(
            "{}-{}-{}",
            std::process::id(),
            ui::time::wall()
                .duration_since(ui::time::UNIX_EPOCH)
                .map_err(|_| "system time is before the Unix epoch".to_string())?
                .as_nanos(),
            NEXT_OPERATION.fetch_add(1, Ordering::Relaxed)
        );
        let journal = store.root().join(format!("{id}.journal"));
        let job = MigrationJob::fresh(id, source, destination, journal)?;
        self.operations.push(OperationRecord::new(&job));
        if let Err(error) = self.persist() {
            self.operations.pop();
            return Err(format!("review could not be saved: {error}"));
        }
        self.source = None;
        self.destination_parent = None;
        self.status = "Review the source, destination, and one-time copy.".into();
        self.status_is_error = false;
        self.structure_dirty = true;
        Ok(())
    }

    fn continued_index(&self, operation_index: usize) -> Option<usize> {
        let c = self.continuation.as_ref()?;
        let op = self.operations.get(operation_index)?;
        (0..c.folders.len()).find(|i| c.operation_id(*i) == op.id())
    }

    fn validate_continued_operation(&self, index: usize) -> Result<(), String> {
        if self.continuation.is_some() && !self.identity_ready {
            return Err("Finish setting up your Tilo identity to continue.".into());
        }
        if let Some(folder) = self.continued_index(index) {
            self.continuation
                .as_ref()
                .ok_or("continuation unavailable")?
                .validate_endpoints(folder, self.identity_ready)?;
        }
        Ok(())
    }

    fn prepare_continued_folder(&mut self, index: usize) -> Result<(), String> {
        if self.worker.is_some() {
            return Err("Wait for the active copy before reviewing another folder.".into());
        }
        let c = self.continuation.as_ref().ok_or("No saved folder review")?;
        let store = self
            .store
            .as_ref()
            .ok_or("Private migration state unavailable")?;
        // Persist resolution before materializing. A crash after the operation
        // write but before UI acknowledgement will find the same stable ID.
        let prepared = c.prepare(index, store, &self.operations, self.identity_ready)?;
        c.save(store)?;
        if let Some(op) = prepared {
            if let Some(existing) = self.operations.iter_mut().find(|old| old.id() == op.id()) {
                *existing = op;
            } else {
                self.operations.push(op);
            }
            self.persist()?;
        }
        Ok(())
    }

    fn check_continued_review_allowed(&self, index: usize) -> Result<(), String> {
        if !self.identity_ready {
            return Err("Finish identity setup in Tilo first.".into());
        }
        if self.worker.is_some() || self.review_request.is_some() || self.gate_request.is_some() {
            return Err("Wait for the active copy or folder review.".into());
        }
        let c = self.continuation.as_ref().ok_or("No saved folder review")?;
        if let Some(op) = self
            .operations
            .iter()
            .find(|op| op.id() == c.operation_id(index))
        {
            Continuation::can_replace_unpublished(op)?;
        }
        Ok(())
    }

    fn resolve_continued(&mut self, index: usize) -> Result<(), String> {
        self.request_continued_review(index, None)
    }

    fn request_continued_review(
        &mut self,
        index: usize,
        selected: Option<PathBuf>,
    ) -> Result<(), String> {
        self.check_continued_review_allowed(index)?;
        let mut c = self.continuation.clone().ok_or("No saved folder review")?;
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("imager-folder-review".into())
            .spawn(move || {
                let result = (|| {
                    let parent = if let Some(parent) = selected {
                        c.select_destination_parent(index, &parent)?;
                        parent
                    } else {
                        let parent = tilo_paths::data_local_dir()
                            .ok_or("No local user data directory")?
                            .join("tilo/files");
                        super::migration_store::ensure_private_root(&parent)
                            .map_err(|e| e.to_string())?;
                        parent
                    };
                    c.resolve(index, &parent, true)?;
                    Ok(c)
                })();
                let _ = sender.send(result);
            })
            .map_err(|e| e.to_string())?;
        self.review_request = Some((index, receiver));
        self.status = "Reviewing source changes and the destination…".into();
        Ok(())
    }

    fn poll_continued_review(&mut self) -> bool {
        let Some((index, receiver)) = &self.review_request else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Folder review stopped; review again.".into())
            }
        };
        let index = *index;
        self.review_request = None;
        if let Some(pending) = &mut self.pending_review_invocation
            && pending.cancellation_requested
        {
            pending.cancelled = true;
            pending.finished = true;
            self.status = "Folder review stopped before it was saved.".into();
            self.structure_dirty = true;
            return true;
        }
        let previous_save_count = self.pending_saves.len();
        let result = result.and_then(|c| {
            self.check_continued_review_allowed(index)?;
            let previous = self.continuation.replace(c);
            if let Err(error) = self.prepare_continued_folder(index) {
                self.continuation = previous;
                return Err(error);
            }
            Ok(())
        });
        if let Some(pending) = &mut self.pending_review_invocation {
            pending.finished = true;
            if self.pending_saves.len() > previous_save_count {
                pending.save_reply = self.pending_saves.pop();
            }
            if let Err(error) = &result {
                pending.failure = Some(error.clone());
            }
        }
        match result {
            Ok(()) => {
                self.status =
                    "Review both paths below. Copy starts only when you choose Copy.".into();
                self.status_is_error = false;
            }
            Err(error) => {
                self.status = error;
                self.status_is_error = true;
            }
        }
        self.structure_dirty = true;
        true
    }

    fn save_continued_mode(&mut self, index: usize, mode: ChoiceMode) -> Result<(), String> {
        if self.worker.is_some() || self.gate_request.is_some() {
            return Err("Stop the running copy before changing folder choices.".into());
        }
        let c = self.continuation.as_mut().ok_or("No saved folder review")?;
        c.set_mode(index, mode)?;
        c.show_on_login = c.folders.iter().any(|f| f.mode != ChoiceMode::Later);
        c.save(
            self.store
                .as_ref()
                .ok_or("Private migration state unavailable")?,
        )?;
        Ok(())
    }

    fn request_identity_gate(&mut self, command: ImportCommand) -> Result<(), String> {
        if self.gate_request.is_some() {
            return Err("Waiting for the Tilo session check.".into());
        }
        let identity_checker = self.identity_checker;
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("import-session-check".into())
            .spawn(move || {
                let _ = tx.send(identity_checker());
            })
            .map_err(|e| e.to_string())?;
        self.gate_request = Some((command, rx));
        self.status = "Checking whether Tilo identity setup is complete…".into();
        Ok(())
    }

    fn start_copy(&mut self, index: usize) -> Result<(), String> {
        if self.pending_prepare_invocation.is_some() || self.pending_review_invocation.is_some() {
            return Err("Wait for the folder review to finish saving before copying.".into());
        }
        if let Some(pending) = &self.pending_copy_invocation
            && self
                .operations
                .get(index)
                .is_none_or(|record| !pending.operation_ids.iter().any(|id| id == record.id()))
        {
            return Err("Wait for the previous copy result before starting another folder.".into());
        }
        self.validate_continued_operation(index)?;
        if !self
            .operations
            .get(index)
            .is_some_and(|o| o.state() == OperationState::AwaitingReview)
        {
            return Err(
                "This copy already has an outcome; use its explicit retry action if needed.".into(),
            );
        }
        if self.worker.is_some() || self.review_request.is_some() {
            return Err("another folder import is already running".into());
        }
        durable_copy_available()
            .map_err(|error| format!("Folder copying is unavailable on this platform: {error}"))?;
        let continued_index = self.continued_index(index);
        if let (Some(c), Some(folder)) = (&self.continuation, continued_index) {
            if c.folders
                .get(folder)
                .is_none_or(|choice| choice.mode != ChoiceMode::Copy)
            {
                return Err("Choose Copy for this folder before starting it.".into());
            }
            c.validate_fresh_destination(folder)?;
        }
        let mut operation = self
            .operations
            .get(index)
            .cloned()
            .ok_or_else(|| "the selected migration operation no longer exists".to_string())?;
        operation.approve();
        let job = operation.start_job()?;
        let job = if let (Some(c), Some(folder)) = (&self.continuation, continued_index) {
            c.constrain_job(folder, job)?
        } else {
            job
        };
        operation.mark_running()?;
        *self
            .operations
            .get_mut(index)
            .ok_or("copy operation disappeared")? = operation;
        self.detail_pages.remove(&index);
        self.detail_request = None;
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| "private migration state is unavailable".to_string())?
            .clone();
        match MigrationWorker::start_persisted(job, writer, self.operations.clone(), index) {
            Ok(worker) => {
                self.worker = Some((index, worker));
                self.status = "Copying and checking files…".into();
                self.status_is_error = false;
                self.structure_dirty = true;
                Ok(())
            }
            Err(error) => {
                if let Some(operation) = self.operations.get_mut(index) {
                    operation.apply_outcome(super::migration_worker::WorkerOutcome::Failed(
                        error.clone(),
                    ));
                }
                let _ = self.persist();
                Err(error)
            }
        }
    }

    fn retry_operation(&mut self, index: usize) -> Result<(), String> {
        if self.pending_copy_invocation.is_some()
            || self.pending_prepare_invocation.is_some()
            || self.pending_review_invocation.is_some()
        {
            return Err("Wait for the previous copy result before retrying.".into());
        }
        self.validate_continued_operation(index)?;
        if self.worker.is_some() {
            return Err("another folder import is already running".into());
        }
        durable_copy_available()
            .map_err(|error| format!("Folder copying is unavailable on this platform: {error}"))?;
        let before = self
            .operations
            .get(index)
            .cloned()
            .ok_or_else(|| "the selected migration operation no longer exists".to_string())?;
        let job = match before.retry_job() {
            Ok(job) => job,
            Err(error) => {
                if let (Some(c), Some(folder)) = (&self.continuation, self.continued_index(index)) {
                    c.unpublished_retry(folder, &before)?.ok_or(error)?
                } else {
                    return Err(error);
                }
            }
        };
        let job = if let (Some(c), Some(folder)) = (&self.continuation, self.continued_index(index))
        {
            c.constrain_job(folder, job)?
        } else {
            job
        };
        self.operations
            .get_mut(index)
            .ok_or_else(|| "the selected migration operation no longer exists".to_string())?
            .mark_running()?;
        self.detail_pages.remove(&index);
        self.detail_request = None;
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| "private migration state is unavailable".to_string())?
            .clone();
        match MigrationWorker::start_persisted(job, writer, self.operations.clone(), index) {
            Ok(worker) => {
                self.worker = Some((index, worker));
                self.status = "Continuing after checking saved progress…".into();
                self.status_is_error = false;
                self.structure_dirty = true;
                Ok(())
            }
            Err(error) => {
                if let Some(operation) = self.operations.get_mut(index) {
                    operation.apply_outcome(super::migration_worker::WorkerOutcome::Failed(
                        error.clone(),
                    ));
                }
                let _ = self.persist();
                Err(error)
            }
        }
    }

    fn cancel_active(&mut self) -> bool {
        let Some((_, worker)) = &self.worker else {
            return false;
        };
        if worker.cancel() {
            self.batch.clear();
            self.status = "Stopping at the next safe copy boundary…".into();
            self.status_is_error = false;
            self.structure_dirty = true;
            true
        } else {
            self.status = "File checks are finished; waiting for the final result.".into();
            self.status_is_error = false;
            self.structure_dirty = true;
            false
        }
    }

    fn poll_persistence_and_details(&mut self, changed: &mut bool) {
        self.pending_saves.retain(|reply| match reply.try_recv() {
            Ok(Ok(saved)) => {
                // An older acknowledgement may fill a missing legacy detail
                // reference, but cannot replace a newer operation state.
                for (current, persisted) in self.operations.iter_mut().zip(saved) {
                    current.retain_detail_reference(&persisted);
                }
                *changed = true;
                false
            }
            Ok(Err(error)) => {
                self.status = format!("Local copy state could not be saved: {error}");
                self.status_is_error = true;
                *changed = true;
                false
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.status =
                    "Local copy state writer stopped before confirming its checkpoint.".into();
                self.status_is_error = true;
                *changed = true;
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
        });
        if let Some((index, receiver)) = &self.detail_request {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("Copy details could not be loaded.".into()))
                }
                Err(mpsc::TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                match result {
                    Ok(page) => {
                        self.detail_pages.insert(*index, page);
                    }
                    Err(error) => {
                        self.status = error;
                        self.status_is_error = true;
                    }
                }
                self.detail_request = None;
                *changed = true;
            }
        }
    }

    fn poll_identity_gate(&mut self, changed: &mut bool) {
        if let Some((command, rx)) = &self.gate_request {
            let result = match rx.try_recv() {
                Ok(r) => Some(r),
                Err(mpsc::TryRecvError::Disconnected) => Some(Err("Session check stopped".into())),
                Err(mpsc::TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                let command = *command;
                self.gate_request = None;
                self.identity_ready = result.is_ok();
                match result {
                    Ok(()) => self.apply_authorized_command(command),
                    Err(e) => {
                        self.status = e;
                        self.status_is_error = true;
                        self.batch.clear();
                    }
                }
                if self.status_is_error {
                    let error = self.status.clone();
                    let copy_related = match command {
                        ImportCommand::Start(index) | ImportCommand::Retry(index) => {
                            self.operations.get(index).is_some_and(|record| {
                                self.pending_copy_invocation
                                    .as_ref()
                                    .is_some_and(|pending| {
                                        pending.operation_ids.iter().any(|id| id == record.id())
                                    })
                            })
                        }
                        ImportCommand::StartBatch => self.pending_copy_invocation.is_some(),
                        _ => false,
                    };
                    if copy_related && let Some(pending) = &mut self.pending_copy_invocation {
                        pending.failure = Some(error.clone());
                    }
                    let review_related = match command {
                        ImportCommand::ResolveContinued(index)
                        | ImportCommand::PickContinuedDestination(index) => {
                            self.continuation.as_ref().is_some_and(|continuation| {
                                self.pending_review_invocation
                                    .as_ref()
                                    .is_some_and(|pending| {
                                        pending.folder_id == continuation.operation_id(index)
                                    })
                            })
                        }
                        _ => false,
                    };
                    if review_related && let Some(pending) = &mut self.pending_review_invocation {
                        pending.failure = Some(error);
                    }
                }
                *changed = true;
            }
        }
    }

    fn acknowledge_completed_continuation(&mut self) {
        if let Some(c) = &mut self.continuation {
            let settled = c.folders.iter().enumerate().all(|(i, f)| {
                f.mode == ChoiceMode::Later
                    || self.operations.iter().any(|op| {
                        op.id() == c.operation_id(i) && op.state() == OperationState::Complete
                    })
            });
            if settled {
                c.show_on_login = false;
                if let Some(store) = &self.store
                    && let Err(error) = c.save(store)
                {
                    self.status = error;
                    self.status_is_error = true;
                }
            }
        }
    }

    fn poll_folder_picker(&mut self) -> bool {
        let mut changed = false;
        let picker_outcome =
            self.picker
                .as_ref()
                .and_then(|(target, receiver)| match receiver.try_recv() {
                    Ok(outcome) => Some((*target, outcome)),
                    Err(mpsc::TryRecvError::Disconnected) => Some((
                        *target,
                        ui::PortalOutcome::Failed(
                            "Folder dialog stopped before responding.".into(),
                        ),
                    )),
                    Err(mpsc::TryRecvError::Empty) => None,
                });
        if let Some((target, outcome)) = picker_outcome {
            self.picker = None;
            changed = true;
            match outcome {
                ui::PortalOutcome::Opened(path) => {
                    match target {
                        PickTarget::Source => self.source = Some(path),
                        PickTarget::DestinationParent => self.destination_parent = Some(path),
                        PickTarget::ContinuedDestination(index) => {
                            let result = self.request_continued_review(index, Some(path));
                            if let Err(e) = result {
                                if let Some(pending) = &mut self.pending_review_invocation {
                                    pending.failure = Some(e.clone());
                                }
                                self.status = e;
                                self.status_is_error = true;
                                self.structure_dirty = true;
                                return true;
                            }
                        }
                    }
                    self.status =
                        "Folder selected. Review is required after both locations are chosen."
                            .into();
                    self.status_is_error = false;
                }
                ui::PortalOutcome::Cancelled => {
                    if let PickTarget::ContinuedDestination(index) = target
                        && let (Some(pending), Some(continuation)) =
                            (&mut self.pending_review_invocation, &self.continuation)
                        && pending.folder_id == continuation.operation_id(index)
                    {
                        pending.cancelled = true;
                    }
                    self.status = "No folder was selected.".into();
                    self.status_is_error = false;
                }
                ui::PortalOutcome::Failed(error) => {
                    self.status = format!("The folder dialog could not open: {error}");
                    self.status_is_error = true;
                    if let Some(pending) = &mut self.pending_review_invocation {
                        pending.failure = Some(self.status.clone());
                    }
                }
            }
        }
        changed
    }

    fn poll_background(&mut self) -> bool {
        let mut changed = self.poll_continued_review();
        self.poll_identity_gate(&mut changed);
        let mut terminal = false;
        // At most one latest progress update and one terminal event per frame.
        for _ in 0..2 {
            let Some((index, worker)) = &mut self.worker else {
                break;
            };
            let Some(event) = worker.try_next() else {
                break;
            };
            changed = true;
            match event {
                WorkerEvent::Progress(progress) => {
                    if let Some(operation) = self.operations.get_mut(*index) {
                        operation.apply_progress(&progress);
                        self.status = format!(
                            "Verified {} across {} file(s).",
                            ui::format::bytes_si(progress.verified_bytes),
                            progress.verified_files
                        );
                    }
                }
                WorkerEvent::Stored(saved) => {
                    if let Some(operation) = self.operations.get_mut(*index) {
                        *operation = *saved;
                        self.status = operation
                            .failure()
                            .unwrap_or_else(|| state_summary(operation.state()))
                            .into();
                        self.status_is_error = !matches!(
                            operation.state(),
                            OperationState::Complete | OperationState::Cancelled
                        );
                    }
                    terminal = true;
                    break;
                }
                WorkerEvent::Finished(outcome) => {
                    if let Some(operation) = self.operations.get_mut(*index) {
                        operation.apply_outcome(outcome);
                        self.status = state_summary(operation.state()).into();
                        self.status_is_error = !matches!(
                            operation.state(),
                            OperationState::Complete | OperationState::Cancelled
                        );
                    }
                    terminal = true;
                    break;
                }
            }
        }
        if terminal {
            if let Some((index, _)) = &self.worker
                && let Some(record) = self.operations.get(*index)
                && let Some(pending) = &mut self.pending_copy_invocation
                && pending.operation_ids.iter().any(|id| id == record.id())
            {
                pending
                    .terminal_states
                    .insert(record.id().to_string(), record.state());
            }
            self.worker = None;
            if !self.batch.is_empty() {
                let next = self.batch.remove(0);
                self.apply_command(ImportCommand::Start(next));
            }
        }
        self.poll_persistence_and_details(&mut changed);
        if terminal {
            self.acknowledge_completed_continuation();
        }

        changed |= self.poll_folder_picker();
        if changed {
            self.structure_dirty = true;
        }
        changed
    }

    fn apply_command(&mut self, command: ImportCommand) {
        if self.continuation.is_some()
            && matches!(
                command,
                ImportCommand::Start(_)
                    | ImportCommand::CheckIdentity
                    | ImportCommand::Retry(_)
                    | ImportCommand::StartBatch
                    | ImportCommand::ResolveContinued(_)
                    | ImportCommand::PickContinuedDestination(_)
            )
        {
            if let Err(e) = self.request_identity_gate(command) {
                self.status = e;
                self.status_is_error = true;
            }
            self.structure_dirty = true;
            return;
        }
        self.apply_authorized_command(command);
    }

    fn reviewed_batch_indices(&self) -> Vec<usize> {
        self.operations
            .iter()
            .enumerate()
            .filter(|(i, op)| {
                op.state() == OperationState::AwaitingReview
                    && self.continued_index(*i).is_some_and(|f| {
                        self.continuation.as_ref().is_some_and(|c| {
                            c.folders
                                .get(f)
                                .is_some_and(|folder| folder.mode == ChoiceMode::Copy)
                        })
                    })
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn start_reviewed_batch(&mut self) -> Result<(), String> {
        self.batch = self.reviewed_batch_indices();
        if self.batch.is_empty() {
            Err("Review at least one saved Copy folder first.".into())
        } else {
            let first = self.batch.remove(0);
            self.start_copy(first)
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the shared command dispatcher keeps UI and semantic actions on one production path"
    )]
    fn apply_authorized_command(&mut self, command: ImportCommand) {
        if self.review_request.is_some() {
            self.status = "Wait for the folder review to finish.".into();
            self.status_is_error = true;
            return;
        }
        let result = match command {
            ImportCommand::CheckIdentity => {
                self.status =
                    "Identity setup is complete. Review your folder destinations to continue."
                        .into();
                Ok(())
            }
            ImportCommand::ResolveContinued(index) => self.resolve_continued(index),
            ImportCommand::PickContinuedDestination(index) => {
                if self.picker.is_some() {
                    self.status = super::strings::t("imager-import-error-picker-unavailable");
                    self.status_is_error = true;
                    return;
                }
                if let Err(error) = self.check_continued_review_allowed(index) {
                    self.status = error;
                    self.status_is_error = true;
                    return;
                }
                self.picker = Some((
                    PickTarget::ContinuedDestination(index),
                    (self.picker_launcher)(),
                ));
                Ok(())
            }
            ImportCommand::ContinuedMode(index, mode) => self.save_continued_mode(index, mode),
            ImportCommand::LaterAll => {
                self.batch.clear();
                if self.worker.is_some() {
                    Err("Stop the active copy before deciding later.".into())
                } else {
                    let count = self.continuation.as_ref().map_or(0, |c| c.folders.len());
                    (0..count).try_for_each(|i| self.save_continued_mode(i, ChoiceMode::Later))
                }
            }
            ImportCommand::StartBatch => self.start_reviewed_batch(),
            ImportCommand::PickSource => {
                if self.picker.is_some() {
                    Err(super::strings::t("imager-import-error-picker-unavailable"))
                } else {
                    self.picker = Some((PickTarget::Source, (self.picker_launcher)()));
                    self.status = "Choose the source folder to copy.".into();
                    Ok(())
                }
            }
            ImportCommand::PickDestination => {
                if self.picker.is_some() {
                    Err(super::strings::t("imager-import-error-picker-unavailable"))
                } else {
                    self.picker = Some((PickTarget::DestinationParent, (self.picker_launcher)()));
                    self.status = "Choose where Tilo should create the copied folder.".into();
                    Ok(())
                }
            }
            ImportCommand::PrepareReview => self.prepare_review(),
            ImportCommand::Start(index) => self.start_copy(index),
            ImportCommand::Retry(index) => self.retry_operation(index),
            ImportCommand::Cancel => {
                if self.cancel_active() {
                    Ok(())
                } else {
                    Err("Copy could not be stopped because file checks are finishing.".into())
                }
            }
            ImportCommand::ClearSelection => {
                self.source = None;
                self.destination_parent = None;
                self.status = "Selection cleared.".into();
                Ok(())
            }
            ImportCommand::NextIssues(index) => self.load_next_issue_page(index),
            ImportCommand::FirstIssues(index) => self.load_first_issue_page(index),
            ImportCommand::ToggleDetails(index) => {
                if !self.expanded_details.remove(&index) {
                    self.expanded_details.insert(index);
                    if self
                        .operations
                        .get(index)
                        .is_some_and(|o| o.issue_count() > 8)
                        && !self.detail_pages.contains_key(&index)
                        && let Err(error) = self.load_issue_page(index, 0)
                    {
                        self.status = error;
                        self.status_is_error = true;
                    }
                }
                Ok(())
            }
        };
        if let Err(error) = result {
            self.batch.clear();
            self.status = error;
            self.status_is_error = true;
        }
        self.structure_dirty = true;
    }

    fn drain_commands(&mut self) -> bool {
        let commands = std::mem::take(&mut *self.commands.borrow_mut());
        let changed = !commands.is_empty();
        for command in commands {
            self.apply_command(command);
        }
        changed
    }

    fn button(&self, id: &str, label: &str, command: ImportCommand) -> Button {
        let sink = Rc::clone(&self.commands);
        Button::new(id, label, None)
            .fit_content(40.0)
            .on_effect(move || sink.borrow_mut().push(command))
    }

    fn selection_card(&self) -> Card {
        let source = self.source.as_ref().map_or_else(
            || "No source selected".to_string(),
            |path| path.display().to_string(),
        );
        let destination = self.destination_parent.as_ref().map_or_else(
            || "No destination selected".to_string(),
            |parent| {
                self.source
                    .as_ref()
                    .and_then(|source| source.file_name())
                    .map_or_else(
                        || parent.display().to_string(),
                        |name| parent.join(name).display().to_string(),
                    )
            },
        );
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(Text::new(&format!("Source: {source}"))))
            .push(Box::new(Text::new(&format!(
                "Copy to: {destination}"
            ))))
            .push(Box::new(Text::new(
                "One-time copy. Originals stay where they are. Later changes are not synced, and this does not share or upload files.",
            ).dimmed()))
            .push(Box::new(Row::new().no_wrap()
                .push(Box::new(self.button("import.pick-source", "Choose source…", ImportCommand::PickSource)))
                .push(Box::new(self.button("import.pick-destination", "Choose destination…", ImportCommand::PickDestination)))));
        if self.source.is_some() || self.destination_parent.is_some() {
            body = body.push(Box::new(self.button(
                "import.clear-selection",
                "Clear selection",
                ImportCommand::ClearSelection,
            )));
        }
        body = body.push(Box::new(
            self.button("import.review", "Review copy", ImportCommand::PrepareReview)
                .primary()
                .with_enabled(self.can_prepare_review()),
        ));
        Card::titled(
            if self.operations.is_empty() {
                "Copy a folder"
            } else {
                "Copy another folder"
            },
            Box::new(body),
        )
    }

    fn page_details(&self, index: usize, operation: &OperationRecord, details: &mut String) {
        if let Some(page) = self.detail_pages.get(&index) {
            *details = page
                .issues
                .iter()
                .map(|(p, reason)| format!("Needs attention: {} — {reason}", p.display()))
                .collect::<Vec<_>>()
                .join("\n");
            for limitation in operation.metadata_limitations() {
                let _ = write!(details, "\nFile properties: {limitation}");
            }
            if let Some(failure) = operation.failure() {
                let _ = write!(details, "\nCould not continue: {failure}");
            }
        }
    }

    fn issue_page_button(&self, index: usize) -> Button {
        let (label, command, enabled) = if self.detail_request.is_some() {
            ("Loading issues…", ImportCommand::NextIssues(index), false)
        } else if self
            .detail_pages
            .get(&index)
            .is_some_and(|page| page.offset > 0 && page.next.is_none())
        {
            (
                "First issue page",
                ImportCommand::FirstIssues(index),
                self.can_load_first_issues(index),
            )
        } else {
            (
                "Next issue page",
                ImportCommand::NextIssues(index),
                self.can_load_next_issues(index),
            )
        };
        self.button(
            &format!("import.operation.{index}.next-issues"),
            label,
            command,
        )
        .with_enabled(enabled)
    }

    fn operation_card(&self, index: usize, operation: &OperationRecord) -> Card {
        let (summary, mut details) = operation_text(operation);
        self.page_details(index, operation, &mut details);
        let mut body = stack().push(Box::new(
            Text::new(&summary).with_id(format!("import.operation.{index}.summary")),
        ));
        // The card's commands form one action row rather than a stack of
        // one-button lines, and the row follows the expanded details: the
        // toggle, the issue paging and the copy action act on that list, so
        // they come after it in reading and Tab order.
        let mut actions = Row::new();
        let mut has_actions = false;
        let mut details_text = None;
        if !details.is_empty() {
            actions = actions.push(Box::new(self.button(
                &format!("import.operation.{index}.toggle-details"),
                if self.expanded_details.contains(&index) {
                    "Hide details"
                } else {
                    "Details"
                },
                ImportCommand::ToggleDetails(index),
            )));
            has_actions = true;
            if self.expanded_details.contains(&index) {
                details_text =
                    Some(Text::new(&details).with_id(format!("import.operation.{index}.details")));
                if operation.issue_count() > 8 {
                    actions = actions.push(Box::new(self.issue_page_button(index)));
                }
            }
        }
        let state_action = match operation.state() {
            OperationState::AwaitingReview => Some(
                self.button(
                    &format!("import.operation.{index}.start"),
                    "Start copy",
                    ImportCommand::Start(index),
                )
                .primary()
                .with_enabled(self.can_start_copy(index)),
            ),
            OperationState::Running
                if self
                    .worker
                    .as_ref()
                    .is_some_and(|(active, _)| *active == index) =>
            {
                Some(self.button(
                    &format!("import.operation.{index}.cancel"),
                    "Stop copy",
                    ImportCommand::Cancel,
                ))
            }
            OperationState::Interrupted
            | OperationState::Cancelled
            | OperationState::Incomplete
            | OperationState::Failed => Some(
                self.button(
                    &format!("import.operation.{index}.retry"),
                    "Continue copy",
                    ImportCommand::Retry(index),
                )
                .with_enabled(self.worker.is_none() && operation.retry_job().is_ok()),
            ),
            OperationState::Ready | OperationState::Running | OperationState::Complete => None,
        };
        if let Some(action) = state_action {
            actions = actions.push(Box::new(action));
            has_actions = true;
        }
        if let Some(text) = details_text {
            body = body.push(Box::new(text));
        }
        if has_actions {
            body = body.push(Box::new(actions));
        }
        Card::titled(
            format!(
                "Folder import · {}",
                operation_state_label(operation.state())
            ),
            Box::new(body),
        )
    }

    fn can_start_continued_batch(&self) -> bool {
        self.review_request.is_none()
            && self.identity_ready
            && self.worker.is_none()
            && self.gate_request.is_none()
            && self.pending_copy_invocation.is_none()
            && self.pending_prepare_invocation.is_none()
            && self.pending_review_invocation.is_none()
            && self.operations.iter().enumerate().any(|(index, op)| {
                op.state() == OperationState::AwaitingReview
                    && self.continued_index(index).is_some_and(|f| {
                        self.continuation
                            .as_ref()
                            .and_then(|c| c.folders.get(f))
                            .is_some_and(|folder| folder.mode == ChoiceMode::Copy)
                    })
            })
    }

    fn continued_review_controls(&self, row: Column, index: usize) -> Column {
        let enabled = self.identity_ready
            && self.worker.is_none()
            && self.review_request.is_none()
            && self.gate_request.is_none();
        // Both destination commands belong to one action row (wrapping when
        // the card is narrow) instead of two single-button lines.
        row.push(Box::new(
            Row::new()
                .push(Box::new(
                    self.button(
                        &format!("import.continued.{index}.review"),
                        "Review destination",
                        ImportCommand::ResolveContinued(index),
                    )
                    .with_enabled(enabled),
                ))
                .push(Box::new(
                    self.button(
                        &format!("import.continued.{index}.destination"),
                        "Choose another destination…",
                        ImportCommand::PickContinuedDestination(index),
                    )
                    .with_enabled(enabled),
                )),
        ))
    }

    fn continuation_card(&self, c: &Continuation) -> Card {
        let mut body = Column::new().with_no_padding().with_intrinsic_height()
            .push(Box::new(Text::new("Saved Desktop folder choices. Complete identity setup in Tilo, then review each actual destination. Originals stay unchanged; local Copy does not enable sync or sharing.").with_id("import.continuation.explanation")));
        if !self.identity_ready {
            body = body.push(Box::new(
                self.button(
                    "import.continued.check-identity",
                    "Continue after identity setup",
                    ImportCommand::CheckIdentity,
                )
                .with_enabled(self.gate_request.is_none()),
            ));
        }
        // Each saved folder is a block of this card separated by space, not
        // a second edged card inside the first (HIG §7).
        let mut folders = stack().with_gap(GROUP);
        for (index, f) in c.folders.iter().enumerate() {
            let source = f.source().map_or_else(|e| e, |p| p.display().to_string());
            let destination = f.destination().map_or_else(
                |e| e,
                |p| {
                    p.map_or_else(
                        || "Destination pending review".into(),
                        |p| p.display().to_string(),
                    )
                },
            );
            let mode = match f.mode {
                ChoiceMode::Copy => "One-time Copy",
                ChoiceMode::Later => "Decide later — retained in Add folders",
                ChoiceMode::Connected => "Connected — unavailable; no sync or copy started",
            };
            let can_review = self
                .operations
                .iter()
                .find(|o| o.id() == c.operation_id(index))
                .is_none_or(|o| {
                    matches!(
                        o.state(),
                        OperationState::AwaitingReview
                            | OperationState::Failed
                            | OperationState::Interrupted
                            | OperationState::Cancelled
                    )
                });
            let mut row = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(
                    Text::new(&format!("Source: {source}\nCopy to: {destination}\n{mode}"))
                        .with_id(format!("import.continued.{index}.paths")),
                ))
                .push(Box::new(
                    Row::new()
                        .no_wrap()
                        .push(Box::new(
                            self.button(
                                &format!("import.continued.{index}.copy"),
                                "One-time Copy",
                                ImportCommand::ContinuedMode(index, ChoiceMode::Copy),
                            )
                            .with_enabled(
                                self.worker.is_none()
                                    && self.review_request.is_none()
                                    && self.gate_request.is_none(),
                            ),
                        ))
                        .push(Box::new(
                            self.button(
                                &format!("import.continued.{index}.later"),
                                "Later",
                                ImportCommand::ContinuedMode(index, ChoiceMode::Later),
                            )
                            .with_enabled(
                                self.worker.is_none()
                                    && self.review_request.is_none()
                                    && self.gate_request.is_none(),
                            ),
                        )),
                ));
            if can_review && f.mode == ChoiceMode::Copy {
                row = self.continued_review_controls(row, index);
            }
            folders = folders.push(Box::new(row));
        }
        if !c.folders.is_empty() {
            body = body.push(Box::new(folders));
        }
        // The card's two closing commands share one action row.
        body = body.push(Box::new(
            Row::new()
                .push(Box::new(
                    self.button(
                        "import.continued.start",
                        "Copy reviewed folders",
                        ImportCommand::StartBatch,
                    )
                    .primary()
                    .with_enabled(self.can_start_continued_batch()),
                ))
                .push(Box::new(
                    self.button(
                        "import.continued.later",
                        "Decide later for all",
                        ImportCommand::LaterAll,
                    )
                    .with_enabled(
                        self.worker.is_none()
                            && self.review_request.is_none()
                            && self.gate_request.is_none(),
                    ),
                )),
        ));
        Card::titled("Continue your Desktop setup", Box::new(body))
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let header = stack()
            .with_gap(RELATED)
            .push(Box::new(
                Text::new("Add folders to Tilo")
                    .with_role(TextRole::Title)
                    .with_heading_level(1),
            ))
            .push(Box::new(Text::new(
                "Choose a folder and where to copy it. Tilo checks each copied file and keeps your originals. Folder sync is not available yet.",
            ).dimmed()));
        // Each group of the page — the unavailable notice, the saved Desktop
        // choices, the imports under their heading, the copy form — sits a
        // `GROUP` after the previous one; cards inside a group keep the
        // sibling step.
        let mut body = page(header);
        if let Err(error) = durable_copy_available() {
            body = body.push(Box::new(Card::titled(
                "Folder copying unavailable",
                Box::new(Text::new(&error).with_live(Politeness::Assertive)),
            )));
        }
        if let Some(c) = &self.continuation {
            body = body.push(Box::new(self.continuation_card(c)));
        }
        if self.operations.iter().enumerate().any(|(index, op)| {
            op.state() != OperationState::AwaitingReview || self.continued_index(index).is_none()
        }) {
            let mut imports = stack().push(Box::new(
                Text::new("Imports")
                    .with_role(TextRole::Heading)
                    .with_heading_level(2),
            ));
            for (index, operation) in self.operations.iter().enumerate() {
                if operation.state() == OperationState::AwaitingReview
                    && self.continued_index(index).is_some()
                {
                    continue;
                }
                imports = imports.push(Box::new(self.operation_card(index, operation)));
            }
            body = body.push(Box::new(imports));
        }
        body = body.push(Box::new(self.selection_card()));
        let content = Scrollable::new_vertical()
            .fill_parent_height()
            .always_visible()
            .with_content_gutter(SCROLLBAR_GUTTER)
            .with_child(Box::new(body));
        let status = Card::new(Box::new(Text::new(&self.status).with_live(
            if self.status_is_error {
                Politeness::Assertive
            } else {
                Politeness::Polite
            },
        )));
        Box::new(
            Container::new().filling().with_child(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_horizontal_padding(PAGE_GUTTER, PAGE_GUTTER)
                    .with_vertical_padding(PAGE_GUTTER)
                    .with_gap(SIBLING)
                    .push(Box::new(content))
                    .push(Box::new(status)),
            )),
        )
    }
}

fn operation_text(operation: &OperationRecord) -> (String, String) {
    let source = operation.source().map_or_else(
        |error| format!("unavailable ({error})"),
        |path| path.display().to_string(),
    );
    let destination = operation.destination().map_or_else(
        |error| format!("unavailable ({error})"),
        |path| path.display().to_string(),
    );
    let mut summary = format!(
        "{source} → {destination}\n{}\nCopied and checked: {} across {} file(s)",
        state_summary(operation.state()),
        ui::format::bytes_si(operation.verified_bytes()),
        operation.verified_file_count()
    );
    if operation.state() == OperationState::Running
        && let Ok(Some(path)) = operation.current_relative_path()
        && !path.as_os_str().is_empty()
    {
        let _ = write!(summary, "\nCurrent: {}", path.display());
    }
    let issues = operation.issues().unwrap_or_default();
    if operation.issue_count() == 1 {
        summary.push_str("\n1 issue needs attention.");
    } else if operation.issue_count() > 0 {
        let _ = write!(
            summary,
            "\n{} issues need attention.",
            operation.issue_count()
        );
    }
    if !operation.metadata_limitations().is_empty() {
        summary.push_str("\nSome file properties may not carry over. See Details.");
    }

    let mut details = String::new();
    if let Some(failure) = operation.failure() {
        let _ = write!(details, "Could not continue: {failure}");
    }
    for (path, reason) in issues {
        let shown = if path.as_os_str().is_empty() {
            "folder".to_string()
        } else {
            path.display().to_string()
        };
        let prefix = if details.is_empty() { "" } else { "\n" };
        let _ = write!(details, "{prefix}Needs attention: {shown} — {reason}");
    }
    for limitation in operation.metadata_limitations() {
        let prefix = if details.is_empty() { "" } else { "\n" };
        let _ = write!(details, "{prefix}File properties: {limitation}");
    }
    (summary, details)
}

fn operation_state_label(state: OperationState) -> &'static str {
    match state {
        OperationState::AwaitingReview => "Review required",
        OperationState::Ready => "Ready",
        OperationState::Running => "Copying",
        OperationState::Interrupted => "Interrupted",
        OperationState::Cancelled => "Stopped",
        OperationState::Incomplete => "Incomplete",
        OperationState::Failed => "Could not start",
        OperationState::Complete => "Complete",
    }
}

const fn operation_state_key(state: OperationState) -> &'static str {
    match state {
        OperationState::AwaitingReview => "awaiting_review",
        OperationState::Ready => "ready",
        OperationState::Running => "running",
        OperationState::Interrupted => "interrupted",
        OperationState::Cancelled => "cancelled",
        OperationState::Incomplete => "incomplete",
        OperationState::Failed => "failed",
        OperationState::Complete => "complete",
    }
}

const fn choice_mode_key(mode: ChoiceMode) -> &'static str {
    match mode {
        ChoiceMode::Copy => "copy",
        ChoiceMode::Connected => "connected",
        ChoiceMode::Later => "later",
    }
}

fn state_summary(state: OperationState) -> &'static str {
    match state {
        OperationState::AwaitingReview => "Review required before any copy starts.",
        OperationState::Ready => "Reviewed and ready to start.",
        OperationState::Running => "Copying and checking files.",
        OperationState::Interrupted => {
            "The copy was interrupted. Check the folders, then continue when ready."
        }
        OperationState::Cancelled => "Copy stopped. Checked files were kept.",
        OperationState::Incomplete => "Some items need review before the copy can finish.",
        OperationState::Failed => "The copy could not start. Open Details before trying again.",
        OperationState::Complete => "Copy checked and complete.",
    }
}

impl Widget for ImportApp {
    ui::provides!(dyn ui::resources::PublishesResources);

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[import_actions::IMPORT_OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
        )
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self
            .pending_prepare_invocation
            .as_ref()
            .is_some_and(|pending| pending.invocation == invocation)
        {
            let result = self
                .pending_prepare_invocation
                .as_ref()?
                .save_reply
                .try_recv();
            let result = match result {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err("Folder review writer stopped before confirming its checkpoint.".into())
                }
            };
            let pending = self.pending_prepare_invocation.take()?;
            return Some(match result {
                Ok(saved) => {
                    for (current, persisted) in self.operations.iter_mut().zip(saved) {
                        current.retain_detail_reference(&persisted);
                    }
                    tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(self.import_projection().value()),
                            resources: Vec::new(),
                        },
                    }
                }
                Err(message) => {
                    self.operations
                        .retain(|record| record.id() != pending.operation_id);
                    self.source.get_or_insert(pending.source);
                    self.destination_parent
                        .get_or_insert(pending.destination_parent);
                    self.status = format!("Folder review could not be saved: {message}");
                    self.status_is_error = true;
                    self.structure_dirty = true;
                    tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "imager.review_save_failed".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    }
                }
            });
        }
        if cancellation_requested
            && self
                .pending_copy_invocation
                .as_ref()
                .is_some_and(|pending| pending.invocation == invocation)
        {
            if self.gate_request.is_some() {
                self.gate_request = None;
                self.batch.clear();
                self.pending_copy_invocation = None;
                return Some(tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some("Folder copy stopped before it started.".into()),
                });
            }
            if self.worker.is_some()
                && self
                    .pending_copy_invocation
                    .as_ref()
                    .is_some_and(|pending| !pending.cancellation_requested)
                && self.cancel_active()
                && let Some(pending) = &mut self.pending_copy_invocation
            {
                pending.cancellation_requested = true;
            }
        }
        if cancellation_requested
            && self
                .pending_review_invocation
                .as_ref()
                .is_some_and(|pending| pending.invocation == invocation)
        {
            if self.review_request.is_some() {
                if let Some(pending) = &mut self.pending_review_invocation {
                    pending.cancellation_requested = true;
                }
                return None;
            }
            if self.gate_request.is_some() || self.picker.is_some() {
                self.gate_request = None;
                self.picker = None;
                self.pending_review_invocation = None;
                return Some(tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some("Folder review stopped before it was saved.".into()),
                });
            }
        }
        if let Some(pending) = self.pending_review_invocation.as_ref() {
            if pending.invocation != invocation {
                return None;
            }
            if self.gate_request.is_some() || self.review_request.is_some() || self.picker.is_some()
            {
                return None;
            }
            if !pending.cancelled && pending.failure.is_none() && !pending.finished {
                return None;
            }
            let save_result = if let Some(reply) = &pending.save_reply {
                match reply.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Empty) => return None,
                    Err(mpsc::TryRecvError::Disconnected) => Some(Err(
                        "Folder review writer stopped before confirming its checkpoint.".into(),
                    )),
                }
            } else {
                None
            };
            let pending = self.pending_review_invocation.take()?;
            if let Some(Ok(saved)) = &save_result {
                for (current, persisted) in self.operations.iter_mut().zip(saved) {
                    current.retain_detail_reference(persisted);
                }
            }
            if let Some(Err(message)) = &save_result {
                self.status = format!("Folder review could not be saved: {message}");
                self.status_is_error = true;
                self.structure_dirty = true;
            }
            let update = if pending.cancelled {
                tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some("No destination folder was selected.".into()),
                }
            } else if pending.failure.is_some() || save_result.as_ref().is_some_and(Result::is_err)
            {
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "imager.review_failed".into(),
                        message: pending
                            .failure
                            .or_else(|| save_result.and_then(Result::err))
                            .unwrap_or_else(|| "Folder review could not be saved.".into()),
                        details: None,
                        retryable: false,
                    },
                }
            } else if self.operations.iter().any(|record| {
                record.id() == pending.folder_id && record.state() == OperationState::AwaitingReview
            }) {
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput {
                        value: Some(self.import_projection().value()),
                        resources: Vec::new(),
                    },
                }
            } else {
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "imager.review_incomplete".into(),
                        message: "Folder review ended without a saved copy plan.".into(),
                        details: None,
                        retryable: false,
                    },
                }
            };
            return Some(update);
        }
        let pending = self.pending_copy_invocation.as_ref()?;
        if pending.invocation != invocation {
            return None;
        }
        if self.gate_request.is_some()
            || self.worker.as_ref().is_some_and(|(index, _)| {
                self.operations
                    .get(*index)
                    .is_some_and(|record| pending.operation_ids.iter().any(|id| id == record.id()))
            })
            || !self.batch.is_empty()
        {
            return None;
        }
        let states = pending
            .operation_ids
            .iter()
            .map(|id| pending.terminal_states.get(id).copied())
            .collect::<Vec<_>>();
        let update = if pending.failure.is_some() {
            tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "imager.copy_failed".into(),
                    message: pending.failure.clone().unwrap_or_else(|| {
                        "The selected folder copy disappeared before completion.".into()
                    }),
                    details: None,
                    retryable: false,
                },
            }
        } else if states
            .iter()
            .all(|state| *state == Some(OperationState::Complete))
        {
            tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput {
                    value: Some(self.import_projection().value()),
                    resources: Vec::new(),
                },
            }
        } else if pending.cancellation_requested
            || states.contains(&Some(OperationState::Cancelled))
        {
            tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("Folder copy stopped before completion.".into()),
            }
        } else if states.iter().any(Option::is_none) {
            tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "imager.copy_failed".into(),
                    message: "The selected folder copy disappeared before completion.".into(),
                    details: None,
                    retryable: false,
                },
            }
        } else {
            tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "imager.copy_incomplete".into(),
                    message: "Folder copy ended before all selected folders completed.".into(),
                    details: None,
                    retryable: false,
                },
            }
        };
        self.pending_copy_invocation = None;
        Some(update)
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.poll_background();
        self.drain_commands();
        if let Some(composed) = &mut self.composed {
            composed.sync(state, path);
        }
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        let mut changed = self.poll_background() || self.drain_commands();
        if let Some(composed) = &mut self.composed {
            changed |= composed.update(state, path);
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
        if let Some(composed) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&child) = children.first()
        {
            composed.apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        if let Some(composed) = &mut self.composed {
            composed.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(composed) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&child) = children.first()
        {
            composed.handle_event(event, &mut ctx.reborrow(ctx.path, child));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|child| child.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|child| child.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "ImportApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_idle_sync(&self) -> bool {
        self.worker.is_some()
            || self.review_request.is_some()
            || self.gate_request.is_some()
            || self.picker.is_some()
            || self.pending_copy_invocation.is_some()
            || self.pending_review_invocation.is_some()
            || self.pending_prepare_invocation.is_some()
            || !self.pending_saves.is_empty()
            || self.detail_request.is_some()
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
            || self
                .composed
                .as_mut()
                .is_some_and(|child| child.take_structure_dirty())
    }
}

impl ui::resources::PublishesResources for ImportApp {
    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let projection = self.import_projection();
        let described = import_actions::resource(&projection);
        let mut published = ui::tilo_resource::publish::<import_actions::ImportGuide>(
            import_actions::IMPORT_RESOURCE_ID,
            &described.label,
            &projection,
        );
        published.presentation = described.presentation;
        vec![published]
    }
}

impl ImportApp {
    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(operation) = import_actions::decode(type_id, payload)? else {
            return Ok(None);
        };
        if kind != operation.kind() {
            return Err(super::strings::t("imager-import-error-operation-kind"));
        }
        let operation_ids = if operation.background() {
            if self.pending_copy_invocation.is_some()
                || self.pending_review_invocation.is_some()
                || self.pending_prepare_invocation.is_some()
            {
                return Err("Another folder import action is still settling.".into());
            }
            match &operation {
                ImportOperation::StartReviewedBatch => self
                    .reviewed_batch_indices()
                    .into_iter()
                    .filter_map(|index| {
                        self.operations
                            .get(index)
                            .map(|record| record.id().to_string())
                    })
                    .collect(),
                ImportOperation::Start { operation_id }
                | ImportOperation::Retry { operation_id } => vec![operation_id.clone()],
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let review_folder = match &operation {
            ImportOperation::ResolveFolder { folder_id }
            | ImportOperation::PickFolderDestination { folder_id } => Some(folder_id.clone()),
            _ => None,
        };
        let preparing = matches!(&operation, ImportOperation::PrepareReview);
        let prepare_paths = if preparing {
            Some((
                self.source.clone().ok_or("Choose a source folder first")?,
                self.destination_parent
                    .clone()
                    .ok_or("Choose a destination folder first")?,
            ))
        } else {
            None
        };
        let output = self.invoke_import_operation(operation)?;
        if preparing {
            let (source, destination_parent) =
                prepare_paths.ok_or("Folder review lost its selected locations")?;
            let save_reply = self
                .pending_saves
                .pop()
                .ok_or_else(|| "Folder review did not queue a durable index save.".to_string())?;
            let operation_id = self
                .operations
                .last()
                .ok_or("Folder review did not create a copy plan")?
                .id()
                .to_string();
            self.pending_prepare_invocation = Some(PendingPrepareInvocation {
                invocation,
                save_reply,
                operation_id,
                source,
                destination_parent,
            });
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else if let Some(folder_id) = review_folder {
            self.pending_review_invocation = Some(PendingReviewInvocation {
                invocation,
                folder_id,
                cancelled: false,
                cancellation_requested: false,
                finished: false,
                failure: None,
                save_reply: None,
            });
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else if !operation_ids.is_empty() {
            self.pending_copy_invocation = Some(PendingCopyInvocation {
                invocation,
                operation_ids,
                terminal_states: HashMap::new(),
                cancellation_requested: false,
                failure: None,
            });
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else {
            Ok(Some(ui::widget::ResourceOperationStart::Completed(output)))
        }
    }
}

#[must_use]
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ImportApp::new())
}

/// A fresh directory for disposable screenshot and test fixtures, in a place
/// the source guard treats as user data.
///
/// Neither the login user's home (CI commonly runs as root, and `/root` is an
/// OS directory) nor the configured temporary directory will do: `TMPDIR` can
/// point anywhere, including below `/var`, and macOS resolves its default
/// beneath `/private`. The guard rightly refuses both.
fn disposable_fixture_dir() -> std::io::Result<tempfile::TempDir> {
    #[cfg(target_os = "macos")]
    return tempfile::tempdir_in("/Users/Shared");
    #[cfg(all(unix, not(target_os = "macos")))]
    return tempfile::tempdir_in("/tmp");
    #[cfg(not(unix))]
    return tempfile::tempdir();
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    fn fixture() -> (tempfile::TempDir, ImportApp, PathBuf, PathBuf) {
        let temp = super::disposable_fixture_dir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let source = base.join("source");
        let destination_parent = base.join("destination-parent");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination_parent).unwrap();
        let store = MigrationStore::open_at(&base.join("state")).unwrap();
        (
            temp,
            ImportApp::with_store(store).unwrap(),
            source,
            destination_parent,
        )
    }

    /// Waits for the worker, then settles its admitted invocation the way the
    /// host does, so the next background action is not refused as unsettled.
    fn wait_terminal(app: &mut ImportApp) {
        let deadline = tilo_test_env::hang_deadline();
        while app.worker.is_some() {
            app.poll_background();
            assert!(ui::time::now() < deadline, "import worker did not finish");
            std::thread::yield_now();
        }
        while let Some(invocation) = app
            .pending_copy_invocation
            .as_ref()
            .map(|pending| pending.invocation)
        {
            app.poll_background();
            if Widget::poll_resource_operation(
                app,
                invocation,
                false,
                &AutoCommit::new(),
                &automerge::ROOT,
            )
            .is_some()
            {
                break;
            }
            assert!(ui::time::now() < deadline, "copy invocation did not settle");
            std::thread::yield_now();
        }
    }

    fn settle_index(app: &mut ImportApp) {
        let deadline = tilo_test_env::hang_deadline();
        while !app.pending_saves.is_empty() {
            app.poll_background();
            assert!(ui::time::now() < deadline);
            std::thread::yield_now();
        }
    }

    fn root_resource(app: &ImportApp) -> tilo_actions::Resource {
        ui::resources::resources(app, &AutoCommit::new())
            .pop()
            .unwrap()
    }

    fn action(resource: &tilo_actions::Resource, id: &str) -> tilo_actions::ActionSpec {
        resource
            .actions
            .iter()
            .find(|candidate| candidate.id == id)
            .unwrap_or_else(|| panic!("missing action {id} on {}", resource.id.as_str()))
            .clone()
    }

    fn invoke_action(
        app: &mut ImportApp,
        action: &tilo_actions::ActionSpec,
    ) -> Result<serde_json::Value, String> {
        let encoded = action
            .build_json(&serde_json::Map::new())
            .map_err(|error| error.to_string())?;
        start_import(app, action.kind, &encoded)
    }

    fn invoke_import_raw(
        app: &mut ImportApp,
        operation: &ImportOperation,
    ) -> Result<serde_json::Value, String> {
        start_import(app, operation.kind(), &import_actions::encode(operation))
    }

    /// The projection an agent sees: a synchronous result carries it, and a
    /// background operation's admission is followed by the public status read.
    fn start_import(
        app: &mut ImportApp,
        kind: tilo_actions::OperationKind,
        encoded: &tilo_actions::EncodedAction,
    ) -> Result<serde_json::Value, String> {
        let started = Widget::start_operation(
            app,
            &ui::widget::ResourceOperation::local(
                ui::tilo_actions::InvocationId::new(),
                kind,
                &encoded.type_id,
                &encoded.payload,
            ),
            &mut AutoCommit::new(),
            &automerge::ROOT,
        )?;
        match started {
            Some(ui::widget::ResourceOperationStart::Completed(output)) => output
                .value
                .ok_or_else(|| "import operation returned no projection".to_string()),
            Some(ui::widget::ResourceOperationStart::Accepted) => observe_import(app),
            None => Err("import operation was not handled".to_string()),
        }
    }

    fn observe_import(app: &mut ImportApp) -> Result<serde_json::Value, String> {
        let observe = action(&root_resource(app), "read-status");
        invoke_action(app, &observe)
    }

    fn wait_for_public_issue_count(app: &mut ImportApp, count: usize) -> serde_json::Value {
        let deadline = tilo_test_env::hang_deadline();
        let mut state = AutoCommit::new();
        loop {
            Widget::update(app, &mut state, &automerge::ROOT);
            let observed = observe_import(app).unwrap();
            if observed["phase"] != "loading_details"
                && observed["operations"][0]["issues"]
                    .as_array()
                    .is_some_and(|issues| issues.len() == count)
            {
                return observed;
            }
            assert!(
                ui::time::now() < deadline,
                "issue page with {count} messages was not publicly observable"
            );
            std::thread::yield_now();
        }
    }

    fn cancelled_picker() -> ui::PortalChannel {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(ui::PortalOutcome::Cancelled)
            .expect("the receiver is held");
        receiver
    }

    fn disconnected_picker() -> ui::PortalChannel {
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        receiver
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "this test seam has the same fallible signature as the production identity boundary"
    )]
    fn identity_ready() -> Result<(), String> {
        Ok(())
    }

    fn identity_denied() -> Result<(), String> {
        Err("Tilo identity is unavailable".into())
    }

    fn start_action(
        app: &mut ImportApp,
        action: &tilo_actions::ActionSpec,
        invocation: tilo_actions::InvocationId,
    ) -> Result<ui::widget::ResourceOperationStart, String> {
        let encoded = action
            .build_json(&serde_json::Map::new())
            .map_err(|error| error.to_string())?;
        Widget::start_operation(
            app,
            &ui::widget::ResourceOperation::local(
                invocation,
                action.kind,
                &encoded.type_id,
                &encoded.payload,
            ),
            &mut AutoCommit::new(),
            &automerge::ROOT,
        )?
        .ok_or_else(|| "import operation was not handled".into())
    }

    fn continuation_choice(source: &Path) -> super::super::controller::FolderChoice {
        super::super::controller::FolderChoice {
            scan: super::super::folders::scan_folder(source).unwrap(),
            destination: super::super::controller::FolderDestination::Unresolved,
            mode: super::super::model::FolderMode::Copy,
            mode_overridden: false,
        }
    }

    /// Operation resources, read status, review endpoints, detail order and shot states.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn import_resources_and_status_publish_reviewed_endpoints_from_real_events() {
        // Phase `standalone_import_publishes_stable_operation_resources_before_layout`.
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            fs::write(source.join("document.txt"), b"kept locally").unwrap();
            let source_text = source.display().to_string();
            let destination_text = destination_parent.display().to_string();
            settle_index(&mut app);
            app.source = Some(source);
            app.destination_parent = Some(destination_parent.clone());
            let collision = destination_parent.join("source");
            fs::create_dir(&collision).unwrap();
            let prepare_review = action(&root_resource(&app), "prepare-review");
            let public_error = invoke_action(&mut app, &prepare_review).unwrap_err();
            assert!(!public_error.contains(&source_text));
            assert!(!public_error.contains(&destination_text));
            assert!(public_error.contains("read-status"), "{public_error}");
            assert!(app.status.contains(&collision.display().to_string()));
            assert!(
                observe_import(&mut app).unwrap()["local"]["status"]
                    .as_str()
                    .is_some_and(|status| status.contains(&collision.display().to_string()))
            );
            fs::remove_dir(collision).unwrap();
            app.prepare_review().unwrap();
            let operation_id = app.operations[0].id().to_string();

            let resources = ui::resources::resources(&app, &AutoCommit::new());

            assert_eq!(resources.len(), 1);
            assert_eq!(resources[0].kind, "imager.import");
            assert_eq!(resources[0].children.len(), 1);
            assert_eq!(resources[0].children[0].kind, "imager.import.operation");
            assert!(resources[0].children[0].id.0.ends_with(&operation_id));
            let exposed = resources[0]
                .children
                .iter()
                .map(|child| {
                    format!(
                        "{} {}",
                        child.label,
                        child
                            .presentation
                            .as_ref()
                            .and_then(|presentation| presentation.detail.as_deref())
                            .unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            assert!(!exposed.contains(&source_text));
            assert!(!exposed.contains(&destination_text));
        }

        // Phase `local_read_status_names_reviewed_endpoints_without_publishing_paths`.
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            settle_index(&mut app);
            fs::write(source.join("document.txt"), b"private copy").unwrap();
            let expected_source = source.display().to_string();
            let expected_destination = destination_parent.join("source").display().to_string();
            app.source = Some(source);
            app.destination_parent = Some(destination_parent);
            let selected = observe_import(&mut app).unwrap();
            assert_eq!(selected["local"]["source"], expected_source);
            assert_eq!(selected["local"]["destination"], expected_destination);

            app.prepare_review().unwrap();
            let reviewed = observe_import(&mut app).unwrap();
            assert_eq!(reviewed["operations"][0]["source"], expected_source);
            assert_eq!(
                reviewed["operations"][0]["destination"],
                expected_destination
            );
            let public = app.import_projection().value().to_string();
            assert!(!public.contains(&expected_source));
            assert!(!public.contains(&expected_destination));
        }

        // Phase `local_read_status_names_continued_folder_endpoints`.
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            settle_index(&mut app);
            fs::write(source.join("document.txt"), b"private copy").unwrap();
            let expected_source = source.display().to_string();
            let expected_destination = destination_parent.join("source").display().to_string();
            let mut continuation = Continuation::capture(&[continuation_choice(&source)]).unwrap();
            continuation.resolve(0, &destination_parent, true).unwrap();
            app.continuation = Some(continuation);

            let local = observe_import(&mut app).unwrap();
            assert_eq!(
                local["continuation"]["folders"][0]["source"],
                expected_source
            );
            assert_eq!(
                local["continuation"]["folders"][0]["destination"],
                expected_destination
            );
            let public = app.import_projection().value().to_string();
            assert!(!public.contains(&expected_source));
            assert!(!public.contains(&expected_destination));
        }

        // Phase `review_uses_the_selected_source_and_destination_parent`.
        // Catches copying into a suggested setup path before both current native
        // grants and the exact fresh destination have been reviewed.
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            app.source = Some(source.clone());
            assert!(app.prepare_review().is_err());

            app.destination_parent = Some(destination_parent.clone());
            app.prepare_review().unwrap();
            let operation = app.operations.last().unwrap();
            assert_eq!(operation.state(), OperationState::AwaitingReview);
            assert_eq!(operation.source().unwrap(), source);
            assert_eq!(
                operation.destination().unwrap(),
                destination_parent.join("source")
            );
            assert!(operation.start_job().is_err());
        }

        // Phase `expanded_details_precede_the_operation_commands`.
        // The expanded issue list precedes the commands that act on it — the
        // details toggle, the issue paging, the copy action — in reading and Tab
        // order (the accessibility tree and `collect_focusable` both walk the
        // widget tree in child order), and the three share one action row.
        {
            let pages = ImportApp::for_shot("import-issue-pages");
            assert!(pages.expanded_details.contains(&0));
            let card = pages.operation_card(0, &pages.operations[0]);
            fn ids(widget: &dyn Widget, out: &mut Vec<String>) {
                if let Some(id) = widget.test_id() {
                    out.push(id.to_string());
                }
                for child in widget.children() {
                    ids(child, out);
                }
            }
            let mut order = Vec::new();
            ids(&card, &mut order);
            let position = |id: &str| {
                order
                    .iter()
                    .position(|x| x == id)
                    .unwrap_or_else(|| panic!("{id} missing from {order:?}"))
            };
            let summary = position("import.operation.0.summary");
            let details = position("import.operation.0.details");
            let toggle = position("import.operation.0.toggle-details");
            let next = position("import.operation.0.next-issues");
            let retry = position("import.operation.0.retry");
            assert!(
                summary < details && details < toggle && toggle < next && next < retry,
                "{order:?}"
            );
        }

        // Phase `screenshot_states_come_from_real_worker_events`.
        // Catches screenshot fixtures drifting into fabricated success labels that
        // never passed through the real worker and copy report classifier.
        {
            let progress = ImportApp::for_shot("import-progress");
            assert_eq!(progress.operations[0].state(), OperationState::Running);
            assert!(progress.operations[0].verified_file_count() > 0);

            let retry = ImportApp::for_shot("import-retry-progress");
            assert_eq!(retry.operations[0].state(), OperationState::Running);
            assert!(retry.operations[0].verified_file_count() > 0);
            assert!(retry.operations[0].issues().unwrap().is_empty());

            let mut partial = ImportApp::for_shot("import-partial");
            assert_eq!(partial.operations[0].state(), OperationState::Incomplete);
            assert!(!partial.operations[0].issues().unwrap().is_empty());
            assert!(partial.expanded_details.is_empty());
            partial.apply_command(ImportCommand::ToggleDetails(0));
            assert!(partial.expanded_details.contains(&0));

            let conflict = ImportApp::for_shot("import-conflict");
            assert_eq!(conflict.operations[0].state(), OperationState::Incomplete);
            assert!(!conflict.operations[0].issues().unwrap().is_empty());
            let conflict_source = conflict.operations[0].source().unwrap().join("Budget.txt");
            let conflict_destination = conflict.operations[0]
                .destination()
                .unwrap()
                .join("Budget.txt");
            assert_eq!(fs::read(conflict_source).unwrap(), b"source version");
            assert_eq!(fs::read(conflict_destination).unwrap(), b"destination edit");
            assert!(conflict.expanded_details.is_empty());

            let mut pages = ImportApp::for_shot("import-issue-pages");
            assert_eq!(pages.operations[0].state(), OperationState::Incomplete);
            assert_eq!(pages.operations[0].issue_count(), 40);
            assert_eq!(pages.detail_pages[&0].issues.len(), 32);
            pages.apply_command(ImportCommand::NextIssues(0));
            let deadline = tilo_test_env::hang_deadline();
            while pages.detail_request.is_some() {
                pages.poll_background();
                assert!(ui::time::now() < deadline);
                std::thread::yield_now();
            }
            assert_eq!(pages.detail_pages[&0].issues.len(), 8);
            assert!(pages.detail_pages[&0].next.is_none());
            assert!(
                pages.detail_pages[&0]
                    .issues
                    .iter()
                    .any(|(p, _)| p == Path::new("Shortcut 39"))
            );

            let complete = ImportApp::for_shot("import-complete");
            assert_eq!(complete.operations[0].state(), OperationState::Complete);
            assert!(complete.operations[0].verified_bytes() > 0);
        }
    }

    /// Review preparation, identity failure and picker outcomes.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn review_action_fails_or_cancels_with_its_picker_and_index() {
        // Phase `prepare_review_effect_fails_when_the_index_save_is_rejected`.
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            settle_index(&mut app);
            fs::write(source.join("document.txt"), b"unchanged original").unwrap();
            app.source = Some(source);
            app.destination_parent = Some(destination_parent);
            fs::remove_file(app.store.as_ref().unwrap().root().join("ownership.lock")).unwrap();
            let review = action(&root_resource(&app), "prepare-review");
            let invocation = tilo_actions::InvocationId::new();

            assert_eq!(
                start_action(&mut app, &review, invocation).unwrap(),
                ui::widget::ResourceOperationStart::Accepted
            );
            let unsaved_id = app.operations[0].id().to_string();
            assert!(!app.import_projection().operations[0].start_available);
            assert!(
                invoke_import_raw(
                    &mut app,
                    &ImportOperation::Start {
                        operation_id: unsaved_id,
                    },
                )
                .is_err(),
                "an unconfirmed review must not start a copy"
            );
            let pending = app.pending_prepare_invocation.as_ref().unwrap();
            app.source = Some(pending.source.clone());
            app.destination_parent = Some(pending.destination_parent.clone());
            assert!(!app.import_projection().prepare_review_available);
            assert!(invoke_import_raw(&mut app, &ImportOperation::PrepareReview).is_err());
            assert_eq!(
                app.operations.len(),
                1,
                "a second review must not be queued"
            );
            let deadline = tilo_test_env::hang_deadline();
            loop {
                app.poll_background();
                if let Some(update) = Widget::poll_resource_operation(
                    &mut app,
                    invocation,
                    false,
                    &AutoCommit::new(),
                    &automerge::ROOT,
                ) {
                    assert!(matches!(
                        update,
                        tilo_actions::InvocationUpdate::Failed { ref error, .. }
                            if !error.retryable
                    ));
                    break;
                }
                assert!(ui::time::now() < deadline, "review save never settled");
                std::thread::yield_now();
            }
            assert!(
                app.operations.is_empty(),
                "failed review must not remain actionable"
            );
            assert!(app.source.is_some(), "source selection should be retained");
            assert!(
                app.destination_parent.is_some(),
                "destination selection should be retained"
            );
        }

        // Phase `identity_failure_yields_failed_folder_review_action`.
        {
            let (_temp, mut app, source, _destination_parent) = fixture();
            settle_index(&mut app);
            fs::write(source.join("document.txt"), b"unchanged original").unwrap();
            app.continuation =
                Some(Continuation::capture(&[continuation_choice(&source)]).unwrap());
            app.identity_ready = true;
            app.identity_checker = identity_denied;
            let review = action(&root_resource(&app).children[0], "review-destination");
            let invocation = tilo_actions::InvocationId::new();

            assert_eq!(
                start_action(&mut app, &review, invocation).unwrap(),
                ui::widget::ResourceOperationStart::Accepted
            );
            let deadline = tilo_test_env::hang_deadline();
            loop {
                app.poll_background();
                if let Some(update) = Widget::poll_resource_operation(
                    &mut app,
                    invocation,
                    false,
                    &AutoCommit::new(),
                    &automerge::ROOT,
                ) {
                    assert!(matches!(
                        update,
                        tilo_actions::InvocationUpdate::Failed { ref error, .. } if !error.retryable
                    ));
                    break;
                }
                assert!(ui::time::now() < deadline, "review failure never settled");
                std::thread::yield_now();
            }
        }

        // Phase `cancelled_folder_destination_picker_cancels_review_action`.
        {
            let (_temp, mut app, source, _destination_parent) = fixture();
            settle_index(&mut app);
            app.continuation =
                Some(Continuation::capture(&[continuation_choice(&source)]).unwrap());
            app.identity_ready = true;
            app.identity_checker = identity_ready;
            app.picker_launcher = cancelled_picker;
            let choose = action(&root_resource(&app).children[0], "choose-destination");
            let invocation = tilo_actions::InvocationId::new();

            assert_eq!(
                start_action(&mut app, &choose, invocation).unwrap(),
                ui::widget::ResourceOperationStart::Accepted
            );
            let deadline = tilo_test_env::hang_deadline();
            loop {
                app.poll_background();
                if let Some(update) = Widget::poll_resource_operation(
                    &mut app,
                    invocation,
                    false,
                    &AutoCommit::new(),
                    &automerge::ROOT,
                ) {
                    assert!(matches!(
                        update,
                        tilo_actions::InvocationUpdate::Cancelled { .. }
                    ));
                    break;
                }
                assert!(
                    ui::time::now() < deadline,
                    "picker cancellation never settled"
                );
                std::thread::yield_now();
            }
        }

        // Phase `disconnected_folder_picker_fails_the_review_action`.
        {
            let (_temp, mut app, source, _destination_parent) = fixture();
            settle_index(&mut app);
            app.continuation =
                Some(Continuation::capture(&[continuation_choice(&source)]).unwrap());
            app.identity_ready = true;
            app.identity_checker = identity_ready;
            app.picker_launcher = disconnected_picker;
            let choose = action(&root_resource(&app).children[0], "choose-destination");
            let invocation = tilo_actions::InvocationId::new();

            assert_eq!(
                start_action(&mut app, &choose, invocation).unwrap(),
                ui::widget::ResourceOperationStart::Accepted
            );
            // The identity gate runs on its own thread before the picker opens, so
            // one poll can observe neither the picker nor its failure yet.
            let deadline = tilo_test_env::hang_deadline();
            let update = loop {
                app.poll_background();
                if let Some(update) = Widget::poll_resource_operation(
                    &mut app,
                    invocation,
                    false,
                    &AutoCommit::new(),
                    &automerge::ROOT,
                ) {
                    break update;
                }
                assert!(
                    ui::time::now() < deadline,
                    "disconnected picker never failed the review action"
                );
                std::thread::yield_now();
            };
            assert!(
                app.gate_request.is_none(),
                "identity gate must have resolved"
            );
            assert!(
                app.picker.is_none(),
                "disconnected portal must clear the picker"
            );
            assert!(matches!(
                update,
                tilo_actions::InvocationUpdate::Failed { .. }
            ));
        }

        // Phase `semantic_picker_refuses_a_second_request_without_spawning_another_picker`.
        {
            let (_temp, mut app, _source, _destination_parent) = fixture();
            settle_index(&mut app);
            app.picker_launcher = cancelled_picker;
            let choose_source = action(&root_resource(&app), "choose-source");

            let pending = invoke_action(&mut app, &choose_source).unwrap();
            assert_eq!(pending["phase"], "choosing_folder");
            assert!(app.picker.is_some());

            let error = invoke_action(&mut app, &choose_source).unwrap_err();
            assert!(error.contains("chooser"), "{error}");
            assert!(app.picker.is_some());
            assert_eq!(app.status, error);

            let (_temp, mut button_app, _source, _destination_parent) = fixture();
            settle_index(&mut button_app);
            button_app.picker_launcher = cancelled_picker;
            button_app.apply_command(ImportCommand::PickSource);
            button_app.apply_command(ImportCommand::PickDestination);
            assert!(button_app.picker.is_some());
            assert!(button_app.status_is_error);
            assert!(button_app.status.contains("chooser"));
        }
    }

    /// Retained operation actions, continuation revisions and review matching.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn retained_and_continuation_actions_keep_revision_identities() {
        // Phase `retained_operation_action_tracks_its_id_after_reordering_and_stales_after_removal`.
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            settle_index(&mut app);
            fs::write(source.join("first.txt"), b"first").unwrap();
            app.source = Some(source);
            app.destination_parent = Some(destination_parent.clone());
            app.prepare_review().unwrap();
            settle_index(&mut app);

            let second_source = destination_parent.parent().unwrap().join("second-source");
            fs::create_dir(&second_source).unwrap();
            fs::write(second_source.join("second.txt"), b"second").unwrap();
            app.source = Some(second_source);
            app.destination_parent = Some(destination_parent);
            app.prepare_review().unwrap();
            let first_id = app.operations[0].id().to_string();
            let second_id = app.operations[1].id().to_string();
            let first_resource = root_resource(&app)
                .children
                .into_iter()
                .find(|resource| resource.id.0.ends_with(&first_id))
                .unwrap();
            let retained_start = action(&first_resource, "start");

            app.operations.swap(0, 1);
            invoke_action(&mut app, &retained_start).unwrap();
            wait_terminal(&mut app);
            assert_eq!(
                app.operations
                    .iter()
                    .find(|operation| operation.id() == first_id)
                    .unwrap()
                    .state(),
                OperationState::Complete
            );
            assert_eq!(
                app.operations
                    .iter()
                    .find(|operation| operation.id() == second_id)
                    .unwrap()
                    .state(),
                OperationState::AwaitingReview
            );

            app.operations
                .retain(|operation| operation.id() != second_id);
            let stale = invoke_import_raw(
                &mut app,
                &ImportOperation::Start {
                    operation_id: second_id,
                },
            )
            .unwrap_err();
            assert!(stale.contains("no longer available"), "{stale}");
            assert!(app.worker.is_none());
        }

        // Phase `continuation_actions_keep_revision_identities_and_reject_replaced_reviews`.
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            settle_index(&mut app);
            fs::write(source.join("first.txt"), b"first").unwrap();
            let second = source.parent().unwrap().join("second-choice");
            fs::create_dir(&second).unwrap();
            fs::write(second.join("second.txt"), b"second").unwrap();
            let continuation = Continuation::capture(&[
                continuation_choice(&source),
                continuation_choice(&second),
            ])
            .unwrap();
            let original_ids = [continuation.operation_id(0), continuation.operation_id(1)];
            app.continuation = Some(continuation);

            let folders = root_resource(&app)
                .children
                .into_iter()
                .filter(|resource| resource.kind == "imager.import.folder")
                .collect::<Vec<_>>();
            let retained_second_later = action(&folders[1], "later");
            let connected = action(&folders[0], "connected");
            assert!(matches!(
                connected.contract.availability,
                tilo_actions::Availability::Unavailable { .. }
            ));
            invoke_action(&mut app, &action(&folders[0], "later")).unwrap();
            assert_eq!(
                [
                    app.continuation.as_ref().unwrap().operation_id(0),
                    app.continuation.as_ref().unwrap().operation_id(1),
                ],
                original_ids
            );
            assert_eq!(
                app.continuation.as_ref().unwrap().folders[0].mode,
                ChoiceMode::Later
            );
            assert!(
                invoke_import_raw(
                    &mut app,
                    &ImportOperation::SetFolderConnected {
                        folder_id: original_ids[0].clone(),
                    },
                )
                .unwrap_err()
                .contains("not available")
            );

            let replacement_source = destination_parent.parent().unwrap().join("replacement");
            fs::create_dir(&replacement_source).unwrap();
            fs::write(replacement_source.join("new.txt"), b"new").unwrap();
            app.continuation =
                Some(Continuation::capture(&[continuation_choice(&replacement_source)]).unwrap());
            let replacement_id = app.continuation.as_ref().unwrap().operation_id(0);
            assert!(!original_ids.contains(&replacement_id));
            let error = invoke_action(&mut app, &retained_second_later).unwrap_err();
            assert!(error.contains("no longer available"), "{error}");
            assert_eq!(
                app.continuation.as_ref().unwrap().folders[0].mode,
                ChoiceMode::Copy
            );

            assert!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        import_actions::IMPORT_OPERATION_TYPE,
                        b"{not-json"
                    ),
                    &mut AutoCommit::new(),
                    &automerge::ROOT
                )
                .is_err()
            );
        }

        // Phase `continuation_batch_affordance_requires_identity_and_reviewed_copy`.
        {
            use ui::headless::{FrameHarness, SemanticExpectation};
            for (scenario, enabled) in [
                ("import-first-login", true),
                ("import-first-login-waiting", false),
                ("import-first-login-later", false),
            ] {
                let app = ImportApp::for_shot(scenario);
                let mut harness = FrameHarness::new(ui::State::new(), Box::new(app), (640, 640));
                harness
                    .expect(&SemanticExpectation::named("import.continued.start").enabled(enabled))
                    .unwrap();
                if enabled {
                    harness
                        .expect(
                            &SemanticExpectation::named("import.continued.0.review").enabled(true),
                        )
                        .unwrap();
                    harness
                        .expect(
                            &SemanticExpectation::named("import.continued.0.destination")
                                .enabled(true),
                        )
                        .unwrap();
                    assert!(
                        !harness
                            .a11y_nodes()
                            .iter()
                            .any(|(_, node)| node.label() == Some("Start copy"))
                    );
                }
            }
        }

        // Phase `mismatched_review_records_refuse_start_without_losing_rereview`.
        {
            let (_temp, mut app, source, parent) = fixture();
            settle_index(&mut app);
            let choice = super::super::controller::FolderChoice {
                scan: super::super::folders::scan_folder(&source).unwrap(),
                destination: super::super::controller::FolderDestination::Unresolved,
                mode: super::super::model::FolderMode::Copy,
                mode_overridden: false,
            };
            let mut old = Continuation::capture(&[choice]).unwrap();
            old.resolve(0, &parent, true).unwrap();
            let old_record = old
                .prepare(0, app.store.as_ref().unwrap(), &[], true)
                .unwrap()
                .unwrap();
            let another = parent.join("another");
            fs::create_dir(&another).unwrap();
            let mut new = old.clone();
            new.select_destination_parent(0, &another).unwrap();
            new.resolve(0, &another, true).unwrap();
            let new_record = new
                .prepare(0, app.store.as_ref().unwrap(), &[], true)
                .unwrap()
                .unwrap();
            // Either ordering of the two durable writes may survive a crash.
            for (continuation, record) in [(new, old_record), (old, new_record)] {
                continuation.save(app.store.as_ref().unwrap()).unwrap();
                app.store.as_ref().unwrap().save(&[record]).unwrap();
                app.continuation = Continuation::load(app.store.as_ref().unwrap()).unwrap();
                app.operations = app.store.as_ref().unwrap().load().unwrap();
                app.identity_ready = true;
                assert!(app.start_copy(0).is_err());
                assert!(app.worker.is_none());
                assert_eq!(app.operations[0].state(), OperationState::AwaitingReview);
                assert!(app.check_continued_review_allowed(0).is_ok());
                app.prepare_continued_folder(0).unwrap();
                settle_index(&mut app);
                assert_eq!(
                    app.operations[0].destination().unwrap(),
                    app.continuation.as_ref().unwrap().folders[0]
                        .destination()
                        .unwrap()
                        .unwrap()
                );
            }
            assert!(!parent.join("source").exists());
            assert!(!another.join("source").exists());
        }
    }

    /// Reviewed batches, completion, identity failures, cancellation and refused stops.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn copy_action_completes_only_after_the_reviewed_worker_finishes() {
        // Phase `reviewed_batch_rechecks_identity_and_starts_only_reviewed_copy_rows`.
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            settle_index(&mut app);
            fs::write(source.join("first.txt"), b"first").unwrap();
            let second = source.parent().unwrap().join("batch-second");
            fs::create_dir(&second).unwrap();
            fs::write(second.join("second.txt"), b"second").unwrap();
            let mut continuation = Continuation::capture(&[
                continuation_choice(&source),
                continuation_choice(&second),
            ])
            .unwrap();
            continuation.resolve(0, &destination_parent, true).unwrap();
            continuation.resolve(1, &destination_parent, true).unwrap();
            app.continuation = Some(continuation);
            app.identity_ready = true;
            app.prepare_continued_folder(0).unwrap();
            app.prepare_continued_folder(1).unwrap();
            settle_index(&mut app);
            app.save_continued_mode(1, ChoiceMode::Later).unwrap();
            let copy_id = app.continuation.as_ref().unwrap().operation_id(0);
            let later_id = app.continuation.as_ref().unwrap().operation_id(1);
            app.identity_checker = identity_ready;
            let start_batch = action(&root_resource(&app), "start-reviewed-folders");

            let pending = invoke_action(&mut app, &start_batch).unwrap();
            assert_eq!(pending["phase"], "checking_identity");
            assert!(app.gate_request.is_some());
            assert!(app.worker.is_none());
            let deadline = tilo_test_env::hang_deadline();
            while app.gate_request.is_some() {
                app.poll_background();
                assert!(ui::time::now() < deadline, "identity check did not finish");
                std::thread::yield_now();
            }
            assert!(
                app.worker.is_some(),
                "batch bypassed the shared worker start"
            );
            wait_terminal(&mut app);
            assert_eq!(
                app.operations
                    .iter()
                    .find(|operation| operation.id() == copy_id)
                    .unwrap()
                    .state(),
                OperationState::Complete
            );
            assert_eq!(
                app.operations
                    .iter()
                    .find(|operation| operation.id() == later_id)
                    .unwrap()
                    .state(),
                OperationState::AwaitingReview
            );
        }

        // Phase `copy_action_succeeds_only_after_the_worker_finishes`.
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            settle_index(&mut app);
            fs::write(source.join("document.txt"), b"copied bytes").unwrap();
            app.source = Some(source);
            app.destination_parent = Some(destination_parent);
            app.prepare_review().unwrap();
            let start = action(&root_resource(&app).children[0], "start");
            let invocation = tilo_actions::InvocationId::new();

            assert_eq!(
                start_action(&mut app, &start, invocation).unwrap(),
                ui::widget::ResourceOperationStart::Accepted
            );
            assert!(
                Widget::poll_resource_operation(
                    &mut app,
                    invocation,
                    false,
                    &AutoCommit::new(),
                    &automerge::ROOT,
                )
                .is_none()
            );

            let deadline = tilo_test_env::hang_deadline();
            loop {
                app.poll_background();
                if let Some(update) = Widget::poll_resource_operation(
                    &mut app,
                    invocation,
                    false,
                    &AutoCommit::new(),
                    &automerge::ROOT,
                ) {
                    assert!(matches!(
                        update,
                        tilo_actions::InvocationUpdate::Succeeded { .. }
                    ));
                    break;
                }
                assert!(ui::time::now() < deadline, "copy action never settled");
                std::thread::yield_now();
            }
            assert_eq!(app.operations[0].state(), OperationState::Complete);
        }

        // Phase `identity_failure_yields_failed_copy_action_instead_of_success`.
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            settle_index(&mut app);
            fs::write(source.join("document.txt"), b"unchanged original").unwrap();
            let mut continuation = Continuation::capture(&[continuation_choice(&source)]).unwrap();
            continuation.resolve(0, &destination_parent, true).unwrap();
            app.continuation = Some(continuation);
            app.identity_ready = true;
            app.prepare_continued_folder(0).unwrap();
            app.identity_checker = identity_denied;
            let start = action(&root_resource(&app), "start-reviewed-folders");
            let invocation = tilo_actions::InvocationId::new();

            assert_eq!(
                start_action(&mut app, &start, invocation).unwrap(),
                ui::widget::ResourceOperationStart::Accepted
            );
            let deadline = tilo_test_env::hang_deadline();
            loop {
                app.poll_background();
                if let Some(update) = Widget::poll_resource_operation(
                    &mut app,
                    invocation,
                    false,
                    &AutoCommit::new(),
                    &automerge::ROOT,
                ) {
                    assert!(matches!(
                        update,
                        tilo_actions::InvocationUpdate::Failed { ref error, .. } if !error.retryable
                    ));
                    break;
                }
                assert!(ui::time::now() < deadline, "identity failure never settled");
                std::thread::yield_now();
            }
            assert!(app.worker.is_none());
            assert_eq!(app.operations[0].state(), OperationState::AwaitingReview);
        }

        // Phase `broker_cancellation_stops_a_queued_copy_before_the_worker_starts`.
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            settle_index(&mut app);
            fs::write(source.join("document.txt"), b"unchanged original").unwrap();
            let mut continuation = Continuation::capture(&[continuation_choice(&source)]).unwrap();
            continuation.resolve(0, &destination_parent, true).unwrap();
            app.continuation = Some(continuation);
            app.identity_ready = true;
            app.prepare_continued_folder(0).unwrap();
            app.identity_checker = identity_ready;
            let start = action(&root_resource(&app), "start-reviewed-folders");
            let invocation = tilo_actions::InvocationId::new();

            assert_eq!(
                start_action(&mut app, &start, invocation).unwrap(),
                ui::widget::ResourceOperationStart::Accepted
            );
            assert!(app.gate_request.is_some());
            let update = Widget::poll_resource_operation(
                &mut app,
                invocation,
                true,
                &AutoCommit::new(),
                &automerge::ROOT,
            );
            assert!(matches!(
                update,
                Some(tilo_actions::InvocationUpdate::Cancelled { .. })
            ));
            assert!(app.gate_request.is_none());
            app.poll_background();
            assert!(app.worker.is_none());
            assert_eq!(app.operations[0].state(), OperationState::AwaitingReview);
        }

        // Phase `stop_copy_command_refuses_when_no_worker_accepts_the_request`.
        {
            let (_temp, mut app, _source, _destination_parent) = fixture();
            settle_index(&mut app);
            app.apply_authorized_command(ImportCommand::Cancel);
            assert!(app.status_is_error);
            assert!(app.status.contains("could not be stopped"));
        }
    }

    /// Semantic cancel/retry, restart retry and partial reports.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn retry_and_restart_resume_the_journaled_worker_honestly() {
        // Phase `semantic_cancel_and_retry_use_the_existing_journaled_worker`.
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            settle_index(&mut app);
            for index in 0..32 {
                fs::write(
                    source.join(format!("archive-{index:02}.bin")),
                    vec![index as u8; 64 * 1024],
                )
                .unwrap();
            }
            app.source = Some(source);
            app.destination_parent = Some(destination_parent);
            app.prepare_review().unwrap();
            let operation = root_resource(&app).children.pop().unwrap();

            invoke_action(&mut app, &action(&operation, "start")).unwrap();
            assert!(app.worker.is_some());
            let deadline = tilo_test_env::hang_deadline();
            while app.worker.is_some() && app.operations[0].verified_file_count() == 0 {
                app.poll_background();
                assert!(
                    ui::time::now() < deadline,
                    "copy made no journaled progress"
                );
                std::thread::yield_now();
            }
            assert!(
                app.worker.is_some(),
                "copy finished before cancellation could be tested"
            );
            assert!(app.operations[0].journal().unwrap().is_file());
            let running = root_resource(&app).children.pop().unwrap();
            invoke_action(&mut app, &action(&running, "cancel")).unwrap();
            wait_terminal(&mut app);
            assert_eq!(app.operations[0].state(), OperationState::Cancelled);

            let cancelled = root_resource(&app).children.pop().unwrap();
            invoke_action(&mut app, &action(&cancelled, "retry")).unwrap();
            assert!(app.worker.is_some());
            wait_terminal(&mut app);
            assert_eq!(app.operations[0].state(), OperationState::Complete);
            assert_eq!(app.operations[0].verified_file_count(), 32);
        }

        // Phase `application_restart_requires_explicit_retry_and_reaches_verified_completion`.
        // Catches an application restart turning a cancelled operation into a
        // fresh overwrite attempt instead of an explicit journal resume.
        {
            let (temp, mut app, source, destination_parent) = fixture();
            for index in 0..24 {
                fs::write(
                    source.join(format!("file-{index}")),
                    vec![index as u8; 1024 * 1024],
                )
                .unwrap();
            }
            app.source = Some(source);
            app.destination_parent = Some(destination_parent);
            app.prepare_review().unwrap();
            app.start_copy(0).unwrap();
            let deadline = tilo_test_env::hang_deadline();
            loop {
                app.poll_background();
                if app.operations[0].verified_file_count() > 0 {
                    assert!(app.cancel_active());
                    break;
                }
                assert!(ui::time::now() < deadline, "copy did not publish progress");
                std::thread::yield_now();
            }
            wait_terminal(&mut app);
            assert_eq!(app.operations[0].state(), OperationState::Cancelled);

            drop(app);
            let deadline = tilo_test_env::hang_deadline();
            let store = loop {
                if let Ok(store) =
                    MigrationStore::open_at(&temp.path().canonicalize().unwrap().join("state"))
                {
                    break store;
                }
                assert!(
                    ui::time::now() < deadline,
                    "previous writer retained store ownership"
                );
                std::thread::yield_now();
            };
            let mut reopened = ImportApp::with_store(store).unwrap();
            assert_eq!(reopened.operations[0].state(), OperationState::Cancelled);
            assert!(reopened.operations[0].verified_file_count() > 0);
            reopened.retry_operation(0).unwrap();
            assert_eq!(reopened.operations[0].state(), OperationState::Running);
            assert_eq!(reopened.operations[0].verified_file_count(), 0);
            assert_eq!(reopened.operations[0].verified_bytes(), 0);
            assert!(reopened.operations[0].issues().unwrap().is_empty());
            assert!(
                reopened.operations[0]
                    .current_relative_path()
                    .unwrap()
                    .is_none()
            );
            wait_terminal(&mut reopened);

            assert_eq!(reopened.operations[0].state(), OperationState::Complete);
        }

        // Phase `partial_worker_report_remains_visible_as_incomplete`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let (_temp, mut app, source, destination_parent) = fixture();
            fs::write(source.join("regular"), b"kept").unwrap();
            symlink(source.join("regular"), source.join("linked")).unwrap();
            app.source = Some(source);
            app.destination_parent = Some(destination_parent);
            app.prepare_review().unwrap();
            app.start_copy(0).unwrap();
            wait_terminal(&mut app);

            assert_eq!(app.operations[0].state(), OperationState::Incomplete);
            assert_eq!(app.operations[0].verified_bytes(), 4);
            assert!(
                app.operations[0]
                    .issues()
                    .unwrap()
                    .iter()
                    .any(|(path, _)| path == Path::new("linked"))
            );
        }
    }

    /// Issue detail pages.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn issue_detail_pages_publish_restore_and_refuse_when_closed() {
        // Phase `semantic_issue_details_publish_completed_pages_through_repeatable_public_reads`.
        {
            let mut app = ImportApp::try_for_shot("import-issue-pages").unwrap();
            let operation = root_resource(&app).children.pop().unwrap();
            let next = action(&operation, "next-issues");
            assert!(matches!(
                next.contract.availability,
                tilo_actions::Availability::Available
            ));

            let pending = invoke_action(&mut app, &next).unwrap();
            assert_eq!(pending["phase"], "loading_details");
            let observed = wait_for_public_issue_count(&mut app, 8);
            let issues = observed["operations"][0]["issues"].as_array().unwrap();
            assert_eq!(issues.len(), 8);
            assert_eq!(issues[0]["path"], "Shortcut 32");
            assert!(issues.iter().all(|issue| {
                issue["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("symbolic link"))
            }));
            assert_eq!(observed["operations"][0]["issue_count"], 40);
            assert_eq!(observed["operations"][0]["details_loading"], false);
            assert_eq!(observed["operations"][0]["next_issues_available"], false);
            assert_eq!(observe_import(&mut app).unwrap(), observed);
            let operation = root_resource(&app).children.pop().unwrap();
            assert!(matches!(
                action(&operation, "next-issues").contract.availability,
                tilo_actions::Availability::Unavailable { .. }
            ));
        }

        // Phase `first_issues_action_is_unavailable_on_a_single_final_page`.
        #[cfg(unix)]
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            settle_index(&mut app);
            fs::write(source.join("document.txt"), b"original").unwrap();
            for index in 0..9 {
                std::os::unix::fs::symlink(
                    source.join("document.txt"),
                    source.join(format!("link-{index}")),
                )
                .unwrap();
            }
            app.source = Some(source);
            app.destination_parent = Some(destination_parent);
            app.prepare_review().unwrap();
            let start = action(&root_resource(&app).children[0], "start");
            invoke_action(&mut app, &start).unwrap();
            wait_terminal(&mut app);
            let operation = root_resource(&app).children.remove(0);
            invoke_action(&mut app, &action(&operation, "toggle-details")).unwrap();
            wait_for_public_issue_count(&mut app, 9);

            let observed = observe_import(&mut app).unwrap();
            assert_eq!(observed["operations"][0]["next_issues_available"], false);
            assert_eq!(observed["operations"][0]["first_issues_available"], false);
        }

        // Phase `semantic_first_issue_page_restores_the_original_page_and_closes_both_payloads`.
        {
            let mut app = ImportApp::try_for_shot("import-issue-pages").unwrap();
            let operation = root_resource(&app).children.pop().unwrap();
            let retained_next = action(&operation, "next-issues");
            invoke_action(&mut app, &retained_next).unwrap();
            let terminal = wait_for_public_issue_count(&mut app, 8);
            assert_eq!(terminal["operations"][0]["next_issues_available"], false);

            let operation = root_resource(&app).children.pop().unwrap();
            let first = action(&operation, "first-issues");
            assert!(matches!(
                first.contract.availability,
                tilo_actions::Availability::Available
            ));
            let close = action(&operation, "toggle-details");
            invoke_action(&mut app, &close).unwrap();
            let closed = root_resource(&app).children.pop().unwrap();
            assert!(matches!(
                action(&closed, "first-issues").contract.availability,
                tilo_actions::Availability::Unavailable { .. }
            ));
            assert!(invoke_action(&mut app, &retained_next).is_err());
            assert!(invoke_action(&mut app, &first).is_err());

            let reopen = action(&closed, "toggle-details");
            invoke_action(&mut app, &reopen).unwrap();
            assert_eq!(
                observe_import(&mut app).unwrap()["operations"][0]["issues"]
                    .as_array()
                    .unwrap()
                    .len(),
                8
            );
            let first = action(&root_resource(&app).children.pop().unwrap(), "first-issues");
            invoke_action(&mut app, &first).unwrap();
            let restored = wait_for_public_issue_count(&mut app, 32);
            assert_eq!(restored["operations"][0]["next_issues_available"], true);
            let restored_operation = root_resource(&app).children.pop().unwrap();
            assert!(matches!(
                action(&restored_operation, "next-issues")
                    .contract
                    .availability,
                tilo_actions::Availability::Available
            ));
            assert!(matches!(
                action(&restored_operation, "first-issues")
                    .contract
                    .availability,
                tilo_actions::Availability::Unavailable { .. }
            ));
        }

        // Phase `terminal_issue_button_uses_the_production_path_to_restore_the_first_page`.
        {
            use ui::headless::{FrameHarness, SemanticExpectation};

            let mut app = ImportApp::try_for_shot("import-issue-pages").unwrap();
            let operation = root_resource(&app).children.pop().unwrap();
            invoke_action(&mut app, &action(&operation, "next-issues")).unwrap();
            wait_for_public_issue_count(&mut app, 8);

            let card = app.operation_card(0, &app.operations[0]);
            let mut harness = FrameHarness::new(ui::State::new(), Box::new(card), (640, 640));
            harness
                .expect(
                    &SemanticExpectation::named("import.operation.0.next-issues")
                        .name_contains("First issue page")
                        .enabled(true),
                )
                .unwrap();
            harness
                .perform_named(
                    "import.operation.0.next-issues",
                    &ui::a11y::Action::Activate,
                )
                .unwrap();
            drop(harness);

            let mut state = AutoCommit::new();
            Widget::update(&mut app, &mut state, &automerge::ROOT);
            let restored = wait_for_public_issue_count(&mut app, 32);
            assert_eq!(restored["operations"][0]["next_issues_available"], true);
        }

        // Phase `retained_next_issue_actions_refuse_while_details_are_closed`.
        {
            let mut app = ImportApp::try_for_shot("import-issue-pages").unwrap();
            let operation = root_resource(&app).children.pop().unwrap();
            let retained_next = action(&operation, "next-issues");
            let close = action(&operation, "toggle-details");
            invoke_action(&mut app, &close).unwrap();

            let closed = root_resource(&app).children.pop().unwrap();
            assert!(matches!(
                action(&closed, "next-issues").contract.availability,
                tilo_actions::Availability::Unavailable { .. }
            ));
            let retained_error = invoke_action(&mut app, &retained_next).unwrap_err();
            assert!(retained_error.contains("not available"), "{retained_error}");
            let operation_id = closed.id.as_str().rsplit('.').next().unwrap().to_string();
            let direct_error =
                invoke_import_raw(&mut app, &ImportOperation::NextIssues { operation_id })
                    .unwrap_err();
            assert!(direct_error.contains("not available"), "{direct_error}");

            let mut state = AutoCommit::new();
            for _ in 0..8 {
                Widget::update(&mut app, &mut state, &automerge::ROOT);
                std::thread::yield_now();
            }
            let reopen = action(
                &root_resource(&app).children.pop().unwrap(),
                "toggle-details",
            );
            invoke_action(&mut app, &reopen).unwrap();
            let observed = observe_import(&mut app).unwrap();
            assert_eq!(
                observed["operations"][0]["issues"]
                    .as_array()
                    .unwrap()
                    .len(),
                32
            );
            assert_eq!(observed["operations"][0]["details_open"], true);
            assert_eq!(observed["operations"][0]["next_issues_available"], true);
        }
    }

    /// Continuation collisions, basename retargeting, reopen and retry.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn continuation_batches_retarget_and_retry_without_duplicate_operations() {
        // Phase `unpublished_continuation_can_review_another_destination_after_collision`.
        {
            for substitute_parent in [false, true] {
                let (_temp, mut app, source, parent) = fixture();
                settle_index(&mut app);
                fs::write(source.join("hello"), b"original").unwrap();
                let choice = super::super::controller::FolderChoice {
                    scan: super::super::folders::scan_folder(&source).unwrap(),
                    destination: super::super::controller::FolderDestination::Unresolved,
                    mode: super::super::model::FolderMode::Copy,
                    mode_overridden: false,
                };
                let mut c = Continuation::capture(&[choice]).unwrap();
                c.resolve(0, &parent, true).unwrap();
                app.continuation = Some(c);
                app.identity_ready = true;
                app.prepare_continued_folder(0).unwrap();
                settle_index(&mut app);
                if substitute_parent {
                    fs::rename(&parent, parent.with_extension("old")).unwrap();
                    fs::create_dir(&parent).unwrap();
                }
                fs::create_dir(parent.join("source")).unwrap();
                fs::write(parent.join("source/existing"), b"keep").unwrap();
                assert!(app.start_copy(0).is_err());
                assert_eq!(app.operations[0].state(), OperationState::AwaitingReview);
                assert!(!app.operations[0].journal().unwrap().exists());
                let another = parent.join("another");
                fs::create_dir(&another).unwrap();
                app.request_continued_review(0, Some(another.clone()))
                    .unwrap();
                assert!(
                    app.needs_idle_sync(),
                    "background review must keep native polling alive"
                );
                let deadline = tilo_test_env::hang_deadline();
                while app.review_request.is_some() {
                    app.poll_background();
                    assert!(ui::time::now() < deadline);
                    std::thread::yield_now();
                }
                assert!(!app.status_is_error, "{}", app.status);
                settle_index(&mut app);
                assert_eq!(app.operations.len(), 1);
                assert_eq!(
                    app.operations[0].destination().unwrap(),
                    another.join("source")
                );
                app.start_copy(0).unwrap();
                wait_terminal(&mut app);
                assert_eq!(
                    app.operations[0].state(),
                    OperationState::Complete,
                    "{}",
                    app.status
                );
                assert_eq!(fs::read(another.join("source/hello")).unwrap(), b"original");
                assert_eq!(fs::read(parent.join("source/existing")).unwrap(), b"keep");
                assert!(
                    app.request_continued_review(0, Some(parent.clone()))
                        .is_err()
                );
            }
        }

        // Phase `same_basename_folders_can_retarget_the_second_without_changing_first_output`.
        {
            let (_temp, mut app, source, parent) = fixture();
            settle_index(&mut app);
            let second = source.with_extension("other").join("source");
            fs::create_dir_all(&second).unwrap();
            fs::write(source.join("hello"), b"first").unwrap();
            fs::write(second.join("hello"), b"second").unwrap();
            let choices: Vec<_> = [&source, &second]
                .into_iter()
                .map(|path| super::super::controller::FolderChoice {
                    scan: super::super::folders::scan_folder(path).unwrap(),
                    destination: super::super::controller::FolderDestination::Unresolved,
                    mode: super::super::model::FolderMode::Copy,
                    mode_overridden: false,
                })
                .collect();
            let mut c = Continuation::capture(&choices).unwrap();
            c.resolve(0, &parent, true).unwrap();
            c.resolve(1, &parent, true).unwrap();
            app.continuation = Some(c);
            app.identity_ready = true;
            app.prepare_continued_folder(0).unwrap();
            app.prepare_continued_folder(1).unwrap();
            settle_index(&mut app);
            app.start_copy(0).unwrap();
            wait_terminal(&mut app);
            assert_eq!(
                app.operations[0].state(),
                OperationState::Complete,
                "{}",
                app.status
            );
            assert!(app.start_copy(1).is_err());
            assert_eq!(app.operations[1].state(), OperationState::AwaitingReview);
            let another = parent.join("another");
            fs::create_dir(&another).unwrap();
            app.request_continued_review(1, Some(another.clone()))
                .unwrap();
            let deadline = tilo_test_env::hang_deadline();
            while app.review_request.is_some() {
                app.poll_background();
                assert!(ui::time::now() < deadline);
                std::thread::yield_now();
            }
            assert!(!app.status_is_error, "{}", app.status);
            settle_index(&mut app);
            app.start_copy(1).unwrap();
            wait_terminal(&mut app);
            assert_eq!(
                app.operations[1].state(),
                OperationState::Complete,
                "{}",
                app.status
            );
            assert_eq!(fs::read(parent.join("source/hello")).unwrap(), b"first");
            assert_eq!(fs::read(another.join("source/hello")).unwrap(), b"second");
            assert_eq!(app.operations.len(), 2);
        }

        // Phase `continuation_batch_uses_existing_worker_and_reopen_does_not_duplicate`.
        {
            let (temp, mut app, source, destination_parent) = fixture();
            settle_index(&mut app);
            fs::write(source.join("hello"), b"hello").unwrap();
            let choice = super::super::controller::FolderChoice {
                scan: super::super::folders::FolderScan {
                    source: source.clone(),
                    file_count: 1,
                    bytes: 5,
                    empty_directory_count: 0,
                    empty_directories: vec![],
                    skipped_count: 0,
                    skipped: vec![],
                },
                destination: super::super::controller::FolderDestination::Unresolved,
                mode: super::super::model::FolderMode::Copy,
                mode_overridden: false,
            };
            let mut controller =
                super::super::controller::SetupController::new(super::super::model::Capabilities {
                    host: super::super::model::HostOs::Linux,
                    desktop_session: true,
                    internal_dual_boot: false,
                });
            assert!(controller.choose_experience(super::super::model::Experience::Desktop));
            controller.next().unwrap();
            controller.add_folder_unresolved(choice.scan);
            controller.next().unwrap();
            let mut continuation = Continuation::capture(controller.folders()).unwrap();
            assert!(continuation.resolve(0, &destination_parent, false).is_err());
            continuation.resolve(0, &destination_parent, true).unwrap();
            continuation.save(app.store.as_ref().unwrap()).unwrap();
            app.continuation = Some(continuation);
            app.identity_ready = true;
            app.prepare_continued_folder(0).unwrap();
            settle_index(&mut app);
            assert_eq!(app.operations.len(), 1);
            app.start_copy(0).unwrap();
            wait_terminal(&mut app);
            assert_eq!(
                app.operations[0].state(),
                OperationState::Complete,
                "{} / {:?}",
                app.status,
                app.operations[0].failure()
            );
            assert_eq!(fs::read(source.join("hello")).unwrap(), b"hello");
            assert_eq!(
                fs::read(destination_parent.join("source/hello")).unwrap(),
                b"hello"
            );
            assert!(app.start_copy(0).is_err());
            settle_index(&mut app);
            let root = temp.path().canonicalize().unwrap().join("state");
            drop(app);
            let deadline = tilo_test_env::hang_deadline();
            let store = loop {
                if let Ok(store) = MigrationStore::open_at(&root) {
                    break store;
                }
                assert!(ui::time::now() < deadline);
                std::thread::yield_now();
            };
            let mut reopened = ImportApp::with_store(store).unwrap();
            reopened.identity_ready = true;
            assert_eq!(reopened.operations.len(), 1);
            reopened.prepare_continued_folder(0).unwrap();
            assert_eq!(reopened.operations.len(), 1);
            assert_eq!(reopened.operations[0].state(), OperationState::Complete);
        }

        // Phase `interrupted_before_journal_creation_can_explicitly_retry_without_new_operation`.
        {
            let (_temp, mut app, source, parent) = fixture();
            settle_index(&mut app);
            fs::write(source.join("hello"), b"original").unwrap();
            let choice = super::super::controller::FolderChoice {
                scan: super::super::folders::FolderScan {
                    source: source.clone(),
                    file_count: 1,
                    bytes: 8,
                    empty_directory_count: 0,
                    empty_directories: vec![],
                    skipped_count: 0,
                    skipped: vec![],
                },
                destination: super::super::controller::FolderDestination::Unresolved,
                mode: super::super::model::FolderMode::Copy,
                mode_overridden: false,
            };
            let mut c = Continuation::capture(&[choice]).unwrap();
            c.resolve(0, &parent, true).unwrap();
            let mut op = c
                .prepare(0, app.store.as_ref().unwrap(), &[], true)
                .unwrap()
                .unwrap();
            op.approve();
            op.mark_running().unwrap();
            app.store.as_ref().unwrap().save(&[op]).unwrap();
            app.operations = app.store.as_ref().unwrap().load().unwrap();
            c.save(app.store.as_ref().unwrap()).unwrap();
            app.continuation = Some(c);
            app.identity_ready = true;
            assert_eq!(app.operations[0].state(), OperationState::Interrupted);
            app.retry_operation(0).unwrap();
            wait_terminal(&mut app);
            assert_eq!(app.operations.len(), 1);
            assert_eq!(
                app.operations[0].state(),
                OperationState::Complete,
                "{}",
                app.status
            );
            assert_eq!(fs::read(source.join("hello")).unwrap(), b"original");
        }
    }

    #[test]
    fn slow_start_checkpoint_keeps_stop_responsive_and_ownership_until_worker_exit() {
        let (_temp, mut app, source, destination_parent) = fixture();
        settle_index(&mut app);
        fs::write(source.join("hello"), b"hello").unwrap();
        app.source = Some(source);
        app.destination_parent = Some(destination_parent.clone());
        app.prepare_review().unwrap();
        settle_index(&mut app);
        let root = app.store.as_ref().unwrap().root().to_owned();
        // The save stays held until this thread releases it. A UI call that
        // waited for it would only return once the hold timed out, which
        // drops the release receiver and fails the send below.
        let (entered, release) = super::super::migration_store::test_support::hold_save(&root, 1);
        app.start_copy(0).unwrap();
        tilo_test_env::recv("the save hook", &entered);
        assert!(!destination_parent.join("source").exists());
        app.poll_background();
        assert!(app.cancel_active());
        drop(app);
        assert!(MigrationStore::open_at(&root).is_err());
        release
            .send(())
            .expect("the start checkpoint was still held when the UI calls returned");
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Ok(store) = MigrationStore::open_at(&root) {
                assert_eq!(store.load().unwrap()[0].state(), OperationState::Cancelled);
                break;
            }
            assert!(ui::time::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!destination_parent.join("source").exists());
    }

    #[test]
    fn slow_terminal_checkpoint_never_reports_premature_completion() {
        let (_temp, mut app, source, destination_parent) = fixture();
        settle_index(&mut app);
        fs::write(source.join("hello"), b"hello").unwrap();
        app.source = Some(source);
        app.destination_parent = Some(destination_parent);
        app.prepare_review().unwrap();
        settle_index(&mut app);
        let root = app.store.as_ref().unwrap().root().to_owned();
        let (entered, release) = super::super::migration_store::test_support::hold_save(&root, 2);
        app.start_copy(0).unwrap();
        // Native durability includes directory fsyncs; allow slower virtual
        // disks to reach the terminal checkpoint without weakening the state
        // assertion that follows.
        tilo_test_env::recv("the save hook", &entered);
        app.poll_background();
        assert_eq!(app.operations[0].state(), OperationState::Running);
        // Polling must not wait for the held checkpoint: had it waited, the
        // hold would have timed out and dropped its release receiver.
        release
            .send(())
            .expect("the terminal checkpoint was still held when polling returned");
        wait_terminal(&mut app);
        assert_eq!(app.operations[0].state(), OperationState::Complete);
    }

    /// Failed terminal and start persistence.
    #[test]
    fn failed_checkpoints_never_report_completion_or_create_output() {
        // Phase `failed_terminal_checkpoint_retains_an_honest_incomplete_result`.
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            settle_index(&mut app);
            fs::write(source.join("hello"), b"hello").unwrap();
            app.source = Some(source.clone());
            app.destination_parent = Some(destination_parent.clone());
            app.prepare_review().unwrap();
            settle_index(&mut app);
            let root = app.store.as_ref().unwrap().root().to_owned();
            let (entered, release) =
                super::super::migration_store::test_support::hold_save(&root, 2);
            app.start_copy(0).unwrap();
            // Native durability includes directory fsyncs; allow slower virtual
            // disks to reach the terminal checkpoint without weakening the state
            // assertion that follows.
            tilo_test_env::recv("the save hook", &entered);
            fs::rename(root.join("ownership.lock"), root.join("old-lock")).unwrap();
            release.send(()).unwrap();
            wait_terminal(&mut app);
            assert_eq!(app.operations[0].state(), OperationState::Incomplete);
            assert_eq!(app.operations[0].verified_bytes(), 5);
            assert!(app.status.contains("could not be saved"));
            assert_eq!(fs::read(source.join("hello")).unwrap(), b"hello");
            assert_eq!(
                fs::read(destination_parent.join("source/hello")).unwrap(),
                b"hello"
            );
        }

        // Phase `failed_start_persistence_never_creates_output`.
        {
            let (_temp, mut app, source, destination_parent) = fixture();
            settle_index(&mut app);
            app.source = Some(source);
            app.destination_parent = Some(destination_parent.clone());
            app.prepare_review().unwrap();
            settle_index(&mut app);
            fs::rename(
                app.store.as_ref().unwrap().root().join("ownership.lock"),
                app.store.as_ref().unwrap().root().join("replaced-lock"),
            )
            .unwrap();
            app.start_copy(0).unwrap();
            wait_terminal(&mut app);
            assert_eq!(app.operations[0].state(), OperationState::Failed);
            assert!(!destination_parent.join("source").exists());
            assert!(
                app.operations[0]
                    .failure()
                    .unwrap()
                    .contains("could not be saved")
            );
        }
    }
}

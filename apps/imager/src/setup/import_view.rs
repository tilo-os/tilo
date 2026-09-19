//! Standalone later-folder import UI.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

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
use winit::event::{Modifiers, WindowEvent};

use super::desktop_continuation::{self, ChoiceMode, Continuation};
use super::folders;
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

type CommandSink = Rc<RefCell<Vec<ImportCommand>>>;

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
    picker: Option<(PickTarget, ui::PortalChannel)>,
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
            picker: None,
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
        let temporary_parent = dirs::data_local_dir()
            .filter(|path| path.is_dir())
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(std::env::temp_dir);
        let temp = tempfile::tempdir_in(temporary_parent).map_err(|error| error.to_string())?;
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
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
                app.wait_for_shot_terminal(deadline)?;
                app.expanded_details.insert(0);
                app.load_issue_page(0, 0)?;
                while app.detail_request.is_some() {
                    app.poll_background();
                    if std::time::Instant::now() > deadline {
                        return Err("issue-page screenshot timed out".into());
                    }
                    std::thread::yield_now();
                }
            }
            _ => {
                std::fs::write(source.join("Welcome.txt"), b"verified import")
                    .map_err(|error| error.to_string())?;
                app.prepare_and_start_shot(&source, &destination_parent)?;
                app.wait_for_shot_terminal(
                    std::time::Instant::now() + std::time::Duration::from_secs(15),
                )?;
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
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
            if std::time::Instant::now() >= deadline {
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
        self.wait_for_shot_terminal(std::time::Instant::now() + std::time::Duration::from_secs(15))
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
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
            if std::time::Instant::now() >= deadline {
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
            if std::time::Instant::now() >= deadline || self.worker.is_none() {
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
        self.wait_for_shot_terminal(
            std::time::Instant::now() + std::time::Duration::from_secs(15),
        )?;
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
        self.start_operation(0)
    }

    fn wait_for_shot_terminal(&mut self, deadline: std::time::Instant) -> Result<(), String> {
        while self.worker.is_some() {
            self.poll_background();
            if std::time::Instant::now() >= deadline {
                return Err("screenshot worker timed out".into());
            }
            std::thread::yield_now();
        }
        Ok(())
    }

    fn run_shot_worker(operation: &mut OperationRecord, job: MigrationJob) -> Result<(), String> {
        let mut worker = MigrationWorker::start(job)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
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
            if std::time::Instant::now() >= deadline {
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
            picker: None,
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

    fn prepare_review(&mut self) -> Result<(), String> {
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
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
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
        if self.worker.is_some() || self.review_request.is_some() {
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
                        let parent = dirs::data_local_dir()
                            .ok_or("No local user data directory")?
                            .join("qos/files");
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
        let result = result.and_then(|c| {
            self.check_continued_review_allowed(index)?;
            let previous = self.continuation.replace(c);
            if let Err(error) = self.prepare_continued_folder(index) {
                self.continuation = previous;
                return Err(error);
            }
            Ok(())
        });
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
        if self.worker.is_some() {
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
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("import-session-check".into())
            .spawn(move || {
                let _ = tx.send(desktop_continuation::trusted_identity_complete());
            })
            .map_err(|e| e.to_string())?;
        self.gate_request = Some((command, rx));
        self.status = "Checking whether Tilo identity setup is complete…".into();
        Ok(())
    }

    fn start_operation(&mut self, index: usize) -> Result<(), String> {
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
        self.batch.clear();
        let Some((_, worker)) = &self.worker else {
            return false;
        };
        if worker.cancel() {
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
        let picker_outcome = self.picker.as_ref().and_then(|(target, receiver)| {
            receiver.try_recv().ok().map(|outcome| (*target, outcome))
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
                    self.status = "No folder was selected.".into();
                    self.status_is_error = false;
                }
                ui::PortalOutcome::Failed(error) => {
                    self.status = format!("The folder dialog could not open: {error}");
                    self.status_is_error = true;
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
                            format_bytes(progress.verified_bytes),
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

    fn start_reviewed_batch(&mut self) -> Result<(), String> {
        self.batch = self
            .operations
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
            .collect();
        if self.batch.is_empty() {
            Err("Review at least one saved Copy folder first.".into())
        } else {
            let first = self.batch.remove(0);
            self.start_operation(first)
        }
    }

    fn apply_authorized_command(&mut self, command: ImportCommand) {
        if self.review_request.is_some() {
            self.status = "Wait for the folder review to finish.".into();
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
                if let Err(error) = self.check_continued_review_allowed(index) {
                    self.status = error;
                    self.status_is_error = true;
                    return;
                }
                self.picker = Some((
                    PickTarget::ContinuedDestination(index),
                    folders::pick_folder(),
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
                self.picker = Some((PickTarget::Source, folders::pick_folder()));
                self.status = "Choose the source folder to copy.".into();
                Ok(())
            }
            ImportCommand::PickDestination => {
                self.picker = Some((PickTarget::DestinationParent, folders::pick_folder()));
                self.status = "Choose where Tilo should create the copied folder.".into();
                Ok(())
            }
            ImportCommand::PrepareReview => self.prepare_review(),
            ImportCommand::Start(index) => self.start_operation(index),
            ImportCommand::Retry(index) => self.retry_operation(index),
            ImportCommand::Cancel => {
                self.cancel_active();
                Ok(())
            }
            ImportCommand::ClearSelection => {
                self.source = None;
                self.destination_parent = None;
                self.status = "Selection cleared.".into();
                Ok(())
            }
            ImportCommand::NextIssues(index) => {
                let offset = self
                    .detail_pages
                    .get(&index)
                    .and_then(|p| p.next)
                    .unwrap_or(0);
                self.load_issue_page(index, offset)
            }
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
                .with_enabled(
                    self.source.is_some()
                        && self.destination_parent.is_some()
                        && self.store.is_some()
                        && self.worker.is_none(),
                ),
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
                    let label = if self.detail_request.is_some() {
                        "Loading issues…"
                    } else if self
                        .detail_pages
                        .get(&index)
                        .is_some_and(|p| p.next.is_none())
                    {
                        "First issue page"
                    } else {
                        "Next issue page"
                    };
                    actions = actions.push(Box::new(
                        self.button(
                            &format!("import.operation.{index}.next-issues"),
                            label,
                            ImportCommand::NextIssues(index),
                        )
                        .with_enabled(self.detail_request.is_none()),
                    ));
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
                .with_enabled(self.worker.is_none() && durable_copy_available().is_ok()),
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
        let enabled = self.identity_ready && self.worker.is_none() && self.review_request.is_none();
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
                        .push(Box::new(self.button(
                            &format!("import.continued.{index}.copy"),
                            "One-time Copy",
                            ImportCommand::ContinuedMode(index, ChoiceMode::Copy),
                        )))
                        .push(Box::new(self.button(
                            &format!("import.continued.{index}.later"),
                            "Later",
                            ImportCommand::ContinuedMode(index, ChoiceMode::Later),
                        ))),
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
                    .with_enabled(self.worker.is_none()),
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
        format_bytes(operation.verified_bytes()),
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

fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = 1_073_741_824.0;
    const MIB: f64 = 1_048_576.0;
    if bytes >= 1_073_741_824 {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else {
        format!("{bytes} bytes")
    }
}

impl Widget for ImportApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.poll_background();
        self.drain_commands();
        if let Some(composed) = &mut self.composed {
            composed.sync(state, path);
        }
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(composed) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&child) = children.first()
        {
            composed.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                child,
                taffy,
            );
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

#[must_use]
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ImportApp::new())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::*;

    fn fixture() -> (tempfile::TempDir, ImportApp, PathBuf, PathBuf) {
        let home = std::env::var_os("HOME").unwrap();
        let temp = tempfile::tempdir_in(home).unwrap();
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

    fn wait_terminal(app: &mut ImportApp) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while app.worker.is_some() {
            app.poll_background();
            assert!(Instant::now() < deadline, "import worker did not finish");
            std::thread::yield_now();
        }
    }

    fn settle_index(app: &mut ImportApp) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !app.pending_saves.is_empty() {
            app.poll_background();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn mismatched_review_records_refuse_start_without_losing_rereview() {
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
            assert!(app.start_operation(0).is_err());
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

    #[test]
    fn unpublished_continuation_can_review_another_destination_after_collision() {
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
            assert!(app.start_operation(0).is_err());
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
            let deadline = Instant::now() + Duration::from_secs(10);
            while app.review_request.is_some() {
                app.poll_background();
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            assert!(!app.status_is_error, "{}", app.status);
            settle_index(&mut app);
            assert_eq!(app.operations.len(), 1);
            assert_eq!(
                app.operations[0].destination().unwrap(),
                another.join("source")
            );
            app.start_operation(0).unwrap();
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

    #[test]
    fn same_basename_folders_can_retarget_the_second_without_changing_first_output() {
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
        app.start_operation(0).unwrap();
        wait_terminal(&mut app);
        assert_eq!(
            app.operations[0].state(),
            OperationState::Complete,
            "{}",
            app.status
        );
        assert!(app.start_operation(1).is_err());
        assert_eq!(app.operations[1].state(), OperationState::AwaitingReview);
        let another = parent.join("another");
        fs::create_dir(&another).unwrap();
        app.request_continued_review(1, Some(another.clone()))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.review_request.is_some() {
            app.poll_background();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!app.status_is_error, "{}", app.status);
        settle_index(&mut app);
        app.start_operation(1).unwrap();
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

    #[test]
    fn continuation_batch_uses_existing_worker_and_reopen_does_not_duplicate() {
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
        app.start_operation(0).unwrap();
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
        assert!(app.start_operation(0).is_err());
        settle_index(&mut app);
        let root = temp.path().canonicalize().unwrap().join("state");
        drop(app);
        let deadline = Instant::now() + Duration::from_secs(10);
        let store = loop {
            if let Ok(store) = MigrationStore::open_at(&root) {
                break store;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        let mut reopened = ImportApp::with_store(store).unwrap();
        reopened.identity_ready = true;
        assert_eq!(reopened.operations.len(), 1);
        reopened.prepare_continued_folder(0).unwrap();
        assert_eq!(reopened.operations.len(), 1);
        assert_eq!(reopened.operations[0].state(), OperationState::Complete);
    }

    #[test]
    fn interrupted_before_journal_creation_can_explicitly_retry_without_new_operation() {
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

    #[test]
    fn continuation_batch_affordance_requires_identity_and_reviewed_copy() {
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
                    .expect(&SemanticExpectation::named("import.continued.0.review").enabled(true))
                    .unwrap();
                harness
                    .expect(
                        &SemanticExpectation::named("import.continued.0.destination").enabled(true),
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
        let (entered, release) = super::super::migration_store::test_support::hold_save(&root, 1);
        let started = Instant::now();
        app.start_operation(0).unwrap();
        assert!(started.elapsed() < Duration::from_millis(200));
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!destination_parent.join("source").exists());
        let started = Instant::now();
        app.poll_background();
        assert!(app.cancel_active());
        assert!(started.elapsed() < Duration::from_millis(200));
        drop(app);
        assert!(MigrationStore::open_at(&root).is_err());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(store) = MigrationStore::open_at(&root) {
                assert_eq!(store.load().unwrap()[0].state(), OperationState::Cancelled);
                break;
            }
            assert!(Instant::now() < deadline);
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
        app.start_operation(0).unwrap();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let started = Instant::now();
        app.poll_background();
        assert!(started.elapsed() < Duration::from_millis(200));
        assert_eq!(app.operations[0].state(), OperationState::Running);
        release.send(()).unwrap();
        wait_terminal(&mut app);
        assert_eq!(app.operations[0].state(), OperationState::Complete);
    }

    #[test]
    fn failed_terminal_checkpoint_retains_an_honest_incomplete_result() {
        let (_temp, mut app, source, destination_parent) = fixture();
        settle_index(&mut app);
        fs::write(source.join("hello"), b"hello").unwrap();
        app.source = Some(source.clone());
        app.destination_parent = Some(destination_parent.clone());
        app.prepare_review().unwrap();
        settle_index(&mut app);
        let root = app.store.as_ref().unwrap().root().to_owned();
        let (entered, release) = super::super::migration_store::test_support::hold_save(&root, 2);
        app.start_operation(0).unwrap();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
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

    #[test]
    fn failed_start_persistence_never_creates_output() {
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
        app.start_operation(0).unwrap();
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

    // Catches copying into a suggested setup path before both current native
    // grants and the exact fresh destination have been reviewed.
    #[test]
    fn review_uses_the_selected_source_and_destination_parent() {
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

    // Catches an application restart turning a cancelled operation into a
    // fresh overwrite attempt instead of an explicit journal resume.
    #[test]
    fn application_restart_requires_explicit_retry_and_reaches_verified_completion() {
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
        app.start_operation(0).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.poll_background();
            if app.operations[0].verified_file_count() > 0 {
                assert!(app.cancel_active());
                break;
            }
            assert!(Instant::now() < deadline, "copy did not publish progress");
            std::thread::yield_now();
        }
        wait_terminal(&mut app);
        assert_eq!(app.operations[0].state(), OperationState::Cancelled);

        drop(app);
        let deadline = Instant::now() + Duration::from_secs(10);
        let store = loop {
            if let Ok(store) =
                MigrationStore::open_at(&temp.path().canonicalize().unwrap().join("state"))
            {
                break store;
            }
            assert!(
                Instant::now() < deadline,
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

    #[cfg(unix)]
    #[test]
    fn partial_worker_report_remains_visible_as_incomplete() {
        use std::os::unix::fs::symlink;

        let (_temp, mut app, source, destination_parent) = fixture();
        fs::write(source.join("regular"), b"kept").unwrap();
        symlink(source.join("regular"), source.join("linked")).unwrap();
        app.source = Some(source);
        app.destination_parent = Some(destination_parent);
        app.prepare_review().unwrap();
        app.start_operation(0).unwrap();
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

    // Catches screenshot fixtures drifting into fabricated success labels that
    // never passed through the real worker and copy report classifier.
    #[test]
    fn screenshot_states_come_from_real_worker_events() {
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
        let deadline = Instant::now() + Duration::from_secs(5);
        while pages.detail_request.is_some() {
            pages.poll_background();
            assert!(Instant::now() < deadline);
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

    /// The expanded issue list precedes the commands that act on it — the
    /// details toggle, the issue paging, the copy action — in reading and Tab
    /// order (the accessibility tree and `collect_focusable` both walk the
    /// widget tree in child order), and the three share one action row.
    #[test]
    fn expanded_details_precede_the_operation_commands() {
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
}

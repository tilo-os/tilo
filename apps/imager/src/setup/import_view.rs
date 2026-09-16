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
use ui::widget::{Button, Card, Column, Container, Row, Scrollable, Spacer, Text, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::{Modifiers, WindowEvent};

use super::folders;
use super::migration_store::{
    IssuePage, MigrationStore, OperationRecord, OperationState, StoreWriter,
};
use super::migration_worker::{
    MigrationJob, MigrationWorker, WorkerEvent, WorkerOutcome, durable_copy_available,
};
use super::view::system_folder_reason;

static NEXT_OPERATION: AtomicU64 = AtomicU64::new(0);

type CommandSink = Rc<RefCell<Vec<ImportCommand>>>;

#[derive(Clone, Copy, Debug)]
enum PickTarget {
    Source,
    DestinationParent,
}

#[derive(Clone, Copy, Debug)]
enum ImportCommand {
    PickSource,
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
            Ok(app) => app,
            Err(error) => Self::unavailable(&error),
        }
    }

    fn unavailable(error: &str) -> Self {
        Self {
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
        let destination_parent = base.join("qOS files");
        std::fs::create_dir(&source).map_err(|error| error.to_string())?;
        std::fs::create_dir(&destination_parent).map_err(|error| error.to_string())?;
        let store = MigrationStore::open_at(&base.join("state"))?;
        let mut app = Self::with_store(store)?;
        app.shot_temp = Some(temp);

        match scenario {
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

    fn start_operation(&mut self, index: usize) -> Result<(), String> {
        if self.worker.is_some() {
            return Err("another folder import is already running".into());
        }
        durable_copy_available()
            .map_err(|error| format!("Folder copying is unavailable on this platform: {error}"))?;
        let operation = self
            .operations
            .get_mut(index)
            .ok_or_else(|| "the selected migration operation no longer exists".to_string())?;
        operation.approve();
        let job = operation.start_job()?;
        operation.mark_running()?;
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
        let job = before.retry_job()?;
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

    fn poll_background(&mut self) -> bool {
        let mut changed = false;
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
        }
        self.poll_persistence_and_details(&mut changed);

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
        if changed {
            self.structure_dirty = true;
        }
        changed
    }

    fn apply_command(&mut self, command: ImportCommand) {
        let result = match command {
            ImportCommand::PickSource => {
                self.picker = Some((PickTarget::Source, folders::pick_folder()));
                self.status = "Choose the source folder to copy.".into();
                Ok(())
            }
            ImportCommand::PickDestination => {
                self.picker = Some((PickTarget::DestinationParent, folders::pick_folder()));
                self.status = "Choose where qOS should create the copied folder.".into();
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
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(
                Text::new(&summary).with_id(format!("import.operation.{index}.summary")),
            ));
        if !details.is_empty() {
            body = body.push(Box::new(self.button(
                &format!("import.operation.{index}.toggle-details"),
                if self.expanded_details.contains(&index) {
                    "Hide details"
                } else {
                    "Details"
                },
                ImportCommand::ToggleDetails(index),
            )));
            if self.expanded_details.contains(&index) {
                body = body.push(Box::new(
                    Text::new(&details).with_id(format!("import.operation.{index}.details")),
                ));
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
                    body = body.push(Box::new(
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
        match operation.state() {
            OperationState::AwaitingReview => {
                body = body.push(Box::new(
                    self.button(
                        &format!("import.operation.{index}.start"),
                        "Start copy",
                        ImportCommand::Start(index),
                    )
                    .primary()
                    .with_enabled(self.worker.is_none() && durable_copy_available().is_ok()),
                ));
            }
            OperationState::Running
                if self
                    .worker
                    .as_ref()
                    .is_some_and(|(active, _)| *active == index) =>
            {
                body = body.push(Box::new(self.button(
                    &format!("import.operation.{index}.cancel"),
                    "Stop copy",
                    ImportCommand::Cancel,
                )));
            }
            OperationState::Interrupted
            | OperationState::Cancelled
            | OperationState::Incomplete
            | OperationState::Failed => {
                body = body.push(Box::new(
                    self.button(
                        &format!("import.operation.{index}.retry"),
                        "Continue copy",
                        ImportCommand::Retry(index),
                    )
                    .with_enabled(self.worker.is_none() && operation.retry_job().is_ok()),
                ));
            }
            OperationState::Ready | OperationState::Running | OperationState::Complete => {}
        }
        Card::titled(
            format!(
                "Folder import · {}",
                operation_state_label(operation.state())
            ),
            Box::new(body),
        )
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(
                Text::new("Add folders to qOS")
                    .with_role(TextRole::Title)
                    .with_heading_level(1),
            ))
            .push(Box::new(Text::new(
                "Choose a folder and where to copy it. qOS checks each copied file and keeps your originals. Folder sync is not available yet.",
            ).dimmed()));
        if let Err(error) = durable_copy_available() {
            body = body.push(Box::new(Card::titled(
                "Folder copying unavailable",
                Box::new(Text::new(&error).with_live(Politeness::Assertive)),
            )));
        }
        if !self.operations.is_empty() {
            body = body.push(Box::new(
                Text::new("Imports")
                    .with_role(TextRole::Heading)
                    .with_heading_level(2),
            ));
            for (index, operation) in self.operations.iter().enumerate() {
                body = body.push(Box::new(self.operation_card(index, operation)));
            }
        }
        body = body.push(Box::new(self.selection_card()));
        let content = Scrollable::new_vertical()
            .fill_parent_height()
            .always_visible()
            .with_content_gutter(14.0)
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
                    .with_horizontal_padding(24.0, 24.0)
                    .with_vertical_padding(18.0)
                    .with_gap(12.0)
                    .push(Box::new(content))
                    .push(Box::new(status))
                    .push(Box::new(Row::new().no_wrap().push(Box::new(Spacer::new())))),
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
}

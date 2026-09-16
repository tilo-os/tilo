//! Standalone later-folder import UI.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

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
use super::migration_store::{MigrationStore, OperationRecord, OperationState};
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
}

/// A later-import surface backed by the same real journaled worker as setup.
pub struct ImportApp {
    store: Option<MigrationStore>,
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
            "import-partial" => app.seed_partial_shot(&source, &destination_parent)?,
            "import-conflict" => app.seed_conflict_shot(&source, &destination_parent)?,
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
        let operations = store.load()?;
        // Persist the load-time Running -> Interrupted normalization. This is
        // an index update only; retry still goes through journal revalidation.
        store.save(&operations)?;
        let status = if operations.is_empty() {
            "Choose a source folder and destination to prepare a one-time copy.".into()
        } else {
            "Previous imports reopened. Interrupted work requires an explicit retry.".into()
        };
        Ok(Self {
            store: Some(store),
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

    fn persist(&self) -> Result<(), String> {
        self.store
            .as_ref()
            .ok_or_else(|| "private migration state is unavailable".to_string())?
            .save(&self.operations)
    }

    fn prepare_review(&mut self) -> Result<(), String> {
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
        let before = self
            .operations
            .get(index)
            .cloned()
            .ok_or_else(|| "the selected migration operation no longer exists".to_string())?;
        let operation = self
            .operations
            .get_mut(index)
            .ok_or_else(|| "the selected migration operation no longer exists".to_string())?;
        operation.approve();
        let job = operation.start_job()?;
        operation.mark_running()?;
        if let Err(error) = self.persist() {
            if let Some(operation) = self.operations.get_mut(index) {
                *operation = before;
            }
            return Err(format!(
                "copy did not start because its operation state could not be saved: {error}"
            ));
        }
        match MigrationWorker::start(job) {
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
        if let Err(error) = self.persist() {
            if let Some(operation) = self.operations.get_mut(index) {
                *operation = before;
            }
            return Err(format!(
                "retry did not start because its operation state could not be saved: {error}"
            ));
        }
        match MigrationWorker::start(job) {
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

    fn poll_background(&mut self) -> bool {
        let mut changed = false;
        let mut terminal = false;
        while let Some((index, worker)) = &mut self.worker {
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
            if let Err(error) = self.persist() {
                self.status = format!(
                    "Copy continues, but local operation state could not be saved: {error}"
                );
                self.status_is_error = true;
            }
        }
        if terminal {
            self.worker = None;
            if let Err(error) = self.persist() {
                self.status = format!("The final result could not be saved locally: {error}");
                self.status_is_error = true;
            }
        }

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
            ImportCommand::ToggleDetails(index) => {
                if !self.expanded_details.remove(&index) {
                    self.expanded_details.insert(index);
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

    fn operation_card(&self, index: usize, operation: &OperationRecord) -> Card {
        let (summary, details) = operation_text(operation);
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
    if issues.len() == 1 {
        summary.push_str("\n1 issue needs attention.");
    } else if !issues.is_empty() {
        let _ = write!(summary, "\n{} issues need attention.", issues.len());
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
        self.worker.is_some() || self.picker.is_some()
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

        let store =
            MigrationStore::open_at(&temp.path().canonicalize().unwrap().join("state")).unwrap();
        let mut reopened = ImportApp::with_store(store).unwrap();
        assert_eq!(reopened.operations[0].state(), OperationState::Cancelled);
        reopened.retry_operation(0).unwrap();
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

        let complete = ImportApp::for_shot("import-complete");
        assert_eq!(complete.operations[0].state(), OperationState::Complete);
        assert!(complete.operations[0].verified_bytes() > 0);
    }
}

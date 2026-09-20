//! Device-local first-login migration choices. Never collaborative state.

use super::controller::{FolderChoice, FolderDestination};
use super::model::FolderMode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use ui::folders::copy::MigrationJob;
use ui::folders::copy::{MigrationStore, NativePath, OperationRecord};
use ui::folders::review::DirectoryIdentity as Identity;
use ui::folders::review::{directory_identity, inspect_copy_review, path_is_absent};

const MAX_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ChoiceMode {
    Copy,
    Connected,
    Later,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "path", deny_unknown_fields)]
enum DestinationIntent {
    Managed,
    Selected(NativePath),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolvedDestination {
    path: NativePath,
    parent_identity: Identity,
    #[serde(default)]
    source_revision: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ContinuedFolder {
    source: NativePath,
    source_identity: Identity,
    intent: DestinationIntent,
    pub(super) mode: ChoiceMode,
    mode_overridden: bool,
    resolved: Option<ResolvedDestination>,
}

impl ContinuedFolder {
    pub(super) fn source(&self) -> Result<PathBuf, String> {
        self.source.decode()
    }
    pub(super) fn destination(&self) -> Result<Option<PathBuf>, String> {
        self.resolved.as_ref().map(|r| r.path.decode()).transpose()
    }
    fn validate_source(&self) -> Result<(), String> {
        if directory_identity(&self.source()?)? != self.source_identity {
            return Err(
                "Selected source changed or its volume was replaced. Select and review it again."
                    .into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Continuation {
    version: u32,
    revision: String,
    pub(super) folders: Vec<ContinuedFolder>,
    pub(super) show_on_login: bool,
}

fn absolute(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    {
        return Err("continuation requires native absolute paths without traversal".into());
    }
    Ok(())
}

impl Continuation {
    pub(super) fn capture(choices: &[FolderChoice]) -> Result<Self, String> {
        if choices.len() > 128 {
            return Err("too many selected folders".into());
        }
        let mut folders = Vec::new();
        for choice in choices {
            let source_identity = directory_identity(&choice.scan.source)?;
            folders.push(ContinuedFolder {
                source: NativePath::encode(&choice.scan.source),
                source_identity,
                intent: match &choice.destination {
                    FolderDestination::Unresolved => DestinationIntent::Managed,
                    FolderDestination::Resolved(p) => {
                        absolute(p)?;
                        DestinationIntent::Selected(NativePath::encode(p))
                    }
                },
                mode: match choice.mode {
                    FolderMode::Copy => ChoiceMode::Copy,
                    FolderMode::Connected => ChoiceMode::Connected,
                    FolderMode::Later => ChoiceMode::Later,
                },
                mode_overridden: choice.mode_overridden,
                resolved: None,
            });
        }
        // Content identity binds this complete reviewed selection, including native
        // source identities, policy overrides and destination intent. A repeated
        // install of the exact same review retains stable operation IDs.
        let revision = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&folders).map_err(|e| e.to_string())?)
        );
        Ok(Self {
            version: 1,
            revision,
            folders,
            show_on_login: !choices.is_empty(),
        })
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("continuation exceeds bounds".into());
        }
        let c: Self = serde_json::from_slice(bytes)
            .map_err(|e| format!("Invalid migration continuation: {e}"))?;
        if c.version != 1
            || c.folders.len() > 128
            || c.revision.len() != 64
            || !c.revision.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("unsupported continuation version, revision or folder count".into());
        }
        for f in &c.folders {
            absolute(&f.source()?)?;
            if let DestinationIntent::Selected(path) = &f.intent {
                absolute(&path.decode()?)?;
            }
            if let Some(path) = f.destination()? {
                absolute(&path)?;
            }
        }
        Ok(c)
    }

    pub(super) fn load(store: &MigrationStore) -> Result<Option<Self>, String> {
        store
            .load_continuation_bytes()?
            .map(|b| Self::decode(&b))
            .transpose()
    }
    pub(super) fn save(&self, store: &MigrationStore) -> Result<(), String> {
        let bytes = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        Self::decode(&bytes)?;
        store.save_continuation_bytes(&bytes)
    }
    pub(super) fn same_review(&self, other: &Self) -> bool {
        self.revision == other.revision
    }
    pub(super) fn operation_id(&self, index: usize) -> String {
        format!("desktop-{}-{index}", self.revision)
    }
    pub(super) fn set_mode(&mut self, index: usize, mode: ChoiceMode) -> Result<(), String> {
        let f = self
            .folders
            .get_mut(index)
            .ok_or("folder choice is no longer available")?;
        f.mode = mode;
        f.mode_overridden = true;
        Ok(())
    }

    /// Resolve and display endpoints. This is still not permission to copy.
    pub(super) fn resolve(
        &mut self,
        index: usize,
        managed_parent: &Path,
        identity_complete: bool,
    ) -> Result<(), String> {
        if !identity_complete {
            return Err(
                "Finish identity setup in the Tilo login screen before reviewing imports.".into(),
            );
        }
        let f = self
            .folders
            .get_mut(index)
            .ok_or("folder choice is no longer available")?;
        f.validate_source()?;
        let source = f.source()?;
        let destination = match &f.intent {
            DestinationIntent::Managed => {
                managed_parent.join(source.file_name().ok_or("source has no folder name")?)
            }
            DestinationIntent::Selected(p) => p.decode()?,
        };
        if destination.starts_with(&source) || source.starts_with(&destination) {
            return Err("source and destination overlap".into());
        }
        if std::fs::symlink_metadata(&destination).is_ok() {
            return Err(format!(
                "Collision: {} already exists. Choose another destination; existing files will not be overwritten.",
                destination.display()
            ));
        }
        let (parent_identity, source_revision) =
            inspect_copy_review(&source, &destination)?.into_expectations();
        f.resolved = Some(ResolvedDestination {
            path: NativePath::encode(&destination),
            parent_identity,
            source_revision: Some(source_revision),
        });
        Ok(())
    }

    pub(super) fn select_destination_parent(
        &mut self,
        index: usize,
        parent: &Path,
    ) -> Result<(), String> {
        absolute(parent)?;
        let f = self
            .folders
            .get_mut(index)
            .ok_or("folder choice is no longer available")?;
        let source = f.source()?;
        f.intent = DestinationIntent::Selected(NativePath::encode(
            &parent.join(source.file_name().ok_or("source has no name")?),
        ));
        f.resolved = None;
        Ok(())
    }

    /// An explicit retry may recover the checkpoint-before-journal crash gap,
    /// but only if both output names are genuinely absent beneath pinned roots.
    pub(super) fn unpublished_retry(
        &self,
        index: usize,
        record: &OperationRecord,
    ) -> Result<Option<MigrationJob>, String> {
        use ui::folders::copy::OperationState;
        if !matches!(
            record.state(),
            OperationState::Interrupted | OperationState::Failed | OperationState::Cancelled
        ) {
            return Ok(None);
        }
        if !path_is_absent(&record.journal()?)? || !path_is_absent(&record.destination()?)? {
            return Ok(None);
        }
        let mut fresh = record.clone();
        fresh.approve();
        self.constrain_job(index, fresh.start_job()?).map(Some)
    }

    pub(super) fn can_replace_unpublished(record: &OperationRecord) -> Result<(), String> {
        use ui::folders::copy::OperationState;
        if !matches!(
            record.state(),
            OperationState::AwaitingReview
                | OperationState::Failed
                | OperationState::Interrupted
                | OperationState::Cancelled
        ) {
            return Err("This copy has published or active work; preserve its outcome and use its existing retry action.".into());
        }
        if !path_is_absent(&record.journal()?)? {
            return Err("This copy has a durable journal; its reviewed destination and source revision must be retained.".into());
        }
        if record.state() != OperationState::AwaitingReview
            && !path_is_absent(&record.destination()?)?
        {
            return Err(
                "Interrupted output requires review; its existing outcome was retained.".into(),
            );
        }
        Ok(())
    }

    pub(super) fn validate_fresh_destination(&self, index: usize) -> Result<(), String> {
        let destination = self
            .folders
            .get(index)
            .ok_or("folder choice is no longer available")?
            .destination()?
            .ok_or("Review the destination first")?;
        if !path_is_absent(&destination)? {
            return Err("Destination collision: choose and review another destination; existing data was preserved.".into());
        }
        Ok(())
    }

    pub(super) fn constrain_job(
        &self,
        index: usize,
        job: MigrationJob,
    ) -> Result<MigrationJob, String> {
        let f = self
            .folders
            .get(index)
            .ok_or("folder choice is no longer available")?;
        let r = f.resolved.as_ref().ok_or("destination is unresolved")?;
        if job.source() != f.source()? || job.destination() != r.path.decode()? {
            return Err("copy job does not match the reviewed endpoints".into());
        }
        Ok(job.with_reviewed_identities(
            f.source_identity.clone(),
            r.parent_identity.clone(),
            r.source_revision
                .ok_or("Source review is outdated; review the folder again.")?,
        ))
    }

    pub(super) fn validate_endpoints(
        &self,
        index: usize,
        identity_complete: bool,
    ) -> Result<(), String> {
        if !identity_complete {
            return Err("Waiting for trusted Tilo identity setup to finish.".into());
        }
        let f = self
            .folders
            .get(index)
            .ok_or("folder choice is no longer available")?;
        if f.mode != ChoiceMode::Copy {
            return Err("This folder is pending; select one-time Copy and review it first.".into());
        }
        f.validate_source()?;
        let r = f
            .resolved
            .as_ref()
            .ok_or("Review the actual destination first")?;
        let destination = r.path.decode()?;
        if directory_identity(destination.parent().ok_or("destination has no parent")?)?
            != r.parent_identity
        {
            return Err("Destination changed; review it again.".into());
        }
        Ok(())
    }

    /// An existing durable ID wins if acknowledgement was interrupted. The
    /// index/journal retains terminal outcomes and remains overwrite authority.
    pub(super) fn prepare(
        &self,
        index: usize,
        store: &MigrationStore,
        operations: &[OperationRecord],
        identity_complete: bool,
    ) -> Result<Option<OperationRecord>, String> {
        self.validate_endpoints(index, identity_complete)?;
        let id = self.operation_id(index);
        let f = self
            .folders
            .get(index)
            .ok_or("folder choice is no longer available")?;
        let source = f.source()?;
        let destination = f.destination()?.ok_or("destination is unresolved")?;
        if let Some(op) = operations.iter().find(|op| op.id() == id) {
            if op.source()? == source
                && op.destination()? == destination
                && !matches!(
                    op.state(),
                    ui::folders::copy::OperationState::Failed
                        | ui::folders::copy::OperationState::Interrupted
                        | ui::folders::copy::OperationState::Cancelled
                )
            {
                return Ok(None);
            }
            Self::can_replace_unpublished(op)?;
        }
        if std::fs::symlink_metadata(&destination).is_ok() {
            return Err("Destination collision: existing data was preserved.".into());
        }
        Ok(Some(OperationRecord::new(&MigrationJob::fresh(
            id.clone(),
            source,
            destination,
            store.root().join(format!("{id}.journal")),
        )?)))
    }
}

/// Save the reviewed setup choices before native installation starts. Repeating
/// an identical review must not discard resolved endpoints or durable IDs.
pub(super) fn persist_review(choices: &[FolderChoice]) -> Result<(), String> {
    if choices.is_empty() {
        return Ok(());
    }
    let captured = Continuation::capture(choices)?;
    let store = MigrationStore::open_default()?;
    if let Some(previous) = Continuation::load(&store)? {
        if previous.same_review(&captured) {
            return Ok(());
        }
        // Replacing an outstanding review would strand the previously selected
        // folders. Finish/resolve that review through Add folders first.
        if previous.show_on_login && !previous.folders.is_empty() {
            return Err("An earlier Desktop folder review is saved. Open Add folders to finish it before replacing the selection.".into());
        }
    }
    captured.save(&store)
}

pub(super) use ui::folders::session::trusted_identity_complete;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::controller::{FolderChoice, FolderDestination};
    use crate::setup::folders::FolderScan;
    use crate::setup::model::FolderMode;
    use std::fs;
    use ui::folders::copy::MigrationStore;

    fn fixture() -> (tempfile::TempDir, MigrationStore, Vec<FolderChoice>) {
        let temp = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
        let base = temp.path().canonicalize().unwrap();
        let source = base.join("Documents");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("hello"), b"original").unwrap();
        let choices = vec![FolderChoice {
            scan: FolderScan {
                source,
                file_count: 1,
                bytes: 8,
                empty_directories: vec![],
                skipped: vec![],
            },
            destination: FolderDestination::Unresolved,
            mode: FolderMode::Copy,
            mode_overridden: false,
        }];
        let store = MigrationStore::open_at(&base.join("state")).unwrap();
        (temp, store, choices)
    }

    #[test]
    fn edits_after_inventory_validation_cannot_become_the_copy_baseline() {
        for edit_already_copied in [false, true] {
            let (_temp, store, choices) = fixture();
            let source = &choices[0].scan.source;
            fs::write(source.join("aaa"), b"before").unwrap();
            fs::write(source.join("zzz"), b"before").unwrap();
            let mut c = Continuation::capture(&choices).unwrap();
            let parent = store.root().parent().unwrap().join("target");
            fs::create_dir(&parent).unwrap();
            c.resolve(0, &parent, true).unwrap();
            let folder = &c.folders[0];
            let r = folder.resolved.as_ref().unwrap();
            let reviewed = (
                folder.source_identity.clone(),
                r.parent_identity.clone(),
                r.source_revision.unwrap(),
            );
            let destination = parent.join("Documents");
            let journal = store.root().join("late-edit.journal");
            let mut edited = false;
            let report = tilo_folder_service::test_support::run_reviewed_copy_for_test(
                source,
                &destination,
                &journal,
                &reviewed,
                false,
                &std::sync::atomic::AtomicBool::new(false),
                |progress| {
                    if progress.current_relative_path == Path::new("zzz") && !edited {
                        edited = true;
                        fs::write(
                            source.join(if edit_already_copied { "aaa" } else { "zzz" }),
                            b"edited",
                        )
                        .unwrap();
                    }
                },
            )
            .unwrap();
            assert!(!report.complete(), "{report:?}");
            assert_eq!(fs::read(destination.join("aaa")).unwrap(), b"before");
            if !edit_already_copied {
                assert!(!destination.join("zzz").exists());
            }
            // Even the ordinary resume route must retain the reviewed journal baseline.
            assert!(
                tilo_folder_service::test_support::resume_folder(
                    &journal,
                    &std::sync::atomic::AtomicBool::new(false),
                    |_| {}
                )
                .is_err()
            );
        }
    }

    #[test]
    fn existing_file_edited_after_final_review_is_refused_before_publication() {
        use ui::folders::copy::{MigrationWorker, WorkerEvent, WorkerOutcome};
        let (_temp, store, choices) = fixture();
        let file = choices[0].scan.source.join("existing.txt");
        fs::write(&file, b"before").unwrap();
        let mut c = Continuation::capture(&choices).unwrap();
        let parent = store.root().parent().unwrap().join("target");
        fs::create_dir(&parent).unwrap();
        c.resolve(0, &parent, true).unwrap();
        let mut op = c.prepare(0, &store, &[], true).unwrap().unwrap();
        op.approve();
        let job = c.constrain_job(0, op.start_job().unwrap()).unwrap();
        let modified = fs::metadata(&file).unwrap().modified().unwrap();
        fs::write(&file, b"edited").unwrap();
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let mut worker = MigrationWorker::start(job).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let outcome = loop {
            if let Some(WorkerEvent::Finished(outcome)) = worker.try_next() {
                break outcome;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert!(matches!(outcome, WorkerOutcome::Failed(_)), "{outcome:?}");
        assert!(!op.journal().unwrap().exists());
        assert!(!parent.join("Documents").exists());
        assert_eq!(fs::read(&file).unwrap(), b"edited");
        drop(worker);
        c.resolve(0, &parent, true).unwrap();
        let job = c.constrain_job(0, op.start_job().unwrap()).unwrap();
        let mut worker = MigrationWorker::start(job).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(WorkerEvent::Finished(outcome)) = worker.try_next() {
                assert!(matches!(outcome, WorkerOutcome::Complete(_)), "{outcome:?}");
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(
            fs::read(parent.join("Documents/existing.txt")).unwrap(),
            b"edited"
        );
    }

    #[test]
    fn source_swapped_after_review_is_rejected_by_worker_before_journal_or_output() {
        use ui::folders::copy::{MigrationWorker, WorkerEvent, WorkerOutcome};
        let (_temp, store, choices) = fixture();
        let mut c = Continuation::capture(&choices).unwrap();
        let parent = store.root().parent().unwrap().join("target");
        fs::create_dir(&parent).unwrap();
        c.resolve(0, &parent, true).unwrap();
        let mut op = c.prepare(0, &store, &[], true).unwrap().unwrap();
        op.approve();
        let job = c.constrain_job(0, op.start_job().unwrap()).unwrap();
        fs::rename(
            &choices[0].scan.source,
            choices[0].scan.source.with_extension("old"),
        )
        .unwrap();
        fs::create_dir(&choices[0].scan.source).unwrap();
        let mut worker = MigrationWorker::start(job).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(event) = worker.try_next() {
                assert!(matches!(
                    event,
                    WorkerEvent::Finished(WorkerOutcome::Failed(_))
                ));
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!op.journal().unwrap().exists());
        assert!(!parent.join("Documents").exists());
    }

    #[test]
    fn destination_swapped_after_review_is_rejected_before_output() {
        use ui::folders::copy::{MigrationWorker, WorkerEvent, WorkerOutcome};
        let (_temp, store, choices) = fixture();
        let mut c = Continuation::capture(&choices).unwrap();
        let parent = store.root().parent().unwrap().join("target");
        fs::create_dir(&parent).unwrap();
        c.resolve(0, &parent, true).unwrap();
        let mut op = c.prepare(0, &store, &[], true).unwrap().unwrap();
        op.approve();
        let job = c.constrain_job(0, op.start_job().unwrap()).unwrap();
        fs::rename(&parent, parent.with_extension("old")).unwrap();
        fs::create_dir(&parent).unwrap();
        let mut worker = MigrationWorker::start(job).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(event) = worker.try_next() {
                assert!(matches!(
                    event,
                    WorkerEvent::Finished(WorkerOutcome::Failed(_))
                ));
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!op.journal().unwrap().exists());
        assert!(!parent.join("Documents").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_non_utf8_source_survives_private_roundtrip() {
        use std::os::unix::ffi::OsStringExt;
        let (_temp, store, mut choices) = fixture();
        let path = choices[0]
            .scan
            .source
            .with_file_name(std::ffi::OsString::from_vec(vec![b'd', 255]));
        fs::rename(&choices[0].scan.source, &path).unwrap();
        choices[0].scan.source = path.clone();
        Continuation::capture(&choices)
            .unwrap()
            .save(&store)
            .unwrap();
        assert_eq!(
            Continuation::load(&store).unwrap().unwrap().folders[0]
                .source()
                .unwrap(),
            path
        );
    }

    #[test]
    fn unpublished_retry_refuses_existing_names_and_unavailable_parents() {
        let (_temp, store, choices) = fixture();
        let mut c = Continuation::capture(&choices).unwrap();
        let parent = store.root().parent().unwrap().join("target");
        fs::create_dir(&parent).unwrap();
        c.resolve(0, &parent, true).unwrap();
        let mut record = c.prepare(0, &store, &[], true).unwrap().unwrap();
        record.approve();
        record.mark_running().unwrap();
        tilo_folder_service::test_support::save_snapshot(&store, &[record]).unwrap();
        let record = store.load().unwrap().remove(0);
        fs::write(record.journal().unwrap(), b"existing journal must remain").unwrap();
        assert!(c.unpublished_retry(0, &record).unwrap().is_none());
        fs::remove_file(record.journal().unwrap()).unwrap();
        fs::create_dir(record.destination().unwrap()).unwrap();
        fs::write(record.destination().unwrap().join("user-edit"), b"keep").unwrap();
        assert!(c.unpublished_retry(0, &record).unwrap().is_none());
        fs::rename(&parent, parent.with_extension("unavailable")).unwrap();
        assert!(c.unpublished_retry(0, &record).is_err());
        assert_eq!(
            fs::read(
                parent
                    .with_extension("unavailable")
                    .join("Documents/user-edit")
            )
            .unwrap(),
            b"keep"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "controller-only disposable native current-user fixture; requires QOS_CONTINUATION_FIXTURE_SOURCE"]
    fn seed_native_first_login_fixture() {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: getuid has no memory contract. A root-run seed would name the
        // wrong account and must never prepare another user's continuation.
        let uid = unsafe { libc::getuid() };
        assert_ne!(uid, 0, "run this fixture as the ordinary proof login user");
        let home = dirs::home_dir().unwrap().canonicalize().unwrap();
        let requested = PathBuf::from(
            std::env::var_os("QOS_CONTINUATION_FIXTURE_SOURCE")
                .expect("set the dedicated fixture root"),
        );
        assert_eq!(requested, home.join("qos-native-migration-fixture"));
        assert_eq!(requested.canonicalize().unwrap(), requested);
        assert_eq!(fs::symlink_metadata(&requested).unwrap().uid(), uid);
        let mut controller =
            super::super::controller::SetupController::new(super::super::model::Capabilities {
                host: super::super::model::HostOs::Linux,
                desktop_session: true,
                internal_dual_boot: false,
            });
        assert!(controller.choose_experience(super::super::model::Experience::Desktop));
        controller.next().unwrap();
        for name in ["Documents", "Pictures"] {
            let path = requested.join(name);
            assert_eq!(fs::symlink_metadata(&path).unwrap().uid(), uid);
            controller.add_folder_unresolved(super::super::folders::scan_folder(&path).unwrap());
        }
        assert!(controller.set_folder_mode(1, FolderMode::Later));
        controller.next().unwrap();
        let expected = Continuation::capture(controller.folders()).unwrap();
        {
            let store = MigrationStore::open_default().unwrap();
            if let Some(existing) = Continuation::load(&store).unwrap() {
                assert!(
                    existing.same_review(&expected),
                    "refusing to overwrite unrelated prior continuation"
                );
            }
        }
        persist_review(controller.folders()).unwrap();
        let store = MigrationStore::open_default().unwrap();
        let actual = Continuation::load(&store).unwrap().unwrap();
        assert!(actual.same_review(&expected));
        assert!(actual.folders.iter().all(|f| f.resolved.is_none()));
        assert!(
            store.load().unwrap().is_empty(),
            "fixture must not contain previous copy operations"
        );
        println!(
            "Seeded reviewed Copy Documents + Later Pictures at {}",
            store.root().display()
        );
    }

    #[test]
    fn private_roundtrip_retains_native_paths_revision_and_per_folder_modes() {
        let (_temp, store, mut choices) = fixture();
        choices[0].mode = FolderMode::Later;
        choices[0].mode_overridden = true;
        let c = Continuation::capture(&choices).unwrap();
        c.save(&store).unwrap();
        let reopened = Continuation::load(&store).unwrap().unwrap();
        assert_eq!(c.revision, reopened.revision);
        assert_eq!(
            reopened.folders[0].source().unwrap(),
            choices[0].scan.source
        );
        assert_eq!(reopened.folders[0].mode, ChoiceMode::Later);
        assert!(reopened.folders[0].mode_overridden);
        assert!(reopened.folders[0].resolved.is_none());
        assert!(
            !String::from_utf8(store.load_continuation_bytes().unwrap().unwrap())
                .unwrap()
                .contains("secret")
        );
    }

    #[test]
    fn malformed_oversized_wrong_platform_and_relative_paths_fail_closed() {
        let (_temp, store, choices) = fixture();
        assert!(Continuation::decode(b"{}").is_err());
        assert!(Continuation::decode(&vec![b' '; MAX_BYTES + 1]).is_err());
        let c = Continuation::capture(&choices).unwrap();
        let mut json = serde_json::to_value(&c).unwrap();
        json["folders"][0]["source"] = serde_json::json!({"platform":"Windows","units":[67,58,92]});
        #[cfg(unix)]
        assert!(Continuation::decode(&serde_json::to_vec(&json).unwrap()).is_err());
        json["folders"][0]["source"] =
            serde_json::to_value(NativePath::encode(std::path::Path::new("relative"))).unwrap();
        assert!(Continuation::decode(&serde_json::to_vec(&json).unwrap()).is_err());
        assert!(
            store
                .save_continuation_bytes(&vec![0; MAX_BYTES + 1])
                .is_err()
        );
    }

    #[test]
    fn source_substitution_or_missing_mount_never_resolves() {
        let (_temp, store, choices) = fixture();
        let mut c = Continuation::capture(&choices).unwrap();
        let source = &choices[0].scan.source;
        fs::rename(source, source.with_extension("old")).unwrap();
        let parent = store.root().parent().unwrap().join("target");
        fs::create_dir(&parent).unwrap();
        assert!(c.resolve(0, &parent, true).is_err());
        fs::create_dir(source).unwrap();
        assert!(c.resolve(0, &parent, true).is_err());
    }

    #[test]
    fn identity_gate_collisions_and_destination_replacement_prevent_copy() {
        let (_temp, store, choices) = fixture();
        let mut c = Continuation::capture(&choices).unwrap();
        let parent = store.root().parent().unwrap().join("target");
        fs::create_dir(&parent).unwrap();
        assert!(c.resolve(0, &parent, false).is_err());
        fs::create_dir(parent.join("Documents")).unwrap();
        assert!(c.resolve(0, &parent, true).is_err());
        fs::remove_dir(parent.join("Documents")).unwrap();
        c.resolve(0, &parent, true).unwrap();
        assert!(c.prepare(0, &store, &[], false).is_err());
        fs::rename(&parent, parent.with_extension("old")).unwrap();
        fs::create_dir(&parent).unwrap();
        assert!(c.prepare(0, &store, &[], true).is_err());
    }

    #[test]
    fn interrupted_acknowledgement_reuses_operation_and_later_never_copies() {
        let (_temp, store, choices) = fixture();
        let mut c = Continuation::capture(&choices).unwrap();
        let parent = store.root().parent().unwrap().join("target");
        fs::create_dir(&parent).unwrap();
        c.resolve(0, &parent, true).unwrap();
        c.save(&store).unwrap();
        let op = c.prepare(0, &store, &[], true).unwrap().unwrap();
        tilo_folder_service::test_support::save_snapshot(&store, &[op]).unwrap();
        let mut replay = Continuation::load(&store).unwrap().unwrap();
        assert!(
            replay
                .prepare(0, &store, &store.load().unwrap(), true)
                .unwrap()
                .is_none()
        );
        replay.set_mode(0, ChoiceMode::Later).unwrap();
        assert!(
            replay
                .prepare(0, &store, &store.load().unwrap(), true)
                .is_err()
        );
        replay.save(&store).unwrap();
        assert_eq!(
            Continuation::load(&store).unwrap().unwrap().folders[0].mode,
            ChoiceMode::Later
        );
        assert!(MigrationStore::open_at(store.root()).is_err());
    }
}

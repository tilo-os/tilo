//! Durable host transaction record: every mutation is recorded `intended`
//! before it is attempted and `done` after it is read back.
//!
//! Storage: the authoritative record lives in a directory only the elevated
//! helper can write or read (Linux `/var/lib/tilo-imager/staging/`, 0700 root,
//! records 0600; Windows `%ProgramData%\Tilo\staging`, protected DACL for
//! SYSTEM + Administrators). The unprivileged UI cannot read that, so the
//! helper also mirrors every save into a world-readable status directory
//! (`/var/lib/tilo-imager/staging-status/`, 0755 root, files 0644; Windows
//! `%ProgramData%\Tilo\staging-status`, Users read-only). Records hold no
//! secrets (the installer profile lives only in the handoff). The mirror is
//! display-only: the helper never reads it back, it always re-reads the
//! private record.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::plan::Plan;

#[cfg(target_os = "linux")]
mod browser;

pub const SCHEMA: u32 = 1;

pub use imager_backend::staging::protocol::{Step, StepRecord, StepState};

/// Nested retained-filesystem checkpoint in the existing authoritative step.
/// This is journal evidence only; native binding/readback remains mandatory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ResizeCheckpoint {
    pub bytes: u64,
    pub state: StepState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseRef {
    pub release_id: String,
    pub manifest_sha256: String,
    pub architecture: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginalFirmware {
    pub boot_order: Vec<String>,
    pub boot_next: Option<String>,
    /// Hex load options of every `BootOrder` entry at first mutation, for
    /// audit. Never written back: `BootOrder` and foreign entries are only
    /// ever read.
    #[serde(default)]
    pub entries: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedBootEntry {
    pub number: String,
    pub load_option_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub schema: u32,
    pub operation_id: String,
    pub plan_sha256: String,
    pub plan: Plan,
    pub release: ReleaseRef,
    pub steps: Vec<StepRecord>,
    pub original_firmware: OriginalFirmware,
    pub owned_boot_entry: Option<OwnedBootEntry>,
    /// Component sizes and digests verified at `stage`, so `arm` can
    /// re-verify the partitions after the download is gone.
    #[serde(default)]
    pub bundle: Option<RecordedBundle>,
    /// Immutable source consent, kept only in the private authoritative record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration: Option<serde_json::Value>,
    #[serde(default)]
    pub profile: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_setup: Option<crate::model::PreparedSetupExpectation>,
    #[cfg(target_os = "linux")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_browser: Option<crate::setup::browser_transfer::Expectation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedComponent {
    pub byte_size: u64,
    pub sha256: String,
    /// Filesystem identity bound by the manifest, so `arm` can recognise the
    /// staging root after the installer has booted (and grown) it once.
    #[serde(default)]
    pub filesystem_uuid: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedBundle {
    pub esp: RecordedComponent,
    pub root: RecordedComponent,
}

/// Where a record stands, for the resume card and the pending-operation rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordStatus {
    /// Some stage or remove step is not done: an interrupted operation.
    InProgress,
    /// Every stage step is done and nothing has been removed.
    Staged,
    /// Every remove step is done; nothing of this operation remains.
    Removed,
}

fn now() -> u64 {
    ui::time::wall()
        .duration_since(ui::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

pub use imager_backend::staging::protocol::valid_operation_id;

impl Record {
    pub fn new(
        operation_id: String,
        plan: Plan,
        release: ReleaseRef,
        original_firmware: OriginalFirmware,
    ) -> Result<Self, String> {
        Ok(Self {
            schema: SCHEMA,
            operation_id,
            plan_sha256: plan.digest().map_err(|error| error.to_string())?,
            plan,
            release,
            steps: Vec::new(),
            original_firmware,
            owned_boot_entry: None,
            bundle: None,
            migration: None,
            profile: serde_json::Map::new(),
            prepared_setup: None,
            #[cfg(target_os = "linux")]
            prepared_browser: None,
        })
    }

    #[cfg(target_os = "linux")]
    pub(super) fn browser_expectation(
        &self,
    ) -> Option<&crate::setup::browser_transfer::Expectation> {
        self.prepared_browser.as_ref()
    }
    #[cfg(not(target_os = "linux"))]
    pub(super) fn browser_expectation(&self) -> Option<&super::prepared_setup::NoBrowserTransfer> {
        None
    }
    pub fn step(&self, step: Step) -> Option<&StepRecord> {
        self.steps.iter().find(|s| s.step == step)
    }

    pub fn state(&self, step: Step) -> Option<StepState> {
        self.step(step).map(|s| s.state)
    }

    pub fn is_done(&self, step: Step) -> bool {
        self.state(step) == Some(StepState::Done)
    }

    /// A string field of a step's detail.
    pub fn detail_str(&self, step: Step, key: &str) -> Option<&str> {
        self.step(step)?.detail.get(key)?.as_str()
    }

    fn resize_key(phase: super::btrfs_resize::Phase) -> (Step, &'static str) {
        use super::btrfs_resize::Phase;
        match phase {
            Phase::ShrinkFilesystem => (Step::Shrink, "btrfs_shrink_filesystem"),
            Phase::ShrinkPartition => (Step::Shrink, "btrfs_shrink_partition"),
            Phase::GrowPartition => (Step::RestoreShrink, "btrfs_grow_partition"),
            Phase::GrowFilesystem => (Step::RestoreShrink, "btrfs_grow_filesystem"),
        }
    }

    pub(super) fn resize_checkpoint(
        &self,
        phase: super::btrfs_resize::Phase,
    ) -> Result<Option<ResizeCheckpoint>, String> {
        let (step, key) = Self::resize_key(phase);
        self.step(step)
            .and_then(|step| step.detail.get(key))
            .map(|value| serde_json::from_value(value.clone()).map_err(|error| error.to_string()))
            .transpose()
    }

    /// The helper saves this private record before calling the native mutation.
    /// Retry may preserve a Done checkpoint; it may never change its target.
    pub(super) fn resize_intent(
        &mut self,
        phase: super::btrfs_resize::Phase,
        bytes: u64,
    ) -> Result<(), String> {
        let (step, key) = Self::resize_key(phase);
        self.validate_resize_target(phase, bytes)?;
        let existing = self.resize_checkpoint(phase)?;
        if let Some(existing) = existing {
            if existing.bytes != bytes {
                return Err("retained filesystem checkpoint target changed".into());
            }
        }
        let checkpoint = existing.unwrap_or(ResizeCheckpoint {
            bytes,
            state: StepState::Intended,
        });
        let step = self
            .steps
            .iter_mut()
            .find(|entry| entry.step == step)
            .ok_or("retained filesystem checkpoint lacks outer intent")?;
        step.detail.insert(
            key.into(),
            serde_json::to_value(checkpoint).map_err(|error| error.to_string())?,
        );
        step.at = now();
        Ok(())
    }

    pub(super) fn resize_done(
        &mut self,
        phase: super::btrfs_resize::Phase,
        bytes: u64,
    ) -> Result<(), String> {
        let (step, key) = Self::resize_key(phase);
        self.validate_resize_target(phase, bytes)?;
        let checkpoint = self
            .resize_checkpoint(phase)?
            .ok_or("retained filesystem completion lacks prior intent")?;
        if checkpoint.bytes != bytes || checkpoint.state == StepState::Failed {
            return Err("retained filesystem completion differs from intent".into());
        }
        let step = self
            .steps
            .iter_mut()
            .find(|entry| entry.step == step)
            .ok_or("retained filesystem completion lacks outer intent")?;
        step.detail.insert(
            key.into(),
            serde_json::to_value(ResizeCheckpoint {
                bytes,
                state: StepState::Done,
            })
            .map_err(|error| error.to_string())?,
        );
        step.at = now();
        Ok(())
    }

    fn validate_resize_target(
        &self,
        phase: super::btrfs_resize::Phase,
        bytes: u64,
    ) -> Result<(), String> {
        use super::btrfs_resize::Phase;
        let shrink = self
            .plan
            .shrink
            .as_ref()
            .ok_or("resize checkpoint lacks shrink plan")?;
        if self.plan.basis.linux_btrfs_volume.is_none()
            || bytes
                != match phase {
                    Phase::ShrinkFilesystem | Phase::ShrinkPartition => shrink.to_bytes,
                    Phase::GrowPartition | Phase::GrowFilesystem => shrink.from_bytes,
                }
        {
            return Err("resize checkpoint differs from retained Linux plan".into());
        }
        Ok(())
    }

    fn validate_resize_checkpoints(&self) -> Result<(), String> {
        use super::btrfs_resize::Phase;
        for phase in [
            Phase::ShrinkFilesystem,
            Phase::ShrinkPartition,
            Phase::GrowPartition,
            Phase::GrowFilesystem,
        ] {
            if let Some(checkpoint) = self.resize_checkpoint(phase)? {
                self.validate_resize_target(phase, checkpoint.bytes)?;
                if checkpoint.state == StepState::Failed {
                    return Err("invalid nested resize checkpoint state".into());
                }
                if self.is_done(Self::resize_key(phase).0) && checkpoint.state != StepState::Done {
                    return Err("completed resize retains unfinished nested checkpoint".into());
                }
                let predecessor = match phase {
                    Phase::ShrinkPartition => Some(Phase::ShrinkFilesystem),
                    Phase::GrowFilesystem => Some(Phase::GrowPartition),
                    _ => None,
                };
                if let Some(predecessor) = predecessor
                    && self
                        .resize_checkpoint(predecessor)?
                        .is_none_or(|previous| previous.state != StepState::Done)
                {
                    return Err("resize checkpoint precedes durable prior readback".into());
                }
            }
        }
        Ok(())
    }

    fn set(
        &mut self,
        step: Step,
        state: StepState,
        detail: serde_json::Map<String, serde_json::Value>,
    ) {
        let at = ui::time::unix_now();
        if let Some(existing) = self.steps.iter_mut().find(|s| s.step == step) {
            existing.state = state;
            existing.at = at;
            existing.detail.extend(detail);
        } else {
            self.steps.push(StepRecord {
                step,
                state,
                detail,
                at,
            });
        }
    }

    /// Record that `step` is about to be attempted. Detail merges into any
    /// earlier attempt's, so identities recorded before a crash survive.
    pub fn intend(&mut self, step: Step, detail: serde_json::Map<String, serde_json::Value>) {
        self.set(step, StepState::Intended, detail);
    }

    /// Record that `step` holds, as read back.
    pub fn done(&mut self, step: Step, detail: serde_json::Map<String, serde_json::Value>) {
        self.set(step, StepState::Done, detail);
    }

    pub fn failed(&mut self, step: Step, error: &str) {
        let mut detail = serde_json::Map::new();
        detail.insert("error".into(), error.into());
        self.set(step, StepState::Failed, detail);
    }

    /// Some mutation may have happened without being confirmed.
    pub fn has_intended(&self) -> bool {
        self.steps.iter().any(|s| s.state == StepState::Intended)
    }

    /// The first step of `order` that is not done, where resume continues.
    pub fn resume_point(&self, order: &[Step]) -> Option<Step> {
        order.iter().copied().find(|s| !self.is_done(*s))
    }

    pub fn status(&self) -> RecordStatus {
        if Step::REMOVE.iter().all(|s| self.is_done(*s)) {
            RecordStatus::Removed
        } else if Step::REMOVE.iter().all(|s| self.state(*s).is_none())
            && Step::STAGE.iter().all(|s| self.is_done(*s))
        {
            RecordStatus::Staged
        } else {
            RecordStatus::InProgress
        }
    }

    /// Structural checks for a record read from disk.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err("unsupported staging record schema".into());
        }
        if !valid_operation_id(&self.operation_id) {
            return Err("invalid staging record operation ID".into());
        }
        if self.plan.digest().map_err(|error| error.to_string())? != self.plan_sha256 {
            return Err("staging record plan does not match its digest".into());
        }
        self.validate_resize_checkpoints()?;
        super::handoff::validate_profile(&self.profile)?;
        if let Some(expected) = &self.prepared_setup
            && (!expected.valid()
                || expected.sealed_bytes > super::prepared_setup::SLOT_BYTES
                || self.plan.basis.host_os() != super::plan::HostOs::Linux)
        {
            return Err("invalid prepared setup custody in staging record".into());
        }
        #[cfg(target_os = "linux")]
        if let Some(browser) = &self.prepared_browser {
            browser.validate().map_err(|error| error.to_string())?;
            if self.prepared_setup.is_none()
                || self.plan.basis.host_os() != super::plan::HostOs::Linux
            {
                return Err("Browser custody requires prepared Linux identity".into());
            }
        }
        if let Some(raw) = &self.migration {
            #[cfg(not(target_os = "linux"))]
            {
                let _ = raw;
                return Err("Linux migration cannot be resumed on this host".into());
            }
            #[cfg(target_os = "linux")]
            {
                let migration = crate::setup::migration_handoff::Plan::parse(
                    &serde_json::to_vec(raw).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                migration
                    .validate_for_install(&self.plan)
                    .map_err(|error| error.to_string())?;
                if migration.operation_id != self.operation_id {
                    return Err("staging migration consent belongs to another operation".into());
                }
            }
        }
        Ok(())
    }
}

/// Record storage: the private authoritative directory plus the public
/// status mirror.
#[derive(Clone, Debug)]
pub struct RecordStore {
    pub dir: PathBuf,
    pub public_dir: Option<PathBuf>,
    /// Refuse records not owned by root/Administrators (production).
    pub require_privileged_owner: bool,
}

pub use imager_backend::staging::record_io::{public_dir, system_dir};

impl RecordStore {
    pub fn system() -> Result<Self, String> {
        Ok(Self {
            dir: system_dir().ok_or("no system record directory on this host")?,
            public_dir: public_dir(),
            require_privileged_owner: true,
        })
    }

    #[cfg(test)]
    pub fn at(dir: &Path) -> Self {
        Self {
            dir: dir.join("staging"),
            public_dir: Some(dir.join("staging-status")),
            require_privileged_owner: false,
        }
    }

    fn file_name(id: &str) -> Result<String, String> {
        if valid_operation_id(id) {
            Ok(format!("{id}.json"))
        } else {
            Err("invalid operation ID".into())
        }
    }

    pub(super) fn prepared_path(&self, id: &str) -> Result<PathBuf, String> {
        if !valid_operation_id(id) {
            return Err("invalid operation ID".into());
        }
        Ok(self.dir.join(format!("{id}.prepared-setup.sealed")))
    }
    pub(super) fn retain_prepared(
        &self,
        id: &str,
        bytes: &[u8],
        expected: &crate::model::PreparedSetupExpectation,
    ) -> Result<(), String> {
        super::prepared_setup::verify(bytes, expected).map_err(|error| error.to_string())?;
        secure::ensure_dir(&self.dir, false)?;
        let path = self.prepared_path(id)?;
        match std::fs::symlink_metadata(&path) {
            Ok(_) => return self.read_prepared(id, expected).map(|_| ()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let mut temporary =
            tempfile::NamedTempFile::new_in(&self.dir).map_err(|error| error.to_string())?;
        std::io::Write::write_all(&mut temporary, bytes).map_err(|error| error.to_string())?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| error.to_string())?;
        if let Err(error) = temporary.persist_noclobber(&path)
            && error.error.kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(error.to_string());
        }
        std::fs::File::open(&self.dir)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        self.read_prepared(id, expected).map(|_| ())
    }
    pub(super) fn read_prepared(
        &self,
        id: &str,
        expected: &crate::model::PreparedSetupExpectation,
    ) -> Result<Vec<u8>, String> {
        let artifact = super::prepared_setup::PreparedArtifact {
            browser: None,
            source_path: self
                .prepared_path(id)?
                .to_str()
                .ok_or("invalid custody path")?
                .into(),
            expectation: expected.clone(),
        };
        #[cfg(unix)]
        // SAFETY: reads credentials only, never changes source ownership.
        let owner = rustix::process::geteuid().as_raw();
        #[cfg(not(unix))]
        let owner = 0;
        artifact.read(owner).map_err(|error| error.to_string())
    }

    /// Persist atomically, then mirror. A failed mirror is an error too: the
    /// UI must learn of every record that might need a Remove.
    pub fn save(&self, record: &Record) -> Result<(), String> {
        // Reject an out-of-order nested resize before either durable private
        // state or its public mirror can replace the last recoverable journal.
        record.validate_resize_checkpoints()?;
        let name = Self::file_name(&record.operation_id)?;
        let bytes = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
        secure::ensure_dir(&self.dir, false)?;
        write_atomic(&self.dir, &name, &bytes, false).map_err(|e| format!("record write: {e}"))?;
        if let Some(public) = &self.public_dir {
            secure::ensure_dir(public, true)?;
            let mut display = record.clone();
            display.migration = None;
            let display_bytes =
                serde_json::to_vec_pretty(&display).map_err(|error| error.to_string())?;
            write_atomic(public, &name, &display_bytes, true)
                .map_err(|e| format!("status write: {e}"))?;
        }
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<Option<Record>, String> {
        let path = self.dir.join(Self::file_name(id)?);
        let bytes = match read_bounded(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("record read: {e}")),
        };
        if self.require_privileged_owner {
            secure::check_owner(&path)?;
        }
        let record: Record = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        record.validate()?;
        if record.operation_id != id {
            return Err("staging record name and content disagree".into());
        }
        Ok(Some(record))
    }

    /// Every valid private record; unreadable or invalid files are errors so
    /// the pending-operation rule cannot be bypassed by a corrupt file.
    pub fn list(&self) -> Result<Vec<Record>, String> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("record directory: {e}")),
        };
        let mut out = Vec::new();
        for entry in entries {
            let name = entry.map_err(|e| e.to_string())?.file_name();
            let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
                continue;
            };
            if !valid_operation_id(id) {
                continue;
            }
            if let Some(record) = self.load(id)? {
                out.push(record);
            }
        }
        out.sort_by(|a, b| a.operation_id.cmp(&b.operation_id));
        Ok(out)
    }
}

/// Unfinished records from the public status mirror in `dir`, skipping
/// anything unreadable. Display-only: authority stays with the helper.
pub fn pending_in(dir: &Path) -> Vec<Record> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Record> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name();
            let id = name.to_str()?.strip_suffix(".json")?.to_string();
            valid_operation_id(&id).then_some(())?;
            let bytes = read_bounded(&e.path()).ok()?;
            let record: Record = serde_json::from_slice(&bytes).ok()?;
            (record.validate().is_ok() && record.operation_id == id).then_some(record)
        })
        .filter(|r| r.status() != RecordStatus::Removed)
        .collect();
    out.sort_by(|a, b| a.operation_id.cmp(&b.operation_id));
    out
}

pub use imager_backend::staging::record_io::write_atomic;
use imager_backend::staging::record_io::{read_bounded, secure};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::staging::plan;

    pub(crate) fn sample_record(id: &str) -> Record {
        let basis = plan::tests::linux_basis(512, 256 << 30, Vec::new());
        let plan = plan::compute(
            &basis,
            plan::Requirements {
                staging_esp_bytes: 600 << 20,
                staging_root_bytes: 40 << 30,
                target_bytes: 64 << 30,
            },
        )
        .unwrap();
        Record::new(
            id.into(),
            plan,
            ReleaseRef {
                release_id: "fixture-only".into(),
                manifest_sha256: "a".repeat(64),
                architecture: "x86_64".into(),
            },
            OriginalFirmware {
                boot_order: vec!["0001".into()],
                boot_next: None,
                entries: BTreeMap::new(),
            },
        )
        .unwrap()
    }

    #[test]
    fn btrfs_resize_journal_requires_durable_order_and_preserves_retry_intent() {
        use crate::setup::staging::btrfs_resize::Phase;
        let temp = tempfile::tempdir().unwrap();
        let store = RecordStore {
            dir: temp.path().join("private"),
            public_dir: None,
            require_privileged_owner: false,
        };
        let mut record = sample_record("btrfs-resize-fixture");
        record.plan.shrink = Some(plan::Shrink {
            partition_guid: "a".repeat(36),
            from_bytes: 128 << 30,
            to_bytes: 64 << 30,
        });
        record.plan.basis.linux_btrfs_volume = Some(super::super::platform::LinuxBtrfsVolume {
            partition_guid: "a".repeat(36),
            filesystem_uuid: "b".repeat(36),
            device_id: 1,
            mount_id: 12,
            first_lba: 2048,
            sector_bytes: 512,
            size_bytes: 128 << 30,
            filesystem_bytes: 128 << 30,
            kernel_partition_bytes: 128 << 30,
            supported_min_bytes: 32 << 30,
            reserve_bytes: 8 << 30,
        });
        record.plan_sha256 = record.plan.digest().unwrap();
        record.intend(Step::Shrink, serde_json::Map::new());
        assert!(
            record
                .resize_done(Phase::ShrinkFilesystem, 64 << 30)
                .is_err()
        );
        record
            .resize_intent(Phase::ShrinkFilesystem, 64 << 30)
            .unwrap();
        store.save(&record).unwrap();
        let mut loaded = store.load(&record.operation_id).unwrap().unwrap();
        assert_eq!(
            loaded
                .resize_checkpoint(Phase::ShrinkFilesystem)
                .unwrap()
                .unwrap()
                .state,
            StepState::Intended
        );
        loaded
            .resize_intent(Phase::ShrinkPartition, 64 << 30)
            .unwrap();
        assert!(
            store.save(&loaded).is_err(),
            "partition intent precedes filesystem readback"
        );
        let retained = store.load(&record.operation_id).unwrap().unwrap();
        assert!(
            retained
                .resize_checkpoint(Phase::ShrinkPartition)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            retained
                .resize_checkpoint(Phase::ShrinkFilesystem)
                .unwrap()
                .unwrap()
                .state,
            StepState::Intended
        );
        loaded
            .resize_done(Phase::ShrinkFilesystem, 64 << 30)
            .unwrap();
        store.save(&loaded).unwrap();
        loaded
            .resize_done(Phase::ShrinkPartition, 64 << 30)
            .unwrap();
        loaded.done(Step::Shrink, serde_json::Map::new());
        store.save(&loaded).unwrap();
        loaded
            .resize_intent(Phase::ShrinkFilesystem, 64 << 30)
            .unwrap();
        assert_eq!(
            loaded
                .resize_checkpoint(Phase::ShrinkFilesystem)
                .unwrap()
                .unwrap()
                .state,
            StepState::Done,
            "retry does not erase observed completion"
        );
        assert!(
            loaded
                .resize_intent(Phase::ShrinkFilesystem, 63 << 30)
                .is_err()
        );
        assert!(
            loaded
                .resize_done(Phase::GrowFilesystem, 128 << 30)
                .is_err()
        );
        loaded.intend(Step::RestoreShrink, serde_json::Map::new());
        loaded
            .resize_intent(Phase::GrowPartition, 128 << 30)
            .unwrap();
        loaded.resize_done(Phase::GrowPartition, 128 << 30).unwrap();
        loaded
            .resize_intent(Phase::GrowFilesystem, 128 << 30)
            .unwrap();
        store.save(&loaded).unwrap();
        loaded.done(Step::RestoreShrink, serde_json::Map::new());
        assert!(
            store.save(&loaded).is_err(),
            "outer Done requires actual filesystem completion"
        );
        loaded
            .resize_done(Phase::GrowFilesystem, 128 << 30)
            .unwrap();
        store.save(&loaded).unwrap();
    }

    /// Record state machine, store round trip, removal/corruption and ownership.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn staging_records_track_steps_persist_atomically_and_reject_foreign_owners() {
        // Phase `state_machine_tracks_one_entry_per_step_and_merges_detail`.
        {
            let mut r = sample_record("op-1");
            assert_eq!(r.status(), RecordStatus::InProgress);
            let mut d = serde_json::Map::new();
            d.insert("partition_guid".into(), "g".into());
            r.intend(Step::CreateEsp, d);
            assert!(r.has_intended());
            r.failed(Step::CreateEsp, "boom");
            assert_eq!(r.state(Step::CreateEsp), Some(StepState::Failed));
            assert_eq!(r.detail_str(Step::CreateEsp, "partition_guid"), Some("g"));
            r.done(Step::CreateEsp, serde_json::Map::new());
            assert_eq!(r.steps.len(), 1);
            assert!(!r.has_intended());
            assert_eq!(r.resume_point(&Step::STAGE), Some(Step::Shrink));
            for s in Step::STAGE {
                r.done(s, serde_json::Map::new());
            }
            assert_eq!(r.resume_point(&Step::STAGE), None);
            assert_eq!(r.status(), RecordStatus::Staged);
            r.intend(Step::RemoveBootNext, serde_json::Map::new());
            assert_eq!(r.status(), RecordStatus::InProgress);
            for s in Step::REMOVE {
                r.done(s, serde_json::Map::new());
            }
            assert_eq!(r.status(), RecordStatus::Removed);
        }

        // Phase `store_round_trips_atomically_and_mirrors_publicly`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let store = RecordStore::at(tmp.path());
            let r = sample_record("0f0e0d0c-0000-4000-8000-000000000001");
            store.save(&r).unwrap();
            store.save(&r).unwrap();
            assert_eq!(store.load(&r.operation_id).unwrap(), Some(r.clone()));
            assert_eq!(store.list().unwrap(), vec![r.clone()]);
            assert_eq!(
                pending_in(store.public_dir.as_ref().unwrap()),
                vec![r.clone()]
            );
            let leftovers: Vec<_> = std::fs::read_dir(&store.dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            assert_eq!(
                leftovers,
                vec![format!("{}.json", r.operation_id)],
                "no temp files"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode(&store.dir), 0o700);
                assert_eq!(
                    mode(&store.dir.join(format!("{}.json", r.operation_id))),
                    0o600
                );
                let public = store.public_dir.clone().unwrap();
                assert_eq!(mode(&public), 0o755);
                assert_eq!(
                    mode(&public.join(format!("{}.json", r.operation_id))),
                    0o644
                );
            }
            assert_eq!(store.load("missing-op").unwrap(), None);
        }

        // Phase `removed_records_are_not_pending_and_corrupt_ones_fail_listing`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let store = RecordStore::at(tmp.path());
            let mut r = sample_record("op-removed");
            for s in Step::REMOVE {
                r.done(s, serde_json::Map::new());
            }
            store.save(&r).unwrap();
            assert!(pending_in(store.public_dir.as_ref().unwrap()).is_empty());
            let mut tampered = sample_record("op-tampered");
            tampered.plan.target_extent.last_lba += 1;
            std::fs::write(
                store.dir.join("op-tampered.json"),
                serde_json::to_vec(&tampered).unwrap(),
            )
            .unwrap();
            assert!(
                store.list().is_err(),
                "a tampered record must not be skipped silently"
            );
            assert!(store.load("../etc").is_err());
        }

        // Phase `privileged_store_rejects_records_not_owned_by_root`.
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            let mut store = RecordStore::at(tmp.path());
            let r = sample_record("op-user");
            store.save(&r).unwrap();
            store.require_privileged_owner = true;
            // Temp dirs are not root-only (0700 but owned by the test user, or
            // under a world-writable parent when run as root): refuse either way.
            let mut parent_open = store.clone();
            parent_open.dir = tmp.path().to_path_buf();
            std::fs::copy(
                store.dir.join("op-user.json"),
                tmp.path().join("op-user.json"),
            )
            .unwrap();
            if tilo_process::effective_uid() == 0 {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o777))
                    .unwrap();
            } else {
                assert!(store.load("op-user").is_err());
            }
            assert!(parent_open.load("op-user").is_err());
        }
    }
}

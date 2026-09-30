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

pub const SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Step {
    Shrink,
    CreateEsp,
    CreateRoot,
    WriteEsp,
    WriteRoot,
    Verify,
    Handoff,
    BootEntry,
    BootNext,
    RemoveBootNext,
    RemoveBootEntry,
    RemoveRoot,
    RemoveEsp,
    RestoreShrink,
}

impl Step {
    /// `stage` steps in execution order.
    pub const STAGE: [Self; 9] = [
        Self::Shrink,
        Self::CreateEsp,
        Self::CreateRoot,
        Self::WriteEsp,
        Self::WriteRoot,
        Self::Verify,
        Self::Handoff,
        Self::BootEntry,
        Self::BootNext,
    ];
    /// `remove` steps in execution order.
    pub const REMOVE: [Self; 5] = [
        Self::RemoveBootNext,
        Self::RemoveBootEntry,
        Self::RemoveRoot,
        Self::RemoveEsp,
        Self::RestoreShrink,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepState {
    Intended,
    Done,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    pub step: Step,
    pub state: StepState,
    #[serde(default)]
    pub detail: serde_json::Map<String, serde_json::Value>,
    /// Seconds since the Unix epoch.
    pub at: u64,
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
        .map_or(0, |d| d.as_secs())
}

/// Operation IDs become file names; allow only a UUID-like identifier.
pub fn valid_operation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() || c == b'-')
}

impl Record {
    pub fn new(
        operation_id: String,
        plan: Plan,
        release: ReleaseRef,
        original_firmware: OriginalFirmware,
    ) -> Self {
        Self {
            schema: SCHEMA,
            operation_id,
            plan_sha256: plan.digest(),
            plan,
            release,
            steps: Vec::new(),
            original_firmware,
            owned_boot_entry: None,
            bundle: None,
        }
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

    fn set(
        &mut self,
        step: Step,
        state: StepState,
        detail: serde_json::Map<String, serde_json::Value>,
    ) {
        let at = now();
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
        if self.plan.digest() != self.plan_sha256 {
            return Err("staging record plan does not match its digest".into());
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

/// Linux: `/var/lib/tilo-imager/staging`; Windows: `%ProgramData%\Tilo\staging`.
pub fn system_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let base = std::env::var_os("ProgramData").map(PathBuf::from)?;
        Some(base.join("Tilo").join("staging"))
    }
    #[cfg(not(windows))]
    {
        Some(PathBuf::from("/var/lib/tilo-imager/staging"))
    }
}

/// The world-readable status mirror next to [`system_dir`].
pub fn public_dir() -> Option<PathBuf> {
    let dir = system_dir()?;
    Some(dir.with_file_name("staging-status"))
}

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

    /// Persist atomically, then mirror. A failed mirror is an error too: the
    /// UI must learn of every record that might need a Remove.
    pub fn save(&self, record: &Record) -> Result<(), String> {
        let name = Self::file_name(&record.operation_id)?;
        let bytes = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
        secure::ensure_dir(&self.dir, false)?;
        write_atomic(&self.dir, &name, &bytes, false).map_err(|e| format!("record write: {e}"))?;
        if let Some(public) = &self.public_dir {
            secure::ensure_dir(public, true)?;
            write_atomic(public, &name, &bytes, true).map_err(|e| format!("status write: {e}"))?;
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

const MAX_RECORD_BYTES: u64 = 4 << 20;

fn read_bounded(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("record is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(std::io::Error::other("record is unexpectedly large"));
    }
    Ok(bytes)
}

/// Write `bytes` to `dir/name` durably and atomically. `public` selects 0644
/// over 0600.
pub fn write_atomic(dir: &Path, name: &str, bytes: &[u8], public: bool) -> std::io::Result<()> {
    let path = dir.join(name);
    #[cfg(unix)]
    {
        atomic_file::write_with_mode(&path, bytes, if public { 0o644 } else { 0o600 })
    }
    #[cfg(not(unix))]
    {
        let _ = public;
        atomic_file::write(&path, bytes)
    }
}

#[cfg(unix)]
mod secure {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;

    /// Create `dir` (and parents) and make it root-safe: private 0700 or
    /// public 0755, never group/world writable, owned by the current euid.
    pub(super) fn ensure_dir(dir: &Path, public: bool) -> Result<(), String> {
        let io = |e: std::io::Error| format!("{}: {e}", dir.display());
        std::fs::create_dir_all(dir).map_err(io)?;
        let meta = std::fs::symlink_metadata(dir).map_err(io)?;
        // SAFETY: geteuid has no preconditions.
        let euid = unsafe { libc::geteuid() };
        if !meta.is_dir() || meta.uid() != euid {
            return Err(format!(
                "{} is not a directory owned by this helper",
                dir.display()
            ));
        }
        let mode = if public { 0o755 } else { 0o700 };
        if meta.permissions().mode() & 0o7777 != mode {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).map_err(io)?;
        }
        Ok(())
    }

    pub(super) fn check_owner(path: &Path) -> Result<(), String> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        let parent = path
            .parent()
            .map(std::fs::symlink_metadata)
            .transpose()
            .map_err(|e| e.to_string())?;
        let root_only = |m: &std::fs::Metadata| m.uid() == 0 && m.permissions().mode() & 0o022 == 0;
        if !meta.is_file() || !root_only(&meta) || !parent.as_ref().is_some_and(root_only) {
            return Err(format!(
                "{} is not a root-owned staging record",
                path.display()
            ));
        }
        Ok(())
    }
}

#[cfg(windows)]
mod secure {
    //! Protected DACLs for the record directories. `%ProgramData%` lets
    //! ordinary users create subdirectories, so a pre-planted directory must
    //! be re-owned and re-protected rather than trusted.
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr;
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        PSID,
    };

    const PRIVATE: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
    const PUBLIC: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FR;;;BU)";

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain([0]).collect()
    }

    struct Local(*mut core::ffi::c_void);
    impl Drop for Local {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: allocated by a LocalAlloc-returning API below.
                unsafe { LocalFree(self.0) };
            }
        }
    }

    pub(super) fn ensure_dir(dir: &Path, public: bool) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let text: Vec<u16> = if public { PUBLIC } else { PRIVATE }
            .encode_utf16()
            .chain([0])
            .collect();
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: valid NUL-terminated SDDL; output freed by `Local`.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut sd,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let _sd = Local(sd);
        let mut owner: PSID = ptr::null_mut();
        let mut defaulted = 0;
        let mut present = 0;
        let mut dacl = ptr::null_mut();
        // SAFETY: `sd` is a live self-relative descriptor.
        unsafe {
            GetSecurityDescriptorOwner(sd, &mut owner, &mut defaulted);
            GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted);
        }
        let path = wide(dir);
        // SAFETY: path is NUL-terminated; owner/dacl point into `sd`.
        let rc = unsafe {
            SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                owner,
                ptr::null_mut(),
                dacl,
                ptr::null(),
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(format!("{}: security update failed ({rc})", dir.display()));
        }
        Ok(())
    }

    pub(super) fn check_owner(path: &Path) -> Result<(), String> {
        let wide_path = wide(path);
        let mut owner: PSID = ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: path NUL-terminated; `sd` freed by `Local`.
        let rc = unsafe {
            GetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut sd,
            )
        };
        let _sd = Local(sd);
        if rc != ERROR_SUCCESS || owner.is_null() {
            return Err(format!("{}: owner unreadable ({rc})", path.display()));
        }
        let mut text = ptr::null_mut();
        // SAFETY: `owner` points into the live descriptor.
        if unsafe { ConvertSidToStringSidW(owner, &mut text) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let _text = Local(text.cast());
        let mut n = 0;
        // SAFETY: ConvertSidToStringSidW returns a NUL-terminated string.
        while n < 256 && unsafe { *text.add(n) } != 0 {
            n += 1;
        }
        // SAFETY: `n` units were just read.
        let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, n) });
        if !matches!(sid.as_str(), "S-1-5-18" | "S-1-5-32-544") {
            return Err(format!(
                "{} is not owned by SYSTEM or Administrators",
                path.display()
            ));
        }
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
mod secure {
    use std::path::Path;
    pub(super) fn ensure_dir(_: &Path, _: bool) -> Result<(), String> {
        Err("staging records are unsupported on this platform".into())
    }
    pub(super) fn check_owner(_: &Path) -> Result<(), String> {
        Err("staging records are unsupported on this platform".into())
    }
}

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
            // SAFETY: geteuid has no preconditions.
            if unsafe { libc::geteuid() } == 0 {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o777))
                    .unwrap();
                assert!(parent_open.load("op-user").is_err());
            } else {
                assert!(store.load("op-user").is_err());
                assert!(parent_open.load("op-user").is_err());
            }
        }
    }
}

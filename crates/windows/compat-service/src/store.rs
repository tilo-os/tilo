use std::collections::BTreeMap;
use std::fs;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use compat_catalogue::{
    ApplicationQuery, LaunchPlan, Layer, Runner, RunnerSettings, SecurityProfileRef,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const CAPSULE_SCHEMA_VERSION: u16 = 1;
const SANDBOX_FILES_ROOT: &str = "/run/tilo/files";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Why a managed executable exists in capsule state. Installer control rows
/// remain visible when setup failed or produced no discoverable shortcut, but
/// can never be launched through the ordinary Open action.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramKind {
    #[default]
    Application,
    InstallerControl,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallerKind {
    Exe,
    Msi,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationStatus {
    Installing,
    Installed,
    NeedsAttention,
    Failed,
    Uninstalling,
    Uninstalled,
}

/// Durable install journal. The source path never enters state: only the hash
/// of the bytes received over `SCM_RIGHTS` and a managed prefix-relative copy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstallationRecord {
    pub source_name: String,
    pub display_name: String,
    pub sha256: String,
    pub kind: InstallerKind,
    pub installer_relative_path: PathBuf,
    pub status: InstallationStatus,
    #[serde(default)]
    pub last_exit_code: Option<i32>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub uninstaller_relative_path: Option<PathBuf>,
}

impl InstallationRecord {
    fn validate(&self) -> Result<(), CapsuleError> {
        if self.source_name.is_empty()
            || self.source_name.len() > 255
            || self.source_name.chars().any(char::is_control)
            || self.display_name.trim().is_empty()
            || self.display_name.len() > 255
            || self.display_name.chars().any(char::is_control)
            || self.sha256.len() != 64
            || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(CapsuleError::InvalidInstallation);
        }
        validate_relative_installer(&self.installer_relative_path, self.kind)?;
        if let Some(uninstaller) = &self.uninstaller_relative_path {
            validate_relative_executable(uninstaller)?;
        }
        if self
            .last_error
            .as_deref()
            .is_some_and(|error| error.len() > 2_048 || error.chars().any(char::is_control))
        {
            return Err(CapsuleError::InvalidInstallation);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapsuleProgram {
    /// Stable opaque identifier used by Tilo search and launch actions.
    pub id: String,
    pub display_name: String,
    /// Relative to the prefix root, normally `drive_c/Program Files/...`.
    pub prefix_relative_executable: PathBuf,
    /// Identity metadata captured by the trusted installer/importer. Live
    /// clients select this program by opaque ID and cannot replace this query.
    #[serde(default)]
    pub query: ApplicationQuery,
    #[serde(default)]
    pub kind: ProgramKind,
    /// Fixed Windows arguments captured by the trusted importer. They are
    /// argv entries, never a command string. Normal discovered apps use none;
    /// MSI setup uses the allow-listed `/i C:\\...` shape.
    #[serde(default)]
    pub arguments: Vec<String>,
}

impl CapsuleProgram {
    pub fn validate(&self) -> Result<(), CapsuleError> {
        validate_id("program id", &self.id)?;
        if self.display_name.trim().is_empty() || self.display_name.chars().any(char::is_control) {
            return Err(CapsuleError::InvalidProgramName);
        }
        validate_relative_executable(&self.prefix_relative_executable)?;
        if self.arguments.len() > 16
            || self.arguments.iter().any(|argument| {
                argument.is_empty()
                    || argument.len() > 1_024
                    || argument.chars().any(char::is_control)
            })
        {
            return Err(CapsuleError::InvalidArguments);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapsuleState {
    pub schema_version: u16,
    pub id: String,
    pub layer: Layer,
    pub runner: Runner,
    pub settings: RunnerSettings,
    pub security_profile: SecurityProfileRef,
    #[serde(default)]
    pub programs: BTreeMap<String, CapsuleProgram>,
    #[serde(default)]
    pub installation: Option<InstallationRecord>,
}

impl CapsuleState {
    pub fn validate(&self) -> Result<(), CapsuleError> {
        if self.schema_version != CAPSULE_SCHEMA_VERSION {
            return Err(CapsuleError::UnsupportedSchema(self.schema_version));
        }
        validate_id("capsule id", &self.id)?;
        if !matches!(self.layer, Layer::Wine | Layer::Proton) {
            return Err(CapsuleError::InvalidLayer);
        }
        for (id, program) in &self.programs {
            program.validate()?;
            if id != &program.id {
                return Err(CapsuleError::ProgramKeyMismatch);
            }
        }
        if let Some(installation) = &self.installation {
            installation.validate()?;
        }
        Ok(())
    }

    #[must_use]
    pub fn program(&self, id: &str) -> Option<&CapsuleProgram> {
        self.programs.get(id)
    }
}

#[derive(Clone, Debug)]
pub struct CapsuleStore {
    /// Canonical, daemon-configured storage root. It is an informational
    /// path; security-sensitive traversal is always rooted at `root_fd`.
    root: PathBuf,
    /// Keeps the originally opened root directory alive. All managed paths
    /// below are opened relative to this descriptor with `O_NOFOLLOW`, so a
    /// same-UID attacker cannot redirect a later operation by renaming or
    /// symlinking a component below the store root.
    root_fd: Arc<File>,
}

impl CapsuleStore {
    /// `root` is daemon configuration, never catalogue or socket input.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, CapsuleError> {
        let root = root.into();
        if !root.is_absolute() || root == Path::new("/") {
            return Err(CapsuleError::UnsafeRoot);
        }
        fs::create_dir_all(&root)?;
        let root = fs::canonicalize(root)?;
        let root_fd = open_directory_nofollow(&root)?;
        Ok(Self {
            root,
            root_fd: Arc::new(root_fd),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn load(&self, id: &str) -> Result<Option<CapsuleState>, CapsuleError> {
        validate_id("capsule id", id)?;
        let directory = match self.capsule_directory(id, false) {
            Ok(directory) => directory,
            Err(CapsuleError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let mut state = match open_regular_nofollow(&directory, "state.json") {
            Ok(state) => state,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        state.read_to_end(&mut bytes)?;
        let state: CapsuleState = serde_json::from_slice(&bytes)?;
        state.validate()?;
        if state.id != id {
            return Err(CapsuleError::StateIdentityMismatch);
        }
        Ok(Some(state))
    }

    /// Enumerate registered programs from private capsule state. Directory
    /// names and state identities are revalidated; malformed state fails the
    /// whole snapshot instead of disappearing from management UI.
    pub fn registered_programs(&self) -> Result<Vec<RegisteredProgram>, CapsuleError> {
        let capsules = match Self::open_child_dir(self.root_fd.as_ref(), "capsules", false) {
            Ok(capsules) => capsules,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let entries = fs::read_dir(fd_path(&capsules))?;
        let mut ids = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect::<Vec<_>>();
        ids.sort();
        let mut programs = Vec::new();
        for capsule_id in ids {
            validate_id("capsule id", &capsule_id)?;
            let Some(state) = self.load(&capsule_id)? else {
                continue;
            };
            for program in state.programs.values() {
                let mut query = program.query.clone();
                if query.executable.is_none() {
                    query.executable =
                        Some(program.prefix_relative_executable.display().to_string());
                }
                programs.push(RegisteredProgram {
                    entry_id: managed_program_id(&state.id, &program.id),
                    app_id: managed_app_id(&state.id, &program.id),
                    capsule_id: state.id.clone(),
                    program: program.clone(),
                    query,
                });
            }
        }
        Ok(programs)
    }

    /// Enumerate capsule states that carry a durable install journal.
    pub fn installations(&self) -> Result<Vec<CapsuleState>, CapsuleError> {
        let capsules = match Self::open_child_dir(self.root_fd.as_ref(), "capsules", false) {
            Ok(capsules) => capsules,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut ids = fs::read_dir(fd_path(&capsules))?
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect::<Vec<_>>();
        ids.sort();
        let mut states = Vec::new();
        for id in ids {
            validate_id("capsule id", &id)?;
            if let Some(state) = self.load(&id)?
                && state.installation.is_some()
            {
                states.push(state);
            }
        }
        Ok(states)
    }

    /// Creates a capsule on first use. A recipe change is never silently
    /// applied to an existing prefix; management UI must explicitly migrate
    /// or duplicate it so rollback remains possible.
    pub fn ensure_for_plan(&self, plan: &LaunchPlan) -> Result<CapsuleState, CapsuleError> {
        if !matches!(plan.runtime.layer, Layer::Wine | Layer::Proton) {
            return Err(CapsuleError::InvalidLayer);
        }
        validate_id("capsule id", &plan.runtime.id)?;
        if let Some(existing) = self.load(&plan.runtime.id)? {
            if existing.layer != plan.runtime.layer
                || existing.runner != plan.runner
                || existing.settings != plan.settings
                || existing.security_profile != plan.security_profile
            {
                return Err(CapsuleError::RecipeChanged(plan.runtime.id.clone()));
            }
            self.prepare_managed_paths(&existing.id)?;
            return Ok(existing);
        }

        let state = CapsuleState {
            schema_version: CAPSULE_SCHEMA_VERSION,
            id: plan.runtime.id.clone(),
            layer: plan.runtime.layer,
            runner: plan.runner.clone(),
            settings: plan.settings.clone(),
            security_profile: plan.security_profile.clone(),
            programs: BTreeMap::new(),
            installation: None,
        };
        state.validate()?;
        self.prepare_managed_paths(&state.id)?;
        self.save(&state)?;
        Ok(state)
    }

    pub fn register_program(
        &self,
        capsule_id: &str,
        program: CapsuleProgram,
    ) -> Result<CapsuleState, CapsuleError> {
        program.validate()?;
        let mut state = self
            .load(capsule_id)?
            .ok_or_else(|| CapsuleError::NotFound(capsule_id.to_string()))?;
        state.programs.insert(program.id.clone(), program);
        self.save(&state)?;
        Ok(state)
    }

    /// Copy a validated installer into the capsule through descriptor-anchored
    /// directories. `expected_sha256` binds the later setup process to the
    /// exact bytes that were inspected even if the selected portal document
    /// changes between inspection and copy.
    pub(crate) fn copy_installer(
        &self,
        capsule_id: &str,
        source: &mut File,
        expected_sha256: &str,
        kind: InstallerKind,
        max_bytes: u64,
    ) -> Result<PathBuf, CapsuleError> {
        validate_id("capsule id", capsule_id)?;
        if expected_sha256.len() != 64
            || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(CapsuleError::InvalidInstallation);
        }
        let capsule = self.capsule_directory(capsule_id, false)?;
        let data = Self::open_child_dir(&capsule, "data", false)?;
        let prefix = Self::open_child_dir(&data, "prefix", false)?;
        let drive_c = Self::open_child_dir(&prefix, "drive_c", true)?;
        let installers = Self::open_child_dir(&drive_c, "tilo-installers", true)?;
        let extension = match kind {
            InstallerKind::Exe => "exe",
            InstallerKind::Msi => "msi",
        };
        let name = format!("{expected_sha256}.{extension}");
        let temporary = format!(
            ".{name}.{}.tmp",
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let mut destination = create_regular_exclusive(&installers, &temporary, 0o600)?;
        source.rewind()?;
        let mut digest = Sha256::new();
        let mut copied = 0_u64;
        let mut buffer = vec![0_u8; 128 * 1024].into_boxed_slice();
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            copied = copied
                .checked_add(u64::try_from(read).map_err(|_| CapsuleError::InstallerTooLarge)?)
                .ok_or(CapsuleError::InstallerTooLarge)?;
            if copied > max_bytes {
                let _ = unlink_at(&installers, &temporary);
                return Err(CapsuleError::InstallerTooLarge);
            }
            let chunk = buffer.get(..read).ok_or(CapsuleError::InstallerChanged)?;
            digest.update(chunk);
            destination.write_all(chunk)?;
        }
        let actual = hex::encode(digest.finalize());
        if actual != expected_sha256 {
            let _ = unlink_at(&installers, &temporary);
            return Err(CapsuleError::InstallerChanged);
        }
        destination.sync_all()?;
        rename_at(&installers, &temporary, &installers, &name)?;
        let _ = installers.sync_all();
        Ok(PathBuf::from("drive_c/tilo-installers").join(name))
    }

    /// Reopen the exact installer retained by a portal-managed capsule for an
    /// explicit VM escalation. Every directory is descriptor-anchored and the
    /// leaf is opened with `O_NOFOLLOW`; the caller must still re-inspect and
    /// digest-bind the bytes before streaming them to the guest.
    pub(crate) fn open_retained_installer(
        &self,
        capsule_id: &str,
        installation: &InstallationRecord,
    ) -> Result<File, CapsuleError> {
        validate_id("capsule id", capsule_id)?;
        installation.validate()?;
        if installation.installer_relative_path.parent()
            != Some(Path::new("drive_c/tilo-installers"))
        {
            return Err(CapsuleError::InvalidInstallation);
        }
        let name = installation
            .installer_relative_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(CapsuleError::InvalidInstallation)?;
        let capsule = self.capsule_directory(capsule_id, false)?;
        let data = Self::open_child_dir(&capsule, "data", false)?;
        let prefix = Self::open_child_dir(&data, "prefix", false)?;
        let drive_c = Self::open_child_dir(&prefix, "drive_c", false)?;
        let installers = Self::open_child_dir(&drive_c, "tilo-installers", false)?;
        Ok(open_regular_nofollow(&installers, name)?)
    }

    /// Find user-facing PE executables after setup without following symlinks
    /// or trusting generated command strings. The scan is intentionally
    /// bounded; a hostile installer cannot turn refresh into unbounded work.
    pub(crate) fn discover_programs(
        &self,
        capsule_id: &str,
        query: &ApplicationQuery,
    ) -> Result<(Vec<CapsuleProgram>, Option<PathBuf>), CapsuleError> {
        const MAX_ENTRIES: usize = 20_000;
        const MAX_PROGRAMS: usize = 64;
        const MAX_DEPTH: usize = 10;
        let paths = self.open_launch_paths(capsule_id)?;
        let prefix_root = paths.prefix_source();
        let mut pending = vec![(prefix_root.join("drive_c"), 0_usize)];
        let mut examined = 0_usize;
        let mut candidates = Vec::new();
        let mut uninstaller = None;
        while let Some((directory, depth)) = pending.pop() {
            if depth > MAX_DEPTH || examined >= MAX_ENTRIES {
                continue;
            }
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            for entry in entries {
                if examined >= MAX_ENTRIES {
                    break;
                }
                examined = examined.saturating_add(1);
                let entry = entry?;
                let file_type = entry.file_type()?;
                if file_type.is_dir() {
                    pending.push((entry.path(), depth.saturating_add(1)));
                    continue;
                }
                let entry_path = entry.path();
                if !file_type.is_file()
                    || !entry_path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
                {
                    continue;
                }
                let Ok(relative) = entry_path.strip_prefix(&prefix_root).map(Path::to_path_buf)
                else {
                    continue;
                };
                let stem = entry_path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or_default();
                let lower = stem.to_ascii_lowercase();
                if lower.starts_with("unins") || lower.contains("uninstall") {
                    if uninstaller.is_none() {
                        uninstaller = Some(relative);
                    }
                    continue;
                }
                let relative_lower = relative.to_string_lossy().to_ascii_lowercase();
                let installed_location = relative_lower.contains("program files")
                    || (relative_lower.contains("appdata") && relative_lower.contains("local"));
                if !installed_location
                    || relative_lower.contains("tilo-installers")
                    || relative_lower.starts_with("drive_c/windows/")
                    || is_helper_executable(&lower)
                    || candidates.len() >= MAX_PROGRAMS
                {
                    continue;
                }
                let id = program_id_from_relative(&relative);
                let mut program_query = query.clone();
                program_query.executable = entry_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string);
                program_query.executable_sha256 = None;
                program_query.installer_sha256 = None;
                candidates.push(CapsuleProgram {
                    id,
                    display_name: executable_display_name(stem),
                    prefix_relative_executable: relative,
                    query: program_query,
                    kind: ProgramKind::Application,
                    arguments: Vec::new(),
                });
            }
        }
        candidates.sort_by(|left, right| left.display_name.cmp(&right.display_name));
        candidates.dedup_by(|left, right| {
            left.prefix_relative_executable == right.prefix_relative_executable
        });
        Ok((candidates, uninstaller))
    }

    /// Atomically remove a capsule from the live namespace before recursive
    /// deletion. A crash leaves only a non-launchable trash entry, never a
    /// half-deleted live application.
    pub(crate) fn retire_capsule(&self, capsule_id: &str) -> Result<(), CapsuleError> {
        validate_id("capsule id", capsule_id)?;
        let capsules = Self::open_child_dir(self.root_fd.as_ref(), "capsules", false)?;
        let trash = Self::open_child_dir(self.root_fd.as_ref(), "trash", true)?;
        let retired = format!(
            "{capsule_id}-{}",
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        rename_at(&capsules, capsule_id, &trash, &retired)?;
        let _ = capsules.sync_all();
        let _ = trash.sync_all();
        fs::remove_dir_all(fd_path(&trash).join(retired))?;
        let _ = trash.sync_all();
        Ok(())
    }

    pub fn save(&self, state: &CapsuleState) -> Result<(), CapsuleError> {
        state.validate()?;
        let directory = self.capsule_directory(&state.id, true)?;
        let base = fd_path(&directory);
        let temp = base.join(format!(
            ".state.{}.{}.tmp",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let path = base.join("state.json");
        let bytes = serde_json::to_vec_pretty(state)?;
        {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        if let Err(error) = fs::rename(&temp, &path) {
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
        let _ = directory.sync_all();
        Ok(())
    }

    #[must_use]
    pub fn prefix_path(&self, id: &str) -> PathBuf {
        self.capsule_dir(id).join("data/prefix")
    }

    #[must_use]
    pub fn home_path(&self, id: &str) -> PathBuf {
        self.capsule_dir(id).join("data/home")
    }

    fn capsule_dir(&self, id: &str) -> PathBuf {
        self.root.join("capsules").join(id)
    }

    /// Open the prefix and home directories through anchored, no-follow
    /// descriptors. The returned descriptors are held by the command plan;
    /// the executor passes these handles directly to Bubblewrap rather than
    /// reopening attacker-replaceable pathnames.
    pub(crate) fn open_launch_paths(&self, id: &str) -> Result<LaunchPaths, CapsuleError> {
        let capsule = self.capsule_directory(id, false)?;
        let data = Self::open_child_dir(&capsule, "data", false)?;
        let prefix = Self::open_child_dir(&data, "prefix", false)?;
        let home = Self::open_child_dir(&data, "home", false)?;
        Ok(LaunchPaths { prefix, home })
    }

    fn capsule_directory(&self, id: &str, create: bool) -> Result<File, CapsuleError> {
        validate_id("capsule id", id)?;
        let capsules = Self::open_child_dir(self.root_fd.as_ref(), "capsules", create)?;
        Ok(Self::open_child_dir(&capsules, id, create)?)
    }

    fn open_child_dir(
        parent: &File,
        component: &str,
        create: bool,
    ) -> Result<File, std::io::Error> {
        match open_directory_at(parent, component) {
            Ok(directory) => Ok(directory),
            Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                mkdir_at(parent, component, 0o700)?;
                open_directory_at(parent, component)
            }
            Err(error) => Err(error),
        }
    }

    /// Reasserts the managed Windows drive shape on every launch. Wine's
    /// default `Z:` link would reveal the entire sandbox namespace. Removing
    /// that managed symlink, retaining the private `C:`, and exposing grants
    /// through `Q:` keeps system paths out of normal Windows file pickers.
    fn prepare_managed_paths(&self, id: &str) -> Result<(), CapsuleError> {
        let capsule = self.capsule_directory(id, true)?;
        let data = Self::open_child_dir(&capsule, "data", true)?;
        let prefix = Self::open_child_dir(&data, "prefix", true)?;
        let _drive_c = Self::open_child_dir(&prefix, "drive_c", true)?;
        let home = Self::open_child_dir(&data, "home", true)?;
        let dosdevices = Self::open_child_dir(&prefix, "dosdevices", true)?;
        let local = Self::open_child_dir(&home, ".local", true)?;
        let _cache = Self::open_child_dir(&home, ".cache", true)?;
        let _config = Self::open_child_dir(&home, ".config", true)?;
        let _share = Self::open_child_dir(&local, "share", true)?;

        // Precreating dosdevices for Q: prevents Wine's fresh-prefix branch
        // from creating its default C: link. Supply that invariant ourselves,
        // including for older prefixes, without accepting a redirected drive.
        let c = fd_path(&dosdevices).join("c:");
        match fs::symlink_metadata(&c) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    && fs::read_link(&c)? == Path::new("../drive_c") => {}
            Ok(_) => return Err(CapsuleError::ManagedDriveCollision("C:")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::os::unix::fs::symlink("../drive_c", c)?;
            }
            Err(error) => return Err(error.into()),
        }

        let z = fd_path(&dosdevices).join("z:");
        match fs::symlink_metadata(&z) {
            Ok(metadata) if metadata.file_type().is_symlink() => fs::remove_file(&z)?,
            Ok(_) => return Err(CapsuleError::ManagedDriveCollision("Z:")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let q = fd_path(&dosdevices).join("q:");
        match fs::read_link(&q) {
            Ok(target) if target == Path::new(SANDBOX_FILES_ROOT) => {}
            Ok(_) => return Err(CapsuleError::ManagedDriveCollision("Q:")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::os::unix::fs::symlink(SANDBOX_FILES_ROOT, q)?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
}

/// Anchored capsule mount sources. Keep the descriptors alive until Bubblewrap
/// has inherited and consumed the directory descriptors.
#[derive(Debug)]
pub(crate) struct LaunchPaths {
    prefix: File,
    home: File,
}

impl LaunchPaths {
    // Discovery runs in this process and can safely use its retained handle.
    fn prefix_source(&self) -> PathBuf {
        fd_path(&self.prefix)
    }

    pub(crate) fn directory_fds(&self) -> [std::os::fd::RawFd; 2] {
        [self.prefix.as_raw_fd(), self.home.as_raw_fd()]
    }
}

fn fd_path(file: &File) -> PathBuf {
    PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        file.as_raw_fd()
    ))
}

fn open_directory_nofollow(path: &Path) -> Result<File, CapsuleError> {
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| CapsuleError::UnsafeRoot)?;
    // SAFETY: `c_path` is NUL-terminated and valid for the duration of open(2).
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    file_from_fd(fd)
}

fn open_directory_at(parent: &File, component: &str) -> Result<File, std::io::Error> {
    let component = std::ffi::CString::new(component).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid path component")
    })?;
    // SAFETY: `parent` is an open directory and `component` is a valid
    // NUL-terminated single path component validated by the caller.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            component.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    file_from_fd_io(fd)
}

fn mkdir_at(parent: &File, component: &str, mode: u32) -> Result<(), std::io::Error> {
    let component = std::ffi::CString::new(component).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid path component")
    })?;
    // SAFETY: `parent` is an open directory and `component` is a valid
    // NUL-terminated single path component.
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), mode) };
    if result == 0 {
        Ok(())
    } else {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            Ok(())
        } else {
            Err(error)
        }
    }
}

fn open_regular_nofollow(parent: &File, component: &str) -> Result<File, std::io::Error> {
    let component = std::ffi::CString::new(component)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid file name"))?;
    // SAFETY: `parent` is an open directory and `component` is a valid
    // NUL-terminated single filename.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            component.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    let file = file_from_fd_io(fd)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("capsule state is not a regular file"));
    }
    Ok(file)
}

fn create_regular_exclusive(
    parent: &File,
    component: &str,
    mode: u32,
) -> Result<File, std::io::Error> {
    let component = std::ffi::CString::new(component)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid file name"))?;
    // SAFETY: the descriptor names a retained directory and `component` is a
    // single NUL-free file name generated by the service.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            component.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            mode,
        )
    };
    file_from_fd_io(fd)
}

fn rename_at(
    source_parent: &File,
    source: &str,
    destination_parent: &File,
    destination: &str,
) -> Result<(), std::io::Error> {
    let source = std::ffi::CString::new(source)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid source"))?;
    let destination = std::ffi::CString::new(destination).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid destination")
    })?;
    // SAFETY: both descriptors name retained directories and the component
    // strings remain alive for the syscall.
    let result = unsafe {
        libc::renameat(
            source_parent.as_raw_fd(),
            source.as_ptr(),
            destination_parent.as_raw_fd(),
            destination.as_ptr(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn unlink_at(parent: &File, component: &str) -> Result<(), std::io::Error> {
    let component = std::ffi::CString::new(component)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid file name"))?;
    // SAFETY: `parent` names a retained directory and `component` is valid.
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), component.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn file_from_fd(fd: i32) -> Result<File, CapsuleError> {
    file_from_fd_io(fd).map_err(Into::into)
}

fn file_from_fd_io(fd: i32) -> Result<File, std::io::Error> {
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: `fd` is a newly returned, uniquely owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredProgram {
    pub entry_id: String,
    /// Canonical Tilo capability subject derived by the service, never a client.
    pub app_id: String,
    pub capsule_id: String,
    pub program: CapsuleProgram,
    pub query: ApplicationQuery,
}

fn managed_program_id(capsule_id: &str, program_id: &str) -> String {
    format!("windows-{}", identity_digest(capsule_id, program_id))
}

fn managed_app_id(capsule_id: &str, program_id: &str) -> String {
    format!(
        "so.tilo.windows.{}",
        identity_digest(capsule_id, program_id)
    )
}

fn identity_digest(capsule_id: &str, program_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(capsule_id.as_bytes());
    digest.update([0]);
    digest.update(program_id.as_bytes());
    // The identity is the first 16 digest bytes, i.e. 32 hex digits.
    let mut identity = hex::encode(digest.finalize());
    identity.truncate(32);
    identity
}

fn program_id_from_relative(relative: &Path) -> String {
    let digest = Sha256::digest(relative.as_os_str().as_encoded_bytes());
    // The first 12 digest bytes, i.e. 24 hex digits.
    let mut suffix = hex::encode(digest);
    suffix.truncate(24);
    format!("app-{suffix}")
}

fn executable_display_name(stem: &str) -> String {
    let words = stem
        .replace(['_', '-'], " ")
        .split_whitespace()
        .map(|word| {
            let mut characters = word.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(characters).collect()
            })
        })
        .collect::<Vec<_>>();
    let display = words.join(" ");
    if display.is_empty() {
        "Windows application".to_string()
    } else {
        display
    }
}

fn is_helper_executable(lower_stem: &str) -> bool {
    [
        "crash",
        "report",
        "update",
        "updater",
        "setup",
        "install",
        "helper",
        "service",
        "redist",
        "vc_redist",
        "dxsetup",
    ]
    .iter()
    .any(|word| lower_stem == *word || lower_stem.ends_with(word))
}

pub(crate) fn validate_id(label: &'static str, value: &str) -> Result<(), CapsuleError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || value.chars().all(|character| character == '.')
    {
        return Err(CapsuleError::InvalidId(label));
    }
    Ok(())
}

fn validate_relative_executable(path: &Path) -> Result<(), CapsuleError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("exe"))
    {
        return Err(CapsuleError::InvalidExecutable);
    }
    Ok(())
}

fn validate_relative_installer(path: &Path, kind: InstallerKind) -> Result<(), CapsuleError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(CapsuleError::InvalidInstallation);
    }
    let extension = path.extension().and_then(|extension| extension.to_str());
    let expected = match kind {
        InstallerKind::Exe => "exe",
        InstallerKind::Msi => "msi",
    };
    if !extension.is_some_and(|extension| extension.eq_ignore_ascii_case(expected)) {
        return Err(CapsuleError::InvalidInstallation);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum CapsuleError {
    #[error("capsule store root must be an absolute, non-root daemon path")]
    UnsafeRoot,
    #[error("invalid {0}")]
    InvalidId(&'static str),
    #[error("capsule schema {0} is unsupported")]
    UnsupportedSchema(u16),
    #[error("VM routes cannot be represented by capsule state")]
    InvalidLayer,
    #[error("program display name is empty or contains control characters")]
    InvalidProgramName,
    #[error("program executable must be a relative .exe path inside the prefix")]
    InvalidExecutable,
    #[error("program arguments are empty, oversized, or contain controls")]
    InvalidArguments,
    #[error("installation journal is invalid")]
    InvalidInstallation,
    #[error("installer exceeds the bounded import limit")]
    InstallerTooLarge,
    #[error("the selected installer changed while it was being imported")]
    InstallerChanged,
    #[error("program map key does not match its stable id")]
    ProgramKeyMismatch,
    #[error("capsule state file contains a different identity")]
    StateIdentityMismatch,
    #[error("capsule {0:?} does not exist")]
    NotFound(String),
    #[error("capsule {0:?} needs an explicit recipe migration")]
    RecipeChanged(String),
    #[error("managed drive {0} was replaced inside the prefix")]
    ManagedDriveCollision(&'static str),
    #[error("capsule storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("capsule state is invalid: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use compat_catalogue::{Architecture, MatchKind, Runtime, SecurityProfileRef};
    use std::io::{SeekFrom, Write};
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    use super::*;

    fn plan() -> LaunchPlan {
        LaunchPlan {
            record_id: Some("example".to_string()),
            match_kind: MatchKind::Executable,
            runtime: Runtime {
                id: "example".to_string(),
                layer: Layer::Wine,
            },
            runner: Runner::new("wine", "11.0").unwrap(),
            settings: RunnerSettings {
                architecture: Architecture::Win64,
                ..RunnerSettings::default()
            },
            security_profile: SecurityProfileRef::new("windows-app").unwrap(),
            proton_recipe: None,
            compatibility_grade: None,
        }
    }

    #[test]
    fn prefix_state_and_c_mappings_are_managed_without_following_links() {
        // Phase `state_and_prefix_are_separate_and_q_is_managed`.
        {
            let directory = TempDir::new().unwrap();
            let store = CapsuleStore::new(directory.path()).unwrap();
            let state = store.ensure_for_plan(&plan()).unwrap();
            assert!(store.load(&state.id).unwrap().is_some());
            assert!(store.prefix_path(&state.id).join("drive_c").is_dir());
            assert_eq!(
                fs::read_link(store.prefix_path(&state.id).join("dosdevices/c:")).unwrap(),
                PathBuf::from("../drive_c")
            );
            assert_eq!(
                fs::read_link(store.prefix_path(&state.id).join("dosdevices/q:")).unwrap(),
                PathBuf::from(SANDBOX_FILES_ROOT)
            );
            assert!(!store.prefix_path(&state.id).join("state.json").exists());
        }
        // Phase `missing_c_mapping_is_repaired_without_changing_prefix_content`.
        {
            let directory = TempDir::new().unwrap();
            let store = CapsuleStore::new(directory.path()).unwrap();
            let state = store.ensure_for_plan(&plan()).unwrap();
            let prefix = store.prefix_path(&state.id);
            fs::write(prefix.join("drive_c/document.txt"), "retained").unwrap();
            fs::remove_file(prefix.join("dosdevices/c:")).unwrap();
            store.ensure_for_plan(&plan()).unwrap();
            assert_eq!(
                fs::read_link(prefix.join("dosdevices/c:")).unwrap(),
                PathBuf::from("../drive_c")
            );
            assert_eq!(
                fs::read_to_string(prefix.join("drive_c/document.txt")).unwrap(),
                "retained"
            );
        }
        // Phase `replaced_c_mapping_is_rejected_without_overwriting_it`.
        {
            for replacement in ["symlink", "file", "directory"] {
                let directory = TempDir::new().unwrap();
                let store = CapsuleStore::new(directory.path()).unwrap();
                let state = store.ensure_for_plan(&plan()).unwrap();
                let mapping = store.prefix_path(&state.id).join("dosdevices/c:");
                fs::remove_file(&mapping).unwrap();
                match replacement {
                    "symlink" => symlink("/outside", &mapping).unwrap(),
                    "file" => fs::write(&mapping, "retained").unwrap(),
                    _ => fs::create_dir(&mapping).unwrap(),
                }
                assert!(matches!(
                    store.ensure_for_plan(&plan()),
                    Err(CapsuleError::ManagedDriveCollision("C:"))
                ));
                match replacement {
                    "symlink" => {
                        assert_eq!(fs::read_link(&mapping).unwrap(), Path::new("/outside"));
                    }
                    "file" => assert_eq!(fs::read_to_string(&mapping).unwrap(), "retained"),
                    _ => assert!(mapping.is_dir()),
                }
            }
        }
        // Phase `symlinked_c_directory_is_rejected_without_touching_its_target`.
        {
            let directory = TempDir::new().unwrap();
            let outside = TempDir::new().unwrap();
            fs::write(outside.path().join("value"), "retained").unwrap();
            let store = CapsuleStore::new(directory.path()).unwrap();
            let state = store.ensure_for_plan(&plan()).unwrap();
            let drive = store.prefix_path(&state.id).join("drive_c");
            fs::remove_dir(&drive).unwrap();
            symlink(outside.path(), &drive).unwrap();
            assert!(store.ensure_for_plan(&plan()).is_err());
            assert_eq!(
                fs::read_to_string(outside.path().join("value")).unwrap(),
                "retained"
            );
            assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
        }
        // Phase `executable_cannot_escape_prefix`.
        {
            let program = CapsuleProgram {
                id: "bad".to_string(),
                display_name: "Bad".to_string(),
                prefix_relative_executable: PathBuf::from("../host.exe"),
                query: ApplicationQuery::default(),
                kind: ProgramKind::Application,
                arguments: Vec::new(),
            };
            assert!(matches!(
                program.validate(),
                Err(CapsuleError::InvalidExecutable)
            ));
        }
    }

    #[test]
    fn capsule_state_saves_migrations_and_installers_are_exact() {
        // Phase `recipe_changes_need_explicit_migration`.
        {
            let directory = TempDir::new().unwrap();
            let store = CapsuleStore::new(directory.path()).unwrap();
            store.ensure_for_plan(&plan()).unwrap();
            let mut changed = plan();
            changed.runner = Runner::new("wine", "12.0").unwrap();
            assert!(matches!(
                store.ensure_for_plan(&changed),
                Err(CapsuleError::RecipeChanged(_))
            ));
        }
        // Phase `symlinked_capsule_directory_is_rejected_before_state_or_prefix_access`.
        {
            let root = TempDir::new().unwrap();
            let outside = TempDir::new().unwrap();
            let store = CapsuleStore::new(root.path()).unwrap();
            fs::create_dir(root.path().join("capsules")).unwrap();
            symlink(outside.path(), root.path().join("capsules/example")).unwrap();

            assert!(store.load("example").is_err());
            assert!(store.ensure_for_plan(&plan()).is_err());
            assert!(
                !outside.path().join("data/prefix").exists(),
                "managed path preparation must not follow the attacker symlink"
            );
        }
        // Phase `saving_replaces_a_state_symlink_without_touching_its_target`.
        {
            let root = TempDir::new().unwrap();
            let outside = root.path().join("outside.json");
            let store = CapsuleStore::new(root.path()).unwrap();
            let state = store.ensure_for_plan(&plan()).unwrap();
            fs::write(&outside, "outside stays intact").unwrap();
            let state_path = store.capsule_dir(&state.id).join("state.json");
            fs::remove_file(&state_path).unwrap();
            symlink(&outside, &state_path).unwrap();

            store.save(&state).unwrap();
            assert_eq!(
                fs::read_to_string(&outside).unwrap(),
                "outside stays intact"
            );
            assert!(
                fs::symlink_metadata(&state_path)
                    .unwrap()
                    .file_type()
                    .is_file()
            );
            assert!(store.load(&state.id).unwrap().is_some());
        }
        // Phase `staged_installer_must_match_the_bytes_that_were_inspected`.
        {
            let directory = TempDir::new().unwrap();
            let store = CapsuleStore::new(directory.path()).unwrap();
            let state = store.ensure_for_plan(&plan()).unwrap();
            let mut selected = tempfile::NamedTempFile::new().unwrap();
            let mut pe = vec![0_u8; 0x200];
            pe[..2].copy_from_slice(b"MZ");
            pe[0x3c..0x40].copy_from_slice(&0x80_u32.to_le_bytes());
            pe[0x80..0x84].copy_from_slice(b"PE\0\0");
            pe[0x84..0x86].copy_from_slice(&0x8664_u16.to_le_bytes());
            selected.write_all(&pe).unwrap();
            selected.flush().unwrap();
            let mut source = selected.reopen().unwrap();
            let inspected = crate::inspect_installer(&mut source, "favorite.exe").unwrap();

            selected.as_file_mut().seek(SeekFrom::Start(0x100)).unwrap();
            selected.as_file_mut().write_all(b"changed").unwrap();
            selected.as_file_mut().flush().unwrap();
            assert!(matches!(
                store.copy_installer(
                    &state.id,
                    &mut source,
                    &inspected.sha256,
                    InstallerKind::Exe,
                    crate::MAX_INSTALLER_BYTES,
                ),
                Err(CapsuleError::InstallerChanged)
            ));
        }
    }
}

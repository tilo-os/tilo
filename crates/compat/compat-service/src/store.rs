use std::collections::BTreeMap;
use std::fs;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use compat_catalogue::{
    ApplicationQuery, LaunchPlan, Layer, Runner, RunnerSettings, SecurityProfileRef,
};
use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const CAPSULE_SCHEMA_VERSION: u16 = 1;
const SANDBOX_FILES_ROOT: &str = "/run/tilo/files";
/// What the capsule's W: drive shows: the Wine runtime as the capsule sees it, never the
/// person's files. Setting a prefix up needs a drive for it: Wine finds the shipped Wine Mono
/// only through a drive (without one it asks to download it, and that question holds the setup
/// until Wine gives up waiting), and the setup's 32-bit helpers load their DLLs from the runtime
/// through a drive before the prefix has its own copies.
const RUNTIME_DRIVE_ROOT: &str = "/usr";
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
    pub id: String,
    pub display_name: String,
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
    /// How a person chose to open this program, when not as its capsule opens: Wine or Proton,
    /// over the same files. Only an administrator of the computer chooses it.
    #[serde(default)]
    pub opens_with: Option<Layer>,
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
        if self.opens_with == Some(Layer::Vm)
            || (self.opens_with.is_some() && self.kind != ProgramKind::Application)
        {
            return Err(CapsuleError::InvalidOpensWith);
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

    /// Open `program_id` of `capsule_id` with `layer` from now on, or as its capsule opens
    /// (`None`). Its files stay where they are: Wine and Proton both open the capsule's prefix.
    pub fn set_opens_with(
        &self,
        capsule_id: &str,
        program_id: &str,
        layer: Option<Layer>,
    ) -> Result<CapsuleState, CapsuleError> {
        let mut state = self
            .load(capsule_id)?
            .ok_or_else(|| CapsuleError::NotFound(capsule_id.to_string()))?;
        let program = state
            .programs
            .get_mut(program_id)
            .ok_or_else(|| CapsuleError::NotFound(program_id.to_string()))?;
        program.opens_with = layer.filter(|layer| *layer != state.layer);
        program.validate()?;
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
        const MAX_SHORTCUTS: usize = 128;
        let paths = self.open_launch_paths(capsule_id)?;
        let prefix_root = paths.prefix_source();
        let mut pending = vec![(prefix_root.join("drive_c"), 0_usize)];
        let mut examined = 0_usize;
        let mut candidates = Vec::new();
        let mut shortcuts = Vec::new();
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
                let extension = entry_path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .unwrap_or_default();
                if !file_type.is_file() {
                    continue;
                }
                let Ok(relative) = entry_path.strip_prefix(&prefix_root).map(Path::to_path_buf)
                else {
                    continue;
                };
                // What Windows itself shows a person: the Start menu's shortcuts. Each names the
                // program its setup installed, and a program no shortcut names (an updater, a tool
                // an installer keeps for itself) is not one a person opens.
                if extension.eq_ignore_ascii_case("lnk") {
                    let relative_lower = relative.to_string_lossy().to_ascii_lowercase();
                    if relative_lower.contains("start menu") && shortcuts.len() < MAX_SHORTCUTS {
                        keep_shortcut(&entry_path, &prefix_root, &mut shortcuts);
                    }
                    continue;
                }
                if !extension.eq_ignore_ascii_case("exe") {
                    continue;
                }
                let stem = entry_path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or_default();
                let lower = stem.to_ascii_lowercase();
                let relative_lower = relative.to_string_lossy().to_ascii_lowercase();
                if lower.starts_with("unins") || lower.contains("uninstall") {
                    // Wine's own `C:\windows\system32\uninstaller.exe` is in every prefix; the
                    // uninstaller is the one setup put beside what it installed.
                    if uninstaller.is_none()
                        && !relative_lower.starts_with("drive_c/windows/")
                        && !is_wine_placeholder(&entry_path)
                    {
                        uninstaller = Some(relative);
                    }
                    continue;
                }
                if !is_application_location(&relative_lower)
                    || is_helper_executable(&lower)
                    || candidates.len() >= MAX_PROGRAMS
                    || is_wine_placeholder(&entry_path)
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
                    opens_with: None,
                });
            }
        }
        let mut listed = listed_by_shortcuts(candidates, &shortcuts);
        listed.sort_by(|left, right| left.display_name.cmp(&right.display_name));
        listed.dedup_by(|left, right| {
            left.prefix_relative_executable == right.prefix_relative_executable
        });
        Ok((listed, uninstaller))
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
        let path = fd_path(&directory).join("state.json");
        let bytes = serde_json::to_vec_pretty(state)?;
        tilo_files::native::atomic::write_with_mode(&path, &bytes, 0o600)?;
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

        for (drive, letter, root) in [
            ("q:", "Q:", SANDBOX_FILES_ROOT),
            ("w:", "W:", RUNTIME_DRIVE_ROOT),
        ] {
            let link = fd_path(&dosdevices).join(drive);
            match fs::read_link(&link) {
                Ok(target) if target == Path::new(root) => {}
                Ok(_) => return Err(CapsuleError::ManagedDriveCollision(letter)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::os::unix::fs::symlink(root, link)?;
                }
                Err(error) => return Err(error.into()),
            }
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
    fn prefix_source(&self) -> PathBuf {
        fd_path(&self.prefix)
    }

    pub(crate) fn directory_fds(&self) -> [BorrowedFd<'_>; 2] {
        [self.prefix.as_fd(), self.home.as_fd()]
    }
}

fn fd_path(file: &File) -> PathBuf {
    PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        file.as_raw_fd()
    ))
}

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);

fn open_directory_nofollow(path: &Path) -> Result<File, CapsuleError> {
    Ok(rustix::fs::open(path, DIRECTORY, Mode::empty())
        .map_err(std::io::Error::from)?
        .into())
}

fn open_directory_at(parent: &File, component: &str) -> Result<File, std::io::Error> {
    Ok(rustix::fs::openat(parent, component, DIRECTORY, Mode::empty())?.into())
}

fn mkdir_at(parent: &File, component: &str, mode: u32) -> Result<(), std::io::Error> {
    match rustix::fs::mkdirat(parent, component, Mode::from_raw_mode(mode)) {
        Ok(()) | Err(Errno::EXIST) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Lay a prefix out the way UMU lays it out before Proton opens it as its data directory: `pfx`
/// names the prefix itself, Proton's shader cache and file list have their places, and the
/// Windows user Proton signs in as (`steamuser`) and the capsule's own (`tilo`, whom Wine set the
/// prefix up for) are one folder, so a program finds the settings it saved under either. The
/// prefix holds what its programs wrote: nothing in it is followed.
pub(crate) fn prepare_for_proton(prefix: BorrowedFd<'_>) -> Result<(), CapsuleError> {
    let prefix = File::from(prefix.try_clone_to_owned()?);
    match rustix::fs::readlinkat(&prefix, "pfx", Vec::new()) {
        Ok(target) if target.as_bytes() == b"." => {}
        Ok(_) => {
            rustix::fs::unlinkat(&prefix, "pfx", AtFlags::empty()).map_err(std::io::Error::from)?;
            rustix::fs::symlinkat(".", &prefix, "pfx").map_err(std::io::Error::from)?;
        }
        Err(Errno::NOENT) => {
            rustix::fs::symlinkat(".", &prefix, "pfx").map_err(std::io::Error::from)?;
        }
        Err(error) => return Err(std::io::Error::from(error).into()),
    }
    mkdir_at(&prefix, "shadercache", 0o700)?;
    drop(
        rustix::fs::openat(
            &prefix,
            "tracked_files",
            OFlags::WRONLY | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )
        .map_err(std::io::Error::from)?,
    );
    mkdir_at(&prefix, "drive_c", 0o755)?;
    let drive = open_directory_at(&prefix, "drive_c")?;
    mkdir_at(&drive, "users", 0o755)?;
    let users = open_directory_at(&drive, "users")?;
    let kind = |name: &str| {
        rustix::fs::statat(&users, name, AtFlags::SYMLINK_NOFOLLOW)
            .map(|stat| rustix::fs::FileType::from_raw_mode(stat.st_mode))
    };
    match (kind("tilo"), kind("steamuser")) {
        (Err(Errno::NOENT), Err(Errno::NOENT)) => {
            mkdir_at(&users, "steamuser", 0o755)?;
            rustix::fs::symlinkat("steamuser", &users, "tilo").map_err(std::io::Error::from)?;
        }
        (Ok(rustix::fs::FileType::Directory), Err(Errno::NOENT)) => {
            rustix::fs::symlinkat("tilo", &users, "steamuser").map_err(std::io::Error::from)?;
        }
        (Err(Errno::NOENT), Ok(rustix::fs::FileType::Directory)) => {
            rustix::fs::symlinkat("steamuser", &users, "tilo").map_err(std::io::Error::from)?;
        }
        _ => {}
    }
    Ok(())
}

fn open_regular_nofollow(parent: &File, component: &str) -> Result<File, std::io::Error> {
    let file: File = rustix::fs::openat(
        parent,
        component,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?
    .into();
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
    Ok(rustix::fs::openat(
        parent,
        component,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(mode),
    )?
    .into())
}

fn rename_at(
    source_parent: &File,
    source: &str,
    destination_parent: &File,
    destination: &str,
) -> Result<(), std::io::Error> {
    Ok(rustix::fs::renameat(
        source_parent,
        source,
        destination_parent,
        destination,
    )?)
}

fn unlink_at(parent: &File, component: &str) -> Result<(), std::io::Error> {
    Ok(rustix::fs::unlinkat(parent, component, AtFlags::empty())?)
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

pub(crate) fn managed_program_id(capsule_id: &str, program_id: &str) -> String {
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

/// Whether `path` is one of the stand-ins Wine writes into every prefix it sets up (Internet
/// Explorer, Windows Media Player, `WordPad` under `Program Files`): they belong to Wine, not to
/// what the person installed, and listing them made every installed app arrive with three
/// strangers. Wine marks them in the DOS stub, right after the 64-byte header.
fn is_wine_placeholder(path: &Path) -> bool {
    const MARKS: [&[u8]; 2] = [b"Wine builtin DLL", b"Wine placeholder DLL"];
    let mut head = Vec::with_capacity(128);
    let Ok(file) = File::open(path) else {
        return false;
    };
    if file.take(128).read_to_end(&mut head).is_err() {
        return false;
    }
    MARKS
        .iter()
        .any(|mark| head.windows(mark.len()).any(|window| window == *mark))
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

/// Keep the Start menu shortcut at `path` as what it opens: its name, and the path its link
/// information gives under the capsule's prefix, lowercased for matching as Windows does.
fn keep_shortcut(path: &Path, prefix: &Path, shortcuts: &mut Vec<(String, Option<String>)>) {
    let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
        return;
    };
    let target = shortcut_target(path)
        .and_then(|target| prefix_relative(prefix, &target))
        .map(|relative| relative.to_string_lossy().to_ascii_lowercase());
    shortcuts.push((name.to_owned(), target));
}

/// What the shortcuts open, as the applications a person has: each candidate whose executable a
/// shortcut targets — or, for one an MSI advertises with no path, whose name matches the
/// shortcut's — named as its shortcut shows.
fn listed_by_shortcuts(
    candidates: Vec<CapsuleProgram>,
    shortcuts: &[(String, Option<String>)],
) -> Vec<CapsuleProgram> {
    candidates
        .into_iter()
        .filter_map(|mut program| {
            let executable = program
                .prefix_relative_executable
                .to_string_lossy()
                .to_ascii_lowercase();
            let stem = program
                .prefix_relative_executable
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let shortcut = shortcuts
                .iter()
                .find(|(_, target)| target.as_deref() == Some(executable.as_str()))
                .or_else(|| {
                    shortcuts
                        .iter()
                        .find(|(name, _)| name.to_ascii_lowercase() == stem)
                })?;
            program.display_name.clone_from(&shortcut.0);
            Some(program)
        })
        .collect()
}

/// The path the Start menu shortcut at `path` names, as Windows wrote it: its local base path and
/// the common suffix beside it. A shortcut an MSI advertises carries no path — its target lives in
/// the installer — and answers `None`.
fn shortcut_target(path: &Path) -> Option<String> {
    const MAX_LNK: u64 = 64 * 1024;
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_LNK)
        .read_to_end(&mut bytes)
        .ok()?;
    shell_link_target(&bytes)
}

/// [`shortcut_target`] over the bytes of a shell link.
fn shell_link_target(bytes: &[u8]) -> Option<String> {
    let word = |at: usize| -> Option<usize> {
        let value = u32::from_le_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?);
        usize::try_from(value).ok()
    };
    if bytes.get(0..4)? != [0x4c, 0, 0, 0] {
        return None;
    }
    // `HasLinkTargetIDList` and `HasLinkInfo`.
    let flags = word(0x14)?;
    let mut cursor: usize = 0x4c;
    if flags & 0x1 != 0 {
        let size = usize::from(u16::from_le_bytes(
            bytes.get(cursor..cursor.checked_add(2)?)?.try_into().ok()?,
        ));
        cursor = cursor.checked_add(size.checked_add(2)?)?;
    }
    if flags & 0x2 == 0 {
        return None;
    }
    let start = cursor;
    let size = word(start)?;
    let end = start.checked_add(size)?.min(bytes.len());
    let header = word(start.checked_add(4)?)?;
    // `VolumeIDAndLocalBasePath`: without it the target is on a network volume or in the installer.
    if word(start.checked_add(8)?)? & 0x1 == 0 {
        return None;
    }
    let base = word(start.checked_add(16)?)?;
    let suffix = word(start.checked_add(24)?)?;
    let (base, suffix) = if header >= 0x24 && word(start.checked_add(28)?).is_some_and(|at| at != 0)
    {
        (
            (word(start.checked_add(28)?)?, true),
            (word(start.checked_add(32)?)?, true),
        )
    } else {
        ((base, false), (suffix, false))
    };
    let read = |(offset, unicode): (usize, bool)| -> Option<String> {
        let field = bytes.get(start.checked_add(offset)?..end)?;
        if unicode {
            let units: Vec<u16> = field
                .chunks_exact(2)
                .map_while(|pair| Some(u16::from_le_bytes([*pair.first()?, *pair.get(1)?])))
                .take_while(|unit| *unit != 0)
                .collect();
            Some(String::from_utf16_lossy(&units))
        } else {
            let nul = field.iter().position(|byte| *byte == 0)?;
            Some(String::from_utf8_lossy(field.get(..nul)?).into_owned())
        }
    };
    let base = read(base)?;
    // A suffix offset of zero names no suffix, never the link information's own first bytes.
    let suffix = if suffix.0 == 0 {
        String::new()
    } else {
        read(suffix).unwrap_or_default()
    };
    let target = format!("{base}{suffix}");
    (!target.trim().is_empty()).then_some(target)
}

/// `target`, a Windows path a shortcut names, as its path under the capsule's `drive_c`: each
/// component matched as Windows matches it, ignoring case. `None` for another drive, a UNC path or
/// one that leaves the prefix.
fn prefix_relative(prefix: &Path, target: &str) -> Option<PathBuf> {
    let target = target.trim().trim_matches('"');
    let mut parts = target.split('\\');
    let drive = parts.next()?.trim_end_matches(':');
    if !drive.eq_ignore_ascii_case("c") {
        return None;
    }
    let mut path = prefix.join("drive_c");
    for part in parts {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        let exact = path.join(part);
        if exact.exists() {
            path = exact;
            continue;
        }
        path = fs::read_dir(&path)
            .ok()?
            .flatten()
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(part)
            })?
            .path();
    }
    path.strip_prefix(prefix).ok().map(Path::to_path_buf)
}

/// Whether a file's path under the prefix is where an application is installed: Program Files,
/// or the person's per-user `Local AppData`, and never Wine's own tree or the retained installers.
fn is_application_location(relative_lower: &str) -> bool {
    let installed = relative_lower.contains("program files")
        || (relative_lower.contains("appdata") && relative_lower.contains("local"));
    installed
        && !relative_lower.contains("tilo-installers")
        && !relative_lower.starts_with("drive_c/windows/")
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
    #[error("a program opens with Wine or Proton, and a setup only as its capsule does")]
    InvalidOpensWith,
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

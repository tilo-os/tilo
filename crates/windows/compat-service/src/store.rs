use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, symlink};
use std::path::{Component, Path, PathBuf};

use compat_catalogue::{
    ApplicationQuery, LaunchPlan, Layer, Runner, RunnerSettings, SecurityProfileRef,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const CAPSULE_SCHEMA_VERSION: u16 = 1;
const SANDBOX_FILES_ROOT: &str = "/run/qos/files";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapsuleProgram {
    /// Stable opaque identifier used by qOS search and launch actions.
    pub id: String,
    pub display_name: String,
    /// Relative to the prefix root, normally `drive_c/Program Files/...`.
    pub prefix_relative_executable: PathBuf,
    /// Identity metadata captured by the trusted installer/importer. Live
    /// clients select this program by opaque ID and cannot replace this query.
    #[serde(default)]
    pub query: ApplicationQuery,
}

impl CapsuleProgram {
    pub fn validate(&self) -> Result<(), CapsuleError> {
        validate_id("program id", &self.id)?;
        if self.display_name.trim().is_empty() || self.display_name.chars().any(char::is_control) {
            return Err(CapsuleError::InvalidProgramName);
        }
        validate_relative_executable(&self.prefix_relative_executable)
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
}

impl CapsuleStore {
    /// `root` is daemon configuration, never catalogue or socket input.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, CapsuleError> {
        let root = root.into();
        if !root.is_absolute() || root == Path::new("/") {
            return Err(CapsuleError::UnsafeRoot);
        }
        Ok(Self { root })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn load(&self, id: &str) -> Result<Option<CapsuleState>, CapsuleError> {
        validate_id("capsule id", id)?;
        let path = self.state_path(id);
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
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
        let capsules = self.root.join("capsules");
        let entries = match fs::read_dir(capsules) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
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

    pub fn save(&self, state: &CapsuleState) -> Result<(), CapsuleError> {
        state.validate()?;
        let directory = self.capsule_dir(&state.id);
        fs::create_dir_all(&directory)?;
        let path = directory.join("state.json");
        let temp = directory.join(format!(".state.{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(state)?;
        {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        if let Err(error) = fs::rename(&temp, &path) {
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
        if let Ok(dir) = fs::File::open(&directory) {
            let _ = dir.sync_all();
        }
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

    fn state_path(&self, id: &str) -> PathBuf {
        self.capsule_dir(id).join("state.json")
    }

    /// Reasserts the managed Windows drive shape on every launch. Wine's
    /// default `Z:` link would reveal the entire sandbox namespace. Removing
    /// that managed symlink and exposing only `Q:` makes granted files legible
    /// and keeps system paths out of normal Windows file pickers.
    fn prepare_managed_paths(&self, id: &str) -> Result<(), CapsuleError> {
        let prefix = self.prefix_path(id);
        let home = self.home_path(id);
        fs::create_dir_all(prefix.join("dosdevices"))?;
        fs::create_dir_all(home.join(".cache"))?;
        fs::create_dir_all(home.join(".config"))?;
        fs::create_dir_all(home.join(".local/share"))?;

        let dosdevices = prefix.join("dosdevices");
        let z = dosdevices.join("z:");
        match fs::symlink_metadata(&z) {
            Ok(metadata) if metadata.file_type().is_symlink() => fs::remove_file(&z)?,
            Ok(_) => return Err(CapsuleError::ManagedDriveCollision("Z:")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let q = dosdevices.join("q:");
        match fs::read_link(&q) {
            Ok(target) if target == Path::new(SANDBOX_FILES_ROOT) => {}
            Ok(_) => return Err(CapsuleError::ManagedDriveCollision("Q:")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                symlink(SANDBOX_FILES_ROOT, q)?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredProgram {
    pub entry_id: String,
    /// Canonical qOS capability subject derived by the service, never a client.
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
        "dev.qos.windows.{}",
        identity_digest(capsule_id, program_id)
    )
}

fn identity_digest(capsule_id: &str, program_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(capsule_id.as_bytes());
    digest.update([0]);
    digest.update(program_id.as_bytes());
    hex_lower(digest.finalize().iter().take(16).copied())
}

fn hex_lower(bytes: impl IntoIterator<Item = u8>) -> String {
    let mut output = String::new();
    for byte in bytes {
        output.push(hex_digit(byte >> 4));
        output.push(hex_digit(byte & 0x0f));
    }
    output
}

fn hex_digit(value: u8) -> char {
    match value {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        10 => 'a',
        11 => 'b',
        12 => 'c',
        13 => 'd',
        14 => 'e',
        _ => 'f',
    }
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
    fn state_and_prefix_are_separate_and_q_is_managed() {
        let directory = TempDir::new().unwrap();
        let store = CapsuleStore::new(directory.path()).unwrap();
        let state = store.ensure_for_plan(&plan()).unwrap();
        assert!(store.load(&state.id).unwrap().is_some());
        assert_eq!(
            fs::read_link(store.prefix_path(&state.id).join("dosdevices/q:")).unwrap(),
            PathBuf::from(SANDBOX_FILES_ROOT)
        );
        assert!(!store.prefix_path(&state.id).join("state.json").exists());
    }

    #[test]
    fn executable_cannot_escape_prefix() {
        let program = CapsuleProgram {
            id: "bad".to_string(),
            display_name: "Bad".to_string(),
            prefix_relative_executable: PathBuf::from("../host.exe"),
            query: ApplicationQuery::default(),
        };
        assert!(matches!(
            program.validate(),
            Err(CapsuleError::InvalidExecutable)
        ));
    }

    #[test]
    fn recipe_changes_need_explicit_migration() {
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
}

//! The workspace an environment belongs to.
//!
//! A Terminal is opened in a workspace, and the environments it lists are that workspace's:
//! their records kept with it, their home the workspace's own files
//! ([`tilo_paths::workspace_tree`]), and no grant into another workspace's files. Who says
//! which workspace a request is for ([`scope_of`]): an app the launcher started names the
//! one it was launched in, written in the connection's handoff before the app holds it,
//! while the session's own process may name any. A request that names none is the primary
//! workspace's ([`WorkspaceId::primary`]), whose home is the device's whole files folder.
//!
//! Every environment's processes see two variables ([`WORKSPACE_ENV`], [`ENVIRONMENT_ENV`]);
//! [`environment_workspace`] answers the same question from the record.

use std::path::{Path, PathBuf};

use collab_types::caps::{CapOverride, Effect, Scope};
use tilo_environment::{Environment, is_record_key};

use crate::records::{RecordError, Records};
use crate::store::Store;

pub const PRIMARY: &str = "personal";

pub const WORKSPACE_ENV: &str = "TILO_WORKSPACE";

pub const ENVIRONMENT_ENV: &str = "TILO_ENVIRONMENT";

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    #[must_use]
    pub fn primary() -> Self {
        Self(PRIMARY.to_owned())
    }

    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw.map(str::trim) {
            None | Some("" | PRIMARY) => Ok(Self::primary()),
            Some(id) if tilo_paths::is_workspace_tree_id(id) => Ok(Self(id.to_owned())),
            Some(id) => Err(format!("{id:?} is not a workspace id")),
        }
    }

    pub fn of_record(environment: &Environment) -> Result<Self, String> {
        Self::parse(Some(&environment.workspace))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_primary(&self) -> bool {
        self.0 == PRIMARY
    }
}

impl std::fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An environment on this device: its workspace and its record's key there.
/// Record keys are a workspace's own (every workspace has a `default`), so
/// the pair is what names one environment; written `<workspace>/<record>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnvironmentId {
    workspace: WorkspaceId,
    record: String,
}

impl EnvironmentId {
    /// # Errors
    /// `record` is not a record key.
    pub fn new(workspace: WorkspaceId, record: &str) -> Result<Self, String> {
        if !is_record_key(record) {
            return Err(format!("{record:?} does not name an environment record"));
        }
        Ok(Self {
            workspace,
            record: record.to_owned(),
        })
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        let (workspace, record) = raw
            .split_once('/')
            .ok_or_else(|| format!("{raw:?} is not <workspace>/<record>"))?;
        if workspace.is_empty() {
            return Err(format!("{raw:?} names no workspace"));
        }
        Self::new(WorkspaceId::parse(Some(workspace))?, record)
    }

    #[must_use]
    pub fn workspace(&self) -> &WorkspaceId {
        &self.workspace
    }

    #[must_use]
    pub fn record(&self) -> &str {
        &self.record
    }
}

impl std::fmt::Display for EnvironmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.workspace, self.record)
    }
}

/// The workspace the environment `environment` belongs to, as its record
/// says: for `mcp-server` and anything else outside a session that has an
/// environment's id ([`ENVIRONMENT_ENV`]) and must know its workspace.
/// `store` is the runtime's store ([`Store::discover`]).
///
/// The record is read with the workspace the id names, and one there that names
/// another workspace is refused, so the answer is the record's and the id's at once.
pub fn environment_workspace(
    store: &Store,
    environment: &EnvironmentId,
) -> Result<WorkspaceId, RecordError> {
    let records = Records::of(&store.for_workspace(environment.workspace()));
    let record = records
        .load(environment.record())?
        .ok_or_else(|| RecordError::Missing(environment.to_string()))?;
    WorkspaceId::of_record(&record)
        .map(|named| {
            if named.is_primary() {
                environment.workspace().clone()
            } else {
                named
            }
        })
        .map_err(RecordError::Model)
}

pub fn scope_of(
    caller: &crate::host::Caller,
    launched: Option<&str>,
    asked: Option<&str>,
) -> Result<WorkspaceId, String> {
    let asked = asked.filter(|asked| !asked.trim().is_empty());
    match caller {
        crate::host::Caller::Session => WorkspaceId::parse(asked),
        crate::host::Caller::App(app) => {
            let launched = WorkspaceId::parse(launched)?;
            match asked
                .map(|asked| WorkspaceId::parse(Some(asked)))
                .transpose()?
            {
                Some(asked) if asked != launched => Err(format!(
                    "{app} was opened in workspace {launched}; its environments are that \
                     workspace's, not {asked}'s"
                )),
                _ => Ok(launched),
            }
        }
    }
}

/// What an environment of a workspace other than the primary one may be
/// granted of the device's files: its own workspace's tree, and nothing
/// else under the files folder — not the folder itself, which holds every
/// workspace's tree, nor any directory above it. Directories outside the
/// files folder are granted as before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confinement {
    files: Vec<PathBuf>,
    own: Vec<PathBuf>,
}

impl Confinement {
    /// The confinement of `workspace` (not the primary one) whose tree is
    /// under `files`. `None` for the primary workspace, which is the whole
    /// files folder.
    #[must_use]
    pub fn new(files: &Path, workspace: &WorkspaceId) -> Option<Self> {
        if workspace.is_primary() {
            return None;
        }
        let own = tilo_paths::workspace_tree(files, workspace.as_str())?;
        let both = |path: PathBuf| {
            let mut paths = vec![normalize(&path)];
            if let Ok(resolved) = path.canonicalize()
                && !paths.contains(&resolved)
            {
                paths.push(resolved);
            }
            paths
        };
        let mut own_paths = both(own);
        // The tree may not be made yet: resolve it through its files folder.
        if let Ok(files) = files.canonicalize() {
            let resolved = files
                .join(tilo_paths::WORKSPACE_TREES_DIR)
                .join(workspace.as_str());
            if !own_paths.contains(&resolved) {
                own_paths.push(resolved);
            }
        }
        Some(Self {
            files: both(files.to_path_buf()),
            own: own_paths,
        })
    }

    /// Whether `grant` stays within the confinement. Only directory grants
    /// that allow are asked about; a host grant or a restriction is never
    /// a way out.
    #[must_use]
    pub fn admits(&self, grant: &CapOverride) -> bool {
        let Scope::FsPath(path) = &grant.scope else {
            return true;
        };
        if !matches!(grant.effect, Effect::Grant) {
            return true;
        }
        self.admits_path(Path::new(path))
    }

    #[must_use]
    pub fn admits_path(&self, path: &Path) -> bool {
        let path = normalize(path);
        if self.own.iter().any(|own| path.starts_with(own)) {
            return true;
        }
        !self
            .files
            .iter()
            .any(|files| path.starts_with(files) || files.starts_with(&path))
    }

    /// Why a grant of `path` is refused.
    #[must_use]
    pub fn refusal(&self, path: &str) -> String {
        format!(
            "{path} is not this workspace's: an environment of a workspace is granted its \
             own files ({}) and nothing else of the device's files",
            self.own
                .first()
                .map_or_else(String::new, |own| own.display().to_string())
        )
    }
}

fn normalize(path: &Path) -> PathBuf {
    tilo_environment::files::normalize(path)
}

//! The workspace an environment belongs to.
//!
//! A Terminal is opened in a workspace, and the environments it lists and
//! opens are that workspace's: their records are kept in the workspace's
//! room, their home is the workspace's own files
//! ([`tilo_paths::workspace_tree`]), and an environment of one workspace is
//! granted nothing of another's files ([`Confinement`]). Two Terminals in two
//! workspaces open two environments, side by side, neither seeing the
//! other's files or records.
//!
//! Who says which workspace a request is for ([`scope_of`]):
//!
//! - an app the launcher started: the workspace it was launched in, which
//!   the launcher writes in the connection's handoff before the app holds
//!   it. The app cannot name another;
//! - the session's own process (the shell hosting Terminal), which holds
//!   the person's authority: the workspace its request names.
//!
//! A request that names none is the **primary workspace**'s
//! ([`WorkspaceId::primary`]): the room the launcher calls `personal`, where
//! every environment made before environments belonged to a workspace is
//! kept, and whose home is the device's whole files folder, as before.
//!
//! Every environment's processes see two variables ([`WORKSPACE_ENV`],
//! [`ENVIRONMENT_ENV`]); [`environment_workspace`] answers the same question
//! from the record.

use std::path::{Path, PathBuf};

use collab_types::caps::{CapOverride, Effect, Scope};
use tilo_environment::{Environment, is_record_key};

use crate::records::{RecordError, Records};
use crate::store::Store;

/// The primary workspace's id: the room an environment is kept in when no
/// workspace is named, as the launcher names a launch with no workspace.
pub const PRIMARY: &str = "personal";

/// Set in every environment's shell and guest: the id of the workspace it
/// belongs to ([`WorkspaceId`]; `personal` for the primary workspace).
pub const WORKSPACE_ENV: &str = "TILO_WORKSPACE";

/// Set in every environment's shell and guest: the environment's id on
/// this device, stable across its sessions ([`EnvironmentId`]):
/// `<workspace>/<record>`.
pub const ENVIRONMENT_ENV: &str = "TILO_ENVIRONMENT";

/// A workspace, by id: the primary workspace, or one of
/// [`tilo_paths::is_workspace_tree_id`]'s ids (a session's workspace ids are
/// UUIDs).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    /// The primary workspace ([`PRIMARY`]).
    #[must_use]
    pub fn primary() -> Self {
        Self(PRIMARY.to_owned())
    }

    /// The workspace `raw` names: the primary one for none, an empty id or
    /// `personal`.
    ///
    /// # Errors
    /// `raw` is not a workspace id.
    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw.map(str::trim) {
            None | Some("" | PRIMARY) => Ok(Self::primary()),
            Some(id) if tilo_paths::is_workspace_tree_id(id) => Ok(Self(id.to_owned())),
            Some(id) => Err(format!("{id:?} is not a workspace id")),
        }
    }

    /// The workspace `environment`'s record names: the primary one for a
    /// record made before environments belonged to a workspace.
    ///
    /// # Errors
    /// The record names something that is not a workspace id.
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

    /// The environment `raw` names, as [`ENVIRONMENT_ENV`] spells it.
    ///
    /// # Errors
    /// `raw` is not `<workspace>/<record>`.
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
/// says: for `agent-door` and anything else outside a session that has an
/// environment's id ([`ENVIRONMENT_ENV`]) and must know its workspace.
/// `store` is the runtime's store ([`Store::discover`]).
///
/// The record is read in the room of the workspace the id names, and a
/// record there that names another workspace is refused, so the answer is
/// the record's and the id's at once.
///
/// # Errors
/// There is no such environment, or its record cannot be read.
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

/// The workspace a request is for, from who asks (`caller`), the workspace
/// the launcher said the app was launched in (`launched`), and the one the
/// request names (`asked`). An app's is the one it was launched in, and an
/// app that names another is refused; the session's own process names the
/// one it wants.
///
/// # Errors
/// An id is malformed, or an app named a workspace it was not launched in.
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
    /// The device's files folder, as named and as resolved.
    files: Vec<PathBuf>,
    /// The workspace's own tree, as named and as resolved.
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

    /// Whether a grant of the directory `path` stays within the confinement.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Caller;

    const W1: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
    const W2: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";

    fn ws(id: &str) -> WorkspaceId {
        WorkspaceId::parse(Some(id)).unwrap()
    }

    #[test]
    fn a_workspace_id_is_one_component_and_none_is_the_primary_workspace() {
        for primary in [None, Some(""), Some(" "), Some("personal")] {
            assert!(
                WorkspaceId::parse(primary).unwrap().is_primary(),
                "{primary:?}"
            );
        }
        assert_eq!(ws(W1).as_str(), W1);
        for bad in ["..", ".", "a/b", "../personal", "a b", "x.y"] {
            assert!(WorkspaceId::parse(Some(bad)).is_err(), "{bad:?}");
        }
        let id = EnvironmentId::parse(&format!("{W1}/default")).unwrap();
        assert_eq!((id.workspace(), id.record()), (&ws(W1), "default"));
        assert_eq!(id.to_string(), format!("{W1}/default"));
        assert!(
            EnvironmentId::parse("personal/env-1")
                .unwrap()
                .workspace()
                .is_primary()
        );
        for bad in [
            "default",
            "/default",
            "w1/../x",
            &format!("{W1}/a/b"),
            "a.b/default",
        ] {
            assert!(EnvironmentId::parse(bad).is_err(), "{bad:?}");
        }
    }

    /// An app's workspace is the launcher's word, never its own; the
    /// session's process names the workspace it wants.
    #[test]
    fn an_app_is_held_to_the_workspace_it_was_launched_in() {
        let app = Caller::App("so.tilo.terminal".into());
        assert_eq!(scope_of(&app, Some(W1), None).unwrap(), ws(W1));
        assert_eq!(scope_of(&app, Some(W1), Some(W1)).unwrap(), ws(W1));
        assert_eq!(scope_of(&app, Some(W1), Some("")).unwrap(), ws(W1));
        let refused = scope_of(&app, Some(W1), Some(W2)).unwrap_err();
        assert!(refused.contains(W2), "{refused}");
        assert!(
            scope_of(&app, None, Some(W2)).is_err(),
            "an app launched in no workspace cannot name one"
        );
        assert!(scope_of(&app, None, None).unwrap().is_primary());
        assert!(scope_of(&app, Some("../x"), None).is_err());

        assert_eq!(scope_of(&Caller::Session, None, Some(W2)).unwrap(), ws(W2));
        assert!(scope_of(&Caller::Session, None, None).unwrap().is_primary());
        assert!(scope_of(&Caller::Session, None, Some("a/b")).is_err());
    }

    #[test]
    fn a_workspace_is_granted_its_own_tree_and_nothing_else_of_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let files = dir.path().join("files");
        std::fs::create_dir_all(&files).unwrap();
        assert!(Confinement::new(&files, &WorkspaceId::primary()).is_none());
        let confined = Confinement::new(&files, &ws(W1)).unwrap();
        let tree = |id: &str| files.join(".workspaces").join(id);

        assert!(confined.admits_path(&tree(W1)));
        assert!(confined.admits_path(&tree(W1).join("src")));
        assert!(!confined.admits_path(&tree(W2)));
        assert!(!confined.admits_path(&tree(W2).join("src")));
        assert!(!confined.admits_path(&tree(W1).join("../").join(W2)));
        assert!(!confined.admits_path(&files), "the folder holds every tree");
        assert!(!confined.admits_path(&files.join(".workspaces")));
        assert!(!confined.admits_path(&files.join("Documents")));
        assert!(!confined.admits_path(dir.path()), "above the files folder");
        assert!(!confined.admits_path(Path::new("/")));
        assert!(confined.admits_path(&dir.path().join("elsewhere")));

        // Through a link to the files folder, as `perm trust-dir` resolves.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&files, &link).unwrap();
        let through = Confinement::new(&link, &ws(W1)).unwrap();
        assert!(!through.admits_path(&files.join(".workspaces").join(W2)));
        assert!(through.admits_path(&files.join(".workspaces").join(W1)));
    }
}

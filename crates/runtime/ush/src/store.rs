//! `~/.local/share/ush/`: where the runtime keeps what is not the record.
//!
//! ```text
//! environments/<record>/home/   an environment's home
//! environments/<record>/var/    its /var/lib and /var/cache
//! workspaces/<id>/environments/ the same, for a workspace other than the primary one
//! sessions/<id>/                live session scratch: generated /etc entries,
//!                               the broker socket, an ephemeral home
//! audit/                        the broker's append-only log
//! ```
//!
//! The environment records themselves are Terminal's ([`crate::records`]);
//! [`Store::records_root`] says where they are. **The store is never bound into a guest**:
//! a guest sees its own home and `/var` at their guest paths, never the store around them,
//! so it cannot reach another environment's home or edit the record of what it asked for.

use std::path::{Path, PathBuf};

use crate::workspace::{Confinement, WorkspaceId};

pub const STORE_ENV: &str = "TILO_USH_HOME";

/// Where the environment records are, when not where [`Store::discover`]
/// would look. The supervisor hands both locations to the sessions it
/// starts, so a session always uses its supervisor's.
pub const RECORDS_ENV: &str = "TILO_USH_RECORDS";

/// Where granted directories' contents are, when not on this machine at the
/// paths the grants name: a grant of `/home/you/src` binds
/// `<grant root>/home/you/src` at `/home/you/src`. A relay sets it to the
/// member's own file area; unset, grants bind the machine's own paths.
pub const GRANT_ROOT_ENV: &str = "TILO_USH_GRANT_ROOT";

pub const WORKSPACE_ENV: &str = "TILO_USH_WORKSPACE";

pub const FILES_ENV: &str = "TILO_USH_FILES";

pub const WORKSPACE_ID_ENV: &str = "TILO_USH_WORKSPACE_ID";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    root: PathBuf,
    records: PathBuf,
    grant_root: Option<PathBuf>,
    workspace: Option<PathBuf>,
    files: Option<PathBuf>,
    workspace_id: WorkspaceId,
    data_home: Option<PathBuf>,
}

impl Store {
    pub fn discover() -> Self {
        let moved = std::env::var_os(STORE_ENV).is_some();
        let mut store = if let Some(raw) = std::env::var_os(STORE_ENV) {
            Self::at(PathBuf::from(raw))
        } else {
            // No `$HOME` and no `$XDG_DATA_HOME` is a broken session, but it is
            // not a reason to write into the current directory.
            let data =
                tilo_paths::data_dir().unwrap_or_else(tilo_paths::xdg::runtime_dir_or_default);
            Self {
                root: data.join("ush"),
                records: crate::records::terminal_data_home(&data, crate::workspace::PRIMARY),
                grant_root: None,
                workspace: None,
                files: None,
                workspace_id: WorkspaceId::primary(),
                data_home: Some(data),
            }
        };
        if let Some(raw) = std::env::var_os(RECORDS_ENV) {
            store.records = PathBuf::from(raw);
            store.data_home = None;
        }
        store.grant_root = std::env::var_os(GRANT_ROOT_ENV)
            .map(PathBuf::from)
            .filter(|root| root.is_absolute());
        store.workspace = match std::env::var_os(WORKSPACE_ENV) {
            Some(raw) => Some(PathBuf::from(raw)).filter(|dir| dir.is_absolute()),
            None if !moved => crate::images::synced_files(),
            None => None,
        };
        store.files = std::env::var_os(FILES_ENV)
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| store.workspace.clone());
        if let Some(raw) = std::env::var_os(WORKSPACE_ID_ENV) {
            match WorkspaceId::parse(raw.to_str()) {
                Ok(id) => store.workspace_id = id,
                Err(why) => tracing::warn!(%why, "ush: the session names no workspace"),
            }
        }
        store
    }

    pub fn at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            records: root.join("records"),
            root,
            grant_root: None,
            workspace: None,
            files: None,
            workspace_id: WorkspaceId::primary(),
            data_home: None,
        }
    }

    #[must_use]
    pub fn with_workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        let workspace = workspace.into();
        self.files = Some(workspace.clone());
        self.workspace = if self.workspace_id.is_primary() {
            Some(workspace)
        } else {
            tilo_paths::workspace_tree(&workspace, self.workspace_id.as_str())
        };
        self
    }

    #[must_use]
    pub fn for_workspace(&self, id: &WorkspaceId) -> Self {
        let mut store = self.clone();
        if let Some(data) = &self.data_home {
            store.records = crate::records::terminal_data_home(data, id.as_str());
        }
        store.workspace = if id.is_primary() {
            self.files.clone()
        } else {
            self.files
                .as_deref()
                .and_then(|files| tilo_paths::workspace_tree(files, id.as_str()))
        };
        store.workspace_id = id.clone();
        store
    }

    pub fn workspace_id(&self) -> &WorkspaceId {
        &self.workspace_id
    }

    pub fn workspace(&self) -> Option<&Path> {
        self.workspace.as_deref()
    }

    pub fn files(&self) -> Option<&Path> {
        self.files.as_deref()
    }

    /// What this store's environments may be granted of the device's files
    /// ([`Confinement`]): `None` for the primary workspace. A device that
    /// has not mounted its files yet is confined against where they mount.
    pub fn confinement(&self) -> Option<Confinement> {
        let files = self
            .files
            .clone()
            .unwrap_or_else(|| PathBuf::from(tilo_paths::WORKSPACE_MOUNT));
        Confinement::new(&files, &self.workspace_id)
    }

    /// Where every workspace's records are, when they are where Terminal
    /// keeps its own (`<data home>/tilo-workspaces`): never seen in a guest, so
    /// no environment reaches another workspace's records.
    pub fn workspaces_dir(&self) -> Option<PathBuf> {
        let workspaces = self.records.ancestors().nth(4)?;
        (workspaces.file_name()? == crate::records::WORKSPACES_DIR)
            .then(|| workspaces.to_path_buf())
    }

    /// Where granted directories' contents are, when not at their own paths.
    pub fn grant_root(&self) -> Option<&Path> {
        self.grant_root.as_deref()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn records_root(&self) -> &Path {
        &self.records
    }

    pub fn environments_dir(&self) -> PathBuf {
        self.root.join("environments")
    }

    pub fn environment_dir(&self, record: &str) -> PathBuf {
        if self.workspace_id.is_primary() {
            self.environments_dir().join(record)
        } else {
            self.root
                .join("workspaces")
                .join(self.workspace_id.as_str())
                .join("environments")
                .join(record)
        }
    }

    /// Where an environment's home was kept before `~` was the workspace's
    /// folder, and still is where a device has no workspace. What an older
    /// environment keeps there stays, readable at this path.
    pub fn environment_home(&self, record: &str) -> PathBuf {
        self.environment_dir(record).join("home")
    }

    pub fn environment_var(&self, record: &str) -> PathBuf {
        self.environment_dir(record).join("var")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn audit_dir(&self) -> PathBuf {
        self.root.join("audit")
    }

    pub fn session_dir(&self, session_id: &str) -> PathBuf {
        self.sessions_dir().join(session_id)
    }

    /// Create the directory skeleton. Idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error if any of the store's subdirectories can't be
    /// created, or the store root's permissions can't be restricted to
    /// owner-only.
    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [
            self.root.clone(),
            self.environments_dir(),
            self.sessions_dir(),
            self.audit_dir(),
        ] {
            std::fs::create_dir_all(&dir)?;
        }
        restrict_to_owner(&self.root)?;
        Ok(())
    }
}

fn restrict_to_owner(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(path, perms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_lays_out_a_private_store_idempotently() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("ush"));
        store.ensure().unwrap();

        assert!(
            store
                .environment_home("default")
                .ends_with("environments/default/home")
        );
        assert!(
            store
                .environment_var("default")
                .ends_with("environments/default/var")
        );
        assert!(store.records_root().starts_with(store.root()));
        assert!(store.sessions_dir().is_dir());
        assert!(store.audit_dir().is_dir());

        let mode = std::fs::metadata(store.root())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "store root is group/world accessible: {mode:o}"
        );

        store.ensure().unwrap();
    }

    #[test]
    fn a_workspace_has_its_own_records_state_and_files() {
        let data = PathBuf::from("/home/you/.local/share");
        let base = Store {
            root: data.join("ush"),
            records: crate::records::terminal_data_home(&data, crate::workspace::PRIMARY),
            grant_root: None,
            workspace: Some(PathBuf::from("/var/workspace")),
            files: Some(PathBuf::from("/var/workspace")),
            workspace_id: WorkspaceId::primary(),
            data_home: Some(data.clone()),
        };
        let w1 = WorkspaceId::parse(Some("w1")).unwrap();
        let store = base.for_workspace(&w1);
        assert_eq!(
            store.records_root(),
            data.join("tilo-workspaces/w1/so.tilo.terminal/.local/share")
        );
        assert_eq!(
            store.workspace(),
            Some(Path::new("/var/workspace/.workspaces/w1"))
        );
        assert_eq!(store.files(), Some(Path::new("/var/workspace")));
        assert_eq!(
            store.environment_var("default"),
            data.join("ush/workspaces/w1/environments/default/var")
        );
        assert_eq!(store.sessions_dir(), base.sessions_dir());
        assert_eq!(store.workspaces_dir(), Some(data.join("tilo-workspaces")));
        assert!(store.confinement().is_some());

        let primary = store.for_workspace(&WorkspaceId::primary());
        assert_eq!(primary, base);
        assert_eq!(
            primary.records_root(),
            data.join("tilo-workspaces/personal/so.tilo.terminal/.local/share")
        );
        assert_eq!(primary.workspace(), Some(Path::new("/var/workspace")));
        assert_eq!(
            primary.environment_var("default"),
            data.join("ush/environments/default/var")
        );
        assert!(primary.confinement().is_none());

        let moved = Store::at("/tmp/ush").for_workspace(&w1);
        assert_eq!(moved.records_root(), Path::new("/tmp/ush/records"));
        assert_eq!(moved.workspace(), None, "no files, no home folder");
        assert_eq!(moved.workspaces_dir(), None);
    }
}

//! The workspace an app is launched into.
//!
//! A workspace is a collaboration workspace: its id is the cluster id every
//! change an app makes is addressed to, what the person granted apps in it is
//! what the launcher lets them reach, and its policy is what the person may do
//! in the app there. Both travel with the launch — the id as
//! `--workspace-id`, the grants as a JSON file under the per-user runtime
//! directory that the launcher reads and the app never sees
//! ([`Workspace::write_grants`]), and the authority as a projection of the
//! policy the launcher binds into the app ([`crate::authority`]).
//!
//! An app launched with no workspace gets the manifest's sandbox and nothing
//! else; that is the safe default, so a workspace is only ever passed when the
//! desktop knows which workspace the launch belongs to.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use collab_types::caps::{Access, CapOverride, LaunchGrants, Scope, Subject};
use tilo_action_policy::WorkspacePolicy;

/// The one app that browses this device's own files. It is handed the home
/// of the machine it runs on; every other app reaches files through what the
/// person granted it in the workspace.
const FILE_MANAGER: &str = "so.tilo.file-explorer";

/// One workspace, as a launch into it needs it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workspace {
    /// The workspace's id, or empty for a launch outside any workspace.
    pub id: String,
    /// What the person granted apps in this workspace.
    pub grants: LaunchGrants,
    /// The workspace's policy as the desk holds it, or `None` when the session
    /// holds none for it: what the person at this device may do in the app
    /// ([`crate::authority`]).
    pub policy: Option<WorkspacePolicy>,
    /// The person at this device, by the id the workspace's governance names
    /// them by: whose authority in it the app is handed.
    pub person: String,
    /// Where the session keeps a launch's private files, under the per-user
    /// runtime directory.
    pub directory: PathBuf,
}

impl Workspace {
    /// A workspace with no grants: an app gets exactly what its manifest
    /// declares.
    #[must_use]
    pub fn new(id: impl Into<String>, directory: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            grants: LaunchGrants::default(),
            policy: None,
            person: String::new(),
            directory: directory.into(),
        }
    }

    /// The workspace apps are launched into by default: `workspace`'s id, the
    /// launcher's capability directory under `runtime`.
    #[must_use]
    pub fn under(runtime: &Path, id: impl Into<String>) -> Self {
        Self::new(id, runtime.join("tilo").join("capabilities"))
    }

    /// Whether a launch into this workspace names a workspace at all.
    #[must_use]
    pub fn is_workspace(&self) -> bool {
        !self.id.is_empty()
    }

    /// What a launch of `app` is granted: this workspace's grants for it, and
    /// for the file manager, this device's `home`.
    ///
    /// Only what the workspace's grants hand this app reaches it — a
    /// capability naming a person, a workspace or another app never does
    /// ([`LaunchGrants::for_app`]).
    ///
    /// Files is handed the home of the machine it runs on because that is the
    /// folder it exists to show. The grant is minted here, per launch, and
    /// never written into [`Self::grants`]: that record is synced, and a
    /// grant naming one machine's home is honoured by a peer's sandbox —
    /// binding the sender's home read-write into it — while the peer's own
    /// Files still opened on its sandbox directory, seeing none of their
    /// files. Each device mints its own.
    #[must_use]
    pub fn grants_for_launch(&self, app: &str, home: Option<PathBuf>) -> LaunchGrants {
        let mut grants = self.grants.for_app(app);
        grants.author = self.may_change(app);
        if app != FILE_MANAGER {
            return grants;
        }
        let Some(home) = home.filter(|home| home.is_absolute()) else {
            return grants;
        };
        grants.sandbox.push(CapOverride::grant(
            Subject::app(app),
            Scope::FsPath(home.display().to_string()),
            Access::Write,
        ));
        grants
    }

    /// Whether `person` may change this workspace through `app`: what its
    /// answer in the workspace's policy says, and nothing where the session
    /// holds no policy for the workspace.
    fn may_change(&self, app: &str) -> bool {
        let Some(policy) = &self.policy else {
            return false;
        };
        policy
            .person_on_device(&self.person, false)
            .decide(
                &policy.node().app(app),
                tilo_action_policy::ActionKind::Undoable,
                &tilo_action_policy::Context::at_time(std::time::SystemTime::now()),
            )
            .answer
            == tilo_action_policy::Answer::Allow
    }

    /// Write the grants where `tilo-launcher --capabilities` reads them, and
    /// return that path: `None` when nothing was granted, so the launch
    /// carries no capability flag at all.
    ///
    /// The file is created owner-only before anything is written to it, and
    /// is left for the launcher to read rather than deleted here: a launch
    /// that fails to start must not take the file with it, and the runtime
    /// directory does not outlive the session.
    ///
    /// # Errors
    /// The directory or the file could not be written.
    pub fn write_grants(&self, app: &str) -> std::io::Result<Option<PathBuf>> {
        // Reverse-DNS in principle, but this arrives from a registry file:
        // refuse anything that could escape the directory.
        if app.is_empty() || app.contains('/') || app.contains("..") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("refusing to write a capability file for app {app:?}"),
            ));
        }
        let grants = self.grants_for_launch(app, tilo_paths::home());
        if grants.is_empty() {
            return Ok(None);
        }
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.directory)?;
        }
        let json = serde_json::to_vec(&grants).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("serialising capabilities: {error}"),
            )
        })?;
        let path = self.directory.join(format!("launch-{app}.json"));
        // Created at 0600 rather than written and chmod-ed after: the
        // window between the two is exactly when another process could open
        // it. Not synced: the launcher reads it through the page cache, and
        // this runs on the session's loop, where a sync under I/O pressure
        // freezes the desktop.
        let mut file = std::fs::File::create(&path).and_then(|file| {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            Ok(file)
        })?;
        file.write_all(&json)?;
        Ok(Some(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::{Access, CapOverride, Scope, Subject};

    fn granted(workspace: &Workspace, app: &str, path: &str) -> LaunchGrants {
        let mut grants = workspace.grants.clone();
        grants.sandbox.push(CapOverride::grant(
            Subject::app(app),
            Scope::FsPath(path.into()),
            Access::Read,
        ));
        grants
    }

    #[test]
    fn a_launch_writes_the_grants_the_launcher_reads_and_names_nothing_else() {
        let directory = tempfile::tempdir().expect("a directory");
        let mut workspace = Workspace::under(directory.path(), "workspace-7");
        assert_eq!(workspace.id, "workspace-7");
        assert!(workspace.is_workspace());
        assert_eq!(
            workspace.write_grants("so.tilo.notes").expect("written"),
            None,
            "nothing granted, no capability file and no flag"
        );

        workspace.grants = granted(&workspace, "so.tilo.notes", "/home/you/notes");
        let path = workspace
            .write_grants("so.tilo.notes")
            .expect("written")
            .expect("a capability file");
        assert!(path.starts_with(directory.path().join("tilo").join("capabilities")));
        let read: LaunchGrants =
            serde_json::from_slice(&std::fs::read(&path).expect("readable")).expect("JSON");
        assert_eq!(
            read, workspace.grants,
            "the launcher reads back what was granted"
        );
        let mode = {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(&path)
                .expect("the file")
                .permissions()
                .mode()
        };
        assert_eq!(mode & 0o777, 0o600, "the person's grants are owner-only");
    }

    #[test]
    fn the_file_manager_is_handed_this_devices_home() {
        let directory = tempfile::tempdir().expect("a directory");
        let workspace = Workspace::under(directory.path(), "workspace-7");
        let home = PathBuf::from("/home/you");
        assert_eq!(
            workspace.grants_for_launch("so.tilo.file-explorer", Some(home.clone())),
            LaunchGrants {
                author: false,
                sandbox: vec![CapOverride::grant(
                    Subject::app("so.tilo.file-explorer"),
                    Scope::FsPath(home.display().to_string()),
                    Access::Write,
                )],
            }
        );
        assert!(
            workspace.grants.sandbox.is_empty(),
            "the workspace's synced record never names this machine's home"
        );
        assert!(
            workspace
                .grants_for_launch("so.tilo.notes", Some(home))
                .is_empty(),
            "no other app browses the device's files"
        );

        // Every launch path writes it where the launcher reads it, from the
        // home this machine is actually running as.
        let Some(home) = tilo_paths::home() else {
            return;
        };
        let path = workspace
            .write_grants("so.tilo.file-explorer")
            .expect("written")
            .expect("a capability file");
        let read: LaunchGrants =
            serde_json::from_slice(&std::fs::read(&path).expect("readable")).expect("JSON");
        assert_eq!(
            read,
            workspace.grants_for_launch("so.tilo.file-explorer", Some(home)),
            "the launcher is handed the home of this device"
        );
    }

    #[test]
    fn a_launch_reads_back_only_what_this_app_was_granted() {
        let directory = tempfile::tempdir().expect("a directory");
        let mut workspace = Workspace::under(directory.path(), "workspace-7");
        workspace.grants = granted(&workspace, "so.tilo.notes", "/home/you/notes");
        workspace.grants = {
            let mut grants = workspace.grants.clone();
            grants.sandbox.push(CapOverride::grant(
                Subject::app("so.tilo.mail"),
                Scope::FsPath("/home/you/mail".into()),
                Access::Read,
            ));
            grants.author = true;
            grants
        };
        let path = workspace
            .write_grants("so.tilo.notes")
            .expect("written")
            .expect("a capability file");
        let read: LaunchGrants =
            serde_json::from_slice(&std::fs::read(&path).expect("readable")).expect("JSON");
        assert_eq!(
            read,
            workspace.grants.for_app("so.tilo.notes"),
            "the launcher reads what was granted to this app, not the workspace's other grants"
        );
        assert_eq!(read.sandbox.len(), 1);
        assert!(read.author);
    }

    #[test]
    fn a_workspace_refuses_a_launch_whose_app_id_could_leave_its_directory() {
        let directory = tempfile::tempdir().expect("a directory");
        let mut workspace = Workspace::under(directory.path(), "workspace");
        workspace.grants = granted(&workspace, "so.tilo.notes", "/home/you/notes");
        for app in ["", "../escape", "so.tilo/notes"] {
            assert!(
                workspace.write_grants(app).is_err(),
                "{app:?} must not name a path"
            );
        }
        assert!(
            !Workspace::under(directory.path(), "").is_workspace(),
            "a launch outside a workspace names no workspace"
        );
    }
}

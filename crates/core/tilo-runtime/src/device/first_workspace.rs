//! The person's workspace on this computer: the one its login names, or, on a computer with no
//! login, the one this device governs for its person. That one is not founded when the daemon
//! starts: the person's desktop may still hand over the workspace it kept
//! (`account/workspaces#import`), which then is theirs here. It is founded when the person first
//! needs one and nothing came in for them, and named in the device's directory from then on.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tilo_desktop_model::WorkspaceId;

use super::workspaces::ServiceWorkspaces;

/// Where a computer with no login keeps the id of its person's workspace, in the device's
/// directory: this, then the person's key in hex. A root that comes to this computer later
/// founds a workspace of its own, never enters one another person founded here.
pub(crate) const FOUNDED: &str = "workspace-of-";
/// What the workspace founded on a computer with no login is called.
pub(crate) const FOUNDED_NAME: &str = "Home";

/// The person's workspace on this computer, once there is one.
pub(crate) struct FirstWorkspace {
    /// The file naming it on a computer with no login; `None` with a login, which names it.
    file: Option<PathBuf>,
    id: tokio::sync::OnceCell<String>,
    workspaces: Arc<ServiceWorkspaces>,
}

impl FirstWorkspace {
    /// The workspace `login` names, or, with no login, the one named in the device's directory
    /// `dir` for `person` when an earlier start had one.
    ///
    /// # Errors
    /// The file naming it cannot be read, or names no workspace.
    pub(crate) fn open(
        workspaces: Arc<ServiceWorkspaces>,
        login: Option<&str>,
        dir: &Path,
        person: &[u8; 32],
    ) -> std::io::Result<Arc<Self>> {
        let file = login
            .is_none()
            .then(|| dir.join(format!("{FOUNDED}{}", hex::encode(person))));
        let named = match (login, &file) {
            (Some(login), _) => Some(login.to_owned()),
            (None, Some(file)) => match std::fs::read_to_string(file) {
                Ok(kept) => Some(
                    WorkspaceId::parse(kept.trim())
                        .ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("{} names no workspace", file.display()),
                            )
                        })?
                        .to_string(),
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            },
            (None, None) => None,
        };
        Ok(Arc::new(Self {
            file,
            id: named.map_or_else(tokio::sync::OnceCell::new, |named| {
                tokio::sync::OnceCell::new_with(Some(named))
            }),
            workspaces,
        }))
    }

    /// The workspace `workspace`, founded earlier on a computer with no login.
    #[cfg(test)]
    pub(crate) fn founded(workspaces: Arc<ServiceWorkspaces>, workspace: WorkspaceId) -> Arc<Self> {
        Arc::new(Self {
            file: Some(PathBuf::from(FOUNDED)),
            id: tokio::sync::OnceCell::new_with(Some(workspace.to_string())),
            workspaces,
        })
    }

    /// The person's workspace, when there is one yet.
    pub(crate) fn get(&self) -> Option<&str> {
        self.id.get().map(String::as_str)
    }

    /// The person's workspace, founded now when there is none yet.
    ///
    /// # Errors
    /// The service did not found it, or its name cannot be kept.
    pub(crate) async fn found(&self) -> Result<String, String> {
        self.id
            .get_or_try_init(|| async {
                let Some(file) = &self.file else {
                    return Err("this computer's login names no workspace".to_owned());
                };
                let workspace = WorkspaceId::new();
                self.workspaces.found(workspace, FOUNDED_NAME).await?;
                self.name(file, workspace)?;
                Ok(workspace.to_string())
            })
            .await
            .cloned()
    }

    /// The workspaces `moved` came in from the person's desktop: the person's own among them is
    /// their workspace here, when they have none yet, and every one's files are shown.
    ///
    /// # Errors
    /// The name of the person's workspace cannot be kept.
    pub(crate) fn moved_in(&self, moved: &[WorkspaceId]) -> Result<(), String> {
        if let Some(file) = &self.file {
            let person = &self.workspaces.device_cert().user_public_key;
            let person = tilo_server_protocol::servers::person_workspace_id(person);
            if let Some(workspace) = moved
                .iter()
                .find(|workspace| workspace.to_string() == person)
                && self.id.get().is_none()
            {
                self.name(file, *workspace)?;
                let _ = self.id.set(person);
            }
        }
        Ok(())
    }

    fn name(&self, file: &Path, workspace: WorkspaceId) -> Result<(), String> {
        tilo_files::native::atomic::write_with_mode(file, workspace.to_string().as_bytes(), 0o600)
            .map_err(|error| format!("{}: {error}", file.display()))?;
        // What waits for a workspace to be kept here (host search) looks again.
        self.workspaces.entered().notify_waiters();
        Ok(())
    }
}

//! Files put into a workspace's tree where this service holds it.
//!
//! A command or an agent that has a file's content and no folder of its own puts it into the
//! workspace through an action, and the action is answered by whichever device holds the tree.
//! Where this service holds it, the put is a change of the tree like a file copied into a
//! mounted folder: it is made on the tree this service syncs ([`TreeSync`]), kept on disk, and
//! handed to the workspace in the order the tree may go out, by the one writer every change of
//! the tree goes through.
//!
//! The tree of each workspace is opened when the first file is put into it, and kept while this
//! service runs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tilo_fs::tree_sync::TreeSync;
use tilo_server_protocol::WorkspaceId;

use crate::workspaces::Workspaces;

/// The trees this service puts files into, by workspace.
pub struct Puts {
    workspaces: Workspaces,
    /// Where each workspace's tree is kept between runs.
    root: PathBuf,
    held: Arc<tokio::sync::Mutex<HashMap<WorkspaceId, Arc<TreeSync>>>>,
}

impl Puts {
    pub(crate) fn new(workspaces: Workspaces, root: PathBuf) -> Self {
        let held: Arc<tokio::sync::Mutex<HashMap<WorkspaceId, Arc<TreeSync>>>> = Arc::default();
        // A workspace this host deleted is let go of: its tree is no one's to sync.
        let mut deletions = workspaces.deletions();
        let stopping = Arc::clone(&held);
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(async move {
                while let Ok((_, workspace)) = deletions.recv().await {
                    stopping.lock().await.remove(&workspace);
                }
            });
        }
        Self {
            workspaces,
            root,
            held,
        }
    }

    /// Change `workspace`'s tree with `change`, as `acting`, and answer what it answered once the
    /// workspace's records carry the change.
    ///
    /// # Errors
    /// This device does not hold the workspace, the change is refused, or the workspace did not
    /// take it in time.
    #[allow(
        clippy::future_not_send,
        reason = "the change runs on the thread that asks for it, through the host's block_on"
    )]
    pub async fn change(
        &self,
        workspace: WorkspaceId,
        acting: Option<tilo_actions::ActionContext>,
        change: &mut dyn FnMut(
            &mut tilo_fs::namespace::Namespace,
        ) -> Result<serde_json::Value, String>,
    ) -> Result<serde_json::Value, String> {
        let tree = self.held_tree(workspace).await?;
        tree.change(acting, change).await
    }

    /// What `read` answers of `workspace`'s tree, as the workspace has it now.
    ///
    /// # Errors
    /// This device does not hold the workspace, or its tree does not open.
    #[allow(
        clippy::future_not_send,
        reason = "the read runs on the thread that asks for it, through the host's block_on"
    )]
    pub async fn read(
        &self,
        workspace: WorkspaceId,
        read: &mut dyn FnMut(
            &mut tilo_fs::namespace::Namespace,
        ) -> Result<serde_json::Value, String>,
    ) -> Result<serde_json::Value, String> {
        let tree = self.held_tree(workspace).await?;
        tree.read(read).await
    }

    /// The tree `workspace`'s files are changed and read through, holding the workspace here,
    /// opened when it is not open yet.
    async fn held_tree(&self, workspace: WorkspaceId) -> Result<Arc<TreeSync>, String> {
        if self
            .workspaces
            .read_now(&workspace.to_string(), |_| ())
            .is_none()
        {
            return Err(format!("this device does not hold {workspace}"));
        }
        self.hold(workspace).await
    }

    /// The tree of `workspace`, opened when it is not open yet: the one this service syncs,
    /// kept under this service's state.
    async fn hold(&self, workspace: WorkspaceId) -> Result<Arc<TreeSync>, String> {
        if let Some(tree) = self.held.lock().await.get(&workspace) {
            return Ok(Arc::clone(tree));
        }
        let certificate = self.workspaces.serving_certificate();
        let keys = (certificate.device_public_key, certificate.user_public_key);
        let kept_at = self.root.join(workspace.to_string());
        let tree = crate::files::start(&self.workspaces, workspace, &kept_at, keys, None, None)
            .await
            .map_err(|error| format!("the workspace's tree does not open: {error}"))?;
        // A put that opened one meanwhile keeps the first: both sync the same records.
        Ok(Arc::clone(
            self.held
                .lock()
                .await
                .entry(workspace)
                .or_insert_with(|| Arc::new(tree)),
        ))
    }
}

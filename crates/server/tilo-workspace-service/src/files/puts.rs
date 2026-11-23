//! Files put into a workspace's tree where this service holds it.
//!
//! A command or an agent that has a file's content and no folder of its own puts it into the
//! workspace through an action, and the action is answered by whichever device holds the tree.
//! Where this service holds it, the put is a change of the tree like a file copied into a
//! mounted folder: it is made on this device's copy, whose verbs go to the workspace's store.
//!
//! The copy of each workspace is opened when the first file is put into it, and kept while this
//! service runs.

use std::collections::HashMap;
use std::sync::Arc;

use tilo_fs::files_sync::StoreSync;
use tilo_server_protocol::WorkspaceId;

use crate::workspaces::Workspaces;

/// The trees this service puts files into, by workspace.
pub struct Puts {
    workspaces: Workspaces,
    held: Arc<tokio::sync::Mutex<HashMap<WorkspaceId, Arc<StoreSync>>>>,
}

impl Puts {
    pub(crate) fn new(workspaces: Workspaces) -> Self {
        let held: Arc<tokio::sync::Mutex<HashMap<WorkspaceId, Arc<StoreSync>>>> = Arc::default();
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
        Self { workspaces, held }
    }

    /// Change `workspace`'s tree with `change`, as `acting`, and answer what it answered once the
    /// store carries the change.
    ///
    /// # Errors
    /// This device does not hold the workspace, the change is refused, or the store did not
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

    /// What `read` answers of `workspace`'s tree, as the store has it now.
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

    /// The copy `workspace`'s files are changed and read through, holding the workspace here,
    /// opened when it is not open yet.
    async fn held_tree(&self, workspace: WorkspaceId) -> Result<Arc<StoreSync>, String> {
        if self
            .workspaces
            .read_now(&workspace.to_string(), |_| ())
            .is_none()
        {
            return Err(format!("this device does not hold {workspace}"));
        }
        if let Some(tree) = self.held.lock().await.get(&workspace) {
            return Ok(Arc::clone(tree));
        }
        let tree = crate::files::open(&self.workspaces, workspace, None)
            .map_err(|error| format!("the workspace's tree does not open: {error}"))?;
        Ok(Arc::clone(
            self.held
                .lock()
                .await
                .entry(workspace)
                .or_insert_with(|| Arc::new(tree)),
        ))
    }
}

//! The workspace's list of its agents, as a host that administers the workspace writes it itself:
//! what an agent spent, how a sub-agent stands, a team's own changes. The list is a record of the
//! workspace's store ([`tilo_workspace_agent::KIND`] of [`tilo_workspace_agent::OWNER`]), and is
//! written as any record of a kind its app declares is.

use automerge::AutoCommit;

use super::Workspaces;

impl Workspaces {
    /// Write `change` into `workspace`'s list of its agents as this host, which administers the
    /// workspace: onto the list as the store holds it, or as its kind founds it where the store
    /// holds none yet. Answers what `change` answered.
    ///
    /// A verb `change` applies names the record's app (`Replica::in_app` with
    /// [`tilo_workspace_agent::OWNER`]): the record's receipt check reads the action as that
    /// app's, and a name guessed from its type is no app the record has, so every device would
    /// refuse the write.
    ///
    /// # Errors
    /// This host does not administer the workspace, `change` failed, or the store refuses it.
    pub async fn author_agents<R>(
        &self,
        workspace: &str,
        change: impl FnOnce(&mut AutoCommit) -> Result<R, String>,
    ) -> Result<R, String> {
        {
            let state = self
                .workspace(workspace)
                .await
                .map_err(|error| error.to_string())?;
            let hosted = state.lock().await;
            hosted.peer().workspace().check_workspace_administration(
                self.device_cert().device_public_key,
                tilo_clock::wall(),
            )?;
        }
        let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        self.author_record(
            workspace,
            (
                tilo_workspace_agent::OWNER,
                tilo_workspace_agent::KIND,
                &list,
            ),
            change,
        )
        .await
    }
}

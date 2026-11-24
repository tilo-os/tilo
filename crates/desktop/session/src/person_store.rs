//! The person's own workspace as this session keeps what is theirs alone in it: through the
//! device's daemon, which keeps the workspace ([`PersonStore`]).
//!
//! The session holds no copy of the workspace. It reads each resource from the daemon, writes a
//! change to one there, which the daemon signs and seals as this device, and hears of every write
//! the daemon takes on the daemon's own feed of them.

use runtime_collab::wire::shell::{ShellCall, ShellReply};
use tilo_peer_state::workspace_documents::DocumentRead;
use tilo_session_client::daemon_documents::{
    DaemonShell, DaemonWritesFeed, document_read, listed, listing, whole,
};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::{Answer, Authoring, Moved, PersonStore};

/// The person's own workspace, kept by the device's daemon this session reaches on `shell`.
pub(crate) struct DaemonPersonStore {
    shell: DaemonShell,
    writes: DaemonWritesFeed,
    certificate: DeviceCert,
    /// The person's workspace's id.
    workspace: String,
}

impl DaemonPersonStore {
    /// The person's workspace that `certificate`'s person keeps, as the daemon on `shell` keeps
    /// it and `writes` tells its writes.
    pub(crate) fn new(
        shell: DaemonShell,
        writes: DaemonWritesFeed,
        certificate: DeviceCert,
    ) -> Self {
        let workspace =
            tilo_server_protocol::servers::person_workspace_id(&certificate.user_public_key);
        Self {
            shell,
            writes,
            certificate,
            workspace,
        }
    }

    /// The resource `(app, name)` as the daemon holds it now, saved; `None` while it holds none.
    async fn saved(&self, app: &str, name: &str) -> Result<Option<Vec<u8>>, String> {
        match document_read(&self.shell.read(whole(&self.workspace, app, name)).await?)? {
            DocumentRead::Whole { bytes, .. } => Ok(Some(bytes)),
            DocumentRead::Absent | DocumentRead::Pending => Ok(None),
            DocumentRead::Changes { .. } | DocumentRead::Unchanged(_) => {
                Err("the daemon answered a whole read with part of a resource".to_owned())
            }
        }
    }
}

impl PersonStore for DaemonPersonStore {
    fn certificate(&self) -> DeviceCert {
        self.certificate.clone()
    }

    fn held(&self) -> Answer<'_, bool> {
        Box::pin(async move {
            match self.shell.ask(ShellCall::Workspaces).await {
                Ok(ShellReply::Workspaces(held)) => held.contains(&self.workspace),
                Ok(other) => {
                    tracing::warn!(answer = ?other, "the daemon did not say which workspaces it keeps");
                    false
                }
                Err(error) => {
                    tracing::warn!(%error, "the daemon did not say which workspaces it keeps");
                    false
                }
            }
        })
    }

    fn routes(&self) -> Answer<'_, Result<Vec<(String, String)>, String>> {
        Box::pin(async move { Ok(listed(&self.shell.read(listing(&self.workspace)).await?)) })
    }

    fn document<'a>(
        &'a self,
        app: &'a str,
        name: &'a str,
    ) -> Answer<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(self.saved(app, name))
    }

    fn author(
        &self,
        app: String,
        name: String,
        authoring: Authoring,
    ) -> Answer<'_, Result<(), String>> {
        Box::pin(async move {
            let held = self.saved(&app, &name).await?;
            let founding = held.is_none();
            let mut document = match held {
                Some(bytes) => automerge::AutoCommit::load(&bytes)
                    .map_err(|error| format!("the person's resource does not read: {error}"))?,
                None => *authoring
                    .genesis
                    .ok_or("the person's workspace holds none of that resource to change")?,
            };
            let before = document.get_heads();
            (authoring.edit)(&mut document)?;
            // A resource the workspace holds none of is written whole, founded by this write; one
            // it holds takes only what this change added.
            let changes = if founding {
                document.save()
            } else {
                document.save_after(&before)
            };
            if !founding && changes.is_empty() {
                return Ok(());
            }
            match self
                .shell
                .ask(ShellCall::Write {
                    workspace: self.workspace.clone(),
                    app,
                    document: name,
                    readers: None,
                    founding,
                    snapshot: founding,
                    changes,
                })
                .await?
            {
                ShellReply::Written(Ok(_)) => Ok(()),
                ShellReply::Written(Err(refusal)) => Err(refusal),
                other => Err(format!("the daemon answered a write with {other:?}")),
            }
        })
    }

    fn moves(&self) -> tokio::sync::mpsc::UnboundedReceiver<Moved> {
        use tokio::sync::broadcast::error::RecvError;
        let (tell, told) = tokio::sync::mpsc::unbounded_channel();
        let mut writes = self.writes.follow();
        let workspace = self.workspace.clone();
        self.shell.runtime().spawn(async move {
            loop {
                let moved = match writes.recv().await {
                    Ok(write) if write.workspace == workspace => {
                        Moved::Resource(write.app, write.record)
                    }
                    // A write that names no workspace is the feed falling behind: the daemon took
                    // writes it did not tell.
                    Ok(write) if write.workspace.is_empty() => Moved::Everything,
                    // Another workspace's write: whether the daemon holds this one may have moved
                    // with it, as a join or a founding writes its first resources.
                    Ok(_) => Moved::Standing,
                    Err(RecvError::Lagged(_)) => Moved::Everything,
                    Err(RecvError::Closed) => return,
                };
                if tell.send(moved).is_err() {
                    return;
                }
            }
        });
        told
    }
}

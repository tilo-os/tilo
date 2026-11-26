//! The person's own workspace as this session keeps what is theirs alone in it: through the
//! device's daemon, which keeps the workspace ([`PersonStore`]).
//!
//! The session holds no copy of the workspace. It reads each resource from the daemon, writes a
//! change to one there, which the daemon signs and seals as this device, and hears of every write
//! the daemon takes on the daemon's own feed of them.

use runtime_collab::wire::shell::{ShellCall, ShellReply};
use tilo_peer_state::workspace_documents::DocumentRead;
use tilo_session_client::daemon_documents::{
    DaemonShell, DaemonWritesFeed, document_read, listing, whole,
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
        Box::pin(async move {
            let listing = self.shell.read(listing(&self.workspace)).await?;
            Ok(
                tilo_actions::remote::ListedResource::read_listing(&self.workspace, "", &listing)
                    .into_iter()
                    .map(|listed| (listed.address.app, listed.address.record))
                    .collect(),
            )
        })
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
            // A record the workspace holds none of starts from its kind's founding, where the
            // caller names one, as the daemon's own writes of the person's workspace found it.
            let mut document = match held {
                Some(bytes) => automerge::AutoCommit::load(&bytes)
                    .map_err(|error| format!("the person's resource does not read: {error}"))?,
                None => authoring
                    .genesis
                    .map_or_else(automerge::AutoCommit::new, |genesis| *genesis),
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
                    kind: authoring.kind.map(str::to_owned),
                    record: name,
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use automerge::ReadDoc as _;
    use automerge::transaction::Transactable as _;
    use tilo_session_client::daemon_documents::inherited_link;

    /// The shell's first write of one of its documents to the person's workspace founds it at the
    /// daemon, which then holds it: the shell's documents carry no genesis of their own, and a
    /// write the daemon was never handed would leave the person's shell on this device alone.
    #[test]
    fn the_first_write_of_a_document_the_workspace_holds_none_of_founds_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let mut home = tilo_test_env::TestHome::new().expect("a home for this computer");
        let (_identity, cert, _key) = crate::governance::tests::provisioned_identity(&mut home);
        let profile = tempfile::tempdir().expect("a profile for the daemon");
        std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o700))
            .expect("the profile is this user's");
        let daemon = tilo_runtime::daemon::Running::start(profile.path())
            .expect("the device's daemon starts");
        let within = tilo_test_env::hang_bound();
        let runtime = tilo_rt::app().handle().clone();
        let link = inherited_link(
            Some(
                daemon
                    .link_shell(within)
                    .expect("the daemon makes the shell's link"),
            ),
            &runtime,
        );
        let shell = DaemonShell::new(link.map(|(link, _)| link), runtime.clone());
        let person =
            tilo_actions::Acting::person(tilo_actions::PersonId::from_root(cert.user_public_key));
        let workspace = tilo_server_protocol::servers::person_workspace_id(&cert.user_public_key);
        let store = DaemonPersonStore::new(shell.clone(), DaemonWritesFeed::start(&shell), cert);
        let (app, name) = (
            tilo_desktop_model::ownership::DESKTOP_APP,
            tilo_desktop_model::ownership::DESKTOP_DOCUMENT,
        );
        runtime.block_on(async {
            let deadline = tokio::time::Instant::now() + within;
            while !store.held().await {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the daemon never held the person's workspace"
                );
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            assert_eq!(
                store.document(app, name).await.expect("the daemon reads"),
                None,
                "the workspace holds none of the document before the shell writes it"
            );
            store
                .author(
                    app.to_owned(),
                    name.to_owned(),
                    Authoring {
                        kind: Some(tilo_desktop_model::ownership::DESKTOP_KIND),
                        genesis: Some(Box::new(
                            tilo_desktop_model::record::founded(&workspace)
                                .expect("the layout founds"),
                        )),
                        edit: Box::new(move |document| {
                            document
                                .put(automerge::ROOT, "edge", "left")
                                .map_err(|error| error.to_string())?;
                            document.commit_with(
                                automerge::transaction::CommitOptions::default().with_message(
                                    tilo_actions::ActionContext::whole_edit_by(&person),
                                ),
                            );
                            Ok(())
                        }),
                    },
                )
                .await
                .expect("the shell's first write of the document lands");
            let held = store
                .document(app, name)
                .await
                .expect("the daemon reads")
                .expect("the workspace holds the document the shell founded");
            let held = automerge::AutoCommit::load(&held).expect("the document reads");
            assert_eq!(
                held.get(automerge::ROOT, "edge")
                    .expect("the document reads")
                    .map(|(value, _)| value.to_string()),
                Some("\"left\"".to_owned()),
                "what the shell wrote is what the workspace holds"
            );
        });
        drop(daemon);
    }
}

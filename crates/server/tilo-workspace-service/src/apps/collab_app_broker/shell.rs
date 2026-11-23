//! A shell connection's calls ([`runtime_collab::wire::shell`]), answered from the broker's
//! [`ShellServices`] as the person it admitted.

use std::sync::Arc;

use runtime_collab::wire::Reply;
use runtime_collab::wire::shell::{SHELL_MAGIC, ShellCall, ShellRead, ShellReply, ShellWrite};
use tilo_peer_state::workspace_documents::{DocumentRead, DocumentWriting};

use crate::apps::shell::{ShellCaller, ShellPeer, ShellServices};

/// A connection the broker admitted as a person's shell.
#[derive(Clone)]
pub(super) struct Shell {
    pub(super) caller: ShellCaller,
    pub(super) services: Arc<ShellServices>,
}

/// Serve `stream`, which opened with the shell's marker, as the shell of the person its account
/// is signed in as; a peer that is no one's shell is refused before its first call.
#[allow(
    clippy::future_not_send,
    reason = "a connection is served on the broker's LocalSet, as every other is"
)]
pub(super) async fn serve(
    mut stream: tokio::net::UnixStream,
    services: Arc<ShellServices>,
) -> Result<(), crate::apps::BrokerError> {
    use tokio::io::AsyncReadExt as _;
    let failed = |error: String| crate::apps::BrokerError::failed(error);
    let credentials = stream
        .peer_cred()
        .map_err(|error| failed(format!("a shell's credentials do not read: {error}")))?;
    let pid = credentials
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| failed("a shell's connection names no process".to_owned()))?;
    let peer = ShellPeer {
        uid: credentials.uid(),
        pid,
    };
    let mut marker = [0_u8; SHELL_MAGIC.len()];
    stream
        .read_exact(&mut marker)
        .await
        .map_err(|error| failed(format!("a shell's marker does not read: {error}")))?;
    let caller = match services.accounts.admit(peer) {
        Ok(caller) => caller,
        Err(refusal) => {
            tracing::warn!(uid = peer.uid, pid = peer.pid, %refusal, "refused a shell's connection");
            return Ok(());
        }
    };
    let (connection, outgoing) = super::wire::WireConnection::shell(Shell { caller, services });
    let (reader, writer) = stream.into_split();
    super::wire::serve(connection, outgoing, reader, writer)
        .await
        .map_err(|error| failed(error.to_string()))
}

/// Answer `call` for `shell`; a connection that is no shell is refused.
pub(super) async fn answer(shell: Option<Shell>, call: ShellCall) -> Result<Reply, String> {
    let shell = shell.ok_or("this connection is not the person's shell")?;
    let services = Arc::clone(&shell.services);
    let reply = match call {
        ShellCall::Read {
            workspace,
            app,
            resource,
            since,
        } => {
            let documents = Arc::clone(&services.documents);
            let read = tokio::task::spawn_blocking(move || {
                documents.read(&workspace, &app, &resource, since.as_deref())
            })
            .await
            .map_err(|_| "the read stopped before it answered".to_owned())??;
            ShellReply::Read(read_on_wire(read))
        }
        ShellCall::Readable {
            workspace,
            app,
            resource,
        } => {
            let documents = Arc::clone(&services.documents);
            ShellReply::Is(
                tokio::task::spawn_blocking(move || {
                    documents.readable(&workspace, &app, &resource)
                })
                .await
                .map_err(|_| "the read stopped before it answered".to_owned())?,
            )
        }
        ShellCall::Listing { workspace } => {
            ShellReply::Listing(services.documents.listing(&workspace)?)
        }
        ShellCall::Holds {
            workspace,
            app,
            resource,
        } => ShellReply::Is(services.documents.holds(&workspace, &app, &resource)),
        ShellCall::Workspaces => ShellReply::Workspaces(services.documents.workspaces()),
        ShellCall::Write {
            workspace,
            app,
            document,
            readers,
            founding,
            snapshot,
            changes,
        } => {
            // The device seals with the keys of the document's group itself: the shell holds
            // none.
            let written = services
                .writes
                .write(
                    &workspace,
                    &changes,
                    DocumentWriting {
                        app: &app,
                        document: &document,
                        readers: readers.as_deref(),
                        founding,
                        snapshot,
                        sealed: None,
                    },
                )
                .await;
            ShellReply::Written(match written {
                Ok(write) => Ok(write_on_wire(&write)?),
                Err(refusal) => Err(serde_json::to_string(&refusal)
                    .map_err(|error| format!("a refusal does not encode: {error}"))?),
            })
        }
        ShellCall::Served {
            workspace,
            documents,
        } => {
            let named: Vec<(&str, &str)> = documents
                .iter()
                .map(|(app, document)| (app.as_str(), document.as_str()))
                .collect();
            let serving = services.writes.served(&workspace, &named);
            ShellReply::Served(match serving {
                Some(serving) => Some(serving.await),
                None => None,
            })
        }
        ShellCall::Invoke { request } => {
            let request = serde_json::from_str(&request)
                .map_err(|error| format!("the invocation does not read: {error}"))?;
            let settled = services.actions.invoke(&shell.caller, request).await?;
            ShellReply::Invoked(
                serde_json::to_string(&settled)
                    .map_err(|error| format!("an invocation's answer does not encode: {error}"))?,
            )
        }
    };
    Ok(Reply::Shell(reply))
}

/// `read` as the wire carries it.
fn read_on_wire(read: DocumentRead) -> ShellRead {
    match read {
        DocumentRead::Whole {
            revision,
            heads,
            bytes,
        } => ShellRead::Whole {
            revision,
            heads,
            bytes,
        },
        DocumentRead::Changes {
            revision,
            heads,
            bytes,
        } => ShellRead::Changes {
            revision,
            heads,
            bytes,
        },
        DocumentRead::Unchanged(revision) => ShellRead::Unchanged(revision),
        DocumentRead::Absent => ShellRead::Absent,
        DocumentRead::Pending => ShellRead::Pending,
    }
}

/// `write` as the wire carries it.
fn write_on_wire(write: &tilo_peer_state::owner::AppWrite) -> Result<ShellWrite, String> {
    Ok(ShellWrite {
        peer: write.peer,
        app_id: write.app_id.clone(),
        cluster_id: write.cluster_id.clone(),
        document_id: write.document_id.clone(),
        author: serde_json::to_string(&write.author)
            .map_err(|error| format!("a write's author does not encode: {error}"))?,
        replayed: write.replayed,
        envelope: postcard::to_allocvec(&write.envelope)
            .map_err(|error| format!("a write does not encode: {error}"))?,
    })
}

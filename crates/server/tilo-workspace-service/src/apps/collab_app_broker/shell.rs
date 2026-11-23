//! A shell connection's calls ([`runtime_collab::wire::shell`]), answered from the dispatch's
//! [`ShellServices`] as the person it admitted.

use std::sync::Arc;

use runtime_collab::wire::Reply;
use runtime_collab::wire::shell::{
    DocumentWritten, SHELL_MAGIC, ShellCall, ShellRead, ShellReply, ShellWrite, VOUCH_MAGIC,
};
use tilo_peer_state::workspace_documents::{DocumentRead, DocumentWriting};

use crate::apps::shell::{ShellCaller, ShellServices};

/// A connection the daemon's dispatch admitted as a person's shell.
#[derive(Clone)]
pub(super) struct Shell {
    pub(super) caller: ShellCaller,
    pub(super) services: Arc<ShellServices>,
    /// The writes the device took since the shell last asked, followed from the moment it
    /// connected, so none falls between two asks.
    pub(super) writes: Arc<tokio::sync::Mutex<crate::apps::shell::DocumentWrites>>,
}

/// Serve `stream`, which opened with the shell's marker, as the shell its link's secret was made
/// for; a connection whose secret is no link's is refused before its first call.
#[allow(
    clippy::future_not_send,
    reason = "a connection is served on the dispatch's LocalSet, as every other is"
)]
pub(super) async fn serve(
    mut stream: tokio::net::UnixStream,
    services: Arc<ShellServices>,
) -> Result<(), crate::apps::BrokerError> {
    use runtime_collab::wire::shell::SHELL_SECRET_BYTES;
    use tokio::io::AsyncReadExt as _;
    let unread = |error: std::io::Error| {
        crate::apps::BrokerError::failed(format!("a shell link's opening does not read: {error}"))
    };
    let mut marker = [0_u8; SHELL_MAGIC.len()];
    stream.read_exact(&mut marker).await.map_err(unread)?;
    let mut secret = [0_u8; SHELL_SECRET_BYTES];
    stream.read_exact(&mut secret).await.map_err(unread)?;
    let Some(caller) = services.links.take(&secret) else {
        tracing::warn!("refused a connection that named no shell link the daemon made");
        return Ok(());
    };
    let followed = Arc::new(tokio::sync::Mutex::new((services.document_writes)()));
    let (connection, outgoing) = super::wire::WireConnection::shell(Shell {
        caller,
        services,
        writes: followed,
    });
    let (reader, writer) = stream.into_split();
    super::wire::serve(connection, outgoing, reader, writer)
        .await
        .map_err(|error| crate::apps::BrokerError::failed(error.to_string()))
}

/// Admit `stream`, which opened with [`VOUCH_MAGIC`], as the app an admitted shell vouched for
/// on its own link ([`ShellCall::Vouch`]): the secret that follows the marker is the one the
/// daemon answered that vouch with, taken by this connection alone. A connection with any other
/// secret, an app's own above all, is refused before its first call. The connection is then the
/// app's, as a launch the daemon started would be.
pub(super) async fn vouched(
    stream: &mut tokio::net::UnixStream,
    services: &crate::apps::services::ProcessServices,
) -> Result<super::socket::AuthenticatedHandoff, super::socket::LaunchRefusal> {
    use runtime_collab::wire::shell::SHELL_SECRET_BYTES;
    use tokio::io::AsyncReadExt as _;
    let peer = super::launcher_handoff::credentialed_peer(stream);
    let refused = |error: String, answerable: bool| super::socket::LaunchRefusal {
        error: crate::apps::BrokerError::failed(error),
        peer: peer.clone(),
        answerable,
    };
    let shell = services
        .shell
        .as_ref()
        .ok_or_else(|| refused("this host answers no shell".to_owned(), false))?;
    let unread =
        |error: std::io::Error| refused(format!("a vouch's opening does not read: {error}"), false);
    let mut marker = [0_u8; VOUCH_MAGIC.len()];
    stream.read_exact(&mut marker).await.map_err(unread)?;
    let mut secret = [0_u8; SHELL_SECRET_BYTES];
    stream.read_exact(&mut secret).await.map_err(unread)?;
    let vouch = shell.links.take_vouched(&secret).ok_or_else(|| {
        refused(
            "only the person's shell vouches for an app, on its own link".to_owned(),
            true,
        )
    })?;
    // The shell's process stands for the app here: the connection lasts while it does.
    let process = super::native_media::PeerProcess::from_socket(stream).ok_or_else(|| {
        refused(
            "the vouching process cannot be pinned on this system".to_owned(),
            true,
        )
    })?;
    let launch = crate::apps::services::AuthenticatedApplicationLaunch {
        continuation_instance: vouch.window,
        launch_ticket: None,
        windowless: vouch.windowless,
    };
    let local_media = super::native_media::LocalAppConnection::attested_keeping(
        vouch.app,
        vouch.workspace,
        process,
        launch,
    )
    .ok_or_else(|| refused("the vouching process ended".to_owned(), false))?;
    Ok(super::socket::AuthenticatedHandoff {
        local_media: Some(local_media),
        launch: None,
    })
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
        call @ ShellCall::Write { .. } => write(&services, call).await?,
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
        ShellCall::Writes => ShellReply::Writes(next_writes(&shell.writes).await),
        call @ (ShellCall::Resources { .. }
        | ShellCall::Vault(_)
        | ShellCall::Receipt { .. }
        | ShellCall::KeepsReceipts { .. }
        | ShellCall::Authorizes { .. }) => kept(&services.workspaces, call).await,
        ShellCall::Vouch(vouch) => vouch_for(&services, vouch)?,
        ShellCall::Invoke { request } => {
            let request = serde_json::from_str(&request)
                .map_err(|error| format!("the invocation does not read: {error}"))?;
            let settled = services.actions.invoke(&shell.caller, request).await;
            ShellReply::Invoked(settled.map(|value| value.to_string()))
        }
    };
    Ok(Reply::Shell(reply))
}

/// A [`ShellCall::Write`], written as the device.
async fn write(services: &ShellServices, call: ShellCall) -> Result<ShellReply, String> {
    let ShellCall::Write {
        workspace,
        app,
        document,
        readers,
        founding,
        snapshot,
        changes,
    } = call
    else {
        return Err("that is no write".to_owned());
    };
    // The device seals with the keys of the document's group itself: the shell holds none.
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
    Ok(ShellReply::Written(match written {
        Ok(write) => Ok(write_on_wire(&write)?),
        Err(refusal) => Err(serde_json::to_string(&refusal)
            .map_err(|error| format!("a refusal does not encode: {error}"))?),
    }))
}

/// Where and with which secret a connection opens to be admitted as the app `vouch` names, which
/// the shell launched.
fn vouch_for(
    services: &ShellServices,
    vouch: runtime_collab::wire::shell::Vouch,
) -> Result<ShellReply, String> {
    if vouch.app.trim().is_empty() || vouch.workspace.trim().is_empty() {
        return Err("a vouch names an empty app or workspace".to_owned());
    }
    Ok(ShellReply::Vouched {
        socket: services.socket.clone(),
        secret: services.links.vouch(vouch),
    })
}

/// The writes `writes` took since it was last asked, waiting for the first; `None` once it missed
/// some, so the shell reads again everything it follows.
async fn next_writes(
    writes: &tokio::sync::Mutex<crate::apps::shell::DocumentWrites>,
) -> Option<Vec<DocumentWritten>> {
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};
    let told = |write: crate::workspaces::ResourceWrite| DocumentWritten {
        workspace: write.workspace,
        app: write.app,
        record: write.record,
    };
    let mut writes = writes.lock().await;
    let mut written = match writes.recv().await {
        Ok(write) => vec![told(write)],
        Err(RecvError::Lagged(_)) => return None,
        // The device's service closed: nothing is written any more, and the shell's connection
        // ends with it.
        Err(RecvError::Closed) => return std::future::pending().await,
    };
    loop {
        match writes.try_recv() {
            Ok(write) => written.push(told(write)),
            Err(TryRecvError::Lagged(_)) => return None,
            Err(TryRecvError::Empty | TryRecvError::Closed) => return Some(written),
        }
    }
}

/// `call`, a resource command, a vault ask, a receipt or a read to decide, answered from the
/// workspaces the device holds.
async fn kept(workspaces: &crate::workspaces::Workspaces, call: ShellCall) -> ShellReply {
    match call {
        ShellCall::Receipt {
            workspace,
            redacted,
        } => ShellReply::Receipted(
            workspaces
                .commit_member_effect_receipt(&workspace, &redacted)
                .await,
        ),
        ShellCall::KeepsReceipts { workspace } => {
            ShellReply::Is(workspaces.is_member(&workspace).await.unwrap_or(false))
        }
        ShellCall::Authorizes {
            workspace,
            app,
            resource,
            certificate,
        } => ShellReply::Authorized(
            serde_json::from_str(&certificate)
                .map_err(|error| format!("the certificate does not read: {error}"))
                .and_then(|certificate| {
                    workspaces.authorizes_resource_read(&workspace, &certificate, &app, &resource)
                }),
        ),
        ShellCall::Resources {
            workspace,
            app,
            command,
        } => ShellReply::Resources(resources(workspaces, &workspace, &app, &command).await),
        ShellCall::Vault(call) => ShellReply::Vault(
            vault(workspaces, call)
                .await
                .map(runtime_collab::wire::shell::VaultAnswer),
        ),
        _ => ShellReply::Resources(Err("that is no resource command or vault ask".to_owned())),
    }
}

/// `command` run on `app`'s resources in `workspace` as the device's person, answered as the
/// store's reply.
async fn resources(
    workspaces: &crate::workspaces::Workspaces,
    workspace: &str,
    app: &str,
    command: &str,
) -> Result<String, String> {
    let command = serde_json::from_str(command)
        .map_err(|error| format!("the resource command does not read: {error}"))?;
    let reply = workspaces
        .own_resource_command(workspace, app, command)
        .await?;
    serde_json::to_string(&reply).map_err(|error| error.to_string())
}

/// `call` asked of its workspace's vault and accounts.
async fn vault(
    workspaces: &crate::workspaces::Workspaces,
    call: runtime_collab::wire::shell::VaultCall,
) -> Result<String, String> {
    use runtime_collab::wire::shell::VaultCall;
    use tilo_actions::remote::Caller;
    use tilo_actions::remote::person_vault::ItemRef;
    let item_of =
        |item: &str| ItemRef::parse(item).ok_or_else(|| format!("{item} names no vault entry"));
    match call {
        VaultCall::Keep {
            workspace,
            person,
            device,
            item,
            title,
            username,
            places,
            password,
        } => {
            let places = places
                .iter()
                .map(|place| accounts::vault_items::Place::parse(place))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            let item = item.as_deref().map(item_of).transpose()?;
            let by = Caller::authenticated(person, device).in_workspace(workspace.clone());
            let kept = workspaces
                .keep_account_password(
                    &workspace,
                    &by,
                    item.as_ref(),
                    crate::workspaces::account_logins::AccountLogin {
                        title,
                        username,
                        places,
                    },
                    zeroize::Zeroizing::new(password),
                )
                .await?;
            Ok(kept.to_string())
        }
        VaultCall::Release {
            workspace,
            person,
            device,
            app,
            item,
        } => {
            let item = item_of(&item)?;
            let by = Caller::authenticated(person, device).in_workspace(workspace.clone());
            let released = workspaces
                .account_secret(&workspace, &by, &app, &item)
                .await?;
            Ok(String::clone(&released.secret))
        }
        VaultCall::Forget { workspace, account } => workspaces
            .remove_workspace_account(&workspace, &account)
            .await
            .map(|forgot| forgot.to_string()),
    }
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

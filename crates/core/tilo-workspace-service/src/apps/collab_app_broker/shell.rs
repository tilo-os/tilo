//! A shell connection's calls ([`runtime_collab::wire::shell`]), answered from the dispatch's
//! [`ShellServices`] as the person it admitted.

use std::sync::Arc;

use runtime_collab::wire::Reply;
use runtime_collab::wire::shell::{
    DocumentWritten, PROCESSES_MAGIC, SHELL_MAGIC, ShellCall, ShellReply, ShellWrite, VOUCH_MAGIC,
};
use tilo_actions::remote::{
    Caller, InvokeArgument, ReceiptStatus, RemoteActionMessage, ResourceReadRequest, resource_parts,
};
use tilo_peer_state::workspace_documents::DocumentWriting;

use crate::apps::services::{Caller as ServiceCaller, PublishedResources};
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

/// Hand the device's tilolang processes the packet socket `stream` carries, when it opened with
/// [`PROCESSES_MAGIC`] and the secret the daemon answered a shell's [`ShellCall::Processes`] with
/// on its own link. Marker, secret and descriptor come as one message. Any other connection, one
/// that dialed the socket itself above all, is closed before anything reaches the processes:
/// only the shell the daemon started hands them an instance.
pub(super) async fn processes(
    stream: tokio::net::UnixStream,
    services: Arc<ShellServices>,
) -> Result<(), crate::apps::BrokerError> {
    use runtime_collab::wire::shell::SHELL_SECRET_BYTES;
    let refused = |why: String| {
        tracing::warn!(%why, "refused a hand-over to the device's tilolang processes");
        Ok(())
    };
    let (opening, packets) = loop {
        if let Err(error) = stream.readable().await {
            return refused(format!("its connection does not read: {error}"));
        }
        match stream.try_io(tokio::io::Interest::READABLE, || {
            runtime_portal::fd_transport::recv_with_fd(&stream)
        }) {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return refused(format!("its opening does not read: {error}")),
            Ok(received) => break received,
        }
    };
    let secret = opening
        .strip_prefix(PROCESSES_MAGIC.as_slice())
        .and_then(|secret| <[u8; SHELL_SECRET_BYTES]>::try_from(secret).ok());
    let (Some(secret), Some(packets)) = (secret, packets) else {
        return refused("its opening is no marker, secret and packet socket".to_owned());
    };
    if !services.links.take_processes(&secret) {
        return refused(
            "only the person's shell hands over, with a secret its link asked for".to_owned(),
        );
    }
    let Some(processes) = services.processes.as_ref() else {
        return refused("no tilolang process can start on this device".to_owned());
    };
    processes(packets);
    Ok(())
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
        opened_for: None,
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
        ShellCall::Processes => {
            if services.processes.is_none() {
                return Err("no tilolang process can start on this device".to_owned());
            }
            ShellReply::Processes {
                socket: services.socket.clone(),
                secret: services.links.processes(),
            }
        }
        ShellCall::Action {
            workspace,
            frame,
            caller,
        } => ShellReply::Action(
            dispatch(
                &services,
                &shell.caller,
                workspace,
                caller.map(|caller| *caller),
                &frame,
            )
            .await?,
        ),
        // The connection registers what its shell serves itself (`WireConnection::provide`).
        ShellCall::Provide { .. } => {
            return Err("a provider is served on the shell's own connection".to_owned());
        }
    };
    Ok(Reply::Shell(reply))
}

/// `frame`, a `tilo_actions::remote::RemoteActionFrame` request the shell made in `workspace`,
/// answered as the person the shell's link was made for, or as the app or agent acting for them
/// the shell names (`named`): the frame answering it. A read of resources is answered by the
/// device's one resource reader, as every device of theirs reads; a typed action of a resource by
/// the provider that applies it; any other request by the device's one dispatch.
async fn dispatch(
    services: &ShellServices,
    caller: &ShellCaller,
    workspace: Option<String>,
    named: Option<tilo_actions::remote::KeptCaller>,
    frame: &[u8],
) -> Result<Vec<u8>, String> {
    use tilo_actions::remote::RemoteActionFrame;
    let message = RemoteActionFrame::decode(frame)
        .map_err(|error| error.to_string())?
        .message;
    let workspaces = &services.workspaces;
    let device = workspaces.device_cert();
    if caller.person != device.user_public_key {
        return Err("this device acts for its own person only".to_owned());
    }
    let named = named
        .map(|named| named_caller(named, &device))
        .transpose()?;
    let person = named
        .clone()
        .unwrap_or_else(|| Caller::authenticated(device.user_public_key, device.device_public_key));
    let answer = match message {
        RemoteActionMessage::ResourceRead {
            correlation, read, ..
        } => {
            // A resource the device's services publish themselves (this device's server
            // settings) is read as the app the read names, decided there as its grant decides.
            if let Some(published) = &services.published
                && let Some(revision) = published_revision(published, &read).await?
            {
                return RemoteActionFrame::new(RemoteActionMessage::ResourceReading {
                    correlation,
                    result: published_read(published, &read, revision)
                        .await
                        .map_err(
                            |reason| tilo_actions::remote::ActionCallError::Unavailable { reason },
                        ),
                })
                .encode()
                .map_err(|error| error.to_string());
            }
            // The person reads through the device's reader of its workspaces; an app or an agent
            // acting for them through the dispatch's, which decides each as the one actor the
            // dispatch decides its calls as: an app bounded by what it declares.
            let reads: Arc<dyn tilo_actions::remote::ResourceReads> = if named.is_some() {
                let (dispatch, _) = services
                    .actions
                    .dispatch(caller, Some(read.workspace.clone()))
                    .await?;
                dispatch
                    .resource_reads()
                    .ok_or("this device's dispatch reads no records")?
            } else {
                workspaces.resource_reads()
            };
            let reader = person.in_workspace(read.workspace.clone());
            reads
                .answer_read(&reader, correlation, read)
                .await
                .ok_or("the read stopped before it answered")?
        }
        RemoteActionMessage::Invoke(request)
            if tilo_actions::remote::is_resource_provider(&request.target.provider) =>
        {
            // A resource the device's services publish themselves is changed by the app the
            // request names, as its own action.
            if let Some(published) = &services.published
                && let Some((app, record)) = resource_parts(&request.target.provider)
                && published_publishes(
                    published,
                    &ServiceCaller {
                        app_id: app.to_owned(),
                        workspace: workspace.clone().unwrap_or_default(),
                    },
                    record,
                )
                .await?
            {
                let arguments = match &request.argument {
                    InvokeArgument::Json(arguments) => serde_json::to_vec(arguments)
                        .map_err(|error| format!("the arguments do not encode: {error}"))?,
                    InvokeArgument::None => br"{}".to_vec(),
                    InvokeArgument::Text(_) => {
                        return Err("a published resource's action takes named arguments".into());
                    }
                };
                return RemoteActionFrame::new(RemoteActionMessage::Receipt(receipt_of(
                    request.invocation,
                    published_action(
                        published,
                        &ServiceCaller {
                            app_id: app.to_owned(),
                            workspace: workspace.unwrap_or_default(),
                        },
                        record,
                        &request.target.address.action,
                        arguments,
                    )
                    .await,
                )))
                .encode()
                .map_err(|error| error.to_string());
            }
            let workspace =
                workspace.ok_or("a resource's action is asked in the workspace that keeps it")?;
            let caller = person.in_workspace(workspace);
            let invocation = request.invocation;
            let receipt = workspaces
                .providers()
                .invoke(&caller, request)
                .ok_or("this device applies no action of that resource")?
                .await
                .ok_or("the action stopped before it answered")?
                .unwrap_or_else(|error| tilo_actions::remote::InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Rejected { error },
                });
            RemoteActionMessage::Receipt(receipt)
        }
        message => {
            let (dispatch, admitted) = services.actions.dispatch(caller, workspace.clone()).await?;
            let caller = match (named, workspace) {
                (Some(named), Some(workspace)) => named.in_workspace(workspace),
                (Some(named), None) => named,
                (None, _) => admitted,
            };
            tilo_actions::remote::serve(&dispatch, &caller, message)
                .ok_or("the dispatch answers requests, and that frame is none")?
                .await
                .ok_or("the dispatch dropped the request")?
        }
    };
    RemoteActionFrame::new(answer)
        .encode()
        .map_err(|error| error.to_string())
}

/// The caller the shell names on its link (`named`): an app or an agent acting for the device's
/// person, decided as itself from here on. The
/// shell speaks for its own person alone, so a caller that names another person, another root, a
/// system caller, or that claims this device's own key without being the person, is refused.
fn named_caller(
    named: tilo_actions::remote::KeptCaller,
    device: &tilo_user_identity::DeviceCert,
) -> Result<Caller, String> {
    let caller = named.carried();
    let person = super::person_id_for_root(device.user_public_key);
    let root = &device.user_public_key;
    let acting = caller.acting();
    let named_person = caller
        .acting_for_person()
        .or_else(|| acting.person_id().filter(|person| !person.is_empty()));
    let plain_person = acting.is_person() && acting.app().is_none();
    let theirs = !caller.is_system()
        && caller.user_public_key().is_none_or(|key| key == root)
        && caller.turn_person().is_none_or(|key| key == root)
        && (plain_person || caller.device_public_key() != Some(&device.device_public_key))
        && match named_person {
            Some(named) => named == person,
            None => caller.user_public_key() == Some(root),
        };
    if !theirs {
        return Err("the shell names a caller that does not act for its person".to_owned());
    }
    Ok(caller.in_new_execution())
}

/// Whether the device's services publish `record`, decided for `caller`: `None` when none here
/// publishes it, so the read falls to the workspaces' records; why `caller` may not read it,
/// when one does.
async fn published_revision(
    published: &Arc<dyn PublishedResources>,
    read: &ResourceReadRequest,
) -> Result<Option<u64>, String> {
    let caller = ServiceCaller {
        app_id: read.app.clone(),
        workspace: read.workspace.clone(),
    };
    let asked = published
        .snapshot(&caller, &read.record)
        .await
        .map_err(|_| "the device's read stopped before it answered".to_owned())?;
    match asked {
        Ok((revision, Some(_))) => Ok(Some(revision)),
        Ok((_, None)) => Ok(None),
        Err(reason) => Err(reason),
    }
}

/// A read of a resource the device's services publish: its reading now, or the next one past
/// `revision` when the read waits for it.
async fn published_read(
    published: &Arc<dyn PublishedResources>,
    read: &ResourceReadRequest,
    revision: u64,
) -> Result<String, String> {
    let caller = ServiceCaller {
        app_id: read.app.clone(),
        workspace: read.workspace.clone(),
    };
    let mut current = published
        .snapshot(&caller, &read.record)
        .await
        .map_err(|_| "the device's read stopped before it answered".to_owned())??;
    let waited = read.wait_ms.unwrap_or(0);
    if read.after.as_deref() == Some(revision.to_string().as_str()) && waited > 0 {
        let (sink, mut readings) = tokio::sync::mpsc::unbounded_channel();
        let watch = published.watch(&caller, &read.record, sink)?;
        let waiting = async {
            while let Some(runtime_collab::wire::Event::Reading {
                revision: moved,
                reading,
                ..
            }) = readings.recv().await
            {
                if moved != revision {
                    return Some((moved, reading));
                }
            }
            None
        };
        if let Ok(Some(moved)) =
            tokio::time::timeout(std::time::Duration::from_millis(u64::from(waited)), waiting).await
        {
            current = moved;
        }
        published.unwatch(watch);
    }
    let value: serde_json::Value = match current.1 {
        Some(reading) => serde_json::from_slice(&reading)
            .map_err(|error| format!("the reading does not read: {error}"))?,
        None => serde_json::Value::Null,
    };
    Ok(serde_json::json!({
        "revision": current.0.to_string(),
        "records": [{"record": read.record, "value": value}],
    })
    .to_string())
}

/// Whether the device's services publish `record` for `caller`; why they refuse it to them, when
/// one publishes it and they may not read it.
async fn published_publishes(
    published: &Arc<dyn PublishedResources>,
    caller: &ServiceCaller,
    record: &str,
) -> Result<bool, String> {
    let asked = published
        .snapshot(caller, record)
        .await
        .map_err(|_| "the device's read stopped before it answered".to_owned())?;
    match asked {
        Ok((_, Some(_))) => Ok(true),
        Ok((_, None)) => Ok(false),
        Err(reason) => Err(reason),
    }
}

/// `action` of `record`, a resource the device's services publish, as `caller`: its receipt.
async fn published_action(
    published: &Arc<dyn PublishedResources>,
    caller: &ServiceCaller,
    record: &str,
    action: &str,
    arguments: Vec<u8>,
) -> runtime_collab::wire::Receipt {
    match published.invoke(caller, record, action, arguments).await {
        Ok(receipt) => receipt,
        Err(error) => {
            tracing::warn!(%error, %record, %action, "the device's published action stopped");
            runtime_collab::wire::Receipt::Refused {
                code: "unanswered".to_owned(),
                message: "the device's action stopped before it answered".to_owned(),
            }
        }
    }
}

/// `receipt`, what the device's services answered for `invocation`, as the action wire says it.
fn receipt_of(
    invocation: tilo_actions::InvocationId,
    receipt: runtime_collab::wire::Receipt,
) -> tilo_actions::remote::InvocationReceipt {
    use tilo_actions::remote::ActionCallError;
    let status = match receipt {
        runtime_collab::wire::Receipt::Accepted => ReceiptStatus::Accepted,
        runtime_collab::wire::Receipt::Done(output) => match serde_json::from_slice(&output) {
            Ok(output) => ReceiptStatus::Completed { output },
            Err(error) => ReceiptStatus::Rejected {
                error: ActionCallError::Unavailable {
                    reason: format!("the device's action answered what does not read: {error}"),
                },
            },
        },
        runtime_collab::wire::Receipt::Refused { code, message } => ReceiptStatus::Rejected {
            error: if code == "unknown_action" {
                ActionCallError::UnknownAction
            } else {
                ActionCallError::Refused { reason: message }
            },
        },
    };
    tilo_actions::remote::InvocationReceipt { invocation, status }
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

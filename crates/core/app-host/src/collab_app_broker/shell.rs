//! A shell connection's calls ([`runtime_collab::wire::shell`]), answered from the dispatch's
//! [`ShellServices`] as the person it admitted.

use std::rc::Rc;
use std::sync::Arc;
use tilo_person_services::PersonServices;

use runtime_collab::wire::Reply;
use runtime_collab::wire::shell::{
    DocumentWritten, SHELL_MAGIC, ShellCall, ShellMemberChange, ShellReply,
};
use tilo_actions::remote::{
    Caller, ReceiptStatus, RemoteActionMessage, ResourceReadRequest, resource_parts,
};
use tilo_peer_state::workspace_documents::DocumentWriting;

use crate::services::{Caller as ServiceCaller, PublishedResources};
use crate::shell::{ShellCaller, ShellServices};

/// A connection the daemon's dispatch admitted as a person's shell.
#[derive(Clone)]
pub(super) struct Shell {
    pub(super) caller: ShellCaller,
    /// The seat the shell's link was made for: its account and process, as the kernel named them
    /// when the link was asked for. `None` for a link made for no seat.
    seat: Option<u64>,
    pub(super) services: Arc<ShellServices>,
    /// The writes the device took since the shell last asked, followed from the moment it
    /// connected, so none falls between two asks.
    pub(super) writes: Arc<tokio::sync::Mutex<crate::shell::DocumentWrites>>,
    /// The descriptors the shell handed over on its link: the connections of the apps it vouches
    /// for, and the processes they run in.
    received: runtime_portal::descriptors::ReceivedDescriptors,
    /// Where the daemon hands the shell descriptors on its link: the open objects a call of the
    /// seat names, which only this device's broker forwards.
    sending: runtime_portal::descriptors::DescriptorSender,
    /// Where a connection the shell hands over is served: the dispatch's own broker.
    broker: Rc<Broker>,
}

impl Shell {
    /// How a call of the seat this shell serves hands it a file: queued on the link, ahead of
    /// the call that names it by the number answered.
    pub(super) fn hand_over(&self) -> tilo_workspace_service::workspaces::providers::HandOver {
        let sending = self.sending.clone();
        Arc::new(move |file: std::fs::File| {
            sending.send(runtime_portal::descriptors::Owned::from(file))
        })
    }
}

/// The broker a shell's handed connections are served by, on the dispatch's `LocalSet`.
pub(super) struct Broker {
    pub(super) state: Arc<super::AppBrokerState>,
    pub(super) directories: Rc<super::LocalActionDirectoryRegistry>,
}

/// The most descriptors a shell link holds that no vouch named yet: a vouch hands three at most,
/// and a shell vouches for one app at a time per launch.
const MAX_HANDED: usize = 64;

/// Serve `stream`, which opened with the shell's marker, as the shell its link's secret was made
/// for; a connection whose secret is no link's is refused before its first call.
#[allow(
    clippy::future_not_send,
    reason = "a connection is served on the dispatch's LocalSet, as every other is"
)]
pub(super) async fn serve(
    mut stream: tilo_socket::tokio::Stream,
    services: Arc<ShellServices>,
    broker: Broker,
) -> Result<(), crate::BrokerError> {
    use runtime_collab::wire::shell::SHELL_SECRET_BYTES;
    use tokio::io::AsyncReadExt as _;
    let unread = |error: std::io::Error| {
        crate::BrokerError::failed(format!("a shell link's opening does not read: {error}"))
    };
    let mut marker = [0_u8; SHELL_MAGIC.len()];
    stream.read_exact(&mut marker).await.map_err(unread)?;
    let mut secret = [0_u8; SHELL_SECRET_BYTES];
    stream.read_exact(&mut secret).await.map_err(unread)?;
    let Some((caller, seat)) = services.links.take(&secret) else {
        tracing::warn!("refused a connection that named no shell link the daemon made");
        return Ok(());
    };
    // A seat has one shell: a second link for it is refused, so two shells never act as one
    // person. The link that took the seat lets it go when it ends.
    if let Some(seat) = seat
        && !services.links.began(seat)
    {
        tracing::warn!("refused a second shell link for a seat that has one");
        return Ok(());
    }
    let _seat = SeatLink {
        links: Arc::clone(&services.links),
        seat,
    };
    let followed = Arc::new(tokio::sync::Mutex::new((services.document_writes)()));
    let received = runtime_portal::descriptors::ReceivedDescriptors::new(MAX_HANDED);
    let sending = runtime_portal::descriptors::DescriptorSender::new();
    // Held across the connection: a shell that ends says nothing of its person any more, and is
    // in no call.
    let speaking = services.speaking.clone();
    let (connection, outgoing) = super::wire::WireConnection::shell(Shell {
        caller,
        seat,
        services,
        writes: followed,
        received: received.clone(),
        sending: sending.clone(),
        broker: Rc::new(broker),
    });
    let (reader, writer) = tokio::io::split(runtime_portal::descriptors::DescriptorStream::duplex(
        stream, sending, received,
    ));
    let served = super::wire::serve(connection, outgoing, reader, writer).await;
    if let Some(speaking) = &speaking {
        speaking.gone();
    }
    served.map_err(|error| crate::BrokerError::failed(error.to_string()))
}

/// A shell link's hold on its seat: the seat takes no new link until this one ends.
struct SeatLink {
    links: Arc<crate::shell::ShellLinks>,
    seat: Option<u64>,
}

impl Drop for SeatLink {
    fn drop(&mut self) {
        if let Some(seat) = self.seat {
            self.links.ended(seat);
        }
    }
}

/// A hand-over to the device's tilolang processes for `shell`: the secret admits the account of
/// the seat its link was made for, which the kernel named when the link was asked for on the
/// native endpoint. The hand-over is made there, and admitted by the account the kernel names for
/// that connection.
fn hand_over(shell: &Shell) -> Result<ShellReply, String> {
    if !shell.services.links.serves_processes() {
        return Err("no tilolang process can start on this device".to_owned());
    }
    let seat = shell
        .seat
        .ok_or("this shell link was made for no seat, so no account hands its processes over")?;
    Ok(ShellReply::Processes {
        socket: shell.services.socket.clone(),
        secret: shell.services.links.processes(seat_account(seat)),
    })
}

/// The account of `seat`, which is numbered by its account and process
/// (`tilo_runtime::seats::seat_id`).
fn seat_account(seat: u64) -> u32 {
    u32::try_from(seat >> 32).unwrap_or(u32::MAX)
}

/// An app's connection a shell handed over and vouched for ([`ShellCall::Vouch`]), with the
/// process it pinned: served as that app, as a launch the daemon attested itself would be.
pub(super) struct Handed {
    pub(super) vouch: runtime_collab::wire::shell::VouchedApp,
    pub(super) process: super::app_process::PeerProcess,
}

impl Handed {
    /// What the shell's vouch established about the connection: the app's process, and the
    /// launch its open is tied back to.
    pub(super) fn admitted(self) -> Result<super::socket::AuthenticatedHandoff, String> {
        let Self { vouch, process } = self;
        let launch = crate::services::AuthenticatedApplicationLaunch {
            continuation_instance: vouch.window,
            launch_ticket: vouch.ticket,
            windowless: vouch.windowless,
            opened_for: None,
            opened_with: None,
        };
        let opened = vouch
            .ticket
            .map(|ticket| super::workspace_open::ConnectedLaunch {
                ticket,
                app_id: vouch.app.clone(),
                workspace: vouch.workspace.clone(),
            });
        let local_media = super::app_process::LocalAppConnection::attested_keeping(
            (vouch.app, vouch.workspace, vouch.member),
            process,
            launch,
        )
        .ok_or("the app the shell vouched for has ended")?;
        Ok(super::socket::AuthenticatedHandoff {
            local_media: Some(local_media),
            launch: opened,
        })
    }
}

/// Carry `connection`, which the shell handed over as a link of its live session, into the
/// device's live session until it ends.
fn carry_live(
    shell: &Shell,
    connection: runtime_portal::descriptors::Owned,
) -> Result<ShellReply, String> {
    let live = shell
        .services
        .live
        .clone()
        .ok_or("this device runs no live session")?;
    let stream = tilo_socket::tokio::from_std(handed_stream(connection)?)
        .map_err(|error| format!("the live link cannot be carried: {error}"))?;
    tokio::spawn(live(stream));
    Ok(ShellReply::Vouched)
}

/// Serve the connection `vouch` hands over as the app it names, on `shell`'s broker: the
/// descriptors it numbers are taken off the shell's link.
fn vouch_for(
    shell: &Shell,
    runtime_collab::wire::shell::Vouch {
        connection,
        app,
        to,
    }: runtime_collab::wire::shell::Vouch,
) -> Result<ShellReply, String> {
    let taken = |number: u32, what: &str| {
        shell
            .received
            .take(number)
            .ok_or_else(|| format!("the shell handed no {what} as descriptor {number}"))
    };
    let connection = taken(connection, "connection")?;
    if to == runtime_collab::wire::shell::HandedTo::Live {
        if app.is_some() {
            return Err("a link of the live session is vouched for by no launch".to_owned());
        }
        return carry_live(shell, connection);
    }
    if to == runtime_collab::wire::shell::HandedTo::DeviceVpn {
        if app.is_some() {
            return Err("a device VPN link is vouched for by no launch".to_owned());
        }
        let vpn = shell
            .services
            .vpn
            .clone()
            .ok_or("this device serves no device VPN")?;
        let stream = tilo_socket::tokio::from_std(handed_stream(connection)?)
            .map_err(|error| format!("the device VPN link cannot be carried: {error}"))?;
        tokio::spawn(vpn(stream));
        return Ok(ShellReply::Vouched);
    }
    if to == runtime_collab::wire::shell::HandedTo::Speakers {
        if app.is_some() {
            return Err("a speakers link is vouched for by no launch".to_owned());
        }
        let speakers = shell
            .services
            .speakers
            .clone()
            .ok_or("this device serves no speakers link")?;
        let stream = tilo_socket::tokio::from_std(handed_stream(connection)?)
            .map_err(|error| format!("a speakers link cannot be carried: {error}"))?;
        tokio::spawn(speakers(stream));
        return Ok(ShellReply::Vouched);
    }
    if to == runtime_collab::wire::shell::HandedTo::Api {
        if app.is_some() {
            return Err("a program's API connection is vouched for by no launch".to_owned());
        }
        let api = shell
            .services
            .api
            .as_ref()
            .ok_or("this device serves no API to its person's programs")?;
        api(&shell.caller, handed_stream(connection)?)?;
        return Ok(ShellReply::Vouched);
    }
    if to == runtime_collab::wire::shell::HandedTo::Identity {
        if app.is_some() {
            return Err("an identity connection is vouched for by no launch".to_owned());
        }
        let identity = shell
            .services
            .identity
            .as_ref()
            .ok_or("this device runs no identity service")?;
        identity(handed_stream(connection)?);
        return Ok(ShellReply::Vouched);
    }
    let handed = match app {
        Some(vouch) => {
            if vouch.app.trim().is_empty() || vouch.workspace.trim().is_empty() {
                return Err("a vouch names an empty app or workspace".to_owned());
            }
            let task = taken(vouch.process, "app process")?;
            let launcher = vouch
                .launcher
                .map(|number| taken(number, "launcher process"))
                .transpose()?;
            let process = super::app_process::PeerProcess::handed(task, launcher)
                .ok_or("the app the shell vouched for has ended")?;
            Some(Handed { vouch, process })
        }
        None => None,
    };
    let stream = tilo_socket::tokio::from_std(handed_stream(connection)?)
        .map_err(|error| format!("the app's connection cannot be served: {error}"))?;
    let broker = Rc::clone(&shell.broker);
    let state = Arc::clone(&broker.state);
    let directories = Rc::clone(&broker.directories);
    let peer_id = state.next_peer_id();
    let app = handed
        .as_ref()
        .map_or_else(String::new, |handed| handed.vouch.app.clone());
    tokio::task::spawn_local(async move {
        if let Err(error) =
            super::session::drive_connection(stream, state, directories, peer_id, None, handed)
                .await
        {
            tracing::warn!(%app, "collaboration broker: peer-{peer_id} disconnected: {error}");
        }
    });
    Ok(ShellReply::Vouched)
}

/// The connection a shell handed over as a descriptor.
#[cfg(unix)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "a handle handed on Windows may be no pipe"
)]
fn handed_stream(
    connection: runtime_portal::descriptors::Owned,
) -> Result<tilo_socket::Stream, String> {
    Ok(tilo_socket::Stream::from(connection))
}

/// The connection a shell handed over as a handle: refused when it is no pipe.
#[cfg(windows)]
fn handed_stream(
    connection: runtime_portal::descriptors::Owned,
) -> Result<tilo_socket::Stream, String> {
    tilo_socket::Stream::try_from(connection)
        .map_err(|error| format!("what the shell handed is no connection: {error}"))
}

/// What a shell's call answers, driven on the broker's local set: a shell holds the broker's
/// handles, which stay on it.
pub(super) type ShellAnswer =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Reply, String>>>>;

/// How long a shell's call runs before the daemon says it still does, and the wait doubles each
/// time it says so again.
const SLOW_CALL: std::time::Duration = std::time::Duration::from_secs(5);

/// How much of a call a slow one says it asked.
const ASKING_SHOWN: usize = 300;

/// Answer `call` for `shell`; a connection that is no shell is refused. A call that runs long
/// says what it asked while it runs: every other ask of the shell's that waits on it, and the
/// desktop that waits on those, is found from it.
pub(super) fn answer(shell: Option<Shell>, call: ShellCall) -> ShellAnswer {
    // The shell's follow of the writes it took waits for the next one, however long that is.
    if matches!(call, ShellCall::Writes) {
        return answered(shell, call);
    }
    // A read or an observe that asked to be held until something moves is held at most
    // `MAX_REMOTE_WAIT`: only past that is it slow, so a follow never reads as a stuck call.
    let patience = if asks_to_wait(&call) {
        SLOW_CALL.saturating_add(tilo_actions::remote::MAX_REMOTE_WAIT)
    } else {
        SLOW_CALL
    };
    let asking = asking(&call);
    Box::pin(watched(answered(shell, call), asking, patience))
}

/// Whether `call` is an action that asked its provider to hold the answer until something moves.
fn asks_to_wait(call: &ShellCall) -> bool {
    match call {
        ShellCall::Action { frame, .. } => tilo_actions::remote::RemoteActionFrame::decode(frame)
            .is_ok_and(|frame| frame.message.waits()),
        _ => false,
    }
}

/// `answer`, saying what it asked once it ran past `first`, and every time it ran as long
/// again as it had.
#[expect(
    clippy::future_not_send,
    reason = "a shell's answer runs on the broker's local set, as `ShellAnswer` says"
)]
async fn watched(
    mut answer: ShellAnswer,
    asking: String,
    first: std::time::Duration,
) -> Result<Reply, String> {
    let started = tilo_clock::now();
    let mut patience = first;
    loop {
        if let Ok(answered) = tokio::time::timeout(patience, &mut answer).await {
            if patience > first {
                tracing::warn!(
                    running_ms = running_ms(started),
                    asking = %asking,
                    "a shell's call was answered after a long wait"
                );
            }
            return answered;
        }
        tracing::warn!(
            running_ms = running_ms(started),
            asking = %asking,
            "a shell's call is still running"
        );
        patience = patience.saturating_mul(2);
    }
}

/// How long a call that started at `started` has run, in milliseconds.
fn running_ms(started: tilo_clock::time::Instant) -> u64 {
    let running = tilo_clock::now().saturating_duration_since(started);
    u64::try_from(running.as_millis()).unwrap_or(u64::MAX)
}

/// What `call` asks, as a slow one says it: an action's message rather than its frame's bytes,
/// a write's document rather than its changes, and no more of either than is shown.
fn asking(call: &ShellCall) -> String {
    use std::fmt::Write as _;
    /// Takes what is written up to [`ASKING_SHOWN`] bytes, and refuses the rest.
    struct Start(String);
    impl std::fmt::Write for Start {
        fn write_str(&mut self, written: &str) -> std::fmt::Result {
            let room = ASKING_SHOWN.saturating_sub(self.0.len());
            let mut end = written.len().min(room);
            while !written.is_char_boundary(end) {
                end = end.saturating_sub(1);
            }
            self.0.push_str(written.get(..end).unwrap_or_default());
            if end < written.len() {
                return Err(std::fmt::Error);
            }
            Ok(())
        }
    }
    let mut start = Start(String::new());
    let written = match call {
        ShellCall::Action {
            workspace, frame, ..
        } => match tilo_actions::remote::RemoteActionFrame::decode(frame) {
            Ok(frame) => write!(start, "Action in {workspace:?}: {:?}", frame.message),
            Err(error) => write!(
                start,
                "Action in {workspace:?}: a frame that does not decode: {error}"
            ),
        },
        ShellCall::Write {
            workspace,
            app,
            record,
            ..
        } => write!(start, "Write of {app} {record} in {workspace}"),
        other => write!(start, "{other:?}"),
    };
    // A call longer than what is shown stops the writing there, and says it goes on.
    if written.is_err() {
        start.0.push('…');
    }
    start.0
}

/// [`answer`], unwatched.
fn answered(shell: Option<Shell>, call: ShellCall) -> ShellAnswer {
    Box::pin(async move {
        let shell = shell.ok_or("this connection is not the person's shell")?;
        let services = Arc::clone(&shell.services);
        let reply = match call {
            ShellCall::Readable {
                workspace,
                app,
                resource,
            } => readable_reply(&services, workspace, app, resource).await?,
            ShellCall::Workspaces => ShellReply::Workspaces(services.documents.workspaces()),
            ShellCall::MemberChanges { workspace } => {
                member_changes_reply(&services, workspace).await?
            }
            ShellCall::AgentKey(call) => agent_key_reply(shell.caller, call).await?,
            call @ ShellCall::Write { .. } => write(&services, call).await?,
            ShellCall::Served {
                workspace,
                documents,
            } => served_reply(&services, workspace, documents).await,
            ShellCall::Press { window, serial, at } => {
                // The compositor saw the person's input reach this window and issued `serial` with
                // it: the daemon keeps it, and a call whose app connection is that window runs as
                // the person once when it names it.
                shell.broker.state.note_press(window, serial, at);
                ShellReply::Pressed
            }
            ShellCall::Writes => ShellReply::Writes(next_writes(&shell.writes).await),
            ShellCall::Say {
                workspace,
                word,
                agents,
            } => {
                let speaking = services
                    .speaking
                    .as_ref()
                    .ok_or("this device takes part in no live session")?;
                speaking.say(&workspace, word.map(|word| *word), agents)?;
                ShellReply::Said
            }
            ShellCall::Join { workspace, call } => {
                let speaking = services
                    .speaking
                    .as_ref()
                    .ok_or("this device takes part in no live session")?;
                ShellReply::Joined(speaking.join(&workspace, Some(call)).await)
            }
            ShellCall::Leave { workspace } => {
                let speaking = services
                    .speaking
                    .as_ref()
                    .ok_or("this device takes part in no live session")?;
                ShellReply::Joined(speaking.join(&workspace, None).await)
            }
            call @ (ShellCall::Resources { .. }
            | ShellCall::Vault(_)
            | ShellCall::Receipt { .. }
            | ShellCall::KeepsReceipts { .. }
            | ShellCall::Authorizes { .. }) => Box::pin(kept(&services.workspaces, call)).await,
            ShellCall::Vouch(vouch) => vouch_for(&shell, vouch)?,
            ShellCall::Processes => hand_over(&shell)?,
            ShellCall::Action {
                workspace,
                frame,
                caller,
                pressed,
            } => {
                let frame = tilo_actions::remote::RemoteActionFrame::decode(&frame)
                    .map_err(|error| error.to_string())?;
                // The person's own call, made by the press it names on the shell's own surfaces (a
                // palette's row, a menu's item, a shortcut's key): it runs as their act once the
                // compositor's record of that press proves it, as a window's press proves its app's
                // call. A call for an app or an agent the shell names follows no press of theirs.
                if caller.is_none()
                    && let RemoteActionMessage::Invoke(request) = &frame.message
                {
                    shell
                        .broker
                        .state
                        .shell_pressed((request.invocation, shell.caller.person), frame.press)
                        .await;
                }
                ShellReply::Action(
                    dispatch(
                        &services,
                        &shell.caller,
                        workspace,
                        caller,
                        pressed,
                        frame.message,
                    )
                    .await?,
                )
            }
            // The connection registers what its shell serves itself (`WireConnection::provide`).
            ShellCall::Provide { .. } => {
                return Err("a provider is served on the shell's own connection".to_owned());
            }
        };
        Ok(Reply::Shell(reply))
    })
}

/// What the device's documents say of `app`'s resource `resource` in `workspace`, as their
/// window may read it.
async fn readable_reply(
    services: &ShellServices,
    workspace: String,
    app: String,
    resource: String,
) -> Result<ShellReply, String> {
    let documents = Arc::clone(&services.documents);
    Ok(ShellReply::Is(
        tokio::task::spawn_blocking(move || documents.readable(&workspace, &app, &resource))
            .await
            .map_err(|_| "the read stopped before it answered".to_owned())?,
    ))
}

/// A build that runs no agents makes no agent's key, so it refuses what the shell asks of one.
#[cfg(not(all(feature = "agents", not(target_arch = "wasm32"))))]
fn agent_key_reply(
    _shell: crate::shell::ShellCaller,
    _call: runtime_collab::wire::shell::AgentKeyCall,
) -> std::future::Ready<Result<ShellReply, String>> {
    std::future::ready(Err(
        "this device's workspace service runs no agents, so it holds no agent's key".to_owned(),
    ))
}

/// What the shell asks of the key of an agent its person admitted, answered with the keys this
/// device makes and its person's root certifies ([`crate::agents::agent_keys::this_device`]): a
/// key of the shell's own person's agents alone, which never leaves the device.
#[cfg(all(feature = "agents", not(target_arch = "wasm32")))]
async fn agent_key_reply(
    shell: crate::shell::ShellCaller,
    call: runtime_collab::wire::shell::AgentKeyCall,
) -> Result<ShellReply, String> {
    use runtime_collab::wire::shell::{AgentKeyAnswer, AgentKeyCall};
    let workspace_of = |workspace: &str| {
        tilo_desktop_model::WorkspaceId::parse(workspace)
            .ok_or_else(|| format!("{workspace} names no workspace"))
    };
    let answered = tokio::task::spawn_blocking(move || {
        let keys = crate::agents::agent_keys::this_device()?;
        match call {
            AgentKeyCall::Grant {
                workspace,
                agent,
                name,
                person,
            } => {
                if person != shell.person {
                    return Err("the shell admits agents of its own person only".to_owned());
                }
                keys.grant(workspace_of(&workspace)?, &agent, &name, person)
                    .map(AgentKeyAnswer::Granted)
            }
            AgentKeyCall::Sign {
                workspace,
                key,
                request,
            } => {
                let (cert, _) = keys.made(&key).ok_or(
                    "the agent's key was not made on this device, or its person revoked it",
                )?;
                if cert.user_public_key != shell.person {
                    return Err("the key is no agent's of the shell's person".to_owned());
                }
                let mut request: tilo_actions::remote::InvokeRequest =
                    serde_json::from_slice(&request).map_err(|error| error.to_string())?;
                keys.sign(workspace_of(&workspace)?, &key, &mut request)?;
                serde_json::to_vec(&request)
                    .map(AgentKeyAnswer::Signed)
                    .map_err(|error| error.to_string())
            }
            AgentKeyCall::Revocation { person, key } => keys
                .revocation(&person, &key)?
                .map(|entry| serde_json::to_vec(&entry).map_err(|error| error.to_string()))
                .transpose()
                .map(AgentKeyAnswer::Revocation),
        }
    })
    .await
    .map_err(|_| "the agent's key stopped before it answered".to_owned())?;
    Ok(ShellReply::AgentKey(answered))
}

/// The member changes of `workspace`, as the device's documents read them.
async fn member_changes_reply(
    services: &ShellServices,
    workspace: String,
) -> Result<ShellReply, String> {
    let documents = Arc::clone(&services.documents);
    Ok(ShellReply::MemberChanges(
        tokio::task::spawn_blocking(move || documents.member_changes(&workspace))
            .await
            .map_err(|_| "the read stopped before it answered".to_owned())?
            .map(|changes| {
                changes
                    .into_iter()
                    .map(|change| ShellMemberChange {
                        change: change.change,
                        message: change.message,
                        time: change.time,
                    })
                    .collect()
            }),
    ))
}

/// What the device's writes record of `documents` in `workspace`, once it is served.
async fn served_reply(
    services: &ShellServices,
    workspace: String,
    documents: Vec<(String, String)>,
) -> ShellReply {
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

/// The frame the device's services publish the action of `request` as, where they publish what
/// its provider names, changed by the app the request names as its own action: `None` where they
/// do not, and the resource's provider answers instead.
async fn published_invoke_answer(
    services: &ShellServices,
    workspace: Option<String>,
    request: &tilo_actions::remote::InvokeRequest,
) -> Result<Option<Vec<u8>>, String> {
    use tilo_actions::remote::{InvokeArgument, RemoteActionFrame, RemoteActionMessage};
    let Some(published) = &services.published else {
        return Ok(None);
    };
    let Some((app, record)) = resource_parts(&request.target.provider) else {
        return Ok(None);
    };
    if !published_publishes(
        published,
        &ServiceCaller {
            app_id: app.to_owned(),
            workspace: workspace.clone().unwrap_or_default(),
        },
        record,
    )
    .await?
    {
        return Ok(None);
    }
    let arguments = match &request.argument {
        InvokeArgument::Json(arguments) => serde_json::to_vec(arguments)
            .map_err(|error| format!("the arguments do not encode: {error}"))?,
        InvokeArgument::None => br"{}".to_vec(),
        InvokeArgument::Text(_) => {
            return Err("a published resource's action takes named arguments".into());
        }
    };
    RemoteActionFrame::new(RemoteActionMessage::Receipt(receipt_of(
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
    .map(Some)
    .map_err(|error| error.to_string())
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
    named: Option<Box<tilo_actions::remote::KeptCaller>>,
    pressed: Option<runtime_collab::wire::shell::ShellPress>,
    message: RemoteActionMessage,
) -> Result<Vec<u8>, String> {
    use tilo_actions::remote::RemoteActionFrame;
    let workspaces = &services.workspaces;
    let device = workspaces.device_cert();
    if caller.person != device.user_public_key {
        return Err("this device acts for its own person only".to_owned());
    }
    let named = named
        .map(|named| named_caller(*named, device))
        .transpose()?;
    let person = named
        .clone()
        .unwrap_or_else(|| Caller::authenticated(device.user_public_key, device.device_public_key));
    let answer = match message {
        RemoteActionMessage::ResourceRead {
            correlation, read, ..
        } => {
            if let Some(answer) = published_read_answer(services, correlation, &read).await? {
                return Ok(answer);
            }
            read_answer(
                services,
                workspaces,
                caller,
                named.is_some(),
                &person,
                correlation,
                read,
            )
            .await?
        }
        RemoteActionMessage::Invoke(request)
            if tilo_actions::remote::is_resource_provider(&request.target.provider) =>
        {
            // A resource the device's services publish themselves is changed by the app the
            // request names, as its own action.
            if let Some(answer) =
                published_invoke_answer(services, workspace.clone(), &request).await?
            {
                return Ok(answer);
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
            let (dispatch, admitted) = services
                .actions
                .dispatch(caller, workspace.clone(), pressed)
                .await?;
            let caller = match (named, workspace) {
                (Some(named), Some(workspace)) => named.in_workspace(workspace),
                (Some(named), None) => named,
                (None, _) => admitted,
            };
            tilo_action_calls::serve(&dispatch, &caller, message)
                .ok_or("the dispatch answers requests, and that frame is none")?
                .await
                .ok_or("the dispatch dropped the request")?
        }
    };
    RemoteActionFrame::new(answer)
        .encode()
        .map_err(|error| error.to_string())
}

/// The frame the device's services publish `read` as, where they publish it (this device's
/// server settings), read as the app the read names: `None` where they do not, and the device's
/// reader of its workspaces answers instead.
async fn published_read_answer(
    services: &ShellServices,
    correlation: u64,
    read: &ResourceReadRequest,
) -> Result<Option<Vec<u8>>, String> {
    use tilo_actions::remote::{ActionCallError, RemoteActionFrame, RemoteActionMessage};
    if read.app == tilo_actions::remote::DEVICE_SETTINGS {
        return Ok(None);
    }
    let Some(published) = &services.published else {
        return Ok(None);
    };
    let Some(revision) = published_revision(published, read).await? else {
        return Ok(None);
    };
    RemoteActionFrame::new(RemoteActionMessage::ResourceReading {
        correlation,
        result: published_read(published, read, revision)
            .await
            .map_err(|reason| ActionCallError::Unavailable { reason }),
    })
    .encode()
    .map(Some)
    .map_err(|error| error.to_string())
}

/// The answer to reading `read` as `person`: by the device's reader of its workspaces, or the
/// dispatch's for an app or an agent acting for the person (`named`, `caller`), which decides
/// each call as the one actor it decides its calls as.
async fn read_answer(
    services: &ShellServices,
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    caller: &ShellCaller,
    named: bool,
    person: &Caller,
    correlation: u64,
    read: ResourceReadRequest,
) -> Result<tilo_actions::remote::RemoteActionMessage, String> {
    let reads: Arc<dyn tilo_actions::remote::ResourceReads> = if named {
        let (dispatch, _) = services
            .actions
            .dispatch(caller, Some(read.workspace.clone()), None)
            .await?;
        dispatch
            .resource_reads()
            .ok_or("this device's dispatch reads no records")?
    } else {
        workspaces.resource_reads()
    };
    let reader = person.clone().in_workspace(read.workspace.clone());
    Ok(reads.answer_read(&reader, correlation, read).await)
}

/// The caller the shell names on its link (`named`): the device's person acting through an app,
/// or an agent acting for them, decided as itself from here on. The
/// shell speaks for its own person alone, so a caller that names another person, another root, a
/// system caller, or that claims this device's own key without being the person, is refused. So
/// is one acted by anything but a person or an agent: an app acting by itself is the one its
/// device's daemon launched, never one the shell names, and nothing borrows an app's grants by
/// naming it.
fn named_caller(
    named: tilo_actions::remote::KeptCaller,
    device: &tilo_user_identity::DeviceCert,
) -> Result<Caller, String> {
    let caller = named.carried();
    let member = caller
        .acting()
        .member_id()
        .is_some_and(|member| member.is_person() || member.is_agent());
    if !member {
        return Err(format!(
            "the shell names {} as a caller, and it speaks only for a person or an agent",
            caller.acting().by
        ));
    }
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

/// How long a read asking to wait `wait_ms` is held: never past [`MAX_REMOTE_WAIT`], so a follow
/// asking for longer is answered with the reading as it stands and asks again.
///
/// [`MAX_REMOTE_WAIT`]: tilo_actions::remote::MAX_REMOTE_WAIT
fn held_for(wait_ms: u32) -> std::time::Duration {
    std::time::Duration::from_millis(u64::from(wait_ms)).min(tilo_actions::remote::MAX_REMOTE_WAIT)
}

/// A read of a resource the device's services publish: its reading now, or the next one past
/// `revision` when the read waits for it.
pub(super) async fn published_read(
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
        if let Ok(Some(moved)) = tokio::time::timeout(held_for(waited), waiting).await {
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
        kind,
        record,
        changes,
    } = call
    else {
        return Err("that is no write".to_owned());
    };
    let written = services
        .writes
        .write(
            &workspace,
            &changes,
            DocumentWriting {
                app: &app,
                kind: kind.as_deref(),
                record: &record,
            },
        )
        .await;
    Ok(ShellReply::Written(match written {
        Ok(()) => Ok(()),
        Err(refusal) => Err(serde_json::to_string(&refusal)
            .map_err(|error| format!("a refusal does not encode: {error}"))?),
    }))
}

/// The writes `writes` took since it was last asked, waiting for the first; `None` once it missed
/// some, so the shell reads again everything it follows.
async fn next_writes(
    writes: &tokio::sync::Mutex<crate::shell::DocumentWrites>,
) -> Option<Vec<DocumentWritten>> {
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};
    let told = |write: tilo_workspace_service::workspaces::ResourceWrite| DocumentWritten {
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
async fn kept(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    call: ShellCall,
) -> ShellReply {
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
            Box::pin(vault(workspaces, call))
                .await
                .map(runtime_collab::wire::shell::VaultAnswer),
        ),
        _ => ShellReply::Resources(Err("that is no resource command or vault ask".to_owned())),
    }
}

/// `command` run on `app`'s resources in `workspace` as the device's person, answered as the
/// store's reply. A step the session stamped as serving an invocation (an agent's, set through
/// a setter) is that invoker's.
async fn resources(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
    app: &str,
    command: &str,
) -> Result<String, String> {
    let command: collab_types::store::wire::Command = serde_json::from_str(command)
        .map_err(|error| format!("the resource command does not read: {error}"))?;
    let serving = invokers(&command);
    let reply = workspaces
        .own_resource_command(workspace, app, command, serving)
        .await?;
    serde_json::to_string(&reply).map_err(|error| error.to_string())
}

/// The invocations `command`'s steps were stamped as serving, with who invoked each and how, as
/// the session recorded it.
fn invokers(
    command: &collab_types::store::wire::Command,
) -> Vec<(tilo_actions::InvocationId, tilo_actions::Acting)> {
    use collab_types::store::wire::Command;
    let stamp = match command {
        Command::Put { stamp, .. }
        | Command::Remove { stamp, .. }
        | Command::Undo { stamp, .. }
        | Command::Edit { stamp, .. }
        | Command::Checkpoint { stamp, .. }
        | Command::Restore { stamp, .. } => stamp,
        Command::Batch(commands) => return commands.iter().flat_map(invokers).collect(),
        _ => return Vec::new(),
    };
    let invoker = stamp.action_context().invoker.map(|invoker| invoker.acting);
    match (uuid::Uuid::parse_str(&stamp.invocation), invoker) {
        (Ok(invocation), Some(acting)) => vec![(tilo_actions::InvocationId(invocation), acting)],
        _ => Vec::new(),
    }
}

/// `call` asked of its workspace's vault and accounts.
async fn vault(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    call: runtime_collab::wire::shell::VaultCall,
) -> Result<String, String> {
    use runtime_collab::wire::shell::VaultCall;
    use tilo_actions::remote::Caller;
    use tilo_actions::remote::ItemRef;
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
            let kept = PersonServices::of(workspaces)
                .keep_account_password(
                    &workspace,
                    (&by, None),
                    item.as_ref(),
                    tilo_person_services::account_logins::AccountLogin {
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
            let released = PersonServices::of(workspaces)
                .account_secret(&workspace, &by, &app, &item)
                .await?;
            Ok(String::clone(&released.secret))
        }
        VaultCall::ReportSync {
            workspace,
            person,
            device,
            account,
            health,
            signed_in,
            message,
        } => {
            let health = accounts::SyncHealth::from_word(&health)
                .ok_or_else(|| format!("{health} is no account's sync health"))?;
            let by = Caller::authenticated(person, device).in_workspace(workspace.clone());
            Box::pin(PersonServices::of(workspaces).report_account_sync(
                (&workspace, by.acting(), None),
                &account,
                health,
                signed_in,
                &message,
            ))
            .await?;
            Ok(account)
        }
        VaultCall::WebsiteSession {
            workspace,
            person,
            command,
        } => {
            let reply = Box::pin(
                PersonServices::of(workspaces).website_session_ask(&workspace, person, command),
            )
            .await?;
            serde_json::to_string(&reply).map_err(|error| error.to_string())
        }
    }
}

#[cfg(test)]
mod named_caller_tests {
    use tilo_actions::remote::{Caller, KeptCaller};

    fn device() -> tilo_user_identity::DeviceCert {
        tilo_user_identity::DeviceCert {
            user_public_key: [3; 32],
            device_public_key: [4; 32],
            device_label: "laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        }
    }

    /// A caller crafted on the shell's link to be an app acting by itself, for the device's
    /// person, is refused: the shell cannot lend any app's grants to whoever it names.
    #[test]
    fn a_shell_naming_an_app_by_itself_is_refused() {
        let device = device();
        let person = super::super::person_id_for_root(device.user_public_key);
        let mut wire = serde_json::to_value(Caller::local_app("so.tilo.chat").kept())
            .expect("a kept caller is written");
        wire["acting"]["for"] =
            serde_json::to_value(vec![tilo_member_types::Acting::person(person.clone())])
                .expect("an acting is written");
        let crafted: KeptCaller = serde_json::from_value(wire).expect("the link reads it");
        let refused = super::named_caller(crafted, &device)
            .expect_err("an app acting by itself is never one the shell names");
        assert!(refused.contains("person or an agent"), "{refused}");

        let through = Caller::local_app("so.tilo.chat").acting_for(person).kept();
        assert!(
            super::named_caller(through, &device).is_ok(),
            "the person acting through an app is named"
        );
    }
}

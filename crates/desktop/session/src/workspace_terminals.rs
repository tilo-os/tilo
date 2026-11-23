//! A Terminal window's shell in a workspace a server hosts, opened where the server runs the
//! workspace's shells and carried to this device.
//!
//! The device's shell runtime asks on the MCP socket ([`runtime_portal::workspace_terminal`]):
//! the window's workspace, the Terminal session record it shows, its size, and the host's
//! session to come back to. A workspace no host other than this device runs is answered
//! [`Reply::Local`] at once. For a hosted one this session, which holds the device's key and its
//! connection to the host, invokes the record's `open-terminal` there as the person, the same
//! way the person's other actions in the workspace go to its host: the owner's runs at once, and
//! a key or a member's grant is asked of the person on their devices first, which the open waits
//! for. The host answers a one-use stream id and its session's id; this device dials the host's
//! terminal streams from its own endpoint, as itself, and the runtime's connection is that stream
//! from [`Reply::Opened`] on, carried byte for byte until either end closes.

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use runtime_portal::workspace_terminal::{ACTION, ALPN, RESOURCE, Reply, Request, is_session_id};
use tilo_actions::remote::{
    Caller, InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};

use crate::hosted_workspaces::{HostedWorkspaces, TerminalHost};

/// The longest an open waits for the host: long enough for the person to see the question on
/// another device and answer it.
const OPEN_WITHIN: Duration = Duration::from_secs(10 * 60);

/// How long the host holds one answer to the open before saying it still runs.
const WAIT_MS: u32 = 10_000;

/// Answer `request`, which the runtime asked on `stream`, on a thread of its own for as long as
/// the shell's stream is carried.
pub(crate) fn open(stream: UnixStream, request: Request, hosting: Option<HostedWorkspaces>) {
    let spawned = std::thread::Builder::new()
        .name("tilo-workspace-terminal".into())
        .spawn(move || serve(stream, &request, hosting.as_ref()));
    if let Err(error) = spawned {
        tracing::warn!(%error, "a workspace's terminal could not be opened");
    }
}

fn serve(mut stream: UnixStream, request: &Request, hosting: Option<&HostedWorkspaces>) {
    let Some(host) = hosting.and_then(|hosting| hosting.terminal_host(&request.workspace)) else {
        // The asker may have gone; its terminal answer then has nowhere to land.
        tilo_failure::ignored!(
            "the asker of a workspace's terminal has gone before its answer",
            stream.write_all(&Reply::Local.line()).ok()
        );
        return;
    };
    if stream.write_all(&Reply::Hosted.line()).is_err() {
        return;
    }
    let runtime = host.runtime.clone();
    let opened = runtime.block_on(async {
        let (stream_id, session) = open_on(&host, request).await?;
        let endpoint = host.endpoint.clone().ok_or_else(|| {
            "this device takes no part in the peer network yet, so it reaches no host".to_owned()
        })?;
        let connection = endpoint
            .connect(host.address.clone(), ALPN)
            .await
            .map_err(|error| format!("the workspace's host could not be reached: {error}"))?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|error| format!("the workspace's host took no terminal stream: {error}"))?;
        Ok::<_, String>((stream_id, session, connection, send, recv))
    });
    let (stream_id, session, connection, send, recv) = match opened {
        Ok(opened) => opened,
        Err(reason) => {
            tracing::info!(workspace = %request.workspace, %reason, "a workspace's terminal did not open");
            tilo_failure::ignored!(
                "the asker of a workspace's terminal has gone before its refusal",
                stream.write_all(&Reply::Refused { reason }.line()).ok()
            );
            return;
        }
    };
    if stream
        .write_all(
            &Reply::Opened {
                stream: stream_id,
                session,
            }
            .line(),
        )
        .is_err()
    {
        return;
    }
    tracing::info!(workspace = %request.workspace, "a workspace's terminal is carried from its host");
    runtime.block_on(async {
        let local = stream
            .set_nonblocking(true)
            .and_then(|()| tokio::net::UnixStream::from_std(stream));
        match local {
            Ok(mut local) => {
                // Whatever ends on one side ends on the other: a closed window ends the shell's
                // stream, and a host that closes it ends the window's.
                let mut carried = tokio::io::join(recv, send);
                let _ = tokio::io::copy_bidirectional(&mut carried, &mut local).await;
            }
            Err(error) => tracing::warn!(%error, "a workspace's terminal could not be carried"),
        }
        connection.close(0_u32.into(), b"the terminal closed");
    });
}

/// Ask `host` to open `request`'s session shell on a terminal for this device, waiting while
/// it waits for the person: the stream's one-use id and the host's session id.
async fn open_on(host: &TerminalHost, request: &Request) -> Result<(String, String), String> {
    let caller = Caller::local_person("tilo");
    let mut arguments = serde_json::Map::new();
    arguments.insert("cols".into(), request.cols.into());
    arguments.insert("rows".into(), request.rows.into());
    if let Some(attach) = &request.attach {
        arguments.insert("attach".into(), attach.clone().into());
    }
    let invocation = InvocationId::new();
    let asked = InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: format!(
                "{}:{}:{}",
                tilo_actions::remote::RESOURCE_PROVIDER,
                tilo_app_id::apps::TERMINAL,
                request.record
            ),
            address: ActionAddress::new(RESOURCE, ACTION),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: Some(WAIT_MS),
        appearance: None,
        signed: None,
    };
    let receipt = host
        .actions
        .invoke(&caller, asked)
        .await
        .ok_or_else(|| "the workspace's host did not answer".to_owned())?
        .map_err(|error| format!("the workspace's host refused the terminal: {error}"))?;
    let output = match receipt {
        InvocationReceipt {
            status: ReceiptStatus::Completed { output },
            ..
        } => output,
        InvocationReceipt {
            status: ReceiptStatus::Accepted,
            ..
        } => settled(host, &caller, invocation).await?,
        InvocationReceipt { status, .. } => return Err(said(&status)),
    };
    let value = output.value.unwrap_or_default();
    let text = |name: &str| value.get(name).and_then(serde_json::Value::as_str);
    match (text("stream"), text("session")) {
        (Some(stream), Some(session)) if is_session_id(session) && !stream.is_empty() => {
            Ok((stream.to_owned(), session.to_owned()))
        }
        _ => Err(format!(
            "the workspace's host answered the terminal with something else: {value}"
        )),
    }
}

/// What `invocation`, still running on `host`, settles with, within [`OPEN_WITHIN`].
async fn settled(
    host: &TerminalHost,
    caller: &Caller,
    invocation: InvocationId,
) -> Result<tilo_actions::ActionOutput, String> {
    let until = tilo_clock::now()
        .checked_add(OPEN_WITHIN)
        .unwrap_or_else(tilo_clock::now);
    loop {
        let state =
            tilo_actions::remote::settle_with(&host.actions, caller, invocation, None, until)
                .await
                .flatten();
        match state.map(|state| state.update) {
            Some(InvocationUpdate::Succeeded { output, .. }) => return Ok(output),
            Some(InvocationUpdate::Failed { error, .. }) => return Err(error.message),
            Some(InvocationUpdate::Cancelled { .. }) => {
                return Err("the terminal's open was cancelled".to_owned());
            }
            Some(_) if tilo_clock::now() < until => {}
            _ => return Err("the workspace's host did not open the terminal in time".to_owned()),
        }
    }
}

fn said(status: &ReceiptStatus) -> String {
    match status {
        ReceiptStatus::Failed { error } => error.message.clone(),
        ReceiptStatus::Rejected { error } => error.to_string(),
        ReceiptStatus::Cancelled { message } => message
            .clone()
            .unwrap_or_else(|| "the terminal's open was cancelled".to_owned()),
        ReceiptStatus::Accepted | ReceiptStatus::Completed { .. } => {
            "the workspace's host answered the terminal with something else".to_owned()
        }
    }
}

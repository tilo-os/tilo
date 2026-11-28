//! What the device's shell runtime (`ush`) asks its daemon: Terminal's environment records, and a
//! hosted workspace's Terminal shell carried to this device.
//!
//! The runtime is one of the person's own programs on this computer, and asks at the daemon's
//! native endpoint as they do ([`tilo_device_identity::login::daemon::DeviceActions`]); nothing
//! else is answered these. The app is Terminal, fixed here and never the asker's to name, and the
//! workspace is always the one the asking Terminal window runs for: a request naming none is
//! refused, never carried out in a workspace guessed for it.
//!
//! - [`TERMINAL_RECORDS`]: the runtime keeps each environment as one of Terminal's documents in
//!   its workspace. Only a record's changes, a record's write and the record list are carried out,
//!   as the device's person, so the daemon keeps and syncs them as it does every app's. Each
//!   answer names that person (`person`), whom the runtime's changes name as their maker.
//! - [`OPEN_TERMINAL`]: a workspace another device runs (a home server, a cloud host) runs its
//!   Terminal sessions' shells there. The session record's `open-terminal` is asked of the host
//!   through this device's dispatch, which asks the person first where the workspace's policy says
//!   so, and the host's terminal stream ([`workspace_terminal::ALPN`]) is dialled from the
//!   device's one endpoint. The runtime is handed one end of a socket pair at once; the host's
//!   answer ([`workspace_terminal::Reply`]) and then its stream follow on it, byte for byte, until
//!   either end closes.

use std::sync::{Arc, Mutex};
#[cfg(unix)]
use std::time::Duration;

use collab_types::store::wire::Command;
use runtime_portal::workspace_terminal;
#[cfg(unix)]
use runtime_portal::workspace_terminal::Reply;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, InvocationState, InvokeArgument,
    InvokeRequest, Pending,
};
#[cfg(unix)]
use tilo_actions::remote::{ReceiptStatus, RemoteActionTarget};
#[cfg(unix)]
use tilo_actions::{ActionAddress, InvocationUpdate};
use tilo_actions::{ActionDescriptor, ActionParam, InvocationId, ParamType};
use tilo_device_identity::login::{OPEN_TERMINAL, PROVIDER, RESOURCE, TERMINAL_RECORDS};
use tilo_workspace_service::workspaces::{Workspaces, standing};

use super::ticket::{failed_as, succeeded};
use super::workspaces::ServiceWorkspaces;

/// The app whose documents the environments are, and whose session records the shells are.
const TERMINAL: &str = tilo_app_id::apps::TERMINAL;
/// The kind of Terminal's records an environment is one of (Terminal's `tilo-app.toml`).
const ENVIRONMENT: &str = "environment";

/// The code these actions are refused with.
const REFUSED: &str = "shell-runtime";

/// The longest an open waits for the host: long enough for the person to see the question on
/// another device and answer it.
#[cfg(unix)]
const OPEN_WITHIN: Duration = Duration::from_secs(10 * 60);

/// How long the host holds one answer to the open before saying it still runs.
#[cfg(unix)]
const WAIT_MS: u32 = 10_000;

/// Whether `request` is one the shell runtime asks.
pub(crate) fn asks(request: &InvokeRequest) -> bool {
    request.target.provider == PROVIDER
        && request.target.address.resource == RESOURCE
        && [TERMINAL_RECORDS, OPEN_TERMINAL].contains(&request.target.address.action.as_str())
}

/// These actions, as the device's catalogue describes them.
pub(crate) fn descriptors() -> Vec<ActionDescriptor> {
    use super::ticket::descriptor;
    let workspace = ActionParam::required(
        "workspace",
        ParamType::Text,
        "The workspace the Terminal window runs for.",
    );
    vec![
        descriptor(
            TERMINAL_RECORDS,
            "Read or write the environment records Terminal keeps in a workspace.",
            &[
                workspace.clone(),
                ActionParam::required(
                    "command",
                    ParamType::Object,
                    "The record command, as the workspace's store reads it.",
                ),
            ],
        ),
        descriptor(
            OPEN_TERMINAL,
            "Carry the shell of a Terminal session in a hosted workspace to this device.",
            &[
                workspace,
                ActionParam::optional("record", ParamType::Text, "The Terminal session record."),
                ActionParam::required("cols", ParamType::Integer, "The terminal's columns."),
                ActionParam::required("rows", ParamType::Integer, "The terminal's rows."),
                ActionParam::optional(
                    "attach",
                    ParamType::Text,
                    "The host's session to come back to.",
                ),
            ],
        ),
    ]
}

/// What the shell runtime's ask needs of the daemon.
pub(crate) struct Asked<'a, H> {
    /// The workspaces this device holds.
    pub(crate) workspaces: &'a Arc<ServiceWorkspaces>,
    /// This device's dispatch, which takes a hosted workspace's actions to its host.
    pub(crate) dispatch: H,
    /// The person the runtime acts for.
    pub(crate) person: Caller,
    /// Where a descriptor the answer carries waits to be written with it: the asking connection's.
    pub(crate) handed: Option<&'a Mutex<Option<runtime_portal::process_link::Descriptor>>>,
}

/// Answer `request`, one the shell runtime asks ([`asks`]).
pub(crate) fn answer<H>(
    asked: &Asked<'_, H>,
    request: &InvokeRequest,
) -> Pending<Result<InvocationReceipt, ActionCallError>>
where
    H: ActionProvider + Clone + Send + Sync + 'static,
{
    let invocation = request.invocation;
    let InvokeArgument::Json(args) = &request.argument else {
        return refused(invocation, "the shell runtime asks with its arguments");
    };
    let Some(workspace) = args
        .get("workspace")
        .and_then(Value::as_str)
        .filter(|workspace| !workspace.trim().is_empty())
    else {
        return refused(
            invocation,
            "Terminal's environments and shells are a workspace's, and this names none: open \
             Terminal in a workspace",
        );
    };
    match request.target.address.action.as_str() {
        TERMINAL_RECORDS => records(
            asked.workspaces.copies().clone(),
            invocation,
            workspace,
            args,
        ),
        _ => open_terminal(asked, invocation, args),
    }
}

/// Carry out the record command `args` names on Terminal's documents in `workspace`.
fn records(
    copies: Workspaces,
    invocation: InvocationId,
    workspace: &str,
    args: &Map<String, Value>,
) -> Pending<Result<InvocationReceipt, ActionCallError>> {
    let command = match args
        .get("command")
        .cloned()
        .map(serde_json::from_value::<Command>)
    {
        Some(Ok(command)) => command,
        Some(Err(error)) => {
            return refused(
                invocation,
                &format!("that is no command of the workspace's store: {error}"),
            );
        }
        None => return refused(invocation, "a record command names its command"),
    };
    // Terminal's own records, and nothing else of the workspace's.
    if !matches!(
        command,
        Command::RecordChanges {
            record: Some(_),
            ..
        } | Command::RecordChange {
            record: Some(_),
            commit: None,
            ..
        } | Command::Records
    ) {
        return refused(
            invocation,
            "only Terminal's environment records are read and written here",
        );
    }
    let workspace = workspace.to_owned();
    // Whom the records are written as: the runtime's changes name the device's person, as the
    // workspace takes only a change naming its caller.
    let person = hex::encode(copies.device_person());
    let (done, answer) = tilo_actions::remote::pending();
    tilo_rt::app().handle().spawn(async move {
        // A first write can wait for this device's keys of the workspace.
        let update = match copies
            .own_kind_command(
                &workspace,
                (TERMINAL, Some(ENVIRONMENT)),
                command,
                Vec::new(),
            )
            .await
            .and_then(|reply| serde_json::to_value(reply).map_err(|error| error.to_string()))
        {
            Ok(reply) => succeeded(invocation, json!({ "reply": reply, "person": person })),
            Err(reason) => failed_as(invocation, REFUSED, &reason),
        };
        done.complete(Ok(InvocationState::new(invocation, update).receipt()));
    });
    answer
}

/// Open the shell `args` names: at once `hosted: false` for a workspace no other device runs, or
/// `hosted: true` with one end of a socket pair handed beside it, on which the host's answer and
/// stream follow.
fn open_terminal<H>(
    asked: &Asked<'_, H>,
    invocation: InvocationId,
    args: &Map<String, Value>,
) -> Pending<Result<InvocationReceipt, ActionCallError>>
where
    H: ActionProvider + Clone + Send + Sync + 'static,
{
    let Some(request) = workspace_terminal::Request::of(&Value::Object(args.clone())) else {
        return refused(invocation, "that names no Terminal shell a host opens");
    };
    let own = asked.workspaces.device_cert().device_public_key;
    let host = tilo_desktop_model::WorkspaceId::parse(&request.workspace)
        .and_then(|id| asked.workspaces.peer(id))
        .and_then(|peer| standing::host_elsewhere(&peer, &own));
    let Some(host) = host else {
        return answered(invocation, json!({ "hosted": false }));
    };
    #[cfg(unix)]
    {
        let Some(handed) = asked.handed else {
            return refused(
                invocation,
                "a hosted Terminal shell is handed over this computer's own socket alone",
            );
        };
        let (ours, theirs) = match std::os::unix::net::UnixStream::pair() {
            Ok(pair) => pair,
            Err(error) => return refused(invocation, &error.to_string()),
        };
        *tilo_lock::MutexRecover::lock_recover(handed) = Some(theirs.into());
        let carrying = Carrying {
            dispatch: asked.dispatch.clone(),
            person: asked.person.clone().in_workspace(request.workspace.clone()),
            endpoint: asked.workspaces.endpoint(),
            address: host.address,
            request,
        };
        tilo_rt::app().handle().spawn(carrying.carry(ours));
        answered(invocation, json!({ "hosted": true }))
    }
    #[cfg(not(unix))]
    {
        // Carrying the shell is what the runtime's dispatch, its person and the descriptor it is
        // handed are for: this platform carries none.
        let _ = (host, &asked.dispatch, &asked.person, asked.handed);
        refused(
            invocation,
            "a hosted Terminal shell is carried on Linux and macOS alone",
        )
    }
}

/// A hosted workspace's Terminal shell, on its way to this device.
#[cfg(unix)]
struct Carrying<H> {
    dispatch: H,
    person: Caller,
    endpoint: iroh::Endpoint,
    address: iroh::EndpointAddr,
    request: workspace_terminal::Request,
}

#[cfg(unix)]
impl<H: ActionProvider + Clone + Send + Sync + 'static> Carrying<H> {
    /// Ask the host for the shell, tell the runtime on `ours` what it answered, and carry the
    /// host's stream there until either end closes.
    async fn carry(self, ours: std::os::unix::net::UnixStream) {
        use tokio::io::AsyncWriteExt as _;
        let workspace = self.request.workspace.clone();
        let local = ours
            .set_nonblocking(true)
            .and_then(|()| tokio::net::UnixStream::from_std(ours));
        let mut local = match local {
            Ok(local) => local,
            Err(error) => {
                tracing::warn!(%error, "a workspace's terminal could not be carried");
                return;
            }
        };
        let opened = self.open().await;
        let (reply, carried) = match opened {
            Ok((stream, session, carried)) => (Reply::Opened { stream, session }, Some(carried)),
            Err(reason) => {
                tracing::info!(%workspace, %reason, "a workspace's terminal did not open");
                (Reply::Refused { reason }, None)
            }
        };
        // A runtime that left before the answer has no terminal to carry.
        if local.write_all(&reply.line()).await.is_err() {
            return;
        }
        let Some((connection, send, recv)) = carried else {
            return;
        };
        tracing::info!(%workspace, "a workspace's terminal is carried from its host");
        // Whatever ends on one side ends on the other: a closed window ends the shell's stream,
        // and a host that closes it ends the window's.
        let mut stream = tokio::io::join(recv, send);
        let _ = tokio::io::copy_bidirectional(&mut stream, &mut local).await;
        connection.close(0_u32.into(), b"the terminal closed");
    }

    /// Ask the host to open the session's shell for this device, and dial its terminal stream:
    /// the stream's one-use id, the host's session id, and the stream.
    async fn open(
        &self,
    ) -> Result<
        (
            String,
            String,
            (
                iroh::endpoint::Connection,
                iroh::endpoint::SendStream,
                iroh::endpoint::RecvStream,
            ),
        ),
        String,
    > {
        let (stream, session) = self.open_on_host().await?;
        let connection = self
            .endpoint
            .connect(self.address.clone(), workspace_terminal::ALPN)
            .await
            .map_err(|error| format!("the workspace's host could not be reached: {error}"))?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|error| format!("the workspace's host took no terminal stream: {error}"))?;
        Ok((stream, session, (connection, send, recv)))
    }

    /// The session record's `open-terminal`, asked of the host through this device's dispatch,
    /// waiting while it waits for the person: the stream's one-use id and the host's session id.
    async fn open_on_host(&self) -> Result<(String, String), String> {
        let request = &self.request;
        let mut arguments = Map::new();
        arguments.insert("cols".into(), request.cols.into());
        arguments.insert("rows".into(), request.rows.into());
        if let Some(attach) = &request.attach {
            arguments.insert("attach".into(), attach.clone().into());
        }
        let invocation = InvocationId::new();
        let asked = InvokeRequest {
            asks: false,
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: TERMINAL.to_owned(),
                address: ActionAddress::new(
                    tilo_actions::remote::record_path(
                        workspace_terminal::RESOURCE,
                        &request.record,
                    ),
                    workspace_terminal::ACTION,
                ),
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: Some(WAIT_MS),
            appearance: None,
            signed: None,
        };
        let receipt = self
            .dispatch
            .invoke(&self.person, asked)
            .await
            .ok_or_else(|| "the workspace's host did not answer".to_owned())?
            .map_err(|error| format!("the workspace's host refused the terminal: {error}"))?;
        let output = match receipt.status {
            ReceiptStatus::Completed { output } => output,
            ReceiptStatus::Accepted => self.settled(invocation).await?,
            status => return Err(said(&status)),
        };
        let value = output.value.unwrap_or_default();
        let text = |name: &str| value.get(name).and_then(Value::as_str);
        match (text("stream"), text("session")) {
            (Some(stream), Some(session))
                if workspace_terminal::is_session_id(session) && !stream.is_empty() =>
            {
                Ok((stream.to_owned(), session.to_owned()))
            }
            _ => Err(format!(
                "the workspace's host answered the terminal with something else: {value}"
            )),
        }
    }

    /// What `invocation`, still running on the host, settles with, within [`OPEN_WITHIN`].
    async fn settled(
        &self,
        invocation: InvocationId,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let until = tilo_clock::now()
            .checked_add(OPEN_WITHIN)
            .unwrap_or_else(tilo_clock::now);
        loop {
            let state = tilo_action_calls::settle_with(
                &self.dispatch,
                &self.person,
                invocation,
                None,
                until,
            )
            .await
            .flatten();
            match state.map(|state| state.update) {
                Some(InvocationUpdate::Succeeded { output, .. }) => return Ok(output),
                Some(InvocationUpdate::Failed { error, .. }) => return Err(error.message),
                Some(InvocationUpdate::Cancelled { .. }) => {
                    return Err("the terminal's open was cancelled".to_owned());
                }
                Some(_) if tilo_clock::now() < until => {}
                _ => {
                    return Err("the workspace's host did not open the terminal in time".to_owned());
                }
            }
        }
    }
}

/// What a receipt that is no answer says.
#[cfg(unix)]
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

/// `invocation` answered with `value` at once, and kept nowhere: the runtime's steps are never
/// acts the person's history shows.
fn answered(
    invocation: InvocationId,
    value: Value,
) -> Pending<Result<InvocationReceipt, ActionCallError>> {
    Pending::ready(Ok(InvocationState::new(
        invocation,
        succeeded(invocation, value),
    )
    .receipt()))
}

/// `invocation` refused, in words.
fn refused(
    invocation: InvocationId,
    reason: &str,
) -> Pending<Result<InvocationReceipt, ActionCallError>> {
    Pending::ready(Ok(InvocationState::new(
        invocation,
        failed_as(invocation, REFUSED, reason),
    )
    .receipt()))
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) use client::install;
#[cfg(target_os = "linux")]
pub(crate) use client::{ShellRuntime, shell_runtime};

/// What the daemon asks the shell runtime it runs, through one handle every provider shares.
///
/// The supervisor owns the workspace's environment records and answers their verbs; a provider
/// that publishes them to apps (the workspace's environments) asks here, and so does anything
/// else on the device that reaches the same runtime, so no provider makes a connection of its
/// own. A request is one line and its answer the line after, on the daemon's one connection to the
/// runtime ([`super::ush_line`]), which the shells apps open are carried on too.
#[cfg(target_os = "linux")]
mod client {
    use std::sync::{Arc, OnceLock};

    use serde_json::Value;
    use tilo_actions::remote::ActionCallError;
    use ush::control_line::ControlLine;

    /// This device's shell runtime, on the daemon's one connection to it.
    pub(crate) struct ShellRuntime {
        line: &'static ControlLine,
    }

    impl ShellRuntime {
        /// The runtime of this login, on the connection the daemon's app shells use too.
        fn here() -> Self {
            Self {
                line: crate::device::ush_line::here(),
            }
        }

        /// The runtime listening at `socket`.
        #[cfg(test)]
        pub(crate) fn at(socket: impl Into<std::path::PathBuf>) -> Self {
            Self {
                line: Box::leak(Box::new(ControlLine::at(socket))),
            }
        }

        /// Ask the supervisor `request` about the environments of `workspace`, and answer what
        /// it said: the environments as `{"environments": [...], "created": "..."}`, or its
        /// refusal.
        pub(crate) fn ask(
            &self,
            workspace: &str,
            request: &Value,
        ) -> Result<Value, ActionCallError> {
            let answered = self.line.ask_value(workspace, request).map_err(|error| {
                ActionCallError::Unavailable {
                    reason: format!(
                        "the environments are managed through Tilo's shell runtime: {error}"
                    ),
                }
            })?;
            match answered.get("result").and_then(Value::as_str) {
                Some("environments") => Ok(serde_json::json!({
                    "environments": answered.get("environments").cloned().unwrap_or(Value::Null),
                    "created": answered.get("created").cloned().unwrap_or(Value::Null),
                })),
                _ => Err(ActionCallError::Refused {
                    reason: answered
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("the shell runtime refused the call")
                        .to_owned(),
                }),
            }
        }
    }

    /// The one shell runtime this daemon asks: made when first asked for, and shared after.
    static SERVED: OnceLock<Arc<ShellRuntime>> = OnceLock::new();

    /// This daemon's shell runtime, shared by every provider that asks the supervisor.
    pub(crate) fn shell_runtime() -> Arc<ShellRuntime> {
        Arc::clone(SERVED.get_or_init(|| Arc::new(ShellRuntime::here())))
    }

    /// Ask `runtime` from now on, where this process would ask the login's: a test's supervisor
    /// of its own. Nothing asks the runtime before this, so the daemon's providers take it; one
    /// installed already stands.
    #[cfg(test)]
    pub(crate) fn install(runtime: Arc<ShellRuntime>) {
        let installed = tilo_failure::ignored!(
            "a test installs one runtime, before the daemon takes its providers",
            SERVED.set(runtime)
        );
        if installed.is_err() {
            tracing::debug!("a shell runtime was installed already; the first one stands");
        }
    }
}

//! Tilo's HTTP API (`/v1`) on a connection to this daemon ([`tilo_actions::remote::api`]): what
//! an SDK on this computer asks, answered in process by the API server, as whoever the connection
//! is admitted as.
//!
//! Every connection is answered by an API whose every workspace call walks the tree that
//! connection's action frames reach, as the caller they are made as there: the person's own plain
//! connection is their own act, with their account beside it; an app's request is decided with
//! the app's own grants, the same admission its frames get, and reaches no account; a workspace
//! VM's tenant and another person's key are what their frames are.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use http_body_util::BodyExt as _;
use tilo_action_line::frame_line;
use tilo_action_line_serve::{CallLanes, Take};
use tilo_actions::remote::api::{ApiBytes, ApiHead, MAX_API_PIECE};
use tilo_actions::remote::{ActionCallError, Caller, RemoteActionMessage};
use tilo_api::direct::Direct;
use tilo_api::session::{Asked, Here, Reached, Workspace};
use tilo_lock::MutexRecover as _;

/// What History names the person's own connection's API runs.
const PERSONS_PROGRAM: &str = "A program of yours";

use super::services::Daemon;

/// The tree a connection's frames reach in a workspace (none: its own), and the caller they are
/// there.
pub(crate) type Trees =
    dyn Fn(Option<&str>) -> io::Result<(tilo_api::session::Provider, Caller)> + Send + Sync;

/// Whose API a connection's requests are answered by.
#[derive(Clone)]
pub(crate) enum Answering {
    /// No API: a connection that reaches the device's own actions alone.
    None,
    /// The person's plain connection: their own act in each workspace, as its frames are, with
    /// their account beside it.
    Person(Weak<Daemon>),
    /// One of the connection's own, over its trees, named `label` in History.
    Own(Weak<Daemon>, String),
}

/// The API lane of one connection: its API, made the first time it is asked, and where its
/// answers are written.
pub(crate) struct ApiLane {
    answering: Answering,
    trees: Arc<Trees>,
    api: OnceLock<Result<Arc<Direct>, String>>,
    out: tilo_action_line_serve::shells::Out,
    /// Set once the connection ended: a stream of events still answering stops.
    ended: Arc<AtomicBool>,
}

impl Drop for ApiLane {
    fn drop(&mut self) {
        self.ended.store(true, Ordering::Relaxed);
    }
}

impl ApiLane {
    pub(crate) fn new(
        answering: Answering,
        trees: Arc<Trees>,
        out: tilo_action_line_serve::shells::Out,
    ) -> Self {
        Self {
            answering,
            trees,
            api: OnceLock::new(),
            out,
            ended: Arc::default(),
        }
    }

    fn api(&self) -> Result<Arc<Direct>, String> {
        self.api
            .get_or_init(|| {
                let stopping = || "this computer's daemon is stopping".to_owned();
                let (daemon, label, account) = match &self.answering {
                    Answering::None => {
                        return Err("this connection reaches no workspace".to_owned());
                    }
                    Answering::Person(daemon) => {
                        let daemon = daemon.upgrade().ok_or_else(stopping)?;
                        let account = daemon.api_here();
                        (daemon, PERSONS_PROGRAM.to_owned(), Some(account))
                    }
                    Answering::Own(daemon, label) => {
                        (daemon.upgrade().ok_or_else(stopping)?, label.clone(), None)
                    }
                };
                let here = ConnectionHere::new(Arc::clone(&self.trees), account)
                    .map_err(|error| error.to_string())?;
                daemon
                    .api_as(Arc::new(here), &label)
                    .map_err(|error| error.to_string())
            })
            .clone()
    }
}

/// The API `program` is served, a program the shell of `seat` handed over once the person said
/// yes to it there: each workspace entered as that program ([`Daemon::enter_handed_program`]),
/// with the person's account beside it, and named by what it runs in History.
pub(crate) fn handed_program(
    daemon: &Arc<Daemon>,
    seat: u64,
    program: &str,
) -> Result<Arc<Direct>, String> {
    let here = HandedHere {
        daemon: Arc::downgrade(daemon),
        seat,
        program: program.to_owned(),
        account: daemon.api_here(),
    };
    daemon
        .api_as(Arc::new(here), program)
        .map_err(|error| error.to_string())
}

/// The API's view of a program a seat's shell handed over: each workspace entered as that
/// program, and everything else (the workspaces listed, the account) answered by the person's
/// own account here.
struct HandedHere {
    daemon: Weak<Daemon>,
    seat: u64,
    program: String,
    account: Arc<dyn Here>,
}

impl Here for HandedHere {
    fn workspaces(&self, key: &tilo_user_identity::DeviceCert) -> Vec<Workspace> {
        self.account.workspaces(key)
    }

    fn enter(&self, workspace: &str, _: &tilo_api::grant_key::GrantKey) -> Asked<'_, Reached> {
        let (daemon, workspace) = (self.daemon.clone(), workspace.to_owned());
        let (seat, program) = (self.seat, self.program.clone());
        Box::pin(async move {
            // Admission waits on the workspaces, which are served on the runtime this runs on.
            tokio::task::spawn_blocking(move || {
                daemon
                    .upgrade()
                    .ok_or_else(|| "this computer's daemon is stopping".to_owned())?
                    .enter_handed_program(&workspace, (seat, &program))
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())?
        })
    }

    fn account(&self) -> tilo_api::session::FoundAccount<'_> {
        self.account.account()
    }

    fn keep(&self, workspace: &str) -> Asked<'_, ()> {
        self.account.keep(workspace)
    }

    fn leave(&self, workspace: &str) -> Asked<'_, ()> {
        self.account.leave(workspace)
    }

    fn join(&self, invite: &str) -> Asked<'_, String> {
        self.account.join(invite)
    }

    fn join_entry(
        &self,
        workspace: &str,
        host: tilo_server_protocol::web::PageHost,
    ) -> Asked<'_, String> {
        self.account.join_entry(workspace, host)
    }

    fn not_here(&self, key: &tilo_user_identity::DeviceCert) -> Vec<Workspace> {
        self.account.not_here(key)
    }

    fn workspaces_changed(&self) -> Option<tilo_actions::remote::Pending<()>> {
        self.account.workspaces_changed()
    }

    fn invocations(&self) -> Option<Arc<tilo_invocations::Invocations>> {
        self.account.invocations()
    }

    fn account_here(&self) -> Option<Arc<dyn tilo_api::session::AccountHere>> {
        self.account.account_here()
    }

    fn create_where_founded(
        &self,
        creating: tilo_api::session::Creating,
    ) -> tilo_api::session::FoundingAnswer<'_> {
        self.account.create_where_founded(creating)
    }
}

impl CallLanes for ApiLane {
    fn take(&self, message: &RemoteActionMessage) -> Take {
        self.take_body(message, &[])
    }

    fn take_body(&self, message: &RemoteActionMessage, body: &[u8]) -> Take {
        let RemoteActionMessage::ApiRequest {
            correlation,
            request,
        } = message
        else {
            return Take::NotCall;
        };
        let correlation = *correlation;
        let api = match self.api() {
            Ok(api) => api,
            Err(reason) => {
                return Take::Answer(frame_line(RemoteActionMessage::ApiResponse {
                    correlation,
                    result: Err(ActionCallError::Unavailable { reason }),
                }));
            }
        };
        let (request, body, out) = (request.clone(), body.to_vec(), Arc::clone(&self.out));
        let ended = Arc::clone(&self.ended);
        tilo_rt::app().spawn(async move {
            let reply = api
                .request(&request.method, &request.target, &request.headers, &body)
                .await;
            let (parts, mut body) = reply.into_parts();
            let head = ApiHead {
                status: parts.status.as_u16(),
                headers: parts
                    .headers
                    .iter()
                    .filter_map(|(name, value)| {
                        Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
                    })
                    .collect(),
            };
            out(frame_line(RemoteActionMessage::ApiResponse {
                correlation,
                result: Ok(head),
            }));
            // A stream of events goes on until it ends: each piece as it comes.
            while let Some(Ok(frame)) = body.frame().await {
                if ended.load(Ordering::Relaxed) {
                    return;
                }
                let Ok(data) = frame.into_data() else {
                    continue;
                };
                for piece in data.chunks(MAX_API_PIECE) {
                    out(frame_line(RemoteActionMessage::ApiBody(ApiBytes {
                        correlation,
                        bytes: piece.to_vec(),
                    })));
                }
            }
            out(frame_line(RemoteActionMessage::ApiEnd { correlation }));
        });
        Take::Silent
    }
}

/// The API's view of one connection: each workspace entered as the tree the connection's frames
/// reach there and the caller they are, and everything else (the workspaces listed, the account)
/// answered by `account`: the daemon's own for the person's own connection, none for any other.
/// A workspace is entered once, as the connection's frames are admitted there once: a download's
/// every piece asks it again.
struct ConnectionHere {
    trees: Arc<Trees>,
    entered: Arc<std::sync::Mutex<HashMap<String, (tilo_api::session::Provider, Caller)>>>,
    account: Arc<dyn Here>,
}

/// The account of a connection that is not the person's own: none, and the one workspace it
/// acts in.
struct NoAccount(String);

impl Here for NoAccount {
    fn workspaces(&self, _: &tilo_user_identity::DeviceCert) -> Vec<Workspace> {
        vec![Workspace {
            id: self.0.clone(),
            name: self.0.clone(),
            host: None,
            role: None,
            labels: tilo_server_protocol::workspaces::WorkspaceLabels::new(),
            unattended: false,
            own: false,
            kept_here: true,
            not_here: None,
            address: None,
        }]
    }

    fn enter(&self, _: &str, _: &tilo_api::grant_key::GrantKey) -> Asked<'_, Reached> {
        Box::pin(async { Err("this connection enters through its own frames".to_owned()) })
    }
}

impl ConnectionHere {
    /// The connection whose frames reach `trees`, its account `account`, or, with none, only
    /// the workspace its own tree is in.
    fn new(trees: Arc<Trees>, account: Option<Arc<dyn Here>>) -> io::Result<Self> {
        let account = if let Some(account) = account {
            account
        } else {
            let (_, caller) = trees(None)?;
            let workspace = caller
                .workspace()
                .ok_or_else(|| io::Error::other("this connection acts in no workspace"))?;
            Arc::new(NoAccount(workspace.to_owned()))
        };
        Ok(Self {
            trees,
            entered: Arc::default(),
            account,
        })
    }
}

impl Here for ConnectionHere {
    fn workspaces(&self, key: &tilo_user_identity::DeviceCert) -> Vec<Workspace> {
        self.account.workspaces(key)
    }

    fn enter(&self, workspace: &str, _: &tilo_api::grant_key::GrantKey) -> Asked<'_, Reached> {
        let (trees, workspace) = (Arc::clone(&self.trees), workspace.to_owned());
        let entered = Arc::clone(&self.entered);
        Box::pin(async move {
            let known = entered.lock_recover().get(&workspace).cloned();
            let (provider, caller) = if let Some(known) = known {
                known
            } else {
                // Admission waits on the workspaces, which are served on the runtime this runs on.
                let asked = workspace.clone();
                let reached = tokio::task::spawn_blocking(move || trees(Some(&asked)))
                    .await
                    .map_err(|error| error.to_string())?
                    .map_err(|error| error.to_string())?;
                entered.lock_recover().insert(workspace, reached.clone());
                reached
            };
            Ok(Reached {
                provider,
                person: String::new(),
                caller: Some(caller),
                tasks: None,
            })
        })
    }

    fn account(&self) -> tilo_api::session::FoundAccount<'_> {
        self.account.account()
    }

    fn keep(&self, workspace: &str) -> Asked<'_, ()> {
        self.account.keep(workspace)
    }

    fn leave(&self, workspace: &str) -> Asked<'_, ()> {
        self.account.leave(workspace)
    }

    fn join(&self, invite: &str) -> Asked<'_, String> {
        self.account.join(invite)
    }

    fn join_entry(
        &self,
        workspace: &str,
        host: tilo_server_protocol::web::PageHost,
    ) -> Asked<'_, String> {
        self.account.join_entry(workspace, host)
    }

    fn not_here(&self, key: &tilo_user_identity::DeviceCert) -> Vec<Workspace> {
        self.account.not_here(key)
    }

    fn workspaces_changed(&self) -> Option<tilo_actions::remote::Pending<()>> {
        self.account.workspaces_changed()
    }

    fn invocations(&self) -> Option<Arc<tilo_invocations::Invocations>> {
        self.account.invocations()
    }

    fn account_here(&self) -> Option<Arc<dyn tilo_api::session::AccountHere>> {
        self.account.account_here()
    }

    fn create_where_founded(
        &self,
        creating: tilo_api::session::Creating,
    ) -> tilo_api::session::FoundingAnswer<'_> {
        self.account.create_where_founded(creating)
    }
}

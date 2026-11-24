//! The session a desktop serves the windows it hosts in process.
//! A window never opens another app itself: it asks its session to open it ([`ui::session_api`]),
//! the same request a native window hands its workspace broker. A desktop that hosts its windows in
//! process, such as a page, answers that request here: [`serve`] makes the session the windows are
//! handed ([`DesktopSession`], a remote action host) and the desktop's end of it ([`Openings`]),
//! which opens each admitted app as the desktop's own launch.
//! A window may also ask to open a resource another app keeps, by its record
//! ([`tilo_actions::remote::application_resource_target`]), as Chat's shared windows do: the desktop
//! opens that app on that record. Any other action of an app's resource
//! ([`tilo_actions::remote::application_action_target`]) is delivered the same way: the desktop
//! opens the app, and once its window is up the host invokes the action there ([`Reveal`]), as the
//! workspace broker delivers it to an app it launched.
//! The windows ask during their frame, on the desktop's thread; the desktop opens the apps when it
//! next takes [`Openings::open`]. Until then the ask stays pending, so a window shows its open as
//! in progress, then done or refused, exactly as with a broker.
//! The session also serves the workspace's activity, as a native session's broker does
//! (`activity`), and carries the person's own asks of what only they reach
//! (their MCP server, [`mcp_server::PROVIDER`]) to the action API the desktop's host hands it
//! for each ([`DesktopSession::serve_person_api`]): the person's host, on a page.
//! A window also reads what another app keeps through the session's record API, as it does through
//! the workspace broker. The desktop holds those records itself, so the session takes the reads and
//! the desktop answers them on its own thread, as it decides each one ([`records`]).
//! A window that asks another app for an action is answered with what the action did there, or with
//! its refusal, once it ran in that app's window ([`RevealAnswer`]): never before. An action asked of
//! a record of the workspace (provider `record:<app>:<record>`, as a link to it is) is run by the
//! workspace's host where the desktop reaches one ([`DesktopSession::serve_workspace_api`]), whoever
//! has the record open, and else by the window that shows it; no window is opened for it.
//! What the workspace's server keeps for its people, its accounts, model providers, vault and
//! what it tells them ([`WORKSPACE_SERVER_PROVIDERS`]), is asked of it the same way.

mod activity;
#[cfg(test)]
mod mcp_server_tests;
mod records;
#[cfg(test)]
mod records_tests;

pub use activity::{ActivityAsk, ActivityReads, ActivityRequests, activity_requests};
pub use records::{ASKED_NOT_TAKEN, HOLD_MAX, HeldRecords, RecordAnswers};

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionCallError, ActionProvider, Caller,
    CataloguePage, CatalogueQuery, Completer, InvocationReceipt, InvocationState, InvokeArgument,
    InvokeRequest, NODE_PERMITS_ACTION, NODE_RESOURCE_PREFIX, Pending, ReceiptStatus,
    SHARE_NODE_ACTION, UNSHARE_NODE_ACTION, application_provider, application_provider_resource,
    formats, mcp_server, pending, person_accounts, person_models, person_telling, person_vault,
    provider_workspace, server_account, session_actions, workspace_activity as wire,
};

use tilo_lock::MutexRecover as _;

use tilo_desktop_search::record_launch;

use tilo_desktop_model::TabId;

use crate::desktop::navigation::{ChromeEffect, LayoutEffect};
use crate::desktop::state::DesktopHandle;

/// What one admitted open answers to.
pub type Answer = Completer<Result<InvocationReceipt, ActionCallError>>;

/// What a window asking a record for an action hears where no window shows the record and no
/// host of the workspace runs it.
pub const NOT_OPEN_HERE: &str = "That is not open in a window here";

/// The launch a [`crate::desktop::state::TileSource`] reads as opening `app` with the content whose hash is `hash`
/// : `{CONTENT_LAUNCH}{app}:{hash}` ([`content_launch`]).
pub const CONTENT_LAUNCH: &str = "content:";

/// The launch that opens `app` with the content whose hash is `hash` , as the workspace broker
/// opens an app with content another app handed over (a capture, a recording).
#[must_use]
pub fn content_launch(app: &str, hash: &str) -> String {
    format!("{CONTENT_LAUNCH}{app}:{hash}")
}

/// [`content_launch`] for the file called `name`, where the one handing it over knows it:
/// `{CONTENT_LAUNCH}{app}:{hash}:{name}`, so the window it opens names the file by it.
#[must_use]
pub fn named_content_launch(app: &str, hash: &str, name: Option<&str>) -> String {
    match name {
        Some(name) => format!("{CONTENT_LAUNCH}{app}:{hash}:{name}"),
        None => content_launch(app, hash),
    }
}

/// The app and content hash a [`content_launch`] names, or `None` for any other launch.
#[must_use]
pub fn launched_content(launch: &str) -> Option<(&str, &str)> {
    launched_file(launch).map(|(app, hash, _)| (app, hash))
}

/// The app, the content hash and the file's name a [`named_content_launch`] names, or `None`
/// for any other launch.
#[must_use]
pub fn launched_file(launch: &str) -> Option<(&str, &str, Option<&str>)> {
    let (app, rest) = launch.strip_prefix(CONTENT_LAUNCH)?.split_once(':')?;
    let (hash, name) = match rest.split_once(':') {
        Some((hash, name)) => (hash, Some(name)),
        None => (rest, None),
    };
    (!app.is_empty()
        && ui::content::valid_reference(hash)
        && name.is_none_or(tilo_actions::remote::valid_file_name))
    .then_some((app, hash, name))
}

/// What an open asks of an app: the app, what of it it opens on (`None` for the app's own),
/// and the content it opens by hash, with the name of the file it is where the open gives one.
pub type ToOpen<'a> = (&'a str, Option<&'a str>, Option<(&'a str, Option<&'a str>)>);

/// An app a window asked to open, on one of its records, with content, or on its own, waiting for
/// the desktop.
pub struct Opening {
    app: String,
    record: Option<String>,
    /// The content the app opens, by hash, which the asking window holds.
    content: Option<String>,
    /// The name of the file the content is, where the asking window knows it.
    name: Option<String>,
    /// The resource the open names in the record, which the app's window is to show once it is
    /// open.
    resource: Option<String>,
    /// What the window is to do with the resource once it is open: show it
    /// ([`APPLICATION_OPEN_ACTION`]) or another of its actions.
    action: String,
    arguments: serde_json::Map<String, serde_json::Value>,
    /// Asked of a record rather than of its app: it runs in the window that shows the record,
    /// which is left where it is, and opens none.
    shown_only: bool,
    invocation: InvocationId,
    answer: Answer,
}

/// The asks the desktop has not taken yet.
pub type Asked = Arc<std::sync::Mutex<Vec<Opening>>>;

/// What a window asked of one node of the workspace's tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeAction {
    /// Grant the members `access` (`view` or `edit` ) on it, as share `share` .
    Share { share: String, access: String },
    /// Take back what share `share` granted.
    Unshare { share: String },
    /// Whether the caller may change it.
    Permits,
}

/// A window's ask of a node, which the desktop answers through its workspace ([`NodeAsk::answer`]):
/// the workspace decides it, as it decides everything a grant gives.
pub struct NodeAsk {
    /// The node, in its text form.
    pub node: String,
    pub action: NodeAction,
    invocation: InvocationId,
    answer: Answer,
}

impl NodeAsk {
    /// Answer the window: the workspace's output, or why it refused.
    pub fn answer(self, outcome: Result<serde_json::Value, String>) {
        self.answer.complete(Ok(InvocationReceipt {
            invocation: self.invocation,
            status: match outcome {
                Ok(value) => ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(value),
                        ..tilo_actions::ActionOutput::default()
                    },
                },
                Err(reason) => ReceiptStatus::Rejected {
                    error: ActionCallError::Refused { reason },
                },
            },
        }));
    }
}

/// What a window asked of `node` , from its request's action and arguments.
pub fn node_action(request: &InvokeRequest) -> Result<NodeAction, ActionCallError> {
    let text = |name: &str| match &request.argument {
        InvokeArgument::Json(arguments) => arguments
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ActionCallError::Refused {
                reason: format!("`{name}` is missing"),
            }),
        _ => Err(ActionCallError::Refused {
            reason: format!("`{name}` is missing"),
        }),
    };
    Ok(match request.target.address.action.as_str() {
        SHARE_NODE_ACTION => NodeAction::Share {
            share: text("share")?,
            access: text("access")?,
        },
        UNSHARE_NODE_ACTION => NodeAction::Unshare {
            share: text("share")?,
        },
        NODE_PERMITS_ACTION => NodeAction::Permits,
        _ => return Err(ActionCallError::UnknownAction),
    })
}

/// The providers only the person reaches, each served by an action API the
/// desktop's host hands the session ([`DesktopSession::serve_person_api`]).
pub const PERSON_PROVIDERS: [&str; 1] = [mcp_server::PROVIDER];

/// What the workspace's server keeps for the people in it, asked of that
/// server the way an action of a resource is ([`DesktopSession::serve_workspace_api`]):
/// the workspace's mail and calendar accounts and model providers, the vault
/// they sign in with (a protected field in a window hands what was typed
/// there), and what its people call important and were told (the server tells
/// while no device is on), and the person's own account with that server (its
/// plan and what they used), and the formats and zone they read dates in, which
/// a server of the person's keeps in their own workspace. The server stores and
/// serves all of it, so a page manages it with none of the person's other
/// devices online.
pub const WORKSPACE_SERVER_PROVIDERS: [&str; 6] = [
    person_accounts::PROVIDER,
    person_models::PROVIDER,
    person_vault::PROVIDER,
    person_telling::PROVIDER,
    server_account::PROVIDER,
    formats::PROVIDER,
];

/// The session handed to the windows a desktop hosts in process, for the workspace `workspace` . It
/// serves only what the desktop itself does: opening an app as a window of this desktop.
pub struct DesktopSession {
    workspace: String,
    asked: Asked,
    nodes: Arc<std::sync::Mutex<Vec<NodeAsk>>>,
    activity: Arc<activity::Api>,
    /// Where the person's asks of each provider only they reach go, once the
    /// host serves one.
    person_apis: std::sync::Mutex<HashMap<String, Arc<dyn ActionProvider + Send + Sync>>>,
    /// The invocations a person's action API admitted, and that API, which
    /// answers for them.
    api_invocations: std::sync::Mutex<HashMap<InvocationId, Arc<dyn ActionProvider + Send + Sync>>>,
    /// The action API of the workspace's host, where the desktop reaches one: what runs an action
    /// asked of a record of the workspace ([`Self::serve_workspace_api`]).
    workspace_api: std::sync::Mutex<Option<Arc<dyn ActionProvider + Send + Sync>>>,
    /// The host actions a window asked the shell to choose for the person
    /// ([`session_actions::SHOW_CONNECT`], [`session_actions::SHOW_PAIR_DEVICE`]), for the
    /// desktop to take.
    chosen: Chosen,
    /// The record reads the windows ask, which the desktop answers on its own thread
    /// ([`Openings::answer_records`]).
    records: Arc<records::Reads>,
    woken: Arc<AtomicBool>,
}

/// Host actions asked of the shell, by id, waiting for the desktop.
pub type Chosen = Arc<std::sync::Mutex<Vec<String>>>;

/// The desktop's end of its [`DesktopSession`].
pub struct Openings {
    asked: Asked,
    chosen: Chosen,
    nodes: Arc<std::sync::Mutex<Vec<NodeAsk>>>,
    activity: ActivityRequests,
    /// The record reads the windows ask of the session.
    records: Arc<records::Reads>,
    /// What the windows opened since [`Openings::take_reveals`] are to show.
    reveals: std::cell::RefCell<Vec<Reveal>>,
}

/// An action a window asked of a resource in an app's record: once the window on that record is up,
/// the host invokes it there, as the workspace broker delivers it to an app it launched. Most often
/// the resource's `open` , which shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reveal {
    /// The app, by package id.
    pub app: String,
    /// The record, as its route names it: empty for the app's own record of the workspace.
    pub record: String,
    /// The resource the action is of.
    pub resource: String,
    /// The action ([`APPLICATION_OPEN_ACTION`] to show the resource).
    pub action: String,
    /// What the action is invoked with.
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// What the window that asked for the action is told once it ran. `None` for an action nobody
    /// waits on: a row the desktop's own search runs.
    pub answer: Option<RevealAnswer>,
}

/// What a window that asked another app for an action is told: the action's own outcome, once the
/// host ran it in the window the desktop opened or found for it. The host answers through one of
/// the methods below, once; a reveal dropped unanswered (an open that names nothing to run there, a
/// host that only opens windows) tells the caller the open went ahead.
#[derive(Clone)]
pub struct RevealAnswer(Arc<Unanswered>);

pub struct Unanswered {
    invocation: InvocationId,
    answer: std::sync::Mutex<Option<Answer>>,
}

impl Drop for Unanswered {
    fn drop(&mut self) {
        let unanswered = self.answer.lock_recover().take();
        if let Some(answer) = unanswered {
            answer.complete(Ok(InvocationReceipt {
                invocation: self.invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
            }));
        }
    }
}

impl RevealAnswer {
    fn new(invocation: InvocationId, answer: Answer) -> Self {
        Self(Arc::new(Unanswered {
            invocation,
            answer: std::sync::Mutex::new(Some(answer)),
        }))
    }

    fn settle(&self, status: ReceiptStatus) {
        let unanswered = self.0.answer.lock_recover().take();
        if let Some(answer) = unanswered {
            answer.complete(Ok(InvocationReceipt {
                invocation: self.0.invocation,
                status,
            }));
        }
    }

    /// The action ran and finished with `output`.
    pub fn completed(&self, output: tilo_actions::ActionOutput) {
        self.settle(ReceiptStatus::Completed { output });
    }

    /// The action ran and failed.
    pub fn failed(&self, error: tilo_actions::outcome::ActionFailure) {
        self.settle(ReceiptStatus::Failed { error });
    }

    /// The action was stopped before it ended.
    pub fn cancelled(&self, message: Option<String>) {
        self.settle(ReceiptStatus::Cancelled { message });
    }

    /// The action did not run: the window's own refusal, or why nothing took it.
    pub fn refused(&self, error: ActionCallError) {
        let unanswered = self.0.answer.lock_recover().take();
        if let Some(answer) = unanswered {
            answer.complete(Err(error));
        }
    }
}

impl PartialEq for RevealAnswer {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for RevealAnswer {}

impl std::fmt::Debug for RevealAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RevealAnswer")
            .field("invocation", &self.0.invocation)
            .finish_non_exhaustive()
    }
}

/// The session for the workspace `workspace` , and the desktop's end of it. `woken` is raised
/// whenever a window asks, so the desktop's host draws a frame that takes the ask.
#[must_use]
pub fn serve(
    workspace: impl Into<String>,
    woken: Arc<AtomicBool>,
) -> (Arc<DesktopSession>, Openings) {
    let asked = Asked::default();
    let nodes = Arc::<std::sync::Mutex<Vec<NodeAsk>>>::default();
    let activity = activity::Api::new(Arc::clone(&woken));
    let chosen = Chosen::default();
    let records = records::Reads::new(Arc::clone(&woken));
    (
        Arc::new(DesktopSession {
            workspace: workspace.into(),
            asked: Arc::clone(&asked),
            nodes: Arc::clone(&nodes),
            activity: Arc::clone(&activity),
            person_apis: std::sync::Mutex::default(),
            api_invocations: std::sync::Mutex::default(),
            workspace_api: std::sync::Mutex::default(),
            chosen: Arc::clone(&chosen),
            records: Arc::clone(&records),
            woken,
        }),
        Openings {
            asked,
            chosen,
            nodes,
            activity: activity.requests(),
            records,
            reveals: std::cell::RefCell::default(),
        },
    )
}

impl DesktopSession {
    /// Carry the person's asks of `provider`, one only they reach
    /// ([`PERSON_PROVIDERS`]), to `api` from now on.
    pub fn serve_person_api(&self, provider: &str, api: Arc<dyn ActionProvider + Send + Sync>) {
        self.person_apis
            .lock_recover()
            .insert(provider.to_owned(), api);
    }

    /// Carry every action a window asks of a record of the workspace (provider
    /// `record:<app>:<record>`) to `api` from now on: the action API of the workspace's host,
    /// which runs it whoever has the record open and decides it by the caller's own access.
    pub fn serve_workspace_api(&self, api: Arc<dyn ActionProvider + Send + Sync>) {
        *self.workspace_api.lock_recover() = Some(api);
    }

    /// Run `request`, asked of the record `record` that `app` keeps in this desktop's workspace.
    /// The workspace's host runs it where the desktop reaches one; a desktop with none hands it to
    /// the window that shows the record ([`Reveal`]) and opens none for it.
    fn ask_of_record(
        &self,
        caller: &Caller,
        (app, record): (String, String),
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // As the broker: what a record offers is its workspace's members' to ask, in this
        // workspace only.
        if caller.workspace() != Some(self.workspace.as_str())
            || request.target.address.action.is_empty()
        {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let host = self.workspace_api.lock_recover().clone();
        if let Some(host) = host {
            self.api_invocations
                .lock_recover()
                .insert(request.invocation, Arc::clone(&host));
            return host.invoke(caller, request);
        }
        let arguments = match request.argument {
            InvokeArgument::None => serde_json::Map::new(),
            InvokeArgument::Json(arguments) => arguments,
            InvokeArgument::Text(_) => {
                return Pending::ready(Err(ActionCallError::Refused {
                    reason: "Asking another app for that isn't available here yet".into(),
                }));
            }
        };
        let (answer, receipt) = pending();
        self.asked.lock_recover().push(Opening {
            app,
            record: (!record.is_empty()).then_some(record),
            content: None,
            name: None,
            resource: Some(request.target.address.resource),
            action: request.target.address.action,
            arguments,
            shown_only: true,
            invocation: request.invocation,
            answer,
        });
        self.woken.store(true, Ordering::Release);
        receipt
    }

    /// Ask the workspace's server for `request`, one of what it keeps for the people in the
    /// workspace ([`WORKSPACE_SERVER_PROVIDERS`]), for `caller`: the person themselves, never an
    /// agent or an app acting for them. The server decides it by the person's own access.
    fn ask_workspace_server(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if !(caller.acting().is_person() && caller.acting().app().is_none()) {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let Some(server) = self.workspace_api.lock_recover().clone() else {
            return Pending::ready(Err(ActionCallError::Unavailable {
                reason: "This desktop isn't in a workspace on a server".into(),
            }));
        };
        self.api_invocations
            .lock_recover()
            .insert(request.invocation, Arc::clone(&server));
        server.invoke(caller, request)
    }

    /// The person's action API that admitted `id`, when one did.
    fn api_of(&self, id: &InvocationId) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
        self.api_invocations.lock_recover().get(id).cloned()
    }

    /// Ask the person's action API for `request`'s provider for `caller`: the
    /// person themselves, never an agent or an app acting for them.
    fn ask_person_api(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if !(caller.acting().is_person() && caller.acting().app().is_none()) {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let api = self
            .person_apis
            .lock_recover()
            .get(&request.target.provider)
            .cloned();
        let Some(api) = api else {
            return Pending::ready(Err(ActionCallError::Unavailable {
                reason: if request.target.provider == mcp_server::PROVIDER {
                    "No MCP server answers here".into()
                } else {
                    "This page isn't signed in as one of your devices".into()
                },
            }));
        };
        self.api_invocations
            .lock_recover()
            .insert(request.invocation, Arc::clone(&api));
        api.invoke(caller, request)
    }

    /// The app `request` opens in this desktop's workspace, the record it
    /// opens it on (`None` for the app's own), and the content it opens in
    /// it, by hash, with the name of the file it is where the request gives one.
    ///
    /// # Errors
    /// A request for another workspace, for something other than an app's
    /// action, or with anything but a content hash to open in it.
    fn app_to_open<'a>(
        &self,
        caller: &Caller,
        request: &'a InvokeRequest,
    ) -> Result<ToOpen<'a>, ActionCallError> {
        let address = &request.target.address;
        if let Some((workspace, app)) = application_provider(&request.target.provider) {
            return self
                .record_to_open(caller, request, workspace)
                .map(|record| (app, record, None));
        }
        let app = address
            .resource
            .strip_prefix(APPLICATION_RESOURCE_PREFIX)
            .filter(|_| address.action == APPLICATION_OPEN_ACTION)
            .ok_or(ActionCallError::UnknownAction)?;
        // As the broker: opening an app is something a workspace member asks of their workspace.
        if caller.workspace().is_none() {
            return Err(ActionCallError::UnknownAction);
        }
        if provider_workspace(&request.target.provider) != Some(self.workspace.as_str()) {
            return Err(ActionCallError::Unavailable {
                reason: "That workspace isn't open here".into(),
            });
        }
        // As the broker: the argument is the content to open, by hash
        // (`ApplicationLaunch::argument`).
        if matches!(request.argument, InvokeArgument::None) {
            return Ok((app, None, None));
        }
        match tilo_actions::remote::ApplicationLaunch::opened_content(&request.argument) {
            Some((hash, name)) if ui::content::valid_reference(hash) => {
                Ok((app, None, Some((hash, name))))
            }
            _ => Err(ActionCallError::Refused {
                reason: "That isn't something an app can open".into(),
            }),
        }
    }

    /// The record an action of a resource in `workspace` names, `None` for the
    /// app's own.
    ///
    /// # Errors
    /// An unnamed action, another workspace, or a caller from outside it.
    fn record_to_open<'a>(
        &self,
        caller: &Caller,
        request: &'a InvokeRequest,
        workspace: &str,
    ) -> Result<Option<&'a str>, ActionCallError> {
        if request.target.address.action.is_empty() || caller.workspace().is_none() {
            return Err(ActionCallError::UnknownAction);
        }
        if workspace != self.workspace {
            return Err(ActionCallError::Unavailable {
                reason: "That workspace isn't open here".into(),
            });
        }
        Ok(application_provider_resource(&request.target.provider))
    }
}

impl DesktopSession {
    /// Show the person what a window asked the shell to show them: how an agent or a computer
    /// connects as them ([`session_actions::CONNECT_AGENT`]), or the link another of their own
    /// devices joins with ([`session_actions::PAIR_DEVICE`]). Each is the host action their own
    /// card or search chooses, which the desktop's host carries out.
    fn choose_for_person(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        host_action: &str,
    ) -> Result<InvocationReceipt, ActionCallError> {
        // Shown to the person on their own screen, as their card shows it: never an agent's.
        if !(caller.acting().is_person() && caller.acting().app().is_none()) {
            return Err(ActionCallError::UnknownAction);
        }
        self.chosen.lock_recover().push(host_action.to_owned());
        self.woken.store(true, Ordering::Release);
        Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: tilo_actions::ActionOutput::default(),
            },
        })
    }
}

impl ActionProvider for DesktopSession {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        // The windows find what they may open in their own catalogue; this session only opens it.
        Pending::ready(Ok(CataloguePage {
            exhausted: true,
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if request.target.provider == wire::PROVIDER {
            // As the broker: a workspace's activity is its members' to read.
            if caller.workspace().is_none() {
                return Pending::ready(Err(ActionCallError::UnknownAction));
            }
            return self.activity.invoke(caller, request);
        }
        if PERSON_PROVIDERS.contains(&request.target.provider.as_str()) {
            return self.ask_person_api(caller, request);
        }
        if WORKSPACE_SERVER_PROVIDERS.contains(&request.target.provider.as_str()) {
            return self.ask_workspace_server(caller, request);
        }
        for (shown, host_action) in [
            (
                session_actions::SHOW_CONNECT,
                session_actions::CONNECT_AGENT,
            ),
            (
                session_actions::SHOW_PAIR_DEVICE,
                session_actions::PAIR_DEVICE,
            ),
        ] {
            if request.target == session_actions::target(shown) {
                return Pending::ready(self.choose_for_person(caller, &request, host_action));
            }
        }
        if let Some(node) = request
            .target
            .address
            .resource
            .strip_prefix(NODE_RESOURCE_PREFIX)
            .filter(|_| provider_workspace(&request.target.provider).is_some())
        {
            // As the broker: what a node grants is its workspace members' business, in this
            // workspace only.
            if caller.workspace().is_none() {
                return Pending::ready(Err(ActionCallError::UnknownAction));
            }
            if provider_workspace(&request.target.provider) != Some(self.workspace.as_str()) {
                return Pending::ready(Err(ActionCallError::Unavailable {
                    reason: "That workspace isn't open here".into(),
                }));
            }
            let action = match node_action(&request) {
                Ok(action) => action,
                Err(error) => return Pending::ready(Err(error)),
            };
            let (answer, receipt) = pending();
            self.nodes.lock_recover().push(NodeAsk {
                node: node.to_owned(),
                action,
                invocation: request.invocation,
                answer,
            });
            self.woken.store(true, Ordering::Release);
            return receipt;
        }
        let record = tilo_actions::remote::resource_parts(&request.target.provider)
            .map(|(app, record)| (app.to_owned(), record.to_owned()));
        if let Some(record) = record {
            return self.ask_of_record(caller, record, request);
        }
        let (app, record, content) = match self.app_to_open(caller, &request) {
            Ok((app, record, content)) => (
                app.to_owned(),
                record.map(str::to_owned),
                content.map(|(hash, name)| (hash.to_owned(), name.map(str::to_owned))),
            ),
            Err(error) => return Pending::ready(Err(error)),
        };
        let (content, name) = content.map_or((None, None), |(hash, name)| (Some(hash), name));
        // An action of an app's resource names the resource; an app's own open names the app.
        let resource = application_provider(&request.target.provider)
            .map(|_| request.target.address.resource.clone());
        let arguments = match request.argument {
            InvokeArgument::None => serde_json::Map::new(),
            InvokeArgument::Text(_) | InvokeArgument::Json(_) if content.is_some() => {
                serde_json::Map::new()
            }
            InvokeArgument::Json(arguments) => arguments,
            InvokeArgument::Text(_) => {
                return Pending::ready(Err(ActionCallError::Refused {
                    reason: "Asking another app for that isn't available here yet".into(),
                }));
            }
        };
        let (answer, receipt) = pending();
        self.asked.lock_recover().push(Opening {
            app,
            record,
            content,
            name,
            resource,
            action: request.target.address.action,
            arguments,
            shown_only: false,
            invocation: request.invocation,
            answer,
        });
        self.woken.store(true, Ordering::Release);
        receipt
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        // Every open is answered whole at admission; what stays open is a view of the activity
        // or of the MCP server.
        if let Some(api) = self.api_of(id) {
            return api.observe(caller, id);
        }
        self.activity.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        if let Some(api) = self.api_of(id) {
            return api.next_change(caller, id);
        }
        self.activity.next_change(caller, id)
    }

    /// The record reads of the windows, once the desktop answers them
    /// ([`Openings::answer_records`]): a desktop that holds no records reads none.
    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        self.records.served()
    }

    /// The next time the records the desktop holds may have changed, for a desktop that answers
    /// its windows' reads; who is in the workspace is not this session's to tell.
    fn changed(
        &self,
        caller: &Caller,
        scope: tilo_actions::remote::WatchScope,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        self.records
            .changed(caller, scope)
            .map(|moved| moved.map(Ok))
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        if self.activity.serves(id) {
            return Pending::ready(self.activity.cancel(caller, id));
        }
        if let Some(api) = self.api_of(id) {
            self.api_invocations.lock_recover().remove(id);
            return api.cancel(caller, id);
        }
        Pending::ready(Err(ActionCallError::Refused {
            reason: "Opening an app can't be cancelled".into(),
        }))
    }
}

impl Openings {
    /// Answer the record reads of this desktop's windows from now on: the session takes each read
    /// a window asks, and the desktop answers them with what this returns, on its own thread, from
    /// the records it holds ([`RecordAnswers::answer`]).
    #[must_use]
    pub fn answer_records(&self) -> RecordAnswers {
        RecordAnswers::of(&self.records)
    }

    /// The desktop's end of the activity the session serves.
    #[must_use]
    pub fn activity(&self) -> ActivityRequests {
        self.activity.clone()
    }

    /// What the windows asked of nodes of the workspace's tree since this was last taken, for the
    /// desktop to ask its workspace.
    #[must_use]
    pub fn take_node_asks(&self) -> Vec<NodeAsk> {
        std::mem::take(&mut *self.nodes.lock_recover())
    }

    /// What the windows opened since this was last taken are to show.
    #[must_use]
    pub fn take_reveals(&self) -> Vec<Reveal> {
        std::mem::take(&mut *self.reveals.borrow_mut())
    }

    /// Open every app the windows asked for since this was last taken,
    /// through the desktop `handle`'s verbs, each one `may_open` admits
    /// (told whether the open shows something that exists, a record or an
    /// item of one). Opening means going there: the window `showing` says
    /// already shows the record an open names (an app and its record's
    /// route, empty for the app's own) comes forward and is what the open's
    /// action runs in, so a link into an app never adds a second window on
    /// what one already shows. Only an open no window shows, or one that
    /// hands the app content, opens a window: on its record when it names
    /// one ([`record_launch`]). An open that names something for the window to
    /// run is answered by what that came to ([`Reveal::answer`]), every other
    /// one here. Whether anything was asked, so the host lays out again.
    pub fn open(
        &self,
        handle: &DesktopHandle,
        may_open: impl Fn(&str, bool) -> Result<(), ActionCallError>,
        showing: impl Fn(&str, &str) -> Option<TabId>,
    ) -> bool {
        let chosen = std::mem::take(&mut *self.chosen.lock_recover());
        for id in &chosen {
            handle.choose_host_action(id);
        }
        let asked = std::mem::take(&mut *self.asked.lock_recover());
        let any = !asked.is_empty() || !chosen.is_empty();
        for Opening {
            app,
            record,
            content,
            name,
            resource,
            action,
            arguments,
            shown_only,
            invocation,
            answer,
        } in asked
        {
            let existing = record.is_some() || resource.is_some();
            let route = record.clone().unwrap_or_default();
            let opened = may_open(&app, existing).and_then(|()| {
                // Content handed to an app is a new thing to show; anything else goes to the
                // window that already shows it. A window that left the shell since `showing`
                // answered is no place to go, and the open opens one as if none had been there.
                let window = content.is_none().then(|| showing(&app, &route)).flatten();
                if shown_only {
                    // Asked of the record, not of its app: it runs where the record is open,
                    // and that window stays where it is.
                    return window
                        .map(|_| ())
                        .ok_or_else(|| ActionCallError::Unavailable {
                            reason: NOT_OPEN_HERE.into(),
                        });
                }
                let shown = window.is_some_and(|tab| {
                    handle
                        .dispatch(LayoutEffect::Focus {
                            tab: tab.to_string(),
                        })
                        .is_ok()
                });
                if shown {
                    return Ok(());
                }
                let launch = match (record, content) {
                    (Some(record), _) => record_launch(&app, &record),
                    (None, Some(hash)) => named_content_launch(&app, &hash, name.as_deref()),
                    (None, None) => app.clone(),
                };
                handle
                    .dispatch(ChromeEffect::OpenApp {
                        app: launch,
                        beside: None,
                        background: None,
                        float: None,
                    })
                    .map_err(|reason| ActionCallError::Unavailable { reason })
            });
            match (opened, resource) {
                (Err(error), _) => answer.complete(Err(error)),
                // The open is all that was asked.
                (Ok(()), None) => answer.complete(Ok(InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Completed {
                        output: tilo_actions::ActionOutput::default(),
                    },
                })),
                // The window is to run something: what that comes to is the answer.
                (Ok(()), Some(resource)) => self.reveals.borrow_mut().push(Reveal {
                    app,
                    record: route,
                    resource,
                    action,
                    arguments,
                    answer: Some(RevealAnswer::new(invocation, answer)),
                }),
            }
        }
        any
    }
}

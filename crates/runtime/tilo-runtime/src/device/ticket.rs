//! The two device actions this computer's daemon publishes: serving a ticket for a new computer
//! (`tilo device add`), and logging in with a ticket another of the person's devices issued
//! (`tilo login`). Both need what only the daemon has: the device's endpoint, its session, and,
//! on a computer that holds the person's root, that root.
//!
//! They are ordinary actions (`tilo.device`'s `devices#issue` and `devices#logIn`), answered by
//! the daemon's action provider. A ticket's certify action is answered by the device action
//! service the daemon's one iroh router already serves (`tilo-actions/1`), as the one call it
//! admits from a computer with no certificate yet, so no second endpoint or service is made.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::workspaces::ServiceWorkspaces;
use anyhow::{Context as _, Result, anyhow, bail};
use collab_types::signer::DeviceSigner;
use ed25519_dalek::SigningKey;
use futures_util::future::BoxFuture;
use iroh::Endpoint;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionCallError, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueResource,
    CatalogueRow, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
};
use tilo_actions::{
    ActionContract, ActionDescriptor, ActionFailure, ActionKind, ActionOutput, ActionParam,
    ExecutionLocus, InputSpec, InvocationId, InvocationUpdate, OperationKind, ParamType,
};
use tilo_desktop_model::WorkspaceId;
use tilo_device_identity::login::{
    ALREADY_A_PERSONS, APPROVE, Device, Direction, Entry, FOUND, ISSUE, LOG_IN, PENDING, PROVIDER,
    Place, REPORTED, RESOURCE, Reached, Report, TICKET_LIFETIME_NS, Ticket,
};
use tilo_identity::store::SigningIdentity;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::{ActionService, NoActions};
use tilo_session_client::device::Device as Entered;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::device_tickets::{Issuer, Role, Root, TicketServer};
use tilo_workspace_service::workspaces::Workspaces;

/// How long the issuer keeps serving once the computer is certified, for it to read its answer.
const TELL_WAIT: Duration = Duration::from_secs(30);
/// How long the issuer keeps serving after it told the computer how its call ended: an answer
/// that ended with its connection is asked for again, and answered where the call is.
const TELL_GRACE: Duration = Duration::from_secs(5);
/// What the device actions' catalogue page is revised by: they are fixed, so one revision.
const REVISION: &str = "tilo.device/1";
/// What this device is ([`device_info`]).
pub(crate) const INFO: &str = "info";

/// The daemon's `tilo.device` provider, as the catalogue lists it beside the workspace's apps: a
/// running service whose resource is `devices`.
#[must_use]
pub(crate) fn provider(workspace: &str) -> CatalogueProvider {
    CatalogueProvider {
        provider: PROVIDER.to_owned(),
        app_id: RESOURCE.to_owned(),
        workspace: workspace.to_owned(),
        browsable: true,
        ..CatalogueProvider::default()
    }
}

/// The `tilo.device` page `query` asks for: the `devices` resource and its actions, resolved and
/// invoked the way an app's are.
#[must_use]
pub(crate) fn catalogue(query: &CatalogueQuery) -> CataloguePage {
    catalogue_of(query, |_| true)
}

/// [`catalogue`] with only what a device answers before its person's services run: what it is,
/// logging it in with a ticket, and starting a new person on it.
#[must_use]
pub(crate) fn device_only_catalogue(query: &CatalogueQuery) -> CataloguePage {
    catalogue_of(query, |action| matches!(action, INFO | LOG_IN | FOUND))
}

/// The `devices` resource and those of its actions `keep` keeps.
#[expect(
    clippy::too_many_lines,
    reason = "every device action's row, read top to bottom"
)]
fn catalogue_of(query: &CatalogueQuery, keep: impl Fn(&str) -> bool) -> CataloguePage {
    let resource = CatalogueResource {
        id: RESOURCE.to_owned(),
        kind: "device".to_owned(),
        label: "Devices".to_owned(),
        presentation: None,
    };
    let row = |action| CatalogueRow {
        provider: PROVIDER.to_owned(),
        app_id: RESOURCE.to_owned(),
        parent_resource: String::new(),
        resource: resource.clone(),
        action,
        ..CatalogueRow::default()
    };
    let rows = vec![
        row(None),
        row(Some(descriptor(
            INFO,
            "This device",
            "What this device is: its key, how it is listed, and the people whose \
                 certificates name it.",
            &[],
        ))),
        row(Some(descriptor(
            ISSUE,
            "Add a device",
            "Print a ticket another computer logs in with, approved by the person's root here.",
            &[
                ActionParam::optional(
                    "wait",
                    ParamType::Boolean,
                    "Wait for the computer to log in with the ticket.",
                ),
                ActionParam::optional(
                    "plan",
                    ParamType::Text,
                    "The planned machine's entry in your Servers workspace, which the \
                         computer's certification takes into its own.",
                ),
                ActionParam::optional(
                    "server",
                    ParamType::Boolean,
                    "The computer is a server, which keeps your workspaces: it gets an \
                         entry in your Servers workspace.",
                ),
            ],
        ))),
        row(Some(descriptor(
            PENDING,
            "Sign a browser in",
            "See the browser a sign-in ticket on another host waits for, and what signing it \
                 in makes it.",
            &[ActionParam::required(
                "ticket",
                ParamType::Text,
                "The ticket the browser's prompt carried.",
            )],
        ))),
        row(Some(descriptor(
            APPROVE,
            "Approve a browser's sign-in",
            "Sign the browser's certificate, so it enters the person's workspace.",
            &[ActionParam::required(
                "ticket",
                ParamType::Text,
                "The ticket the browser's prompt carried.",
            )],
        ))),
        row(Some(descriptor(
            REPORTED,
            "Report a login's step",
            "Tell the browser that showed the ticket what this computer did as it logged in.",
            &[
                ActionParam::required(
                    "ticket",
                    ParamType::Text,
                    "The ticket this computer logged in with.",
                ),
                ActionParam::required("step", ParamType::Text, "What it did."),
            ],
        ))),
        row(Some(descriptor(
            LOG_IN,
            "Log in with a ticket",
            "Become one of the person's devices with a ticket another of their computers printed, \
             or, on a computer that holds the person's identity, log in at a server by its line.",
            &[
                ActionParam::optional("ticket", ParamType::Text, "The ticket, as it was printed."),
                ActionParam::optional(
                    "server",
                    ParamType::Text,
                    "A server's line from its web-host.json, instead of a ticket.",
                ),
                ActionParam::optional("agent", ParamType::Text, "The agent that runs here."),
                ActionParam::optional(
                    "label",
                    ParamType::Text,
                    "How the devices list this computer.",
                ),
            ],
        ))),
        row(Some(descriptor(
            FOUND,
            "Start a new person",
            "Make a new person on this device, whose root it keeps: for a device no one has set \
                 up, by its own account.",
            &[],
        ))),
    ];
    let rows = rows
        .into_iter()
        .filter(|row| {
            row.action
                .as_ref()
                .is_none_or(|action| keep(&action.address.action))
        })
        .collect();
    CataloguePage::from_rows(REVISION.to_owned(), rows, query)
}

/// `page`, the workspace's apps, with the daemon's own `tilo.device` provider beside them: listed
/// for every caller, and its page answered whole when the query names it.
#[must_use]
pub(crate) fn with_device_actions(
    mut page: CataloguePage,
    query: &CatalogueQuery,
    workspace: &str,
) -> CataloguePage {
    if query.provider.as_deref() == Some(PROVIDER) {
        return catalogue(query);
    }
    if query.provider.is_none() {
        page.providers.push(provider(workspace));
    }
    page
}

/// `action` of `devices`, as the catalogue publishes it.
pub(crate) fn descriptor(
    action: &str,
    label: &str,
    description: &str,
    params: &[ActionParam],
) -> ActionDescriptor {
    ActionDescriptor {
        did: None,
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(RESOURCE, action),
        resource_kind: "device".to_owned(),
        resource_label: "Devices".to_owned(),
        action_label: label.to_owned(),
        keywords: Vec::new(),
        state: tilo_actions::ActionState::default(),
        icon: None,
        description: Some(description.to_owned()),
        locus: ExecutionLocus::ClientLocal,
        kind: OperationKind::Effect,
        contract: ActionContract {
            kind: ActionKind::Outside,
            ..ActionContract::default()
        },
        input: InputSpec::None,
        params: params.to_vec(),
        view: None,
    }
}

/// The person's root on this computer, read when a ticket is asked for: the store the daemon
/// keeps, or a test's own.
pub(crate) type RootSource = Arc<dyn Fn() -> Result<Option<SigningIdentity>> + Send + Sync>;

/// What the daemon supplies the ticket server: its endpoint, the device it is, the server it
/// enters through (once it knows one), the person's root, and the workspace service a certified
/// computer's entry is written through.
struct Daemon {
    key: SigningKey,
    cert: DeviceCert,
    endpoint: Endpoint,
    /// Where this device enters its person's workspaces, on a device a ticket made.
    home: Option<Arc<super::home::Home>>,
    /// The person's root, when this computer holds it: what approves a computer a ticket adds.
    root: RootSource,
    /// The person's workspace here: on a computer no ticket made, the one a computer it adds
    /// enters. `None` where this computer holds none another computer can enter.
    first: Option<Arc<super::first_workspace::FirstWorkspace>>,
    /// The workspace service this daemon's device writes a certified computer's entry through,
    /// and the session it asks its host through while it is a member.
    workspaces: Option<Arc<ServiceWorkspaces>>,
    service: ActionService,
}

impl Daemon {
    /// Where a computer this ticket adds enters: the server this device enters through, or, on
    /// one no ticket made, the person's workspace here through this daemon's endpoint, founded
    /// now when they have none yet.
    async fn entered(&self) -> Result<Entry, String> {
        if let Some(here) = self.home.as_ref().and_then(|home| home.get()) {
            return tilo_api::daemon::enter::login_entry(&here);
        }
        let first = self.first.as_ref().ok_or_else(|| {
            "This computer holds no workspace another computer can enter.".to_owned()
        })?;
        let workspace = first.found().await?;
        let name = self
            .workspaces
            .as_ref()
            .zip(WorkspaceId::parse(&workspace))
            .and_then(|(workspaces, id)| workspaces.workspace(id))
            .map(|described| described.name);
        // The session a computer enters over proves the certificate the workspace service runs
        // under, which is the device's own unless the service has one of its own.
        let server_cert = self.workspaces.as_ref().map_or_else(
            || self.cert.clone(),
            |service| service.copies().serving_certificate().clone(),
        );
        let mut entry = tilo_api::daemon::enter::founded_entry(
            &self.endpoint,
            (workspace, String::new()),
            server_cert,
        );
        entry.workspace_name = name;
        Ok(entry)
    }
}

impl Issuer for Daemon {
    fn serving(&self) -> Result<(Reached, ActionService), String> {
        Ok((
            tilo_api::daemon::enter::reached_of(&self.endpoint.addr()),
            self.service.clone(),
        ))
    }

    fn device(&self) -> Result<(DeviceCert, Arc<dyn DeviceSigner>), String> {
        Ok((self.cert.clone(), Arc::new(self.key.clone())))
    }

    fn root(&self) -> Result<Option<Root>, String> {
        (self.root)()
            .map(|root| root.map(|root| Arc::new(root) as Root))
            .map_err(|error| format!("{error:#}"))
    }

    fn entry(&self) -> BoxFuture<'_, Result<Entry, String>> {
        Box::pin(self.entered())
    }

    fn servers(&self) -> Option<Workspaces> {
        self.workspaces
            .as_ref()
            .map(|service| service.copies().clone())
    }

    /// A member device asks its host to write the entry, through its session; one that holds the
    /// person's Servers workspace writes it itself, founding it when it founded the person's own
    /// workspaces.
    fn admit<'a>(
        &'a self,
        role: &'a Role,
        cert: &'a DeviceCert,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let service = self.workspaces.as_ref().ok_or_else(|| {
                "this computer holds no workspace service to take a computer in".to_owned()
            })?;
            let session = service.session();
            match role {
                Role::Server => {
                    server_entry(
                        service.copies(),
                        (session, cert),
                        None,
                        self.first.as_ref().is_some_and(|first| first.founds()),
                    )
                    .await
                }
                Role::Plan(plan) => plan_entry(service.copies(), session, plan, cert).await,
            }
        })
    }
}

/// Record the certified `cert` as a server in the person's Servers document, as a server's own
/// start does when it is taken in: an entry under its device key, dialled at `endpoint`, or by its
/// key alone until the server fills its endpoint itself when it first starts.
pub(super) async fn server_entry(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    (session, cert): (Option<&Entered>, &DeviceCert),
    endpoint: Option<iroh::EndpointAddr>,
    founds: bool,
) -> Result<(), String> {
    use tilo_workspace_service::workspaces::servers;

    let endpoint = match endpoint {
        Some(endpoint) => endpoint,
        None => iroh::EndpointAddr::from_parts(
            iroh::EndpointId::from_bytes(&cert.device_public_key)
                .map_err(|error| format!("the server's device key: {error}"))?,
            Vec::new(),
        ),
    };
    if let Some(device) = session {
        let bytes = tilo_user_identity::canonical::canonical_bytes(cert)
            .map_err(|error| format!("the server's certificate: {error}"))?;
        let endpoint = serde_json::to_string(&endpoint).map_err(|error| error.to_string())?;
        return device
            .host_action(
                "server.admit",
                Map::from_iter([
                    ("cert".to_owned(), hex::encode(bytes).into()),
                    ("endpoint".to_owned(), endpoint.into()),
                ]),
            )
            .await
            .map(drop)
            .map_err(|error| format!("{error}"));
    }
    if !tilo_workspace_service::workspaces::person_workspace::holds_person_workspace(workspaces)
        .await
    {
        if !founds {
            return Err(
                "this computer does not hold the person's Servers workspace yet".to_owned(),
            );
        }
        tilo_workspace_service::workspaces::person_workspace::found(workspaces).await?;
    }
    servers::admit(workspaces, cert, &endpoint).await
}

/// Record the certified `cert` on the plan's entry, as one of the person's own devices does when
/// it approves a machine: the entry becomes the machine's under its key, and the device that
/// governs the Servers workspace hands it over from there.
pub(super) async fn plan_entry(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    session: Option<&Entered>,
    plan: &str,
    cert: &DeviceCert,
) -> Result<(), String> {
    use tilo_workspace_service::workspaces::servers;

    if let Some(device) = session {
        let shown = device
            .host_action(
                "machine.show",
                Map::from_iter([("plan".to_owned(), plan.into())]),
            )
            .await
            .map_err(|error| format!("{error}"))?;
        let nonce = shown
            .pointer("/plan/token/nonce")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("plan {plan} names no token"))?;
        return device
            .host_action(
                "machine.admit",
                Map::from_iter([
                    (
                        "device".to_owned(),
                        hex::encode(cert.device_public_key).into(),
                    ),
                    ("nonce".to_owned(), nonce.into()),
                ]),
            )
            .await
            .map(drop)
            .map_err(|error| format!("{error}"));
    }
    servers::admit_planned(workspaces, plan, &cert.device_public_key).await
}

/// What this computer's daemon answers the device actions with: its endpoint, the device it is,
/// and the ticket server the one action service its router answers carries a certify call to.
#[derive(Clone)]
pub(crate) struct Tickets(Arc<Inner>);

struct Inner {
    key: SigningKey,
    endpoint: Endpoint,
    /// Where this device enters its person's workspaces, on a device a ticket made.
    home: Option<Arc<super::home::Home>>,
    place: Place,
    tickets: TicketServer,
    /// Each invocation of a device action, and where it is.
    calls: std::sync::Mutex<HashMap<InvocationId, InvocationState>>,
    runtime: tokio::runtime::Handle,
    /// The device a ticket made of this computer, once it logged in with one: the person's part
    /// of the daemon starts again as it.
    logged_in: tokio::sync::watch::Sender<Option<Box<Device>>>,
    /// The device's own certificate: what a computer holding its person's root logs in at a
    /// server as.
    cert: DeviceCert,
    /// This daemon's own workspaces, where the person's own workspace names their servers.
    workspaces: Option<Arc<ServiceWorkspaces>>,
}

impl Tickets {
    /// Serve tickets from this daemon, on `service`: the one action service its router answers
    /// the device actions on, which admits a ticket's certify call while a computer asks.
    ///
    /// # Panics
    /// Outside a runtime context, where an answer could not be followed.
    pub(crate) fn new(
        (key, cert): (SigningKey, DeviceCert),
        endpoint: Endpoint,
        home: Option<Arc<super::home::Home>>,
        root: RootSource,
        first: Option<Arc<super::first_workspace::FirstWorkspace>>,
        workspaces: Option<Arc<ServiceWorkspaces>>,
        place: Place,
        service: ActionService,
    ) -> Self {
        let tickets = TicketServer::new(Arc::new(Daemon {
            key: key.clone(),
            cert: cert.clone(),
            endpoint: endpoint.clone(),
            home: home.clone(),
            root,
            first,
            workspaces: workspaces.clone(),
            service,
        }));
        Self(Arc::new(Inner {
            key,
            endpoint,
            home,
            place,
            tickets,
            calls: std::sync::Mutex::new(HashMap::new()),
            runtime: tokio::runtime::Handle::current(),
            logged_in: tokio::sync::watch::Sender::new(None),
            cert,
            workspaces,
        }))
    }

    /// The device a ticket made of this computer through these device actions, once one did.
    pub(crate) fn logged_in(&self) -> tokio::sync::watch::Receiver<Option<Box<Device>>> {
        self.0.logged_in.subscribe()
    }

    /// Where this device enters its person's workspaces, once it knows.
    fn here(&self) -> Option<Device> {
        self.0.home.as_ref().and_then(|home| home.get())
    }

    /// Serve again the planned machines' tickets this daemon issued before it last stopped.
    pub(crate) async fn restore(&self) {
        if let Err(error) = self.0.tickets.restore().await {
            tracing::warn!(%error, "the planned machines' tickets were not served again");
        }
    }

    /// Whether `request` is one of the device actions.
    pub(crate) fn answers(request: &InvokeRequest) -> bool {
        request.target.provider == PROVIDER
            && request.target.address.resource == RESOURCE
            && matches!(
                request.target.address.action.as_str(),
                INFO | ISSUE | LOG_IN | PENDING | APPROVE | REPORTED | FOUND
            )
    }

    /// Take `request`, one of [`Self::answers`]' actions: its receipt.
    pub(crate) fn invoke(
        &self,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        if let Some(state) = self.0.calls.lock_recover().get(&invocation) {
            return Pending::ready(Ok(state.receipt()));
        }
        let InvokeArgument::Json(argument) = &request.argument else {
            return Pending::ready(Err(ActionCallError::refused(
                "a device action asks with its arguments",
            )));
        };
        let args = Value::Object(argument.clone());
        let action = request.target.address.action.clone();
        if action == FOUND {
            return Pending::ready(Err(ActionCallError::refused(format!(
                "{ALREADY_A_PERSONS}: it serves them"
            ))));
        }
        if action == INFO {
            let update = succeeded(invocation, device_info(&self.0.place, &self.0.key));
            let state = InvocationState::new(invocation, update);
            self.0
                .calls
                .lock_recover()
                .insert(invocation, state.clone());
            return Pending::ready(Ok(state.receipt()));
        }
        let accepted = InvocationState::new(invocation, InvocationUpdate::Accepted { invocation });
        let receipt = accepted.receipt();
        self.0.calls.lock_recover().insert(invocation, accepted);
        let served = self.clone();
        self.0.runtime.spawn(async move {
            let answer = match action.as_str() {
                LOG_IN => served.log_in(&args).await,
                PENDING => served.pending(&args).await,
                APPROVE => served.approve(&args).await,
                REPORTED => served.report(&args).await,
                _ if args.get("wait").and_then(Value::as_bool) == Some(true) => {
                    served.waited().await
                }
                _ => served.issue(&args).await,
            };
            let update = match answer {
                Ok(value) => succeeded(invocation, value),
                Err(error) => failed(invocation, &format!("{error:#}")),
            };
            served
                .0
                .calls
                .lock_recover()
                .insert(invocation, InvocationState::new(invocation, update));
        });
        Pending::ready(Ok(receipt))
    }

    /// Where the device action `id` is, once it was taken.
    pub(crate) fn observe(&self, id: &InvocationId) -> Option<InvocationState> {
        self.0.calls.lock_recover().get(id).cloned()
    }

    /// Serve a ticket for a new computer, approved by the person's root on this computer: the
    /// ticket as the one string the person hands on. The person who runs `tilo device add`
    /// chooses what the computer becomes; the ticket it brings carries nothing of its own.
    ///
    /// A computer with a login names the server it enters through. One with the person's root and
    /// no login is itself a host: the workspace it founded is the one the computer enters, through
    /// this daemon's endpoint and the certificate its session proves.
    async fn issue(&self, args: &Value) -> Result<Value> {
        let plan = args
            .get("plan")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|plan| !plan.is_empty())
            .map(str::to_owned);
        let role = match plan {
            Some(plan) => Some(Role::Plan(plan)),
            None if args.get("server").and_then(Value::as_bool) == Some(true) => Some(Role::Server),
            None => None,
        };
        let ticket = self
            .0
            .tickets
            .issue(role)
            .await
            .map_err(|error| anyhow!(error))?;
        Ok(json!({"ticket": ticket.encode()}))
    }

    /// Wait for the ticket served now to certify a computer, while it lasts, then for that
    /// computer to read its answer: which one it was.
    async fn waited(&self) -> Result<Value> {
        let served = self
            .0
            .tickets
            .last()
            .ok_or_else(|| anyhow!("No ticket is waiting: run `tilo device add` again."))?;
        let mut certified = served.certified();
        let cert = match tokio::time::timeout(
            Duration::from_nanos(TICKET_LIFETIME_NS),
            certified.wait_for(Option::is_some),
        )
        .await
        {
            Ok(Ok(found)) => Option::<DeviceCert>::clone(&found),
            _ => bail!(
                "No computer logged in with the ticket before it ran out: run `tilo device add` \
                 again."
            ),
        }
        .context("the certificate given is there")?;
        let mut told = served.told();
        let _ = tokio::time::timeout(TELL_WAIT, told.wait_for(|told| *told)).await;
        // The computer may ask a lost answer again: the call is answered where it is.
        tokio::time::sleep(TELL_GRACE).await;
        let bytes = tilo_user_identity::canonical::canonical_bytes(&cert)
            .map_err(|error| anyhow!("the certificate given: {error}"))?;
        Ok(json!({
            "device": hex::encode(cert.device_public_key),
            "label": cert.device_label,
            "cert": hex::encode(bytes),
        }))
    }

    /// Log in with the ticket another of the person's devices issued, as [`login_asked`] reads
    /// it: file the certificate it answers, say which device this computer became, and have the
    /// person's part of the daemon start again as it.
    async fn log_in(&self, args: &Value) -> Result<Value> {
        if let Some(line) = args.get("server").and_then(Value::as_str) {
            return self.log_in_at(line).await;
        }
        let (ticket, agent, label) = login_asked(args)?;
        let device = tilo_api::daemon::enter::log_in(
            &self.0.key,
            &self.0.endpoint,
            &self.0.place,
            ticket,
            agent,
            label,
        )
        .await?;
        let said = logged_in(&device);
        self.0.logged_in.send_replace(Some(Box::new(device)));
        Ok(said)
    }

    /// Log this computer, which holds its person's root, in at the server whose line (what it
    /// keeps in `web-host.json`) is `line`: no ticket is asked for, since the root certified this
    /// device already. The server is named among the hosts of the person's own workspace, so
    /// the copy names it from now on and every later start enters through it, and the person's
    /// part starts again through it now.
    async fn log_in_at(&self, line: &str) -> Result<Value> {
        if !self.0.cert.capabilities.can_admit_devices {
            bail!(
                "Only a computer that holds your Tilo identity logs in at a server by itself: log \
                 in here with a ticket from `tilo device add` on one of your devices."
            );
        }
        let host: tilo_server_protocol::web::PageHost =
            serde_json::from_str(line.trim()).map_err(|error| {
                anyhow!("That is not a server's line from its web-host.json: {error}")
            })?;
        let workspaces = self.0.workspaces.as_ref().ok_or_else(|| {
            anyhow!("This computer's workspaces are not open yet: try again in a moment.")
        })?;
        let device =
            tilo_api::daemon::enter::log_in_at(&self.0.key, &self.0.cert, host, None).await?;
        let address = device
            .server_address
            .as_ref()
            .ok_or_else(|| anyhow!("that server names no address to reach it at"))?;
        // Where this device keeps the person's own workspace, it names the server among its
        // hosts; where it does not yet, it takes it in through the server, which names itself
        // there, and the copy names it all the same.
        let copies = workspaces.copies();
        if tilo_workspace_service::workspaces::person_workspace::holds_person_workspace(copies)
            .await
        {
            let person =
                tilo_server_protocol::servers::person_workspace_id(&self.0.cert.user_public_key);
            copies
                .attach_server(
                    &person,
                    device.server_cert.device_public_key,
                    address.endpoint_addr.clone(),
                    tilo_peer_state::CopyRole::KeyHolder,
                )
                .await
                .map_err(|error| anyhow!("the server did not take your own workspace: {error}"))?;
        }
        let said = logged_in(&device);
        self.0.logged_in.send_replace(Some(Box::new(device)));
        Ok(said)
    }

    /// `ticket`'s text, parsed: what a device action names the host and direction by.
    fn ticket_in(args: &Value, direction: Direction) -> Result<Ticket> {
        let text = args
            .get("ticket")
            .and_then(Value::as_str)
            .context("a ticket is needed")?;
        let ticket = Ticket::parse(text)
            .map_err(|_| anyhow!("This is not a Tilo ticket: ask for a new one."))?;
        match ticket.direction {
            Direction::Login if direction == Direction::SignIn => bail!(
                "This ticket logs a computer in: run `tilo login <ticket>` with it, not \
                 `tilo device add`."
            ),
            Direction::SignIn if direction == Direction::Login => bail!(
                "This ticket signs a browser in: run `tilo device add <ticket>` with it, not \
                 `tilo login`."
            ),
            _ => {}
        }
        Ok(ticket)
    }

    /// Call `action` on the host `ticket` names, as this device, in the workspace it enters: what
    /// a device of the person's does on the host that served a ticket.
    async fn at_host(&self, ticket: &Ticket, action: &str, argument: Value) -> Result<Value> {
        use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
        let here = self
            .here()
            .ok_or_else(|| anyhow!(tilo_api::daemon::enter::NOT_LOGGED_IN))?;
        let key = self.0.key.clone();
        let issuer = tilo_api::daemon::enter::address_of(&ticket.issuer)?;
        let dialed = tilo_server_client::ServerAddress::at(issuer);
        let service = ActionService::new(key, Arc::new(NoActions));
        let device = Arc::new(service.remote(&self.0.endpoint, &dialed.addr(), &here.workspace));
        let Value::Object(argument) = argument else {
            bail!("a device action asks with its arguments");
        };
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(RESOURCE, action),
            },
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let settled = device
            .invoke_until_settled(request, Duration::from_secs(60))
            .await;
        match settled {
            Some(Ok(InvocationUpdate::Succeeded { output, .. })) => {
                Ok(output.value.unwrap_or(Value::Null))
            }
            Some(Ok(InvocationUpdate::Failed { error, .. })) => {
                bail!("{}", error.message.trim_end_matches('.'))
            }
            Some(Ok(_)) | None => bail!(tilo_api::daemon::enter::UNREACHED),
            Some(Err(error)) => Err(anyhow!(tilo_api::daemon::enter::certify_refused(&error))),
        }
    }

    /// The browser a sign-in `ticket` waits for, as the host sees it.
    async fn pending(&self, args: &Value) -> Result<Value> {
        let ticket = Self::ticket_in(args, Direction::SignIn)?;
        self.at_host(&ticket, PENDING, json!({"secret": ticket.secret_hex()}))
            .await
    }

    /// Sign the browser's certificate for the sign-in `ticket`, so it is signed in.
    async fn approve(&self, args: &Value) -> Result<Value> {
        let ticket = Self::ticket_in(args, Direction::SignIn)?;
        let cert: Option<tilo_user_identity::DeviceCert> = args
            .get("cert")
            .cloned()
            .and_then(|cert| serde_json::from_value(cert).ok());
        let here = self
            .here()
            .ok_or_else(|| anyhow!(tilo_api::daemon::enter::NOT_LOGGED_IN))?;
        let answer = self
            .at_host(
                &ticket,
                APPROVE,
                json!({
                    "secret": ticket.secret_hex(),
                    "cert": cert,
                    "label": here.cert.device_label,
                }),
            )
            .await?;
        Ok(match &cert {
            Some(cert) => json!({
                "label": cert.device_label,
                "device": hex::encode(cert.device_public_key),
            }),
            None => answer,
        })
    }

    /// Tell the browser that showed `ticket` what this computer did as it logged in.
    async fn report(&self, args: &Value) -> Result<Value> {
        let ticket = Self::ticket_in(args, Direction::Login)?;
        let report: Report =
            serde_json::from_value(Value::Object(args.as_object().cloned().unwrap_or_default()))
                .map_err(|_| anyhow!("a report names its ticket and step"))?;
        let step = serde_json::to_value(report.step)?;
        self.at_host(
            &ticket,
            REPORTED,
            json!({
                "ticket": ticket.encode(),
                "step": step,
                "agent": report.agent,
                "detail": report.detail,
            }),
        )
        .await?;
        Ok(json!({}))
    }
}

/// What this device is ([`INFO`]): its key, its host name, and the people whose certificates
/// in `place` name it, an empty list before anyone's does.
pub(crate) fn device_info(place: &Place, key: &SigningKey) -> Value {
    let device = key.verifying_key().to_bytes();
    let people: Vec<String> = tilo_device_identity::login::certified_by(place, &device)
        .iter()
        .map(hex::encode)
        .collect();
    json!({
        "device": hex::encode(device),
        "label": tilo_paths::session::host_name(),
        "people": people,
    })
}

pub(crate) fn succeeded(invocation: InvocationId, value: Value) -> InvocationUpdate {
    InvocationUpdate::Succeeded {
        invocation,
        output: ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        },
    }
}

pub(crate) fn failed(invocation: InvocationId, message: &str) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: ActionFailure {
            code: "device-action".to_owned(),
            message: message.to_owned(),
            details: None,
            retryable: false,
        },
    }
}

/// The ticket, the agent and the label a `devices#logIn` asks with.
pub(crate) fn login_asked(args: &Value) -> Result<(&str, &str, Option<&str>)> {
    let text = |name: &str| args.get(name).and_then(Value::as_str);
    let ticket = text("ticket").context("a ticket is needed")?;
    let agent = text("agent")
        .filter(|agent| !agent.trim().is_empty())
        .context("the agent that runs on this computer is needed")?;
    Ok((ticket, agent, text("label")))
}

pub(crate) use tilo_api::daemon::enter::logged_in;

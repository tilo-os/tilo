//! The two device actions this computer's daemon publishes: serving a ticket for a new computer
//! (`tilo device add`), and logging in with a ticket another of the person's devices issued
//! (`tilo login`). Both need what only the daemon has: the device's endpoint, its session, and,
//! on a computer that holds the person's root, that root.
//!
//! They are ordinary actions (`tilo.device`'s `devices#issue` and `devices#logIn`), answered by
//! the daemon's action provider, beside the person's devices themselves ([`manage`]): listing,
//! renaming, revoking and updating them. A ticket's certify action is answered by the device action
//! service the daemon's one iroh router already serves (`tilo-actions/1`), as the one call it
//! admits from a computer with no certificate yet, so no second endpoint or service is made.

mod manage;

pub(crate) use manage::OnThisDevice;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::workspaces::ServiceWorkspaces;
use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::{Signer as _, SigningKey};
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
use tilo_device_identity::login::{
    APPROVE, Approval, Approved, Approver, Asked, Declined, Device, Direction, Entry, ISSUE,
    LOG_IN, PENDING, PLANNED_TICKET_LIFETIME_NS, PROVIDER, Place, RENAME, REPORTED, RESOURCE,
    REVOKE, Report, SCOPE, TICKET_LIFETIME_NS, Ticket, UPDATE,
};
use tilo_identity::store::SigningIdentity;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::{ActionService, NoActions};
use tilo_peer_state::new_device::{ServedTicket, ServedTickets};
use tilo_session_client::device::Device as Entered;
use tilo_user_identity::{DeviceCert, RevocationList, StatementSigner};
use tilo_workspace_service::device_tickets::TicketServer;

/// How long the issuer keeps serving once the computer is certified, for it to read its answer.
const TELL_WAIT: Duration = Duration::from_secs(30);
/// How long the issuer keeps serving after it told the computer how its call ended: an answer
/// that ended with its connection is asked for again, and answered where the call is.
const TELL_GRACE: Duration = Duration::from_secs(5);
/// What the device actions' catalogue page is revised by: they are fixed, so one revision.
const REVISION: &str = "tilo.device/1";
/// What this device is ([`device_info`]).
pub(crate) const INFO: &str = "info";
/// The site a computer's login names when the device that adds it has none of its own: the
/// same one an invitation over iroh names.
const FOUNDED_SITE: &str = "https://tilo.sh/";

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
/// and logging it in with a ticket.
#[must_use]
pub(crate) fn device_only_catalogue(query: &CatalogueQuery) -> CataloguePage {
    catalogue_of(query, |action| matches!(action, INFO | LOG_IN))
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
            RENAME,
            "Rename a device",
            "What one of the person's devices is called in every list.",
            &[
                ActionParam::required("device", ParamType::Text, "The device's key."),
                ActionParam::required("name", ParamType::Text, "Its new name."),
            ],
        ))),
        row(Some(descriptor(
            REVOKE,
            "Revoke a device",
            "End one of the person's devices' logins, everywhere: signed by their root here.",
            &[
                ActionParam::required("device", ParamType::Text, "The device's key."),
                ActionParam::optional(
                    "reason",
                    ParamType::Text,
                    "retired, lost, stolen or compromised.",
                ),
            ],
        ))),
        row(Some(descriptor(
            UPDATE,
            "Update a device",
            "Update the device's apps and system, as `tilo update` does on it.",
            &[ActionParam::optional(
                "device",
                ParamType::Text,
                "The device's key: another of the person's devices carries it out itself.",
            )],
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
            "Become one of the person's devices with a ticket another of their computers printed.",
            &[
                ActionParam::required("ticket", ParamType::Text, "The ticket, as it was printed."),
                ActionParam::required("agent", ParamType::Text, "The agent that runs here."),
                ActionParam::optional(
                    "label",
                    ParamType::Text,
                    "How the devices list this computer.",
                ),
            ],
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
fn descriptor(
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
            // Taking a key back is for good: the dispatch asks the person once, as an asked
            // invocation, whoever asks for it.
            ..if action == REVOKE {
                ActionContract {
                    risk: tilo_actions::RiskLevel::Privileged,
                    confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                    ..ActionContract::default()
                }
            } else {
                ActionContract::default()
            }
        },
        input: InputSpec::None,
        params: params.to_vec(),
        view: None,
    }
}

/// The person's root on this computer, read when a ticket is asked for: the store the daemon
/// keeps, or a test's own.
pub(crate) type RootSource = Arc<dyn Fn() -> Result<Option<SigningIdentity>> + Send + Sync>;

/// A workspace this daemon founded and governs, which a computer added here enters through it.
#[derive(Clone)]
pub(crate) struct Founded {
    pub workspace: String,
    pub name: String,
}

/// What a ticket adds, beyond a member: the role its issuer chose, which the issuer's daemon
/// writes into the person's Servers document when the computer is certified.
pub(crate) enum Role {
    /// A server, which keeps the person's workspaces: a new entry under its device key.
    Server,
    /// The machine the plan `plan` describes: its entry is that plan's, keyed to the device.
    Plan(String),
}

/// The role a certified computer is taken in with, and the service this daemon's device writes
/// it through: its member copy of the person's Servers workspace, or the one it founded.
pub(crate) struct Admission {
    pub role: Role,
    pub service: Option<Arc<ServiceWorkspaces>>,
    /// Whether this daemon founded the person's own workspaces: a device with the person's root
    /// and no login makes the Servers document when nothing holds it yet.
    pub founds: bool,
    pub runtime: tokio::runtime::Handle,
}

/// The person's root on this computer approving the computer that came with a ticket: it signs
/// the computer's certificate with the root, and the approval with this device's own key, which
/// the root certified as [`Self::cert`].
pub(super) struct RootApprover<R> {
    pub root: R,
    pub device: SigningKey,
    pub cert: DeviceCert,
    /// The role the ticket named, when it named one: the certified computer is taken into the
    /// person's Servers document, by that role's ordinary action.
    pub admission: Option<Admission>,
}

impl<R: StatementSigner + Send + Sync + 'static> Approver for RootApprover<R> {
    fn approve(&self, asked: Asked) -> Approval {
        let mut cert = asked.cert.clone();
        let approved = tilo_user_identity::sign_device_cert(&mut cert, &self.root)
            .map(|()| Approved {
                cert: cert.clone(),
                approver: self.cert.clone(),
                signature: self.device.sign(&asked.digest).to_bytes(),
            })
            .map_err(|error| Declined(format!("this device's root could not sign: {error}")));
        if approved.is_ok()
            && let Some(admission) = &self.admission
        {
            admission.record(cert);
        }
        Box::pin(std::future::ready(approved))
    }
}

impl Admission {
    /// Take the certified computer in, by the role its ticket named, through the ordinary typed
    /// action on the person's Servers document.
    fn record(&self, cert: DeviceCert) {
        let Some(service) = self.service.clone() else {
            tracing::warn!(
                "this computer holds no workspace service, so a certified computer was not taken in"
            );
            return;
        };
        let role = match &self.role {
            Role::Server => Role::Server,
            Role::Plan(plan) => Role::Plan(plan.clone()),
        };
        let founds = self.founds;
        let session = service.session().cloned();
        self.runtime.spawn(async move {
            let session = session.as_ref();
            let written = match role {
                Role::Server => {
                    server_entry(service.copies(), (session, &cert), None, founds).await
                }
                Role::Plan(plan) => plan_entry(service.copies(), session, &plan, &cert).await,
            };
            if let Err(error) = written {
                tracing::warn!(%error, "a certified computer was not taken into the person's servers");
            }
        });
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
/// the login it serves (if any), and the one action service that serves a ticket's certify call.
#[derive(Clone)]
pub(crate) struct Tickets(Arc<Inner>);

struct Inner {
    key: SigningKey,
    cert: DeviceCert,
    endpoint: Endpoint,
    login: Option<(SigningKey, Device)>,
    /// The person's root, when this computer holds it: what approves a computer a ticket adds.
    root: RootSource,
    /// The workspace founded here, when this computer has no login: a computer it adds enters it.
    founded: Option<Founded>,
    /// The workspace service this daemon's device writes a certified computer's entry through,
    /// and the session it asks its host through while it is a member.
    workspaces: Option<Arc<ServiceWorkspaces>>,
    place: Place,
    service: ActionService,
    /// The ticket `tilo device add` printed last, while it waits for its computer, and whether
    /// it is a planned machine's: a later one replaces it unless it is.
    outstanding: std::sync::Mutex<Option<(ServedTicket, bool)>>,
    /// Every ticket the service answers a computer's certify call for now: planned machines'
    /// beside the one printed last.
    served: ServedTickets,
    /// The person's devices, as they are listed, named and revoked from this one.
    fleet: TicketServer,
    /// Each invocation of a device action, and where it is.
    calls: std::sync::Mutex<HashMap<InvocationId, InvocationState>>,
    /// What starts this device's update, where it is not `tilo update`.
    update: std::sync::OnceLock<manage::UpdateStart>,
    /// Where the person's devices are reached that no lookup finds.
    reach: std::sync::Mutex<HashMap<[u8; 32], iroh::EndpointAddr>>,
    runtime: tokio::runtime::Handle,
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
        login: Option<(SigningKey, Device)>,
        root: RootSource,
        founded: Option<Founded>,
        workspaces: Option<Arc<ServiceWorkspaces>>,
        place: Place,
        service: ActionService,
    ) -> Self {
        let fleet = TicketServer::new(Arc::new(manage::Fleet {
            key: key.clone(),
            cert: cert.clone(),
            endpoint: endpoint.clone(),
            root: Arc::clone(&root),
            workspaces: workspaces.clone(),
            service: service.clone(),
        }));
        Self(Arc::new(Inner {
            key,
            cert,
            endpoint,
            login,
            root,
            founded,
            workspaces,
            place,
            service,
            outstanding: std::sync::Mutex::new(None),
            served: ServedTickets::default(),
            fleet,
            calls: std::sync::Mutex::new(HashMap::new()),
            update: std::sync::OnceLock::new(),
            reach: std::sync::Mutex::default(),
            runtime: tokio::runtime::Handle::current(),
        }))
    }

    /// Whether `request` is one of the device actions.
    pub(crate) fn answers(request: &InvokeRequest) -> bool {
        request.target.provider == PROVIDER
            && request.target.address.resource == RESOURCE
            && matches!(
                request.target.address.action.as_str(),
                INFO | ISSUE | LOG_IN | PENDING | APPROVE | REPORTED | RENAME | REVOKE | UPDATE
            )
    }

    /// Whether `request` only reads what the device is ([`INFO`]), and lets nobody in.
    pub(crate) fn only_reads(request: &InvokeRequest) -> bool {
        Self::answers(request) && request.target.address.action == INFO
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
            return Pending::ready(Err(refused("a device action asks with its arguments")));
        };
        let args = Value::Object(argument.clone());
        let action = request.target.address.action.clone();
        let wait = args.get("wait").and_then(Value::as_bool) == Some(true);
        // A ticket names where the new computer dials this one: made before this device's
        // endpoint reached one of its relays, it would name only the network this device stands
        // in. It waits for the relay first, as an invitation does.
        let unreached = action == ISSUE && !wait && !reached(&self.0.endpoint);
        let waiting = matches!(
            action.as_str(),
            LOG_IN | PENDING | APPROVE | REPORTED | RENAME | REVOKE | UPDATE
        ) || wait
            || unreached;
        if !waiting {
            let answer = if action == INFO {
                Ok(device_info(&self.0.place, &self.0.key))
            } else {
                self.issue(&args)
            };
            let update = match answer {
                Ok(value) => succeeded(invocation, value),
                Err(error) => failed(invocation, &format!("{error:#}")),
            };
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
                RENAME | REVOKE | UPDATE => served.manage(&action, &args).await,
                ISSUE if !wait => {
                    tilo_endpoint_reachability::await_relay(&served.0.endpoint).await;
                    served.issue(&args)
                }
                _ => served.waited().await,
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

    fn outstanding(&self) -> Option<ServedTicket> {
        self.0
            .outstanding
            .lock_recover()
            .as_ref()
            .map(|(served, _)| served.clone())
    }

    /// The certificate this daemon's workspace session proves: the service's own, which its
    /// device issued, or the device's certificate where no service is open.
    fn serving_cert(&self) -> DeviceCert {
        self.0.workspaces.as_ref().map_or_else(
            || self.0.cert.clone(),
            |service| service.copies().serving_certificate().clone(),
        )
    }

    /// Serve a ticket for a new computer, approved by the person's root on this computer: the
    /// ticket as the one string the person hands on.
    ///
    /// A computer with a login names the server it enters through. One with the person's root and
    /// no login is itself a host: the workspace it founded is the one the computer enters, through
    /// this daemon's endpoint and the certificate its session proves.
    fn issue(&self, args: &Value) -> Result<Value> {
        let plan = args
            .get("plan")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|plan| !plan.is_empty())
            .map(str::to_owned);
        let as_server = args.get("server").and_then(Value::as_bool) == Some(true);
        let here = self.0.login.as_ref().map(|(_, device)| device);
        let (user, capabilities) = match here {
            Some(here) => (here.cert.user_public_key, &here.cert.capabilities),
            None => (self.0.cert.user_public_key, &self.0.cert.capabilities),
        };
        // A device puts on a ticket only what it holds itself: the same rule every grant follows,
        // unless it holds the person's root, which certifies whatever the person approves. One
        // running on an operating system Tilo does not control adds computers as such.
        let mut capability = tilo_user_identity::DeviceCapabilities::member();
        capability.foreign |= capabilities.foreign;
        let root = (self.0.root)()?;
        if root.is_none() && !capability.grants_no_more_than(capabilities) {
            bail!("this device holds less than the computer it would add, so it can't add one");
        }
        let root = root.ok_or_else(|| {
            anyhow!(
                "This computer does not hold your identity, so it cannot add a device: run \
                 `tilo device add` on the one that does."
            )
        })?;
        if root.public_key() != user {
            bail!(
                "This computer is logged in as another person than the one whose identity it holds."
            );
        }
        let enter = self.entry(here)?;
        let planned = plan.is_some();
        let role = match plan {
            Some(plan) => Some(Role::Plan(plan)),
            None if as_server => Some(Role::Server),
            None => None,
        };
        let admission = match role {
            Some(role) => {
                if self.0.workspaces.is_none() {
                    bail!(
                        "This computer's workspace service is not open, so it cannot take a \
                         server into the person's Servers document."
                    );
                }
                Some(Admission {
                    role,
                    service: self.0.workspaces.clone(),
                    founds: self.0.founded.is_some(),
                    runtime: self.0.runtime.clone(),
                })
            }
            None => None,
        };
        let ticket = Ticket::new(tilo_api::daemon::enter::reached_of(&self.0.endpoint.addr()));
        let served = ServedTicket::new(
            ticket.clone(),
            user,
            enter,
            capability,
            Arc::new(RootApprover {
                root,
                device: self.0.key.clone(),
                cert: self.0.cert.clone(),
                admission,
            }),
            RevocationList::new(),
            // A planned machine's medium may sit on a shelf before the machine installs from it.
            if planned {
                PLANNED_TICKET_LIFETIME_NS
            } else {
                TICKET_LIFETIME_NS
            },
        );
        // The one uncertified call the router answers is a served ticket's certify. A ticket
        // printed now replaces the one printed before, but never a planned machine's.
        let replaced = self
            .0
            .outstanding
            .lock_recover()
            .replace((served.clone(), planned));
        if let Some((replaced, false)) = replaced {
            self.0.served.withdraw(&replaced);
        }
        self.0.served.serve(served);
        self.0
            .service
            .admit_uncertified(SCOPE, Arc::new(self.0.served.clone()));
        Ok(json!({"ticket": ticket.encode()}))
    }

    /// Where a computer this ticket adds enters: the server this computer's login names, or, with
    /// no login, the workspace founded here through this daemon's endpoint.
    fn entry(&self, here: Option<&Device>) -> Result<Entry> {
        let Some(here) = here else {
            let founded = self.0.founded.as_ref().ok_or_else(|| {
                anyhow!("This computer holds no workspace another computer can enter.")
            })?;
            return Ok(Entry {
                workspace: founded.workspace.clone(),
                workspace_name: Some(founded.name.clone()),
                // The session a computer enters over proves the certificate the workspace
                // service runs under, which is the device's own unless the service has one
                // of its own.
                server_cert: self.serving_cert(),
                server: tilo_api::daemon::enter::reached_of(&self.0.endpoint.addr()),
                site: FOUNDED_SITE.to_owned(),
                host_origin: format!(
                    "{}{}",
                    tilo_api::daemon::enter::IROH_ONLY,
                    self.0.endpoint.id()
                ),
            });
        };
        let published = here
            .server_address
            .as_ref()
            .ok_or_else(|| anyhow!(tilo_api::daemon::enter::LOGIN_ENDED))?;
        let mut reached = tilo_api::daemon::enter::reached_of(&published.endpoint_addr);
        if reached.relay.is_none() {
            reached.relay.clone_from(&published.relay_url);
        }
        Ok(Entry {
            workspace: here.workspace.clone(),
            workspace_name: here.workspace_name.clone(),
            server_cert: here.server_cert.clone(),
            server: reached,
            site: here.site.clone(),
            host_origin: here.host_origin.clone(),
        })
    }

    /// Wait for the ticket served now to certify a computer, while it lasts, then for that
    /// computer to read its answer: which one it was.
    async fn waited(&self) -> Result<Value> {
        let served = self
            .outstanding()
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
        // The person's other devices list it as soon as the host keeps it there.
        self.0.tickets.keep_listed(&cert).await;
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
    /// it: keep the login it answers and say which device this computer became.
    async fn log_in(&self, args: &Value) -> Result<Value> {
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
        Ok(logged_in(&device))
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

    /// Call `action` on the host `ticket` names, as this computer's login, in the workspace that
    /// login enters: what a device of the person's does on the host that served a ticket.
    async fn at_host(&self, ticket: &Ticket, action: &str, argument: Value) -> Result<Value> {
        use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
        let (key, here) = tilo_device_identity::login::load(&self.0.place)?
            .ok_or_else(|| anyhow!(tilo_api::daemon::enter::NOT_LOGGED_IN))?;
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
        let (_key, here) = tilo_device_identity::login::load(&self.0.place)?
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

fn refused(reason: impl Into<String>) -> ActionCallError {
    ActionCallError::Refused {
        reason: reason.into(),
    }
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

/// What `devices#logIn` answers: the device this computer became.
pub(crate) fn logged_in(device: &tilo_device_identity::login::Device) -> Value {
    json!({
        "label": device.label(),
        "workspace": device.workspace,
        "workspace_name": device.workspace_name,
        "host_origin": device.host_origin,
    })
}

/// Whether `endpoint` names where it is reached from another network: one of its relays answers
/// it, or this device has none to wait for.
fn reached(endpoint: &iroh::Endpoint) -> bool {
    endpoint.addr().relay_urls().next().is_some()
        || tilo_endpoint_reachability::relays().relays.is_empty()
}

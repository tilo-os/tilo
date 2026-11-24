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

mod agent_key;
mod manage;

pub(crate) use manage::OnThisDevice;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::workspaces::ServiceWorkspaces;
use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
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
    APPROVE, BACKUP, CERTIFY_AGENT, Device, Direction, FOUND, IDENTITIES, ISSUE, LOG_IN, PENDING,
    PROTECT_IDENTITY, PROVIDER, Place, RECOVERY_WORDS, REMOVE_IDENTITY, RENAME, REPORTED, RESOURCE,
    RESTORE, REVOKE, Report, TICKET_LIFETIME_NS, Ticket, UPDATE, USE_IDENTITY,
};
use tilo_identity::store::SigningIdentity;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::{ActionService, NoActions};
use tilo_session_client::device::Device as Entered;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::device_tickets::{Role, TicketServer};

/// How long the issuer keeps serving once the computer is certified, for it to read its answer.
const TELL_WAIT: Duration = Duration::from_secs(30);
/// How long the issuer keeps serving after it told the computer how its call ended: an answer
/// that ended with its connection is asked for again, and answered where the call is.
const TELL_GRACE: Duration = Duration::from_secs(5);
/// What the device actions' catalogue page is revised by: they are fixed, so one revision.
const REVISION: &str = "tilo.device/1";
/// What this device is ([`device_info`]).
pub(crate) const INFO: &str = "info";
/// The resource the update actions are on: a server's turn among its person's
/// servers, which the automatic update path asks its own daemon for.
pub(crate) const UPDATES: &str = "updates";
/// May this machine restart into the staged image now? Arguments: `target`.
pub(crate) const PREPARE: &str = "prepare";
/// This machine booted `image` and passed its health verdict. Arguments: `image`.
pub(crate) const BOOTED: &str = "booted";
/// Ask the person's devices for this server's rollback floor to move.
pub(crate) const FLOOR: &str = "floor";
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
    catalogue_of(query, |action| !matches!(action, FOUND | RESTORE))
}

/// [`catalogue`] with only what a device answers before its person's services run: what it is,
/// logging it in with a ticket, and making it a new or a restored person's first device.
#[must_use]
pub(crate) fn device_only_catalogue(query: &CatalogueQuery) -> CataloguePage {
    catalogue_of(query, |action| {
        matches!(action, INFO | LOG_IN | FOUND | RESTORE)
    })
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
            "Start as a new person",
            "Make a new person whose first device this computer is, with their own workspace.",
            &[ActionParam::optional(
                "name",
                ParamType::Text,
                "What the person is called: the computer account's name when left out.",
            )],
        ))),
        row(Some(descriptor(
            RESTORE,
            "Restore a person",
            "Bring a person back on this computer from their recovery words or a backup.",
            &[
                ActionParam::optional("words", ParamType::Text, "The 24 recovery words."),
                ActionParam::optional(
                    "backup",
                    ParamType::Text,
                    "A backup Tilo wrote, its bytes in hexadecimal.",
                ),
                ActionParam::optional(
                    "passphrase",
                    ParamType::Text,
                    "The passphrase the backup was sealed with.",
                ),
            ],
        ))),
        row(Some(descriptor(
            RECOVERY_WORDS,
            "Show the recovery words",
            "The 24 words that bring the person back on a new computer, from the root kept here.",
            &[],
        ))),
        row(Some(descriptor(
            CERTIFY_AGENT,
            "Certify an agent's key",
            "Certify the key of an agent that acts for the person, with the root kept here.",
            &[ActionParam::required(
                "grant",
                ParamType::Text,
                "The agent's grant key, as hex.",
            )],
        ))),
        row(Some(descriptor(
            IDENTITIES,
            "List the identities",
            "The person's identities this device keeps, without secrets, and which one new apps \
                 sign in as.",
            &[],
        ))),
        row(Some(descriptor(
            USE_IDENTITY,
            "Use an identity for new apps",
            "Sign apps opened from now on in as this identity.",
            &[IDENTITY_ID, IDENTITY_CONFIRM],
        ))),
        row(Some(descriptor(
            REMOVE_IDENTITY,
            "Remove an identity",
            "Delete an identity, with its key, from this device.",
            &[IDENTITY_ID, IDENTITY_CONFIRM],
        ))),
        row(Some(descriptor(
            PROTECT_IDENTITY,
            "Protect an identity's key",
            "Move an identity's key from a plain file into protected storage.",
            &[IDENTITY_ID, IDENTITY_CONFIRM],
        ))),
        row(Some(descriptor(
            BACKUP,
            "Back up the person",
            "A backup of the person's root kept here, sealed with a passphrase.",
            &[ActionParam::required(
                "passphrase",
                ParamType::Text,
                "What the backup is sealed with.",
            )],
        ))),
    ];
    // A server's turn among its person's servers, which its own daemon answers:
    // the automatic update path asks the machine itself before it reboots.
    let updates = CatalogueResource {
        id: UPDATES.to_owned(),
        kind: "update".to_owned(),
        label: "Updates".to_owned(),
        presentation: None,
    };
    let update_row = |action| CatalogueRow {
        provider: PROVIDER.to_owned(),
        app_id: UPDATES.to_owned(),
        parent_resource: String::new(),
        resource: updates.clone(),
        action,
        ..CatalogueRow::default()
    };
    let update_rows = vec![
        update_row(None),
        update_row(Some(update_descriptor(
            PREPARE,
            "Take this server's update turn",
            "Take the person's servers' update lease, check every workspace's copies, and drain \
             this server, so it may restart into the staged image alone.",
            &[ActionParam::required(
                "target",
                ParamType::Text,
                "The staged image it would restart into.",
            )],
        ))),
        update_row(Some(update_descriptor(
            BOOTED,
            "An update booted",
            "Say which image this server booted and that it passed its health verdict, so the \
             update lease goes to the person's next server.",
            &[ActionParam::required(
                "image",
                ParamType::Text,
                "The image it booted.",
            )],
        ))),
        update_row(Some(update_descriptor(
            FLOOR,
            "Ask the rollback floor to move",
            "Ask the person's other devices for approvals for this server's floor to move past \
             the generation that booted.",
            &[
                ActionParam::required(
                    "generation",
                    ParamType::Integer,
                    "The generation that booted.",
                ),
                ActionParam::required("image", ParamType::Text, "The image it booted."),
                ActionParam::required("updates", ParamType::Text, "The updates it follows."),
                ActionParam::required(
                    "authority",
                    ParamType::Text,
                    "The public half of the person's floor key.",
                ),
                ActionParam::required(
                    "request",
                    ParamType::Text,
                    "What its TPM states, as `tilo-rollback floor-request` wrote it.",
                ),
            ],
        ))),
    ];
    // A device before its person's services run takes no update turn.
    let update_rows = if keep(PREPARE) {
        update_rows
    } else {
        Vec::new()
    };
    let rows = rows
        .into_iter()
        .chain(update_rows)
        .filter(|row| {
            row.action
                .as_ref()
                .is_none_or(|action| keep(&action.address.action))
        })
        .collect();
    CataloguePage::from_rows(REVISION.to_owned(), rows, query)
}

/// Ask the person's devices, from this machine, for its rollback floor to move,
/// and keep what they signed where `tilo-rollback-confirm` spends it.
async fn floor_answer(
    machine: &tilo_server::rollout::ServerMachine,
    args: &Value,
) -> Result<Value, String> {
    let text = |field: &str| {
        args.get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|said| !said.trim().is_empty())
            .ok_or_else(|| format!("floor names no {field}"))
    };
    let asked = tilo_server::servers::floor::Asked {
        generation: args
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| "floor names no generation".to_owned())?,
        image: text("image")?,
        updates: text("updates")?,
        authority: text("authority")?,
        request: text("request")?,
    };
    let staging = machine.state_root().join("approvals-next");
    let answer = match machine.floor(asked).await? {
        Ok(approvals) => {
            tilo_server::rollout::store_approvals(&staging, &approvals)?;
            tilo_server::rollout::Answer::Floor {
                approvals,
                why: String::new(),
            }
        }
        Err(why) => tilo_server::rollout::Answer::Floor {
            approvals: Vec::new(),
            why,
        },
    };
    serde_json::to_value(answer).map_err(|error| error.to_string())
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
    let mut descriptor = ActionDescriptor {
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
    };
    if SECRET_ANSWERS.contains(&action) {
        descriptor
            .contract
            .tags
            .push(tilo_actions::SECRET_ANSWER_TAG.to_owned());
    }
    if ROOT_ONLY.contains(&action) {
        descriptor
            .contract
            .tags
            .push(tilo_actions::ROOT_ONLY_TAG.to_owned());
    }
    descriptor
}

/// The identity an identity step names.
const IDENTITY_ID: ActionParam = ActionParam::required(
    "id",
    ParamType::Text,
    "The identity's id, as the list answers it.",
);
/// The person's explicit word for an identity change.
const IDENTITY_CONFIRM: ActionParam = ActionParam::required(
    "confirm",
    ParamType::Boolean,
    "The person confirms the change.",
);

/// The device actions whose answer is a secret ([`tilo_actions::SECRET_ANSWER_TAG`]): handed to
/// the person once, and kept nowhere here.
const SECRET_ANSWERS: [&str; 2] = [RECOVERY_WORDS, BACKUP];

/// The device actions only the person's root signs ([`tilo_actions::ROOT_ONLY_TAG`]): a device that
/// keeps none is capped for them, and they are answered at one that does.
const ROOT_ONLY: [&str; 2] = [REVOKE, CERTIFY_AGENT];

/// The device actions as the device's one dispatch runs them: it admitted the caller, recorded the
/// invocation, and follows it here.
impl tilo_actions::remote::ActionProvider for Tickets {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    fn query(
        &self,
        _caller: &tilo_actions::remote::Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(catalogue(&query)))
    }

    fn invoke(
        &self,
        _caller: &tilo_actions::remote::Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Tickets::invoke(self, &request)
    }

    fn observe(
        &self,
        _caller: &tilo_actions::remote::Caller,
        id: &InvocationId,
    ) -> Option<InvocationState> {
        Tickets::observe(self, id)
    }

    fn cancel(
        &self,
        _caller: &tilo_actions::remote::Caller,
        id: &InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(match Tickets::observe(self, id) {
            Some(_) => ActionCallError::NotCancellable {
                reason: "a device action ends by itself".into(),
            },
            None => ActionCallError::UnknownInvocation,
        }))
    }
}

/// `action` of [`UPDATES`], as the catalogue publishes it: the same shape as a
/// device action, on the update resource.
fn update_descriptor(
    action: &str,
    label: &str,
    description: &str,
    params: &[ActionParam],
) -> ActionDescriptor {
    let mut descriptor = descriptor(action, label, description, params);
    descriptor.address = tilo_actions::ActionAddress::new(UPDATES, action);
    descriptor.resource_kind = "update".to_owned();
    descriptor.resource_label = "Updates".to_owned();
    descriptor
}

/// The person's root on this computer, read when a ticket is asked for: the store the daemon
/// keeps, or a test's own.
pub(crate) type RootSource = Arc<dyn Fn() -> Result<Option<SigningIdentity>> + Send + Sync>;

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
    /// Where this device enters its person's workspaces, on a device a ticket made.
    home: Option<Arc<super::home::Home>>,
    /// The person's root, when this computer holds it: what approves a computer a ticket adds.
    root: RootSource,
    /// The person's workspace here, on a computer no ticket made: the one a computer it adds
    /// enters, founded when one first does.
    first: Option<Arc<super::first_workspace::FirstWorkspace>>,
    /// The workspace service this daemon's device writes a certified computer's entry through,
    /// and the session it asks its host through while it is a member.
    workspaces: Option<Arc<ServiceWorkspaces>>,
    place: Place,
    service: ActionService,
    /// The tickets this device serves, and the person's devices as they are named and revoked
    /// from this one.
    fleet: TicketServer,
    /// Each invocation of a device action, and where it is.
    calls: std::sync::Mutex<HashMap<InvocationId, InvocationState>>,
    /// What starts this device's update, where it is not `tilo update`.
    update: std::sync::OnceLock<manage::UpdateStart>,
    /// This machine's part in its person's update rollout, when this computer is
    /// one of their servers: what the update actions take its turn through.
    rollout: std::sync::Mutex<Option<tilo_server::rollout::ServerMachine>>,
    /// Where the person's devices are reached that no lookup finds.
    reach: std::sync::Mutex<HashMap<[u8; 32], iroh::EndpointAddr>>,
    /// The agents' keys this daemon serves a sign-in ticket for, one per agent.
    agent_keys: std::sync::Mutex<Vec<agent_key::AgentKey>>,
    /// Told whenever a device action that waited ended.
    ended: tokio::sync::Notify,
    runtime: tokio::runtime::Handle,
    /// The device a ticket made of this computer, once it logged in with one: the person's part
    /// of the daemon starts again as it.
    logged_in: tokio::sync::watch::Sender<Option<Box<Device>>>,
}

impl Tickets {
    /// Serve again every planned machine's ticket this daemon issued before it last stopped and
    /// that still waits for its machine.
    pub(crate) async fn restore(&self) {
        if let Err(error) = self.0.fleet.restore().await {
            tracing::warn!(%error, "the planned machines' tickets are not served again yet");
        }
    }

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
        let fleet = TicketServer::new(Arc::new(manage::Fleet {
            key: key.clone(),
            cert: cert.clone(),
            endpoint: endpoint.clone(),
            home: home.clone(),
            root: Arc::clone(&root),
            first: first.clone(),
            workspaces: workspaces.clone(),
            service: service.clone(),
        }));
        Self(Arc::new(Inner {
            key,
            cert,
            endpoint,
            home,
            root,
            first,
            workspaces,
            place,
            service,
            fleet,
            calls: std::sync::Mutex::new(HashMap::new()),
            update: std::sync::OnceLock::new(),
            rollout: std::sync::Mutex::new(None),
            reach: std::sync::Mutex::default(),
            agent_keys: std::sync::Mutex::default(),
            ended: tokio::sync::Notify::new(),
            runtime: tokio::runtime::Handle::current(),
            logged_in: tokio::sync::watch::Sender::new(None),
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

    /// This machine's part in its person's update rollout, when this computer is
    /// one of their servers: the update actions take its turn through it.
    pub(crate) fn with_rollout(self, rollout: Option<tilo_server::rollout::ServerMachine>) -> Self {
        *self.0.rollout.lock_recover() = rollout;
        self
    }

    /// Whether `request` is one of the device actions.
    pub(crate) fn answers(request: &InvokeRequest) -> bool {
        request.target.provider == PROVIDER
            && ((request.target.address.resource == RESOURCE
                && matches!(
                    request.target.address.action.as_str(),
                    INFO | ISSUE
                        | LOG_IN
                        | PENDING
                        | APPROVE
                        | REPORTED
                        | RENAME
                        | REVOKE
                        | UPDATE
                        | RECOVERY_WORDS
                        | BACKUP
                        | CERTIFY_AGENT
                        | IDENTITIES
                        | USE_IDENTITY
                        | REMOVE_IDENTITY
                        | PROTECT_IDENTITY
                ))
                || (request.target.address.resource == UPDATES
                    && matches!(
                        request.target.address.action.as_str(),
                        PREPARE | BOOTED | FLOOR
                    )))
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
        if request.target.address.resource == UPDATES {
            return self.take_the_update_turn(request);
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
            ISSUE | LOG_IN | PENDING | APPROVE | REPORTED | RENAME | REVOKE | UPDATE
        );
        if !waiting {
            let answer = match action.as_str() {
                INFO => Ok(device_info(&self.0.place, &self.0.key)),
                RECOVERY_WORDS => super::services::recovery_words()
                    .map(|words| json!({ "words": words }))
                    .map_err(Into::into),
                BACKUP => super::services::backup(
                    args.get("passphrase")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )
                .map(|backup| json!({ "backup": hex::encode(backup) }))
                .map_err(Into::into),
                CERTIFY_AGENT => self.certify_agent(&args),
                _ => super::services::identity_step(&action, &args).map_err(Into::into),
            };
            let update = match answer {
                Ok(value) => succeeded(invocation, value),
                Err(error) => failed(invocation, &format!("{error:#}")),
            };
            let state = InvocationState::new(invocation, update);
            let receipt = state.receipt();
            // A secret is handed over in the receipt alone: what is kept of the call says only
            // that it was given.
            let kept = if SECRET_ANSWERS.contains(&action.as_str())
                && matches!(state.update, InvocationUpdate::Succeeded { .. })
            {
                InvocationState::new(invocation, succeeded(invocation, json!({ "given": true })))
            } else {
                state
            };
            self.0.calls.lock_recover().insert(invocation, kept);
            return Pending::ready(Ok(receipt));
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
                _ if wait => served.waited().await,
                _ => {
                    if unreached {
                        tilo_endpoint_reachability::await_relay(&served.0.endpoint).await;
                    }
                    served.issue(&args).await
                }
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
            served.0.ended.notify_waiters();
        });
        Pending::ready(Ok(receipt))
    }

    /// Take `request` and wait for it to end: how it ended.
    pub(crate) async fn run_to_end(&self, request: &InvokeRequest) -> InvocationUpdate {
        let invocation = request.invocation;
        if let Some(Err(error)) = Tickets::invoke(self, request).await {
            return failed(invocation, &error.to_string());
        }
        loop {
            let ended = self.0.ended.notified();
            tokio::pin!(ended);
            ended.as_mut().enable();
            match self.observe(&invocation) {
                Some(state) if state.update.is_terminal() => return state.update,
                None => return failed(invocation, "the device action was not taken"),
                Some(_) => ended.await,
            }
        }
    }

    /// Take `request`, one of the update actions: this machine's turn among its
    /// person's servers, answered once the machine replied.
    fn take_the_update_turn(
        &self,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        if let Some(state) = self.0.calls.lock_recover().get(&invocation) {
            return Pending::ready(Ok(state.receipt()));
        }
        let InvokeArgument::Json(argument) = &request.argument else {
            return Pending::ready(Err(refused("an update action asks with its arguments")));
        };
        let args = Value::Object(argument.clone());
        let action = request.target.address.action.clone();
        let Some(machine) = self.0.rollout.lock_recover().clone() else {
            return Pending::ready(Err(refused(
                "this computer keeps no workspaces: it has no update turn to take",
            )));
        };
        let (completer, pending) = tilo_actions::remote::pending();
        let served = self.clone();
        self.0.runtime.spawn(async move {
            let answer = match action.as_str() {
                FLOOR => floor_answer(&machine, &args).await,
                other => tilo_server::rollout::answer_action(&machine, other, &args)
                    .await
                    .and_then(|answer| {
                        serde_json::to_value(answer).map_err(|error| error.to_string())
                    }),
            };
            let update = match answer {
                Ok(value) => succeeded(invocation, value),
                Err(why) => failed(invocation, &why),
            };
            let state = InvocationState::new(invocation, update);
            served
                .0
                .calls
                .lock_recover()
                .insert(invocation, state.clone());
            served.0.ended.notify_waiters();
            completer.complete(Ok(state.receipt()));
        });
        pending
    }

    /// Where the device action `id` is, once it was taken.
    pub(crate) fn observe(&self, id: &InvocationId) -> Option<InvocationState> {
        self.0.calls.lock_recover().get(id).cloned()
    }

    /// Whether `request` asks one of the device actions whose answer is a secret
    /// ([`tilo_actions::SECRET_ANSWER_TAG`]), as their catalogue rows say.
    pub(crate) fn answers_a_secret(request: &InvokeRequest) -> bool {
        Self::listed(request).is_some_and(|action| action.contract.answers_a_secret())
    }

    /// Whether `request` asks one of the device actions only the person's root signs
    /// ([`tilo_actions::ROOT_ONLY_TAG`]), as their catalogue rows say.
    pub(crate) fn root_only(request: &InvokeRequest) -> bool {
        Self::listed(request).is_some_and(|action| action.contract.root_only())
    }

    /// The device action `request` asks, as the catalogue lists it.
    pub(crate) fn listed(request: &InvokeRequest) -> Option<ActionDescriptor> {
        if !Self::answers(request) {
            return None;
        }
        catalogue(&CatalogueQuery::default())
            .rows
            .into_iter()
            .filter_map(|row| row.action)
            .find(|action| action.address.action == request.target.address.action)
    }

    /// Serve a ticket for a new computer, approved by the person's root on this computer: the
    /// ticket as the one string the person hands on. The person who runs `tilo device add`
    /// chooses what the computer becomes; the ticket it brings carries nothing of its own.
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
            .fleet
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
            .fleet
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
        // A key approved for an agent is one of the person's keys like every other: listed with
        // their devices, and revoked as one is (`devices#revoke`).
        if let (Some(cert), Some(workspaces)) = (&cert, &self.0.workspaces)
            && cert.capabilities.agent.is_some()
            && let Err(error) = tilo_workspace_service::workspaces::person_workspace::devices::keep(
                workspaces.copies(),
                cert,
            )
            .await
        {
            tracing::warn!(%error, "an agent's approved key is not listed among the person's devices");
        }
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
    failed_as(invocation, "device-action", message)
}

/// The device action `invocation` failed, with the refusal's stable `code`.
pub(crate) fn failed_as(invocation: InvocationId, code: &str, message: &str) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: ActionFailure {
            code: code.to_owned(),
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

/// Whether `endpoint` names where it is reached from another network: one of its relays answers
/// it, or this device has none to wait for.
fn reached(endpoint: &iroh::Endpoint) -> bool {
    endpoint.addr().relay_urls().next().is_some()
        || tilo_endpoint_reachability::relays().relays.is_empty()
}

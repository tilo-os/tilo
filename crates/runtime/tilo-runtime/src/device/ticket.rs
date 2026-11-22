//! The two device actions this computer's daemon publishes: serving a ticket for a new computer
//! (`tilo device add`), and logging in with a ticket another of the person's devices issued
//! (`tilo login`). Both need what only the daemon has: the device's endpoint, its session, and,
//! on a computer that holds the person's root, that root.
//!
//! They are ordinary actions (`tilo.device`'s `devices#issue` and `devices#logIn`), answered by
//! the daemon's action provider. A ticket's certify action is served on the daemon's one iroh
//! router (`super::services::serve_personally` accepts the device actions ALPN beside the person's
//! store), so no second endpoint is made.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::{Signer as _, SigningKey};
use iroh::Endpoint;
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueResource,
    CatalogueRow, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
};
use tilo_actions::{
    ActionContract, ActionDescriptor, ActionFailure, ActionKind, ActionOutput, ActionParam,
    ExecutionLocus, InputSpec, InvocationId, InvocationUpdate, OperationKind, ParamType,
};
use tilo_device_identity::login::{
    Approval, Approved, Approver, Asked, CERTIFY, CertifyAnswer, CertifyInput, Declined, Device,
    Entry, ISSUE, LOG_IN, PROVIDER, Place, RESOURCE, SCOPE, TICKET_LIFETIME_NS, Ticket,
};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::{ActionService, NoActions};
use tilo_peer_state::new_device::ServedTicket;
use tilo_user_identity::{DeviceCert, RevocationList, StatementSigner};

/// How long the issuer keeps serving once the computer is certified, for it to read its answer.
const TELL_WAIT: Duration = Duration::from_secs(30);
/// How long the issuer keeps serving after it told the computer how its call ended: an answer
/// that ended with its connection is asked for again, and answered where the call is.
const TELL_GRACE: Duration = Duration::from_secs(5);
/// How many times the call asking the device to certify this computer is sent.
const CERTIFY_TRIES: u32 = 3;
/// What the device actions' catalogue page is revised by: they are fixed, so one revision.
const REVISION: &str = "tilo.device/1";

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

/// The `tilo.device` page `query` asks for: the `devices` resource and its two actions, resolved
/// and invoked the way an app's are.
#[must_use]
pub(crate) fn catalogue(query: &CatalogueQuery) -> CataloguePage {
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
    CataloguePage::from_rows(
        REVISION.to_owned(),
        vec![
            row(None),
            row(Some(descriptor(
                ISSUE,
                "Add a device",
                "Print a ticket another computer logs in with, approved by the person's root here.",
                &[ActionParam::optional(
                    "wait",
                    ParamType::Boolean,
                    "Wait for the computer to log in with the ticket.",
                )],
            ))),
            row(Some(descriptor(
                LOG_IN,
                "Log in with a ticket",
                "Become one of the person's devices with a ticket another of their computers printed.",
                &[
                    ActionParam::required(
                        "ticket",
                        ParamType::Text,
                        "The ticket, as it was printed.",
                    ),
                    ActionParam::required("agent", ParamType::Text, "The agent that runs here."),
                    ActionParam::optional(
                        "label",
                        ParamType::Text,
                        "How the devices list this computer.",
                    ),
                ],
            ))),
        ],
        query,
    )
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
            ..ActionContract::default()
        },
        input: InputSpec::None,
        params: params.to_vec(),
        view: None,
    }
}

/// The person's root on this computer approving the computer that came with a ticket: it signs
/// the computer's certificate with the root, and the approval with this device's own key, which
/// the root certified as [`Self::cert`].
pub(super) struct RootApprover<R> {
    pub root: R,
    pub device: SigningKey,
    pub cert: DeviceCert,
}

impl<R: StatementSigner + Send + Sync + 'static> Approver for RootApprover<R> {
    fn approve(&self, asked: Asked) -> Approval {
        let mut cert = asked.cert.clone();
        let approved = tilo_user_identity::sign_device_cert(&mut cert, &self.root)
            .map(|()| Approved {
                cert,
                approver: self.cert.clone(),
                signature: self.device.sign(&asked.digest).to_bytes(),
            })
            .map_err(|error| Declined(format!("this device's root could not sign: {error}")));
        Box::pin(std::future::ready(approved))
    }
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
    place: Place,
    service: ActionService,
    /// The ticket the service answers for now, while it waits for its computer.
    outstanding: std::sync::Mutex<Option<ServedTicket>>,
    /// Each invocation of a device action, and where it is.
    calls: std::sync::Mutex<HashMap<InvocationId, InvocationState>>,
    runtime: tokio::runtime::Handle,
}

impl Tickets {
    /// Serve tickets from this daemon.
    ///
    /// # Panics
    /// Outside a runtime context, where an answer could not be followed.
    pub(crate) fn new(
        key: SigningKey,
        cert: DeviceCert,
        endpoint: Endpoint,
        login: Option<(SigningKey, Device)>,
        place: Place,
    ) -> Self {
        let service = ActionService::new(key.clone(), Arc::new(NoActions));
        Self(Arc::new(Inner {
            key,
            cert,
            endpoint,
            login,
            place,
            service,
            outstanding: std::sync::Mutex::new(None),
            calls: std::sync::Mutex::new(HashMap::new()),
            runtime: tokio::runtime::Handle::current(),
        }))
    }

    /// The action service the daemon's router answers the device actions ALPN with.
    pub(crate) fn service(&self) -> ActionService {
        self.0.service.clone()
    }

    /// Whether `request` is one of the two device actions.
    pub(crate) fn answers(request: &InvokeRequest) -> bool {
        request.target.provider == PROVIDER
            && request.target.address.resource == RESOURCE
            && matches!(request.target.address.action.as_str(), ISSUE | LOG_IN)
    }

    /// What the issued certificate grants, as `args` names it: a member device, or one that
    /// administers the person's others. The person who runs `tilo device add` chooses it; the
    /// ticket the new computer brings carries no capability of its own.
    fn capability(args: &Value) -> tilo_user_identity::DeviceCapabilities {
        match args.get("capability").and_then(Value::as_str) {
            Some("admin") => tilo_user_identity::DeviceCapabilities::administrator(),
            _ => tilo_user_identity::DeviceCapabilities::member(),
        }
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
        let waiting = action == LOG_IN || args.get("wait").and_then(Value::as_bool) == Some(true);
        if !waiting {
            let update = match self.issue(&args) {
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
        self.0.outstanding.lock_recover().clone()
    }

    /// Serve a ticket for a new computer, approved by the person's root on this computer: the
    /// ticket as the one string the person hands on.
    fn issue(&self, args: &Value) -> Result<Value> {
        let here = self
            .0
            .login
            .as_ref()
            .map(|(_, device)| device)
            .ok_or_else(|| anyhow!(tilo_api::daemon::enter::NOT_LOGGED_IN))?;
        // A device puts on a ticket only what it holds itself: the same rule every grant follows.
        let capability = Self::capability(args);
        if !capability.grants_no_more_than(&here.cert.capabilities) {
            bail!("this device isn't an administrator, so it can't add one");
        }
        let root = super::person::root()?.ok_or_else(|| {
            anyhow!(
                "This computer does not hold your identity, so it cannot add a device: run \
                 `tilo device add` on the one that does."
            )
        })?;
        if root.public_key() != here.cert.user_public_key {
            bail!(
                "This computer is logged in as another person than the one whose identity it holds."
            );
        }
        let published = here
            .server_address
            .as_ref()
            .ok_or_else(|| anyhow!(tilo_api::daemon::enter::LOGIN_ENDED))?;
        let ticket = Ticket::new(tilo_api::daemon::enter::reached_of(&self.0.endpoint.addr()));
        let mut reached = tilo_api::daemon::enter::reached_of(&published.endpoint_addr);
        if reached.relay.is_none() {
            reached.relay.clone_from(&published.relay_url);
        }
        let enter = Entry {
            workspace: here.workspace.clone(),
            workspace_name: here.workspace_name.clone(),
            server_cert: here.server_cert.clone(),
            server: reached,
            site: here.site.clone(),
            host_origin: here.host_origin.clone(),
        };
        let served = ServedTicket::new(
            ticket.clone(),
            here.cert.user_public_key,
            enter,
            capability,
            Arc::new(RootApprover {
                root,
                device: self.0.key.clone(),
                cert: self.0.cert.clone(),
            }),
            RevocationList::new(),
        );
        // The one uncertified call the router now answers is this ticket's certify.
        self.0
            .service
            .admit_uncertified(SCOPE, Arc::new(served.clone()));
        *self.0.outstanding.lock_recover() = Some(served);
        Ok(json!({"ticket": ticket.encode()}))
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

    /// Log in with `ticket` another of the person's devices issued, as `agent`, listed as
    /// `label`: keep the login it answers and say which device this computer became.
    async fn log_in(&self, args: &Value) -> Result<Value> {
        let ticket = args
            .get("ticket")
            .and_then(Value::as_str)
            .context("a ticket is needed")?;
        let ticket = Ticket::parse(ticket)
            .map_err(|_| anyhow!("This is not a Tilo ticket: ask for a new one."))?;
        let agent = args
            .get("agent")
            .and_then(Value::as_str)
            .filter(|agent| !agent.trim().is_empty())
            .context("the agent that runs on this computer is needed")?;
        let label = args.get("label").and_then(Value::as_str);
        let answer = self.certified_with(&ticket, agent, label).await?;
        let device = tilo_api::daemon::enter::device_of(answer, agent);
        tilo_device_identity::login::store(&self.0.place, &device)
            .context("keeping this computer's login")?;
        Ok(json!({
            "label": device.label(),
            "workspace": device.workspace,
            "workspace_name": device.workspace_name,
            "host_origin": device.host_origin,
        }))
    }

    /// Ask the device that issued `ticket` to have the person's root certify this computer's key,
    /// for `agent`, listed as `label`: its answer, once the person approved. A call lost on its
    /// way is asked again with the same invocation, which the issuer answers where the call is.
    async fn certified_with(
        &self,
        ticket: &Ticket,
        agent: &str,
        label: Option<&str>,
    ) -> Result<CertifyAnswer> {
        use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
        let issuer = tilo_api::daemon::enter::address_of(&ticket.issuer)?;
        let dialed = tilo_server_client::ServerAddress::at(issuer);
        let input = CertifyInput {
            secret: ticket.secret_hex(),
            device_public_key: hex::encode(self.0.key.verifying_key().to_bytes()),
            agent: agent.to_owned(),
            label: label.map(str::to_owned),
        };
        let serde_json::Value::Object(argument) = serde_json::to_value(&input)? else {
            bail!("the certify input is an object");
        };
        let service = ActionService::new(self.0.key.clone(), Arc::new(NoActions));
        let device = Arc::new(service.remote(&self.0.endpoint, &dialed.addr(), SCOPE));
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(RESOURCE, CERTIFY),
            },
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let now = std::time::Instant::now();
        let deadline = now
            .checked_add(Duration::from_nanos(TICKET_LIFETIME_NS))
            .unwrap_or(now);
        let mut sent = 1;
        let settled = loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let answer = Arc::clone(&device)
                .invoke_until_settled(request.clone(), left)
                .await;
            match answer {
                Some(Err(error))
                    if sent < CERTIFY_TRIES
                        && matches!(error, ActionCallError::Unavailable { .. }) =>
                {
                    sent = sent.saturating_add(1);
                }
                answer => break answer,
            }
        };
        match settled {
            Some(Ok(InvocationUpdate::Succeeded { output, .. })) => output
                .value
                .and_then(|value| serde_json::from_value(value).ok())
                .ok_or_else(|| anyhow!(tilo_api::daemon::enter::TOO_OLD)),
            Some(Ok(InvocationUpdate::Failed { error, .. })) => Err(anyhow!(
                "Your device didn't add this computer: {}.",
                error.message.trim_end_matches('.')
            )),
            Some(Ok(_)) | None => Err(anyhow!(tilo_api::daemon::enter::UNREACHED)),
            Some(Err(error)) => Err(anyhow!(certify_refused(&error))),
        }
    }
}

fn refused(reason: impl Into<String>) -> ActionCallError {
    ActionCallError::Refused {
        reason: reason.into(),
    }
}

fn succeeded(invocation: InvocationId, value: Value) -> InvocationUpdate {
    InvocationUpdate::Succeeded {
        invocation,
        output: ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        },
    }
}

fn failed(invocation: InvocationId, message: &str) -> InvocationUpdate {
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

/// What a certify call the issuing device did not take says.
fn certify_refused(error: &ActionCallError) -> String {
    match error {
        ActionCallError::Refused { reason } => {
            format!(
                "Your device didn't take this ticket: {}.",
                reason.trim_end_matches('.')
            )
        }
        ActionCallError::UnknownAction => tilo_api::daemon::enter::TOO_OLD.to_owned(),
        _ => "The device that made this ticket can't be reached from here: it may be asleep, or \
              its `tilo device add` ended. Ask for a new ticket."
            .to_owned(),
    }
}

//! The person's devices on this desktop: a ticket for a new computer (Settings' "Add a device",
//! `tilo machine add`), and the list Settings' devices page shows, names and revokes from,
//! answered through the one ticket server every device of the person's runs
//! ([`tilo_workspace_service::device_tickets`]).
//!
//! The desktop supplies what is its own: the endpoint and device action service its network
//! binds, its identity, the person's root from its fleet store, and the workspace service that
//! holds the person's Servers workspace, where their devices' record and a planned machine's plan
//! and ticket are kept.
//!
//! The issue answers at once and the certification happens over the network, so a caller never
//! waits on the new computer.

use std::sync::{Arc, OnceLock};

use futures_util::future::BoxFuture;
use tilo_actions::remote::admitted::{offered, settled, top_level};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Admitted, Caller, CataloguePage, CatalogueQuery,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Outcome, Pending,
    device_tickets as wire,
};
use tilo_actions::{
    ActionContract, ActionDescriptor, ActionKind, ActionOutput, ExecutionLocus, InvocationId,
    OperationKind, ResultSpec,
};
use tilo_device_identity::login::{Entry, Reached};
use tilo_peer_state::actions::ActionService;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::device_tickets::{Issuer, Role, Root, TicketServer};
use tilo_workspace_service::service::ServiceSlot;
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

use crate::device_actions::DeviceActions;

/// The application whose window adds a device here.
const APP_ID: &str = "so.tilo.settings";

/// This device's `tilo.device` provider: a ticket for a new computer, served on the endpoint and
/// action service the session already runs.
#[derive(Clone)]
pub(crate) struct DeviceTickets {
    tickets: TicketServer,
    service: ServiceSlot,
}

impl DeviceTickets {
    pub(crate) fn new(
        endpoint: Option<Arc<OnceLock<iroh::Endpoint>>>,
        actions: Option<Arc<OnceLock<DeviceActions>>>,
        service: ServiceSlot,
        fabric: Option<crate::platform::fabric::SharedLink>,
    ) -> Self {
        Self {
            tickets: TicketServer::new(Arc::new(Desktop {
                endpoint,
                actions,
                service: service.clone(),
                fabric,
            })),
            service,
        }
    }

    /// Serve a ticket for a computer to join this device's, as the one string the person hands
    /// on: what Settings' "Add a device" sheet shows where no web host keeps the workspace.
    pub(crate) async fn issue(&self) -> Result<String, String> {
        self.tickets.issue(None).await.map(|ticket| ticket.encode())
    }

    /// Serve again the planned machines' tickets this device issued before it last stopped,
    /// once its workspace service, and with it the person's Servers workspace, is open.
    pub(crate) fn restore_once_open(&self) {
        let tickets = self.tickets.clone();
        let service = self.service.clone();
        tilo_rt::app().handle().spawn(async move {
            service.installed().await;
            if let Err(error) = tickets.restore().await {
                tracing::warn!(%error, "the planned machines' tickets were not served again");
            }
        });
    }
}

/// What the desktop supplies its ticket server.
struct Desktop {
    /// Where this device answers, once the network bound it: where a new computer dials.
    endpoint: Option<Arc<OnceLock<iroh::Endpoint>>>,
    /// This device's action service, which answers the ticket's certify call: `None` on a session
    /// that takes part as nobody.
    actions: Option<Arc<OnceLock<DeviceActions>>>,
    /// The workspace service this device holds its workspaces in.
    service: ServiceSlot,
    /// The person's devices as this desktop's session hears them live: which are present now.
    fabric: Option<crate::platform::fabric::SharedLink>,
}

impl Desktop {
    fn bound(&self) -> Result<iroh::Endpoint, String> {
        self.endpoint
            .as_ref()
            .and_then(|endpoint| endpoint.get().cloned())
            .ok_or_else(|| {
                "This computer's network is not up yet: try again in a moment.".to_owned()
            })
    }

    /// Where the certified computer enters: the workspace this computer was logged into, or the
    /// one it founded itself, through this endpoint.
    async fn entered(&self) -> Result<Entry, String> {
        if let Some(device) = tilo_api::daemon::who::home().ok().flatten() {
            return tilo_api::daemon::enter::login_entry(&device);
        }
        // No login: this computer founded its own workspace, and the computer it adds enters it
        // through this endpoint and the certificate its workspace service proves.
        let endpoint = self.bound()?;
        let service = self.service.service().ok_or_else(|| {
            "This computer holds no workspace another computer can enter.".to_owned()
        })?;
        let workspaces = service.workspaces();
        let listed = workspaces.list().await;
        let first = listed.first().ok_or_else(|| {
            "This computer holds no workspace another computer can enter.".to_owned()
        })?;
        Ok(tilo_api::daemon::enter::founded_entry(
            &endpoint,
            (first.workspace.clone(), first.name.clone()),
            workspaces.serving_certificate().clone(),
        ))
    }
}

impl Issuer for Desktop {
    fn serving(&self) -> Result<(Reached, ActionService), String> {
        let endpoint = self.bound()?;
        let actions = self
            .actions
            .as_ref()
            .and_then(|actions| actions.get())
            .ok_or_else(|| {
                "This computer is not taking part as anyone yet, so it cannot add a device."
                    .to_owned()
            })?;
        Ok((
            tilo_api::daemon::enter::reached_of(&endpoint.addr()),
            actions.action_service(),
        ))
    }

    fn device(&self) -> Result<(DeviceCert, Arc<dyn collab_types::signer::DeviceSigner>), String> {
        let identity = crate::sync::device_identity().ok_or_else(|| {
            "This computer has no identity yet, so it cannot add a device: set this computer up \
             first."
                .to_owned()
        })?;
        Ok((identity.device_cert, identity.signer))
    }

    fn root(&self) -> Result<Option<Root>, String> {
        let (cert, _) = self.device()?;
        let Ok(key) =
            crate::identity_admin::fleet_store()?.identity_key(&hex::encode(cert.user_public_key))
        else {
            return Ok(None);
        };
        Ok(Some(Arc::new(key)))
    }

    fn entry(&self) -> BoxFuture<'_, Result<Entry, String>> {
        Box::pin(self.entered())
    }

    fn servers(&self) -> Option<Workspaces> {
        self.service
            .service()
            .map(|service| service.workspaces().clone())
    }

    /// The keys this desktop's root certified here, which the record of the person's devices
    /// takes in as the desktop holds it.
    fn known(&self) -> Vec<DeviceCert> {
        self.device()
            .map(|(cert, _)| crate::identity_admin::root_certificates(&cert.user_public_key))
            .unwrap_or_default()
    }

    /// The person's devices on a live session with this desktop now, as its device fabric hears
    /// them.
    fn present(&self) -> Vec<[u8; 32]> {
        self.fabric
            .as_ref()
            .map(|fabric| {
                crate::platform::fabric::FabricLink::online_device_ids(fabric)
                    .iter()
                    .filter_map(|device| {
                        let mut key = [0; 32];
                        hex::decode_to_slice(device.as_str(), &mut key).ok()?;
                        Some(key)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The fleet store keeps what this desktop's root revoked, which refuses the key here from
    /// then on.
    fn keep_revocation(&self, entry: &tilo_user_identity::RevocationEntry) {
        let kept = crate::identity_admin::fleet_store().and_then(|fleets| {
            fleets
                .record_revocation(&hex::encode(entry.user_public_key), entry)
                .map_err(|error| error.to_string())
        });
        if let Err(error) = kept {
            tracing::warn!(%error, "this desktop does not keep the revocation it signed");
        }
    }
}

/// One of the device actions, as the catalogue lists it.
fn descriptor(action: &str) -> ActionDescriptor {
    use tilo_actions::{ActionParam, ConfirmationPolicy, ParamType, RiskLevel};
    let object = |required: &[&str]| ResultSpec::Json {
        schema: serde_json::json!({ "type": "object", "required": required }),
    };
    let device = ActionParam::required(
        "device",
        ParamType::Text,
        "The device's key, 64 hexadecimal characters, as the list of devices answers it.",
    );
    let (label, description, kind, contract, params) = match action {
        wire::LIST => (
            "Your devices",
            "List your devices: which is this one, which are online, and which you took back.",
            OperationKind::View,
            ActionContract {
                result: object(&["devices"]),
                ..ActionContract::view()
            },
            Vec::new(),
        ),
        wire::RENAME => (
            "Rename a device",
            "Name one of your devices in every list of them.",
            OperationKind::Effect,
            ActionContract {
                kind: ActionKind::Outside,
                result: object(&["device", "name"]),
                ..ActionContract::default()
            },
            vec![
                device,
                ActionParam::required("name", ParamType::Text, "What the device is called."),
            ],
        ),
        wire::REVOKE => (
            "Revoke a device",
            "Take one of your devices back for good: every device of yours refuses it from now on.",
            OperationKind::Effect,
            ActionContract {
                kind: ActionKind::Outside,
                risk: RiskLevel::Privileged,
                confirmation: ConfirmationPolicy::Confirm,
                result: object(&["device", "revoked"]),
                ..ActionContract::default()
            },
            vec![
                device,
                ActionParam::optional(
                    "reason",
                    ParamType::Text,
                    "lost, stolen, retired or compromised (retired when none is given).",
                ),
            ],
        ),
        _ => (
            "Add a device",
            "Print a one-use code for a new computer to join as one of this person's devices.",
            OperationKind::Effect,
            ActionContract {
                kind: ActionKind::Outside,
                result: object(&["ticket"]),
                ..ActionContract::default()
            },
            vec![ActionParam::optional(
                "plan",
                ParamType::Text,
                "The planned machine's entry in your Servers workspace, which the computer's \
                 certification takes into its own.",
            )],
        ),
    };
    ActionDescriptor {
        did: None,
        semantic_identity: None,
        address: wire::target(action).address,
        resource_kind: wire::RESOURCE.to_owned(),
        resource_label: "Devices".to_owned(),
        action_label: label.to_owned(),
        keywords: Vec::new(),
        state: tilo_actions::ActionState::default(),
        icon: None,
        description: Some(description.to_owned()),
        locus: ExecutionLocus::ClientLocal,
        kind,
        contract,
        input: tilo_actions::InputSpec::None,
        params,
        view: None,
    }
}

/// The device actions this desktop answers.
const ACTIONS: [&str; 4] = [wire::ISSUE, wire::LIST, wire::RENAME, wire::REVOKE];

/// What a refused or failed device action answers.
fn refused(message: impl Into<String>) -> tilo_actions::ActionFailure {
    tilo_actions::ActionFailure {
        code: "device-ticket-refused".to_owned(),
        message: message.into(),
        details: None,
        retryable: false,
    }
}

impl DeviceTickets {
    /// Answer the person's devices from now on (a ticket for a new computer, the list, names and
    /// revocations), admitted by the grants of Settings' app, for the person at this device alone.
    pub(crate) fn register(self: Arc<Self>, providers: &Providers) {
        providers.provide(wire::PROVIDER, APP_ID, Reach::ThisDevice, self);
    }

    fn descriptors() -> Vec<ActionDescriptor> {
        ACTIONS.into_iter().map(descriptor).collect()
    }

    /// Carry out an invocation the device's dispatch admitted for `caller`.
    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let action = ACTIONS
            .into_iter()
            .find(|action| request.target.address.action == *action)
            .ok_or("no such device action")?;
        let args = match &request.argument {
            InvokeArgument::None => serde_json::Value::Object(serde_json::Map::new()),
            InvokeArgument::Json(args) => serde_json::Value::Object(args.clone()),
            InvokeArgument::Text(_) => {
                return Err("a device action is asked with its arguments as an object".to_owned());
            }
        };
        // What names and revokes the person's devices is theirs alone to ask: an agent or an app
        // acting for nobody never does it through here.
        if action == wire::RENAME || action == wire::REVOKE {
            tilo_broker_core::BrokerActor::for_caller(caller)
                .direct_person()
                .ok_or("only you name or revoke your devices")?;
        }
        // A step asked from the session's own thread runs on the app's runtime, as the restore
        // of tickets does; one asked from a runtime's task runs on that runtime.
        let runtime = tokio::runtime::Handle::try_current()
            .unwrap_or_else(|_| tilo_rt::app().handle().clone());
        let (done, asked) = tilo_actions::remote::pending();
        let tickets = self.tickets.clone();
        runtime.spawn(async move {
            let answer = match action {
                wire::LIST => tickets.list().await,
                wire::RENAME => tickets.rename(&args).await,
                wire::REVOKE => tickets.revoke(&args).await,
                // A planned machine's ticket lasts as long as its medium may sit on a shelf, and
                // is served beside the ticket printed last; the machine takes the plan's entry.
                _ => {
                    let plan = args
                        .get("plan")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|plan| !plan.is_empty())
                        .map(|plan| Role::Plan(plan.to_owned()));
                    tickets
                        .issue(plan)
                        .await
                        .map(|ticket| serde_json::json!({ "ticket": ticket.encode() }))
                }
            };
            done.complete(
                answer
                    .map(|value| ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    })
                    .map_err(|message| Box::new(refused(message))),
            );
        });
        Ok(asked)
    }
}

impl ActionProvider for DeviceTickets {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        offered(wire::PROVIDER, APP_ID, top_level(Self::descriptors()), &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.run(caller, request, Admitted::default())
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        settled(request.invocation, self.start(caller, &request, &admitted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A computer with no identity cannot print a code, and says what to do about it.
    #[test]
    fn a_computer_with_no_identity_prints_no_code_and_says_why() {
        let root = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_IDENTITY_ROOT", root.path());
        let tickets = DeviceTickets::new(
            None,
            None,
            tilo_workspace_service::service::ServiceSlot::default(),
            None,
        );
        // The ticket is issued on a worker, as the broker's own runtime does it: the test
        // thread only waits for the answer.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let caller = Caller::local_person(hex::encode([7; 32]))
            .in_workspace("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0");
        // The plan arrives as the dispatch hands a provider its arguments: typed.
        let mut plan = serde_json::Map::new();
        plan.insert("plan".to_owned(), "6498fd08fe34f46d4165a5d9f3fe251c".into());
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            on_behalf_of: None,
            target: wire::target(wire::ISSUE),
            argument: InvokeArgument::Json(plan),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let descriptors = DeviceTickets::descriptors();
        assert_eq!(descriptors[0].address, wire::target(wire::ISSUE).address);
        assert_eq!(descriptors.len(), ACTIONS.len());
        let asked = tickets
            .start(&caller, &request, &Admitted::default())
            .unwrap();
        // The issue runs on the runtime's worker, which holds what it needs.
        drop(tickets);
        let answer = asked
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("the issue answers at once");
        let Err(failure) = answer else {
            panic!("a computer with no identity printed a code");
        };
        assert!(
            failure.message.contains("has no identity yet"),
            "{}",
            failure.message
        );
    }
}

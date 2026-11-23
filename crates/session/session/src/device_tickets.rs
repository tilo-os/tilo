//! Serving a ticket for a new computer: what Settings' "Add a device" and `tilo machine add` ask
//! for, answered by the daemon this desktop is, through the one ticket server every device of
//! the person's runs ([`tilo_workspace_service::device_tickets`]).
//!
//! The desktop supplies what is its own: the endpoint and device action service its network
//! binds, its identity, the person's root from its fleet store, and the workspace service that
//! holds the person's Servers workspace, where a planned machine's plan and ticket are kept.
//!
//! The issue answers at once and the certification happens over the network, so a caller never
//! waits on the new computer.

use std::sync::{Arc, OnceLock};

use futures_util::future::BoxFuture;
use tilo_actions::remote::{Pending, device_tickets as wire};
use tilo_actions::{
    ActionContract, ActionDescriptor, ActionKind, ActionOutput, ExecutionLocus, OperationKind,
    ResultSpec,
};
use tilo_broker_core::steps::{HostStepOutcome, HostStepRequest, HostStepServices};
use tilo_device_identity::login::{Entry, Reached};
use tilo_peer_state::actions::ActionService;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::device_tickets::{Issuer, Role, Root, TicketServer};
use tilo_workspace_service::service::ServiceSlot;
use tilo_workspace_service::workspaces::Workspaces;

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
    ) -> Self {
        Self {
            tickets: TicketServer::new(Arc::new(Desktop {
                endpoint,
                actions,
                service: service.clone(),
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
}

/// The step's one action, as the catalogue lists it.
fn descriptor() -> ActionDescriptor {
    ActionDescriptor {
        did: None,
        semantic_identity: None,
        address: wire::target(wire::ISSUE).address,
        resource_kind: wire::RESOURCE.to_owned(),
        resource_label: "Devices".to_owned(),
        action_label: "Add a device".to_owned(),
        keywords: Vec::new(),
        state: tilo_actions::ActionState::default(),
        icon: None,
        description: Some(
            "Print a one-use code for a new computer to join as one of this person's devices."
                .to_owned(),
        ),
        locus: ExecutionLocus::ClientLocal,
        kind: OperationKind::Effect,
        contract: ActionContract {
            kind: ActionKind::Outside,
            result: ResultSpec::Json {
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["ticket"],
                    "properties": { "ticket": { "type": "string" } }
                }),
            },
            ..ActionContract::default()
        },
        input: tilo_actions::InputSpec::None,
        params: vec![tilo_actions::ActionParam::optional(
            "plan",
            tilo_actions::ParamType::Text,
            "The planned machine's entry in your Servers workspace, which the computer's \
             certification takes into its own.",
        )],
        view: None,
    }
}

/// What a refused or failed issue answers.
fn refused(message: impl Into<String>) -> tilo_actions::ActionFailure {
    tilo_actions::ActionFailure {
        code: "device-ticket-refused".to_owned(),
        message: message.into(),
        details: None,
        retryable: false,
    }
}

impl HostStepServices for DeviceTickets {
    fn provider(&self) -> &'static str {
        wire::PROVIDER
    }

    fn app_id(&self) -> &str {
        APP_ID
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        vec![descriptor()]
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        if request.address.action != wire::ISSUE {
            return Err("no such device action".to_owned());
        }
        let plan = request
            .argument
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()
            .map_err(|_| "a ticket is asked for with its arguments as an object".to_owned())?
            .and_then(|args| {
                args.get("plan")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .map(str::to_owned)
            })
            .filter(|plan| !plan.is_empty());
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "this computer cannot serve a ticket right now".to_owned())?;
        let (done, asked) = tilo_actions::remote::pending();
        let tickets = self.tickets.clone();
        runtime.spawn(async move {
            // A planned machine's ticket lasts as long as its medium may sit on a shelf, and is
            // served beside the ticket printed last; the machine takes the plan's entry.
            let answer = tickets
                .issue(plan.map(Role::Plan))
                .await
                .map(|ticket| ActionOutput {
                    value: Some(serde_json::json!({ "ticket": ticket.encode() })),
                    resources: Vec::new(),
                });
            done.complete(answer.map_err(|message| Box::new(refused(message))));
        });
        Ok(asked)
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
        );
        // The ticket is issued on a worker, as the broker's own runtime does it: the test
        // thread only waits for the answer.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let actor = tilo_broker_core::actor::BrokerActor::person(hex::encode([7; 32]));
        let request = HostStepRequest {
            workspace: "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0",
            actor: &actor,
            address: &wire::target(wire::ISSUE).address,
            argument: None,
            provider: None,
            group: None,
            stop: std::sync::Arc::default(),
        };
        assert_eq!(
            tickets.descriptors()[0].address,
            wire::target(wire::ISSUE).address
        );
        let asked = tickets.start(&request).unwrap();
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

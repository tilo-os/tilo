//! Serving a ticket for a new computer: what Settings' "Add a device" asks for, answered by the
//! daemon this desktop is.
//!
//! A ticket is one string — where this device is reached, and a one-use secret. The new computer
//! dials this device over the device action service and calls the ticket's certify action there
//! ([`tilo_device_identity::login::CERTIFY`]); this device admits that call alone, and the
//! person's root, held here, signs the computer's certificate. The person asked for the ticket
//! themselves, so the computer that comes with it is approved as it arrives.
//!
//! A ticket for a planned machine (`tilo machine add`) names the plan: it lasts as long as the
//! medium carrying it may sit on a shelf, it is served beside the ticket printed last, and the
//! machine it certifies takes the plan's entry in the person's Servers workspace, which this
//! device holds.
//!
//! The issue answers at once and the certification happens over the network, so a caller never
//! waits on the new computer.

use std::sync::{Arc, Mutex, OnceLock};

use tilo_actions::remote::{Pending, device_tickets as wire};
use tilo_actions::{
    ActionContract, ActionDescriptor, ActionKind, ActionOutput, ExecutionLocus, OperationKind,
    ResultSpec,
};
use tilo_broker_core::steps::{HostStepOutcome, HostStepRequest, HostStepServices};
use tilo_device_identity::login;
use tilo_device_identity::login::{
    Approval, Approved, Approver, Asked, Declined, Entry, SCOPE, Ticket,
};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::new_device::{ServedTicket, ServedTickets};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, StatementSigner as _};
use tilo_workspace_service::service::WorkspaceService;
use tilo_workspace_service::workspaces::servers;

use crate::device_actions::DeviceActions;

/// The application whose window adds a device here.
const APP_ID: &str = "so.tilo.settings";

/// The site a computer that enters this device's own workspace is shown.
const FOUNDED_SITE: &str = "https://tilo.sh/";

/// This device's `tilo.device` provider: a ticket for a new computer, served on the endpoint and
/// action service the session already runs.
#[derive(Clone)]
pub(crate) struct DeviceTickets {
    /// Where this device answers, once the network bound it: where a new computer dials.
    endpoint: Option<Arc<OnceLock<iroh::Endpoint>>>,
    /// This device's action service, which answers the ticket's certify call: `None` on a session
    /// that takes part as nobody.
    actions: Option<Arc<OnceLock<DeviceActions>>>,
    /// The workspace service this device holds its workspaces in: what a computer entering with
    /// this ticket reaches when there is no login naming another.
    service: tilo_workspace_service::service::ServiceSlot,
    /// Every ticket this device serves now: planned machines' beside the one printed last.
    served: ServedTickets,
    /// The ticket printed last for a computer at hand, which a newer one replaces.
    printed: Arc<Mutex<Option<ServedTicket>>>,
}

impl DeviceTickets {
    pub(crate) fn new(
        endpoint: Option<Arc<OnceLock<iroh::Endpoint>>>,
        actions: Option<Arc<OnceLock<DeviceActions>>>,
        service: tilo_workspace_service::service::ServiceSlot,
    ) -> Self {
        Self {
            endpoint,
            actions,
            service,
            served: ServedTickets::default(),
            printed: Arc::default(),
        }
    }

    /// Serve a ticket for a computer to join this device's, as the one string the person hands
    /// on; with `plan`, for the machine that plan describes.
    async fn issue(&self, plan: Option<String>) -> Result<String, String> {
        let identity = crate::sync::device_identity().ok_or_else(|| {
            "This computer has no identity yet, so it cannot add a device: set this computer up \
             first."
                .to_owned()
        })?;
        let cert = identity.device_cert.clone();
        let endpoint = self
            .endpoint
            .as_ref()
            .and_then(|endpoint| endpoint.get().cloned())
            .ok_or_else(|| {
                "This computer's network is not up yet: try again in a moment.".to_owned()
            })?;
        let actions = self
            .actions
            .as_ref()
            .and_then(|actions| actions.get())
            .ok_or_else(|| {
                "This computer is not taking part as anyone yet, so it cannot add a device."
                    .to_owned()
            })?;
        let root = root_of(cert.user_public_key)?;
        let admission = match plan {
            Some(plan) => {
                let service = self.service.service().ok_or_else(|| {
                    "This computer holds no Servers workspace, so it cannot add a planned machine."
                        .to_owned()
                })?;
                servers::read(service.workspaces())
                    .await?
                    .waiting_plan(&plan)?;
                Some(Planned { plan, service })
            }
            None => None,
        };
        let planned = admission.is_some();
        let entry = self.entry(&endpoint).await?;
        let ticket = Ticket::new(tilo_api::daemon::enter::reached_of(&endpoint.addr()));
        let served = ServedTicket::new(
            ticket.clone(),
            cert.user_public_key,
            entry,
            DeviceCapabilities::member(),
            Arc::new(DeviceApprover {
                root,
                device: Arc::clone(&identity.signer),
                cert,
                admission,
            }),
            RevocationList::new(),
            // A planned machine's medium may sit on a shelf before the machine installs from it.
            if planned {
                login::PLANNED_TICKET_LIFETIME_NS
            } else {
                login::TICKET_LIFETIME_NS
            },
        );
        // The one call this device answers without a certificate is a served ticket's certify. A
        // newer ticket printed for a computer at hand takes the place of the one printed before
        // it, which stops admitting a computer; a planned machine's stays served beside it.
        if !planned && let Some(replaced) = self.printed.lock_recover().replace(served.clone()) {
            self.served.withdraw(&replaced);
        }
        self.served.serve(served);
        actions
            .action_service()
            .admit_uncertified(SCOPE, Arc::new(self.served.clone()));
        Ok(ticket.encode())
    }

    /// Where the certified computer enters: the workspace this computer was logged into, or the
    /// one it founded itself, through this endpoint.
    async fn entry(&self, endpoint: &iroh::Endpoint) -> Result<Entry, String> {
        let place = login::Place::here(tilo_api::client::device_keys())
            .map_err(|error| error.to_string())?;
        if let Some((_, device)) = login::load(&place).map_err(|error| error.to_string())? {
            let published = device
                .server_address
                .as_ref()
                .ok_or_else(|| tilo_api::daemon::enter::LOGIN_ENDED.to_owned())?;
            let mut server = tilo_api::daemon::enter::reached_of(&published.endpoint_addr);
            if server.relay.is_none() {
                server.relay.clone_from(&published.relay_url);
            }
            return Ok(Entry {
                workspace: device.workspace,
                workspace_name: device.workspace_name,
                server_cert: device.server_cert,
                server,
                site: device.site,
                host_origin: device.host_origin,
            });
        }
        // No login: this computer founded its own workspace, and the computer it adds enters it
        // through this endpoint and the certificate its workspace service proves.
        let service = self.service.service().ok_or_else(|| {
            "This computer holds no workspace another computer can enter.".to_owned()
        })?;
        let workspaces = service.workspaces();
        let listed = workspaces.list().await;
        let first = listed.first().ok_or_else(|| {
            "This computer holds no workspace another computer can enter.".to_owned()
        })?;
        Ok(Entry {
            workspace: first.workspace.clone(),
            workspace_name: Some(first.name.clone()),
            server_cert: workspaces.serving_certificate().clone(),
            server: tilo_api::daemon::enter::reached_of(&endpoint.addr()),
            site: FOUNDED_SITE.to_owned(),
            host_origin: format!("{}{}", tilo_api::daemon::enter::IROH_ONLY, endpoint.id()),
        })
    }
}

/// The person's root, held here, as the signer of a new computer's certificate.
fn root_of(person: [u8; 32]) -> Result<Arc<tilo_enrollment::root::PersonSigner>, String> {
    let key = crate::identity_admin::fleet_store()?
        .identity_key(&hex::encode(person))
        .map_err(|_| {
            "This computer does not hold your identity, so it cannot add a device: add one from \
             a computer that does."
                .to_owned()
        })?;
    let root = Arc::new(tilo_enrollment::root::PersonSigner::new(key));
    if root.public_key() != person {
        return Err(
            "This computer is logged in as another person than the one whose identity it holds."
                .to_owned(),
        );
    }
    Ok(root)
}

/// The plan a ticket was served for, and the service holding the person's Servers workspace
/// the machine's entry is taken in through.
struct Planned {
    plan: String,
    service: Arc<WorkspaceService>,
}

/// The person's root approving a computer that came with a ticket: it signs the computer's
/// certificate, and the approval with this device's own key, which the root certified. A planned
/// machine is approved once the plan's entry is its own.
struct DeviceApprover {
    root: Arc<tilo_enrollment::root::PersonSigner>,
    device: Arc<dyn tilo_server_client::DeviceSigner>,
    cert: DeviceCert,
    admission: Option<Planned>,
}

impl Approver for DeviceApprover {
    fn approve(&self, asked: Asked) -> Approval {
        let mut cert = asked.cert.clone();
        let signed = tilo_user_identity::sign_device_cert(&mut cert, self.root.as_ref());
        let device = Arc::clone(&self.device);
        let approver = self.cert.clone();
        let admission = self
            .admission
            .as_ref()
            .map(|planned| (planned.plan.clone(), Arc::clone(&planned.service)));
        Box::pin(async move {
            signed.map_err(|error| {
                Declined(format!("this computer's root could not sign: {error}"))
            })?;
            // A planned machine that did not take its plan's entry would boot as nobody's
            // server: it is told so, and its install stops with that reason.
            if let Some((plan, service)) = admission {
                servers::admit_planned(service.workspaces(), &plan, &cert.device_public_key)
                    .await
                    .map_err(|error| {
                        Declined(format!(
                            "the plan's entry did not take this machine: {error}"
                        ))
                    })?;
            }
            let signature = device
                .sign(asked.digest.to_vec())
                .await
                .map_err(|error| Declined(format!("this computer could not sign: {error}")))?;
            Ok(Approved {
                cert,
                approver,
                signature,
            })
        })
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
        let tickets = self.clone();
        runtime.spawn(async move {
            let answer = tickets.issue(plan).await.map(|ticket| ActionOutput {
                value: Some(serde_json::json!({ "ticket": ticket })),
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

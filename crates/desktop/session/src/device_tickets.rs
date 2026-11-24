//! The person's devices on this desktop: a ticket for a new computer (Settings' "Add a device",
//! `tilo machine add`), the names and revocations Settings' devices page asks for, and the
//! identities this device keeps (listing, using, removing and protecting one), answered by this
//! computer's daemon, which holds the person's root and identity store and serves every device
//! action. The desktop only hands the call on, so no app reads a secret or signs with the root.

use std::sync::Arc;

use tilo_actions::remote::Admits;
use tilo_actions::remote::admitted::top_level;
use tilo_actions::remote::{
    Admitted, Caller, InvokeArgument, InvokeRequest, Outcome, Pending, device_tickets as wire,
};
use tilo_actions::{
    ActionContract, ActionDescriptor, ActionKind, ActionOutput, ExecutionLocus, OperationKind,
    ResultSpec,
};
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

/// The application whose window adds a device here.
const APP_ID: &str = "so.tilo.settings";

/// Settings' page of the person's devices, whose "Add a device" prints the ticket a new device
/// joins with.
const ADD_DEVICE_PAGE: &str = "settings://devices_sync";

impl crate::Shared {
    /// Add a device, wherever it was asked for (the person's own card, Settings' Devices and
    /// access, an app's own offer): Settings at its devices page, where the ticket is printed.
    pub(crate) fn open_add_device(&self) {
        crate::quick_settings::open_settings(self, ADD_DEVICE_PAGE);
    }
}

/// This device's `tilo.device` provider for Settings, handing each call to the daemon.
#[derive(Clone, Default)]
pub(crate) struct DeviceTickets;

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
    // A change to one of the identities this device keeps, which the person confirms.
    let change = |label: &'static str, description: &'static str| {
        (
            label,
            description,
            OperationKind::Effect,
            ActionContract {
                kind: ActionKind::Outside,
                risk: RiskLevel::Privileged,
                confirmation: ConfirmationPolicy::Confirm,
                result: object(&["id"]),
                ..ActionContract::default()
            },
            vec![
                ActionParam::required("id", ParamType::Text, "The identity's id, as listed."),
                ActionParam::required("confirm", ParamType::Boolean, "You confirm the change."),
            ],
        )
    };
    let (label, description, kind, contract, params) = match action {
        wire::IDENTITIES => (
            "List your identities",
            "Show the identities on this device and which one new apps sign in as.",
            OperationKind::View,
            ActionContract {
                result: ResultSpec::Json {
                    schema: serde_json::json!({ "type": "array", "items": { "type": "object" } }),
                },
                ..ActionContract::view()
            },
            Vec::new(),
        ),
        wire::USE_IDENTITY => change(
            "Use an identity for new apps",
            "Sign apps you open from now on in as one of your identities.",
        ),
        wire::REMOVE_IDENTITY => change(
            "Remove an identity from this device",
            "Delete an identity you no longer use here, with its key.",
        ),
        wire::PROTECT_IDENTITY => change(
            "Protect an identity's key",
            "Move an identity's key from a plain file into protected storage.",
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
const ACTIONS: [&str; 7] = [
    wire::ISSUE,
    wire::RENAME,
    wire::REVOKE,
    wire::IDENTITIES,
    wire::USE_IDENTITY,
    wire::REMOVE_IDENTITY,
    wire::PROTECT_IDENTITY,
];

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
    /// Answer the person's devices from now on (a ticket for a new computer, names and
    /// revocations), admitted by the grants of Settings' app, for the person at this device alone.
    pub(crate) fn register(self: Arc<Self>, providers: &Providers) {
        providers.provide(wire::PROVIDER, APP_ID, Reach::ThisDevice, self);
    }

    fn descriptors() -> Vec<ActionDescriptor> {
        ACTIONS.into_iter().map(descriptor).collect()
    }
}

impl Admits for DeviceTickets {
    const PROVIDER: &'static str = wire::PROVIDER;
    const APP: &'static str = APP_ID;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        top_level(Self::descriptors())
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
        // What names and revokes the person's devices, and their identities, is theirs alone to
        // ask: an agent or an app acting for nobody never does it through here.
        if action != wire::ISSUE {
            tilo_broker_core::BrokerActor::for_caller(caller)
                .direct_person()
                .ok_or("only you administer your devices and identities")?;
        }
        // The daemon answers over its socket, which blocks: the call waits on a thread of its
        // own, not on the runtime that asked it.
        let (done, asked) = tilo_actions::remote::pending();
        std::thread::spawn(move || {
            let answer =
                tilo_api::daemon::this_computer().and_then(|daemon| daemon.act(action, args));
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

//! The person's devices on this desktop: a ticket for a new computer (Settings' "Add a device",
//! `tilo machine add`), the names and revocations Settings' devices page asks for, and the
//! identities this device keeps (listing, using, removing and protecting one), answered by this
//! computer's daemon, which holds the person's root and identity store and serves every device
//! action. The desktop only hands the call on, so no app reads a secret or signs with the root.

use tilo_actions::remote::Admits;
use tilo_actions::remote::admitted::top_level;
use tilo_actions::remote::{
    Admitted, Caller, InvokeArgument, InvokeRequest, Outcome, Pending, device_tickets as wire,
};
use tilo_actions::{ActionDescriptor, ActionOutput};

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
    fn descriptors() -> Vec<ActionDescriptor> {
        wire::HANDED_ON
            .into_iter()
            .filter_map(wire::descriptor)
            .collect()
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
        let action = wire::HANDED_ON
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
            // Followed until it settles: a revoke on a device without the person's root waits for
            // their answer at one that keeps it.
            let answer = tilo_api::daemon::this_computer()
                .and_then(|daemon| daemon.act_followed(action, args));
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

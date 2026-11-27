//! What the person's shell tells the apps on its device about their windows, carried out by the
//! device's daemon, which holds the apps' connections.
//!
//! Two actions under [`PROVIDER`]: [`FOLLOW`] tells every app of a workspace whose view the person
//! follows now, and [`HANDOFF`] asks one app instance for the next step of moving its window to
//! another device. Only the person's own shell calls them, on this device: an app or an agent that
//! names them is refused.

use serde::{Deserialize, Serialize};

use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_actions::{InvocationId, MemberId};

/// The application whose actions these are: the person's shell.
pub use tilo_app_id::apps::SHELL as APP;
/// The provider the device's daemon carries them out under.
pub const PROVIDER: &str = "tilo.window-follow";
/// The resource both are actions of.
pub const RESOURCE: &str = "shell.windows";
/// Tell a workspace's apps whose view the person follows ([`Follow`]).
pub const FOLLOW: &str = "follow";
/// Ask one app instance for the next step of moving its window ([`Handoff`]).
pub const HANDOFF: &str = "hand-off";

/// What [`FOLLOW`] is asked: the member whose view the person follows, or none to stop.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Follow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<MemberId>,
}

/// What [`HANDOFF`] is asked: the step `request` of the window of `app`'s instance `instance`.
/// `R` is the handoff step as the apps read it (`collab_types::handoff::HandoffRequest`), which
/// this crate cannot name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff<R> {
    pub app: String,
    pub instance: u64,
    pub request: R,
}

/// The call of `action` with `argument`, as the shell makes it.
#[must_use]
pub fn request(action: &str, argument: &impl Serialize) -> InvokeRequest {
    InvokeRequest {
        asks: false,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: ActionAddress::new(RESOURCE, action),
        },
        argument: match serde_json::to_value(argument) {
            Ok(serde_json::Value::Object(fields)) => InvokeArgument::Json(fields),
            _ => InvokeArgument::None,
        },
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

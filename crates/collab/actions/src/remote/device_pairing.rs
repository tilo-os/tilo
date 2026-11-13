use super::{ActionAddress, RemoteActionTarget};

/// The provider the host answers pairing under.
pub const PROVIDER: &str = "tilo.device-pairing";
/// The resource the steps are actions of.
pub const RESOURCE: &str = "identity.device.pair";
/// Look for a nearby device (`role`); answers the device found.
pub const DISCOVER: &str = "discover";
/// Admit a discovered device (`role`, `device`, `service`, `confirm`).
pub const APPROVE: &str = "approve";
/// Forget a discovered device and stop approving it (`device`).
pub const REJECT: &str = "reject";
/// The devices another of the person's devices found, asking here to be
/// the person; answers each with the `ask` that names it.
pub const ASKING: &str = "asking";
/// Approve or decline one of them (`ask`, `approve`, `confirm`), as its
/// notification does.
pub const ANSWER: &str = "answer";

#[must_use]
pub fn target(step: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, step),
    }
}

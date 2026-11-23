//! What the fabric's services reach outside themselves through, each handed in by whoever mounts
//! them: the person's other devices ([`Invoker`], action calls on the one device link), the desktop
//! session on this device when one is attached ([`SeatCalls`]), and the person's store, where what
//! lasts of each device is kept ([`Facts`]).

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tilo_actions::ActionAddress;
use tilo_actions::remote::{ActionCallError, Pending, RemoteActionTarget};

use crate::DeviceId;

/// What a device without a session attached answers an action that needs one.
pub const CANT_NOW: &str = "This device can't do that now";

/// The prefix every action on another of the person's devices is addressed under:
/// `devices/<id>/<resource>`.
pub const DEVICES_PREFIX: &str = "devices/";

/// One action call to another of the person's devices.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceCall {
    /// The device the call is for.
    pub device: DeviceId,
    /// The action there: its provider, and its address under `devices/<id>/`.
    pub target: RemoteActionTarget,
    /// What it is asked with.
    pub input: Value,
}

impl DeviceCall {
    /// The call of `provider`'s `resource#action` on `device`, with `input`.
    #[must_use]
    pub fn new(
        device: &DeviceId,
        provider: &str,
        resource: &str,
        action: &str,
        input: impl Serialize,
    ) -> Self {
        Self {
            device: device.clone(),
            target: device_target(device, provider, resource, action),
            input: serde_json::to_value(input).unwrap_or(Value::Null),
        }
    }
}

/// The target of `provider`'s `resource#action` on `device`.
#[must_use]
pub fn device_target(
    device: &DeviceId,
    provider: &str,
    resource: &str,
    action: &str,
) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: provider.to_owned(),
        address: ActionAddress::new(
            format!("{DEVICES_PREFIX}{}/{resource}", device.as_str()),
            action,
        ),
    }
}

/// Where this device's calls to the person's other devices go: action calls on the device link,
/// answered by the provider the call names on the device it names.
pub trait Invoker: Send + Sync {
    /// Ask `call.device` to run `call.target` with `call.input`, answered with the action's output.
    fn call(&self, call: DeviceCall) -> Pending<Result<Value, ActionCallError>>;
}

/// The desktop session on this device, as the services that need its screen, clipboard or
/// notifications reach it: the session provider's own actions. A device with no session attached
/// answers [`CANT_NOW`].
pub trait SeatCalls: Send + Sync {
    /// Run the session's `resource#action` with `input`.
    fn call(
        &self,
        resource: &str,
        action: &str,
        input: Value,
    ) -> Pending<Result<Value, ActionCallError>>;
}

/// No session on this device: every call answers [`CANT_NOW`].
#[derive(Clone, Copy, Debug, Default)]
pub struct NoSeat;

impl SeatCalls for NoSeat {
    fn call(
        &self,
        _resource: &str,
        _action: &str,
        _input: Value,
    ) -> Pending<Result<Value, ActionCallError>> {
        Pending::ready(Err(cant_now()))
    }
}

/// The refusal of an action that needs something this device has not got now.
#[must_use]
pub fn cant_now() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: CANT_NOW.to_owned(),
    }
}

/// What lasts of each of the person's devices, in the person's own store: each device writes only
/// its own facts, and reads every device's. A fact is the postcard bytes of its type.
pub trait Facts: Send + Sync {
    /// Keep `value` as this device's fact `name`.
    ///
    /// # Errors
    /// Why the store did not take it.
    fn keep(&self, name: &str, value: Vec<u8>) -> Result<(), String>;

    /// Every device's fact `name`, as the store holds them now.
    fn read(&self, name: &str) -> Vec<(DeviceId, Vec<u8>)>;
}

/// Every device's fact `name` that reads as a `T`; one that does not is no fact of this kind.
pub fn read_facts<T: DeserializeOwned>(facts: &dyn Facts, name: &str) -> Vec<(DeviceId, T)> {
    facts
        .read(name)
        .into_iter()
        .filter_map(|(device, value)| Some((device, postcard::from_bytes(&value).ok()?)))
        .collect()
}

/// Keep `value` as this device's fact `name`.
///
/// # Errors
/// Why it could not be written or kept.
pub fn keep_fact<T: Serialize>(facts: &dyn Facts, name: &str, value: &T) -> Result<(), String> {
    let value = postcard::to_allocvec(value).map_err(|error| error.to_string())?;
    facts.keep(name, value)
}

/// A protocol value carried inside an action's JSON as the hex of its postcard bytes: what a
/// value whose own encoding is postcard's (signatures, sealed halves) travels as.
///
/// # Errors
/// Never in practice: a value that does not encode.
pub fn pack<T: Serialize>(value: &T) -> Result<String, ActionCallError> {
    postcard::to_allocvec(value)
        .map(hex::encode)
        .map_err(|error| ActionCallError::Unavailable {
            reason: error.to_string(),
        })
}

/// The value [`pack`] made.
///
/// # Errors
/// [`ActionCallError::InvalidRequest`] for anything else.
pub fn unpack<T: DeserializeOwned>(packed: &str) -> Result<T, ActionCallError> {
    let invalid = |reason: String| ActionCallError::InvalidRequest { reason };
    let bytes = hex::decode(packed).map_err(|error| invalid(error.to_string()))?;
    postcard::from_bytes(&bytes).map_err(|error| invalid(error.to_string()))
}

/// Run `pending` for its effect, saying when it failed: a call nobody waits on.
pub fn spend(pending: Pending<Result<Value, ActionCallError>>, what: &'static str) {
    pending.then(move |answer| match answer {
        Some(Ok(_)) => {}
        Some(Err(error)) => tracing::debug!(%error, what, "a call to another device was refused"),
        None => tracing::debug!(what, "a call to another device went unanswered"),
    });
}

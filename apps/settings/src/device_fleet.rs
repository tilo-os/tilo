//! The person's trusted devices, as the host's identity actions answer
//! them.
//!
//! The host keeps the root-signed device fleet and the root key a
//! revocation is signed with. Settings asks it for the list and to revoke
//! one, through the action door, so neither the fleet file nor any secret
//! enters this process; the answers never enter widget or Automerge state.

use ui::tilo_actions::remote::identity_admin::{self, FleetDevice};
use ui::tilo_actions::remote::{Confirmation, InvokeArgument};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceView {
    pub label: String,
    pub key: [u8; 32],
    pub fingerprint: String,
    pub current: bool,
    pub revoked: bool,
    pub can_host: bool,
}

/// Ask the host for the devices trusted under the workspace's identity.
pub(crate) fn list(remote: &ui::RemoteActions) -> ui::LocalState<ui::InvocationCall> {
    remote.invoke(
        identity_admin::target(identity_admin::DEVICES),
        InvokeArgument::None,
        None,
    )
}

/// Ask the host to revoke `device` for `reason` (`lost`, `stolen`,
/// `retired` or `compromised`). Choosing the destructive row is the
/// person's confirmation; the host signs the revocation with the root key.
pub(crate) fn revoke(
    remote: &ui::RemoteActions,
    device: &[u8; 32],
    reason: &str,
) -> ui::LocalState<ui::InvocationCall> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("device".into(), hex::encode(device).into());
    arguments.insert("reason".into(), reason.into());
    arguments.insert("confirm".into(), true.into());
    remote.invoke(
        identity_admin::target(identity_admin::REVOKE),
        InvokeArgument::Json(arguments),
        Some(Confirmation::Confirmed),
    )
}

/// The devices the host's list answered.
///
/// # Errors
///
/// When the answer is not a device list.
pub(crate) fn devices(value: Option<serde_json::Value>) -> Result<Vec<DeviceView>, String> {
    let listed: Vec<FleetDevice> =
        serde_json::from_value(value.unwrap_or(serde_json::Value::Array(Vec::new())))
            .map_err(|_| "the device list the host answered is not readable".to_string())?;
    listed
        .into_iter()
        .map(|device| {
            let mut key = [0; 32];
            hex::decode_to_slice(&device.key_hex, &mut key)
                .map_err(|_| "the host answered a malformed device key".to_string())?;
            Ok(DeviceView {
                label: device.label,
                key,
                fingerprint: device.fingerprint,
                current: device.current,
                revoked: device.revoked,
                can_host: device.can_host,
            })
        })
        .collect()
}

/// What a settled host call answered, or why it did not.
///
/// # Errors
///
/// The host's refusal or failure, or that it was cancelled.
pub(crate) fn settled(
    call: &ui::LocalState<ui::InvocationCall>,
) -> Option<Result<ui::tilo_actions::ActionOutput, String>> {
    use ui::tilo_actions::InvocationUpdate;
    let answer = call.get();
    if !answer.is_settled() {
        return None;
    }
    Some(match answer {
        ui::RemoteCall::Done(InvocationUpdate::Succeeded { output, .. }) => Ok(output),
        ui::RemoteCall::Done(InvocationUpdate::Failed { error, .. }) => Err(error.message),
        ui::RemoteCall::Refused(error) => Err(error.to_string()),
        ui::RemoteCall::Done(_) | ui::RemoteCall::Pending => {
            Err("the host stopped before it answered".into())
        }
    })
}

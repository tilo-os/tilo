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
pub(crate) fn list(remote: &ui::RemoteActions) -> ui::InvocationCall {
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
) -> ui::InvocationCall {
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
pub(crate) fn devices(value: Option<serde_json::Value>) -> Result<Vec<DeviceView>, String> {
    let listed: Vec<FleetDevice> =
        serde_json::from_value(value.unwrap_or(serde_json::Value::Array(Vec::new())))
            .map_err(|_| "the device list the host answered is not readable".to_string())?;
    views(listed)
}

/// The devices `listed` names, as the pane shows them.
pub(crate) fn views(listed: Vec<FleetDevice>) -> Result<Vec<DeviceView>, String> {
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
    call: &ui::InvocationCall,
) -> Option<Result<ui::tilo_actions::ActionOutput, String>> {
    use ui::tilo_actions::InvocationUpdate;
    let answer = call.get();
    if !answer.is_finished() {
        return None;
    }
    Some(match answer {
        ui::Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => Ok(output),
        ui::Arrival::Ready(InvocationUpdate::Failed { error, .. }) => Err(error.message),
        ui::Arrival::Failed(error) => Err(error.to_string()),
        ui::Arrival::Ready(_) | ui::Arrival::Pending { .. } => {
            Err("the host stopped before it answered".into())
        }
    })
}

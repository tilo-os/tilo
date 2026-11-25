//! Who a key is when it acts in a workspace that holds its certificate: its certificate alone
//! decides, and every host that admits a key builds its caller here.

use tilo_action_policy::Grant;
use tilo_actions::remote::Caller;
use tilo_user_identity::DeviceCert;

/// The caller the key `certificate` certifies acts as: a server key is the person at the key, as
/// its certificate says ([`tilo_user_identity::DeviceCapabilities::own_key_in`]), carrying the
/// key beside them ([`Caller::own_key`]); any other agent's grant is the agent its key names
/// ([`tilo_action_policy::key_agent_id`]), never the person who delegated it.
///
/// `granted` is read for an agent's grant alone: the workspace's grants naming that agent, the
/// ones its person approved, which decide every call of its run. A server key holds its person's
/// own, so nothing is read for it.
///
/// # Errors
/// The workspace's grants naming the key's agent could not be read.
pub async fn caller_of_key(
    certificate: &DeviceCert,
    granted: impl Future<Output = Result<Vec<Grant>, String>> + Send,
) -> Result<Caller, String> {
    if certificate.capabilities.server_key {
        Ok(Caller::own_key(
            certificate.user_public_key,
            certificate.device_public_key,
            &certificate.device_label,
        ))
    } else if certificate.capabilities.agent.is_some() {
        Ok(Caller::delegated_agent(
            tilo_action_policy::key_agent_id(&certificate.device_public_key),
            certificate.user_public_key,
            certificate.device_public_key,
        )
        .granting(granted.await?))
    } else {
        // A machine of the person's, on a link: the person, with no key beside them.
        Ok(Caller::authenticated(
            certificate.user_public_key,
            certificate.device_public_key,
        ))
    }
}

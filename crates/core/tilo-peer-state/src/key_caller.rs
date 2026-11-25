//! Who a key is when it acts in a workspace that holds its certificate: its certificate alone
//! decides, and every host that admits a key builds its caller here.

use tilo_action_policy::Grant;
use tilo_actions::remote::Caller;
use tilo_user_identity::DeviceCert;

/// The caller a key's `certificate` is on a host that needs no workspace's grants to build it: a
/// server key is the person at the key, carrying it beside them ([`Caller::own_key`]); any other
/// grant is the person here, and where its calls land they are the agent its certificate names
/// ([`caller_of_key`]).
#[must_use]
pub fn caller_of_certificate(certificate: &DeviceCert) -> Caller {
    if certificate.capabilities.server_key {
        Caller::own_key(
            certificate.user_public_key,
            certificate.device_public_key,
            &certificate.device_label,
        )
    } else {
        Caller::authenticated(certificate.user_public_key, certificate.device_public_key)
    }
}

/// The caller the key `certificate` certifies acts as where no workspace's grants are read
/// first: the host's own operations, asked of its action service or in process, and a
/// workspace's governance verbs a device asks its own service for. A server key is its person
/// ([`caller_of_certificate`]); any other agent's grant is the agent its key names
/// ([`tilo_action_policy::key_agent_id`]), never the person who delegated it, so what it does
/// beyond reading waits for that person's yes ([`crate::key_caller`]).
#[must_use]
pub fn caller_of_key_here(certificate: &DeviceCert) -> Caller {
    if certificate.capabilities.agent.is_some() && !certificate.capabilities.server_key {
        Caller::delegated_agent(
            tilo_action_policy::key_agent_id(&certificate.device_public_key),
            certificate.user_public_key,
            certificate.device_public_key,
        )
    } else {
        Caller::authenticated(certificate.user_public_key, certificate.device_public_key)
    }
}

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
    if certificate.capabilities.agent.is_some() && !certificate.capabilities.server_key {
        Ok(caller_of_key_here(certificate).granting(granted.await?))
    } else {
        Ok(caller_of_certificate(certificate))
    }
}

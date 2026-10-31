//! Task agents: when a person asks, the shell creates the task, mints a key
//! for it, signs a task delegation with this device's key, carries the
//! delegation on the task's run beside its starter, and launches the agent
//! under that key (docs/architecture/agents.md, "Task agents and
//! collaborators").
//!
//! Nothing here admits anyone: the broker verifies the delegation's chain
//! when the agent joins and on every revalidation, and the run's checks
//! verify it again. The device already acts as its person in full, so a
//! narrower agent it mints widens nothing.

use std::time::Duration;

use tilo_peer_state::join::task_delegation::{MAX_TASK_DELEGATION_NS, TaskDelegation};

/// How long a task delegation lasts: a task has no deadline of its own, so
/// its agent's authority ends a day on at the latest, sooner when the task
/// ends.
const TASK_DELEGATION_LIFETIME: Duration = Duration::from_nanos(MAX_TASK_DELEGATION_NS);

/// The composer's choice of a new agent made for the task, rather than a
/// collaborator by name. It is the task agent prefix alone, which no task
/// agent's id and no collaborator's id ever is.
pub(crate) const NEW_TASK_AGENT: &str = tilo_peer_state::join::task_delegation::TASK_AGENT_PREFIX;

/// A task's agent, minted on this device: its id and delegation for the
/// task's run, and the credentials it is launched with.
pub(crate) struct MintedTaskAgent {
    pub agent_id: String,
    /// The delegation, as the run's record keeps it.
    pub delegation_hex: String,
    credentials: runtime_collab::TaskAgentCredentials,
}

impl std::fmt::Debug for MintedTaskAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MintedTaskAgent")
            .field("agent_id", &self.agent_id)
            .finish_non_exhaustive()
    }
}

/// Mint the agent for the task `work_group` (its issue's address) in
/// `room`: a fresh key, delegated by this device's key for the identity the
/// room selects.
///
/// # Errors
/// The room selects no identity, this device's certificate is not trusted
/// now, or the clock or the delegation's fields are out of bounds.
pub(crate) fn mint(room: &str, work_group: &str) -> Result<MintedTaskAgent, String> {
    let (device, certificate) = tilo_identity_host::broker::trusted_current_device(room)
        .map_err(|error| format!("this device cannot delegate a task agent: {error}"))?;
    let agent_key = ed25519_dalek::SigningKey::from_bytes(&crate::random::nonzero::<32>());
    let issued = tilo_clock::try_unix_now_ns().ok_or("the clock is unavailable")?;
    let lifetime = u64::try_from(TASK_DELEGATION_LIFETIME.as_nanos()).unwrap_or(u64::MAX);
    let delegation = TaskDelegation::sign(
        &device,
        certificate.user_public_key,
        agent_key.verifying_key().to_bytes(),
        work_group,
        room,
        issued,
        issued.saturating_add(lifetime),
    )
    .map_err(|error| format!("the task delegation could not be signed: {error}"))?;
    drop(device);
    let device_certificate = serde_json::to_vec(&certificate)
        .map_err(|error| format!("encoding this device's certificate failed: {error}"))?;
    Ok(MintedTaskAgent {
        agent_id: delegation.agent_id.clone(),
        delegation_hex: delegation.to_hex(),
        credentials: runtime_collab::TaskAgentCredentials {
            workspace_id: room.to_owned(),
            device_certificate,
            delegation: delegation.encode(),
            agent_key: agent_key.to_bytes(),
        },
    })
}

impl MintedTaskAgent {
    /// What the agent joins with: the device's certificate and the task
    /// delegation, proved by the task key's signature over the room's join
    /// bytes. The desktop's agent door joins its connection's run with it
    /// ([`tilo_action_broker::collab_app_broker::TaskRunHost`]).
    ///
    /// # Errors
    /// The presentation could not be encoded.
    pub(crate) fn presentation(&self) -> Result<Vec<u8>, String> {
        use ed25519_dalek::Signer as _;
        let credentials = &self.credentials;
        let proof = ed25519_dalek::SigningKey::from_bytes(&credentials.agent_key)
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                &credentials.workspace_id,
            ))
            .to_bytes();
        credentials
            .presentation(proof)
            .map_err(|error| format!("encoding the task agent's presentation failed: {error}"))
    }

    /// Launch the agent for its task, under its key: once the task's run
    /// carries its delegation, so its join finds the task going.
    ///
    /// # Errors
    /// As [`crate::shell::launcher::launch_task_agent`].
    pub(crate) fn launch(&self, workspace: &crate::shell::state::Workspace) -> Result<(), String> {
        crate::shell::launcher::launch_task_agent(workspace, &self.agent_id, &self.credentials)
            .map(drop)
            .map_err(|error| error.user_message())
    }
}

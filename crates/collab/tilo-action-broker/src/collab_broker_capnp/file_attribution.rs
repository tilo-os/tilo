//! Which agent an application's file writes are for.
//!
//! An agent writes files only through an app's action: the app writes
//! through the mount, so the version is this device's, signed for its person.
//! Before it writes, the app names the paths and the invocation it is running
//! (`attributeFileWrites`). The agent is never the app's to name: it comes
//! from that live invocation's custody here, exactly as the clipboard takes
//! its owner, and only for an agent acting through this device's own session
//! (a local connection or a broker-leased task, never a peer's device). The
//! filesystem then names that agent, for this device's person, in the signed
//! record of each version it makes of those paths. Nothing is granted.
use super::invocation_journal::InvocationOwner;
use super::{CapnpBrokerState, CapnpChannelHandle, PendingInvocation};
use crate::services::FileWriteAttribution;
use tilo_actions::InvocationId;

/// Most paths one attribution names.
pub(crate) const MAX_ATTRIBUTED_PATHS: usize = 256;

impl CapnpChannelHandle {
    /// Hand the filesystem the agent behind this connection's coming writes
    /// to `paths` for its running invocation `parent`. `Ok(false)` when that
    /// invocation is no agent's acting through this session.
    pub(super) fn attribute_writes_for_invocation(
        &self,
        paths: Vec<String>,
        parent: Option<InvocationId>,
    ) -> Result<bool, String> {
        if paths.is_empty() || paths.len() > MAX_ATTRIBUTED_PATHS {
            return Err("an attribution names between one and 256 paths".into());
        }
        let parent = parent.ok_or("a write is attributed only to a running invocation")?;
        let pending = self
            .clipboard_original(Some(parent))?
            .ok_or("a write is attributed only to a running invocation")?;
        let Some(agent_id) = custody_agent(&self.state, &pending) else {
            return Ok(false);
        };
        let service = self
            .state
            .services
            .file_write_attributions
            .as_ref()
            .ok_or("write attribution unavailable")?;
        let principal = format!("agent:{agent_id}");
        let (red, green, blue) = ui::presence::user_color(&principal);
        service.attribute(FileWriteAttribution {
            paths,
            label: agent_id.clone(),
            agent_id,
            color: [red, green, blue],
        })?;
        Ok(true)
    }
}

/// The agent a live invocation runs for, as this broker admitted it, when
/// it acts through this device's own session: from the caller the broker
/// retained, or, for a broker-leased run without one, from the invoker the
/// broker stamped. `None` for a person, a Shortcut, an app, and anything a
/// peer's device invoked here.
pub(super) fn custody_agent(
    state: &CapnpBrokerState,
    pending: &PendingInvocation,
) -> Option<String> {
    if matches!(pending.owner, InvocationOwner::Certified { .. }) {
        return None;
    }
    let principal = match pending.caller.as_ref() {
        Some(caller) => {
            let caller = caller.current(state)?;
            let actor = state.caller_actor(&caller, &pending.cluster_id);
            actor.is_agent.then(|| actor.id().to_owned())?
        }
        None => {
            let invoker = pending.context.invoker.as_ref()?;
            (invoker.kind == tilo_actions::ActorKind::Agent && invoker.actor == pending.actor)
                .then(|| invoker.actor.clone())?
        }
    };
    let id = principal.strip_prefix("agent:")?.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

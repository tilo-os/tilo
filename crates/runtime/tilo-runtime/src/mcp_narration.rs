//! What a session's actions on records are doing as they go, told to a door
//! that shows its agent working ([`Acting`]).

use tilo_actions::remote::{ActionHost, RemoteActionTarget};
use tilo_lock::MutexRecover as _;

use super::McpServer;

/// The record an agent call worked in: the app's id and the workspace's
/// record id, as the provider of the resource the call resolved names it.
/// What a door that shows its agent present says the agent acts in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActedRecord {
    /// The app's stable id (`so.tilo.notes`).
    pub app: String,
    /// The workspace's record id, which a provider's own token names.
    pub record: String,
}

/// What an agent's action on a record is doing now: the record, the action,
/// its phase, and once it is done, where it left the agent in the record. A
/// door that shows its agent working tells the people watching from it.
#[derive(Clone, Debug, PartialEq)]
pub struct Acting {
    pub invocation: tilo_actions::InvocationId,
    /// The record the action runs in.
    pub record: ActedRecord,
    /// The action, on the resource it is published on.
    pub action: tilo_actions::ActionAddress,
    /// Locating as it is asked, applying once the host took it, then how it
    /// ended.
    pub phase: tilo_actions::InteractionPhase,
    /// Where it left the agent, once it is done: the text it changed.
    pub left_at: Option<tilo_actions::remote::ActorPlace>,
}

/// The phase a terminal `update` ends its narration in.
pub(super) fn ended(update: &tilo_actions::InvocationUpdate) -> tilo_actions::InteractionPhase {
    use tilo_actions::{InteractionPhase, InvocationUpdate};
    match update {
        InvocationUpdate::Failed { error, .. } => InteractionPhase::Failed {
            code: Some(error.code.clone()).filter(|code| !code.trim().is_empty()),
            message: if error.message.trim().is_empty() {
                error.code.clone()
            } else {
                error.message.clone()
            },
        },
        InvocationUpdate::Cancelled { message, .. } => InteractionPhase::Cancelled {
            message: message.clone(),
        },
        _ => InteractionPhase::Committed,
    }
}

/// Who hears [`Acting`] as it happens.
pub type Narration = std::sync::Arc<dyn Fn(Acting) + Send + Sync>;

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// Tell the narration `invocation` reached `phase`. An invocation that
    /// runs in no record of the workspace is told nobody: it has no place a
    /// person watches.
    pub(super) fn narrate(
        &self,
        invocation: tilo_actions::InvocationId,
        target: Option<&RemoteActionTarget>,
        phase: tilo_actions::InteractionPhase,
        left_at: Option<tilo_actions::remote::ActorPlace>,
    ) {
        let Some(narration) = &self.narration else {
            return;
        };
        let acting = {
            let mut narrating = self.narrating.lock_recover();
            // A link changes nothing and moves no one: the person is not
            // told the agent asked for one.
            if let Some((app, record)) = target
                .filter(|target| target.address.action != tilo_actions::remote::SHARE_LINK_ACTION)
                .and_then(|target| tilo_actions::remote::record_parts(&target.provider))
                .filter(|_| !narrating.contains_key(&invocation))
                && let Some(target) = target
            {
                narrating.insert(
                    invocation,
                    Acting {
                        invocation,
                        record: ActedRecord {
                            app: app.to_owned(),
                            record: record.to_owned(),
                        },
                        action: target.address.clone(),
                        phase: phase.clone(),
                        left_at: None,
                    },
                );
            }
            let terminal = phase.is_terminal();
            let Some(acting) = narrating.get_mut(&invocation) else {
                return;
            };
            acting.phase = phase;
            acting.left_at = left_at;
            let acting = acting.clone();
            if terminal {
                narrating.remove(&invocation);
            }
            acting
        };
        narration(acting);
    }
}

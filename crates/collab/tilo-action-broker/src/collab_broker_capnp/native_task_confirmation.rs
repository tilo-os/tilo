//! A run's action that asks first pauses the run for the person.
//!
//! Before a run's child starts, its draft is checked against the question
//! its issue holds, so a pause outlives the agent, the broker and the host
//! restarting ([`CapnpBrokerState::tracker_confirmation`]):
//!
//! - no question for these exact arguments, and the grants ask first: the
//!   agent asks it on the issue and is told to wait
//!   ([`tilo_actions::draft::AWAITING_CONFIRMATION`]); nothing runs;
//! - still waiting: the same answer;
//! - confirmed: the child runs with the agent's arguments as the person
//!   changed them, admitted as confirmed;
//! - declined: the agent is told, with the person's reason
//!   ([`tilo_actions::draft::DECLINED`]).
//!
//! An agent never confirms for itself: what it sends is only ever matched
//! against what its person answered.

use super::native_task_execution::ChildReask;
use super::{CapnpBrokerState, PendingTaskRun, PublishedAction};

/// How a run's child may start.
pub(super) enum NativeGate {
    /// As requested: nothing asks first.
    Proceed,
    /// The person confirmed it: with these arguments, as they changed them,
    /// admitted as confirmed. The confirmation is settled once it starts.
    Confirmed {
        argument: String,
        confirmation: String,
    },
}

impl CapnpBrokerState {
    /// Decide how `reask`'s child may start.
    ///
    /// # Errors
    /// The child waits for the person, the person declined it, or the run
    /// or action can no longer be read.
    pub(super) fn native_confirmation_gate(
        &self,
        reask: &ChildReask<'_>,
    ) -> Result<NativeGate, String> {
        let run = self.task_run_for(reask.peer_id, reask.agent_id, reask.run_id, reask.lease)?;
        let target = reask.target;
        let (surface, offered) = self
            .surfaces
            .lock()
            .map_err(|_| "task action surface unavailable")?
            .get(&target.peer_id)
            .filter(|surface| surface.route.cluster_id == run.cluster_id)
            .and_then(|surface| surface.action_projection(target.resource, target.action))
            .ok_or("the task action is no longer published in this workspace")?;
        self.native_confirmation_for(
            &run,
            reask.lease,
            (&surface.app_id, &offered),
            (target.resource, target.action),
            target.argument,
        )
    }

    /// [`Self::native_confirmation_gate`] for `offered`, an action of `app`
    /// on `resource`, whether it is published by the running application or
    /// retained from a closed one: a run's action that asks first on its way
    /// to an application being launched pauses at a reviewable draft too.
    ///
    /// # Errors
    /// As [`Self::native_confirmation_gate`].
    pub(super) fn native_confirmation_for(
        &self,
        run: &PendingTaskRun,
        lease: &str,
        offered: (&str, &PublishedAction),
        target: (&str, &str),
        argument: Option<&str>,
    ) -> Result<NativeGate, String> {
        self.tracker_confirmation(run, lease, offered, target, argument)
    }
}

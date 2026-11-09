//! The person's agent door, as the overview of their Tilo follows it: the
//! door's live read ([`agent_door::DOOR`]), asked of the host that serves it
//! and followed as agents connect, work and leave. It is the read Console's
//! agent card shows, not a second one: the same door, asked of the same host
//! ([`ActionHost`]), and each host's answer is what its own run names.
//!
//! A host that refuses the read, or a session that reaches none, leaves the
//! rows as they are: the overview says nothing of agents it cannot hear
//! about, and asks again a while later.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_actions::door::ActionAddress;
use tilo_actions::remote::{
    ActionHost, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget, agent_door,
};
use tilo_actions::{InvocationId, InvocationUpdate};

/// How long the door is given to answer before its read is asked again: a
/// refused read, or a session that reached no host, is worth another try
/// later, never a frame spent waiting for it.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_secs(15);

/// The agent door's read, as the overview follows it.
#[derive(Default)]
pub struct DoorRead {
    /// The read being followed, once asked.
    read: Option<InvocationId>,
    /// When it was asked, so one that never answers is asked again.
    asked: Option<ui::time::Instant>,
    /// What the door last answered.
    answer: Option<agent_door::Door>,
    /// Whether a wake is out for the answer to move.
    armed: Arc<AtomicBool>,
}

impl DoorRead {
    /// Follow the door `host` serves for `caller`, waking `wake` when the
    /// answer may have moved. Whether the door's answer changed, so the rows
    /// are to be drawn again.
    pub fn follow(
        &mut self,
        host: &dyn ActionHost,
        caller: &Caller,
        wake: &Arc<dyn Fn() + Send + Sync>,
    ) -> bool {
        let now = ui::time::Instant::now();
        let overdue = self
            .asked
            .is_some_and(|asked| now.saturating_duration_since(asked) >= ASK_AGAIN);
        let id = match self.read {
            Some(id) if self.answer.is_some() || !overdue => id,
            _ => {
                let id = InvocationId::new();
                self.read = Some(id);
                self.asked = Some(now);
                // The answer is the read's own state, followed below: a host
                // that refuses the ask answers here, with nothing to show.
                drop(host.invoke(caller, request(id, agent_door::DOOR, InvokeArgument::None)));
                id
            }
        };
        let answer = host.observe(caller, &id).and_then(|state| door_of(&state));
        let changed = answer.is_some() && answer != self.answer;
        if changed {
            self.answer = answer;
        }
        // One wake out at a time: a host that holds its answer says when it
        // moved, and one that cannot is asked again on the next frame.
        if !self.armed.load(Ordering::Relaxed)
            && let Some(change) = host.next_change(caller, &id)
        {
            self.armed.store(true, Ordering::Release);
            let armed = Arc::clone(&self.armed);
            let wake = Arc::clone(wake);
            change.then(move |_| {
                armed.store(false, Ordering::Release);
                wake();
            });
        }
        changed
    }

    /// The door as it last answered, for the overview's rows.
    #[must_use]
    pub fn door(&self) -> Option<&agent_door::Door> {
        self.answer.as_ref()
    }

    /// Take back grant `grant` at the door `host` serves, as the person:
    /// the door ends its runs and lets it in no more, and a page hands the
    /// device's revocation to their root to sign, as it revokes any of
    /// their devices. The door answers in its own time; what it says next
    /// is what the read shows.
    pub fn revoke(host: &dyn ActionHost, caller: &Caller, grant: &str) {
        let request = request(
            InvocationId::new(),
            agent_door::REVOKE,
            agent_door::argument(&agent_door::GrantRef {
                grant: grant.to_owned(),
            }),
        );
        drop(host.invoke(caller, request));
    }
}

/// The invocation of door `action`, with `argument`.
fn request(invocation: InvocationId, action: &str, argument: InvokeArgument) -> InvokeRequest {
    InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider: agent_door::PROVIDER.to_owned(),
            address: ActionAddress::new(agent_door::RESOURCE, action),
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

/// What a read's state says of the door.
fn door_of(state: &tilo_actions::remote::InvocationState) -> Option<agent_door::Door> {
    let InvocationUpdate::Item { value, .. } = &state.update else {
        return None;
    };
    serde_json::from_value(value.clone()).ok()
}

#[cfg(test)]
mod tests;

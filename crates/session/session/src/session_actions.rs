//! The session's own actions, offered to a turn's agent as steps of the broker
//! (`docs/designs/app-language/shell.md` §8.1, "Any action").
//!
//! The agent answering a person's turn arranges the screen as the REPL does: the
//! arrangement's `place` and `place-found`, the undo steps, the windows and the notices.
//! Those are published by the session on its own door, on the loop's thread, and this is
//! the provider its broker answers them under, so one catalogue names them for every caller
//! that has a broker: the agent door, a launched agent, and a task's program alike.
//!
//! Each step is decided where it lands: the broker admits it as the turn's step (the
//! person's grants, the agent's, the issue's), and the session carries it out on the loop's
//! pass, attributed to the agent and the person who asked, as a change the person takes
//! back like any other. The arrangement is `Rc<RefCell<…>>` on the loop's thread and is
//! never shared: an invocation is handed to [`SessionDoors`] and answered on the pass, as
//! the door's own calls are.

use std::sync::Arc;

use tilo_action_broker::services::{HostStepOutcome, HostStepRequest, HostStepServices};
use tilo_actions::remote::session_actions as wire;
use tilo_actions::remote::{Pending, pending};
use tilo_actions::{ActionDescriptor, ActionFailure, ActionOutput};

use crate::agent_door::{SessionDoors, Steps};

/// The session's own actions, as the broker's catalogue names them.
pub(crate) struct SessionActions {
    doors: Arc<SessionDoors>,
}

impl SessionActions {
    /// The session's actions, whose rows are read through `doors`.
    pub(crate) fn new(doors: Arc<SessionDoors>) -> Self {
        Self { doors }
    }

    /// The rows the session's own door offers now, read on its pass.
    fn offered(&self) -> Steps {
        let (done, asked) = pending();
        self.doors.steps(done);
        asked
            .wait_timeout(crate::agent_door::STEP_WAIT)
            .ok()
            .flatten()
            .unwrap_or_default()
    }
}

impl HostStepServices for SessionActions {
    fn provider(&self) -> &'static str {
        wire::PROVIDER
    }

    fn app_id(&self) -> &'static str {
        wire::PROVIDER
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        self.offered()
            .rows
            .into_iter()
            .map(|placed| placed.action)
            .collect()
    }

    fn rows(&self) -> Vec<tilo_actions::door::PlacedAction> {
        self.offered().rows
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        let argument = request.argument.map(str::to_owned);
        let (done, asked) = pending();
        self.doors.invoke_step(
            request.actor.id().to_owned(),
            request.address.clone(),
            argument,
            done,
        );
        Ok(asked.map(|ran| {
            ran.map_err(|reason| {
                Box::new(ActionFailure {
                    code: "session_step_refused".into(),
                    message: reason,
                    details: None,
                    retryable: false,
                })
            })
            .map(|output| ActionOutput {
                value: output,
                resources: Vec::new(),
            })
        }))
    }
}

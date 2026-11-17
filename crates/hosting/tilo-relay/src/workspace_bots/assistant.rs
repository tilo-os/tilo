//! What this host answers for the programs that run for a person
//! ([`tilo_agent_session::bot::host_steps`]).
//!
//! Starting a turn of a person's assistant is one entry of the host's, which
//! every event, reminder and sub-agent starts work through, and calling a
//! person's own model is another. This module is the one place a program's
//! steps reach both from. Until those entries are on this host, an
//! instruction a program hands over finds nothing to take it and a
//! judgement no model to answer it: the run is tried again, and kept as a
//! dead letter once its attempts are spent, so what was not delivered can
//! be read and run again.

use std::sync::Arc;

use serde_json::Value;
use tilo_agent_session::bot::host_steps::{HostStepError, HostSteps, Instruction, Judgement};

/// What this host answers for a program that runs for a person.
#[must_use]
pub fn host_steps() -> Arc<dyn HostSteps> {
    Arc::new(NoAssistant)
}

/// A host that starts no turn of a person's assistant and calls no
/// person's model.
struct NoAssistant;

impl HostSteps for NoAssistant {
    fn instruct(&self, _instruction: Instruction) -> Result<String, HostStepError> {
        Err(HostStepError::Unavailable(
            "this host starts no turn of a person's assistant yet".into(),
        ))
    }

    fn judge(&self, _judgement: Judgement) -> Result<Value, HostStepError> {
        Err(HostStepError::Unavailable(
            "this host calls no person's model yet".into(),
        ))
    }
}

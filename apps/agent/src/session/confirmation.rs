//! A step whose operation asks the person before it acts.
//!
//! An operation that asks first is shown to the person by its own app, on the resource the operation
//! acts on; a turn's step is asked about on the system's own sheet, and the host answers the agent
//! once the person decided there (`tilo_workspace_service::apps`'s agent connection, and the session's
//! `step_questions`). The agent waits for that answer in the person's own time.

/// The host's `error` as the step's outcome: a decline in the person's
/// words, with what to do next; anything else as it came.
pub(super) fn step_failure(error: String) -> String {
    let marker = format!("{}: ", tilo_actions::draft::DECLINED);
    match error.find(&marker) {
        Some(at) => {
            let said = error
                .get(at.saturating_add(marker.len())..)
                .unwrap_or_default()
                .trim();
            format!(
                "{said}. The person declined this step: do not retry it; continue without it or \
                 end the task, and say so in your reply"
            )
        }
        None => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A decline reaches the model in the person's words; anything else as it came.
    #[test]
    fn a_decline_reaches_the_model_in_the_persons_words() {
        let error = "refused: action_declined: the person declined Send message: Not to Bob";
        let said = step_failure(error.into());
        assert!(said.starts_with("the person declined Send message: Not to Bob."));
        assert!(said.contains("do not retry it"));
        assert_eq!(step_failure("timeout".into()), "timeout");
    }
}

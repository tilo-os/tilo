//! A step whose operation asks the person before it acts.
//!
//! An operation that asks first is shown to the person by its own app, on
//! the record the operation acts on; a conversation has no place to put that
//! question yet, so the turn cannot wait for it. The step ends with the
//! reason and the model plans around it: it says so in the reply and
//! continues, or ends the turn.

use super::{TaskAction, TaskActionStop};

/// Whether the host's `error` says the step waits for the person.
pub(super) fn is_awaiting(error: &str) -> bool {
    error.contains(tilo_actions::draft::AWAITING_CONFIRMATION)
}

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

/// Why a step that asks the person cannot go on in a conversation.
pub(super) fn cannot_ask(action: &TaskAction) -> TaskActionStop {
    TaskActionStop::Failed(format!(
        "{} asks the person to confirm it first, and this conversation cannot show that \
         question: say in your reply that it is apart, and continue without it or end the turn",
        action.label
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decline_reaches_the_model_in_the_persons_words() {
        let error = "Failed: action_declined: the person declined Send message: Not to Bob";
        let said = step_failure(error.into());
        assert!(said.starts_with("the person declined Send message: Not to Bob."));
        assert!(said.contains("do not retry it"));
        assert!(is_awaiting(
            "Failed: action_awaiting_confirmation: Send message waits for the person"
        ));
        assert!(!is_awaiting(error));
        assert_eq!(step_failure("timeout".into()), "timeout");
    }
}

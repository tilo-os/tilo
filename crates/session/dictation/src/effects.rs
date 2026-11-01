//! The `dictation:session` resource's actions.
//!
//! Every one acts on this device only, and none of them is consent: allowing
//! the microphone is the person's answer on the session's own prompt
//! ([`crate::DictationLink::allow`]), which no action can stand in for. There
//! is deliberately no action for copying a kept transcript either: copying it
//! takes the words out of the session, so only the prompt the person is
//! looking at can do it.

use tilo_resource::Effect;

/// The `dictation:session` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.dictation.session")]
pub enum DictationAction {
    /// Dictate into the field the person is typing in, or insert what was
    /// heard when a dictation is already listening. The person is asked to
    /// allow the microphone for that dictation first.
    #[effect(
        own_device,
        attended,
        label = "Dictate",
        icon = "microphone",
        keywords = "dictation, dictate, speech to text, microphone, diktieren"
    )]
    Start,
    /// Stop recording and insert what was heard into the same field.
    #[effect(
        own_device,
        label = "Insert what was heard",
        icon = "microphone-off",
        keywords = "stop dictation, finish dictation, insert the text"
    )]
    Finish,
    /// Stop recording and throw away what was heard.
    #[effect(
        own_device,
        idempotent,
        label = "Discard the dictation",
        icon = "x",
        keywords = "cancel dictation, throw away the dictation"
    )]
    Cancel,
    /// Throw away a transcript that was not inserted.
    #[effect(
        own_device,
        idempotent,
        label = "Discard the transcript",
        icon = "trash",
        keywords = "recovery, transcript, throw away the transcript"
    )]
    DiscardTranscript,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_action_allows_the_microphone_or_takes_a_transcript_out() {
        let actions: Vec<String> = DictationAction::specs()
            .into_iter()
            .map(|spec| spec.id.clone())
            .collect();
        assert_eq!(
            actions,
            ["start", "finish", "cancel", "discard-transcript"],
            "consent and copying stay on the person's own prompt"
        );
        for spec in DictationAction::specs() {
            assert_eq!(
                spec.operation_type.as_deref(),
                Some("so.tilo.dictation.session")
            );
        }
    }
}

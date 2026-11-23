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
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        attended,
        icon = "microphone",
        keywords = "dictation, dictate, speech to text, microphone, diktieren"
    )]
    Start,
    /// Stop recording and insert what was heard into the same field.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "microphone-off",
        keywords = "stop dictation, finish dictation, insert the text"
    )]
    Finish,
    /// Stop recording and throw away what was heard.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "x",
        keywords = "cancel dictation, throw away the dictation"
    )]
    Cancel,
    /// Throw away a transcript that was not inserted.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "trash",
        keywords = "recovery, transcript, throw away the transcript"
    )]
    DiscardTranscript,
}

//! A turn: the message an agent acts on.
//!
//! An agent acts inside a turn, caused by a message in a conversation it takes part in. Every op
//! of the turn carries it, so a receiver can check the cause it arrived with and presence can
//! show what the agent works on. What the turn may do is decided as every action is
//! ([`tilo_member_types::Acting`], [`crate::Authority::of_acting`]): for the person whose message it
//! answers, bounded by the agent and by the person the agent belongs to, so a turn answering
//! another member does what all of them may.

/// The message an agent acts on.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Turn {
    /// The conversation the message was posted in, by its address in the
    /// workspace.
    pub conversation: String,
    /// The message, by its id in that conversation.
    pub message: String,
}

impl Turn {
    /// The turn the message `message` in `conversation` makes.
    #[must_use]
    pub fn new(conversation: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            conversation: conversation.into(),
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_reads_and_writes_as_its_parts() {
        let turn = Turn::new("chat/workspace/channels/1", "m1");
        let text = serde_json::to_string(&turn).unwrap();
        assert_eq!(serde_json::from_str::<Turn>(&text).unwrap(), turn);
    }
}

//! What the person is writing and has not sent: their own record, kept in
//! their own room.
//!
//! A draft is the writer's alone. It follows them to their other devices, it
//! survives the window closing, and it never enters a conversation's record:
//! nothing here is sent, queued or shown to anyone else. What a person typed
//! and did not send is theirs until they send it.
//!
//! The key is the transport route the words were typed for — service, account
//! and recipient — because a draft belongs to one conversation: the same person
//! on Signal and on SMS are two threads, and words typed into one must not turn
//! up in the other.

use std::collections::BTreeMap;

use ui::tilo_resource::Refusal;
use ui::tilo_resource::{self, Model};

/// The most a draft may hold, matching what a message may carry.
const MAX_DRAFT_CHARS: usize = 16 * 1024;
/// The most a conversation key may hold, so a hostile key cannot grow the
/// record without limit.
const MAX_KEY_CHARS: usize = 512;

/// The person's unsent message text, by conversation.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct MessageDrafts {
    /// The text of each unsent message, under its conversation.
    pub drafts: BTreeMap<String, String>,
}

#[ui::tilo_resource::resource(kind = "messages.drafts", label = "Message drafts", params = drafts)]
impl MessageDrafts {
    /// Keep a draft.
    ///
    /// What the person has typed into a conversation's composer so far. An
    /// empty (or blank) body removes the draft: a cleared field is not a
    /// message half-written.
    #[action(housekeeping, audience = "agents")]
    pub fn keep_draft(
        &mut self,
        #[param("The conversation the words are typed for")] conversation: String,
        #[param("The words as they stand")] body: String,
    ) -> Result<(), Refusal> {
        if conversation.is_empty() || conversation.len() > MAX_KEY_CHARS {
            return Err(Refusal::new(
                "a draft is kept under the conversation it is written in",
            ));
        }
        if body.chars().count() > MAX_DRAFT_CHARS {
            return Err(Refusal::new("a draft is at most 16 000 characters"));
        }
        if body.trim().is_empty() {
            self.drafts.remove(&conversation);
        } else {
            self.drafts.insert(conversation, body);
        }
        Ok(())
    }

    /// Drop a draft.
    ///
    /// The words were sent, or the person threw them away. Nothing else in the
    /// record changes.
    #[action(housekeeping, audience = "agents")]
    pub fn drop_draft(
        &mut self,
        #[param("The conversation whose draft goes")] conversation: String,
    ) {
        self.drafts.remove(&conversation);
    }
}

/// The conversation key a draft is kept under: the exact route the words were
/// typed for, as this app names a route everywhere else.
///
/// Every reviewed fact is part of it — the service, the sender account, the
/// recipient, and the contact association and identity revision the choice was
/// confirmed under. A route re-reviewed into a different identity is a
/// different conversation: words typed for one must not resurface in the
/// other.
#[must_use]
pub(crate) fn key(choice: &crate::view::Choice) -> String {
    choice.value()
}

#[cfg(test)]
mod tests {
    use ui::tilo_resource::Replica;

    use super::*;

    /// One action as the door takes it: stamped with when it was made.
    fn stamped(action: MessageDraftsAction) -> ui::tilo_resource::Stamped<MessageDraftsAction> {
        ui::tilo_resource::Stamped {
            operation: action,
            now_ms: 1_782_828_300_000,
        }
    }

    fn keep(conversation: &str, body: &str) -> ui::tilo_resource::Stamped<MessageDraftsAction> {
        stamped(MessageDraftsAction::KeepDraft(drafts::KeepDraft {
            conversation: conversation.into(),
            body: body.into(),
        }))
    }

    #[test]
    fn a_draft_is_kept_per_conversation_and_cleared_when_it_is_blank() {
        let mut ada = Replica::<MessageDrafts>::new().as_author("u_ada");
        ada.apply_action(&keep("signal:a1:+15550100", "see you at 8"))
            .expect("a draft is kept");
        ada.apply_action(&keep("sms:a2:+15550100", "on my way"))
            .expect("a draft is kept");
        let state = ada.state().expect("the record is readable");
        assert_eq!(
            state.drafts.len(),
            2,
            "two transports are two conversations"
        );
        assert_eq!(
            state.drafts.get("signal:a1:+15550100").map(String::as_str),
            Some("see you at 8")
        );
        // A blank draft is no draft.
        ada.apply_action(&keep("signal:a1:+15550100", "   "))
            .expect("a draft is cleared");
        let state = ada.state().expect("the record is readable");
        assert!(!state.drafts.contains_key("signal:a1:+15550100"));
        assert!(state.drafts.contains_key("sms:a2:+15550100"));
    }

    #[test]
    fn a_dropped_draft_is_gone_and_the_others_stay() {
        let mut ada = Replica::<MessageDrafts>::new().as_author("u_ada");
        ada.apply_action(&keep("signal:a1:+15550100", "half a thought"))
            .expect("a draft is kept");
        ada.apply_action(&stamped(MessageDraftsAction::DropDraft(
            drafts::DropDraft {
                conversation: "signal:a1:+15550100".into(),
            },
        )))
        .expect("a draft is dropped");
        assert!(
            ada.state()
                .expect("the record is readable")
                .drafts
                .is_empty()
        );
    }

    #[test]
    fn a_key_names_one_route_of_one_person() {
        let route = crate::view::Choice {
            service: "signal".into(),
            account: "a1".into(),
            recipient: "+15550100".into(),
            association: "e1".into(),
            scope: "s1".into(),
            revision: "r1".into(),
        };
        // The same facts name the same conversation, and a re-reviewed
        // identity names another one.
        assert_eq!(key(&route), key(&route.clone()));
        let rereviewed = crate::view::Choice {
            revision: "r2".into(),
            ..route.clone()
        };
        assert_ne!(key(&route), key(&rereviewed));
    }
}

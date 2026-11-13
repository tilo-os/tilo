//! What is being written and has not been sent, in a record beside the conversations and
//! kept in the workspace Messages is open in. A draft survives the window closing and never
//! enters a conversation's record: nothing here is sent, queued or shown to anyone else.

use std::collections::BTreeMap;

use ui::tilo_resource::Refusal;
use ui::tilo_resource::{self, Model};

const MAX_DRAFT_CHARS: usize = 16 * 1024;
/// The most a conversation key may hold, so a hostile key cannot grow the record without
/// limit.
const MAX_KEY_CHARS: usize = 512;

/// The person's unsent message text, by conversation.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct MessageDrafts {
    /// The text of each unsent message, under its conversation.
    pub drafts: BTreeMap<String, String>,
}

#[ui::tilo_resource::resource(kind = "messages.drafts", params = drafts)]
impl MessageDrafts {
    /// Keep a draft.
    ///
    /// What the person has typed into a conversation's composer so far.
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
    /// The words were sent, or the person threw them away.
    #[action(housekeeping, audience = "agents")]
    pub fn drop_draft(
        &mut self,
        #[param("The conversation whose draft goes")] conversation: String,
    ) {
        self.drafts.remove(&conversation);
    }
}

/// The conversation key a draft is kept under: the exact route the words were typed for, as
/// this app names a route everywhere else.
///
/// A route re-reviewed into a different identity is a different conversation: words typed
/// for one must not resurface in the other.
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
        assert_eq!(key(&route), key(&route.clone()));
        let rereviewed = crate::view::Choice {
            revision: "r2".into(),
            ..route.clone()
        };
        assert_ne!(key(&route), key(&rereviewed));
    }
}

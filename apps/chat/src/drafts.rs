//! What the person is writing and has not sent: their own record, kept in
//! their own room.
//!
//! A draft is the writer's alone. It follows them to their other devices
//! and never reaches the room the chat is shared in: what one person types
//! never shows in another's composer, and keystrokes never spend the room's
//! change budget. Sending a message is what puts words in the room.

use std::collections::BTreeMap;

use tilo_document::RichText;
use ui::tilo_resource::{self, Model, Refusal};

/// The person's unsent messages, by channel or thread.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ChatDrafts {
    /// Each draft under its channel id, or `channel::t::message` for a reply
    /// in that message's thread.
    pub drafts: BTreeMap<String, RichText>,
}

#[ui::tilo_resource::resource(kind = "chat.drafts", label = "Chat drafts", params = drafts)]
impl ChatDrafts {
    /// Start a draft.
    ///
    /// The empty draft a composer writes in, made the first time the person
    /// types in that channel or thread.
    #[action(housekeeping)]
    pub fn start_draft(
        &mut self,
        #[param("The channel, or the channel and thread, it is written in")] key: String,
    ) -> Result<(), Refusal> {
        if key.is_empty() || key.len() > 512 {
            return Err(Refusal::new("a draft is kept under its channel or thread"));
        }
        self.drafts.entry(key).or_default();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ui::tilo_resource::{Invoke, Replica};

    use super::*;

    #[test]
    fn a_draft_is_rich_text_edited_at_its_key() {
        let mut ada = Replica::<ChatDrafts>::new().as_author("u_ada");
        let append = ChatDrafts::paths()
            .drafts
            .key("c_general")
            .perform(&tilo_document::RichTextAction::AppendParagraph {
                text: "half a thought".into(),
            })
            .unwrap();
        ada.apply(Invoke::edit(&append).unwrap()).unwrap();
        assert_eq!(
            crate::projection::draft_text(ada.doc(), "c_general").trim_start(),
            "half a thought"
        );
        // Typing into a draft never started starts it, holding just what
        // was typed; the other drafts stay as they were.
        let elsewhere = ChatDrafts::paths()
            .drafts
            .key("c_random")
            .perform(&tilo_document::RichTextAction::AppendParagraph {
                text: "an aside".into(),
            })
            .unwrap();
        ada.apply(Invoke::edit(&elsewhere).unwrap()).unwrap();
        assert_eq!(
            crate::projection::draft_text(ada.doc(), "c_random").trim_start(),
            "an aside"
        );
        assert_eq!(
            crate::projection::draft_text(ada.doc(), "c_general").trim_start(),
            "half a thought"
        );
        assert_eq!(ada.state().unwrap().drafts.len(), 2);
    }
}

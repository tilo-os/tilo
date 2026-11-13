//! What is being written and has not been sent: a record of Chat's beside
//! the conversation, kept in the workspace Chat is open in.
//!
//! A draft is never part of the conversation's record: keystrokes never
//! spend the conversation's change budget. Sending a message is what puts
//! words in the conversation.

use std::collections::BTreeMap;

use tilo_document::RichText;
use ui::tilo_resource::items::item_resource_id;
use ui::tilo_resource::search_index::{IndexKind, IndexPerson};
use ui::tilo_resource::{self, Demand, Doc, Id, Model, ModelError, Refusal, SearchEntry};

use crate::record::Chat;

/// The person's unsent messages, by channel or thread.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ChatDrafts {
    /// Each draft under its channel id, or `channel::t::message` for a reply
    /// in that message's thread.
    pub drafts: BTreeMap<String, RichText>,
}

#[ui::tilo_resource::resource(
    kind = "chat.drafts",
    params = drafts,
    search = ChatDrafts::search,
    load = ChatDrafts::load_drafts,
)]
impl ChatDrafts {
    /// Start a draft.
    ///
    /// The empty draft a composer writes in, made the first time the person
    /// types in that channel or thread.
    #[action(housekeeping, audience = "agents")]
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

    /// What search finds in the person's drafts: the words they have not
    /// sent, opening the conversation they are writing in.
    ///
    /// The record is theirs, not the workspace's, and it is read like any
    /// other record: a device that never opened Chat finds the draft in it,
    /// and one they send stops being found with it.
    #[must_use]
    pub fn search(&self) -> Vec<SearchEntry> {
        self.drafts
            .iter()
            .filter_map(|(key, draft)| {
                // A thread's draft is keyed `channel::t::message`; the
                // channel's row opens the conversation it is written in.
                let channel = key.split_once("::").map_or(key.as_str(), |(at, _)| at);
                let line = line(&draft.text())?;
                let anchor =
                    item_resource_id::<Chat>("channels", &Id::from_raw(channel.to_owned()));
                let place = IndexPerson {
                    name: String::new(),
                    anchor: anchor.clone(),
                    identity: channel.to_owned(),
                    agent: false,
                };
                Some(
                    SearchEntry::new(line, "")
                        .anchored(anchor)
                        .of_kind(IndexKind::Message)
                        .with_icon("pencil")
                        .in_place(place),
                )
            })
            .collect()
    }

    /// The words of every draft, read from `doc`: a draft is nothing but what
    /// the person typed, so a read of the record reads them however little of
    /// it the reader asked for — its order says nothing without them.
    fn load_drafts(&mut self, doc: &Doc, _demand: &Demand) -> Result<(), ModelError> {
        let whole = Demand::range(0..usize::MAX);
        for draft in self.drafts.values_mut() {
            draft.demand(doc, &whole)?;
        }
        Ok(())
    }
}

/// The line a conversation's row carries for unsent words ("Draft: see you at
/// 8"): the toolkit's, the same one Messages shows under a person, so a draft
/// is found the same way in both apps.
pub(crate) fn line(text: &str) -> Option<String> {
    ui::widget::conversation::draft::line(text)
}

#[cfg(test)]
mod tests {
    use ui::tilo_resource::{Invoke, Replica};

    use super::*;

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

    /// A draft is found by the words that were typed, opening the conversation it is written in.
    fn the_words_not_yet_sent_are_found() {
        let mut ada = Replica::<ChatDrafts>::new().as_author("u_ada");
        let append = ChatDrafts::paths()
            .drafts
            .key("c_general")
            .perform(&tilo_document::RichTextAction::AppendParagraph {
                text: "see you at 8".into(),
            })
            .unwrap();
        ada.apply(Invoke::edit(&append).unwrap()).unwrap();

        let entries = ada.live().expect("the draft reads").search();
        assert_eq!(entries.len(), 1, "one draft, one entry: {entries:?}");
        assert!(
            entries[0].title.ends_with("see you at 8"),
            "{}",
            entries[0].title
        );
        assert_eq!(
            entries[0].anchor.as_deref(),
            Some("chat.workspace/channels/c_general"),
            "the conversation it is written in"
        );
    }

    /// A person's drafts: edited apart, and found by search.
    /// Each case was a test of its own, on a fixture of its own; a failure
    /// names the cases that failed.
    #[test]
    fn a_draft_is_edited_at_its_key_and_found_by_its_words() {
        crate::testing::cases!(
            a_draft_is_rich_text_edited_at_its_key,
            the_words_not_yet_sent_are_found,
        );
    }
}

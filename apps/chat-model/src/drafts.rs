//! What is being written and has not been sent: a resource of Chat's beside
//! the conversation, kept in the workspace Chat is open in.
//!
//! A draft is never part of the conversation's resource: keystrokes never
//! become changes of the conversation. Sending a message is what puts words
//! in the conversation.

use std::collections::BTreeMap;

use tilo_document::RichText;
use tilo_resource::items::item_resource_id;
use tilo_resource::search_index::{IndexKind, IndexPerson};
use tilo_resource::{self, Demand, Doc, Id, Model, ModelError, Refusal, SearchEntry};

use crate::chat::Chat;

/// The person's unsent messages, by channel or thread.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ChatDrafts {
    /// Each draft under its channel id, or `channel::t::message` for a reply
    /// in that message's thread.
    pub drafts: BTreeMap<String, RichText>,
}

#[tilo_resource::resource(
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
    /// The resource is theirs, not the workspace's, and it is read like any
    /// other resource: a device that never opened Chat finds the draft in it,
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
    /// the person typed, so a read of the resource reads them however little of
    /// it the reader asked for — its order says nothing without them.
    fn load_drafts(&mut self, doc: &Doc, _demand: &Demand) -> Result<(), ModelError> {
        let whole = Demand::range(0..usize::MAX);
        for draft in self.drafts.values_mut() {
            draft.demand(doc, &whole)?;
        }
        Ok(())
    }
}

/// The words of an unsent draft a conversation's row carries: the first of
/// them, on one line.
pub fn line(text: &str) -> Option<String> {
    preview(text)
}

/// The most of a draft the row shows.
const DRAFT_PREVIEW_CHARS: usize = 60;

/// The first words of a draft, on one line.
fn preview(body: &str) -> Option<String> {
    let first = body.lines().find(|line| !line.trim().is_empty())?;
    let folded = first.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(elide(&folded, DRAFT_PREVIEW_CHARS))
}

/// `text` cut to at most `chars` characters, with an ellipsis when anything
/// was dropped.
fn elide(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => format!("{}…", text.get(..at).unwrap_or(text)),
        None => text.to_owned(),
    }
}

//! A draft is kept, and shown where the conversation is listed.
//!
//! What a person was writing and has not sent is theirs: it survives the window
//! closing, it is there when they come back to that channel or that person, and
//! the list of conversations says so before they open it — a draft you cannot
//! find is a draft you have lost.
//!
//! Two pieces here. [`DraftBook`] is the rule: a draft is per conversation, a
//! blank draft is no draft, and sending removes the draft only while it is still
//! the one that was sent (a word typed while the send was in flight is not
//! thrown away). [`preview`] is the line a listing draws: the first line of the
//! body, its runs of whitespace folded to one space, elided at a grapheme
//! boundary, prefixed "Draft: ".
//!
//! Persistence is the app's: a draft belongs on the person's own record
//! (their room, their other devices), never in the shared conversation. This
//! module holds the rule set, not the storage.

use std::collections::BTreeMap;

/// How much of a draft a listing shows before eliding.
pub const DRAFT_PREVIEW_CHARS: usize = 60;

/// The first line of a draft body, whitespace folded and elided: what a
/// conversation list shows under a name. `None` when there is nothing to say
/// (a body of spaces is not a draft).
#[must_use]
pub fn preview(body: &str) -> Option<String> {
    let first = body.lines().find(|line| !line.trim().is_empty())?;
    let folded = first.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(elide(&folded, DRAFT_PREVIEW_CHARS))
}

/// The line a conversation's row carries for a draft: "Draft: see you at 8".
/// `None` when the body has nothing in it.
#[must_use]
pub fn line(body: &str) -> Option<String> {
    let preview = preview(body)?;
    Some(format!("{}: {preview}", super::t("conversation-draft")))
}

/// `text` cut to at most `chars` characters on a grapheme boundary, with an
/// ellipsis when anything was dropped.
fn elide(text: &str, chars: usize) -> String {
    let mut count = 0;
    let mut cut = None;
    for (index, _) in crate::text_break::grapheme_indices(text) {
        if count == chars {
            cut = Some(index);
            break;
        }
        count = count.saturating_add(1);
    }
    match cut {
        Some(at) => format!("{}…", text.get(..at).unwrap_or(text)),
        None => text.to_owned(),
    }
}

/// The drafts a person has, by conversation key.
///
/// The app keeps this in step with the person's record; the rules that make a
/// draft a draft live here so both apps agree about them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftBook {
    drafts: BTreeMap<String, String>,
}

impl DraftBook {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The draft under `conversation`, when it has anything in it.
    #[must_use]
    pub fn get(&self, conversation: &str) -> Option<&str> {
        self.drafts
            .get(conversation)
            .map(String::as_str)
            .filter(|body| !body.trim().is_empty())
    }

    /// Whether `conversation` holds a draft.
    #[must_use]
    pub fn has(&self, conversation: &str) -> bool {
        self.get(conversation).is_some()
    }

    /// The row line for `conversation`'s draft, when it has one.
    #[must_use]
    pub fn line(&self, conversation: &str) -> Option<String> {
        self.get(conversation).and_then(line)
    }

    /// What the person has just typed into `conversation`: the draft becomes
    /// this text, or stops being a draft when it is blank.
    pub fn edit(&mut self, conversation: &str, body: &str) {
        if body.trim().is_empty() {
            self.drafts.remove(conversation);
            return;
        }
        self.drafts.insert(conversation.to_owned(), body.to_owned());
    }

    /// Take the draft `conversation` had, for sending. `None` when there was
    /// nothing to send.
    pub fn take(&mut self, conversation: &str) -> Option<String> {
        let body = self.drafts.remove(conversation)?;
        (!body.trim().is_empty()).then_some(body)
    }

    /// The draft that was sent, once the send is done: removed only while it is
    /// still exactly what was sent. A word typed while the send was in flight
    /// is the next message, not something to throw away.
    pub fn sent(&mut self, conversation: &str, sent: &str) {
        if self
            .drafts
            .get(conversation)
            .is_some_and(|held| held == sent)
        {
            self.drafts.remove(conversation);
        }
    }

    /// Every conversation with a draft, in key order.
    pub fn conversations(&self) -> impl Iterator<Item = &str> {
        self.drafts
            .iter()
            .filter(|(_, body)| !body.trim().is_empty())
            .map(|(conversation, _)| conversation.as_str())
    }

    /// How many conversations hold a draft.
    #[must_use]
    pub fn len(&self) -> usize {
        self.conversations().count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every draft as `(conversation, body)`, for writing back to the person's
    /// record.
    #[must_use]
    pub fn entries(&self) -> Vec<(&str, &str)> {
        self.drafts
            .iter()
            .filter(|(_, body)| !body.trim().is_empty())
            .map(|(conversation, body)| (conversation.as_str(), body.as_str()))
            .collect()
    }
}

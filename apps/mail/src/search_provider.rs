//! Mail as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: Mail declares its
//! searchable type by implementing [`SearchProvider`] over a snapshot of the
//! in-memory mailbox, so a message is findable from the unified Ctrl+K palette
//! by subject, sender or body. Activating a row hands back an opaque
//! `action_id` the app resolves to a [`MessageId`], selecting its folder and
//! opening it.
//!
//! The provider is deliberately **library-scoped and synchronous**: it delegates
//! to [`Mailbox::search`], which walks only the messages already loaded in
//! memory (excluding Trash) — never the IMAP engine. A Ctrl+K query therefore
//! never blocks on the network; the live server search stays in Mail's own
//! search field, driven off-thread by the engine worker.

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};

use crate::model::{Mailbox, MessageId};

/// The kind label (and result-group header) Mail produces.
pub const MAIL_KIND: &str = "Mail";

/// The `action_id` prefix. A chosen row's `action_id` is `mail.msg:<uuid>`;
/// [`parse_action_id`] turns it back into a [`MessageId`].
const ACTION_PREFIX: &str = "mail.msg:";

/// Shown for a message whose subject is blank.
const NO_SUBJECT: &str = "(no subject)";

/// A [`SearchProvider`] over a snapshot of the mailbox. Rebuild it (cheap —
/// the mailbox is already in memory) whenever the search surface opens.
pub struct MailSearchProvider {
    mailbox: Mailbox,
}

impl MailSearchProvider {
    /// Snapshot the current mailbox into a provider.
    pub fn new(mailbox: &Mailbox) -> Self {
        Self {
            mailbox: mailbox.clone(),
        }
    }

    /// The `action_id` for a message — what a chosen row hands back.
    pub fn action_id(id: MessageId) -> String {
        format!("{ACTION_PREFIX}{id}")
    }

    /// Resolve an `action_id` produced by this provider back to a
    /// [`MessageId`]. Returns `None` for an id this provider did not emit.
    pub fn parse_action_id(action_id: &str) -> Option<MessageId> {
        action_id
            .strip_prefix(ACTION_PREFIX)
            .and_then(|rest| rest.parse::<MessageId>().ok())
    }
}

impl SearchProvider for MailSearchProvider {
    fn name(&self) -> &str {
        MAIL_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        // Delegate to the mailbox's own case-insensitive full-text search
        // (subject / sender / body), whole-mailbox scope minus Trash. It
        // already returns an empty set for a blank query, so the palette never
        // dumps the whole inbox.
        self.mailbox
            .search(query, None)
            .into_iter()
            .map(|m| {
                let title = if m.subject.trim().is_empty() {
                    NO_SUBJECT.to_string()
                } else {
                    m.subject.clone()
                };
                SearchResult::new(MAIL_KIND, title, Self::action_id(m.id))
                    .with_subtitle(m.from.display())
                    .with_icon(Icon::Mail)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchRegistry;

    fn sample() -> Mailbox {
        crate::sample::sample_mailbox()
    }

    #[test]
    fn provider_finds_a_message_by_subject_or_sender() {
        let mb = sample();
        let provider = MailSearchProvider::new(&mb);
        // Take a real subject word from the seeded corpus and prove it surfaces.
        let some = mb
            .messages
            .values()
            .next()
            .expect("sample mailbox is populated")
            .clone();
        let word = some
            .subject
            .split_whitespace()
            .next()
            .unwrap_or(&some.subject);
        let hits = provider.search(word);
        assert!(
            !hits.is_empty(),
            "a real subject word surfaces at least its message"
        );
        assert!(hits.iter().all(|r| r.kind == MAIL_KIND));
        assert!(
            hits.iter().all(|r| r.subtitle.is_some()),
            "every row shows a sender"
        );
    }

    #[test]
    fn blank_query_returns_nothing() {
        let provider = MailSearchProvider::new(&sample());
        assert!(
            provider.search("   ").is_empty(),
            "no whole-inbox dump into the palette"
        );
    }

    #[test]
    fn action_id_round_trips_to_a_stored_message() {
        let mb = sample();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(MailSearchProvider::new(&mb)));

        let some = mb.messages.values().next().unwrap().clone();
        let word = some
            .subject
            .split_whitespace()
            .next()
            .unwrap_or(&some.subject);
        let results = registry.query(word);
        let hit = results.first().expect("a result for a real subject word");
        let id = MailSearchProvider::parse_action_id(&hit.action_id)
            .expect("action_id parses back to a MessageId");
        assert!(
            mb.get(id).is_some(),
            "the id resolves to a real stored message"
        );
    }

    #[test]
    fn parse_action_id_rejects_foreign_ids() {
        assert!(MailSearchProvider::parse_action_id("file:/etc/passwd").is_none());
        assert!(MailSearchProvider::parse_action_id("mail.msg:not-a-uuid").is_none());
    }

    #[test]
    fn widget_hooks_find_then_open_the_message() {
        // Drive the exact pair the shell palette calls on the real MailApp.
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::MailApp::new_sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        // Find a real message through the app's own provider.
        let (action_id, id) = {
            let providers = app.search_providers();
            assert!(!providers.is_empty(), "mail publishes a search provider");
            // Query a subject word that exists in the sample corpus.
            let mb = crate::sample::sample_mailbox();
            let some = mb.messages.values().next().unwrap().clone();
            let word = some
                .subject
                .split_whitespace()
                .next()
                .unwrap_or(&some.subject)
                .to_string();
            let hits = providers[0].search(&word);
            let hit = hits
                .first()
                .expect("a hit for a seeded subject word")
                .clone();
            let id = MailSearchProvider::parse_action_id(&hit.action_id).unwrap();
            (hit.action_id, id)
        };

        app.activate_search_result(&action_id, &mut doc, &ROOT);
        assert_eq!(
            app.selected_message(),
            Some(id),
            "activation opened the chosen message"
        );
    }
}

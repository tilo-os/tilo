//! Mail as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: Mail declares its
//! searchable type by implementing [`SearchProvider`] over a snapshot of the
//! in-memory mailbox, so a message is findable from the unified Ctrl+K palette
//! by subject, sender or body. Messages and folders name stable exported
//! resource `View` operations, resolved by the shared search host.
//!
//! The provider is deliberately **library-scoped and synchronous**: it delegates
//! to [`Mailbox::search`], which walks only the messages already loaded in
//! memory (excluding Trash) — never the IMAP engine. A Ctrl+K query therefore
//! never blocks on the network; the live server search stays in Mail's own
//! search field, driven off-thread by the engine worker.

use qos_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
};

use crate::model::{Folder, Mailbox};

/// The kind label (and result-group header) Mail produces.
pub const MAIL_KIND: &str = "Mail";
pub const FOLDER_KIND: &str = "Mail folders";

/// Shown for a message whose subject is blank.
const NO_SUBJECT: &str = "(no subject)";

/// A [`SearchProvider`] over a snapshot of the mailbox. Rebuild it (cheap —
/// the mailbox is already in memory) whenever the search surface opens.
pub struct MailSearchProvider {
    mailbox: Mailbox,
    /// Stable newest-first traversal prepared once when the provider snapshot
    /// is built, so a limited query can stop as soon as its budget is full.
    search_order: Vec<crate::model::MessageId>,
}

impl MailSearchProvider {
    /// Snapshot the current mailbox into a provider.
    pub fn new(mailbox: &Mailbox) -> Self {
        let mut search_order = mailbox
            .order
            .iter()
            .rev()
            .take(DEFAULT_SEARCH_INDEX_LIMIT)
            .copied()
            .collect::<Vec<_>>();
        search_order.sort_by_key(|id| {
            std::cmp::Reverse(mailbox.get(*id).map_or(0, |message| message.timestamp_min))
        });
        let mut bounded = Mailbox::new(mailbox.account.clone());
        for id in &search_order {
            if let Some(message) = mailbox.get(*id) {
                bounded.insert(message.clone());
            }
        }
        Self {
            mailbox: bounded,
            search_order,
        }
    }

    fn message_address(id: crate::model::MessageId) -> ActionAddress {
        ActionAddress::new(format!("mail.message:{id}"), "open")
    }

    fn folder_address(folder: Folder) -> ActionAddress {
        ActionAddress::new(format!("mail.folder:{}", folder.as_key()), "open")
    }

    fn search_up_to(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let query = query.trim().to_ascii_lowercase();
        if query.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut results = Vec::with_capacity(limit.min(32));
        for id in &self.search_order {
            let Some(message) = self.mailbox.get(*id) else {
                continue;
            };
            if message.folder == Folder::Trash
                || !(message.subject.to_ascii_lowercase().contains(&query)
                    || message.from.display().to_ascii_lowercase().contains(&query)
                    || message.from.email.to_ascii_lowercase().contains(&query)
                    || message.body.to_ascii_lowercase().contains(&query))
            {
                continue;
            }
            let title = if message.subject.trim().is_empty() {
                NO_SUBJECT.to_string()
            } else {
                message.subject.clone()
            };
            results.push(
                SearchResult::for_action(MAIL_KIND, title, Self::message_address(message.id))
                    .with_subtitle(message.from.display())
                    .with_icon(Icon::Mail),
            );
            if results.len() == limit {
                return results;
            }
        }
        for folder in Folder::ALL {
            if folder.title().to_ascii_lowercase().contains(&query) {
                results.push(
                    SearchResult::for_action(
                        FOLDER_KIND,
                        folder.title(),
                        Self::folder_address(folder),
                    )
                    .with_subtitle("Mail folder")
                    .with_icon(folder.icon()),
                );
                if results.len() == limit {
                    break;
                }
            }
        }
        results
    }
}

impl SearchProvider for MailSearchProvider {
    fn name(&self) -> &str {
        MAIL_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_up_to(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.search_up_to(query, limit.min(DEFAULT_SEARCH_RESULT_LIMIT))
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<qos_actions::Resource> {
        if let Some(id) = address.resource.strip_prefix("mail.message:") {
            let id = id.parse().ok()?;
            let message = self.mailbox.get(id)?;
            return Some(crate::actions::message_resource(
                &crate::actions::MessageRow {
                    id: message.id.to_string(),
                    subject: message.subject.clone(),
                    thread_subject: message.thread_subject(),
                    folder: message.folder,
                    read: message.read,
                    starred: message.starred,
                    labels: message.labels.clone(),
                    attachments: message
                        .attachments
                        .iter()
                        .map(|attachment| crate::actions::AttachmentRow {
                            filename: attachment.filename.clone(),
                            size: attachment.size,
                            downloadable: attachment.is_downloadable(),
                        })
                        .collect(),
                },
            ));
        }
        let folder = Folder::from_key(address.resource.strip_prefix("mail.folder:")?)?;
        // Mail's global search snapshot has no Automerge document, so it cannot
        // know whether a saved draft exists. The stable folder/action contract
        // remains identical; the draft-only child materializes when the app's
        // full resource provider is queried.
        Some(crate::actions::system_folder_resource(folder, false))
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
    fn limited_search_honours_the_materialisation_budget() {
        let provider = MailSearchProvider::new(&sample());
        let query = provider
            .mailbox
            .messages
            .values()
            .next()
            .and_then(|message| message.subject.split_whitespace().next())
            .unwrap_or("mail");
        assert!(provider.search_limited(query, 1).len() <= 1);
        assert!(provider.search_limited(query, 0).is_empty());
    }

    #[test]
    fn every_hit_uses_a_canonical_resource_action() {
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
        assert!(hit.action_address().is_some());
        assert!(
            MailSearchProvider::new(&mb)
                .search("Inbox")
                .iter()
                .any(|hit| { hit.kind == FOLDER_KIND && hit.action_address().is_some() })
        );
    }

    #[test]
    fn folder_hits_materialize_their_own_canonical_open_resource() {
        let provider = MailSearchProvider::new(&sample());
        let hit = provider
            .search("Archive")
            .into_iter()
            .find(|hit| hit.kind == FOLDER_KIND)
            .expect("Archive folder is searchable");
        let address = hit.action_address().expect("canonical folder address");
        let resource = provider
            .resolve_action_resource(&address)
            .expect("a folder search hit resolves without requiring the full tree");
        assert_eq!(resource.id.as_str(), address.resource);
        assert!(resource.actions.iter().any(|action| action.id == "open"));
    }

    #[test]
    fn search_address_resolves_and_navigates_exactly() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::MailApp::new_sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        // Find a real message through the app's own provider.
        let (address, id) = {
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
            let hit = hits.into_iter().next().expect("a seeded message is a hit");
            let address = hit.action_address().expect("canonical address");
            let id = address
                .resource
                .strip_prefix("mail.message:")
                .expect("message resource id")
                .parse()
                .expect("valid message id");
            (address, id)
        };

        let resource = <crate::app::MailApp as qos_actions::ResourceProvider>::resource(
            &app,
            &doc,
            &address.resource,
        )
        .expect("search address resolves lazily");
        let encoded = qos_actions::door::encode(&[resource], &address, None).unwrap();
        let heads_before_navigation = doc.get_heads();
        assert!(
            app.invoke_resource_operation(
                qos_actions::OperationKind::View,
                &encoded.type_id,
                &encoded.payload,
                &doc,
                &ROOT,
            )
            .unwrap()
        );
        assert_eq!(
            app.selected_message(),
            Some(id),
            "activation opened the chosen message"
        );
        assert_eq!(
            doc.get_heads(),
            heads_before_navigation,
            "a View operation does not mutate collaborative mail state"
        );

        let folder_hit = app.search_providers()[0]
            .search("Archive")
            .into_iter()
            .find(|hit| hit.kind == FOLDER_KIND)
            .expect("Archive folder is searchable");
        let folder_address = folder_hit
            .action_address()
            .expect("canonical folder address");
        let resources = app.resources(&doc);
        let encoded = qos_actions::door::encode(&resources, &folder_address, None)
            .expect("folder address resolves against Mail's exported tree");
        assert!(
            app.invoke_resource_operation(
                qos_actions::OperationKind::View,
                &encoded.type_id,
                &encoded.payload,
                &doc,
                &ROOT,
            )
            .unwrap()
        );
        assert_eq!(app.mail_resource_ctx(&doc).current_folder, Folder::Archive);
        assert_eq!(doc.get_heads(), heads_before_navigation);
    }
}

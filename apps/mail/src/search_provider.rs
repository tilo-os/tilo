//! Mail as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: Mail declares its
//! searchable type by implementing [`SearchProvider`] over a snapshot of the
//! in-memory mailbox, so a message is findable from the unified Ctrl+K palette
//! by subject, sender or body. Messages and folders name stable exported
//! resource `View` operations, resolved by the shared search host.
//!
//! The provider is deliberately **library-scoped and synchronous**: it indexes
//! every message already loaded in memory (excluding Trash), never the IMAP
//! engine. A Ctrl+K query therefore never blocks on the network; the live
//! server search stays in Mail's own search field, driven off-thread by the
//! engine worker.

use std::collections::{HashMap, HashSet};

use qos_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult};

use crate::model::{Folder, Mailbox};

/// The kind label (and result-group header) Mail produces.
pub const MAIL_KIND: &str = "Mail";
pub const FOLDER_KIND: &str = "Mail folders";

/// Shown for a message whose subject is blank.
const NO_SUBJECT: &str = "(no subject)";

/// One fully-indexed mail resource plus the (lowercased) corpus that search
/// needs. It deliberately does not retain a second [`Mailbox`]: duplicating a
/// large attachment-heavy mailbox just to search it was costly and the old
/// 4,096-message cap made older mail undiscoverable.
#[derive(Debug, Clone)]
struct IndexedMessage {
    id: crate::model::MessageId,
    row: crate::actions::MessageRow,
    sender: String,
    /// Subject, sender names/addresses, body and labels in the same substring
    /// matching domain as the in-app search field.
    haystack: String,
}

/// A [`SearchProvider`] over every already-loaded message. Rebuild it when the
/// search surface opens; index storage grows linearly with loaded mail and
/// never contacts IMAP.
pub struct MailSearchProvider {
    /// Newest-first, excluding Trash. Ordering here makes bounded result
    /// materialisation useful instead of arbitrary `HashMap` order.
    records: Vec<IndexedMessage>,
    /// A lossless byte-bigram candidate index. It supports arbitrary
    /// Unicode-case-folded *substrings* (not merely whole tokens), including a
    /// match in an old body. Postings retain record order.
    bigrams: HashMap<u16, Vec<usize>>,
    /// One-byte queries use a separate lossless index. Keeping it distinct
    /// avoids inventing an exact-token shortcut for short searches.
    bytes: [Vec<usize>; 256],
}

/// We never inspect unbounded message bodies in an interactive palette query.
/// The index chooses the rarest query gram first, and this secondary guard
/// covers adversarial/common grams while preserving recent, exact substring
/// matches. The result limit is itself bounded by the shared search contract.
const MAX_CANDIDATE_INSPECTIONS: usize = DEFAULT_SEARCH_RESULT_LIMIT * 8;

impl MailSearchProvider {
    /// Snapshot the current mailbox into a provider.
    pub fn new(mailbox: &Mailbox) -> Self {
        let mut messages = mailbox
            .messages
            .values()
            .filter(|message| message.folder != Folder::Trash)
            .collect::<Vec<_>>();
        messages.sort_by_key(|message| std::cmp::Reverse(message.timestamp_min));
        let records = messages
            .into_iter()
            .map(|message| IndexedMessage {
                id: message.id,
                row: crate::actions::MessageRow {
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
                sender: message.from.display(),
                haystack: format!(
                    "{}\n{}\n{}\n{}\n{}",
                    message.subject,
                    message.from.display(),
                    message.from.email,
                    message.body,
                    message.labels.join(" ")
                )
                .to_lowercase(),
            })
            .collect::<Vec<_>>();
        let mut provider = Self {
            records,
            bigrams: HashMap::new(),
            bytes: std::array::from_fn(|_| Vec::new()),
        };
        for (index, record) in provider.records.iter().enumerate() {
            // De-duplicate per record so a repeated character/gram cannot
            // consume the candidate budget before another matching message.
            let mut seen_bytes = [false; 256];
            let mut seen_bigrams = HashSet::new();
            for byte in record.haystack.bytes() {
                let byte_index = usize::from(byte);
                let Some(seen) = seen_bytes.get_mut(byte_index) else {
                    continue;
                };
                if !*seen {
                    if let Some(postings) = provider.bytes.get_mut(byte_index) {
                        postings.push(index);
                    }
                    *seen = true;
                }
            }
            for window in record.haystack.as_bytes().windows(2) {
                let Some(gram) = bigram(window) else {
                    continue;
                };
                if seen_bigrams.insert(gram) {
                    provider.bigrams.entry(gram).or_default().push(index);
                }
            }
        }
        provider
    }

    fn message_address(id: crate::model::MessageId) -> ActionAddress {
        ActionAddress::new(format!("mail.message:{id}"), "open")
    }

    fn folder_address(folder: Folder) -> ActionAddress {
        ActionAddress::new(format!("mail.folder:{}", folder.as_key()), "open")
    }

    fn search_up_to(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let query = query.trim().to_lowercase();
        // Palette browse is intentionally explicit: an empty query must not
        // materialise the mailbox's newest messages. That is both less
        // distracting than a surprise inbox dump and keeps opening Ctrl+K
        // bounded for a very large account.
        if query.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut results = Vec::with_capacity(limit.min(32));
        let Some(candidates) = self.candidates(&query) else {
            return Self::folder_results(&query, results, limit);
        };
        let mut inspected = 0;
        for &index in candidates {
            if inspected == MAX_CANDIDATE_INSPECTIONS || results.len() == limit {
                break;
            }
            let Some(message) = self.records.get(index) else {
                continue;
            };
            inspected += 1;
            if !message.haystack.contains(&query) {
                continue;
            }
            let title = if message.row.subject.trim().is_empty() {
                NO_SUBJECT.to_string()
            } else {
                message.row.subject.clone()
            };
            results.push(
                SearchResult::for_action(MAIL_KIND, title, Self::message_address(message.id))
                    .with_subtitle(message.sender.clone())
                    .with_icon(Icon::Mail),
            );
        }
        Self::folder_results(&query, results, limit)
    }

    /// Choose the rarest lossless query gram. It bounds costly body substring
    /// inspections without using exact-token matching: every real substring
    /// match is present in the selected posting list.
    fn candidates(&self, query: &str) -> Option<&[usize]> {
        let bytes = query.as_bytes();
        if bytes.len() == 1 {
            return bytes
                .first()
                .and_then(|byte| self.bytes.get(usize::from(*byte)))
                .map(Vec::as_slice);
        }
        let mut rarest: Option<&[usize]> = None;
        for window in bytes.windows(2) {
            let gram = bigram(window)?;
            // A real substring necessarily includes every one of its
            // bigrams. Do not quietly drop a missing gram and widen the
            // candidate set to a common partial match.
            let postings = self.bigrams.get(&gram)?.as_slice();
            if rarest.is_none_or(|current| postings.len() < current.len()) {
                rarest = Some(postings);
            }
        }
        rarest
    }

    fn folder_results(
        query: &str,
        mut results: Vec<SearchResult>,
        limit: usize,
    ) -> Vec<SearchResult> {
        for folder in Folder::ALL {
            if folder.title().to_lowercase().contains(query) {
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

/// Read one bigram without indexing into an arbitrary slice. `windows(2)` is
/// the caller today, but the total helper keeps the index construction and
/// query path robust if either evolves.
fn bigram(bytes: &[u8]) -> Option<u16> {
    let [first, second] = bytes else {
        return None;
    };
    Some(u16::from_be_bytes([*first, *second]))
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
            let message = self.records.iter().find(|message| message.row.id == id)?;
            return Some(crate::actions::message_resource(&message.row));
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
            "opening Ctrl+K is not an implicit whole-inbox browse"
        );
    }

    #[test]
    fn unicode_case_folded_infixes_remain_searchable() {
        let mut mailbox = sample();
        let mut message = mailbox
            .messages
            .values()
            .next()
            .expect("seeded message")
            .clone();
        message.id = uuid::Uuid::new_v4();
        message.folder = Folder::Inbox;
        message.subject = "Málaga release notes".into();
        message.body.clear();
        message.attachments.clear();
        mailbox.insert(message.clone());
        let provider = MailSearchProvider::new(&mailbox);
        assert!(
            provider
                .search("ÁLA")
                .iter()
                .any(|hit| hit.title == message.subject),
            "Unicode case folding keeps a mixed-case substring discoverable"
        );
    }

    #[test]
    fn an_absent_query_gram_short_circuits_before_body_inspection() {
        let provider = MailSearchProvider::new(&sample());
        assert!(
            provider.candidates("\0\0").is_none(),
            "a query gram absent from the index has no possible substring match"
        );
    }

    #[test]
    fn limited_search_honours_the_materialisation_budget() {
        let mailbox = sample();
        let query = mailbox
            .messages
            .values()
            .next()
            .and_then(|message| message.subject.split_whitespace().next())
            .unwrap_or("mail");
        let provider = MailSearchProvider::new(&mailbox);
        assert!(provider.search_limited(query, 1).len() <= 1);
        assert!(provider.search_limited(query, 0).is_empty());
    }

    #[test]
    fn complete_index_finds_an_old_substring_past_the_former_snapshot_cap() {
        let mut mailbox = sample();
        let mut target = mailbox
            .messages
            .values()
            .next()
            .expect("seeded message")
            .clone();
        target.id = uuid::Uuid::new_v4();
        target.folder = Folder::Inbox;
        target.subject = "SearchIndexBoundaryFragment".into();
        target.body.clear();
        target.attachments.clear();
        // Insert the target first, then enough later records that the old
        // newest-4,096 snapshot would have silently hidden it. The query is an
        // infix, not a whole-token lookup.
        mailbox.insert(target.clone());
        for timestamp in 0..4_200_i64 {
            let mut filler = target.clone();
            filler.id = uuid::Uuid::new_v4();
            filler.subject = format!("Routine status {timestamp}");
            filler.timestamp_min = timestamp;
            mailbox.insert(filler);
        }
        let provider = MailSearchProvider::new(&mailbox);
        assert!(
            provider.records.len() > 4_096,
            "the whole corpus is indexed"
        );
        let hits = provider.search("aryfrag");
        assert!(hits.iter().any(|hit| hit.title == target.subject));
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

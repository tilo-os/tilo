//! Mail as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: Mail declares its
//! searchable type by implementing [`SearchProvider`] over a retained catalogue
//! of the in-memory mailbox, so a message is findable from the unified Ctrl+K
//! palette by subject, sender or body. Messages and folders name stable exported
//! resource `View` operations, resolved by the shared search host.
//!
//! The provider is deliberately **library-scoped and synchronous**: it indexes
//! every message already loaded in memory (excluding Trash), never the IMAP
//! engine. A Ctrl+K query therefore never blocks on the network; the live
//! server search stays in Mail's own search field, driven off-thread by the
//! engine worker.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use qos_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor,
    SearchPage, SearchProvider, SearchResult,
};

use crate::model::{Folder, Mailbox};

/// The kind label (and result-group header) Mail produces.
pub const MAIL_KIND: &str = "Mail";
pub const FOLDER_KIND: &str = "Mail folders";

/// Shown for a message whose subject is blank.
const NO_SUBJECT: &str = "(no subject)";

/// Retained state shared by every palette provider cloned from one Mail app.
/// `known_ids` makes removal explicit: an address from an already-open palette
/// is refused once a later mailbox revision removed or trashed that message.
#[derive(Debug, Default)]
struct CatalogState {
    mailbox_revision: Option<u64>,
    known_ids: HashSet<String>,
}

/// A complete, revision-aware catalogue of already-loaded mail.
///
/// The app retains one instance and cheaply clones this handle when Ctrl+K
/// opens. `refresh` is O(1) until the mailbox reports a new revision; only
/// then does it reconcile the complete corpus. Queries delegate to the shared
/// bounded `SearchCatalog`, so keystrokes never walk message bodies.
#[derive(Clone)]
pub struct MailSearchProvider {
    catalog: SearchCatalog,
    rows: Arc<RwLock<HashMap<String, crate::actions::MessageRow>>>,
    state: Arc<Mutex<CatalogState>>,
}

impl MailSearchProvider {
    /// Index every currently loaded message, then retain the catalogue for
    /// future palette openings and mailbox revisions.
    pub fn new(mailbox: &Mailbox) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(MAIL_KIND),
            rows: Arc::new(RwLock::new(HashMap::new())),
            state: Arc::new(Mutex::new(CatalogState::default())),
        };
        for folder in Folder::ALL {
            provider.index_folder(folder);
        }
        provider.refresh(mailbox);
        provider
    }

    fn message_address(id: crate::model::MessageId) -> ActionAddress {
        ActionAddress::new(format!("mail.message:{id}"), "open")
    }

    fn folder_address(folder: Folder) -> ActionAddress {
        ActionAddress::new(format!("mail.folder:{}", folder.as_key()), "open")
    }

    /// Reconcile the retained catalogue only when searchable mailbox state
    /// changed. This is intentionally callable through a shared provider so a
    /// palette opened just after an engine event sees the new revision without
    /// reconstructing its provider for every keypress.
    pub fn refresh(&self, mailbox: &Mailbox) {
        let revision = mailbox.search_revision();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.mailbox_revision == Some(revision) {
            return;
        }
        let mut visible = HashSet::new();
        let mut rows = self
            .rows
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for message in mailbox
            .messages
            .values()
            .filter(|message| message.folder != Folder::Trash)
        {
            let id = format!("mail.message:{}", message.id);
            visible.insert(id.clone());
            let row = crate::actions::message_row(message);
            self.index_message(message, &id);
            rows.insert(row.id.clone(), row);
        }
        for id in state.known_ids.difference(&visible) {
            self.catalog.remove(id);
            if let Some(message_id) = id.strip_prefix("mail.message:") {
                rows.remove(message_id);
            }
        }
        drop(rows);
        state.known_ids = visible;
        state.mailbox_revision = Some(revision);
    }

    fn index_message(&self, message: &crate::model::Message, resource_id: &str) {
        let result = SearchResult::for_action(
            MAIL_KIND,
            if message.subject.trim().is_empty() {
                NO_SUBJECT.to_string()
            } else {
                message.subject.clone()
            },
            Self::message_address(message.id),
        )
        .with_subtitle(message.from.display())
        .with_icon(Icon::Mail);
        let _ = self.catalog.upsert(
            resource_id,
            format!(
                "{}\n{}\n{}\n{}\n{}",
                message.subject,
                message.from.display(),
                message.from.email,
                message.body,
                message.labels.join(" ")
            ),
            result,
        );
    }

    fn index_folder(&self, folder: Folder) {
        let address = Self::folder_address(folder);
        let _ = self.catalog.upsert(
            address.resource.clone(),
            folder.title(),
            SearchResult::for_action(FOLDER_KIND, folder.title(), address)
                .with_subtitle("Mail folder")
                .with_icon(folder.icon()),
        );
    }

    fn search_up_to(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let query = query.trim();
        // Palette browse is intentionally explicit: an empty query must not
        // materialise the mailbox's newest messages. That is both less
        // distracting than a surprise inbox dump and keeps opening Ctrl+K
        // bounded for a very large account.
        if query.is_empty() || limit == 0 {
            return Vec::new();
        }
        self.catalog
            .query_page(
                query,
                None,
                DEFAULT_SEARCH_CANDIDATE_LIMIT,
                limit.min(DEFAULT_SEARCH_RESULT_LIMIT),
            )
            .results
    }
}

impl SearchProvider for MailSearchProvider {
    fn name(&self) -> &str {
        MAIL_KIND
    }

    fn action_directory_generation(&self) -> Option<u64> {
        Some(self.catalog.stats().generation)
    }

    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.catalog.cursor_is_current(cursor)
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_up_to(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.search_up_to(query, limit.min(DEFAULT_SEARCH_RESULT_LIMIT))
    }

    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        if query.trim().is_empty() {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        }
        self.catalog
            .query_page(query.trim(), cursor, work_budget, result_limit)
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<qos_actions::Resource> {
        if address.action != "open" {
            return None;
        }
        if let Some(id) = address.resource.strip_prefix("mail.message:") {
            // A retained provider shares this map with every open palette. If
            // a later mailbox revision removed or trashed the message, refuse
            // the old address instead of reviving stale read/star state.
            let row = self
                .rows
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(id)
                .cloned()?;
            return Some(crate::actions::message_resource(&row));
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
        let page = provider.search_page("\0\0", None, 8, 8);
        assert!(page.results.is_empty());
        assert_eq!(page.inspected, 0, "missing grams must avoid body work");
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
    fn complete_catalogue_finds_an_old_substring_past_the_former_snapshot_cap() {
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
        assert_eq!(
            provider.catalog.stats().records,
            mailbox
                .messages
                .values()
                .filter(|message| message.folder != Folder::Trash)
                .count()
                + Folder::ALL.len()
        );
        let hits = provider.search("aryfrag");
        assert!(hits.iter().any(|hit| hit.title == target.subject));
    }

    #[test]
    fn paged_search_bounds_inspection_and_reaches_every_broad_match() {
        let mut mailbox = Mailbox::default();
        let template = sample().messages.values().next().unwrap().clone();
        for index in 0..257_i64 {
            let mut message = template.clone();
            message.id = uuid::Uuid::new_v4();
            message.subject = format!("Common catalogue message {index}");
            message.folder = Folder::Inbox;
            mailbox.insert(message);
        }
        let provider = MailSearchProvider::new(&mailbox);
        let mut cursor = None;
        let mut seen = 0;
        loop {
            let page = provider.search_page("common", cursor.as_ref(), 17, 5);
            assert!(page.inspected <= 17, "each page has a hard work budget");
            assert!(page.results.len() <= 5, "rows are materialised per page");
            seen += page.results.len();
            let Some(next) = page.next else { break };
            cursor = Some(next);
        }
        assert_eq!(seen, 257, "pagination must not lose broad matches");
    }

    #[test]
    fn retained_catalogue_refuses_a_stale_message_address() {
        let mut mailbox = sample();
        let provider = MailSearchProvider::new(&mailbox);
        let message = mailbox
            .messages
            .values()
            .find(|message| message.folder != Folder::Trash)
            .unwrap()
            .clone();
        let address = MailSearchProvider::message_address(message.id);
        assert!(provider.resolve_action_resource(&address).is_some());

        mailbox.delete(message.id);
        provider.refresh(&mailbox);
        assert!(
            provider.resolve_action_resource(&address).is_none(),
            "a palette row from an earlier revision cannot revive trashed mail"
        );
    }

    #[test]
    fn refresh_without_a_new_revision_keeps_the_catalogue_generation() {
        let mailbox = sample();
        let provider = MailSearchProvider::new(&mailbox);
        let generation = provider.catalog.stats().generation;
        provider.refresh(&mailbox);
        assert_eq!(provider.catalog.stats().generation, generation);
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

//! Mail as a Tilo global-search source.
//!
//! Mirrors the reference wiring in `apps/contacts`: Mail declares its
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

use std::collections::HashMap;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tilo_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, FieldWeight, SearchCatalog,
    SearchCursor, SearchPage, SearchProvider, SearchRecord, SearchResult,
};

use crate::model::{Folder, Mailbox};

/// The kind label (and result-group header) Mail produces.
pub const MAIL_KIND: &str = "Mail";
pub const FOLDER_KIND: &str = "Mail folders";

/// Shown for a message whose subject is blank.
const NO_SUBJECT: &str = "(no subject)";

/// Retained state shared by every palette provider cloned from one Mail app.
/// The revision and resource rows are published with each replacement so an
/// address from an already-open palette is refused once a later mailbox
/// revision removed or trashed that message.
#[derive(Debug, Default)]
struct CatalogState {
    mailbox_revision: Option<u64>,
    /// Failed indexing is an availability error, never an excuse to keep
    /// presenting an unverified catalogue as current.
    last_error: Option<String>,
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
    /// A resource/action tree is authored only when a caller activates an
    /// exact address; retained search rows are scalar `MessageRow` metadata.
    #[cfg(test)]
    materialized_resources: Arc<AtomicUsize>,
    rows: Arc<RwLock<HashMap<String, crate::actions::MessageRow>>>,
    state: Arc<Mutex<CatalogState>>,
}

impl MailSearchProvider {
    /// Index every currently loaded message, then retain the catalogue for
    /// future palette openings and mailbox revisions.
    pub fn new(mailbox: &Mailbox) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(MAIL_KIND),
            #[cfg(test)]
            materialized_resources: Arc::new(AtomicUsize::new(0)),
            rows: Arc::new(RwLock::new(HashMap::new())),
            state: Arc::new(Mutex::new(CatalogState::default())),
        };
        if let Err(_error) = provider.refresh(mailbox) {
            // `refresh` retains the exact failure in `last_error`; Mail's
            // resource provider exposes it as `mail.search-status` until a
            // successful snapshot replaces it.
        }
        provider
    }

    fn message_address(id: crate::model::MessageId) -> ActionAddress {
        ActionAddress::new(format!("mail.message:{id}"), "open")
    }

    fn folder_address(folder: Folder) -> ActionAddress {
        ActionAddress::new(format!("mail.folder:{}", folder.as_key()), "open")
    }

    /// Reconcile loaded mail into the retained catalogue.
    ///
    /// # Errors
    ///
    /// Returns an indexing error instead of silently dropping a canonical
    /// search row. The previous catalogue remains available while callers
    /// surface the failure to the user.
    pub fn refresh(&self, mailbox: &Mailbox) -> Result<(), String> {
        let revision = mailbox.search_revision();
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.mailbox_revision == Some(revision) && state.last_error.is_none() {
            return Ok(());
        }
        drop(state);

        // `replace_all` builds postings off to the side and exchanges the
        // snapshot in one generation. A 100k-message refresh therefore does
        // not acquire the catalogue lock 100k times or leave a half-indexed
        // mailbox visible if one generated row is invalid.
        let mut catalog_rows = Vec::with_capacity(mailbox.messages.len() + Folder::ALL.len());
        for folder in Folder::ALL {
            catalog_rows.push(Self::folder_catalog_row(folder));
        }
        let mut resource_rows = HashMap::with_capacity(mailbox.messages.len());
        for message in mailbox
            .messages
            .values()
            .filter(|message| message.folder != Folder::Trash)
        {
            let row = crate::actions::message_row(message);
            catalog_rows.push(Self::message_catalog_row(message));
            resource_rows.insert(row.id.clone(), row);
        }
        self.replace_snapshot(revision, catalog_rows, resource_rows)
    }

    /// Atomically publish a validated search snapshot and its matching lazy
    /// resource rows. Keeping this small boundary explicit lets every caller
    /// observe a failed catalogue replacement instead of falling back to a
    /// silently incomplete index.
    fn replace_snapshot(
        &self,
        revision: u64,
        catalog_rows: Vec<SearchRecord>,
        resource_rows: HashMap<String, crate::actions::MessageRow>,
    ) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .mailbox_revision
            .is_some_and(|current| current > revision)
        {
            return Ok(());
        }
        if let Err(error) = self.catalog.replace_all_records(catalog_rows) {
            let error = format!("Mail search could not replace its catalogue: {error}");
            state.last_error = Some(error.clone());
            return Err(error);
        }
        let mut rows = self
            .rows
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *rows = resource_rows;
        drop(rows);
        state.mailbox_revision = Some(revision);
        state.last_error = None;
        drop(state);
        Ok(())
    }

    /// The last rejected catalogue refresh, if any.
    #[must_use]
    pub fn last_error(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_error
            .clone()
    }

    #[cfg(test)]
    fn materialized_resource_count(&self) -> usize {
        self.materialized_resources.load(Ordering::Relaxed)
    }

    fn message_catalog_row(message: &crate::model::Message) -> SearchRecord {
        let resource_id = format!("mail.message:{}", message.id);
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
        SearchRecord::new(resource_id, result)
            .field(&message.subject, FieldWeight::TITLE)
            .field(message.from.display(), FieldWeight::SUBTITLE)
            .field(&message.from.email, FieldWeight::IDENTITY)
            .field(message.labels.join(" "), FieldWeight::METADATA)
            .field(&message.body, FieldWeight::BODY)
    }

    fn folder_catalog_row(folder: Folder) -> SearchRecord {
        let address = Self::folder_address(folder);
        SearchRecord::new(
            address.resource.clone(),
            SearchResult::for_action(FOLDER_KIND, folder.title(), address)
                .with_subtitle("Mail folder")
                .with_icon(folder.icon()),
        )
        .field(folder.title(), FieldWeight::TITLE)
    }

    #[cfg(test)]
    pub(crate) fn inject_snapshot_failure_for_test(&self) {
        let error = self.replace_snapshot(
            u64::MAX,
            vec![
                SearchRecord::new(
                    "mail.message:not-the-address",
                    SearchResult::for_action(
                        MAIL_KIND,
                        "Invalid row",
                        Self::message_address(uuid::Uuid::nil()),
                    ),
                )
                .field("invalid row", FieldWeight::TITLE),
            ],
            HashMap::new(),
        );
        debug_assert!(error.is_err(), "test fixture must reject the bad row");
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
        let (work_budget, result_limit) = Self::page_limits(DEFAULT_SEARCH_CANDIDATE_LIMIT, limit);
        self.catalog
            .query_page(query, None, work_budget, result_limit)
            .results
    }

    fn page_limits(work_budget: usize, result_limit: usize) -> (usize, usize) {
        (
            work_budget.min(DEFAULT_SEARCH_CANDIDATE_LIMIT),
            result_limit.min(DEFAULT_SEARCH_RESULT_LIMIT),
        )
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
        let (work_budget, result_limit) = Self::page_limits(work_budget, result_limit);
        self.catalog
            .query_page(query.trim(), cursor, work_budget, result_limit)
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<tilo_actions::Resource> {
        if address.action != "open" {
            return None;
        }
        if let Some(id) = address.resource.strip_prefix("mail.message:") {
            // A retained provider shares this map with every open palette. If
            // a later mailbox revision removed or trashed the message, refuse
            // the old address instead of reviving stale read/star state.
            // Serialize resolution with an in-flight snapshot replacement: a
            // new catalogue hit never observes the previous resource rows.
            let _state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let row = self
                .rows
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(id)
                .cloned()?;
            #[cfg(test)]
            self.materialized_resources.fetch_add(1, Ordering::Relaxed);
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
    use ui::search::testing::{LATE_HIT_NEEDLE, assert_late_hit_reachable};

    use ui::search::SearchRegistry;

    fn sample() -> Mailbox {
        crate::sample::sample_mailbox()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn provider_matches_subject_sender_and_folded_infixes() {
        // Phase `provider_finds_a_message_by_subject_or_sender`.
        {
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

        // Phase `blank_query_returns_nothing`.
        {
            let provider = MailSearchProvider::new(&sample());
            assert!(
                provider.search("   ").is_empty(),
                "opening Ctrl+K is not an implicit whole-inbox browse"
            );
        }

        // Phase `unicode_case_folded_infixes_remain_searchable`.
        {
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

        // Phase `an_absent_query_gram_short_circuits_before_body_inspection`.
        {
            let provider = MailSearchProvider::new(&sample());
            let page = provider.search_page("\0\0", None, 8, 8);
            assert!(page.results.is_empty());
            assert_eq!(page.inspected, 0, "missing grams must avoid body work");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn search_budgets_clamp_untrusted_limits_and_still_reach_every_match() {
        // Phase `limited_search_honours_the_materialisation_budget`.
        {
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

        // Phase `paged_search_clamps_an_untrusted_result_limit`.
        {
            let mut mailbox = Mailbox::default();
            let template = sample().messages.values().next().unwrap().clone();
            for index in 0..300_u128 {
                let mut message = template.clone();
                message.id = uuid::Uuid::from_u128(index + 1);
                message.subject = format!("Bounded result message {index}");
                message.body.clear();
                message.attachments.clear();
                mailbox.insert(message);
            }
            let provider = MailSearchProvider::new(&mailbox);
            assert_eq!(
                provider
                    .search_page("bounded", None, usize::MAX, usize::MAX)
                    .results
                    .len(),
                DEFAULT_SEARCH_RESULT_LIMIT,
                "a caller cannot materialize an unbounded palette page"
            );
        }

        // Phase `paged_search_clamps_an_untrusted_work_budget`.
        {
            assert_eq!(
                MailSearchProvider::page_limits(usize::MAX, usize::MAX),
                (DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT),
                "both untrusted page bounds are clamped before the catalogue sees them"
            );
            let mut mailbox = Mailbox::default();
            let template = sample().messages.values().next().unwrap().clone();
            for index in 0..300_u128 {
                let mut message = template.clone();
                message.id = uuid::Uuid::from_u128(index + 1);
                message.subject = format!("Bounded work message {index}");
                message.body.clear();
                message.attachments.clear();
                mailbox.insert(message);
            }
            let provider = MailSearchProvider::new(&mailbox);
            let page = provider.search_page("bounded", None, usize::MAX, 1);
            assert!(
                page.inspected <= DEFAULT_SEARCH_CANDIDATE_LIMIT,
                "a caller cannot force unbounded candidate inspection"
            );
        }

        // Phase `paged_search_bounds_inspection_and_reaches_every_broad_match`.
        {
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

        // Phase `late_message_remains_exactly_reachable_in_a_large_catalogue`.
        {
            let (provider, hit) = assert_late_hit_reachable(
                |corpus| {
                    let mut mailbox = Mailbox::default();
                    let template = sample().messages.values().next().unwrap().clone();
                    for row in corpus {
                        let mut message = template.clone();
                        message.id = uuid::Uuid::from_u128(row.index as u128 + 1);
                        message.subject = row.text;
                        message.body.clear();
                        message.labels.clear();
                        message.attachments.clear();
                        mailbox.insert(message);
                    }
                    let provider = MailSearchProvider::new(&mailbox);
                    assert_eq!(
                        provider.materialized_resource_count(),
                        0,
                        "retained search rows hold compact MessageRow metadata, not prebuilt Resource/ActionSpec trees"
                    );
                    provider
                },
                3_000,
            );
            assert_eq!(hit.title, format!("ordinary record 2999 {LATE_HIT_NEEDLE}"));
            let address = hit.action_address().expect("canonical address");
            let resource = provider
                .resolve_action_resource(&address)
                .expect("provider resolves the late exact message resource");
            assert!(resource.actions.iter().any(|action| action.id == "open"));
            assert_eq!(
                provider.materialized_resource_count(),
                1,
                "only the exact activated hit materializes its action tree"
            );
        }

        // Phase `complete_catalogue_finds_an_old_substring_past_the_former_snapshot_cap`.
        {
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn catalogue_snapshots_publish_generations_and_refuse_stale_addresses() {
        // Phase `retained_catalogue_refuses_a_stale_message_address`.
        {
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
            provider.refresh(&mailbox).unwrap();
            assert!(
                provider.resolve_action_resource(&address).is_none(),
                "a palette row from an earlier revision cannot revive trashed mail"
            );
        }

        // Phase `exact_message_resolution_waits_for_a_snapshot_publication_boundary`.
        {
            use std::sync::mpsc;

            let mailbox = sample();
            let provider = MailSearchProvider::new(&mailbox);
            let message = mailbox
                .messages
                .values()
                .find(|message| message.folder != Folder::Trash)
                .expect("sample message");
            let address = MailSearchProvider::message_address(message.id);
            // `replace_snapshot` holds this same state mutex while it swaps the
            // search catalogue and its matching lazy rows. An activation therefore
            // cannot see the new catalogue with the old row map in between.
            let publication = provider.state.lock().unwrap();
            let (started_tx, started_rx) = mpsc::channel();
            let (finished_tx, finished_rx) = mpsc::channel();
            let provider_clone = provider.clone();
            let worker = std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                let resolved_resource = provider_clone.resolve_action_resource(&address);
                finished_tx.send(resolved_resource.is_some()).unwrap();
            });
            started_rx.recv().unwrap();
            assert!(
                matches!(finished_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "resolution cannot pass the publication boundary early"
            );
            drop(publication);
            assert!(finished_rx.recv().unwrap());
            worker.join().unwrap();
        }

        // Phase `refresh_invalidates_old_continuation_cursors`.
        {
            let mut mailbox = Mailbox::default();
            let template = sample().messages.values().next().unwrap().clone();
            for index in 0..4_u128 {
                let mut message = template.clone();
                message.id = uuid::Uuid::from_u128(index + 1);
                message.subject = format!("Cursor mail {index}");
                mailbox.insert(message);
            }
            let provider = MailSearchProvider::new(&mailbox);
            let cursor = provider
                .search_page("cursor", None, 1, 1)
                .next
                .expect("the broad result set is paged");
            let mut changed = template;
            changed.id = uuid::Uuid::from_u128(99);
            changed.subject = "Cursor mail changed".into();
            mailbox.insert(changed);
            provider.refresh(&mailbox).unwrap();
            assert!(!provider.action_directory_cursor_is_current(&cursor));
        }

        // Phase `failed_snapshot_replacement_is_reported_without_discarding_the_last_catalogue`.
        {
            let mailbox = sample();
            let provider = MailSearchProvider::new(&mailbox);
            let existing = mailbox.messages.values().next().unwrap();
            let error = provider.replace_snapshot(
                mailbox.search_revision() + 1,
                vec![
                    SearchRecord::new(
                        "mail.message:not-the-address",
                        SearchResult::for_action(
                            MAIL_KIND,
                            "Invalid row",
                            MailSearchProvider::message_address(existing.id),
                        ),
                    )
                    .field("invalid row", FieldWeight::TITLE),
                ],
                HashMap::new(),
            );
            assert!(
                error.is_err(),
                "the shared catalogue rejects an invalid row"
            );
            assert!(
                provider.last_error().is_some(),
                "the provider retains a surfaced availability error"
            );
            assert!(
                provider
                    .search(&existing.subject)
                    .iter()
                    .any(|hit| hit.title == existing.subject),
                "a rejected replacement leaves the previous valid snapshot available"
            );
        }

        // Phase `refresh_without_a_new_revision_keeps_the_catalogue_generation`.
        {
            let mailbox = sample();
            let provider = MailSearchProvider::new(&mailbox);
            let generation = provider.catalog.stats().generation;
            provider.refresh(&mailbox).unwrap();
            assert_eq!(provider.catalog.stats().generation, generation);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn search_hits_resolve_to_canonical_resource_actions() {
        // Phase `canonical_message_resolution_keeps_all_meaningful_actions`.
        {
            let mailbox = sample();
            let provider = MailSearchProvider::new(&mailbox);
            let message = mailbox.messages.values().next().unwrap();
            let query = message
                .subject
                .split_whitespace()
                .next()
                .unwrap_or(&message.subject);
            let hit = provider
                .search(query)
                .into_iter()
                .find(|hit| hit.title == message.subject)
                .expect("canonical message result");
            let resource = provider
                .resolve_action_resource(&hit.action_address().unwrap())
                .expect("provider-path exact resource");
            for action in [
                "open",
                "inspect",
                "read",
                "star",
                "archive",
                "delete",
                "add-label",
            ] {
                assert!(
                    resource
                        .actions
                        .iter()
                        .any(|published| published.id == action),
                    "canonical resolution retains {action}"
                );
            }
        }

        // Phase `every_hit_uses_a_canonical_resource_action`.
        {
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

        // Phase `folder_hits_materialize_their_own_canonical_open_resource`.
        {
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

        // Phase `search_address_resolves_and_navigates_exactly`.
        {
            use automerge::{AutoCommit, ROOT};
            use ui::widget::Widget;

            let mut app = crate::app::MailApp::new_sample();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);

            // Find a real message through the app's own provider.
            let (address, id) = {
                let providers = ui::search::capability::search_providers(&app);
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

            let resource = crate::app::MailApp::publish_resource(&app, &doc, &address.resource)
                .expect("search address resolves lazily");
            let encoded = tilo_actions::door::encode(&[resource], &address, None).unwrap();
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::default(),
                        tilo_actions::OperationKind::View,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert_eq!(
                app.selected_message(),
                Some(id),
                "activation opened the chosen message"
            );
            assert_eq!(
                crate::doc::MailDoc::get_read(&doc, &id.to_string()),
                Some(true),
                "search activation follows the same explicit Open → SetRead action"
            );

            let folder_hit = ui::search::capability::search_providers(&app)[0]
                .search("Archive")
                .into_iter()
                .find(|hit| hit.kind == FOLDER_KIND)
                .expect("Archive folder is searchable");
            let folder_address = folder_hit
                .action_address()
                .expect("canonical folder address");
            let resources = ui::resources::resources(&app, &doc);
            let encoded = tilo_actions::door::encode(&resources, &folder_address, None)
                .expect("folder address resolves against Mail's exported tree");
            let heads_before_folder_navigation = doc.get_heads();
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert_eq!(app.mail_resource_ctx(&doc).current_folder, Folder::Archive);
            assert_eq!(doc.get_heads(), heads_before_folder_navigation);
        }
    }
}

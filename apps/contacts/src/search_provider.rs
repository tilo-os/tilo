//! Contacts as a Tilo global-search source.
//!
//! This is the reference wiring for the shared search framework
//! ([`ui::search`]): Contacts declares its searchable type by implementing
//! [`SearchProvider`], turning matching address-book records and durable group
//! names into typed [`SearchResult`]s that any search surface can rank and
//! present alongside every other provider's hits. Each hit addresses an
//! exported `open` view action, so pointer, keyboard, assistive technology and
//! agents all enter through the same brokered operation.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use ui::icons::Icon;

use ui::search::{
    DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor,
    SearchPage, SearchProvider, SearchResult,
};

use crate::model::{Contact, ContactId, ContactStore, normalize_text};

/// The kind label (and result group header) Contacts produces.
pub const CONTACT_KIND: &str = "Contacts";

/// A complete, cursor-paged catalogue over the address-book snapshot.
///
/// `SearchCatalog` indexes every stable contact resource, while the small map
/// lets the search framework lazily resolve an item omitted from Contacts'
/// bounded open-resource page. Cloning a provider is cheap: the index itself
/// is an `Arc` handle managed by the shared search implementation.
#[derive(Clone)]
pub struct ContactSearchProvider {
    /// The catalogue and lazy labels are one published generation.  Keeping
    /// them under the same lock prevents an activation from observing a new
    /// search row before the corresponding exact label is available.
    snapshot: Arc<Mutex<ContactSearchSnapshot>>,
}

/// Only the public label needed to resolve a search hit's canonical `open`
/// action is retained beside the index. Full contacts remain authoritative in
/// `ContactsApp`/the CRDT and exact edit resources are materialised there.
#[derive(Default)]
struct ContactSearchLookup {
    contacts: BTreeMap<ContactId, String>,
    groups: BTreeMap<String, String>,
}

struct ContactSearchSnapshot {
    catalog: SearchCatalog,
    lookup: ContactSearchLookup,
}

impl Default for ContactSearchSnapshot {
    fn default() -> Self {
        Self {
            catalog: SearchCatalog::new(CONTACT_KIND),
            lookup: ContactSearchLookup::default(),
        }
    }
}

impl ContactSearchProvider {
    /// Snapshot the current address book into a provider.
    pub fn new(store: &ContactStore) -> Self {
        let provider = Self {
            snapshot: Arc::new(Mutex::new(ContactSearchSnapshot::default())),
        };
        provider
            .refresh(store)
            .expect("Contacts search index rows must have canonical addresses");
        provider
    }

    /// Atomically replace the complete retained index. On error the prior
    /// generation and exact lookup remain intact, so Ctrl+K never advertises
    /// a partial book; callers can surface the diagnostic to the user.
    pub fn refresh(&self, store: &ContactStore) -> Result<(), ui::search::SearchCatalogError> {
        let mut lookup = ContactSearchLookup::default();
        let mut rows = Vec::with_capacity(store.contacts.len());
        for contact in &store.contacts {
            let result = contact_result(contact);
            let address = result
                .action_address()
                .expect("contact search result has a canonical action");
            lookup.contacts.insert(contact.id, contact.display_name());
            rows.push((
                address.resource,
                normalize_text(&searchable_fields(contact).join("\0")),
                result,
            ));
        }
        for group in crate::actions::ordered_groups(&store.contacts) {
            let result = group_result(&group);
            let address = result
                .action_address()
                .expect("group search result has a canonical action");
            lookup
                .groups
                .insert(address.resource.clone(), group.clone());
            rows.push((
                address.resource,
                normalize_text(&format!("group {group}")),
                result,
            ));
        }
        // `replace_all` validates off-lock internally, then exchanges its
        // generation. Holding the snapshot lock through that exchange and
        // the matching label swap makes the two views indivisible to search
        // and exact-resolution callers. A rejected replacement leaves both
        // pieces of the prior, last-good generation intact.
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.catalog.replace_all(rows)?;
        snapshot.lookup = lookup;
        Ok(())
    }

    #[cfg(test)]
    fn stats(&self) -> ui::search::SearchCatalogStats {
        self.snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .stats()
    }

    fn page_limits(work_budget: usize, result_limit: usize) -> (usize, usize) {
        (
            work_budget.min(DEFAULT_SEARCH_CANDIDATE_LIMIT),
            result_limit.min(DEFAULT_SEARCH_RESULT_LIMIT),
        )
    }
}

/// Every user-searchable field in a contact. The vector rather than a
/// flattened string retains field boundaries via a newline separator.
fn searchable_fields(contact: &Contact) -> Vec<String> {
    let mut fields = vec![
        contact.first.clone(),
        contact.last.clone(),
        contact.org.clone(),
        contact.title.clone(),
        contact.website.clone(),
        contact.notes.clone(),
    ];
    if let Some(birthday) = contact.birthday {
        fields.extend([birthday.iso(), birthday.pretty()]);
    }
    for field in &contact.phones {
        fields.extend([field.label.clone(), field.value.clone()]);
    }
    for field in &contact.emails {
        fields.extend([field.label.clone(), field.value.clone()]);
    }
    for address in &contact.addresses {
        fields.extend([
            address.label.clone(),
            address.street.clone(),
            address.city.clone(),
            address.region.clone(),
            address.postal.clone(),
            address.country.clone(),
            address.one_line(),
        ]);
    }
    fields.extend(contact.groups.iter().map(|group| group.value.clone()));
    fields
        .into_iter()
        .filter(|field| !field.is_empty())
        .collect()
}

impl SearchProvider for ContactSearchProvider {
    fn name(&self) -> &str {
        CONTACT_KIND
    }

    fn action_directory_generation(&self) -> Option<u64> {
        Some(
            self.snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .catalog
                .stats()
                .generation,
        )
    }

    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .cursor_is_current(cursor)
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .search_limited(
                &normalize_text(query),
                limit.min(DEFAULT_SEARCH_RESULT_LIMIT),
            )
    }

    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        let (work_budget, result_limit) = Self::page_limits(work_budget, result_limit);
        self.snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .query_page(&normalize_text(query), cursor, work_budget, result_limit)
    }

    fn resolve_action_resource(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::Resource> {
        let snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(raw_id) = address.resource.strip_prefix("contacts.contact:") {
            let id = raw_id.parse::<ContactId>().ok()?;
            let label = snapshot.lookup.contacts.get(&id).cloned()?;
            return (address == &crate::actions::open_contact_address(id))
                .then(|| crate::actions::contact_open_resource(id, &label));
        }
        let group = snapshot.lookup.groups.get(&address.resource).cloned()?;
        (address == &crate::actions::open_group_address(&group))
            .then(|| crate::actions::group_resource(&group))
    }
}

fn contact_result(contact: &Contact) -> SearchResult {
    let mut r = SearchResult::for_action(
        CONTACT_KIND,
        contact.display_name(),
        crate::actions::open_contact_address(contact.id),
    )
    .with_icon(Icon::Mail);
    let sub = contact.subtitle();
    if !sub.is_empty() {
        r = r.with_subtitle(sub);
    }
    // Float favourites a notch above equally-strong matches.
    if contact.favorite {
        r = r.with_score(1);
    }
    r
}

fn group_result(group: &str) -> SearchResult {
    SearchResult::for_action(
        CONTACT_KIND,
        format!("Group: {group}"),
        crate::actions::open_group_address(group),
    )
    .with_subtitle("Filter contacts in this group")
    .with_icon(Icon::Users)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Contact, ContactStore, sample_contacts};
    use ui::search::SearchRegistry;
    use uuid::Uuid;

    fn store_with_samples() -> ContactStore {
        let mut store = ContactStore::new();
        for c in sample_contacts() {
            store.upsert(c);
        }
        store
    }

    #[test]
    fn provider_finds_a_real_contact_by_name() {
        let store = store_with_samples();
        let provider = ContactSearchProvider::new(&store);
        let hits = provider.search("hopper");
        assert!(
            hits.iter().any(|r| r.title == "Grace Hopper"),
            "the provider surfaces the real matching contact, got {:?}",
            hits.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
        assert!(
            hits.iter().all(|r| r.kind == CONTACT_KIND),
            "all hits are tagged Contacts"
        );
    }

    #[test]
    fn matches_a_field_the_title_does_not_show() {
        // Grace Hopper's email is `grace.hopper@navy.mil`; a "navy" query hits
        // the email even though the display title is just the name — proving the
        // provider searches the whole record, not only the visible title.
        let store = store_with_samples();
        let provider = ContactSearchProvider::new(&store);
        let hits = provider.search("navy");
        assert!(
            hits.iter().any(|r| r.title == "Grace Hopper"),
            "a field-only (email) match still surfaces the contact"
        );
    }

    #[test]
    fn indexes_every_contact_field_with_unicode_normalization() {
        let mut contact = Contact::new();
        contact.first = "Jose\u{301}".into(); // decomposed é
        contact.birthday = crate::model::Birthday::parse("--02-29");
        contact
            .phones
            .push(crate::model::Labeled::new("satellite", "+1 555 0100"));
        contact.addresses.push(crate::model::Address {
            label: "Sommarstuga".into(),
            city: "Göteborg".into(),
            ..Default::default()
        });
        let mut store = ContactStore::new();
        store.upsert(contact.clone());
        let provider = ContactSearchProvider::new(&store);

        for query in [
            "José",
            "satellite",
            "Sommarstuga",
            "Göteborg",
            "February 29",
        ] {
            assert_eq!(
                provider
                    .search(query)
                    .first()
                    .map(|result| result.title.as_str()),
                Some("Jose\u{301}"),
                "{query:?} reaches a stored field with a canonical open action"
            );
        }
        assert_eq!(
            provider.search("José")[0].action_address(),
            Some(crate::actions::open_contact_address(contact.id))
        );
    }

    #[test]
    fn result_addresses_the_exported_open_action() {
        let store = store_with_samples();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(ContactSearchProvider::new(&store)));

        let groups = registry.query_grouped("grace");
        assert_eq!(groups[0].kind, CONTACT_KIND);
        let top = &groups[0].results[0];
        assert_eq!(top.title, "Grace Hopper", "the best match leads");

        let contact = store
            .contacts
            .iter()
            .find(|contact| contact.display_name() == top.title)
            .expect("the result names a stored contact");
        assert_eq!(
            top.action_address(),
            Some(crate::actions::open_contact_address(contact.id)),
            "search publishes the resource tree's canonical view action"
        );
    }

    #[test]
    fn favourites_carry_a_ranking_bias() {
        let store = store_with_samples();
        let provider = ContactSearchProvider::new(&store);
        // Ada Lovelace is a favourite in the sample set.
        let hits = provider.search("ada");
        let ada = hits
            .iter()
            .find(|r| r.title == "Ada Lovelace")
            .expect("Ada is found");
        assert!(
            ada.score >= 1,
            "a favourite result carries a positive score bias"
        );
    }

    #[test]
    fn empty_query_returns_contacts_and_durable_groups_as_one_result_group() {
        let store = store_with_samples();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(ContactSearchProvider::new(&store)));
        let groups = registry.query_grouped("");
        assert_eq!(groups.len(), 1, "one Contacts group");
        assert_eq!(
            groups[0].results.len(),
            store.len() + store.all_groups().len(),
            "every contact and durable group is browsable"
        );
    }

    #[test]
    fn group_search_uses_the_canonical_group_resource_and_resolves_lazily() {
        let store = store_with_samples();
        let group = store
            .all_groups()
            .into_iter()
            .find(|group| group == "Work")
            .expect("the samples include the Work group");
        let provider = ContactSearchProvider::new(&store);
        let hit = provider
            .search("group work")
            .into_iter()
            .find(|result| result.title == "Group: Work")
            .expect("group names are globally searchable");
        let address = hit.action_address().expect("group result is actionable");
        assert_eq!(address, crate::actions::open_group_address(&group));
        let resource = provider
            .resolve_action_resource(&address)
            .expect("the group resolves without expanding the palette page");
        assert!(
            tilo_actions::door::resolve(&[resource], &address).is_some(),
            "the exact group open action is agent-addressable"
        );
    }

    #[test]
    fn contacts_beyond_the_legacy_snapshot_limit_are_searchable() {
        let mut store = ContactStore::new();
        for index in 0..4_097 {
            store.upsert(Contact {
                id: Uuid::new_v4(),
                first: format!("Contact {index}"),
                last: if index == 4_096 {
                    "needle beyond legacy cap".into()
                } else {
                    String::new()
                },
                ..Contact::default()
            });
        }

        let hits = ContactSearchProvider::new(&store).search("needle beyond legacy cap");
        assert_eq!(
            hits.len(),
            1,
            "the complete index reaches the final contact"
        );
        assert_eq!(hits[0].title, "Contact 4096 needle beyond legacy cap");
    }

    #[test]
    fn paged_catalogue_reaches_a_late_contact_without_unbounded_allocation() {
        let contacts = (0..100_001u128)
            .map(|index| Contact {
                id: Uuid::from_u128(index + 1),
                first: if index == 100_000 {
                    "Late catalogue needle".into()
                } else {
                    format!("Contact {index}")
                },
                ..Contact::default()
            })
            .collect();
        let provider = ContactSearchProvider::new(&ContactStore { contacts });

        let page = provider.search_page("catalogue needle", None, 4, 1);
        assert_eq!(page.inspected, 1, "rare postings avoid scanning the book");
        assert_eq!(page.results.len(), 1, "one bounded result is materialised");
        assert_eq!(page.results[0].title, "Late catalogue needle");
        assert!(page.next.is_none(), "the unique match is exhausted");
        let address = page.results[0]
            .action_address()
            .expect("catalogue hits use canonical open addresses");
        let resource = provider
            .resolve_action_resource(&address)
            .expect("the late hit resolves without expanding the contextual page");
        assert!(tilo_actions::door::resolve(&[resource], &address).is_some());
        assert_eq!(provider.stats().records, 100_001);
    }

    #[test]
    fn paged_search_clamps_untrusted_work_and_result_limits() {
        let contacts = (0..(DEFAULT_SEARCH_RESULT_LIMIT + 64) as u128)
            .map(|index| Contact {
                id: Uuid::from_u128(index + 1),
                first: format!("Common bounded contact {index}"),
                ..Contact::default()
            })
            .collect();
        let provider = ContactSearchProvider::new(&ContactStore { contacts });
        assert_eq!(
            ContactSearchProvider::page_limits(usize::MAX, usize::MAX),
            (DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT),
            "both caller-provided limits are clamped before querying the retained catalogue"
        );
        let page = provider.search_page("common bounded", None, usize::MAX, usize::MAX);
        assert!(
            page.inspected <= DEFAULT_SEARCH_CANDIDATE_LIMIT,
            "a caller cannot force unbounded candidate inspection"
        );
        assert!(
            page.results.len() <= DEFAULT_SEARCH_RESULT_LIMIT,
            "a caller cannot materialise an unbounded palette page"
        );
        assert!(
            provider.search_limited("common bounded", usize::MAX).len()
                <= DEFAULT_SEARCH_RESULT_LIMIT,
            "the non-paged compatibility API uses the same result cap"
        );
    }

    #[test]
    fn paged_unicode_query_continues_with_an_opaque_cursor() {
        let contacts = (0..3u128)
            .map(|index| Contact {
                id: Uuid::from_u128(index + 1),
                first: format!("Cafe\u{301} contact {index}"),
                ..Contact::default()
            })
            .collect();
        let provider = ContactSearchProvider::new(&ContactStore { contacts });

        let first = provider.search_page("CAFÉ", None, 1, 1);
        assert_eq!(first.results.len(), 1);
        assert_eq!(first.inspected, 1);
        let cursor = first.next.expect("two matching resources remain");
        // The cursor deliberately stays opaque. Supplying an NFC spelling of
        // the equivalent query continues the same canonical query page.
        let second = provider.search_page("Café", Some(&cursor), 1, 1);
        assert_eq!(second.results.len(), 1);
        assert_ne!(second.results[0].action_id, first.results[0].action_id);
        assert_eq!(second.inspected, 1);
    }

    #[test]
    fn failed_refresh_keeps_the_last_complete_catalogue_and_cursor() {
        let store = store_with_samples();
        let provider = ContactSearchProvider::new(&store);
        let first = provider.search_page("", None, 1, 1);
        let cursor = first.next.expect("the sample book has more than one row");
        let before = provider.search("grace");

        let duplicate = Contact {
            id: store.contacts[0].id,
            first: "Duplicate id must fail atomically".into(),
            ..Contact::default()
        };
        let invalid = ContactStore {
            contacts: vec![store.contacts[0].clone(), duplicate],
        };
        assert!(
            provider.refresh(&invalid).is_err(),
            "duplicate canonical resource ids reject the new generation"
        );
        assert_eq!(
            provider.search("grace"),
            before,
            "last good results remain live"
        );
        assert!(
            provider.action_directory_cursor_is_current(&cursor),
            "a failed replacement leaves the prior generation/current cursor intact"
        );
    }

    #[test]
    fn search_and_exact_resolution_share_the_snapshot_publication_boundary() {
        use std::sync::mpsc;

        let store = store_with_samples();
        let provider = ContactSearchProvider::new(&store);
        let result = provider
            .search("grace")
            .into_iter()
            .next()
            .expect("sample contact is searchable");
        let address = result
            .action_address()
            .expect("sample contact uses its canonical open address");

        // Both operations lock the same snapshot that `refresh` holds while
        // it replaces catalogue generation and exact labels. Neither may
        // observe a halfway-published generation.
        let publication = provider.snapshot.lock().expect("snapshot lock");
        let (search_started_tx, search_started_rx) = mpsc::channel();
        let (resolve_started_tx, resolve_started_rx) = mpsc::channel();
        let (search_done_tx, search_done_rx) = mpsc::channel();
        let (resolve_done_tx, resolve_done_rx) = mpsc::channel();
        let search_provider = provider.clone();
        let resolve_provider = provider.clone();
        let search_worker = std::thread::spawn(move || {
            search_started_tx.send(()).expect("test coordinator lives");
            search_done_tx
                .send(search_provider.search("grace"))
                .expect("test coordinator lives");
        });
        let resolve_worker = std::thread::spawn(move || {
            resolve_started_tx.send(()).expect("test coordinator lives");
            resolve_done_tx
                .send(resolve_provider.resolve_action_resource(&address).is_some())
                .expect("test coordinator lives");
        });

        search_started_rx.recv().expect("search started");
        resolve_started_rx.recv().expect("resolution started");

        assert!(
            matches!(search_done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "a search cannot pass the publication boundary early"
        );
        assert!(
            matches!(resolve_done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "exact resolution cannot pair a new catalogue with old labels"
        );
        drop(publication);
        assert_eq!(
            search_done_rx
                .recv()
                .expect("search resumes after publication")[0]
                .title,
            "Grace Hopper"
        );
        assert!(
            resolve_done_rx
                .recv()
                .expect("exact resolution resumes after publication")
        );
        search_worker.join().expect("search worker completes");
        resolve_worker.join().expect("resolve worker completes");
    }

    #[test]
    fn sensitive_search_terms_find_contacts_without_leaking_private_values() {
        let mut contact = Contact::new();
        contact.first = "Private person".into();
        contact.emails.push(crate::model::Labeled::new(
            "personal",
            "very-private@example.test",
        ));
        contact.notes = "confidential medical note".into();
        let provider = ContactSearchProvider::new(&ContactStore {
            contacts: vec![contact],
        });

        for term in ["very-private@example.test", "confidential medical note"] {
            let hit = provider
                .search(term)
                .into_iter()
                .next()
                .expect("private field is searchable");
            assert!(!hit.title.contains(term));
            assert!(
                hit.subtitle
                    .as_deref()
                    .is_none_or(|subtitle| !subtitle.contains(term))
            );
        }
    }
}

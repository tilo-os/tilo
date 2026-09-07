//! Contacts as a qOS global-search source.
//!
//! This is the reference wiring for the shared search framework
//! ([`ui::search`]): Contacts declares its searchable type by implementing
//! [`SearchProvider`], turning matching address-book records and durable group
//! names into typed [`SearchResult`]s that any search surface can rank and
//! present alongside every other provider's hits. Each hit addresses an
//! exported `open` view action, so pointer, keyboard, assistive technology and
//! agents all enter through the same brokered operation.

use std::collections::BTreeMap;
use ui::icons::Icon;

use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
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
    catalog: SearchCatalog,
    contacts: BTreeMap<ContactId, Contact>,
    /// Canonical resource id → deterministic display spelling. Keeping this
    /// separately from contacts means a group resolves even when it was not
    /// included in the bounded contextual palette page.
    groups: BTreeMap<String, String>,
}

impl ContactSearchProvider {
    /// Snapshot the current address book into a provider.
    pub fn new(store: &ContactStore) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(CONTACT_KIND),
            contacts: store
                .contacts
                .iter()
                .cloned()
                .map(|contact| (contact.id, contact))
                .collect(),
            groups: crate::actions::ordered_groups(&store.contacts)
                .into_iter()
                .map(|name| (crate::actions::group_resource_id(&name), name))
                .collect(),
        };
        // Browse pages use insertion order, so seed the catalogue in durable
        // UUID order rather than incidental Automerge/map projection order.
        for contact in provider.contacts.values() {
            provider.index_contact(contact);
        }
        for group in provider.groups.values() {
            provider.index_group(group);
        }
        provider
    }

    fn index_contact(&self, contact: &Contact) {
        let result = contact_result(contact);
        let Some(address) = result.action_address() else {
            return;
        };
        // NFC normalisation happens before the shared lowercasing/indexing
        // layer, so a continuation cursor remains valid when callers switch
        // between canonically equivalent Unicode spellings of the query.
        let searchable = normalize_text(&searchable_fields(contact).join("\0"));
        let _ = self.catalog.upsert(address.resource, searchable, result);
    }

    fn index_group(&self, group: &str) {
        let result = group_result(group);
        let Some(address) = result.action_address() else {
            return;
        };
        // Prefixing the name helps people discover the filter through generic
        // queries such as "group work", while the normalised name keeps direct
        // matching equivalent to the in-app group picker.
        let searchable = normalize_text(&format!("group {group}"));
        let _ = self.catalog.upsert(address.resource, searchable, result);
    }

    #[cfg(test)]
    fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
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
        Some(self.catalog.stats().generation)
    }

    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.catalog.cursor_is_current(cursor)
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.catalog.search_limited(&normalize_text(query), limit)
    }

    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        self.catalog
            .query_page(&normalize_text(query), cursor, work_budget, result_limit)
    }

    fn resolve_action_resource(
        &self,
        address: &qos_actions::ActionAddress,
    ) -> Option<qos_actions::Resource> {
        if let Some(raw_id) = address.resource.strip_prefix("contacts.contact:") {
            let id = raw_id.parse::<ContactId>().ok()?;
            let contact = self.contacts.get(&id)?;
            return (address == &crate::actions::open_contact_address(id))
                .then(|| crate::actions::contact_resource(contact));
        }
        let group = self.groups.get(&address.resource)?;
        (address == &crate::actions::open_group_address(group))
            .then(|| crate::actions::group_resource(group))
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
            qos_actions::door::resolve(&[resource], &address).is_some(),
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
        assert!(qos_actions::door::resolve(&[resource], &address).is_some());
        assert_eq!(provider.stats().records, 100_001);
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
}

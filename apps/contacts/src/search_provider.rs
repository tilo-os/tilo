//! Contacts as a qOS global-search source.
//!
//! This is the reference wiring for the shared search framework
//! ([`ui::search`]): Contacts declares its searchable type by implementing
//! [`SearchProvider`], turning matching address-book records into typed
//! [`SearchResult`]s that any search surface can rank and present alongside
//! every other provider's hits. Each hit addresses the contact resource's
//! exported `open` view action, so pointer, keyboard, assistive technology and
//! agents all enter through the same brokered operation.

use ui::icons::Icon;
use ui::search::{DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult, SearchTextIndex};

use crate::model::{Contact, ContactStore};

/// The kind label (and result group header) Contacts produces.
pub const CONTACT_KIND: &str = "Contacts";

/// A contact plus its independently searchable fields. A newline separates
/// values in the indexed projection, so ordinary text queries cannot bridge
/// the end of a phone number into the start of an email.
struct SearchEntry {
    contact: Contact,
    sources: Vec<String>,
}

/// A [`SearchProvider`] over a snapshot of the address book. Rebuild it (cheap)
/// whenever the store changes, or when the search surface opens.
pub struct ContactSearchProvider {
    entries: Vec<SearchEntry>,
    /// Complete Unicode substring index over the address book. The result
    /// window is bounded, but no contact is silently left out of the corpus.
    index: SearchTextIndex,
}

impl ContactSearchProvider {
    /// Snapshot the current address book into a provider.
    pub fn new(store: &ContactStore) -> Self {
        let entries: Vec<_> = store
            .contacts
            .iter()
            .cloned()
            .map(|contact| SearchEntry {
                sources: searchable_fields(&contact),
                contact,
            })
            .collect();
        Self {
            index: SearchTextIndex::new(entries.iter().map(|entry| entry.sources.join("\n"))),
            entries,
        }
    }

    fn matching_entry_ids(&self, query: &str, limit: usize) -> Vec<usize> {
        if query.is_empty() || limit == 0 {
            return Vec::new();
        }
        self.index.matching_ids(query, limit)
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
    fields.extend(contact.phones.iter().map(|field| field.value.clone()));
    fields.extend(contact.emails.iter().map(|field| field.value.clone()));
    fields.extend(
        contact
            .addresses
            .iter()
            .map(crate::model::Address::one_line),
    );
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

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let limit = limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
        if limit == 0 {
            return Vec::new();
        }
        if query.is_empty() {
            return self
                .entries
                .iter()
                .take(limit)
                .map(|entry| contact_result(&entry.contact))
                .collect();
        }
        self.matching_entry_ids(query, limit)
            .into_iter()
            .filter_map(|id| self.entries.get(id))
            .map(|entry| contact_result(&entry.contact))
            .collect()
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
    fn empty_query_returns_the_whole_book_as_one_group() {
        let store = store_with_samples();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(ContactSearchProvider::new(&store)));
        let groups = registry.query_grouped("");
        assert_eq!(groups.len(), 1, "one Contacts group");
        assert_eq!(
            groups[0].results.len(),
            store.len(),
            "every contact is browsable"
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
}

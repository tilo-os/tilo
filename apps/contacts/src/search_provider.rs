//! Contacts as a qOS global-search source.
//!
//! This is the reference wiring for the shared search framework
//! ([`ui::search`]): Contacts declares its searchable type by implementing
//! [`SearchProvider`], turning matching address-book records into typed
//! [`SearchResult`]s that any search surface — the app's own field, or a future
//! shell-level Ctrl+K — can rank and present alongside every other provider's
//! hits. The app no longer has to render or rank a bespoke result list; it only
//! declares what it can find and dispatches on the opaque `action_id` a chosen
//! row hands back.

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};

use crate::model::{ContactId, ContactStore};

/// The kind label (and result group header) Contacts produces.
pub const CONTACT_KIND: &str = "Contacts";

/// The `action_id` prefix. A chosen row's `action_id` is `contact:<uuid>`;
/// [`parse_action_id`] turns it back into a [`ContactId`] the app selects.
const ACTION_PREFIX: &str = "contact:";

/// A [`SearchProvider`] over a snapshot of the address book. Rebuild it (cheap)
/// whenever the store changes, or when the search surface opens.
pub struct ContactSearchProvider {
    contacts: Vec<crate::model::Contact>,
}

impl ContactSearchProvider {
    /// Snapshot the current address book into a provider.
    pub fn new(store: &ContactStore) -> Self {
        Self {
            contacts: store.contacts.clone(),
        }
    }

    /// The `action_id` for a contact — what a chosen row hands back.
    pub fn action_id(id: ContactId) -> String {
        format!("{ACTION_PREFIX}{id}")
    }

    /// Resolve an `action_id` produced by this provider back to a
    /// [`ContactId`], so the host can select the chosen contact. Returns `None`
    /// for an id this provider did not emit.
    pub fn parse_action_id(action_id: &str) -> Option<ContactId> {
        action_id
            .strip_prefix(ACTION_PREFIX)
            .and_then(|rest| ContactId::parse_str(rest).ok())
    }
}

impl SearchProvider for ContactSearchProvider {
    fn name(&self) -> &str {
        CONTACT_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        // The model's cross-field matcher expects a lower-cased query and
        // searches name / org / title / phones / emails / addresses / notes /
        // groups — richer than a title-only match, so a phone- or email-only
        // hit still surfaces. The registry then re-ranks by the query's fit
        // against the visible title.
        let q = query.to_lowercase();
        self.contacts
            .iter()
            .filter(|c| c.matches(&q))
            .map(|c| {
                let mut r =
                    SearchResult::new(CONTACT_KIND, c.display_name(), Self::action_id(c.id))
                        .with_icon(Icon::Mail);
                let sub = c.subtitle();
                if !sub.is_empty() {
                    r = r.with_subtitle(sub);
                }
                // Float favourites a notch above equally-strong matches.
                if c.favorite {
                    r = r.with_score(1);
                }
                r
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ContactStore, sample_contacts};
    use ui::search::SearchRegistry;

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
    fn activation_id_resolves_back_to_the_chosen_contact() {
        // The end-to-end seam: register the provider, query through the shared
        // registry, take the ranked top hit, and dispatch its opaque action_id
        // back to a real contact in the store.
        let store = store_with_samples();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(ContactSearchProvider::new(&store)));

        let groups = registry.query_grouped("grace");
        assert_eq!(groups[0].kind, CONTACT_KIND);
        let top = &groups[0].results[0];
        assert_eq!(top.title, "Grace Hopper", "the best match leads");

        let id = ContactSearchProvider::parse_action_id(&top.action_id)
            .expect("action_id parses back to a ContactId");
        let contact = store
            .get(id)
            .expect("the id resolves to a real stored contact");
        assert_eq!(
            contact.display_name(),
            top.title,
            "activation lands on the right record"
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
    fn parse_action_id_rejects_foreign_ids() {
        assert!(ContactSearchProvider::parse_action_id("file:/etc/passwd").is_none());
        assert!(ContactSearchProvider::parse_action_id("contact:not-a-uuid").is_none());
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
}

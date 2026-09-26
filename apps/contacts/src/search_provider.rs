//! Contacts as a Tilo global-search source.
//!
//! This is the reference wiring for the shared search framework
//! ([`ui::search`]): Contacts declares its searchable type by implementing
//! `SearchProvider`, turning matching address-book records and durable group
//! names into typed [`SearchResult`]s that any search surface can rank and
//! present alongside every other provider's hits. Each hit addresses an
//! exported `open` view action, so pointer, keyboard, assistive technology and
//! agents all enter through the same brokered operation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use ui::icons::Icon;
use ui::search::{CatalogSearchProvider, FieldWeight, SearchCatalog, SearchRecord, SearchResult};

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
    #[cfg(test)]
    before_publish: Arc<Mutex<Option<RefreshPublicationHook>>>,
    #[cfg(test)]
    full_refreshes: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
struct RefreshPublicationHook {
    built: std::sync::mpsc::Sender<()>,
    continue_after_build: std::sync::mpsc::Receiver<()>,
}

/// Only the public label needed to resolve a search hit's canonical `open`
/// action is retained beside the index. Full contacts remain authoritative in
/// `ContactsApp`/the CRDT and exact edit resources are materialised there.
#[derive(Default)]
struct ContactSearchLookup {
    contacts: BTreeMap<ContactId, String>,
    groups: BTreeMap<String, String>,
    /// Logical group identity -> observed display spellings and their counts.
    /// This is compact metadata, not a second contact projection.
    group_variants: BTreeMap<String, BTreeMap<String, usize>>,
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
            #[cfg(test)]
            before_publish: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            full_refreshes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        if let Err(error) = provider.refresh(store) {
            // A malformed collaborative projection must never bring down the
            // app while it is opening. `ContactsApp::sync` retries this
            // retained provider and surfaces the diagnostic as a toast.
            tracing::error!(%error, "Contacts search index could not initialise");
        }
        provider
    }

    /// Atomically replace the complete retained index. On error the prior
    /// generation and exact lookup remain intact, so Ctrl+K never advertises
    /// a partial book; callers can surface the diagnostic to the user.
    ///
    /// # Errors
    ///
    /// Returns a catalogue error when the collaborative projection contains
    /// malformed or duplicate canonical resource rows, leaving the last-good
    /// snapshot published.
    pub fn refresh(&self, store: &ContactStore) -> Result<(), ui::search::SearchCatalogError> {
        let mut lookup = ContactSearchLookup::default();
        let mut rows = Vec::with_capacity(store.contacts.len());
        for contact in &store.contacts {
            let result = contact_result(contact);
            lookup.contacts.insert(contact.id, contact.display_name());
            rows.push(contact_record(contact, result));
        }
        let mut ignored_affected_groups = BTreeSet::new();
        for contact in &store.contacts {
            update_group_variants(
                &mut lookup.group_variants,
                contact,
                true,
                &mut ignored_affected_groups,
            );
        }
        for (key, variants) in &lookup.group_variants {
            let Some(group) = variants.keys().next() else {
                continue;
            };
            let resource = crate::actions::group_resource_id(key);
            lookup.groups.insert(resource.clone(), group.clone());
            rows.push(group_record(resource, group));
        }
        // Build every posting and gram before taking the public snapshot
        // mutex. A 100k-contact refresh therefore leaves current searches and
        // exact actions responsive until one short, atomic generation swap.
        // A rejected replacement leaves the prior, last-good snapshot intact.
        let catalog = SearchCatalog::new(CONTACT_KIND);
        catalog.replace_all_records(rows)?;
        #[cfg(test)]
        let hook = self
            .before_publish
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        #[cfg(test)]
        if let Some(hook) = hook {
            let _ = hook.built.send(());
            let _ = hook.continue_after_build.recv();
        }
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // `SearchCatalog::new` starts at generation zero. Rebase the completed
        // replacement while holding the short publication boundary so a stale
        // cursor never accidentally matches a different snapshot generation.
        catalog.advance_after(snapshot.catalog.stats().generation)?;
        snapshot.catalog = catalog;
        snapshot.lookup = lookup;
        #[cfg(test)]
        self.full_refreshes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        drop(snapshot);
        Ok(())
    }

    /// Update the retained catalogue for one locally-applied contact change.
    /// The app only uses this when its in-memory CRDT projection was current
    /// before dispatch; merges and bulk imports retain the full, atomic
    /// [`Self::refresh`] path instead.
    ///
    /// # Errors
    ///
    /// Returns a catalogue error if advancing the retained index would exceed
    /// its generation space; callers then retain the prior searchable snapshot.
    #[allow(
        clippy::too_many_lines,
        reason = "the atomic preflight, bounded catalogue mutation, and lookup publication must remain together so their fail-closed ordering is reviewable"
    )]
    pub(crate) fn apply_contact_change(
        &self,
        before: Option<&Contact>,
        after: Option<&Contact>,
    ) -> Result<(), ui::search::SearchCatalogError> {
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut affected_groups = BTreeSet::new();
        if let Some(before) = before {
            collect_group_keys(before, &mut affected_groups);
        }
        if let Some(after) = after {
            collect_group_keys(after, &mut affected_groups);
        }
        let mut next_variants = affected_groups
            .iter()
            .filter_map(|key| {
                snapshot
                    .lookup
                    .group_variants
                    .get(key)
                    .cloned()
                    .map(|variants| (key.clone(), variants))
            })
            .collect::<BTreeMap<_, _>>();
        let mut ignored_groups = BTreeSet::new();
        if let Some(before) = before {
            update_group_variants(&mut next_variants, before, false, &mut ignored_groups);
        }
        if let Some(after) = after {
            update_group_variants(&mut next_variants, after, true, &mut ignored_groups);
        }

        let contact_resource = after
            .map(|contact| crate::actions::contact_resource_id(contact.id))
            .or_else(|| before.map(|contact| crate::actions::contact_resource_id(contact.id)));
        let mut group_updates = Vec::with_capacity(affected_groups.len());
        for key in &affected_groups {
            let resource = crate::actions::group_resource_id(key);
            let group = next_variants
                .get(key)
                .and_then(|variants| variants.keys().next().cloned());
            group_updates.push((key.clone(), resource, group));
        }

        let removing_contact = before.is_some() && after.is_none();
        if removing_contact
            && !snapshot
                .catalog
                .contains(contact_resource.as_deref().unwrap_or_default())
        {
            return Err(ui::search::SearchCatalogError::MissingResource);
        }
        if group_updates
            .iter()
            .any(|(_, resource, group)| group.is_none() && !snapshot.catalog.contains(resource))
        {
            return Err(ui::search::SearchCatalogError::MissingResource);
        }
        let inserts = usize::from(after.is_some_and(|_| {
            contact_resource
                .as_deref()
                .is_some_and(|resource| !snapshot.catalog.contains(resource))
        })) + group_updates
            .iter()
            .filter(|(_, resource, group)| group.is_some() && !snapshot.catalog.contains(resource))
            .count();
        snapshot.catalog.preflight_mutations(
            usize::from(contact_resource.is_some()) + group_updates.len(),
            inserts,
        )?;

        if let Some(after) = after {
            snapshot
                .catalog
                .upsert_record(contact_record(after, contact_result(after)))?;
        } else if let Some(resource) = &contact_resource
            && !snapshot.catalog.remove(resource)
        {
            return Err(ui::search::SearchCatalogError::MissingResource);
        }
        for (_, resource, group) in &group_updates {
            if let Some(group) = group {
                snapshot
                    .catalog
                    .upsert_record(group_record(resource.clone(), group))?;
            } else if !snapshot.catalog.remove(resource) {
                return Err(ui::search::SearchCatalogError::MissingResource);
            }
        }

        if let Some(before) = before {
            snapshot.lookup.contacts.remove(&before.id);
        }
        if let Some(after) = after {
            snapshot
                .lookup
                .contacts
                .insert(after.id, after.display_name());
        }
        for (key, resource, group) in group_updates {
            if let Some(variants) = next_variants.remove(&key) {
                snapshot.lookup.group_variants.insert(key, variants);
            } else {
                snapshot.lookup.group_variants.remove(&key);
            }
            if let Some(group) = group {
                snapshot.lookup.groups.insert(resource, group);
            } else {
                snapshot.lookup.groups.remove(&resource);
            }
        }
        drop(snapshot);
        Ok(())
    }

    #[cfg(test)]
    fn pause_before_publish(
        &self,
        built: std::sync::mpsc::Sender<()>,
        continue_after_build: std::sync::mpsc::Receiver<()>,
    ) {
        *self
            .before_publish
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(RefreshPublicationHook {
            built,
            continue_after_build,
        });
    }

    #[cfg(test)]
    fn stats(&self) -> ui::search::SearchCatalogStats {
        self.snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .stats()
    }

    #[cfg(test)]
    pub(crate) fn full_refreshes(&self) -> usize {
        self.full_refreshes
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

fn update_group_variants(
    variants: &mut BTreeMap<String, BTreeMap<String, usize>>,
    contact: &Contact,
    add: bool,
    affected: &mut BTreeSet<String>,
) {
    for entry in &contact.groups {
        let name = entry.value.trim();
        if name.is_empty() {
            continue;
        }
        let key = normalize_text(name);
        affected.insert(key.clone());
        if add {
            *variants
                .entry(key)
                .or_default()
                .entry(name.to_string())
                .or_default() += 1;
            continue;
        }
        let remove_key = {
            let Some(spellings) = variants.get_mut(&key) else {
                continue;
            };
            let Some(count) = spellings.get_mut(name) else {
                continue;
            };
            *count = count.saturating_sub(1);
            if *count == 0 {
                spellings.remove(name);
            }
            spellings.is_empty()
        };
        if remove_key {
            variants.remove(&key);
        }
    }
}

fn collect_group_keys(contact: &Contact, keys: &mut BTreeSet<String>) {
    for entry in &contact.groups {
        let name = entry.value.trim();
        if !name.is_empty() {
            keys.insert(normalize_text(name));
        }
    }
}

/// Every user-searchable field in a contact, weighted by how likely a
/// person is to look the contact up by it. Separate fields keep their
/// boundaries: a query never matches across two of them.
fn searchable_fields(contact: &Contact) -> Vec<(String, FieldWeight)> {
    let mut fields = vec![
        (contact.display_name(), FieldWeight::TITLE),
        (contact.first.clone(), FieldWeight::TITLE),
        (contact.last.clone(), FieldWeight::TITLE),
        (contact.org.clone(), FieldWeight::SUBTITLE),
        (contact.title.clone(), FieldWeight::SUBTITLE),
        (contact.website.clone(), FieldWeight::IDENTITY),
        (contact.notes.clone(), FieldWeight::BODY),
    ];
    if let Some(birthday) = contact.birthday {
        fields.extend([
            (birthday.iso(), FieldWeight::METADATA),
            (birthday.pretty(), FieldWeight::METADATA),
        ]);
    }
    for field in contact.phones.iter().chain(&contact.emails) {
        fields.extend([
            (field.label.clone(), FieldWeight::METADATA),
            (field.value.clone(), FieldWeight::IDENTITY),
        ]);
    }
    for address in &contact.addresses {
        fields.extend(
            [
                address.label.clone(),
                address.street.clone(),
                address.city.clone(),
                address.region.clone(),
                address.postal.clone(),
                address.country.clone(),
                address.one_line(),
            ]
            .map(|field| (field, FieldWeight::METADATA)),
        );
    }
    fields.extend(
        contact
            .groups
            .iter()
            .map(|group| (group.value.clone(), FieldWeight::METADATA)),
    );
    fields.retain(|(field, _)| !field.is_empty());
    fields
}

fn contact_record(contact: &Contact, result: SearchResult) -> SearchRecord {
    searchable_fields(contact).into_iter().fold(
        SearchRecord::new(crate::actions::contact_resource_id(contact.id), result),
        |record, (text, weight)| record.field(text, weight),
    )
}

fn group_record(resource: String, group: &str) -> SearchRecord {
    SearchRecord::new(resource, group_result(group))
        .field(group, FieldWeight::TITLE)
        .field(format!("group {group}"), FieldWeight::METADATA)
}

impl CatalogSearchProvider for ContactSearchProvider {
    fn provider_name(&self) -> &str {
        CONTACT_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(
            self.snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .catalog
                .clone(),
        )
    }

    fn resolve_action_address(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::Resource> {
        let snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let resolved = if let Some(raw_id) = address.resource.strip_prefix("contacts.contact:") {
            let id = raw_id.parse::<ContactId>().ok()?;
            let label = snapshot.lookup.contacts.get(&id).cloned()?;
            (address == &crate::actions::open_contact_address(id))
                .then(|| crate::actions::contact_open_resource(id, &label))
        } else {
            let group = snapshot.lookup.groups.get(&address.resource).cloned()?;
            (address == &crate::actions::open_group_address(&group))
                .then(|| crate::actions::group_resource(&group))
        };
        drop(snapshot);
        resolved
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
    use ui::search::testing::{LATE_HIT_NEEDLE, assert_late_hit_reachable};
    use ui::search::{DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT};
    use ui::search::{SearchProvider, SearchRegistry};
    use uuid::Uuid;

    fn store_with_samples() -> ContactStore {
        let mut store = ContactStore::new();
        for c in sample_contacts() {
            store.upsert(c);
        }
        store
    }

    /// Search finds contacts by any normalised field, including hidden and sensitive ones without leaking them, and ranks favourites.
    #[test]
    fn search_matches_every_field_privately_with_ranking() {
        // --- phase: provider_finds_a_real_contact_by_name ---
        {
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

        // --- phase: matches_a_field_the_title_does_not_show ---
        {
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

        // --- phase: indexes_every_contact_field_with_unicode_normalization ---
        {
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

        // --- phase: favourites_carry_a_ranking_bias ---
        {
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

        // --- phase: sensitive_search_terms_find_contacts_without_leaking_private_values ---
        {
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

    /// Results address the exported open action; the empty query browses contacts and groups; group hits resolve lazily.
    #[test]
    fn search_results_address_exported_contact_and_group_resources() {
        // --- phase: result_addresses_the_exported_open_action ---
        {
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

        // --- phase: empty_query_returns_contacts_and_durable_groups_as_one_result_group ---
        {
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

        // --- phase: group_search_uses_the_canonical_group_resource_and_resolves_lazily ---
        {
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
    }

    /// Paged search reaches contacts beyond the legacy limit with bounded allocation, clamped limits and opaque Unicode cursors.
    #[test]
    fn paged_search_reaches_late_contacts_within_bounds() {
        // --- phase: contacts_beyond_the_legacy_snapshot_limit_are_searchable ---
        {
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

        // --- phase: paged_catalogue_reaches_a_late_contact_without_unbounded_allocation ---
        {
            let (provider, hit) = assert_late_hit_reachable(
                |corpus| {
                    let contacts = corpus
                        .map(|row| Contact {
                            id: Uuid::from_u128(row.index as u128 + 1),
                            first: row.text,
                            ..Contact::default()
                        })
                        .collect();
                    ContactSearchProvider::new(&ContactStore { contacts })
                },
                3_000,
            );
            assert_eq!(hit.title, format!("ordinary record 2999 {LATE_HIT_NEEDLE}"));
            let page = provider.search_page(LATE_HIT_NEEDLE, None, 4, 1);
            assert_eq!(page.inspected, 1, "rare postings avoid scanning the book");
            assert_eq!(page.results.len(), 1, "one bounded result is materialised");
            assert!(page.next.is_none(), "the unique match is exhausted");
            let address = hit
                .action_address()
                .expect("catalogue hits use canonical open addresses");
            let resource = provider
                .resolve_action_resource(&address)
                .expect("the late hit resolves without expanding the contextual page");
            assert!(tilo_actions::door::resolve(&[resource], &address).is_some());
            assert_eq!(provider.stats().records, 3_000);
        }

        // --- phase: paged_search_clamps_untrusted_work_and_result_limits ---
        {
            let contacts = (0..(DEFAULT_SEARCH_RESULT_LIMIT + 64) as u128)
                .map(|index| Contact {
                    id: Uuid::from_u128(index + 1),
                    first: format!("Common bounded contact {index}"),
                    ..Contact::default()
                })
                .collect();
            let provider = ContactSearchProvider::new(&ContactStore { contacts });
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

        // --- phase: paged_unicode_query_continues_with_an_opaque_cursor ---
        {
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

    /// Catalogue snapshots publish atomically: failed or exhausted refreshes keep the last one, success invalidates cursors, and queries stay responsive.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn catalogue_snapshots_publish_atomically_off_lock() {
        // --- phase: failed_refresh_keeps_the_last_complete_catalogue_and_cursor ---
        {
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

        // --- phase: successful_refresh_invalidates_the_predecessor_cursor ---
        {
            let mut store = store_with_samples();
            let provider = ContactSearchProvider::new(&store);
            let cursor = provider
                .search_page("", None, 1, 1)
                .next
                .expect("the sample book has another browse result");
            store.contacts[0].first = "Updated Ada".into();
            provider
                .refresh(&store)
                .expect("valid complete replacement");
            assert!(
                !provider.action_directory_cursor_is_current(&cursor),
                "a complete replacement must advance the retained generation"
            );
        }

        // --- phase: exhausted_incremental_delta_keeps_the_published_snapshot_intact ---
        {
            let store = store_with_samples();
            let provider = ContactSearchProvider::new(&store);
            let before = store.contacts[0].clone();
            let address = crate::actions::open_contact_address(before.id);
            let old_hits = provider.search(&before.display_name());
            let old_resource = provider
                .resolve_action_resource(&address)
                .expect("the current contact resolves");
            {
                let snapshot = provider.snapshot.lock().expect("snapshot lock");
                snapshot
                    .catalog
                    .advance_after(u64::MAX - 1)
                    .expect("the final generation is representable");
            }
            let mut after = before.clone();
            after.first = "This update must not publish".into();

            assert!(matches!(
                provider.apply_contact_change(Some(&before), Some(&after)),
                Err(ui::search::SearchCatalogError::GenerationExhausted)
            ));
            assert_eq!(provider.search(&before.display_name()), old_hits);
            assert!(provider.search("this update must not publish").is_empty());
            let resource = provider
                .resolve_action_resource(&address)
                .expect("the prior exact resource remains available");
            assert_eq!(resource.id, old_resource.id);
            assert_eq!(resource.label, old_resource.label);
        }

        // --- phase: search_and_exact_resolution_share_the_snapshot_publication_boundary ---
        {
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

        // --- phase: queries_stay_responsive_while_a_new_generation_builds_off_lock ---
        {
            use std::sync::mpsc;

            let store = store_with_samples();
            let provider = ContactSearchProvider::new(&store);
            let grace = provider
                .search("grace")
                .into_iter()
                .next()
                .expect("current generation contains Grace");
            let grace_address = grace
                .action_address()
                .expect("current generation has an exact open address");

            let mut replacement = ContactStore::new();
            for index in 0..4_097_u128 {
                replacement.upsert(Contact {
                    id: Uuid::from_u128(index + 10_000),
                    first: format!("Replacement {index}"),
                    ..Contact::default()
                });
            }
            let (built_tx, built_rx) = mpsc::channel();
            let (continue_tx, continue_rx) = mpsc::channel();
            provider.pause_before_publish(built_tx, continue_rx);
            let refresh_provider = provider.clone();
            let refresh = std::thread::spawn(move || refresh_provider.refresh(&replacement));

            built_rx
                .recv()
                .expect("replacement postings were built before publication");
            // The refresh worker is paused immediately before it acquires the
            // snapshot mutex. Both requests must therefore complete against the
            // still-current generation instead of waiting behind O(n) rebuild.
            assert_eq!(provider.search("grace")[0].title, "Grace Hopper");
            assert!(provider.resolve_action_resource(&grace_address).is_some());

            continue_tx.send(()).expect("allow atomic publication");
            refresh
                .join()
                .expect("refresh worker joins")
                .expect("replacement index is valid");
            assert!(provider.search("grace").is_empty());
            assert_eq!(
                provider.search("replacement 4096")[0].title,
                "Replacement 4096"
            );
        }
    }
}

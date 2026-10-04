//! The vault in the platform search.
//!
//! An item is findable by its name, its username and its websites, and
//! nothing else: passwords, notes and custom field values never enter the
//! index. A hit runs the item's exported `open` action, so search, the
//! palette and agents show an item the same way. A `kind:` facet narrows a
//! query to logins, API keys, notes, Wi-Fi networks or other secrets.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use accounts::{SecretItem, SecretItemKind};
use ui::icons::Icon;
use ui::search::{CatalogSearchProvider, Facet, SearchCatalog, SearchQuery, SearchResult};

/// The result group the vault's hits appear under.
pub const ITEM_KIND: &str = "Passwords";

/// The facet key a query narrows by.
pub const KIND_FACET: &str = "kind";

const KINDS: [(SecretItemKind, &str, &str); 5] = [
    (SecretItemKind::Login, "login", "Login"),
    (SecretItemKind::ApiKey, "api-key", "API key"),
    (SecretItemKind::SecureNote, "note", "Secure note"),
    (SecretItemKind::Wifi, "wifi", "Wi-Fi"),
    (SecretItemKind::Other, "other", "Other"),
];

fn kind_slug(kind: SecretItemKind) -> &'static str {
    KINDS
        .iter()
        .find(|(candidate, ..)| *candidate == kind)
        .map_or("other", |(_, slug, _)| slug)
}

fn icon(kind: SecretItemKind) -> Icon {
    match kind {
        SecretItemKind::Login => Icon::Globe,
        SecretItemKind::ApiKey => Icon::Code,
        SecretItemKind::SecureNote => Icon::FileText,
        SecretItemKind::Wifi => Icon::Wifi,
        SecretItemKind::Other => Icon::Shield,
    }
}

/// A cheap handle to the vault's search catalogue.
#[derive(Clone)]
pub struct VaultSearchProvider {
    catalog: SearchCatalog,
    /// Resource id → kind slug, for the facet.
    kinds: Arc<RwLock<HashMap<String, &'static str>>>,
}

impl VaultSearchProvider {
    pub fn new() -> Self {
        Self {
            catalog: SearchCatalog::new(ITEM_KIND),
            kinds: Arc::default(),
        }
    }

    /// Index exactly `items` (metadata only), replacing what was there.
    pub(crate) fn replace(&self, items: &[SecretItem]) {
        let mut kinds = HashMap::new();
        let rows: Vec<(String, String, SearchResult)> = items
            .iter()
            .map(|item| {
                let address = crate::actions::open_item_address(&item.id);
                let resource = address.resource.clone();
                let mut result = SearchResult::for_action(ITEM_KIND, item.title.clone(), address)
                    .with_icon(icon(item.kind));
                let summary = crate::vault::summary(item);
                if !summary.is_empty() {
                    result = result.with_subtitle(summary);
                }
                if crate::vault::is_favorite(item) {
                    result = result.with_score(1);
                }
                let mut searchable = vec![item.title.as_str(), crate::vault::identity(item)];
                searchable.extend(item.origins.iter().map(String::as_str));
                kinds.insert(resource.clone(), kind_slug(item.kind));
                (resource, searchable.join("\0"), result)
            })
            .collect();
        if self.catalog.replace_all(rows).is_ok() {
            *self.kinds.write().unwrap_or_else(PoisonError::into_inner) = kinds;
        }
    }
}

impl Default for VaultSearchProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CatalogSearchProvider for VaultSearchProvider {
    fn provider_name(&self) -> &str {
        ITEM_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }

    fn facets(&self) -> Vec<Facet> {
        vec![
            KINDS
                .iter()
                .fold(Facet::new(KIND_FACET, "Kind"), |facet, (_, slug, label)| {
                    facet.value(*slug, *label)
                }),
        ]
    }

    fn admits(&self, hit: &SearchResult, query: &SearchQuery) -> bool {
        if query.filters().is_empty() {
            return true;
        }
        let kind = self
            .kinds
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&hit.anchor())
            .copied();
        kind.is_none_or(|kind| query.admits(KIND_FACET, &[kind]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchProvider;

    #[test]
    fn search_finds_names_usernames_and_sites_but_never_secrets() {
        let provider = VaultSearchProvider::new();
        let full = crate::demo::full_items();
        // The provider is fed the window's metadata cache; feed it the
        // full items to prove it would not index secrets even if given them.
        provider.replace(&full);

        for query in ["Northstar", "ada@example.com", "bank.example"] {
            let hits = provider.search(query);
            assert!(
                hits.iter().any(|hit| hit.title == "Northstar Bank"),
                "{query} finds the bank login"
            );
        }
        for secret in ["Harbor-Lights-2019", "4821-7730", "harbourlights"] {
            assert!(
                provider.search(secret).is_empty(),
                "{secret} must not be searchable"
            );
        }
        let hit = provider
            .search("Northstar")
            .into_iter()
            .next()
            .expect("a hit");
        assert_eq!(
            hit.action_address(),
            Some(crate::actions::open_item_address(&accounts::SecretId(
                "demo-bank".into()
            )))
        );
    }

    #[test]
    fn kind_facet_narrows_hits() {
        let provider = VaultSearchProvider::new();
        provider.replace(&crate::demo::items());
        let facets = SearchProvider::facets(&provider);
        let page = provider.search_page(&SearchQuery::parse("kind:wifi", &facets), None, 100, 100);
        let titles: Vec<&str> = page.results.iter().map(|hit| hit.title.as_str()).collect();
        assert_eq!(titles, ["Home Wi-Fi"]);
    }
}

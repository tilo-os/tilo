//! The vault in the platform search.
//!
//! An item is findable by its name, its username and its websites, and
//! nothing else: passwords, notes and custom field values never enter the
//! index. A hit is anchored at the item's row and opens it there, as an
//! agent's `open` of that anchor does. A `kind:` facet narrows a
//! query to logins, API keys, notes, Wi-Fi networks or other secrets.
//!
//! Every entry is secret-bearing: the system's search index keeps the
//! metadata after Passwords closes, and finds it only while Passwords
//! reports its vault unlocked ([`crate::vault::Core::secrets_unlocked`]).

use accounts::{SecretItem, SecretItemKind};
use ui::icons::Icon;
use ui::tilo_resource::{SearchEntry, SearchWeight};

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

pub(crate) fn icon(kind: SecretItemKind) -> Icon {
    match kind {
        SecretItemKind::Login => Icon::Globe,
        SecretItemKind::ApiKey => Icon::Code,
        SecretItemKind::SecureNote => Icon::FileText,
        SecretItemKind::Wifi => Icon::Wifi,
        SecretItemKind::Other => Icon::Shield,
    }
}

/// Resource search receives only item metadata, even when full secret-bearing
/// records are supplied. Secret and custom-field values never enter entries.
pub(crate) fn entries(items: &[SecretItem]) -> Vec<SearchEntry> {
    items
        .iter()
        .map(|item| {
            let mut entry = SearchEntry::new(item.title.clone(), String::new())
                .anchored(crate::actions::item_resource_id(&item.id.0))
                .grouped(ITEM_KIND)
                .with_icon(icon(item.kind).name())
                .facet(KIND_FACET, "Kind", kind_slug(item.kind))
                .field(crate::vault::identity(item), SearchWeight::Identity)
                .secret_bearing();
            let summary = crate::vault::summary(item);
            if !summary.is_empty() {
                entry = entry.subtitled(summary);
            }
            if crate::vault::is_favorite(item) {
                entry = entry.boosted(1);
            }
            for origin in &item.origins {
                entry = entry.field(origin, SearchWeight::Identity);
            }
            entry
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchProvider;

    fn providers(
        app: &mut ui::framework::testing::TestApp<crate::resource::PasswordsShell>,
    ) -> Vec<Box<dyn SearchProvider>> {
        ui::search::capability::search_providers(app.harness().root_widget())
    }
    #[test]
    fn search_finds_names_usernames_and_sites_but_never_secrets() {
        let metadata = format!("{:?}", entries(&crate::demo::full_items()));
        for secret in ["Harbor-Lights-2019", "4821-7730", "harbourlights"] {
            assert!(
                !metadata.contains(secret),
                "full records never publish secret metadata"
            );
        }
        let mut app =
            ui::framework::testing::TestApp::<crate::resource::PasswordsShell>::capture(None)
                .unwrap();
        let providers = providers(&mut app);
        assert_eq!(
            providers.len(),
            1,
            "the framework owns exactly one search source"
        );
        let provider = &providers[0];
        for query in ["Northstar", "ada@example.com", "bank.example"] {
            assert!(
                provider
                    .search(query)
                    .iter()
                    .any(|hit| hit.title == "Northstar Bank"),
                "{query} finds the login"
            );
        }
        for secret in ["Harbor-Lights-2019", "4821-7730", "harbourlights"] {
            assert!(
                provider.search(secret).is_empty(),
                "{secret} must not be searchable"
            );
        }
        assert!(
            entries(&crate::demo::full_items())
                .iter()
                .all(|entry| entry.secret),
            "every vault entry is secret-bearing, so the system index gates it on the lock"
        );
        let hit = provider
            .search("Northstar")
            .into_iter()
            .next()
            .expect("a hit");
        assert_eq!(
            hit.action_id,
            crate::actions::item_resource_id("demo-bank"),
            "a hit is anchored at the item's row"
        );
    }
    #[test]
    fn only_a_window_on_the_vault_reports_its_lock() {
        // The fixture captures show opens no vault: it says nothing.
        assert_eq!(crate::PasswordsSurface::demo().secrets_unlocked(), None);
        let closed = crate::vault::Core::with_model(crate::PasswordsModel::closed());
        assert_eq!(
            closed.secrets_unlocked(),
            None,
            "a past version says nothing"
        );
        let home = tempfile::tempdir().unwrap();
        let open = crate::vault::Core::with_model(crate::PasswordsModel::open_in(home.path()));
        assert_eq!(open.secrets_unlocked(), Some(open.recovery_error.is_none()));
    }

    #[test]
    fn kind_facet_narrows_hits() {
        let mut app =
            ui::framework::testing::TestApp::<crate::resource::PasswordsShell>::capture(None)
                .unwrap();
        let providers = providers(&mut app);
        let provider = &providers[0];
        let facets = SearchProvider::facets(provider.as_ref());
        let page = provider.search_page(
            &ui::search::SearchQuery::parse("kind:wifi", &facets),
            None,
            100,
            100,
        );
        let titles: Vec<_> = page.results.iter().map(|hit| hit.title.as_str()).collect();
        assert_eq!(titles, ["Home Wi-Fi"]);
    }
}

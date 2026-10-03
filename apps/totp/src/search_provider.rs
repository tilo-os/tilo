//! Authenticator accounts as a Tilo global-search source.
//!
//! Mirrors the reference wiring in `apps/contacts`: the authenticator
//! declares its searchable type by implementing `SearchProvider` over a
//! snapshot of the vault, so a 2FA account is findable from the unified Ctrl+K
//! palette by issuer or account name. Activating a row selects the account
//! (clearing any in-app filter so it is revealed).
//!
//! The provider is library-scoped and synchronous: it matches the in-memory
//! vault the app already holds — no secret or code generation enters the query
//! path. Every hit names the account resource's exported `open` View action.

use std::collections::BTreeMap;
use ui::icons::Icon;
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

use crate::model::{Account, Vault};

/// The kind label (and result-group header) the authenticator produces.
pub const ACCOUNT_KIND: &str = "Accounts";

/// A cheap handle to the complete, secret-free account catalogue retained by
/// the app. Cloning it for Ctrl+K is O(1).
#[derive(Clone)]
pub struct TotpSearchProvider {
    catalog: SearchCatalog,
}

impl TotpSearchProvider {
    /// Snapshot the current vault into a provider.
    pub fn new(vault: &Vault) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(ACCOUNT_KIND),
        };
        for account in &vault.accounts {
            provider.index_account(account);
        }
        provider
    }

    #[allow(
        clippy::expect_used,
        reason = "SearchResult::for_action is canonical and uses the same account resource id passed to SearchCatalog"
    )]
    fn index_account(&self, account: &Account) {
        let issuer = crate::actions::safe_identity_field(&account.issuer, &account.secret);
        let label = crate::actions::safe_identity_field(&account.label, &account.secret);
        let title = if issuer.is_empty() {
            if label.is_empty() { "Account" } else { label }
        } else {
            issuer
        };
        let mut result = SearchResult::for_action(
            ACCOUNT_KIND,
            title,
            crate::actions::open_account_address(account.id),
        )
        .with_icon(Icon::Shield);
        if !label.is_empty() && label != title {
            result = result.with_subtitle(label);
        }
        let address = result.action_address().expect("constructor is canonical");
        // Never index the secret or generated code: only user-visible identity.
        self.catalog
            .upsert(address.resource, format!("{issuer}\0{label}"), result)
            .expect("account action addresses its own resource");
    }

    pub(crate) fn update(&self, before: &Vault, after: &Vault) {
        let old: BTreeMap<_, _> = before.accounts.iter().map(|a| (a.id, a)).collect();
        let new: BTreeMap<_, _> = after.accounts.iter().map(|a| (a.id, a)).collect();
        for id in old.keys().filter(|id| !new.contains_key(*id)) {
            self.catalog
                .remove(&crate::actions::account_resource_id(*id));
        }
        for (id, account) in new {
            if old.get(&id).copied() != Some(account) {
                self.index_account(account);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
    }
}

impl CatalogSearchProvider for TotpSearchProvider {
    fn provider_name(&self) -> &str {
        ACCOUNT_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Vault;
    use ui::search::SearchProvider;
    fn populated() -> Vault {
        let mut v = Vault::new();
        for a in crate::model::demo_accounts() {
            v.upsert(a);
        }
        v
    }

    /// Search by issuer, empty-query browsing and the uncapped catalog.
    #[test]
    fn search_finds_accounts_and_browses_the_whole_vault() {
        // Case `provider_finds_an_account_by_issuer`.
        {
            let vault = populated();
            let provider = TotpSearchProvider::new(&vault);
            let hits = provider.search("github");
            assert!(
                hits.iter().any(|r| r.title == "GitHub"),
                "the provider surfaces the account"
            );
            assert!(hits.iter().all(|r| r.kind == ACCOUNT_KIND));
        }

        // Case `empty_query_browses_the_whole_vault`.
        {
            let vault = populated();
            let provider = TotpSearchProvider::new(&vault);
            assert_eq!(provider.search("").len(), vault.len());
        }

        // Case `complete_catalog_finds_an_account_beyond_the_former_snapshot_cap`.
        {
            let template = crate::model::demo_accounts().remove(0);
            let mut vault = Vault::new();
            for index in 0..4_500 {
                let mut account = template.clone();
                account.id = uuid::Uuid::new_v4();
                account.issuer = if index == 4_321 {
                    "Cobalt Orchid Account".into()
                } else {
                    format!("Account {index}")
                };
                account.order = index;
                vault.accounts.push(account);
            }
            let provider = TotpSearchProvider::new(&vault);
            let generation = provider.stats().generation;
            let palette_handle = provider.clone();
            let hits = palette_handle.search_limited("Cobalt Orchid", 4);
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].title, "Cobalt Orchid Account");
            assert_eq!(provider.stats().generation, generation);
        }
    }

    /// Hits open the exported view without legacy navigation, doc writes or secret-shaped labels.
    #[test]
    fn search_hits_open_the_exported_view_without_leaking_secrets() {
        // Case `every_hit_resolves_to_the_exported_open_view`.
        {
            use automerge::{AutoCommit, ROOT};
            use ui::widget::Widget;

            let mut app = crate::app::AuthenticatorApp::demo();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);
            let hits = ui::search::capability::search_providers(&app)[0].search("");
            assert!(!hits.is_empty());
            assert!(hits.iter().all(|hit| {
                hit.action_address().is_some_and(|address| {
                    ui::resources::resource(&app, &doc, &address.resource).is_some_and(|resource| {
                        tilo_actions::door::resolve(&[resource], &address).is_some_and(|spec| {
                            spec.kind == tilo_actions::OperationKind::View
                                && spec.contract == tilo_actions::ActionContract::view()
                        })
                    })
                })
            }));
        }

        // Case `brokered_search_reveals_account_without_legacy_navigation_or_doc_write`.
        {
            use automerge::{AutoCommit, ROOT};
            use ui::widget::Widget;

            let mut app = crate::app::AuthenticatorApp::demo();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);
            let target = ui::search::capability::search_providers(&app)[0]
                .search("")
                .into_iter()
                .nth(1)
                .expect("multiple demo accounts");
            let address = target.action_address().expect("canonical action address");
            let id = address
                .resource
                .strip_prefix("totp.account:")
                .expect("account resource")
                .parse()
                .expect("account uuid");
            let mut resources = ui::resources::resources(&app, &doc);
            resources.push(
                ui::resources::resource(&app, &doc, &address.resource)
                    .expect("canonical result lazily resolves"),
            );
            let mut palette = ui::ActionPalette::new();
            palette.load_open_resources(&resources);
            palette.set_providers(ui::search::capability::search_providers(&app));
            palette.open();
            palette.set_query(&target.title);
            let index = palette
                .result_labels()
                .iter()
                .position(|label| label == &target.title)
                .expect("target remains in brokered results");
            palette.set_cursor(index);
            let before = doc.get_heads();
            palette.activate_selected();
            let operation = palette.take_operation().expect("broker operation queued");
            assert_eq!(operation.kind, tilo_actions::OperationKind::View);
            assert!(palette.take_navigation().is_none());
            assert!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        operation.kind,
                        &operation
                            .encoded()
                            .expect("test requires prepared encoding")
                            .type_id,
                        &operation
                            .encoded()
                            .expect("test requires prepared encoding")
                            .payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .map(|started| started.is_some())
                .expect("Authenticator handles its exported operation")
            );
            assert_eq!(app.selected_account(), Some(id));
            assert_eq!(doc.get_heads(), before, "opening is presentation-only");
        }

        // Case `secret_shaped_identity_text_is_redacted_from_search_labels`.
        {
            let mut vault = Vault::new();
            let mut account = crate::model::demo_accounts().remove(0);
            account.issuer = format!("Login {}", account.secret);
            account.label = account.secret.clone();
            let secret = account.secret.clone();
            vault.upsert(account);
            let hit = TotpSearchProvider::new(&vault).search("").remove(0);
            assert!(!hit.title.contains(&secret));
            assert!(
                !hit.subtitle
                    .as_deref()
                    .is_some_and(|text| text.contains(&secret))
            );
        }
    }
}

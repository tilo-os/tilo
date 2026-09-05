//! Authenticator accounts as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: the authenticator
//! declares its searchable type by implementing [`SearchProvider`] over a
//! snapshot of the vault, so a 2FA account is findable from the unified Ctrl+K
//! palette by issuer or account name. Activating a row selects the account
//! (clearing any in-app filter so it is revealed).
//!
//! The provider is library-scoped and synchronous: it matches the in-memory
//! vault the app already holds — no secret or code generation enters the query
//! path. Every hit names the account resource's exported `open` View action.

use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
};

use crate::model::{Account, Vault};

/// The kind label (and result-group header) the authenticator produces.
pub const ACCOUNT_KIND: &str = "Accounts";

/// A [`SearchProvider`] over a snapshot of the vault. Rebuild it (cheap)
/// whenever the search surface opens.
pub struct TotpSearchProvider {
    accounts: Vec<Account>,
}

impl TotpSearchProvider {
    /// Snapshot the current vault into a provider.
    pub fn new(vault: &Vault) -> Self {
        Self {
            accounts: vault
                .ordered()
                .into_iter()
                .take(DEFAULT_SEARCH_INDEX_LIMIT)
                .collect(),
        }
    }
}

impl SearchProvider for TotpSearchProvider {
    fn name(&self) -> &str {
        ACCOUNT_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        // Match issuer + label (the account's title is the issuer, or the label
        // when the issuer is blank); the registry re-ranks by the title. An
        // empty query browses the whole vault.
        let q = query.to_lowercase();
        self.accounts
            .iter()
            .filter(|a| a.issuer.to_lowercase().contains(&q) || a.label.to_lowercase().contains(&q))
            .map(|a| {
                let issuer = crate::actions::safe_identity_field(&a.issuer, &a.secret);
                let label = crate::actions::safe_identity_field(&a.label, &a.secret);
                let title = if issuer.is_empty() {
                    if label.is_empty() { "Account" } else { label }
                } else {
                    issuer
                };
                let mut r = SearchResult::for_action(
                    ACCOUNT_KIND,
                    title,
                    crate::actions::open_account_address(a.id),
                )
                .with_icon(Icon::Shield);
                if !label.is_empty() && label != title {
                    r = r.with_subtitle(label);
                }
                r
            })
            .take(limit.min(DEFAULT_SEARCH_RESULT_LIMIT))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Vault;
    fn populated() -> Vault {
        let mut v = Vault::new();
        for a in crate::model::demo_accounts() {
            v.upsert(a);
        }
        v
    }

    #[test]
    fn provider_finds_an_account_by_issuer() {
        let vault = populated();
        let provider = TotpSearchProvider::new(&vault);
        let hits = provider.search("github");
        assert!(
            hits.iter().any(|r| r.title == "GitHub"),
            "the provider surfaces the account"
        );
        assert!(hits.iter().all(|r| r.kind == ACCOUNT_KIND));
    }

    #[test]
    fn empty_query_browses_the_whole_vault() {
        let vault = populated();
        let provider = TotpSearchProvider::new(&vault);
        assert_eq!(provider.search("").len(), vault.len());
    }

    #[test]
    fn every_hit_resolves_to_the_exported_open_view() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::AuthenticatorApp::demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let resources = app.resources(&doc);
        let hits = app.search_providers()[0].search("");
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|hit| {
            hit.action_address().is_some_and(|address| {
                qos_actions::door::resolve(&resources, &address).is_some_and(|spec| {
                    spec.kind == qos_actions::OperationKind::View
                        && spec.contract == qos_actions::ActionContract::view()
                })
            })
        }));
    }

    #[test]
    fn brokered_search_reveals_account_without_legacy_navigation_or_doc_write() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::AuthenticatorApp::demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let target = app.search_providers()[0]
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
        let resources = app.resources(&doc);
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.set_providers(app.search_providers());
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
        assert_eq!(operation.kind, qos_actions::OperationKind::View);
        assert!(palette.take_navigation().is_none());
        assert!(
            app.invoke_resource_operation(
                operation.kind,
                &operation.encoded.type_id,
                &operation.encoded.payload,
                &doc,
                &ROOT,
            )
            .expect("Authenticator handles its exported operation")
        );
        assert_eq!(app.selected_account(), Some(id));
        assert_eq!(doc.get_heads(), before, "opening is presentation-only");
    }

    #[test]
    fn secret_shaped_identity_text_is_redacted_from_search_labels() {
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

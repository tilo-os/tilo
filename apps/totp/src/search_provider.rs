//! Authenticator accounts as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: the authenticator
//! declares its searchable type by implementing [`SearchProvider`] over a
//! snapshot of the vault, so a 2FA account is findable from the unified Ctrl+K
//! palette by issuer or account name. Activating a row selects the account
//! (clearing any in-app filter so it is revealed).
//!
//! The provider is library-scoped and synchronous: it matches the in-memory
//! vault the app already holds — no code generation or network on the query
//! path.

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};

use crate::model::{Account, AccountId, Vault};

/// The kind label (and result-group header) the authenticator produces.
pub const ACCOUNT_KIND: &str = "Accounts";

/// The `action_id` prefix. A chosen row's `action_id` is `account:<uuid>`;
/// [`parse_action_id`] turns it back into an [`AccountId`].
const ACTION_PREFIX: &str = "account:";

/// A [`SearchProvider`] over a snapshot of the vault. Rebuild it (cheap)
/// whenever the search surface opens.
pub struct TotpSearchProvider {
    accounts: Vec<Account>,
}

impl TotpSearchProvider {
    /// Snapshot the current vault into a provider.
    pub fn new(vault: &Vault) -> Self {
        Self {
            accounts: vault.ordered(),
        }
    }

    /// The `action_id` for an account — what a chosen row hands back.
    pub fn action_id(id: AccountId) -> String {
        format!("{ACTION_PREFIX}{id}")
    }

    /// Resolve an `action_id` produced by this provider back into an
    /// [`AccountId`]. Returns `None` for an id this provider did not emit.
    pub fn parse_action_id(action_id: &str) -> Option<AccountId> {
        action_id
            .strip_prefix(ACTION_PREFIX)
            .and_then(|rest| rest.parse::<AccountId>().ok())
    }
}

impl SearchProvider for TotpSearchProvider {
    fn name(&self) -> &str {
        ACCOUNT_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        // Match issuer + label (the account's title is the issuer, or the label
        // when the issuer is blank); the registry re-ranks by the title. An
        // empty query browses the whole vault.
        let q = query.to_lowercase();
        self.accounts
            .iter()
            .filter(|a| a.issuer.to_lowercase().contains(&q) || a.label.to_lowercase().contains(&q))
            .map(|a| {
                let mut r = SearchResult::new(ACCOUNT_KIND, a.title(), Self::action_id(a.id))
                    .with_icon(Icon::Shield);
                if !a.label.trim().is_empty() && a.label != a.title() {
                    r = r.with_subtitle(a.label.clone());
                }
                r
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Vault;
    use ui::search::SearchRegistry;

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
    fn action_id_round_trips_to_a_stored_account() {
        let vault = populated();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(TotpSearchProvider::new(&vault)));
        let some = vault.ordered().into_iter().next().unwrap();
        let hit = registry
            .query("")
            .into_iter()
            .find(|r| r.action_id == TotpSearchProvider::action_id(some.id))
            .expect("the account is browsable through the registry");
        let id = TotpSearchProvider::parse_action_id(&hit.action_id).unwrap();
        assert!(
            vault.get(id).is_some(),
            "the id resolves to a real stored account"
        );
    }

    #[test]
    fn parse_action_id_rejects_foreign_ids() {
        assert!(TotpSearchProvider::parse_action_id("file:/etc/passwd").is_none());
        assert!(TotpSearchProvider::parse_action_id("account:not-a-uuid").is_none());
    }

    #[test]
    fn widget_hooks_find_then_select_the_account() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::AuthenticatorApp::demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        // Browse the app's OWN provider (its real ids) and take an account.
        let (action_id, id) = {
            let providers = app.search_providers();
            assert!(
                !providers.is_empty(),
                "the authenticator publishes a search provider"
            );
            let hit = providers[0]
                .search("")
                .into_iter()
                .next()
                .expect("the vault is browsable");
            let id = TotpSearchProvider::parse_action_id(&hit.action_id).unwrap();
            (hit.action_id, id)
        };

        app.activate_search_result(&action_id, &mut doc, &ROOT);
        assert_eq!(
            app.selected_account(),
            Some(id),
            "activation selected the chosen account"
        );
    }
}

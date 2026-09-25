//! Typed lifecycle actions for the current local desktop session.
//!
//! These are deliberately separate from ordinary searchable chrome actions:
//! logging out tears down every client in the session and must always retain a
//! destructive, confirm, at-most-once contract.  Headless development tools
//! can address it exactly without synthesising pointer input.

use std::rc::Rc;

use crate::shell::chrome_state::Chrome;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

pub const PROVIDER_ID: &str = "session";
pub const LOGOUT_ID: &str = "logout";

fn logout_contract() -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        permission: tilo_actions::PermissionRequirement::Admin,
        risk: tilo_actions::RiskLevel::Destructive,
        confirmation: tilo_actions::ConfirmationPolicy::Confirm,
        retry: tilo_actions::RetryPolicy::AtMostOnce,
        tags: vec!["desktop".into(), "session".into(), "logout".into()],
        ..tilo_actions::ActionContract::default()
    }
}

pub struct SessionActionsProvider;

impl SessionActionsProvider {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for SessionActionsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for SessionActionsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Session"
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let needle = ctx.needle.trim().to_ascii_lowercase();
        if !needle.is_empty()
            && !"log out".contains(&needle)
            && !"logout".contains(&needle)
            && !LOGOUT_ID.contains(&needle)
        {
            return Vec::new();
        }
        vec![SearchResult {
            kind: ResourceKind::Action,
            operation_kind: tilo_actions::OperationKind::Effect,
            provider_id: PROVIDER_ID.into(),
            id: LOGOUT_ID.into(),
            title: "Log out of Tilo".into(),
            subtitle: Some(
                "End this desktop session cleanly and return to the display manager".into(),
            ),
            badge: Some("Session".into()),
            score: if needle.is_empty() { 0 } else { 100 },
            params: Vec::new(),
            contract: Some(logout_contract()),
            on_open: Rc::new(|_doc, _root, _chrome: &Chrome, _args| {
                os_actions::request_logout()
                    .map_err(crate::shell::search::action_api::ActionError::Refused)
            }),
            on_invoke: None,
            on_begin_invoke: None,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::search::action_api::{ActionCatalog, AllVisible};
    use crate::shell::search::{SearchRegistry, SearchScope};
    use automerge::{AutoCommit, ROOT};

    #[test]
    fn logout_is_exactly_addressable_and_declares_destructive_confirmation() {
        let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(SessionActionsProvider::new()));
        let state = chrome.borrow();
        let catalog = ActionCatalog::build_exact(
            &registry,
            PROVIDER_ID,
            LOGOUT_ID,
            SearchScope::All,
            None,
            &state,
            &AutoCommit::new(),
            &ROOT,
            &AllVisible,
        );
        let action = catalog
            .enumerate()
            .into_iter()
            .next()
            .expect("logout action");
        let contract = action.contract.expect("logout contract");
        assert_eq!(contract.risk, tilo_actions::RiskLevel::Destructive);
        assert_eq!(
            contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert_eq!(contract.retry, tilo_actions::RetryPolicy::AtMostOnce);
    }
}

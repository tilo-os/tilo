//! Typed lifecycle actions for the current local desktop session.
//!
//! These are deliberately separate from ordinary searchable chrome actions:
//! logging out tears down every client in the session and must always retain a
//! destructive, confirm, at-most-once contract.  Headless development tools
//! can address it exactly without synthesising pointer input.

use std::cell::RefCell;
use std::rc::Rc;

use crate::shell::chrome_state::Chrome;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

pub const PROVIDER_ID: &str = "session";
pub const LOGOUT_ID: &str = "logout";

/// One admission path for every shell-owned session lifecycle entry point.
///
/// The visual power menu and typed action provider must not disagree about a
/// portal inhibitor: both hold a clone backed by the compositor's registry.
#[derive(Clone, Debug, Default)]
pub struct SessionActionPolicy {
    inhibitors: Rc<RefCell<tilo_power_mgmt::InhibitRegistry>>,
}

impl SessionActionPolicy {
    pub fn new(inhibitors: tilo_power_mgmt::InhibitRegistry) -> Self {
        Self {
            inhibitors: Rc::new(RefCell::new(inhibitors)),
        }
    }

    /// Point every holder of this policy at the registry exported by the
    /// compositor's portal backend.
    pub fn attach(&self, inhibitors: tilo_power_mgmt::InhibitRegistry) {
        *self.inhibitors.borrow_mut() = inhibitors;
    }

    fn inhibit_scope(action: os_actions::SessionAction) -> Option<u32> {
        match action {
            os_actions::SessionAction::Logout => Some(tilo_power_mgmt::inhibit::INHIBIT_LOGOUT),
            os_actions::SessionAction::SwitchUser => {
                Some(tilo_power_mgmt::inhibit::INHIBIT_USER_SWITCH)
            }
            _ => None,
        }
    }

    /// Refuse inhibited actions before publishing an edge or touching the OS.
    ///
    /// # Errors
    ///
    /// The user-facing reason when a portal inhibitor blocks `action`.
    pub fn admit(&self, action: os_actions::SessionAction) -> Result<(), String> {
        let Some(scope) = Self::inhibit_scope(action) else {
            return Ok(());
        };
        let inhibitors = self.inhibitors.borrow();
        if !inhibitors.is_inhibited_for(scope) {
            return Ok(());
        }
        let reason = inhibitors
            .active_reasons_for(scope)
            .into_iter()
            .next()
            .unwrap_or_else(|| "An app has unsaved work".into());
        Err(format!(
            "{reason} — close the app or finish its work first."
        ))
    }

    /// Admit and synchronously publish logout's private request + local edge.
    /// Other admitted actions are returned for the caller's worker thread.
    ///
    /// # Errors
    ///
    /// The inhibitor reason from [`Self::admit`], or the failure to write the
    /// wrapper's private logout request.
    pub fn begin(&self, action: os_actions::SessionAction) -> Result<(), String> {
        self.admit(action)?;
        if action == os_actions::SessionAction::Logout {
            os_actions::request_logout()?;
        }
        Ok(())
    }
}

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

pub struct SessionActionsProvider {
    policy: SessionActionPolicy,
}

impl SessionActionsProvider {
    pub fn new(policy: SessionActionPolicy) -> Self {
        Self { policy }
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
        let policy = self.policy.clone();
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
            on_open: Rc::new(move |_doc, _root, _chrome: &Chrome, _args| {
                policy
                    .begin(os_actions::SessionAction::Logout)
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
        registry.register(Rc::new(SessionActionsProvider::new(
            SessionActionPolicy::new(tilo_power_mgmt::InhibitRegistry::new()),
        )));
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

    #[test]
    fn typed_logout_policy_refuses_the_same_portal_inhibitor_as_the_power_menu() {
        let inhibitors = tilo_power_mgmt::InhibitRegistry::new();
        let _hold = inhibitors.acquire_for(
            "Document has unsaved work",
            tilo_power_mgmt::inhibit::INHIBIT_LOGOUT,
        );
        let policy = SessionActionPolicy::new(inhibitors);

        let error = policy
            .begin(os_actions::SessionAction::Logout)
            .expect_err("typed logout must stop before publishing its request edge");
        assert!(error.contains("Document has unsaved work"));
        assert!(error.contains("finish its work"));
        assert!(
            !os_actions::take_logout_request(),
            "refusal must not publish the compositor logout edge"
        );
    }

    #[test]
    fn logout_inhibitor_does_not_block_unrelated_session_actions() {
        let inhibitors = tilo_power_mgmt::InhibitRegistry::new();
        let _hold = inhibitors.acquire_for(
            "Document has unsaved work",
            tilo_power_mgmt::inhibit::INHIBIT_LOGOUT,
        );
        let policy = SessionActionPolicy::new(inhibitors);

        assert!(policy.admit(os_actions::SessionAction::SwitchUser).is_ok());
        assert!(policy.admit(os_actions::SessionAction::Suspend).is_ok());
    }
}

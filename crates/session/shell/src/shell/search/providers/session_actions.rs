//! Typed lifecycle actions for the current local desktop session.
//!
//! These are deliberately separate from ordinary searchable chrome actions:
//! logging out tears down every client in the session and must always retain a
//! destructive, confirm, at-most-once contract.  Headless development tools
//! can address it exactly without synthesising pointer input.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::shell::chrome_state::Chrome;
use crate::shell::i18n::{t, t_args};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

pub const PROVIDER_ID: &str = "session";
pub const LOGOUT_ID: &str = "logout";

/// How a system action appears in the action catalogue on a shell that
/// does or does not own the machine: available, or unavailable because the
/// host system manages it.
pub(crate) fn system_action_availability(
    surfaces: os_actions::SystemSurfaces,
) -> tilo_actions::Availability {
    if surfaces.is_owned() {
        tilo_actions::Availability::Available
    } else {
        tilo_actions::Availability::Unavailable {
            code: os_actions::HOST_MANAGED_CODE.into(),
            reason: os_actions::HOST_MANAGED_REASON.into(),
        }
    }
}

/// One admission path for every shell-owned session lifecycle entry point.
///
/// The visual power menu and typed action provider must not disagree about a
/// portal inhibitor, or about whether this shell owns the machine at all:
/// both hold a clone backed by the compositor's registry and its answer.
#[derive(Clone, Debug)]
pub struct SessionActionPolicy {
    inhibitors: Rc<RefCell<tilo_power_mgmt::InhibitRegistry>>,
    surfaces: Rc<Cell<os_actions::SystemSurfaces>>,
}

impl Default for SessionActionPolicy {
    fn default() -> Self {
        Self::new(tilo_power_mgmt::InhibitRegistry::default())
    }
}

impl SessionActionPolicy {
    /// A policy for this process, owning the machine's session actions only
    /// when the compositor established that it owns the machine.
    pub fn new(inhibitors: tilo_power_mgmt::InhibitRegistry) -> Self {
        Self {
            inhibitors: Rc::new(RefCell::new(inhibitors)),
            surfaces: Rc::new(Cell::new(os_actions::owner::current())),
        }
    }

    /// The same policy, and every clone of it, for a shell that does or
    /// does not own the machine.
    #[must_use]
    pub fn with_surfaces(self, surfaces: os_actions::SystemSurfaces) -> Self {
        self.surfaces.set(surfaces);
        self
    }

    /// Whether this shell owns the machine's session actions.
    pub fn surfaces(&self) -> os_actions::SystemSurfaces {
        self.surfaces.get()
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

    /// Refuse inhibited actions, and every action on a shell that does not
    /// own the machine, before publishing an edge or touching the OS.
    ///
    /// # Errors
    ///
    /// [`os_actions::HOST_MANAGED_REASON`] on a shell that does not own the
    /// machine, else the user-facing reason when a portal inhibitor blocks
    /// `action`.
    pub fn admit(&self, action: os_actions::SessionAction) -> Result<(), String> {
        self.surfaces().require()?;
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
            .unwrap_or_else(|| t("search-session-unsaved-work"));
        Err(t_args(
            "search-session-inhibited",
            &[("reason", reason.into())],
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

fn logout_contract(surfaces: os_actions::SystemSurfaces) -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        availability: system_action_availability(surfaces),
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

    fn display_name(&self) -> String {
        crate::shell::i18n::t("search-provider-session")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let needle = ctx.needle.trim().to_lowercase();
        let title = t("search-session-logout");
        if !needle.is_empty()
            && !title.to_lowercase().contains(&needle)
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
            title,
            subtitle: Some(t("search-session-logout-detail")),
            badge: Some(t("search-badge-session")),
            score: if needle.is_empty() { 0 } else { 100 },
            params: Vec::new(),
            contract: Some(logout_contract(policy.surfaces())),
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
    fn logout_is_addressable_confirmed_and_inhibited_only_like_the_power_menu() {
        // Phase `logout_is_exactly_addressable_and_declares_destructive_confirmation`.
        {
            let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
            let mut registry = SearchRegistry::new();
            registry.register(Rc::new(SessionActionsProvider::new(
                SessionActionPolicy::new(tilo_power_mgmt::InhibitRegistry::new())
                    .with_surfaces(os_actions::SystemSurfaces::Owned),
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

        // Phase `typed_logout_policy_refuses_the_same_portal_inhibitor_as_the_power_menu`.
        {
            let inhibitors = tilo_power_mgmt::InhibitRegistry::new();
            let _hold = inhibitors.acquire_for(
                "Document has unsaved work",
                tilo_power_mgmt::inhibit::INHIBIT_LOGOUT,
            );
            let policy = SessionActionPolicy::new(inhibitors)
                .with_surfaces(os_actions::SystemSurfaces::Owned);

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

        // Phase `logout_inhibitor_does_not_block_unrelated_session_actions`.
        {
            let inhibitors = tilo_power_mgmt::InhibitRegistry::new();
            let _hold = inhibitors.acquire_for(
                "Document has unsaved work",
                tilo_power_mgmt::inhibit::INHIBIT_LOGOUT,
            );
            let policy = SessionActionPolicy::new(inhibitors)
                .with_surfaces(os_actions::SystemSurfaces::Owned);

            assert!(policy.admit(os_actions::SessionAction::SwitchUser).is_ok());
            assert!(policy.admit(os_actions::SessionAction::Suspend).is_ok());
        }
    }

    #[test]
    fn a_shell_that_does_not_own_the_machine_offers_logout_only_as_host_managed() {
        let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
        let policy = SessionActionPolicy::new(tilo_power_mgmt::InhibitRegistry::new())
            .with_surfaces(os_actions::SystemSurfaces::Hosted);
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(SessionActionsProvider::new(policy.clone())));
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
            .expect("logout stays discoverable");
        assert_eq!(
            action.contract.expect("logout contract").availability,
            tilo_actions::Availability::Unavailable {
                code: os_actions::HOST_MANAGED_CODE.into(),
                reason: os_actions::HOST_MANAGED_REASON.into(),
            }
        );
        for action in [
            os_actions::SessionAction::Lock,
            os_actions::SessionAction::Logout,
            os_actions::SessionAction::SwitchUser,
            os_actions::SessionAction::Suspend,
            os_actions::SessionAction::Restart,
            os_actions::SessionAction::ShutDown,
        ] {
            assert_eq!(
                policy.begin(action),
                Err(os_actions::HOST_MANAGED_REASON.to_owned()),
                "{action:?}"
            );
        }
        assert!(!os_actions::take_logout_request());
    }
}

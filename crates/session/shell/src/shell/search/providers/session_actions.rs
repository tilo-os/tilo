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
use crate::shell::search::action_api::{ActionParam, ParamType};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use ui::search::FieldWeight;

pub const PROVIDER_ID: &str = "session";
pub const LOGOUT_ID: &str = "logout";

/// The power flyout's other rows, by action id: they share its labels, and
/// the host's session executor runs them from the chrome queue.
type QueuedAction = (
    &'static str,
    os_actions::SessionAction,
    &'static str,
    &'static str,
);

const QUEUED_ACTIONS: [QueuedAction; 5] = [
    (
        "lock",
        os_actions::SessionAction::Lock,
        "power-lock",
        "power-lock-detail",
    ),
    (
        "switch-user",
        os_actions::SessionAction::SwitchUser,
        "power-switch-user",
        "power-switch-user-detail",
    ),
    (
        "suspend",
        os_actions::SessionAction::Suspend,
        "power-suspend",
        "power-suspend-detail",
    ),
    (
        "restart",
        os_actions::SessionAction::Restart,
        "power-restart",
        "power-restart-detail",
    ),
    (
        "shut-down",
        os_actions::SessionAction::ShutDown,
        "power-shut-down",
        "power-shut-down-detail",
    ),
];

/// One admission path for every shell-owned session lifecycle entry point.
///
/// The visual power menu and typed action provider must not disagree about a
/// portal inhibitor: both hold a clone backed by the compositor's registry.
#[derive(Clone, Debug, Default)]
pub struct SessionActionPolicy {
    inhibitors: Rc<RefCell<tilo_power_mgmt::InhibitRegistry>>,
    /// Whether the display manager can open a greeter beside this session.
    /// Without it, Switch user is offered by no entry point.
    user_switching: Rc<Cell<bool>>,
    /// Set for a session that does not own the seat (headless, nested): the
    /// machine belongs to another desktop, which alone may suspend, restart
    /// or shut it down, or switch its user.
    detached_from_host: Rc<Cell<bool>>,
}

impl SessionActionPolicy {
    pub fn new(inhibitors: tilo_power_mgmt::InhibitRegistry) -> Self {
        Self {
            inhibitors: Rc::new(RefCell::new(inhibitors)),
            user_switching: Rc::default(),
            detached_from_host: Rc::default(),
        }
    }

    pub fn detach_from_host(&self) {
        self.detached_from_host.set(true);
    }

    /// Point every holder of this policy at the registry exported by the
    /// compositor's portal backend.
    pub fn attach(&self, inhibitors: tilo_power_mgmt::InhibitRegistry) {
        *self.inhibitors.borrow_mut() = inhibitors;
    }

    /// The session's inhibitor registry, shared with every holder.
    pub fn inhibitors(&self) -> tilo_power_mgmt::InhibitRegistry {
        self.inhibitors.borrow().clone()
    }

    pub fn set_user_switching(&self, supported: bool) {
        self.user_switching.set(supported);
    }

    /// Whether this session offers `action` at all.
    pub fn offers(&self, action: os_actions::SessionAction) -> bool {
        use os_actions::SessionAction::{Restart, ShutDown, Suspend, SwitchUser};
        match action {
            SwitchUser => self.user_switching.get() && !self.detached_from_host.get(),
            Suspend | Restart | ShutDown => !self.detached_from_host.get(),
            _ => true,
        }
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
        if self.detached_from_host.get() && !self.offers(action) {
            return Err(t("search-session-not-offered"));
        }
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

fn queued_contract(id: &str, action: os_actions::SessionAction) -> tilo_actions::ActionContract {
    let tags = vec!["desktop".into(), "session".into(), id.into()];
    match action {
        os_actions::SessionAction::Lock => tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Undoable,
            risk: tilo_actions::RiskLevel::Low,
            confirmation: tilo_actions::ConfirmationPolicy::None,
            retry: tilo_actions::RetryPolicy::Idempotent,
            tags,
            ..tilo_actions::ActionContract::default()
        },
        os_actions::SessionAction::Suspend => tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Outside,
            risk: tilo_actions::RiskLevel::Low,
            confirmation: tilo_actions::ConfirmationPolicy::Confirm,
            retry: tilo_actions::RetryPolicy::AtMostOnce,
            tags,
            ..tilo_actions::ActionContract::default()
        },
        _ => tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Outside,
            risk: tilo_actions::RiskLevel::Destructive,
            confirmation: tilo_actions::ConfirmationPolicy::Confirm,
            retry: tilo_actions::RetryPolicy::AtMostOnce,
            tags,
            ..tilo_actions::ActionContract::default()
        },
    }
}

fn logout_contract() -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Outside,
        risk: tilo_actions::RiskLevel::Destructive,
        confirmation: tilo_actions::ConfirmationPolicy::Confirm,
        retry: tilo_actions::RetryPolicy::AtMostOnce,
        tags: vec!["desktop".into(), "session".into(), "logout".into()],
        ..tilo_actions::ActionContract::default()
    }
}

/// What a tier earns for a session row. Lock, restart and shut down end
/// what the person is doing, so they come up only for their name, or for a
/// word they are known by typed from its start: never because a keyword is
/// misspelt into something else, or holds the query in the middle.
fn session_ranks(tier: ui::search::MatchTier, weight: FieldWeight) -> Option<i32> {
    use ui::search::MatchTier;
    if weight == FieldWeight::KEYWORD
        && matches!(
            tier,
            MatchTier::Typo | MatchTier::WordPrefix | MatchTier::Word | MatchTier::Contains
        )
    {
        return None;
    }
    tier.rank_in(weight)
}

pub struct SessionActionsProvider {
    policy: SessionActionPolicy,
    /// The rows' searchable text, indexed while it stays the same.
    ranker: ui::search::ListIndex,
    /// The rows and their searchable text, built once for what they are
    /// built from ([`BuiltFrom`]): a keystroke only searches them.
    built: RefCell<Option<Built>>,
}

/// Which session actions are offered, the desktop's language, and the
/// settings document whose shortcuts the rows show, and whether the local
/// administrator may inspect retained-home retirement.
type BuiltFrom = (
    Vec<bool>,
    String,
    Option<settings_store::SettingsStamp>,
    bool,
);

struct Built {
    from: BuiltFrom,
    rows: Vec<SearchResult>,
    fields: Vec<Vec<(String, FieldWeight)>>,
}

impl SessionActionsProvider {
    pub fn new(policy: SessionActionPolicy) -> Self {
        Self {
            policy,
            ranker: ui::search::ListIndex::default().with_ranks(session_ranks),
            built: RefCell::default(),
        }
    }

    fn built_from(&self, chrome: &crate::shell::chrome_state::ChromeState) -> BuiltFrom {
        #[cfg(target_os = "linux")]
        let retirement = retirement_admin(chrome);
        #[cfg(not(target_os = "linux"))]
        let retirement = {
            let _ = chrome;
            false
        };
        (
            QUEUED_ACTIONS
                .iter()
                .map(|(_, action, ..)| self.policy.offers(*action))
                .collect(),
            crate::shell::i18n::current_locale(),
            settings_store::stamp(),
            retirement,
        )
    }

    fn build(&self, from: BuiltFrom) -> Built {
        let mut rows: Vec<SearchResult> = QUEUED_ACTIONS
            .iter()
            .zip(&from.0)
            .filter(|(_, offered)| **offered)
            .map(|(&entry, _)| self.queued_result(entry))
            .collect();
        rows.insert(1.min(rows.len()), self.logout_result());
        #[cfg(target_os = "linux")]
        if from.3 {
            rows.push(retirement_result());
        }
        let shortcuts = settings_store::hardware::ShortcutConfig::load();
        for row in &mut rows {
            row.shortcut = super::actions::action_shortcut(&shortcuts, &row.id);
        }
        let fields = rows
            .iter()
            .map(|row| {
                let mut fields = vec![
                    (row.title.to_lowercase(), FieldWeight::TITLE),
                    (row.id.clone(), FieldWeight::IDENTITY),
                    (
                        row.subtitle.clone().unwrap_or_default().to_lowercase(),
                        FieldWeight::SUBTITLE,
                    ),
                ];
                // Lock, restart, shut down: the other words for each, in
                // every language.
                fields.extend(
                    crate::shell::i18n::words_in_every_locale(&format!(
                        "{}session-{}",
                        crate::shell::i18n::KEYWORD_PREFIX,
                        row.id
                    ))
                    .into_iter()
                    .map(|word| (word, FieldWeight::KEYWORD)),
                );
                fields
            })
            .collect();
        Built { from, rows, fields }
    }
}

impl SearchProvider for SessionActionsProvider {
    fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        use ui::icons::Icon;
        Some(match result.id.as_str() {
            "lock" => Icon::Lock,
            "switch-user" => Icon::Users,
            "suspend" => Icon::Moon,
            "restart" => Icon::RotateCcw,
            "shut-down" => Icon::Power,
            LOGOUT_ID => Icon::ArrowLeft,
            _ => return None,
        })
    }
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
        let from = self.built_from(ctx.chrome);
        let mut built = self.built.borrow_mut();
        if built.as_ref().is_none_or(|built| built.from != from) {
            *built = Some(self.build(from));
        }
        let Some(Built { rows, fields, .. }) = built.as_ref() else {
            return Vec::new();
        };
        crate::shell::search::rank_kept_rows(&self.ranker, ctx.needle.trim(), rows, fields)
    }
}

/// A confirmed session action is invocable only with `confirm: true`; the
/// executor refuses an argument the action does not declare.
fn confirmation_params(contract: &tilo_actions::ActionContract) -> Vec<ActionParam> {
    match contract.confirmation {
        tilo_actions::ConfirmationPolicy::Confirm => vec![ActionParam::required(
            "confirm",
            ParamType::Boolean,
            "Explicitly confirm this session action",
        )],
        _ => Vec::new(),
    }
}

impl SessionActionsProvider {
    fn queued_result(&self, entry: QueuedAction) -> SearchResult {
        let (id, action, title_key, detail_key) = entry;
        let contract = queued_contract(id, action);
        let policy = self.policy.clone();
        SearchResult {
            shortcut: None,
            kind: ResourceKind::Action,
            operation_kind: tilo_actions::OperationKind::Effect,
            provider_id: PROVIDER_ID.into(),
            id: id.into(),
            title: t(title_key),
            subtitle: Some(t(detail_key)),
            badge: None,
            score: 0,
            params: confirmation_params(&contract),
            contract: Some(contract),
            on_open: Rc::new(move |_doc, _root, chrome: &Chrome, _args| {
                policy
                    .admit(action)
                    .map_err(crate::shell::search::action_api::ActionError::Refused)?;
                chrome.borrow_mut().request_session_action(action);
                Ok(())
            }),
            on_invoke: None,
            on_begin_invoke: None,
        }
    }

    fn logout_result(&self) -> SearchResult {
        let contract = logout_contract();
        let policy = self.policy.clone();
        SearchResult {
            shortcut: None,
            kind: ResourceKind::Action,
            operation_kind: tilo_actions::OperationKind::Effect,
            provider_id: PROVIDER_ID.into(),
            id: LOGOUT_ID.into(),
            title: t("search-session-logout"),
            subtitle: Some(t("search-session-logout-detail")),
            badge: None,
            score: 0,
            params: confirmation_params(&contract),
            contract: Some(contract),
            on_open: Rc::new(move |_doc, _root, _chrome: &Chrome, _args| {
                policy
                    .begin(os_actions::SessionAction::Logout)
                    .map_err(crate::shell::search::action_api::ActionError::Refused)
            }),
            on_invoke: None,
            on_begin_invoke: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::search::action_api::{ActionCatalog, AllVisible, ArgValue, ResourceRef};
    use crate::shell::search::{SearchRegistry, SearchScope};
    use automerge::{AutoCommit, ROOT};
    use tilo_actions::{ConfirmationPolicy as Confirm, RiskLevel as Risk};

    fn confirmed() -> (String, ArgValue) {
        ("confirm".into(), ArgValue::Boolean(true))
    }

    fn exact_catalog(policy: &SessionActionPolicy, chrome: &Chrome, id: &str) -> ActionCatalog {
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(SessionActionsProvider::new(policy.clone())));
        let state = chrome.borrow();
        ActionCatalog::build_exact(
            &registry,
            &ResourceRef::new(PROVIDER_ID, id),
            SearchScope::All,
            None,
            &state,
            &AutoCommit::new(),
            &ROOT,
            &AllVisible,
        )
    }

    #[test]
    fn every_power_flyout_row_is_a_typed_action_queued_for_the_session_executor() {
        // Phase `each_row_is_exactly_addressable_with_a_contract_matching_its_risk`.
        {
            let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
            let policy = SessionActionPolicy::new(tilo_power_mgmt::InhibitRegistry::new());
            policy.set_user_switching(true);
            for (id, risk, confirmation) in [
                ("lock", Risk::Low, Confirm::None),
                ("suspend", Risk::Low, Confirm::Confirm),
                ("switch-user", Risk::Destructive, Confirm::Confirm),
                ("restart", Risk::Destructive, Confirm::Confirm),
                ("shut-down", Risk::Destructive, Confirm::Confirm),
            ] {
                let actions = exact_catalog(&policy, &chrome, id).enumerate();
                let [action] = actions.as_slice() else {
                    panic!("{id}: expected exactly one action, got {}", actions.len());
                };
                let contract = action.contract.as_ref().expect("session contract");
                assert_eq!(contract.risk, risk, "{id}");
                assert_eq!(contract.confirmation, confirmation, "{id}");
            }
        }

        // Phase `switch_user_is_offered_only_where_the_display_manager_supports_it`.
        {
            let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
            let policy = SessionActionPolicy::new(tilo_power_mgmt::InhibitRegistry::new());
            assert!(
                exact_catalog(&policy, &chrome, "switch-user")
                    .enumerate()
                    .is_empty()
            );
            policy.set_user_switching(true);
            assert_eq!(
                exact_catalog(&policy, &chrome, "switch-user")
                    .enumerate()
                    .len(),
                1
            );
        }

        // Phase `a_session_detached_from_the_host_never_offers_its_power`.
        {
            let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
            let policy = SessionActionPolicy::new(tilo_power_mgmt::InhibitRegistry::new());
            policy.set_user_switching(true);
            policy.detach_from_host();
            for id in ["suspend", "switch-user", "restart", "shut-down"] {
                assert!(
                    exact_catalog(&policy, &chrome, id).enumerate().is_empty(),
                    "{id} must not be offered by a headless or nested session"
                );
            }
            for action in [
                os_actions::SessionAction::Suspend,
                os_actions::SessionAction::SwitchUser,
                os_actions::SessionAction::Restart,
                os_actions::SessionAction::ShutDown,
            ] {
                assert!(policy.admit(action).is_err(), "{action:?} admitted");
            }
            assert_eq!(exact_catalog(&policy, &chrome, "lock").enumerate().len(), 1);
            assert!(policy.admit(os_actions::SessionAction::Lock).is_ok());
            assert!(policy.admit(os_actions::SessionAction::Logout).is_ok());

            let mut menu = crate::shell::power_menu::PowerMenu::for_session(true);
            let offered = menu.row_count();
            menu.detach_from_host();
            assert_eq!(
                menu.row_count(),
                offered - 4,
                "the flyout drops suspend, switch user, restart and shut down"
            );
        }
    }

    #[test]
    fn invoking_a_session_action_queues_it_and_honours_inhibits() {
        // Phase `invoking_queues_for_the_executor_and_actuates_nothing_itself`.
        {
            let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
            let policy = SessionActionPolicy::new(tilo_power_mgmt::InhibitRegistry::new());
            let mut doc = AutoCommit::new();
            let catalog = exact_catalog(&policy, &chrome, "restart");
            let restart = catalog
                .select(&ResourceRef::new(PROVIDER_ID, "restart"))
                .expect("exact session action");
            assert!(
                restart
                    .invoke(&mut ui::widget::Edits::lend(&mut doc), &ROOT, &chrome, [])
                    .is_err(),
                "a destructive session action needs explicit confirmation"
            );
            assert!(
                chrome
                    .borrow_mut()
                    .take_session_action_requests()
                    .is_empty()
            );
            restart
                .invoke(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &ROOT,
                    &chrome,
                    [confirmed()],
                )
                .expect("confirmed restart is admitted");
            exact_catalog(&policy, &chrome, "lock")
                .select(&ResourceRef::new(PROVIDER_ID, "lock"))
                .expect("exact session action")
                .invoke(&mut ui::widget::Edits::lend(&mut doc), &ROOT, &chrome, [])
                .expect("lock needs no confirmation");
            assert_eq!(
                chrome.borrow_mut().take_session_action_requests(),
                vec![
                    os_actions::SessionAction::Restart,
                    os_actions::SessionAction::Lock
                ]
            );
            assert!(
                !chrome.borrow_mut().take_session_lock_request(),
                "a typed lock reaches the executor, not the lock edge directly"
            );
        }

        // Phase `an_inhibited_action_is_refused_before_it_is_queued`.
        {
            let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
            let inhibitors = tilo_power_mgmt::InhibitRegistry::new();
            let _hold = inhibitors.acquire_for(
                "Call is sharing the screen",
                tilo_power_mgmt::inhibit::INHIBIT_USER_SWITCH,
            );
            let policy = SessionActionPolicy::new(inhibitors);
            policy.set_user_switching(true);
            let error = exact_catalog(&policy, &chrome, "switch-user")
                .select(&ResourceRef::new(PROVIDER_ID, "switch-user"))
                .expect("exact session action")
                .invoke(
                    &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
                    &ROOT,
                    &chrome,
                    [confirmed()],
                )
                .expect_err("an inhibitor refuses the typed action like the flyout row");
            assert!(format!("{error:?}").contains("Call is sharing the screen"));
            assert!(
                chrome
                    .borrow_mut()
                    .take_session_action_requests()
                    .is_empty()
            );
        }
    }

    #[test]
    fn the_words_people_type_find_their_session_action() {
        let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
        let policy = SessionActionPolicy::new(tilo_power_mgmt::InhibitRegistry::new());
        policy.set_user_switching(true);
        let provider = SessionActionsProvider::new(policy);
        let state = chrome.borrow();
        let doc = AutoCommit::new();
        for (needle, id) in [
            ("lock", "lock"),
            ("sign out", LOGOUT_ID),
            ("logout", LOGOUT_ID),
            ("shutdown", "shut-down"),
            ("power off", "shut-down"),
            ("turn off", "shut-down"),
            ("reboot", "restart"),
            ("sleep", "suspend"),
            ("switch user", "switch-user"),
        ] {
            let mut rows = provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle,
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &ROOT,
            });
            rows.sort_by_key(|row| std::cmp::Reverse(row.score));
            assert_eq!(
                rows.first().map(|row| row.id.as_str()),
                Some(id),
                "{needle}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn retirement_rows_leave_the_cached_index_when_the_session_locks() {
        let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
        let provider = SessionActionsProvider::new(SessionActionPolicy::new(
            tilo_power_mgmt::InhibitRegistry::new(),
        ));
        let mut from = provider.built_from(&chrome.borrow());
        // Seed the previously admitted administrator's cached rows. Locking
        // must remove the row even if language and shortcuts have not changed.
        from.3 = true;
        let built = provider.build(from);
        assert!(built.rows.iter().any(|row| row.id == "retained-retirement"));
        assert_eq!(built.rows.len(), built.fields.len());
        *provider.built.borrow_mut() = Some(built);
        chrome.borrow_mut().set_session_locked(true);
        let doc = AutoCommit::new();
        let rows = provider.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "",
            wanted: &[ResourceKind::Action],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &ROOT,
        });
        assert!(rows.iter().all(|row| row.id != "retained-retirement"));
        assert!(!provider.built.borrow().as_ref().unwrap().from.3);
    }

    #[test]
    fn logout_is_addressable_confirmed_and_inhibited_only_like_the_power_menu() {
        // Phase `logout_is_exactly_addressable_and_declares_destructive_confirmation`.
        {
            let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
            let mut registry = SearchRegistry::new();
            registry.register(Rc::new(SessionActionsProvider::new(
                SessionActionPolicy::new(tilo_power_mgmt::InhibitRegistry::new()),
            )));
            let state = chrome.borrow();
            let catalog = ActionCatalog::build_exact(
                &registry,
                &ResourceRef::new(PROVIDER_ID, LOGOUT_ID),
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
            assert!(
                action
                    .params
                    .iter()
                    .any(|param| param.name.as_ref() == "confirm"),
                "a confirmed contract must declare the argument that satisfies it"
            );
        }

        // Phase `typed_logout_policy_refuses_the_same_portal_inhibitor_as_the_power_menu`.
        {
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

        // Phase `logout_inhibitor_does_not_block_unrelated_session_actions`.
        {
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
}

#[cfg(target_os = "linux")]
fn retirement_admin(chrome: &crate::shell::chrome_state::ChromeState) -> bool {
    if chrome.session_locked() {
        return false;
    }
    let Ok(store) = crate::shell::identity_provisioning::DiskIdentityStore::from_env() else {
        return false;
    };
    store.path().parent().is_some_and(|path| {
        crate::compositor::admin_control::authentication_session_decision(path).is_ok()
    })
}
#[cfg(target_os = "linux")]
fn retirement_result() -> SearchResult {
    SearchResult {
        shortcut: None,
        kind: ResourceKind::Action,
        operation_kind: tilo_actions::OperationKind::View,
        provider_id: PROVIDER_ID.into(),
        id: "retained-retirement".into(),
        title: t("session-retained-retirement"),
        subtitle: Some(t("session-retained-retirement-detail")),
        badge: None,
        score: 0,
        params: Vec::new(),
        contract: Some(tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Read,
            result: tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({
                    "type":"object", "additionalProperties":false,
                    "required":["schema","entries"],
                    "properties":{
                        "schema":{"type":"integer","const":1},
                        "entries":{"type":"array","maxItems":32,"items":{"type":"object"}}
                    }
                }),
            },
            ..tilo_actions::ActionContract::default()
        }),
        on_open: Rc::new(|_, _, _, _| Ok(())),
        on_invoke: Some(Rc::new(|_, _, chrome, _| {
            if !retirement_admin(&chrome.borrow()) {
                return Err(crate::shell::search::action_api::ActionError::Explained(t(
                    "portal-auth-local-admin-required",
                )));
            }
            let value =
                crate::shell::launcher::retained_home::retirement::snapshot().map_err(|error| {
                    crate::shell::search::action_api::ActionError::Refused(error.to_string())
                })?;
            Ok(crate::shell::search::ActionReceipt {
                broker_invocation_id: None,
                update: tilo_actions::InvocationUpdate::Succeeded {
                    invocation: tilo_actions::InvocationId::new(),
                    output: tilo_actions::ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                },
            })
        })),
        on_begin_invoke: None,
    }
}

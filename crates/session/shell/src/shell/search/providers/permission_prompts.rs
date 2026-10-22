//! Answers to existing session-local portal prompts, never arbitrary grants.
use std::rc::Rc;

use crate::shell::agent_admission::MemberChoice;
use crate::shell::chrome_state::PromptRequest;
use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::i18n::t;
use crate::shell::overlay_host::{
    is_active_workspace_owner, prompt_choice_to_outcome, prompt_label,
};
use crate::shell::search::action_api::{ActionError, ActionParam, ParamType};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use ui::widget::PromptChoice;

pub const PROVIDER_ID: &str = "permission-prompts";

/// Common native/semantic decision path. Persistence remains owned by the
/// waiting portal or ush request, outside the collaborative document.
pub(crate) fn answer_pending_prompt(
    state: &mut ChromeState,
    id: uuid::Uuid,
    choice: PromptChoice,
) -> Result<(), ActionError> {
    answer_pending_prompt_as(state, id, choice, None)
}

/// [`answer_pending_prompt`], admitting an agent door client as `member`
/// when the person named one.
pub(crate) fn answer_pending_prompt_as(
    state: &mut ChromeState,
    id: uuid::Uuid,
    choice: PromptChoice,
    member: Option<MemberChoice>,
) -> Result<(), ActionError> {
    if state.pending_grants.front().is_none_or(|p| p.id != id) {
        return Err(ActionError::Explained(t("permission-request-expired")));
    }
    if choice == PromptChoice::Always && !is_active_workspace_owner(state) {
        return Err(ActionError::Explained(t(
            "permission-request-permanent-owner-required",
        )));
    }
    if let Some(mut pending) = state.pending_grants.pop_front() {
        pending.answer_as(prompt_choice_to_outcome(choice), member);
        state.bump();
    }
    Ok(())
}

/// A decision's parameters: always the confirmation; for an agent door
/// client also which agent it is admitted as, and a new agent's preset.
fn prompt_params(agent_admission: bool) -> Vec<ActionParam> {
    let mut params = vec![ActionParam::required_owned(
        "confirm",
        ParamType::Boolean,
        t("permission-request-confirm"),
    )];
    if agent_admission {
        params.push(ActionParam::optional_owned(
            "agent",
            ParamType::Text,
            t("agent-door-param-agent"),
        ));
        params.push(ActionParam::optional_owned(
            "preset",
            ParamType::Text,
            t("agent-door-param-preset"),
        ));
    }
    params
}

/// The agent the person named, if they named one.
fn member_choice(
    args: &crate::shell::search::action_api::ActionArgs,
) -> Result<Option<MemberChoice>, ActionError> {
    let preset = args
        .text("preset")
        .map(|value| {
            tilo_action_policy::roles::AgentPreset::parse(value)
                .ok_or_else(|| ActionError::Explained(t("agent-door-preset-invalid")))
        })
        .transpose()?;
    Ok(args.text("agent").map(|agent| MemberChoice {
        agent: agent.to_owned(),
        preset,
    }))
}

/// The address word of one answer.
fn decision_id(choice: PromptChoice) -> &'static str {
    match choice {
        PromptChoice::Allow => "allow-once",
        PromptChoice::Session => "allow-session",
        PromptChoice::Always => "allow-always",
        PromptChoice::Deny => "deny",
    }
}

#[derive(Default)]
pub struct PermissionPromptsProvider {
    ranker: ui::search::ListIndex,
}

impl SearchProvider for PermissionPromptsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }
    fn name(&self) -> &'static str {
        "Permission requests"
    }
    fn display_name(&self) -> String {
        crate::shell::i18n::t("search-provider-permission-prompts")
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) || !is_active_workspace_owner(ctx.chrome) {
            return Vec::new();
        }
        let Some(pending) = ctx.chrome.pending_grants.front() else {
            return Vec::new();
        };
        let prompt_id = pending.id;
        let agent_admission = matches!(pending.request, PromptRequest::AgentAdmission(_));
        // The answers the prompt offers, as it names them.
        let offered: Vec<(PromptChoice, String)> = match &pending.request {
            PromptRequest::DesktopControl(ask) => ask.choices(),
            _ => [
                PromptChoice::Allow,
                PromptChoice::Session,
                PromptChoice::Always,
                PromptChoice::Deny,
            ]
            .into_iter()
            .map(|choice| (choice, ui::widget::permission_prompt::button_label(choice)))
            .collect(),
        };
        let rows: Vec<_> = offered
            .into_iter()
            .map(|(choice, title)| (decision_id(choice), choice, title))
            .map(|(decision, choice, title)| SearchResult {
                facts: Vec::new(),
                shortcut: None,
                kind: ResourceKind::Action,
                operation_kind: tilo_actions::OperationKind::Effect,
                provider_id: PROVIDER_ID.into(),
                id: format!("{decision}@{prompt_id}"),
                title,
                subtitle: Some(format!(
                    "{}: {}",
                    pending.app_id,
                    prompt_label(&pending.request)
                )),
                badge: Some(self.display_name()),
                score: 0,
                params: prompt_params(agent_admission),
                contract: Some(tilo_actions::ActionContract {
                    kind: tilo_actions::ActionKind::Outside,
                    risk: tilo_actions::RiskLevel::Destructive,
                    confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                    retry: tilo_actions::RetryPolicy::AtMostOnce,
                    ..tilo_actions::ActionContract::default()
                }),
                on_open: Rc::new(move |_doc, _root, chrome: &Chrome, args| {
                    let mut state = chrome.borrow_mut();
                    if !is_active_workspace_owner(&state) {
                        return Err(ActionError::Explained(t(
                            "permission-request-owner-required",
                        )));
                    }
                    let member = member_choice(args)?;
                    answer_pending_prompt_as(&mut state, prompt_id, choice, member)
                }),
                on_invoke: None,
                on_begin_invoke: None,
            })
            .collect();
        let fields: Vec<_> = rows
            .iter()
            .map(|row| {
                vec![
                    (row.title.clone(), ui::search::FieldWeight::TITLE),
                    (row.id.clone(), ui::search::FieldWeight::IDENTITY),
                    (PROVIDER_ID.to_string(), ui::search::FieldWeight::IDENTITY),
                    (self.display_name(), ui::search::FieldWeight::SUBTITLE),
                    (
                        row.subtitle.clone().unwrap_or_default(),
                        ui::search::FieldWeight::SUBTITLE,
                    ),
                ]
            })
            .collect();
        crate::shell::search::rank_rows(&self.ranker, ctx.needle.trim(), rows, &fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::PendingPrompt;
    use crate::shell::search::action_api::{ActionCatalog, AllVisible, ArgValue, ResourceRef};
    use crate::shell::search::{SearchRegistry, SearchScope};
    use crate::shell::state::{Capability, GRANT_ALWAYS, WorkspaceTree};
    use automerge::{AutoCommit, ROOT};

    fn pending(
        chrome: &Chrome,
    ) -> (
        uuid::Uuid,
        tokio::sync::oneshot::Receiver<crate::portal::PromptOutcome>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let request = PendingPrompt::with_reply(
            "so.tilo.terminal".into(),
            Capability::HostShell {
                scope: GRANT_ALWAYS,
            },
            tx,
        );
        let id = request.id;
        chrome.borrow_mut().pending_grants.push_back(request);
        (id, rx)
    }
    fn catalog(chrome: &Chrome, id: &str) -> ActionCatalog {
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(PermissionPromptsProvider::default()));
        ActionCatalog::build_exact(
            &registry,
            &ResourceRef::new(PROVIDER_ID, id),
            SearchScope::All,
            None,
            &chrome.borrow(),
            &AutoCommit::new(),
            &ROOT,
            &AllVisible,
        )
    }
    #[test]
    fn exact_confirmed_owner_decisions_answer_only_the_waiting_request() {
        for (decision, expected) in [
            ("allow-once", crate::portal::PromptOutcome::AllowOnce),
            ("allow-session", crate::portal::PromptOutcome::AllowSession),
            ("allow-always", crate::portal::PromptOutcome::AllowAlways),
            ("deny", crate::portal::PromptOutcome::Deny),
        ] {
            let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
            chrome.borrow_mut().test_only_owner_override = Some(true);
            let (id, mut rx) = pending(&chrome);
            let action_id = format!("{decision}@{id}");
            let actions = catalog(&chrome, &action_id);
            let action = actions
                .select(&ResourceRef::new(PROVIDER_ID, &action_id))
                .expect("pending decision");
            let contract = action.contract().expect("contract");
            assert_eq!(contract.kind, tilo_actions::ActionKind::Outside);
            assert_eq!(contract.retry, tilo_actions::RetryPolicy::AtMostOnce);
            let mut doc = AutoCommit::new();
            assert!(
                action
                    .invoke(&mut ui::widget::Edits::lend(&mut doc), &ROOT, &chrome, [])
                    .is_err()
            );
            assert!(rx.try_recv().is_err());
            action
                .invoke(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &ROOT,
                    &chrome,
                    [("confirm".into(), ArgValue::Boolean(true))],
                )
                .expect("owner decision");
            assert_eq!(
                rx.try_recv().expect("actual waiting portal response"),
                expected
            );
            assert!(chrome.borrow().pending_grants.is_empty());
            assert!(
                action
                    .invoke(
                        &mut ui::widget::Edits::lend(&mut doc),
                        &ROOT,
                        &chrome,
                        [("confirm".into(), ArgValue::Boolean(true))]
                    )
                    .is_err()
            );
            assert!(catalog(&chrome, &action_id).enumerate().is_empty());
        }
    }
    #[test]
    fn provider_query_discovers_the_current_head_without_exposing_queued_successors() {
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        chrome.borrow_mut().test_only_owner_override = Some(true);
        let (head, _head_rx) = pending(&chrome);
        let (successor, _next_rx) = pending(&chrome);
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(PermissionPromptsProvider::default()));
        let state = chrome.borrow();
        let actions = ActionCatalog::build(
            &registry,
            PROVIDER_ID,
            SearchScope::All,
            None,
            &state,
            &AutoCommit::new(),
            &ROOT,
            &AllVisible,
        )
        .enumerate();
        assert_eq!(actions.len(), 4);
        for action in actions {
            assert!(action.resource.id.ends_with(&head.to_string()));
            assert!(!action.resource.id.contains(&successor.to_string()));
        }
    }

    #[test]
    fn stale_same_app_and_lost_owner_callbacks_cannot_grant() {
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        chrome.borrow_mut().test_only_owner_override = Some(true);
        let (id, _rx) = pending(&chrome);
        let action_id = format!("allow-always@{id}");
        let actions = catalog(&chrome, &action_id);
        let action = actions
            .select(&ResourceRef::new(PROVIDER_ID, &action_id))
            .expect("pending action");
        let mut doc = AutoCommit::new();
        chrome.borrow_mut().test_only_owner_override = Some(false);
        assert!(catalog(&chrome, &action_id).enumerate().is_empty());
        assert!(
            action
                .invoke(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &ROOT,
                    &chrome,
                    [("confirm".into(), ArgValue::Boolean(true))]
                )
                .is_err()
        );
        assert_eq!(
            chrome
                .borrow()
                .pending_grants
                .front()
                .expect("still pending")
                .id,
            id
        );
        chrome.borrow_mut().test_only_owner_override = Some(true);
        chrome.borrow_mut().pending_grants.pop_front();
        let (new_id, mut new_rx) = pending(&chrome);
        assert_ne!(id, new_id);
        assert!(
            action
                .invoke(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &ROOT,
                    &chrome,
                    [("confirm".into(), ArgValue::Boolean(true))]
                )
                .is_err()
        );
        assert!(new_rx.try_recv().is_err());
        assert_eq!(
            chrome
                .borrow()
                .pending_grants
                .front()
                .expect("new pending")
                .id,
            new_id
        );
    }
}

//! The existing local administrator-authentication sheet, through Search/Actions.
use crate::shell::portal_dialog::authentication_actions::AuthenticationTicket;
use crate::shell::search::action_api::{ActionError, ActionParam, ParamType};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use crate::shell::{chrome_state::ChromeState, i18n::t, portal_dialog::PortalDialogQueue};
use std::rc::Rc;
pub(crate) const PROVIDER_ID: &str = "authentication-prompts";
pub(crate) struct AuthenticationPromptsProvider {
    queue: PortalDialogQueue,
    ranker: ui::search::ListIndex,
    first_use: Rc<dyn Fn() -> bool>,
}
impl AuthenticationPromptsProvider {
    pub(crate) fn new(queue: PortalDialogQueue) -> Self {
        Self::with_first_use_scope(queue, Rc::new(|| false))
    }
    pub(crate) fn with_first_use_scope(
        queue: PortalDialogQueue,
        first_use: Rc<dyn Fn() -> bool>,
    ) -> Self {
        Self {
            queue,
            first_use,
            ranker: ui::search::ListIndex::default(),
        }
    }
}
fn current_admin(chrome: &ChromeState, first_use: &dyn Fn() -> bool) -> bool {
    if chrome.session_locked() {
        return false;
    }
    let Ok(store) = crate::shell::identity_provisioning::DiskIdentityStore::from_env() else {
        return false;
    };
    let Some(directory) = store.path().parent() else {
        return false;
    };
    crate::compositor::admin_control::control_session_decision(directory, first_use()).is_ok()
}
fn answer_params(ticket: &AuthenticationTicket) -> Vec<ActionParam> {
    vec![
        ActionParam::required_owned("identity", ParamType::Text, t("portal-auth-identity-param"))
            .with_choices(ticket.request().identities.clone()),
        ActionParam::required_owned("secret", ParamType::Secret, ticket.request().prompt.clone()),
    ]
}
// Both console and palette dispatch the same attempt, with typed Secret binding.
fn deliver(
    queue: &PortalDialogQueue,
    ticket: &AuthenticationTicket,
    cancel: bool,
    args: &tilo_actions::ActionArgs,
) -> bool {
    if cancel {
        queue.cancel_authentication(ticket)
    } else {
        match (args.text("identity"), args.text("secret")) {
            (Some(identity), Some(secret)) => {
                queue.answer_authentication(ticket, identity, secret.to_owned())
            }
            _ => false,
        }
    }
}
fn delivery_receipt(
    queue: &PortalDialogQueue,
    ticket: &AuthenticationTicket,
    cancel: bool,
    args: &tilo_actions::ActionArgs,
) -> Result<crate::shell::search::ActionReceipt, ActionError> {
    if !deliver(queue, ticket, cancel, args) {
        return Err(ActionError::Explained(t("portal-auth-request-expired")));
    }
    // Completion is delivery to the real waiting exchange, never PAM success.
    Ok(crate::shell::search::ActionReceipt::completed())
}

impl SearchProvider for AuthenticationPromptsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }
    fn name(&self) -> &'static str {
        "Administrator authentication"
    }
    fn display_name(&self) -> String {
        t("search-provider-authentication-prompts")
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action)
            || !current_admin(ctx.chrome, &*self.first_use)
        {
            return Vec::new();
        }
        let Some(ticket) = self.queue.current_authentication() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for cancel in [false, true] {
            let queue = self.queue.clone();
            let selected = ticket.clone();
            let params = if cancel {
                Vec::new()
            } else {
                answer_params(&ticket)
            };
            let first_use = self.first_use.clone();
            let invoke: Rc<crate::shell::search::InvokeFn> =
                Rc::new(move |_doc, _root, chrome, args| {
                    if !current_admin(&chrome.borrow(), &*first_use) {
                        return Err(ActionError::Explained(t(
                            "portal-auth-local-admin-required",
                        )));
                    }
                    delivery_receipt(&queue, &selected, cancel, args)
                });
            let open_invoke = invoke.clone();
            rows.push(SearchResult {
                kind: ResourceKind::Action,
                operation_kind: tilo_actions::OperationKind::Effect,
                provider_id: PROVIDER_ID.into(),
                id: format!(
                    "{}@{}",
                    if cancel { "cancel" } else { "answer" },
                    ticket.generation
                ),
                title: t(if cancel {
                    "portal-cancel"
                } else {
                    "portal-authenticate"
                }),
                subtitle: Some(format!(
                    "{} ({})",
                    ticket.request().message,
                    ticket.request().action_id
                )),
                badge: Some(self.display_name()),
                shortcut: None,
                score: 0,
                params,
                contract: Some(tilo_actions::ActionContract {
                    kind: tilo_actions::ActionKind::Outside,
                    risk: tilo_actions::RiskLevel::Privileged,
                    retry: tilo_actions::RetryPolicy::AtMostOnce,
                    ..tilo_actions::ActionContract::default()
                }),
                on_open: Rc::new(move |doc, root, chrome, args| {
                    open_invoke(doc, root, chrome, args).map(|_| ())
                }),
                on_invoke: Some(invoke),
                on_begin_invoke: None,
            });
        }
        let fields: Vec<_> = rows
            .iter()
            .map(|row| {
                vec![
                    (row.title.clone(), ui::search::FieldWeight::TITLE),
                    (
                        row.subtitle.clone().unwrap_or_default(),
                        ui::search::FieldWeight::SUBTITLE,
                    ),
                    (PROVIDER_ID.into(), ui::search::FieldWeight::IDENTITY),
                    (row.id.clone(), ui::search::FieldWeight::IDENTITY),
                ]
            })
            .collect();
        crate::shell::search::rank_rows(&self.ranker, ctx.needle.trim(), rows, &fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::search::action_api::{ActionArgs, ArgValue};

    #[test]
    fn real_waiter_binds_offered_account_and_secret_and_refuses_reused_cookie_ticket() {
        let queue = PortalDialogQueue::default();
        let request = || crate::shell::portal_dialog::AuthenticationRequest {
            handle: "polkit:semantic-retry".into(),
            message: "Install".into(),
            action_id: "org.example.install".into(),
            identities: vec!["admin".into()],
            identity: 0,
            prompt: "Password:".into(),
            echo: false,
            notice: None,
        };
        let mut reply = queue.enqueue_authentication(request()).unwrap();
        let ticket = queue.current_authentication().unwrap();
        let params = answer_params(&ticket);
        assert_eq!(params[1].ty, ParamType::Secret);
        assert!(
            ActionArgs::bind(
                &params,
                [
                    ("identity".into(), ArgValue::Text("admin".into())),
                    ("secret".into(), ArgValue::Text("private".into()))
                ]
            )
            .is_err()
        );
        assert!(
            ActionArgs::bind(
                &params,
                [
                    ("identity".into(), ArgValue::Text("other".into())),
                    ("secret".into(), ArgValue::Secret("private".into()))
                ]
            )
            .is_err()
        );
        let bound = ActionArgs::bind(
            &params,
            [
                ("identity".into(), ArgValue::Text("admin".into())),
                ("secret".into(), ArgValue::Secret("private".into())),
            ],
        )
        .unwrap();
        assert!(!format!("{bound:?}").contains("private"));
        let receipt = delivery_receipt(&queue, &ticket, false, &bound).unwrap();
        assert_eq!(
            reply.try_recv().unwrap(),
            crate::shell::portal_dialog::AuthenticationDecision::Answered {
                identity: 0,
                secret: "private".into()
            }
        );
        // The canonical local receipt says only that the real waiting exchange
        // received the answer; it contains no credential or PAM outcome.
        assert!(!serde_json::to_string(&receipt).unwrap().contains("private"));
        assert!(receipt.broker_invocation_id.is_none());
        let mut retry = queue.enqueue_authentication(request()).unwrap();
        assert!(!deliver(&queue, &ticket, false, &bound));
        assert!(!deliver(&queue, &ticket, true, &ActionArgs::none()));
        assert!(retry.try_recv().is_err());
        let current = queue.current_authentication().unwrap();
        assert_ne!(current.generation, ticket.generation);
        assert!(deliver(&queue, &current, true, &ActionArgs::none()));
        assert_eq!(
            retry.try_recv().unwrap(),
            crate::shell::portal_dialog::AuthenticationDecision::Cancelled
        );
    }
}

//! The signed keeper's permission for their personal assistant to work on the workspace host.
//! A conversation shows what the host currently allows and sends only the keeper's chosen request.

use tilo_actions::remote::InvokeArgument;
use tilo_actions::{InvocationUpdate, MemberId};
use tilo_server_protocol::agents::{
    AgentGrantAction, AgentGrantReceipt, AgentGrantStatus, GrantState,
};
use ui::prelude::*;
use ui::widget::arrange::Stack;
use ui::{Arrival, InvocationCall, RemoteActions};

use crate::Chat;

mod apps;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Conversation,
    Manage,
    ChooseApps,
}

#[derive(Clone)]
struct Request {
    call: InvocationCall,
    action: AgentGrantAction,
}

impl PartialEq for Request {
    fn eq(&self, other: &Self) -> bool {
        self.call.state().same_state(&other.call.state())
    }
}

#[derive(Clone)]
struct Access {
    remote: RemoteActions,
    keeper: MemberId,
    agent: String,
    selected: LocalState<Option<String>>,
    request: LocalState<Request>,
    notice: LocalState<String>,
    panel: LocalState<Panel>,
    comparison_code: LocalState<Option<String>>,
}

fn call(
    remote: &RemoteActions,
    agent: &str,
    action: AgentGrantAction,
    argument: InvokeArgument,
) -> Request {
    Request {
        call: remote.invoke(action.target(agent), argument, None),
        action,
    }
}

impl Access {
    fn at_own_conversation(&self) -> bool {
        self.selected.get().as_deref() == Some(self.agent.as_str())
            && ui::invocation_scope::delivered_invocation().is_none()
            && ui::invocation_scope::current_invoker()
                .is_none_or(|who| who.acting.direct_person() == self.keeper.person_id())
    }

    fn ask(&self, action: AgentGrantAction, argument: InvokeArgument) {
        if !self.at_own_conversation() || self.request.get().action != AgentGrantAction::Status {
            return;
        }
        self.notice.set(String::new());
        self.request
            .set(call(&self.remote, &self.agent, action, argument));
    }

    fn button(&self, id: &'static str, label: &'static str, action: AgentGrantAction) -> Element {
        let access = self.clone();
        Button::new(id, label)
            .on_effect(move || access.ask(action, InvokeArgument::None))
            .into_widget()
    }

    fn status(&self, status: &AgentGrantStatus, panel: Panel, has_choices: bool) -> Element {
        if status.ready && panel == Panel::Conversation {
            let manage = self.panel.clone();
            return Button::new("chat-assistant-grant-manage", "chat-assistant-grant-manage")
                .on_view(move || manage.set(Panel::Manage))
                .into_widget();
        }
        let mut controls = Stack::row().wrap();
        let message = match status.state {
            GrantState::Pending => "chat-assistant-grant-pending",
            GrantState::Approved if status.ready => "chat-assistant-grant-active",
            GrantState::Approved
                if !status.apps.is_empty()
                    && !status.apps.iter().any(|app| app == ui::apps::CHAT) =>
            {
                "chat-assistant-grant-without-chat"
            }
            GrantState::Approved => "chat-assistant-grant-connecting",
            GrantState::Missing => "chat-assistant-grant-missing",
            GrantState::Denied => "chat-assistant-grant-denied",
            GrantState::Expired => "chat-assistant-grant-expired",
            GrantState::Revoked => "chat-assistant-grant-revoked",
        };
        match status.state {
            GrantState::Pending | GrantState::Approved => {
                controls = controls.push(self.button(
                    "chat-assistant-grant-refresh",
                    "chat-assistant-grant-refresh",
                    AgentGrantAction::Status,
                ));
                let withdraw = if status.state == GrantState::Pending {
                    "chat-assistant-grant-withdraw"
                } else {
                    "chat-assistant-grant-revoke"
                };
                controls = controls.push(self.button(withdraw, withdraw, AgentGrantAction::Revoke));
            }
            _ => {
                controls = controls
                    .push(self.button(
                        "chat-assistant-grant-request",
                        "chat-assistant-grant-request",
                        AgentGrantAction::RequestChat,
                    ))
                    .push(self.button(
                        "chat-assistant-grant-work",
                        "chat-assistant-grant-work",
                        AgentGrantAction::RequestWork,
                    ));
                if has_choices {
                    let access = self.clone();
                    controls = controls.push(
                        Button::new(
                            "chat-assistant-grant-choose-apps",
                            "chat-assistant-grant-choose-apps",
                        )
                        .on_view(move || {
                            if access.at_own_conversation() {
                                access.panel.set(Panel::ChooseApps);
                            }
                        })
                        .into_widget(),
                    );
                }
            }
        }
        let mut panel = Stack::stack().gap(ui::tokens::Rhythm::Tight).push(
            Text::new(message)
                .with_id("chat-assistant-grant-status")
                .with_live(ui::a11y::Politeness::Polite)
                .into_widget(),
        );
        if status.state == GrantState::Pending
            && let Some(code) = self.comparison_code.get()
        {
            panel = panel.push(
                Text::with_args("chat-assistant-grant-code", &[("code", code.into())])
                    .with_id("chat-assistant-grant-code")
                    .into_widget(),
            );
        }
        if matches!(status.state, GrantState::Pending | GrantState::Approved) {
            let apps = if status.apps.is_empty() {
                crate::i18n::t("chat-assistant-grant-all-apps")
            } else {
                status
                    .apps
                    .iter()
                    .map(|app| match app.as_str() {
                        ui::apps::CHAT => crate::i18n::t("chat-app-title"),
                        ui::apps::TASKS => crate::i18n::t("chat-assistant-app-tasks"),
                        ui::apps::ASSISTANT => crate::i18n::t("chat-assistant-app-assistant"),
                        ui::apps::SHORTCUTS => crate::i18n::t("chat-assistant-app-shortcuts"),
                        ui::apps::MAIL => crate::i18n::t("chat-assistant-app-mail"),
                        ui::apps::CALENDAR => crate::i18n::t("chat-assistant-app-calendar"),
                        ui::apps::PASSWORDS => crate::i18n::t("chat-assistant-app-passwords"),
                        other => ui::apps::builtin_label_for_app_id(other)
                            .unwrap_or_else(|| other.to_owned()),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            panel = panel.push(
                Text::with_args("chat-assistant-grant-scope", &[("apps", apps.into())])
                    .into_widget(),
            );
            if let Some(at) = status
                .expires_at_ms
                .and_then(|at| i64::try_from(at).ok())
                .and_then(|at| jiff::Timestamp::from_millisecond(at).ok())
            {
                let at = at.to_zoned(jiff::tz::TimeZone::system());
                panel = panel.push(
                    Text::with_args(
                        "chat-assistant-grant-until",
                        &[("date", at.date().to_string().into())],
                    )
                    .into_widget(),
                );
            }
        } else {
            panel = panel.push(Text::new("chat-assistant-grant-choices").into_widget());
        }
        if !self.notice.get().is_empty() {
            panel = panel.push(
                Text::from_state(&self.notice, String::clone)
                    .with_live(ui::a11y::Politeness::Polite)
                    .into_widget(),
            );
        }
        Box::new(panel.push(Box::new(controls)))
    }
}

pub(super) fn view(cx: &ViewCx<Chat>, selected: &LocalState<Option<String>>) -> Element {
    let Some(keeper) = cx.me().cloned() else {
        return ui::widget::optional(None);
    };
    let remote = RemoteActions::current();
    let selection = selected.clone();
    cx.switch(selected, move |cx, agent| {
        let Some(agent) = agent else {
            return ui::widget::optional(None);
        };
        let access = Access {
            remote: remote.clone(),
            keeper: keeper.clone(),
            agent: agent.clone(),
            selected: selection.clone(),
            request: cx.local(call(
                &remote,
                agent,
                AgentGrantAction::Status,
                InvokeArgument::None,
            )),
            notice: cx.local(String::new()),
            panel: cx.local(Panel::Conversation),
            comparison_code: cx.local(None),
        };
        cx.switch(&access.request.clone(), move |cx, request| {
            let access = access.clone();
            let action = request.action;
            cx.switch(&request.call.state(), move |cx, arrival| {
                let result = match arrival {
                    Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => {
                        Ok(output.value.as_ref())
                    }
                    Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
                        Err(error.message.clone())
                    }
                    Arrival::Ready(InvocationUpdate::Cancelled { message, .. }) => Err(message
                        .clone()
                        .unwrap_or_else(|| crate::i18n::t("chat-assistant-grant-cancelled"))),
                    Arrival::Failed(error) => Err(error.to_string()),
                    _ => return ui::widget::optional(None),
                };
                if action != AgentGrantAction::Status {
                    match result {
                        Err(why) => access.notice.set(why),
                        Ok(_) if action == AgentGrantAction::Revoke => {
                            access.comparison_code.set(None)
                        }
                        Ok(value) => {
                            let receipt = value.and_then(|value| {
                                serde_json::from_value::<AgentGrantReceipt>(value.clone()).ok()
                            });
                            access.comparison_code.set(
                                receipt
                                    .filter(|receipt| {
                                        receipt.grant == access.agent
                                            && !receipt.pairing_code.is_empty()
                                    })
                                    .map(|receipt| receipt.pairing_code),
                            );
                        }
                    }
                    access.panel.set(Panel::Conversation);
                    access.request.set(call(
                        &access.remote,
                        &access.agent,
                        AgentGrantAction::Status,
                        InvokeArgument::None,
                    ));
                    return ui::widget::optional(None);
                }
                let Some(status) = result.ok().flatten().and_then(|value| {
                    serde_json::from_value::<AgentGrantStatus>(value.clone()).ok()
                }) else {
                    return ui::widget::optional(None);
                };
                if status.state != GrantState::Pending {
                    access.comparison_code.set(None);
                }
                access.with_app_choices(cx, status)
            })
        })
    })
}

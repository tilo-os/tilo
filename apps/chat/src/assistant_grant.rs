//! The signed keeper's permission for their personal assistant to work on the workspace host.
//! A conversation shows what the host currently allows and sends only the keeper's chosen request.

use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};
use tilo_actions::{ActionAddress, InvocationUpdate, MemberId};
use tilo_server_protocol::agents::{
    AGENT_RESOURCE_PREFIX, AGENTS_PROVIDER, AgentGrantStatus, GRANT_STATUS_ACTION, GrantState,
    REQUEST_ASSISTANT_WORK_GRANT_ACTION, REQUEST_CHAT_GRANT_ACTION, REVOKE_ACTION,
};
use ui::prelude::*;
use ui::widget::arrange::Stack;
use ui::{Arrival, InvocationCall, RemoteActions};

use crate::Chat;

#[derive(Clone)]
struct Request {
    call: InvocationCall,
    changed: bool,
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
    managing: LocalState<bool>,
}

fn call(remote: &RemoteActions, agent: &str, action: &str) -> Request {
    let target = RemoteActionTarget {
        provider: AGENTS_PROVIDER.to_owned(),
        address: ActionAddress::new(format!("{AGENT_RESOURCE_PREFIX}{agent}"), action),
    };
    Request {
        call: remote.invoke(target, InvokeArgument::None, None),
        changed: action != GRANT_STATUS_ACTION,
    }
}

impl Access {
    fn ask(&self, action: &str) {
        if self.selected.get().as_deref() != Some(self.agent.as_str())
            || ui::invocation_scope::delivered_invocation().is_some()
            || ui::invocation_scope::current_invoker()
                .is_some_and(|who| who.acting.direct_person() != self.keeper.person_id())
            || self.request.get().changed
        {
            return;
        }
        self.notice.set(String::new());
        self.request.set(call(&self.remote, &self.agent, action));
    }

    fn button(&self, id: &'static str, label: &'static str, action: &'static str) -> Element {
        let access = self.clone();
        Button::new(id, label)
            .on_effect(move || access.ask(action))
            .into_widget()
    }

    fn status(&self, status: &AgentGrantStatus, managing: bool) -> Element {
        if status.ready && !managing {
            let manage = self.managing.clone();
            return Button::new("chat-assistant-grant-manage", "chat-assistant-grant-manage")
                .on_view(move || manage.set(true))
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
                    GRANT_STATUS_ACTION,
                ));
                if status.state == GrantState::Approved {
                    controls = controls.push(self.button(
                        "chat-assistant-grant-revoke",
                        "chat-assistant-grant-revoke",
                        REVOKE_ACTION,
                    ));
                }
            }
            _ => {
                controls = controls
                    .push(self.button(
                        "chat-assistant-grant-request",
                        "chat-assistant-grant-request",
                        REQUEST_CHAT_GRANT_ACTION,
                    ))
                    .push(self.button(
                        "chat-assistant-grant-work",
                        "chat-assistant-grant-work",
                        REQUEST_ASSISTANT_WORK_GRANT_ACTION,
                    ));
            }
        }
        let mut panel = Stack::stack().gap(ui::tokens::Rhythm::Tight).push(
            Text::new(message)
                .with_id("chat-assistant-grant-status")
                .with_live(ui::a11y::Politeness::Polite)
                .into_widget(),
        );
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
                        other => other.to_owned(),
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
            request: cx.local(call(&remote, agent, GRANT_STATUS_ACTION)),
            notice: cx.local(String::new()),
            managing: cx.local(false),
        };
        cx.switch(&access.request.clone(), move |cx, request| {
            let access = access.clone();
            let changed = request.changed;
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
                if changed {
                    if let Err(why) = result {
                        access.notice.set(why);
                    }
                    access.managing.set(false);
                    access
                        .request
                        .set(call(&access.remote, &access.agent, GRANT_STATUS_ACTION));
                    return ui::widget::optional(None);
                }
                let Some(status) = result.ok().flatten().and_then(|value| {
                    serde_json::from_value::<AgentGrantStatus>(value.clone()).ok()
                }) else {
                    return ui::widget::optional(None);
                };
                let access = access.clone();
                cx.switch(&access.managing.clone(), move |_, managing| {
                    access.status(&status, *managing)
                })
            })
        })
    })
}

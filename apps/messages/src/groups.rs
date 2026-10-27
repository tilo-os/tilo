//! Private, transient service membership inspection. No workspace CRDT writes.
use crate::{CATALOG, Messages, MessagesEffect, view::Sources};
use std::rc::Rc;
use tilo_messaging::{
    AccountState, GroupList, GroupMembership, GroupRole, MessagingClient, ServiceAccount,
};
use ui::{
    LocalState,
    framework::{Actions, EffectAnswer, EffectCx, Effected},
    widget::{Button, Column, Element, Scrollable, Text},
};

mod operations;

enum Page {
    List(GroupList),
    Members(GroupMembership),
}
#[derive(Clone)]
pub(crate) struct GroupsView {
    client: Option<MessagingClient>,
    page: LocalState<Option<Rc<Page>>>,
    generation: LocalState<u64>,
    history: LocalState<Option<Rc<tilo_messaging::GroupHistory>>>,
    draft: LocalState<String>,
    operation: LocalState<String>,
}
impl GroupsView {
    pub(crate) fn new(client: Option<MessagingClient>) -> Self {
        Self {
            client,
            page: LocalState::new(None),
            generation: LocalState::new(0),
            history: LocalState::new(None),
            draft: LocalState::new(String::new()),
            operation: LocalState::new(uuid::Uuid::new_v4().to_string()),
        }
    }
    pub(crate) fn key(&self) -> u64 {
        self.generation.get()
    }
    pub(crate) fn visible(&self) -> bool {
        self.page.get().is_some()
    }
    pub(crate) fn close(&self) {
        self.generation.set(self.key().saturating_add(1));
        self.page.set(None);
        self.history.set(None);
        self.draft.set(String::new());
        self.operation.set(uuid::Uuid::new_v4().to_string());
    }
    /// What the window bar names while the groups are open, and the
    /// effect refreshing an open group's membership.
    pub(crate) fn shown(&self) -> Option<(String, Option<MessagesEffect>)> {
        let page = self.page.get()?;
        Some(match page.as_ref() {
            Page::List(_) => (CATALOG.t("messages-groups"), None),
            Page::Members(page) => (
                if page.group.title.is_empty() {
                    CATALOG.t("messages-group-untitled")
                } else {
                    page.group.title.clone()
                },
                Some(MessagesEffect::ReadGroupMembership {
                    service: page.account.service.clone(),
                    account: page.account.account.clone(),
                    group: page.group.id.clone(),
                }),
            ),
        })
    }
    pub(crate) fn handles(effect: &MessagesEffect) -> bool {
        matches!(
            effect,
            MessagesEffect::ListGroups { .. }
                | MessagesEffect::ReadGroupMembership { .. }
                | MessagesEffect::ReadGroupHistory { .. }
                | MessagesEffect::SendGroupText { .. }
                | MessagesEffect::AddGroupMember { .. }
                | MessagesEffect::RemoveGroupMember { .. }
                | MessagesEffect::LeaveGroup { .. }
                | MessagesEffect::CloseGroups
        )
    }
    fn admitted(&self, sources: &Sources, account: &ServiceAccount) -> bool {
        account.service == "whatsapp"
            && sources.accounts().accounts.iter().any(|(id, a)| {
                id.as_str() == account.account && a.type_id == account.service && a.enabled
            })
            && self
                .client
                .as_ref()
                .is_some_and(|client| client.account_state(account) == AccountState::Ready)
    }
    pub(crate) fn effect(
        &self,
        sources: &Sources,
        effect: MessagesEffect,
        cx: &mut EffectCx<Messages>,
    ) -> EffectAnswer {
        if operations::handles(&effect) {
            return operations::effect(self, sources, effect, cx);
        }
        let (account, group) = match effect {
            MessagesEffect::ListGroups { service, account } => {
                (ServiceAccount::new(service, account), None)
            }
            MessagesEffect::ReadGroupMembership {
                service,
                account,
                group,
            } => (ServiceAccount::new(service, account), Some(group)),
            MessagesEffect::CloseGroups => {
                self.close();
                return Ok(Effected::done());
            }
            _ => return Err(CATALOG.t("messages-transport-unavailable")),
        };
        if !self.admitted(sources, &account) {
            self.close();
            return Err(CATALOG.t("messages-groups-unavailable"));
        }
        let client = self
            .client
            .as_ref()
            .and_then(|client| client.groups(&account))
            .ok_or_else(|| CATALOG.t("messages-groups-unavailable"))?;
        let same = matches!(self.page.get().as_deref(),Some(Page::Members(p)) if p.account==account && group.as_ref()==Some(&p.group.id));
        let draft = same.then(|| (self.draft.get(), self.operation.get()));
        self.close();
        if let Some((draft, operation)) = draft {
            self.draft.set(draft);
            self.operation.set(operation);
        }
        let generation = self.key();
        let view = self.clone();
        let sources = sources.clone();
        Ok(cx.spawn(move |_| async move {
            match group { Some(group) => client.read(group).await.map(Page::Members), None => client.list().await.map(Page::List) }
        }, move |(), result, _| {
            if view.key() != generation { return Err(CATALOG.t("messages-groups-unavailable")); }
            if !view.admitted(&sources, &account) { view.close(); return Err(CATALOG.t("messages-groups-unavailable")); }
            let page = result.map_err(|_| CATALOG.t("messages-groups-unavailable"))?;
            let value = match &page {
                Page::List(list) => serde_json::json!({"service":list.account.service,"account":list.account.account,"groups":list.groups.iter().map(|g| serde_json::json!({"id":g.id,"title":g.title})).collect::<Vec<_>>()}),
                Page::Members(page) => serde_json::json!({"service":page.account.service,"account":page.account.account,"group":page.group.id,"title":page.group.title,"announcement":page.announcement,"self_member":page.self_member,"review":page.review,"members":page.members.iter().map(|m|serde_json::json!({"address":m.address,"role":role(m.role)})).collect::<Vec<_>>()}),
            };
            view.page.set(Some(Rc::new(page)));
            view.generation.set(generation.saturating_add(1));
            Ok(Effected::value(value))
        }))
    }
    pub(crate) fn panel(&self, sources: &Sources, actions: &Actions<Messages>) -> Element {
        let Some(page) = self.page.get() else {
            return Box::new(Text::new(&CATALOG.t("messages-groups-unavailable")));
        };
        let account = match page.as_ref() {
            Page::List(p) => &p.account,
            Page::Members(p) => &p.account,
        };
        if !self.admitted(sources, account) {
            self.close();
            return Box::new(Text::new(&CATALOG.t("messages-groups-unavailable")));
        }
        let label = sources
            .accounts()
            .accounts
            .get(&ui::prelude::Id::from_raw(&account.account))
            .map_or_else(
                || account.account.clone(),
                |a| format!("{} ({})", a.display_name, account.account),
            );
        let account_label = CATALOG.t_args(
            "messages-account-groups",
            &[("account", ui::i18n::ArgValue::Text(label))],
        );
        let mut body = Column::new()
            .with_id("messages.groups")
            .push(Box::new(Text::new(&CATALOG.t("messages-groups-scope"))));
        body = body.push(Box::new(Text::new(&account_label)));
        match page.as_ref() {
            Page::List(list) => {
                if list.groups.is_empty() {
                    body = body.push(Box::new(Text::new(&CATALOG.t("messages-groups-empty"))));
                }
                for group in &list.groups {
                    let label = if group.title.is_empty() {
                        CATALOG.t("messages-group-untitled")
                    } else {
                        group.title.clone()
                    };
                    body = body.push(bind(
                        actions,
                        Button::new(&format!("messages.group:{}", group.id), &label),
                        &MessagesEffect::ReadGroupMembership {
                            service: account.service.clone(),
                            account: account.account.clone(),
                            group: group.id.clone(),
                        },
                    ));
                }
            }
            Page::Members(page) => {
                for member in &page.members {
                    body = body
                        .push(Box::new(Text::new(&member.address)))
                        .push(Box::new(Text::new(
                            &CATALOG.t(&format!("messages-group-role-{}", role(member.role))),
                        )));
                }
                body = operations::panel(self, sources, actions, page, body);
            }
        }
        Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(Box::new(body)),
        )
    }
    pub(crate) fn refresh_loaded(&self, sources: &Sources, jobs: &ui::framework::Jobs<Messages>) {
        if self.history.get().is_none() {
            return;
        }
        let selected = self.page.get();
        let Some(Page::Members(page)) = selected.as_deref() else {
            return;
        };
        let request = MessagesEffect::ReadGroupHistory {
            service: page.account.service.clone(),
            account: page.account.account.clone(),
            group: page.group.id.clone(),
        };
        jobs.start::<()>("messages.group.history", |cx| {
            self.effect(sources, request, cx)
        });
    }
    /// The accounts whose groups can be listed, one entry each; none
    /// without such an account, so the people below keep the pane.
    pub(crate) fn entries(
        &self,
        sources: &Sources,
        actions: &Actions<Messages>,
    ) -> Option<Element> {
        let mut body = Column::new();
        let mut any = false;
        for (id, a) in sources
            .accounts()
            .accounts
            .iter()
            .filter(|(_, a)| a.type_id == "whatsapp" && a.enabled)
        {
            let account = ServiceAccount::new("whatsapp", id.as_str());
            let label = CATALOG.t_args(
                "messages-account-groups",
                &[(
                    "account",
                    ui::i18n::ArgValue::Text(format!("{} ({id})", a.display_name)),
                )],
            );
            any = true;
            body = body.push(bind(
                actions,
                Button::new(&format!("messages.groups.account:{id}"), &label)
                    .with_enabled(self.admitted(sources, &account)),
                &MessagesEffect::ListGroups {
                    service: account.service,
                    account: account.account,
                },
            ));
        }
        any.then(|| Box::new(body) as Element)
    }
}
fn role(role: GroupRole) -> &'static str {
    match role {
        GroupRole::Member => "member",
        GroupRole::Admin => "admin",
        GroupRole::SuperAdmin => "super-admin",
    }
}

fn bind(actions: &Actions<Messages>, button: Button, effect: &MessagesEffect) -> Element {
    Box::new(actions.bind_effect_invocation(button, effect))
}

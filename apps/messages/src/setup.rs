//! Ephemeral private linking through the exact host-lent account capability.
use crate::{CATALOG, Messages, MessagesEffect, view::Sources};
use std::sync::Arc;
use tilo_messaging::{AccountState, MessagingClient, Pairing, ServiceAccount, SetupClient};
use ui::{
    LocalState,
    framework::{Actions, EffectAnswer, EffectCx, Effected},
    widget::{Button, Column, Container, Element, QrWidget, Scrollable, Text},
};
#[derive(Clone, Default)]
struct Projection {
    visible: bool,
    selected: Option<ServiceAccount>,
    pairing: Option<Arc<Pairing>>,
    pending: bool,
    completed: bool,
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SetupKey {
    visible: bool,
    selected: Option<ServiceAccount>,
    attempt: Option<String>,
    pending: bool,
    completed: bool,
    state: AccountState,
    first: (tilo_messaging::EnrollmentPhase, bool, u64),
}
#[derive(Clone)]
pub(crate) struct SetupView {
    client: Option<MessagingClient>,
    first: crate::enrollment::EnrollmentView,
    state: LocalState<Projection>,
    epoch: LocalState<u64>,
}
impl SetupView {
    pub(crate) fn new(
        client: Option<MessagingClient>,
        first: Option<tilo_messaging::EnrollmentClient>,
        principal: Option<&str>,
    ) -> Self {
        Self {
            client,
            first: crate::enrollment::EnrollmentView::new(first, principal),
            state: LocalState::new(Projection::default()),
            epoch: LocalState::new(0),
        }
    }
    pub(crate) fn visible(&self) -> bool {
        self.state.with(|state| state.visible)
    }
    pub(crate) fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        if self.visible()
            && let Some(wake) = self.first.wake()
        {
            return Some(wake);
        }
        self.state.with(|state| {
            if !state.visible || state.pairing.is_none() {
                return None;
            }
            let after = self.current(state.selected.as_ref())?.wake_after()?;
            ui::time::now().checked_add(after)
        })
    }
    pub(crate) fn key(&self) -> SetupKey {
        self.state.with(|state| SetupKey {
            visible: state.visible,
            selected: state.selected.clone(),
            attempt: state
                .pairing
                .as_ref()
                .map(|private| private.attempt.clone()),
            pending: state.pending,
            completed: state.completed,
            state: self.account_state(state.selected.as_ref()),
            first: self.first.key(),
        })
    }
    fn account_state(&self, account: Option<&ServiceAccount>) -> AccountState {
        account
            .zip(self.client.as_ref())
            .map_or(AccountState::Unavailable, |(account, client)| {
                client.account_state(account)
            })
    }
    fn current(&self, account: Option<&ServiceAccount>) -> Option<SetupClient> {
        self.client.as_ref()?.setup(account?)
    }
    pub(crate) fn close(&self) {
        self.first.close();
        self.epoch.update(|epoch| *epoch = epoch.saturating_add(1));
        let active = self.state.with(|state| {
            (
                state.selected.clone(),
                state.pending || state.pairing.is_some(),
            )
        });
        if active.1
            && let Some(client) = self.current(active.0.as_ref())
        {
            client.cancel();
        }
        self.state.set(Projection::default());
    }

    pub(crate) fn handles(effect: &MessagesEffect) -> bool {
        crate::enrollment::EnrollmentView::handles(effect)
            || matches!(
                effect,
                MessagesEffect::OpenAccountSetup
                    | MessagesEffect::SelectSetupAccount { .. }
                    | MessagesEffect::BeginLink { .. }
                    | MessagesEffect::CompleteLink { .. }
                    | MessagesEffect::CancelLink
            )
    }
    fn checked(
        &self,
        sources: &Sources,
        service: String,
        account: String,
    ) -> Result<(ServiceAccount, SetupClient), String> {
        let identity = ServiceAccount::new(service, account);
        if !sources.accounts().accounts.iter().any(|(id, account)| {
            id.as_str() == identity.account
                && account.type_id == identity.service
                && account.enabled
        }) {
            return Err(CATALOG.t("messages-no-account"));
        }
        if !self
            .state
            .with(|state| state.visible && state.selected.as_ref() == Some(&identity))
        {
            return Err(CATALOG.t("messages-setup-select"));
        }
        let client = self
            .current(Some(&identity))
            .ok_or_else(|| CATALOG.t("messages-setup-unavailable"))?;
        Ok((identity, client))
    }
    pub(crate) fn effect(
        &self,
        sources: &Sources,
        effect: MessagesEffect,
        cx: &mut EffectCx<Messages>,
    ) -> EffectAnswer {
        if crate::enrollment::EnrollmentView::handles(&effect) {
            if !self.visible() {
                return Err(CATALOG.t("messages-setup-select"));
            }
            return self.first.effect(effect, cx);
        }
        match effect {
            MessagesEffect::OpenAccountSetup => {
                self.state.update(|state| state.visible = true);
                Ok(Effected::done())
            }
            MessagesEffect::SelectSetupAccount { service, account } => {
                if !sources.accounts().accounts.iter().any(|(id, row)| {
                    id.as_str() == account
                        && row.type_id == service
                        && row.enabled
                        && matches!(service.as_str(), "tilo" | "signal" | "whatsapp" | "sms")
                }) {
                    return Err(CATALOG.t("messages-no-account"));
                }
                if self.state.with(|state| {
                    state.selected.as_ref().is_some_and(|current| {
                        current.service == service && current.account == account
                    })
                }) {
                    return Ok(Effected::done());
                }
                self.close();
                self.state.set(Projection {
                    visible: true,
                    selected: Some(ServiceAccount::new(service, account)),
                    ..Projection::default()
                });
                Ok(Effected::done())
            }
            MessagesEffect::BeginLink { service, account } => {
                self.begin(sources, service, account, cx)
            }
            MessagesEffect::CompleteLink {
                service,
                account,
                attempt,
            } => self.finish(sources, service, account, attempt, cx),
            MessagesEffect::CancelLink => {
                self.close();
                Ok(Effected::done())
            }
            _ => Err(CATALOG.t("messages-setup-unavailable")),
        }
    }
    fn begin(
        &self,
        sources: &Sources,
        service: String,
        account: String,
        cx: &mut EffectCx<Messages>,
    ) -> EffectAnswer {
        let (identity, client) = self.checked(sources, service, account)?;
        if self
            .state
            .with(|state| state.pending || state.pairing.is_some() || state.completed)
        {
            return Err(CATALOG.t("messages-setup-active"));
        }
        self.state.update(|state| state.pending = true);
        self.epoch.update(|epoch| *epoch = epoch.saturating_add(1));
        let admitted = self.epoch.get();
        let epoch = self.epoch.clone();
        let accounts = sources.clone();
        let projection = self.state.clone();
        let invalidated = client.clone();
        Ok(cx.spawn(
            move |_| async move { client.begin().await },
            move |(), result, _| {
                if epoch.get() != admitted
                    || !account_current(&accounts, &identity)
                    || !projection
                        .with(|state| state.visible && state.selected.as_ref() == Some(&identity))
                {
                    invalidated.cancel();
                    return Err(CATALOG.t("messages-setup-interrupted"));
                }
                projection.update(|state| state.pending = false);
                let private = result.map_err(|_| CATALOG.t("messages-setup-interrupted"))?;
                let result = serde_json::json!({"state":"linking","service":private.account.service,"account":private.account.account,"attempt":private.attempt,"generation":private.generation});
                projection.update(|state| state.pairing = Some(Arc::new(private)));
                Ok(Effected::value(result))
            },
        ))
    }
    fn finish(
        &self,
        sources: &Sources,
        service: String,
        account: String,
        attempt: String,
        cx: &mut EffectCx<Messages>,
    ) -> EffectAnswer {
        let (identity, client) = self.checked(sources, service, account)?;
        if !self.state.with(|state| {
            !state.pending
                && state.pairing.as_ref().is_some_and(|private| {
                    private.attempt == attempt && private.account == identity
                })
        }) {
            return Err(CATALOG.t("messages-setup-active"));
        }
        self.state.update(|state| state.pending = true);
        self.epoch.update(|epoch| *epoch = epoch.saturating_add(1));
        let admitted = self.epoch.get();
        let epoch = self.epoch.clone();
        let accounts = sources.clone();
        let projection = self.state.clone();
        let invalidated = client.clone();
        Ok(cx.spawn(move |_| async move { client.finish(attempt).await }, move |(), result, _| {
            if epoch.get() != admitted || !account_current(&accounts, &identity) || !projection.with(|state| state.visible && state.selected.as_ref() == Some(&identity)) { invalidated.cancel(); return Err(CATALOG.t("messages-setup-interrupted")); }
            projection.update(|state| { state.pending = false; state.pairing = None; });
            let linked = result.map_err(|_| CATALOG.t("messages-setup-interrupted"))?;
            projection.update(|state| state.completed = true);
            Ok(Effected::value(serde_json::json!({"state":"linked","service":linked.account.service,"account":linked.account.account,"scope":linked.scope})))
        }))
    }
    pub(crate) fn panel(&self, sources: &Sources, actions: &Actions<Messages>) -> Element {
        if self.state.with(|state| {
            state
                .selected
                .as_ref()
                .is_some_and(|account| !account_current(sources, account))
        }) {
            self.close();
        }
        let state = self.state.get();
        let mut content = Column::new()
            .with_intrinsic_height()
            .push(Box::new(Text::new(&CATALOG.t("messages-setup-select"))));
        for (id, account) in sources.accounts().accounts.iter().filter(|(_, row)| {
            row.enabled && matches!(row.type_id.as_str(), "tilo" | "signal" | "whatsapp" | "sms")
        }) {
            let label = CATALOG.t_args(
                "messages-account-label",
                &[
                    (
                        "service",
                        ui::i18n::ArgValue::Text(
                            CATALOG.t(&format!("messages-{}", account.type_id)),
                        ),
                    ),
                    (
                        "account",
                        ui::i18n::ArgValue::Text(format!("{} ({id})", account.display_name)),
                    ),
                ],
            );
            content = content.push(Box::new(actions.bind_effect_invocation(
                Button::new(&format!("messages.setup-account:{id}"), &label),
                &MessagesEffect::SelectSetupAccount {
                    service: account.type_id.clone(),
                    account: id.as_str().into(),
                },
            )));
        }
        if let Some(account) = &state.selected {
            content = self.account_panel(content, actions, &state, account);
        } else if sources.accounts().accounts.is_empty() {
            content = content.push(Box::new(Text::new(
                &CATALOG.t("messages-account-unconfigured"),
            )));
        }
        content = self.first.panel(content, actions);
        let close = actions.bind_effect_invocation(
            Button::new("messages.setup-cancel", "messages-cancel-link"),
            &MessagesEffect::CancelLink,
        );
        content = content.push(Box::new(close));
        Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(Box::new(Container::new().with_child(Box::new(content)))),
        )
    }
    fn account_panel(
        &self,
        mut content: Column,
        actions: &Actions<Messages>,
        state: &Projection,
        account: &ServiceAccount,
    ) -> Column {
        content = content.push(Box::new(Text::new(&CATALOG.t_args(
            "messages-setup-selected",
            &[
                (
                    "service",
                    ui::i18n::ArgValue::Text(CATALOG.t(&format!("messages-{}", account.service))),
                ),
                ("account", ui::i18n::ArgValue::Text(account.account.clone())),
            ],
        ))));
        let native = self.current(Some(account));
        let lifecycle = self.account_state(Some(account));
        if state.completed {
            return content.push(Box::new(Text::new(&CATALOG.t("messages-setup-linked"))));
        }
        if let Some(private) = &state.pairing {
            if lifecycle == AccountState::Linking {
                content = content.push(Box::new(Text::new(&CATALOG.t("messages-setup-scan"))));
                content = content.push(Box::new(
                    QrWidget::new(private.qr_bytes().to_vec())
                        .with_id("messages.setup-code")
                        .with_alt(CATALOG.t("messages-setup-code"))
                        .with_secret(),
                ));
                let finish = actions.bind_effect_invocation(
                    Button::new("messages.setup-finish", "messages-finish-link")
                        .with_enabled(!state.pending),
                    &MessagesEffect::CompleteLink {
                        service: account.service.clone(),
                        account: account.account.clone(),
                        attempt: private.attempt.clone(),
                    },
                );
                return content.push(Box::new(finish));
            }
            self.state.update(|current| current.pairing = None);
        }
        content = content.push(Box::new(Text::new(
            &CATALOG.t(&format!("messages-account-{}", lifecycle.as_str())),
        )));
        if account.service == "tilo" {
            return content;
        }
        let begin = actions.bind_effect_invocation(
            Button::new("messages.setup-start", "messages-start-link").with_enabled(
                native.is_some()
                    && !state.pending
                    && !matches!(
                        lifecycle,
                        AccountState::Locked | AccountState::NeedsRelink | AccountState::Linking
                    ),
            ),
            &MessagesEffect::BeginLink {
                service: account.service.clone(),
                account: account.account.clone(),
            },
        );
        content.push(Box::new(begin))
    }
}

fn account_current(sources: &Sources, identity: &ServiceAccount) -> bool {
    sources.accounts().accounts.iter().any(|(id, row)| {
        id.as_str() == identity.account && row.type_id == identity.service && row.enabled
    })
}

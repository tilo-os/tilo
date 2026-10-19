//! First-account presentation is local and private, never shared app state.
use crate::{CATALOG, Messages, MessagesEffect};
use std::sync::Arc;
use tilo_messaging::{
    EnrollmentClient, EnrollmentPhase, EnrollmentReview, Pairing, ServiceAccount,
};
use ui::{
    LocalState,
    framework::{Actions, EffectAnswer, EffectCx, Effected},
    icons::Icon,
    widget::{Button, Column, Element, ListRow, QrWidget, Section, Text},
};
#[derive(Clone, Default)]
struct Projection {
    pairing: Option<Arc<Pairing>>,
    review: Option<EnrollmentReview>,
    account: Option<ServiceAccount>,
    pending: bool,
    proposal: Option<String>,
    sims: Option<Vec<tilo_messaging::SimChoice>>,
}
#[derive(Clone)]
pub(crate) struct EnrollmentView {
    client: Option<EnrollmentClient>,
    state: LocalState<Projection>,
    epoch: LocalState<u64>,
}
impl EnrollmentView {
    pub(crate) fn new(client: Option<EnrollmentClient>, principal: Option<&str>) -> Self {
        Self {
            client: client.filter(|client| principal == Some(client.principal())),
            state: LocalState::new(Projection::default()),
            epoch: LocalState::new(0),
        }
    }
    pub(crate) fn phase(&self) -> EnrollmentPhase {
        self.state.with(|state| {
            state
                .account
                .as_ref()
                .zip(self.client.as_ref())
                .map_or(EnrollmentPhase::Empty, |(account, client)| {
                    client.phase(account)
                })
        })
    }
    pub(crate) fn key(&self) -> (EnrollmentPhase, bool, u64) {
        (
            self.phase(),
            self.state.with(|state| state.pending),
            self.epoch.get(),
        )
    }
    pub(crate) fn wake(&self) -> Option<ui::time::Instant> {
        self.state
            .with(|state| state.account.is_some())
            .then_some(())?;
        matches!(
            self.phase(),
            EnrollmentPhase::Linking | EnrollmentPhase::AwaitingPermission
        )
        .then(|| ui::time::now().checked_add(ui::time::Duration::from_millis(250)))
        .flatten()
    }
    pub(crate) fn close(&self) {
        self.epoch.update(|epoch| *epoch = epoch.saturating_add(1));
        if let Some(client) = &self.client {
            client.cancel_sim_selection();
        }
        if self.phase() != EnrollmentPhase::Ready
            && let Some(client) = &self.client
            && let Some(account) = self.state.with(|state| state.account.clone())
        {
            client.cancel(&account);
        }
        self.state.set(Projection::default());
    }
    fn can_reset(&self) -> bool {
        self.phase() == EnrollmentPhase::Interrupted
            && self.state.with(|state| {
                state
                    .account
                    .as_ref()
                    .zip(self.client.as_ref())
                    .is_some_and(|(account, client)| client.linked(account).is_none())
            })
    }
    pub(crate) fn handles(effect: &MessagesEffect) -> bool {
        matches!(
            effect,
            MessagesEffect::ResetFirstEnrollment
                | MessagesEffect::BeginFirstEnrollment { .. }
                | MessagesEffect::ListSmsSims
                | MessagesEffect::SelectSmsSim { .. }
                | MessagesEffect::CompleteFirstEnrollment { .. }
                | MessagesEffect::ReviewFirstEnrollment { .. }
        )
    }
    pub(crate) fn effect(
        &self,
        effect: MessagesEffect,
        cx: &mut EffectCx<Messages>,
    ) -> EffectAnswer {
        let client = self
            .client
            .clone()
            .ok_or_else(|| CATALOG.t("messages-setup-unavailable"))?;
        match effect {
            MessagesEffect::ResetFirstEnrollment => {
                if !self.can_reset() {
                    return Err(CATALOG.t("messages-setup-active"));
                }
                self.close();
                Ok(Effected::done())
            }
            MessagesEffect::ListSmsSims => self.sims(client, cx),
            MessagesEffect::SelectSmsSim { selection } => self.select_sim(client, selection, cx),
            MessagesEffect::BeginFirstEnrollment { service } if service == "tilo" => {
                self.prepare(client, service, cx)
            }
            MessagesEffect::BeginFirstEnrollment { service } => self.begin(client, service, cx),
            MessagesEffect::CompleteFirstEnrollment {
                service,
                account,
                attempt,
            } => self.finish(client, ServiceAccount::new(service, account), attempt, cx),
            MessagesEffect::ReviewFirstEnrollment {
                service,
                account,
                display_name,
            } => self.review(
                client,
                ServiceAccount::new(service, account),
                display_name,
                cx,
            ),
            _ => Err(CATALOG.t("messages-setup-unavailable")),
        }
    }
    fn sims(&self, client: EnrollmentClient, cx: &mut EffectCx<Messages>) -> EffectAnswer {
        if !client.supports_sim_selection()
            || self
                .state
                .with(|state| state.pending || state.account.is_some())
        {
            return Err(CATALOG.t("messages-setup-active"));
        }
        self.state.update(|state| state.pending = true);
        self.epoch.update(|epoch| *epoch = epoch.saturating_add(1));
        let admitted = self.epoch.get();
        let epoch = self.epoch.clone();
        let state = self.state.clone();
        let invalidated = client.clone();
        Ok(cx.spawn(move |_| async move { client.sim_choices().await }, move |(), result, _| {
            if epoch.get() != admitted {
                invalidated.cancel_sim_selection();
                return Err(CATALOG.t("messages-setup-interrupted"));
            }
            state.update(|state| state.pending = false);
            let choices = result.map_err(|_| CATALOG.t("messages-setup-unavailable"))?;
            let value = serde_json::json!({"state":"select_sim", "choices": choices.iter().map(|choice| serde_json::json!({"selection": choice.selection, "label": choice.label})).collect::<Vec<_>>()});
            state.update(|state| state.sims = Some(choices));
            epoch.update(|epoch| *epoch = epoch.saturating_add(1));
            Ok(Effected::value(value))
        }))
    }
    fn select_sim(
        &self,
        client: EnrollmentClient,
        selection: String,
        cx: &mut EffectCx<Messages>,
    ) -> EffectAnswer {
        if !self.state.with(|state| {
            !state.pending
                && state.account.is_none()
                && state.sims.as_ref().is_some_and(|choices| {
                    choices.iter().any(|choice| choice.selection == selection)
                })
        }) {
            return Err(CATALOG.t("messages-setup-active"));
        }
        self.state.update(|state| state.pending = true);
        self.epoch.update(|epoch| *epoch = epoch.saturating_add(1));
        let admitted = self.epoch.get();
        let epoch = self.epoch.clone();
        let state = self.state.clone();
        let invalidated = client.clone();
        Ok(cx.spawn(move |_| async move { client.select_sim(selection).await }, move |(), result, _| {
            if epoch.get() != admitted {
                if let Ok(review) = result { invalidated.cancel(&review.account); }
                return Err(CATALOG.t("messages-setup-interrupted"));
            }
            state.update(|state| state.pending = false);
            let review = result.map_err(|_| CATALOG.t("messages-setup-unavailable"))?;
            let value = serde_json::json!({"state":"review","service":"sms","account":review.account.account});
            state.update(|state| {
                state.account = Some(review.account.clone());
                state.review = Some(review);
                state.sims = None;
            });
            epoch.update(|epoch| *epoch = epoch.saturating_add(1));
            Ok(Effected::value(value))
        }))
    }
    fn prepare(
        &self,
        client: EnrollmentClient,
        service: String,
        cx: &mut EffectCx<Messages>,
    ) -> EffectAnswer {
        if self
            .state
            .with(|state| state.pending || state.account.is_some())
        {
            return Err(CATALOG.t("messages-setup-active"));
        }
        self.state.update(|state| state.pending = true);
        self.epoch.update(|epoch| *epoch = epoch.saturating_add(1));
        let admitted = self.epoch.get();
        let epoch = self.epoch.clone();
        let projection = self.state.clone();
        let invalidated = client.clone();
        Ok(cx.spawn(move |_| async move { client.prepare(service).await }, move |(), result, _| {
            if epoch.get() != admitted {
                if let Ok(review) = result { invalidated.cancel(&review.account); }
                return Err(CATALOG.t("messages-setup-interrupted"));
            }
            projection.update(|state| state.pending = false);
            let review = result.map_err(|_| CATALOG.t("messages-setup-unavailable"))?;
            let value = serde_json::json!({"state":"review","service":review.account.service,"account":review.account.account});
            projection.update(|state| { state.account = Some(review.account.clone()); state.review = Some(review); });
            Ok(Effected::value(value))
        }))
    }
    fn begin(
        &self,
        client: EnrollmentClient,
        service: String,
        cx: &mut EffectCx<Messages>,
    ) -> EffectAnswer {
        let original = self.state.with(|state| state.account.clone());
        let prepare = service == "whatsapp" && original.is_none();
        if self.state.with(|state| state.pending)
            || original.as_ref().is_some_and(|account| {
                service != "whatsapp"
                    || account.service != service
                    || self.phase() != EnrollmentPhase::Linking
            })
        {
            return Err(CATALOG.t("messages-setup-active"));
        }
        if prepare {
            return self.prepare(client, service, cx);
        }
        self.state.update(|state| state.pending = true);
        self.epoch.update(|epoch| *epoch = epoch.saturating_add(1));
        let admitted = self.epoch.get();
        let epoch = self.epoch.clone();
        let projection = self.state.clone();
        let invalidated = client.clone();
        Ok(cx.spawn(move |_| async move { client.begin(service).await }, move |(), result, _| {
            if epoch.get() != admitted {
                if let Ok(private) = result { invalidated.cancel(&private.account); }
                return Err(CATALOG.t("messages-setup-interrupted"));
            }
            projection.update(|state| state.pending = false);
            let private = result.map_err(|_| CATALOG.t("messages-setup-unavailable"))?;
            if original.as_ref().is_some_and(|account| account != &private.account) {
                invalidated.cancel(&private.account);
                return Err(CATALOG.t("messages-setup-interrupted"));
            }
            let value = serde_json::json!({"state":"linking", "service":private.account.service,"account":private.account.account,"attempt":private.attempt,"generation":private.generation});
            projection.update(|state| { state.account = Some(private.account.clone()); state.pairing = Some(Arc::new(private)); });
            Ok(Effected::value(value))
        }))
    }
    fn finish(
        &self,
        client: EnrollmentClient,
        account: ServiceAccount,
        attempt: String,
        cx: &mut EffectCx<Messages>,
    ) -> EffectAnswer {
        if !self.state.with(|state| {
            !state.pending
                && state
                    .pairing
                    .as_ref()
                    .is_some_and(|private| private.account == account && private.attempt == attempt)
        }) {
            return Err(CATALOG.t("messages-setup-active"));
        }
        self.state.update(|state| state.pending = true);
        let admitted = self.epoch.get();
        let epoch = self.epoch.clone();
        let projection = self.state.clone();
        let invalidated = client.clone();
        let original = account.clone();
        Ok(cx.spawn(move |_| async move { client.finish(account, attempt).await }, move |(), result, _| {
            if epoch.get() != admitted { invalidated.cancel(&original); return Err(CATALOG.t("messages-setup-interrupted")); }
            projection.update(|state| state.pending = false);
            let review = result.map_err(|_| CATALOG.t("messages-setup-unavailable"))?;
            projection.update(|state| { state.review = Some(review); state.pairing = None; });
            Ok(Effected::value(serde_json::json!({"state":"review","service":original.service,"account":original.account})))
        }))
    }
    fn review(
        &self,
        client: EnrollmentClient,
        account: ServiceAccount,
        display_name: String,
        cx: &mut EffectCx<Messages>,
    ) -> EffectAnswer {
        if !self.state.with(|state| {
            !state.pending
                && state
                    .review
                    .as_ref()
                    .is_some_and(|review| review.account == account)
                && (state.proposal.is_none()
                    || (account.service == "whatsapp"
                        && state
                            .review
                            .as_ref()
                            .is_some_and(|review| review.number.is_some())
                        && client.phase(&account) == EnrollmentPhase::Review))
        }) {
            return Err(CATALOG.t("messages-setup-active"));
        }
        self.state.update(|state| state.pending = true);
        let admitted = self.epoch.get();
        let epoch = self.epoch.clone();
        let projection = self.state.clone();
        let invalidated = client.clone();
        let original = account.clone();
        let reviewed = client.clone();
        Ok(cx.spawn(
            move |_| async move { client.review(account, display_name).await },
            move |(), result, _| {
                if epoch.get() != admitted {
                    invalidated.cancel(&original);
                    return Err(CATALOG.t("messages-setup-interrupted"));
                }
                projection.update(|state| state.pending = false);
                let proposal = result.map_err(|_| CATALOG.t("messages-setup-interrupted"))?;
                projection.update(|state| state.proposal = Some(proposal.clone()));
                Ok(Effected::value(
                    serde_json::json!({"state":if reviewed.phase(&original) == EnrollmentPhase::Ready {"ready"} else {"awaiting_permission"},"proposal":proposal}),
                ))
            },
        ))
    }
    pub(crate) fn panel(&self, mut content: Column, actions: &Actions<Messages>) -> Column {
        let state = self.state.get();
        let phase = self.phase();
        if phase == EnrollmentPhase::Interrupted {
            return content
                .push(Box::new(Text::new(
                    &CATALOG.t("messages-setup-interrupted"),
                )))
                .push(Box::new(
                    actions.bind_effect_invocation(
                        Button::new("messages.first-account-reset", "messages-first-reset")
                            .with_enabled(self.can_reset()),
                        &MessagesEffect::ResetFirstEnrollment,
                    ),
                ));
        }
        if let Some(private) = &state.pairing
            && phase == EnrollmentPhase::Linking
        {
            return Self::pairing_panel(content, actions, &state, private);
        }
        if phase == EnrollmentPhase::Ready {
            return content.push(Box::new(Text::new(&CATALOG.t("messages-setup-linked"))));
        }
        if let Some(review) = &state.review {
            return Self::review_panel(content, actions, &state, phase, review);
        }
        content = self.sim_panel(content, actions, &state);
        // One row per service, each saying what linking it takes. Without
        // the messaging service on this device none can start, and the
        // section says so instead of offering rows that do nothing.
        let available = self.client.is_some() && !state.pending && state.account.is_none();
        let mut rows: Vec<Element> = Vec::new();
        if self.client.is_none() {
            rows.push(Box::new(
                Text::new(&CATALOG.t("messages-setup-service-missing")).dimmed(),
            ));
        }
        for (id, service, title, hint, icon) in [
            (
                "messages.first-whatsapp",
                "whatsapp",
                "messages-first-whatsapp",
                "messages-first-whatsapp-hint",
                Icon::Phone,
            ),
            (
                "messages.first-account-start",
                "signal",
                "messages-first-signal",
                "messages-first-signal-hint",
                Icon::Lock,
            ),
            (
                "messages.first-tilo",
                "tilo",
                "messages-first-tilo",
                "messages-first-tilo-hint",
                Icon::User,
            ),
        ] {
            rows.push(Box::new(
                actions.bind_effect_invocation(
                    ListRow::new(id, CATALOG.t(title))
                        .secondary(CATALOG.t(hint))
                        .icon(icon)
                        .disclosure()
                        .with_enabled(available),
                    &MessagesEffect::BeginFirstEnrollment {
                        service: service.into(),
                    },
                ),
            ));
        }
        content.push(Box::new(Section::titled(
            CATALOG.t("messages-setup-link-heading"),
            rows,
        )))
    }
    fn review_panel(
        mut content: Column,
        actions: &Actions<Messages>,
        state: &Projection,
        phase: EnrollmentPhase,
        review: &EnrollmentReview,
    ) -> Column {
        let explanation = if review.account.service == "whatsapp" && review.number.is_none() {
            CATALOG.t("messages-whatsapp-storage-explanation")
        } else {
            CATALOG.t_args(
                "messages-first-self-review",
                &[(
                    "identity",
                    ui::i18n::ArgValue::Text(
                        review
                            .number
                            .clone()
                            .unwrap_or_else(|| review.scope.clone()),
                    ),
                )],
            )
        };
        content = content.push(Box::new(Text::new(&explanation)));
        if review.account.service == "whatsapp" && phase == EnrollmentPhase::Linking {
            return content.push(Box::new(
                actions.bind_effect_invocation(
                    Button::new(
                        "messages.first-whatsapp-code",
                        "messages-whatsapp-show-code",
                    )
                    .with_enabled(!state.pending),
                    &MessagesEffect::BeginFirstEnrollment {
                        service: "whatsapp".into(),
                    },
                ),
            ));
        }
        if state.proposal.is_some()
            && !(review.account.service == "whatsapp"
                && review.number.is_some()
                && phase == EnrollmentPhase::Review)
        {
            return content.push(Box::new(Text::new(&CATALOG.t(
                if phase == EnrollmentPhase::Interrupted {
                    "messages-setup-interrupted"
                } else {
                    "messages-first-permission-review"
                },
            ))));
        }
        content.push(Box::new(
            actions.bind_effect_invocation(
                Button::new(
                    "messages.first-account-review",
                    if review.account.service == "whatsapp" && review.number.is_none() {
                        "messages-whatsapp-review-storage"
                    } else {
                        "messages-first-confirm"
                    },
                )
                .with_enabled(!state.pending && phase == EnrollmentPhase::Review),
                &MessagesEffect::ReviewFirstEnrollment {
                    service: review.account.service.clone(),
                    account: review.account.account.clone(),
                    display_name: CATALOG.t(&format!("messages-{}", review.account.service)),
                },
            ),
        ))
    }
    fn pairing_panel(
        mut content: Column,
        actions: &Actions<Messages>,
        state: &Projection,
        private: &Pairing,
    ) -> Column {
        content = content.push(Box::new(Text::new(&CATALOG.t("messages-setup-scan"))));
        content = content.push(Box::new(
            QrWidget::new(private.qr_bytes().to_vec())
                .with_id("messages.first-account-code")
                .with_alt(CATALOG.t("messages-setup-code"))
                .with_secret(),
        ));
        if private.account.service == "whatsapp" {
            content = content.push(Box::new(
                actions.bind_effect_invocation(
                    Button::new(
                        "messages.first-whatsapp-refresh",
                        "messages-whatsapp-refresh-code",
                    )
                    .with_enabled(!state.pending),
                    &MessagesEffect::BeginFirstEnrollment {
                        service: "whatsapp".into(),
                    },
                ),
            ));
        }
        content.push(Box::new(
            actions.bind_effect_invocation(
                Button::new("messages.first-account-finish", "messages-finish-link")
                    .with_enabled(!state.pending),
                &MessagesEffect::CompleteFirstEnrollment {
                    service: private.account.service.clone(),
                    account: private.account.account.clone(),
                    attempt: private.attempt.clone(),
                },
            ),
        ))
    }
    fn sim_panel(
        &self,
        mut content: Column,
        actions: &Actions<Messages>,
        state: &Projection,
    ) -> Column {
        if let Some(choices) = &state.sims {
            if choices.is_empty() {
                content = content.push(Box::new(Text::new(&CATALOG.t("messages-no-sims"))));
            }
            for choice in choices {
                let label = CATALOG.t_args(
                    "messages-select-sim-name",
                    &[("sim", ui::i18n::ArgValue::Text(choice.label.clone()))],
                );
                content = content.push(Box::new(
                    actions.bind_effect_invocation(
                        Button::new(&format!("messages.sim.{}", choice.selection), &label)
                            .with_enabled(!state.pending),
                        &MessagesEffect::SelectSmsSim {
                            selection: choice.selection.clone(),
                        },
                    ),
                ));
            }
        }
        if self
            .client
            .as_ref()
            .is_some_and(EnrollmentClient::supports_sim_selection)
        {
            content = content.push(Box::new(
                actions.bind_effect_invocation(
                    ListRow::new("messages.first-sms", CATALOG.t("messages-first-sms"))
                        .icon(Icon::Phone)
                        .disclosure()
                        .with_enabled(!state.pending && state.account.is_none()),
                    &MessagesEffect::ListSmsSims,
                ),
            ));
        }
        content
    }
}

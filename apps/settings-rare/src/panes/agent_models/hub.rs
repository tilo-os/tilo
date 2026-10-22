//! Hugging Face in Agent Models.
//!
//! The signed catalog's recommendation is a starting point, not the only
//! source: a person searches the Hub, opens any model by its id or link
//! (their own fine-tunes and private models included), reviews each
//! variant's files, size, fit for this device and licence, and downloads
//! one, or adds a GGUF file from this device. Gated models download with
//! the person's Hugging Face sign-in (in the browser, or with a code entered
//! on another device) or a pasted access token; either is kept in this
//! device's vault, where only the model manager reads it.
//! The model manager does every network and store operation; this module
//! shows what it answers, and every control is an [`AgentModelsEffect`].

use std::sync::Arc;

use account_custody::hugging_face::{self, HUGGING_FACE_DOWNLOADS, HubCredential, HubSignIn};
use accounts::{ServiceAuthorization, ServiceCredentials};
use agent_provider_config::local::ModelArtifactFormat;
use agent_provider_config::local::hub::{
    HUB_ORIGIN, HubAccount, HubFitReason, HubFormat, HubModelOffer, HubRef, HubSearchResult,
    HubTokenState, HubVariantOffer, TOKEN_ID, TOKEN_PURPOSE, TOKEN_SERVICE, model_page_url,
};
use agent_provider_config::local::snapshot::HardwareFit;
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow};
use ui::tilo_resource::Effect as _;
use ui::widget::{Button, ButtonRole, ProgressBar, Row, Text, TextInput};

use super::{
    AgentModelsEffect, AgentModelsModel, LocalModelControl, area, byte_size, fit_label, fit_tone,
};
use crate::effects::{SettingsEffect, effect};
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::agent_sign_in::{self, SignInWait, Waited};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Item, SettingsSection, field_row, note, row, rows, section, section_with, semantic_status_pill,
    thing_row, value_row,
};

/// What Agent Models keeps of the Hub between frames.
pub(crate) struct HubBrowser {
    query: LocalState<String>,
    reference: LocalState<String>,
    token: LocalState<String>,
    /// The last search's results, or why it failed.
    pub(crate) results: Option<Result<Vec<HubSearchResult>, String>>,
    /// The model last shown, or why it could not be read.
    pub(crate) offer: Option<Result<HubModelOffer, String>>,
    /// The variant whose download is being reviewed.
    pub(crate) reviewing: Option<String>,
    /// A search or lookup is on its way.
    pub(crate) working: bool,
    /// What is stored here, or why that cannot be read.
    pub(crate) token_state: Result<HubTokenState, String>,
    /// A sign-in waiting for the person; it ends with what it stored.
    signing_in: Option<SignInWait<HubTokenState>>,
}

impl HubBrowser {
    pub(crate) fn new(token_state: Result<HubTokenState, String>) -> Self {
        Self {
            query: LocalState::new(String::new()),
            reference: LocalState::new(String::new()),
            token: LocalState::new(String::new()),
            results: None,
            offer: None,
            reviewing: None,
            working: false,
            token_state,
            signing_in: None,
        }
    }

    /// Whether a sign-in waits for the person.
    pub(crate) fn signing_in(&self) -> bool {
        self.signing_in.is_some()
    }

    /// Stop the sign-in under way.
    pub(crate) fn cancel_sign_in(&mut self) {
        if let Some(wait) = self.signing_in.take() {
            wait.cancel();
        }
    }

    /// Sign in with Hugging Face: in the browser, or (`by_code`) with a code
    /// entered on Hugging Face's page from any device. A worker waits for
    /// the person, asks `userinfo` who signed in and stores the sign-in
    /// through `control` where a pasted token goes.
    ///
    /// # Errors
    /// Why it could not start, for the person.
    pub(crate) fn sign_in(
        &mut self,
        by_code: bool,
        custodian: &account_custody::Custodian,
        opener: &Arc<dyn crate::panes::accounts::BrowserOpener>,
        (userinfo, control): (&str, &Arc<dyn LocalModelControl>),
    ) -> Result<(), String> {
        self.cancel_sign_in();
        let control = Arc::clone(control);
        let store = move |sign_in: HubSignIn| {
            let account = HubAccount {
                username: sign_in.username.clone(),
                name: sign_in.name.clone(),
            };
            control
                .save_hub_token(&HubCredential::encode(&sign_in))
                .map(|()| HubTokenState::SignedIn { account })
        };
        let failed = |error: &dyn std::fmt::Display| {
            t_args(
                "agent-sign-in-failed",
                &[
                    ("service", "Hugging Face".into()),
                    ("error", error.to_string().into()),
                ],
            )
        };
        let userinfo = userinfo.to_owned();
        let wait = if by_code {
            let custodian = custodian.clone();
            SignInWait::device(
                opener,
                move || {
                    custodian
                        .begin_device_sign_in(HUGGING_FACE_DOWNLOADS)
                        .map_err(|error| failed(&error))
                },
                move |device| {
                    hugging_face::finish_device(device, &userinfo)
                        .map_err(|error| failed(&error))
                        .and_then(store)
                },
            )
        } else {
            let authorization = custodian
                .begin_sign_in_for(HUGGING_FACE_DOWNLOADS, None)
                .map_err(|error| failed(&error))?;
            let cancel = authorization
                .canceller()
                .ok_or_else(|| t("agent-sign-in-ports-busy"))?;
            let url = authorization.url().to_owned();
            SignInWait::browser(&url, cancel, opener, move || {
                hugging_face::finish_browser(authorization, &userinfo)
                    .map_err(|error| failed(&error))
                    .and_then(store)
            })
        };
        self.signing_in = Some(wait);
        Ok(())
    }

    /// Take the sign-in's news: the notice to show when it ended, and
    /// whether anything changed.
    pub(crate) fn poll(&mut self) -> (bool, Option<String>) {
        let Some(wait) = self.signing_in.as_mut() else {
            return (false, None);
        };
        match wait.poll() {
            Ok(Waited::Nothing) => (false, None),
            Ok(Waited::Changed) => (true, None),
            Ok(Waited::Done(state)) => (true, Some(self.signed_in(Ok(state)))),
            Err(error) => (true, Some(self.signed_in(Err(error)))),
        }
    }

    /// Block until the sign-in under way ended; the notice it leaves.
    #[cfg(test)]
    pub(crate) fn finish_sign_in_for_test(&mut self) -> Option<String> {
        let outcome = self
            .signing_in
            .as_mut()?
            .finish_for_test(tilo_test_env::hang_bound());
        Some(self.signed_in(outcome))
    }

    /// The sign-in ended with `outcome`: what it stored, or why it failed.
    fn signed_in(&mut self, outcome: Result<HubTokenState, String>) -> String {
        self.signing_in = None;
        match outcome {
            Ok(state) => {
                let notice = match &state {
                    HubTokenState::SignedIn { account } => t_args(
                        "agent-hub-signed-in-as",
                        &[("account", account_label(account).into())],
                    ),
                    _ => t("agent-hub-token-saved"),
                };
                self.token_state = Ok(state);
                notice
            }
            Err(error) => error,
        }
    }

    /// The token typed into the token field, emptied as it is taken.
    pub(crate) fn take_typed_token(&self) -> String {
        let token = self.token.with(|token| token.trim().to_owned());
        self.token.set(String::new());
        token
    }

    /// The shown model's variant `name`, offered at `repo`'s `revision`.
    pub(crate) fn offered(
        &self,
        repo: &str,
        revision: &str,
        name: &str,
    ) -> Option<&HubVariantOffer> {
        match &self.offer {
            Some(Ok(offer)) if offer.repo == repo && offer.revision == revision => {
                offer.variant(name)
            }
            _ => None,
        }
    }

    /// The shown model's variant `name`, when it can be downloaded here.
    pub(crate) fn downloadable(&self, name: &str) -> Option<&HubVariantOffer> {
        match &self.offer {
            Some(Ok(offer)) => offer
                .variant(name)
                .filter(|variant| variant.fit.fit != HardwareFit::Unsupported),
            _ => None,
        }
    }

    /// Finding a model, the shown model, and the token.
    pub(crate) fn sections(&self, cx: &PaneCx, available: bool) -> Vec<SettingsSection> {
        let mut sections = vec![self.find_section(cx, available)];
        if let Some(offer) = &self.offer {
            sections.push(match offer {
                Ok(offer) => self.offer_section(cx, offer),
                Err(error) => section(
                    &t("agent-hub-model-title"),
                    rows(vec![row(crate::widgets::error_caption(&t_args(
                        "agent-hub-model-error",
                        &[("error", error.as_str().into())],
                    )))]),
                ),
            });
        }
        sections.push(self.token_section(cx));
        sections
    }

    fn find_section(&self, cx: &PaneCx, available: bool) -> SettingsSection {
        let mut body = vec![
            note(&t("agent-hub-caption")),
            field_row(
                &t("agent-hub-search"),
                Box::new(
                    TextInput::from_state(&self.query)
                        .with_id("agent-models-hub-query")
                        .with_accessible_name(t("agent-hub-search"))
                        .with_placeholder(t("agent-hub-search-hint")),
                ),
            ),
        ];
        let mut buttons = Row::new();
        for (index, (format, label)) in [
            (HubFormat::Gguf, "agent-hub-search-gguf"),
            (HubFormat::Safetensors, "agent-hub-search-safetensors"),
        ]
        .into_iter()
        .enumerate()
        {
            if index > 0 {
                buttons = buttons.push(crate::widgets::gap(ui::tokens::SpacingStep::S2));
            }
            let query = self.query.clone();
            let mut button = Button::new(&format!("agent-models-hub-search-{index}"), &t(label))
                .with_enabled(available && !self.working);
            if index == 0 {
                button = button.role(ButtonRole::Primary);
            }
            buttons = buttons.push(Box::new(cx.bind(
                button,
                effect(AgentModelsEffect::SEARCH_HUB_MODELS).map(move |()| {
                    (
                        query.with(|query| query.trim().to_owned()),
                        Some(format_name(format).to_owned()),
                    )
                }),
            )));
        }
        body.push(row(Box::new(buttons)));
        Self::push_field(
            cx,
            &mut body,
            (
                "reference",
                "agent-hub-reference",
                "agent-hub-reference-hint",
            ),
            (
                &self.reference,
                "agent-hub-show",
                available && !self.working,
            ),
            AgentModelsEffect::SHOW_HUB_MODEL,
        );
        // The person chooses the file in the file chooser, which hands it
        // over open: neither Settings nor the model service takes a path.
        body.push(note(&t("agent-hub-file")));
        body.push(row(Box::new(
            cx.bind_invocation(
                Button::new("agent-models-hub-file-go", &t("agent-hub-add-file"))
                    .with_enabled(available),
                AgentModelsEffect::AddModelFile,
            ),
        )));
        if self.working {
            body.push(row(Box::new(
                ProgressBar::indeterminate().with_label(t("agent-hub-working")),
            )));
        }
        match &self.results {
            None => {}
            Some(Err(error)) => body.push(row(crate::widgets::error_caption(&t_args(
                "agent-hub-search-error",
                &[("error", error.as_str().into())],
            )))),
            Some(Ok(results)) if results.is_empty() => body.push(note(&t("agent-hub-no-results"))),
            Some(Ok(results)) => {
                for result in results {
                    body.push(row(Box::new(result_row(cx, result, available))));
                }
            }
        }
        section(&t("agent-hub-title"), rows(body))
    }

    /// A field and the button that sends what it holds as `bound`'s one
    /// argument.
    fn push_field(
        cx: &PaneCx,
        body: &mut Vec<Item>,
        (name, label, hint): (&str, &str, &str),
        (state, action, enabled): (&LocalState<String>, &str, bool),
        bound: ui::tilo_resource::ActionRef<AgentModelsEffect, String>,
    ) {
        body.push(field_row(
            &t(label),
            Box::new(
                TextInput::from_state(state)
                    .with_id(&format!("agent-models-hub-{name}"))
                    .with_accessible_name(t(label))
                    .with_placeholder(t(hint)),
            ),
        ));
        let state = state.clone();
        body.push(row(Box::new(cx.bind(
            Button::new(&format!("agent-models-hub-{name}-go"), &t(action)).with_enabled(enabled),
            effect(bound).map(move |()| state.with(|value| value.trim().to_owned())),
        ))));
    }

    fn offer_section(&self, cx: &PaneCx, offer: &HubModelOffer) -> SettingsSection {
        let short: String = offer.revision.chars().take(12).collect();
        let mut body = vec![
            value_row(&t("agent-revision"), &short),
            value_row(&t("agent-license"), &offer.license.id),
        ];
        if offer.gated.is_gated() {
            body.push(note(&t(if offer.signed_in {
                "agent-hub-gated-caption"
            } else {
                "agent-hub-gated-token-caption"
            })));
            body.push(row(Box::new(cx.bind_invocation(
                Button::new("agent-models-hub-open-page", &t("agent-hub-open-page")),
                AgentModelsEffect::OpenHubModelPage {
                    repo: offer.repo.clone(),
                },
            ))));
        }
        if offer.variants.is_empty() {
            body.push(note(&t("agent-hub-no-variants")));
        }
        for variant in &offer.variants {
            let name = &variant.variant.name;
            if self.reviewing.as_deref() == Some(name.as_str()) {
                push_confirm(cx, &mut body, offer, variant);
                continue;
            }
            let mut details = vec![
                format_label(variant),
                byte_size(variant.variant.download_bytes()),
                fit_reason(variant),
            ];
            if !variant.runtime_available {
                details.push(t("agent-hub-no-runtime"));
            }
            let download = Row::new()
                .push(semantic_status_pill(
                    &fit_label(variant.fit.fit),
                    fit_tone(variant.fit.fit),
                ))
                .push(crate::widgets::gap(ui::tokens::SpacingStep::S2))
                .push(Box::new(
                    cx.bind_invocation(
                        Button::new(
                            &format!("agent-models-hub-download-{}", super::token(name)),
                            &t("agent-hub-download"),
                        )
                        .with_enabled(variant.fit.fit != HardwareFit::Unsupported),
                        AgentModelsEffect::ReviewHubVariant {
                            variant: name.clone(),
                            show: true,
                        },
                    ),
                ));
            body.push(row(Box::new(thing_row(
                &format!("agent-models.hub-variant:{name}"),
                name,
                &details,
                Some(Box::new(download)),
            ))));
        }
        section_with(
            &offer.repo,
            Box::new(Text::new(&t("agent-hub-model-title")).dimmed()),
            rows(body),
        )
    }

    fn token_section(&self, cx: &PaneCx) -> SettingsSection {
        let mut body = vec![note(&t("agent-hub-token-caption"))];
        let state = match &self.token_state {
            Ok(state) => state.clone(),
            Err(error) => {
                body.push(row(crate::widgets::error_caption(&t_args(
                    "agent-hub-token-read-error",
                    &[("error", error.as_str().into())],
                ))));
                HubTokenState::None
            }
        };
        if self.token_state.is_ok() {
            let status = match &state {
                HubTokenState::None => t("agent-hub-token-none"),
                HubTokenState::Pasted => t("agent-hub-token-stored"),
                HubTokenState::SignedIn { account } => t_args(
                    "agent-hub-signed-in-as",
                    &[("account", account_label(account).into())],
                ),
                HubTokenState::Ended { account } => t_args(
                    "agent-hub-sign-in-ended",
                    &[("account", account_label(account).into())],
                ),
            };
            body.push(row(Box::new(Text::new(&status).dimmed())));
        }
        if let Some(wait) = &self.signing_in {
            body.push(match wait.device_code() {
                Some((code, page)) => agent_sign_in::device_code_note(code, page),
                None => note(&t("agent-sign-in-waiting")),
            });
            body.push(row(Box::new(cx.bind_invocation(
                Button::new("agent-models-hub-sign-in-cancel", &t("agent-cancel")),
                AgentModelsEffect::CancelHuggingFaceSignIn,
            ))));
        } else {
            let mut buttons = Row::new()
                .push(Box::new(
                    cx.bind_invocation(
                        Button::new("agent-models-hub-sign-in", &t("agent-sign-in-huggingface"))
                            .role(ButtonRole::Primary),
                        AgentModelsEffect::SignInWithHuggingFace,
                    ),
                ))
                .push(crate::widgets::gap(ui::tokens::SpacingStep::S2))
                .push(Box::new(cx.bind_invocation(
                    Button::new(
                        "agent-models-hub-sign-in-code",
                        &t("agent-hub-sign-in-code"),
                    ),
                    AgentModelsEffect::SignInWithHuggingFaceCode,
                )));
            if state.stored() {
                let remove = if matches!(state, HubTokenState::Pasted) {
                    "agent-hub-token-remove"
                } else {
                    "agent-hub-sign-out"
                };
                buttons = buttons
                    .push(crate::widgets::gap(ui::tokens::SpacingStep::S2))
                    .push(Box::new(cx.bind_invocation(
                        Button::new("agent-models-hub-token-remove", &t(remove)),
                        AgentModelsEffect::RemoveHubToken,
                    )));
            }
            body.push(row(Box::new(buttons)));
        }
        body.push(note(&t("agent-hub-token-paste-caption")));
        body.push(field_row(
            &t("agent-hub-token"),
            Box::new(
                TextInput::from_state(&self.token)
                    .with_id("agent-models-hub-token")
                    .with_secret(true)
                    .with_accessible_name(t("agent-hub-token"))
                    .with_placeholder(t("agent-hub-token-hint")),
            ),
        ));
        body.push(row(Box::new(cx.bind_invocation(
            Button::new("agent-models-hub-token-save", &t("agent-hub-token-save")),
            AgentModelsEffect::SaveHubToken { token: None },
        ))));
        section(&t("agent-hub-token-title"), rows(body))
    }

    /// One row per search result and per variant of the shown model, each
    /// with what can be done to it: the same doors the controls use.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let offer = |effect: AgentModelsEffect| SettingsEffect::from(effect).bound();
        let mut found = Vec::new();
        if let Some(Ok(results)) = &self.results {
            for result in results {
                found.push(
                    LiveRow::new(
                        format!("settings.agent-hub-result:{}", result.repo),
                        "settings.agent-hub-result",
                        result.repo.clone(),
                    )
                    .operation(offer(AgentModelsEffect::ShowHubModel {
                        model: result.repo.clone(),
                    })),
                );
            }
        }
        if let Some(Ok(model)) = &self.offer {
            for variant in &model.variants {
                let mut live = LiveRow::new(
                    format!(
                        "settings.agent-hub-variant:{}:{}",
                        model.repo, variant.variant.name
                    ),
                    "settings.agent-hub-variant",
                    format!("{} {}", model.repo, variant.variant.name),
                );
                if variant.fit.fit != HardwareFit::Unsupported {
                    live = live.operation(offer(AgentModelsEffect::InstallHubModel {
                        repo: model.repo.clone(),
                        revision: model.revision.clone(),
                        variant: variant.variant.name.clone(),
                    }));
                }
                found.push(live);
            }
        }
        found
    }
}

/// Answer one of the Hub's effects; the download itself is manager work
/// ([`super::ManagerWork`]).
pub(crate) fn answer(
    mut model: std::cell::RefMut<'_, AgentModelsModel>,
    effect: AgentModelsEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    match effect {
        AgentModelsEffect::ReviewHubVariant { variant, show } => {
            if show && model.hub.downloadable(&variant).is_none() {
                return Err(t("agent-hub-changed"));
            }
            model.hub.reviewing = show.then_some(variant);
            Ok(Effected::done())
        }
        AgentModelsEffect::OpenHubModelPage { repo } => {
            let reference = HubRef::parse(&repo).map_err(|_| t("agent-hub-changed"))?;
            // The portal reports a failure to open on its own.
            drop(model.opener.open(&model_page_url(&reference.repo)));
            Ok(Effected::done())
        }
        AgentModelsEffect::SearchHubModels { query, format } => {
            let format = format.unwrap_or_else(|| format_name(HubFormat::Gguf).into());
            if query.trim().is_empty() {
                return Err(t("agent-hub-query-empty"));
            }
            model.hub.working = true;
            let control = Arc::clone(&model.control);
            drop(model);
            Ok(crate::device_read::job(
                cx,
                move || control.hub_search(query.trim(), &format),
                |app: &mut SettingsApp, results: Result<Vec<HubSearchResult>, String>, _| {
                    let shared = area(app).ok_or_else(crate::area::missing)?.model();
                    let mut model = shared.borrow_mut();
                    model.hub.working = false;
                    model.hub.results = Some(results.clone());
                    model.changed = true;
                    results.map(Effected::value)
                },
            ))
        }
        AgentModelsEffect::ShowHubModel { model: reference } => {
            model.hub.working = true;
            let control = Arc::clone(&model.control);
            drop(model);
            Ok(crate::device_read::job(
                cx,
                move || control.hub_model(reference.trim()),
                |app: &mut SettingsApp, offer: Result<HubModelOffer, String>, _| {
                    let shared = area(app).ok_or_else(crate::area::missing)?.model();
                    let mut model = shared.borrow_mut();
                    model.hub.working = false;
                    model.hub.offer = Some(offer.clone());
                    model.hub.reviewing = None;
                    model.changed = true;
                    offer.map(Effected::value)
                },
            ))
        }
        AgentModelsEffect::SaveHubToken { token } => {
            let token = token.map_or_else(
                || model.hub.take_typed_token(),
                |token| token.trim().to_owned(),
            );
            if token.is_empty() {
                return Err(t("agent-hub-token-empty"));
            }
            let control = Arc::clone(&model.control);
            drop(model);
            Ok(token_job(cx, control, move |control| {
                control
                    .save_hub_token(&token)
                    .map(|()| t("agent-hub-token-saved"))
            }))
        }
        AgentModelsEffect::RemoveHubToken => {
            model.hub.cancel_sign_in();
            let control = Arc::clone(&model.control);
            drop(model);
            Ok(token_job(cx, control, |control| {
                control
                    .forget_hub_token()
                    .map(|()| t("agent-hub-token-removed"))
            }))
        }
        AgentModelsEffect::SignInWithHuggingFace | AgentModelsEffect::SignInWithHuggingFaceCode => {
            let model = &mut *model;
            model.hub.sign_in(
                matches!(effect, AgentModelsEffect::SignInWithHuggingFaceCode),
                &model.custodian,
                &model.opener,
                (&model.hub_userinfo, &model.control),
            )?;
            Ok(Effected::done())
        }
        AgentModelsEffect::CancelHuggingFaceSignIn => {
            model.hub.cancel_sign_in();
            Ok(Effected::done())
        }
        _ => Err("not a Hugging Face effect".into()),
    }
}

/// Change the stored Hugging Face token on a worker, then read back
/// whether one is stored, which the token group shows.
fn token_job(
    cx: &mut EffectCx<Settings, SettingsApp>,
    control: Arc<dyn LocalModelControl>,
    change: impl FnOnce(&dyn LocalModelControl) -> Result<String, String> + Send + 'static,
) -> Effected {
    crate::device_read::job(
        cx,
        move || {
            let outcome = change(control.as_ref());
            (outcome, control.hub_token())
        },
        |app: &mut SettingsApp,
         (outcome, stored): (Result<String, String>, Result<HubTokenState, String>),
         _| {
            let shared = area(app).ok_or_else(crate::area::missing)?.model();
            let mut model = shared.borrow_mut();
            model.hub.token_state = stored;
            model.notice = Some(match &outcome {
                Ok(message) | Err(message) => message.clone(),
            });
            model.changed = true;
            outcome.map(|_| Effected::done())
        },
    )
}

/// The licence confirmation of one variant: what downloads, from where, at
/// which commit, under which licence, with accept and cancel.
fn push_confirm(
    cx: &PaneCx,
    body: &mut Vec<Item>,
    offer: &HubModelOffer,
    variant: &HubVariantOffer,
) {
    let name = &variant.variant.name;
    let widget = super::token(name);
    body.extend([
        value_row(&t("agent-hub-variant"), name),
        value_row(
            &t("agent-download"),
            &byte_size(variant.variant.download_bytes()),
        ),
        value_row(&t("agent-source"), &format!("{HUB_ORIGIN}/{}", offer.repo)),
        value_row(&t("agent-license"), &offer.license.id),
        note(&t_args(
            "agent-license-accept",
            &[("url", offer.license.url.as_str().into())],
        )),
    ]);
    if !variant.runtime_available {
        body.push(note(&t("agent-hub-no-runtime")));
    }
    body.push(row(Box::new(
        Row::new()
            .push(Box::new(
                cx.bind_invocation(
                    Button::new(
                        &format!("agent-models-hub-confirm-{widget}"),
                        &t("agent-accept-download"),
                    )
                    .role(ButtonRole::Primary),
                    AgentModelsEffect::InstallHubModel {
                        repo: offer.repo.clone(),
                        revision: offer.revision.clone(),
                        variant: name.clone(),
                    },
                ),
            ))
            .push(crate::widgets::gap(ui::tokens::SpacingStep::S2))
            .push(Box::new(cx.bind_invocation(
                Button::new(
                    &format!("agent-models-hub-cancel-{widget}"),
                    &t("agent-cancel"),
                ),
                AgentModelsEffect::ReviewHubVariant {
                    variant: name.clone(),
                    show: false,
                },
            ))),
    )));
}

fn result_row(cx: &PaneCx, result: &HubSearchResult, available: bool) -> ui::widget::ListRow {
    let mut details = vec![t_args(
        "agent-hub-result-stats",
        &[
            ("downloads", result.downloads.into()),
            ("likes", result.likes.into()),
        ],
    )];
    if let Some(license) = &result.license {
        details.push(t_args(
            "agent-hub-result-license",
            &[("license", license.as_str().into())],
        ));
    }
    if result.gated {
        details.push(t("agent-hub-gated"));
    }
    thing_row(
        &format!("agent-models.hub-result:{}", result.repo),
        &result.repo,
        &details,
        Some(Box::new(
            cx.bind_invocation(
                Button::new(
                    &format!("agent-models-hub-show-{}", super::token(&result.repo)),
                    &t("agent-hub-show"),
                )
                .with_enabled(available),
                AgentModelsEffect::ShowHubModel {
                    model: result.repo.clone(),
                },
            ),
        )),
    )
}

/// The search filter's name on the model manager's command line.
pub(crate) const fn format_name(format: HubFormat) -> &'static str {
    match format {
        HubFormat::Gguf => "gguf",
        HubFormat::Safetensors => "safetensors",
    }
}

fn format_label(variant: &HubVariantOffer) -> String {
    match variant.variant.format {
        ModelArtifactFormat::LlamaCppGgufShards => t_args(
            "agent-hub-format-shards",
            &[("count", variant.variant.files.len().into())],
        ),
        ModelArtifactFormat::HfSnapshotFiles => t("agent-hub-format-safetensors"),
        _ => t("agent-hub-format-gguf"),
    }
}

fn fit_reason(variant: &HubVariantOffer) -> String {
    match variant.fit.reason {
        HubFitReason::Accelerator => t("agent-hub-fit-accelerator"),
        HubFitReason::AcceleratorAndMemory => t("agent-hub-fit-accelerator-and-memory"),
        HubFitReason::Memory => t("agent-hub-fit-memory"),
        HubFitReason::TooLarge => t_args(
            "agent-hub-fit-too-large",
            &[(
                "size",
                byte_size(variant.fit.needs_memory_mib.saturating_mul(1024 * 1024)).into(),
            )],
        ),
        HubFitReason::ProcessorTooSmall => t("agent-hub-fit-processor-too-small"),
        HubFitReason::NoStorage => t("agent-hub-fit-no-storage"),
    }
}

/// Ask the model manager something it answers in JSON; the Hub may take a
/// while.
pub(crate) fn manager_answer<T: serde::de::DeserializeOwned>(args: &[&str]) -> Result<T, String> {
    crate::device_read::off_ui_thread();
    let output = super::manager_run(args, std::time::Duration::from_secs(2 * 60))?;
    if !output.status.success() {
        return Err(output.stderr_diagnostic());
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("model service returned an invalid answer: {error}"))
}

/// Ask the person for a GGUF model file in the file chooser: its name and
/// the file it opened, or `None` when they cancelled.
pub(crate) fn choose_model_file() -> Result<Option<(String, std::fs::File)>, String> {
    let result =
        ui::portal::PickRequest::open(&t("agent-hub-file-choose"), &t("agent-hub-add-file"))
            .filter(ui::portal::FileFilter::new(&t("agent-hub-file-filter")).glob("*.gguf"))
            .thread_name("settings-model-file")
            .send_open_file()
            .recv();
    match result {
        Ok(ui::portal::OpenFileOutcome::Opened(file)) => {
            let name = file.display_name().to_owned();
            Ok(Some((name, file.into_file())))
        }
        Ok(ui::portal::OpenFileOutcome::Cancelled) => Ok(None),
        Ok(ui::portal::OpenFileOutcome::Failed(error)) => Err(error),
        Err(_) => Err(t("agent-hub-file-chooser-gone")),
    }
}

/// Hand the model manager `file`, called `name`, as its standard input; the
/// model service receives it as a descriptor and copies it into the store.
pub(crate) fn manager_import(name: &str, file: std::fs::File) -> Result<(), String> {
    crate::device_read::off_ui_thread();
    let mut command = std::process::Command::new("tilo-model-manager");
    command.args(["import-file", &format!("--name={name}")]);
    let output = bounded_command::run_prepared_with_stdin(
        command,
        file,
        super::MANAGER_OUTPUT_LIMIT,
        std::time::Duration::from_secs(2 * 60),
    )
    .map_err(|error| {
        t_args(
            "agent-manager-unavailable",
            &[("error", error.to_string().into())],
        )
    })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(output.stderr_diagnostic())
    }
}

/// Ask the model manager to change something, with its reason when it
/// refuses. Values travel as `--name=value`, so one starting with `-` is
/// never read as a flag.
pub(crate) fn manager_request(args: &[&str]) -> Result<(), String> {
    crate::device_read::off_ui_thread();
    let output = super::manager_run(args, std::time::Duration::from_secs(2 * 60))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(output.stderr_diagnostic())
    }
}

/// Download `variant` of `repo` at `revision`, accepting its licence.
pub(crate) fn manager_install(repo: &str, revision: &str, variant: &str) -> Result<(), String> {
    manager_request(&[
        "hub-install",
        &format!("--repo={repo}"),
        &format!("--revision={revision}"),
        &format!("--variant={variant}"),
        "--accept-license",
    ])
}

/// Where the person's token lives in the device vault; the model manager
/// reads it from there for gated and private models.
fn token_binding() -> Result<(accounts::CredentialRef, ServiceAuthorization), String> {
    let authorization = ServiceAuthorization::new(TOKEN_SERVICE, TOKEN_PURPOSE, HUB_ORIGIN)
        .map_err(|error| error.to_string())?;
    let reference = authorization
        .credential_ref(TOKEN_ID)
        .map_err(|error| error.to_string())?;
    Ok((reference, authorization))
}

/// What the model manager reports of the token or sign-in stored here; a
/// sign-in that is due is renewed on the way.
pub(crate) fn token_state() -> Result<HubTokenState, String> {
    manager_answer(&["hub-account"])
}

/// A Hugging Face account as Settings names it.
fn account_label(account: &HubAccount) -> String {
    match &account.name {
        Some(name) => format!("{name} ({})", account.username),
        None => account.username.clone(),
    }
}

/// Store `token` (a pasted one, or a sign-in) in this device's vault.
pub(crate) fn store_token(token: &str) -> Result<(), String> {
    crate::device_read::off_ui_thread();
    let (reference, authorization) = token_binding()?;
    ServiceCredentials::open()
        .store(&reference, &authorization, token)
        .map_err(|error| error.to_string())
}

/// Remove the stored token.
pub(crate) fn forget_token() -> Result<(), String> {
    crate::device_read::off_ui_thread();
    let (reference, authorization) = token_binding()?;
    ServiceCredentials::open()
        .remove(&reference, &authorization)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;

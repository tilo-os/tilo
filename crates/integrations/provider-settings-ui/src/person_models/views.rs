//! What a window shows of a person's model providers.

use std::sync::Arc;

use ui::LocalState;
use ui::tilo_actions::remote::person_models::{AccountUse, ListingProblem, ProviderView};
use ui::tokens::TextRole;
use ui::widget::{
    Button, ButtonRole, Dropdown, FieldRole, Link, Presentation, Row, SecretKind, SecretPrompt,
    Spacer, Text, TextInput, TextInputKind,
};

use super::key_field::KeyField;
use super::{
    ModelBindings, ModelChoice, PersonModels, PersonModelsEffect, READY_MADE, SignIn, Work,
};
use crate::i18n::{t, t_args};
use crate::widgets::{
    El, Item, SettingsSection, block, caption, field_row, field_row_described, row, rows, section,
    thing_row,
};

impl PersonModels {
    /// Everything about the person's providers: the providers, linking
    /// another, and the model each kind of work runs on. Before a model is
    /// linked it is the first minute alone.
    pub fn sections<B: ModelBindings>(&self, bind: &B) -> Vec<SettingsSection> {
        if let Some(signing_in) = self.signing_in_section(bind) {
            return vec![signing_in];
        }
        if self.linked() != Some(true) {
            return self.first_model_sections(bind);
        }
        vec![
            self.providers_section(bind),
            self.add_section(bind),
            self.work_section(bind),
        ]
    }

    /// The first minute, one step at a time: the provider, its key in the
    /// protected field, then the model.
    pub fn first_model_sections<B: ModelBindings>(&self, bind: &B) -> Vec<SettingsSection> {
        if self.kept.is_none() {
            return vec![self.waiting_section(bind)];
        }
        let typing = self
            .key
            .provider
            .as_ref()
            .and_then(|id| self.providers().iter().find(|provider| provider.id == *id));
        if let Some(provider) = typing {
            return vec![section(
                &provider.name,
                rows(vec![block(self.key_field(provider))]),
            )];
        }
        if let Some(provider) = self.providers().iter().find(|provider| provider.configured) {
            return vec![self.choose_model_section(bind, provider)];
        }
        if let Some(signing_in) = self.signing_in_section(bind) {
            return vec![signing_in];
        }
        let mut items = Vec::new();
        for provider in self.providers() {
            items.extend(self.provider_rows(bind, provider));
        }
        items.extend(self.sign_in_rows(bind));
        items.extend(self.ready_made_rows(bind));
        items.extend(self.said());
        vec![section(&t("models-setup-title"), rows(items))]
    }

    /// Before the host answered: nothing yet, or why it cannot.
    fn waiting_section<B: ModelBindings>(&self, bind: &B) -> SettingsSection {
        let mut items: Vec<Item> = self.said().into_iter().collect();
        if !items.is_empty() {
            let again = Button::new("models.retry", &t("models-try-again"));
            items.push(buttons(vec![
                bind.invoking(again, PersonModelsEffect::ReadProviders),
            ]));
        }
        section(&t("models-setup-title"), rows(items))
    }

    /// What the host last refused, where a person reads it.
    fn said(&self) -> Option<Item> {
        self.error.as_ref().map(|reason| block(caption(reason)))
    }

    fn choose_model_section<B: ModelBindings>(
        &self,
        bind: &B,
        provider: &ProviderView,
    ) -> SettingsSection {
        let mut items = self.provider_rows(bind, provider);
        if !self.asking_models(&provider.id) && !provider.models.is_empty() {
            let label = t("models-choose-model");
            items.push(field_row(
                &label,
                chooser(
                    bind,
                    "models.first",
                    &label,
                    String::new(),
                    ModelChoice(None),
                    Self::offered_by(provider).collect(),
                    Some(&t("models-choose-placeholder")),
                ),
            ));
        }
        items.extend(self.said());
        section(&t("models-choose-title"), rows(items))
    }

    fn providers_section<B: ModelBindings>(&self, bind: &B) -> SettingsSection {
        let mut items = Vec::new();
        for provider in self.providers() {
            items.extend(self.provider_rows(bind, provider));
        }
        items.extend(self.said());
        section(&t("models-providers-title"), rows(items))
    }

    fn add_section<B: ModelBindings>(&self, bind: &B) -> SettingsSection {
        let signed_in = self
            .providers()
            .iter()
            .any(|provider| provider.signs_in && provider.configured);
        let mut items = if signed_in {
            Vec::new()
        } else {
            self.sign_in_rows(bind)
        };
        items.extend(self.ready_made_rows(bind));
        section(&t("models-add-title"), rows(items))
    }

    /// Signing in with an `OpenAI` account: to its `ChatGPT` plan, or with
    /// the smaller button to its API usage. A sign-in that failed says why
    /// beneath.
    fn sign_in_rows<B: ModelBindings>(&self, bind: &B) -> Vec<Item> {
        let plan = Button::new("models.sign-in.chatgpt", &t("models-sign-in"))
            .role(ButtonRole::Primary)
            .with_accessible_name(&t("models-sign-in-chatgpt"));
        let plan = bind.invoking(plan, PersonModelsEffect::SignInToOpenAi { using: None });
        let usage = Button::new("models.sign-in.api-usage", &t("models-sign-in-api-usage"));
        let usage = bind.invoking(
            usage,
            PersonModelsEffect::SignInToOpenAi {
                using: Some("api-usage".to_owned()),
            },
        );
        let trailing = Row::new().push(Box::new(usage)).push(Box::new(plan));
        let mut items = vec![row(Box::new(thing_row(
            "models.ready.chatgpt",
            &t("models-chatgpt"),
            &[t("models-chatgpt-description")],
            Some(Box::new(trailing)),
        )))];
        if let SignIn::Failed(reason) = &self.sign_in {
            items.push(block(caption(reason)));
        }
        items
    }

    /// While a sign-in is under way, it alone: the code, where it is
    /// entered, and the way back out.
    fn signing_in_section<B: ModelBindings>(&self, bind: &B) -> Option<SettingsSection> {
        let cancel = || {
            buttons(vec![bind.invoking(
                Button::new("models.sign-in.cancel", &t("models-sign-in-cancel")),
                PersonModelsEffect::CancelOpenAiSignIn,
            )])
        };
        let title = |using: &AccountUse| match using {
            AccountUse::Subscription => t("models-sign-in-title"),
            AccountUse::ApiUsage => t("models-sign-in-api-usage-title"),
        };
        match &self.sign_in {
            SignIn::Beginning(using, _) => Some(section(
                &title(using),
                rows(vec![
                    block(caption(&t("models-sign-in-beginning"))),
                    cancel(),
                ]),
            )),
            SignIn::Waiting { using, started, .. } => Some(section(
                &title(using),
                rows(vec![
                    block(Box::new(Text::new(&t("models-sign-in-enter")))),
                    block(Box::new(
                        Text::new(&started.code).with_role(TextRole::Title),
                    )),
                    block(Box::new(
                        Link::new(&started.page)
                            .with_label(&t("models-sign-in-open-page"))
                            .with_id("models.sign-in.page"),
                    )),
                    block(caption(&t("models-sign-in-waiting"))),
                    cancel(),
                ]),
            )),
            SignIn::Idle | SignIn::Failed(_) => None,
        }
    }

    /// The model each kind of work runs on, in a person's words, with the
    /// one default every other kind follows.
    fn work_section<B: ModelBindings>(&self, bind: &B) -> SettingsSection {
        let offered = self.offered();
        let label = t("models-work-default");
        let mut items = vec![field_row_described(
            &label,
            &t("models-work-default-description"),
            chooser(
                bind,
                "models.default",
                &label,
                self.default_choice(),
                ModelChoice(None),
                offered.clone(),
                None,
            ),
        )];
        for work in Work::ALL {
            let label = t(&format!("models-work-{}", work.id()));
            let mut options = vec![(String::new(), t("models-work-follows-default"))];
            options.extend(offered.iter().cloned());
            items.push(field_row_described(
                &label,
                &t(&format!("models-work-{}-description", work.id())),
                chooser(
                    bind,
                    &format!("models.work.{}", work.id()),
                    &label,
                    self.work_choice(work),
                    ModelChoice(Some(work)),
                    options,
                    None,
                ),
            ));
        }
        section(&t("models-work-title"), rows(items))
    }

    /// One provider: its row, and under it the field taking its key while
    /// that is typed, or what is known about its models.
    fn provider_rows<B: ModelBindings>(&self, bind: &B, provider: &ProviderView) -> Vec<Item> {
        let id = provider.id.as_str();
        // A kept key the provider turns away is never shown as added.
        let state = match (
            provider.signs_in,
            provider.configured,
            self.problems.get(id),
        ) {
            (true, true, _) => t("models-signed-in"),
            (true, false, _) => t("models-signed-out"),
            (false, true, Some(ListingProblem::KeyRefused)) => t("models-key-not-accepted"),
            (false, true, _) => t("models-key-kept"),
            (false, false, _) => t("models-key-none"),
        };
        let key = if provider.signs_in && !provider.configured {
            bind.invoking(
                Button::new(
                    &format!("models.sign-in.again.{id}"),
                    &t("models-sign-in-again"),
                ),
                PersonModelsEffect::SignInToOpenAi { using: None },
            )
        } else if provider.signs_in {
            bind.invoking(
                Button::new(&format!("models.sign-out.{id}"), &t("models-sign-out")),
                PersonModelsEffect::ForgetProviderKey {
                    provider: id.to_owned(),
                },
            )
        } else if provider.configured {
            bind.invoking(
                Button::new(&format!("models.key.remove.{id}"), &t("models-key-remove")),
                PersonModelsEffect::ForgetProviderKey {
                    provider: id.to_owned(),
                },
            )
        } else {
            bind.invoking(
                Button::new(&format!("models.key.add.{id}"), &t("models-key-add")),
                PersonModelsEffect::EnterProviderKey {
                    provider: id.to_owned(),
                },
            )
        };
        let unlink = Button::new(&format!("models.remove.{id}"), &t("models-unlink"))
            .with_accessible_name(&t_args(
                "models-unlink-named",
                &[("provider", provider.name.as_str().into())],
            ));
        let unlink = bind.invoking(
            unlink,
            PersonModelsEffect::UnlinkProvider {
                provider: id.to_owned(),
            },
        );
        let trailing = Row::new().push(Box::new(key)).push(Box::new(unlink));
        let mut items = vec![row(Box::new(thing_row(
            &format!("models.provider.{id}"),
            &provider.name,
            &[state],
            Some(Box::new(trailing)),
        )))];
        if self.key.provider.as_deref() == Some(id) {
            items.push(block(self.key_field(provider)));
        } else if self.asking_models(id) {
            items.push(block(caption(&t_args(
                "models-asking",
                &[("provider", provider.name.as_str().into())],
            ))));
        } else {
            items.extend(self.problem_rows(bind, provider));
        }
        items
    }

    /// Why a provider has no models to choose from, and the one thing to do
    /// about it.
    fn problem_rows<B: ModelBindings>(&self, bind: &B, provider: &ProviderView) -> Vec<Item> {
        let id = provider.id.as_str();
        let says = |key: &str| {
            block(caption(&t_args(
                key,
                &[("provider", provider.name.as_str().into())],
            )))
        };
        let ask = |label: &str| {
            buttons(vec![bind.invoking(
                Button::new(&format!("models.ask.{id}"), &t(label)),
                PersonModelsEffect::AskProviderModels {
                    provider: id.to_owned(),
                },
            )])
        };
        match self.problems.get(id) {
            None if provider.configured && provider.models.is_empty() => {
                vec![says("models-none-listed"), ask("models-ask")]
            }
            None => Vec::new(),
            Some(ListingProblem::NoKey | ListingProblem::KeyRefused) => {
                vec![says("models-problem-no-key")]
            }
            Some(ListingProblem::NoList) => {
                let label = t("models-name-model");
                let name = TextInput::from_state(&self.model_name(id))
                    .with_id(&format!("models.name.{id}"))
                    .with_accessible_name(label.as_str());
                let add = Button::new(
                    &format!("models.name.add.{id}"),
                    &t("models-name-model-add"),
                );
                vec![
                    says("models-problem-no-list"),
                    field_row(&label, Box::new(name)),
                    buttons(vec![bind.invoking(
                        add,
                        PersonModelsEffect::NameProviderModel {
                            provider: id.to_owned(),
                        },
                    )]),
                ]
            }
            Some(ListingProblem::Unreachable) => {
                vec![says("models-problem-unreachable"), ask("models-ask-again")]
            }
            Some(ListingProblem::NotAllowed) => vec![says("models-problem-not-allowed")],
            Some(ListingProblem::Unreadable) => {
                vec![says("models-problem-unreadable"), ask("models-ask-again")]
            }
        }
    }

    /// The providers linked by name the person has not linked yet, then any
    /// other, with its form once it is asked for. One the person linked is
    /// listed once, among theirs, with its key and whatever it needs.
    fn ready_made_rows<B: ModelBindings>(&self, bind: &B) -> Vec<Item> {
        let mut items: Vec<Item> = READY_MADE
            .iter()
            .filter(|preset| {
                !self
                    .providers()
                    .iter()
                    .any(|provider| provider.endpoint == preset.endpoint())
            })
            .map(|preset| {
                let link =
                    Button::new(&format!("models.link.{}", preset.slug()), &t("models-link"))
                        .with_accessible_name(&t_args(
                            "models-link-named",
                            &[("provider", preset.label().into())],
                        ));
                let link = bind.invoking(
                    link,
                    PersonModelsEffect::LinkProvider {
                        preset: preset.slug().to_owned(),
                    },
                );
                row(Box::new(thing_row(
                    &format!("models.ready.{}", preset.slug()),
                    preset.label(),
                    &[preset.description().to_owned()],
                    Some(Box::new(link)),
                )))
            })
            .collect();
        let other = Button::new("models.other", &t("models-link"))
            .with_accessible_name(&t("models-other-link"));
        items.push(row(Box::new(thing_row(
            "models.ready.other",
            &t("models-other"),
            &[t("models-other-description")],
            Some(Box::new(
                bind.invoking(other, PersonModelsEffect::NameOtherProvider),
            )),
        ))));
        if self.naming_other {
            items.extend(self.other_form(bind));
        }
        items
    }

    fn other_form<B: ModelBindings>(&self, bind: &B) -> Vec<Item> {
        let name = t("models-form-name");
        let endpoint = t("models-form-endpoint");
        let protocol = t("models-form-protocol");
        let mut speaks = Dropdown::from_state(Vec::new(), &self.form_protocol)
            .with_id("models.other.protocol")
            .with_resolved_accessible_name(protocol.as_str());
        speaks.set_value_options(vec![
            ("open-ai-compatible".to_owned(), t("models-protocol-openai")),
            ("anthropic".to_owned(), t("models-protocol-anthropic")),
        ]);
        let link =
            Button::new("models.other.link", &t("models-form-link")).role(ButtonRole::Primary);
        vec![
            field_row(
                &name,
                Box::new(
                    TextInput::from_state(&self.form_name)
                        .with_kind(TextInputKind::Name)
                        .with_id("models.other.name")
                        .with_accessible_name(name.as_str()),
                ),
            ),
            field_row_described(
                &endpoint,
                &t("models-form-endpoint-hint"),
                Box::new(
                    TextInput::from_state(&self.form_endpoint)
                        .with_kind(TextInputKind::Url)
                        .with_id("models.other.endpoint")
                        .with_accessible_name(endpoint.as_str()),
                ),
            ),
            field_row(&protocol, Box::new(speaks)),
            buttons(vec![
                bind.invoking(link, PersonModelsEffect::LinkOtherProvider),
            ]),
        ]
    }

    /// The protected field for `provider`'s key. Before the key is kept it
    /// says where what the person writes will go.
    fn key_field(&self, provider: &ProviderView) -> El {
        let asking = self.key.sending.then(|| {
            t_args(
                "models-asking",
                &[("provider", provider.name.as_str().into())],
            )
        });
        let prompt = SecretPrompt::new(SecretKind::Secret)
            .with_presentation(Presentation::Inline)
            .with_id(&format!("models.key.{}", provider.id))
            .with_label(FieldRole::Secret, t("models-key-label"))
            .with_primary_label(t("models-key-add"))
            .with_notice(Some(t_args(
                "models-key-sent-to",
                &[("provider", provider.name.as_str().into())],
            )))
            .with_secret_buffer(Arc::clone(&self.key.buffer))
            .with_error(self.key.refused.clone())
            .with_busy(asking);
        Box::new(KeyField::new(
            &provider.id,
            prompt,
            self.key.answers.clone(),
        ))
    }
}

/// A line of buttons, leading.
fn buttons(buttons: Vec<Button>) -> Item {
    let mut line = Row::new();
    for button in buttons {
        line = line.push(Box::new(button));
    }
    row(Box::new(line.push(Box::new(Spacer::new()))))
}

/// A model chooser showing `selected`, each choice of its recorded as
/// `choice` says.
fn chooser<B: ModelBindings>(
    bind: &B,
    id: &str,
    label: &str,
    selected: String,
    choice: ModelChoice,
    options: Vec<(String, String)>,
    placeholder: Option<&str>,
) -> El {
    let mut chooser = Dropdown::from_state(Vec::new(), &LocalState::new(selected))
        .with_id(id)
        .with_resolved_accessible_name(label);
    if let Some(placeholder) = placeholder {
        chooser = chooser.with_placeholder(placeholder);
    }
    chooser.set_value_options(options);
    Box::new(bind.choosing(chooser, choice))
}

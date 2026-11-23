//! A person's model providers where their workspace runs on a host: the
//! providers they linked, the key the host keeps for each, and the model
//! each kind of their work runs on.
//!
//! The host keeps all of it ([`person_models`]): a page holds no key and
//! asks no provider. Here is the one piece every window shows it with:
//! Settings' page, the Model Providers window, and the first minute of an
//! assistant ([`first_model`]), where nothing can answer before a provider
//! is linked.
//!
//! A key is typed into the protected field ([`key_field`]) and goes to the
//! host on the request only the person themselves can make. No effect here
//! carries one, so nothing that invokes these effects, an agent or a
//! Shortcut, ever holds a key.

mod key_field;
mod surface;
#[cfg(test)]
mod tests;
mod views;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_provider_config::Interaction;
use agent_provider_config::presets::Preset;
use ui::LocalState;
use ui::framework::{ActionControl, Actions};
use ui::lock::MutexRecover as _;
use ui::tilo_actions::InvocationUpdate;
use ui::tilo_actions::remote::person_models::{
    self, AccountUse, AddModel, AddPreset, AddProvider, BeginSignIn, ListedModels, ListingProblem,
    ModelProviders, Protocol, ProviderView, SetAssignments, SetDefault, SetKey, SignInStarted,
    SignInState, WaitSignIn,
};
use ui::tilo_actions::remote::{ActionCallError, InvokeArgument};
use ui::tilo_resource::{self, Binds, Effect, Resource};
use zeroize::Zeroize as _;

use crate::i18n::{t, t_args};
use key_field::KeyAnswers;

pub use surface::{ModelsSurface, everything, everything_or, first_model};

/// The link that opens Settings on a person's model providers.
pub const SETTINGS_LINK: &str = "settings://agent_models";

/// The providers a page links by name, in the order it offers them.
pub const READY_MADE: [Preset; 4] = [
    Preset::GEMINI,
    Preset::ANTHROPIC,
    Preset::OPENAI,
    Preset::OPENROUTER,
];

/// What separates a provider's id from a model's in a chooser's value.
const CHOICE: char = '\u{1}';

/// A kind of a person's work, in their words. Each is one or two of the
/// kinds the model configuration stores ([`Interaction`]): no list of its
/// own, only the names a person knows them by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Work {
    /// What the assistant itself answers and does.
    MainThread,
    /// What it hands to other agents.
    SubAgents,
    /// Sorting and filtering: one label chosen for an input.
    QuickJudgments,
}

impl Work {
    pub const ALL: [Self; 3] = [Self::MainThread, Self::SubAgents, Self::QuickJudgments];

    /// The name an effect takes.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::MainThread => "main-thread",
            Self::SubAgents => "sub-agents",
            Self::QuickJudgments => "quick-judgments",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|work| work.id() == id)
    }

    /// The stored kinds this work is: the main thread answers as a request
    /// and works as a task.
    #[must_use]
    pub const fn kinds(self) -> &'static [Interaction] {
        match self {
            Self::MainThread => &[Interaction::Request, Interaction::Task],
            Self::SubAgents => &[Interaction::Subtask],
            Self::QuickJudgments => &[Interaction::Classifier],
        }
    }
}

/// What a window asks about a person's model providers.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum PersonModelsEffect {
    /// Read the model providers again.
    #[effect(kind = "outside")]
    #[effect(job)]
    ReadProviders,
    /// Link one of the model providers Tilo knows by name.
    #[effect(kind = "outside")]
    #[effect(job)]
    LinkProvider {
        #[param(
            "The provider: `gemini`, `anthropic`, `openai` or `openrouter`",
            labelled_choices = READY_MADE.map(|preset| (preset.slug(), preset.label()))
        )]
        preset: String,
    },
    /// Show the form that names another model provider.
    #[effect(view)]
    NameOtherProvider,
    /// Link the model provider the form names.
    #[effect(kind = "outside")]
    #[effect(job)]
    LinkOtherProvider,
    /// Show the protected field that takes a provider's API key.
    #[effect(view)]
    EnterProviderKey {
        #[param("The provider's id")]
        provider: String,
    },
    /// Remove a provider's API key.
    #[effect(kind = "outside")]
    #[effect(job, destructive)]
    ForgetProviderKey {
        #[param("The provider's id")]
        provider: String,
    },
    /// Remove a provider and its API key.
    #[effect(kind = "outside")]
    #[effect(job, destructive)]
    UnlinkProvider {
        #[param("The provider's id")]
        provider: String,
    },
    /// Ask a provider which models it offers.
    #[effect(kind = "outside")]
    #[effect(job)]
    AskProviderModels {
        #[param("The provider's id")]
        provider: String,
    },
    /// Add the model named in a provider's field.
    #[effect(kind = "outside")]
    #[effect(job)]
    NameProviderModel {
        #[param("The provider's id")]
        provider: String,
    },
    /// Sign in with an `OpenAI` account, to use models on its `ChatGPT`
    /// plan, or with `api-usage` on its API organisation.
    #[effect(kind = "outside")]
    #[effect(job)]
    SignInToOpenAi {
        #[param("`subscription` (the ChatGPT plan, the default) or `api-usage`")]
        using: Option<String>,
    },
    /// Stop the `OpenAI` sign-in that waits for its code.
    #[effect(kind = "outside")]
    #[effect(job)]
    CancelOpenAiSignIn,
    /// Choose the model work runs on where no other is chosen.
    #[effect(kind = "outside")]
    #[effect(job, idempotent)]
    ChooseDefaultModel {
        #[param("The provider's id")]
        provider: String,
        #[param("The model's id, one the provider offers")]
        model: String,
    },
    /// Choose the model one kind of work runs on; omit both to let it
    /// follow the default again.
    #[effect(kind = "outside")]
    #[effect(job, idempotent)]
    ChooseWorkModel {
        #[param("`main-thread`, `sub-agents` or `quick-judgments`")]
        work: String,
        #[param("The provider's id")]
        provider: Option<String>,
        #[param("The model's id, one the provider offers")]
        model: Option<String>,
    },
}

/// How a window binds this piece's controls to the effects it offers.
pub trait ModelBindings {
    /// `control` (a button), bound to `effect`.
    fn invoking<C: ActionControl<()>>(&self, control: C, effect: PersonModelsEffect) -> C;
    /// `control` (a chooser), each choice of its invoking what `choice`
    /// makes of it.
    fn choosing<C: ActionControl<String>>(&self, control: C, choice: ModelChoice) -> C;
}

impl<R: Resource> ModelBindings for Actions<R> {
    fn invoking<C: ActionControl<()>>(&self, control: C, effect: PersonModelsEffect) -> C {
        self.bind_effect_invocation(control, &effect)
    }

    fn choosing<C: ActionControl<String>>(&self, control: C, choice: ModelChoice) -> C {
        self.bind_effect(control, choice)
    }
}

/// A model chooser's choice as the effect that records it: the default's
/// (`None`), or one kind of work's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelChoice(pub Option<Work>);

impl Binds<PersonModelsEffect, String> for ModelChoice {
    fn action_id(&self) -> &'static str {
        match self.0 {
            None => PersonModelsEffect::ChooseDefaultModel {
                provider: String::new(),
                model: String::new(),
            }
            .id(),
            Some(_) => PersonModelsEffect::ChooseWorkModel {
                work: String::new(),
                provider: None,
                model: None,
            }
            .id(),
        }
    }

    fn arguments(
        &self,
        input: String,
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        let chosen = input.split_once(CHOICE);
        let effect = match (self.0, chosen) {
            (None, Some((provider, model))) => PersonModelsEffect::ChooseDefaultModel {
                provider: provider.to_owned(),
                model: model.to_owned(),
            },
            (None, None) => return Err(t("models-choice-incomplete")),
            (Some(work), chosen) => PersonModelsEffect::ChooseWorkModel {
                work: work.id().to_owned(),
                provider: chosen.map(|(provider, _)| provider.to_owned()),
                model: chosen.map(|(_, model)| model.to_owned()),
            },
        };
        Ok(Effect::arguments(&effect))
    }
}

/// What a request out to the host answers, and so what its answer changes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Asked {
    /// A provider just linked: its key is asked for next.
    Linked,
    /// A provider whose key was just kept: its models are asked for next.
    KeyKept(String),
    /// One provider, changed.
    Provider,
    /// A provider's models, asked of it.
    Listing(String),
    /// A provider, with the model named in its field.
    Named(String),
    /// Every provider and what runs where.
    Everything,
}

/// What the host made of a request.
enum Outcome {
    Answered(serde_json::Value),
    Refused(String),
    /// Nothing here keeps a person's providers: no host is in reach.
    Unserved(String),
}

/// An `OpenAI` sign-in, as far as it got.
enum SignIn {
    /// None is under way.
    Idle,
    /// The server is asking `OpenAI` for a code.
    Beginning(AccountUse, ui::InvocationCall),
    /// The person enters `started`'s code on `OpenAI`'s page; the server is
    /// asked how it goes.
    Waiting {
        using: AccountUse,
        started: SignInStarted,
        call: ui::InvocationCall,
    },
    /// It ended without a grant, for this reason.
    Failed(String),
}

/// The key being typed.
struct KeyEntry {
    /// The provider it is for.
    provider: Option<String>,
    /// What is typed, which outlives each rebuild of the field.
    buffer: Arc<Mutex<String>>,
    /// Why the last key was not taken.
    refused: Option<String>,
    /// Whether the key typed is on its way to the host.
    sending: bool,
    answers: KeyAnswers,
}

impl KeyEntry {
    fn open(&mut self, provider: String) {
        self.buffer.lock_recover().zeroize();
        self.provider = Some(provider);
        self.refused = None;
        self.sending = false;
    }

    fn close(&mut self) {
        self.buffer.lock_recover().zeroize();
        self.provider = None;
        self.refused = None;
        self.sending = false;
    }
}

/// A person's model providers as the host answered them, and the requests
/// that change them.
pub struct PersonModels {
    remote: ui::RemoteActions,
    listing: Option<ui::InvocationCall>,
    seen: u64,
    kept: Option<ModelProviders>,
    served: Option<bool>,
    error: Option<String>,
    asked: Vec<(Asked, ui::InvocationCall)>,
    /// Why each provider's models could not be listed, when last asked.
    problems: HashMap<String, ListingProblem>,
    key: KeyEntry,
    sign_in: SignIn,
    naming_other: bool,
    form_name: LocalState<String>,
    form_endpoint: LocalState<String>,
    form_protocol: LocalState<String>,
    /// The model being named for each provider that lists none.
    model_names: RefCell<HashMap<String, LocalState<String>>>,
}

impl PersonModels {
    /// The person's providers, asked of the host `remote` reaches.
    #[must_use]
    pub fn new(remote: ui::RemoteActions) -> Self {
        let mut models = Self::unasked(remote);
        models.read();
        models
    }

    /// The person's providers, not asked for: what a window that only
    /// describes itself holds (a capture, History), which asks no host
    /// anything.
    #[must_use]
    pub fn unasked(remote: ui::RemoteActions) -> Self {
        Self {
            remote,
            listing: None,
            seen: 0,
            kept: None,
            served: None,
            error: None,
            asked: Vec::new(),
            problems: HashMap::new(),
            key: KeyEntry {
                provider: None,
                buffer: Arc::default(),
                refused: None,
                sending: false,
                answers: KeyAnswers::new(),
            },
            sign_in: SignIn::Idle,
            naming_other: false,
            form_name: LocalState::new(String::new()),
            form_endpoint: LocalState::new(String::new()),
            form_protocol: LocalState::new("open-ai-compatible".to_owned()),
            model_names: RefCell::default(),
        }
    }

    fn read(&mut self) {
        self.listing = Some(self.remote.invoke(
            person_models::target(person_models::LIST),
            InvokeArgument::None,
            None,
        ));
        // A new request counts its revisions from the start.
        self.seen = u64::MAX;
    }

    /// Whether a host keeps the person's providers: `None` until it
    /// answered, `false` where none is in reach (a device, a page alone).
    #[must_use]
    pub fn served(&self) -> Option<bool> {
        self.served
    }

    /// Whether the person has a model to run on: a default model of a
    /// provider whose key is kept. `None` until the host answered.
    #[must_use]
    pub fn linked(&self) -> Option<bool> {
        let kept = self.kept.as_ref()?;
        Some(
            kept.default_provider
                .as_ref()
                .and_then(|id| kept.providers.iter().find(|provider| provider.id == *id))
                .is_some_and(|provider| provider.configured && provider.default_model.is_some()),
        )
    }

    /// The providers the host keeps.
    #[must_use]
    pub fn providers(&self) -> &[ProviderView] {
        self.kept.as_ref().map_or(&[], |kept| &kept.providers)
    }

    /// Whether a request that changes something is out, so the window
    /// keeps drawing for it.
    #[must_use]
    pub fn busy(&self) -> bool {
        !self.asked.is_empty()
            || matches!(self.sign_in, SignIn::Beginning(..) | SignIn::Waiting { .. })
    }

    /// The model used where no other is chosen, as a chooser's value;
    /// empty while there is none.
    #[must_use]
    pub fn default_choice(&self) -> String {
        let Some(kept) = &self.kept else {
            return String::new();
        };
        kept.default_provider
            .as_ref()
            .and_then(|id| kept.providers.iter().find(|provider| provider.id == *id))
            .and_then(|provider| {
                let model = provider.default_model.as_ref()?;
                Some(format!("{}{CHOICE}{model}", provider.id))
            })
            .unwrap_or_default()
    }

    /// The model `work` runs on, as a chooser's value; empty while it
    /// follows the default. Where its kinds differ, the one it works as.
    #[must_use]
    pub fn work_choice(&self, work: Work) -> String {
        let Some(kept) = &self.kept else {
            return String::new();
        };
        work.kinds()
            .iter()
            .rev()
            .find_map(|kind| {
                kept.assignments
                    .iter()
                    .find(|chosen| chosen.kind == kind.id())
            })
            .map(|chosen| format!("{}{CHOICE}{}", chosen.provider, chosen.model))
            .unwrap_or_default()
    }

    /// Every model a provider with a key offers, as a chooser's values and
    /// labels.
    #[must_use]
    pub fn offered(&self) -> Vec<(String, String)> {
        self.providers()
            .iter()
            .filter(|provider| provider.configured)
            .flat_map(Self::offered_by)
            .collect()
    }

    fn offered_by(provider: &ProviderView) -> impl Iterator<Item = (String, String)> + '_ {
        provider.models.iter().map(move |model| {
            let label = t_args(
                "agent-kind-model-label",
                &[
                    ("model", model.label.as_str().into()),
                    ("provider", provider.name.as_str().into()),
                ],
            );
            (format!("{}{CHOICE}{}", provider.id, model.id), label)
        })
    }

    fn asking_models(&self, provider: &str) -> bool {
        self.asked.iter().any(
            |(asked, _)| matches!(asked, Asked::Listing(id) | Asked::KeyKept(id) if id == provider),
        )
    }

    fn model_name(&self, provider: &str) -> LocalState<String> {
        self.model_names
            .borrow_mut()
            .entry(provider.to_owned())
            .or_insert_with(|| LocalState::new(String::new()))
            .clone()
    }

    /// Ask the host for what `effect` names.
    ///
    /// # Errors
    /// When the effect names nothing the host could be asked for.
    pub fn apply(&mut self, effect: PersonModelsEffect) -> Result<(), String> {
        let named = |provider: &str| serde_json::json!({ "provider": provider });
        match effect {
            PersonModelsEffect::ReadProviders => {
                self.error = None;
                self.read();
                Ok(())
            }
            PersonModelsEffect::LinkProvider { preset } => self.ask(
                Asked::Linked,
                person_models::ADD_PRESET,
                &AddPreset { preset },
            ),
            PersonModelsEffect::NameOtherProvider => {
                self.naming_other = true;
                Ok(())
            }
            PersonModelsEffect::LinkOtherProvider => {
                let adding = AddProvider {
                    name: self.form_name.get(),
                    protocol: match self.form_protocol.get().as_str() {
                        "anthropic" => Protocol::Anthropic,
                        _ => Protocol::OpenAiCompatible,
                    },
                    endpoint: self.form_endpoint.get(),
                    headers: Vec::new(),
                };
                self.ask(Asked::Linked, person_models::ADD_PROVIDER, &adding)
            }
            PersonModelsEffect::EnterProviderKey { provider } => {
                if !self.providers().iter().any(|kept| kept.id == provider) {
                    return Err(t("models-provider-unknown"));
                }
                self.key.open(provider);
                Ok(())
            }
            PersonModelsEffect::ForgetProviderKey { provider } => self.ask(
                Asked::Provider,
                person_models::REMOVE_KEY,
                &named(&provider),
            ),
            PersonModelsEffect::UnlinkProvider { provider } => {
                if self.key.provider.as_deref() == Some(provider.as_str()) {
                    self.key.close();
                }
                self.problems.remove(&provider);
                self.ask(
                    Asked::Everything,
                    person_models::REMOVE_PROVIDER,
                    &named(&provider),
                )
            }
            PersonModelsEffect::AskProviderModels { provider } => {
                let asking = named(&provider);
                self.ask(
                    Asked::Listing(provider),
                    person_models::REFRESH_MODELS,
                    &asking,
                )
            }
            PersonModelsEffect::NameProviderModel { provider } => {
                let model = self.model_name(&provider).get();
                let adding = AddModel {
                    provider: provider.clone(),
                    model,
                };
                self.ask(Asked::Named(provider), person_models::ADD_MODEL, &adding)
            }
            PersonModelsEffect::SignInToOpenAi { using } => self.begin_sign_in(using.as_deref()),
            PersonModelsEffect::CancelOpenAiSignIn => {
                self.cancel_sign_in();
                Ok(())
            }
            PersonModelsEffect::ChooseDefaultModel { provider, model } => self.ask(
                Asked::Everything,
                person_models::SET_DEFAULT,
                &SetDefault { provider, model },
            ),
            PersonModelsEffect::ChooseWorkModel {
                work,
                provider,
                model,
            } => {
                let work = Work::from_id(&work).ok_or_else(|| t("models-work-unknown"))?;
                let choosing = SetAssignments {
                    kinds: work
                        .kinds()
                        .iter()
                        .map(|kind| kind.id().to_owned())
                        .collect(),
                    provider,
                    model,
                };
                self.ask(Asked::Everything, person_models::SET_ASSIGNMENTS, &choosing)
            }
        }
    }

    /// Begin signing in to `OpenAI`, for its plan or `api-usage`, letting
    /// go of any sign-in under way.
    fn begin_sign_in(&mut self, using: Option<&str>) -> Result<(), String> {
        let using = match using {
            None | Some("subscription") => AccountUse::Subscription,
            Some("api-usage") => AccountUse::ApiUsage,
            Some(_) => return Err(t("models-sign-in-use-unknown")),
        };
        self.cancel_sign_in();
        let call = self.remote.invoke(
            person_models::target(person_models::BEGIN_SIGN_IN),
            json_argument(&BeginSignIn { using })?,
            None,
        );
        self.sign_in = SignIn::Beginning(using, call);
        Ok(())
    }

    /// Let go of the sign-in under way, telling the server.
    fn cancel_sign_in(&mut self) {
        if let SignIn::Waiting { started, .. } = &self.sign_in
            && let Ok(argument) = json_argument(&WaitSignIn {
                sign_in: started.sign_in.clone(),
            })
        {
            // Nothing waits for the answer: the sign-in is let go here either way.
            let _ = self.remote.invoke(
                person_models::target(person_models::CANCEL_SIGN_IN),
                argument,
                None,
            );
        }
        self.sign_in = SignIn::Idle;
    }

    /// Ask the server how the sign-in `started` goes.
    fn wait_sign_in(&self, started: &SignInStarted) -> Result<ui::InvocationCall, String> {
        Ok(self.remote.invoke(
            person_models::target(person_models::WAIT_SIGN_IN),
            json_argument(&WaitSignIn {
                sign_in: started.sign_in.clone(),
            })?,
            None,
        ))
    }

    /// Take what the server answered about the sign-in under way. Whether
    /// anything changed.
    fn poll_sign_in(&mut self) -> bool {
        let outcome = match &self.sign_in {
            SignIn::Beginning(_, call) | SignIn::Waiting { call, .. } => match settled(call) {
                Some(outcome) => outcome,
                None => return false,
            },
            SignIn::Idle | SignIn::Failed(_) => return false,
        };
        let state = std::mem::replace(&mut self.sign_in, SignIn::Idle);
        self.sign_in = match (state, outcome) {
            (_, Outcome::Refused(reason) | Outcome::Unserved(reason)) => SignIn::Failed(reason),
            (SignIn::Beginning(using, _), Outcome::Answered(value)) => {
                match serde_json::from_value::<SignInStarted>(value) {
                    Ok(started) => match self.wait_sign_in(&started) {
                        Ok(call) => SignIn::Waiting {
                            using,
                            started,
                            call,
                        },
                        Err(reason) => SignIn::Failed(reason),
                    },
                    Err(_) => SignIn::Failed(t("models-unreadable")),
                }
            }
            (SignIn::Waiting { using, started, .. }, Outcome::Answered(value)) => {
                match serde_json::from_value::<SignInState>(value) {
                    Ok(SignInState::Waiting) => match self.wait_sign_in(&started) {
                        Ok(call) => SignIn::Waiting {
                            using,
                            started,
                            call,
                        },
                        Err(reason) => SignIn::Failed(reason),
                    },
                    Ok(SignInState::Done { provider }) => {
                        self.keep(provider);
                        // Signing in may have chosen the default model too.
                        self.read();
                        SignIn::Idle
                    }
                    Ok(SignInState::Failed { reason }) => SignIn::Failed(reason),
                    Err(_) => SignIn::Failed(t("models-unreadable")),
                }
            }
            (other, Outcome::Answered(_)) => other,
        };
        true
    }

    fn ask(
        &mut self,
        what: Asked,
        action: &str,
        argument: &impl serde::Serialize,
    ) -> Result<(), String> {
        let argument = json_argument(argument)?;
        let call = self
            .remote
            .invoke(person_models::target(action), argument, None);
        self.asked.push((what, call));
        Ok(())
    }

    /// Take what the person typed and what the host answered since the last
    /// pass. Whether what a window shows of this changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = self.poll_sign_in();
        for (provider, key) in self.key.answers.take() {
            changed = true;
            let Some(mut key) = key else {
                self.key.close();
                continue;
            };
            let setting = SetKey {
                provider: provider.clone(),
                key: std::mem::take(&mut *key),
            };
            self.key.refused = None;
            self.key.sending = true;
            if let Err(reason) =
                self.ask(Asked::KeyKept(provider), person_models::SET_KEY, &setting)
            {
                self.key.sending = false;
                self.key.refused = Some(reason);
            }
            let SetKey { mut key, .. } = setting;
            key.zeroize();
        }
        if let Some(call) = &self.listing {
            let revision = call.revision();
            if revision != self.seen {
                self.seen = revision;
                changed = true;
                match settled(call) {
                    Some(Outcome::Answered(value)) => {
                        self.served = Some(true);
                        self.everything(value);
                    }
                    Some(Outcome::Refused(reason)) => {
                        self.served = Some(true);
                        self.error = Some(reason);
                    }
                    Some(Outcome::Unserved(reason)) => {
                        self.served = Some(false);
                        self.error = Some(reason);
                    }
                    None => {}
                }
            }
        }
        let mut waiting = Vec::new();
        for (what, call) in std::mem::take(&mut self.asked) {
            match settled(&call) {
                None => waiting.push((what, call)),
                Some(outcome) => {
                    changed = true;
                    self.take(what, outcome);
                }
            }
        }
        // An answer may have asked for the next thing.
        waiting.append(&mut self.asked);
        self.asked = waiting;
        changed
    }

    fn take(&mut self, what: Asked, outcome: Outcome) {
        let value = match outcome {
            Outcome::Answered(value) => value,
            Outcome::Refused(reason) | Outcome::Unserved(reason) => {
                if matches!(what, Asked::KeyKept(_)) {
                    self.key.sending = false;
                    self.key.refused = Some(reason);
                } else {
                    self.error = Some(reason);
                }
                return;
            }
        };
        self.error = None;
        match what {
            Asked::Linked => {
                if let Some(provider) = self.provider(value) {
                    self.naming_other = false;
                    self.form_name.set(String::new());
                    self.form_endpoint.set(String::new());
                    self.key.open(provider);
                }
            }
            Asked::KeyKept(provider) => {
                self.provider(value);
                self.key.close();
                let asking = serde_json::json!({ "provider": provider });
                if let Err(reason) = self.ask(
                    Asked::Listing(provider),
                    person_models::REFRESH_MODELS,
                    &asking,
                ) {
                    self.error = Some(reason);
                }
            }
            Asked::Provider => {
                self.provider(value);
            }
            Asked::Listing(provider) => match serde_json::from_value::<ListedModels>(value) {
                Ok(listed) => {
                    let name = listed.provider.name.clone();
                    self.keep(listed.provider);
                    match listed.problem {
                        Some(problem) => {
                            if problem == ListingProblem::KeyRefused {
                                self.key.open(provider.clone());
                                self.key.refused = Some(t_args(
                                    "models-key-refused",
                                    &[("provider", name.as_str().into())],
                                ));
                            }
                            self.problems.insert(provider, problem);
                        }
                        None => {
                            self.problems.remove(&provider);
                        }
                    }
                }
                Err(_) => self.error = Some(t("models-unreadable")),
            },
            Asked::Named(provider) => {
                if self.provider(value).is_some() {
                    self.model_name(&provider).set(String::new());
                }
            }
            Asked::Everything => self.everything(value),
        }
    }

    fn everything(&mut self, value: serde_json::Value) {
        match serde_json::from_value::<ModelProviders>(value) {
            Ok(kept) => {
                self.kept = Some(kept);
                self.error = None;
            }
            Err(_) => self.error = Some(t("models-unreadable")),
        }
    }

    /// Keep the one provider `value` answers, and say which it is.
    fn provider(&mut self, value: serde_json::Value) -> Option<String> {
        let Ok(provider) = serde_json::from_value::<ProviderView>(value) else {
            self.error = Some(t("models-unreadable"));
            return None;
        };
        let id = provider.id.clone();
        self.keep(provider);
        Some(id)
    }

    fn keep(&mut self, provider: ProviderView) {
        let kept = self.kept.get_or_insert_with(ModelProviders::default);
        match kept
            .providers
            .iter_mut()
            .find(|kept| kept.id == provider.id)
        {
            Some(kept) => *kept = provider,
            None => kept.providers.push(provider),
        }
    }
}

/// `argument` as a request's arguments.
fn json_argument(argument: &impl serde::Serialize) -> Result<InvokeArgument, String> {
    match serde_json::to_value(argument).map_err(|error| error.to_string())? {
        serde_json::Value::Object(map) => Ok(InvokeArgument::Json(map)),
        _ => Err("a request's arguments are a JSON object".into()),
    }
}

/// The host's answer to a request that has finished, or `None` while it
/// still runs.
fn settled(call: &ui::InvocationCall) -> Option<Outcome> {
    match call.get() {
        ui::Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => Some(Outcome::Answered(
            output.value.unwrap_or(serde_json::Value::Null),
        )),
        ui::Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
            Some(Outcome::Refused(error.message))
        }
        ui::Arrival::Ready(InvocationUpdate::Cancelled { message, .. }) => Some(Outcome::Refused(
            message.unwrap_or_else(|| t("models-cancelled")),
        )),
        ui::Arrival::Failed(
            error @ (ActionCallError::Unavailable { .. } | ActionCallError::UnknownAction),
        ) => Some(Outcome::Unserved(error.to_string())),
        // The host's own words are the reason; the kind of refusal says
        // nothing more to a person.
        ui::Arrival::Failed(
            ActionCallError::Refused { reason } | ActionCallError::InvalidRequest { reason },
        ) => Some(Outcome::Refused(reason)),
        ui::Arrival::Failed(error) => Some(Outcome::Refused(error.to_string())),
        ui::Arrival::Ready(_) | ui::Arrival::Pending { .. } => None,
    }
}

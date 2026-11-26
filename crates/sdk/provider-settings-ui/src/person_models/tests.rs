#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::{Arc, Mutex};

use ui::framework::ActionControl;
use ui::framework::testing::TestApp;
use ui::lock::MutexRecover as _;
use ui::tilo_actions::ActionOutput;
use ui::tilo_actions::remote::person_models::{Assignment, OfferedModel};
use ui::tilo_actions::remote::{
    ActionProvider, Caller, InvocationReceipt, Pending, ReceiptStatus, TestProvider,
};
use ui::widget::FieldRole;
use zeroize::Zeroizing;

use super::*;
use crate::widgets;

/// The key the fake provider lists its models to.
const GOOD_KEY: &str = "sk-good-key-48213";
/// A key of a provider that lists no models.
const LISTLESS_KEY: &str = "sk-listless-key";

fn en() {
    ui::i18n::I18nManager::init("en-US");
}

/// Controls left as they are made: what a test reads of a page needs no
/// window to offer them.
struct Unbound;

impl ModelBindings for Unbound {
    fn invoking<C: ActionControl<()>>(&self, control: C, _: PersonModelsEffect) -> C {
        control
    }

    fn choosing<C: ActionControl<String>>(&self, control: C, _: ModelChoice) -> C {
        control
    }
}

/// What a fake host keeps for its one person.
#[derive(Default)]
struct Kept {
    providers: ModelProviders,
    keys: HashMap<String, String>,
    /// Whether the person entered the code of the sign-in under way.
    code_entered: bool,
}

impl Kept {
    fn provider(&mut self, id: &str) -> Result<&mut ProviderView, ActionCallError> {
        self.providers
            .providers
            .iter_mut()
            .find(|provider| provider.id == id)
            .ok_or_else(|| refused("That provider is not kept here"))
    }

    /// Answer `action` as the host does, over what is kept.
    fn answer(
        &mut self,
        action: &str,
        argument: &serde_json::Value,
    ) -> Result<serde_json::Value, ActionCallError> {
        match action {
            person_models::LIST => {}
            person_models::REMOVE_PROVIDER
            | person_models::SET_DEFAULT
            | person_models::SET_ASSIGNMENTS => self.choose(action, argument)?,
            _ => return self.change(action, argument),
        }
        Ok(serde_json::to_value(&self.providers).unwrap())
    }

    /// The requests that answer every provider and what runs where.
    fn choose(
        &mut self,
        action: &str,
        argument: &serde_json::Value,
    ) -> Result<(), ActionCallError> {
        let text = |name: &str| argument[name].as_str().unwrap_or_default().to_owned();
        let id = text("provider");
        match action {
            person_models::REMOVE_PROVIDER => {
                self.provider(&id)?;
                self.providers.providers.retain(|kept| kept.id != id);
                self.providers
                    .assignments
                    .retain(|kept| kept.provider != id);
                if self.providers.default_provider.as_deref() == Some(id.as_str()) {
                    self.providers.default_provider = None;
                }
            }
            person_models::SET_DEFAULT => {
                self.provider(&id)?.default_model = Some(text("model"));
                self.providers.default_provider = Some(id);
            }
            _ => {
                let kinds: Vec<String> = argument["kinds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|kind| kind.as_str().map(str::to_owned))
                    .collect();
                self.providers
                    .assignments
                    .retain(|kept| !kinds.contains(&kept.kind));
                if let (Some(provider), Some(model)) =
                    (argument["provider"].as_str(), argument["model"].as_str())
                {
                    self.providers
                        .assignments
                        .extend(kinds.into_iter().map(|kind| Assignment {
                            kind,
                            provider: provider.to_owned(),
                            model: model.to_owned(),
                        }));
                }
            }
        }
        Ok(())
    }

    /// Answer a sign-in's `action`: its code, then waiting until the code
    /// was entered, then signed in.
    fn sign_in(&mut self, action: &str) -> serde_json::Value {
        match action {
            person_models::BEGIN_SIGN_IN => serde_json::to_value(SignInStarted {
                sign_in: "sign-in-1".into(),
                code: "WXYZ-4821".into(),
                page: "https://auth.example/codex/device".into(),
            })
            .unwrap(),
            _ if !self.code_entered => serde_json::to_value(SignInState::Waiting).unwrap(),
            _ => {
                let signed_in = ProviderView {
                    id: "chatgpt".into(),
                    name: "ChatGPT".into(),
                    configured: true,
                    signs_in: true,
                    models: vec![OfferedModel {
                        id: "gpt-5.5".into(),
                        label: "GPT-5.5".into(),
                    }],
                    default_model: Some("gpt-5.5".into()),
                    ..ProviderView::default()
                };
                self.providers.providers.push(signed_in.clone());
                self.providers.default_provider = Some("chatgpt".into());
                serde_json::to_value(SignInState::Done {
                    provider: signed_in,
                })
                .unwrap()
            }
        }
    }

    /// The requests that answer the one provider they changed.
    fn change(
        &mut self,
        action: &str,
        argument: &serde_json::Value,
    ) -> Result<serde_json::Value, ActionCallError> {
        let text = |name: &str| argument[name].as_str().unwrap_or_default().to_owned();
        let view = |provider: &ProviderView| serde_json::to_value(provider).unwrap();
        let id = text("provider");
        match action {
            person_models::ADD_PRESET => {
                let preset = Preset::from_slug(&text("preset")).ok_or_else(|| {
                    refused("That provider is not one this provider adds by name")
                })?;
                let added = ProviderView {
                    id: preset.slug().to_owned(),
                    name: preset.label().to_owned(),
                    endpoint: preset.endpoint().to_owned(),
                    ..ProviderView::default()
                };
                self.providers.providers.push(added.clone());
                Ok(view(&added))
            }
            person_models::ADD_PROVIDER => {
                if text("name").trim().is_empty() {
                    return Err(refused("A provider needs a name"));
                }
                let added = ProviderView {
                    id: "other".into(),
                    name: text("name"),
                    endpoint: text("endpoint"),
                    ..ProviderView::default()
                };
                self.providers.providers.push(added.clone());
                Ok(view(&added))
            }
            person_models::SET_KEY => {
                // As the server does: a key the provider turns away is not kept.
                let key = text("key");
                if ![GOOD_KEY, LISTLESS_KEY].contains(&key.as_str()) {
                    let name = self.provider(&id)?.name.clone();
                    return Err(refused(&format!("{name} did not accept this key.")));
                }
                self.keys.insert(id.clone(), key);
                let provider = self.provider(&id)?;
                provider.configured = true;
                Ok(view(provider))
            }
            person_models::REMOVE_KEY => {
                self.keys.remove(&id);
                let provider = self.provider(&id)?;
                provider.configured = false;
                Ok(view(provider))
            }
            person_models::REFRESH_MODELS => {
                let key = self.keys.get(&id).cloned();
                let provider = self.provider(&id)?;
                let problem = match key.as_deref() {
                    Some(GOOD_KEY) => {
                        provider.models = ["large", "small"]
                            .map(|model| OfferedModel {
                                id: model.into(),
                                label: model.into(),
                            })
                            .to_vec();
                        None
                    }
                    Some(LISTLESS_KEY) => Some(ListingProblem::NoList),
                    Some(_) => Some(ListingProblem::KeyRefused),
                    None => Some(ListingProblem::NoKey),
                };
                Ok(serde_json::to_value(ListedModels {
                    provider: provider.clone(),
                    problem,
                })
                .unwrap())
            }
            person_models::BEGIN_SIGN_IN | person_models::WAIT_SIGN_IN => Ok(self.sign_in(action)),
            person_models::ADD_MODEL => {
                let provider = self.provider(&id)?;
                provider.models.push(OfferedModel {
                    id: text("model"),
                    label: text("model"),
                });
                Ok(view(provider))
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }
}

fn refused(reason: &str) -> ActionCallError {
    ActionCallError::Refused {
        reason: reason.to_owned(),
    }
}

/// A host that keeps a person's providers as the real one does, and what it
/// keeps.
fn provider() -> (Arc<TestProvider>, Arc<Mutex<Kept>>) {
    let kept = Arc::new(Mutex::new(Kept::default()));
    let answering = Arc::clone(&kept);
    let provider = TestProvider::new().answering(move |_, request| {
        let argument = match &request.argument {
            InvokeArgument::Json(map) => serde_json::Value::Object(map.clone()),
            _ => serde_json::Value::Null,
        };
        let answered = answering
            .lock_recover()
            .answer(&request.target.address.action, &argument);
        Pending::ready(answered.map(|value| InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(value),
                    ..Default::default()
                },
            },
        }))
    });
    (Arc::new(provider), kept)
}

fn remote(provider: &Arc<TestProvider>) -> ui::RemoteActions {
    let api: Arc<dyn ActionProvider + Send + Sync> = provider.clone();
    ui::RemoteActions::with_provider(api, Caller::local_person("owner"))
}

/// Take every answer the host has for `models`.
fn settle(models: &mut PersonModels) {
    while models.poll() {}
}

/// What a page of `sections` says, as a screen reader gets it.
fn said(sections: Vec<widgets::SettingsSection>) -> String {
    widgets::announced_text(widgets::page(sections).as_ref()).join("\n")
}

fn type_key(models: &PersonModels, provider: &str, key: &str) {
    models
        .key
        .answers
        .answer(provider, Some(Zeroizing::new(key.to_owned())));
}

/// The actions `host` was asked, oldest first.
fn asked(provider: &TestProvider) -> Vec<String> {
    provider
        .asked()
        .into_iter()
        .map(|request| request.target.address.action)
        .collect()
}

/// A person with one provider linked, its key kept and its models listed.
fn linked_provider(provider: &Arc<TestProvider>) -> PersonModels {
    let mut models = PersonModels::new(remote(provider));
    settle(&mut models);
    models
        .apply(PersonModelsEffect::LinkProvider {
            preset: "gemini".into(),
        })
        .unwrap();
    settle(&mut models);
    type_key(&models, "gemini", GOOD_KEY);
    settle(&mut models);
    models
}

/// The first minute, with nothing set up: the providers by name, the key in
/// the protected field with where what is written will go, then the model.
/// The key goes to the host on the one request that keeps it, and nowhere a
/// page shows or asks anything else.
#[test]
fn the_first_minute_links_a_provider_takes_its_key_and_picks_a_model() {
    en();
    let (provider, kept) = provider();
    let mut models = PersonModels::new(remote(&provider));
    settle(&mut models);
    assert_eq!(models.served(), Some(true));
    assert_eq!(models.linked(), Some(false));
    let offered = said(models.first_model_sections(&Unbound));
    for provider in [
        "Choose a model provider",
        "Google Gemini",
        "Anthropic",
        "OpenAI",
        "OpenRouter",
        "Another provider",
    ] {
        assert!(offered.contains(provider), "{provider}: {offered}");
    }

    models
        .apply(PersonModelsEffect::LinkProvider {
            preset: "gemini".into(),
        })
        .unwrap();
    settle(&mut models);
    assert_eq!(models.key.provider.as_deref(), Some("gemini"));
    // One step at a time: the key's field alone, under the provider's name.
    let asking = models.first_model_sections(&Unbound);
    assert_eq!(asking.len(), 1);
    let asking = said(asking);
    assert!(asking.contains("Google Gemini"), "{asking}");
    assert!(!asking.contains("OpenRouter"), "{asking}");

    type_key(&models, "gemini", GOOD_KEY);
    settle(&mut models);
    assert_eq!(models.key.provider, None);
    assert!(models.key.buffer.lock_recover().is_empty());
    assert_eq!(
        asked(&provider),
        [
            person_models::LIST,
            person_models::ADD_PRESET,
            person_models::SET_KEY,
            person_models::REFRESH_MODELS,
        ]
    );
    assert_eq!(kept.lock_recover().keys["gemini"], GOOD_KEY);
    for request in provider.asked() {
        if request.target.address.action != person_models::SET_KEY {
            let carried = format!("{:?}", request.argument);
            assert!(!carried.contains(GOOD_KEY), "{carried}");
        }
    }

    let choosing = said(models.first_model_sections(&Unbound));
    assert!(choosing.contains("Choose a model"), "{choosing}");
    assert!(!choosing.contains(GOOD_KEY), "{choosing}");
    assert_eq!(
        PersonModels::offered_by(&models.providers()[0])
            .map(|(value, _)| value)
            .collect::<Vec<_>>(),
        ["gemini\u{1}large", "gemini\u{1}small"]
    );
    assert_eq!(models.linked(), Some(false));

    models
        .apply(PersonModelsEffect::ChooseDefaultModel {
            provider: "gemini".into(),
            model: "large".into(),
        })
        .unwrap();
    settle(&mut models);
    assert_eq!(models.linked(), Some(true));
    assert_eq!(models.default_choice(), "gemini\u{1}large");
    assert!(!models.busy());
}

#[test]
fn a_provider_with_no_models_does_not_hide_the_other_ways_to_get_started() {
    en();
    let (provider, kept) = provider();
    kept.lock_recover().providers.providers = vec![
        ProviderView {
            id: "first".into(),
            name: "First provider".into(),
            configured: true,
            ..ProviderView::default()
        },
        ProviderView {
            id: "second".into(),
            name: "Second provider".into(),
            configured: true,
            models: vec![OfferedModel {
                id: "ready".into(),
                label: "Ready model".into(),
            }],
            ..ProviderView::default()
        },
    ];
    let api: Arc<dyn ActionProvider + Send + Sync> = provider;
    let _served = ui::RemoteActions::enter(api, Caller::local_person("owner"));
    let mut app = TestApp::<crate::ProviderSettingsApp>::for_person("owner").unwrap();
    app.settle();
    let options = app
        .with_widget::<ui::widget::Dropdown, _>(|chooser| chooser.options().to_vec())
        .expect("the second provider has a model to choose");
    assert_eq!(options, ["Ready model from Second provider"]);
    app.harness().click_named("models.link.anthropic").unwrap();
    app.settle();
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("sent to Anthropic")
    );
    assert_eq!(kept.lock_recover().providers.providers.len(), 3);
}

/// A key the provider turns away is asked for again, in the same field,
/// with what the provider said about it.
#[test]
fn a_key_the_provider_refuses_is_asked_for_again() {
    en();
    let (provider, _) = provider();
    let mut models = PersonModels::new(remote(&provider));
    settle(&mut models);
    models
        .apply(PersonModelsEffect::LinkProvider {
            preset: "anthropic".into(),
        })
        .unwrap();
    settle(&mut models);
    type_key(&models, "anthropic", "sk-not-a-key");
    settle(&mut models);
    assert_eq!(models.key.provider.as_deref(), Some("anthropic"));
    assert_eq!(
        models.key.refused.as_deref(),
        Some("Anthropic did not accept this key.")
    );
    assert_eq!(models.linked(), Some(false));
    assert!(models.key.buffer.lock_recover().is_empty());
    let shown = said(models.sections(&Unbound));
    assert!(!shown.contains("Key added"), "{shown}");

    // Letting the field go leaves the provider, which still needs its key.
    models.key.answers.answer("anthropic", None);
    settle(&mut models);
    assert_eq!(models.key.provider, None);
    let waiting = said(models.first_model_sections(&Unbound));
    assert!(waiting.contains("Choose a model"), "{waiting}");
    // One Anthropic, among the person's providers: never offered again to link.
    assert!(
        !waiting.contains(Preset::ANTHROPIC.description()),
        "the linked provider is not offered again: {waiting}"
    );
    assert_eq!(
        waiting.matches("Gemini models from").count(),
        1,
        "{waiting}"
    );

    type_key(&models, "anthropic", GOOD_KEY);
    settle(&mut models);
    assert_eq!(models.providers()[0].models.len(), 2);
}

/// A kept key the provider turns away later, once it was taken back there,
/// is shown as not accepted, never as added.
#[test]
fn a_kept_key_the_provider_turns_away_later_is_not_shown_as_added() {
    en();
    let (provider, kept) = provider();
    let mut models = linked_provider(&provider);
    assert!(said(models.sections(&Unbound)).contains("Key added"));

    kept.lock_recover()
        .keys
        .insert("gemini".into(), "sk-taken-back-there".into());
    models
        .apply(PersonModelsEffect::AskProviderModels {
            provider: "gemini".into(),
        })
        .unwrap();
    settle(&mut models);
    // The page asks for a key again, in the provider's own field.
    assert_eq!(models.key.provider.as_deref(), Some("gemini"));
    let asking = said(models.sections(&Unbound));
    assert!(!asking.contains("Key added"), "{asking}");

    // Let go, the provider says its key was not accepted.
    models.key.answers.answer("gemini", None);
    settle(&mut models);
    let shown = said(models.sections(&Unbound));
    assert!(shown.contains("Key not accepted"), "{shown}");
    assert!(!shown.contains("Key added"), "{shown}");
}

/// A provider that lists no models takes the model's name typed, and that
/// model is then the one to choose.
#[test]
fn a_provider_with_no_list_takes_a_model_by_name() {
    en();
    let (provider, _) = provider();
    let mut models = PersonModels::new(remote(&provider));
    settle(&mut models);
    models.form_name.set("Our server".into());
    models.form_endpoint.set("https://models.example/v1".into());
    models.apply(PersonModelsEffect::NameOtherProvider).unwrap();
    assert!(said(models.first_model_sections(&Unbound)).contains("Address"));
    models.apply(PersonModelsEffect::LinkOtherProvider).unwrap();
    settle(&mut models);
    type_key(&models, "other", LISTLESS_KEY);
    settle(&mut models);
    let listless = said(models.first_model_sections(&Unbound));
    assert!(
        listless.contains("Our server has no list of models"),
        "{listless}"
    );

    models.model_name("other").set("typed-model".into());
    models
        .apply(PersonModelsEffect::NameProviderModel {
            provider: "other".into(),
        })
        .unwrap();
    settle(&mut models);
    assert_eq!(models.providers()[0].models[0].id, "typed-model");
    assert_eq!(models.model_name("other").get(), "");
    let named = provider
        .asked()
        .into_iter()
        .find(|request| request.target.address.action == person_models::ADD_MODEL)
        .expect("the provider was asked to add the model");
    let InvokeArgument::Json(argument) = named.argument else {
        panic!("a model is named as JSON");
    };
    assert_eq!(argument["model"], serde_json::json!("typed-model"));

    // A provider with no name is the host's to refuse, said where the
    // person reads it.
    models.form_name.set(String::new());
    models.apply(PersonModelsEffect::LinkOtherProvider).unwrap();
    settle(&mut models);
    assert_eq!(models.error.as_deref(), Some("A provider needs a name"));
}

/// A model for each kind of work, in a person's words: the main thread is
/// the two kinds it answers and works as, chosen together, and a kind left
/// alone follows the default.
#[test]
fn each_kind_of_work_takes_a_model_in_a_persons_words() {
    en();
    let (provider, kept) = provider();
    let mut models = linked_provider(&provider);
    let choose = |models: &mut PersonModels, work: Work, model: Option<&str>| {
        models
            .apply(PersonModelsEffect::ChooseWorkModel {
                work: work.id().into(),
                provider: model.map(|_| "gemini".into()),
                model: model.map(str::to_owned),
            })
            .unwrap();
        settle(models);
    };
    models
        .apply(PersonModelsEffect::ChooseDefaultModel {
            provider: "gemini".into(),
            model: "small".into(),
        })
        .unwrap();
    settle(&mut models);
    choose(&mut models, Work::MainThread, Some("large"));
    choose(&mut models, Work::QuickJudgments, Some("small"));

    let stored = |kind: &str| {
        kept.lock_recover()
            .providers
            .assignments
            .iter()
            .find(|chosen| chosen.kind == kind)
            .map(|chosen| chosen.model.clone())
    };
    assert_eq!(stored("request").as_deref(), Some("large"));
    assert_eq!(stored("task").as_deref(), Some("large"));
    assert_eq!(stored("classifier").as_deref(), Some("small"));
    assert_eq!(stored("subtask"), None);
    assert_eq!(models.work_choice(Work::MainThread), "gemini\u{1}large");
    assert_eq!(models.work_choice(Work::SubAgents), "");
    assert_eq!(models.default_choice(), "gemini\u{1}small");

    let page = said(models.sections(&Unbound));
    for words in [
        "What each kind of work runs on",
        "Default",
        "Main thread",
        "Sub-agents",
        "Quick judgments",
        "Model providers",
        "Link another provider",
    ] {
        assert!(page.contains(words), "{words}: {page}");
    }

    // With neither provider nor model the main thread follows the default
    // again, in one request for both of its kinds.
    choose(&mut models, Work::MainThread, None);
    assert_eq!(stored("request"), None);
    assert_eq!(stored("task"), None);
    assert_eq!(models.work_choice(Work::MainThread), "");
    let together = provider
        .asked()
        .into_iter()
        .rfind(|request| request.target.address.action == person_models::SET_ASSIGNMENTS)
        .expect("the choice went to the provider");
    let InvokeArgument::Json(argument) = together.argument else {
        panic!("a choice travels as JSON");
    };
    assert_eq!(argument["kinds"], serde_json::json!(["request", "task"]));

    assert!(
        models
            .apply(PersonModelsEffect::ChooseWorkModel {
                work: "errands".into(),
                provider: None,
                model: None,
            })
            .is_err()
    );
}

/// What a chooser hands over is the effect that records it, so a person's
/// choice, a script's and an agent's are the same request.
#[test]
fn a_choosers_choice_is_the_effect_that_records_it() {
    en();
    let read = |choice: ModelChoice, input: &str| {
        let arguments = choice.arguments(input.to_owned())?;
        PersonModelsEffect::decode(choice.action_id(), &arguments)
            .map_err(|error| error.to_string())
    };
    assert_eq!(
        read(ModelChoice(None), "gemini\u{1}large"),
        Ok(PersonModelsEffect::ChooseDefaultModel {
            provider: "gemini".into(),
            model: "large".into(),
        })
    );
    assert!(read(ModelChoice(None), "").is_err());
    assert_eq!(
        read(ModelChoice(Some(Work::SubAgents)), "anthropic\u{1}sonnet"),
        Ok(PersonModelsEffect::ChooseWorkModel {
            work: "sub-agents".into(),
            provider: Some("anthropic".into()),
            model: Some("sonnet".into()),
        })
    );
    assert_eq!(
        read(ModelChoice(Some(Work::QuickJudgments)), ""),
        Ok(PersonModelsEffect::ChooseWorkModel {
            work: "quick-judgments".into(),
            provider: None,
            model: None,
        })
    );
    for work in Work::ALL {
        assert_eq!(Work::from_id(work.id()), Some(work));
    }
}

/// Removing a provider takes it from what is shown, with the key field
/// that stood open for it.
#[test]
fn a_provider_removed_is_gone_with_its_key_field() {
    en();
    let (provider, kept) = provider();
    let mut models = linked_provider(&provider);
    models
        .apply(PersonModelsEffect::EnterProviderKey {
            provider: "gemini".into(),
        })
        .unwrap();
    assert!(
        models
            .apply(PersonModelsEffect::EnterProviderKey {
                provider: "nobody's".into(),
            })
            .is_err()
    );
    models
        .apply(PersonModelsEffect::UnlinkProvider {
            provider: "gemini".into(),
        })
        .unwrap();
    settle(&mut models);
    assert!(models.providers().is_empty());
    assert_eq!(models.key.provider, None);
    assert!(kept.lock_recover().providers.providers.is_empty());
}

/// Where no host keeps a person's providers the piece says so, and asks
/// again when told to.
#[test]
fn where_no_host_is_in_reach_the_piece_says_so() {
    en();
    let provider = Arc::new(TestProvider::new().answering(|_, _| {
        Pending::ready(Err(ActionCallError::Unavailable {
            reason: "The workspace's server can't be reached from here right now".into(),
        }))
    }));
    let mut models = PersonModels::new(remote(&provider));
    settle(&mut models);
    assert_eq!(models.served(), Some(false));
    assert_eq!(models.linked(), None);
    assert!(!models.busy());
    let page = said(models.first_model_sections(&Unbound));
    assert!(page.contains("can't be reached"), "{page}");
    assert!(page.contains("Try again"), "{page}");

    models.apply(PersonModelsEffect::ReadProviders).unwrap();
    settle(&mut models);
    assert_eq!(provider.asked().len(), 2);
}

/// The Model Providers window of a person whose workspace runs on a host
/// shows what that host keeps, and the first minute runs through the
/// window's own controls: the provider's button, the protected field, the
/// model's effect. The key is in nothing the window publishes.
#[test]
fn the_window_links_a_provider_through_its_own_controls() {
    en();
    let (provider, kept) = provider();
    let api: Arc<dyn ActionProvider + Send + Sync> = provider.clone();
    let _served = ui::RemoteActions::enter(api, Caller::local_person("owner"));
    let mut app = TestApp::<crate::ProviderSettingsApp>::for_person("owner").unwrap();
    app.settle();
    let offered = app.harness().inspection_json().unwrap();
    assert!(offered.contains("Choose a model provider"), "{offered}");
    assert!(!offered.contains("set up by an administrator"), "{offered}");

    app.harness().click_named("models.link.anthropic").unwrap();
    app.settle();
    assert_eq!(
        asked(&provider),
        [person_models::LIST, person_models::ADD_PRESET]
    );
    let asking = app.harness().inspection_json().unwrap();
    assert!(asking.contains("sent to Anthropic"), "{asking}");

    app.with_widget::<key_field::KeyField, _>(|field| {
        let prompt = field.prompt_mut();
        assert!(prompt.edit_field(FieldRole::Secret, |field| field.set_text(GOOD_KEY)));
        prompt.submit();
    })
    .expect("the key's protected field stands in the window");
    app.settle();
    assert_eq!(kept.lock_recover().keys["anthropic"], GOOD_KEY);
    let choosing = app.harness().inspection_json().unwrap();
    assert!(choosing.contains("Choose a model"), "{choosing}");
    assert!(!choosing.contains(GOOD_KEY), "{choosing}");
    assert!(!format!("{:?}", app.history()).contains(GOOD_KEY));

    app.effect(&PersonModelsEffect::ChooseDefaultModel {
        provider: "anthropic".into(),
        model: "large".into(),
    })
    .unwrap();
    app.settle();
    assert_eq!(
        kept.lock_recover().providers.default_provider.as_deref(),
        Some("anthropic")
    );
    let page = app.harness().inspection_json().unwrap();
    for words in [
        "Model providers",
        "What each kind of work runs on",
        "Main thread",
        "Sub-agents",
        "Quick judgments",
    ] {
        assert!(page.contains(words), "{words}: {page}");
    }
}

/// Signing in with an `OpenAI` account from a page: the server's one-time
/// code and `OpenAI`'s page are shown while the server waits, and once the
/// code was entered the plan is the model everything is asked on, with no key
/// typed anywhere.
#[test]
fn a_page_signs_in_to_chatgpt_with_a_code_and_uses_the_plan() {
    en();
    let (provider, kept) = provider();
    let mut models = PersonModels::new(remote(&provider));
    settle(&mut models);
    let offered = said(models.first_model_sections(&Unbound));
    assert!(offered.contains("ChatGPT"), "{offered}");
    assert!(offered.contains("no API key needed"), "{offered}");

    models
        .apply(PersonModelsEffect::SignInToOpenAi { using: None })
        .unwrap();
    // The server answers each wait at once here: a few passes are a few waits.
    for _ in 0..4 {
        models.poll();
    }
    assert!(models.busy());
    let waiting = said(models.sections(&Unbound));
    assert!(waiting.contains("Sign in to ChatGPT"), "{waiting}");
    assert!(waiting.contains("WXYZ-4821"), "{waiting}");
    assert!(waiting.contains("Open OpenAI's sign-in page"), "{waiting}");
    assert!(!waiting.contains("Google Gemini"), "{waiting}");

    kept.lock_recover().code_entered = true;
    for _ in 0..4 {
        models.poll();
    }
    settle(&mut models);
    assert!(!models.busy());
    assert_eq!(models.linked(), Some(true));
    let signed_in = said(models.sections(&Unbound));
    assert!(signed_in.contains("Signed in"), "{signed_in}");
    assert!(!signed_in.contains("Add key"), "{signed_in}");
    let asked = asked(&provider);
    assert_eq!(asked.first().map(String::as_str), Some(person_models::LIST));
    assert!(
        asked.contains(&person_models::BEGIN_SIGN_IN.to_owned()),
        "{asked:?}"
    );
    assert!(
        !asked.contains(&person_models::SET_KEY.to_owned()),
        "{asked:?}"
    );
}

/// A sign-in let go of tells the server, and the page is back where it was.
#[test]
fn a_sign_in_is_cancelled_on_the_server_too() {
    en();
    let (provider, _kept) = provider();
    let mut models = PersonModels::new(remote(&provider));
    settle(&mut models);
    models
        .apply(PersonModelsEffect::SignInToOpenAi {
            using: Some("api-usage".into()),
        })
        .unwrap();
    for _ in 0..3 {
        models.poll();
    }
    assert!(said(models.sections(&Unbound)).contains("API usage"));
    models
        .apply(PersonModelsEffect::CancelOpenAiSignIn)
        .unwrap();
    assert!(!models.busy());
    assert!(asked(&provider).contains(&person_models::CANCEL_SIGN_IN.to_owned()));
    assert!(said(models.sections(&Unbound)).contains("Choose a model provider"));
}

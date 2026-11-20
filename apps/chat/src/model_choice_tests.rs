//! What the switcher offers: every model of every provider whose key the server keeps, and the
//! name a reply's model goes by.

use super::*;
use tilo_actions::remote::person_models::{OfferedModel, Protocol};

fn provider(id: &str, name: &str, configured: bool, models: &[(&str, &str)]) -> ProviderView {
    ProviderView {
        id: id.to_owned(),
        name: name.to_owned(),
        protocol: Protocol::default(),
        endpoint: "https://models.example.com/v1".to_owned(),
        configured,
        signs_in: false,
        models: models
            .iter()
            .map(|(id, label)| OfferedModel {
                id: (*id).to_owned(),
                label: (*label).to_owned(),
            })
            .collect(),
        default_model: models.first().map(|(id, _)| (*id).to_owned()),
        reasoning: None,
    }
}

/// The choices a server's answer offers, as a window that asked and was answered holds them.
fn choices_of(providers: Vec<ProviderView>, default: Option<&str>) -> ModelChoices {
    let choices = ModelChoices::new(true);
    *choices.kept.borrow_mut() = Some(ModelProviders {
        providers,
        default_provider: default.map(str::to_owned),
        assignments: Vec::new(),
    });
    choices
}

#[test]
fn every_model_of_every_connected_provider_is_offered_and_named_by_its_provider() {
    let choices = choices_of(
        vec![
            provider(
                "chatgpt",
                "ChatGPT",
                true,
                &[("gpt-5.5", "GPT-5.5"), ("gpt-5.5-mini", "GPT-5.5 mini")],
            ),
            provider("anthropic", "Anthropic", true, &[("sonnet", "Sonnet")]),
            // A provider whose key was removed offers nothing: an agent cannot use it.
            provider("local", "Local", false, &[("llama", "Llama")]),
        ],
        Some("chatgpt"),
    );
    assert_eq!(
        choices.choices(),
        Some(vec![
            Choice {
                value: "chatgpt/gpt-5.5".into(),
                label: "GPT-5.5 · ChatGPT".into(),
            },
            Choice {
                value: "chatgpt/gpt-5.5-mini".into(),
                label: "GPT-5.5 mini · ChatGPT".into(),
            },
            Choice {
                value: "anthropic/sonnet".into(),
                label: "Sonnet · Anthropic".into(),
            },
        ])
    );
    // The agent's model names a choice by provider/model, by the model's
    // id, or by what a person reads on it.
    assert_eq!(
        choices.find("anthropic/sonnet").map(|found| found.label),
        Some("Sonnet · Anthropic".into())
    );
    assert_eq!(
        choices.find("sonnet").map(|found| found.value),
        Some("anthropic/sonnet".into())
    );
    assert_eq!(
        choices.find("Sonnet · Anthropic").map(|found| found.value),
        Some("anthropic/sonnet".into())
    );
    // Nothing the server keeps answers nothing.
    assert_eq!(choices.find("local/llama"), None);
    // The model an agent with none of its own answers on.
    assert_eq!(
        choices.default_model().map(|found| found.value),
        Some("chatgpt/gpt-5.5".into())
    );
    // A model chosen on a device this server does not share keeps its own name.
    assert_eq!(
        choices.label(Some("elsewhere/model")),
        Some("elsewhere/model".into())
    );
}

#[test]
fn a_capture_asks_nothing_and_offers_nothing() {
    let choices = ModelChoices::new(false);
    choices.poll();
    assert_eq!(choices.choices(), None);
    assert_eq!(
        choices.label(Some("chatgpt/gpt-5.5")),
        Some("chatgpt/gpt-5.5".into())
    );
}

#[test]
fn the_listing_is_read_from_the_server_that_keeps_the_providers() {
    use std::sync::Arc;

    use tilo_actions::ActionOutput;
    use tilo_actions::remote::{
        ActionProvider, Caller, InvocationReceipt, Pending, ReceiptStatus, TestProvider,
    };

    let answer = ModelProviders {
        providers: vec![
            provider(
                "chatgpt",
                "ChatGPT",
                true,
                &[("gpt-5.5", "GPT-5.5"), ("gpt-5.5-mini", "GPT-5.5 mini")],
            ),
            provider("anthropic", "Anthropic", true, &[("sonnet", "Sonnet")]),
        ],
        default_provider: Some("chatgpt".to_owned()),
        assignments: Vec::new(),
    };
    let provider = Arc::new(TestProvider::new().answering(move |_caller, request| {
        assert_eq!(request.target.address.action, person_models::LIST);
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(serde_json::to_value(&answer).unwrap()),
                    ..ActionOutput::default()
                },
            },
        }))
    }));
    let served: Arc<dyn ActionProvider + Send + Sync> = provider;
    let _entered = ui::RemoteActions::enter(served, Caller::local_person("ada"));

    let choices = ModelChoices::new(true);
    choices.poll();
    assert_eq!(
        choices.choices().map(|offered| offered.len()),
        Some(3),
        "the server's own listing, every model it keeps"
    );
    assert_eq!(
        choices.default_model().map(|found| found.value),
        Some("chatgpt/gpt-5.5".into())
    );
}

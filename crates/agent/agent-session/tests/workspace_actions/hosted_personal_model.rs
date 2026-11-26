//! The keeper links the same loopback provider served by the tilo.sh rehearsal, through Settings.
use super::{Browser, Hosted, InvokeArgument, PersonalAssistant};
use fake_model_providers::FakeProviders;
use serde_json::{Value, json};
use tilo_actions::remote::person_models;

const MODEL: &str = "gpt-5.5";
const FIXTURE_KEY: &str = "sk-fake-api-personal-assistant";
pub(super) const REPLY: &str = "[OpenAI gpt-5.5] You said:";

async fn ask(page: &mut Browser, verb: &str, args: Value) -> Value {
    let target = person_models::target(verb);
    page.invoke_on(
        &target.provider,
        target.address,
        InvokeArgument::Json(args.as_object().unwrap().clone()),
    )
    .await
    .unwrap_or_else(|error| panic!("model settings action {verb} failed: {error}"))
    .value
    .expect("model settings return their current state")
}

pub(super) async fn link(server: &Hosted, assistant: &mut PersonalAssistant) -> FakeProviders {
    let fake = FakeProviders::start().await.unwrap();
    server
        .workspaces()
        .unwrap()
        .call_private_model_addresses(true);
    let page = &mut assistant.page;
    let added = ask(
        page,
        person_models::ADD_PROVIDER,
        json!({"name": "Local model", "protocol": "open-ai-compatible", "endpoint": fake.openai_api_url()}),
    )
    .await;
    let provider = added["id"].as_str().expect("the provider has an id");
    let kept = ask(
        page,
        person_models::SET_KEY,
        json!({"provider": provider, "key": FIXTURE_KEY}),
    )
    .await;
    assert_eq!(kept["configured"], true);
    let listed = ask(
        page,
        person_models::REFRESH_MODELS,
        json!({"provider": provider}),
    )
    .await;
    let listed: person_models::ListedModels = serde_json::from_value(listed).unwrap();
    assert_eq!(listed.problem, None);
    assert!(listed.provider.models.iter().any(|model| model.id == MODEL));
    ask(
        page,
        person_models::SET_ASSIGNMENTS,
        json!({"provider": provider, "model": MODEL, "kinds": ["request"]}),
    )
    .await;
    fake
}

pub(super) fn assert_streamed(fake: &FakeProviders) {
    assert!(
        fake.requests().iter().any(|request| {
            request.method == "POST"
                && request.path.starts_with("/openai/v1/")
                && request.json["model"] == MODEL
                && request.json["stream"] == true
                && request
                    .header("authorization")
                    .and_then(|auth| auth.strip_prefix("Bearer "))
                    == Some(FIXTURE_KEY)
        }),
        "the assistant's turn reached the keeper's model as an authenticated streaming request"
    );
}

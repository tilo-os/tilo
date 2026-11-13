//! The page's model service end to end against a fake provider on loopback:
//! Model Providers saves a provider and its key through the service, and a
//! window's request reaches the provider with that key and comes back as
//! the window's answer. The provider here is a local fixture, never a real
//! one, and its key a placeholder.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test fixture: a failed step fails the test"
)]
use std::sync::Arc;

use test_http::{Reply, Responder};

use agent_provider_config::management::{CredentialStatus, ProviderAdmin as _};
use agent_provider_config::protocol::{
    Content, ErrorKind, Message, ModelBackend as _, ModelRequest, Recovery, Role, ToolChoice,
};
use agent_provider_config::{
    EndpointLocation, ModelDescriptor, ProviderConfig, ProviderProfile, ProviderProtocol,
    SecretReference,
};
use tilo_model_gateway::page_service::{MemoryKeys, PageService};

/// A provider on loopback answering each request with the next of `answers`.
fn fake_provider(answers: Vec<&str>) -> (String, Responder) {
    let served = Responder::start(
        answers
            .into_iter()
            .map(|answer| {
                Reply::status(200)
                    .header("content-type", "application/json")
                    .body(answer)
            })
            .collect(),
    );
    (format!("{}/v1", served.url()), served)
}

fn ask(text: &str) -> ModelRequest {
    ModelRequest {
        messages: vec![Message::text(Role::User, text)],
        tools: vec![],
        tool_choice: ToolChoice::None,
        max_output_tokens: 64,
    }
}

#[test]
fn a_window_asks_the_provider_the_person_set_up_with_its_key() {
    settings_store::install_source(Some(Arc::new(settings_store::HostDocument::new(
        serde_json::Map::new(),
        |_| Ok(()),
    ))));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let service = Arc::new(PageService::new(Arc::new(MemoryKeys::default())).unwrap());
    let models = service.models(None);

    // Nothing set up: the window hears how to set a provider up.
    let error = runtime.block_on(models.complete(&ask("Hi"))).unwrap_err();
    assert_eq!(error.kind, ErrorKind::NoProvider);
    assert_eq!(error.recovery(), Recovery::AddProvider);

    let (endpoint, served) = fake_provider(vec![
        r#"{"data":[{"id":"fake-model"}]}"#,
        r#"{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"Hello from the provider"}}],
            "usage":{"prompt_tokens":5,"completion_tokens":4,"total_tokens":9}}"#,
    ]);
    let id = "fake-provider".to_owned();
    let profile = ProviderProfile {
        id: id.clone(),
        label: "Fake provider".into(),
        protocol: ProviderProtocol::OpenAiCompatible,
        endpoint,
        location: EndpointLocation::ThisDevice,
        credential: Some(SecretReference::provider_api_key(&id)),
        models: vec![ModelDescriptor::new("fake-model")],
        default_model: Some("fake-model".into()),
        ..ProviderProfile::local_vllm()
    };
    let config = ProviderConfig {
        profiles: vec![profile],
        default_profile: Some(id.clone()),
        ..ProviderConfig::default()
    };
    // What Model Providers does: save the provider, then its key.
    runtime.block_on(async {
        let snapshot = service.get_config().await.unwrap();
        service.set_config(snapshot.revision, config).await.unwrap();
        assert_eq!(
            service.credential_status(id.clone()).await.unwrap(),
            CredentialStatus::Missing
        );
        service
            .set_api_key(id.clone(), "placeholder-test-key".into())
            .await
            .unwrap();
        assert_eq!(
            service.credential_status(id.clone()).await.unwrap(),
            CredentialStatus::Configured
        );
        let listed = service.list_models(id.clone()).await.unwrap();
        assert_eq!(listed[0].id, "fake-model");
        // A stale revision does not overwrite what was saved since.
        let stale = service
            .set_config("0".into(), ProviderConfig::default())
            .await
            .unwrap_err();
        assert_eq!(stale.kind, ErrorKind::Conflict);
    });

    let answer = runtime.block_on(models.complete(&ask("Hi"))).unwrap();
    assert_eq!(
        answer.message.content,
        vec![Content::Text {
            text: "Hello from the provider".into()
        }]
    );
    assert_eq!(answer.usage.total_tokens, 9);
    let seen = served.drain_requests();
    assert_eq!(seen[0].target, "/v1/models");
    assert_eq!(seen[1].target, "/v1/chat/completions");
    assert_eq!(
        seen[1].header("authorization"),
        Some("Bearer placeholder-test-key")
    );
    let body: serde_json::Value = serde_json::from_str(&seen[1].body_text()).unwrap();
    assert_eq!(body["model"], "fake-model");
    assert_eq!(
        body["messages"][0],
        serde_json::json!({"role": "user", "content": "Hi"})
    );
    settings_store::install_source(None);
}

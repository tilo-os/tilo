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
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use agent_provider_config::management::{CredentialStatus, ProviderAdmin as _};
use agent_provider_config::protocol::{
    Content, ErrorKind, Message, ModelBackend as _, ModelRequest, Recovery, Role, ToolChoice,
};
use agent_provider_config::{
    EndpointLocation, ModelDescriptor, ProviderConfig, ProviderProfile, ProviderProtocol,
    SecretReference,
};
use tilo_model_client::page_service::{MemoryKeys, PageService};

/// One request the fake provider received.
struct Seen {
    path: String,
    authorization: Option<String>,
    body: String,
}

/// A provider on loopback answering each request with the next of `answers`.
fn fake_provider(answers: Vec<&'static str>) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for answer in answers {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            let (mut length, mut authorization) = (0, None);
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                let (name, value) = header.split_once(':').unwrap();
                match name.to_ascii_lowercase().as_str() {
                    "content-length" => length = value.trim().parse().unwrap(),
                    "authorization" => authorization = Some(value.trim().to_owned()),
                    _ => {}
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            log.lock().unwrap().push(Seen {
                path,
                authorization,
                body: String::from_utf8(body).unwrap(),
            });
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                answer.len()
            )
            .unwrap();
        }
    });
    (endpoint, seen)
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

    let (endpoint, seen) = fake_provider(vec![
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
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    assert_eq!(seen[0].path, "/v1/models");
    assert_eq!(seen[1].path, "/v1/chat/completions");
    assert_eq!(
        seen[1].authorization.as_deref(),
        Some("Bearer placeholder-test-key")
    );
    let body: serde_json::Value = serde_json::from_str(&seen[1].body).unwrap();
    assert_eq!(body["model"], "fake-model");
    assert_eq!(
        body["messages"][0],
        serde_json::json!({"role": "user", "content": "Hi"})
    );
    settings_store::install_source(None);
}

//! Chat in a browser page: the page lends it its own model service
//! (`tilo_model_gateway::page_service`), as the web runtime does when its
//! host lends none, and asking the assistant reaches the provider the person
//! set up, with their key, and lands the answer in the channel. The
//! provider is a loopback fixture, never a real one, and the key a
//! placeholder.
#![allow(
    clippy::expect_used,
    reason = "test fixture helpers fail immediately on a broken step"
)]
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::rc::Rc;
use std::sync::Arc;

use agent_provider_config::{
    EndpointLocation, ModelDescriptor, ProviderConfig, ProviderProfile, ProviderProtocol,
    SecretReference,
};
use tilo_model_gateway::page_service::{MemoryKeys, PageService};
use ui::models::{Models, ProviderAdmin as _};

use crate::{ChatApp, identity::LocalIdentity};

/// A provider on loopback answering one chat completion; hands back the
/// request's authorization and body.
fn fake_provider(text: &'static str) -> (String, std::thread::JoinHandle<(String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
    let endpoint = format!(
        "http://{}/v1",
        listener.local_addr().expect("fake provider address")
    );
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("provider request");
        let mut reader = BufReader::new(stream.try_clone().expect("provider stream"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("request line");
        assert!(line.starts_with("POST /v1/chat/completions "), "{line}");
        let (mut length, mut authorization) = (0, String::new());
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).expect("request header");
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            let (name, value) = header.split_once(':').expect("header");
            match name.to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse().expect("length"),
                "authorization" => value.trim().clone_into(&mut authorization),
                _ => {}
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).expect("request body");
        let answer = serde_json::json!({
            "choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": text}}]
        })
        .to_string();
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
            answer.len()
        )
        .expect("answer");
        (authorization, String::from_utf8(body).expect("utf-8 body"))
    });
    (endpoint, server)
}

#[test]
fn chat_asks_the_provider_set_up_in_the_page_through_its_lent_service() {
    settings_store::install_source(Some(Arc::new(settings_store::HostDocument::new(
        serde_json::Map::new(),
        |_| Ok(()),
    ))));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("runtime");
    let _inside = runtime.enter();
    let (endpoint, provider) = fake_provider("Answer from the page's provider");

    // Model Providers, lent the same service, sets the provider and its key up.
    let service = Arc::new(PageService::new(Arc::new(MemoryKeys::default())).expect("service"));
    let id = "page-provider".to_owned();
    let config = ProviderConfig {
        profiles: vec![ProviderProfile {
            id: id.clone(),
            label: "Page provider".into(),
            protocol: ProviderProtocol::OpenAiCompatible,
            endpoint,
            location: EndpointLocation::ThisDevice,
            credential: Some(SecretReference::provider_api_key(&id)),
            models: vec![ModelDescriptor::new("page-model")],
            default_model: Some("page-model".into()),
            ..ProviderProfile::local_vllm()
        }],
        default_profile: Some(id.clone()),
        ..ProviderConfig::default()
    };
    runtime.block_on(async {
        let snapshot = service.get_config().await.expect("read providers");
        service
            .set_config(snapshot.revision, config)
            .await
            .expect("save provider");
        service
            .set_api_key(id, "placeholder-page-key".into())
            .await
            .expect("save key");
    });

    let identity = LocalIdentity::demo();
    let mut doc = crate::testing::Workspace::default();
    crate::testing::starter(&mut doc, &identity.id, &identity.name, &identity.initials);
    crate::testing::say(&mut doc, &identity.id, "c_chat", "What is on today?");
    let mut app = ChatApp::new(
        identity.clone(),
        crate::app::ChatRuntime::Unavailable,
        tilo_content::ContentStore::memory(),
    )
    .with_models(Models::new(Rc::new(service.models(None))));
    app.attach_framework(ui::framework::Dispatcher::detached(
        "so.tilo.chat",
        &identity.id,
    ));
    app.attach_workspace(&doc);
    app.reload(&doc);
    app.ensure_selection();
    app.reload(&doc);
    assert_eq!(
        app.model_command("c_chat", "ask").expect("ask")["started"],
        true
    );
    tilo_test_env::wait_until("the page's model to answer", || {
        if !app.host_model.needs_frame() {
            return true;
        }
        app.pump_model();
        app.land(&mut doc);
        app.reload(&doc);
        false
    });
    let (authorization, body) = provider.join().expect("provider");
    assert_eq!(authorization, "Bearer placeholder-page-key");
    let body: serde_json::Value = serde_json::from_str(&body).expect("json body");
    assert_eq!(body["model"], "page-model");
    assert_eq!(body["messages"][0]["content"], "What is on today?");
    let reply = crate::testing::messages(&doc, "c_chat")
        .last()
        .cloned()
        .expect("reply");
    assert_eq!(reply.text, "Answer from the page's provider");
    settings_store::install_source(None);
}

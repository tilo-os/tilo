//! Signing in to providers other than `OpenAI`, on the real path: the page's
//! actions on the server, the server's flow against a fake issuer, the grant
//! (or key) in the workspace's vault, and the token a call is asked with.
//!
//! The fake issuers are `test_http` responders, so no real account is
//! reached: the device codes, the pages' codes and the Copilot exchange are
//! all answered here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::future_not_send,
    reason = "the server and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the server it drives, and the fake responder is one connection's receiver, read on this test's own thread"
)]

use std::sync::Arc;

use account_credentials::{CredentialStore, OAuthIssuer, OAuthUse, Provider, Refresh, Revocation};
use agent_provider_config::presets::Preset;
use agent_provider_config::protocol::{Message, ModelRequest, Role, ToolChoice};
use agent_provider_config::{EndpointLocation, ProviderProfile};
use serde_json::json;
use test_http::{Reply, Request, Responder};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;
use tilo_workspace_records::models as model_providers;
use tilo_workspace_records::models::{SignInStarted, SignInState};

use crate::fixtures::{TestHost, admit, ask_own, hosted, hosting_device, test_host};
use crate::person_settings::{ProviderKeys, provider_config, save_provider_config};

/// A fake issuer's descriptor: its device endpoint where `device` says, the
/// token endpoint where every sign-in is traded, and `refresh` for how a
/// grant renews.
fn fake_issuer(base: &str, device: Option<&str>, refresh: Refresh) -> Provider {
    Provider {
        id: "fake".into(),
        label: "Fake".into(),
        authorization_endpoint: format!("{base}/authorize"),
        token_endpoint: format!("{base}/token"),
        revocation: Revocation::Unsupported,
        scopes: Vec::new(),
        authorization_params: Vec::new(),
        refresh,
        device_authorization_endpoint: device.map(|path| format!("{base}{path}")),
    }
}

/// A fake issuer: the device code, the tokens, the Copilot exchange, a
/// model list, an `OpenRouter` key and a chat turn.
struct Fake {
    responder: Responder,
    /// What the fake was asked, in order.
    seen: Arc<std::sync::Mutex<Vec<Request>>>,
}

impl Fake {
    /// A fake that hands out `access` and `refresh`, lists `model`, and
    /// answers a chat turn with `[Fake model] hello`.
    fn start(access: &'static str, refresh: &'static str, model: &'static str) -> Self {
        let seen: Arc<std::sync::Mutex<Vec<Request>>> = Arc::default();
        let recording = Arc::clone(&seen);
        let responder = Responder::handle(move |request: &Request| {
            recording.lock().unwrap().push(request.clone());
            let target = request.target.as_str();
            let reply = if request.method == "POST" && target.ends_with("/device/code") {
                Reply::status(200).body(
                    json!({
                        "device_code": "device-1",
                        "user_code": "ABCD-EFGH",
                        "verification_uri": "https://example.org/device",
                        "expires_in": 900,
                        "interval": 0,
                    })
                    .to_string(),
                )
            } else if request.method == "POST" && target.ends_with("/token") {
                Reply::status(200).body(
                    json!({
                        "access_token": access,
                        "token_type": "bearer",
                        "refresh_token": refresh,
                        "expires_in": 3600,
                    })
                    .to_string(),
                )
            } else if request.method == "POST" && target.ends_with("/keys") {
                Reply::status(200).body(json!({"key": "sk-or-fake"}).to_string())
            } else if request.method == "GET" && target.ends_with("/copilot") {
                Reply::status(200).body(
                    json!({
                        "token": "tid=fake-copilot",
                        "expires_at": tilo_clock::unix_now().saturating_add(3600),
                    })
                    .to_string(),
                )
            } else if request.method == "GET" && target.ends_with("/models") {
                Reply::status(200).body(
                    json!({"data": [{"id": model, "display_name": "Fake model"}]}).to_string(),
                )
            } else if request.method == "POST" && target.ends_with("/chat/completions") {
                Reply::status(200)
                    .header("content-type", "text/event-stream")
                    .body(format!(
                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                        json!({
                            "id": "chat-1",
                            "object": "chat.completion.chunk",
                            "choices": [{"index": 0, "delta": {"content": "[Fake model] hello"}}],
                        }),
                        json!({
                            "id": "chat-1",
                            "object": "chat.completion.chunk",
                            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 2},
                        }),
                    ))
            } else {
                Reply::status(404)
            };
            Some(reply)
        });
        Self { responder, seen }
    }

    fn url(&self) -> String {
        self.responder.url()
    }

    fn requests(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }
}

/// The workspace a fake issuer is set up in, its person, and the actions.
struct Hosted {
    actions: tilo_workspace_service::workspaces::actions::ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    ada: DeviceCert,
    root: std::path::PathBuf,
    workspaces: Arc<crate::PersonServices>,
    workspace: String,
    /// The host the test keeps alive: dropping it closes the workspace.
    _server: TestHost,
    /// And the directory it runs in.
    _dir: tempfile::TempDir,
}

async fn hosting(fake: &Fake, use_: OAuthUse, profile: ProviderProfile) -> Hosted {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = Arc::new(crate::PersonServices::of(server.workspaces()));
    workspaces.call_private_model_addresses(true);
    workspaces.sign_in_through(CredentialStore::for_root(None).with_use(
        use_,
        fake_issuer(&fake.url(), Some("/device/code"), Refresh::RefreshToken),
        "fake-client",
    ));
    let (workspace, workspace_id, actions) = hosted(&workspaces).await;
    let ada = admit(&workspaces, &workspace, &owner, 1).await;
    let root = workspaces.workspace_root(&workspace);
    let mut config = provider_config(&root);
    config.profiles.push(profile);
    save_provider_config(&root, &config).unwrap();
    Hosted {
        actions,
        workspace_id,
        ada,
        root,
        workspaces,
        workspace,
        _server: server,
        _dir: dir,
    }
}

/// A model request asking `text`.
fn asking(text: &str) -> serde_json::Value {
    serde_json::to_value(ModelRequest {
        messages: vec![Message::text(Role::User, text)],
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 64,
    })
    .unwrap()
}

/// Ask the page's action for `service`, and answer what beginning it said.
async fn begin(host: &Hosted, service: &str, project: &str, model: &str) -> SignInStarted {
    let started = ask_own(
        &host.actions,
        host.workspace_id,
        &host.ada,
        model_providers::BEGIN_SIGN_IN,
        json!({"service": service, "project": project, "model": model}),
    )
    .await
    .expect("the sign-in begins");
    serde_json::from_value(started).unwrap()
}

/// Wait on `sign_in` until it ended.
async fn wait(host: &Hosted, sign_in: &str) -> SignInState {
    for _ in 0..40 {
        let state = ask_own(
            &host.actions,
            host.workspace_id,
            &host.ada,
            model_providers::WAIT_SIGN_IN,
            json!({"signIn": sign_in}),
        )
        .await
        .expect("the sign-in is waited for");
        let state: SignInState = serde_json::from_value(state).unwrap();
        if state != SignInState::Waiting {
            return state;
        }
    }
    panic!("the sign-in never ended");
}

/// Play the browser: send the page's callback a code, keeping its `state`.
fn answer_page(page: &str, callback_parameter: &str) {
    let url = tilo_net::Url::parse(page).unwrap();
    let callback = url
        .query_pairs()
        .find(|(key, _)| key == callback_parameter)
        .unwrap_or_else(|| panic!("the page names no {callback_parameter}: {page}"))
        .1
        .into_owned();
    let callback = tilo_net::Url::parse(&callback).unwrap();
    let mut stream =
        std::net::TcpStream::connect((callback.host_str().unwrap(), callback.port().unwrap()))
            .unwrap();
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default();
    std::io::Write::write_all(
        &mut stream,
        format!(
            "GET {}?code=approved&state={state} HTTP/1.1\r\n\r\n",
            callback.path()
        )
        .as_bytes(),
    )
    .unwrap();
    let mut answer = String::new();
    let _ = std::io::Read::read_to_string(&mut stream, &mut answer);
}

/// A member signs in to GitHub Copilot by a code entered on GitHub's page;
/// the grant is kept, its models are listed, and a call is asked with the
/// Copilot token the grant was traded for.
#[tokio::test]
async fn a_copilot_sign_in_keeps_its_grant_and_answers_with_the_traded_token() {
    let fake = Fake::start("gho-github", "ghr-github", "gpt-copilot");
    let mut profile = Preset::COPILOT.profile();
    profile.endpoint = fake.url();
    profile.location = EndpointLocation::ThisDevice;
    let use_ = OAuthUse::new(OAuthIssuer::GitHub, account_credentials::Capability::Models);
    let host = hosting(&fake, use_, profile).await;
    // The issuer renews a GitHub grant by trading it for a Copilot token.
    host.workspaces
        .sign_in_through(CredentialStore::for_root(None).with_use(
            use_,
            fake_issuer(
                &fake.url(),
                Some("/device/code"),
                Refresh::CopilotToken {
                    endpoint: format!("{}/copilot", fake.url()),
                },
            ),
            "fake-client",
        ));

    let started = begin(&host, "github-copilot", "", "").await;
    assert_eq!(started.code, "ABCD-EFGH");
    assert_eq!(started.page, "https://example.org/device");
    let state = wait(&host, &started.sign_in).await;
    let SignInState::Done { provider } = state else {
        panic!("the sign-in did not finish: {state:?}");
    };
    assert!(provider.signs_in && provider.configured, "{provider:?}");
    assert_eq!(provider.endpoint, fake.url());
    assert_eq!(
        provider.models.first().map(|model| model.id.as_str()),
        Some("gpt-copilot")
    );

    // The vault keeps the grant: what a page reads carries no token.
    let listed = ask_own(
        &host.actions,
        host.workspace_id,
        &host.ada,
        model_providers::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert!(!listed.to_string().contains("gho-github"), "{listed}");
    assert!(listed["providers"][0]["configured"].as_bool().unwrap());

    // The call is asked with the Copilot token the GitHub grant was traded for.
    let asked = ask_own(
        &host.actions,
        host.workspace_id,
        &host.ada,
        model_providers::ASK,
        asking("Hello"),
    )
    .await
    .unwrap();
    assert!(asked.to_string().contains("Fake model"), "{asked}");
    let calls = fake.requests();
    assert!(
        calls
            .iter()
            .any(|request| request.header("authorization") == Some("Bearer tid=fake-copilot")),
        "the call carried the traded token: {:?}",
        calls
            .iter()
            .map(|request| request.target.clone())
            .collect::<Vec<_>>()
    );
}

/// A member signs in with Hugging Face: the token becomes the key the model
/// this computer runs downloads with, and the local provider is what keeps it.
#[tokio::test]
async fn a_hugging_face_sign_in_becomes_the_key_its_downloads_use() {
    let fake = Fake::start("hf-access", "hf-refresh", "hf-none");
    let use_ = OAuthUse::new(
        OAuthIssuer::HuggingFace,
        account_credentials::Capability::ModelDownloads,
    );
    let host = hosting(&fake, use_, ProviderProfile::local_vllm()).await;

    let started = begin(&host, "hugging-face", "", "").await;
    assert!(!started.code.is_empty(), "a code is entered");
    let state = wait(&host, &started.sign_in).await;
    let SignInState::Done { provider } = state else {
        panic!("the sign-in did not finish: {state:?}");
    };
    assert!(provider.configured, "{provider:?}");
    let local = provider_config(&host.root)
        .profiles
        .into_iter()
        .find(|profile| profile.location == EndpointLocation::ThisDevice)
        .unwrap();
    let keys = ProviderKeys::of(host.workspaces.vault_at(&host.workspace), "");
    assert_eq!(
        keys.get(&local).await.unwrap().map(|key| key.to_string()),
        Some("hf-access".to_owned()),
        "the token is the local provider's key"
    );
}

/// A member signs in to Vertex AI on Google's page: the browser comes back to
/// the loopback, and the grant is kept and named by the provider.
#[tokio::test]
async fn a_google_sign_in_on_its_page_keeps_the_grant() {
    let fake = Fake::start("ya29-access", "google-refresh", "gemini-4-argon");
    let use_ = OAuthUse::new(OAuthIssuer::Google, account_credentials::Capability::Models);
    let host = hosting(
        &fake,
        use_,
        ProviderProfile::google_vertex("test-project", "global", "gemini-4-argon"),
    )
    .await;

    let started = begin(&host, "google-vertex", "test-project", "").await;
    assert!(started.code.is_empty(), "a page asks for no code");
    answer_page(&started.page, "redirect_uri");
    let state = wait(&host, &started.sign_in).await;
    let SignInState::Done { provider } = state else {
        panic!("the sign-in did not finish: {state:?}");
    };
    assert!(provider.signs_in && provider.configured, "{provider:?}");
    assert!(provider.endpoint.contains("test-project"), "{provider:?}");
    let listed = ask_own(
        &host.actions,
        host.workspace_id,
        &host.ada,
        model_providers::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert!(!listed.to_string().contains("ya29-access"), "{listed}");
}

/// A member signs in to `OpenRouter`: its page hands back a key, which is
/// kept as the provider's key.
#[tokio::test]
async fn an_openrouter_sign_in_keeps_the_key_it_was_issued() {
    let fake = Fake::start("unused", "", "openrouter-model");
    let host = hosting(
        &fake,
        OAuthUse::new(OAuthIssuer::GitHub, account_credentials::Capability::Models),
        Preset::OPENROUTER.profile(),
    )
    .await;
    host.workspaces.sign_in_to_openrouter_at(
        "https://example.org/auth".into(),
        format!("{}/keys", fake.url()),
    );

    let started = begin(&host, "openrouter", "", "").await;
    answer_page(&started.page, "callback_url");
    let state = wait(&host, &started.sign_in).await;
    let SignInState::Done { provider } = state else {
        panic!("the sign-in did not finish: {state:?}");
    };
    assert!(provider.configured, "{provider:?}");
    assert!(!provider.signs_in, "a key is not a grant: {provider:?}");
    assert!(
        fake.requests()
            .iter()
            .any(|request| request.target.ends_with("/keys")),
        "the key was traded for"
    );
}

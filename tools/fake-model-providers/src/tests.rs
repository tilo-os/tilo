//! The fake's own tests, exercising it through Tilo's real wire code: the sign-in a server
//! makes, and one streamed answer per API parsed back into a turn.

use base64::Engine as _;
use serde_json::{Value, json};

use agent_provider_config::presets::Preset;
use agent_provider_config::protocol::{
    Content, Message, ModelRequest, ModelResponse, Role, StopReason, ToolCall, ToolChoice,
    ToolDefinition, ToolResult,
};
use agent_provider_config::{ModelDescriptor, OpenAiApi, ProviderProfile, transform, wire};
use sha2::Digest as _;

use crate::FakeProviders;

/// A request one test sends, answered with the body the fake returns.
async fn call(
    fake: &FakeProviders,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: Option<(&'static str, String)>,
) -> (u16, String) {
    let stream = tokio::net::TcpStream::connect(fake.addr())
        .await
        .expect("the fake accepts a connection");
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .expect("an HTTP/1.1 handshake");
    tokio::spawn(connection);
    let mut builder = hyper::Request::builder().method(method).uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, value.as_str());
    }
    let payload = match body {
        Some((content_type, text)) => {
            builder = builder.header("content-type", content_type);
            http_body_util::Full::new(bytes::Bytes::from(text))
        }
        None => http_body_util::Full::new(bytes::Bytes::new()),
    };
    let request = builder.body(payload).expect("a request");
    let response = sender.send_request(request).await.expect("an answer");
    let status = response.status().as_u16();
    let body = http_body_util::BodyExt::collect(response.into_body())
        .await
        .expect("an answer body")
        .to_bytes();
    (status, String::from_utf8(body.to_vec()).expect("UTF-8"))
}

async fn json_post(fake: &FakeProviders, path: &str, body: &Value) -> (u16, Value) {
    let (status, text) = call(
        fake,
        "POST",
        path,
        &[("accept", "application/json".into())],
        Some(("application/json", body.to_string())),
    )
    .await;
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn form_post(fake: &FakeProviders, path: &str, fields: &[(&str, &str)]) -> (u16, Value) {
    let body = fields
        .iter()
        .map(|(key, value)| format!("{key}={}", encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    let (status, text) = call(
        fake,
        "POST",
        path,
        &[("content-type", "application/x-www-form-urlencoded".into())],
        Some(("application/x-www-form-urlencoded", body)),
    )
    .await;
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn encode(text: &str) -> String {
    percent_encoding::utf8_percent_encode(text, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn claims(token: &str) -> Value {
    let payload = token.split('.').nth(1).expect("a JWT with a payload");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .expect("base64url claims");
    serde_json::from_slice(&bytes).expect("JSON claims")
}

/// One signed-in account, as the device flow grants it.
struct SignedIn {
    access: String,
    id: String,
    refresh: String,
}

/// Go through the whole sign-in a server makes: code, polling, approval, exchange.
async fn sign_in(fake: &FakeProviders) -> SignedIn {
    let (status, code) = json_post(
        fake,
        "/auth/api/accounts/deviceauth/usercode",
        &json!({"client_id": "app_fake"}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(code["interval"], "1", "the interval is a string");
    let user_code = code["user_code"].as_str().expect("a code").to_owned();
    let polling = json!({
        "device_auth_id": code["device_auth_id"],
        "user_code": user_code,
    });
    let (status, pending) = json_post(fake, "/auth/api/accounts/deviceauth/token", &polling).await;
    assert_eq!(status, 403, "{pending}");
    assert!(fake.pending_codes().contains(&user_code));
    assert!(fake.approve(&user_code));
    let (status, granted) = json_post(fake, "/auth/api/accounts/deviceauth/token", &polling).await;
    assert_eq!(status, 200);
    let verifier = granted["code_verifier"].as_str().expect("a verifier");
    let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()));
    assert_eq!(granted["code_challenge"], expected, "PKCE S256");
    let redirect = format!("{}/deviceauth/callback", fake.issuer_url());
    let (status, tokens) = form_post(
        fake,
        "/auth/oauth/token",
        &[
            ("grant_type", "authorization_code"),
            (
                "code",
                granted["authorization_code"].as_str().expect("a code"),
            ),
            ("redirect_uri", redirect.as_str()),
            ("client_id", "app_fake"),
            ("code_verifier", verifier),
        ],
    )
    .await;
    assert_eq!(status, 200, "{tokens}");
    SignedIn {
        access: tokens["access_token"]
            .as_str()
            .expect("an access token")
            .to_owned(),
        id: tokens["id_token"].as_str().expect("an id token").to_owned(),
        refresh: tokens["refresh_token"]
            .as_str()
            .expect("a refresh token")
            .to_owned(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn the_device_sign_in_grants_tokens_and_rotates_its_refresh() {
    let fake = FakeProviders::start().await.expect("the fake starts");
    let fake = &fake;
    let signed = sign_in(fake).await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock")
        .as_secs();

    let access = claims(&signed.access);
    assert_eq!(access["email"], crate::EMAIL);
    assert_eq!(
        access["https://api.openai.com/auth"]["chatgpt_account_id"],
        crate::ACCOUNT_ID
    );
    assert_eq!(
        access["https://api.openai.com/auth"]["chatgpt_plan_type"],
        "plus"
    );
    let expires = access["exp"].as_u64().expect("an expiry");
    assert!(expires >= now + 3000 && expires <= now + 3700, "{expires}");
    assert_eq!(claims(&signed.id)["email"], crate::EMAIL);

    let refresh = |token: String| {
        let body = json!({
            "client_id": "app_fake",
            "grant_type": "refresh_token",
            "refresh_token": token,
            "scope": "openid profile email",
        });
        async move { json_post(fake, "/auth/oauth/token", &body).await }
    };
    let (status, renewed) = refresh(signed.refresh.clone()).await;
    assert_eq!(status, 200, "{renewed}");
    assert_ne!(
        renewed["refresh_token"], signed.refresh,
        "the refresh rotated"
    );
    assert_ne!(renewed["access_token"], signed.access, "a new access token");
    assert_eq!(fake.refresh_count(), 1);

    let (status, refused) = refresh(signed.refresh).await;
    assert_eq!(status, 401);
    assert_eq!(refused["error"], "refresh_token_reused");
    assert_eq!(fake.refresh_count(), 1, "a refused reuse is no refresh");

    let exchange = |subject: &str| {
        let subject = subject.to_owned();
        async move {
            form_post(
                fake,
                "/auth/oauth/token",
                &[
                    (
                        "grant_type",
                        "urn:ietf:params:oauth:grant-type:token-exchange",
                    ),
                    ("client_id", "app_fake"),
                    ("requested_token", "openai-api-key"),
                    ("subject_token", subject.as_str()),
                    (
                        "subject_token_type",
                        "urn:ietf:params:oauth:token-type:id_token",
                    ),
                ],
            )
            .await
        }
    };
    let (status, key) = exchange(&signed.id).await;
    assert_eq!(status, 200, "{key}");
    assert!(
        key["access_token"]
            .as_str()
            .is_some_and(|key| key.starts_with(crate::API_KEY_PREFIX))
    );
    fake.set_api_key_exchange(false);
    let (status, refused) = exchange(&signed.id).await;
    assert_eq!(status, 401, "{refused}");
}

#[tokio::test(flavor = "current_thread")]
async fn an_expired_or_unknown_access_token_is_refused_by_the_backend() {
    let fake = FakeProviders::start().await.expect("the fake starts");
    let fake = &fake;
    fake.set_token_lifetime(0);
    let signed = sign_in(fake).await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock")
        .as_secs();
    assert!(claims(&signed.access)["exp"].as_u64().expect("an expiry") <= now);

    let body = json!({
        "model": "gpt-5.5",
        "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
    })
    .to_string();
    let profile = chatgpt(fake);
    let made = transform::profile_headers(&profile, Some(&signed.access)).expect("headers");
    let headers: Vec<(&str, String)> = made
        .iter()
        .map(|header| (&*header.name, header.value.to_string()))
        .collect();
    let (status, text) = call(
        fake,
        "POST",
        "/backend-api/codex/responses",
        &headers,
        Some(("application/json", body.clone())),
    )
    .await;
    assert_eq!(status, 401, "an expired token is refused: {text}");

    let (status, _) = call(
        fake,
        "POST",
        "/backend-api/codex/responses",
        &[
            ("authorization", "Bearer unknown".into()),
            ("chatgpt-account-id", crate::ACCOUNT_ID.into()),
        ],
        Some(("application/json", body)),
    )
    .await;
    assert_eq!(status, 401, "an unknown token is refused");
}

#[tokio::test(flavor = "current_thread")]
async fn the_page_approves_only_a_code_it_issued() {
    let fake = FakeProviders::start().await.expect("the fake starts");
    let fake = &fake;
    let (status, code) = json_post(
        fake,
        "/auth/api/accounts/deviceauth/usercode",
        &json!({"client_id": "app_fake"}),
    )
    .await;
    assert_eq!(status, 200);
    let user_code = code["user_code"].as_str().expect("a code").to_owned();

    let (status, page) = call(fake, "GET", "/auth/codex/device", &[], None).await;
    assert_eq!(status, 200);
    assert!(page.contains("Continue"), "{page}");

    let submit = |typed: String| async move {
        let body = format!("code={}", encode(&typed));
        call(
            fake,
            "POST",
            "/auth/codex/device",
            &[("content-type", "application/x-www-form-urlencoded".into())],
            Some(("application/x-www-form-urlencoded", body)),
        )
        .await
    };
    let polling = json!({
        "device_auth_id": code["device_auth_id"],
        "user_code": user_code,
    });
    let (status, wrong) = submit("ZZZZ-ZZZZ".to_owned()).await;
    assert_eq!(status, 400, "{wrong}");
    assert!(wrong.contains("not valid"), "{wrong}");
    let (status, _) = json_post(fake, "/auth/api/accounts/deviceauth/token", &polling).await;
    assert_eq!(status, 403, "a wrong code approves nothing");

    let (status, approved) = submit(user_code).await;
    assert_eq!(status, 200, "{approved}");
    assert!(approved.contains("signed in"), "{approved}");
    let (status, _) = json_post(fake, "/auth/api/accounts/deviceauth/token", &polling).await;
    assert_eq!(status, 200, "the typed code approves its sign-in");
}

#[tokio::test(flavor = "current_thread")]
async fn model_lists_parse_on_every_provider() {
    let fake = FakeProviders::start().await.expect("the fake starts");
    let fake = &fake;
    for (path, key_header, label) in [
        (
            "/backend-api/codex/models?client_version=1.0.0",
            "authorization",
            "ChatGPT",
        ),
        ("/openai/v1/models", "authorization", "OpenAI"),
        ("/anthropic/v1/models", "x-api-key", "Anthropic"),
    ] {
        let (status, body) = call(fake, "GET", path, &[(key_header, "any".into())], None).await;
        assert_eq!(status, 200, "{label}");
        let models = transform::parse_models(&body).expect(label);
        assert!(!models.is_empty(), "{label}");
    }
}

fn chatgpt(fake: &FakeProviders) -> ProviderProfile {
    let mut profile = Preset::CHATGPT.profile();
    profile.endpoint = fake.chatgpt_backend_url();
    profile.models = vec![ModelDescriptor::new("gpt-5.5")];
    profile
}

fn openai(fake: &FakeProviders, api: OpenAiApi) -> ProviderProfile {
    let mut profile = Preset::OPENAI.profile();
    profile.endpoint = fake.openai_api_url();
    profile.openai_api = Some(api);
    profile.models = vec![ModelDescriptor::new("gpt-5.5")];
    profile
}

fn anthropic(fake: &FakeProviders) -> ProviderProfile {
    let mut profile = Preset::ANTHROPIC.profile();
    profile.endpoint = fake.anthropic_api_url();
    profile.models = vec![ModelDescriptor::new("claude-sonnet-5-5")];
    profile
}

fn echo() -> ToolDefinition {
    ToolDefinition {
        name: "echo".into(),
        description: "say it back".into(),
        input_schema: json!({"type": "object", "properties": {"say": {"type": "string"}}}),
    }
}

fn request(text: &str, tools: Vec<ToolDefinition>) -> ModelRequest {
    ModelRequest {
        messages: vec![
            Message::text(Role::System, "You are a test."),
            Message::text(Role::User, text),
        ],
        tools,
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 64,
    }
}

/// One turn as Tilo's own wire code reads it back from the fake.
async fn ask(
    fake: &FakeProviders,
    profile: &ProviderProfile,
    model: &str,
    secret: &str,
    request: &ModelRequest,
) -> ModelResponse {
    let body = wire::body(
        profile,
        model,
        request,
        wire::Ask {
            streaming: true,
            extra: None,
        },
    )
    .expect("a request body");
    let url = wire::url(profile);
    let path = url.strip_prefix(&fake.base_url()).unwrap_or(&url);
    let made = transform::profile_headers(profile, Some(secret)).expect("headers");
    let headers: Vec<(&str, String)> = made
        .iter()
        .map(|header| (&*header.name, header.value.to_string()))
        .collect();
    let (status, text) = call(
        fake,
        "POST",
        path,
        &headers,
        Some(("application/json", body.to_string())),
    )
    .await;
    assert_eq!(status, 200, "{path}: {text}");
    let mut stream = wire::Stream::of(profile);
    let mut deltas = Vec::new();
    stream.push(text.as_bytes(), &mut deltas).expect("a stream");
    stream
        .finish(request, &mut deltas)
        .expect("a finished turn")
}

fn text_of(response: &ModelResponse) -> String {
    response
        .message
        .content
        .iter()
        .filter_map(|content| match content {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "every provider's text, call and summary in one loop"
)]
async fn every_wire_answers_a_streamed_text_a_call_and_a_summary() {
    let fake = FakeProviders::start().await.expect("the fake starts");
    let fake = &fake;
    let signed = sign_in(fake).await;
    let (status, key) = form_post(
        fake,
        "/auth/oauth/token",
        &[
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:token-exchange",
            ),
            ("client_id", "app_fake"),
            ("requested_token", "openai-api-key"),
            ("subject_token", signed.id.as_str()),
            (
                "subject_token_type",
                "urn:ietf:params:oauth:token-type:id_token",
            ),
        ],
    )
    .await;
    assert_eq!(status, 200);
    let key = key["access_token"].as_str().expect("an API key").to_owned();

    for (provider, profile, model, secret) in [
        ("ChatGPT", chatgpt(fake), "gpt-5.5", signed.access.clone()),
        (
            "OpenAI",
            openai(fake, OpenAiApi::Responses),
            "gpt-5.5",
            key.clone(),
        ),
        (
            "OpenAI",
            openai(fake, OpenAiApi::ChatCompletions),
            "gpt-5.5",
            key.clone(),
        ),
        (
            "Anthropic",
            anthropic(fake),
            "claude-sonnet-5-5",
            "sk-ant-fake".to_owned(),
        ),
    ] {
        let plain = ask(
            fake,
            &profile,
            model,
            &secret,
            &request("hello", Vec::new()),
        )
        .await;
        assert_eq!(plain.stop_reason, StopReason::EndTurn, "{provider}");
        assert_eq!(
            text_of(&plain),
            format!("[{provider} {model}] You said: hello")
        );
        assert!(plain.usage.input_tokens > 0, "{provider}: input usage");
        assert!(plain.usage.output_tokens > 0, "{provider}: output usage");

        let calling = ask(
            fake,
            &profile,
            model,
            &secret,
            &request("make it happen\ndo echo {\"say\":\"hi\"}", vec![echo()]),
        )
        .await;
        assert_eq!(calling.stop_reason, StopReason::ToolCalls, "{provider}");
        let calls = calling.tool_calls().expect("a call");
        assert_eq!(calls.len(), 1, "{provider}");
        assert_eq!(calls[0].name, "echo");
        assert_eq!(calls[0].input, json!({"say": "hi"}));
        assert!(calling.usage.output_tokens > 0, "{provider}: call usage");

        let mut messages = request("make it happen", Vec::new()).messages;
        messages.push(Message {
            role: Role::Assistant,
            content: vec![Content::ToolCall(ToolCall {
                id: "call-1".into(),
                name: "echo".into(),
                input: json!({"say": "hi"}),
                extra_content: None,
            })],
        });
        messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: "call-1".into(),
                content: "the answer".into(),
                is_error: false,
            })],
        });
        let done = ask(
            fake,
            &profile,
            model,
            &secret,
            &ModelRequest {
                messages,
                ..request("", Vec::new())
            },
        )
        .await;
        assert_eq!(done.stop_reason, StopReason::EndTurn, "{provider}");
        assert_eq!(
            text_of(&done),
            format!("[{provider} {model}] Done: the answer")
        );
    }
    assert!(
        fake.requests()
            .iter()
            .any(|request| request.path.ends_with("/chat/completions")),
        "the chat wire was exercised"
    );
}

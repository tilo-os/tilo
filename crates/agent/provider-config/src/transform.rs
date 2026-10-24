//! One single-shot model call in either provider dialect: the request body,
//! the endpoint URL, the authentication headers and the answer text; and
//! the provider's model list.
//!
//! These are pure pieces with no HTTP client, so every Tilo process that asks
//! a provider for one answer (the per-user model gateway, a host summarising
//! activity) speaks the same dialects through the same code instead of each
//! carrying its own vendor-specific request shapes.

use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::{ModelDescriptor, OpenAiApi, ProviderProfile, ProviderProtocol, SignInIssuer};

/// The Anthropic Messages API version every request names.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// How the model samples its answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sampling {
    /// `temperature: 0`: the same input gives the same answer wherever the
    /// model allows it. The gateway's transforms want this.
    #[default]
    Deterministic,
    /// No `temperature` at all and, on Anthropic, extended thinking turned
    /// off. Some current models (Claude Haiku 5.5 among them) refuse any
    /// non-default temperature with a 400, and they think adaptively by
    /// default, which would spend a small `max_tokens` before any text is
    /// written. A short, bounded answer from such a model needs both.
    ProviderDefaultNoThinking,
}

/// One single-shot request: a system instruction and one user turn, no tools.
#[derive(Clone, Copy, Debug)]
pub struct Ask<'a> {
    pub model: &'a str,
    pub instructions: &'a str,
    pub input: &'a str,
    pub max_tokens: u32,
    pub sampling: Sampling,
}

/// The provider request body for `ask`. The OpenAI-compatible dialect asks
/// for a JSON object, which every caller of a single-shot transform expects.
pub fn body(protocol: ProviderProtocol, ask: &Ask<'_>) -> Value {
    let mut body = match protocol {
        ProviderProtocol::Anthropic => json!({
            "model": ask.model,
            "max_tokens": ask.max_tokens,
            "system": ask.instructions,
            "messages": [{ "role": "user", "content": ask.input }]
        }),
        ProviderProtocol::OpenAiCompatible => json!({
            "model": ask.model,
            "max_tokens": ask.max_tokens,
            "response_format": { "type": "json_object" },
            "messages": [
                { "role": "system", "content": ask.instructions },
                { "role": "user", "content": ask.input }
            ]
        }),
    };
    if let Some(object) = body.as_object_mut() {
        match ask.sampling {
            Sampling::Deterministic => {
                object.insert("temperature".into(), Value::from(0));
            }
            Sampling::ProviderDefaultNoThinking => {
                if protocol == ProviderProtocol::Anthropic {
                    object.insert("thinking".into(), json!({ "type": "disabled" }));
                }
            }
        }
    }
    body
}

/// The answer text out of a provider response no longer than `max_bytes`.
/// A refusal or a cut-off answer is an error rather than text: the caller
/// would otherwise validate half an answer.
pub fn answer(protocol: ProviderProtocol, body: &str, max_bytes: usize) -> Result<String, String> {
    let value: Value =
        serde_json::from_str(body).map_err(|_| "provider response was unreadable".to_string())?;
    let text = match protocol {
        ProviderProtocol::Anthropic => {
            match value.get("stop_reason").and_then(Value::as_str) {
                Some("refusal") => return Err("the model declined the transform".into()),
                Some("max_tokens") => return Err("the model's answer was cut off".into()),
                _ => {}
            }
            value
                .get("content")
                .and_then(Value::as_array)
                .and_then(|blocks| {
                    blocks
                        .iter()
                        .find(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                })
                .and_then(|block| block.get("text"))
                .and_then(Value::as_str)
        }
        ProviderProtocol::OpenAiCompatible => {
            let choice = value
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first());
            if choice
                .and_then(|choice| choice.get("finish_reason"))
                .and_then(Value::as_str)
                == Some("length")
            {
                return Err("the model's answer was cut off".into());
            }
            choice
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("content"))
                .and_then(Value::as_str)
        }
    }
    .ok_or_else(|| "provider response carried no answer text".to_string())?;
    if text.len() > max_bytes {
        return Err("the model's answer exceeds its size limit".into());
    }
    Ok(text.to_string())
}

/// The request URL for a provider base `endpoint`, which may already name
/// the dialect's path.
pub fn url(protocol: ProviderProtocol, endpoint: &str) -> String {
    let base = endpoint.trim_end_matches('/');
    match protocol {
        ProviderProtocol::Anthropic if base.ends_with("/v1/messages") => base.into(),
        ProviderProtocol::Anthropic => format!("{base}/v1/messages"),
        ProviderProtocol::OpenAiCompatible if base.ends_with("/chat/completions") => base.into(),
        ProviderProtocol::OpenAiCompatible => format!("{base}/chat/completions"),
    }
}

/// One request header. `secret` marks a value carrying the credential, which
/// HTTP clients should keep out of debug output.
pub struct Header {
    pub name: &'static str,
    pub value: Zeroizing<String>,
    pub secret: bool,
}

/// The headers that version and authenticate a request. Anthropic always
/// needs a key; an OpenAI-compatible endpoint (a local server, say) may not.
///
/// # Errors
///
/// Anthropic without a `secret`.
pub fn headers(protocol: ProviderProtocol, secret: Option<&str>) -> Result<Vec<Header>, String> {
    match (protocol, secret) {
        (ProviderProtocol::Anthropic, None) => {
            Err("Anthropic requests require a configured credential".into())
        }
        (ProviderProtocol::Anthropic, Some(secret)) => Ok(vec![
            Header {
                name: "anthropic-version",
                value: Zeroizing::new(ANTHROPIC_VERSION.into()),
                secret: false,
            },
            Header {
                name: "x-api-key",
                value: Zeroizing::new(secret.into()),
                secret: true,
            },
        ]),
        (ProviderProtocol::OpenAiCompatible, Some(secret)) => Ok(vec![Header {
            name: "authorization",
            value: Zeroizing::new(format!("Bearer {secret}")),
            secret: true,
        }]),
        (ProviderProtocol::OpenAiCompatible, None) => Ok(Vec::new()),
    }
}

/// Whether `profile` speaks the `OpenAI` Responses format rather than its
/// protocol's: the `ChatGPT` backend a `ChatGPT` sign-in reaches serves only
/// that. Decided by the sign-in, which also decides the endpoint, so a
/// reader that predates it sees an ordinary profile it cannot use rather
/// than a protocol it cannot read.
pub fn speaks_responses(profile: &ProviderProfile) -> bool {
    profile.sign_in_issuer() == Some(SignInIssuer::ChatGpt)
}

/// The `OpenAI` API an OpenAI-compatible `profile`'s agent turns take;
/// `None` for an Anthropic one. A `ChatGPT` sign-in takes Responses, its
/// backend's only API; otherwise the profile's own choice, or Responses at
/// `OpenAI`'s own API and chat completions at any other endpoint. Single-shot
/// asks ([`profile_body`]) keep the dialect of [`speaks_responses`].
pub fn openai_api(profile: &ProviderProfile) -> Option<OpenAiApi> {
    if profile.protocol != ProviderProtocol::OpenAiCompatible {
        return None;
    }
    if speaks_responses(profile) {
        return Some(OpenAiApi::Responses);
    }
    Some(profile.openai_api.unwrap_or_else(|| {
        if is_openai_endpoint(&profile.endpoint) {
            OpenAiApi::Responses
        } else {
            OpenAiApi::ChatCompletions
        }
    }))
}

/// Whether `endpoint` is `OpenAI`'s own API.
fn is_openai_endpoint(endpoint: &str) -> bool {
    endpoint
        .trim()
        .strip_prefix("https://")
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .is_some_and(|host| {
            let host = host.strip_suffix(":443").unwrap_or(host);
            host.eq_ignore_ascii_case("api.openai.com")
        })
}

/// The Responses route of `profile`'s endpoint, which may already name it
/// or name chat completions.
pub fn responses_url(profile: &ProviderProfile) -> String {
    let base = profile.endpoint.trim_end_matches('/');
    let base = base.strip_suffix("/chat/completions").unwrap_or(base);
    if base.ends_with("/responses") {
        base.into()
    } else {
        format!("{base}/responses")
    }
}

/// The request URL for `profile`: [`url()`], or its Responses route.
pub fn profile_url(profile: &ProviderProfile) -> String {
    if speaks_responses(profile) {
        responses_url(profile)
    } else {
        url(profile.protocol, &profile.endpoint)
    }
}

/// The request body of `ask` for `profile`: [`body`], or a Responses
/// request. That one always streams and is never stored, as the `ChatGPT`
/// backend requires, and names neither a temperature nor an output limit,
/// which its reasoning models refuse; [`profile_answer`] reads the stream
/// and still bounds the answer.
pub fn profile_body(profile: &ProviderProfile, ask: &Ask<'_>) -> Value {
    if !speaks_responses(profile) {
        return body(profile.protocol, ask);
    }
    json!({
        "model": ask.model,
        "instructions": ask.instructions,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": ask.input }]
        }],
        "text": { "format": { "type": "json_object" } },
        "stream": true,
        "store": false
    })
}

/// The answer text of a response to [`profile_body`]; see [`answer`].
///
/// # Errors
///
/// As [`answer`].
pub fn profile_answer(
    profile: &ProviderProfile,
    body: &str,
    max_bytes: usize,
) -> Result<String, String> {
    if !speaks_responses(profile) {
        return answer(profile.protocol, body, max_bytes);
    }
    let response = responses_result(body)?;
    match response.get("status").and_then(Value::as_str) {
        Some("incomplete") => return Err("the model's answer was cut off".into()),
        Some("failed" | "cancelled") => return Err("the model's answer failed".into()),
        _ => {}
    }
    let mut text = String::new();
    for item in response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
    {
        for part in item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match part.get("type").and_then(Value::as_str) {
                Some("refusal") => return Err("the model declined the transform".into()),
                Some("output_text") => {
                    text.push_str(part.get("text").and_then(Value::as_str).unwrap_or_default());
                }
                _ => {}
            }
        }
    }
    if text.is_empty() {
        return Err("provider response carried no answer text".into());
    }
    if text.len() > max_bytes {
        return Err("the model's answer exceeds its size limit".into());
    }
    Ok(text)
}

/// The final response object of a Responses reply: the `response` of its
/// terminal stream event, or the body itself when it did not stream.
fn responses_result(body: &str) -> Result<Value, String> {
    let unreadable = || "provider response was unreadable".to_string();
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        return Ok(value);
    }
    let mut last = None;
    for line in body.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(event) = serde_json::from_str::<Value>(data.trim()) else {
            continue;
        };
        if matches!(
            event.get("type").and_then(Value::as_str),
            Some("response.completed" | "response.incomplete" | "response.failed")
        ) {
            last = event.get("response").cloned();
        }
    }
    last.ok_or_else(unreadable)
}

/// The URL listing `profile`'s models: `GET /v1/models` of an Anthropic
/// endpoint, `GET /models` of the others.
pub fn models_url(profile: &ProviderProfile) -> String {
    let base = profile.endpoint.trim_end_matches('/');
    let base = base
        .strip_suffix("/chat/completions")
        .or_else(|| base.strip_suffix("/v1/messages"))
        .or_else(|| base.strip_suffix("/responses"))
        .unwrap_or(base);
    match profile.protocol {
        ProviderProtocol::Anthropic if base.ends_with("/v1") => format!("{base}/models"),
        ProviderProtocol::Anthropic => format!("{base}/v1/models"),
        // The ChatGPT backend lists the models of the client version it is
        // told; any current version lists the current models.
        ProviderProtocol::OpenAiCompatible if speaks_responses(profile) => {
            format!("{base}/models?client_version={CHATGPT_CLIENT_VERSION}")
        }
        ProviderProtocol::OpenAiCompatible => format!("{base}/models"),
    }
}

/// At most this many models are read from one list.
const MAX_LISTED_MODELS: usize = 512;

/// The models of a model-list response: `data[].id` (`OpenAI`, Anthropic
/// and every compatible provider) or `models[].slug` (the `ChatGPT` backend),
/// with the provider's display name and context size when it gives them.
/// Sorted by id; duplicates and blank ids dropped.
pub fn parse_models(body: &str) -> Result<Vec<ModelDescriptor>, String> {
    let value: Value =
        serde_json::from_str(body).map_err(|_| "provider model list was unreadable".to_string())?;
    let (entries, id_key) = if let Some(data) = value.get("data").and_then(Value::as_array) {
        (data, "id")
    } else if let Some(models) = value.get("models").and_then(Value::as_array) {
        (models, "slug")
    } else {
        return Err("provider answered with no model list".into());
    };
    let text = |entry: &Value, keys: &[&str]| {
        keys.iter()
            .find_map(|key| entry.get(*key).and_then(Value::as_str))
            .map(str::trim)
            .filter(|text| {
                !text.is_empty() && text.len() <= 256 && !text.contains(char::is_control)
            })
            .map(str::to_string)
    };
    let mut models: Vec<ModelDescriptor> = entries
        .iter()
        .filter_map(|entry| {
            let id = text(entry, &[id_key, "id"])?;
            let context = ["context_length", "context_window", "max_context_length"]
                .iter()
                .find_map(|key| entry.get(*key).and_then(Value::as_u64))
                .and_then(|tokens| u32::try_from(tokens).ok());
            Some(ModelDescriptor {
                label: text(entry, &["display_name", "name"]).unwrap_or_else(|| id.clone()),
                context_window: context,
                ..ModelDescriptor::new(id)
            })
        })
        .take(MAX_LISTED_MODELS)
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    Ok(models)
}

/// The `originator` a `ChatGPT` backend request names.
const CHATGPT_ORIGINATOR: &str = "tilo";
/// The client version a `ChatGPT` model list is asked for.
const CHATGPT_CLIENT_VERSION: &str = "1.0.0";
/// The editor identity GitHub Copilot's API requires of every request.
const COPILOT_HEADERS: [(&str, &str); 4] = [
    ("copilot-integration-id", "vscode-chat"),
    ("editor-version", "vscode/1.107.0"),
    ("editor-plugin-version", "copilot-chat/0.35.0"),
    ("openai-intent", "conversation-panel"),
];

/// The `ChatGPT` account a `ChatGPT` access token is for: the
/// `chatgpt_account_id` its `https://api.openai.com/auth` claim names. Read,
/// not verified: the token is the backend's to verify, and the account
/// only routes the request.
fn chatgpt_account_id(token: &str) -> Option<String> {
    use base64::Engine as _;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    let account = claims
        .get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()?;
    (!account.is_empty()
        && account.len() <= 128
        && account
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'))
    .then(|| account.to_string())
}

/// Every header a request to `profile` carries: [`headers`], then the
/// billing project of a Google sign-in. A person's own Google credentials
/// are billed to the project of the `OAuth` client that minted them unless
/// `x-goog-user-project` names another, and Tilo's client project is not
/// one the person can use.
///
/// # Errors
///
/// As [`headers`].
pub fn profile_headers(
    profile: &ProviderProfile,
    secret: Option<&str>,
) -> Result<Vec<Header>, String> {
    let mut all = headers(profile.protocol, secret)?;
    let constant = |name: &'static str, value: &str| Header {
        name,
        value: Zeroizing::new(value.into()),
        secret: false,
    };
    match profile.sign_in_issuer() {
        Some(SignInIssuer::ChatGpt) => {
            all.push(constant("originator", CHATGPT_ORIGINATOR));
            // The backend serves the subscription of the account the
            // token names; a token naming none is refused there, not here.
            if let Some(account) = secret.and_then(chatgpt_account_id) {
                all.push(Header {
                    name: "chatgpt-account-id",
                    value: Zeroizing::new(account),
                    secret: false,
                });
            }
        }
        Some(SignInIssuer::GitHubCopilot) => {
            for (name, value) in COPILOT_HEADERS {
                all.push(constant(name, value));
            }
        }
        _ => {}
    }
    if profile.signs_in_with_google()
        && let Some(project) = profile
            .sign_in
            .as_ref()
            .and_then(|sign_in| sign_in.billing_project.as_deref())
    {
        all.push(Header {
            name: "x-goog-user-project",
            value: Zeroizing::new(project.to_string()),
            secret: false,
        });
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(sampling: Sampling) -> Ask<'static> {
        Ask {
            model: "model-x",
            instructions: "be brief",
            input: "the input",
            max_tokens: 200,
            sampling,
        }
    }

    #[test]
    fn bodies_carry_one_turn_no_tools_and_the_chosen_sampling() {
        for protocol in ProviderProtocol::ALL.iter().copied() {
            let body = body(protocol, &ask(Sampling::Deterministic));
            assert_eq!(body["model"], "model-x");
            assert_eq!(body["max_tokens"], 200);
            assert_eq!(body["temperature"], 0);
            assert!(body.get("tools").is_none());
            assert!(body.get("thinking").is_none());
            assert!(body.to_string().contains("the input"));
            assert!(body.to_string().contains("be brief"));
        }
        let anthropic = body(
            ProviderProtocol::Anthropic,
            &ask(Sampling::ProviderDefaultNoThinking),
        );
        assert!(anthropic.get("temperature").is_none());
        assert_eq!(anthropic["thinking"], json!({ "type": "disabled" }));
        assert_eq!(anthropic["system"], "be brief");
        let openai = body(
            ProviderProtocol::OpenAiCompatible,
            &ask(Sampling::ProviderDefaultNoThinking),
        );
        assert!(openai.get("temperature").is_none());
        assert!(openai.get("thinking").is_none());
        assert_eq!(openai["response_format"]["type"], "json_object");
    }

    #[test]
    fn answers_are_the_text_and_never_a_refusal_a_fragment_or_oversized() {
        let anthropic = r#"{"stop_reason":"end_turn","content":[{"type":"thinking","thinking":""},{"type":"text","text":"{\"result\":1}"}]}"#;
        assert_eq!(
            answer(ProviderProtocol::Anthropic, anthropic, 64).unwrap(),
            r#"{"result":1}"#
        );
        assert!(answer(ProviderProtocol::Anthropic, anthropic, 4).is_err());
        let refused = r#"{"stop_reason":"refusal","content":[]}"#;
        assert!(answer(ProviderProtocol::Anthropic, refused, 64).is_err());
        let cut = r#"{"stop_reason":"max_tokens","content":[{"type":"text","text":"{"}]}"#;
        assert!(answer(ProviderProtocol::Anthropic, cut, 64).is_err());
        let openai = r#"{"choices":[{"finish_reason":"stop","message":{"content":"{}"}}]}"#;
        assert_eq!(
            answer(ProviderProtocol::OpenAiCompatible, openai, 64).unwrap(),
            "{}"
        );
        let truncated = r#"{"choices":[{"finish_reason":"length","message":{"content":"{"}}]}"#;
        assert!(answer(ProviderProtocol::OpenAiCompatible, truncated, 64).is_err());
        assert!(answer(ProviderProtocol::OpenAiCompatible, "not json", 64).is_err());
    }

    #[test]
    fn urls_and_headers_follow_the_dialect() {
        use ProviderProtocol::{Anthropic, OpenAiCompatible};
        assert_eq!(
            url(Anthropic, "https://api.anthropic.com/"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            url(Anthropic, "https://x/v1/messages"),
            "https://x/v1/messages"
        );
        assert_eq!(
            url(OpenAiCompatible, "http://127.0.0.1:8000/v1"),
            "http://127.0.0.1:8000/v1/chat/completions"
        );
        assert_eq!(
            url(OpenAiCompatible, "https://x/v1/chat/completions"),
            "https://x/v1/chat/completions"
        );

        assert!(headers(Anthropic, None).is_err());
        let anthropic = headers(Anthropic, Some("k")).unwrap();
        let pairs: Vec<_> = anthropic
            .iter()
            .map(|header| (header.name, header.value.as_str(), header.secret))
            .collect();
        assert_eq!(
            pairs,
            [
                ("anthropic-version", ANTHROPIC_VERSION, false),
                ("x-api-key", "k", true)
            ]
        );
        let openai = headers(OpenAiCompatible, Some("k")).unwrap();
        assert_eq!(openai[0].name, "authorization");
        assert_eq!(openai[0].value.as_str(), "Bearer k");
        assert!(openai[0].secret);
        assert!(headers(OpenAiCompatible, None).unwrap().is_empty());
    }

    #[test]
    fn a_google_sign_in_bills_its_project_and_a_key_profile_nothing_extra() {
        let vertex = ProviderProfile::google_vertex("tilo-509618", "global", "gemini-4-argon");
        assert_eq!(
            url(vertex.protocol, &vertex.endpoint),
            "https://aiplatform.googleapis.com/v1/projects/tilo-509618/locations/global/endpoints/openapi/chat/completions"
        );
        let sent: Vec<_> = profile_headers(&vertex, Some("ya29.token"))
            .unwrap()
            .iter()
            .map(|header| (header.name, header.value.to_string(), header.secret))
            .collect();
        assert_eq!(
            sent,
            [
                ("authorization", "Bearer ya29.token".to_string(), true),
                ("x-goog-user-project", "tilo-509618".to_string(), false),
            ]
        );
        let keyed = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
        assert_eq!(profile_headers(&keyed, Some("k")).unwrap().len(), 1);
        let mut other = vertex;
        other.sign_in = Some(crate::SignInCredential {
            issuer: "elsewhere".into(),
            billing_project: Some("p".into()),
        });
        assert_eq!(profile_headers(&other, Some("k")).unwrap().len(), 1);
    }

    fn jwt(claims: &Value) -> String {
        use base64::Engine as _;
        let encode = |value: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value);
        format!(
            "{}.{}.signature",
            encode(br#"{"alg":"RS256"}"#),
            encode(claims.to_string().as_bytes())
        )
    }

    #[test]
    fn a_chatgpt_sign_in_speaks_responses_to_its_account() {
        let chatgpt = crate::presets::Preset::ChatGpt.profile();
        assert!(speaks_responses(&chatgpt));
        assert_eq!(
            profile_url(&chatgpt),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        let token = jwt(&json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct-1_x"}
        }));
        let sent: Vec<_> = profile_headers(&chatgpt, Some(&token))
            .unwrap()
            .iter()
            .map(|header| (header.name, header.value.to_string()))
            .collect();
        assert_eq!(sent[0], ("authorization", format!("Bearer {token}")));
        assert!(sent.contains(&("chatgpt-account-id", "acct-1_x".to_string())));
        assert!(sent.contains(&("originator", CHATGPT_ORIGINATOR.to_string())));
        // A token naming no account, or a malformed one, adds none.
        for token in [jwt(&json!({"sub": "x"})), "opaque".to_string()] {
            assert!(chatgpt_account_id(&token).is_none());
        }
        assert!(
            chatgpt_account_id(&jwt(&json!({
                "https://api.openai.com/auth": {"chatgpt_account_id": "a\nb"}
            })))
            .is_none()
        );

        let body = profile_body(&chatgpt, &ask(Sampling::Deterministic));
        assert_eq!(body["instructions"], "be brief");
        assert_eq!(body["input"][0]["content"][0]["text"], "the input");
        assert_eq!(
            (body["stream"].clone(), body["store"].clone()),
            (json!(true), json!(false))
        );
        assert!(body.get("temperature").is_none() && body.get("max_output_tokens").is_none());

        let streamed = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"{}\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",",
            "\"output\":[{\"type\":\"reasoning\"},{\"type\":\"message\",\"content\":",
            "[{\"type\":\"output_text\",\"text\":\"{\\\"a\\\":1}\"}]}]}}\n\n"
        );
        assert_eq!(
            profile_answer(&chatgpt, streamed, 64).unwrap(),
            r#"{"a":1}"#
        );
        assert!(profile_answer(&chatgpt, streamed, 3).is_err());
        let cut = r#"{"status":"incomplete","output":[]}"#;
        assert!(profile_answer(&chatgpt, cut, 64).is_err());
        let refused = r#"{"status":"completed","output":[{"type":"message","content":[{"type":"refusal","refusal":"no"}]}]}"#;
        assert!(profile_answer(&chatgpt, refused, 64).is_err());
        assert!(profile_answer(&chatgpt, "event: ping\n\n", 64).is_err());
    }

    #[test]
    fn openai_s_own_api_takes_responses_and_other_endpoints_chat_unless_told() {
        use crate::presets::Preset;
        let openai = Preset::OpenAi.profile();
        assert_eq!(openai_api(&openai), Some(OpenAiApi::Responses));
        assert!(!speaks_responses(&openai));
        assert_eq!(
            profile_url(&openai),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            responses_url(&openai),
            "https://api.openai.com/v1/responses"
        );
        let mut typed = Preset::Compatible.profile();
        assert_eq!(typed.openai_api, None);
        for endpoint in [
            "https://api.openai.com/v1",
            "https://API.openai.com:443/v1/chat/completions",
        ] {
            typed.endpoint = endpoint.into();
            assert_eq!(openai_api(&typed), Some(OpenAiApi::Responses), "{endpoint}");
            assert_eq!(responses_url(&typed).rsplit('/').next(), Some("responses"));
        }
        for endpoint in [
            "http://api.openai.com/v1",
            "https://api.openai.com.example.test/v1",
            "http://127.0.0.1:8000/v1",
            "https://openrouter.ai/api/v1",
        ] {
            typed.endpoint = endpoint.into();
            assert_eq!(
                openai_api(&typed),
                Some(OpenAiApi::ChatCompletions),
                "{endpoint}"
            );
        }
        typed.endpoint = "http://127.0.0.1:8787/v1/chat/completions".into();
        typed.openai_api = Some(OpenAiApi::Responses);
        assert_eq!(openai_api(&typed), Some(OpenAiApi::Responses));
        assert_eq!(responses_url(&typed), "http://127.0.0.1:8787/v1/responses");
        let mut chat = Preset::OpenAi.profile();
        chat.openai_api = Some(OpenAiApi::ChatCompletions);
        assert_eq!(openai_api(&chat), Some(OpenAiApi::ChatCompletions));
        assert_eq!(
            openai_api(&Preset::ChatGpt.profile()),
            Some(OpenAiApi::Responses)
        );
        assert_eq!(
            openai_api(&Preset::Groq.profile()),
            Some(OpenAiApi::ChatCompletions)
        );
        assert_eq!(openai_api(&Preset::Anthropic.profile()), None);
    }

    #[test]
    fn a_copilot_sign_in_names_the_editor_its_api_requires() {
        let copilot = crate::presets::Preset::Copilot.profile();
        assert!(!speaks_responses(&copilot));
        let names: Vec<_> = profile_headers(&copilot, Some("tid"))
            .unwrap()
            .iter()
            .map(|header| header.name)
            .collect();
        assert!(names.contains(&"copilot-integration-id") && names.contains(&"editor-version"));
        assert_eq!(
            profile_url(&copilot),
            "https://api.githubcopilot.com/chat/completions"
        );
    }

    #[test]
    fn model_lists_come_from_each_dialect() {
        use crate::presets::Preset;
        assert_eq!(
            models_url(&Preset::Anthropic.profile()),
            "https://api.anthropic.com/v1/models"
        );
        assert_eq!(
            models_url(&Preset::Groq.profile()),
            "https://api.groq.com/openai/v1/models"
        );
        let mut typed = Preset::Compatible.profile();
        typed.endpoint = "http://10.0.0.2:8000/v1/chat/completions".into();
        assert_eq!(models_url(&typed), "http://10.0.0.2:8000/v1/models");
        assert!(
            models_url(&Preset::ChatGpt.profile())
                .starts_with("https://chatgpt.com/backend-api/codex/models?client_version=")
        );

        let openai = r#"{"object":"list","data":[{"id":"gpt-b"},{"id":"gpt-a","context_length":128000},{"id":"gpt-a"},{"id":" "}]}"#;
        let models = parse_models(openai).unwrap();
        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, ["gpt-a", "gpt-b"]);
        assert_eq!(models[0].context_window, Some(128_000));
        assert_eq!(models[0].label, "gpt-a");
        let anthropic = r#"{"data":[{"type":"model","id":"claude-x","display_name":"Claude X"}],"has_more":false}"#;
        assert_eq!(parse_models(anthropic).unwrap()[0].label, "Claude X");
        let chatgpt = r#"{"models":[{"slug":"gpt-5-codex","display_name":"GPT-5 Codex"}]}"#;
        assert_eq!(parse_models(chatgpt).unwrap()[0].id, "gpt-5-codex");
        assert!(parse_models(r#"{"error":"nope"}"#).is_err());
        assert!(parse_models("<html>").is_err());
    }
}

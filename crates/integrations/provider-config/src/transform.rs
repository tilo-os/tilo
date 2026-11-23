//! One single-shot model call (a system instruction and one input, answered
//! with text), the endpoint URL and authentication headers of a profile, and
//! the provider's model list.
//!
//! These are pure pieces with no HTTP client. A single-shot ask is an
//! ordinary model turn written and read by the one wire codec
//! ([`crate::wire`]), plus the few fields only such an ask sets (its
//! sampling and the shape of its answer), so every Tilo process that asks a
//! provider for one answer speaks the same dialects through the same code.

use serde_json::{Map, Value, json};
use zeroize::Zeroizing;

use crate::protocol::{Content, Message, ModelRequest, Role, StopReason, ToolChoice};
use crate::{
    KeyHeader, ModelDescriptor, OpenAiApi, ProviderProfile, ProviderProtocol, SignInIssuer, wire,
};

/// The Anthropic Messages API version every request names.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// How the model samples its answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sampling {
    /// `temperature: 0`: the same input gives the same answer wherever the
    /// model allows it. The gateway's transforms want this.
    #[default]
    Deterministic,
    /// No `temperature` and no other setting: whatever the provider does by
    /// default.
    ProviderDefault,
    /// No `temperature` and, on Anthropic, extended thinking turned off.
    /// Some current models (Claude Haiku 5.5 among them) refuse any
    /// non-default temperature with a 400, and they think adaptively by
    /// default, which would spend a small `max_tokens` before any text is
    /// written. A short, bounded answer from such a model needs both.
    ProviderDefaultNoThinking,
}

/// What shape the answer must have.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Output<'a> {
    /// Free text.
    #[default]
    Text,
    /// One JSON object. The OpenAI-compatible dialect says so with
    /// `response_format`; Anthropic's Messages API has no such switch and is
    /// only asked for one.
    JsonObject,
    /// A JSON document matching `schema`, which Anthropic enforces with
    /// `output_config`, spending `effort` (`"low"` for a short, bounded
    /// answer) on it. The OpenAI-compatible dialect constrains nothing.
    Schema { schema: &'a Value, effort: &'a str },
}

/// One single-shot request: a system instruction and one user turn, no tools.
#[derive(Clone, Copy, Debug)]
pub struct Ask<'a> {
    pub model: &'a str,
    /// The system instruction; empty for none, which adds no system message.
    pub instructions: &'a str,
    pub input: &'a str,
    /// The most tokens the answer may use; `None` leaves it to the provider.
    pub max_tokens: Option<u32>,
    pub sampling: Sampling,
    pub output: Output<'a>,
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
    /// Static for the headers a protocol and a key send; a profile's own
    /// ([`crate::ProviderProfile::headers`]) are named in its settings.
    pub name: std::borrow::Cow<'static, str>,
    pub value: Zeroizing<String>,
    pub secret: bool,
}

/// The headers that version and authenticate a request. Anthropic needs a
/// key; an OpenAI-compatible endpoint (a local server, say) may not. A
/// profile's own rule, which also admits a keyless local endpoint, is
/// [`profile_headers`].
///
/// # Errors
///
/// Anthropic without a `secret`.
pub fn headers(protocol: ProviderProtocol, secret: Option<&str>) -> Result<Vec<Header>, String> {
    keyed_headers(protocol, KeyHeader::of(protocol), secret)
}

/// [`headers`], with the key in `key_header` rather than the protocol's
/// own.
///
/// # Errors
///
/// Anthropic without a `secret`.
pub fn keyed_headers(
    protocol: ProviderProtocol,
    key_header: KeyHeader,
    secret: Option<&str>,
) -> Result<Vec<Header>, String> {
    let key = |secret: &str| Header {
        name: key_header.name().into(),
        value: Zeroizing::new(match key_header {
            KeyHeader::Bearer => format!("Bearer {secret}"),
            KeyHeader::XApiKey | KeyHeader::ApiKey => secret.into(),
        }),
        secret: true,
    };
    match (protocol, secret) {
        (ProviderProtocol::Anthropic, None) => {
            Err("Anthropic requests require a configured credential".into())
        }
        (ProviderProtocol::Anthropic, Some(secret)) => Ok(dialect_headers(protocol)
            .into_iter()
            .chain([key(secret)])
            .collect()),
        (ProviderProtocol::OpenAiCompatible, Some(secret)) => Ok(vec![key(secret)]),
        (ProviderProtocol::OpenAiCompatible, None) => Ok(Vec::new()),
    }
}

/// The headers a `protocol`'s dialect requires of every request, before its
/// credential.
fn dialect_headers(protocol: ProviderProtocol) -> Vec<Header> {
    match protocol {
        ProviderProtocol::Anthropic => vec![Header {
            name: "anthropic-version".into(),
            value: Zeroizing::new(ANTHROPIC_VERSION.into()),
            secret: false,
        }],
        ProviderProtocol::OpenAiCompatible => Vec::new(),
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

/// The most a single-shot ask's answer is allowed when it names no bound.
const DEFAULT_MAX_TOKENS: u32 = 4096;

impl Ask<'_> {
    /// The turn this ask is: its instruction and its input, no tools.
    fn request(&self) -> ModelRequest {
        let mut messages = Vec::with_capacity(2);
        if !self.instructions.is_empty() {
            messages.push(Message::text(Role::System, self.instructions));
        }
        messages.push(Message::text(Role::User, self.input));
        ModelRequest {
            messages,
            tools: Vec::new(),
            tool_choice: ToolChoice::None,
            max_output_tokens: self.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        }
    }

    /// The fields only a single-shot ask sets, on `api`'s wire.
    fn fields(&self, api: wire::Api) -> Map<String, Value> {
        let mut fields = Map::new();
        match self.sampling {
            Sampling::Deterministic => {
                fields.insert("temperature".into(), Value::from(0));
            }
            Sampling::ProviderDefault => {}
            Sampling::ProviderDefaultNoThinking => {
                if api == wire::Api::Anthropic {
                    fields.insert("thinking".into(), json!({ "type": "disabled" }));
                }
            }
        }
        match (self.output, api) {
            (Output::JsonObject, wire::Api::Chat) => {
                fields.insert("response_format".into(), json!({ "type": "json_object" }));
            }
            (Output::JsonObject, wire::Api::Responses | wire::Api::ChatGpt) => {
                fields.insert(
                    "text".into(),
                    json!({ "format": { "type": "json_object" } }),
                );
            }
            (Output::Schema { schema, effort }, wire::Api::Anthropic) => {
                fields.insert(
                    "output_config".into(),
                    json!({
                        "effort": effort,
                        "format": { "type": "json_schema", "schema": schema }
                    }),
                );
            }
            _ => {}
        }
        fields
    }
}

/// The request body of `ask` for `profile`, on the wire its turns take
/// ([`wire::Api::of`]).
///
/// # Errors
///
/// The ask is no request the profile's wire can carry.
pub fn profile_body(profile: &ProviderProfile, ask: &Ask<'_>) -> Result<Value, String> {
    let fields = ask.fields(wire::Api::of(profile));
    wire::body(
        profile,
        ask.model,
        &ask.request(),
        wire::Ask {
            streaming: false,
            extra: Some(&fields),
        },
    )
    .map_err(|error| error.message)
}

/// The answer text of `body`, a provider's response to [`profile_body`] of
/// `ask`: whole, or the stream of a wire that always streams. A refusal or a
/// cut-off answer is an error rather than text: the caller would otherwise
/// validate half an answer.
///
/// # Errors
///
/// The response is no answer, a refusal, cut off, without text, or longer
/// than `max_bytes`.
pub fn profile_answer(
    profile: &ProviderProfile,
    ask: &Ask<'_>,
    body: &str,
    max_bytes: usize,
) -> Result<String, String> {
    let request = ask.request();
    let api = wire::Api::of(profile);
    let response = if api.always_streams() {
        let mut stream = wire::Stream::of(profile);
        let mut deltas = Vec::new();
        stream
            .push(body.as_bytes(), &mut deltas)
            .and_then(|()| stream.finish(&request, &mut deltas))
            .map_err(|error| error.model_error().message)
    } else {
        wire::response(profile, &request, body).map_err(|error| error.message)
    }
    .map_err(|why| {
        if why.is_empty() {
            "provider response was unreadable".to_owned()
        } else {
            why
        }
    })?;
    match response.stop_reason {
        StopReason::Refusal => return Err("the model declined the transform".into()),
        StopReason::OutputLimit => return Err("the model's answer was cut off".into()),
        StopReason::EndTurn | StopReason::ToolCalls => {}
    }
    let text: String = response
        .message
        .content
        .iter()
        .filter_map(|content| match content {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if text.is_empty() {
        return Err("provider response carried no answer text".into());
    }
    if text.len() > max_bytes {
        return Err("the model's answer exceeds its size limit".into());
    }
    Ok(text)
}

/// The URL listing `profile`'s models: its own path under the endpoint, or
/// from its origin when the path starts with `/`
/// ([`ProviderProfile::models_path`]), else `GET /v1/models` of an Anthropic
/// endpoint and `GET /models` of the others. `None` when the provider lists
/// none.
pub fn models_url(profile: &ProviderProfile) -> Option<String> {
    let base = profile.endpoint.trim_end_matches('/');
    let base = base
        .strip_suffix("/chat/completions")
        .or_else(|| base.strip_suffix("/v1/messages"))
        .or_else(|| base.strip_suffix("/responses"))
        .unwrap_or(base);
    if let Some(path) = &profile.models_path {
        let path = path.trim();
        if path.is_empty() {
            return None;
        }
        // A path from the origin: an Anthropic-compatible endpoint whose
        // provider lists its models beside its OpenAI-compatible one.
        if path.starts_with('/') {
            let origin = url::Url::parse(base).ok()?.origin().ascii_serialization();
            return Some(format!("{origin}{path}"));
        }
        return Some(format!("{base}/{path}"));
    }
    Some(match profile.protocol {
        ProviderProtocol::Anthropic if base.ends_with("/v1") => format!("{base}/models"),
        ProviderProtocol::Anthropic => format!("{base}/v1/models"),
        // The ChatGPT backend lists the models of the client version it is
        // told; any current version lists the current models.
        ProviderProtocol::OpenAiCompatible if speaks_responses(profile) => {
            format!("{base}/models?client_version={CHATGPT_CLIENT_VERSION}")
        }
        ProviderProtocol::OpenAiCompatible => format!("{base}/models"),
    })
}

/// At most this many models are read from one list.
const MAX_LISTED_MODELS: usize = 512;

/// The models of a model-list response: `data[].id` (`OpenAI`, Anthropic
/// and every compatible provider), `models[].slug` (the `ChatGPT` backend)
/// or `models[].name` (Gemini's own list), with the provider's display
/// name, context size and capabilities when it gives them, and the family
/// table's capabilities when it does not ([`crate::capabilities`]).
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
            let id = text(entry, &[id_key, "id", "name"])?;
            let mut model = ModelDescriptor {
                label: text(entry, &["display_name", "displayName", "name"])
                    .unwrap_or_else(|| id.clone()),
                ..ModelDescriptor::new(id)
            };
            crate::capabilities::apply_listing(&mut model, entry);
            Some(model)
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

/// The claims of a JSON Web Token (an `OpenAI` sign-in's access or id
/// token), read and not verified: whoever issued it verifies it, and its
/// holder only reads what it says of itself (whose account, until when).
#[must_use]
pub fn token_claims(token: &str) -> Option<Value> {
    use base64::Engine as _;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The `ChatGPT` account a `ChatGPT` access token is for: the
/// `chatgpt_account_id` its `https://api.openai.com/auth` claim names. Read,
/// not verified: the token is the backend's to verify, and the account
/// only routes the request.
fn chatgpt_account_id(token: &str) -> Option<String> {
    let claims = token_claims(token)?;
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
/// A keyless local profile ([`crate::is_unauthenticated_local`])
/// authenticates no one, so it sends its dialect's headers and no
/// credential on either protocol: the same rule that let the gateway plan
/// without a key.
///
/// # Errors
///
/// Anthropic without a `secret`, unless the profile is a keyless local one.
pub fn profile_headers(
    profile: &ProviderProfile,
    secret: Option<&str>,
) -> Result<Vec<Header>, String> {
    let key_header = profile
        .key_header
        .unwrap_or_else(|| KeyHeader::of(profile.protocol));
    let mut all = match secret {
        Some(secret) => keyed_headers(profile.protocol, key_header, Some(secret))?,
        None if crate::is_unauthenticated_local(profile) => dialect_headers(profile.protocol),
        None => keyed_headers(profile.protocol, key_header, None)?,
    };
    let constant = |name: &'static str, value: &str| Header {
        name: name.into(),
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
                    name: "chatgpt-account-id".into(),
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
            name: "x-goog-user-project".into(),
            value: Zeroizing::new(project.to_string()),
            secret: false,
        });
    }
    // The provider's own headers, which settings refuse to name a key or the
    // protocol's: checked again here, so settings written by hand send none of
    // those either.
    for header in &profile.headers {
        header.check()?;
        all.push(Header {
            name: header.name.trim().to_owned().into(),
            value: Zeroizing::new(header.value.clone()),
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
            max_tokens: Some(200),
            sampling,
            output: Output::Text,
        }
    }

    fn hosted(protocol: ProviderProtocol) -> ProviderProfile {
        ProviderProfile {
            endpoint: "https://models.example.test/v1".into(),
            ..ProviderProfile::hosted(protocol)
        }
    }

    fn sent(protocol: ProviderProtocol, ask: &Ask<'_>) -> Value {
        profile_body(&hosted(protocol), ask).unwrap()
    }

    #[test]
    fn bodies_carry_one_turn_no_tools_and_the_chosen_sampling() {
        for protocol in ProviderProtocol::ALL.iter().copied() {
            let body = sent(protocol, &ask(Sampling::Deterministic));
            assert_eq!(body["model"], "model-x");
            assert_eq!(body["max_tokens"], 200);
            assert_eq!(body["temperature"], 0);
            assert!(body.get("tools").is_none());
            assert!(body.get("thinking").is_none());
            assert!(body.get("response_format").is_none());
            assert!(body.to_string().contains("the input"));
            assert!(body.to_string().contains("be brief"));
        }
        let anthropic = sent(
            ProviderProtocol::Anthropic,
            &ask(Sampling::ProviderDefaultNoThinking),
        );
        assert!(anthropic.get("temperature").is_none());
        assert_eq!(anthropic["thinking"], json!({ "type": "disabled" }));
        assert!(anthropic["system"].to_string().contains("be brief"));
        let openai = sent(
            ProviderProtocol::OpenAiCompatible,
            &ask(Sampling::ProviderDefaultNoThinking),
        );
        assert!(openai.get("temperature").is_none());
        assert!(openai.get("thinking").is_none());
        let plain = sent(ProviderProtocol::Anthropic, &ask(Sampling::ProviderDefault));
        assert!(plain.get("temperature").is_none());
        assert!(plain.get("thinking").is_none());
    }

    #[test]
    fn a_body_names_the_shape_its_answer_must_have() {
        let schema = json!({ "type": "object" });
        let mut structured = ask(Sampling::ProviderDefault);
        structured.output = Output::Schema {
            schema: &schema,
            effort: "low",
        };
        let anthropic = sent(ProviderProtocol::Anthropic, &structured);
        assert_eq!(anthropic["output_config"]["effort"], "low");
        assert_eq!(anthropic["output_config"]["format"]["type"], "json_schema");
        assert_eq!(anthropic["output_config"]["format"]["schema"], schema);
        let openai = sent(ProviderProtocol::OpenAiCompatible, &structured);
        assert!(openai.get("output_config").is_none());

        let mut object = ask(Sampling::Deterministic);
        object.output = Output::JsonObject;
        assert_eq!(
            sent(ProviderProtocol::OpenAiCompatible, &object)["response_format"]["type"],
            "json_object"
        );
        assert!(
            sent(ProviderProtocol::Anthropic, &object)
                .get("response_format")
                .is_none()
        );
        let mut responses = hosted(ProviderProtocol::OpenAiCompatible);
        responses.openai_api = Some(OpenAiApi::Responses);
        assert_eq!(
            profile_body(&responses, &object).unwrap()["text"]["format"]["type"],
            "json_object"
        );

        let mut silent = ask(Sampling::Deterministic);
        silent.instructions = "";
        assert!(
            sent(ProviderProtocol::Anthropic, &silent)
                .get("system")
                .is_none()
        );
        let messages = sent(ProviderProtocol::OpenAiCompatible, &silent);
        assert_eq!(messages["messages"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn answers_are_the_text_and_never_a_refusal_a_fragment_or_oversized() {
        let asked = ask(Sampling::Deterministic);
        let anthropic = hosted(ProviderProtocol::Anthropic);
        let answer = |profile: &ProviderProfile, body: &str, most: usize| {
            profile_answer(profile, &asked, body, most)
        };
        let said = r#"{"stop_reason":"end_turn","content":[{"type":"thinking","thinking":""},{"type":"text","text":"{\"result\":1}"}]}"#;
        assert_eq!(answer(&anthropic, said, 64).unwrap(), r#"{"result":1}"#);
        assert!(answer(&anthropic, said, 4).is_err());
        let refused = r#"{"stop_reason":"refusal","content":[]}"#;
        assert!(answer(&anthropic, refused, 64).is_err());
        let cut = r#"{"stop_reason":"max_tokens","content":[{"type":"text","text":"{"}]}"#;
        assert!(answer(&anthropic, cut, 64).is_err());
        let openai = hosted(ProviderProtocol::OpenAiCompatible);
        let whole = r#"{"choices":[{"finish_reason":"stop","message":{"content":"{}"}}]}"#;
        assert_eq!(answer(&openai, whole, 64).unwrap(), "{}");
        let truncated = r#"{"choices":[{"finish_reason":"length","message":{"content":"{"}}]}"#;
        assert!(answer(&openai, truncated, 64).is_err());
        assert!(answer(&openai, "not json", 64).is_err());
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
            .map(|header| (&*header.name, header.value.as_str(), header.secret))
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
    fn a_keyless_local_profile_sends_no_credential_on_either_protocol() {
        for protocol in ProviderProtocol::ALL.iter().copied() {
            let mut profile = ProviderProfile::local_vllm();
            profile.protocol = protocol;
            profile.endpoint = "http://127.0.0.1:8787".into();
            assert!(crate::is_unauthenticated_local(&profile));
            let made = profile_headers(&profile, None).unwrap();
            let sent: Vec<_> = made
                .iter()
                .map(|header| (&*header.name, header.value.to_string(), header.secret))
                .collect();
            let expected: &[(&str, &str, bool)] = match protocol {
                ProviderProtocol::Anthropic => &[("anthropic-version", ANTHROPIC_VERSION, false)],
                ProviderProtocol::OpenAiCompatible => &[],
            };
            assert_eq!(
                sent,
                expected
                    .iter()
                    .map(|(name, value, secret)| (*name, (*value).to_string(), *secret))
                    .collect::<Vec<_>>(),
                "{protocol:?}"
            );
        }
    }

    #[test]
    fn a_remote_endpoint_without_a_key_still_refuses_the_anthropic_protocol() {
        let mut hosted = ProviderProfile::hosted(ProviderProtocol::Anthropic);
        hosted.credential = None;
        assert!(!crate::is_unauthenticated_local(&hosted));
        assert!(profile_headers(&hosted, None).is_err());

        // Named as this device, but at an address something else resolves.
        let mut named = hosted;
        named.location = crate::EndpointLocation::ThisDevice;
        named.endpoint = "http://localhost:8787".into();
        assert!(!crate::is_unauthenticated_local(&named));
        assert!(profile_headers(&named, None).is_err());

        // A credential reference means the key is the vault's to resolve.
        let mut keyed = ProviderProfile::local_vllm();
        keyed.protocol = ProviderProtocol::Anthropic;
        keyed.endpoint = "http://127.0.0.1:8787".into();
        keyed.credential = Some(crate::SecretReference::provider_api_key(&keyed.id));
        assert!(!crate::is_unauthenticated_local(&keyed));
        assert!(profile_headers(&keyed, None).is_err());
    }

    #[test]
    fn a_google_sign_in_bills_its_project_and_a_key_profile_nothing_extra() {
        let vertex = ProviderProfile::google_vertex("tilo-509618", "global", "gemini-4-argon");
        assert_eq!(
            url(vertex.protocol, &vertex.endpoint),
            "https://aiplatform.googleapis.com/v1/projects/tilo-509618/locations/global/endpoints/openapi/chat/completions"
        );
        let made = profile_headers(&vertex, Some("ya29.token")).unwrap();
        let sent: Vec<_> = made
            .iter()
            .map(|header| (&*header.name, header.value.to_string(), header.secret))
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
            kept: None,
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
        let chatgpt = crate::presets::Preset::CHATGPT.profile();
        assert!(speaks_responses(&chatgpt));
        assert_eq!(
            wire::url(&chatgpt),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        let token = jwt(&json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct-1_x"}
        }));
        let made = profile_headers(&chatgpt, Some(&token)).unwrap();
        let sent: Vec<_> = made
            .iter()
            .map(|header| (&*header.name, header.value.to_string()))
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

        let asked = ask(Sampling::Deterministic);
        let body = profile_body(&chatgpt, &asked).unwrap();
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
            profile_answer(&chatgpt, &asked, streamed, 64).unwrap(),
            r#"{"a":1}"#
        );
        assert!(profile_answer(&chatgpt, &asked, streamed, 3).is_err());
        let event = |response: &str| {
            format!("data: {{\"type\":\"response.completed\",\"response\":{response}}}\n\n")
        };
        let cut = event(r#"{"status":"incomplete","output":[]}"#);
        assert!(profile_answer(&chatgpt, &asked, &cut, 64).is_err());
        let refused = event(
            r#"{"status":"completed","output":[{"type":"message","content":[{"type":"refusal","refusal":"no"}]}]}"#,
        );
        assert!(profile_answer(&chatgpt, &asked, &refused, 64).is_err());
        assert!(profile_answer(&chatgpt, &asked, "event: ping\n\n", 64).is_err());
    }

    #[test]
    fn openai_s_own_api_takes_responses_and_other_endpoints_chat_unless_told() {
        use crate::presets::Preset;
        let openai = Preset::OPENAI.profile();
        assert_eq!(openai_api(&openai), Some(OpenAiApi::Responses));
        assert!(!speaks_responses(&openai));
        assert_eq!(wire::url(&openai), "https://api.openai.com/v1/responses");
        assert_eq!(
            responses_url(&openai),
            "https://api.openai.com/v1/responses"
        );
        let mut typed = Preset::COMPATIBLE.profile();
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
        let mut chat = Preset::OPENAI.profile();
        chat.openai_api = Some(OpenAiApi::ChatCompletions);
        assert_eq!(openai_api(&chat), Some(OpenAiApi::ChatCompletions));
        assert_eq!(
            openai_api(&Preset::CHATGPT.profile()),
            Some(OpenAiApi::Responses)
        );
        assert_eq!(
            openai_api(&Preset::GROQ.profile()),
            Some(OpenAiApi::ChatCompletions)
        );
        assert_eq!(openai_api(&Preset::ANTHROPIC.profile()), None);
    }

    #[test]
    fn a_copilot_sign_in_names_the_editor_its_api_requires() {
        let copilot = crate::presets::Preset::COPILOT.profile();
        assert!(!speaks_responses(&copilot));
        let made = profile_headers(&copilot, Some("tid")).unwrap();
        let names: Vec<_> = made.iter().map(|header| &*header.name).collect();
        assert!(names.contains(&"copilot-integration-id") && names.contains(&"editor-version"));
        assert_eq!(
            wire::url(&copilot),
            "https://api.githubcopilot.com/chat/completions"
        );
    }

    #[test]
    fn model_lists_come_from_each_dialect() {
        use crate::presets::Preset;
        assert_eq!(
            models_url(&Preset::ANTHROPIC.profile()).as_deref(),
            Some("https://api.anthropic.com/v1/models")
        );
        assert_eq!(
            models_url(&Preset::GROQ.profile()).as_deref(),
            Some("https://api.groq.com/openai/v1/models")
        );
        let mut typed = Preset::COMPATIBLE.profile();
        typed.endpoint = "http://10.0.0.2:8000/v1/chat/completions".into();
        assert_eq!(
            models_url(&typed).as_deref(),
            Some("http://10.0.0.2:8000/v1/models")
        );
        assert!(
            models_url(&Preset::CHATGPT.profile())
                .unwrap()
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

    #[test]
    fn a_providers_own_headers_go_with_every_request_and_never_stand_for_its_key() {
        use crate::ProviderHeader;
        let header = |name: &str, value: &str| ProviderHeader {
            name: name.into(),
            value: value.into(),
        };
        let mut profile = ProviderProfile {
            protocol: ProviderProtocol::Anthropic,
            endpoint: "https://api.anthropic.com".into(),
            headers: vec![header("anthropic-workspace-id", "wrkspc_1")],
            ..ProviderProfile::default()
        };
        let made = profile_headers(&profile, Some("k")).unwrap();
        let sent: Vec<_> = made
            .iter()
            .map(|header| (&*header.name, header.value.as_str(), header.secret))
            .collect();
        assert_eq!(
            sent,
            [
                ("anthropic-version", ANTHROPIC_VERSION, false),
                ("x-api-key", "k", true),
                ("anthropic-workspace-id", "wrkspc_1", false),
            ]
        );
        for refused in [
            header("X-Api-Key", "another key"),
            header("Authorization", "Bearer other"),
            header("anthropic-version", "1999-01-01"),
            header("bad name", "v"),
            header("x-ok", "line\r\nInjected: yes"),
        ] {
            assert!(refused.check().is_err(), "{refused:?} is refused");
            profile.headers = vec![refused];
            assert!(
                profile_headers(&profile, Some("k")).is_err(),
                "settings written by hand send no refused header"
            );
        }
    }
}

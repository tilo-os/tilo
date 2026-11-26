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

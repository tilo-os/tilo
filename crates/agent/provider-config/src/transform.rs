//! One single-shot model call in either provider dialect: the request body,
//! the endpoint URL, the authentication headers and the answer text.
//!
//! These are pure pieces with no HTTP client, so every Tilo process that asks
//! a provider for one answer (the per-user model gateway, a host summarising
//! activity) speaks the same dialects through the same code instead of each
//! carrying its own vendor-specific request shapes.

use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::{ProviderProfile, ProviderProtocol};

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
///
/// # Errors
///
/// The response is unreadable, a refusal, cut off, has no text, or the text
/// is longer than `max_bytes`.
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
}

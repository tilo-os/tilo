//! Reaching one model, over the one protocol every provider Tilo offers
//! speaks: OpenAI-compatible chat completions, whose request and answer
//! shapes are [`transform`](agent_provider_config::transform)'s.
//!
//! A run names an endpoint — a local runtime on the loopback, a hosted
//! provider, a stand-in in CI — and asks its cases that. Nothing here reads a
//! key from the environment, a settings file or an account store: the caller
//! hands the key in, so a benchmark run never picks up whichever credential
//! happened to be around. That is the difference between a number you can
//! trust and one that came from a silent fallback to another provider.
//!
//! ## What is recorded besides the answer
//!
//! The model the endpoint reported using, the tokens it said it spent, and
//! the request's own digest: comparing two models on the same cases is only
//! honest if the report says which endpoint answered and which request it
//! was asked, not which one was configured.

use std::collections::BTreeMap;
use std::time::Duration;

use agent_provider_config::ProviderProtocol;
use agent_provider_config::transform::{self, Ask, Output, Sampling};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// How long one case may take before the run counts it as no answer.
pub const TIMEOUT: Duration = Duration::from_secs(120);

/// The longest answer read from one case.
const MAX_ANSWER_BYTES: usize = 1024 * 1024;

/// Where a run asks its questions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// The `chat/completions` URL, exactly: a run states it once, so the
    /// report cannot be read as being about a different provider than the
    /// one that answered.
    pub url: String,
    /// The model name the endpoint is asked for.
    pub model: String,
    /// The name this endpoint is reported under, e.g. `local` or
    /// `hosted-small`. Unique within a run.
    pub label: String,
    /// What a million tokens cost, in the currency the report prints. A local
    /// model states zero.
    pub price: Price,
}

/// What a run costs per million tokens, in millionths of the currency unit:
/// an exact price is an integer, so a report never shows a rounded one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Price {
    /// Millionths of a currency unit per million input tokens.
    pub input: u64,
    /// Millionths of a currency unit per million output tokens.
    pub output: u64,
}

impl Price {
    /// What `usage` costs.
    #[must_use]
    pub fn of(&self, usage: &Usage) -> Cost {
        // Tokens times the millionth price, over a million.
        let charged = |tokens: u64, millionths: u64| {
            u128::from(tokens)
                .saturating_mul(u128::from(millionths))
                .checked_div(1_000_000)
                .unwrap_or_default()
        };
        let total = charged(usage.input_tokens, self.input)
            .saturating_add(charged(usage.output_tokens, self.output));
        Cost {
            millionths: u64::try_from(total).unwrap_or(u64::MAX),
        }
    }
}

/// What a run spent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cost {
    /// Millionths of the currency unit.
    pub millionths: u64,
}

impl Cost {
    /// The cost as an amount with six places, the precision it accumulated at.
    #[must_use]
    pub fn amount(self) -> String {
        format!(
            "{}.{:06}",
            self.millionths.checked_div(1_000_000).unwrap_or_default(),
            self.millionths.checked_rem(1_000_000).unwrap_or_default()
        )
    }
}

impl std::ops::Add for Cost {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            millionths: self.millionths.saturating_add(other.millionths),
        }
    }
}

/// What the endpoint said it spent on one answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Tokens the endpoint counted in the prompt.
    pub input_tokens: u64,
    /// Tokens the endpoint counted in the answer.
    pub output_tokens: u64,
}

impl Usage {
    /// This usage plus `other`, saturating rather than wrapping: a run never
    /// reports fewer tokens than it saw.
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
        }
    }
}

/// A case's answer, as the endpoint gave it.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    /// The assistant's text. Empty is an answer too, and a case fails on it
    /// rather than the run treating it as no answer at all.
    pub text: String,
    /// The model the endpoint reported, which need not be the one asked for.
    pub served_model: Option<String>,
    /// The tokens the endpoint reported spending.
    pub usage: Usage,
    /// The digest of the request this answered, so two runs' reports can be
    /// compared without storing the prompts twice.
    pub request: String,
}

/// Why an endpoint gave no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreachable {
    /// The provider's error identifiers or the transport's own words — never
    /// the URL, the key or the prompt.
    pub reason: String,
}

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// The chat-completions body a case is asked with: the instructions as the
/// system message, the input as the user message, and nothing sampled, so two
/// runs of one case on one model differ only when the model does. The
/// endpoint's own answer length applies: a benchmark asks the model, not a
/// limit of the harness.
#[must_use]
pub fn request(endpoint: &Endpoint, instructions: &str, input: &str) -> Value {
    transform::body(
        ProviderProtocol::OpenAiCompatible,
        &Ask {
            model: &endpoint.model,
            instructions,
            input,
            max_tokens: None,
            sampling: Sampling::Deterministic,
            output: Output::Text,
        },
    )
}

/// The digest a report names the request by: sixteen hex digits, enough for
/// two runs' report to agree or disagree on what was asked.
#[must_use]
pub fn digest(body: &Value) -> String {
    let mut hash = Sha256::new();
    hash.update(body.to_string().as_bytes());
    let digest = hash.finalize();
    digest.iter().take(8).fold(String::new(), |mut text, byte| {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// One client for a run's cases: Tilo's own TLS defaults, no retries, and a
/// whole-request deadline so a hung endpoint fails one case instead of the
/// run.
///
/// # Errors
///
/// When the client cannot be built.
pub fn client() -> Result<reqwest::blocking::Client, String> {
    tilo_net::http::blocking_client()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("no HTTP client: {error}"))
}

/// Ask `endpoint` one question, `instructions` with `input`.
///
/// # Errors
///
/// [`Unreachable`] when the endpoint could not be reached, refused the
/// request or answered in a shape that is not a completion. The run counts
/// that as the case failing and prints why; it never asks another endpoint
/// instead.
pub fn ask(
    client: &reqwest::blocking::Client,
    endpoint: &Endpoint,
    key: &str,
    instructions: &str,
    input: &str,
) -> Result<Answer, Unreachable> {
    let body = request(endpoint, instructions, input);
    let mut outgoing = client.post(&endpoint.url).json(&body);
    if !key.is_empty() {
        outgoing = outgoing.bearer_auth(key);
    }
    let reply = outgoing.send().map_err(|error| Unreachable {
        // The transport's own words say what failed without echoing the URL,
        // the key or the prompt.
        reason: format!("no answer: {error}"),
    })?;
    let status = reply.status().as_u16();
    let bytes = reply.bytes().map_err(|error| Unreachable {
        reason: format!("the answer could not be read: {error}"),
    })?;
    let parsed: Value = serde_json::from_slice(&bytes).unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(Unreachable {
            reason: format!(
                "the endpoint refused the request with status {status}{}",
                error_fields(&parsed)
            ),
        });
    }
    read_answer(&body, &parsed)
}

/// The provider error's `type`, `param` and `code`, each only when it is a
/// short identifier: the provider's prose never becomes part of a report.
fn error_fields(body: &Value) -> String {
    use std::fmt::Write as _;
    let error = body
        .get("error")
        .filter(|error| error.is_object())
        .unwrap_or(body);
    let mut fields = String::new();
    for name in ["type", "param", "code"] {
        let Some(value) = error.get(name).and_then(Value::as_str) else {
            continue;
        };
        if !value.is_empty()
            && value.len() <= 64
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'_' | b'.' | b'[' | b']')
            })
        {
            let _ = write!(fields, " {name}={value}");
        }
    }
    fields
}

fn read_answer(body: &Value, reply: &Value) -> Result<Answer, Unreachable> {
    let text = transform::answer_value(ProviderProtocol::OpenAiCompatible, reply, MAX_ANSWER_BYTES)
        .map_err(|reason| Unreachable { reason })?;
    let served_model = reply
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let tokens = |path: &str| {
        reply
            .pointer(path)
            .and_then(Value::as_u64)
            .unwrap_or_default()
    };
    Ok(Answer {
        text,
        served_model,
        usage: Usage {
            input_tokens: tokens("/usage/prompt_tokens"),
            output_tokens: tokens("/usage/completion_tokens"),
        },
        request: digest(body),
    })
}

/// The endpoints a run names in a file, by label.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Endpoints(pub BTreeMap<String, Endpoint>);

impl Endpoints {
    /// The endpoints in `path`: `{"endpoints": [ {…}, … ]}`, one entry per
    /// label. Keys are not read from here — the file is meant to be
    /// reviewable and committed, so it holds no secret.
    ///
    /// # Errors
    ///
    /// When the file is not this shape, or two entries share a label.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let bytes = std::fs::read(path)
            .map_err(|error| format!("{} cannot be read: {error}", path.display()))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{} is not an endpoint list: {error}", path.display()))?;
        let entries = value
            .get("endpoints")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{} has no `endpoints` list", path.display()))?;
        let mut endpoints = BTreeMap::new();
        for entry in entries {
            let field = |name: &str| {
                entry
                    .get(name)
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| format!("an endpoint needs `{name}`"))
            };
            let label = field("label")?;
            let millionths = |name: &str| {
                entry
                    .pointer(&format!("/price/{name}"))
                    .and_then(Value::as_u64)
                    .unwrap_or_default()
            };
            let endpoint = Endpoint {
                url: field("url")?,
                model: field("model")?,
                label: label.clone(),
                price: Price {
                    input: millionths("input"),
                    output: millionths("output"),
                },
            };
            if endpoints.insert(label.clone(), endpoint).is_some() {
                return Err(format!("two endpoints are labelled `{label}`"));
            }
        }
        Ok(Self(endpoints))
    }

    /// The endpoint labelled `label`.
    #[must_use]
    pub fn get(&self, label: &str) -> Option<&Endpoint> {
        self.0.get(label)
    }

    /// Every label, in order.
    #[must_use]
    pub fn labels(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_price_is_charged_per_token_without_rounding_the_accumulation() {
        let price = Price {
            input: 3_000_000,
            output: 15_000_000,
        };
        let cost = price.of(&Usage {
            input_tokens: 1_200,
            output_tokens: 340,
        });
        // 1200 x 3 / 1e6 + 340 x 15 / 1e6 = 0.0036 + 0.0051
        assert_eq!(cost.amount(), "0.008700");
        assert_eq!(
            Price::default().of(&Usage {
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
            }),
            Cost::default(),
            "a local model costs nothing to run"
        );
        assert_eq!((cost + cost).amount(), "0.017400");
    }

    #[test]
    fn a_request_names_the_model_and_samples_nothing() {
        let endpoint = Endpoint {
            url: "http://127.0.0.1:1/v1/chat/completions".into(),
            model: "small".into(),
            label: "local".into(),
            price: Price::default(),
        };
        let body = request(&endpoint, "rewrite this", "the text");
        assert_eq!(body["model"], "small");
        assert_eq!(body["temperature"], 0);
        assert!(body.get("max_tokens").is_none(), "{body}");
        assert!(body.get("response_format").is_none(), "{body}");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "rewrite this");
        assert_eq!(body["messages"][1]["content"], "the text");
        assert_eq!(digest(&body).len(), 16);
        assert_ne!(
            digest(&body),
            digest(&request(&endpoint, "other", "the text"))
        );
    }

    #[test]
    fn an_error_body_is_reported_by_its_identifiers_only() {
        use serde_json::json;
        let spoken = json!({"error": {"message": "Incorrect API key sk-abc",
                                      "code": "invalid_api_key",
                                      "type": "invalid_request_error"}});
        let fields = error_fields(&spoken);
        assert!(fields.contains("code=invalid_api_key"), "{fields}");
        assert!(!fields.contains("sk-abc"));
        assert_eq!(error_fields(&json!({"error": "plain prose"})), "");
    }

    #[test]
    fn endpoints_are_named_once_each() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("endpoints.json");
        std::fs::write(
            &path,
            r#"{"endpoints": [{"label": "local", "url": "http://127.0.0.1:11434/v1/chat/completions",
                                "model": "qwen", "price": {"input": 0, "output": 0}}]}"#,
        )
        .unwrap();
        let loaded = Endpoints::load(&path).unwrap();
        assert_eq!(loaded.labels(), ["local"]);
        assert_eq!(loaded.get("local").unwrap().price, Price::default());
        std::fs::write(
            &path,
            r#"{"endpoints": [{"label": "a", "url": "u", "model": "m"},
                              {"label": "a", "url": "v", "model": "n"}]}"#,
        )
        .unwrap();
        assert!(Endpoints::load(&path).unwrap_err().contains("labelled"));
    }
}

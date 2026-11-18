//! What carries a model's turns, holding its credential, and the usage log
//! every turn is written to.
//!
//! Whatever carries the turns ([`PlanTurns`]: the gateway for a Settings
//! provider, the trusted host's own client for an endpoint it was given)
//! holds the credential; its callers see requests and responses only.
//! Every turn is logged to `TILO_AGENT_USAGE_LOG` when set, with the
//! provider's token counts, why it stopped (a refusal included) and how many
//! pictures it carried, never their bytes.

use std::time::{Duration, Instant};

use tilo_model_gateway::host_backend::protocol::{
    Content, ErrorKind, ModelError, ModelRequest, ModelResponse, StopReason,
};

/// The most characters of a refusal's own words kept in a usage line.
const MOST_REFUSAL_CHARS: usize = 300;

/// What carries a model's turns, holding the credential.
pub trait PlanTurns: Send {
    /// Answer one turn's `request`. `key` names the turn, the same for
    /// every attempt at it, so a metered endpoint bills it once; `retry`
    /// says this is the turn asked again after its first call was lost to
    /// the network, for the usage log.
    fn turn(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
        retry: bool,
    ) -> Result<ModelResponse, ModelError>;

    /// The model's name, for the usage log.
    fn model(&self) -> &str;

    /// Whether the model is shown the pictures a tool answers with. One that
    /// is not reads that it was not shown them.
    fn sees(&self) -> bool {
        false
    }
}

/// The Settings provider's turns, through the per-user model gateway.
pub struct GatewayTurns {
    profile_id: String,
    model_id: String,
    sees: bool,
}

impl GatewayTurns {
    /// Turns over `selection`'s profile and model.
    #[must_use]
    pub fn new(selection: tilo_agent::providers::ProviderSelection) -> Self {
        Self {
            profile_id: selection.profile_id,
            sees: selection.model.vision,
            model_id: selection.model.id,
        }
    }
}

impl PlanTurns for GatewayTurns {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        deadline: Instant,
        retry: bool,
    ) -> Result<ModelResponse, ModelError> {
        if !tilo_model_gateway::available() {
            return Err(ModelError::new(ErrorKind::Unavailable));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(ModelError::new(ErrorKind::TimedOut));
        }
        let started = Instant::now();
        let answer = tilo_model_gateway::complete_streaming(
            &self.profile_id,
            &self.model_id,
            request.clone(),
            left,
            &mut |_| {},
        )
        .map_err(|error| error.model_error());
        record_usage(
            (&self.model_id, None),
            request,
            started.elapsed(),
            &answer,
            Attempt {
                hedge: false,
                retry,
            },
        );
        answer
    }

    fn model(&self) -> &str {
        &self.model_id
    }

    fn sees(&self) -> bool {
        self.sees
    }
}

/// Which call of a turn a usage line counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Attempt {
    /// The same request sent again while the first was slow.
    pub hedge: bool,
    /// The turn asked again after its first call was lost to the network.
    pub retry: bool,
}

/// Appends one JSON line per model call to `TILO_AGENT_USAGE_LOG`, when
/// set: which agent asked (its thread's name), how long it took, the
/// provider's token counts, why it stopped and a refusal's words, how many
/// pictures it carried and their encoded bytes (never the pictures), and
/// whether it was a hedge or a lost call asked again. Benchmarks price runs
/// from these lines.
pub(crate) fn record_usage(
    (model, tier): (&str, Option<&str>),
    request: &ModelRequest,
    elapsed: Duration,
    answer: &Result<ModelResponse, ModelError>,
    attempt: Attempt,
) {
    use std::io::Write as _;
    let Some(path) = std::env::var_os("TILO_AGENT_USAGE_LOG") else {
        return;
    };
    let agent = std::thread::current().name().map(str::to_owned);
    let mut line = usage_line(
        model,
        agent.as_deref(),
        (elapsed, pictures_in(request)),
        answer,
        attempt,
    );
    if let (Some(tier), Some(fields)) = (tier, line.as_object_mut()) {
        fields.insert("service_tier".into(), tier.into());
    }
    // One write per line: `writeln!` writes in pieces, and agents in other
    // processes append to the same log at once.
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()));
    if let Err(error) = written {
        tracing::warn!(%error, "cannot record model usage");
    }
}

/// How many pictures `request` carries, and their bytes as sent (encoded):
/// for the usage log, which never holds the pictures.
fn pictures_in(request: &ModelRequest) -> (usize, usize) {
    request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            Content::Image { base64, .. } => Some(base64.len()),
            _ => None,
        })
        .fold((0_usize, 0_usize), |(count, bytes), size| {
            (count.saturating_add(1), bytes.saturating_add(size))
        })
}

/// The refusal a response stands for, in its own words (empty when it gave
/// none): a refusal often arrives with no content at all, which read as an
/// empty answer would hide why the model gave none.
fn refusal(response: &ModelResponse) -> Option<String> {
    (response.stop_reason == StopReason::Refusal).then(|| {
        response
            .message
            .content
            .iter()
            .filter_map(|block| match block {
                Content::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>()
            .trim()
            .chars()
            .take(MOST_REFUSAL_CHARS)
            .collect()
    })
}

/// A call's token counts as the usage log keeps them, in the Responses
/// API's shape: every input token, of which `input_tokens_details` read
/// from and wrote to the provider's cache, and every output token, of which
/// `output_tokens_details` reasoned.
fn usage_counts(usage: &tilo_model_gateway::host_backend::protocol::Usage) -> serde_json::Value {
    serde_json::json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "total_tokens": usage.total_tokens,
        "input_tokens_details": {
            "cached_tokens": usage.cached_input_tokens,
            "cache_write_tokens": usage.cache_write_tokens,
        },
        "output_tokens_details": {"reasoning_tokens": usage.reasoning_tokens},
    })
}

/// The line [`record_usage`] appends for one model call.
fn usage_line(
    model: &str,
    agent: Option<&str>,
    (elapsed, (pictures, picture_bytes)): (Duration, (usize, usize)),
    answer: &Result<ModelResponse, ModelError>,
    attempt: Attempt,
) -> serde_json::Value {
    let response = answer.as_ref().ok();
    serde_json::json!({
        "at_unix_ms": tilo_clock::unix_now_ms(),
        "agent": agent,
        "model": model,
        "elapsed_ms": elapsed.as_millis(),
        "ok": answer.is_ok(),
        "error": answer.as_ref().err().map(ToString::to_string),
        "usage": response.map(|response| usage_counts(&response.usage)),
        // Why the model stopped (a refusal, a cut-off), so an empty answer
        // is told from a refused one.
        "stop_reason": response.map(|response| response.stop_reason),
        "refusal": response.and_then(refusal),
        "pictures": pictures,
        "picture_bytes": picture_bytes,
        "hedge": attempt.hedge,
        "retry": attempt.retry,
    })
}

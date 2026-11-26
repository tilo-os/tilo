//! One request to a provider. The wire is [`agent_provider_config::wire`]'s,
//! shared with every other Tilo process that asks a model; this module owns
//! the trust boundary around it: the gateway's own HTTP client (no ambient
//! proxies, redirects or automatic retries), a bounded read of every answer,
//! one deadline, asking again only what the provider provably did not take,
//! and no provider text in any error or log line.
use std::time::{Duration, Instant};

use agent_provider_config::ProviderProfile;
use agent_provider_config::protocol::{self as neutral, ModelError};
use agent_provider_config::transform as provider_transform;
use zeroize::{Zeroize as _, Zeroizing};

#[cfg(test)]
use super::provider_client;
use super::{
    Again, DeltaCoalescer, Emit, MAX_STREAM_BYTES, PlanCall, PlanTiming, ProviderFailure, Signs,
    StreamAssembly, StreamCall, classified_error_advice, emit_complete, error_body, log_token,
    plan_send, road, stream_failure,
};
use crate::{MAX_DELTA_EVENTS, MAX_STREAM_EVENTS, Response};
use agent_provider_config::wire::{self, Delta, Stream, StreamError};

#[cfg(test)]
#[path = "catalogue_call_tests.rs"]
mod catalogue_tests;
#[cfg(test)]
#[path = "google_call_tests.rs"]
mod google_tests;
#[cfg(test)]
#[path = "provider_conformance_tests.rs"]
mod provider_conformance_tests;
#[cfg(test)]
#[path = "call_stream_tests.rs"]
pub(super) mod stream_tests;
#[cfg(test)]
#[path = "subscription_call_tests.rs"]
mod subscription_tests;
#[cfg(test)]
#[path = "call_tests.rs"]
mod tests;

/// What a request that got no response means. A connection that could
/// not be made (no address, refused, its TLS handshake failed) and a
/// request the client handed back unwritten ([`never_sent`]) provably
/// reached no provider: those are asked again. A connection lost after
/// the request was written may have been taken, so its outcome is unknown.
pub(super) fn send_failure(error: &reqwest::Error) -> ProviderFailure {
    let unsent = error.is_connect() || never_sent(error);
    ProviderFailure {
        model_error: crate::openai::Failure::transport(error.is_timeout(), unsent).model_error(),
        why: if error.is_timeout() {
            "provider request timed out"
        } else {
            "provider transport failed"
        }
        .into(),
        again: if error.is_timeout() || unsent {
            Again::Soon(None)
        } else {
            Again::Never
        },
        outcome_unknown: !error.is_builder() && !unsent,
        unprocessed: unsent,
        status: None,
    }
}

/// Whether the HTTP client gave the request back before writing any of it:
/// hyper cancels a request whose connection closed before it was sent. It
/// sends one again by itself only on a reused connection, so on a new one
/// the cancellation reaches here.
fn never_sent(error: &reqwest::Error) -> bool {
    hyper_cause(error).is_some_and(hyper::Error::is_canceled)
}

/// The hyper error under a reqwest one, if any.
fn hyper_cause(error: &reqwest::Error) -> Option<&hyper::Error> {
    let mut cause = std::error::Error::source(error);
    while let Some(error) = cause {
        if let Some(hyper) = error.downcast_ref::<hyper::Error>() {
            return Some(hyper);
        }
        cause = error.source();
    }
    None
}

/// A word for why a request got no response, for the log: whether its
/// connection failed, or was lost before or after the request went out.
fn send_cause(error: &reqwest::Error) -> &'static str {
    let reset = || {
        let mut cause = std::error::Error::source(error);
        while let Some(error) = cause {
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::ConnectionReset)
            {
                return true;
            }
            cause = error.source();
        }
        false
    };
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if never_sent(error) {
        "not_sent"
    } else if hyper_cause(error).is_some_and(hyper::Error::is_incomplete_message) {
        "closed_before_response"
    } else if reset() {
        "reset"
    } else {
        "other"
    }
}

/// A request that could not be written: nothing was sent.
fn unsendable() -> ProviderFailure {
    let mut failure = stream_failure("provider transport failed");
    failure.model_error = ModelError::new(neutral::ErrorKind::InvalidRequest);
    failure.outcome_unknown = false;
    failure
}

/// Where one request goes and how.
pub(super) struct Post<'a> {
    pub(super) client: &'a reqwest::Client,
    pub(super) profile: &'a ProviderProfile,
    pub(super) url: String,
    pub(super) secret: Option<&'a str>,
    /// The model named, for the log only.
    pub(super) model: &'a str,
    pub(super) budget: Duration,
    /// Ask again while the provider provably did not take it.
    pub(super) resend: bool,
    /// Where the moments the provider's headers and its first bytes came
    /// are noted, and how long it may show nothing before the call is let
    /// go. None where nobody reads them.
    pub(super) signs: Option<&'a Signs>,
}

impl Post<'_> {
    /// What `waiting` on the provider gives, or the call's failure where the
    /// provider showed nothing for as long as it may ([`Signs::or_silence`]).
    async fn or_silence<T>(
        &self,
        waiting: impl std::future::Future<Output = T>,
        what: &str,
    ) -> Result<T, ProviderFailure> {
        match self.signs {
            Some(signs) => signs.or_silence(waiting, what).await,
            None => Ok(waiting.await),
        }
    }

    /// The next piece of `response`'s body, noted as a sign of life.
    async fn chunk(
        &self,
        response: &mut reqwest::Response,
    ) -> Result<Option<bytes::Bytes>, ProviderFailure> {
        let chunk = self
            .or_silence(response.chunk(), "no byte")
            .await?
            .map_err(|_| lost())?;
        if chunk.is_some()
            && let Some(signs) = self.signs
        {
            signs.event_came();
        }
        Ok(chunk)
    }

    /// `asked`, the request with what its provider is asked beside it
    /// ([`road`]), or `plain` where nothing is added or the provider refused
    /// that before. A provider that refuses what was added is asked once
    /// more without it, and is not sent it again.
    async fn send_beside(
        &self,
        plain: &serde_json::Value,
        asked: &serde_json::Value,
    ) -> Result<reqwest::Response, ProviderFailure> {
        let beside = road::added(plain, asked);
        if beside.is_empty() || road::refused(self.profile, self.model) {
            return self.send(plain).await;
        }
        let mut sent = self.send(asked).await;
        if let Err(refusal) = sent.as_ref()
            && road::refuses(refusal)
        {
            // Content-free: the status and the fields' names, never the
            // provider's words.
            eprintln!(
                "tilo-model-gateway: provider refused what the gateway asks beside the request \
                 (status={}) model={} fields={}; asking once more without it",
                refusal
                    .status
                    .map_or_else(|| "-".to_string(), |status| status.to_string()),
                log_token(self.model),
                beside.join(",")
            );
            sent = self.send(plain).await;
            // A request refused either way was not refused for what was
            // added.
            if !sent.as_ref().is_err_and(road::refuses) {
                road::remember(self.profile, self.model);
            }
        }
        sent
    }

    /// The response to `body` once the provider accepted it: its status a
    /// success, the rest of it still to read.
    async fn send(&self, body: &serde_json::Value) -> Result<reqwest::Response, ProviderFailure> {
        let mut headers = reqwest::header::HeaderMap::new();
        for header in provider_transform::profile_headers(self.profile, self.secret)
            .map_err(|_| unsendable())?
        {
            let mut value = reqwest::header::HeaderValue::from_str(header.value.as_str())
                .map_err(|_| unsendable())?;
            value.set_sensitive(header.secret);
            let name = reqwest::header::HeaderName::from_bytes(header.name.as_bytes())
                .map_err(|_| unsendable())?;
            headers.insert(name, value);
        }
        if !self.resend {
            return self.once(&headers, body, self.budget).await;
        }
        let started = Instant::now();
        plan_send(
            || self.budget.checked_sub(started.elapsed()),
            tokio::time::sleep,
            &|| false, // No output can precede the HTTP response headers.
            |budget| self.once(&headers, body, budget),
        )
        .await
    }

    async fn once(
        &self,
        headers: &reqwest::header::HeaderMap,
        body: &serde_json::Value,
        budget: Duration,
    ) -> Result<reqwest::Response, ProviderFailure> {
        let outgoing = self
            .client
            .post(&self.url)
            .timeout(budget)
            .headers(headers.clone())
            .json(body)
            .send();
        let response = self
            .or_silence(outgoing, "no response headers")
            .await?
            .map_err(|error| {
                // Content-free: what failed, never the URL or the error's text.
                eprintln!(
                    "tilo-model-gateway: provider transport failed model={} cause={}",
                    log_token(self.model),
                    send_cause(&error)
                );
                send_failure(&error)
            })?;
        if let Some(signs) = self.signs {
            signs.headers_came();
        }
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let retry_header = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let detail = error_body(response).await;
        // The provider's own words may quote the key: only its error's
        // identifiers are logged, and only when they look like one.
        eprintln!(
            "tilo-model-gateway: provider refused model={} status={}{}",
            log_token(self.model),
            status.as_u16(),
            crate::openai::error_fields(&detail)
        );
        let classified = crate::openai::Failure::from_http(
            status.as_u16(),
            &String::from_utf8_lossy(&detail),
            retry_header.as_deref(),
        );
        let again = if classified.retryable() {
            Again::Soon(classified.retry_after)
        } else {
            Again::Never
        };
        let retry_after = match again {
            Again::Soon(Some(wait)) if !wait.is_zero() && wait <= Duration::from_secs(3600) => {
                format!(" retry_after_s={}", wait.as_secs_f32().ceil())
            }
            _ => String::new(),
        };
        let unprocessed = classified.refused_unprocessed(status.as_u16());
        Err(ProviderFailure {
            model_error: classified.model_error(),
            why: format!(
                "provider returned HTTP {}{}{retry_after}",
                status.as_u16(),
                classified_error_advice(&classified)
            ),
            again,
            outcome_unknown: !unprocessed,
            unprocessed,
            status: Some(status.as_u16()),
        })
    }
}

/// `body` posted as `post` says, and the whole of the answer's text, read
/// to at most [`MAX_STREAM_BYTES`]: a one-shot ask whose body its caller
/// wrote (a local model's photo index).
pub(super) async fn post(
    post: Post<'_>,
    body: &serde_json::Value,
) -> Result<String, ProviderFailure> {
    let read = async {
        let mut response = post.send(body).await?;
        let mut text = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| lost())? {
            if text.len().saturating_add(chunk.len()) > MAX_STREAM_BYTES {
                return Err(stream_failure("provider response exceeds limit"));
            }
            text.extend_from_slice(&chunk);
        }
        String::from_utf8(text).map_err(|_| stream_failure("provider response was not UTF-8"))
    };
    tokio::time::timeout(post.budget, read)
        .await
        .unwrap_or_else(|_| Err(timed_out()))
}

fn lost() -> ProviderFailure {
    let mut failure = stream_failure(
        "provider network connection failed; check connectivity; partial output was retained",
    );
    failure.model_error = ModelError::new(neutral::ErrorKind::Transport);
    failure
}

fn timed_out() -> ProviderFailure {
    let mut failure = stream_failure(
        "provider request timed out; partial output was retained; try a shorter request",
    );
    failure.model_error = ModelError::new(neutral::ErrorKind::TimedOut);
    failure
}

/// What a stream that gave no answer means for the caller.
fn stream_error(error: StreamError) -> ProviderFailure {
    match error {
        StreamError::Provider(body) => {
            let classified = crate::openai::Failure::from_http(0, &body, None);
            let mut failure = stream_failure(&format!(
                "provider stream failed{}",
                classified_error_advice(&classified)
            ));
            failure.model_error = classified.model_error();
            failure
        }
        StreamError::Truncated | StreamError::TooLarge => {
            let mut failure = stream_failure(
                "provider stream was truncated; partial output was retained; no tools were authorized",
            );
            failure.model_error = ModelError::new(neutral::ErrorKind::Truncated);
            failure
        }
        StreamError::Malformed(found) => stream_failure(&format!(
            "provider response was malformed or unsupported; partial output was retained; no \
             tools were authorized ({found})"
        )),
    }
}

/// One neutral turn asked of `profile`'s `model`: written on its wire, sent,
/// and read back whole or as a stream. A stream's presentation goes to
/// `streaming`'s emitter as it arrives; its calls are emitted as complete
/// only once the whole turn validated.
pub(super) async fn turn(
    post: &Post<'_>,
    request: &neutral::ModelRequest,
    extra: Option<&serde_json::Map<String, serde_json::Value>>,
    target: crate::openai::Target,
    streaming: Option<(Emit<'_>, &mut PlanTiming, bool)>,
) -> Result<neutral::ModelResponse, ProviderFailure> {
    let api = wire::Api::of(post.profile);
    let streams = streaming.is_some() || api.always_streams();
    let body = wire::body(
        post.profile,
        post.model,
        request,
        wire::Ask {
            streaming: streams,
            extra,
        },
    )
    .map_err(|error| {
        let mut failure = unsendable();
        failure.why = format!("the request cannot be sent: {}", error.message);
        failure.model_error = error;
        failure
    })?;
    let body = target.shape(body);
    // What this provider is asked beside the request, written last so the
    // profile's omitted fields and the caller's own still decide.
    let mut asked = body.clone();
    if let Some(object) = asked.as_object_mut() {
        road::beside(post.profile, post.model, object);
    }
    let read = async {
        let mut response = post.send_beside(&body, &asked).await?;
        if !streams {
            let mut text = Vec::new();
            while let Some(chunk) = post.chunk(&mut response).await? {
                if text.len().saturating_add(chunk.len()) > MAX_STREAM_BYTES {
                    return Err(stream_failure("provider response exceeds limit"));
                }
                text.extend_from_slice(&chunk);
            }
            let text = String::from_utf8(text)
                .map_err(|_| stream_failure("provider response was not UTF-8"))?;
            return wire::response(post.profile, request, &text)
                .map_err(|error| stream_error(StreamError::Malformed(error.message)));
        }
        let mut stream = Stream::of(post.profile);
        let mut progress = StreamProgress::default();
        let mut deltas = Vec::new();
        let (mut emit, mut timing, wanted) = match streaming {
            Some((emit, timing, wanted)) => (Some(emit), Some(timing), wanted),
            None => (None, None, false),
        };
        if wanted {
            progress.coalescer = Some(DeltaCoalescer::default());
        }
        while let Some(chunk) = post.chunk(&mut response).await? {
            if let Some(timing) = timing.as_deref_mut() {
                timing.status = Some(200);
                timing
                    .first_byte
                    .get_or_insert_with(|| timing.started.elapsed());
            }
            // What the chunk completed is shown even when the rest of it
            // fails the turn: presentation keeps what arrived.
            let pushed = stream.push(&chunk, &mut deltas);
            if let (Some(emit), Some(timing)) = (emit.as_deref_mut(), timing.as_deref_mut()) {
                for delta in std::mem::take(&mut deltas) {
                    progress.delta(delta, timing, emit)?;
                }
            }
            deltas.clear();
            pushed.map_err(stream_error)?;
        }
        let answer = stream.finish(request, &mut deltas).map_err(stream_error)?;
        if let (Some(emit), Some(timing)) = (emit, timing) {
            for delta in deltas {
                progress.delta(delta, timing, emit)?;
            }
            progress.authorize(&answer, timing, emit)?;
        }
        Ok(answer)
    };
    tokio::time::timeout(post.budget, read)
        .await
        .unwrap_or_else(|_| Err(timed_out()))
}

/// What a stream has shown its caller so far.
#[derive(Default)]
struct StreamProgress {
    assembly: StreamAssembly,
    coalescer: Option<DeltaCoalescer>,
    completed: std::collections::BTreeSet<usize>,
    text_frames: usize,
}

impl StreamProgress {
    fn delta(
        &mut self,
        delta: Delta,
        timing: &mut PlanTiming,
        emit: Emit<'_>,
    ) -> Result<(), ProviderFailure> {
        match delta {
            Delta::CallOpened { index, id, name } => {
                timing
                    .first_token
                    .get_or_insert_with(|| timing.started.elapsed());
                if index != self.assembly.calls.len() || index >= MAX_STREAM_EVENTS {
                    return Err(stream_failure("invalid or excessive tool parts"));
                }
                self.assembly.calls.push(StreamCall {
                    id: id.unwrap_or_default(),
                    name,
                    ..Default::default()
                });
            }
            Delta::CallArguments { index, json } => {
                timing
                    .first_token
                    .get_or_insert_with(|| timing.started.elapsed());
                self.assembly
                    .calls
                    .get_mut(index)
                    .ok_or_else(|| stream_failure("arguments before tool start"))?
                    .arguments
                    .push_str(&json);
                if let Some(coalescer) = &mut self.coalescer {
                    coalescer.flush(&self.assembly.calls, index, Instant::now(), false, emit)?;
                }
            }
            Delta::CallClosed { index, id, name } => {
                let slot = self
                    .assembly
                    .calls
                    .get_mut(index)
                    .ok_or_else(|| stream_failure("tool end before start"))?;
                // Never authorize a silently renamed operation.
                if (!slot.name.is_empty() && slot.name != name) || !self.completed.insert(index) {
                    return Err(stream_failure("provider changed or repeated a tool"));
                }
                slot.id = id;
                slot.name = name;
                if let Some(coalescer) = &mut self.coalescer {
                    coalescer.flush(&self.assembly.calls, index, Instant::now(), true, emit)?;
                }
            }
            Delta::Text(text) | Delta::Reasoning(text) if text.is_empty() => {}
            Delta::Text(text) => {
                timing
                    .first_token
                    .get_or_insert_with(|| timing.started.elapsed());
                self.text(EVENT_TEXT_DELTA, text, emit)?;
            }
            Delta::Reasoning(text) => {
                self.assembly.reasoning_chunks = self.assembly.reasoning_chunks.saturating_add(1);
                self.assembly.reasoning_bytes = self
                    .assembly
                    .reasoning_bytes
                    .saturating_add(text.len() as u64);
                timing
                    .first_reasoning
                    .get_or_insert_with(|| timing.started.elapsed());
                self.text(EVENT_REASONING_DELTA, text, emit)?;
            }
        }
        timing.observe_stream(
            self.assembly.reasoning_chunks,
            self.assembly.reasoning_bytes,
        );
        Ok(())
    }

    /// A presentation frame of words. Presentation must not silently lose
    /// bytes while the turn is reported as complete: past the cap the turn
    /// fails, keeping what was already shown.
    fn text(&mut self, kind: &str, text: String, emit: Emit<'_>) -> Result<(), ProviderFailure> {
        let frames = self
            .coalescer
            .as_ref()
            .map_or(self.text_frames, |coalescer| coalescer.frames);
        if frames >= MAX_DELTA_EVENTS {
            return Err(stream_error(StreamError::Truncated));
        }
        self.text_frames = self.text_frames.saturating_add(1);
        if let Some(coalescer) = &mut self.coalescer {
            coalescer.frames = coalescer.frames.saturating_add(1);
        }
        let event = Response {
            ok: true,
            output: Some(serde_json::Value::String(text).to_string()),
            configured: None,
            error: None,
            model_error: None,
            event: Some(kind.into()),
        };
        if emit(&event) {
            Ok(())
        } else {
            Err(stream_failure(
                "the agent left before the provider finished",
            ))
        }
    }

    /// Emit the turn's calls as complete, now that the whole turn validated:
    /// every call the stream opened closed, in order.
    fn authorize(
        &mut self,
        answer: &neutral::ModelResponse,
        timing: &mut PlanTiming,
        emit: Emit<'_>,
    ) -> Result<(), ProviderFailure> {
        timing.observe_answer(answer);
        if !matches!(
            answer.stop_reason,
            neutral::StopReason::EndTurn | neutral::StopReason::ToolCalls
        ) {
            return Ok(());
        }
        if self.completed.len() != self.assembly.calls.len() {
            return Err(stream_error(StreamError::Truncated));
        }
        emit_complete(
            &self.assembly.calls,
            0,
            self.assembly.calls.len(),
            emit,
            timing,
            self.coalescer.as_mut(),
        )
        .map(|_| ())
    }
}

/// The frame kinds a stream's words go out as.
const EVENT_TEXT_DELTA: &str = "text_delta";
const EVENT_REASONING_DELTA: &str = "reasoning_delta";

/// How a turn is carried besides what its call names.
#[derive(Clone, Copy, Default)]
pub(super) struct Carry {
    /// Ask again a request the provider provably did not take (it was busy,
    /// or no connection to it could be made), waiting what it asks.
    pub(super) resend: bool,
    /// Hold the turn to its caller's own deadline alone, not also to what a
    /// person's device waits for a plan.
    pub(super) callers_deadline: bool,
}

/// `request` asked of `profile` with `secret`, as `call` says: the one path
/// every neutral turn takes once its profile and key are known, whether the
/// gateway read them from its person's settings or a trusted host handed it
/// a person's own. The answer is priced here, and the call's content-free
/// timing line is written here.
pub(super) async fn complete_with(
    client: &reqwest::Client,
    call: &PlanCall<'_>,
    (profile, mut secret): (&ProviderProfile, Option<Zeroizing<String>>),
    request: &neutral::ModelRequest,
    emit: Emit<'_>,
    (mut timing, carry): (PlanTiming, Carry),
) -> Result<neutral::ModelResponse, ModelError> {
    timing.stream = call.stream;
    let budget = if carry.callers_deadline {
        timing.timeout_ms = Some(call.timeout.as_millis());
        call.timeout
            .checked_sub(timing.started.elapsed())
            .filter(|budget| !budget.is_zero())
            .ok_or_else(|| ModelError::new(neutral::ErrorKind::TimedOut))?
    } else {
        timing
            .budget(call, profile, Some(u64::from(request.max_output_tokens)))
            .map_err(|_| ModelError::new(neutral::ErrorKind::TimedOut))?
    };
    let signs = std::sync::Arc::clone(&timing.signs);
    let result = turn(
        &Post {
            client,
            profile,
            url: wire::url(profile),
            secret: secret.as_ref().map(|secret| secret.as_str()),
            model: call.model_id,
            budget,
            resend: carry.resend,
            signs: Some(&signs),
        },
        request,
        None,
        crate::openai::Target::Standard,
        call.stream.then_some((emit, &mut timing, true)),
    )
    .await;
    secret.zeroize();
    // Why the answer was not used, for the timing line: the person reads
    // the error's kind, whoever debugs the run reads this.
    match result {
        Ok(mut answer) => {
            timing.status = Some(200);
            timing.observe_answer(&answer);
            // What this answer costs, priced here so every caller spends
            // against one figure: the gateway's, at its price table's list
            // prices.
            answer.cost_micros = crate::pricing::cost_micros(call.model_id, &answer.usage);
            timing.log(call, Ok(()));
            Ok(answer)
        }
        Err(failure) => {
            timing.status = timing.status.or(failure.status);
            timing.log(call, Err(&failure.why));
            Err(failure.model_error)
        }
    }
}

/// `request` asked of `profile`'s `model` with `secret`, its answer read
/// whole.
#[cfg(test)]
pub(super) async fn host_complete(
    client: &reqwest::Client,
    profile: &ProviderProfile,
    secret: Option<&str>,
    model: &str,
    request: neutral::ModelRequest,
    budget: Duration,
    target: crate::openai::Target,
) -> Result<neutral::ModelResponse, ProviderFailure> {
    turn(
        &Post {
            client,
            profile,
            url: wire::url(profile),
            secret,
            model,
            budget,
            resend: false,
            signs: None,
        },
        &request,
        None,
        target,
        None,
    )
    .await
}

/// `request` asked of `profile`'s `checked-model` with `secret`, its answer
/// streamed to `emit` (argument previews too when `previews`).
#[cfg(test)]
pub(super) async fn streamed(
    profile: &ProviderProfile,
    secret: Option<&str>,
    request: &neutral::ModelRequest,
    emit: Emit<'_>,
    timing: &mut PlanTiming,
    previews: bool,
) -> Result<neutral::ModelResponse, ProviderFailure> {
    let signs = std::sync::Arc::clone(&timing.signs);
    turn(
        &Post {
            client: &provider_client().unwrap(),
            profile,
            url: wire::url(profile),
            secret,
            model: "checked-model",
            budget: tilo_test_env::hang_bound(),
            resend: false,
            signs: Some(&signs),
        },
        request,
        None,
        crate::openai::Target::Standard,
        Some((emit, timing, previews)),
    )
    .await
}

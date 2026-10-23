//! Asking the engines at once and merging what they answer.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use serde::Serialize;

use crate::engines::google::{self, Token};
use crate::engines::{Engine, EngineError, EngineRequest, EngineResult};
use crate::merge::{SearchResult, merge};

/// Bounds on one search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchLimits {
    /// The longest one engine may take, from request to parsed page.
    pub engine_timeout: Duration,
    /// The most bytes of one engine's page read; the rest is ignored, since
    /// results come first.
    pub max_page_bytes: usize,
    /// The most redirects one engine request follows.
    pub max_redirects: u8,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            engine_timeout: Duration::from_secs(8),
            max_page_bytes: 3 * 1024 * 1024,
            max_redirects: 3,
        }
    }
}

/// The longest query sent, in characters.
pub const MAX_QUERY_CHARS: usize = 400;

/// How one engine fared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineStatus {
    /// Answered, with this many results.
    Ok,
    /// Showed a challenge instead of results.
    Blocked,
    /// Said it is getting too many requests, now or recently, so it was
    /// left alone; it is asked again once its backoff ends.
    RateLimited,
    /// Could not be reached, timed out, or answered something unreadable.
    Failed,
    /// Does not take this kind of query.
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EngineReport {
    pub engine: Engine,
    pub status: EngineStatus,
    pub results: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A finished search: merged results, best first, and how each engine fared.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub engines: Vec<EngineReport>,
}

/// Why a search produced nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchError {
    /// The query is empty or only whitespace.
    EmptyQuery,
    /// The query is longer than [`MAX_QUERY_CHARS`].
    QueryTooLong,
    /// No engine was asked: none named, or none takes this query.
    NoEngines,
    /// Every engine asked failed or was blocked; each one's report says why.
    AllFailed(Vec<EngineReport>),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyQuery => f.write_str("the query is empty"),
            Self::QueryTooLong => {
                write!(f, "the query is longer than {MAX_QUERY_CHARS} characters")
            }
            Self::NoEngines => f.write_str("none of the named engines takes this query"),
            Self::AllFailed(reports) => {
                f.write_str("no search engine answered: ")?;
                for (i, report) in reports.iter().enumerate() {
                    if i > 0 {
                        f.write_str("; ")?;
                    }
                    write!(
                        f,
                        "{} {}",
                        report.engine,
                        report.detail.as_deref().unwrap_or("failed")
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for SearchError {}

/// How long an engine is left alone after its first rate limit. Each further
/// one in a row doubles it, up to [`MAX_BACKOFF`].
pub const FIRST_BACKOFF: Duration = Duration::from_secs(5 * 60);
/// The longest an engine is left alone.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60 * 60);
/// How long a Google token is used before a fresh one is fetched. Tokens
/// last longer, and a refused one is dropped at once anyway.
const TOKEN_LIFETIME: Duration = Duration::from_secs(60 * 60);
/// The most bytes of Google's loader script read for its token.
const MAX_TOKEN_SCRIPT_BYTES: usize = 256 * 1024;

/// What searches remember from one to the next: which engines are backing
/// off, and Google's current token. Kept in memory only, never written to
/// disk. Share one between searches; a fresh one forgets both.
#[derive(Debug, Default)]
pub struct SearchState {
    backoff: Mutex<HashMap<Engine, Backoff>>,
    google_token: Mutex<Option<(Token, Instant)>>,
}

#[derive(Clone, Copy, Debug)]
struct Backoff {
    until: Instant,
    /// How long the next rate limit in a row backs off for.
    next: Duration,
}

impl SearchState {
    /// How much longer `engine` is left alone, if it is backing off at `now`.
    pub fn backing_off(&self, engine: Engine, now: Instant) -> Option<Duration> {
        let backoff = self.backoff.lock().unwrap_or_else(PoisonError::into_inner);
        backoff
            .get(&engine)
            .map(|b| b.until.saturating_duration_since(now))
            .filter(|left| !left.is_zero())
    }

    /// Record that `engine` was rate-limited at `now`, and return how long it
    /// is now left alone.
    pub fn rate_limited(&self, engine: Engine, now: Instant) -> Duration {
        let mut backoff = self.backoff.lock().unwrap_or_else(PoisonError::into_inner);
        let wait = backoff.get(&engine).map_or(FIRST_BACKOFF, |b| b.next);
        backoff.insert(
            engine,
            Backoff {
                until: now.checked_add(wait).unwrap_or(now),
                next: wait.saturating_mul(2).min(MAX_BACKOFF),
            },
        );
        wait
    }

    /// Record that `engine` answered: its next rate limit starts the backoff
    /// over.
    pub fn answered(&self, engine: Engine) {
        self.backoff
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&engine);
    }

    fn fresh_google_token(&self, now: Instant) -> Option<Token> {
        let token = self
            .google_token
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        token
            .as_ref()
            .filter(|(_, fetched)| now.saturating_duration_since(*fetched) < TOKEN_LIFETIME)
            .map(|(token, _)| token.clone())
    }

    fn keep_google_token(&self, token: Option<(Token, Instant)>) {
        *self
            .google_token
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = token;
    }
}

/// Ask `engines` for `query` at once and merge their results, best first.
///
/// An engine that fails, times out or shows a challenge only loses its own
/// results; its report says what happened. An engine that says it is getting
/// too many requests is left alone for a while, recorded in `state`, and
/// reported as rate-limited without being asked until then.
///
/// # Errors
///
/// [`SearchError::EmptyQuery`], [`SearchError::QueryTooLong`],
/// [`SearchError::NoEngines`], and [`SearchError::AllFailed`] when not one
/// engine answered.
pub async fn search(
    client: &tilo_net::Client,
    state: &SearchState,
    query: &str,
    engines: &[Engine],
    limits: &SearchLimits,
) -> Result<SearchResponse, SearchError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(SearchError::EmptyQuery);
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(SearchError::QueryTooLong);
    }
    let mut asked = Vec::new();
    let mut reports: Vec<EngineReport> = Vec::new();
    let now = Instant::now();
    for &engine in engines {
        if asked.iter().any(|(e, _)| *e == engine) || reports.iter().any(|r| r.engine == engine) {
            continue;
        }
        if let Some(left) = state.backing_off(engine, now) {
            reports.push(EngineReport {
                engine,
                status: EngineStatus::RateLimited,
                results: 0,
                detail: Some(format!(
                    "rate-limited; asked again in {} s",
                    left.as_secs().max(1)
                )),
            });
            continue;
        }
        match engine.request(query) {
            Some(request) => asked.push((engine, request)),
            None => reports.push(EngineReport {
                engine,
                status: EngineStatus::Skipped,
                results: 0,
                detail: Some("does not take this kind of query".into()),
            }),
        }
    }
    if asked.is_empty() {
        if reports
            .iter()
            .any(|r| r.status == EngineStatus::RateLimited)
        {
            return Err(SearchError::AllFailed(reports));
        }
        return Err(SearchError::NoEngines);
    }
    let answers = join_all(asked.into_iter().map(|(engine, request)| async move {
        let answer = tokio::time::timeout(
            limits.engine_timeout,
            ask(client, state, engine, request, limits),
        )
        .await
        .unwrap_or_else(|_| Err(Failure::Failed("timed out".into())));
        (engine, answer)
    }))
    .await;
    let mut answered = Vec::new();
    for (engine, answer) in answers {
        match &answer {
            Ok(_) => state.answered(engine),
            Err(Failure::RateLimited(reason)) => {
                let wait = state.rate_limited(engine, Instant::now());
                tracing::warn!(%engine, reason, backoff_s = wait.as_secs(), "engine is rate-limiting");
            }
            Err(_) => {}
        }
        reports.push(report(engine, &answer));
        if let Ok(results) = answer {
            answered.push((engine, results));
        }
    }
    reports.sort_by_key(|report| Engine::ALL.iter().position(|e| *e == report.engine));
    if answered.is_empty() {
        tracing::warn!(?reports, "no search engine answered");
        return Err(SearchError::AllFailed(reports));
    }
    // Merge in the engines' stable order, so equal scores rank the same way
    // whichever answered first.
    answered.sort_by_key(|(engine, _)| Engine::ALL.iter().position(|e| e == engine));
    Ok(SearchResponse {
        results: merge(&answered),
        engines: reports,
    })
}

/// What `answer` says about how `engine` fared.
fn report(engine: Engine, answer: &Result<Vec<EngineResult>, Failure>) -> EngineReport {
    match answer {
        Ok(results) => EngineReport {
            engine,
            status: EngineStatus::Ok,
            results: results.len(),
            detail: None,
        },
        Err(Failure::RateLimited(reason)) => EngineReport {
            engine,
            status: EngineStatus::RateLimited,
            results: 0,
            detail: Some(reason.clone()),
        },
        Err(Failure::Blocked(reason)) => EngineReport {
            engine,
            status: EngineStatus::Blocked,
            results: 0,
            detail: Some(reason.clone()),
        },
        Err(Failure::Failed(reason)) => EngineReport {
            engine,
            status: EngineStatus::Failed,
            results: 0,
            detail: Some(reason.clone()),
        },
    }
}

enum Failure {
    Blocked(String),
    RateLimited(String),
    Failed(String),
}

impl From<EngineError> for Failure {
    fn from(error: EngineError) -> Self {
        match error {
            EngineError::Blocked(reason) => Self::Blocked(reason),
            EngineError::RateLimited(reason) => Self::RateLimited(reason),
            EngineError::Refused(reason) | EngineError::Layout(reason) => Self::Failed(reason),
        }
    }
}

async fn ask(
    client: &tilo_net::Client,
    state: &SearchState,
    engine: Engine,
    mut request: EngineRequest,
    limits: &SearchLimits,
) -> Result<Vec<EngineResult>, Failure> {
    if engine != Engine::Google {
        return send(client, engine, request, limits).await;
    }
    let token = if let Some(token) = state.fresh_google_token(Instant::now()) {
        token
    } else {
        let token = google_token(client, limits).await?;
        state.keep_google_token(Some((token.clone(), Instant::now())));
        token
    };
    google::authorize(&mut request.url, &token);
    let answer = send(client, engine, request, limits).await;
    if answer.is_err() {
        // The token may be what was refused; the next search fetches another.
        state.keep_google_token(None);
    }
    answer
}

/// A fresh token from Google's search element loader.
async fn google_token(client: &tilo_net::Client, limits: &SearchLimits) -> Result<Token, Failure> {
    let request =
        google::token_request().ok_or_else(|| Failure::Failed("no token address".into()))?;
    let mut builder = client.get(request.url.as_str());
    for (name, value) in &request.headers {
        builder = builder.header(*name, value.as_str());
    }
    let response = builder
        .send()
        .await
        .map_err(|e| Failure::Failed(e.to_string()))?;
    let status = response.status();
    if status == tilo_net::StatusCode::TOO_MANY_REQUESTS {
        return Err(Failure::RateLimited("Google: HTTP 429".into()));
    }
    let script = read_capped(response, MAX_TOKEN_SCRIPT_BYTES.min(limits.max_page_bytes))
        .await
        .map_err(|e| Failure::Failed(e.to_string()))?;
    google::parse_token(&script).ok_or_else(|| {
        Failure::Failed(format!(
            "Google's search element handed out no token (HTTP {})",
            status.as_u16()
        ))
    })
}

/// Send one engine request, following its redirects, and read the answer.
async fn send(
    client: &tilo_net::Client,
    engine: Engine,
    request: EngineRequest,
    limits: &SearchLimits,
) -> Result<Vec<EngineResult>, Failure> {
    let mut url = request.url;
    let mut hops = 0u8;
    let response = loop {
        // A redirect after a form is followed with a plain `GET`, as a
        // browser does.
        let mut builder = match request.form.as_deref() {
            Some(fields) if hops == 0 => client.post(url.as_str()).form(fields),
            _ => client.get(url.as_str()),
        }
        .header("accept", "text/html,application/xhtml+xml;q=0.9,*/*;q=0.5");
        for (name, value) in &request.headers {
            builder = builder.header(*name, value.as_str());
        }
        let response = builder
            .send()
            .await
            .map_err(|e| Failure::Failed(e.to_string()))?;
        if !response.status().is_redirection() {
            break response;
        }
        let next = response
            .headers()
            .get(tilo_net::header::LOCATION)
            .and_then(|location| location.to_str().ok())
            .and_then(|location| url.join(location).ok())
            .filter(|next| next.scheme() == "https")
            .ok_or_else(|| Failure::Failed("redirected without a usable address".into()))?;
        hops = hops.saturating_add(1);
        if hops > limits.max_redirects {
            return Err(Failure::Failed("redirected too many times".into()));
        }
        url = next;
    };
    let status = response.status();
    if status == tilo_net::StatusCode::TOO_MANY_REQUESTS {
        return Err(Failure::RateLimited(format!("{engine}: HTTP 429")));
    }
    // A challenge often comes with 403; read it to say which.
    let body = read_capped(response, limits.max_page_bytes)
        .await
        .map_err(|e| Failure::Failed(e.to_string()))?;
    match engine.parse(&body) {
        Ok(results) => Ok(results),
        Err(EngineError::Layout(_)) if !status.is_success() => {
            Err(Failure::Failed(format!("HTTP {}", status.as_u16())))
        }
        Err(error) => Err(error.into()),
    }
}

/// The body as text, at most `cap` bytes of it.
pub(crate) async fn read_capped(
    mut response: tilo_net::Response,
    cap: usize,
) -> Result<String, tilo_net::NetError> {
    let (bytes, _) = read_bytes_capped(&mut response, cap).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// At most `cap` bytes of the body, and whether there was more.
pub(crate) async fn read_bytes_capped(
    response: &mut tilo_net::Response,
    cap: usize,
) -> Result<(Vec<u8>, bool), tilo_net::NetError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let room = cap.saturating_sub(body.len());
        if chunk.len() > room {
            body.extend_from_slice(chunk.get(..room).unwrap_or_default());
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_limited_engine_backs_off_longer_each_time_until_it_answers() {
        let state = SearchState::default();
        let start = Instant::now();
        assert_eq!(state.backing_off(Engine::Google, start), None);
        assert_eq!(state.rate_limited(Engine::Google, start), FIRST_BACKOFF);
        assert_eq!(
            state.backing_off(Engine::Google, start),
            Some(FIRST_BACKOFF)
        );
        assert_eq!(state.backing_off(Engine::Bing, start), None);
        let later = start.checked_add(FIRST_BACKOFF).unwrap();
        assert_eq!(state.backing_off(Engine::Google, later), None);
        assert_eq!(
            state.rate_limited(Engine::Google, later),
            FIRST_BACKOFF.saturating_mul(2)
        );
        for _ in 0..10 {
            state.rate_limited(Engine::Google, later);
        }
        assert_eq!(state.rate_limited(Engine::Google, later), MAX_BACKOFF);
        state.answered(Engine::Google);
        assert_eq!(state.backing_off(Engine::Google, later), None);
        assert_eq!(state.rate_limited(Engine::Google, later), FIRST_BACKOFF);
    }

    #[test]
    fn engines_backing_off_are_reported_without_being_asked() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = crate::client().unwrap();
        let state = SearchState::default();
        state.rate_limited(Engine::Google, Instant::now());
        // No request is made: the only engine named is backing off.
        let error = runtime
            .block_on(search(
                &client,
                &state,
                "rust",
                &[Engine::Google, Engine::Google],
                &SearchLimits::default(),
            ))
            .unwrap_err();
        let SearchError::AllFailed(reports) = error else {
            panic!("{error:?}");
        };
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].status, EngineStatus::RateLimited);
        assert!(
            reports[0]
                .detail
                .as_deref()
                .unwrap()
                .contains("asked again in")
        );
    }
}

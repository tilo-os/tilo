//! Asking the engines at once and merging what they answer.

use std::fmt;
use std::time::Duration;

use futures_util::future::join_all;
use serde::Serialize;

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
    /// Could not be reached, timed out, or answered something unreadable.
    Failed,
    /// Does not take this kind of query.
    Skipped,
}

/// One engine's part in a search.
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

/// Ask `engines` for `query` at once and merge their results, best first.
///
/// An engine that fails, times out or shows a challenge only loses its own
/// results; its report says what happened.
///
/// # Errors
///
/// [`SearchError::EmptyQuery`], [`SearchError::QueryTooLong`],
/// [`SearchError::NoEngines`], and [`SearchError::AllFailed`] when not one
/// engine answered.
pub async fn search(
    client: &tilo_net::Client,
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
    let mut reports = Vec::new();
    for &engine in engines {
        if asked.iter().any(|(e, _)| *e == engine) {
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
        return Err(SearchError::NoEngines);
    }
    let answers = join_all(asked.into_iter().map(|(engine, request)| async move {
        let answer =
            tokio::time::timeout(limits.engine_timeout, ask(client, engine, request, limits))
                .await
                .unwrap_or_else(|_| Err(Failure::Failed("timed out".into())));
        (engine, answer)
    }))
    .await;
    let mut answered = Vec::new();
    for (engine, answer) in answers {
        reports.push(match &answer {
            Ok(results) => EngineReport {
                engine,
                status: EngineStatus::Ok,
                results: results.len(),
                detail: None,
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
        });
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

enum Failure {
    Blocked(String),
    Failed(String),
}

async fn ask(
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
    // A challenge often comes with 403 or 429; read it to say which.
    let body = read_capped(response, limits.max_page_bytes)
        .await
        .map_err(|e| Failure::Failed(e.to_string()))?;
    match engine.parse(&body) {
        Ok(results) => Ok(results),
        Err(EngineError::Blocked(reason)) => Err(Failure::Blocked(reason)),
        Err(EngineError::Layout(_)) if !status.is_success() => {
            Err(Failure::Failed(format!("HTTP {}", status.as_u16())))
        }
        Err(EngineError::Layout(reason)) => Err(Failure::Failed(reason)),
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

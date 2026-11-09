//! A person's runtime, as the door reaches it: one HTTP endpoint per
//! runtime serving every workspace it takes part in, `/w/<workspace>/mcp`
//! (`tilo-runtime`'s streamable HTTP MCP), and `/w/<workspace>/join`, which
//! has the runtime join a workspace by invitation.
//!
//! The bearer token says the caller is the hosting provider's door; it
//! reaches nothing by itself. Whatever opens something there (a session, a
//! join) is answered with a fresh challenge, which the grant's key signs for
//! that one workspace ([`tilo_user_identity::agent_endpoint_audience`]); the
//! runtime checks the certificate under the person's root, its scope and
//! the person's revocations, and binds the session to it.

use crate::grant_key::GrantKey;
use base64::Engine as _;
use serde_json::Value;
use std::time::Duration;

/// The header a runtime names its challenge in.
pub const CHALLENGE_HEADER: &str = "tilo-grant-challenge";
/// The header the grant's proof goes in: its canonical encoding, base64url.
pub const PROOF_HEADER: &str = "tilo-grant";
pub const SESSION_HEADER: &str = "mcp-session-id";
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The most a workspace endpoint's answer may be: its own requests are
/// bounded at 1 MiB, its answers carry pages and content.
const MAX_UPSTREAM_BYTES: usize = 32 * 1024 * 1024;

/// Where a person's runtime answers, and the token that proves the caller is
/// the door.
#[derive(Clone, PartialEq, Eq)]
pub struct Route {
    /// The runtime's endpoint, `http://<internal address>`, no trailing slash.
    pub base: String,
    pub token: String,
}

impl std::fmt::Debug for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Route")
            .field("base", &self.base)
            .field("token", &"[redacted]")
            .finish()
    }
}

impl Route {
    /// The MCP endpoint of `workspace`.
    #[must_use]
    pub fn mcp(&self, workspace: &str) -> String {
        format!("{}/w/{workspace}/mcp", self.base)
    }

    /// Where the runtime joins `workspace`.
    #[must_use]
    pub fn join(&self, workspace: &str) -> String {
        format!("{}/w/{workspace}/join", self.base)
    }
}

/// An answer: its status, headers and JSON body, if it had one.
pub type Answer = (u16, tilo_net::HeaderMap, Option<Value>);

/// POST `body` to `url`, in the session `session` when there is one. When
/// the runtime asks for a proof (`401` with a challenge) and `grant` is
/// given, answer the challenge for `workspace` once.
///
/// # Errors
/// The runtime could not be reached or broke off.
pub async fn post(
    http: &tilo_net::Client,
    route: &Route,
    url: &str,
    session: Option<&str>,
    proof_for: Option<(&GrantKey, &str)>,
    body: &Value,
) -> Result<Answer, String> {
    let first = send(http, route, url, session, None, body).await?;
    let (Some((grant, workspace)), 401) = (proof_for, first.0) else {
        return Ok(first);
    };
    let Some(challenge) = first
        .1
        .get(CHALLENGE_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| hex::decode(text).ok())
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
    else {
        return Ok(first);
    };
    let proof = grant
        .prove(
            &tilo_user_identity::agent_endpoint_audience(workspace),
            challenge,
        )
        .and_then(|proof| tilo_user_identity::canonical::canonical_bytes(&proof).ok())
        .map(|bytes| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
        .ok_or_else(|| "the grant could not answer the runtime's challenge".to_owned())?;
    send(http, route, url, session, Some(&proof), body).await
}

async fn send(
    http: &tilo_net::Client,
    route: &Route,
    url: &str,
    session: Option<&str>,
    proof: Option<&str>,
    body: &Value,
) -> Result<Answer, String> {
    let mut request = http
        .post(url)
        .bearer_auth(&route.token)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .timeout(UPSTREAM_TIMEOUT)
        .body(body.to_string());
    if let Some(session) = session {
        request = request.header(SESSION_HEADER, session);
    }
    if let Some(proof) = proof {
        request = request.header(PROOF_HEADER, proof);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("the workspace's endpoint could not be reached: {error}"))?;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = response
        .bytes_limited(MAX_UPSTREAM_BYTES)
        .await
        .map_err(|error| format!("the workspace's endpoint broke off: {error}"))?;
    // A JSON answer as it is; a plain one (a refusal's reason) as a string.
    let body = serde_json::from_slice::<Value>(&bytes).ok().or_else(|| {
        std::str::from_utf8(&bytes)
            .ok()
            .filter(|text| !text.is_empty())
            .map(|text| Value::String(text.to_owned()))
    });
    Ok((status, headers, body))
}

/// Have the runtime at `route` join `workspace` with `invitation` (the
/// signed invitation, hex, as the host issued it), as `grant`. It goes as
/// the text every joiner takes: the signed bytes, base64url.
///
/// # Errors
/// The runtime refused or could not be reached.
pub async fn join(
    http: &tilo_net::Client,
    route: &Route,
    grant: &GrantKey,
    workspace: &str,
    invitation: &str,
) -> Result<(), String> {
    let signed = hex::decode(invitation)
        .map_err(|_| "the host issued an invitation that is not hex".to_owned())?;
    let text = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signed);
    let body = serde_json::json!({"invitation": text});
    let (status, _, answer) = post(
        http,
        route,
        &route.join(workspace),
        None,
        Some((grant, workspace)),
        &body,
    )
    .await?;
    if status == 200 {
        return Ok(());
    }
    let reason = match &answer {
        Some(Value::String(text)) => text.clone(),
        Some(answer) => answer
            .get("error")
            .and_then(Value::as_str)
            .map_or_else(|| format!("HTTP {status}"), str::to_owned),
        None => format!("HTTP {status}"),
    };
    Err(format!("the runtime did not join the workspace: {reason}"))
}

/// End the session `session` at `url`.
pub async fn close(http: &tilo_net::Client, route: &Route, url: &str, session: &str) {
    let _ = http
        .delete(url)
        .bearer_auth(&route.token)
        .header(SESSION_HEADER, session)
        .timeout(Duration::from_secs(10))
        .send()
        .await;
}

//! The OAuth 2.1 authorization server the MCP authorization profile asks
//! for, so an agent connects with nothing configured but the server's URL:
//!
//! 1. The agent's first request has no token; the server answers 401 with
//!    `WWW-Authenticate` naming its protected-resource metadata (RFC 9728).
//! 2. That metadata names the server as the authorization server, whose
//!    metadata (RFC 8414) names the endpoints below.
//! 3. The agent registers itself (RFC 7591), then opens `/authorize` in the
//!    person's browser with a PKCE challenge (RFC 7636) and the resource it
//!    wants a token for (RFC 8707).
//! 4. The person signs in, chooses the scope and agrees; the browser returns
//!    to the agent with a code, which it exchanges at `/token` with its
//!    verifier.
//!
//! Only public clients, only the authorization-code and refresh-token
//! grants, only S256.

use crate::store::{Client, Scope, TokenError};
use serde_json::{Value, json};
use std::{collections::HashMap, hash::BuildHasher};

pub const PROTECTED_RESOURCE_PATH: &str = "/.well-known/oauth-protected-resource";
pub const AUTHORIZATION_SERVER_PATH: &str = "/.well-known/oauth-authorization-server";
/// Some clients look for `OpenID` discovery first; the same document answers.
pub const OPENID_CONFIGURATION_PATH: &str = "/.well-known/openid-configuration";
pub const REGISTER_PATH: &str = "/register";
pub const AUTHORIZE_PATH: &str = "/authorize";
pub const TOKEN_PATH: &str = "/token";
pub const REVOKE_PATH: &str = "/revoke";
/// The scope an MCP client may ask for; the person's choice on the consent
/// page decides what it reaches.
pub const MCP_SCOPE: &str = "mcp";

const MAX_REDIRECT_URIS: usize = 8;
const MAX_URI_CHARS: usize = 2_048;
const MAX_NAME_CHARS: usize = 80;

/// The authorization server's metadata (RFC 8414).
#[must_use]
pub fn server_metadata(issuer: &str) -> Value {
    json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}{AUTHORIZE_PATH}"),
        "token_endpoint": format!("{issuer}{TOKEN_PATH}"),
        "registration_endpoint": format!("{issuer}{REGISTER_PATH}"),
        "revocation_endpoint": format!("{issuer}{REVOKE_PATH}"),
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "revocation_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": [MCP_SCOPE],
    })
}

/// The protected-resource metadata (RFC 9728) of `resource`.
#[must_use]
pub fn resource_metadata(issuer: &str, resource: &str) -> Value {
    json!({
        "resource": resource,
        "authorization_servers": [issuer],
        "bearer_methods_supported": ["header"],
        "scopes_supported": [MCP_SCOPE],
        "resource_name": "Tilo",
    })
}

/// A registration request's client, checked: a public client with redirect
/// URIs a browser can return to safely.
///
/// # Errors
/// The RFC 7591 error and its description.
pub fn registration(body: &Value) -> Result<(String, Vec<String>), (&'static str, String)> {
    let invalid = |message: &str| ("invalid_client_metadata", message.to_owned());
    let uris = body
        .get("redirect_uris")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            (
                "invalid_redirect_uri",
                "redirect_uris is required".to_owned(),
            )
        })?;
    if uris.is_empty() || uris.len() > MAX_REDIRECT_URIS {
        return Err((
            "invalid_redirect_uri",
            format!("between 1 and {MAX_REDIRECT_URIS} redirect_uris"),
        ));
    }
    let uris = uris
        .iter()
        .map(|uri| {
            uri.as_str()
                .filter(|uri| redirect_allowed(uri))
                .map(str::to_owned)
                .ok_or_else(|| {
                    (
                        "invalid_redirect_uri",
                        "redirect URIs must be https, http on loopback, or an app's own scheme"
                            .to_owned(),
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    match body
        .get("token_endpoint_auth_method")
        .and_then(Value::as_str)
    {
        None | Some("none") => {}
        Some(_) => {
            return Err(invalid(
                "only public clients (token_endpoint_auth_method none) register here",
            ));
        }
    }
    if let Some(grants) = body.get("grant_types").and_then(Value::as_array)
        && grants
            .iter()
            .any(|grant| !matches!(grant.as_str(), Some("authorization_code" | "refresh_token")))
    {
        return Err(invalid(
            "only the authorization_code and refresh_token grants",
        ));
    }
    if let Some(types) = body.get("response_types").and_then(Value::as_array)
        && types.iter().any(|kind| kind.as_str() != Some("code"))
    {
        return Err(invalid("only the code response type"));
    }
    let name: String = body
        .get("client_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    let name = name.trim();
    Ok((
        if name.is_empty() { "MCP agent" } else { name }.to_owned(),
        uris,
    ))
}

/// What RFC 7591 answers for a registered client.
#[must_use]
pub fn registered(client: &Client) -> Value {
    json!({
        "client_id": client.id,
        "client_id_issued_at": client.issued_at_ms / 1000,
        "client_name": client.name,
        "redirect_uris": client.redirect_uris,
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    })
}

/// Whether a browser may be sent back to `uri` with a code: https anywhere,
/// http only to the machine itself (where agents like Claude Code listen),
/// or an application's private scheme (RFC 8252). Never a fragment.
fn redirect_allowed(uri: &str) -> bool {
    if uri.len() > MAX_URI_CHARS || uri.contains('#') {
        return false;
    }
    let Ok(parsed) = url::Url::parse(uri) else {
        return false;
    };
    match parsed.scheme() {
        "https" => parsed.host().is_some(),
        "http" => matches!(
            parsed.host(),
            Some(
                url::Host::Domain("localhost")
                    | url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST)
                    | url::Host::Ipv6(std::net::Ipv6Addr::LOCALHOST)
            )
        ),
        // A private-use scheme names an app on the device; it must look like
        // one (a dot, as a reversed domain) so `javascript:` and `data:` are
        // never accepted.
        scheme => scheme.contains('.'),
    }
}

/// Whether `presented` is one of `client`'s registered redirect URIs.
/// Loopback redirects may use any port, as RFC 8252 asks: a native app
/// learns its port only when it listens.
#[must_use]
pub fn redirect_registered(client: &Client, presented: &str) -> bool {
    client
        .redirect_uris
        .iter()
        .any(|registered| registered == presented || loopback_same_but_port(registered, presented))
}

fn loopback_same_but_port(registered: &str, presented: &str) -> bool {
    let (Ok(mut registered), Ok(mut presented)) =
        (url::Url::parse(registered), url::Url::parse(presented))
    else {
        return false;
    };
    let loopback = |url: &url::Url| {
        url.scheme() == "http"
            && matches!(
                url.host(),
                Some(
                    url::Host::Domain("localhost")
                        | url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST)
                        | url::Host::Ipv6(std::net::Ipv6Addr::LOCALHOST)
                )
            )
    };
    if !loopback(&registered) || !loopback(&presented) {
        return false;
    }
    let _ = registered.set_port(None);
    let _ = presented.set_port(None);
    registered == presented
}

/// An authorization request, checked far enough that the browser may be
/// sent back to its redirect URI with an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizeRequest {
    pub client_id: String,
    pub redirect_uri: String,
    pub state: Option<String>,
    pub challenge: String,
    /// The workspace the requested resource is pinned to, when the agent
    /// was pointed at `/w/<workspace>`.
    pub pinned: Option<String>,
}

/// Why an authorization request is refused.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthorizeError {
    /// The client or its redirect URI cannot be trusted: shown to the person,
    /// never redirected.
    Untrusted(&'static str),
    /// Sent back to the client's redirect URI (RFC 6749 §4.1.2.1).
    Redirect {
        redirect_uri: String,
        state: Option<String>,
        error: &'static str,
        description: &'static str,
    },
}

/// Check `/authorize`'s query against the client that registered.
///
/// # Errors
/// [`AuthorizeError`]: whether the browser can be sent back.
pub fn authorize_request<S: BuildHasher>(
    query: &HashMap<String, String, S>,
    client: Option<&Client>,
    issuer: &str,
) -> Result<AuthorizeRequest, AuthorizeError> {
    let client = client.ok_or(AuthorizeError::Untrusted("This app is not registered."))?;
    let redirect_uri = match (query.get("redirect_uri"), client.redirect_uris.as_slice()) {
        (Some(uri), _) if redirect_registered(client, uri) => uri.clone(),
        (None, [only]) => only.clone(),
        _ => {
            return Err(AuthorizeError::Untrusted(
                "This app asked to return somewhere it did not register.",
            ));
        }
    };
    let state = query.get("state").cloned();
    let fail = |error, description| AuthorizeError::Redirect {
        redirect_uri: redirect_uri.clone(),
        state: state.clone(),
        error,
        description,
    };
    if query.get("response_type").map(String::as_str) != Some("code") {
        return Err(fail("unsupported_response_type", "only response_type=code"));
    }
    if query.get("code_challenge_method").map(String::as_str) != Some("S256") {
        return Err(fail(
            "invalid_request",
            "PKCE with code_challenge_method=S256 is required",
        ));
    }
    let challenge = query
        .get("code_challenge")
        .filter(|challenge| challenge.len() == 43 && challenge.bytes().all(unreserved))
        .ok_or_else(|| fail("invalid_request", "code_challenge must be a S256 challenge"))?
        .clone();
    if let Some(scope) = query.get("scope")
        && scope
            .split(' ')
            .any(|scope| !scope.is_empty() && scope != MCP_SCOPE)
    {
        return Err(fail("invalid_scope", "the only scope is mcp"));
    }
    let pinned = match query.get("resource") {
        None => None,
        Some(resource) => match resource_scope(issuer, resource) {
            Some(pinned) => pinned,
            None => return Err(fail("invalid_target", "the resource is not this server")),
        },
    };
    Ok(AuthorizeRequest {
        client_id: client.id.clone(),
        redirect_uri,
        state,
        challenge,
        pinned,
    })
}

/// What `resource` (RFC 8707) names on this server: `Some(None)` for the
/// server itself, `Some(Some(workspace))` for `/w/<workspace>`, `None` for
/// anything else.
#[must_use]
pub fn resource_scope(issuer: &str, resource: &str) -> Option<Option<String>> {
    let rest = resource.strip_prefix(issuer)?;
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() || rest == "/mcp" {
        return Some(None);
    }
    crate::pinned_workspace(rest).map(Some)
}

/// Whether the scope a token carries may be used at the resource whose
/// workspace pin is `pinned`.
#[must_use]
pub fn scope_reaches(scope: &Scope, pinned: Option<&str>) -> bool {
    // Any grant at the server's root: a workspace grant's session is pinned all
    // the same, so a client configured with the root URL keeps working. An
    // account grant reaches each of the person's workspaces at its own URL,
    // as a backend works in each customer's; the relay still admits it only
    // where the workspace is the person's.
    match (scope, pinned) {
        (Scope::Workspace(workspace), Some(pinned)) => workspace == pinned,
        (Scope::Account | Scope::Workspace(_), _) => true,
    }
}

/// Where the browser goes back to with `params`; `None` if `redirect_uri`
/// is not a URL, which a registered one always is.
#[must_use]
pub fn redirect_to(redirect_uri: &str, params: &[(&str, &str)]) -> Option<String> {
    let mut url = url::Url::parse(redirect_uri).ok()?;
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in params {
            pairs.append_pair(key, value);
        }
    }
    Some(url.into())
}

/// Whether a PKCE verifier is well formed (RFC 7636 §4.1).
#[must_use]
pub fn verifier_valid(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len()) && verifier.bytes().all(unreserved)
}

const fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// The token endpoint's error body.
#[must_use]
pub fn token_error(error: &TokenError) -> Value {
    match error {
        TokenError::InvalidGrant => json!({"error":"invalid_grant"}),
        TokenError::InvalidClient => json!({"error":"invalid_client"}),
    }
}

/// A form body (`application/x-www-form-urlencoded`) as a map; a key given
/// twice is refused, as RFC 6749 asks.
#[must_use]
pub fn form(body: &[u8]) -> Option<HashMap<String, String>> {
    let mut map = HashMap::new();
    for (key, value) in url::form_urlencoded::parse(body) {
        if map.insert(key.into_owned(), value.into_owned()).is_some() {
            return None;
        }
    }
    Some(map)
}

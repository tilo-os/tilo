//! The one `OAuth` 2.0 client for Tilo's installed apps.
//!
//! Mail signs in to Gmail and Outlook with it; Settings connects Google
//! Drive, `OneDrive` and Dropbox with it. Every provider is the same flow
//! (RFC 6749 authorization code, RFC 7636 PKCE with S256, RFC 8252 loopback
//! redirect), so providers differ only in data: a [`Provider`] descriptor
//! carries the endpoints, scopes, provider-specific authorization parameters
//! and how (or whether) a grant can be revoked.
//!
//! # Pieces
//!
//! - [`Provider`] and [`ClientId`]: descriptors as data. A client id is read
//!   from the environment first and then from the value baked in at build
//!   time; a missing id is [`ClientId::resolve`] returning `None`, which
//!   callers present as "not configured" rather than starting a flow that
//!   cannot finish.
//! - [`Pkce`], [`AuthorizationRequest`]: the verifier/challenge pair and the
//!   browser URL, with a fresh random `state`.
//! - [`loopback::LoopbackServer`]: the one-shot `127.0.0.1` redirect listener
//!   on an ephemeral port, validating `state` and cancellable from another
//!   thread.
//! - [`TokenSet`], [`parse_token_response`]: token endpoint answers, with
//!   `invalid_grant` surfaced as [`OAuthError::InvalidGrant`] so a revoked
//!   refresh token becomes "sign in again" instead of a generic failure.
//! - `client` (feature `client`): [`client::OAuthClient`] for the code
//!   exchange, refresh and revocation, and [`client::Authorization`] tying
//!   the whole browser round trip together.
//!
//! Nothing here stores tokens. Callers own custody (the account vault) and
//! decide which party ever sees which token.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::fmt;

use base64::Engine as _;
use getrandom::{SysRng, rand_core::TryRng as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

#[cfg(feature = "client")]
pub mod client;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
#[cfg(all(test, feature = "client"))]
mod flow_tests;
pub mod loopback;

/// How a provider lets an app give up a grant when the account is removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Revocation {
    /// The provider has no per-app revocation endpoint (Microsoft identity
    /// platform); the grant simply stops being used and expires.
    Unsupported,
    /// RFC 7009: `POST endpoint` with `token=<refresh token>` (Google).
    Rfc7009 { endpoint: String },
    /// `POST endpoint` authorised with the access token as a bearer and an
    /// empty body (Dropbox `auth/token/revoke`).
    BearerPost { endpoint: String },
}

/// One `OAuth` provider as data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    /// Stable identifier (`google`, `microsoft`, `dropbox`).
    pub id: String,
    /// Human name shown in the sign-in UI.
    pub label: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub revocation: Revocation,
    /// Scopes requested, joined with spaces on the wire.
    pub scopes: Vec<String>,
    /// Provider-specific authorization parameters, e.g. Google's
    /// `access_type=offline` + `prompt=consent` or Dropbox's
    /// `token_access_type=offline`, which make the provider issue a refresh
    /// token.
    pub authorization_params: Vec<(String, String)>,
}

impl Provider {
    /// The space-separated scope string.
    pub fn scope(&self) -> String {
        self.scopes.join(" ")
    }
}

/// Which loopback ports the redirect listener may take.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RedirectPorts {
    /// Any free port. For providers that match a loopback redirect without
    /// its port (RFC 8252 §7.3): Google desktop clients and Microsoft public
    /// clients.
    #[default]
    Any,
    /// The first free port of this list. For providers that match redirect
    /// URIs exactly, port included (Dropbox): each port is registered with
    /// the app as `http://127.0.0.1:<port>/oauth`.
    Registered(Vec<u16>),
}

/// What a provider's developer console issued for this app.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Registration {
    pub client_id: String,
    /// Sent on token requests when set. Google issues one to installed-app
    /// ("Desktop") clients and requires it on the code exchange and refresh.
    /// An installed app cannot keep a secret (RFC 8252 §8.5), so this is
    /// not a credential; PKCE protects the code.
    pub client_secret: Option<String>,
    pub redirect_ports: RedirectPorts,
    /// When no registered port is free, let the person paste the code the
    /// provider displays instead (providers whose `redirect_uri` is
    /// optional: Dropbox).
    pub paste_code_fallback: bool,
}

impl Registration {
    /// A public client on any loopback port, without a secret.
    pub fn public(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            ..Self::default()
        }
    }

    /// The client secret, when one is set and not blank.
    pub fn secret(&self) -> Option<&str> {
        self.client_secret
            .as_deref()
            .map(str::trim)
            .filter(|secret| !secret.is_empty())
    }
}

impl From<&str> for Registration {
    fn from(client_id: &str) -> Self {
        Self::public(client_id)
    }
}

impl From<String> for Registration {
    fn from(client_id: String) -> Self {
        Self::public(client_id)
    }
}

impl fmt::Debug for Registration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registration")
            .field("client_id", &self.client_id)
            .field("client_secret", &self.secret().map(|_| "<set>"))
            .field("redirect_ports", &self.redirect_ports)
            .field("paste_code_fallback", &self.paste_code_fallback)
            .finish_non_exhaustive()
    }
}

/// Where a registered `OAuth` client id comes from: the environment variable
/// `key` at runtime, else the value of the same variable at build time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientId {
    /// The configuration key, e.g. `TILO_OAUTH_GOOGLE_CLIENT_ID`.
    pub key: &'static str,
    /// The value captured by `option_env!(key)` in the crate that owns the
    /// descriptor.
    pub build_time: Option<&'static str>,
}

impl ClientId {
    pub const fn new(key: &'static str, build_time: Option<&'static str>) -> Self {
        Self { key, build_time }
    }

    /// The client id, or `None` when neither the environment nor the build
    /// provided one. `None` is the "not configured" state.
    pub fn resolve(&self) -> Option<String> {
        std::env::var(self.key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                self.build_time
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
    }
}

/// A PKCE verifier/challenge pair (RFC 7636). The verifier is sent on the
/// token exchange; its S256 challenge on the authorization request, binding
/// the two so an intercepted code cannot be redeemed.
#[derive(Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl fmt::Debug for Pkce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pkce")
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

impl Pkce {
    /// A fresh 256-bit verifier and its S256 challenge.
    ///
    /// # Errors
    /// [`OAuthError::Randomness`] when the OS CSPRNG is unavailable.
    pub fn generate() -> Result<Self, OAuthError> {
        Ok(Self::from_verifier(&random_token()?))
    }

    /// The S256 challenge for a known verifier.
    pub fn from_verifier(verifier: &str) -> Self {
        Self {
            verifier: verifier.to_string(),
            challenge: s256_challenge(verifier),
        }
    }
}

/// `BASE64URL(SHA256(verifier))`, the S256 transform.
pub fn s256_challenge(verifier: &str) -> String {
    base64url_nopad(&Sha256::digest(verifier.as_bytes()))
}

/// 256 random bits as URL-safe base64: a PKCE verifier or a `state` value.
///
/// # Errors
/// [`OAuthError::Randomness`] when the OS CSPRNG is unavailable.
pub fn random_token() -> Result<String, OAuthError> {
    let mut bytes = [0_u8; 32];
    SysRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| OAuthError::Randomness)?;
    Ok(base64url_nopad(&bytes))
}

/// URL-safe base64 without padding (RFC 4648 §5).
pub fn base64url_nopad(input: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input)
}

/// The authorization request the browser is sent to.
#[derive(Clone, Debug)]
pub struct AuthorizationRequest<'a> {
    pub provider: &'a Provider,
    pub client_id: &'a str,
    /// Empty for a sign-in whose code the provider displays for pasting.
    pub redirect_uri: &'a str,
    pub code_challenge: &'a str,
    pub state: &'a str,
    /// Pre-fills the account on the provider's page when known.
    pub login_hint: Option<&'a str>,
}

impl AuthorizationRequest<'_> {
    /// The browser URL: `response_type=code`, PKCE S256, `state`, scopes and
    /// the provider's own parameters, in that order.
    pub fn url(&self) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query
            .append_pair("client_id", self.client_id)
            .append_pair("response_type", "code");
        if !self.redirect_uri.is_empty() {
            query.append_pair("redirect_uri", self.redirect_uri);
        }
        query
            .append_pair("scope", &self.provider.scope())
            .append_pair("code_challenge", self.code_challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", self.state);
        for (key, value) in &self.provider.authorization_params {
            query.append_pair(key, value);
        }
        if let Some(hint) = self.login_hint.filter(|hint| !hint.is_empty()) {
            query.append_pair("login_hint", hint);
        }
        let separator = if self.provider.authorization_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        format!(
            "{}{separator}{}",
            self.provider.authorization_endpoint,
            query.finish()
        )
    }
}

/// Tokens from a code exchange or refresh.
///
/// The serialised shape is what existing vault entries hold, so it is part
/// of the on-disk format. `Debug` never prints a token.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    /// Access token lifetime in seconds as reported by the provider; `0`
    /// when it reported none.
    #[serde(default)]
    pub expires_in: u64,
    /// Unix seconds at which the access token was issued.
    #[serde(default)]
    pub obtained_at: u64,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("has_refresh_token", &!self.refresh_token.is_empty())
            .field("expires_in", &self.expires_in)
            .field("obtained_at", &self.obtained_at)
            .finish_non_exhaustive()
    }
}

impl TokenSet {
    /// Stamp `obtained_at` with the current wall clock.
    #[must_use]
    pub fn stamp_now(mut self) -> Self {
        self.obtained_at = tilo_clock::unix_now();
        self
    }

    /// Unix seconds at which the access token expires, `None` when the
    /// provider reported no lifetime.
    pub fn expires_at(&self) -> Option<u64> {
        (self.expires_in > 0).then(|| self.obtained_at.saturating_add(self.expires_in))
    }

    /// Whether the access token is expired at `now` or within `skew` seconds
    /// of it. A token without a reported lifetime counts as expired so it is
    /// always refreshed before use.
    pub fn is_expired_with_skew(&self, skew: u64, now: u64) -> bool {
        self.expires_at()
            .is_none_or(|expires| now.saturating_add(skew) >= expires)
    }

    /// Whether the access token should be refreshed before the next request
    /// (a 60 second margin).
    pub fn needs_refresh(&self) -> bool {
        self.is_expired_with_skew(60, tilo_clock::unix_now())
    }
}

/// Why an `OAuth` step failed. Messages never contain tokens, codes or
/// response bodies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OAuthError {
    /// The OS CSPRNG could not produce a verifier or state.
    Randomness,
    /// The loopback redirect listener could not be bound.
    Loopback(String),
    /// Every registered redirect port is taken by another program.
    RedirectPortsBusy,
    /// The sign-in needs the code the provider displayed, pasted in.
    NeedsPastedCode,
    /// The system browser could not be opened.
    Browser(String),
    /// The redirect carried a `state` other than the one we sent (CSRF).
    StateMismatch,
    /// The person or the provider declined (`error=access_denied`, …).
    Denied(String),
    /// Nobody completed the sign-in before the deadline.
    TimedOut,
    /// The flow was cancelled from the UI.
    Cancelled,
    /// The grant (authorization code or refresh token) is invalid, expired
    /// or revoked: the account must be signed in again.
    InvalidGrant,
    /// The token endpoint could not be reached.
    Network(String),
    /// The token endpoint answered something other than a token.
    Protocol(String),
}

impl OAuthError {
    /// Whether the account needs an interactive sign-in to recover.
    pub fn needs_authentication(&self) -> bool {
        matches!(self, Self::InvalidGrant)
    }
}

impl fmt::Display for OAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Randomness => f.write_str("the system random number generator is unavailable"),
            Self::Loopback(error) => {
                write!(f, "could not listen for the sign-in redirect: {error}")
            }
            Self::RedirectPortsBusy => f.write_str(
                "every port registered for the sign-in redirect is in use by another program",
            ),
            Self::NeedsPastedCode => {
                f.write_str("paste the code the provider showed after you allowed access")
            }
            Self::Browser(error) => write!(f, "could not open the sign-in page: {error}"),
            Self::StateMismatch => f.write_str("the sign-in response did not match this request"),
            Self::Denied(reason) => write!(f, "sign-in was declined ({reason})"),
            Self::TimedOut => f.write_str("sign-in was not completed in time"),
            Self::Cancelled => f.write_str("sign-in was cancelled"),
            Self::InvalidGrant => f.write_str("the sign-in is no longer valid; sign in again"),
            Self::Network(error) => write!(f, "could not reach the sign-in service: {error}"),
            Self::Protocol(error) => {
                write!(f, "the sign-in service answered unexpectedly: {error}")
            }
        }
    }
}

impl std::error::Error for OAuthError {}

/// Parse a token endpoint JSON answer. A refresh answer may omit
/// `refresh_token` (the old one stays valid), so an empty one is not an
/// error. The result is stamped with the current time.
///
/// # Errors
/// [`OAuthError::InvalidGrant`] for `error=invalid_grant`, otherwise
/// [`OAuthError::Protocol`] naming only the `OAuth` error code.
pub fn parse_token_response(body: &str) -> Result<TokenSet, OAuthError> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| OAuthError::Protocol("the token response is not JSON".into()))?;
    if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
        return Err(token_error(error));
    }
    let access_token = value
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| OAuthError::Protocol("the token response has no access token".into()))?;
    Ok(TokenSet {
        access_token: access_token.to_string(),
        refresh_token: value
            .get("refresh_token")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        expires_in: value
            .get("expires_in")
            .and_then(|lifetime| {
                // Some providers send the lifetime as a string.
                lifetime
                    .as_u64()
                    .or_else(|| lifetime.as_str().and_then(|text| text.parse().ok()))
            })
            .unwrap_or(0),
        obtained_at: tilo_clock::unix_now(),
    })
}

/// Map an RFC 6749 §5.2 error code. Only the registered code is kept: the
/// description is provider prose that may echo request data.
pub fn token_error(code: &str) -> OAuthError {
    match code {
        "invalid_grant" => OAuthError::InvalidGrant,
        other => {
            let code: String = other
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .take(64)
                .collect();
            OAuthError::Protocol(format!("error {code}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn google() -> Provider {
        Provider {
            id: "google".into(),
            label: "Google".into(),
            authorization_endpoint: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_endpoint: "https://oauth2.googleapis.com/token".into(),
            revocation: Revocation::Rfc7009 {
                endpoint: "https://oauth2.googleapis.com/revoke".into(),
            },
            scopes: vec!["https://mail.google.com/".into()],
            authorization_params: vec![
                ("access_type".into(), "offline".into()),
                ("prompt".into(), "consent".into()),
            ],
        }
    }

    #[test]
    fn pkce_s256_challenge_matches_rfc7636_appendix_b() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(
            pkce.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_is_valid_and_matches_its_challenge() {
        let pkce = Pkce::generate().unwrap();
        assert_eq!(pkce.verifier.len(), 43);
        assert!(
            pkce.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_eq!(pkce.challenge, s256_challenge(&pkce.verifier));
        assert_ne!(random_token().unwrap(), random_token().unwrap());
        assert!(!format!("{pkce:?}").contains(&pkce.verifier));
    }

    #[test]
    fn base64url_matches_rfc4648_vectors() {
        assert_eq!(base64url_nopad(b""), "");
        assert_eq!(base64url_nopad(b"f"), "Zg");
        assert_eq!(base64url_nopad(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64url_nopad(&[0xfb, 0xff, 0xbf]), "-_-_");
    }

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            hex::encode(Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn google_authorization_url_is_pinned_byte_for_byte() {
        let provider = google();
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        let url = AuthorizationRequest {
            provider: &provider,
            client_id: "cid.apps.googleusercontent.com",
            redirect_uri: "http://127.0.0.1:8080/oauth",
            code_challenge: &pkce.challenge,
            state: "xyz 123",
            login_hint: Some("person@gmail.com"),
        }
        .url();
        assert_eq!(
            url,
            "https://accounts.google.com/o/oauth2/v2/auth?client_id=cid.apps.googleusercontent.com\
             &response_type=code&redirect_uri=http%3A%2F%2F127.0.0.1%3A8080%2Foauth\
             &scope=https%3A%2F%2Fmail.google.com%2F\
             &code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM\
             &code_challenge_method=S256&state=xyz+123&access_type=offline\
             &prompt=consent&login_hint=person%40gmail.com"
        );
    }

    #[test]
    fn provider_parameters_follow_an_existing_query() {
        let mut provider = google();
        provider.authorization_endpoint = "https://example.test/authorize?tenant=x".into();
        provider.authorization_params.clear();
        let url = AuthorizationRequest {
            provider: &provider,
            client_id: "c",
            redirect_uri: "http://127.0.0.1:1/oauth",
            code_challenge: "ch",
            state: "s",
            login_hint: None,
        }
        .url();
        assert!(url.starts_with("https://example.test/authorize?tenant=x&client_id=c&"));
        assert!(!url.contains("login_hint"));
    }

    #[test]
    fn client_id_prefers_runtime_then_build_time_and_treats_blank_as_missing() {
        let key = "TILO_OAUTH_TEST_ONLY_CLIENT_ID_UNSET";
        assert_eq!(ClientId::new(key, None).resolve(), None);
        assert_eq!(ClientId::new(key, Some("  ")).resolve(), None);
        assert_eq!(
            ClientId::new(key, Some("built-in")).resolve().as_deref(),
            Some("built-in")
        );
    }

    #[test]
    fn token_response_parses_access_refresh_and_lifetime() {
        let tokens = parse_token_response(
            r#"{"access_token":"ya29.abc","expires_in":3599,"refresh_token":"1//xyz","token_type":"Bearer"}"#,
        )
        .unwrap();
        assert_eq!(tokens.access_token, "ya29.abc");
        assert_eq!(tokens.refresh_token, "1//xyz");
        assert_eq!(tokens.expires_in, 3599);
        let debug = format!("{tokens:?}");
        assert!(
            !debug.contains("ya29") && !debug.contains("1//xyz"),
            "{debug}"
        );
    }

    #[test]
    fn token_errors_are_typed_and_never_echo_descriptions() {
        assert_eq!(
            parse_token_response(
                r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#
            ),
            Err(OAuthError::InvalidGrant)
        );
        let Err(OAuthError::Protocol(message)) = parse_token_response(
            r#"{"error":"invalid_client","error_description":"secret-bearing prose"}"#,
        ) else {
            panic!("expected a protocol error");
        };
        assert_eq!(message, "error invalid_client");
        assert!(parse_token_response("<html>").is_err());
        assert!(parse_token_response(r#"{"access_token":""}"#).is_err());
    }

    #[test]
    fn expiry_accounts_for_lifetime_and_skew() {
        let tokens = TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_in: 3600,
            obtained_at: 1_000_000,
        };
        assert!(!tokens.is_expired_with_skew(60, 1_000_100));
        assert!(tokens.is_expired_with_skew(60, 1_003_550));
        assert!(tokens.is_expired_with_skew(60, 1_004_601));
        let no_lifetime = TokenSet {
            expires_in: 0,
            ..tokens
        };
        assert!(no_lifetime.is_expired_with_skew(60, 1_000_000));
        assert_eq!(no_lifetime.expires_at(), None);
    }

    #[test]
    fn stored_token_shape_is_unchanged() {
        // Vault entries written by Mail's sign-in must keep deserialising.
        let stored =
            r#"{"access_token":"at","refresh_token":"rt","expires_in":3600,"obtained_at":1000}"#;
        let tokens: TokenSet = serde_json::from_str(stored).unwrap();
        assert_eq!(serde_json::to_string(&tokens).unwrap(), stored);
    }
}

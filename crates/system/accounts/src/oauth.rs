//! The one `OAuth2` authorization-code + PKCE implementation (RFC 6749,
//! RFC 7636, RFC 8252) every account consumer uses.
//!
//! Mail signs in to Google and Outlook with it; the model gateway signs in to
//! Microsoft Entra ID for Azure `OpenAI` with it. A consumer describes *who* it
//! talks to with an [`OAuthClient`] (issuer endpoints, client id, scopes); the
//! flow itself — PKCE, the browser redirect onto a loopback listener, the
//! code exchange, refresh, and sealing tokens in the [`SecretVault`](crate::SecretVault)
//! — lives only here.
//!
//! ## Client registrations
//!
//! A working flow needs an OAuth client id registered with the issuer. Tilo
//! ships one slot per issuer family ([`ClientSlot`]); the shipped id may be
//! empty until a registration exists. A developer or an image build can
//! override a slot with `TILO_OAUTH_<SLOT>_CLIENT_ID`, `…_TENANT` and
//! `…_AUTHORITY` (the last points the whole flow at a test issuer). No client
//! secret exists: every registration is a public client, which is what PKCE
//! is for.
//!
//! ## What needs the network (`http`)
//!
//! [`exchange_code`], [`refresh_tokens`] and [`run_loopback_authorization`]
//! post to the token endpoint and are compiled only with the `http` feature,
//! so offline consumers never link an HTTP client.

use std::fmt::Write as _;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tilo_clock::unix_now;

/// The authorization and token endpoints of one issuer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthIssuer {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
}

impl OAuthIssuer {
    /// A Microsoft identity platform (v2.0) issuer rooted at `authority`
    /// (normally `https://login.microsoftonline.com`) for `tenant`
    /// (`common`, `organizations`, or a tenant id).
    #[must_use]
    pub fn microsoft(authority: &str, tenant: &str) -> Self {
        let authority = authority.trim_end_matches('/');
        Self {
            authorization_endpoint: format!("{authority}/{tenant}/oauth2/v2.0/authorize"),
            token_endpoint: format!("{authority}/{tenant}/oauth2/v2.0/token"),
        }
    }
}

/// Everything a consumer states about the party it signs in to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthClient {
    pub issuer: OAuthIssuer,
    pub client_id: String,
    /// Space-separated scopes requested at authorization.
    pub scopes: String,
    /// Issuer-specific authorization parameters (Google's `access_type`).
    pub extra_authorization_params: Vec<(String, String)>,
}

/// The registration families Tilo ships a client slot for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientSlot {
    /// Google accounts (Gmail).
    Google,
    /// Microsoft consumer and work accounts for Outlook mail.
    Microsoft,
    /// Microsoft Entra ID work accounts for Azure resources (Azure `OpenAI`).
    Entra,
}

impl ClientSlot {
    const fn env_prefix(self) -> &'static str {
        match self {
            Self::Google => "TILO_OAUTH_GOOGLE",
            Self::Microsoft => "TILO_OAUTH_MICROSOFT",
            Self::Entra => "TILO_OAUTH_ENTRA",
        }
    }

    /// The environment variable that overrides this slot's client id.
    #[must_use]
    pub fn client_id_env(self) -> String {
        format!("{}_CLIENT_ID", self.env_prefix())
    }

    /// The registration shipped with this Tilo image. An empty client id
    /// means no registration has been provisioned for the slot yet.
    const fn shipped(self) -> ShippedRegistration {
        match self {
            Self::Google => ShippedRegistration {
                client_id: "",
                tenant: "",
                authority: "https://accounts.google.com",
            },
            Self::Microsoft => ShippedRegistration {
                client_id: "",
                tenant: "common",
                authority: "https://login.microsoftonline.com",
            },
            // Azure OpenAI admits only work/school accounts.
            Self::Entra => ShippedRegistration {
                client_id: "",
                tenant: "organizations",
                authority: "https://login.microsoftonline.com",
            },
        }
    }

    /// The effective registration: the shipped slot with any environment
    /// override applied. `None` when no client id is configured, so a caller
    /// can say sign-in is unavailable instead of opening a broken flow.
    #[must_use]
    pub fn registration(self) -> Option<ClientRegistration> {
        let shipped = self.shipped();
        let read = |suffix: &str| {
            std::env::var(format!("{}_{suffix}", self.env_prefix()))
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let client_id = read("CLIENT_ID").unwrap_or_else(|| shipped.client_id.to_string());
        if client_id.is_empty() {
            return None;
        }
        Some(ClientRegistration {
            slot: self,
            client_id,
            tenant: read("TENANT").unwrap_or_else(|| shipped.tenant.to_string()),
            authority: read("AUTHORITY").unwrap_or_else(|| shipped.authority.to_string()),
        })
    }
}

struct ShippedRegistration {
    client_id: &'static str,
    tenant: &'static str,
    authority: &'static str,
}

/// A configured client registration for one [`ClientSlot`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientRegistration {
    pub slot: ClientSlot,
    pub client_id: String,
    pub tenant: String,
    pub authority: String,
}

impl ClientRegistration {
    /// The issuer this registration signs in against.
    #[must_use]
    pub fn issuer(&self) -> OAuthIssuer {
        match self.slot {
            ClientSlot::Google => {
                let authority = self.authority.trim_end_matches('/');
                if authority == "https://accounts.google.com" {
                    OAuthIssuer {
                        authorization_endpoint: "https://accounts.google.com/o/oauth2/v2/auth"
                            .into(),
                        token_endpoint: "https://oauth2.googleapis.com/token".into(),
                    }
                } else {
                    OAuthIssuer {
                        authorization_endpoint: format!("{authority}/authorize"),
                        token_endpoint: format!("{authority}/token"),
                    }
                }
            }
            ClientSlot::Microsoft | ClientSlot::Entra => {
                OAuthIssuer::microsoft(&self.authority, &self.tenant)
            }
        }
    }

    /// A client for this registration requesting `scopes`.
    #[must_use]
    pub fn client(&self, scopes: &str) -> OAuthClient {
        let extra_authorization_params = if self.slot == ClientSlot::Google {
            // Google returns a refresh token only for offline, consented access.
            vec![
                ("access_type".into(), "offline".into()),
                ("prompt".into(), "consent".into()),
            ]
        } else {
            Vec::new()
        };
        OAuthClient {
            issuer: self.issuer(),
            client_id: self.client_id.clone(),
            scopes: scopes.to_string(),
            extra_authorization_params,
        }
    }
}

/// A PKCE verifier/challenge pair (RFC 7636). The verifier is sent on the token
/// exchange; the base64url-SHA256 challenge is sent on the authorization
/// request, binding the two so an intercepted code can't be redeemed.
#[derive(Clone, Debug)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// Generate a fresh high-entropy verifier (256 bits) and its S256
    /// challenge.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = Vec::with_capacity(32);
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self::from_verifier(&base64url_nopad(&bytes))
    }

    /// Derive the S256 challenge for a known verifier.
    #[must_use]
    pub fn from_verifier(verifier: &str) -> Self {
        Self {
            verifier: verifier.to_string(),
            challenge: base64url_nopad(&sha256(verifier.as_bytes())),
        }
    }
}

/// A fully specified authorization request.
pub struct AuthRequest {
    pub client: OAuthClient,
    pub redirect_uri: String,
    pub pkce: Pkce,
    pub state: String,
    /// Pre-fills the account at the issuer (`login_hint`); empty for none.
    pub login_hint: String,
}

impl AuthRequest {
    /// The browser authorization URL (`response_type=code` + PKCE S256).
    #[must_use]
    pub fn authorization_url(&self) -> String {
        let mut url = self.client.issuer.authorization_endpoint.clone();
        url.push(if url.contains('?') { '&' } else { '?' });
        let mut pairs: Vec<(&str, &str)> = vec![
            ("client_id", &self.client.client_id),
            ("response_type", "code"),
            ("redirect_uri", &self.redirect_uri),
            ("scope", &self.client.scopes),
            ("code_challenge", &self.pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", &self.state),
        ];
        for (key, value) in &self.client.extra_authorization_params {
            pairs.push((key, value));
        }
        if !self.login_hint.is_empty() {
            pairs.push(("login_hint", &self.login_hint));
        }
        let query = pairs
            .iter()
            .map(|(key, value)| format!("{key}={}", encode_component(value)))
            .collect::<Vec<_>>()
            .join("&");
        url.push_str(&query);
        url
    }
}

/// Percent-encode everything outside RFC 3986's unreserved set.
#[must_use]
pub fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The tokens a successful exchange or refresh returns. Sealed in the vault
/// ([`tokens_to_secret`]), never written to a plaintext settings or account
/// document.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    /// Lifetime of the access token in seconds, as reported by the issuer.
    #[serde(default)]
    pub expires_in: u64,
    /// Unix seconds at which the access token was obtained.
    #[serde(default)]
    pub obtained_at: u64,
}

impl OAuthTokens {
    /// Stamp `obtained_at` with the current wall clock.
    #[must_use]
    pub fn stamp_now(mut self) -> Self {
        self.obtained_at = unix_now();
        self
    }

    /// Whether the access token is expired, or within `skew` seconds of it.
    /// A token with no reported lifetime is treated as already expired.
    #[must_use]
    pub fn is_expired_with_skew(&self, skew: u64, now: u64) -> bool {
        if self.expires_in == 0 {
            return true;
        }
        now.saturating_add(skew) >= self.obtained_at.saturating_add(self.expires_in)
    }

    /// Whether the access token needs refreshing right now (60 s skew).
    #[must_use]
    pub fn needs_refresh(&self) -> bool {
        self.is_expired_with_skew(60, unix_now())
    }
}

/// Parse a token-endpoint JSON response. `None` for an error payload. A
/// refresh response may omit `refresh_token`; that is not an error.
#[must_use]
pub fn parse_token_response(json: &str) -> Option<OAuthTokens> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let access_token = value.get("access_token")?.as_str()?.to_string();
    if access_token.is_empty() {
        return None;
    }
    Some(OAuthTokens {
        access_token,
        refresh_token: value
            .get("refresh_token")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        expires_in: value
            .get("expires_in")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        obtained_at: unix_now(),
    })
}

/// Seal a token bundle into a vault [`Secret`](crate::Secret).
#[must_use]
pub fn tokens_to_secret(tokens: &OAuthTokens) -> crate::Secret {
    crate::Secret {
        password: String::new(),
        token: serde_json::to_string(tokens).ok(),
    }
}

/// Recover a token bundle from a vault [`Secret`](crate::Secret).
#[must_use]
pub fn tokens_from_secret(secret: &crate::Secret) -> Option<OAuthTokens> {
    serde_json::from_str(secret.token.as_ref()?).ok()
}

/// A one-shot loopback HTTP listener for the authorization-code redirect
/// (RFC 8252 §7.3). It binds an ephemeral `127.0.0.1` port, so no privilege
/// and no fixed-port collision is involved.
pub struct LoopbackServer {
    listener: std::net::TcpListener,
    port: u16,
}

impl LoopbackServer {
    /// Bind `127.0.0.1:0`.
    ///
    /// # Errors
    /// Returns `Err` if the loopback interface can't be bound.
    pub fn bind() -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        Ok(Self { listener, port })
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The redirect URI to register in the authorization request.
    #[must_use]
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/oauth", self.port)
    }

    /// Block until the browser hits the redirect and return its `code`,
    /// verifying `expected_state` (CSRF guard) first.
    ///
    /// # Errors
    /// Returns `Err` when the deadline elapses, the socket fails, the issuer
    /// reports `error=`, or the returned `state` does not match.
    pub fn wait_for_code(
        &self,
        expected_state: &str,
        timeout: std::time::Duration,
    ) -> Result<String, String> {
        use std::io::Read;
        let deadline = std::time::Instant::now()
            .checked_add(timeout)
            .ok_or("sign-in timeout is out of range")?;
        self.listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        loop {
            if std::time::Instant::now() >= deadline {
                return Err("timed out waiting for the sign-in redirect".into());
            }
            let mut stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
                Err(error) => return Err(error.to_string()),
            };
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            let mut buffer = [0u8; 4096];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(buffer.get(..read).unwrap_or(&buffer));
            let line = request.lines().next().unwrap_or("");
            match parse_redirect_line(line) {
                Some(RedirectResult::Code { code, state }) => {
                    let matches = state == expected_state;
                    let _ = write_http_response(
                        &mut stream,
                        if matches {
                            "Signed in. You can close this tab and return to Tilo."
                        } else {
                            "Sign-in state mismatch. Please try again from Tilo."
                        },
                    );
                    if !matches {
                        return Err("authorization state mismatch (possible CSRF)".into());
                    }
                    return Ok(code);
                }
                Some(RedirectResult::Error(error)) => {
                    let _ =
                        write_http_response(&mut stream, "Sign-in failed. You can close this tab.");
                    return Err(format!("authorization denied: {error}"));
                }
                None => {
                    let _ = write_http_response(&mut stream, "Waiting for sign-in…");
                }
            }
        }
    }
}

fn write_http_response(stream: &mut std::net::TcpStream, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    let page = format!(
        "<!doctype html><html><head><meta charset=utf-8><title>Tilo sign-in</title></head>\
         <body style=\"font-family:system-ui;padding:3rem;text-align:center\">{body}</body></html>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        page.len(),
        page
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// The meaningful outcomes of a redirect request line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RedirectResult {
    Code { code: String, state: String },
    Error(String),
}

/// Parse the first request line of the loopback callback
/// (`GET /oauth?code=…&state=… HTTP/1.1`). `None` for any other request.
#[must_use]
pub fn parse_redirect_line(line: &str) -> Option<RedirectResult> {
    let path = line.split_whitespace().nth(1)?;
    let query = path.split_once('?').map_or("", |(_, query)| query);
    let mut code = None;
    let mut state = String::new();
    let mut error = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = value.into_owned(),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(error) = error {
        return Some(RedirectResult::Error(error));
    }
    code.map(|code| RedirectResult::Code { code, state })
}

#[cfg(feature = "http")]
fn post_token_form(client: &OAuthClient, params: &[(&str, &str)]) -> Result<String, String> {
    let http = tilo_net::http::blocking_client()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| error.to_string())?;
    http.post(&client.issuer.token_endpoint)
        .form(params)
        .send()
        .and_then(reqwest::blocking::Response::text)
        .map_err(|error| error.to_string())
}

/// Exchange an authorization `code` for tokens.
///
/// # Errors
/// Returns `Err` if the request fails or the issuer rejects the exchange.
#[cfg(feature = "http")]
pub fn exchange_code(
    client: &OAuthClient,
    redirect_uri: &str,
    code: &str,
    verifier: &str,
) -> Result<OAuthTokens, String> {
    let body = post_token_form(
        client,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &client.client_id),
            ("redirect_uri", redirect_uri),
            ("code", code),
            ("code_verifier", verifier),
        ],
    )?;
    parse_token_response(&body).ok_or_else(|| "the issuer rejected the sign-in code".into())
}

/// Refresh an access token. The issuer may omit a new refresh token; the
/// existing one is carried forward then.
///
/// # Errors
/// Returns `Err` if the request fails or the issuer rejects the refresh.
#[cfg(feature = "http")]
pub fn refresh_tokens(client: &OAuthClient, refresh_token: &str) -> Result<OAuthTokens, String> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("client_id", client.client_id.as_str()),
        ("refresh_token", refresh_token),
    ];
    // Microsoft requires the scope again on refresh; others ignore it.
    if !client.scopes.is_empty() {
        form.push(("scope", client.scopes.as_str()));
    }
    let body = post_token_form(client, &form)?;
    let mut tokens = parse_token_response(&body)
        .ok_or_else(|| "the issuer rejected the token refresh; sign in again".to_string())?;
    if tokens.refresh_token.is_empty() {
        tokens.refresh_token = refresh_token.to_string();
    }
    Ok(tokens)
}

/// Run the installed-app flow: bind the loopback redirect, open the browser
/// at the issuer's consent page, capture the code and exchange it.
///
/// # Errors
/// Returns `Err` if the listener can't be bound, the browser cannot open, the
/// redirect never arrives or is refused, or the exchange fails.
#[cfg(feature = "http")]
pub fn run_loopback_authorization(
    client: &OAuthClient,
    login_hint: &str,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
    timeout: std::time::Duration,
) -> Result<OAuthTokens, String> {
    let server =
        LoopbackServer::bind().map_err(|error| format!("could not bind loopback: {error}"))?;
    let redirect_uri = server.redirect_uri();
    let pkce = Pkce::generate();
    let state = base64url_nopad(uuid::Uuid::new_v4().as_bytes());
    let request = AuthRequest {
        client: client.clone(),
        redirect_uri: redirect_uri.clone(),
        pkce: pkce.clone(),
        state: state.clone(),
        login_hint: login_hint.to_string(),
    };
    open_browser(&request.authorization_url())
        .map_err(|error| format!("could not open sign-in browser: {error}"))?;
    let code = server.wait_for_code(&state, timeout)?;
    exchange_code(client, &redirect_uri, &code, &pkce.verifier).map(OAuthTokens::stamp_now)
}

/// The vault-held access token for an OAuth-signed-in account, refreshed and
/// resealed first when it is about to expire.
///
/// # Errors
/// Returns `Err` when the account has no OAuth token, the token expired with
/// no refresh token, or the refresh or the vault save fails.
#[cfg(feature = "http")]
pub fn fresh_access_token(
    vault: &mut crate::SecretVault,
    account: &crate::AccountId,
    client: &OAuthClient,
) -> Result<zeroize::Zeroizing<String>, String> {
    let secret = vault
        .get(account)
        .ok_or("not signed in (no OAuth token in the vault)")?;
    let mut tokens =
        tokens_from_secret(&secret).ok_or("the stored credential is not an OAuth token")?;
    if tokens.needs_refresh() {
        if tokens.refresh_token.is_empty() {
            return Err("the sign-in expired; sign in again".into());
        }
        tokens = refresh_tokens(client, &tokens.refresh_token)?.stamp_now();
        vault.put(account, &tokens_to_secret(&tokens));
        vault
            .try_save()
            .map_err(|error| format!("could not save refreshed sign-in credentials: {error}"))?;
    }
    Ok(zeroize::Zeroizing::new(std::mem::take(
        &mut tokens.access_token,
    )))
}

/// URL-safe base64 with no padding (RFC 4648 §5), as PKCE mandates.
#[must_use]
pub fn base64url_nopad(input: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input)
}

/// SHA-256 digest (FIPS 180-4), the PKCE S256 hash.
#[must_use]
pub fn sha256(input: &[u8]) -> [u8; 32] {
    Sha256::digest(input).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn google() -> OAuthClient {
        ClientRegistration {
            slot: ClientSlot::Google,
            client_id: "cid.apps.googleusercontent.com".into(),
            tenant: String::new(),
            authority: "https://accounts.google.com".into(),
        }
        .client("https://mail.google.com/")
    }

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            hex::encode(sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex::encode(sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn base64url_matches_rfc4648_vectors() {
        assert_eq!(base64url_nopad(b""), "");
        assert_eq!(base64url_nopad(b"f"), "Zg");
        assert_eq!(base64url_nopad(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64url_nopad(&[0xfb, 0xff, 0xbf]), "-_-_");
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
    fn generate_yields_valid_verifier_and_matching_challenge() {
        let pkce = Pkce::generate();
        assert!((43..=128).contains(&pkce.verifier.len()));
        assert!(
            pkce.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_eq!(
            pkce.challenge,
            Pkce::from_verifier(&pkce.verifier).challenge
        );
    }

    #[test]
    fn google_authorization_url_is_pinned_byte_for_byte() {
        let request = AuthRequest {
            client: google(),
            redirect_uri: "http://127.0.0.1:8080/oauth".into(),
            pkce: Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            state: "xyz 123".into(),
            login_hint: "person@gmail.com".into(),
        };
        assert_eq!(
            request.authorization_url(),
            "https://accounts.google.com/o/oauth2/v2/auth?client_id=cid.apps.googleusercontent.com\
             &response_type=code&redirect_uri=http%3A%2F%2F127.0.0.1%3A8080%2Foauth\
             &scope=https%3A%2F%2Fmail.google.com%2F\
             &code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM\
             &code_challenge_method=S256&state=xyz%20123&access_type=offline\
             &prompt=consent&login_hint=person%40gmail.com"
        );
    }

    #[test]
    fn entra_registration_targets_the_organizations_tenant_and_no_google_params() {
        let registration = ClientRegistration {
            slot: ClientSlot::Entra,
            client_id: "app-guid".into(),
            tenant: "organizations".into(),
            authority: "https://login.microsoftonline.com/".into(),
        };
        let client =
            registration.client("https://cognitiveservices.azure.com/.default offline_access");
        assert_eq!(
            client.issuer.token_endpoint,
            "https://login.microsoftonline.com/organizations/oauth2/v2.0/token"
        );
        let request = AuthRequest {
            client,
            redirect_uri: "http://127.0.0.1:1/oauth".into(),
            pkce: Pkce::generate(),
            state: "s".into(),
            login_hint: String::new(),
        };
        let url = request.authorization_url();
        assert!(
            url.starts_with(
                "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?"
            )
        );
        assert!(!url.contains("access_type"));
        assert!(url.contains("cognitiveservices.azure.com"));
    }

    #[test]
    fn an_unprovisioned_slot_has_no_registration() {
        // The shipped Entra slot carries no client id until one is registered
        // and no override is set in the test environment.
        if std::env::var(ClientSlot::Entra.client_id_env()).is_err() {
            assert!(ClientSlot::Entra.registration().is_none());
        }
    }

    #[test]
    fn token_response_parses_access_and_refresh_and_rejects_errors() {
        let tokens = parse_token_response(
            r#"{"access_token":"ya29.abc","expires_in":3599,"refresh_token":"1//xyz"}"#,
        )
        .expect("valid token payload");
        assert_eq!(tokens.access_token, "ya29.abc");
        assert_eq!(tokens.refresh_token, "1//xyz");
        assert_eq!(tokens.expires_in, 3599);
        assert!(parse_token_response(r#"{"error":"invalid_grant"}"#).is_none());
    }

    #[test]
    fn parse_redirect_line_extracts_code_state_and_errors() {
        assert_eq!(
            parse_redirect_line("GET /oauth?state=xyz&code=4%2F0Afoo&scope=mail HTTP/1.1"),
            Some(RedirectResult::Code {
                code: "4/0Afoo".into(),
                state: "xyz".into()
            })
        );
        assert_eq!(
            parse_redirect_line("GET /oauth?error=access_denied&state=xyz HTTP/1.1"),
            Some(RedirectResult::Error("access_denied".into()))
        );
        assert!(parse_redirect_line("GET /favicon.ico HTTP/1.1").is_none());
    }

    #[test]
    fn loopback_server_captures_code_and_rejects_a_state_mismatch() {
        use std::io::{Read, Write};
        let server = LoopbackServer::bind().expect("bind loopback");
        let port = server.port();
        let client = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
            stream
                .write_all(b"GET /oauth?code=THECODE&state=STATE123 HTTP/1.1\r\n\r\n")
                .expect("write");
            let mut response = Vec::new();
            let _ = stream.read_to_end(&mut response);
            assert!(String::from_utf8_lossy(&response).contains("close this tab"));
        });
        assert_eq!(
            server
                .wait_for_code("STATE123", std::time::Duration::from_secs(5))
                .unwrap(),
            "THECODE"
        );
        client.join().unwrap();

        let server = LoopbackServer::bind().expect("bind");
        let port = server.port();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                let _ = stream.write_all(b"GET /oauth?code=X&state=WRONG HTTP/1.1\r\n\r\n");
            }
        });
        let error = server
            .wait_for_code("EXPECTED", std::time::Duration::from_secs(5))
            .unwrap_err();
        assert!(error.contains("state mismatch"), "{error}");
    }

    #[test]
    fn tokens_round_trip_through_a_vault_secret_and_expire_with_skew() {
        let tokens = OAuthTokens {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            expires_in: 3600,
            obtained_at: 1_000_000,
        };
        let secret = tokens_to_secret(&tokens);
        assert!(secret.password.is_empty());
        assert_eq!(tokens_from_secret(&secret), Some(tokens.clone()));
        assert!(!tokens.is_expired_with_skew(60, 1_000_100));
        assert!(tokens.is_expired_with_skew(60, 1_003_550));
        assert!(
            OAuthTokens {
                expires_in: 0,
                ..tokens
            }
            .is_expired_with_skew(60, 1_000_000)
        );
    }

    /// A configurable issuer: the whole flow — authorization redirect, code
    /// exchange and refresh — against a loopback token endpoint.
    #[cfg(feature = "http")]
    #[test]
    fn the_flow_runs_end_to_end_against_a_configured_issuer() {
        use std::io::{Read, Write};
        let issuer = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let issuer_port = issuer.local_addr().unwrap().port();
        let token_server = std::thread::spawn(move || {
            let mut forms = Vec::new();
            for reply in [
                r#"{"access_token":"first","refresh_token":"keep","expires_in":1}"#,
                r#"{"access_token":"second","expires_in":3600}"#,
            ] {
                let (mut stream, _) = issuer.accept().unwrap();
                let mut request = vec![0u8; 8192];
                let read = stream.read(&mut request).unwrap();
                forms.push(String::from_utf8_lossy(&request[..read]).to_string());
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
            forms
        });
        let client = OAuthClient {
            issuer: OAuthIssuer::microsoft(
                &format!("http://127.0.0.1:{issuer_port}"),
                "organizations",
            ),
            client_id: "test-client".into(),
            scopes: "scope-a offline_access".into(),
            extra_authorization_params: Vec::new(),
        };
        let tokens = run_loopback_authorization(
            &client,
            "",
            |url| {
                let parsed = url::Url::parse(url).map_err(|error| error.to_string())?;
                let pairs: std::collections::HashMap<_, _> =
                    parsed.query_pairs().into_owned().collect();
                let redirect = url::Url::parse(&pairs["redirect_uri"]).unwrap();
                let state = pairs["state"].clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    let mut stream =
                        std::net::TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
                            .unwrap();
                    write!(
                        stream,
                        "GET /oauth?code=CODE&state={state} HTTP/1.1\r\n\r\n"
                    )
                    .unwrap();
                });
                Ok(())
            },
            std::time::Duration::from_secs(10),
        )
        .expect("sign-in completes");
        assert_eq!(tokens.access_token, "first");
        let refreshed = refresh_tokens(&client, &tokens.refresh_token).unwrap();
        assert_eq!(refreshed.access_token, "second");
        assert_eq!(refreshed.refresh_token, "keep");
        let forms = token_server.join().unwrap();
        assert!(forms[0].contains("grant_type=authorization_code"));
        assert!(forms[0].contains("code=CODE"));
        assert!(forms[0].contains("code_verifier="));
        assert!(forms[1].contains("grant_type=refresh_token"));
        assert!(forms[1].contains("scope=scope-a"));
    }
}

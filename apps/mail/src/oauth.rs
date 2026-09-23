//! `OAuth2` (authorization-code + PKCE) for the mail providers that block
//! plain-password IMAP/SMTP — Gmail and Outlook now **require** OAuth, so the
//! setup flow signs the user in through the browser instead of asking for a
//! password.
//!
//! ## What is always compiled
//! The pure pieces — PKCE generation (SHA-256 + base64url), the authorization
//! URL builder, and the token-response parser — need no network and are always
//! built, so they are unit-tested in every configuration (no network, no
//! `live` feature). The `authorization_url` a browser would open and the
//! `code_verifier` an exchange would send are produced here.
//!
//! ## What needs the network (`live`)
//! [`exchange_code`] performs the actual HTTPS POST to the provider's token
//! endpoint; it is gated behind the `live` feature so the headless build and
//! `--shots` never open a socket.
//!
//! ## Client registration
//! A working OAuth flow needs a **registered OAuth client id** for the
//! provider (Google Cloud Console / Azure AD). Tilo ships without one baked in
//! (it would be a shared secret), so the id is read from the environment at
//! runtime — `TILO_MAIL_GOOGLE_CLIENT_ID` / `TILO_MAIL_MICROSOFT_CLIENT_ID`.
//! Without it the setup UI still constructs the (correct) authorization URL but
//! notes that a client id is required to complete sign-in.

use std::fmt::Write as _;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// The OAuth providers we preset for. Both are IMAP/SMTP mail providers that
/// require OAuth rather than a password.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthProvider {
    Google,
    Microsoft,
}

/// The three endpoint URLs + scope string for a provider.
pub struct OAuthEndpoints {
    pub auth_url: &'static str,
    pub token_url: &'static str,
    /// Space-separated OAuth scopes granting IMAP + SMTP access.
    pub scopes: &'static str,
}

impl OAuthProvider {
    /// Recognise a provider from an email domain (lower-cased).
    pub fn from_domain(domain: &str) -> Option<Self> {
        match domain.to_ascii_lowercase().as_str() {
            "gmail.com" | "googlemail.com" => Some(OAuthProvider::Google),
            "outlook.com" | "hotmail.com" | "live.com" | "office365.com" | "msn.com" => {
                Some(OAuthProvider::Microsoft)
            }
            _ => None,
        }
    }

    pub fn endpoints(self) -> OAuthEndpoints {
        match self {
            OAuthProvider::Google => OAuthEndpoints {
                auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
                token_url: "https://oauth2.googleapis.com/token",
                scopes: "https://mail.google.com/",
            },
            OAuthProvider::Microsoft => OAuthEndpoints {
                auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
                token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
                scopes: "offline_access https://outlook.office.com/IMAP.AccessAsUser.All \
                         https://outlook.office.com/SMTP.Send",
            },
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            OAuthProvider::Google => "Google",
            OAuthProvider::Microsoft => "Microsoft",
        }
    }

    /// Environment variable that carries the registered OAuth client id.
    pub fn client_id_env(self) -> &'static str {
        match self {
            OAuthProvider::Google => "TILO_MAIL_GOOGLE_CLIENT_ID",
            OAuthProvider::Microsoft => "TILO_MAIL_MICROSOFT_CLIENT_ID",
        }
    }

    /// The registered client id from the environment, if provisioned. `None`
    /// means "no client id configured" — the flow can be shown but not
    /// completed until one is set.
    pub fn client_id(self) -> Option<String> {
        std::env::var(self.client_id_env())
            .ok()
            .filter(|s| !s.is_empty())
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
    /// challenge. Uses UUID v4 randomness (getrandom-backed) so no extra RNG
    /// dependency is pulled in.
    pub fn generate() -> Self {
        let mut bytes = Vec::with_capacity(32);
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        let verifier = base64url_nopad(&bytes);
        Self::from_verifier(&verifier)
    }

    /// Derive the S256 challenge for a known verifier — the deterministic path
    /// used by the round-trip test.
    pub fn from_verifier(verifier: &str) -> Self {
        let challenge = base64url_nopad(&sha256(verifier.as_bytes()));
        Self {
            verifier: verifier.to_string(),
            challenge,
        }
    }
}

/// A fully-specified authorization request. `authorization_url` renders the URL
/// the user's browser is sent to.
pub struct AuthRequest {
    pub provider: OAuthProvider,
    pub client_id: String,
    pub redirect_uri: String,
    pub pkce: Pkce,
    pub state: String,
    /// The account email, sent as `login_hint` so the provider pre-fills it.
    pub login_hint: String,
}

impl AuthRequest {
    /// Build the browser authorization URL (`response_type=code` + PKCE S256).
    pub fn authorization_url(&self) -> String {
        let e = self.provider.endpoints();
        let mut url = String::from(e.auth_url);
        url.push('?');
        let mut push = |k: &str, v: &str, first: &mut bool| {
            if !*first {
                url.push('&');
            }
            *first = false;
            url.push_str(k);
            url.push('=');
            url.push_str(&percent_encode(v));
        };
        let mut first = true;
        push("client_id", &self.client_id, &mut first);
        push("response_type", "code", &mut first);
        push("redirect_uri", &self.redirect_uri, &mut first);
        push("scope", e.scopes, &mut first);
        push("code_challenge", &self.pkce.challenge, &mut first);
        push("code_challenge_method", "S256", &mut first);
        push("state", &self.state, &mut first);
        // Google needs these to return a refresh token on first consent.
        if self.provider == OAuthProvider::Google {
            push("access_type", "offline", &mut first);
            push("prompt", "consent", &mut first);
        }
        if !self.login_hint.is_empty() {
            push("login_hint", &self.login_hint, &mut first);
        }
        url
    }
}

/// The tokens returned by a successful code exchange. The access token is used
/// as the IMAP/SMTP `XOAUTH2` bearer; the refresh token renews it. Never
/// written to the shared settings JSON — sealed in the [`accounts::SecretVault`]
/// instead (see [`tokens_to_secret`] / [`tokens_from_secret`]).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    /// Lifetime of the access token in seconds, as reported by the provider.
    #[serde(default)]
    pub expires_in: u64,
    /// Unix seconds at which the access token was obtained. Combined with
    /// `expires_in` this yields the absolute expiry, so a reload can tell
    /// whether the cached access token is still usable or must be refreshed.
    #[serde(default)]
    pub obtained_at: u64,
}

impl OAuthTokens {
    /// Stamp `obtained_at` with the current wall clock (call right after an
    /// exchange/refresh so the expiry can be computed on later reloads).
    #[must_use]
    pub fn stamp_now(mut self) -> Self {
        self.obtained_at = unix_now();
        self
    }

    /// Whether the access token is expired (or within `skew` seconds of it, so
    /// a request that takes a moment doesn't race the boundary). A token with
    /// no reported lifetime is treated as already-expired so it is always
    /// refreshed before use.
    pub fn is_expired_with_skew(&self, skew: u64, now: u64) -> bool {
        if self.expires_in == 0 {
            return true;
        }
        now + skew >= self.obtained_at.saturating_add(self.expires_in)
    }

    /// Whether the access token needs refreshing right now (60s skew).
    pub fn needs_refresh(&self) -> bool {
        self.is_expired_with_skew(60, unix_now())
    }
}

/// Parse a provider token-endpoint JSON response into [`OAuthTokens`], stamped
/// with the current time. Returns `None` when the payload is an error (no
/// `access_token`). A refresh response omits `refresh_token` (the old one stays
/// valid), so an empty refresh token here is not an error.
pub fn parse_token_response(json: &str) -> Option<OAuthTokens> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let access_token = v.get("access_token")?.as_str()?.to_string();
    if access_token.is_empty() {
        return None;
    }
    let refresh_token = v
        .get("refresh_token")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    let expires_in = v
        .get("expires_in")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Some(OAuthTokens {
        access_token,
        refresh_token,
        expires_in,
        obtained_at: unix_now(),
    })
}

/// Serialise a token bundle into a vault [`accounts::Secret`] (the credential
/// store keeps it sealed at rest, never in the plaintext account JSON).
pub fn tokens_to_secret(tokens: &OAuthTokens) -> accounts::Secret {
    accounts::Secret {
        password: String::new(),
        token: serde_json::to_string(tokens).ok(),
    }
}

/// Recover a token bundle from a vault [`accounts::Secret`], if it holds one.
pub fn tokens_from_secret(secret: &accounts::Secret) -> Option<OAuthTokens> {
    let raw = secret.token.as_ref()?;
    serde_json::from_str(raw).ok()
}

/// The `XOAUTH2` SASL initial-response string IMAP `AUTHENTICATE` and SMTP
/// `AUTH` expect (RFC 7628 / Google + Microsoft mail): the base64 of
/// `user=<email>^Aauth=Bearer <token>^A^A` where `^A` is the `\x01` control
/// byte. Pure + dependency-free so it is unit-tested in every build.
pub fn xoauth2_sasl(user: &str, access_token: &str) -> String {
    let raw = format!("user={user}\x01auth=Bearer {access_token}\x01\x01");
    base64_standard(raw.as_bytes())
}

/// Standard base64 (RFC 4648 §4, `+`/`/` alphabet, `=` padding) — the encoding
/// SASL/XOAUTH2 mandates (distinct from the URL-safe [`base64url_nopad`] PKCE
/// uses).
pub fn base64_standard(input: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(input)
}

/// Wall-clock unix seconds (0 if the clock is before the epoch).
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Exchange an authorization `code` for tokens at the provider's token
/// endpoint. Network path — compiled only under `live`.
///
/// # Errors
/// Returns `Err` if the HTTPS request fails (build/send/read) or the token
/// endpoint's response can't be parsed as a successful token payload.
#[cfg(feature = "live")]
pub fn exchange_code(
    provider: OAuthProvider,
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    verifier: &str,
) -> Result<OAuthTokens, String> {
    let e = provider.endpoints();
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("code", code),
        ("code_verifier", verifier),
    ];
    let client = tilo_net::http::blocking_client()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(e.token_url)
        .form(&params)
        .send()
        .map_err(|e| e.to_string())?;
    let body = resp.text().map_err(|e| e.to_string())?;
    parse_token_response(&body).ok_or_else(|| format!("token endpoint rejected exchange: {body}"))
}

/// A one-shot loopback HTTP listener for the OAuth authorization-code redirect.
///
/// The installed-app OAuth flow (RFC 8252) redirects the browser to
/// `http://127.0.0.1:<port>/…?code=…` after the user consents. We bind an
/// **ephemeral** port up front (so no privilege and no fixed port collision),
/// hand its `redirect_uri` to the provider, then block on a single inbound
/// connection to read the `code`. Pure `std::net` — no TLS, no async, no extra
/// dependency — so it is compiled and unit-tested in every build.
pub struct LoopbackServer {
    listener: std::net::TcpListener,
    port: u16,
}

impl LoopbackServer {
    /// Bind `127.0.0.1:0` (an OS-chosen free port). `Err` only if the loopback
    /// interface can't be bound at all.
    ///
    /// # Errors
    /// Returns `Err` if the loopback interface can't be bound or its local
    /// address can't be read back.
    pub fn bind() -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        Ok(Self { listener, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The redirect URI to register in the authorization request — must match
    /// exactly what the provider echoes back.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/oauth", self.port)
    }

    /// Block until the browser hits the redirect, then return the `code`,
    /// verifying `expected_state` (CSRF guard) before accepting it. Serves the
    /// browser a small "you can close this tab" page either way. Fails on a
    /// provider-reported `error=`, a state mismatch, a missing code, or the
    /// `timeout` elapsing with no callback.
    ///
    /// # Errors
    /// Returns `Err` if the deadline elapses with no callback, the socket
    /// errors, the provider reports `error=`, or the returned `state`
    /// doesn't match `expected_state`.
    pub fn wait_for_code(
        &self,
        expected_state: &str,
        timeout: std::time::Duration,
    ) -> Result<String, String> {
        use std::io::Read;
        self.listener
            .set_nonblocking(false)
            .map_err(|e| e.to_string())?;
        let deadline = std::time::Instant::now() + timeout;
        // Accept connections until we get the real callback (browsers often
        // fire a favicon/preflight request first); ignore anything without a
        // recognisable auth response until the deadline.
        loop {
            if std::time::Instant::now() >= deadline {
                return Err("timed out waiting for the sign-in redirect".into());
            }
            self.listener
                .set_nonblocking(true)
                .map_err(|e| e.to_string())?;
            let mut stream = match self.listener.accept() {
                Ok((s, _)) => s,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
                Err(e) => return Err(e.to_string()),
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .ok();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            // `Read::read` never returns more bytes than the buffer holds,
            // so `n` is always a valid slice bound; the fallback (the whole
            // buffer) is never actually hit.
            let request = String::from_utf8_lossy(buf.get(..n).unwrap_or(&buf));
            let line = request.lines().next().unwrap_or("");
            match parse_redirect_line(line) {
                Some(RedirectResult::Code { code, state }) => {
                    let ok = state == expected_state;
                    let body = if ok {
                        "Signed in. You can close this tab and return to Mail."
                    } else {
                        "Sign-in state mismatch. Please try again from Mail."
                    };
                    let _ = write_http_response(&mut stream, body);
                    if !ok {
                        return Err("authorization state mismatch (possible CSRF)".into());
                    }
                    return Ok(code);
                }
                Some(RedirectResult::Error(err)) => {
                    let _ =
                        write_http_response(&mut stream, "Sign-in failed. You can close this tab.");
                    return Err(format!("authorization denied: {err}"));
                }
                None => {
                    // Not the callback (e.g. /favicon.ico) — answer and keep
                    // waiting for the real one.
                    let _ = write_http_response(&mut stream, "Waiting for sign-in…");
                }
            }
        }
    }
}

fn write_http_response(stream: &mut std::net::TcpStream, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    let page = format!(
        "<!doctype html><html><head><meta charset=utf-8><title>Tilo Mail</title></head>\
         <body style=\"font-family:system-ui;padding:3rem;text-align:center\">{body}</body></html>"
    );
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        page.len(),
        page
    );
    stream.write_all(resp.as_bytes())?;
    stream.flush()
}

/// The meaningful outcomes of an OAuth redirect GET line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RedirectResult {
    Code { code: String, state: String },
    Error(String),
}

/// Parse the first request line of the loopback callback
/// (`GET /oauth?code=…&state=… HTTP/1.1`) into its `code`+`state`, or the
/// provider-reported `error`. `None` for a request that carries neither (a
/// favicon fetch, a bare `/`). Pure + testable.
pub fn parse_redirect_line(line: &str) -> Option<RedirectResult> {
    let path = line.split_whitespace().nth(1)?;
    let query = path.split_once('?').map_or("", |(_, q)| q);
    let mut code = None;
    let mut state = String::new();
    let mut error = None;
    // The provider redirects a browser here, so the query is
    // `application/x-www-form-urlencoded` — a bare `+` is an encoded space.
    for (k, v) in ui::uri::parse_form_query(query) {
        match k.as_str() {
            "code" => code = Some(v),
            "state" => state = v,
            "error" => error = Some(v),
            _ => {}
        }
    }
    if let Some(e) = error {
        return Some(RedirectResult::Error(e));
    }
    code.map(|code| RedirectResult::Code { code, state })
}

/// Refresh an expired access token using its refresh token. Network path —
/// compiled only under `live`. The provider may omit a new refresh token in
/// the response (the old one stays valid), so the caller should carry it
/// forward when the field comes back empty.
///
/// # Errors
/// Returns `Err` if the HTTPS request fails (build/send/read) or the token
/// endpoint's response can't be parsed as a successful token payload.
#[cfg(feature = "live")]
pub fn refresh_tokens(
    provider: OAuthProvider,
    client_id: &str,
    refresh_token: &str,
) -> Result<OAuthTokens, String> {
    let e = provider.endpoints();
    let params = [
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", refresh_token),
    ];
    let client = tilo_net::http::blocking_client()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(e.token_url)
        .form(&params)
        .send()
        .map_err(|e| e.to_string())?;
    let body = resp.text().map_err(|e| e.to_string())?;
    let mut tokens =
        parse_token_response(&body).ok_or_else(|| format!("token refresh rejected: {body}"))?;
    // Carry the existing refresh token forward when the provider didn't send a
    // fresh one (Google typically doesn't on refresh).
    if tokens.refresh_token.is_empty() {
        tokens.refresh_token = refresh_token.to_string();
    }
    Ok(tokens)
}

/// Run the full installed-app authorization-code + PKCE flow: bind the loopback
/// redirect, open the browser to the provider consent screen, capture the code,
/// and exchange it for tokens. Blocks (on the worker thread) until the user
/// consents or `timeout` elapses. Network path — compiled only under `live`.
///
/// # Errors
/// Returns `Err` if the loopback listener can't be bound, the browser cannot open, the redirect
/// never arrives (timeout, state mismatch, provider `error=`), or the
/// subsequent code exchange fails.
#[cfg(feature = "live")]
pub fn run_loopback_authorization(
    provider: OAuthProvider,
    client_id: &str,
    login_hint: &str,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
    timeout: std::time::Duration,
) -> Result<OAuthTokens, String> {
    let server = LoopbackServer::bind().map_err(|e| format!("could not bind loopback: {e}"))?;
    let redirect_uri = server.redirect_uri();
    let pkce = Pkce::generate();
    let state = base64url_nopad(uuid::Uuid::new_v4().as_bytes());
    let req = AuthRequest {
        provider,
        client_id: client_id.to_string(),
        redirect_uri: redirect_uri.clone(),
        pkce: pkce.clone(),
        state: state.clone(),
        login_hint: login_hint.to_string(),
    };
    open_browser(&req.authorization_url())
        .map_err(|error| format!("could not open sign-in browser: {error}"))?;
    let code = server.wait_for_code(&state, timeout)?;
    exchange_code(provider, client_id, &redirect_uri, &code, &pkce.verifier)
        .map(OAuthTokens::stamp_now)
}

/// URL-safe base64 with no padding (RFC 4648 §5) — the encoding PKCE mandates
/// for both the verifier and the challenge.
pub fn base64url_nopad(input: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input)
}

/// Percent-encode a query-parameter value (RFC 3986 unreserved set kept raw).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// SHA-256 digest of `input` (FIPS 180-4) — the PKCE S256 challenge hash.
pub fn sha256(input: &[u8]) -> [u8; 32] {
    Sha256::digest(input).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "live")]
    #[test]
    fn authorization_reports_browser_failure_before_waiting_for_callback() {
        let result = run_loopback_authorization(
            OAuthProvider::Google,
            "test-client",
            "fixture@example.invalid",
            |url| {
                assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
                Err("fixture browser unavailable".into())
            },
            std::time::Duration::ZERO,
        );
        assert_eq!(
            result.unwrap_err(),
            "could not open sign-in browser: fixture browser unavailable"
        );
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn base64url_matches_rfc4648_vectors() {
        assert_eq!(base64url_nopad(b""), "");
        assert_eq!(base64url_nopad(b"f"), "Zg");
        assert_eq!(base64url_nopad(b"fo"), "Zm8");
        assert_eq!(base64url_nopad(b"foo"), "Zm9v");
        assert_eq!(base64url_nopad(b"foobar"), "Zm9vYmFy");
        // URL-safe alphabet: must never emit '+' or '/' or '='.
        let s = base64url_nopad(&[0xfb, 0xff, 0xbf]);
        assert!(
            !s.contains('+') && !s.contains('/') && !s.contains('='),
            "{s}"
        );
    }

    #[test]
    fn pkce_s256_challenge_matches_rfc7636_appendix_b() {
        // RFC 7636 Appendix B canonical vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let pkce = Pkce::from_verifier(verifier);
        assert_eq!(pkce.verifier, verifier);
        assert_eq!(
            pkce.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn sha256_matches_nist_multi_block_vectors() {
        // FIPS 180-4 example vectors that cross the one-block padding boundary
        // (56 bytes needs a second block for the length) and span two blocks.
        assert_eq!(
            hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex(&sha256(
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
            )),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
        assert_eq!(
            hex(&sha256(&vec![b'a'; 1_000_000])),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn base64_variants_differ_only_in_alphabet_and_padding() {
        // 0xfb 0xff 0xbf hits both alphabet-specific sextets (62 and 63).
        assert_eq!(base64url_nopad(&[0xfb, 0xff, 0xbf]), "-_-_");
        assert_eq!(base64_standard(&[0xfb, 0xff, 0xbf]), "+/+/");
        assert_eq!(base64url_nopad(&[0xfb, 0xff]), "-_8");
        assert_eq!(base64_standard(&[0xfb, 0xff]), "+/8=");
        assert_eq!(base64url_nopad(&[0xfb]), "-w");
        assert_eq!(base64_standard(&[0xfb]), "+w==");
        // Neither encoder wraps long output into lines.
        let long = [0xa5_u8; 300];
        assert_eq!(base64url_nopad(&long).len(), 400);
        assert_eq!(base64_standard(&long).len(), 400);
        assert!(!base64_standard(&long).contains(['\r', '\n']));
    }

    #[test]
    fn xoauth2_sasl_matches_google_documentation_vector() {
        // The worked example in Google's XOAUTH2 protocol documentation.
        assert_eq!(
            xoauth2_sasl(
                "someuser@example.com",
                "ya29.vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg"
            ),
            "dXNlcj1zb21ldXNlckBleGFtcGxlLmNvbQFhdXRoPUJlYXJlciB5YTI5LnZGOWRmdDRxbVRjMk52YjNSbGNrQmhkSFJoZG1semRHRXVZMjl0Q2cBAQ=="
        );
    }

    #[test]
    fn percent_encode_keeps_only_the_rfc3986_unreserved_set() {
        assert_eq!(percent_encode("AZaz09-_.~"), "AZaz09-_.~");
        assert_eq!(
            percent_encode("a b+c/d?e=f&g%h:i@j"),
            "a%20b%2Bc%2Fd%3Fe%3Df%26g%25h%3Ai%40j"
        );
        // Non-ASCII is encoded byte-by-byte as upper-case UTF-8 escapes.
        assert_eq!(percent_encode("café ☕"), "caf%C3%A9%20%E2%98%95");
        assert_eq!(percent_encode("*!'()"), "%2A%21%27%28%29");
    }

    #[test]
    fn google_authorization_url_is_pinned_byte_for_byte() {
        let req = AuthRequest {
            provider: OAuthProvider::Google,
            client_id: "cid.apps.googleusercontent.com".into(),
            redirect_uri: "http://127.0.0.1:8080/oauth".into(),
            pkce: Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            state: "xyz 123".into(),
            login_hint: "person@gmail.com".into(),
        };
        assert_eq!(
            req.authorization_url(),
            "https://accounts.google.com/o/oauth2/v2/auth?client_id=cid.apps.googleusercontent.com\
             &response_type=code&redirect_uri=http%3A%2F%2F127.0.0.1%3A8080%2Foauth\
             &scope=https%3A%2F%2Fmail.google.com%2F\
             &code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM\
             &code_challenge_method=S256&state=xyz%20123&access_type=offline\
             &prompt=consent&login_hint=person%40gmail.com"
        );
    }

    #[test]
    fn generate_yields_valid_verifier_and_matching_challenge() {
        let p = Pkce::generate();
        // RFC 7636: verifier length 43..=128 after base64url of 32 bytes = 43.
        assert!(
            p.verifier.len() >= 43 && p.verifier.len() <= 128,
            "len {}",
            p.verifier.len()
        );
        assert!(
            p.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        // The challenge is the S256 of exactly this verifier.
        assert_eq!(p.challenge, Pkce::from_verifier(&p.verifier).challenge);
    }

    #[test]
    fn google_authorization_url_is_well_formed() {
        let req = AuthRequest {
            provider: OAuthProvider::Google,
            client_id: "cid.apps.googleusercontent.com".into(),
            redirect_uri: "http://127.0.0.1:8080/oauth".into(),
            pkce: Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            state: "xyz123".into(),
            login_hint: "person@gmail.com".into(),
        };
        let url = req.authorization_url();
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("client_id=cid.apps.googleusercontent.com"));
        // redirect_uri + login_hint are percent-encoded.
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A8080%2Foauth"));
        assert!(url.contains("login_hint=person%40gmail.com"));
        // Google-specific refresh-token params.
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
        // The mail scope is present (percent-encoded ':' and '/').
        assert!(url.contains("scope=https%3A%2F%2Fmail.google.com%2F"));
    }

    #[test]
    fn microsoft_url_uses_microsoft_endpoint_and_no_google_params() {
        let req = AuthRequest {
            provider: OAuthProvider::Microsoft,
            client_id: "app-guid".into(),
            redirect_uri: "http://127.0.0.1:8080/oauth".into(),
            pkce: Pkce::generate(),
            state: "s".into(),
            login_hint: String::new(),
        };
        let url = req.authorization_url();
        assert!(url.starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize?"));
        assert!(
            !url.contains("access_type=offline"),
            "google-only param leaked"
        );
        // Dots are unreserved, so the scope substring survives percent-encoding.
        assert!(
            url.contains("IMAP.AccessAsUser.All"),
            "outlook IMAP scope present"
        );
    }

    #[test]
    fn provider_recognised_from_domain() {
        assert_eq!(
            OAuthProvider::from_domain("gmail.com"),
            Some(OAuthProvider::Google)
        );
        assert_eq!(
            OAuthProvider::from_domain("Outlook.com"),
            Some(OAuthProvider::Microsoft)
        );
        assert_eq!(OAuthProvider::from_domain("fastmail.com"), None);
    }

    #[test]
    fn token_response_parses_access_and_refresh() {
        let json = r#"{"access_token":"ya29.abc","expires_in":3599,"refresh_token":"1//xyz","token_type":"Bearer"}"#;
        let t = parse_token_response(json).expect("valid token payload");
        assert_eq!(t.access_token, "ya29.abc");
        assert_eq!(t.refresh_token, "1//xyz");
        assert_eq!(t.expires_in, 3599);
    }

    #[test]
    fn token_response_rejects_error_payload() {
        let json = r#"{"error":"invalid_grant","error_description":"Bad Request"}"#;
        assert!(
            parse_token_response(json).is_none(),
            "error payload must not parse as tokens"
        );
    }

    #[test]
    fn base64_standard_matches_rfc4648_vectors() {
        // RFC 4648 §10 vectors — the padded, +/ alphabet SASL requires.
        assert_eq!(base64_standard(b""), "");
        assert_eq!(base64_standard(b"f"), "Zg==");
        assert_eq!(base64_standard(b"fo"), "Zm8=");
        assert_eq!(base64_standard(b"foo"), "Zm9v");
        assert_eq!(base64_standard(b"foob"), "Zm9vYg==");
        assert_eq!(base64_standard(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_standard(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn xoauth2_sasl_has_exact_wire_shape() {
        // The decoded SASL string must be user=<u>^Aauth=Bearer <t>^A^A, and
        // the base64 must be the standard (padded) alphabet.
        let s = xoauth2_sasl("user@example.com", "ya29.tok");
        // The base64 must encode exactly `user=<u>^Aauth=Bearer <t>^A^A`.
        let expected_raw = "user=user@example.com\x01auth=Bearer ya29.tok\x01\x01";
        assert_eq!(s, base64_standard(expected_raw.as_bytes()));
        // Standard alphabet only (SASL mandates +/ and `=` padding, never the
        // URL-safe -_ variant).
        assert!(
            s.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '+' || ch == '/' || ch == '=')
        );
        assert!(
            !s.contains('-') && !s.contains('_'),
            "must be standard, not URL-safe base64: {s}"
        );
    }

    #[test]
    fn parse_redirect_line_extracts_code_and_state() {
        let line = "GET /oauth?state=xyz&code=4%2F0Afoo&scope=mail HTTP/1.1";
        match parse_redirect_line(line) {
            Some(RedirectResult::Code { code, state }) => {
                assert_eq!(code, "4/0Afoo", "percent-decoded code");
                assert_eq!(state, "xyz");
            }
            other => panic!("expected a code, got {other:?}"),
        }
    }

    #[test]
    fn parse_redirect_line_surfaces_provider_error() {
        let line = "GET /oauth?error=access_denied&state=xyz HTTP/1.1";
        assert_eq!(
            parse_redirect_line(line),
            Some(RedirectResult::Error("access_denied".into()))
        );
    }

    #[test]
    fn parse_redirect_line_ignores_non_callback_requests() {
        assert!(parse_redirect_line("GET /favicon.ico HTTP/1.1").is_none());
        assert!(parse_redirect_line("GET / HTTP/1.1").is_none());
    }

    #[test]
    fn loopback_server_captures_code_from_a_real_local_client() {
        use std::io::Write;
        // Bind the real ephemeral loopback listener, connect a local client to
        // its redirect_uri, and prove wait_for_code returns the captured code.
        // Exercises the production socket path — no network egress.
        let server = LoopbackServer::bind().expect("bind loopback");
        let uri = server.redirect_uri();
        let port = server.port();
        assert!(uri.starts_with(&format!("http://127.0.0.1:{port}/")));
        let client = std::thread::spawn(move || {
            use std::io::Read;
            // Give the acceptor a moment to enter its loop.
            std::thread::sleep(std::time::Duration::from_millis(100));
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
            s.write_all(
                b"GET /oauth?code=THECODE&state=STATE123 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            )
            .expect("write request");
            let mut resp = Vec::new();
            let _ = s.read_to_end(&mut resp);
            // The browser gets a human-readable close-tab page.
            assert!(String::from_utf8_lossy(&resp).contains("close this tab"));
        });
        let code = server
            .wait_for_code("STATE123", std::time::Duration::from_secs(5))
            .expect("capture code");
        assert_eq!(code, "THECODE");
        client.join().unwrap();
    }

    #[test]
    fn loopback_server_rejects_state_mismatch() {
        use std::io::Write;
        let server = LoopbackServer::bind().expect("bind");
        let port = server.port();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                let _ = s.write_all(b"GET /oauth?code=X&state=WRONG HTTP/1.1\r\n\r\n");
            }
        });
        let err = server
            .wait_for_code("EXPECTED", std::time::Duration::from_secs(5))
            .unwrap_err();
        assert!(err.contains("state mismatch"), "{err}");
    }

    #[test]
    fn tokens_round_trip_through_a_vault_secret() {
        let tokens = OAuthTokens {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            expires_in: 3600,
            obtained_at: 1000,
        };
        let secret = tokens_to_secret(&tokens);
        assert!(
            secret.password.is_empty(),
            "OAuth secret carries no password"
        );
        assert_eq!(tokens_from_secret(&secret), Some(tokens));
    }

    #[test]
    fn token_expiry_accounts_for_lifetime_and_skew() {
        let t = OAuthTokens {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_in: 3600,
            obtained_at: 1_000_000,
        };
        // Well within lifetime: not expired.
        assert!(!t.is_expired_with_skew(60, 1_000_100));
        // Past expiry: expired.
        assert!(t.is_expired_with_skew(60, 1_004_601));
        // Within the skew window before expiry: treated as expired so it's
        // refreshed proactively.
        assert!(t.is_expired_with_skew(60, 1_003_550));
        // A token with no reported lifetime is always considered expired.
        let no_life = OAuthTokens {
            expires_in: 0,
            obtained_at: 1_000_000,
            ..t
        };
        assert!(no_life.is_expired_with_skew(60, 1_000_000));
    }
}

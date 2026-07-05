//! OAuth2 (authorization-code + PKCE) for the mail providers that block
//! plain-password IMAP/SMTP — Gmail and Outlook now **require** OAuth, so the
//! setup flow signs the user in through the browser instead of asking for a
//! password.
//!
//! ## What is always compiled
//! The pure pieces — PKCE generation (SHA-256 + base64url), the authorization
//! URL builder, and the token-response parser — are dependency-free and always
//! built, so they are unit-tested in every configuration (no network, no
//! `live` feature). The `authorization_url` a real browser would open and the
//! `code_verifier` an exchange would send are produced here for real.
//!
//! ## What needs the network (`live`)
//! [`exchange_code`] performs the actual HTTPS POST to the provider's token
//! endpoint; it is gated behind the `live` feature so the headless build and
//! `--shots` never open a socket.
//!
//! ## Client registration
//! A working OAuth flow needs a **registered OAuth client id** for the
//! provider (Google Cloud Console / Azure AD). qOS ships without one baked in
//! (it would be a shared secret), so the id is read from the environment at
//! runtime — `QOS_MAIL_GOOGLE_CLIENT_ID` / `QOS_MAIL_MICROSOFT_CLIENT_ID`.
//! Without it the setup UI still constructs the (correct) authorization URL but
//! notes that a client id is required to complete sign-in.

use serde::{Deserialize, Serialize};

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
            OAuthProvider::Google => "QOS_MAIL_GOOGLE_CLIENT_ID",
            OAuthProvider::Microsoft => "QOS_MAIL_MICROSOFT_CLIENT_ID",
        }
    }

    /// The registered client id from the environment, if provisioned. `None`
    /// means "no client id configured" — the flow can be shown but not
    /// completed until one is set.
    pub fn client_id(self) -> Option<String> {
        std::env::var(self.client_id_env()).ok().filter(|s| !s.is_empty())
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
        Self { verifier: verifier.to_string(), challenge }
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
    /// Build the browser authorization URL (response_type=code + PKCE S256).
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
/// written to the shared settings JSON — a real deployment keeps these in the
/// OS secret store.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    /// Lifetime of the access token in seconds, as reported by the provider.
    #[serde(default)]
    pub expires_in: u64,
}

/// Parse a provider token-endpoint JSON response into [`OAuthTokens`]. Returns
/// `None` when the payload is an error (no `access_token`).
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
    let expires_in = v.get("expires_in").and_then(|t| t.as_u64()).unwrap_or(0);
    Some(OAuthTokens { access_token, refresh_token, expires_in })
}

/// Exchange an authorization `code` for tokens at the provider's token
/// endpoint. Network path — compiled only under `live`.
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
    let client = reqwest::blocking::Client::builder()
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

// ---- base64url + SHA-256 (RFC 4648 / FIPS 180-4), inline & dependency-free --

/// URL-safe base64 with no padding (RFC 4648 §5) — the encoding PKCE mandates
/// for both the verifier and the challenge.
pub fn base64url_nopad(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3f) as usize] as char);
        }
    }
    out
}

/// Percent-encode a query-parameter value (RFC 3986 unreserved set kept raw).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// SHA-256 digest of `input` (FIPS 180-4). Small self-contained implementation
/// — the only use is the PKCE S256 challenge.
pub fn sha256(input: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
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
        assert!(!s.contains('+') && !s.contains('/') && !s.contains('='), "{s}");
    }

    #[test]
    fn pkce_s256_challenge_matches_rfc7636_appendix_b() {
        // RFC 7636 Appendix B canonical vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let pkce = Pkce::from_verifier(verifier);
        assert_eq!(pkce.verifier, verifier);
        assert_eq!(pkce.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn generate_yields_valid_verifier_and_matching_challenge() {
        let p = Pkce::generate();
        // RFC 7636: verifier length 43..=128 after base64url of 32 bytes = 43.
        assert!(p.verifier.len() >= 43 && p.verifier.len() <= 128, "len {}", p.verifier.len());
        assert!(p.verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
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
        assert!(!url.contains("access_type=offline"), "google-only param leaked");
        // Dots are unreserved, so the scope substring survives percent-encoding.
        assert!(url.contains("IMAP.AccessAsUser.All"), "outlook IMAP scope present");
    }

    #[test]
    fn provider_recognised_from_domain() {
        assert_eq!(OAuthProvider::from_domain("gmail.com"), Some(OAuthProvider::Google));
        assert_eq!(OAuthProvider::from_domain("Outlook.com"), Some(OAuthProvider::Microsoft));
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
        assert!(parse_token_response(json).is_none(), "error payload must not parse as tokens");
    }
}

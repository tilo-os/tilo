//! Signing in with Discord to link a Discord account: the OAuth 2
//! authorization code flow with the `identify` scope, whose consent page
//! sends the person back to `GET /discord/callback` on this host.
//!
//! The workspace's Discord connector starts the round trip for a signed-in
//! person: it keeps the hash of the round trip's `state`, bound to them, and
//! hands the consent page's address to their device. Coming back, the
//! callback trades the code for the account with the application's client
//! secret (which never leaves this host), and hands the account, the state's
//! hash and the check of a fresh one-time token to the bridge, which offers
//! the account to the person who started it. The browser is sent on to the
//! web entry with the token, where that person, signed in, takes the offer. A
//! round trip someone else started, a state used before, or one that ran out
//! links nothing: the workspace keeps one state per person, uses it up, and
//! offers the account only to its starter, and only the browser that came
//! back holds the token.

use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::PersistentArgs;

/// What came back from Discord, ready to hand to the workspace's Discord
/// bridge: the account Discord vouches for, the round trip it answers, and
/// the check of the token the browser was sent on with.
pub(super) struct Authorized {
    pub user_id: String,
    pub user_name: String,
    pub display_name: Option<String>,
    pub avatar: Option<String>,
    /// SHA-256 (hex) of the round trip's `state`.
    pub state_sha256: String,
    /// SHA-256 (hex) of SHA-256 (hex) of `token`.
    pub link_check: String,
    /// A one-time link token: 256 random bits in hex.
    pub token: String,
}

/// What Discord answers `/users/@me` with, as far as linking reads it.
#[derive(Debug, Deserialize)]
struct Me {
    id: String,
    username: String,
    #[serde(default)]
    global_name: Option<String>,
    #[serde(default)]
    avatar: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Token {
    access_token: String,
}

/// Why a callback linked nothing, in words for the person.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Failed {
    /// They said no on the consent page: back to Tilo, nothing to say.
    Declined,
    /// The callback came without a code or state, or with ones that cannot
    /// be ours.
    Malformed,
    /// Discord did not take the code (used already, run out) or could not
    /// be reached.
    Discord,
}

impl Failed {
    /// What the page says.
    pub(super) const fn said(&self) -> &'static str {
        match self {
            Self::Declined => "Linking was cancelled on Discord.",
            Self::Malformed => {
                "This address doesn't carry a sign-in from Discord. Go back to Tilo and choose Connect Discord again."
            }
            Self::Discord => {
                "Discord didn't confirm the sign-in. It may have taken too long. Go back to Tilo and choose Connect Discord again."
            }
        }
    }
}

/// The callback's settings, the application secret read once at start.
pub(super) struct SignIn {
    client_id: String,
    secret: String,
    redirect_url: String,
    /// Where Discord's API is (`/api/v10` is appended): `https://discord.com`
    /// in production, a local server in tests.
    base: String,
    client: tilo_net::Client,
}

impl SignIn {
    /// The sign-in `args` configure, or `None` without a client id.
    ///
    /// # Errors
    /// A client id without its secret, redirect or consent page, or a secret
    /// file that cannot be read.
    pub(super) fn from_args(args: &PersistentArgs) -> anyhow::Result<Option<Self>> {
        let Some(client_id) = args.discord_client_id.clone() else {
            return Ok(None);
        };
        ensure!(
            client_id.bytes().all(|b| b.is_ascii_digit()),
            "--discord-client-id is the Discord application's numeric id, not {client_id:?}"
        );
        let secret_file = args
            .discord_client_secret_file
            .as_deref()
            .context("--discord-client-id needs --discord-client-secret-file")?;
        let secret = std::fs::read_to_string(secret_file)
            .with_context(|| format!("read {}", secret_file.display()))?
            .trim()
            .to_owned();
        ensure!(
            !secret.is_empty() && !secret.chars().any(char::is_whitespace),
            "{} holds no client secret",
            secret_file.display()
        );
        let redirect_url = args
            .discord_redirect_url
            .clone()
            .context("--discord-client-id needs --discord-redirect-url")?;
        let base = args
            .discord_api_base
            .as_deref()
            .unwrap_or("https://discord.com")
            .trim_end_matches('/')
            .to_owned();
        for (name, url) in [
            ("--discord-redirect-url", &redirect_url),
            ("--discord-api-base", &base),
        ] {
            ensure!(
                url.starts_with("https://") || url.starts_with("http://"),
                "{name} is an http(s) address, not {url:?}"
            );
        }
        let client = tilo_net::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirects(tilo_net::Redirects::None)
            .build()
            .context("the Discord sign-in client")?;
        Ok(Some(Self {
            client_id,
            secret,
            redirect_url,
            base,
            client,
        }))
    }

    /// Trade the callback's `query` for the account Discord vouches for.
    ///
    /// # Errors
    /// Why nothing is linked.
    pub(super) async fn callback(&self, query: &str) -> Result<Authorized, Failed> {
        let pairs = query_pairs(query);
        let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        if get("error").is_some() {
            return Err(Failed::Declined);
        }
        let (Some(code), Some(state)) = (get("code"), get("state")) else {
            return Err(Failed::Malformed);
        };
        if code.is_empty() || code.len() > 256 || state.len() < 16 || state.len() > 256 {
            return Err(Failed::Malformed);
        }
        let api = format!("{}/api/v10", self.base);
        let token = self
            .client
            .post(format!("{api}/oauth2/token"))
            .basic_auth(&self.client_id, Some(&self.secret))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", self.redirect_url.as_str()),
            ])
            .send()
            .await
            .map_err(|_| Failed::Discord)?;
        if !token.status().is_success() {
            return Err(Failed::Discord);
        }
        let token: Token = token.json().await.map_err(|_| Failed::Discord)?;
        let me = self
            .client
            .get(format!("{api}/users/@me"))
            .bearer_auth(&token.access_token)
            .send()
            .await
            .map_err(|_| Failed::Discord)?;
        if !me.status().is_success() {
            return Err(Failed::Discord);
        }
        let me: Me = me.json().await.map_err(|_| Failed::Discord)?;
        if me.id.is_empty() || !me.id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Failed::Discord);
        }
        let link = new_token().ok_or(Failed::Discord)?;
        Ok(Authorized {
            display_name: me
                .global_name
                .filter(|name| !name.is_empty() && *name != me.username),
            user_id: me.id,
            user_name: me.username,
            avatar: me.avatar.filter(|hash| !hash.is_empty()),
            state_sha256: sha256_hex(state.as_bytes()),
            link_check: sha256_hex(sha256_hex(link.as_bytes()).as_bytes()),
            token: link,
        })
    }
}

/// The pairs of a query string, percent-decoded (`+` as a space).
fn query_pairs(query: &str) -> Vec<(String, String)> {
    let decode = |text: &str| {
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut at = 0;
        while let Some(&byte) = bytes.get(at) {
            let hex = bytes
                .get(at.saturating_add(1)..at.saturating_add(3))
                .and_then(|pair| std::str::from_utf8(pair).ok())
                .and_then(|pair| u8::from_str_radix(pair, 16).ok());
            match (byte, hex) {
                (b'%', Some(decoded)) => {
                    out.push(decoded);
                    at = at.saturating_add(3);
                }
                (b'+', _) => {
                    out.push(b' ');
                    at = at.saturating_add(1);
                }
                _ => {
                    out.push(byte);
                    at = at.saturating_add(1);
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            (decode(key), decode(value))
        })
        .collect()
}

/// SHA-256 (lower-case hex) of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A one-time link token: 256 random bits in hex.
fn new_token() -> Option<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(hex::encode(bytes))
}

/// Where the browser goes on to: the web entry, with the token where the
/// page takes link secrets from.
pub(super) fn back_to_tilo(web_entry: &str, token: Option<&str>) -> String {
    let entry = web_entry.split('#').next().unwrap_or(web_entry);
    match token {
        Some(token) => format!("{entry}#?link={token}"),
        None => entry.to_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn args(client_id: Option<&str>, secret: Option<&str>) -> PersistentArgs {
        PersistentArgs {
            discord_client_id: client_id.map(str::to_owned),
            discord_client_secret_file: secret.map(PathBuf::from),
            discord_redirect_url: client_id.map(|_| "https://eu1.example/discord/callback".into()),
            discord_authorize_url: client_id.map(|_| "https://discord.com/oauth2/authorize".into()),
            ..Default::default()
        }
    }

    #[test]
    fn sign_in_is_off_without_a_client_id_and_needs_its_secret_with_one() {
        assert!(SignIn::from_args(&args(None, None)).unwrap().is_none());
        let Err(error) = SignIn::from_args(&args(Some("123"), None)) else {
            panic!("a client id without its secret file started the sign-in");
        };
        assert!(error.to_string().contains("--discord-client-secret-file"));

        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("discord-client-secret");
        std::fs::write(&secret, "s3cret\n").unwrap();
        let sign_in = SignIn::from_args(&args(Some("123"), Some(secret.to_str().unwrap())))
            .unwrap()
            .unwrap();
        assert_eq!(sign_in.client_id, "123");
        assert_eq!(sign_in.base, "https://discord.com");
        assert_eq!(sign_in.redirect_url, "https://eu1.example/discord/callback");

        std::fs::write(&secret, "two words").unwrap();
        assert!(
            SignIn::from_args(&args(Some("123"), Some(secret.to_str().unwrap()))).is_err(),
            "a secret with whitespace in it is not a secret"
        );
        assert!(
            SignIn::from_args(&args(Some("not-a-number"), Some(secret.to_str().unwrap()))).is_err(),
            "the client id is Discord's numeric one"
        );
    }

    #[test]
    fn the_browser_goes_back_with_the_token_as_a_link_secret() {
        assert_eq!(
            back_to_tilo("https://tilo.example/#/x", Some("ab")),
            "https://tilo.example/#?link=ab"
        );
        assert_eq!(
            back_to_tilo("https://tilo.example/", None),
            "https://tilo.example/"
        );
    }

    #[tokio::test]
    async fn the_callback_trades_the_code_for_the_account_and_a_one_time_token() {
        let discord = FakeDiscord::answering("42", "ana", Some("Ana A"), Some("abc"));
        let sign_in = sign_in_at(&discord.base, "123", "s3cret");
        let state = "0123456789abcdef0123456789abcdef";
        let authorized = sign_in
            .callback(&format!("code=the-code&state={state}"))
            .await
            .unwrap();
        assert_eq!(authorized.user_id, "42");
        assert_eq!(authorized.user_name, "ana");
        assert_eq!(authorized.display_name.as_deref(), Some("Ana A"));
        assert_eq!(authorized.avatar.as_deref(), Some("abc"));
        assert_eq!(authorized.state_sha256, sha256_hex(state.as_bytes()));
        assert_eq!(
            authorized.link_check,
            sha256_hex(sha256_hex(authorized.token.as_bytes()).as_bytes())
        );
        assert_eq!(authorized.token.len(), 64, "256 bits in hex");

        let requests = discord.requests();
        assert_eq!(requests.len(), 2, "{requests:?}");
        let token = &requests[0];
        assert!(token.starts_with("POST /api/v10/oauth2/token "));
        assert!(
            token.contains(&format!(
                "authorization: Basic {}",
                base64_std("123:s3cret")
            )),
            "{token}"
        );
        assert!(token.contains("grant_type=authorization_code"), "{token}");
        assert!(token.contains("code=the-code"), "{token}");
        assert!(
            token.contains("redirect_uri=https%3A%2F%2Feu1.example%2Fdiscord%2Fcallback")
                || token.contains("redirect_uri=https://eu1.example/discord/callback"),
            "{token}"
        );
        let me = &requests[1];
        assert!(me.starts_with("GET /api/v10/users/@me "));
        assert!(me.contains("authorization: Bearer access"), "{me}");
    }

    #[tokio::test]
    async fn a_callback_that_links_nothing_says_why() {
        let discord = FakeDiscord::answering("42", "ana", None, None);
        let sign_in = sign_in_at(&discord.base, "123", "s3cret");
        assert_eq!(
            sign_in
                .callback("error=access_denied&state=0123456789abcdef")
                .await
                .err(),
            Some(Failed::Declined)
        );
        assert_eq!(
            sign_in.callback("code=the-code").await.err(),
            Some(Failed::Malformed),
            "no state"
        );
        assert_eq!(
            sign_in.callback("code=the-code&state=short").await.err(),
            Some(Failed::Malformed),
            "a state too short to be one of ours"
        );
        assert!(discord.requests().is_empty(), "nothing was traded");

        // Discord refusing the code: nothing linked, and the person is told.
        let refusing = FakeDiscord::refusing();
        let sign_in = sign_in_at(&refusing.base, "123", "s3cret");
        assert_eq!(
            sign_in
                .callback("code=the-code&state=0123456789abcdef")
                .await
                .err(),
            Some(Failed::Discord)
        );
    }

    fn sign_in_at(base: &str, client_id: &str, secret: &str) -> SignIn {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret");
        std::fs::write(&file, secret).unwrap();
        let mut args = args(Some(client_id), Some(file.to_str().unwrap()));
        args.discord_api_base = Some(base.to_owned());
        SignIn::from_args(&args).unwrap().unwrap()
    }

    fn base64_std(text: &str) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    /// A local stand-in for Discord's API: two answers, and every request it
    /// read kept as its head (request line and headers, then its body).
    struct FakeDiscord {
        base: String,
        seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        _server: std::thread::JoinHandle<()>,
    }

    impl FakeDiscord {
        fn answering(id: &str, name: &str, global: Option<&str>, avatar: Option<&str>) -> Self {
            let mut me = serde_json::json!({"id": id, "username": name});
            if let Some(global) = global {
                me["global_name"] = global.into();
            }
            if let Some(avatar) = avatar {
                me["avatar"] = avatar.into();
            }
            Self::start(vec![
                (true, "{\"access_token\":\"access\"}".to_owned()),
                (true, me.to_string()),
            ])
        }

        fn refusing() -> Self {
            Self::start(vec![(false, "{\"error\":\"invalid_grant\"}".to_owned())])
        }

        /// Serve `answers` in order, each a success or a 400 with `body`.
        fn start(answers: Vec<(bool, String)>) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let kept = std::sync::Arc::clone(&seen);
            let server = std::thread::spawn(move || {
                use std::io::{Read as _, Write as _};
                for (ok, body) in answers {
                    let Ok((mut stream, _)) = listener.accept() else {
                        return;
                    };
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 4096];
                    loop {
                        if let Some(at) = find(&request, b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&request[..at]).to_lowercase();
                            let length = head
                                .lines()
                                .find_map(|line| line.strip_prefix("content-length:"))
                                .and_then(|value| value.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            // A body that long can only come after its head.
                            if request.len() >= at.saturating_add(4).saturating_add(length) {
                                break;
                            }
                        }
                        match stream.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    kept.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&request).into_owned());
                    let status = if ok { "200 OK" } else { "400 Bad Request" };
                    let answer = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(answer.as_bytes());
                }
            });
            Self {
                base,
                seen,
                _server: server,
            }
        }

        fn requests(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}

//! Signing in with Discord to link a Discord account (discord-bridge.md
//! §5, "Linking"): the OAuth 2 authorization code flow with the `identify`
//! scope, whose consent page sends the person back to `GET
//! /discord/callback` on this host.
//!
//! The room's Discord connector starts the round trip for a signed-in
//! person: it keeps the hash of the round trip's `state`, bound to them,
//! and hands the consent page's address to their device. Coming back, the
//! callback trades the code for the account with the application's client
//! secret (which never leaves this host), and hands the account, the
//! state's hash and the check of a fresh one-time token to the room
//! (`OperatorCommand::DiscordAuthorized`), whose bridge offers the account
//! to the person who started it. The browser is sent on to the web entry
//! with the token, where that person, signed in, takes the offer. A
//! round trip someone else started, a state used before, or one that ran
//! out links nothing: the room keeps one state per person, uses it up, and
//! offers the account only to its starter, and only the browser that came
//! back holds the token.

use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tilo_relay_protocol::room_operator::OperatorCommand;

/// The settings: the Discord application's client id and secret, the
/// callback as registered with the application, and Discord's addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignInConfig {
    /// `TILO_ROOMS_DISCORD_CLIENT_ID`.
    pub client_id: String,
    /// `TILO_ROOMS_DISCORD_CLIENT_SECRET_FILE`, by default
    /// `/run/secrets/discord-client-secret` when that is mounted.
    pub client_secret_file: PathBuf,
    /// `TILO_ROOMS_DISCORD_REDIRECT_URL`, by default
    /// `<TILO_ROOMS_PUBLIC_API>/discord/callback`.
    pub redirect_url: String,
    /// `TILO_ROOMS_DISCORD_BASE`, by default `https://discord.com`: where
    /// the consent page (`/oauth2/authorize`) and the API (`/api/v10`) are.
    pub base: String,
}

impl SignInConfig {
    /// The settings `var` gives, or `None` without a client id.
    ///
    /// # Errors
    /// A client id without a secret, or a malformed address.
    pub fn from_vars(
        var: &impl Fn(&str) -> Option<String>,
        public_api: &str,
    ) -> Result<Option<Self>> {
        let set = |name: &str| {
            var(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let Some(client_id) = set("TILO_ROOMS_DISCORD_CLIENT_ID") else {
            return Ok(None);
        };
        ensure!(
            client_id.bytes().all(|b| b.is_ascii_digit()),
            "TILO_ROOMS_DISCORD_CLIENT_ID is the application's numeric id"
        );
        let mounted = PathBuf::from("/run/secrets/discord-client-secret");
        let client_secret_file = set("TILO_ROOMS_DISCORD_CLIENT_SECRET_FILE")
            .map(PathBuf::from)
            .or_else(|| mounted.is_file().then_some(mounted))
            .context("TILO_ROOMS_DISCORD_CLIENT_ID needs TILO_ROOMS_DISCORD_CLIENT_SECRET_FILE")?;
        let redirect_url = set("TILO_ROOMS_DISCORD_REDIRECT_URL")
            .unwrap_or_else(|| format!("{}/discord/callback", public_api.trim_end_matches('/')));
        let base = set("TILO_ROOMS_DISCORD_BASE")
            .unwrap_or_else(|| "https://discord.com".to_owned())
            .trim_end_matches('/')
            .to_owned();
        for (name, url) in [
            ("TILO_ROOMS_DISCORD_REDIRECT_URL", &redirect_url),
            ("TILO_ROOMS_DISCORD_BASE", &base),
        ] {
            ensure!(
                url.starts_with("https://") || url.starts_with("http://"),
                "{name} is an http(s) address"
            );
        }
        Ok(Some(Self {
            client_id,
            client_secret_file,
            redirect_url,
            base,
        }))
    }

    /// What the room needs to offer a round trip: the client id, the
    /// callback and the consent page, as the relay child's arguments. The
    /// secret is not among them.
    #[must_use]
    pub fn relay_args(&self) -> [String; 6] {
        [
            "--discord-client-id".to_owned(),
            self.client_id.clone(),
            "--discord-redirect-url".to_owned(),
            self.redirect_url.clone(),
            "--discord-authorize-url".to_owned(),
            format!("{}/oauth2/authorize", self.base),
        ]
    }
}

/// The callback's settings, the secret read once at start.
pub struct SignIn {
    config: SignInConfig,
    secret: String,
    client: tilo_net::Client,
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
pub enum Failed {
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
    #[must_use]
    pub const fn said(&self) -> &'static str {
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

/// What came back from Discord, ready to hand to the room: the command,
/// and the token to send the browser on with.
pub struct Authorized {
    pub command: OperatorCommand,
    pub token: String,
}

impl SignIn {
    /// The callback for `config`, its secret read from its file.
    ///
    /// # Errors
    /// The secret cannot be read, or the HTTP client not built.
    pub fn new(config: SignInConfig) -> Result<Self> {
        let secret = std::fs::read_to_string(&config.client_secret_file)
            .with_context(|| format!("read {}", config.client_secret_file.display()))?
            .trim()
            .to_owned();
        ensure!(
            !secret.is_empty() && !secret.chars().any(char::is_whitespace),
            "{} holds no client secret",
            config.client_secret_file.display()
        );
        let client = tilo_net::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .redirects(tilo_net::Redirects::None)
            .build()
            .context("the Discord sign-in client")?;
        Ok(Self {
            config,
            secret,
            client,
        })
    }

    /// Trade the callback's `query` for the account Discord vouches for.
    ///
    /// # Errors
    /// Why nothing is linked.
    pub async fn callback(&self, query: &str) -> std::result::Result<Authorized, Failed> {
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
        let api = format!("{}/api/v10", self.config.base);
        let token = self
            .client
            .post(format!("{api}/oauth2/token"))
            .basic_auth(&self.config.client_id, Some(&self.secret))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", self.config.redirect_url.as_str()),
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
            command: OperatorCommand::DiscordAuthorized {
                display_name: me
                    .global_name
                    .filter(|name| !name.is_empty() && *name != me.username),
                user_id: me.id,
                user_name: me.username,
                avatar: me.avatar.filter(|hash| !hash.is_empty()),
                state_sha256: sha256_hex(state.as_bytes()),
                link_check: sha256_hex(sha256_hex(link.as_bytes()).as_bytes()),
            },
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
#[must_use]
pub fn back_to_tilo(web_entry: &str, token: Option<&str>) -> String {
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

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn sign_in_is_off_without_a_client_id_and_needs_a_secret_with_one() {
        assert_eq!(
            SignInConfig::from_vars(&vars(&[]), "https://h").unwrap(),
            None
        );
        assert!(
            SignInConfig::from_vars(&vars(&[("TILO_ROOMS_DISCORD_CLIENT_ID", "1")]), "https://h")
                .is_err()
                || std::path::Path::new("/run/secrets/discord-client-secret").is_file()
        );
        let config = SignInConfig::from_vars(
            &vars(&[
                ("TILO_ROOMS_DISCORD_CLIENT_ID", "123"),
                ("TILO_ROOMS_DISCORD_CLIENT_SECRET_FILE", "/run/x"),
            ]),
            "https://eu1.example/",
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.redirect_url, "https://eu1.example/discord/callback");
        assert_eq!(config.base, "https://discord.com");
        assert_eq!(
            config.relay_args()[5],
            "https://discord.com/oauth2/authorize"
        );
        assert!(
            !config.relay_args().iter().any(|arg| arg.contains("/run/x")),
            "the secret stays with the service"
        );
        assert!(
            SignInConfig::from_vars(
                &vars(&[
                    ("TILO_ROOMS_DISCORD_CLIENT_ID", "abc"),
                    ("TILO_ROOMS_DISCORD_CLIENT_SECRET_FILE", "/run/x"),
                ]),
                "https://h"
            )
            .is_err()
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
}

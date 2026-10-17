//! Provider-confirmed identity binding before a Spotify grant becomes prepared.
use std::io::Read as _;
use std::time::Duration;

use account_custody::{Capability, OAuthIssuer, OAuthUse, SpotifyIdentity, TokenSet};
use accounts::{Account, AccountId};
use serde::Deserialize;

use super::connect::{ConnectionError, Connector, load_store};

const MAX_PROFILE: u64 = 64 * 1024;

#[derive(Deserialize)]
struct Profile {
    id: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

impl Profile {
    fn identity(&self) -> Result<SpotifyIdentity, ConnectionError> {
        let mut value = serde_json::json!({"id":self.id});
        if let Some(account) = &self.account_id {
            value["account_id"] = account.clone().into();
        }
        SpotifyIdentity::from_profile(&value).map_err(|_| ConnectionError::SignInFailed)
    }

    fn matches_saved_binding(&self, config: &serde_json::Value) -> bool {
        if let Some(account) = config.get("spotify_account_id") {
            return account
                .as_str()
                .is_some_and(|v| valid_identity(v) && Some(v) == self.account_id.as_deref());
        }
        config.get("spotify_user_id").is_none_or(|user| {
            user.as_str().filter(|v| valid_identity(v)) == Some(self.id.as_str())
        })
    }
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

impl Connector {
    fn spotify_profile(&self, token: &str) -> Result<Profile, ConnectionError> {
        let endpoint = "https://api.spotify.com/v1/me";
        #[cfg(any(test, feature = "test-support"))]
        let endpoint = self.spotify_api.as_deref().unwrap_or(endpoint);
        let client = tilo_net::http::blocking_client()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ConnectionError::SignInFailed)?;
        let response = client
            .get(endpoint)
            .bearer_auth(token)
            .send()
            .map_err(|_| ConnectionError::SignInFailed)?;
        if !response.status().is_success()
            || response.content_length().is_some_and(|n| n > MAX_PROFILE)
        {
            return Err(ConnectionError::SignInFailed);
        }
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        response
            .take(MAX_PROFILE + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ConnectionError::SignInFailed)?;
        if bytes.len() as u64 > MAX_PROFILE {
            return Err(ConnectionError::SignInFailed);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| ConnectionError::SignInFailed)?;
        SpotifyIdentity::from_profile(&value).map_err(|_| ConnectionError::SignInFailed)?;
        let profile: Profile =
            serde_json::from_value(value).map_err(|_| ConnectionError::SignInFailed)?;
        if !valid_identity(&profile.id)
            || profile
                .account_id
                .as_deref()
                .is_some_and(|v| !valid_identity(v))
        {
            return Err(ConnectionError::SignInFailed);
        }
        Ok(profile)
    }

    pub(super) fn persist_verified_spotify(
        &self,
        id: AccountId,
        mut tokens: TokenSet,
        playback: bool,
    ) -> Result<AccountId, ConnectionError> {
        use zeroize::Zeroize;
        let result = self.persist_spotify_identity(id, &tokens, playback);
        tokens.access_token.zeroize();
        tokens.refresh_token.zeroize();
        result
    }

    fn persist_spotify_identity(
        &self,
        id: AccountId,
        tokens: &TokenSet,
        playback: bool,
    ) -> Result<AccountId, ConnectionError> {
        let profile = self.spotify_profile(&tokens.access_token)?;
        let opposite = AccountId::from(if playback {
            "music.spotify"
        } else {
            "music.spotify.streaming"
        });
        let store = load_store(&self.root)?;
        if let Some(existing) = store.get(&opposite) {
            if existing.type_id.as_str() != "spotify" {
                return Err(ConnectionError::SignInFailed);
            }
            let usage = OAuthUse::new(
                OAuthIssuer::Spotify,
                if playback {
                    Capability::Music
                } else {
                    Capability::MusicStreaming
                },
            );
            let access = self
                .custodian
                .oauth_access_token(&opposite, usage, false)
                .map_err(|_| ConnectionError::SignInFailed)?;
            let previous = self.spotify_profile(access.as_str())?;
            if profile.identity()? != previous.identity()?
                || !previous.matches_saved_binding(&existing.config)
            {
                return Err(ConnectionError::SignInFailed);
            }
        }
        let mut account = Account::new(id, "spotify");
        account.display_name = profile
            .display_name
            .filter(|v| valid_identity(v))
            .unwrap_or_else(|| profile.id.clone());
        account.config = serde_json::json!({"provider":"spotify", "spotify_user_id":profile.id});
        if let Some(stable) = profile.account_id {
            account.config["spotify_account_id"] = stable.into();
        }
        self.persist(account, account_custody::oauth_secret(tokens))
    }
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn profile_server(profiles: Vec<serde_json::Value>) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1/me", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        for profile in profiles {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(15)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 16384);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET /v1/me HTTP/1.1\r\n"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer ")
            );
            let body = serde_json::to_vec(&profile).unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    (endpoint, worker)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connector(
        root: &std::path::Path,
        endpoint: String,
        issuer: &tilo_oauth::fake::FakeAuthorizationServer,
    ) -> Connector {
        let table = account_custody::ClientTable::parse(
            r#"
[spotify]
client_id = "tilo-test-client"
[spotify.scopes]
music = ["user-read-private", "user-library-read"]
music_streaming = ["streaming", "user-read-private"]
"#,
        )
        .unwrap();
        let custody = account_custody::Custodian::for_root(Some(root.to_owned()))
            .with_client_table(table)
            .with_issuer_endpoints(OAuthIssuer::Spotify, issuer.provider());
        let mut connector = Connector::with_custodian(root.to_owned(), custody);
        connector.spotify_api = Some(endpoint);
        connector
    }

    fn finish(connector: &Connector, playback: bool) -> Result<AccountId, ConnectionError> {
        let auth = if playback {
            connector.begin_spotify_streaming()
        } else {
            connector.begin_spotify()
        }
        .unwrap();
        let url = auth.url().to_owned();
        let browser = std::thread::spawn(move || tilo_oauth::fake::browse(&url));
        let result = if playback {
            connector.finish_spotify_streaming(auth)
        } else {
            connector.finish_spotify(auth)
        };
        browser.join().unwrap().unwrap();
        result
    }

    #[test]
    fn spotify_preparation_refuses_cross_account_in_both_consent_orders_without_altering_first_grant()
     {
        for first_playback in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("accounts");
            let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
            let original = serde_json::json!({"id":"ada","account_id":"stable-ada"});
            let other = serde_json::json!({"id":"other","account_id":"stable-other"});
            let (endpoint, api) = profile_server(vec![original.clone(), other, original]);
            let connector = connector(&root, endpoint, &issuer);
            let first = finish(&connector, first_playback).unwrap();
            let metadata = std::fs::read(accounts::store_path_in(&root)).unwrap();
            let secret = std::fs::read(accounts::vault_dir_in(&root).join("vault.enc")).unwrap();
            assert_eq!(
                finish(&connector, !first_playback),
                Err(ConnectionError::SignInFailed)
            );
            api.join().unwrap();
            assert_eq!(
                std::fs::read(accounts::store_path_in(&root)).unwrap(),
                metadata
            );
            assert_eq!(
                std::fs::read(accounts::vault_dir_in(&root).join("vault.enc")).unwrap(),
                secret
            );
            assert_eq!(
                load_store(&root).unwrap().get(&first).unwrap().config["spotify_user_id"],
                "ada"
            );
            assert_eq!(load_store(&root).unwrap().len(), 1);
        }
    }

    #[test]
    fn spotify_preparation_accepts_same_stable_account_with_changed_user_ids_in_both_orders() {
        for first_playback in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("accounts");
            let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
            let (endpoint, api) = profile_server(vec![
                serde_json::json!({"id":"original-name","account_id":"immutable-account"}),
                serde_json::json!({"id":"second-name","account_id":"immutable-account"}),
                serde_json::json!({"id":"renamed-original","account_id":"immutable-account"}),
            ]);
            let connector = connector(&root, endpoint, &issuer);
            let first = finish(&connector, first_playback).unwrap();
            let second = finish(&connector, !first_playback).unwrap();
            api.join().unwrap();
            let store = load_store(&root).unwrap();
            assert_eq!(store.len(), 2);
            assert_eq!(
                store.get(&first).unwrap().config["spotify_user_id"],
                "original-name"
            );
            assert_eq!(
                store.get(&second).unwrap().config["spotify_user_id"],
                "second-name"
            );
            for id in [first, second] {
                assert_eq!(
                    store.get(&id).unwrap().config["spotify_account_id"],
                    "immutable-account"
                );
            }
        }
    }

    #[test]
    fn spotify_preparation_refuses_invalid_provider_identity_before_any_custody_write() {
        for profile in [
            serde_json::json!({"id":""}),
            serde_json::json!({"id":"ada","account_id":"bad\nidentity"}),
            serde_json::json!({"id":"ada","account_id":null}),
            serde_json::json!({"error":"denied"}),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("accounts");
            let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
            let (endpoint, api) = profile_server(vec![profile]);
            let connector = connector(&root, endpoint, &issuer);
            assert_eq!(
                finish(&connector, false),
                Err(ConnectionError::SignInFailed)
            );
            api.join().unwrap();
            assert!(!root.exists());
        }
    }
}

//! Signing in with Hugging Face for the model manager's downloads.
//!
//! Settings signs the person in, in the system browser (PKCE over a
//! loopback redirect) or with a device code on a device without one, and
//! stores the result in the device vault where a pasted access token goes.
//! The model manager reads it from there, renews the access token when
//! Hugging Face gave a refresh token, and otherwise reports the sign-in
//! ended so Settings asks for a new one. A pasted token stays a plain
//! string; a sign-in is stored as [`HubCredential::encode`] writes it.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_oauth::client::{Authorization, OAuthClient};
use tilo_oauth::device::DeviceAuthorization;
use tilo_oauth::{OAuthError, Provider, Registration, TokenSet};
use zeroize::Zeroizing;

use crate::{Capability, ClientTable, OAuthIssuer, OAuthUse};

/// The use a Hugging Face sign-in for downloads is made and renewed with.
pub const HUGGING_FACE_DOWNLOADS: OAuthUse =
    OAuthUse::new(OAuthIssuer::HuggingFace, Capability::ModelDownloads);

/// Who a Hugging Face access token belongs to (`OpenID` Connect userinfo).
pub const USERINFO_ENDPOINT: &str = "https://huggingface.co/oauth/userinfo";

/// How long a browser sign-in is waited for.
const BROWSER_WAIT: Duration = Duration::from_secs(10 * 60);

/// The descriptor and registration of [`HUGGING_FACE_DOWNLOADS`], `None`
/// when this build has no Hugging Face client.
pub fn config() -> Option<(Provider, Registration)> {
    ClientTable::defaults().config(HUGGING_FACE_DOWNLOADS)
}

/// A person signed in with Hugging Face: the grant and who it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubSignIn {
    pub tokens: TokenSet,
    pub username: String,
    /// Their display name, when it differs from the username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The stored form of a sign-in, apart from any pasted token.
#[derive(Serialize, Deserialize)]
struct Stored {
    hugging_face_sign_in: HubSignIn,
}

/// What the device vault holds for Hugging Face.
pub enum HubCredential {
    /// An access token the person pasted.
    Token(Zeroizing<String>),
    SignedIn(HubSignIn),
}

impl HubCredential {
    /// Read the vault's value: a sign-in, else a pasted token.
    pub fn parse(stored: &str) -> Self {
        serde_json::from_str::<Stored>(stored).map_or_else(
            |_| Self::Token(Zeroizing::new(stored.trim().to_owned())),
            |stored| Self::SignedIn(stored.hugging_face_sign_in),
        )
    }

    /// The vault's value for `sign_in`.
    pub fn encode(sign_in: &HubSignIn) -> Zeroizing<String> {
        Zeroizing::new(serde_json::json!({ "hugging_face_sign_in": sign_in }).to_string())
    }
}

impl HubSignIn {
    /// Who `tokens` belong to, asked at `userinfo`.
    ///
    /// # Errors
    /// The userinfo request's failure, or an answer without a username.
    pub fn of(http: &OAuthClient, userinfo: &str, tokens: TokenSet) -> Result<Self, OAuthError> {
        let info = http.userinfo(userinfo, &tokens.access_token)?;
        let text = |key: &str| {
            info.get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let username = text("preferred_username")
            .ok_or_else(|| OAuthError::Protocol("the userinfo answer has no username".into()))?;
        let name = text("name").filter(|name| *name != username);
        Ok(Self {
            tokens,
            username,
            name,
        })
    }

    /// Whether the access token's reported lifetime is over, or within a
    /// minute of it. One reported without a lifetime is used until Hugging
    /// Face refuses it.
    pub fn due(&self) -> bool {
        self.tokens.expires_at().is_some() && self.tokens.needs_refresh()
    }

    /// The sign-in with a fresh access token.
    ///
    /// # Errors
    /// [`OAuthError::InvalidGrant`] when there is no refresh token or
    /// Hugging Face no longer accepts it: the person signs in again.
    pub fn renewed(
        &self,
        http: &OAuthClient,
        (provider, client): &(Provider, Registration),
    ) -> Result<Self, OAuthError> {
        let tokens = http.refresh(provider, client, &self.tokens.refresh_token)?;
        Ok(Self {
            tokens,
            ..self.clone()
        })
    }
}

/// Wait for the browser, redeem the code and ask who signed in. Blocking.
///
/// # Errors
/// Any [`OAuthError`] of the redirect, the exchange or the userinfo request.
pub fn finish_browser(
    authorization: Authorization,
    userinfo: &str,
) -> Result<HubSignIn, OAuthError> {
    let http = OAuthClient::new()?;
    let tokens = authorization.complete(&http, BROWSER_WAIT)?;
    HubSignIn::of(&http, userinfo, tokens)
}

/// Wait until the person entered the device code, then ask who signed in.
/// Blocking.
///
/// # Errors
/// Any [`OAuthError`] of the wait or the userinfo request.
pub fn finish_device(
    authorization: DeviceAuthorization,
    userinfo: &str,
) -> Result<HubSignIn, OAuthError> {
    let http = OAuthClient::new()?;
    let tokens = authorization.complete(&http)?;
    HubSignIn::of(&http, userinfo, tokens)
}

#[cfg(test)]
mod tests {
    use test_http::{Reply, Responder};
    use tilo_oauth::fake::{FakeAuthorizationServer, browse};

    use super::*;

    fn userinfo(server: &Responder) -> String {
        format!("{}/oauth/userinfo", server.url())
    }

    fn person() -> Reply {
        Reply::status(200)
            .header("Content-Type", "application/json")
            .body(r#"{"sub":"1","preferred_username":"ada","name":"Ada Lovelace"}"#)
    }

    #[test]
    fn a_browser_sign_in_knows_who_signed_in_and_round_trips_through_the_vault() {
        let server = FakeAuthorizationServer::start();
        let people = Responder::start(vec![person()]);
        let authorization =
            Authorization::begin(&server.provider(), &Registration::public("hf"), None).unwrap();
        let url = authorization.url().to_owned();
        let browser = std::thread::spawn(move || browse(&url));
        let sign_in = finish_browser(authorization, &userinfo(&people)).unwrap();
        browser.join().unwrap().unwrap();
        assert_eq!(sign_in.username, "ada");
        assert_eq!(sign_in.name.as_deref(), Some("Ada Lovelace"));
        let asked = people.next_request();
        assert_eq!(
            asked.header("authorization"),
            Some(format!("Bearer {}", sign_in.tokens.access_token).as_str())
        );

        let stored = HubCredential::encode(&sign_in);
        let HubCredential::SignedIn(read) = HubCredential::parse(&stored) else {
            panic!("a sign-in reads back as one");
        };
        assert_eq!(read, sign_in);
        let HubCredential::Token(pasted) = HubCredential::parse(" hf_pasted \n") else {
            panic!("a pasted token stays a token");
        };
        assert_eq!(pasted.as_str(), "hf_pasted");
    }

    #[test]
    fn a_due_sign_in_renews_and_one_without_a_refresh_token_must_sign_in_again() {
        let server = FakeAuthorizationServer::start();
        server.accept_refresh_token("refresh-old");
        let config = (server.provider(), Registration::public("hf"));
        let http = OAuthClient::new().unwrap();
        let mut sign_in = HubSignIn {
            tokens: TokenSet {
                access_token: "old".into(),
                refresh_token: "refresh-old".into(),
                expires_in: 60,
                obtained_at: 1,
                scope: None,
            },
            username: "ada".into(),
            name: None,
        };
        assert!(sign_in.due());
        let renewed = sign_in.renewed(&http, &config).unwrap();
        assert_ne!(renewed.tokens.access_token, "old");
        assert_eq!(renewed.tokens.refresh_token, "refresh-old");
        assert_eq!(renewed.username, "ada");
        assert!(!renewed.due());

        sign_in.tokens.refresh_token.clear();
        assert_eq!(
            sign_in.renewed(&http, &config).unwrap_err(),
            OAuthError::InvalidGrant
        );
        // Without a reported lifetime the token is used until refused.
        sign_in.tokens.expires_in = 0;
        assert!(!sign_in.due());
    }

    #[test]
    fn a_device_sign_in_asks_for_the_downloads_scopes() {
        let json = |body: &str| {
            Reply::status(200)
                .header("Content-Type", "application/json")
                .body(body.to_owned())
        };
        let issuer = Responder::start(vec![
            json(
                r#"{"device_code":"D","user_code":"HF-CODE","verification_uri":"https://huggingface.co/oauth/device","interval":0}"#,
            ),
            json(r#"{"access_token":"hf_oauth_x","expires_in":28800,"refresh_token":"r"}"#),
        ]);
        let people = Responder::start(vec![person()]);
        let (mut provider, client) = config().expect("Hugging Face is configured");
        provider.device_authorization_endpoint = Some(format!("{}/oauth/device", issuer.url()));
        provider.token_endpoint = format!("{}/oauth/token", issuer.url());
        let device =
            DeviceAuthorization::begin(&OAuthClient::new().unwrap(), &provider, &client).unwrap();
        assert_eq!(device.user_code(), "HF-CODE");
        let sign_in = finish_device(device, &userinfo(&people)).unwrap();
        assert_eq!(sign_in.tokens.access_token, "hf_oauth_x");
        let asked = issuer.next_request().body_text();
        assert!(
            asked.contains("scope=openid+profile+gated-repos"),
            "{asked}"
        );
        assert!(asked.contains("client_id=3c6f0f73-"), "{asked}");
    }

    #[test]
    fn a_userinfo_answer_without_a_username_is_refused() {
        let people = Responder::start(vec![Reply::status(200).body(r#"{"sub":"1"}"#)]);
        let tokens = TokenSet {
            access_token: "a".into(),
            ..TokenSet::default()
        };
        assert!(matches!(
            HubSignIn::of(&OAuthClient::new().unwrap(), &userinfo(&people), tokens),
            Err(OAuthError::Protocol(_))
        ));
    }
}

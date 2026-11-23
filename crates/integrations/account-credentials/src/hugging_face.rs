//! Signing in with Hugging Face for the model manager's downloads.
//!
//! Settings signs the person in, in the system browser (PKCE over a
//! loopback redirect) or with a device code on a device without one, and
//! keeps the grant as the workspace's Hugging Face account's sign-in, in the
//! workspace's vault where a pasted access token goes. The workspace service
//! renews it for [`HUGGING_FACE_DOWNLOADS`] and releases the model manager
//! only its access token.

use std::time::Duration;

use tilo_oauth::client::{Authorization, OAuthClient};
use tilo_oauth::device::DeviceAuthorization;
use tilo_oauth::{OAuthError, Provider, Registration, TokenSet};

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
fn config() -> Option<(Provider, Registration)> {
    ClientTable::defaults().config(HUGGING_FACE_DOWNLOADS)
}

/// Begin signing in by device code, on a device without a browser: the
/// person opens its page elsewhere and enters its code there.
///
/// # Errors
/// This build has no Hugging Face client, or Hugging Face refused to begin.
pub fn begin_device() -> Result<DeviceAuthorization, OAuthError> {
    let (provider, client) = config()
        .ok_or_else(|| OAuthError::Protocol("this build has no Hugging Face sign-in".into()))?;
    DeviceAuthorization::begin(&OAuthClient::new()?, &provider, &client)
}

/// A person signed in with Hugging Face: the grant and who it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubSignIn {
    pub tokens: TokenSet,
    pub username: String,
    /// Their display name, when it differs from the username.
    pub name: Option<String>,
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

    tilo_test_env::scenario! {
        /// Signing in to Hugging Face, in the browser and by device code.
        fn a_hugging_face_sign_in_knows_its_person_and_scopes;

        phase a_browser_sign_in_knows_who_signed_in;
        {
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
        }

        phase a_device_sign_in_asks_for_the_downloads_scopes;
        {
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

        phase a_userinfo_answer_without_a_username_is_refused;
        {
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
}

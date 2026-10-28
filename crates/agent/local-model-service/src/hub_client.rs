//! The model manager's Hugging Face client and the person's access token.

use std::fs::File;
use std::io::{Read, Write};
use std::time::Duration;

use account_custody::hugging_face::{self, HubCredential, HubSignIn};
use accounts::{CredentialRef, ServiceAuthorization, ServiceCredentials};
use agent_provider_config::local::hub::{
    HUB_ORIGIN, HubAccount, HubTokenState, TOKEN_ID, TOKEN_PURPOSE, TOKEN_SERVICE,
};
use local_model_service::hub::HubApi;
use local_model_service::{HttpsTransport, ServiceError};
use tilo_oauth::OAuthError;
use tilo_oauth::client::OAuthClient;
use zeroize::Zeroizing;

/// Where the token or sign-in Settings stored lives in the device vault.
fn token_binding() -> Result<(CredentialRef, ServiceAuthorization), String> {
    let authorization = ServiceAuthorization::new(TOKEN_SERVICE, TOKEN_PURPOSE, HUB_ORIGIN)
        .map_err(|error| error.to_string())?;
    let reference = authorization
        .credential_ref(TOKEN_ID)
        .map_err(|error| error.to_string())?;
    Ok((reference, authorization))
}

/// The token requests carry, and what Settings is told of it.
pub(crate) struct HubCredentialRead {
    pub(crate) token: Option<Zeroizing<String>>,
    pub(crate) state: HubTokenState,
}

/// The person's Hugging Face credential on this device: a pasted token, or
/// a sign-in's access token, renewed and stored back when it is due.
///
/// # Errors
/// An unreadable vault (never mistaken for "no token"), or a renewal that
/// could not reach Hugging Face.
pub(crate) fn stored_credential() -> Result<HubCredentialRead, String> {
    let (reference, authorization) = token_binding()?;
    let vault = ServiceCredentials::open();
    let Some(stored) = vault
        .resolve(&reference, &authorization)
        .map_err(|error| format!("the Hugging Face token cannot be read: {error}"))?
    else {
        return Ok(HubCredentialRead {
            token: None,
            state: HubTokenState::None,
        });
    };
    match HubCredential::parse(&stored) {
        HubCredential::Token(token) => Ok(HubCredentialRead {
            token: Some(token),
            state: HubTokenState::Pasted,
        }),
        HubCredential::SignedIn(sign_in) => settle(
            sign_in,
            |sign_in| {
                let config = hugging_face::config().ok_or_else(|| {
                    OAuthError::Protocol("this build has no Hugging Face sign-in".into())
                })?;
                sign_in.renewed(&OAuthClient::new()?, &config)
            },
            |renewed| {
                vault
                    .store(&reference, &authorization, &HubCredential::encode(renewed))
                    .map_err(|error| {
                        format!("the renewed Hugging Face sign-in cannot be stored: {error}")
                    })
            },
        ),
    }
}

/// A sign-in's access token: as stored while it lasts, else renewed and
/// saved. One that cannot be renewed is ended, and requests go without it.
fn settle(
    sign_in: HubSignIn,
    renew: impl FnOnce(&HubSignIn) -> Result<HubSignIn, OAuthError>,
    save: impl FnOnce(&HubSignIn) -> Result<(), String>,
) -> Result<HubCredentialRead, String> {
    let account = HubAccount {
        username: sign_in.username.clone(),
        name: sign_in.name.clone(),
    };
    let sign_in = if sign_in.due() {
        match renew(&sign_in) {
            Ok(renewed) => {
                save(&renewed)?;
                renewed
            }
            Err(OAuthError::InvalidGrant) => {
                return Ok(HubCredentialRead {
                    token: None,
                    state: HubTokenState::Ended { account },
                });
            }
            Err(error) => {
                return Err(format!(
                    "the Hugging Face sign-in cannot be renewed: {error}"
                ));
            }
        }
    } else {
        sign_in
    };
    Ok(HubCredentialRead {
        token: Some(Zeroizing::new(sign_in.tokens.access_token)),
        state: HubTokenState::SignedIn { account },
    })
}

/// What `hub-account` answers: the stored credential's state, as JSON.
pub(crate) fn account_state() -> Result<String, String> {
    serde_json::to_string(&stored_credential()?.state).map_err(|error| error.to_string())
}

/// Sign in with Hugging Face by device code, for a device without a
/// browser: the code and its page go to standard error, and the sign-in is
/// stored where Settings stores one.
pub(crate) fn sign_in_with_code() -> Result<Option<String>, String> {
    let (provider, client) =
        hugging_face::config().ok_or("this build has no Hugging Face sign-in")?;
    let http = OAuthClient::new().map_err(|error| error.to_string())?;
    let device = tilo_oauth::device::DeviceAuthorization::begin(&http, &provider, &client)
        .map_err(|error| error.to_string())?;
    eprintln!(
        "To sign in with Hugging Face, open {} and enter {}",
        device.verification_uri(),
        device.user_code()
    );
    let sign_in = hugging_face::finish_device(device, hugging_face::USERINFO_ENDPOINT)
        .map_err(|error| error.to_string())?;
    let (reference, authorization) = token_binding()?;
    ServiceCredentials::open()
        .store(&reference, &authorization, &HubCredential::encode(&sign_in))
        .map_err(|error| error.to_string())?;
    Ok(Some(format!(
        "Signed in to Hugging Face as {}",
        sign_in.username
    )))
}

/// HTTPS to the Hub, with the person's token when one is stored, so their
/// gated and private models answer too. Downloads redirect to the Hub's
/// content servers: redirects are followed only to HTTPS, and the client
/// drops the token when a redirect leaves `huggingface.co`.
pub(crate) struct HubClient {
    client: reqwest::blocking::Client,
    token: Option<Zeroizing<String>>,
}

impl HubClient {
    /// A client with the stored token.
    ///
    /// # Errors
    /// An unreadable vault or an unbuildable client.
    pub(crate) fn new() -> Result<Self, ServiceError> {
        let client = tilo_net::http::blocking_client()
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    attempt.error("too many redirects")
                } else if attempt.url().scheme() != "https" {
                    attempt.error("redirect away from HTTPS")
                } else {
                    attempt.follow()
                }
            }))
            .tls_backend_rustls()
            .connect_timeout(Duration::from_secs(15))
            // Each blocking read waits at most this long; a large download
            // takes as long as it takes and resumes where it stopped.
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|error| ServiceError::Backend(error.to_string()))?;
        Ok(Self {
            client,
            token: stored_credential().map_err(ServiceError::Backend)?.token,
        })
    }

    /// Whether requests carry the person's token.
    pub(crate) const fn signed_in(&self) -> bool {
        self.token.is_some()
    }

    fn request(&self, url: &str) -> Result<reqwest::blocking::RequestBuilder, ServiceError> {
        if !url.starts_with(&format!("{HUB_ORIGIN}/")) {
            return Err(ServiceError::NonHttpsArtifact);
        }
        let request = self.client.get(url);
        Ok(match &self.token {
            Some(token) => request.bearer_auth(token.as_str()),
            None => request,
        })
    }
}

/// What the Hub's refusal means for the person.
fn refusal(status: reqwest::StatusCode, signed_in: bool) -> ServiceError {
    ServiceError::Backend(match status.as_u16() {
        401 if signed_in => format!(
            "Hugging Face no longer accepts the stored sign-in or token ({status}): sign in again in Settings"
        ),
        403 if signed_in => format!(
            "Hugging Face refused access ({status}): accept the model's terms on its page with the account whose token is stored here"
        ),
        401 | 403 => format!(
            "Hugging Face refused access ({status}): this model needs its terms accepted and a Hugging Face token stored here"
        ),
        404 => format!("Hugging Face has no such model or file ({status})"),
        429 => format!("Hugging Face is limiting requests ({status}); try again later"),
        _ => format!("Hugging Face answered {status}"),
    })
}

impl HubApi for HubClient {
    fn get(&mut self, url: &str, limit: usize) -> Result<Vec<u8>, ServiceError> {
        let response = self
            .request(url)?
            .send()
            .map_err(|error| ServiceError::Backend(error.to_string()))?;
        if !response.status().is_success() {
            return Err(refusal(response.status(), self.token.is_some()));
        }
        let mut body = Vec::new();
        response
            .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
            .read_to_end(&mut body)?;
        if body.len() > limit {
            return Err(ServiceError::Backend(
                "Hugging Face's answer is too large".into(),
            ));
        }
        Ok(body)
    }
}

impl HttpsTransport for HubClient {
    fn resume(
        &mut self,
        url: &str,
        start: u64,
        maximum_bytes: u64,
        progress: &mut dyn FnMut(u64),
        destination: &mut File,
    ) -> Result<u64, ServiceError> {
        let response = self
            .request(url)?
            .header(reqwest::header::RANGE, format!("bytes={start}-"))
            .send()
            .map_err(|error| ServiceError::Backend(error.to_string()))?;
        if !response.status().is_success() {
            return Err(refusal(response.status(), self.token.is_some()));
        }
        copy_range(response, start, maximum_bytes, progress, destination)
    }
}

/// Append a ranged response's body to `destination`, refusing a server that
/// ignored the range or sends more than `maximum_bytes`.
pub(crate) fn copy_range(
    mut response: reqwest::blocking::Response,
    start: u64,
    maximum_bytes: u64,
    progress: &mut dyn FnMut(u64),
    destination: &mut File,
) -> Result<u64, ServiceError> {
    if start > 0 {
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(ServiceError::Backend(
                "resumed HTTPS request did not return 206 Partial Content".into(),
            ));
        }
        let range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if !range.starts_with(&format!("bytes {start}-")) {
            return Err(ServiceError::Backend(
                "resumed HTTPS response had an invalid Content-Range".into(),
            ));
        }
    }
    if response
        .content_length()
        .is_some_and(|length| length > maximum_bytes)
    {
        return Err(ServiceError::PartialTooLarge);
    }
    let mut count = 0_u64;
    let mut buffer = vec![0_u8; 32 * 1024];
    loop {
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        // `Read` never reports more bytes than the buffer holds.
        let chunk = buffer.get(..read).ok_or(ServiceError::PartialTooLarge)?;
        let read = u64::try_from(read).map_err(|_| ServiceError::PartialTooLarge)?;
        count = count
            .checked_add(read)
            .ok_or(ServiceError::PartialTooLarge)?;
        if count > maximum_bytes {
            return Err(ServiceError::PartialTooLarge);
        }
        destination.write_all(chunk)?;
        // State is published by the service after every confirmed write,
        // not optimistically from Content-Length.
        progress(count);
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use tilo_oauth::TokenSet;

    use super::*;

    fn sign_in(expires_in: u64, obtained_at: u64) -> HubSignIn {
        HubSignIn {
            tokens: TokenSet {
                access_token: "hf_oauth_old".into(),
                refresh_token: "refresh".into(),
                expires_in,
                obtained_at,
                scope: None,
            },
            username: "ada".into(),
            name: None,
        }
    }

    fn signed_in_as_ada() -> HubTokenState {
        HubTokenState::SignedIn {
            account: HubAccount {
                username: "ada".into(),
                name: None,
            },
        }
    }

    #[test]
    fn a_current_sign_in_is_used_as_stored() {
        let read = settle(
            sign_in(3600, tilo_clock::unix_now()),
            |_| panic!("nothing to renew"),
            |_| panic!("nothing to save"),
        )
        .unwrap();
        assert_eq!(
            read.token.as_deref().map(String::as_str),
            Some("hf_oauth_old")
        );
        assert_eq!(read.state, signed_in_as_ada());
    }

    #[test]
    fn a_due_sign_in_is_renewed_and_saved() {
        let mut saved = None;
        let read = settle(
            sign_in(60, 1),
            |old| {
                let mut renewed = old.clone();
                renewed.tokens.access_token = "hf_oauth_new".into();
                Ok(renewed)
            },
            |renewed| {
                saved = Some(renewed.tokens.access_token.clone());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            read.token.as_deref().map(String::as_str),
            Some("hf_oauth_new")
        );
        assert_eq!(saved.as_deref(), Some("hf_oauth_new"));
        assert_eq!(read.state, signed_in_as_ada());
    }

    #[test]
    fn a_sign_in_that_cannot_be_renewed_has_ended() {
        let read = settle(
            sign_in(60, 1),
            |_| Err(OAuthError::InvalidGrant),
            |_| panic!("nothing to save"),
        )
        .unwrap();
        assert!(read.token.is_none());
        assert!(
            matches!(read.state, HubTokenState::Ended { ref account } if account.username == "ada")
        );
        assert!(
            settle(
                sign_in(60, 1),
                |_| Err(OAuthError::Network("offline".into())),
                |_| panic!("nothing to save"),
            )
            .is_err()
        );
    }
}

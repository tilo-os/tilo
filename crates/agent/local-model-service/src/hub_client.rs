//! The model manager's Hugging Face client and the person's access token.

use std::fs::File;
use std::io::{Read, Write};
use std::time::Duration;

use accounts::{CredentialRef, ServiceAuthorization, ServiceCredentials};
use agent_provider_config::local::hub::{HUB_ORIGIN, TOKEN_ID, TOKEN_PURPOSE, TOKEN_SERVICE};
use local_model_service::hub::HubApi;
use local_model_service::{HttpsTransport, ServiceError};
use zeroize::Zeroizing;

/// Where the token Settings stored lives in the device vault.
fn token_binding() -> Result<(CredentialRef, ServiceAuthorization), String> {
    let authorization = ServiceAuthorization::new(TOKEN_SERVICE, TOKEN_PURPOSE, HUB_ORIGIN)
        .map_err(|error| error.to_string())?;
    let reference = authorization
        .credential_ref(TOKEN_ID)
        .map_err(|error| error.to_string())?;
    Ok((reference, authorization))
}

/// The person's Hugging Face token, when one is stored on this device.
///
/// # Errors
/// An unreadable vault: never mistaken for "no token".
pub(crate) fn stored_token() -> Result<Option<Zeroizing<String>>, String> {
    let (reference, authorization) = token_binding()?;
    ServiceCredentials::open()
        .resolve(&reference, &authorization)
        .map_err(|error| format!("the Hugging Face token cannot be read: {error}"))
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
            token: stored_token().map_err(ServiceError::Backend)?,
        })
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
        401 | 403 if signed_in => format!(
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

//! Requests to Graph and to the pre-authenticated download and upload URLs
//! it hands out, and the mapping of their statuses to [`Error`].

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use cloud_source::{Credential, CredentialSource, Error, Result};
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{AUTHORIZATION, HeaderValue, RETRY_AFTER};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;

use crate::graph::ErrorBody;

/// Largest JSON body read (a delta page of a few hundred items is well
/// under a megabyte).
pub const MAX_JSON_BYTES: u64 = 16 * 1024 * 1024;
/// Largest error body inspected for its code.
const MAX_ERROR_BYTES: u64 = 64 * 1024;

/// Whether a request carries the account's bearer token. Only Graph itself
/// gets it; download and upload URLs are pre-authenticated, and Graph asks
/// clients not to send the token there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auth {
    Bearer,
    Preauthenticated,
}

/// A failed response, with what the error mapping needs.
pub struct Failure {
    pub status: StatusCode,
    pub code: String,
    pub retry_after: Option<Duration>,
    pub location: Option<String>,
}

#[derive(Clone)]
pub struct Http {
    client: Client,
    credentials: Arc<dyn CredentialSource>,
}

impl Http {
    pub fn new(credentials: Arc<dyn CredentialSource>) -> Result<Self> {
        let client = tilo_net::http::blocking_client()
            .user_agent("Tilo-Storage/0.1")
            // Whole-request deadline: large ranged reads and fragment
            // uploads stream for a while; the connect timeout catches dead
            // servers.
            .timeout(Duration::from_secs(30 * 60))
            .connect_timeout(Duration::from_secs(10))
            // Redirects are handled explicitly: a version download answers
            // 302 to a pre-authenticated URL that must not get the token.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::Offline)?;
        Ok(Self {
            client,
            credentials,
        })
    }

    pub fn request(&self, method: Method, url: &str) -> RequestBuilder {
        self.client.request(method, url)
    }

    fn authorize(&self, request: RequestBuilder) -> Result<RequestBuilder> {
        let token = match self.credentials.credential()? {
            Credential::Bearer { token, .. } => token,
            Credential::Basic { .. } | Credential::Session { .. } => {
                return Err(Error::Unsupported("credentials other than an OAuth token"));
            }
        };
        let mut value = HeaderValue::from_str(&format!("Bearer {}", token.as_str()))
            .map_err(|_| Error::Unauthorized)?;
        value.set_sensitive(true);
        Ok(request.header(AUTHORIZATION, value))
    }

    /// Send `request`; any status comes back as `Ok`.
    pub fn send_raw(&self, request: RequestBuilder, auth: Auth) -> Result<Response> {
        let request = match auth {
            Auth::Bearer => self.authorize(request)?,
            Auth::Preauthenticated => request,
        };
        request.send().map_err(|_| Error::Offline)
    }

    /// Send `request`; a non-success status is an [`Error`].
    pub fn send(&self, request: RequestBuilder, auth: Auth) -> Result<Response> {
        let response = self.send_raw(request, auth)?;
        if response.status().is_success() {
            return Ok(response);
        }
        Err(self.error(failure(response), auth))
    }

    /// Send and parse a JSON body.
    pub fn json<T: DeserializeOwned>(&self, request: RequestBuilder, auth: Auth) -> Result<T> {
        read_json(self.send(request, auth)?)
    }

    /// The provider-neutral error for a failed response. A rejected bearer
    /// token is reported to the credential source.
    pub fn error(&self, failure: Failure, auth: Auth) -> Error {
        let Failure {
            status,
            code,
            retry_after,
            ..
        } = failure;
        match status {
            StatusCode::UNAUTHORIZED if auth == Auth::Bearer => {
                self.credentials.rejected();
                Error::Unauthorized
            }
            // A pre-authenticated URL that stopped working has expired.
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Error::Forbidden,
            StatusCode::NOT_FOUND | StatusCode::GONE => Error::NotFound,
            StatusCode::CONFLICT if code == "nameAlreadyExists" => Error::AlreadyExists,
            StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED => {
                Error::Conflict { current: None }
            }
            StatusCode::INSUFFICIENT_STORAGE => Error::InsufficientStorage,
            // 509 is SharePoint's "bandwidth limit exceeded" throttle.
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                Error::RateLimited { retry_after }
            }
            status if status.as_u16() == 509 => Error::RateLimited { retry_after },
            status if status.is_redirection() => Error::InvalidResponse("unexpected redirect"),
            status => Error::Status(status.as_u16()),
        }
    }
}

/// Take a failed response apart.
pub fn failure(response: Response) -> Failure {
    let status = response.status();
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let retry_after = header(RETRY_AFTER).and_then(|value| parse_retry_after(&value));
    let location = header(reqwest::header::LOCATION);
    let mut body = Vec::new();
    let _ = response.take(MAX_ERROR_BYTES).read_to_end(&mut body);
    let code = serde_json::from_slice::<ErrorBody>(&body)
        .map(|body| body.error.code)
        .unwrap_or_default();
    Failure {
        status,
        code,
        retry_after,
        location,
    }
}

/// `Retry-After` in seconds (what Graph sends). HTTP dates are not used by
/// Graph and read as absent.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    value
        .parse::<f64>()
        .ok()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(Duration::from_secs_f64)
}

/// Read a bounded JSON body.
pub fn read_json<T: DeserializeOwned>(response: Response) -> Result<T> {
    let mut body = Vec::new();
    response
        .take(MAX_JSON_BYTES.saturating_add(1))
        .read_to_end(&mut body)
        .map_err(|_| Error::Offline)?;
    if body.len() as u64 > MAX_JSON_BYTES {
        return Err(Error::TooLarge);
    }
    serde_json::from_slice(&body).map_err(|_| Error::InvalidResponse("JSON body"))
}

/// A reader that fails instead of ending early: a body shorter than the
/// bytes it must carry is an error, never a short file.
pub struct Exact<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Exact<R> {
    pub fn new(inner: R, len: u64) -> Self {
        Self {
            inner,
            remaining: len,
        }
    }
}

impl<R: Read> Read for Exact<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let limit = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let window = buf.get_mut(..limit).unwrap_or_default();
        let read = self.inner.read(window)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the download ended before its declared length",
            ));
        }
        self.remaining = self.remaining.saturating_sub(read as u64);
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_reads_seconds() {
        assert_eq!(parse_retry_after(" 7 "), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after("1.5"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    }

    #[test]
    fn short_bodies_are_errors() {
        let mut out = Vec::new();
        let error = Exact::new(&b"abc"[..], 5)
            .read_to_end(&mut out)
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
        let mut out = Vec::new();
        Exact::new(&b"abcdef"[..], 4).read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abcd");
    }
}

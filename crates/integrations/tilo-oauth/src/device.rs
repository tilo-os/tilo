//! RFC 8628 device authorization: a code the person enters on another
//! page, for issuers that register no loopback redirect (GitHub).

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tilo_lock::MutexRecover as _;

use crate::client::OAuthClient;
use crate::{OAuthError, Provider, Refresh, Registration, TokenSet};

const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// The interval RFC 8628 §3.2 prescribes when the issuer names none.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);
/// What `slow_down` adds to the interval (RFC 8628 §3.5).
const SLOW_DOWN: Duration = Duration::from_secs(5);
const MOST_WAIT: Duration = Duration::from_secs(15 * 60);

pub struct DeviceAuthorization {
    provider: Provider,
    client: Registration,
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: Duration,
    expires: Duration,
    cancel: DeviceCancel,
}

#[derive(Clone, Default)]
pub struct DeviceCancel(Arc<(Mutex<bool>, Condvar)>);

impl DeviceCancel {
    pub fn cancel(&self) {
        let (cancelled, wake) = &*self.0;
        *cancelled.lock_recover() = true;
        wake.notify_all();
    }

    /// Wait `duration` unless cancelled first; whether it was cancelled.
    fn wait(&self, duration: Duration) -> bool {
        let (cancelled, wake) = &*self.0;
        let waited = wake
            .wait_timeout_while(cancelled.lock_recover(), duration, |cancelled| !*cancelled)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *waited.0
    }
}

impl DeviceAuthorization {
    /// Ask `provider`'s device authorization endpoint for a code.
    ///
    /// # Errors
    /// [`OAuthError::Protocol`] when the issuer has no device endpoint or
    /// answers something else than a code, [`OAuthError::Network`].
    pub fn begin(
        http: &OAuthClient,
        provider: &Provider,
        client: &Registration,
    ) -> Result<Self, OAuthError> {
        let endpoint = provider
            .device_authorization_endpoint
            .as_deref()
            .ok_or_else(|| OAuthError::Protocol("this issuer has no device sign-in".into()))?;
        let scope = provider.scope();
        let mut params = vec![("client_id", client.client_id.as_str())];
        if !scope.is_empty() {
            params.push(("scope", scope.as_str()));
        }
        let (status, body) = OAuthClient::read(http.post_form(endpoint, &params))?;
        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|_| OAuthError::Protocol(format!("device endpoint answered HTTP {status}")))?;
        if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
            return Err(crate::token_error(error));
        }
        let text = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.is_empty() && text.len() <= 2048)
                .map(str::to_string)
        };
        let seconds = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .map(Duration::from_secs)
        };
        let missing = |what: &str| OAuthError::Protocol(format!("the device answer has no {what}"));
        let verification_uri = text("verification_uri")
            .or_else(|| text("verification_url"))
            .filter(|uri| uri.starts_with("https://") || uri.starts_with("http://127.0.0.1"))
            .ok_or_else(|| missing("verification page"))?;
        Ok(Self {
            provider: provider.clone(),
            client: client.clone(),
            device_code: text("device_code").ok_or_else(|| missing("device code"))?,
            user_code: text("user_code").ok_or_else(|| missing("user code"))?,
            verification_uri,
            interval: seconds("interval").unwrap_or(DEFAULT_INTERVAL),
            expires: seconds("expires_in").unwrap_or(MOST_WAIT).min(MOST_WAIT),
            cancel: DeviceCancel::default(),
        })
    }

    pub fn user_code(&self) -> &str {
        &self.user_code
    }

    pub fn verification_uri(&self) -> &str {
        &self.verification_uri
    }

    pub fn canceller(&self) -> DeviceCancel {
        self.cancel.clone()
    }

    /// Wait until the person entered the code, then return the grant.
    /// Blocking. For an issuer whose grant is renewed by a token exchange
    /// ([`Refresh::CopilotToken`]) the issuer's token becomes the refresh
    /// token and the first exchange is made here, so a sign-in that cannot
    /// be used fails now rather than at the first request.
    ///
    /// # Errors
    /// [`OAuthError::Cancelled`], [`OAuthError::TimedOut`] when the code
    /// expired, [`OAuthError::Denied`], or any token endpoint failure.
    pub fn complete(self, http: &OAuthClient) -> Result<TokenSet, OAuthError> {
        let started = Instant::now();
        let mut interval = self.interval;
        loop {
            if self.cancel.wait(interval) {
                return Err(OAuthError::Cancelled);
            }
            if started.elapsed() >= self.expires {
                return Err(OAuthError::TimedOut);
            }
            let params = [
                ("grant_type", DEVICE_CODE_GRANT),
                ("device_code", self.device_code.as_str()),
                ("client_id", self.client.client_id.as_str()),
            ];
            let (_, body) =
                OAuthClient::read(http.post_form(&self.provider.token_endpoint, &params))?;
            match poll_answer(&body) {
                Poll::Pending => {}
                Poll::SlowDown => interval = interval.saturating_add(SLOW_DOWN),
                Poll::Done(result) => {
                    let tokens = result?;
                    return match &self.provider.refresh {
                        Refresh::CopilotToken { .. } => {
                            http.refresh(&self.provider, &self.client, &tokens.access_token)
                        }
                        Refresh::RefreshToken => Ok(tokens),
                    };
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Poll {
    Pending,
    SlowDown,
    Done(Result<TokenSet, OAuthError>),
}

fn poll_answer(body: &str) -> Poll {
    let error = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    match error.as_deref() {
        Some("authorization_pending") => Poll::Pending,
        Some("slow_down") => Poll::SlowDown,
        Some("expired_token") => Poll::Done(Err(OAuthError::TimedOut)),
        Some("access_denied") => Poll::Done(Err(OAuthError::Denied("access_denied".into()))),
        _ => Poll::Done(crate::parse_token_response(body)),
    }
}

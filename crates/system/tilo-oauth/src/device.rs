//! RFC 8628 device authorization: a code the person enters on another
//! page, for issuers that register no loopback redirect (GitHub).
//!
//! [`DeviceAuthorization::begin`] asks the issuer for a code; the person
//! enters [`DeviceAuthorization::user_code`] at
//! [`DeviceAuthorization::verification_uri`] in any browser, and
//! [`DeviceAuthorization::complete`] (on a worker) asks the token endpoint
//! at the interval the issuer set until they have. A [`DeviceCancel`] ends
//! the wait at once.

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
/// The longest a code is waited for, whatever the issuer says.
const MOST_WAIT: Duration = Duration::from_secs(15 * 60);

/// One device sign-in in progress.
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

/// Ends a pending [`DeviceAuthorization::complete`] from another thread.
#[derive(Clone, Default)]
pub struct DeviceCancel(Arc<(Mutex<bool>, Condvar)>);

impl DeviceCancel {
    /// The wait returns [`OAuthError::Cancelled`].
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

    /// The code the person enters.
    pub fn user_code(&self) -> &str {
        &self.user_code
    }

    /// The page they enter it on.
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

/// What one token request of the wait said.
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

#[cfg(test)]
mod tests {
    use super::*;
    use test_http::{Reply, Responder};

    fn json(body: &serde_json::Value) -> Reply {
        Reply::status(200)
            .header("Content-Type", "application/json")
            .body(body.to_string())
    }

    fn provider(base: &str, refresh: Refresh) -> Provider {
        Provider {
            id: "github".into(),
            label: "GitHub".into(),
            authorization_endpoint: String::new(),
            token_endpoint: format!("{base}/login/oauth/access_token"),
            revocation: crate::Revocation::Unsupported,
            scopes: vec!["read:user".into()],
            authorization_params: Vec::new(),
            refresh,
            device_authorization_endpoint: Some(format!("{base}/login/device/code")),
        }
    }

    #[test]
    fn a_device_code_waits_for_the_person_then_trades_for_a_copilot_token() {
        let replies = vec![
            json(&serde_json::json!({
                "device_code": "DEV", "user_code": "ABCD-1234",
                "verification_uri": "https://github.com/login/device",
                "expires_in": 900, "interval": 0
            })),
            json(&serde_json::json!({"error": "authorization_pending"})),
            json(&serde_json::json!({
                "access_token": "gho_person", "token_type": "bearer", "scope": "read:user"
            })),
            json(&serde_json::json!({"token": "tid=copilot", "expires_at": 4_102_444_800_u64})),
        ];
        let server = Responder::start(replies);
        let copilot = Refresh::CopilotToken {
            endpoint: format!("{}/copilot_internal/v2/token", server.url()),
        };
        let provider = provider(&server.url(), copilot);
        let http = OAuthClient::new().unwrap();
        let client = Registration::public("Iv1.client");
        let device = DeviceAuthorization::begin(&http, &provider, &client).unwrap();
        assert_eq!(device.user_code(), "ABCD-1234");
        assert_eq!(device.verification_uri(), "https://github.com/login/device");
        let tokens = device.complete(&http).unwrap();
        assert_eq!(tokens.access_token, "tid=copilot");
        assert_eq!(tokens.refresh_token, "gho_person");
        assert!(tokens.expires_in > 0);

        let asked = server.next_request();
        assert!(asked.body_text().contains("client_id=Iv1.client"));
        assert!(asked.body_text().contains("scope=read%3Auser"));
        for _ in 0..2 {
            let poll = server.next_request();
            assert!(poll.body_text().contains("device_code=DEV"));
            assert!(
                poll.body_text()
                    .contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code")
            );
        }
        let exchange = server.next_request();
        assert_eq!(exchange.method, "GET");
        assert_eq!(exchange.header("authorization"), Some("token gho_person"));
        assert!(exchange.header("editor-version").is_some());
    }

    #[test]
    fn a_revoked_github_token_is_sign_in_again_and_no_subscription_says_so() {
        for (status, expected) in [
            (401, OAuthError::InvalidGrant),
            (
                404,
                OAuthError::Protocol("this GitHub account has no Copilot subscription".into()),
            ),
        ] {
            let server = Responder::start(vec![Reply::status(status)]);
            let provider = provider(
                &server.url(),
                Refresh::CopilotToken {
                    endpoint: server.url(),
                },
            );
            let http = OAuthClient::new().unwrap();
            assert_eq!(
                http.refresh(&provider, &Registration::public("c"), "gho_x")
                    .unwrap_err(),
                expected
            );
        }
    }

    #[test]
    fn the_wait_ends_when_cancelled_denied_or_expired() {
        assert_eq!(
            poll_answer(r#"{"error":"authorization_pending"}"#),
            Poll::Pending
        );
        assert_eq!(poll_answer(r#"{"error":"slow_down"}"#), Poll::SlowDown);
        assert_eq!(
            poll_answer(r#"{"error":"expired_token"}"#),
            Poll::Done(Err(OAuthError::TimedOut))
        );
        assert!(matches!(
            poll_answer(r#"{"error":"access_denied"}"#),
            Poll::Done(Err(OAuthError::Denied(_)))
        ));
        let cancel = DeviceCancel::default();
        cancel.cancel();
        assert!(cancel.wait(tilo_test_env::hang_bound()));
        assert!(!DeviceCancel::default().wait(Duration::ZERO));

        let server = Responder::start(vec![json(&serde_json::json!({
            "device_code": "D", "user_code": "U",
            "verification_uri": "https://example.com/device", "interval": 3600
        }))]);
        let http = OAuthClient::new().unwrap();
        let device = DeviceAuthorization::begin(
            &http,
            &provider(&server.url(), Refresh::RefreshToken),
            &Registration::public("c"),
        )
        .unwrap();
        let cancel = device.canceller();
        let waiting = std::thread::spawn(move || device.complete(&OAuthClient::new().unwrap()));
        cancel.cancel();
        assert_eq!(waiting.join().unwrap().unwrap_err(), OAuthError::Cancelled);
    }

    #[test]
    fn a_device_answer_without_a_safe_page_is_refused() {
        let server = Responder::start(vec![json(&serde_json::json!({
            "device_code": "D", "user_code": "U", "verification_uri": "javascript:alert(1)"
        }))]);
        let http = OAuthClient::new().unwrap();
        assert!(matches!(
            DeviceAuthorization::begin(
                &http,
                &provider(&server.url(), Refresh::RefreshToken),
                &Registration::public("c"),
            ),
            Err(OAuthError::Protocol(_))
        ));
    }
}

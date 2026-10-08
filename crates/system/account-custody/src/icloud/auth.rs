//! Signing in to iCloud: what Settings drives (through [`super::WebICloud`])
//! to turn an Apple ID into the provider's [`StoredSession`], and to keep
//! that session alive. Obtaining a session is custody's job; the provider,
//! `cloud-source-icloud`, only consumes one.
//!
//! The flow is the one iCloud.com's own sign-in uses, as re-implemented by
//! rclone's `iclouddrive` backend and pyicloud:
//!
//! 1. [`Authenticator::sign_in`] runs the SRP-6a exchange against
//!    `idmsa.apple.com` (`/authorize/signin`, `/federate`, `/signin/init`,
//!    `/signin/complete`) through [`tilo_crypto::srp_apple`]. The password
//!    never leaves the device. A still valid trust token from an earlier
//!    sign-in skips 2FA.
//! 2. When Apple wants a second factor (HTTP 409) the result is
//!    [`SignIn::NeedsCode`]: a code has been pushed to the person's trusted
//!    devices, or sent by SMS when the account has none. Settings shows the
//!    [`PendingSignIn::phones`] for an SMS fallback
//!    ([`Authenticator::send_sms`]) and hands the code to
//!    [`Authenticator::submit_code`].
//! 3. The code is verified, the browser is trusted (`/2sv/trust`, which
//!    yields the long-lived trust token) and `setup.icloud.com/accountLogin`
//!    turns the idmsa session token into iCloud web cookies.
//! 4. Accounts with Advanced Data Protection keep Drive's keys off Apple's
//!    servers; the web API cannot read them. That is reported as
//!    [`AuthError::AdvancedDataProtection`], never as a generic failure.
//!
//! Later, [`Authenticator::refresh`] revalidates the cookies, re-runs
//! `accountLogin` from the session and trust tokens, and (when Settings still
//! holds the password) re-runs SRP with the trust token, all without a person.
//! When only a new 2FA code would help it returns
//! [`AuthError::SessionExpired`].
//!
//! Nothing here logs; secrets are zeroized when dropped and redacted from
//! `Debug`.

use std::fmt;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::blocking::Client;
use reqwest::header::HeaderMap;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;
use zeroize::Zeroizing;

use cloud_source_icloud::auth::{AccountInfo, AuthError, Endpoints, StoredSession};
use cloud_source_icloud::http::{self, Cookies, ICLOUD_ORIGIN, secret};
use tilo_crypto::srp_apple;

/// A trusted phone number that can receive a code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedPhone {
    pub id: u64,
    /// As Apple shows it, mostly obscured (`+•• ••••• •••12`).
    pub number: String,
    /// `sms` or `voice`.
    pub mode: String,
}

/// Where the code the person types came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodeDelivery {
    /// Pushed to the account's trusted Apple devices.
    TrustedDevice,
    /// Sent to a trusted phone.
    Phone { id: u64, mode: String },
}

/// idmsa's per-sign-in state: the headers Apple expects echoed back and the
/// cookies it set.
#[derive(Clone, Default, Serialize, Deserialize)]
struct IdmsaState {
    frame_id: String,
    #[serde(with = "secret")]
    scnt: Zeroizing<String>,
    #[serde(with = "secret")]
    session_id: Zeroizing<String>,
    #[serde(with = "secret")]
    auth_attributes: Zeroizing<String>,
    #[serde(with = "secret")]
    session_token: Zeroizing<String>,
    #[serde(with = "secret")]
    trust_token: Zeroizing<String>,
    account_country: String,
    cookies: Cookies,
}

impl IdmsaState {
    fn new(trust_token: Option<&str>) -> Self {
        Self {
            frame_id: uuid::Uuid::new_v4().to_string(),
            trust_token: Zeroizing::new(trust_token.unwrap_or_default().to_owned()),
            ..Self::default()
        }
    }

    fn absorb(&mut self, headers: &HeaderMap) {
        self.cookies.absorb(headers);
        let take = |name: &str, slot: &mut Zeroizing<String>| {
            if let Some(value) = http::header(headers, name) {
                *slot = Zeroizing::new(value.to_owned());
            }
        };
        take("scnt", &mut self.scnt);
        take("X-Apple-ID-Session-Id", &mut self.session_id);
        take("X-Apple-Auth-Attributes", &mut self.auth_attributes);
        take("X-Apple-Session-Token", &mut self.session_token);
        take("X-Apple-TwoSV-Trust-Token", &mut self.trust_token);
        if let Some(country) = http::header(headers, "X-Apple-ID-Account-Country") {
            country.clone_into(&mut self.account_country);
        }
    }
}

/// A sign-in waiting for its two-factor code. Serializable so Settings can
/// keep it across UI steps; it holds secrets and is zeroized on drop.
#[derive(Clone, Serialize, Deserialize)]
pub struct PendingSignIn {
    apple_id: String,
    state: IdmsaState,
    phones: Vec<TrustedPhone>,
    delivery: CodeDelivery,
}

impl fmt::Debug for PendingSignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingSignIn")
            .field("apple_id", &self.apple_id)
            .field("phones", &self.phones)
            .field("delivery", &self.delivery)
            .finish_non_exhaustive()
    }
}

impl PendingSignIn {
    pub fn apple_id(&self) -> &str {
        &self.apple_id
    }

    /// Trusted phones an SMS or voice code can go to.
    pub fn phones(&self) -> &[TrustedPhone] {
        &self.phones
    }

    /// Where the code the person is about to type was sent.
    pub fn delivery(&self) -> &CodeDelivery {
        &self.delivery
    }
}

/// The outcome of [`Authenticator::sign_in`].
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "short-lived; returned once per sign-in"
)]
pub enum SignIn {
    /// Signed in without a second factor (a trusted browser or trust token).
    Complete(StoredSession),
    /// A code was sent; finish with [`Authenticator::submit_code`].
    NeedsCode(PendingSignIn),
}

/// Runs sign-in, 2FA and refresh against one set of [`Endpoints`].
#[derive(Clone)]
pub struct Authenticator {
    http: Client,
    endpoints: Endpoints,
}

impl fmt::Debug for Authenticator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Authenticator")
            .field("endpoints", &self.endpoints)
            .finish_non_exhaustive()
    }
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The first `serviceErrors[].code`, Apple's reason for a refusal.
    fn service_error(&self) -> Option<String> {
        let body = self.json();
        let errors = body
            .get("serviceErrors")
            .or_else(|| body.get("service_errors"))?;
        errors
            .as_array()?
            .first()?
            .get("code")?
            .as_str()
            .map(ToOwned::to_owned)
    }
}

fn status_error(answer: &Answer) -> AuthError {
    match answer.status.as_u16() {
        429 | 503 => AuthError::RateLimited {
            retry_after: http::retry_after(&answer.headers),
        },
        status => AuthError::Status(status),
    }
}

impl Authenticator {
    /// A client for `endpoints` (Apple's, or a fake server's).
    ///
    /// # Errors
    /// [`AuthError::Offline`] when the HTTPS client cannot be built.
    pub fn new(endpoints: Endpoints) -> Result<Self, AuthError> {
        let http = http::client(Duration::from_secs(60)).map_err(|_| AuthError::Offline)?;
        Ok(Self { http, endpoints })
    }

    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    /// Sign in with an Apple ID and password. With `trust_token` from an
    /// earlier sign-in Apple may skip the second factor.
    ///
    /// # Errors
    /// [`AuthError::InvalidCredentials`] for a wrong Apple ID or password,
    /// [`AuthError::AccountLocked`], [`AuthError::NoTrustedPhone`],
    /// [`AuthError::AdvancedDataProtection`], and transport or protocol
    /// failures.
    pub fn sign_in(
        &self,
        apple_id: &str,
        password: &str,
        trust_token: Option<&str>,
    ) -> Result<SignIn, AuthError> {
        self.sign_in_with(apple_id, password, trust_token, true)
    }

    fn sign_in_with(
        &self,
        apple_id: &str,
        password: &str,
        trust_token: Option<&str>,
        ask_for_code: bool,
    ) -> Result<SignIn, AuthError> {
        // Apple's client-side proof is computed over the lowercased ID.
        let apple_id = apple_id.trim().to_lowercase();
        let mut state = IdmsaState::new(trust_token);
        self.authorize(&mut state)?;
        let federate = self.idmsa(
            &mut state,
            Method::POST,
            "/federate?isRememberMeEnabled=true",
            Some(&json!({"accountName": apple_id, "rememberMe": true})),
        )?;
        if !federate.status.is_success() {
            return Err(match federate.status.as_u16() {
                400 | 401 | 403 | 404 => AuthError::InvalidCredentials,
                _ => status_error(&federate),
            });
        }

        let client = srp_apple::SrpClient::new().map_err(|_| AuthError::Protocol("randomness"))?;
        let init = self.idmsa(
            &mut state,
            Method::POST,
            "/signin/init",
            Some(&json!({
                "a": BASE64.encode(client.public()),
                "accountName": apple_id,
                "protocols": ["s2k", "s2k_fo"],
            })),
        )?;
        if !init.status.is_success() {
            return Err(match init.status.as_u16() {
                400 | 401 | 403 | 404 => AuthError::InvalidCredentials,
                _ => status_error(&init),
            });
        }
        let challenge: SrpChallenge =
            serde_json::from_slice(&init.body).map_err(|_| AuthError::Protocol("signin/init"))?;
        let salt = BASE64
            .decode(challenge.salt.as_bytes())
            .map_err(|_| AuthError::Protocol("salt"))?;
        let server_public = BASE64
            .decode(challenge.b.as_bytes())
            .map_err(|_| AuthError::Protocol("server public value"))?;
        let derived =
            srp_apple::derive_password(password, &salt, challenge.iteration, &challenge.protocol)
                .map_err(|_| AuthError::Protocol("password protocol"))?;
        let proofs = client
            .respond(&apple_id, &derived, &salt, &server_public)
            .map_err(|_| AuthError::Protocol("server public value"))?;

        let trust_tokens: Vec<&str> = if state.trust_token.is_empty() {
            Vec::new()
        } else {
            vec![state.trust_token.as_str()]
        };
        let body = json!({
            "accountName": apple_id,
            "m1": BASE64.encode(proofs.m1),
            "m2": BASE64.encode(proofs.m2),
            "c": challenge.c,
            "rememberMe": true,
            "trustTokens": trust_tokens,
        });
        let complete = self.idmsa(
            &mut state,
            Method::POST,
            "/signin/complete?isRememberMeEnabled=true",
            Some(&body),
        )?;
        match complete.status.as_u16() {
            200 => {}
            409 => {
                if !ask_for_code {
                    return Err(AuthError::SessionExpired);
                }
                return self
                    .begin_second_factor(apple_id, state)
                    .map(SignIn::NeedsCode);
            }
            412 => {
                // Accounts without 2FA acknowledge a notice first.
                let repair = self.idmsa(
                    &mut state,
                    Method::POST,
                    "/repair/complete",
                    Some(&json!({})),
                )?;
                if !repair.status.is_success() {
                    return Err(status_error(&repair));
                }
            }
            401 | 403 => {
                return Err(match complete.service_error().as_deref() {
                    Some("-20209" | "-20283") => AuthError::AccountLocked,
                    _ => AuthError::InvalidCredentials,
                });
            }
            _ => return Err(status_error(&complete)),
        }
        let session = self.account_login(&apple_id, &state)?;
        Ok(SignIn::Complete(session))
    }

    /// Ask for a code: push to trusted devices, or SMS when there are none.
    fn begin_second_factor(
        &self,
        apple_id: String,
        mut state: IdmsaState,
    ) -> Result<PendingSignIn, AuthError> {
        let auth_state = self.auth_state(&mut state).unwrap_or_default();
        let phones = auth_state.phones();
        let mut pending = PendingSignIn {
            apple_id,
            state,
            phones,
            delivery: CodeDelivery::TrustedDevice,
        };
        if auth_state.no_trusted_devices() {
            let phone = pending
                .phones
                .first()
                .map(|phone| phone.id)
                .ok_or(AuthError::NoTrustedPhone)?;
            self.send_sms(&mut pending, phone)?;
        } else {
            // Recent iOS versions no longer push on the 409 alone. A failed
            // push is not fatal: the person can still ask for an SMS.
            let _ = self.idmsa(
                &mut pending.state,
                Method::PUT,
                "/verify/trusteddevice/securitycode",
                None,
            );
        }
        Ok(pending)
    }

    fn auth_state(&self, state: &mut IdmsaState) -> Result<AuthState, AuthError> {
        let answer = self.idmsa(state, Method::GET, "", None)?;
        if !answer.status.is_success() {
            return Err(status_error(&answer));
        }
        serde_json::from_slice(&answer.body).map_err(|_| AuthError::Protocol("auth state"))
    }

    /// Push a fresh code to the trusted devices.
    ///
    /// # Errors
    /// [`AuthError::SignInExpired`] when Apple dropped the pending sign-in,
    /// and transport failures.
    pub fn resend_push(&self, pending: &mut PendingSignIn) -> Result<(), AuthError> {
        let answer = self.idmsa(
            &mut pending.state,
            Method::PUT,
            "/verify/trusteddevice/securitycode",
            None,
        )?;
        if !answer.status.is_success() {
            return Err(self.code_step_error(&answer));
        }
        pending.delivery = CodeDelivery::TrustedDevice;
        Ok(())
    }

    /// Send the code to trusted phone `phone` instead.
    ///
    /// # Errors
    /// As [`Authenticator::resend_push`].
    pub fn send_sms(&self, pending: &mut PendingSignIn, phone: u64) -> Result<(), AuthError> {
        let mode = pending
            .phones
            .iter()
            .find(|candidate| candidate.id == phone)
            .map_or_else(|| "sms".to_owned(), |candidate| candidate.mode.clone());
        let answer = self.idmsa(
            &mut pending.state,
            Method::PUT,
            "/verify/phone",
            Some(&json!({"phoneNumber": {"id": phone}, "mode": mode})),
        )?;
        if !answer.status.is_success() {
            return Err(self.code_step_error(&answer));
        }
        pending.delivery = CodeDelivery::Phone { id: phone, mode };
        Ok(())
    }

    /// Verify the code the person typed, trust this client and log in to
    /// iCloud.
    ///
    /// # Errors
    /// [`AuthError::InvalidCode`], after which `pending` stays usable;
    /// [`AuthError::SignInExpired`], [`AuthError::AdvancedDataProtection`],
    /// and transport or protocol failures.
    pub fn submit_code(
        &self,
        pending: &mut PendingSignIn,
        code: &str,
    ) -> Result<StoredSession, AuthError> {
        let code = code.trim();
        let (path, body) = match &pending.delivery {
            CodeDelivery::TrustedDevice => (
                "/verify/trusteddevice/securitycode",
                json!({"securityCode": {"code": code}}),
            ),
            CodeDelivery::Phone { id, mode } => (
                "/verify/phone/securitycode",
                json!({"securityCode": {"code": code}, "phoneNumber": {"id": id}, "mode": mode}),
            ),
        };
        let answer = self.idmsa(&mut pending.state, Method::POST, path, Some(&body))?;
        // Since mid-2026 idmsa may answer 409 to an accepted code; the
        // session token it still issues is the ground truth.
        let accepted = answer.status.is_success()
            || (answer.status == StatusCode::CONFLICT
                && http::header(&answer.headers, "X-Apple-Session-Token").is_some());
        if !accepted {
            return Err(self.code_step_error(&answer));
        }
        let trust = self.idmsa(&mut pending.state, Method::GET, "/2sv/trust", None)?;
        if !trust.status.is_success() {
            return Err(status_error(&trust));
        }
        if pending.state.trust_token.is_empty() {
            return Err(AuthError::Protocol("no trust token"));
        }
        let session = self.account_login(&pending.apple_id, &pending.state)?;
        if session.needs_second_factor() {
            return Err(AuthError::Protocol("the code was not accepted by iCloud"));
        }
        Ok(session)
    }

    fn code_step_error(&self, answer: &Answer) -> AuthError {
        let _ = self;
        match answer.status.as_u16() {
            400 => AuthError::InvalidCode,
            401 | 403 => AuthError::SignInExpired,
            _ => status_error(answer),
        }
    }

    /// Keep a session alive without a person: revalidate its cookies, then
    /// log in again from its session and trust tokens, then (with the
    /// password Settings holds, if any) re-run SRP with the trust token.
    ///
    /// # Errors
    /// [`AuthError::SessionExpired`] when only a new 2FA code would help;
    /// transport and protocol failures otherwise.
    pub fn refresh(
        &self,
        session: &StoredSession,
        password: Option<&str>,
    ) -> Result<StoredSession, AuthError> {
        match self.validate(session) {
            Err(AuthError::SessionExpired) => {}
            other => return other,
        }
        let mut state = IdmsaState::new(Some(session.trust_token()));
        state.session_token = Zeroizing::new(session.session_token().to_owned());
        session
            .account_country()
            .clone_into(&mut state.account_country);
        match self.account_login(session.apple_id(), &state) {
            Ok(renewed) if !renewed.needs_second_factor() => return Ok(renewed),
            Ok(_) | Err(AuthError::SessionExpired) => {}
            Err(error) => return Err(error),
        }
        let Some(password) = password else {
            return Err(AuthError::SessionExpired);
        };
        match self.sign_in_with(
            session.apple_id(),
            password,
            Some(session.trust_token()),
            false,
        )? {
            SignIn::Complete(renewed) if !renewed.needs_second_factor() => Ok(renewed),
            SignIn::Complete(_) | SignIn::NeedsCode(_) => Err(AuthError::SessionExpired),
        }
    }

    /// Check a session's cookies with `/validate`, absorbing any rotated
    /// cookies.
    ///
    /// # Errors
    /// [`AuthError::SessionExpired`] when they are no longer valid;
    /// transport and protocol failures otherwise.
    pub fn validate(&self, session: &StoredSession) -> Result<StoredSession, AuthError> {
        let (info, cookies) = self.setup(Method::POST, "validate", session.cookies(), None)?;
        let renewed = session.revalidated(&cookies, &info)?;
        if renewed.needs_second_factor() {
            return Err(AuthError::SessionExpired);
        }
        Ok(renewed)
    }

    /// `setup.icloud.com/accountLogin` from the idmsa session token.
    fn account_login(
        &self,
        apple_id: &str,
        state: &IdmsaState,
    ) -> Result<StoredSession, AuthError> {
        if state.session_token.is_empty() {
            return Err(AuthError::Protocol("no session token"));
        }
        let body = Zeroizing::new(
            json!({
                "accountCountryCode": state.account_country,
                "dsWebAuthToken": state.session_token.as_str(),
                "extended_login": true,
                "trustToken": state.trust_token.as_str(),
            })
            .to_string(),
        );
        let (info, cookies) = self.setup(
            Method::POST,
            "accountLogin",
            &Cookies::default(),
            Some(body),
        )?;
        StoredSession::from_login(
            apple_id,
            &state.account_country,
            state.session_token.clone(),
            state.trust_token.clone(),
            cookies,
            &info,
        )
    }

    fn setup(
        &self,
        method: Method,
        path: &str,
        cookies: &Cookies,
        body: Option<Zeroizing<String>>,
    ) -> Result<(AccountInfo, Cookies), AuthError> {
        let url = join(&self.endpoints.setup, path);
        let mut request = self
            .http
            .request(method, url)
            .header("Origin", ICLOUD_ORIGIN)
            .header("Referer", format!("{ICLOUD_ORIGIN}/"))
            .header("Content-Type", "application/json");
        if !cookies.is_empty() {
            request = request.header("Cookie", cookies.header().as_str());
        }
        request = match body {
            Some(body) => request.body(body.as_bytes().to_vec()),
            None => request.header("Content-Length", "0"),
        };
        let response = request.send().map_err(|_| AuthError::Offline)?;
        let status = response.status();
        let headers = response.headers().clone();
        let mut jar = Cookies::default();
        jar.absorb(&headers);
        match status.as_u16() {
            200..=299 => {}
            401 | 421 | 450 => return Err(AuthError::SessionExpired),
            _ => {
                let body = http::read_bounded(response).unwrap_or_default();
                return Err(status_error(&Answer {
                    status,
                    headers,
                    body,
                }));
            }
        }
        let info: AccountInfo =
            http::read_json(response).map_err(|_| AuthError::Protocol("account"))?;
        Ok((info, jar))
    }

    fn authorize(&self, state: &mut IdmsaState) -> Result<(), AuthError> {
        let frame = format!("auth-{}", state.frame_id);
        let mut url = join(&self.endpoints.auth, "../authorize/signin");
        url.query_pairs_mut()
            .append_pair("frame_id", &frame)
            .append_pair("language", "en_US")
            .append_pair("skVersion", "7")
            .append_pair("iframeId", &frame)
            .append_pair("client_id", &self.endpoints.widget_key)
            .append_pair("redirect_uri", ICLOUD_ORIGIN)
            .append_pair("response_type", "code")
            .append_pair("response_mode", "web_message")
            .append_pair("state", &frame)
            .append_pair("authVersion", "latest");
        let response = self
            .http
            .get(url)
            .header("Accept", "*/*")
            .send()
            .map_err(|_| AuthError::Offline)?;
        state.absorb(response.headers());
        if !response.status().is_success() {
            return Err(status_error(&Answer {
                status: response.status(),
                headers: response.headers().clone(),
                body: Vec::new(),
            }));
        }
        Ok(())
    }

    /// One request to `idmsa` with the headers its web client sends.
    fn idmsa(
        &self,
        state: &mut IdmsaState,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Answer, AuthError> {
        let url = join(&self.endpoints.auth, path);
        let frame = format!("auth-{}", state.frame_id);
        let origin = self.endpoints.auth_origin();
        let key = self.endpoints.widget_key.as_str();
        let mut request = self
            .http
            .request(method, url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("Origin", origin.as_str())
            .header("Referer", format!("{origin}/"))
            .header("X-Apple-Widget-Key", key)
            .header("X-Apple-OAuth-Client-Id", key)
            .header("X-Apple-OAuth-Client-Type", "firstPartyAuth")
            .header("X-Apple-OAuth-Redirect-URI", ICLOUD_ORIGIN)
            .header("X-Apple-OAuth-Require-Grant-Code", "true")
            .header("X-Apple-OAuth-Response-Mode", "web_message")
            .header("X-Apple-OAuth-Response-Type", "code")
            .header("X-Apple-OAuth-State", frame.as_str())
            .header("X-Apple-Frame-Id", frame.as_str())
            .header("X-Requested-With", "XMLHttpRequest")
            .header("X-Apple-Mandate-Security-Upgrade", "0")
            .header("X-Apple-I-Require-UE", "true")
            .header(
                "X-Apple-I-FD-Client-Info",
                format!(
                    "{{\"U\":\"{}\",\"L\":\"en-US\",\"Z\":\"GMT+00:00\",\"V\":\"1.1\",\"F\":\"\"}}",
                    http::USER_AGENT
                ),
            );
        for (name, value) in [
            ("scnt", &state.scnt),
            ("X-Apple-ID-Session-Id", &state.session_id),
            ("X-Apple-Auth-Attributes", &state.auth_attributes),
        ] {
            if !value.is_empty() {
                request = request.header(name, value.as_str());
            }
        }
        if !state.cookies.is_empty() {
            request = request.header("Cookie", state.cookies.header().as_str());
        }
        request = match body {
            Some(body) => request.body(Zeroizing::new(body.to_string()).as_bytes().to_vec()),
            None => request.header("Content-Length", "0"),
        };
        let response = request.send().map_err(|_| AuthError::Offline)?;
        let status = response.status();
        let headers = response.headers().clone();
        state.absorb(&headers);
        let body = http::read_bounded(response).map_err(|_| AuthError::Offline)?;
        Ok(Answer {
            status,
            headers,
            body,
        })
    }
}

/// `base` + `/path` (a leading `..` climbs one segment), keeping `base`'s
/// own path.
fn join(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let mut base_path = base.path().trim_end_matches('/').to_owned();
    let mut rest = path.trim_start_matches('/');
    while let Some(stripped) = rest.strip_prefix("../") {
        if let Some(cut) = base_path.rfind('/') {
            base_path.truncate(cut);
        }
        rest = stripped;
    }
    if rest.is_empty() {
        url.set_path(&base_path);
    } else {
        url.set_path(&format!("{base_path}/{rest}"));
    }
    url.set_query((!query.is_empty()).then_some(query));
    url
}

#[derive(Deserialize)]
struct SrpChallenge {
    iteration: u32,
    salt: String,
    protocol: String,
    b: String,
    #[serde(default)]
    c: Value,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AuthState {
    trusted_phone_numbers: Vec<PhoneWire>,
    trusted_phone_number: Option<PhoneWire>,
    no_trusted_devices: Option<bool>,
    phone_number_verification: Option<Box<AuthState>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PhoneWire {
    id: u64,
    number_with_dial_code: String,
    obfuscated_number: String,
    push_mode: String,
}

impl AuthState {
    /// Some accounts nest everything under `phoneNumberVerification`.
    fn inner(&self) -> &Self {
        match &self.phone_number_verification {
            Some(inner)
                if self.trusted_phone_numbers.is_empty() && self.trusted_phone_number.is_none() =>
            {
                inner
            }
            _ => self,
        }
    }

    fn no_trusted_devices(&self) -> bool {
        self.inner().no_trusted_devices.unwrap_or(false)
    }

    fn phones(&self) -> Vec<TrustedPhone> {
        let inner = self.inner();
        let wires: Vec<&PhoneWire> = if inner.trusted_phone_numbers.is_empty() {
            inner.trusted_phone_number.iter().collect()
        } else {
            inner.trusted_phone_numbers.iter().collect()
        };
        wires
            .into_iter()
            .map(|wire| TrustedPhone {
                id: wire.id,
                number: if wire.obfuscated_number.is_empty() {
                    wire.number_with_dial_code.clone()
                } else {
                    wire.obfuscated_number.clone()
                },
                mode: if wire.push_mode.is_empty() {
                    "sms".to_owned()
                } else {
                    wire.push_mode.clone()
                },
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_paths_join_under_their_base() {
        let auth = Url::parse("https://idmsa.apple.com/appleauth/auth").unwrap();
        assert_eq!(
            join(&auth, "/signin/init").as_str(),
            "https://idmsa.apple.com/appleauth/auth/signin/init"
        );
        assert_eq!(
            join(&auth, "../authorize/signin").as_str(),
            "https://idmsa.apple.com/appleauth/authorize/signin"
        );
        assert_eq!(
            join(&auth, "/federate?isRememberMeEnabled=true").as_str(),
            "https://idmsa.apple.com/appleauth/auth/federate?isRememberMeEnabled=true"
        );
        assert_eq!(
            join(&auth, "").as_str(),
            "https://idmsa.apple.com/appleauth/auth"
        );
    }

    #[test]
    fn nested_auth_state_is_unwrapped() {
        let state: AuthState = serde_json::from_value(json!({
            "phoneNumberVerification": {
                "trustedPhoneNumber": {"id": 2, "obfuscatedNumber": "•••• 12", "pushMode": "sms"},
                "noTrustedDevices": true
            }
        }))
        .unwrap();
        assert!(state.no_trusted_devices());
        assert_eq!(
            state.phones(),
            vec![TrustedPhone {
                id: 2,
                number: "•••• 12".into(),
                mode: "sms".into()
            }]
        );
    }
}

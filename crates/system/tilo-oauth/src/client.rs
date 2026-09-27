//! The blocking token client and the browser round trip.

use std::time::Duration;

use crate::loopback::{LoopbackCancel, LoopbackServer};
use crate::{AuthorizationRequest, OAuthError, Pkce, Provider, Registration, Revocation, TokenSet};

/// Deadline for one token endpoint request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Largest token endpoint answer read.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

/// HTTPS client for token endpoints. Redirects are refused: a token request
/// is never replayed to another origin.
#[derive(Clone)]
pub struct OAuthClient {
    http: reqwest::blocking::Client,
}

impl OAuthClient {
    /// # Errors
    /// [`OAuthError::Network`] when the TLS client cannot be built.
    pub fn new() -> Result<Self, OAuthError> {
        let http = tilo_net::http::blocking_client()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| OAuthError::Network(error.without_url().to_string()))?;
        Ok(Self { http })
    }

    /// Redeem an authorization code (with its PKCE verifier) for tokens.
    /// `redirect_uri` is the one the authorization request carried, `None`
    /// when the provider displayed the code instead of redirecting.
    ///
    /// # Errors
    /// [`OAuthError::InvalidGrant`] for a rejected code or verifier,
    /// [`OAuthError::Network`] or [`OAuthError::Protocol`] otherwise.
    pub fn exchange_code(
        &self,
        provider: &Provider,
        client: &Registration,
        redirect_uri: Option<&str>,
        code: &str,
        verifier: &str,
    ) -> Result<TokenSet, OAuthError> {
        let mut params = vec![
            ("grant_type", "authorization_code"),
            ("client_id", client.client_id.as_str()),
        ];
        if let Some(redirect_uri) = redirect_uri {
            params.push(("redirect_uri", redirect_uri));
        }
        params.extend([("code", code), ("code_verifier", verifier)]);
        self.token_request(provider, client, params)
    }

    /// Exchange a refresh token for a fresh access token. The existing
    /// refresh token is carried forward when the provider does not rotate it.
    ///
    /// # Errors
    /// [`OAuthError::InvalidGrant`] when the refresh token was revoked or
    /// expired, [`OAuthError::Network`] or [`OAuthError::Protocol`] otherwise.
    pub fn refresh(
        &self,
        provider: &Provider,
        client: &Registration,
        refresh_token: &str,
    ) -> Result<TokenSet, OAuthError> {
        if refresh_token.is_empty() {
            return Err(OAuthError::InvalidGrant);
        }
        let mut tokens = self.token_request(
            provider,
            client,
            vec![
                ("grant_type", "refresh_token"),
                ("client_id", client.client_id.as_str()),
                ("refresh_token", refresh_token),
            ],
        )?;
        if tokens.refresh_token.is_empty() {
            tokens.refresh_token = refresh_token.to_string();
        }
        Ok(tokens)
    }

    /// Give up the grant at the provider, where it supports that. Returns
    /// `Ok(false)` when the provider has no revocation endpoint.
    ///
    /// # Errors
    /// [`OAuthError::Network`] or [`OAuthError::Protocol`] when the provider
    /// could not be told.
    pub fn revoke(
        &self,
        provider: &Provider,
        client: &Registration,
        tokens: &TokenSet,
    ) -> Result<bool, OAuthError> {
        let request = match &provider.revocation {
            Revocation::Unsupported => return Ok(false),
            Revocation::Rfc7009 { endpoint } => {
                let token = if tokens.refresh_token.is_empty() {
                    &tokens.access_token
                } else {
                    &tokens.refresh_token
                };
                self.http
                    .post(endpoint)
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(form(&[("token", token), ("client_id", &client.client_id)]))
            }
            Revocation::BearerPost { endpoint } => {
                self.http.post(endpoint).bearer_auth(&tokens.access_token)
            }
        };
        let response = request
            .send()
            .map_err(|error| OAuthError::Network(error.without_url().to_string()))?;
        let status = response.status();
        // RFC 7009 §2.2: an already invalid token is also a success.
        if status.is_success() || status.as_u16() == 400 {
            Ok(true)
        } else {
            Err(OAuthError::Protocol(format!(
                "revocation answered HTTP {}",
                status.as_u16()
            )))
        }
    }

    fn token_request<'a>(
        &self,
        provider: &Provider,
        client: &'a Registration,
        mut params: Vec<(&str, &'a str)>,
    ) -> Result<TokenSet, OAuthError> {
        use std::io::Read as _;
        if let Some(secret) = client.secret() {
            params.push(("client_secret", secret));
        }
        let response = self
            .http
            .post(&provider.token_endpoint)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .body(form(&params))
            .send()
            .map_err(|error| OAuthError::Network(error.without_url().to_string()))?;
        let status = response.status().as_u16();
        let mut body = String::new();
        response
            .take(MAX_RESPONSE_BYTES)
            .read_to_string(&mut body)
            .map_err(|error| OAuthError::Network(error.to_string()))?;
        match crate::parse_token_response(&body) {
            Ok(tokens) if (200..300).contains(&status) => Ok(tokens),
            Ok(_) => Err(OAuthError::Protocol(format!(
                "token endpoint answered HTTP {status}"
            ))),
            Err(OAuthError::Protocol(_)) if !(200..300).contains(&status) => Err(
                OAuthError::Protocol(format!("token endpoint answered HTTP {status}")),
            ),
            Err(error) => Err(error),
        }
    }
}

fn form(params: &[(&str, &str)]) -> String {
    let mut body = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in params {
        body.append_pair(key, value);
    }
    body.finish()
}

/// How the authorization code comes back.
enum Return {
    /// The browser is redirected to this listener.
    Loopback(LoopbackServer),
    /// The provider displays the code; the person pastes it.
    Pasted,
}

/// One browser sign-in in progress: the bound loopback redirect (or, when
/// every registered port is taken and the provider allows it, a code to be
/// pasted), the PKCE pair, the `state`, and the URL to open.
///
/// [`Authorization::begin`] is cheap and non-blocking, so the UI thread can
/// start it, open [`Authorization::url`] through the desktop's URI opener and
/// hand the value to a worker for [`Authorization::complete`].
pub struct Authorization {
    provider: Provider,
    client: Registration,
    code_return: Return,
    pkce: Pkce,
    state: String,
    url: String,
}

impl Authorization {
    /// # Errors
    /// [`OAuthError::Loopback`], [`OAuthError::RedirectPortsBusy`] (when the
    /// registration has no paste fallback) or [`OAuthError::Randomness`].
    pub fn begin(
        provider: &Provider,
        client: &Registration,
        login_hint: Option<&str>,
    ) -> Result<Self, OAuthError> {
        let code_return = match LoopbackServer::bind_to(&client.redirect_ports) {
            Ok(server) => Return::Loopback(server),
            Err(OAuthError::RedirectPortsBusy) if client.paste_code_fallback => Return::Pasted,
            Err(error) => return Err(error),
        };
        let pkce = Pkce::generate()?;
        let state = crate::random_token()?;
        let redirect_uri = match &code_return {
            Return::Loopback(server) => server.redirect_uri(),
            Return::Pasted => String::new(),
        };
        let url = AuthorizationRequest {
            provider,
            client_id: &client.client_id,
            redirect_uri: &redirect_uri,
            code_challenge: &pkce.challenge,
            state: &state,
            login_hint,
        }
        .url();
        Ok(Self {
            provider: provider.clone(),
            client: client.clone(),
            code_return,
            pkce,
            state,
            url,
        })
    }

    /// The authorization URL to open in the system browser.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The loopback redirect URI, `None` when the code is to be pasted.
    pub fn redirect_uri(&self) -> Option<String> {
        match &self.code_return {
            Return::Loopback(server) => Some(server.redirect_uri()),
            Return::Pasted => None,
        }
    }

    /// Whether the provider will display the code for the person to paste
    /// ([`Authorization::complete_with_code`]) instead of redirecting.
    pub fn needs_pasted_code(&self) -> bool {
        matches!(self.code_return, Return::Pasted)
    }

    /// Cancels [`Authorization::complete`] from another thread; `None` when
    /// nothing waits for a redirect.
    pub fn canceller(&self) -> Option<LoopbackCancel> {
        match &self.code_return {
            Return::Loopback(server) => Some(server.canceller()),
            Return::Pasted => None,
        }
    }

    /// Wait for the redirect, check `state`, and redeem the code.
    ///
    /// # Errors
    /// Any [`OAuthError`] of the redirect wait or the code exchange;
    /// [`OAuthError::NeedsPastedCode`] when the code is to be pasted.
    pub fn complete(self, client: &OAuthClient, timeout: Duration) -> Result<TokenSet, OAuthError> {
        let Return::Loopback(server) = &self.code_return else {
            return Err(OAuthError::NeedsPastedCode);
        };
        let code = server.wait_for_code(&self.state, timeout)?;
        client.exchange_code(
            &self.provider,
            &self.client,
            Some(&server.redirect_uri()),
            &code,
            &self.pkce.verifier,
        )
    }

    /// Redeem a code the provider displayed and the person pasted.
    ///
    /// # Errors
    /// [`OAuthError::InvalidGrant`] for a wrong code, or any other
    /// [`OAuthError`] of the exchange.
    pub fn complete_with_code(
        self,
        client: &OAuthClient,
        code: &str,
    ) -> Result<TokenSet, OAuthError> {
        let code = code.trim();
        if code.is_empty() {
            return Err(OAuthError::NeedsPastedCode);
        }
        client.exchange_code(
            &self.provider,
            &self.client,
            self.redirect_uri().as_deref(),
            code,
            &self.pkce.verifier,
        )
    }
}

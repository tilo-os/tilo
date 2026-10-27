//! `OpenRouter`'s own sign-in (its PKCE flow for apps): the person
//! approves Tilo on `OpenRouter`'s page, which hands back a code that
//! [`KeyAuthorization::finish`] trades for a new API key on their account.
//!
//! The result is an ordinary `OpenRouter` API key, not a grant: the caller
//! stores it the way a typed key is stored, and the person can revoke it
//! in `OpenRouter`'s key settings like any other.

use std::time::Duration;

use tilo_oauth::loopback::{LoopbackCancel, LoopbackServer};
use tilo_oauth::{Pkce, Redirect, RedirectPorts};
use zeroize::Zeroizing;

use crate::{CustodyError, SIGN_IN_TIMEOUT};

/// `OpenRouter`'s authorization page.
pub const AUTH_ENDPOINT: &str = "https://openrouter.ai/auth";
/// Where an authorization code is traded for a key.
pub const KEYS_ENDPOINT: &str = "https://openrouter.ai/api/v1/auth/keys";

/// One `OpenRouter` sign-in in progress.
pub struct KeyAuthorization {
    server: LoopbackServer,
    pkce: Pkce,
    state: String,
    url: String,
    keys_endpoint: String,
}

impl KeyAuthorization {
    /// Bind the redirect listener and build the page to open. Non-blocking.
    /// `auth_endpoint` and `keys_endpoint` are [`AUTH_ENDPOINT`] and
    /// [`KEYS_ENDPOINT`] outside tests.
    ///
    /// # Errors
    /// [`CustodyError::SignIn`] when no loopback port or randomness is
    /// available.
    pub fn begin(auth_endpoint: &str, keys_endpoint: &str) -> Result<Self, CustodyError> {
        // `OpenRouter` accepts a callback on any `localhost` port.
        let redirect = Redirect {
            host: "localhost".into(),
            ..Redirect::default()
        };
        let server = LoopbackServer::bind_to(&RedirectPorts::Any)?.at(&redirect);
        let pkce = Pkce::generate()?;
        let state = tilo_oauth::random_token()?;
        let mut url = url::Url::parse(auth_endpoint)
            .map_err(|_| CustodyError::SignIn("invalid OpenRouter sign-in page".into()))?;
        url.query_pairs_mut()
            .append_pair("callback_url", &server.redirect_uri())
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("key_label", "Tilo");
        Ok(Self {
            server,
            pkce,
            state,
            url: url.into(),
            keys_endpoint: keys_endpoint.into(),
        })
    }

    /// The page to open in the browser.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Ends [`Self::finish`] from another thread.
    pub fn canceller(&self) -> LoopbackCancel {
        self.server.canceller()
    }

    /// Wait for `OpenRouter`'s redirect and trade its code for an API key.
    /// Blocking.
    ///
    /// # Errors
    /// The wait's [`tilo_oauth::OAuthError`] (cancelled, timed out,
    /// declined), [`CustodyError::Offline`], or [`CustodyError::SignIn`]
    /// when `OpenRouter` refuses the code or answers without a key.
    pub fn finish(self) -> Result<Zeroizing<String>, CustodyError> {
        let code = self.server.wait_for_code(&self.state, SIGN_IN_TIMEOUT)?;
        let http = tilo_net::http::blocking_client()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| CustodyError::Offline)?;
        let response = http
            .post(&self.keys_endpoint)
            .json(&serde_json::json!({
                "code": code,
                "code_verifier": self.pkce.verifier,
                "code_challenge_method": "S256",
            }))
            .send()
            .map_err(|_| CustodyError::Offline)?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(CustodyError::SignIn(format!(
                "OpenRouter did not accept the sign-in (HTTP {status}); try again"
            )));
        }
        let body: serde_json::Value = response
            .json()
            .map_err(|_| CustodyError::SignIn("OpenRouter answered without a key".into()))?;
        body.get("key")
            .and_then(serde_json::Value::as_str)
            .filter(|key| !key.trim().is_empty() && key.len() <= 4096)
            .map(|key| Zeroizing::new(key.to_string()))
            .ok_or_else(|| CustodyError::SignIn("OpenRouter answered without a key".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_http::{Reply, Responder};

    fn redirect(url: &str, query: &str) {
        let callback = url::Url::parse(url)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == "callback_url")
            .unwrap()
            .1
            .into_owned();
        let callback = url::Url::parse(&callback).unwrap();
        assert_eq!(callback.host_str(), Some("localhost"));
        let mut stream =
            std::net::TcpStream::connect(("127.0.0.1", callback.port().unwrap())).unwrap();
        std::io::Write::write_all(
            &mut stream,
            format!("GET {}?{query} HTTP/1.1\r\n\r\n", callback.path()).as_bytes(),
        )
        .unwrap();
        let mut answer = String::new();
        let _ = std::io::Read::read_to_string(&mut stream, &mut answer);
    }

    fn state_of(url: &str) -> String {
        url::Url::parse(url)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned()
    }

    #[test]
    fn an_approved_sign_in_comes_back_as_a_key_bound_to_its_verifier() {
        let keys = Responder::start(vec![
            Reply::status(200).body(r#"{"key":"sk-or-v1-new","user_id":"u"}"#),
        ]);
        let authorization = KeyAuthorization::begin(AUTH_ENDPOINT, &keys.url()).unwrap();
        let url = authorization.url().to_string();
        assert!(url.starts_with("https://openrouter.ai/auth?callback_url="));
        assert!(url.contains("code_challenge_method=S256"));
        let state = state_of(&url);
        let browser =
            std::thread::spawn(move || redirect(&url, &format!("code=C0DE&state={state}")));
        let key = authorization.finish().unwrap();
        browser.join().unwrap();
        assert_eq!(key.as_str(), "sk-or-v1-new");
        let exchange: serde_json::Value =
            serde_json::from_str(&keys.next_request().body_text()).unwrap();
        assert_eq!(exchange["code"], "C0DE");
        assert_eq!(exchange["code_challenge_method"], "S256");
        assert!(
            exchange["code_verifier"]
                .as_str()
                .is_some_and(|v| v.len() >= 43)
        );
    }

    #[test]
    fn a_forged_redirect_or_a_refused_code_gives_no_key() {
        let keys = Responder::silent();
        let authorization = KeyAuthorization::begin(AUTH_ENDPOINT, &keys.url()).unwrap();
        let url = authorization.url().to_string();
        let browser = std::thread::spawn(move || redirect(&url, "code=C&state=forged"));
        assert!(authorization.finish().is_err());
        browser.join().unwrap();
        keys.assert_untouched();

        let refusing = Responder::start(vec![Reply::status(403).body("{}")]);
        let authorization = KeyAuthorization::begin(AUTH_ENDPOINT, &refusing.url()).unwrap();
        let url = authorization.url().to_string();
        let state = state_of(&url);
        let browser = std::thread::spawn(move || redirect(&url, &format!("code=C&state={state}")));
        assert!(matches!(
            authorization.finish(),
            Err(CustodyError::SignIn(_))
        ));
        browser.join().unwrap();

        let cancelled = KeyAuthorization::begin(AUTH_ENDPOINT, &refusing.url()).unwrap();
        cancelled.canceller().cancel();
        assert!(cancelled.finish().is_err());
    }
}

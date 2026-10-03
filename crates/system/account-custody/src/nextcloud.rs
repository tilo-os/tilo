//! Nextcloud Login Flow v2.
//!
//! Instead of asking for a password, Settings asks the server to start a
//! login flow, opens the returned login page in the browser, and polls the
//! flow endpoint until the person has approved access there. The server then
//! answers with an app password minted for this device, which is what the
//! vault keeps; the person's own password never passes through Tilo and the
//! app password can be revoked on its own (Nextcloud › Security › Devices).
//!
//! See <https://docs.nextcloud.com/server/latest/developer_manual/client_apis/LoginFlow/index.html>.

use std::fmt;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use url::Url;
use zeroize::Zeroizing;

/// Name shown in the person's Nextcloud device list.
const USER_AGENT: &str = "Tilo";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;
/// The server keeps a flow alive for 20 minutes.
pub const FLOW_LIFETIME: Duration = Duration::from_secs(20 * 60);
/// Pause between polls of the flow endpoint.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NextcloudError {
    /// The server URL is not an HTTPS URL without credentials.
    InvalidServer(&'static str),
    /// The server did not answer like a Nextcloud server.
    NotNextcloud,
    Offline(String),
    Cancelled,
    TimedOut,
    Protocol(&'static str),
}

impl fmt::Display for NextcloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidServer(reason) => write!(f, "{reason}"),
            Self::NotNextcloud => f.write_str("that server did not answer like a Nextcloud server"),
            Self::Offline(error) => write!(f, "could not reach the server: {error}"),
            Self::Cancelled => f.write_str("sign-in was cancelled"),
            Self::TimedOut => f.write_str("sign-in was not completed in time"),
            Self::Protocol(reason) => write!(f, "the server answered unexpectedly: {reason}"),
        }
    }
}

impl std::error::Error for NextcloudError {}

/// What a completed flow yields.
pub struct NextcloudLogin {
    /// The server root the app password is valid for.
    pub server: String,
    pub login_name: String,
    pub app_password: Zeroizing<String>,
}

impl fmt::Debug for NextcloudLogin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NextcloudLogin")
            .field("server", &self.server)
            .field("login_name", &self.login_name)
            .finish_non_exhaustive()
    }
}

/// Parse and check a server URL: HTTPS without credentials, or plain HTTP
/// only to a loopback host.
///
/// # Errors
/// [`NextcloudError::InvalidServer`].
pub fn server_url(input: &str) -> Result<Url, NextcloudError> {
    let mut url = Url::parse(input.trim())
        .map_err(|_| NextcloudError::InvalidServer("enter a valid server URL"))?;
    if !secure(&url) {
        return Err(NextcloudError::InvalidServer("use an HTTPS server URL"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(NextcloudError::InvalidServer(
            "remove credentials from the server URL",
        ));
    }
    url.set_query(None);
    url.set_fragment(None);
    let trimmed = url.path().trim_end_matches('/').to_string();
    url.set_path(&trimmed);
    Ok(url)
}

fn secure(url: &Url) -> bool {
    match url.scheme() {
        "https" => url.host().is_some(),
        "http" => {
            matches!(
                url.host(),
                Some(url::Host::Ipv4(ip)) if ip.is_loopback()
            ) || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
                || url.host_str() == Some("localhost")
        }
        _ => false,
    }
}

fn same_host(a: &Url, b: &Url) -> bool {
    a.host_str() == b.host_str() && a.port_or_known_default() == b.port_or_known_default()
}

fn client() -> Result<reqwest::blocking::Client, NextcloudError> {
    tilo_net::http::blocking_client()
        .user_agent(USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| NextcloudError::Offline(error.without_url().to_string()))
}

fn read_json(response: reqwest::blocking::Response) -> Result<serde_json::Value, NextcloudError> {
    use std::io::Read as _;
    let mut body = String::new();
    response
        .take(MAX_RESPONSE_BYTES)
        .read_to_string(&mut body)
        .map_err(|error| NextcloudError::Offline(error.to_string()))?;
    serde_json::from_str(&body).map_err(|_| NextcloudError::Protocol("not JSON"))
}

/// A started login flow.
pub struct LoginFlow {
    http: reqwest::blocking::Client,
    server: Url,
    login_url: String,
    poll_endpoint: Url,
    token: Zeroizing<String>,
}

impl fmt::Debug for LoginFlow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginFlow")
            .field("server", &self.server.as_str())
            .finish_non_exhaustive()
    }
}

/// Start a login flow on `server`.
///
/// # Errors
/// [`NextcloudError::InvalidServer`], [`NextcloudError::Offline`] or
/// [`NextcloudError::NotNextcloud`].
pub fn begin(server: &str) -> Result<LoginFlow, NextcloudError> {
    let server = server_url(server)?;
    let http = client()?;
    let start = format!(
        "{}/index.php/login/v2",
        server.as_str().trim_end_matches('/')
    );
    let response = http
        .post(start)
        .send()
        .map_err(|error| NextcloudError::Offline(error.without_url().to_string()))?;
    if !response.status().is_success() {
        return Err(NextcloudError::NotNextcloud);
    }
    let value = read_json(response).map_err(|_| NextcloudError::NotNextcloud)?;
    let text = |pointer: &str| {
        value
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let (Some(login_url), Some(endpoint), Some(token)) =
        (text("/login"), text("/poll/endpoint"), text("/poll/token"))
    else {
        return Err(NextcloudError::NotNextcloud);
    };
    let poll_endpoint =
        Url::parse(&endpoint).map_err(|_| NextcloudError::Protocol("invalid poll endpoint"))?;
    let login =
        Url::parse(&login_url).map_err(|_| NextcloudError::Protocol("invalid login URL"))?;
    // The poll token and later the app password travel to the poll
    // endpoint: it must be the server that was asked, over a secure channel.
    if !secure(&poll_endpoint) || !same_host(&poll_endpoint, &server) {
        return Err(NextcloudError::Protocol(
            "poll endpoint is on another server",
        ));
    }
    if !secure(&login) {
        return Err(NextcloudError::Protocol("insecure login page"));
    }
    Ok(LoginFlow {
        http,
        server,
        login_url,
        poll_endpoint,
        token: Zeroizing::new(token),
    })
}

impl LoginFlow {
    /// The page to open in the browser.
    pub fn login_url(&self) -> &str {
        &self.login_url
    }

    /// Poll until the person approves access in the browser. Returns early
    /// with [`NextcloudError::Cancelled`] once `cancel` receives a message or
    /// its sender is dropped; the pause between polls is that same wait.
    ///
    /// # Errors
    /// [`NextcloudError::Cancelled`], [`NextcloudError::TimedOut`],
    /// [`NextcloudError::Offline`] or [`NextcloudError::Protocol`].
    pub fn wait(
        &self,
        cancel: &mpsc::Receiver<()>,
        interval: Duration,
        lifetime: Duration,
    ) -> Result<NextcloudLogin, NextcloudError> {
        let started = Instant::now();
        loop {
            if let Some(login) = self.poll()? {
                return Ok(login);
            }
            let remaining = lifetime.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(NextcloudError::TimedOut);
            }
            match cancel.recv_timeout(interval.min(remaining)) {
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(NextcloudError::Cancelled);
                }
            }
        }
    }

    fn poll(&self) -> Result<Option<NextcloudLogin>, NextcloudError> {
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        body.append_pair("token", &self.token);
        let response = self
            .http
            .post(self.poll_endpoint.clone())
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body.finish())
            .send()
            .map_err(|error| NextcloudError::Offline(error.without_url().to_string()))?;
        match response.status().as_u16() {
            404 => return Ok(None),
            200 => {}
            _ => return Err(NextcloudError::Protocol("unexpected poll status")),
        }
        let value = read_json(response)?;
        let text = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let (Some(server), Some(login_name), Some(app_password)) =
            (text("server"), text("loginName"), text("appPassword"))
        else {
            return Err(NextcloudError::Protocol("incomplete login result"));
        };
        let server = server_url(&server)
            .map_err(|_| NextcloudError::Protocol("insecure server in login result"))?;
        if !same_host(&server, &self.server) {
            return Err(NextcloudError::Protocol(
                "login result names another server",
            ));
        }
        Ok(Some(NextcloudLogin {
            server: server.as_str().trim_end_matches('/').to_string(),
            login_name,
            app_password: Zeroizing::new(app_password),
        }))
    }
}

/// Revoke an app password (account removal).
///
/// # Errors
/// [`NextcloudError::Offline`] or [`NextcloudError::Protocol`].
pub fn revoke_app_password(
    server: &str,
    login_name: &str,
    app_password: &str,
) -> Result<(), NextcloudError> {
    let server = server_url(server)?;
    let response = client()?
        .delete(format!(
            "{}/ocs/v2.php/core/apppassword",
            server.as_str().trim_end_matches('/')
        ))
        .header("OCS-APIRequest", "true")
        .basic_auth(login_name, Some(app_password))
        .send()
        .map_err(|error| NextcloudError::Offline(error.without_url().to_string()))?;
    // 401: the app password is already gone.
    if response.status().is_success() || response.status().as_u16() == 401 {
        Ok(())
    } else {
        Err(NextcloudError::Protocol("app password revocation failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use test_http::{Reply, Responder};

    fn fake_nextcloud(pending_polls: usize) -> (Responder, std::sync::Arc<Mutex<Vec<String>>>) {
        let tokens = std::sync::Arc::new(Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&tokens);
        let polls = AtomicUsize::new(0);
        let base = std::sync::Arc::new(Mutex::new(String::new()));
        let base_for_handler = std::sync::Arc::clone(&base);
        let responder = Responder::handle(move |request| {
            let base = base_for_handler.lock().unwrap().clone();
            match (request.method.as_str(), request.target.as_str()) {
                ("POST", "/index.php/login/v2") => Some(
                    Reply::status(200).body(
                        serde_json::json!({
                            "poll": {"token": "poll-token-1", "endpoint": format!("{base}/login/v2/poll")},
                            "login": format!("{base}/login/v2/flow/abc"),
                        })
                        .to_string(),
                    ),
                ),
                ("POST", "/login/v2/poll") => {
                    seen.lock().unwrap().push(request.body_text());
                    if polls.fetch_add(1, Ordering::SeqCst) < pending_polls {
                        Some(Reply::status(404))
                    } else {
                        Some(Reply::status(200).body(
                            serde_json::json!({
                                "server": base,
                                "loginName": "ada",
                                "appPassword": "minted-app-password",
                            })
                            .to_string(),
                        ))
                    }
                }
                ("DELETE", "/ocs/v2.php/core/apppassword") => Some(Reply::status(200)),
                _ => Some(Reply::status(404)),
            }
        });
        *base.lock().unwrap() = responder.url();
        (responder, tokens)
    }

    #[test]
    fn login_flow_polls_until_the_app_password_is_minted() {
        let (server, tokens) = fake_nextcloud(2);
        let flow = begin(&format!("{}/", server.url())).unwrap();
        assert_eq!(
            flow.login_url(),
            format!("{}/login/v2/flow/abc", server.url())
        );
        let (_keep_open, cancel) = mpsc::channel();
        let login = flow
            .wait(&cancel, Duration::ZERO, Duration::from_secs(30))
            .unwrap();
        assert_eq!(login.server, server.url());
        assert_eq!(login.login_name, "ada");
        assert_eq!(login.app_password.as_str(), "minted-app-password");
        let polls = tokens.lock().unwrap().clone();
        assert_eq!(polls.len(), 3, "two pending answers, then the result");
        assert!(polls.iter().all(|body| body == "token=poll-token-1"));
        assert!(!format!("{login:?}").contains("minted"));
        revoke_app_password(&login.server, &login.login_name, &login.app_password).unwrap();
        let revoke = server
            .drain_requests()
            .into_iter()
            .find(|request| request.method == "DELETE")
            .expect("revocation request");
        assert_eq!(revoke.header("OCS-APIRequest"), Some("true"));
        assert!(
            revoke
                .header("Authorization")
                .is_some_and(|value| value.starts_with("Basic "))
        );
    }

    #[test]
    fn cancelling_stops_polling() {
        let (server, tokens) = fake_nextcloud(usize::MAX);
        let flow = begin(&server.url()).unwrap();
        let (stop, cancel) = mpsc::channel();
        stop.send(()).unwrap();
        assert_eq!(
            flow.wait(&cancel, Duration::from_secs(60), Duration::from_secs(60))
                .unwrap_err(),
            NextcloudError::Cancelled
        );
        assert_eq!(tokens.lock().unwrap().len(), 1);
        let (_keep_open, idle) = mpsc::channel();
        assert_eq!(
            flow.wait(&idle, Duration::ZERO, Duration::ZERO)
                .unwrap_err(),
            NextcloudError::TimedOut
        );
    }

    #[test]
    fn a_poll_endpoint_on_another_host_is_refused() {
        let responder = Responder::handle(|_| {
            Some(
                Reply::status(200).body(
                    serde_json::json!({
                        "poll": {"token": "t", "endpoint": "https://collector.example/poll"},
                        "login": "https://collector.example/login",
                    })
                    .to_string(),
                ),
            )
        });
        assert_eq!(
            begin(&responder.url()).unwrap_err(),
            NextcloudError::Protocol("poll endpoint is on another server")
        );
    }

    #[test]
    fn server_urls_must_be_secure() {
        assert!(server_url("https://cloud.example.com/").is_ok());
        assert!(server_url("http://127.0.0.1:8080").is_ok());
        assert!(server_url("http://cloud.example.com").is_err());
        assert!(server_url("https://ada:pw@cloud.example.com").is_err());
        assert_eq!(
            server_url("https://cloud.example.com/nc/?x=1")
                .unwrap()
                .as_str(),
            "https://cloud.example.com/nc"
        );
    }
}

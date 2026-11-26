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
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use url::Url;
use zeroize::Zeroizing;

const USER_AGENT: &str = "Tilo";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;
pub const FLOW_LIFETIME: Duration = Duration::from_secs(20 * 60);
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NextcloudError {
    InvalidServer(&'static str),
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

pub struct NextcloudLogin {
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
    accounts::is_secure_endpoint(url.as_str())
}

fn same_host(a: &Url, b: &Url) -> bool {
    a.host_str() == b.host_str() && a.port_or_known_default() == b.port_or_known_default()
}

fn client(lookup: Option<&tilo_net::Lookup>) -> Result<reqwest::blocking::Client, NextcloudError> {
    let builder = match lookup {
        Some(lookup) => tilo_net::http::blocking_client_looking(Arc::clone(lookup)),
        None => tilo_net::http::blocking_client(),
    };
    builder
        .user_agent(USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(offline)
}

fn offline(error: reqwest::Error) -> NextcloudError {
    let why = tilo_net::http::refused_by_lookup(&error)
        .unwrap_or_else(|| error.without_url().to_string());
    NextcloudError::Offline(why)
}

/// For a flow given its own look-up, ask it about an address written into `url`
/// before anything is sent: a connection asks it for a name, and looks no
/// written-out address up.
fn check_written(lookup: Option<&tilo_net::Lookup>, url: &Url) -> Result<(), NextcloudError> {
    if let (Some(lookup), Some(address)) = (lookup, tilo_net::address::written_out(url)) {
        tilo_net::address::asked_as_written(lookup, address)
            .map_err(|error| NextcloudError::Offline(error.to_string()))?;
    }
    Ok(())
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

/// A pair that stops a login flow: sending on it, or dropping the sender,
/// ends [`LoginFlow::wait`] with [`NextcloudError::Cancelled`].
#[must_use]
pub fn cancellation() -> (mpsc::Sender<()>, mpsc::Receiver<()>) {
    mpsc::channel()
}

pub struct LoginFlow {
    http: reqwest::blocking::Client,
    lookup: Option<tilo_net::Lookup>,
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

pub fn begin(server: &str) -> Result<LoginFlow, NextcloudError> {
    begin_with(server, None)
}

/// Begin a login flow whose every request connects only to addresses `lookup`
/// answers at that connection: the first call and every poll of the flow it
/// returns. Takes no proxy from the environment, and asks `lookup` before
/// sending to an address written into a URL.
pub fn begin_looking(server: &str, lookup: tilo_net::Lookup) -> Result<LoginFlow, NextcloudError> {
    begin_with(server, Some(lookup))
}

fn begin_with(server: &str, lookup: Option<tilo_net::Lookup>) -> Result<LoginFlow, NextcloudError> {
    let server = server_url(server)?;
    check_written(lookup.as_ref(), &server)?;
    let http = client(lookup.as_ref())?;
    let start = format!(
        "{}/index.php/login/v2",
        server.as_str().trim_end_matches('/')
    );
    let response = http.post(start).send().map_err(offline)?;
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
        lookup,
        server,
        login_url,
        poll_endpoint,
        token: Zeroizing::new(token),
    })
}

impl LoginFlow {
    pub fn login_url(&self) -> &str {
        &self.login_url
    }

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

    /// Ask once whether the person approved the sign-in: `None` while they have not. What
    /// [`Self::wait`] asks at each interval, for a caller that follows the flow on its own.
    ///
    /// # Errors
    /// The server could not be reached or answered something a login flow does not.
    pub fn poll(&self) -> Result<Option<NextcloudLogin>, NextcloudError> {
        check_written(self.lookup.as_ref(), &self.poll_endpoint)?;
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        body.append_pair("token", &self.token);
        let response = self
            .http
            .post(self.poll_endpoint.clone())
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body.finish())
            .send()
            .map_err(offline)?;
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

pub fn revoke_app_password(
    server: &str,
    login_name: &str,
    app_password: &str,
) -> Result<(), NextcloudError> {
    let server = server_url(server)?;
    let response = client(None)?
        .delete(format!(
            "{}/ocs/v2.php/core/apppassword",
            server.as_str().trim_end_matches('/')
        ))
        .header("OCS-APIRequest", "true")
        .basic_auth(login_name, Some(app_password))
        .send()
        .map_err(offline)?;
    if response.status().is_success() || response.status().as_u16() == 401 {
        Ok(())
    } else {
        Err(NextcloudError::Protocol("app password revocation failed"))
    }
}

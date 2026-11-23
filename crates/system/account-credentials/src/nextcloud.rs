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

    tilo_test_env::scenario! {
        /// Nextcloud's login flow v2.
        fn the_nextcloud_login_flow_polls_only_its_own_secure_server;

        phase login_flow_polls_until_the_app_password_is_minted;
        {
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

        phase cancelling_stops_polling;
        {
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

        phase a_poll_endpoint_on_another_host_is_refused;
        {
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

        phase server_urls_must_be_secure;
        {
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

    struct CountingListener {
        port: u16,
        accepted: Arc<AtomicUsize>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl CountingListener {
        fn start() -> Self {
            let listener =
                std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("a port");
            let port = listener.local_addr().expect("its address").port();
            let accepted = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let count = Arc::clone(&accepted);
            let stopping = Arc::clone(&stop);
            let worker = std::thread::spawn(move || {
                while let Ok((stream, _)) = listener.accept() {
                    if stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    count.fetch_add(1, Ordering::SeqCst);
                    drop(stream);
                }
            });
            Self {
                port,
                accepted,
                stop,
                worker: Some(worker),
            }
        }

        fn count(&self) -> usize {
            self.accepted.load(Ordering::SeqCst)
        }
    }

    impl Drop for CountingListener {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, self.port));
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn fake_nextcloud_at_host(
        host: &str,
        pending_polls: usize,
    ) -> (Responder, Arc<Mutex<Vec<String>>>) {
        let tokens = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&tokens);
        let polls = AtomicUsize::new(0);
        let base = Arc::new(Mutex::new(String::new()));
        let base_for_handler = Arc::clone(&base);
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
                _ => Some(Reply::status(404)),
            }
        });
        *base.lock().unwrap() = format!("http://{host}:{}", responder.port());
        (responder, tokens)
    }

    fn poll_ready<T>(
        mut future: std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>,
    ) -> T {
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => panic!("fixture lookup returned Pending"),
        }
    }

    fn toggle_lookup(
        open: &Arc<std::sync::atomic::AtomicBool>,
        asked: &Arc<AtomicUsize>,
    ) -> tilo_net::Lookup {
        let open = Arc::clone(open);
        let asked = Arc::clone(asked);
        let inner: tilo_net::Lookup = Arc::new(move |_name: String| {
            Box::pin(async move { Ok(vec![std::net::IpAddr::from(std::net::Ipv4Addr::LOCALHOST)]) })
        });
        Arc::new(move |name: String| {
            asked.fetch_add(1, Ordering::SeqCst);
            let open = open.load(Ordering::SeqCst);
            let inner = Arc::clone(&inner);
            Box::pin(async move {
                if open {
                    inner(name).await
                } else {
                    tilo_net::address::public_addresses(&inner, &name).await
                }
            })
        })
    }

    #[test]
    fn begin_looking_refuses_a_name_that_turns_inside_and_written_inside_addresses() {
        let listener = CountingListener::start();
        let asked = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&asked);
        let turning: tilo_net::Lookup = Arc::new(move |_name: String| {
            let nth = counted.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Ok(vec![if nth == 0 {
                    std::net::IpAddr::from([93, 184, 216, 34])
                } else {
                    std::net::IpAddr::from(std::net::Ipv4Addr::LOCALHOST)
                }])
            })
        });
        let judged = tilo_net::address::public_only(turning);

        let first = poll_ready(judged("localhost".to_owned())).unwrap();
        assert_eq!(first, vec![std::net::IpAddr::from([93, 184, 216, 34])]);
        assert_eq!(asked.load(Ordering::SeqCst), 1);

        let named = format!("https://localhost:{}", listener.port);
        assert_eq!(
            begin_looking(&named, Arc::clone(&judged)).unwrap_err(),
            NextcloudError::Offline(
                "localhost answers with an address of this machine or its network".into()
            )
        );
        assert_eq!(listener.count(), 0, "nothing is connected to");
        assert_eq!(asked.load(Ordering::SeqCst), 2, "looked up at connection");

        let literal = format!("http://127.0.0.1:{}", listener.port);
        assert_eq!(
            begin_looking(
                &literal,
                tilo_net::address::public_only(tilo_net::address::system_lookup()),
            )
            .unwrap_err(),
            NextcloudError::Offline(
                "127.0.0.1 answers with an address of this machine or its network".into()
            )
        );
        assert_eq!(listener.count(), 0, "written-out inside address untouched");

        // A device caller's `begin` behaves as on main and connects to loopback.
        assert!(matches!(begin(&named), Err(NextcloudError::Offline(_))));
        assert_eq!(listener.count(), 1, "device caller connects as on main");
    }

    #[test]
    fn a_login_flow_asks_its_given_lookup_on_every_poll() {
        let (server, tokens) = fake_nextcloud_at_host("localhost", 1);
        let base = format!("http://localhost:{}", server.port());
        let asked = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&asked);
        let lookup: tilo_net::Lookup = Arc::new(move |_name: String| {
            counted.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(vec![std::net::IpAddr::from(std::net::Ipv4Addr::LOCALHOST)]) })
        });

        let flow = begin_looking(&base, lookup).unwrap();
        assert_eq!(flow.login_url(), format!("{base}/login/v2/flow/abc"));
        assert_eq!(asked.load(Ordering::SeqCst), 1, "begin asks the lookup");
        assert!(flow.poll().unwrap().is_none(), "first poll is pending");
        assert_eq!(
            asked.load(Ordering::SeqCst),
            2,
            "first poll asks the given lookup"
        );
        let login = flow.poll().unwrap().expect("second poll completes");
        assert_eq!(
            asked.load(Ordering::SeqCst),
            3,
            "second poll asks the given lookup"
        );
        assert_eq!(login.server, base);
        assert_eq!(login.login_name, "ada");
        assert_eq!(login.app_password.as_str(), "minted-app-password");
        assert_eq!(tokens.lock().unwrap().len(), 2);

        // Shutting off private addresses mid-flow refuses subsequent polls by name.
        let (named_server, named_tokens) = fake_nextcloud_at_host("localhost", 0);
        let named_base = format!("http://localhost:{}", named_server.port());
        let open = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let named_asked = Arc::new(AtomicUsize::new(0));
        let flow = begin_looking(&named_base, toggle_lookup(&open, &named_asked)).unwrap();
        assert_eq!(named_asked.load(Ordering::SeqCst), 1);
        open.store(false, Ordering::SeqCst);
        assert_eq!(
            flow.poll().unwrap_err(),
            NextcloudError::Offline(
                "localhost answers with an address of this machine or its network".into()
            )
        );
        assert_eq!(named_asked.load(Ordering::SeqCst), 2);
        assert!(named_tokens.lock().unwrap().is_empty());

        // And refuses subsequent polls to a written-out IP literal as well.
        let (literal_server, literal_tokens) = fake_nextcloud(0);
        let literal_open = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let literal_asked = Arc::new(AtomicUsize::new(0));
        let flow = begin_looking(
            &literal_server.url(),
            toggle_lookup(&literal_open, &literal_asked),
        )
        .unwrap();
        literal_open.store(false, Ordering::SeqCst);
        assert_eq!(
            flow.poll().unwrap_err(),
            NextcloudError::Offline(
                "127.0.0.1 answers with an address of this machine or its network".into()
            )
        );
        assert!(literal_tokens.lock().unwrap().is_empty());

        // Device caller's `begin` completes a localhost flow as on main.
        let (device_server, device_tokens) = fake_nextcloud_at_host("localhost", 0);
        let device_base = format!("http://localhost:{}", device_server.port());
        let flow = begin(&device_base).unwrap();
        let login = flow.poll().unwrap().expect("device poll completes");
        assert_eq!(login.server, device_base);
        assert_eq!(login.login_name, "ada");
        assert_eq!(login.app_password.as_str(), "minted-app-password");
        assert_eq!(device_tokens.lock().unwrap().len(), 1);
    }
}

//! The one-shot loopback redirect listener (RFC 8252 §7.3).
//!
//! [`LoopbackServer::bind`] takes an ephemeral port on `127.0.0.1` (or
//! [`LoopbackServer::bind_to`] the first free registered port, for providers
//! that match redirect URIs exactly) before the browser is opened, so its [`LoopbackServer::redirect_uri`] can go into the
//! authorization request. [`LoopbackServer::wait_for_code`] then blocks in
//! `accept` until the browser arrives with `code` and `state`. The deadline
//! and a UI-side [`LoopbackCancel`] both end the wait by connecting to the
//! listener themselves, so nothing polls.

use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use crate::{OAuthError, Redirect, RedirectPorts};

pub const REDIRECT_PATH: &str = "/oauth";

const WAITING: u8 = 0;
const CANCELLED: u8 = 1;
const TIMED_OUT: u8 = 2;

/// The bound redirect listener.
pub struct LoopbackServer {
    listener: TcpListener,
    address: SocketAddr,
    stop: Arc<AtomicU8>,
    redirect: Redirect,
}

/// Ends a pending [`LoopbackServer::wait_for_code`] from another thread.
#[derive(Clone)]
pub struct LoopbackCancel {
    address: SocketAddr,
    stop: Arc<AtomicU8>,
}

impl LoopbackCancel {
    /// Cancel the wait; it returns [`OAuthError::Cancelled`].
    pub fn cancel(&self) {
        wake(&self.stop, CANCELLED, self.address);
    }
}

fn wake(stop: &AtomicU8, reason: u8, address: SocketAddr) {
    if stop
        .compare_exchange(WAITING, reason, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        // The accept loop observes the flag once this connection lands.
        let _ = TcpStream::connect(address);
    }
}

/// What a redirect request carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RedirectResult {
    Code { code: String, state: String },
    Error(String),
}

impl LoopbackServer {
    /// Bind `127.0.0.1:0`.
    ///
    /// # Errors
    /// [`OAuthError::Loopback`] when the loopback interface cannot be bound.
    pub fn bind() -> Result<Self, OAuthError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(|error| OAuthError::Loopback(error.to_string()))?;
        Self::listen(listener)
    }

    /// Bind as `ports` allows: an ephemeral port, or the first free one of
    /// the registered ports.
    ///
    /// # Errors
    /// [`OAuthError::RedirectPortsBusy`] when every registered port is
    /// taken, [`OAuthError::Loopback`] for any other failure.
    pub fn bind_to(ports: &RedirectPorts) -> Result<Self, OAuthError> {
        let RedirectPorts::Registered(ports) = ports else {
            return Self::bind();
        };
        for &port in ports {
            match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
                Ok(listener) => return Self::listen(listener),
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {}
                Err(error) => return Err(OAuthError::Loopback(error.to_string())),
            }
        }
        Err(OAuthError::RedirectPortsBusy)
    }

    fn listen(listener: TcpListener) -> Result<Self, OAuthError> {
        let address = listener
            .local_addr()
            .map_err(|error| OAuthError::Loopback(error.to_string()))?;
        Ok(Self {
            listener,
            address,
            stop: Arc::new(AtomicU8::new(WAITING)),
            redirect: Redirect::default(),
        })
    }

    /// Redirect to `redirect`'s host and path instead of
    /// `127.0.0.1` and [`REDIRECT_PATH`]. The listener stays on
    /// `127.0.0.1`, where a browser's `localhost` arrives.
    #[must_use]
    pub fn at(mut self, redirect: &Redirect) -> Self {
        self.redirect = redirect.clone();
        self
    }

    pub fn port(&self) -> u16 {
        self.address.port()
    }

    /// The exact redirect URI to send in the authorization request.
    pub fn redirect_uri(&self) -> String {
        format!(
            "http://{}:{}{}",
            self.redirect.host,
            self.address.port(),
            self.redirect.path
        )
    }

    pub fn canceller(&self) -> LoopbackCancel {
        LoopbackCancel {
            address: self.address,
            stop: Arc::clone(&self.stop),
        }
    }

    /// Block until the browser delivers the redirect, then return the code
    /// after checking `expected_state`. Requests that are not the redirect
    /// (a favicon fetch) are answered and ignored.
    ///
    /// # Errors
    /// [`OAuthError::StateMismatch`], [`OAuthError::Denied`] for a provider
    /// `error=`, [`OAuthError::TimedOut`], [`OAuthError::Cancelled`] or
    /// [`OAuthError::Loopback`] for a socket failure.
    pub fn wait_for_code(
        &self,
        expected_state: &str,
        timeout: Duration,
    ) -> Result<String, OAuthError> {
        let (finished, deadline) = mpsc::channel::<()>();
        let stop = Arc::clone(&self.stop);
        let address = self.address;
        std::thread::Builder::new()
            .name("tilo-oauth-deadline".into())
            .spawn(move || {
                if deadline.recv_timeout(timeout) == Err(mpsc::RecvTimeoutError::Timeout) {
                    wake(&stop, TIMED_OUT, address);
                }
            })
            .map_err(|error| OAuthError::Loopback(error.to_string()))?;
        let result = self.accept_redirect(expected_state);
        drop(finished);
        result
    }

    fn accept_redirect(&self, expected_state: &str) -> Result<String, OAuthError> {
        loop {
            let (mut stream, _) = self
                .listener
                .accept()
                .map_err(|error| OAuthError::Loopback(error.to_string()))?;
            match self.stop.load(Ordering::SeqCst) {
                CANCELLED => return Err(OAuthError::Cancelled),
                TIMED_OUT => return Err(OAuthError::TimedOut),
                _ => {}
            }
            let line = read_request_line(&mut stream);
            match parse_redirect_at(&line, &self.redirect.path) {
                Some(RedirectResult::Code { code, state }) => {
                    if state != expected_state {
                        respond(
                            &mut stream,
                            "This sign-in response does not belong to the request Tilo made. \
                             Start again from Settings.",
                        );
                        return Err(OAuthError::StateMismatch);
                    }
                    respond(
                        &mut stream,
                        "Signed in. You can close this tab and return to Tilo.",
                    );
                    return Ok(code);
                }
                Some(RedirectResult::Error(error)) => {
                    respond(
                        &mut stream,
                        "Sign-in was not completed. You can close this tab.",
                    );
                    return Err(OAuthError::Denied(error));
                }
                None => respond(&mut stream, "Waiting for sign-in…"),
            }
        }
    }
}

fn read_request_line(stream: &mut TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buffer = [0_u8; 8192];
    let mut filled = 0_usize;
    while let Some(free) = buffer.get_mut(filled..) {
        if free.is_empty() {
            break;
        }
        match stream.read(free) {
            Ok(0) | Err(_) => break,
            Ok(read) => filled = filled.saturating_add(read),
        }
        if buffer
            .get(..filled)
            .is_some_and(|bytes| bytes.windows(2).any(|pair| pair == b"\r\n"))
        {
            break;
        }
    }
    let text = String::from_utf8_lossy(buffer.get(..filled).unwrap_or_default());
    text.lines().next().unwrap_or_default().to_string()
}

fn respond(stream: &mut TcpStream, message: &str) {
    let page = format!(
        "<!doctype html><html><head><meta charset=utf-8><title>Tilo</title></head>\
         <body style=\"font-family:system-ui;padding:3rem;text-align:center\">{message}</body></html>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Parse the request line of the redirect (`GET /oauth?code=…&state=… HTTP/1.1`).
/// `None` for a request that is not the redirect.
pub fn parse_redirect_line(line: &str) -> Option<RedirectResult> {
    parse_redirect_at(line, REDIRECT_PATH)
}

fn parse_redirect_at(line: &str, expected_path: &str) -> Option<RedirectResult> {
    let target = line.split_whitespace().nth(1)?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != expected_path {
        return None;
    }
    let mut code = None;
    let mut state = String::new();
    let mut error = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = value.into_owned(),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(error) = error {
        let error: String = error
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .take(64)
            .collect();
        return Some(RedirectResult::Error(error));
    }
    code.map(|code| RedirectResult::Code { code, state })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browser_get(port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
            .unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response
    }

    #[test]
    fn redirect_line_parsing() {
        assert_eq!(
            parse_redirect_line("GET /oauth?state=xyz&code=4%2F0Afoo&scope=mail HTTP/1.1"),
            Some(RedirectResult::Code {
                code: "4/0Afoo".into(),
                state: "xyz".into()
            })
        );
        assert_eq!(
            parse_redirect_line("GET /oauth?error=access_denied&state=xyz HTTP/1.1"),
            Some(RedirectResult::Error("access_denied".into()))
        );
        assert_eq!(parse_redirect_line("GET /favicon.ico HTTP/1.1"), None);
        assert_eq!(parse_redirect_line("GET /?code=x HTTP/1.1"), None);
        assert_eq!(parse_redirect_line(""), None);
    }

    #[test]
    fn captures_the_code_after_ignoring_a_favicon_request() {
        let server = LoopbackServer::bind().unwrap();
        assert!(
            server
                .redirect_uri()
                .starts_with(&format!("http://127.0.0.1:{}/", server.port()))
        );
        let port = server.port();
        // The listener is bound, so the browser's connections queue in the
        // backlog until the wait accepts them.
        let browser = std::thread::spawn(move || {
            browser_get(port, "/favicon.ico");
            browser_get(port, "/oauth?code=THECODE&state=STATE123")
        });
        let code = server
            .wait_for_code("STATE123", tilo_test_env::hang_bound())
            .unwrap();
        assert_eq!(code, "THECODE");
        assert!(browser.join().unwrap().contains("close this tab"));
    }

    #[test]
    fn a_registered_host_and_path_are_the_redirect() {
        let redirect = Redirect {
            host: "localhost".into(),
            path: "/auth/callback".into(),
        };
        let server = LoopbackServer::bind().unwrap().at(&redirect);
        let port = server.port();
        assert_eq!(
            server.redirect_uri(),
            format!("http://localhost:{port}/auth/callback")
        );
        let browser = std::thread::spawn(move || {
            browser_get(port, "/oauth?code=WRONG_PATH&state=S");
            browser_get(port, "/auth/callback?code=C&state=S")
        });
        assert_eq!(
            server.wait_for_code("S", tilo_test_env::hang_bound()),
            Ok("C".into())
        );
        browser.join().unwrap();
    }

    #[test]
    fn rejects_a_state_mismatch() {
        let server = LoopbackServer::bind().unwrap();
        let port = server.port();
        let browser = std::thread::spawn(move || browser_get(port, "/oauth?code=X&state=WRONG"));
        assert_eq!(
            server.wait_for_code("EXPECTED", tilo_test_env::hang_bound()),
            Err(OAuthError::StateMismatch)
        );
        browser.join().unwrap();
    }

    #[test]
    fn deadline_and_cancel_end_the_wait() {
        let server = LoopbackServer::bind().unwrap();
        assert_eq!(
            server.wait_for_code("S", Duration::ZERO),
            Err(OAuthError::TimedOut)
        );
        let server = LoopbackServer::bind().unwrap();
        server.canceller().cancel();
        assert_eq!(
            server.wait_for_code("S", tilo_test_env::hang_bound()),
            Err(OAuthError::Cancelled)
        );
    }
}

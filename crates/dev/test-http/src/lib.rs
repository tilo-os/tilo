//! One in-process HTTP/1.1 responder for tests.
//!
//! Protocol clients kept growing their own throwaway `TcpListener` loops, each
//! with slightly different request parsing and none of them able to express a
//! hostile reply. [`Responder`] replaces them: it answers a scripted sequence
//! of [`Reply`]s, one per connection, and records every [`Request`] it saw so
//! the test can assert on methods, headers and bodies.
//!
//! Every reply closes its connection, so a client never reuses a socket across
//! scripted replies and the request order is exactly the reply order.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

/// How long [`Responder::next_request`] waits before declaring the client
/// never sent the request the test expected.
const REQUEST_WAIT: Duration = Duration::from_secs(10);
/// Largest request a test client is expected to send.
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

/// One scripted response.
#[derive(Clone, Debug)]
pub struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    content_length: bool,
}

impl Reply {
    /// A reply with `status`, no extra headers and an empty body.
    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            content_length: true,
        }
    }

    /// Add a response header.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// Set the response body.
    #[must_use]
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// Omit `Content-Length` and delimit the body by closing the connection,
    /// the shape a hostile server uses to defeat length-header checks.
    #[must_use]
    pub fn without_content_length(mut self) -> Self {
        self.content_length = false;
        self
    }

    fn reason(&self) -> &'static str {
        match self.status {
            200 => "OK",
            201 => "Created",
            204 => "No Content",
            207 => "Multi-Status",
            301 => "Moved Permanently",
            302 => "Found",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            412 => "Precondition Failed",
            _ => "Response",
        }
    }

    fn write_to(&self, stream: &mut TcpStream) -> std::io::Result<()> {
        let mut head = format!("HTTP/1.1 {} {}\r\n", self.status, self.reason());
        if self.content_length {
            let _ = write!(head, "Content-Length: {}\r\n", self.body.len());
        }
        head.push_str("Connection: close\r\n");
        for (name, value) in &self.headers {
            let _ = write!(head, "{name}: {value}\r\n");
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        stream.write_all(&self.body)?;
        stream.flush()
    }
}

/// One request the responder received.
#[derive(Clone, Debug)]
pub struct Request {
    /// The request method as sent, e.g. `PROPFIND`.
    pub method: String,
    /// The request target, normally an origin-form path.
    pub target: String,
    /// Header names and values in wire order.
    pub headers: Vec<(String, String)>,
    /// The request body.
    pub body: Vec<u8>,
}

impl Request {
    /// The first header named `name`, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The body as text, lossily decoded.
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// A listener on `127.0.0.1` that answers a scripted reply sequence.
pub struct Responder {
    listener: TcpListener,
    port: u16,
    requests: Receiver<Request>,
}

impl Responder {
    /// Answer `replies` in order, one connection each.
    ///
    /// # Panics
    /// If the loopback listener cannot be created.
    #[allow(
        clippy::expect_used,
        reason = "a test fixture that cannot bind loopback has no meaningful fallback"
    )]
    pub fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback responder");
        let port = listener.local_addr().expect("responder address").port();
        let (sender, requests) = mpsc::channel();
        if !replies.is_empty() {
            let worker = listener.try_clone().expect("clone responder listener");
            std::thread::spawn(move || serve(&worker, replies, &sender));
        }
        Self {
            listener,
            port,
            requests,
        }
    }

    /// A responder that answers nothing, for asserting that a client never
    /// connected (see [`Responder::assert_untouched`]).
    pub fn silent() -> Self {
        Self::start(Vec::new())
    }

    /// The listening port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `http://127.0.0.1:<port>` without a trailing slash.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The next recorded request.
    ///
    /// # Panics
    /// If no request arrives in time.
    #[allow(
        clippy::expect_used,
        reason = "a missing request is the test failure this fixture reports"
    )]
    pub fn next_request(&self) -> Request {
        self.requests
            .recv_timeout(REQUEST_WAIT)
            .expect("the client never sent the expected request")
    }

    /// Assert that no connection is waiting on a silent responder.
    ///
    /// # Panics
    /// If a client connected.
    #[allow(
        clippy::expect_used,
        clippy::panic,
        reason = "an unexpected connection is the test failure this fixture reports"
    )]
    pub fn assert_untouched(&self) {
        self.listener
            .set_nonblocking(true)
            .expect("nonblocking responder");
        match self.listener.accept() {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Ok(_) => panic!("a client connected to a responder that must stay untouched"),
            Err(error) => panic!("responder listener failed: {error}"),
        }
    }
}

fn serve(listener: &TcpListener, replies: Vec<Reply>, sender: &Sender<Request>) {
    for reply in replies {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let Some(request) = read_request(&mut stream) else {
            return;
        };
        if sender.send(request).is_err() {
            return;
        }
        if reply.write_to(&mut stream).is_err() {
            return;
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    stream.set_read_timeout(Some(REQUEST_WAIT)).ok()?;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    let header_end = loop {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(buffer.get(..read)?);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position.checked_add(4)?;
        }
        if bytes.len() > MAX_REQUEST_BYTES {
            return None;
        }
    };
    let head = String::from_utf8_lossy(bytes.get(..header_end)?).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let target = request_line.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect();
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let total = header_end.checked_add(length)?;
    if total > MAX_REQUEST_BYTES {
        return None;
    }
    while bytes.len() < total {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(buffer.get(..read)?);
    }
    Some(Request {
        method,
        target,
        headers,
        body: bytes.get(header_end..total)?.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(port: u16, raw: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(raw.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn answers_in_order_and_records_requests() {
        let responder = Responder::start(vec![
            Reply::status(207).header("ETag", "\"v1\"").body("<x/>"),
            Reply::status(204),
        ]);
        let first = roundtrip(
            responder.port(),
            "PROPFIND /dav/ HTTP/1.1\r\nDepth: 1\r\nContent-Length: 4\r\n\r\nbody",
        );
        assert!(
            first.starts_with("HTTP/1.1 207 Multi-Status\r\n"),
            "{first}"
        );
        assert!(first.contains("ETag: \"v1\""));
        assert!(first.ends_with("<x/>"));
        let second = roundtrip(responder.port(), "DELETE /dav/a HTTP/1.1\r\n\r\n");
        assert!(second.starts_with("HTTP/1.1 204"));

        let propfind = responder.next_request();
        assert_eq!(propfind.method, "PROPFIND");
        assert_eq!(propfind.target, "/dav/");
        assert_eq!(propfind.header("depth"), Some("1"));
        assert_eq!(propfind.body_text(), "body");
        assert_eq!(responder.next_request().method, "DELETE");
    }

    #[test]
    fn streams_a_body_without_content_length() {
        let responder = Responder::start(vec![
            Reply::status(200).body("abc").without_content_length(),
        ]);
        let response = roundtrip(responder.port(), "GET / HTTP/1.1\r\n\r\n");
        assert!(!response.to_ascii_lowercase().contains("content-length"));
        assert!(response.ends_with("abc"));
    }

    #[test]
    fn silent_responder_detects_no_connection() {
        Responder::silent().assert_untouched();
    }
}

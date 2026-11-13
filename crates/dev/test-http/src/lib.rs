//! One in-process HTTP/1.1 responder for tests. [`Responder`] answers a scripted sequence of
//! [`Reply`]s, one per connection, and records every [`Request`] it saw so the test can assert on
//! methods, headers and bodies; it can also express a hostile reply (a body with no length, one cut
//! short, a malformed head), which a plain `TcpListener` loop cannot.
//!
//! Every scripted reply closes its connection, so a client never reuses a socket across scripted
//! replies and the request order is exactly the reply order.
//!
//! [`Responder::handle`] answers with a function instead of a script, for a stateful fake server.
//! [`Responder::serve`] hands the function the whole [`Peer`] connection, for a reply written in
//! pieces while the test watches the client, for a client that keeps its connection across
//! requests, or for a test that needs to see the client hang up. Each connection is served on its
//! own thread.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

/// How long [`Responder::next_request`] waits before declaring that the client never sent the
/// request the test expected.
const REQUEST_WAIT: Duration = Duration::from_secs(10);
/// Largest request a test client is expected to send.
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    content_length: bool,
    close: bool,
}

impl Reply {
    /// A reply with `status`, no extra headers and an empty body.
    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            content_length: true,
            close: true,
        }
    }

    /// Leave the connection open after the reply, so the client sends its next request on the same
    /// socket (see [`Peer::next_request`]).
    #[must_use]
    pub fn keep_alive(mut self) -> Self {
        self.close = false;
        self
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

    /// Omit `Content-Length` and delimit the body by closing the connection, the shape a hostile
    /// server uses to defeat length-header checks.
    #[must_use]
    pub fn without_content_length(mut self) -> Self {
        self.content_length = false;
        self
    }

    /// Send only the first half of the body and close the connection, the way a dropped download
    /// ends. With `declare_length` the head still announces the full length, so a client that holds
    /// the server to its `Content-Length` notices; without it only a client that knows how many
    /// bytes to expect can tell.
    #[must_use]
    pub fn cut_short(mut self, declare_length: bool) -> Self {
        let full = self.body.len();
        self.body.truncate(full.checked_div(2).unwrap_or_default());
        if declare_length {
            self.headers
                .push(("Content-Length".to_owned(), full.to_string()));
        }
        self.content_length = false;
        self
    }

    fn reason(&self) -> &'static str {
        match self.status {
            200 => "OK",
            201 => "Created",
            204 => "No Content",
            206 => "Partial Content",
            207 => "Multi-Status",
            301 => "Moved Permanently",
            302 => "Found",
            400 => "Bad Request",
            401 => "Unauthorized",
            402 => "Payment Required",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            409 => "Conflict",
            412 => "Precondition Failed",
            416 => "Range Not Satisfiable",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            507 => "Insufficient Storage",
            _ => "Response",
        }
    }

    fn write_to(&self, stream: &mut TcpStream) -> std::io::Result<()> {
        let mut head = format!("HTTP/1.1 {} {}\r\n", self.status, self.reason());
        if self.content_length {
            let _ = write!(head, "Content-Length: {}\r\n", self.body.len());
        }
        if self.close {
            head.push_str("Connection: close\r\n");
        }
        for (name, value) in &self.headers {
            let _ = write!(head, "{name}: {value}\r\n");
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        stream.write_all(&self.body)?;
        stream.flush()
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    /// The request method as sent, e.g. `PROPFIND`.
    pub method: String,
    /// The request target, normally an origin-form path.
    pub target: String,
    /// Header names and values in wire order.
    pub headers: Vec<(String, String)>,
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

pub struct Responder {
    listener: TcpListener,
    port: u16,
    requests: Receiver<Request>,
    stop: Option<Arc<AtomicBool>>,
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
            stop: None,
        }
    }

    /// Answer every request with `handler` until the responder is dropped. `None` closes the
    /// connection without a response, the way a server that goes away mid-request looks to a client.
    /// Requests are also recorded for [`Responder::next_request`].
    ///
    /// # Panics
    /// If the loopback listener cannot be created.
    pub fn handle<F>(handler: F) -> Self
    where
        F: Fn(&Request) -> Option<Reply> + Send + Sync + 'static,
    {
        Self::serve(move |peer| {
            let reply = handler(peer.request());
            if let Some(reply) = reply {
                peer.reply(&reply);
            }
        })
    }

    /// Answer every connection with `handler`, which writes what the client sees through [`Peer`]: a
    /// whole [`Reply`], raw bytes in as many pieces as the test wants, the client's next request on
    /// the same connection, or the wait for the client to hang up.
    ///
    /// # Panics
    /// If the loopback listener cannot be created.
    #[allow(
        clippy::expect_used,
        reason = "a test fixture that cannot bind loopback has no meaningful fallback"
    )]
    pub fn serve<F>(handler: F) -> Self
    where
        F: Fn(&mut Peer) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback responder");
        let port = listener.local_addr().expect("responder address").port();
        let (sender, requests) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker = listener.try_clone().expect("clone responder listener");
        let handler = Arc::new(handler);
        let stopping = Arc::clone(&stop);
        std::thread::spawn(move || {
            for stream in worker.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(mut stream) = stream else { continue };
                let handler = Arc::clone(&handler);
                let sender = sender.clone();
                std::thread::spawn(move || {
                    let Some(request) = read_request(&mut stream) else {
                        return;
                    };
                    let _ = sender.send(request.clone());
                    handler(&mut Peer {
                        stream: &mut stream,
                        request,
                        recorded: sender,
                    });
                });
            }
        });
        Self {
            listener,
            port,
            requests,
            stop: Some(stop),
        }
    }

    /// Every request recorded so far that has not been taken yet.
    pub fn drain_requests(&self) -> Vec<Request> {
        self.requests.try_iter().collect()
    }

    /// A responder that answers nothing, for asserting that a client never connected (see
    /// [`Responder::assert_untouched`]).
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

/// One connection a [`Responder`] is answering, handed to a [`Responder::serve`] handler.
pub struct Peer<'a> {
    stream: &'a mut TcpStream,
    request: Request,
    recorded: Sender<Request>,
}

impl Peer<'_> {
    /// The request that opened this connection.
    pub fn request(&self) -> &Request {
        &self.request
    }

    /// Write a whole reply.
    pub fn reply(&mut self, reply: &Reply) {
        let _ = reply.write_to(self.stream);
    }

    /// Write `bytes` exactly as given, without a head, so a reply can arrive in pieces or be
    /// malformed on purpose. `false` once the client has gone away.
    #[must_use]
    pub fn write(&mut self, bytes: &[u8]) -> bool {
        self.stream.write_all(bytes).is_ok() && self.stream.flush().is_ok()
    }

    /// The client's next request on this connection, or `None` when it closed instead of asking
    /// again.
    pub fn next_request(&mut self) -> Option<Request> {
        let request = read_request(self.stream)?;
        let _ = self.recorded.send(request.clone());
        Some(request)
    }

    /// Wait for the client to hang up. `true` once it has, `false` if it sent
    /// more instead or the wait ran out.
    pub fn wait_for_close(&mut self) -> bool {
        if self.stream.set_read_timeout(Some(REQUEST_WAIT)).is_err() {
            return false;
        }
        let mut buffer = [0_u8; 1024];
        match self.stream.read(&mut buffer) {
            Ok(0) => true,
            Ok(_) => false,
            Err(error) => matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
            ),
        }
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::SeqCst);
            // Wake the accept loop so it sees the flag and exits.
            let _ = TcpStream::connect(("127.0.0.1", self.port));
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
    let chunked = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding") && value.eq_ignore_ascii_case("chunked")
    });
    if chunked {
        let mut rest = bytes.get(header_end..)?.to_vec();
        let body = read_chunked(stream, &mut rest)?;
        return Some(Request {
            method,
            target,
            headers,
            body,
        });
    }
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

/// Decode a chunked body; `buffered` holds bytes already read past the head.
fn read_chunked(stream: &mut TcpStream, buffered: &mut Vec<u8>) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let line_end = loop {
            if let Some(position) = buffered.windows(2).position(|window| window == b"\r\n") {
                break position;
            }
            let read = stream.read(&mut buffer).ok()?;
            if read == 0 {
                return None;
            }
            buffered.extend_from_slice(buffer.get(..read)?);
        };
        let line = String::from_utf8_lossy(buffered.get(..line_end)?).into_owned();
        let size = usize::from_str_radix(line.split(';').next()?.trim(), 16).ok()?;
        let start = line_end.checked_add(2)?;
        let needed = start.checked_add(size)?.checked_add(2)?;
        while buffered.len() < needed {
            let read = stream.read(&mut buffer).ok()?;
            if read == 0 {
                return None;
            }
            buffered.extend_from_slice(buffer.get(..read)?);
        }
        body.extend_from_slice(buffered.get(start..start.checked_add(size)?)?);
        buffered.drain(..needed);
        if size == 0 {
            return Some(body);
        }
        if body.len() > MAX_REQUEST_BYTES {
            return None;
        }
    }
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

    /// Answers go out in order with requests recorded, a body streams without Content-Length, and a
    /// silent responder detects no connection.
    #[test]
    fn the_test_server_answers_streams_and_detects_silence() {
        // Phase: answers_in_order_and_records_requests.
        {
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

        // Phase: streams_a_body_without_content_length.
        {
            let responder = Responder::start(vec![
                Reply::status(200).body("abc").without_content_length(),
            ]);
            let response = roundtrip(responder.port(), "GET / HTTP/1.1\r\n\r\n");
            assert!(!response.to_ascii_lowercase().contains("content-length"));
            assert!(response.ends_with("abc"));
        }

        // Phase: cuts_a_body_short_with_or_without_its_length.
        {
            let responder = Responder::start(vec![
                Reply::status(200).body("abcdef").cut_short(true),
                Reply::status(206).body("abcdef").cut_short(false),
            ]);
            let declared = roundtrip(responder.port(), "GET / HTTP/1.1\r\n\r\n");
            assert!(declared.contains("Content-Length: 6\r\n"), "{declared}");
            assert!(declared.ends_with("\r\n\r\nabc"), "{declared}");
            let undeclared = roundtrip(responder.port(), "GET / HTTP/1.1\r\n\r\n");
            assert!(!undeclared.to_ascii_lowercase().contains("content-length"));
            assert!(undeclared.ends_with("\r\n\r\nabc"), "{undeclared}");
        }

        // Phase: silent_responder_detects_no_connection.
        {
            Responder::silent().assert_untouched();
        }
    }

    /// A served connection carries a second request, its reply arrives in the pieces the handler
    /// writes, and the handler sees the client hang up.
    #[test]
    fn a_served_connection_streams_keeps_alive_and_reports_the_client_leaving() {
        let responder = Responder::serve(|peer| {
            assert_eq!(peer.request().target, "/first");
            peer.reply(&Reply::status(200).body("one").keep_alive());
            let next = peer
                .next_request()
                .expect("the client asks again on the same connection");
            assert_eq!(next.target, "/second");
            let _ = peer.write(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n");
            let _ = peer.write(b"tw");
            let _ = peer.write(b"o!");
            peer.wait_for_close();
        });
        let mut stream = TcpStream::connect(("127.0.0.1", responder.port())).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream.write_all(b"GET /first HTTP/1.1\r\n\r\n").unwrap();
        let mut first = [0_u8; 128];
        let read = stream.read(&mut first).unwrap();
        let first = String::from_utf8_lossy(first.get(..read).unwrap()).into_owned();
        assert!(first.starts_with("HTTP/1.1 200 OK\r\n"), "{first}");
        assert!(first.ends_with("\r\n\r\none"), "{first}");
        stream.write_all(b"GET /second HTTP/1.1\r\n\r\n").unwrap();
        let mut second = String::new();
        while !second.ends_with("two!") {
            let mut buffer = [0_u8; 128];
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "the responder closed early: {second}");
            second.push_str(&String::from_utf8_lossy(buffer.get(..read).unwrap()));
        }
        assert!(second.starts_with("HTTP/1.1 200 OK\r\n"), "{second}");
        drop(stream);
        assert_eq!(responder.drain_requests().len(), 2);
    }

    #[test]
    fn a_handler_answers_concurrent_and_chunked_requests() {
        let responder = Responder::handle(|request| {
            (request.method != "DROP").then(|| {
                Reply::status(200).body(format!("{}:{}", request.target, request.body_text()))
            })
        });
        let port = responder.port();
        let threads: Vec<_> = (0..4)
            .map(|i| {
                std::thread::spawn(move || {
                    roundtrip(
                        port,
                        &format!("GET /{i} HTTP/1.1\r\nContent-Length: 1\r\n\r\nx"),
                    )
                })
            })
            .collect();
        for (i, thread) in threads.into_iter().enumerate() {
            assert!(thread.join().unwrap().ends_with(&format!("/{i}:x")));
        }
        let chunked = roundtrip(
            port,
            "PUT /c HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n",
        );
        assert!(chunked.ends_with("/c:abcde"), "{chunked}");
        assert_eq!(roundtrip(port, "DROP / HTTP/1.1\r\n\r\n"), "");
        assert_eq!(responder.drain_requests().len(), 6);
    }
}

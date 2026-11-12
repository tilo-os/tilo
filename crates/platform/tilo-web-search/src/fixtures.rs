//! A server on this machine that answers like a search engine or a web page.
//!
//! Test builds only (`fixture-servers`): [`crate::fetch_fixture`] and
//! [`crate::search_fixture`] reach it, where the shipped paths refuse this
//! machine's own addresses, and a test reads the engines' scrapers and the
//! page reader against answers it writes itself.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

/// One answer a fixture server gives: the path it answers, the media type it
/// labels the body with, and the body itself.
#[derive(Clone, Debug)]
pub struct Route {
    /// The path answered, without its query (`/search`).
    pub path: String,
    /// The body's media type; empty sends the body with no type at all.
    pub content_type: String,
    pub body: Vec<u8>,
}

impl Route {
    /// A route answering `path` with `body`, labelled `content_type`.
    #[must_use]
    pub fn new(path: &str, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.to_owned(),
            content_type: content_type.to_owned(),
            body: body.into(),
        }
    }
}

/// One request a fixture server answered, as the engine sent it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// `GET` or `POST`.
    pub method: String,
    /// The path asked for, without its query.
    pub path: String,
    /// The query string, as sent (`q=rust&adlt=strict`), or empty.
    pub query: String,
    /// The body of a form submission.
    pub body: String,
}

impl Request {
    /// The value of `name` in the query string or the form body, whichever
    /// carried it.
    pub fn parameter(&self, name: &str) -> Option<String> {
        self.parameters().into_iter().find_map(
            |(key, value)| {
                if key == name { Some(value) } else { None }
            },
        )
    }

    /// Every parameter of the query string and of the form body, in order.
    pub fn parameters(&self) -> Vec<(String, String)> {
        let mut parameters = Vec::new();
        for source in [self.query.as_str(), self.body.as_str()] {
            for pair in source.split('&').filter(|pair| !pair.is_empty()) {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                parameters.push((decode(key), decode(value)));
            }
        }
        parameters
    }
}

/// A parameter as it was sent: `+` is a space and `%xx` its byte.
fn decode(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        at = at.saturating_add(1);
        match byte {
            b'+' => decoded.push(b' '),
            b'%' => {
                let hex = bytes
                    .get(at..at.saturating_add(2))
                    .and_then(|hex| std::str::from_utf8(hex).ok())
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                match hex {
                    Some(byte) => {
                        decoded.push(byte);
                        at = at.saturating_add(2);
                    }
                    None => decoded.push(byte),
                }
            }
            byte => decoded.push(byte),
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// A server on this machine, listening on a free port, answering `routes`
/// until it is dropped.
pub struct FixtureServer {
    address: SocketAddr,
    base: String,
    stop: Arc<AtomicBool>,
    listening: Option<JoinHandle<()>>,
    seen: Arc<Mutex<Vec<Request>>>,
}

impl FixtureServer {
    /// Answer `routes` on a free port of this machine from now on.
    ///
    /// # Errors
    ///
    /// [`std::io::Error`] when the address cannot be bound or the thread
    /// cannot be started.
    pub fn serve(routes: Vec<Route>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let routes = Arc::new(routes);
        let listening = {
            let stop = Arc::clone(&stop);
            let seen = Arc::clone(&seen);
            let routes = Arc::clone(&routes);
            std::thread::Builder::new()
                .name("tilo-web-search-fixtures".into())
                .spawn(move || accept(&listener, &routes, &seen, &stop))?
        };
        Ok(Self {
            address,
            base: format!("http://{address}"),
            stop,
            listening: Some(listening),
            seen,
        })
    }

    /// Where the server listens, such as `http://127.0.0.1:41234`.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The address of `path` on the server.
    #[must_use]
    pub fn at(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Every request answered so far, in the order they arrived.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Accept is parked until a connection arrives: this one wakes it so
        // the thread reads the flag and ends.
        if TcpStream::connect(self.address).is_err() {
            return;
        }
        if let Some(listening) = self.listening.take()
            && listening.join().is_err()
        {
            tracing::warn!("the fixture server's thread did not end cleanly");
        }
    }
}

/// Answer every connection until the server is dropped.
fn accept(
    listener: &TcpListener,
    routes: &Arc<Vec<Route>>,
    seen: &Arc<Mutex<Vec<Request>>>,
    stop: &AtomicBool,
) {
    while !stop.load(Ordering::Relaxed) {
        let (stream, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "the fixture server could not accept a connection");
                return;
            }
        };
        let routes = Arc::clone(routes);
        let seen = Arc::clone(seen);
        if let Err(error) = std::thread::Builder::new()
            .name("tilo-web-search-fixture-answer".into())
            .spawn(move || answer(&stream, &routes, &seen))
        {
            tracing::warn!(%error, "the fixture server could not answer a connection");
        }
    }
}

/// Read one request from `stream` and answer it with the route for its path,
/// or a 404.
fn answer(stream: &TcpStream, routes: &[Route], seen: &Mutex<Vec<Request>>) {
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let Some(request) = read_request(&mut reader) else {
        return;
    };
    seen.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(request.clone());
    let found = routes.iter().find(|route| route.path == request.path);
    let mut writer = stream;
    let (head, body) = match found {
        Some(route) => {
            let kind = if route.content_type.is_empty() {
                String::new()
            } else {
                format!("Content-Type: {}\r\n", route.content_type)
            };
            (
                format!(
                    "HTTP/1.1 200 OK\r\n{kind}Content-Length: {}\r\n",
                    route.body.len()
                ),
                route.body.as_slice(),
            )
        }
        None => (
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n".to_owned(),
            &[][..],
        ),
    };
    let head = format!("{head}Connection: close\r\n\r\n");
    let answered = writer
        .write_all(head.as_bytes())
        .and_then(|()| writer.write_all(body))
        .and_then(|()| writer.flush());
    if answered.is_err() {
        tracing::debug!("a fixture client went away before its answer");
    }
}

/// The request a client sent, or `None` when the connection ended first.
fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next().unwrap_or("/").to_owned();
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let body = if length == 0 {
        Vec::new()
    } else {
        let mut body = vec![0; length];
        reader.read_exact(&mut body).ok()?;
        body
    };
    Some(Request {
        method,
        path: path.to_owned(),
        query: query.to_owned(),
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

//! A server on this machine that answers like a search engine or a web page.
//!
//! Test builds only (`fixture-servers`): the fixture entry points reach it where the
//! shipped paths refuse this machine's own addresses, so a test reads the engines'
//! scrapers and the page reader against answers it writes itself.

use std::sync::{Arc, Mutex, PoisonError};

#[derive(Clone, Debug)]
pub struct Route {
    pub path: String,
    /// The body's media type; empty sends the body with no type at all.
    pub content_type: String,
    pub body: Vec<u8>,
}

impl Route {
    #[must_use]
    pub fn new(path: &str, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.to_owned(),
            content_type: content_type.to_owned(),
            body: body.into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// The query string, as sent (`q=rust&adlt=strict`), or empty.
    pub query: String,
    pub body: String,
}

impl Request {
    pub fn parameter(&self, name: &str) -> Option<String> {
        self.parameters().into_iter().find_map(
            |(key, value)| {
                if key == name { Some(value) } else { None }
            },
        )
    }

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

/// A server on this machine, listening on a free port, answering `routes` (404 otherwise).
pub struct FixtureServer {
    responder: test_http::Responder,
    seen: Arc<Mutex<Vec<Request>>>,
}

impl FixtureServer {
    /// Answer `routes` on a free port of this machine from now on, 404 otherwise.
    ///
    /// # Panics
    ///
    /// If the port cannot be bound.
    #[allow(
        clippy::expect_used,
        reason = "a fixture server that cannot bind loopback has no meaningful fallback"
    )]
    #[must_use]
    pub fn serve(routes: Vec<Route>) -> Self {
        let routes = Arc::new(routes);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        let responder = test_http::Responder::handle(move |request| {
            let (path, query) = request
                .target
                .split_once('?')
                .unwrap_or((request.target.as_str(), ""));
            recorded
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(Request {
                    method: request.method.clone(),
                    path: path.to_owned(),
                    query: query.to_owned(),
                    body: request.body_text(),
                });
            let Some(found) = routes.iter().find(|route| route.path == path) else {
                return Some(test_http::Reply::status(404));
            };
            let reply = test_http::Reply::status(200).body(found.body.clone());
            Some(if found.content_type.is_empty() {
                reply
            } else {
                reply.header("Content-Type", &found.content_type)
            })
        });
        Self { responder, seen }
    }

    #[must_use]
    pub fn at(&self, path: &str) -> String {
        format!("{}{path}", self.responder.url())
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

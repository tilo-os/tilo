//! Blocking `WebDAV` client on Tilo's TLS defaults.
//!
//! The client builds requests — method, headers, credentials, conditional
//! `ETag`s — and reads bounded bodies. It deliberately does not interpret
//! status codes: a 412, a 404 on delete or a 403 from an expired sync token
//! mean different things to different callers, so [`DavResponse`] hands the
//! status back and callers map it.

use std::fmt;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH};
use reqwest::{IntoUrl, Method};
use zeroize::Zeroizing;

pub use reqwest::{StatusCode, Url};

use crate::body::{PropName, propfind_body};
use crate::multistatus::{self, Limits, Multistatus, XmlError};

const XML_CONTENT_TYPE: &str = "application/xml; charset=utf-8";

/// HTTP Basic credentials, zeroized on drop.
pub struct Credentials {
    username: String,
    password: Zeroizing<String>,
}

impl Credentials {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: Zeroizing::new(password.into()),
        }
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// What the client does with a 3xx.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redirects {
    /// Follow up to ten hops. reqwest drops `Authorization` when a hop
    /// changes host, so credentials never follow a cross-origin redirect.
    Follow,
    /// Return the 3xx to the caller. Stored endpoints use this: discovery
    /// belongs to account setup, and a request body is never replayed.
    Refuse,
}

/// Transport configuration.
#[derive(Clone, Debug)]
pub struct Options {
    pub user_agent: Option<&'static str>,
    /// Whole-request deadline.
    pub timeout: Duration,
    pub connect_timeout: Option<Duration>,
    pub redirects: Redirects,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            user_agent: None,
            timeout: Duration::from_secs(20),
            connect_timeout: None,
            redirects: Redirects::Follow,
        }
    }
}

/// `Depth` header values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    Zero,
    One,
    Infinity,
}

impl Depth {
    fn header(self) -> &'static str {
        match self {
            Self::Zero => "0",
            Self::One => "1",
            Self::Infinity => "infinity",
        }
    }
}

/// A write/delete precondition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precondition<'a> {
    /// Unconditional.
    None,
    /// `If-Match: <etag>`; `"*"` requires only that the resource exists.
    IfMatch(&'a str),
    /// `If-None-Match: *`: create only, never overwrite.
    IfNoneMatchAny,
}

/// Transport and bounded-read failures. HTTP statuses are not errors.
#[derive(Debug)]
pub enum Error {
    /// The HTTP client could not be built.
    Build(reqwest::Error),
    /// The request could not be sent or the response headers not received.
    Transport(reqwest::Error),
    /// Reading the response body failed.
    Read(std::io::Error),
    /// The body is larger than the caller's limit.
    TooLarge,
    /// The body is not an acceptable multistatus document.
    Xml(XmlError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build(error) | Self::Transport(error) => fmt::Display::fmt(error, f),
            Self::Read(error) => fmt::Display::fmt(error, f),
            Self::TooLarge => f.write_str("DAV response exceeds the size limit"),
            Self::Xml(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for Error {}

/// A blocking DAV client. Cloning is cheap and shares the connection pool.
#[derive(Clone, Debug)]
pub struct DavClient {
    http: Client,
    credentials: Option<Arc<Credentials>>,
}

impl DavClient {
    /// # Errors
    /// If the TLS/HTTP stack cannot be initialised.
    pub fn new(options: &Options) -> Result<Self, Error> {
        let mut builder = tilo_net::http::blocking_client().timeout(options.timeout);
        if let Some(timeout) = options.connect_timeout {
            builder = builder.connect_timeout(timeout);
        }
        if let Some(agent) = options.user_agent {
            builder = builder.user_agent(agent);
        }
        if options.redirects == Redirects::Refuse {
            builder = builder.redirect(reqwest::redirect::Policy::none());
        }
        Ok(Self {
            http: builder.build().map_err(Error::Build)?,
            credentials: None,
        })
    }

    /// The same transport, authenticating every request with `credentials`.
    #[must_use]
    pub fn with_credentials(&self, credentials: Credentials) -> Self {
        Self {
            http: self.http.clone(),
            credentials: Some(Arc::new(credentials)),
        }
    }

    /// `PROPFIND` for `props` at `depth`.
    ///
    /// # Errors
    /// On transport failure.
    pub fn propfind(
        &self,
        url: impl IntoUrl,
        depth: Depth,
        props: &[PropName],
    ) -> Result<DavResponse, Error> {
        let request = self
            .request(method(b"PROPFIND"), url)
            .header("Depth", depth.header())
            .header(CONTENT_TYPE, XML_CONTENT_TYPE)
            .body(propfind_body(props));
        send(request)
    }

    /// `REPORT` with an XML `body` (see [`crate::body`]).
    ///
    /// # Errors
    /// On transport failure.
    pub fn report(
        &self,
        url: impl IntoUrl,
        depth: Depth,
        body: String,
    ) -> Result<DavResponse, Error> {
        let request = self
            .request(method(b"REPORT"), url)
            .header("Depth", depth.header())
            .header(CONTENT_TYPE, XML_CONTENT_TYPE)
            .body(body);
        send(request)
    }

    /// `GET`.
    ///
    /// # Errors
    /// On transport failure.
    pub fn get(&self, url: impl IntoUrl) -> Result<DavResponse, Error> {
        send(self.request(Method::GET, url))
    }

    /// `PUT` `body` under `precondition`, declaring `content_type` if given.
    ///
    /// # Errors
    /// On transport failure.
    pub fn put(
        &self,
        url: impl IntoUrl,
        body: Vec<u8>,
        content_type: Option<&str>,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, Error> {
        let mut request = self.request(Method::PUT, url).body(body);
        if let Some(content_type) = content_type {
            request = request.header(CONTENT_TYPE, content_type);
        }
        send(apply(request, precondition))
    }

    /// `DELETE` under `precondition`.
    ///
    /// # Errors
    /// On transport failure.
    pub fn delete(
        &self,
        url: impl IntoUrl,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, Error> {
        send(apply(self.request(Method::DELETE, url), precondition))
    }

    /// `MKCOL`.
    ///
    /// # Errors
    /// On transport failure.
    pub fn mkcol(&self, url: impl IntoUrl) -> Result<DavResponse, Error> {
        send(self.request(method(b"MKCOL"), url))
    }

    /// `MOVE` to `destination`; `overwrite: false` sends `Overwrite: F`.
    ///
    /// # Errors
    /// On transport failure.
    pub fn move_to(
        &self,
        url: impl IntoUrl,
        destination: &Url,
        overwrite: bool,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, Error> {
        let request = self
            .request(method(b"MOVE"), url)
            .header("Destination", destination.as_str())
            .header("Overwrite", if overwrite { "T" } else { "F" });
        send(apply(request, precondition))
    }

    fn request(&self, method: Method, url: impl IntoUrl) -> RequestBuilder {
        let request = self.http.request(method, url);
        match &self.credentials {
            Some(credentials) => {
                request.basic_auth(&credentials.username, Some(credentials.password.as_str()))
            }
            None => request,
        }
    }
}

/// A response whose body has not been read yet.
#[derive(Debug)]
pub struct DavResponse {
    inner: Response,
}

impl DavResponse {
    pub fn status(&self) -> StatusCode {
        self.inner.status()
    }

    /// The `ETag` header, if present and visible ASCII.
    pub fn etag(&self) -> Option<String> {
        self.inner
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned)
    }

    /// Read at most `limit` bytes. A server that declares or streams more is
    /// refused without buffering past the limit, whether or not it sent a
    /// `Content-Length`.
    ///
    /// # Errors
    /// [`Error::TooLarge`] past the limit, [`Error::Read`] on I/O failure.
    pub fn read_bounded(self, limit: usize) -> Result<Vec<u8>, Error> {
        let limit64 = u64::try_from(limit).unwrap_or(u64::MAX);
        if self
            .inner
            .content_length()
            .is_some_and(|length| length > limit64)
        {
            return Err(Error::TooLarge);
        }
        let mut bytes = Vec::new();
        self.inner
            .take(limit64.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(Error::Read)?;
        if bytes.len() > limit {
            return Err(Error::TooLarge);
        }
        Ok(bytes)
    }

    /// Read and parse a multistatus body under `limits`.
    ///
    /// # Errors
    /// As [`Self::read_bounded`] with `limits.max_bytes`, then [`Error::Xml`].
    pub fn multistatus(self, limits: &Limits) -> Result<Multistatus, Error> {
        let bytes = self.read_bounded(limits.max_bytes)?;
        multistatus::parse_bytes(&bytes, limits).map_err(Error::Xml)
    }
}

fn apply(request: RequestBuilder, precondition: Precondition<'_>) -> RequestBuilder {
    match precondition {
        Precondition::None => request,
        Precondition::IfMatch(etag) => request.header(IF_MATCH, etag),
        Precondition::IfNoneMatchAny => request.header(IF_NONE_MATCH, "*"),
    }
}

fn send(request: RequestBuilder) -> Result<DavResponse, Error> {
    request
        .send()
        .map(|inner| DavResponse { inner })
        .map_err(Error::Transport)
}

/// Extension methods are fixed ASCII tokens; `from_bytes` only fails on
/// invalid token characters.
fn method(name: &'static [u8]) -> Method {
    Method::from_bytes(name).unwrap_or(Method::GET)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::props;
    use test_http::{Reply, Responder};

    fn client() -> DavClient {
        DavClient::new(&Options::default())
            .unwrap()
            .with_credentials(Credentials::new("ada", "s3cret"))
    }

    const LISTING: &str = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/a.txt</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;

    #[test]
    fn propfind_sends_depth_props_and_basic_auth() {
        let server = Responder::start(vec![Reply::status(207).body(LISTING)]);
        let response = client()
            .propfind(
                format!("{}/dav/", server.url()),
                Depth::One,
                &[props::GETETAG],
            )
            .unwrap();
        assert_eq!(response.status(), StatusCode::MULTI_STATUS);
        let listing = response.multistatus(&Limits::default()).unwrap();
        assert_eq!(listing.responses[0].prop_text("getetag"), Some("\"1\""));
        let request = server.next_request();
        assert_eq!(request.method, "PROPFIND");
        assert_eq!(request.target, "/dav/");
        assert_eq!(request.header("depth"), Some("1"));
        assert_eq!(request.header("content-type"), Some(XML_CONTENT_TYPE));
        assert_eq!(
            request.header("authorization"),
            Some("Basic YWRhOnMzY3JldA==")
        );
        assert!(request.body_text().contains("<D:getetag/>"));
    }

    #[test]
    fn conditional_writes_carry_their_preconditions() {
        let server = Responder::start(vec![
            Reply::status(201).header("ETag", "\"v1\""),
            Reply::status(412),
            Reply::status(204),
            Reply::status(201),
        ]);
        let client = client();
        let url = format!("{}/cal/a.ics", server.url());
        let created = client
            .put(
                &url,
                b"x".to_vec(),
                Some("text/calendar"),
                Precondition::IfNoneMatchAny,
            )
            .unwrap();
        assert_eq!(created.etag().as_deref(), Some("\"v1\""));
        let conflict = client
            .put(&url, b"y".to_vec(), None, Precondition::IfMatch("\"v0\""))
            .unwrap();
        assert_eq!(conflict.status(), StatusCode::PRECONDITION_FAILED);
        client.delete(&url, Precondition::IfMatch("*")).unwrap();
        let destination = Url::parse(&format!("{}/cal/b.ics", server.url())).unwrap();
        client
            .move_to(&url, &destination, false, Precondition::None)
            .unwrap();

        let create = server.next_request();
        assert_eq!(create.header("if-none-match"), Some("*"));
        assert_eq!(create.header("if-match"), None);
        assert_eq!(create.header("content-type"), Some("text/calendar"));
        let update = server.next_request();
        assert_eq!(update.header("if-match"), Some("\"v0\""));
        assert_eq!(update.header("content-type"), None);
        let delete = server.next_request();
        assert_eq!(delete.method, "DELETE");
        assert_eq!(delete.header("if-match"), Some("*"));
        let moved = server.next_request();
        assert_eq!(moved.method, "MOVE");
        assert_eq!(moved.header("destination"), Some(destination.as_str()));
        assert_eq!(moved.header("overwrite"), Some("F"));
    }

    #[test]
    fn bodies_are_bounded_with_and_without_content_length() {
        let server = Responder::start(vec![
            Reply::status(200).body(vec![b'x'; 64]),
            Reply::status(200)
                .body(vec![b'x'; 64])
                .without_content_length(),
            Reply::status(200)
                .body(vec![b'x'; 64])
                .without_content_length(),
        ]);
        let client = client();
        for _ in 0..2 {
            let response = client.get(server.url()).unwrap();
            assert!(matches!(response.read_bounded(63), Err(Error::TooLarge)));
        }
        let exact = client.get(server.url()).unwrap().read_bounded(64).unwrap();
        assert_eq!(exact.len(), 64);
    }

    #[test]
    fn hostile_multistatus_bodies_are_refused() {
        let doctype =
            r#"<?xml version="1.0"?><!DOCTYPE m [<!ENTITY a "aaaa">]><multistatus xmlns="DAV:"/>"#;
        let server = Responder::start(vec![
            Reply::status(207).body(doctype),
            Reply::status(207).body("<html>sign in</html>"),
        ]);
        let client = client();
        let refused = client.propfind(server.url(), Depth::Zero, &[]).unwrap();
        assert!(matches!(
            refused.multistatus(&Limits::default()),
            Err(Error::Xml(XmlError::Doctype))
        ));
        let html = client.propfind(server.url(), Depth::Zero, &[]).unwrap();
        assert!(matches!(
            html.multistatus(&Limits::default()),
            Err(Error::Xml(XmlError::NotMultistatus))
        ));
    }

    #[test]
    fn refused_redirects_reach_the_caller_and_never_the_target() {
        let target = Responder::silent();
        let location = format!("{}/stolen", target.url());
        let server = Responder::start(vec![Reply::status(302).header("Location", &location)]);
        let client = DavClient::new(&Options {
            redirects: Redirects::Refuse,
            ..Options::default()
        })
        .unwrap()
        .with_credentials(Credentials::new("ada", "s3cret"));
        let response = client.propfind(server.url(), Depth::Zero, &[]).unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        assert!(server.next_request().header("authorization").is_some());
        target.assert_untouched();
    }

    #[test]
    fn transport_failures_are_errors_not_statuses() {
        let closed = Responder::silent();
        let url = closed.url();
        drop(closed);
        assert!(matches!(client().get(url), Err(Error::Transport(_))));
    }
}

//! Blocking `WebDAV` client on Tilo's TLS defaults.
//!
//! The client builds requests (method, headers, credentials, conditional `ETag`s) and
//! reads bounded bodies. It deliberately does not interpret status codes: a 412, a 404
//! on delete or a 403 from an expired sync token mean different things to different
//! callers, so [`DavResponse`] hands the status back and callers map it.

use std::fmt;
use std::io::Read;
use std::sync::Arc;

use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH, RANGE};
use reqwest::{IntoUrl, Method};

pub use reqwest::{StatusCode, Url};

use crate::body::{PropName, propfind_body};
use crate::multistatus::{self, Limits, Multistatus, XmlError};
use crate::request::{Asked, XML_CONTENT_TYPE};
pub use crate::request::{Credentials, Depth, Options, Precondition, Redirects};

pub enum Body {
    Empty,
    Bytes(Vec<u8>),
    /// A streamed body of exactly `len` bytes, sent with that
    /// `Content-Length` and never buffered whole.
    Reader(Box<dyn Read + Send>, u64),
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::Bytes(bytes) => write!(f, "Bytes({})", bytes.len()),
            Self::Reader(_, len) => write!(f, "Reader({len})"),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Build(reqwest::Error),
    Transport(reqwest::Error),
    Read(std::io::Error),
    TooLarge,
    Xml(XmlError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // A name the client's own look-up refused: its words say why
            // nothing was sent.
            Self::Build(error) | Self::Transport(error) => {
                match tilo_net::http::refused_by_lookup(error) {
                    Some(why) => f.write_str(&why),
                    None => fmt::Display::fmt(error, f),
                }
            }
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
    redirects: Redirects,
    /// The look-up every connection asks, where one was given
    /// ([`Options::lookup`]).
    asked: Option<Asked>,
}

impl DavClient {
    /// # Errors
    /// If the TLS/HTTP stack cannot be initialised.
    pub fn new(options: &Options) -> Result<Self, Error> {
        let builder = match &options.lookup {
            Some(lookup) => tilo_net::http::blocking_client_looking(Arc::clone(lookup)),
            None => tilo_net::http::blocking_client(),
        };
        let mut builder = builder.timeout(options.timeout);
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
            redirects: options.redirects,
            asked: options.lookup.clone().map(Asked),
        })
    }

    #[must_use]
    pub fn with_credentials(&self, credentials: Credentials) -> Self {
        Self {
            http: self.http.clone(),
            credentials: Some(Arc::new(credentials)),
            redirects: self.redirects,
            asked: self.asked.clone(),
        }
    }

    pub(crate) fn refuses_redirects(&self) -> bool {
        self.redirects == Redirects::Refuse
    }

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
        self.sent(request)
    }

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
        self.sent(request)
    }

    pub fn get(&self, url: impl IntoUrl) -> Result<DavResponse, Error> {
        self.sent(self.request(Method::GET, url))
    }

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
        self.sent(apply(request, precondition))
    }

    pub fn delete(
        &self,
        url: impl IntoUrl,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, Error> {
        self.sent(apply(self.request(Method::DELETE, url), precondition))
    }

    pub fn mkcol(&self, url: impl IntoUrl) -> Result<DavResponse, Error> {
        self.sent(self.request(method(b"MKCOL"), url))
    }

    /// `MOVE` to `destination`; `overwrite: false` sends `Overwrite: F`.
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
        self.sent(apply(request, precondition))
    }

    /// `GET` of `range` (a half-open byte range; the whole resource when
    /// `None`) under `precondition`. A server that ignores ranges answers
    /// 200 with the whole body; check the status and its `Content-Range`
    /// header.
    pub fn get_range(
        &self,
        url: impl IntoUrl,
        range: Option<std::ops::Range<u64>>,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, Error> {
        let mut request = self.request(Method::GET, url);
        if let Some(range) = range.filter(|range| range.end > range.start) {
            request = request.header(
                RANGE,
                format!("bytes={}-{}", range.start, range.end.saturating_sub(1)),
            );
        }
        self.sent(apply(request, precondition))
    }

    /// Any method with extra headers and a body. For protocol extensions
    /// (chunked upload sessions, `COPY`) the typed methods do not cover.
    pub fn send(
        &self,
        method_name: &str,
        url: impl IntoUrl,
        headers: &[(&str, &str)],
        body: Body,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, Error> {
        let method = Method::from_bytes(method_name.as_bytes())
            .map_err(|_| Error::Read(std::io::Error::other("invalid HTTP method")))?;
        let mut request = self.request(method, url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request = match body {
            Body::Empty => request,
            Body::Bytes(bytes) => request.body(bytes),
            Body::Reader(reader, len) => request
                .header(CONTENT_LENGTH, len)
                .body(reqwest::blocking::Body::sized(reader, len)),
        };
        self.sent(apply(request, precondition))
    }

    fn request(&self, method: Method, url: impl IntoUrl) -> RequestBuilder {
        let request = self.http.request(method, url);
        match &self.credentials {
            Some(credentials) if credentials.is_bearer() => {
                request.bearer_auth(credentials.password())
            }
            Some(credentials) => {
                request.basic_auth(credentials.username(), Some(credentials.password()))
            }
            None => request,
        }
    }

    /// Send `request`. Where this client was given a look-up, the address
    /// the request's URL writes out is asked of it first: a connection asks
    /// it for a name, and looks no written-out address up.
    fn sent(&self, request: RequestBuilder) -> Result<DavResponse, Error> {
        let Some(asked) = &self.asked else {
            return send(request);
        };
        let (http, request) = request.build_split();
        let request = request.map_err(Error::Transport)?;
        asked.of(request.url()).map_err(Error::Read)?;
        http.execute(request)
            .map(|inner| DavResponse { inner })
            .map_err(Error::Transport)
    }
}

#[derive(Debug)]
pub struct DavResponse {
    inner: Response,
}

impl DavResponse {
    pub fn status(&self) -> StatusCode {
        self.inner.status()
    }

    pub fn etag(&self) -> Option<String> {
        self.inner
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned)
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.inner
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned)
    }

    pub fn content_length(&self) -> Option<u64> {
        self.inner.content_length()
    }

    /// The body as a stream. The caller bounds how much it reads.
    pub fn into_reader(self) -> Box<dyn Read + Send> {
        Box::new(self.inner)
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

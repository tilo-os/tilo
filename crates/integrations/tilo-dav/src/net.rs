//! Async `WebDAV` client on `tilo_net`, for Linux, macOS and the web.
//!
//! The same requests as the blocking [`client`](crate::client) and the same bounded
//! bodies, as futures an app runs on its app runtime. Statuses are not interpreted here
//! either; [`DavResponse`] hands them back.
//!
//! In a page the server must allow the page's origin (CORS), including the
//! `PROPFIND`/`REPORT` methods and the `Depth`, `If-Match` and `Authorization` headers;
//! most `CalDAV` servers do not, so a page reaches them only through the host's fetch
//! proxy when the caller opts in with [`DavClient::with_fetch_proxy`].

use std::fmt;
use std::sync::Arc;

use tilo_net::header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH};
use tilo_net::proxy::ProxyUse;
use tilo_net::{Method, NetError, RequestBuilder};

pub use tilo_net::{StatusCode, Url};

use crate::body::{PropName, propfind_body};
use crate::multistatus::{self, Limits, Multistatus, XmlError};
#[cfg(not(target_family = "wasm"))]
use crate::request::Asked;
use crate::request::XML_CONTENT_TYPE;
pub use crate::request::{Credentials, Depth, Options, Precondition, Redirects};

/// A request's answer on its way, boxed so the async code awaiting it
/// across crates polls it here rather than compiling it again.
pub type DavFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug)]
pub enum Error {
    /// The request could not be built or sent, the response headers did not
    /// arrive, or the body failed while streaming.
    Transport(NetError),
    TooLarge,
    Xml(XmlError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => fmt::Display::fmt(error, f),
            Self::TooLarge => f.write_str("DAV response exceeds the size limit"),
            Self::Xml(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for Error {}

impl From<NetError> for Error {
    fn from(error: NetError) -> Self {
        match error {
            NetError::TooLarge { .. } => Self::TooLarge,
            other => Self::Transport(other),
        }
    }
}

/// An async DAV client. Cloning is cheap and shares the connection pool.
#[derive(Clone, Debug)]
pub struct DavClient {
    http: tilo_net::Client,
    credentials: Option<Arc<Credentials>>,
    /// The look-up every connection asks, where one was given
    /// ([`Options::lookup`]).
    #[cfg(not(target_family = "wasm"))]
    asked: Option<Asked>,
}

impl DavClient {
    pub fn new(options: &Options) -> Result<Self, Error> {
        Self::with_fetch_proxy(options, ProxyUse::Never)
    }

    /// A client that, in a page, may reach servers through the host's fetch
    /// proxy (see [`tilo_net::proxy`]).
    pub fn with_fetch_proxy(options: &Options, proxy: ProxyUse) -> Result<Self, Error> {
        let mut builder = tilo_net::Client::builder()
            .timeout(options.timeout)
            .redirects(match options.redirects {
                Redirects::Follow => tilo_net::Redirects::Follow(10),
                Redirects::Refuse => tilo_net::Redirects::None,
            })
            .fetch_proxy(proxy);
        if let Some(timeout) = options.connect_timeout {
            builder = builder.connect_timeout(timeout);
        }
        if let Some(agent) = options.user_agent {
            builder = builder.user_agent(agent);
        }
        #[cfg(not(target_family = "wasm"))]
        if let Some(lookup) = &options.lookup {
            // No proxy of the environment either: it would look names up
            // behind the look-up this client was given.
            builder = builder.lookup(Arc::clone(lookup)).direct();
        }
        Ok(Self {
            http: builder.build()?,
            credentials: None,
            #[cfg(not(target_family = "wasm"))]
            asked: options.lookup.clone().map(Asked),
        })
    }

    #[must_use]
    pub fn with_credentials(&self, credentials: Credentials) -> Self {
        Self {
            http: self.http.clone(),
            credentials: Some(Arc::new(credentials)),
            #[cfg(not(target_family = "wasm"))]
            asked: self.asked.clone(),
        }
    }

    pub async fn propfind(
        &self,
        url: &str,
        depth: Depth,
        props: &[PropName],
    ) -> Result<DavResponse, Error> {
        let request = self
            .request(method(b"PROPFIND"), url)?
            .header("Depth", depth.header())
            .header(CONTENT_TYPE, XML_CONTENT_TYPE)
            .body(propfind_body(props));
        send(request).await
    }

    pub async fn report(
        &self,
        url: &str,
        depth: Depth,
        body: String,
    ) -> Result<DavResponse, Error> {
        let request = self
            .request(method(b"REPORT"), url)?
            .header("Depth", depth.header())
            .header(CONTENT_TYPE, XML_CONTENT_TYPE)
            .body(body);
        send(request).await
    }

    pub async fn get(&self, url: &str) -> Result<DavResponse, Error> {
        send(self.request(Method::GET, url)?).await
    }

    /// `PUT` `body` under `precondition`, declaring `content_type` if given.
    pub async fn put(
        &self,
        url: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, Error> {
        let mut request = self.request(Method::PUT, url)?.body(body);
        if let Some(content_type) = content_type {
            request = request.header(CONTENT_TYPE, content_type);
        }
        send(apply(request, precondition)).await
    }

    pub async fn delete(
        &self,
        url: &str,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, Error> {
        self.delete_saying(url, &[], precondition).await
    }

    /// `DELETE` under `precondition`, saying `headers` beside it: what a
    /// server is told about the removal, such as `Schedule-Reply: F` to a
    /// calendar server that would answer an organiser for it (RFC 6638, 8.1).
    pub async fn delete_saying(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, Error> {
        let mut request = self.request(Method::DELETE, url)?;
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        send(apply(request, precondition)).await
    }

    pub async fn mkcol(&self, url: &str) -> Result<DavResponse, Error> {
        send(self.request(method(b"MKCOL"), url)?).await
    }

    /// `MOVE` to `destination`; `overwrite: false` sends `Overwrite: F`.
    pub async fn move_to(
        &self,
        url: &str,
        destination: &Url,
        overwrite: bool,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, Error> {
        let request = self
            .request(method(b"MOVE"), url)?
            .header("Destination", destination.as_str())
            .header("Overwrite", if overwrite { "T" } else { "F" });
        send(apply(request, precondition)).await
    }

    /// A request to `url`, signed in. Where this client was given a look-up,
    /// the address the URL writes out is asked of it first: a connection
    /// asks it for a name, and looks no written-out address up.
    fn request(&self, method: Method, url: &str) -> Result<RequestBuilder, Error> {
        #[cfg(not(target_family = "wasm"))]
        if let Some(asked) = &self.asked
            && let Ok(url) = Url::parse(url)
        {
            asked
                .of(&url)
                .map_err(|refused| Error::Transport(NetError::Blocked(refused.to_string())))?;
        }
        let request = self.http.request(method, url);
        Ok(match &self.credentials {
            Some(credentials) if credentials.is_bearer() => request.header(
                "Authorization",
                format!("Bearer {}", credentials.password()).as_str(),
            ),
            Some(credentials) => {
                request.basic_auth(credentials.username(), Some(credentials.password()))
            }
            None => request,
        })
    }
}

#[derive(Debug)]
pub struct DavResponse {
    inner: tilo_net::Response,
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

    /// Read at most `limit` bytes. A server that declares or streams more is refused
    /// without buffering past the limit, whether or not it sent a `Content-Length`.
    ///
    /// # Errors
    /// [`Error::TooLarge`] past the limit, [`Error::Transport`] on failure.
    pub fn read_bounded(self, limit: usize) -> DavFuture<'static, Result<Vec<u8>, Error>> {
        Box::pin(async move { Ok(self.inner.bytes_limited(limit).await?.to_vec()) })
    }

    /// Read and parse a multistatus body under `limits`.
    ///
    /// # Errors
    /// As [`Self::read_bounded`] with `limits.max_bytes`, then [`Error::Xml`].
    pub fn multistatus(self, limits: &Limits) -> DavFuture<'_, Result<Multistatus, Error>> {
        Box::pin(async move {
            let bytes = self.read_bounded(limits.max_bytes).await?;
            multistatus::parse_bytes(&bytes, limits).map_err(Error::Xml)
        })
    }
}

fn apply(request: RequestBuilder, precondition: Precondition<'_>) -> RequestBuilder {
    match precondition {
        Precondition::None => request,
        Precondition::IfMatch(etag) => request.header(IF_MATCH, etag),
        Precondition::IfNoneMatchAny => request.header(IF_NONE_MATCH, "*"),
    }
}

async fn send(request: RequestBuilder) -> Result<DavResponse, Error> {
    Ok(DavResponse {
        inner: request.send().await?,
    })
}

/// Extension methods are fixed ASCII tokens; `from_bytes` only fails on
/// invalid token characters.
fn method(name: &'static [u8]) -> Method {
    Method::from_bytes(name).unwrap_or(Method::GET)
}

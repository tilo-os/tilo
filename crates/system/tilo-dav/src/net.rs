//! Async `WebDAV` client on `tilo_net`, for Linux, macOS and the web.
//!
//! The same requests as the blocking [`client`](crate::client) — method,
//! headers, credentials, conditional `ETag`s — and the same bounded bodies,
//! as futures an app runs on its app runtime. Statuses are not interpreted
//! here either; [`DavResponse`] hands them back.
//!
//! In a page the server must allow the page's origin (CORS), including the
//! `PROPFIND`/`REPORT` methods and the `Depth`, `If-Match` and
//! `Authorization` headers; most `CalDAV` servers do not, so a page reaches
//! them only through the host's fetch proxy when the caller opts in with
//! [`DavClient::with_fetch_proxy`].

use std::fmt;
use std::sync::Arc;

use tilo_net::header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH};
use tilo_net::proxy::ProxyUse;
use tilo_net::{Method, NetError, RequestBuilder};

pub use tilo_net::{StatusCode, Url};

use crate::body::{PropName, propfind_body};
use crate::multistatus::{self, Limits, Multistatus, XmlError};
use crate::request::XML_CONTENT_TYPE;
pub use crate::request::{Credentials, Depth, Options, Precondition, Redirects};

/// Transport and bounded-read failures. HTTP statuses are not errors.
#[derive(Debug)]
pub enum Error {
    /// The request could not be built or sent, the response headers did not
    /// arrive, or the body failed while streaming.
    Transport(NetError),
    /// The body is larger than the caller's limit.
    TooLarge,
    /// The body is not an acceptable multistatus document.
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
}

impl DavClient {
    /// A client for `options`, fetching directly.
    ///
    /// # Errors
    /// If the TLS/HTTP stack cannot be initialised.
    pub fn new(options: &Options) -> Result<Self, Error> {
        Self::with_fetch_proxy(options, ProxyUse::Never)
    }

    /// A client that, in a page, may reach servers through the host's fetch
    /// proxy (see [`tilo_net::proxy`]).
    ///
    /// # Errors
    /// If the TLS/HTTP stack cannot be initialised.
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
        Ok(Self {
            http: builder.build()?,
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
    pub async fn propfind(
        &self,
        url: &str,
        depth: Depth,
        props: &[PropName],
    ) -> Result<DavResponse, Error> {
        let request = self
            .request(method(b"PROPFIND"), url)
            .header("Depth", depth.header())
            .header(CONTENT_TYPE, XML_CONTENT_TYPE)
            .body(propfind_body(props));
        send(request).await
    }

    /// `REPORT` with an XML `body` (see [`crate::body`]).
    ///
    /// # Errors
    /// On transport failure.
    pub async fn report(
        &self,
        url: &str,
        depth: Depth,
        body: String,
    ) -> Result<DavResponse, Error> {
        let request = self
            .request(method(b"REPORT"), url)
            .header("Depth", depth.header())
            .header(CONTENT_TYPE, XML_CONTENT_TYPE)
            .body(body);
        send(request).await
    }

    /// `GET`.
    ///
    /// # Errors
    /// On transport failure.
    pub async fn get(&self, url: &str) -> Result<DavResponse, Error> {
        send(self.request(Method::GET, url)).await
    }

    /// `PUT` `body` under `precondition`, declaring `content_type` if given.
    ///
    /// # Errors
    /// On transport failure.
    pub async fn put(
        &self,
        url: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, Error> {
        let mut request = self.request(Method::PUT, url).body(body);
        if let Some(content_type) = content_type {
            request = request.header(CONTENT_TYPE, content_type);
        }
        send(apply(request, precondition)).await
    }

    /// `DELETE` under `precondition`.
    ///
    /// # Errors
    /// On transport failure.
    pub async fn delete(
        &self,
        url: &str,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, Error> {
        send(apply(self.request(Method::DELETE, url), precondition)).await
    }

    /// `MKCOL`.
    ///
    /// # Errors
    /// On transport failure.
    pub async fn mkcol(&self, url: &str) -> Result<DavResponse, Error> {
        send(self.request(method(b"MKCOL"), url)).await
    }

    /// `MOVE` to `destination`; `overwrite: false` sends `Overwrite: F`.
    ///
    /// # Errors
    /// On transport failure.
    pub async fn move_to(
        &self,
        url: &str,
        destination: &Url,
        overwrite: bool,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, Error> {
        let request = self
            .request(method(b"MOVE"), url)
            .header("Destination", destination.as_str())
            .header("Overwrite", if overwrite { "T" } else { "F" });
        send(apply(request, precondition)).await
    }

    fn request(&self, method: Method, url: &str) -> RequestBuilder {
        let request = self.http.request(method, url);
        match &self.credentials {
            Some(credentials) if credentials.is_bearer() => request.header(
                "Authorization",
                format!("Bearer {}", credentials.password()).as_str(),
            ),
            Some(credentials) => {
                request.basic_auth(credentials.username(), Some(credentials.password()))
            }
            None => request,
        }
    }
}

/// A response whose body has not been read yet.
#[derive(Debug)]
pub struct DavResponse {
    inner: tilo_net::Response,
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
    /// [`Error::TooLarge`] past the limit, [`Error::Transport`] on failure.
    pub async fn read_bounded(self, limit: usize) -> Result<Vec<u8>, Error> {
        Ok(self.inner.bytes_limited(limit).await?.to_vec())
    }

    /// Read and parse a multistatus body under `limits`.
    ///
    /// # Errors
    /// As [`Self::read_bounded`] with `limits.max_bytes`, then [`Error::Xml`].
    pub async fn multistatus(self, limits: &Limits) -> Result<Multistatus, Error> {
        let bytes = self.read_bounded(limits.max_bytes).await?;
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

    fn run<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    const LISTING: &str = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/a.txt</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;

    #[test]
    fn propfind_sends_depth_props_and_basic_auth() {
        let server = Responder::start(vec![Reply::status(207).body(LISTING)]);
        let listing = run(async {
            let response = client()
                .propfind(
                    &format!("{}/dav/", server.url()),
                    Depth::One,
                    &[props::GETETAG],
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::MULTI_STATUS);
            response.multistatus(&Limits::default()).await.unwrap()
        });
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
        ]);
        let url = format!("{}/cal/a.ics", server.url());
        run(async {
            let client = client();
            let created = client
                .put(
                    &url,
                    b"x".to_vec(),
                    Some("text/calendar"),
                    Precondition::IfNoneMatchAny,
                )
                .await
                .unwrap();
            assert_eq!(created.etag().as_deref(), Some("\"v1\""));
            let conflict = client
                .put(&url, b"y".to_vec(), None, Precondition::IfMatch("\"v0\""))
                .await
                .unwrap();
            assert_eq!(conflict.status(), StatusCode::PRECONDITION_FAILED);
        });
        let create = server.next_request();
        assert_eq!(create.header("if-none-match"), Some("*"));
        assert_eq!(create.header("content-type"), Some("text/calendar"));
        assert_eq!(server.next_request().header("if-match"), Some("\"v0\""));
    }

    #[test]
    fn bodies_are_bounded_with_and_without_content_length() {
        let server = Responder::start(vec![
            Reply::status(200).body(vec![b'x'; 64]),
            Reply::status(200)
                .body(vec![b'x'; 64])
                .without_content_length(),
        ]);
        run(async {
            let client = client();
            for _ in 0..2 {
                let response = client.get(&server.url()).await.unwrap();
                assert!(matches!(
                    response.read_bounded(63).await,
                    Err(Error::TooLarge)
                ));
            }
        });
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
        let status = run(async {
            client
                .propfind(&server.url(), Depth::Zero, &[])
                .await
                .unwrap()
                .status()
        });
        assert_eq!(status, StatusCode::FOUND);
        assert!(server.next_request().header("authorization").is_some());
        target.assert_untouched();
    }
}

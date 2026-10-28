//! HTTP requests with streaming bodies, the same on every target.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use ::http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, USER_AGENT};
use ::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use bytes::{Bytes, BytesMut};
use url::Url;

use crate::NetError;
use crate::proxy::ProxyUse;

#[cfg(not(target_family = "wasm"))]
use crate::native::http as backend;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use crate::web::fetch as backend;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redirects {
    /// Follow up to this many hops. A page follows at most 20, whatever the
    /// limit.
    Follow(u8),
    /// Answer the redirect itself. A page cannot read redirect responses, so
    /// there the request fails with [`NetError::Redirect`].
    None,
}

impl Default for Redirects {
    fn default() -> Self {
        Self::Follow(10)
    }
}

/// Settings shared by every request of a [`Client`].
#[derive(Clone, Debug, Default)]
pub(crate) struct Config {
    pub(crate) timeout: Option<Duration>,
    pub(crate) connect_timeout: Option<Duration>,
    pub(crate) default_headers: HeaderMap,
    pub(crate) redirects: Redirects,
    pub(crate) proxy: ProxyUse,
}

/// An HTTP client: a connection pool on the desktop, the page's `fetch` on
/// the web. Cheap to clone; clones share the pool.
///
/// Every request future is `Send`, so it can run on `tilo_rt::app()` from any
/// target. On the desktop it needs a tokio runtime with I/O and timers, which
/// the app runtime is.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    config: Config,
    backend: backend::Backend,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    /// A client with the defaults: no overall timeout, 10 redirects, no
    /// fetch proxy.
    ///
    /// # Panics
    ///
    /// Never in practice: the default configuration always builds. Use
    /// [`Client::builder`] to handle a failure.
    #[must_use]
    pub fn new() -> Self {
        #[allow(
            clippy::expect_used,
            reason = "the default configuration has no invalid values"
        )]
        Self::builder()
            .build()
            .expect("the default HTTP client builds")
    }

    /// Configure a client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Start a request.
    pub fn request(&self, method: Method, url: impl AsRef<str>) -> RequestBuilder {
        let request = Url::parse(url.as_ref())
            .map_err(|error| NetError::InvalidUrl(format!("{}: {error}", url.as_ref())))
            .and_then(|url| Request::checked(method, url));
        RequestBuilder {
            client: self.clone(),
            request,
        }
    }

    /// Start a `GET`.
    pub fn get(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::GET, url)
    }

    /// Start a `POST`.
    pub fn post(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::POST, url)
    }

    /// Start a `PUT`.
    pub fn put(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::PUT, url)
    }

    /// Start a `DELETE`.
    pub fn delete(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::DELETE, url)
    }

    /// Start a `HEAD`.
    pub fn head(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::HEAD, url)
    }

    /// Send `request` and wait for the response head. The body streams
    /// afterwards through the [`Response`].
    ///
    /// A status of 4xx or 5xx is a response, not an error; see
    /// [`Response::error_for_status`].
    ///
    /// # Errors
    ///
    /// [`NetError::Connect`] when the server was not reached,
    /// [`NetError::Timeout`], [`NetError::Redirect`], and
    /// [`NetError::Blocked`] when the host's fetch proxy refused the
    /// destination.
    pub fn send(
        &self,
        mut request: Request,
    ) -> impl Future<Output = Result<Response, NetError>> + Send + use<> {
        for (name, value) in &self.inner.config.default_headers {
            if !request.headers.contains_key(name) {
                request.headers.insert(name.clone(), value.clone());
            }
        }
        if request.proxy.is_none() {
            request.proxy = Some(self.inner.config.proxy);
        }
        if request.timeout.is_none() {
            request.timeout = self.inner.config.timeout;
        }
        let inner = Arc::clone(&self.inner);
        async move {
            let raw = inner.backend.send(&inner.config, request).await?;
            Ok(Response {
                status: raw.status,
                headers: raw.headers,
                url: raw.url,
                body: raw.body,
            })
        }
    }
}

/// Settings for a [`Client`].
#[derive(Clone, Debug, Default)]
#[must_use]
pub struct ClientBuilder {
    config: Config,
    error: Option<NetError>,
}

impl ClientBuilder {
    /// The longest a request may take, from sending it to the end of its
    /// body. A request's own [`RequestBuilder::timeout`] replaces it.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.config.timeout = Some(timeout);
        self
    }

    /// The longest establishing a connection may take. A page has no
    /// separate connection phase and applies only the overall timeout.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.config.connect_timeout = Some(timeout);
        self
    }

    /// The `User-Agent` header. A page sends the browser's own instead,
    /// except through the host's fetch proxy.
    pub fn user_agent(self, agent: impl AsRef<str>) -> Self {
        self.default_header(USER_AGENT, agent.as_ref())
    }

    /// A header sent with every request that does not set it itself.
    pub fn default_header<K, V>(mut self, name: K, value: V) -> Self
    where
        HeaderName: TryFrom<K>,
        <HeaderName as TryFrom<K>>::Error: fmt::Display,
        HeaderValue: TryFrom<V>,
        <HeaderValue as TryFrom<V>>::Error: fmt::Display,
    {
        match header(name, value) {
            Ok((name, value)) => {
                self.config.default_headers.insert(name, value);
            }
            Err(error) => self.error = self.error.or(Some(error)),
        }
        self
    }

    /// Whether to follow redirects.
    pub fn redirects(mut self, redirects: Redirects) -> Self {
        self.config.redirects = redirects;
        self
    }

    /// Whether requests may go through the host's fetch [`proxy`](crate::proxy).
    /// Off unless the app opts in; a request can override it.
    pub fn fetch_proxy(mut self, proxy: ProxyUse) -> Self {
        self.config.proxy = proxy;
        self
    }

    /// Build the client.
    ///
    /// # Errors
    ///
    /// [`NetError::InvalidRequest`] for a default header that is not valid,
    /// or when the desktop TLS stack cannot be set up.
    pub fn build(self) -> Result<Client, NetError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let backend = backend::Backend::new(&self.config)?;
        Ok(Client {
            inner: Arc::new(Inner {
                config: self.config,
                backend,
            }),
        })
    }
}

fn header<K, V>(name: K, value: V) -> Result<(HeaderName, HeaderValue), NetError>
where
    HeaderName: TryFrom<K>,
    <HeaderName as TryFrom<K>>::Error: fmt::Display,
    HeaderValue: TryFrom<V>,
    <HeaderValue as TryFrom<V>>::Error: fmt::Display,
{
    let name = HeaderName::try_from(name)
        .map_err(|error| NetError::InvalidRequest(format!("header name: {error}")))?;
    let value = HeaderValue::try_from(value)
        .map_err(|error| NetError::InvalidRequest(format!("header {name}: {error}")))?;
    Ok((name, value))
}

/// One HTTP request.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Request {
    pub method: Method,
    /// An `http` or `https` URL.
    pub url: Url,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
    /// Replaces the client's timeout for this request.
    pub timeout: Option<Duration>,
    /// Replaces the client's fetch-proxy choice for this request.
    pub proxy: Option<ProxyUse>,
}

impl Request {
    /// A request with no headers or body.
    ///
    /// # Errors
    ///
    /// [`NetError::InvalidUrl`] unless the scheme is `http` or `https`.
    pub fn new(method: Method, url: Url) -> Result<Self, NetError> {
        Self::checked(method, url)
    }

    fn checked(method: Method, url: Url) -> Result<Self, NetError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(NetError::InvalidUrl(format!(
                "{url}: only http and https URLs can be fetched"
            )));
        }
        Ok(Self {
            method,
            url,
            headers: HeaderMap::new(),
            body: None,
            timeout: None,
            proxy: None,
        })
    }
}

/// A request being assembled; [`send`](Self::send) reports any error made
/// while building it.
#[must_use = "a request does nothing until it is sent"]
pub struct RequestBuilder {
    client: Client,
    request: Result<Request, NetError>,
}

impl fmt::Debug for RequestBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestBuilder")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

impl RequestBuilder {
    fn map(mut self, change: impl FnOnce(&mut Request) -> Result<(), NetError>) -> Self {
        if let Ok(request) = &mut self.request
            && let Err(error) = change(request)
        {
            self.request = Err(error);
        }
        self
    }

    /// Add a header; repeated names are all sent.
    pub fn header<K, V>(self, name: K, value: V) -> Self
    where
        HeaderName: TryFrom<K>,
        <HeaderName as TryFrom<K>>::Error: fmt::Display,
        HeaderValue: TryFrom<V>,
        <HeaderValue as TryFrom<V>>::Error: fmt::Display,
    {
        let parsed = header(name, value);
        self.map(|request| {
            let (name, value) = parsed?;
            request.headers.append(name, value);
            Ok(())
        })
    }

    pub fn headers(self, headers: HeaderMap) -> Self {
        self.map(|request| {
            let mut current = None;
            for (name, value) in headers {
                if name.is_some() {
                    current = name;
                }
                if let Some(name) = &current {
                    request.headers.append(name.clone(), value);
                }
            }
            Ok(())
        })
    }

    /// `Authorization: Bearer <token>`.
    pub fn bearer_auth(self, token: impl fmt::Display) -> Self {
        self.sensitive_header(AUTHORIZATION, format!("Bearer {token}"))
    }

    /// `Authorization: Basic …` for `user` and `password`.
    pub fn basic_auth(self, user: impl fmt::Display, password: Option<impl fmt::Display>) -> Self {
        use base64::Engine as _;
        let credentials = match password {
            Some(password) => format!("{user}:{password}"),
            None => format!("{user}:"),
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
        self.sensitive_header(AUTHORIZATION, format!("Basic {encoded}"))
    }

    fn sensitive_header(self, name: HeaderName, value: String) -> Self {
        self.map(|request| {
            let mut value = HeaderValue::try_from(value)
                .map_err(|error| NetError::InvalidRequest(format!("header {name}: {error}")))?;
            value.set_sensitive(true);
            request.headers.insert(name, value);
            Ok(())
        })
    }

    /// Append query parameters to the URL.
    pub fn query<K: AsRef<str>, V: AsRef<str>>(self, pairs: &[(K, V)]) -> Self {
        self.map(|request| {
            request
                .url
                .query_pairs_mut()
                .extend_pairs(pairs.iter().map(|(k, v)| (k.as_ref(), v.as_ref())));
            Ok(())
        })
    }

    pub fn body(self, body: impl Into<Bytes>) -> Self {
        let body = body.into();
        self.map(|request| {
            request.body = Some(body);
            Ok(())
        })
    }

    /// An `application/x-www-form-urlencoded` body.
    pub fn form<K: AsRef<str>, V: AsRef<str>>(self, pairs: &[(K, V)]) -> Self {
        let encoded = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs.iter().map(|(k, v)| (k.as_ref(), v.as_ref())))
            .finish();
        self.map(|request| {
            request.headers.insert(
                CONTENT_TYPE,
                HeaderValue::from_static("application/x-www-form-urlencoded"),
            );
            request.body = Some(Bytes::from(encoded));
            Ok(())
        })
    }

    /// A JSON body, with `Content-Type: application/json`.
    #[cfg(feature = "json")]
    pub fn json<T: serde::Serialize + ?Sized>(self, value: &T) -> Self {
        let encoded = serde_json::to_vec(value);
        self.map(|request| {
            let encoded = encoded.map_err(|error| NetError::InvalidRequest(error.to_string()))?;
            request
                .headers
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            request.body = Some(Bytes::from(encoded));
            Ok(())
        })
    }

    /// The longest this request may take, body included.
    pub fn timeout(self, timeout: Duration) -> Self {
        self.map(|request| {
            request.timeout = Some(timeout);
            Ok(())
        })
    }

    /// Whether this request may go through the host's fetch
    /// [`proxy`](crate::proxy).
    pub fn fetch_proxy(self, proxy: ProxyUse) -> Self {
        self.map(|request| {
            request.proxy = Some(proxy);
            Ok(())
        })
    }

    /// The assembled request, for [`Client::send`].
    pub fn build(self) -> Result<Request, NetError> {
        self.request
    }

    /// Send the request; see [`Client::send`].
    ///
    /// # Errors
    ///
    /// Any error made while building it, then those of [`Client::send`].
    pub fn send(self) -> impl Future<Output = Result<Response, NetError>> + Send + use<> {
        let sent = self.request.map(|request| self.client.send(request));
        async move { sent?.await }
    }
}

/// A response head; the body streams on demand.
pub struct Response {
    status: StatusCode,
    headers: HeaderMap,
    url: Url,
    body: backend::Body,
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("url", &self.url.as_str())
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

impl Response {
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The URL that answered, after redirects.
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The `Content-Length` the server declared.
    #[must_use]
    pub fn content_length(&self) -> Option<u64> {
        self.headers
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse().ok())
    }

    /// `Err(NetError::Status)` for a 4xx or 5xx status.
    ///
    /// # Errors
    ///
    /// [`NetError::Status`] with the status.
    pub fn error_for_status(self) -> Result<Self, NetError> {
        if self.status.is_client_error() || self.status.is_server_error() {
            Err(NetError::Status(self.status))
        } else {
            Ok(self)
        }
    }

    /// The next piece of the body; `None` at its end.
    ///
    /// # Errors
    ///
    /// [`NetError::Body`] when the connection failed mid-body,
    /// [`NetError::Timeout`] when the request's deadline passed.
    pub fn chunk(&mut self) -> impl Future<Output = Result<Option<Bytes>, NetError>> + Send + '_ {
        self.body.chunk()
    }

    /// The whole body.
    ///
    /// # Errors
    ///
    /// As [`chunk`](Self::chunk).
    pub async fn bytes(self) -> Result<Bytes, NetError> {
        self.bytes_limited(usize::MAX).await
    }

    /// The whole body, refusing one longer than `limit` bytes before reading
    /// past it.
    ///
    /// # Errors
    ///
    /// [`NetError::TooLarge`], and those of [`chunk`](Self::chunk).
    pub async fn bytes_limited(mut self, limit: usize) -> Result<Bytes, NetError> {
        if let Some(declared) = self.content_length()
            && usize::try_from(declared).map_or(true, |declared| declared > limit)
        {
            return Err(NetError::TooLarge { limit });
        }
        let mut body = BytesMut::new();
        while let Some(chunk) = self.chunk().await? {
            if body.len().saturating_add(chunk.len()) > limit {
                return Err(NetError::TooLarge { limit });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body.freeze())
    }

    /// The whole body as text, with invalid UTF-8 replaced.
    ///
    /// # Errors
    ///
    /// As [`chunk`](Self::chunk).
    pub async fn text(self) -> Result<String, NetError> {
        let bytes = self.bytes().await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The whole body, parsed as JSON.
    ///
    /// # Errors
    ///
    /// [`NetError::Protocol`] when it does not parse, and those of
    /// [`chunk`](Self::chunk).
    #[cfg(feature = "json")]
    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, NetError> {
        let bytes = self.bytes().await?;
        serde_json::from_slice(&bytes).map_err(|error| NetError::Protocol(error.to_string()))
    }
}

/// What a backend answers before the body.
pub(crate) struct RawResponse {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) url: Url,
    pub(crate) body: backend::Body,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_urls_are_requests() {
        let client = Client::new();
        assert!(matches!(
            client.get("ftp://example.com/").build(),
            Err(NetError::InvalidUrl(_))
        ));
        assert!(matches!(
            client.get("not a url").build(),
            Err(NetError::InvalidUrl(_))
        ));
        assert!(client.get("https://example.com/").build().is_ok());
    }

    #[test]
    fn a_bad_header_surfaces_at_build() {
        let client = Client::new();
        let built = client
            .get("https://example.com/")
            .header("bad header", "x")
            .build();
        assert!(matches!(built, Err(NetError::InvalidRequest(_))));
    }

    #[test]
    fn builders_assemble_the_request() {
        let client = Client::new();
        let request = client
            .post("https://example.com/search")
            .query(&[("q", "a b")])
            .basic_auth("user", Some("pw"))
            .form(&[("k", "v&w")])
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        assert_eq!(request.url.as_str(), "https://example.com/search?q=a+b");
        assert_eq!(request.headers[AUTHORIZATION], "Basic dXNlcjpwdw==");
        assert!(request.headers[AUTHORIZATION].is_sensitive());
        assert_eq!(request.body.as_deref(), Some(&b"k=v%26w"[..]));
        assert_eq!(request.timeout, Some(Duration::from_secs(3)));
    }
}

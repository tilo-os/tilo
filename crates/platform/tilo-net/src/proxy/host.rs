//! The host side of the fetch proxy (feature `proxy-host`): wire protocol v1
//! as a workspace host serves it to the pages of its members.
//!
//! [`FetchProxy::serve`] takes one request the host already authenticated and
//! answers the target's response or a [`ProxyFailure`].

use std::collections::HashMap;
use std::fmt::Write as _;
use std::future::Future;
use std::hash::Hash;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use ::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, header};
use bytes::Bytes;
use futures_util::Stream;
use http_body::Frame;
use http_body_util::BodyExt as _;
use tilo_lock::MutexRecover as _;
use tokio::time::Instant;
use url::Url;

use super::{
    ERROR_HEADER, FINAL_URL_HEADER, FORWARD_PREFIX, MARKER_HEADER, MARKER_PARAM, ProxyFailure,
    SESSION_PARAM, VERSION,
};
use crate::NetError;

#[derive(Clone, Debug)]
pub struct Config {
    pub max_request_bytes: usize,
    pub max_response_bytes: u64,
    pub deadline: Duration,
    pub max_redirects: usize,
    pub destinations: Destinations,
    pub limits: RateLimits,
    pub budgets: Budgets,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_request_bytes: 16 * 1024 * 1024,
            max_response_bytes: 64 * 1024 * 1024,
            deadline: Duration::from_secs(30),
            max_redirects: 10,
            destinations: Destinations::Public,
            limits: RateLimits::default(),
            budgets: Budgets::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destinations {
    /// Public addresses only: never the proxy's own network.
    Public,
    /// Any address, loopback and private ones included: a development proxy
    /// reaching a local test server.
    Any,
}

/// A token bucket: `burst` at once, refilled at `per_minute`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rate {
    pub burst: f64,
    pub per_minute: f64,
}

impl Rate {
    fn per_second(self) -> f64 {
        self.per_minute / 60.0
    }
}

/// What the proxy allows each device, each destination and everyone. The
/// defaults suit a public workspace on a busy day.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateLimits {
    pub device_requests: Rate,
    pub device_bytes: Rate,
    pub destination_requests: Rate,
    pub device_in_flight: usize,
    pub in_flight: usize,
}

impl Default for RateLimits {
    fn default() -> Self {
        Self {
            device_requests: Rate {
                burst: 240.0,
                per_minute: 240.0,
            },
            device_bytes: Rate {
                burst: 128.0 * 1024.0 * 1024.0,
                per_minute: 8.0 * 1024.0 * 1024.0,
            },
            destination_requests: Rate {
                burst: 1200.0,
                per_minute: 3000.0,
            },
            device_in_flight: 16,
            in_flight: 256,
        }
    }
}

/// Response bytes the proxy relays over [`Budgets::window`], per visitor and
/// per workspace; `None` is no budget. The defaults let a person watch a few
/// hours of video a day and a workspace's people a few days' worth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budgets {
    pub window: Duration,
    pub visitor_bytes: Option<u64>,
    /// Bytes everyone in one workspace together may receive per window.
    ///
    /// Spelled `workspace_bytes` after the [`Workspace`] identifier it counts against,
    /// which is published API.
    pub workspace_bytes: Option<u64>,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(24 * 60 * 60),
            visitor_bytes: Some(2 * 1024 * 1024 * 1024),
            workspace_bytes: Some(64 * 1024 * 1024 * 1024),
        }
    }
}

/// A workspace on the host, by the host's own identifier for it; its people
/// share one budget. Hosts outside this crate import the type under this
/// name, so the name stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Workspace(pub u128);

/// The device a request is made for: the key the host authenticated its
/// session with. A development host uses [`Caller::LOCAL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Caller(pub [u8; 32]);

impl Caller {
    pub const LOCAL: Self = Self([0; 32]);

    fn short(self) -> String {
        let [a, b, c, d, ..] = self.0;
        format!("{a:02x}{b:02x}{c:02x}{d:02x}")
    }
}

/// Resolves a name to its addresses. Hosts use the system's resolver; tests
/// inject names that resolve wherever they need.
pub type Lookup = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>
        + Send
        + Sync,
>;

#[must_use]
pub fn system_lookup() -> Lookup {
    Arc::new(|name: String| {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((name.as_str(), 0))
                .await?
                .map(|address| address.ip())
                .collect())
        })
    })
}

/// Ports a browser never fetches from (the Fetch standard's "bad ports"):
/// mail, shell, file sharing and other services that would take an HTTP
/// request as something else.
pub const BAD_PORTS: &[u16] = &[
    0, 1, 7, 9, 11, 13, 15, 17, 19, 20, 21, 22, 23, 25, 37, 42, 43, 53, 69, 77, 79, 87, 95, 101,
    102, 103, 104, 109, 110, 111, 113, 115, 117, 119, 123, 135, 137, 139, 143, 161, 179, 389, 427,
    465, 512, 513, 514, 515, 526, 530, 531, 532, 540, 548, 554, 556, 563, 587, 601, 636, 989, 990,
    993, 995, 1719, 1720, 1723, 2049, 3659, 4045, 4190, 5060, 5061, 6000, 6566, 6665, 6666, 6667,
    6668, 6669, 6679, 6697, 10080,
];

/// The methods the proxy relays: HTTP's, and the `WebDAV` ones calendars and
/// file servers speak.
const METHODS: &[&str] = &[
    "GET",
    "HEAD",
    "POST",
    "PUT",
    "PATCH",
    "DELETE",
    "OPTIONS",
    "PROPFIND",
    "PROPPATCH",
    "REPORT",
    "MKCOL",
    "MKCALENDAR",
    "COPY",
    "MOVE",
    "LOCK",
    "UNLOCK",
    "SEARCH",
];

/// Whether `address` is on the public internet, and the one judge of literal
/// addresses written into a URL: [`crate::address`].
pub use crate::address::{is_non_public_literal, is_public, written_out};

#[derive(Debug)]
struct BlockedAddress {
    name: String,
}

impl std::fmt::Display for BlockedAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} resolves to an address on the proxy's own network",
            self.name
        )
    }
}

impl std::error::Error for BlockedAddress {}

/// The resolver the proxy's connections use: one lookup, every address
/// checked, the checked addresses dialled.
struct CheckedResolver {
    lookup: Lookup,
    destinations: Destinations,
}

impl reqwest::dns::Resolve for CheckedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let lookup = Arc::clone(&self.lookup);
        let destinations = self.destinations;
        let name = name.as_str().to_owned();
        Box::pin(async move {
            let addresses = lookup(name.clone()).await?;
            if addresses.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("{name} does not resolve"),
                )
                .into());
            }
            if destinations == Destinations::Public
                && addresses.iter().any(|address| !is_public(*address))
            {
                return Err(Box::new(BlockedAddress { name }) as Box<_>);
            }
            let addresses: reqwest::dns::Addrs = Box::new(
                addresses
                    .into_iter()
                    .map(|address| SocketAddr::new(address, 0)),
            );
            Ok(addresses)
        })
    }
}

/// Whether `error` is a [`guarded_client`]'s refusal to dial a name, because
/// one of the addresses it resolved to is off the public internet: the
/// request was not sent. A caller that tells its person why a service was
/// not reached reads this where the proxy reads its own refusal.
#[must_use]
pub fn refused_address(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut source = Some(error);
    while let Some(cause) = source {
        if cause.is::<BlockedAddress>() {
            return true;
        }
        source = cause.source();
    }
    false
}

/// A client that reaches only what `destinations` lets it: it follows no
/// redirect, takes no proxy from the environment, and dials the addresses
/// `lookup` answered once every one of them was checked.
fn guarded(destinations: Destinations, lookup: Lookup) -> reqwest::ClientBuilder {
    crate::http::client()
        // Bodies arrive decompressed, as a page's `fetch` reads them; the
        // response limit then bounds what was decompressed.
        .gzip(true)
        .brotli(true)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .referer(false)
        .connect_timeout(Duration::from_secs(10))
        .dns_resolver(Arc::new(CheckedResolver {
            lookup,
            destinations,
        }))
}

/// A client for a server's own request to an address a person named (a model
/// provider's endpoint), made as the proxy makes its requests: no redirect
/// followed, no proxy taken from the environment, and under
/// [`Destinations::Public`] only public addresses dialled, checked at the
/// lookup the connection uses. A literal address is never looked up, so
/// ask [`check_destination`] about the URL first.
///
/// # Errors
/// When the client cannot be made.
pub fn guarded_client(
    destinations: Destinations,
    lookup: Lookup,
) -> Result<reqwest::Client, NetError> {
    guarded(destinations, lookup)
        .build()
        .map_err(|error| NetError::Protocol(error.to_string()))
}

/// Whether a [`guarded_client`] may be sent to `url`: an http or https
/// address, on a port the web uses, and under [`Destinations::Public`] no
/// literal address off the public internet.
///
/// # Errors
/// [`NetError::Blocked`] saying why not.
pub fn check_destination(url: &Url, destinations: Destinations) -> Result<(), NetError> {
    check_target(url, destinations).map_err(|refused| NetError::Blocked(refused.message))
}

#[derive(Debug)]
struct Refused {
    failure: ProxyFailure,
    message: String,
    retry_after: Option<Duration>,
}

impl Refused {
    fn new(failure: ProxyFailure, message: impl Into<String>) -> Self {
        Self {
            failure,
            message: message.into(),
            retry_after: None,
        }
    }

    fn limited(message: impl Into<String>, retry_after: Duration) -> Self {
        Self {
            failure: ProxyFailure::RateLimited,
            message: message.into(),
            retry_after: Some(retry_after),
        }
    }

    fn over_budget(message: impl Into<String>, retry_after: Duration) -> Self {
        Self {
            failure: ProxyFailure::OverBudget,
            message: message.into(),
            retry_after: Some(retry_after),
        }
    }
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

/// Token buckets by key, swept of full ones as the map grows.
struct Buckets<K> {
    rate: Rate,
    buckets: HashMap<K, Bucket>,
}

/// Buckets kept before the full ones are swept.
const SWEEP_AT: usize = 4096;

impl<K: Eq + Hash + Clone> Buckets<K> {
    fn new(rate: Rate) -> Self {
        Self {
            rate,
            buckets: HashMap::new(),
        }
    }

    fn bucket(&mut self, key: &K, now: Instant) -> &mut Bucket {
        if self.buckets.len() >= SWEEP_AT && !self.buckets.contains_key(key) {
            let rate = self.rate;
            self.buckets.retain(|_, bucket| {
                refill(rate, bucket, now);
                bucket.tokens < rate.burst
            });
        }
        let rate = self.rate;
        let bucket = self.buckets.entry(key.clone()).or_insert(Bucket {
            tokens: rate.burst,
            at: now,
        });
        refill(rate, bucket, now);
        bucket
    }

    fn take(&mut self, key: &K, amount: f64, now: Instant) -> Result<(), Duration> {
        let rate = self.rate;
        let bucket = self.bucket(key, now);
        if bucket.tokens >= amount {
            bucket.tokens -= amount;
            return Ok(());
        }
        Err(wait(rate, amount - bucket.tokens))
    }

    fn empty_for(&mut self, key: &K, now: Instant) -> Option<Duration> {
        let rate = self.rate;
        let bucket = self.bucket(key, now);
        (bucket.tokens <= 0.0).then(|| wait(rate, 1.0 - bucket.tokens))
    }

    fn debit(&mut self, key: &K, amount: f64, now: Instant) {
        self.bucket(key, now).tokens -= amount;
    }

    fn len(&self) -> usize {
        self.buckets.len()
    }
}

fn refill(rate: Rate, bucket: &mut Bucket, now: Instant) {
    let elapsed = now.saturating_duration_since(bucket.at).as_secs_f64();
    bucket.tokens = rate
        .burst
        .min(elapsed.mul_add(rate.per_second(), bucket.tokens));
    bucket.at = now;
}

/// How long `missing` tokens take to come back, at least a second.
fn wait(rate: Rate, missing: f64) -> Duration {
    let per_second = rate.per_second();
    if per_second <= 0.0 {
        return Duration::from_secs(3600);
    }
    Duration::from_secs_f64((missing / per_second).clamp(1.0, 3600.0).ceil())
}

struct Window {
    spent: u64,
    since: Instant,
}

/// Byte budgets by key over a fixed window that starts with a key's first
/// byte; windows that ended are swept as the map grows.
struct Spent<K> {
    limit: Option<u64>,
    window: Duration,
    windows: HashMap<K, Window>,
}

impl<K: Eq + Hash + Clone> Spent<K> {
    fn new(limit: Option<u64>, window: Duration) -> Self {
        Self {
            limit,
            window,
            windows: HashMap::new(),
        }
    }

    fn current(&mut self, key: &K, now: Instant) -> &mut Window {
        if self.windows.len() >= SWEEP_AT && !self.windows.contains_key(key) {
            let window = self.window;
            self.windows
                .retain(|_, current| now.saturating_duration_since(current.since) < window);
        }
        let window = self.window;
        let current = self.windows.entry(key.clone()).or_insert(Window {
            spent: 0,
            since: now,
        });
        if now.saturating_duration_since(current.since) >= window {
            current.spent = 0;
            current.since = now;
        }
        current
    }

    fn exhausted(&mut self, key: &K, now: Instant) -> Option<Duration> {
        let limit = self.limit?;
        let window = self.window;
        let current = self.current(key, now);
        (current.spent >= limit).then(|| {
            window
                .saturating_sub(now.saturating_duration_since(current.since))
                .max(Duration::from_secs(1))
        })
    }

    fn spend(&mut self, key: &K, bytes: u64, now: Instant) -> bool {
        let Some(limit) = self.limit else {
            return false;
        };
        let current = self.current(key, now);
        current.spent = current.spent.saturating_add(bytes);
        current.spent >= limit
    }
}

struct Limiter {
    device_requests: Buckets<Caller>,
    device_bytes: Buckets<Caller>,
    destinations: Buckets<String>,
    visitor_spent: Spent<Caller>,
    workspace_spent: Spent<Workspace>,
    device_in_flight: HashMap<Caller, usize>,
    in_flight: usize,
}

#[derive(Default)]
struct Metrics {
    answered: AtomicU64,
    bad_request: AtomicU64,
    unauthorized: AtomicU64,
    blocked: AtomicU64,
    rate_limited: AtomicU64,
    over_budget: AtomicU64,
    too_large: AtomicU64,
    timeout: AtomicU64,
    unreachable: AtomicU64,
    request_bytes: AtomicU64,
    response_bytes: AtomicU64,
}

impl Metrics {
    fn refused(&self, failure: ProxyFailure) {
        let counter = match failure {
            ProxyFailure::BadRequest => &self.bad_request,
            ProxyFailure::Unauthorized => &self.unauthorized,
            ProxyFailure::Blocked => &self.blocked,
            ProxyFailure::RateLimited => &self.rate_limited,
            ProxyFailure::OverBudget => &self.over_budget,
            ProxyFailure::TooLarge => &self.too_large,
            ProxyFailure::Timeout => &self.timeout,
            ProxyFailure::Unreachable => &self.unreachable,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub answered: u64,
    /// Requests refused or failed, by [`ProxyFailure::code`].
    pub failed: Vec<(&'static str, u64)>,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub in_flight: u64,
    pub devices: u64,
}

struct Inner {
    config: Config,
    client: reqwest::Client,
    limiter: Mutex<Limiter>,
    metrics: Metrics,
}

/// The fetch proxy a host serves. Clones share their connections, budgets
/// and counters.
#[derive(Clone)]
pub struct FetchProxy {
    inner: Arc<Inner>,
}

impl FetchProxy {
    pub fn new(config: Config) -> Result<Self, NetError> {
        Self::with_lookup(config, system_lookup())
    }

    pub fn with_lookup(config: Config, lookup: Lookup) -> Result<Self, NetError> {
        let client = guarded(config.destinations, lookup)
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| NetError::Protocol(error.to_string()))?;
        let limits = config.limits;
        let budgets = config.budgets;
        Ok(Self {
            inner: Arc::new(Inner {
                client,
                limiter: Mutex::new(Limiter {
                    device_requests: Buckets::new(limits.device_requests),
                    device_bytes: Buckets::new(limits.device_bytes),
                    destinations: Buckets::new(limits.destination_requests),
                    visitor_spent: Spent::new(budgets.visitor_bytes, budgets.window),
                    workspace_spent: Spent::new(budgets.workspace_bytes, budgets.window),
                    device_in_flight: HashMap::new(),
                    in_flight: 0,
                }),
                metrics: Metrics::default(),
                config,
            }),
        })
    }

    /// Relays `request` for `caller`, whom the host authenticated: the target's
    /// response, or the proxy's failure.
    pub async fn serve<B>(&self, caller: Caller, request: Request<B>) -> Response<ProxyBody>
    where
        B: http_body::Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        self.serve_in(caller, &[], request).await
    }

    /// [`Self::serve`] for a caller who belongs to `workspaces` on the host: what it
    /// receives counts against each workspace's budget too, and a workspace that
    /// spent its budget is refused.
    pub async fn serve_in<B>(
        &self,
        caller: Caller,
        workspaces: &[Workspace],
        request: Request<B>,
    ) -> Response<ProxyBody>
    where
        B: http_body::Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let deadline = Instant::now()
            .checked_add(self.inner.config.deadline)
            .unwrap_or_else(Instant::now);
        let method = request.method().clone();
        match self.relay(caller, workspaces, request, deadline).await {
            Ok(response) => {
                self.inner.metrics.answered.fetch_add(1, Ordering::Relaxed);
                response
            }
            Err((refused, destination)) => {
                log_refusal(caller, &method, destination.as_deref(), &refused);
                self.refusal(&refused)
            }
        }
    }

    /// The proxy's answer when the host refuses a request itself, such as
    /// [`ProxyFailure::Unauthorized`] for a page without a session.
    #[must_use]
    pub fn refuse(&self, failure: ProxyFailure, message: &str) -> Response<ProxyBody> {
        self.refusal(&Refused::new(failure, message))
    }

    fn refusal(&self, refused: &Refused) -> Response<ProxyBody> {
        self.inner.metrics.refused(refused.failure);
        failure_response(refused)
    }

    #[must_use]
    pub fn metrics(&self) -> MetricsSnapshot {
        let metrics = &self.inner.metrics;
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        let (in_flight, devices) = {
            let limiter = self.inner.limiter.lock_recover();
            (limiter.in_flight, limiter.device_requests.len())
        };
        MetricsSnapshot {
            answered: read(&metrics.answered),
            failed: vec![
                ("bad-request", read(&metrics.bad_request)),
                ("unauthorized", read(&metrics.unauthorized)),
                ("blocked", read(&metrics.blocked)),
                ("rate-limited", read(&metrics.rate_limited)),
                ("over-budget", read(&metrics.over_budget)),
                ("too-large", read(&metrics.too_large)),
                ("timeout", read(&metrics.timeout)),
                ("unreachable", read(&metrics.unreachable)),
            ],
            request_bytes: read(&metrics.request_bytes),
            response_bytes: read(&metrics.response_bytes),
            in_flight: u64::try_from(in_flight).unwrap_or(u64::MAX),
            devices: u64::try_from(devices).unwrap_or(u64::MAX),
        }
    }

    #[must_use]
    pub fn prometheus(&self, prefix: &str) -> String {
        let snapshot = self.metrics();
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# HELP {prefix}_requests_total Fetch proxy requests by outcome: answered by the target, or the failure the proxy answered with."
        );
        let _ = writeln!(out, "# TYPE {prefix}_requests_total counter");
        let _ = writeln!(
            out,
            "{prefix}_requests_total{{outcome=\"answered\"}} {}",
            snapshot.answered
        );
        for (code, count) in &snapshot.failed {
            let _ = writeln!(out, "{prefix}_requests_total{{outcome=\"{code}\"}} {count}");
        }
        for (name, kind, help, value) in [
            (
                "request_bytes_total",
                "counter",
                "Request body bytes sent on to targets.",
                snapshot.request_bytes,
            ),
            (
                "response_bytes_total",
                "counter",
                "Response body bytes relayed to pages.",
                snapshot.response_bytes,
            ),
            (
                "in_flight",
                "gauge",
                "Fetch proxy requests in flight.",
                snapshot.in_flight,
            ),
            (
                "devices",
                "gauge",
                "Devices that used the fetch proxy lately.",
                snapshot.devices,
            ),
        ] {
            let _ = writeln!(out, "# HELP {prefix}_{name} {help}");
            let _ = writeln!(out, "# TYPE {prefix}_{name} {kind}");
            let _ = writeln!(out, "{prefix}_{name} {value}");
        }
        out
    }

    async fn relay<B>(
        &self,
        caller: Caller,
        workspaces: &[Workspace],
        request: Request<B>,
        deadline: Instant,
    ) -> Result<Response<ProxyBody>, (Refused, Option<String>)>
    where
        B: http_body::Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let config = &self.inner.config;
        let (parts, body) = request.into_parts();
        let checked = check_request(&parts).map_err(|refused| (refused, None))?;
        let (mut method, mut url, form) = checked;
        let origin = |url: &Url| Some(url.origin().ascii_serialization());
        let permit = self
            .admit(caller, workspaces)
            .map_err(|refused| (refused, origin(&url)))?;
        let mut body = request_body(body, config.max_request_bytes, deadline)
            .await
            .map_err(|refused| (refused, origin(&url)))?;
        let mut headers = outgoing_headers(&parts.headers);
        if form == Form::Query {
            piece_of_range(&mut headers);
        }
        let mut hop = 0usize;
        let answer = loop {
            self.check_hop(&url, Instant::now())
                .map_err(|refused| (refused, origin(&url)))?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            let mut outgoing = self
                .inner
                .client
                .request(method.clone(), url.clone())
                .headers(headers.clone())
                .timeout(remaining);
            if let Some(body) = &body {
                self.inner.metrics.request_bytes.fetch_add(
                    u64::try_from(body.len()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
                outgoing = outgoing.body(body.clone());
            }
            let answer = match tokio::time::timeout_at(deadline, outgoing.send()).await {
                Err(_) => Err(Refused::new(
                    ProxyFailure::Timeout,
                    format!("{} did not answer in time", host_of(&url)),
                )),
                Ok(Err(error)) => Err(classify(&error, &url)),
                Ok(Ok(answer)) => Ok(answer),
            }
            .map_err(|refused| (refused, origin(&url)))?;
            let location = answer
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .filter(|_| answer.status().is_redirection())
                .and_then(|location| url.join(location).ok());
            let Some(next) = location else {
                break answer;
            };
            if hop >= config.max_redirects {
                return Err((
                    Refused::new(ProxyFailure::Unreachable, "too many redirects"),
                    origin(&url),
                ));
            }
            hop = hop.saturating_add(1);
            if next.origin() != url.origin() {
                headers.remove(header::AUTHORIZATION);
                headers.remove(header::COOKIE);
            }
            if redirect_becomes_get(answer.status(), &method) {
                method = Method::GET;
                body = None;
                headers.remove(header::CONTENT_TYPE);
            }
            url = next;
        };
        log_answer(caller, &method, &url, answer.status(), hop);
        let relayed = Relayed {
            answer: None,
            deadline,
            received: 0,
            limit: config.max_response_bytes,
            proxy: self.clone(),
            caller,
            workspaces: workspaces.into(),
            over_budget: false,
            _permit: permit,
        };
        streamed(answer, &url, relayed).map_err(|refused| (refused, origin(&url)))
    }

    fn admit(&self, caller: Caller, workspaces: &[Workspace]) -> Result<Permit, Refused> {
        let limits = self.inner.config.limits;
        let now = Instant::now();
        {
            let mut limiter = self.inner.limiter.lock_recover();
            let running = limiter.device_in_flight.get(&caller).copied().unwrap_or(0);
            if running >= limits.device_in_flight {
                return Err(Refused::limited(
                    "this device has too many requests in flight",
                    Duration::from_secs(1),
                ));
            }
            if limiter.in_flight >= limits.in_flight {
                return Err(Refused::limited(
                    "the fetch proxy is busy",
                    Duration::from_secs(1),
                ));
            }
            if let Some(wait) = limiter.visitor_spent.exhausted(&caller, now) {
                return Err(Refused::over_budget(
                    "this device received its share of the fetch proxy for now",
                    wait,
                ));
            }
            if let Some(wait) = workspaces
                .iter()
                .filter_map(|workspace| limiter.workspace_spent.exhausted(workspace, now))
                .max()
            {
                return Err(Refused::over_budget(
                    "this workspace received its share of the fetch proxy for now",
                    wait,
                ));
            }
            if let Some(wait) = limiter.device_bytes.empty_for(&caller, now) {
                return Err(Refused::limited(
                    "this device fetched too much through the proxy lately",
                    wait,
                ));
            }
            limiter
                .device_requests
                .take(&caller, 1.0, now)
                .map_err(|wait| {
                    Refused::limited("this device sent too many requests lately", wait)
                })?;
            limiter
                .device_in_flight
                .insert(caller, running.saturating_add(1));
            limiter.in_flight = limiter.in_flight.saturating_add(1);
        }
        Ok(Permit {
            proxy: Arc::clone(&self.inner),
            caller,
        })
    }

    /// Whether the proxy may go to `url` now: scheme, port, a literal address,
    /// and the destination's budget. A name's addresses are checked as it is
    /// resolved.
    fn check_hop(&self, url: &Url, now: Instant) -> Result<(), Refused> {
        check_target(url, self.inner.config.destinations)?;
        let host = host_of(url);
        self.inner
            .limiter
            .lock_recover()
            .destinations
            .take(&host, 1.0, now)
            .map_err(|wait| Refused::limited(format!("too many requests to {host} lately"), wait))
    }

    fn received(&self, caller: Caller, workspaces: &[Workspace], bytes: usize) -> bool {
        let bytes_u64 = u64::try_from(bytes).unwrap_or(u64::MAX);
        self.inner
            .metrics
            .response_bytes
            .fetch_add(bytes_u64, Ordering::Relaxed);
        #[allow(
            clippy::cast_precision_loss,
            reason = "a byte budget; a chunk is far below 2^52 bytes"
        )]
        let amount = bytes as f64;
        let now = Instant::now();
        let mut limiter = self.inner.limiter.lock_recover();
        limiter.device_bytes.debit(&caller, amount, now);
        let mut spent = limiter.visitor_spent.spend(&caller, bytes_u64, now);
        for workspace in workspaces {
            spent |= limiter.workspace_spent.spend(workspace, bytes_u64, now);
        }
        spent
    }
}

/// A place in flight; released when the response body ends or is dropped.
struct Permit {
    proxy: Arc<Inner>,
    caller: Caller,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut limiter = self.proxy.limiter.lock_recover();
        limiter.in_flight = limiter.in_flight.saturating_sub(1);
        let running = limiter
            .device_in_flight
            .get(&self.caller)
            .copied()
            .unwrap_or(0)
            .saturating_sub(1);
        if running == 0 {
            limiter.device_in_flight.remove(&self.caller);
        } else {
            limiter.device_in_flight.insert(self.caller, running);
        }
    }
}

async fn request_body<B>(body: B, limit: usize, deadline: Instant) -> Result<Option<Bytes>, Refused>
where
    B: http_body::Body<Data = Bytes> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    match tokio::time::timeout_at(
        deadline,
        http_body_util::Limited::new(body, limit).collect(),
    )
    .await
    {
        Err(_) => Err(Refused::new(
            ProxyFailure::Timeout,
            "the request body did not arrive in time",
        )),
        Ok(Err(_)) => Err(Refused::new(
            ProxyFailure::TooLarge,
            format!("the request body is larger than {limit} bytes, or did not arrive whole"),
        )),
        Ok(Ok(collected)) => {
            let bytes = collected.to_bytes();
            Ok((!bytes.is_empty()).then_some(bytes))
        }
    }
}

fn redirect_becomes_get(status: StatusCode, method: &Method) -> bool {
    (status == StatusCode::SEE_OTHER && method != Method::HEAD)
        || (matches!(status, StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND)
            && method == Method::POST)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    Header,
    Query,
}

/// The most of an open-ended byte range one exchange answers for a request
/// in the query form; the element asks for the rest as it plays.
pub const MEDIA_PIECE: u64 = 2 * 1024 * 1024;

/// Turns an open-ended `Range: bytes=N-` into `bytes=N-(N+MEDIA_PIECE-1)`: a
/// range-capable target answers that piece with `206` and the whole length,
/// and the media element asks for the next piece itself.
fn piece_of_range(headers: &mut HeaderMap) {
    let start = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().strip_prefix("bytes="))
        .and_then(|spec| spec.strip_suffix('-'))
        .and_then(|start| start.parse::<u64>().ok());
    if let Some(start) = start {
        let end = start.saturating_add(MEDIA_PIECE - 1);
        if let Ok(value) = HeaderValue::from_str(&format!("bytes={start}-{end}")) {
            headers.insert(header::RANGE, value);
        }
    }
}

fn query_param(uri: &::http::Uri, name: &str) -> Option<String> {
    url::form_urlencoded::parse(uri.query()?.as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

#[must_use]
pub fn query_session(method: &Method, uri: &::http::Uri) -> Option<String> {
    matches!(*method, Method::GET | Method::HEAD)
        .then(|| query_param(uri, SESSION_PARAM))
        .flatten()
}

fn check_request(parts: &::http::request::Parts) -> Result<(Method, Url, Form), Refused> {
    let header = parts
        .headers
        .get(MARKER_HEADER)
        .and_then(|value| value.to_str().ok())
        == Some(VERSION);
    let query = matches!(parts.method, Method::GET | Method::HEAD)
        && query_param(&parts.uri, MARKER_PARAM).as_deref() == Some(VERSION);
    let form = match (header, query) {
        (true, _) => Form::Header,
        (false, true) => Form::Query,
        (false, false) => {
            return Err(Refused::new(
                ProxyFailure::BadRequest,
                format!("{MARKER_HEADER}: {VERSION} is required"),
            ));
        }
    };
    if !METHODS.contains(&parts.method.as_str()) {
        return Err(Refused::new(
            ProxyFailure::BadRequest,
            format!("the proxy does not relay {}", parts.method),
        ));
    }
    let target = query_param(&parts.uri, "url")
        .and_then(|target| Url::parse(&target).ok())
        .ok_or_else(|| {
            Refused::new(
                ProxyFailure::BadRequest,
                "url is missing or not an absolute URL",
            )
        })?;
    if !matches!(target.scheme(), "http" | "https") {
        return Err(Refused::new(
            ProxyFailure::BadRequest,
            format!("{}: is not http or https", target.scheme()),
        ));
    }
    Ok((parts.method.clone(), target, form))
}

fn check_target(url: &Url, destinations: Destinations) -> Result<(), Refused> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Refused::new(
            ProxyFailure::BadRequest,
            format!("{}: is not http or https", url.scheme()),
        ));
    }
    let port = url.port_or_known_default().unwrap_or(0);
    if BAD_PORTS.contains(&port) {
        return Err(Refused::new(
            ProxyFailure::Blocked,
            format!("port {port} is not for the web"),
        ));
    }
    if url.host().is_none() {
        return Err(Refused::new(ProxyFailure::BadRequest, "url names no host"));
    }
    if destinations == Destinations::Public && is_non_public_literal(url) {
        return Err(Refused::new(
            ProxyFailure::Blocked,
            format!("{} is on the proxy's own network", host_of(url)),
        ));
    }
    Ok(())
}

fn host_of(url: &Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => String::new(),
    }
}

fn classify(error: &reqwest::Error, url: &Url) -> Refused {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(cause) = source {
        if let Some(blocked) = cause.downcast_ref::<BlockedAddress>() {
            return Refused::new(ProxyFailure::Blocked, blocked.to_string());
        }
        source = cause.source();
    }
    if error.is_timeout() {
        return Refused::new(
            ProxyFailure::Timeout,
            format!("{} did not answer in time", host_of(url)),
        );
    }
    let mut message = format!("{}: ", host_of(url));
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    let mut deepest = String::new();
    while let Some(cause) = source {
        deepest = cause.to_string();
        source = cause.source();
    }
    message.push_str(&deepest);
    Refused::new(ProxyFailure::Unreachable, message)
}

/// Whether `name` is a request header a server never lets a caller set on an
/// outgoing request: hop-by-hop framing, target host, proxy connection and
/// proxy credentials, and forwarding headers that name the caller.
///
/// Matched ASCII case-insensitively. Callers with further restrictions of
/// their own (such as the fetch proxy's ingress headers or a hosted web call's
/// credential and method-override headers) layer them on top of this check.
#[must_use]
pub fn is_withheld_request_header(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "connection",
        "content-length",
        "expect",
        "forwarded",
        "host",
        "keep-alive",
        "proxy-authorization",
        "proxy-connection",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "x-real-ip",
    ];
    NAMES.iter().any(|held| name.eq_ignore_ascii_case(held))
        || name
            .as_bytes()
            .get(..b"x-forwarded-".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"x-forwarded-"))
}

/// Request headers that never reach the target: the shared withheld set
/// ([`is_withheld_request_header`]), the page's own identity at the host,
/// connection management, and what the ingress added about the caller.
fn withheld(name: &str) -> bool {
    const EXTRAS: &[&str] = &["accept-encoding", "true-client-ip", "via"];
    is_withheld_request_header(name)
        || EXTRAS.contains(&name)
        || name.starts_with("x-tilo-")
        || name.starts_with("proxy-")
        || name.starts_with("cf-")
}

/// The headers to send the target: the page's, without its credentials at the
/// host, with the ones the browser would not send restored from
/// [`FORWARD_PREFIX`]. A restored header replaces the browser's own of that
/// name (its `User-Agent`, say).
fn outgoing_headers(incoming: &HeaderMap) -> HeaderMap {
    let mut plain = HeaderMap::with_capacity(incoming.len());
    let mut restored = HeaderMap::new();
    for (name, value) in incoming {
        let text = name.as_str();
        if let Some(original) = text.strip_prefix(FORWARD_PREFIX) {
            if !withheld(original)
                && let Ok(original) = HeaderName::try_from(original)
            {
                restored.append(original, value.clone());
            }
        } else if !withheld(text)
            && !text.starts_with("sec-")
            && !matches!(text, "cookie" | "origin" | "referer")
        {
            plain.append(name.clone(), value.clone());
        }
    }
    for name in restored.keys() {
        plain.remove(name);
    }
    for (name, value) in &restored {
        plain.append(name.clone(), value.clone());
    }
    plain
}

/// Response headers the page never sees: connection management, and the
/// length of a body this proxy re-frames.
fn dropped_response(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "content-length"
            | "keep-alive"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Response headers the browser would apply to the host's origin, not to the
/// target's: they reach the app renamed under [`FORWARD_PREFIX`].
fn renamed_response(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "age",
        "alt-svc",
        "cache-control",
        "cdn-cache-control",
        "clear-site-data",
        "cloudflare-cdn-cache-control",
        "content-security-policy",
        "content-security-policy-report-only",
        "expires",
        "link",
        "nel",
        "origin-agent-cluster",
        "permissions-policy",
        "pragma",
        "proxy-authenticate",
        "refresh",
        "report-to",
        "reporting-endpoints",
        "service-worker-allowed",
        "set-cookie",
        "set-cookie2",
        "strict-transport-security",
        "surrogate-control",
        "timing-allow-origin",
        "vary",
        "www-authenticate",
        "x-frame-options",
    ];
    NAMES.contains(&name)
        || name.starts_with("access-control-")
        || name.starts_with("cross-origin-")
        || name.starts_with("x-tilo-")
}

fn response_headers(upstream: &HeaderMap, final_url: &Url) -> HeaderMap {
    let mut headers = HeaderMap::with_capacity(upstream.len().saturating_add(2));
    for (name, value) in upstream {
        let text = name.as_str();
        if dropped_response(text) {
            continue;
        }
        if renamed_response(text) {
            if let Ok(renamed) = HeaderName::try_from(format!("{FORWARD_PREFIX}{text}")) {
                headers.append(renamed, value.clone());
            }
        } else {
            headers.append(name.clone(), value.clone());
        }
    }
    if let Ok(final_url) = HeaderValue::from_str(final_url.as_str()) {
        headers.insert(FINAL_URL_HEADER, final_url);
    }
    // A shared cache in front of the host (a CDN) must never answer one
    // device's proxied request to another.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers
}

fn failure_response(refused: &Refused) -> Response<ProxyBody> {
    let mut response = Response::new(ProxyBody::full(Bytes::from(refused.message.clone())));
    *response.status_mut() =
        StatusCode::from_u16(refused.failure.status()).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = response.headers_mut();
    headers.insert(
        ERROR_HEADER,
        HeaderValue::from_static(refused.failure.code()),
    );
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(wait) = refused.retry_after
        && let Ok(value) = HeaderValue::from_str(&wait.as_secs().max(1).to_string())
    {
        headers.insert(header::RETRY_AFTER, value);
    }
    response
}

fn log_refusal(caller: Caller, method: &Method, destination: Option<&str>, refused: &Refused) {
    let device = caller.short();
    let destination = destination.unwrap_or("-");
    let code = refused.failure.code();
    match refused.failure {
        // Someone probing the host's own network.
        ProxyFailure::Blocked => tracing::warn!(
            target: "fetch_proxy",
            device, %method, destination, code, "refused a destination on the host's own network"
        ),
        ProxyFailure::RateLimited
        | ProxyFailure::OverBudget
        | ProxyFailure::Unauthorized
        | ProxyFailure::BadRequest => {
            tracing::info!(target: "fetch_proxy", device, %method, destination, code, "refused");
        }
        ProxyFailure::TooLarge | ProxyFailure::Timeout | ProxyFailure::Unreachable => {
            tracing::debug!(target: "fetch_proxy", device, %method, destination, code, "failed");
        }
    }
}

/// Whether the target's answer with `status` is logged where the host's
/// operators read: a refusal or a failure upstream is, so a page whose
/// fetches fail can be diagnosed; a plain answer only at `debug`.
fn answer_is_notable(status: StatusCode) -> bool {
    status.is_client_error() || status.is_server_error()
}

/// One line per relayed request: who asked, the destination's host (never its
/// path, query, cookies or body), the target's status and how many redirects
/// led there.
fn log_answer(caller: Caller, method: &Method, url: &Url, status: StatusCode, redirects: usize) {
    let device = caller.short();
    let destination = host_of(url);
    let notable = answer_is_notable(status);
    let status = status.as_u16();
    if notable {
        tracing::info!(target: "fetch_proxy", device, %method, destination, status, redirects, "upstream refused");
    } else {
        tracing::debug!(target: "fetch_proxy", device, %method, destination, status, redirects, "answered");
    }
}

/// The target's `answer` from `url` as the page's response, its body streamed
/// through `relayed` (bounded, on the clock and on the budgets); refused when
/// it declares a body over the limit.
fn streamed(
    answer: reqwest::Response,
    url: &Url,
    mut relayed: Relayed,
) -> Result<Response<ProxyBody>, Refused> {
    if answer
        .content_length()
        .is_some_and(|length| length > relayed.limit)
    {
        return Err(Refused::new(
            ProxyFailure::TooLarge,
            format!("the response is larger than {} bytes", relayed.limit),
        ));
    }
    let status = answer.status();
    let headers = response_headers(answer.headers(), url);
    relayed.answer = Some(answer);
    let mut response = Response::new(ProxyBody::streamed(relayed));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

struct Relayed {
    answer: Option<reqwest::Response>,
    deadline: Instant,
    received: u64,
    limit: u64,
    proxy: FetchProxy,
    caller: Caller,
    workspaces: Arc<[Workspace]>,
    /// A budget this response counts against was spent by its last chunk: it ends
    /// here, with an error if more was coming.
    over_budget: bool,
    _permit: Permit,
}

/// Why a relayed body stopped early. The page sees its response end with a
/// network error, never a short body that reads as whole.
#[derive(Debug)]
pub struct ProxyBodyError(String);

impl std::fmt::Display for ProxyBodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProxyBodyError {}

type Chunks = Pin<Box<dyn Stream<Item = Result<Bytes, ProxyBodyError>> + Send>>;

enum Kind {
    Full(Option<Bytes>),
    Pending(Option<Box<Relayed>>),
    Stream(Chunks),
}

pub struct ProxyBody {
    kind: Kind,
}

impl ProxyBody {
    fn full(bytes: Bytes) -> Self {
        Self {
            kind: Kind::Full(Some(bytes)),
        }
    }

    fn streamed(relayed: Relayed) -> Self {
        Self {
            kind: Kind::Pending(Some(Box::new(relayed))),
        }
    }
}

fn chunks(relayed: Relayed) -> Chunks {
    Box::pin(futures_util::stream::unfold(
        Some(relayed),
        |state| async move {
            let mut relayed = state?;
            let answer = relayed.answer.as_mut()?;
            match tokio::time::timeout_at(relayed.deadline, answer.chunk()).await {
                Err(_) => Some((
                    Err(ProxyBodyError("the response did not arrive in time".into())),
                    None,
                )),
                Ok(Err(error)) => Some((
                    Err(ProxyBodyError(format!("the response broke off: {error}"))),
                    None,
                )),
                Ok(Ok(None)) => None,
                Ok(Ok(Some(_))) if relayed.over_budget => Some((
                    Err(ProxyBodyError(
                        "the proxy relayed as much as its budget allows for now".into(),
                    )),
                    None,
                )),
                Ok(Ok(Some(chunk))) => {
                    let size = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
                    relayed.received = relayed.received.saturating_add(size);
                    if relayed.received > relayed.limit {
                        return Some((
                            Err(ProxyBodyError(format!(
                                "the response is larger than {} bytes",
                                relayed.limit
                            ))),
                            None,
                        ));
                    }
                    relayed.over_budget =
                        relayed
                            .proxy
                            .received(relayed.caller, &relayed.workspaces, chunk.len());
                    Some((Ok(chunk), Some(relayed)))
                }
            }
        },
    ))
}

impl http_body::Body for ProxyBody {
    type Data = Bytes;
    type Error = ProxyBodyError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, ProxyBodyError>>> {
        let this = self.get_mut();
        if let Kind::Pending(relayed) = &mut this.kind {
            this.kind = match relayed.take() {
                Some(relayed) => Kind::Stream(chunks(*relayed)),
                None => Kind::Full(None),
            };
        }
        match &mut this.kind {
            Kind::Full(bytes) => Poll::Ready(bytes.take().map(|bytes| Ok(Frame::data(bytes)))),
            Kind::Pending(_) => Poll::Ready(None),
            Kind::Stream(stream) => stream
                .as_mut()
                .poll_next(cx)
                .map(|chunk| chunk.map(|chunk| chunk.map(Frame::data))),
        }
    }

    fn is_end_stream(&self) -> bool {
        matches!(self.kind, Kind::Full(None))
    }
}

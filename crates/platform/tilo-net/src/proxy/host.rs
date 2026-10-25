//! The host side of the fetch proxy (feature `proxy-host`): wire protocol v1
//! as a workspace host serves it to the pages of its members.
//!
//! [`FetchProxy::serve`] takes one request the host already authenticated
//! (the [`Caller`] is the device whose session it carries) and answers the
//! target's response or a [`ProxyFailure`]. The host does the rest: which
//! path it serves the proxy at, who may call it, and the CORS headers for a
//! page on another origin. `tilo-relay` serves it to its browser sessions;
//! the `tilo-fetch-proxy` program serves it unauthenticated on loopback for
//! the development server.
//!
//! What it enforces, whoever hosts it:
//!
//! - **Destinations:** `http` and `https` on a port browsers allow
//!   ([`BAD_PORTS`] are refused). Every name is resolved once, every address
//!   it resolves to must be public ([`is_public`]), and the connection goes
//!   to those checked addresses, never to a second lookup that a rebinding
//!   name could answer differently. Redirects are followed here, each hop
//!   checked again; `Authorization` and `Cookie` are dropped when a hop
//!   changes origin.
//! - **Methods:** the HTTP and `WebDAV` methods apps use; never `CONNECT` or
//!   `TRACE`. A `GET` or `HEAD` may carry the marker in the query instead
//!   ([`super::media_url`]), for a media element; an open-ended byte range
//!   it asks for is answered [`MEDIA_PIECE`] bytes at a time, so a stream
//!   that plays for minutes is many short exchanges, each within the
//!   bounds.
//! - **Credentials:** the page's own cookies and session never leave; the
//!   caller's address and the ingress's forwarding headers are not passed on.
//!   Response headers that the host's origin would act on (cookies,
//!   authentication prompts, caching, security policies, site data) reach
//!   the page renamed, so only the app reads them.
//! - **Bounds:** request and response bodies, and one deadline for the
//!   whole exchange, body included ([`Config`]).
//! - **Rates:** per device, requests and response bytes; per destination
//!   host, requests from everyone together; requests in flight, per device
//!   and in total ([`RateLimits`]).
//! - **Budgets:** response bytes relayed per visitor and per room over a
//!   window, a day by default ([`Budgets`]). Rates smooth a burst out; a
//!   budget caps what one visitor, or everyone in a room together, streams
//!   through the host in a day. Spent, the proxy answers
//!   [`ProxyFailure::OverBudget`] until the window turns over, and a
//!   response it is relaying ends there with an error.
//! - **Logs:** one line per refusal under the `fetch_proxy` target, naming
//!   the device and the destination's origin only. Neither bodies, headers,
//!   paths nor queries are logged: they carry people's credentials.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::future::Future;
use std::hash::Hash;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
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
use url::{Host, Url};

use super::{
    ERROR_HEADER, FINAL_URL_HEADER, FORWARD_PREFIX, MARKER_HEADER, MARKER_PARAM, PEER_LOCAL,
    PEER_PARAM, ProxyFailure, SESSION_PARAM, VERSION,
};
use crate::NetError;

mod local;

/// Bounds of one proxied exchange, and who may reach what.
#[derive(Clone, Debug)]
pub struct Config {
    /// Largest request body the proxy reads.
    pub max_request_bytes: usize,
    /// Largest response body it relays, after decompression.
    pub max_response_bytes: u64,
    /// The whole exchange, redirects and the response body included.
    pub deadline: Duration,
    /// Redirects followed before the proxy gives up.
    pub max_redirects: usize,
    /// Which addresses it may connect to.
    pub destinations: Destinations,
    /// How much each device, and everyone, may ask of it.
    pub limits: RateLimits,
    /// How much it relays per visitor and per room over a window.
    pub budgets: Budgets,
    /// Whether it also answers for the host machine's own files
    /// ([`PEER_LOCAL`]): an app in a page reads the same paths its person
    /// reads in the desktop session, without downloading them. Off by
    /// default, and never on from a client's say-so.
    pub peer: bool,
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
            peer: false,
        }
    }
}

/// Which addresses the proxy connects to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destinations {
    /// Public addresses only: never the proxy's own network.
    Public,
    /// Any address, loopback and private ones included: a development
    /// proxy reaching a local test server.
    Any,
}

/// A token bucket: `burst` at once, refilled at `per_minute`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rate {
    /// What may be spent at once.
    pub burst: f64,
    /// What comes back per minute.
    pub per_minute: f64,
}

impl Rate {
    fn per_second(self) -> f64 {
        self.per_minute / 60.0
    }
}

/// What the proxy allows each device, each destination and everyone.
///
/// The defaults suit a public room on a busy day: a person scrolling a feed
/// reader or a video list (a list and its thumbnails is some sixty requests)
/// never meets them, while a script using the host as an open proxy runs
/// out within a minute.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateLimits {
    /// Requests per device.
    pub device_requests: Rate,
    /// Response bytes per device.
    pub device_bytes: Rate,
    /// Requests per destination host, from every device together, so the
    /// host cannot be turned on one site.
    pub destination_requests: Rate,
    pub device_in_flight: usize,
    /// Requests in flight at once, in total.
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

/// Response bytes the proxy relays over [`Budgets::window`], per visitor
/// and per room; `None` is no budget.
///
/// The defaults let a person watch a few hours of video a day through the
/// host (a 360p stream is some 300 MiB an hour) and a room's people
/// together a few days' worth, so one busy room cannot spend a host's
/// transfer allowance on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budgets {
    /// How long a budget lasts, from the first byte spent in it.
    pub window: Duration,
    /// Bytes one visitor (a [`Caller`]) may receive per window.
    pub visitor_bytes: Option<u64>,
    /// Bytes everyone in one [`Room`] together may receive per window.
    pub room_bytes: Option<u64>,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(24 * 60 * 60),
            visitor_bytes: Some(2 * 1024 * 1024 * 1024),
            room_bytes: Some(64 * 1024 * 1024 * 1024),
        }
    }
}

/// A room or workspace on the host that a request's visitor belongs to, by
/// the host's own identifier for it; its people share one budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Room(pub u64);

/// The device a request is made for: the key the host authenticated its
/// session with. A development host, which authenticates nobody, uses
/// [`Caller::LOCAL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Caller(pub [u8; 32]);

impl Caller {
    /// Everyone, on a development host.
    pub const LOCAL: Self = Self([0; 32]);

    /// The first bytes of the device key, enough to tell devices apart in a
    /// log line.
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

/// The system's resolver.
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

/// Whether `address` is on the public internet: not loopback, private
/// (RFC 1918, unique-local), link-local, CGNAT, multicast, unspecified,
/// documentation or otherwise reserved, also when an IPv6 address embeds
/// one of those IPv4 addresses (mapped, NAT64, 6to4).
#[must_use]
pub fn is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => public_v4(address),
        IpAddr::V6(address) => public_v6(address),
    }
}

fn public_v4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    let reserved = a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..128).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..32).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224;
    !reserved
}

fn public_v6(address: Ipv6Addr) -> bool {
    if let Some(v4) = address.to_ipv4_mapped() {
        return public_v4(v4);
    }
    let [s0, s1, s2, s3, s4, s5, s6, s7] = address.segments();
    let embedded = |high: u16, low: u16| {
        let [a, b] = high.to_be_bytes();
        let [c, d] = low.to_be_bytes();
        Ipv4Addr::new(a, b, c, d)
    };
    // NAT64 (64:ff9b::/96, 64:ff9b:1::/48) reaches the IPv4 address it ends in.
    if s0 == 0x64 && s1 == 0xff9b {
        return public_v4(embedded(s6, s7));
    }
    // 6to4 (2002::/16) reaches the IPv4 address after its prefix.
    if s0 == 0x2002 {
        return public_v4(embedded(s1, s2));
    }
    let reserved =
        // ::, ::1 and the deprecated IPv4-compatible addresses.
        (s0 == 0 && s1 == 0 && s2 == 0 && s3 == 0 && s4 == 0 && s5 == 0)
        // Discard-only.
        || (s0 == 0x0100 && s1 == 0 && s2 == 0 && s3 == 0)
        // Teredo, which embeds an obscured IPv4 address, and documentation.
        || (s0 == 0x2001 && (s1 == 0 || s1 == 0x0db8))
        // Unique-local, link-local, site-local and multicast.
        || (s0 & 0xfe00) == 0xfc00
        || (s0 & 0xffc0) == 0xfe80
        || (s0 & 0xffc0) == 0xfec0
        || (s0 & 0xff00) == 0xff00;
    // Only global unicast (2000::/3) is routed on the internet.
    !reserved && (s0 & 0xe000) == 0x2000
}

/// A name that resolved to an address on the proxy's own network.
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

/// Why the proxy answered instead of the target.
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

    /// Spend `amount` now, or say how long until there is enough.
    fn take(&mut self, key: &K, amount: f64, now: Instant) -> Result<(), Duration> {
        let rate = self.rate;
        let bucket = self.bucket(key, now);
        if bucket.tokens >= amount {
            bucket.tokens -= amount;
            return Ok(());
        }
        Err(wait(rate, amount - bucket.tokens))
    }

    /// How long until the bucket holds anything at all.
    fn empty_for(&mut self, key: &K, now: Instant) -> Option<Duration> {
        let rate = self.rate;
        let bucket = self.bucket(key, now);
        (bucket.tokens <= 0.0).then(|| wait(rate, 1.0 - bucket.tokens))
    }

    /// Spend `amount` after the fact; the bucket may go below empty.
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

/// What one key spent in its current window.
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

    /// The key's window, started now when it has none or its last ended.
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

    /// How long until the key may receive again, when it spent its budget.
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

    /// Count `bytes` against the key; whether that spent its budget.
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
    room_spent: Spent<Room>,
    device_in_flight: HashMap<Caller, usize>,
    in_flight: usize,
}

/// Counters for a host's metrics page.
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

/// The proxy's counters at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    /// Requests answered with the target's response.
    pub answered: u64,
    /// Requests refused or failed, by [`ProxyFailure::code`].
    pub failed: Vec<(&'static str, u64)>,
    /// Request body bytes sent on to targets.
    pub request_bytes: u64,
    /// Response body bytes relayed to pages.
    pub response_bytes: u64,
    /// Requests in flight now.
    pub in_flight: u64,
    /// Devices whose request budget is being tracked (used it lately).
    pub devices: u64,
}

struct Inner {
    config: Config,
    client: reqwest::Client,
    limiter: Mutex<Limiter>,
    metrics: Metrics,
}

/// The fetch proxy a host serves. Cheap to clone; clones share their
/// connections, budgets and counters.
#[derive(Clone)]
pub struct FetchProxy {
    inner: Arc<Inner>,
}

impl FetchProxy {
    /// A proxy on the system's resolver.
    ///
    /// # Errors
    ///
    /// [`NetError::Protocol`] when the HTTP client cannot be built.
    pub fn new(config: Config) -> Result<Self, NetError> {
        Self::with_lookup(config, system_lookup())
    }

    /// A proxy that resolves names with `lookup`.
    ///
    /// # Errors
    ///
    /// [`NetError::Protocol`] when the HTTP client cannot be built.
    pub fn with_lookup(config: Config, lookup: Lookup) -> Result<Self, NetError> {
        let client = crate::http::client()
            // Bodies arrive decompressed, as a page's `fetch` reads them;
            // the response limit then bounds what was decompressed.
            .gzip(true)
            .brotli(true)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .referer(false)
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(30))
            .dns_resolver(Arc::new(CheckedResolver {
                lookup,
                destinations: config.destinations,
            }))
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
                    room_spent: Spent::new(budgets.room_bytes, budgets.window),
                    device_in_flight: HashMap::new(),
                    in_flight: 0,
                }),
                metrics: Metrics::default(),
                config,
            }),
        })
    }

    /// Relay `request` for `caller`, whom the host authenticated: the
    /// target's response, or the proxy's failure.
    pub async fn serve<B>(&self, caller: Caller, request: Request<B>) -> Response<ProxyBody>
    where
        B: http_body::Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        self.serve_in(caller, &[], request).await
    }

    /// [`Self::serve`] for a caller who belongs to `rooms` on the host: what
    /// it receives counts against each room's budget too, and a room that
    /// spent its budget is refused.
    pub async fn serve_in<B>(
        &self,
        caller: Caller,
        rooms: &[Room],
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
        match self.relay(caller, rooms, request, deadline).await {
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
    /// [`ProxyFailure::Unauthorized`] for a page without a session; counted
    /// like the proxy's own refusals.
    #[must_use]
    pub fn refuse(&self, failure: ProxyFailure, message: &str) -> Response<ProxyBody> {
        self.refusal(&Refused::new(failure, message))
    }

    fn refusal(&self, refused: &Refused) -> Response<ProxyBody> {
        self.inner.metrics.refused(refused.failure);
        failure_response(refused)
    }

    /// The proxy's counters now.
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

    /// The counters as Prometheus text, each name starting with `prefix`.
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

    /// The exchange; a refusal comes back with the destination's origin,
    /// when it got that far, for the log.
    async fn relay<B>(
        &self,
        caller: Caller,
        rooms: &[Room],
        request: Request<B>,
        deadline: Instant,
    ) -> Result<Response<ProxyBody>, (Refused, Option<String>)>
    where
        B: http_body::Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let config = &self.inner.config;
        let (parts, body) = request.into_parts();
        let checked = check_request(&parts, config.peer).map_err(|refused| (refused, None))?;
        let (mut method, mut url, form) = checked;
        let origin = |url: &Url| Some(url.origin().ascii_serialization());
        let permit = self
            .admit(caller, rooms)
            .map_err(|refused| (refused, origin(&url)))?;
        let mut body = request_body(body, config.max_request_bytes, deadline)
            .await
            .map_err(|refused| (refused, origin(&url)))?;
        let mut headers = outgoing_headers(&parts.headers);
        if form == Form::Query {
            piece_of_range(&mut headers);
        }
        if url.scheme() == PEER_SCHEME {
            return Self::peer_answer(self, caller, rooms, &method, &url, deadline, permit);
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
            rooms: rooms.into(),
            over_budget: false,
            _permit: permit,
        };
        streamed(answer, &url, relayed).map_err(|refused| (refused, origin(&url)))
    }

    /// The answer to a peer request: this machine's own file, read here and
    /// relayed under the same bounds, clock and budgets as a site's answer, so
    /// a page reads either without knowing which it got.
    fn peer_answer(
        proxy: &FetchProxy,
        caller: Caller,
        rooms: &[Room],
        method: &Method,
        url: &Url,
        deadline: Instant,
        permit: Permit,
    ) -> Result<Response<ProxyBody>, (Refused, Option<String>)> {
        let origin = || Some(url.origin().ascii_serialization());
        let path = PathBuf::from(url.path());
        let answer = local::answer(&path, method);
        log_answer(caller, method, url, answer.status(), 0);
        let relayed = Relayed {
            answer: None,
            deadline,
            received: 0,
            limit: proxy.inner.config.max_response_bytes,
            proxy: proxy.clone(),
            caller,
            rooms: rooms.into(),
            over_budget: false,
            _permit: permit,
        };
        streamed(answer, url, relayed).map_err(|refused| (refused, origin()))
    }

    /// Take one request from `caller`'s budget and one place in flight.
    fn admit(&self, caller: Caller, rooms: &[Room]) -> Result<Permit, Refused> {
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
            if let Some(wait) = rooms
                .iter()
                .filter_map(|room| limiter.room_spent.exhausted(room, now))
                .max()
            {
                return Err(Refused::over_budget(
                    "this room received its share of the fetch proxy for now",
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

    /// Whether the proxy may go to `url` now: scheme, port, a literal
    /// address, and the destination's budget. A name's addresses are
    /// checked as it is resolved.
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

    /// Count `bytes` relayed to `caller` in `rooms`; whether a budget they
    /// count against is spent now.
    fn received(&self, caller: Caller, rooms: &[Room], bytes: usize) -> bool {
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
        for room in rooms {
            spent |= limiter.room_spent.spend(room, bytes_u64, now);
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

/// The method and target of a proxy request, or why it is refused.
/// The request's body, whole, or `None` when it is empty: at most `limit`
/// bytes, arrived by `deadline`.
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

/// Whether a redirect with `status` turns `method` into a `GET` without a
/// body, as browsers follow it.
fn redirect_becomes_get(status: StatusCode, method: &Method) -> bool {
    (status == StatusCode::SEE_OTHER && method != Method::HEAD)
        || (matches!(status, StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND)
            && method == Method::POST)
}

/// How a request named itself a proxy request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// [`MARKER_HEADER`], as `tilo_net` sends every request.
    Header,
    /// [`MARKER_PARAM`] in the query: a `GET` or `HEAD` from a page element
    /// that cannot send headers.
    Query,
}

/// The most of an open-ended byte range one exchange answers for a request
/// in the query form; the element asks for the rest as it plays.
pub const MEDIA_PIECE: u64 = 2 * 1024 * 1024;

/// Turn an open-ended `Range: bytes=N-` into `bytes=N-(N+MEDIA_PIECE-1)`: a
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

/// The value of query parameter `name` in `uri`.
fn query_param(uri: &::http::Uri, name: &str) -> Option<String> {
    url::form_urlencoded::parse(uri.query()?.as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// The session a request in the query form carries ([`SESSION_PARAM`]):
/// only a `GET` or `HEAD`, as the marker.
#[must_use]
pub fn query_session(method: &Method, uri: &::http::Uri) -> Option<String> {
    matches!(*method, Method::GET | Method::HEAD)
        .then(|| query_param(uri, SESSION_PARAM))
        .flatten()
}

/// The scheme a peer request names: this machine's own path, not a site.
const PEER_SCHEME: &str = "file";

fn check_request(
    parts: &::http::request::Parts,
    peer: bool,
) -> Result<(Method, Url, Form), Refused> {
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
    // A peer request is this machine's own path, written as the URL it is on
    // disk; the host reads it only when it offers [`Config::peer`].
    let peer_target = query_param(&parts.uri, PEER_PARAM).as_deref() == Some(PEER_LOCAL);
    if peer_target {
        if !peer {
            return Err(Refused::new(
                ProxyFailure::BadRequest,
                "this host does not answer for its own files",
            ));
        }
        if target.scheme() != PEER_SCHEME {
            return Err(Refused::new(
                ProxyFailure::BadRequest,
                format!(
                    "a peer request names a {PEER_SCHEME}: path, not {}",
                    target.scheme()
                ),
            ));
        }
        return Ok((parts.method.clone(), target, form));
    }
    if query_param(&parts.uri, PEER_PARAM).is_some() {
        return Err(Refused::new(
            ProxyFailure::BadRequest,
            format!("{PEER_PARAM} is missing or names no known resource"),
        ));
    }
    if !matches!(target.scheme(), "http" | "https") {
        return Err(Refused::new(
            ProxyFailure::BadRequest,
            format!("{}: is not http or https", target.scheme()),
        ));
    }
    Ok((parts.method.clone(), target, form))
}

/// Scheme, port and a literal address of `url`.
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
    let literal = match url.host() {
        None => {
            return Err(Refused::new(ProxyFailure::BadRequest, "url names no host"));
        }
        Some(Host::Domain(_)) => None,
        Some(Host::Ipv4(address)) => Some(IpAddr::V4(address)),
        Some(Host::Ipv6(address)) => Some(IpAddr::V6(address)),
    };
    if destinations == Destinations::Public && literal.is_some_and(|address| !is_public(address)) {
        return Err(Refused::new(
            ProxyFailure::Blocked,
            format!("{} is on the proxy's own network", host_of(url)),
        ));
    }
    Ok(())
}

/// `host[:port]` of `url`, as logs and messages name a destination.
fn host_of(url: &Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => String::new(),
    }
}

/// A failed exchange with the target, as the page is told.
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

/// Request headers that never reach the target: the page's own identity at
/// the host, connection management, and what the ingress added about the
/// caller.
fn withheld(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "accept-encoding",
        "connection",
        "content-length",
        "expect",
        "forwarded",
        "host",
        "keep-alive",
        "te",
        "trailer",
        "transfer-encoding",
        "true-client-ip",
        "upgrade",
        "via",
        "x-real-ip",
    ];
    NAMES.contains(&name)
        || name.starts_with("x-tilo-")
        || name.starts_with("proxy-")
        || name.starts_with("x-forwarded-")
        || name.starts_with("cf-")
}

/// The headers to send the target: the page's, without its credentials at
/// the host, with the ones the browser would not send restored from
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

/// Response headers the browser would apply to the host's origin, not to
/// the target's: they reach the app renamed under [`FORWARD_PREFIX`].
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

/// One line per relayed request: who asked, the destination's host (never
/// its path, query, cookies or body), the target's status and how many
/// redirects led there.
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

/// The target's `answer` from `url` as the page's response, its body
/// streamed through `relayed` (bounded, on the clock and on the budgets);
/// refused when it declares a body over the limit.
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

/// A relayed response body as it streams: bounded, and on the clock.
struct Relayed {
    answer: Option<reqwest::Response>,
    deadline: Instant,
    received: u64,
    limit: u64,
    proxy: FetchProxy,
    caller: Caller,
    rooms: Arc<[Room]>,
    /// A budget this response counts against was spent by its last chunk:
    /// it ends here, with an error if more was coming.
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
    /// The relay state until the first poll turns it into the stream.
    Pending(Option<Box<Relayed>>),
    Stream(Chunks),
}

/// The body of a proxy answer: the target's, streamed, or the proxy's own
/// failure text.
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
                            .received(relayed.caller, &relayed.rooms, chunk.len());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upstream_answer_is_logged_by_host_alone_and_failures_stand_out() {
        let url = Url::parse("https://inv.example.org:8443/api/v1/videos/abc?token=secret#x")
            .expect("a url");
        assert_eq!(
            host_of(&url),
            "inv.example.org:8443",
            "no path, query or fragment"
        );
        assert!(answer_is_notable(StatusCode::FORBIDDEN));
        assert!(answer_is_notable(StatusCode::BAD_GATEWAY));
        assert!(!answer_is_notable(StatusCode::OK));
        assert!(!answer_is_notable(StatusCode::PARTIAL_CONTENT));
    }

    #[test]
    fn private_and_reserved_addresses_are_not_public() {
        for address in [
            "0.0.0.0",
            "10.1.2.3",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "192.0.2.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "2002:a00:1::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "2001::1",
        ] {
            assert!(
                !is_public(address.parse().unwrap()),
                "{address} must not be public"
            );
        }
        for address in [
            "1.1.1.1",
            "93.184.216.34",
            "172.32.0.1",
            "100.128.0.1",
            "2606:4700:4700::1111",
            "::ffff:1.1.1.1",
            "64:ff9b::101:101",
        ] {
            assert!(is_public(address.parse().unwrap()), "{address} is public");
        }
    }

    #[test]
    fn the_page_credentials_and_caller_address_never_leave() {
        let mut incoming = HeaderMap::new();
        for (name, value) in [
            ("x-tilo-proxy", "1"),
            ("x-tilo-session", "secret"),
            ("cookie", "page=1"),
            ("origin", "https://tilo.example"),
            ("referer", "https://tilo.example/"),
            ("x-forwarded-for", "203.0.113.9"),
            ("cf-connecting-ip", "203.0.113.9"),
            ("sec-fetch-mode", "cors"),
            ("user-agent", "Browser"),
            ("accept", "application/json"),
            ("authorization", "Bearer app"),
            ("x-tilo-forward-user-agent", "App/1"),
            ("x-tilo-forward-cookie", "app=2"),
            ("x-tilo-forward-x-tilo-session", "smuggled"),
            ("x-tilo-forward-host", "internal"),
        ] {
            incoming.append(name, HeaderValue::from_static(value));
        }
        let sent = outgoing_headers(&incoming);
        assert_eq!(sent["user-agent"], "App/1");
        assert_eq!(sent.get_all("user-agent").iter().count(), 1);
        assert_eq!(sent["cookie"], "app=2");
        assert_eq!(sent["accept"], "application/json");
        assert_eq!(sent["authorization"], "Bearer app");
        for name in [
            "x-tilo-proxy",
            "x-tilo-session",
            "origin",
            "referer",
            "x-forwarded-for",
            "cf-connecting-ip",
            "sec-fetch-mode",
            "host",
        ] {
            assert!(!sent.contains_key(name), "{name} must not be sent");
        }
    }

    #[test]
    fn a_device_out_of_requests_waits_for_its_bucket() {
        let rate = Rate {
            burst: 2.0,
            per_minute: 60.0,
        };
        let mut buckets = Buckets::new(rate);
        let now = Instant::now();
        let device = Caller([1; 32]);
        assert!(buckets.take(&device, 1.0, now).is_ok());
        assert!(buckets.take(&device, 1.0, now).is_ok());
        assert_eq!(buckets.take(&device, 1.0, now), Err(Duration::from_secs(1)));
        assert!(buckets.take(&Caller([2; 32]), 1.0, now).is_ok());
        let later = now.checked_add(Duration::from_secs(1)).unwrap();
        assert!(buckets.take(&device, 1.0, later).is_ok());
    }
}

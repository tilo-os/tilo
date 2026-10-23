//! Binding an endpoint, dialing over the peer network, naming the route.

use std::time::Duration;

use anyhow::{Context as _, anyhow};
use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Relays {
    /// The public n0 relays, and n0's lookup to find a device by its id.
    Public,
    /// One relay of the person's own (or a test harness's), and no lookup:
    /// a device is found through it alone.
    Own(RelayUrl),
}

impl Relays {
    /// `url`, or [`crate::RELAY_ENV`], or the public relays.
    pub fn choose(url: Option<&str>) -> anyhow::Result<Self> {
        let configured = url
            .map(str::to_owned)
            .or_else(|| std::env::var(crate::RELAY_ENV).ok())
            .filter(|url| !url.trim().is_empty());
        match configured {
            None => Ok(Self::Public),
            Some(url) => Ok(Self::Own(
                url.trim()
                    .parse()
                    .with_context(|| format!("relay {url:?} is not a URL"))?,
            )),
        }
    }

    /// The configured relay, if it is not the public ones.
    pub fn own(&self) -> Option<&RelayUrl> {
        match self {
            Self::Public => None,
            Self::Own(url) => Some(url),
        }
    }
}

/// Bind an endpoint with `secret` that uses `relays`.
///
/// # Errors
/// When the endpoint cannot bind its sockets.
pub async fn bind(
    secret: iroh::SecretKey,
    relays: &Relays,
    alpns: Vec<Vec<u8>>,
) -> anyhow::Result<Endpoint> {
    let builder = match relays {
        Relays::Public => Endpoint::builder(presets::N0),
        Relays::Own(url) => Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Custom(RelayMap::from(url.clone()))),
    };
    builder
        .secret_key(secret)
        .alpns(alpns)
        .bind()
        .await
        .map_err(|error| anyhow!("bind the peer-network endpoint: {error}"))
}

/// Where to dial a device's admin endpoint: its id, and the relay it was
/// last known to use, so no lookup is needed to find it.
pub fn address(
    endpoint: EndpointId,
    relays: &Relays,
    home_relay: Option<&RelayUrl>,
) -> EndpointAddr {
    let mut addr = EndpointAddr::new(endpoint);
    if let Some(url) = relays.own().or(home_relay) {
        addr = addr.with_relay_url(url.clone());
    }
    addr
}

/// How long one dial attempt may take.
pub const ATTEMPT: Duration = Duration::from_secs(15);
/// Between attempts while the device is unreachable.
pub const REDIAL_AFTER: Duration = Duration::from_secs(2);

/// Dial `addr` until it answers or `budget` runs out.
pub async fn dial(
    endpoint: &Endpoint,
    addr: &EndpointAddr,
    budget: Duration,
) -> anyhow::Result<Connection> {
    let started = tokio::time::Instant::now();
    loop {
        let attempt = tokio::time::timeout(ATTEMPT, endpoint.connect(addr.clone(), crate::ALPN));
        let failure = match attempt.await {
            Ok(Ok(connection)) => return Ok(connection),
            Ok(Err(error)) => error.to_string(),
            Err(_) => format!("no answer within {}s", ATTEMPT.as_secs()),
        };
        if started.elapsed().saturating_add(REDIAL_AFTER) >= budget {
            return Err(anyhow!(failure));
        }
        tokio::time::sleep(REDIAL_AFTER).await;
    }
}

/// The path a connection sends on now, in words: `relay <url>` or
/// `direct <address>`.
pub fn route(connection: &Connection) -> Route {
    let paths = connection.paths();
    let selected = paths
        .iter()
        .find(iroh::endpoint::Path::is_selected)
        .or_else(|| paths.iter().next());
    match selected.map(|path| path.remote_addr().clone()) {
        Some(iroh::TransportAddr::Relay(url)) => Route::Relay(url.to_string()),
        Some(iroh::TransportAddr::Ip(addr)) => Route::Direct(addr.to_string()),
        Some(other) => Route::Direct(format!("{other:?}")),
        None => Route::Unknown,
    }
}

/// The route a session took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Relay(String),
    Direct(String),
    Unknown,
}

impl Route {
    /// `relay` or `direct`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Relay(_) => "relay",
            Self::Direct(_) => "direct",
            Self::Unknown => "unknown",
        }
    }

    /// The relay's URL or the peer's address.
    pub fn via(&self) -> &str {
        match self {
            Self::Relay(via) | Self::Direct(via) => via,
            Self::Unknown => "",
        }
    }
}

impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Relay(url) => write!(f, "relay {url}"),
            Self::Direct(addr) => write!(f, "direct {addr}"),
            Self::Unknown => f.write_str("an unknown path"),
        }
    }
}

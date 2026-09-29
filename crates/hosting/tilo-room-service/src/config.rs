//! Environment configuration; see README.md.
use anyhow::{Context, Result, ensure};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

pub struct Config {
    /// Plain HTTP room API and page assets (behind the ingress's TLS).
    pub api: SocketAddr,
    /// TLS front for `<room>.<domain>`, reached by SNI passthrough.
    pub tls: SocketAddr,
    pub domain: String,
    /// Port in room origins when the public TLS front is not on 443.
    pub public_port: Option<u16>,
    /// PEM chain and key for `*.<domain>`; a self-signed pair when absent.
    pub certificate: Option<(PathBuf, PathBuf)>,
    pub relay_binary: PathBuf,
    /// The browser runtime bundle and the join page, served under `/app/`.
    pub assets: PathBuf,
    pub state: PathBuf,
    pub max_rooms: usize,
    pub ttl: Duration,
    /// Guests a room admits besides the page that created it.
    pub max_guests: usize,
    pub port_base: u16,
    /// Page origins that create rooms and join them (the site), besides the
    /// API's own origin, which serves the join page.
    pub site_origins: Vec<String>,
    /// The API's public base URL, for join links and `/app/`.
    pub public_api: String,
    /// Rooms serve browsers on their own port (TCP and UDP, the relay's own
    /// certificate) instead of behind the TLS front, so the live plane
    /// (WebTransport) reaches them.
    pub direct_ports: bool,
    /// Local review in a plain browser: rooms serve pages on
    /// `http://localhost:<port + 64>` (loopback only), so no certificate
    /// exception or host mapping is needed; the live plane then runs over
    /// the HTTPS stream fallback unless WebTransport reaches the room's
    /// TLS port.
    pub local: bool,
}

/// Offset of a local room's plain HTTP port from its TLS port.
pub const LOCAL_PLAIN_OFFSET: u16 = 64;

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn parsed<T: std::str::FromStr>(name: &str, default: T) -> Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    var(name).map_or(Ok(default), |value| {
        value.parse().with_context(|| format!("{name} is invalid"))
    })
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let path =
            |name: &str, default: &str| PathBuf::from(var(name).unwrap_or_else(|| default.into()));
        let certificate = match (var("TILO_ROOMS_TLS_CERT"), var("TILO_ROOMS_TLS_KEY")) {
            (Some(cert), Some(key)) => Some((cert.into(), key.into())),
            (None, None) => None,
            _ => anyhow::bail!("TILO_ROOMS_TLS_CERT and TILO_ROOMS_TLS_KEY go together"),
        };
        let config = Self {
            api: parsed("TILO_ROOMS_API", SocketAddr::from(([0, 0, 0, 0], 8790)))?,
            tls: parsed("TILO_ROOMS_TLS", SocketAddr::from(([0, 0, 0, 0], 9443)))?,
            domain: var("TILO_ROOMS_DOMAIN").unwrap_or_else(|| "localhost".into()),
            public_port: var("TILO_ROOMS_PUBLIC_PORT")
                .map(|port| port.parse().context("TILO_ROOMS_PUBLIC_PORT is invalid"))
                .transpose()?,
            certificate,
            relay_binary: path("TILO_ROOMS_RELAY_BIN", "/usr/local/bin/tilo-relay"),
            assets: path("TILO_ROOMS_ASSETS", "/srv/tilo-room-assets"),
            state: path("TILO_ROOMS_STATE", "/run/tilo-rooms"),
            max_rooms: parsed("TILO_ROOMS_MAX", 8)?,
            ttl: Duration::from_secs(parsed("TILO_ROOMS_TTL_SECS", 1800)?),
            max_guests: parsed("TILO_ROOMS_MAX_GUESTS", 4)?,
            port_base: parsed("TILO_ROOMS_PORT_BASE", 9700)?,
            site_origins: var("TILO_ROOMS_SITE_ORIGINS")
                .unwrap_or_else(|| "http://localhost:4321".into())
                .split(',')
                .map(|origin| origin.trim().to_owned())
                .filter(|origin| !origin.is_empty())
                .collect(),
            public_api: var("TILO_ROOMS_PUBLIC_API")
                .unwrap_or_else(|| "http://localhost:8790".into())
                .trim_end_matches('/')
                .to_owned(),
            direct_ports: var("TILO_ROOMS_DIRECT_PORTS").is_some_and(|value| value == "1"),
            local: var("TILO_ROOMS_LOCAL").is_some_and(|value| value == "1"),
        };
        ensure!(
            !(config.local && config.direct_ports),
            "TILO_ROOMS_LOCAL and TILO_ROOMS_DIRECT_PORTS exclude each other"
        );
        ensure!(
            config
                .domain
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
                && !config.domain.is_empty(),
            "TILO_ROOMS_DOMAIN must be a lowercase DNS name"
        );
        ensure!(
            config.assets.is_absolute() && config.state.is_absolute(),
            "TILO_ROOMS_ASSETS and TILO_ROOMS_STATE must be absolute"
        );
        ensure!(
            (1..=64).contains(&config.max_rooms) && config.max_guests <= 16,
            "TILO_ROOMS_MAX must be 1-64 and TILO_ROOMS_MAX_GUESTS at most 16"
        );
        ensure!(
            (60..=24 * 3600).contains(&config.ttl.as_secs()),
            "TILO_ROOMS_TTL_SECS must be 60-86400"
        );
        ensure!(
            config
                .port_base
                .checked_add(2 * LOCAL_PLAIN_OFFSET)
                .is_some(),
            "TILO_ROOMS_PORT_BASE leaves no room for 128 ports"
        );
        Ok(config)
    }

    /// `https://<room>.<domain>[:port]`; with direct ports, the room's own
    /// `port`.
    pub fn origin(&self, room: &str, port: u16) -> String {
        if self.local {
            return format!(
                "http://localhost:{}",
                port.saturating_add(LOCAL_PLAIN_OFFSET)
            );
        }
        if self.direct_ports {
            return format!("https://{room}.{}:{port}", self.domain);
        }
        match self.public_port.filter(|port| *port != 443) {
            Some(port) => format!("https://{room}.{}:{port}", self.domain),
            None => format!("https://{room}.{}", self.domain),
        }
    }

    /// The API's own origin (scheme, host and port of `public_api`).
    pub fn api_origin(&self) -> String {
        let rest = self
            .public_api
            .split_once("://")
            .map_or("", |(_, rest)| rest);
        let scheme = self
            .public_api
            .split_once("://")
            .map_or("https", |(scheme, _)| scheme);
        format!("{scheme}://{}", rest.split('/').next().unwrap_or_default())
    }

    /// Whether a page at `origin` may call the API.
    pub fn allows(&self, origin: &str) -> bool {
        origin == self.api_origin()
            || self
                .site_origins
                .iter()
                .any(|allowed| allowed == origin || allowed == "*")
    }
}

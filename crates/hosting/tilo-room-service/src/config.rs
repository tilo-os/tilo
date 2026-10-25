//! Environment configuration; see README.md.
use anyhow::{Context, Result, ensure};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crate::limits::TrustedProxies;

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
    /// The browser runtime bundle, served under `/app/` and, as the web
    /// entry guest links name, `/open/`.
    pub assets: PathBuf,
    pub state: PathBuf,
    pub max_rooms: usize,
    pub ttl: Duration,
    /// Guests a room admits besides the page that created it.
    pub max_guests: usize,
    pub port_base: u16,
    /// Page origins that create rooms and join them (the site), besides the
    /// API's own origin, which serves the entry pages.
    pub site_origins: Vec<String>,
    /// The API's public base URL, for guest links and the bundle.
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
    /// Room relays serve their members' pages the fetch proxy
    /// (`TILO_ROOMS_FETCH_PROXY`, on unless `0`), so apps in the page read
    /// sites that send no CORS headers through the room's host.
    pub fetch_proxy: bool,
    /// One always-on public room instead of short-lived ones.
    pub persistent: Option<PersistentConfig>,
}

/// The settings of a persistent public room (`TILO_ROOMS_PERSISTENT`).
pub struct PersistentConfig {
    /// The room's name: its directory under `TILO_ROOMS_STATE`.
    pub name: String,
    /// The name people see for it (`TILO_ROOMS_NAME`); unset, the relay
    /// shows `name` with a capital first letter.
    pub shown_name: Option<String>,
    /// What the room is, as the site's directory of public workspaces says
    /// (`TILO_ROOMS_DESCRIPTION`); unset, none.
    pub description: String,
    /// The channels its Chat opens with while it has none
    /// (`TILO_ROOMS_CHAT_CHANNELS`); unset, `general`, since a chat without a
    /// channel has nowhere for a visitor's first message to go.
    pub chat_channels: Vec<String>,
    /// The channel its members report problems to, kept in its Chat on
    /// every start (`TILO_ROOMS_FEEDBACK_CHANNEL`); unset, none.
    pub feedback_channel: Option<String>,
    /// The strips, by name, the room's shared desktop has
    /// (`TILO_ROOMS_STRIPS`); unset, none beyond what people make.
    pub strips: Vec<String>,
    /// Texts published outside the room that get a discussion thread in it
    /// (`TILO_ROOMS_DISCUSSIONS`: semicolon-separated `<address>=<title>`),
    /// so a reader arriving from such a text finds the people discussing it.
    /// Unset, none. A text whose thread the room keeps is left as it is.
    pub discussions: Vec<String>,
    /// Proxies whose `X-Forwarded-For` names the visitor.
    pub trusted_proxies: TrustedProxies,
    /// The header those proxies name a visitor's country in
    /// (`TILO_ROOMS_COUNTRY_HEADER`, such as `CF-IPCountry`): the relay
    /// shows it to a person approving a new device. Unset, none is shown.
    pub country_header: Option<String>,
    /// User public keys (hex) made Owners of the room at start.
    pub owners: Vec<String>,
    /// User public keys (hex) made Moderators of the room at start.
    pub moderators: Vec<String>,
    /// A file of more owners and moderators (`TILO_ROOMS_ROLES_FILE`,
    /// `{"owners": [hex], "moderators": [hex]}`), read again whenever it
    /// changes: the operator grants or takes back a role while the room runs.
    pub roles_file: Option<std::path::PathBuf>,
    /// Bot directory entries, with their inputs, installed at first start.
    pub bots: Vec<String>,
    /// Apps authored into the room at first start.
    pub seed_apps: Vec<String>,
    /// Apps the room's members write through (`TILO_ROOMS_AUTHOR_APPS`,
    /// comma-separated): each authors the room's content, so a guest, an
    /// Editor, can talk in its Chat. Unset, every seeded app: they are what
    /// the room offers its visitors. `none`, none, and the room is
    /// read-only to its guests whatever their role.
    pub author_apps: Vec<String>,
    /// Whether the room shares its members' windows and strips
    /// (`TILO_ROOMS_LAYOUT`): `personal` unless set (everyone comes in
    /// through the site's door, so each member's windows stay on their
    /// device and the room shares its records and people), or `shared`.
    /// Settled at every start.
    pub layout: String,
    /// Who the room's door lets in (`TILO_ROOMS_ADMISSION`): anyone as a
    /// guest (`open`, unless set), or only its members (`invite`), so a
    /// newcomer needs an invitation from someone in it.
    pub admission: Admission,
    /// How many requests to be invited the door keeps while the room takes
    /// people by invitation (`TILO_ROOMS_WAIT_LIST_MAX`, 10000 unless set,
    /// `0` keeps none): someone without an invitation leaves an email
    /// address, and whoever may invite sees it with the room's join requests.
    pub wait_list_max: usize,
    /// Where someone on the wait list asks to be taken off it
    /// (`TILO_ROOMS_CONTACT`: an email address or an `https://` page), said
    /// under the door's email field; unset, the door names none.
    pub contact: Option<String>,
    /// Days a member may be away (no session) before their membership ends
    /// (`TILO_ROOMS_INACTIVE_AFTER_DAYS`, 30 unless set); one who comes back
    /// later joins through the door again as the same person. Membership
    /// persists otherwise: an hour offline removes nobody.
    pub inactive_after_days: u64,
    /// How much of the files members post in its Chat the room keeps once
    /// they left, in MiB: the largest file (`TILO_ROOMS_ATTACHMENT_MAX_MIB`,
    /// at most 64), one member's together (`TILO_ROOMS_ATTACHMENT_MEMBER_MIB`)
    /// and everyone's (`TILO_ROOMS_ATTACHMENT_ROOM_MIB`, `0` keeps none).
    /// Each unset leaves the relay's default: 25, 250 and 2048.
    pub attachments: AttachmentQuota,
    /// The room's owner root, mounted from the operator's encrypted secrets.
    pub root_file: PathBuf,
    /// The Discord bot token, when the bridge runs.
    pub discord_token_file: Option<PathBuf>,
    /// The model API key the room's moderation agent asks with, when it
    /// runs; mounted from the operator's encrypted secrets.
    pub moderation_key_file: Option<PathBuf>,
    /// The model the moderation agent asks, when not the default.
    pub moderation_model: Option<String>,
    /// Spectator sessions (the homepage embed) at once.
    pub max_spectators: usize,
    /// Guest joins one address may make (`TILO_ROOMS_GUEST_JOINS_PER_10_MIN`,
    /// `TILO_ROOMS_GUEST_JOINS_PER_DAY`). An office or a mobile carrier puts
    /// many visitors behind one address, and each one past the limit only
    /// watches until it frees up.
    pub guest_joins: crate::limits::JoinRates,
    /// Spectator certificates one address may take
    /// (`TILO_ROOMS_SPECTATES_PER_10_MIN`; no daily cap).
    pub spectator_joins: crate::limits::JoinRates,
    /// The relay's published origin when it is reached through the ingress
    /// (TLS ends there and forwards to `relay_plain`), so no public port or
    /// load balancer of its own is needed.
    pub relay_origin: Option<String>,
    /// The relay's plain HTTP listener that the ingress forwards to.
    pub relay_plain: SocketAddr,
    /// The Tilo app's web entry (`https://app.example/`) the relay announces,
    /// so `share-link` and bots mint links that open the room there.
    pub web_entry: Option<String>,
    /// WebTransport reached directly, beside an ingress that passes HTTP
    /// only.
    pub live: Option<LiveListener>,
}

/// The relay's own QUIC listener for WebTransport, published under a name
/// that bypasses the HTTP-only proxy in front of `relay_origin`. Browsers
/// try it first and fall back to the HTTPS stream through the ingress.
#[derive(Clone, Debug)]
pub struct LiveListener {
    /// The `https://` origin browsers dial, e.g. `https://live.rooms.example`.
    pub origin: String,
    /// The certificate's host name: `origin` without scheme and port.
    pub host: String,
    /// Where the relay listens (UDP for WebTransport; HTTPS on TCP too).
    pub listen: SocketAddr,
    /// PEM chain and key for `host`, re-read when renewed in place.
    pub certificate: (PathBuf, PathBuf),
}

/// Reads the direct WebTransport listener, which only makes sense beside an
/// ingress origin.
fn live_listener(
    var: impl Fn(&str) -> Option<String> + Copy,
    relay_origin: Option<&str>,
) -> Result<Option<LiveListener>> {
    let Some(origin) = var("TILO_ROOMS_LIVE_ORIGIN") else {
        return Ok(None);
    };
    let origin = origin.trim_end_matches('/').to_owned();
    let authority = origin.strip_prefix("https://").unwrap_or_default();
    let host = authority
        .rsplit_once(':')
        .filter(|(_, port)| port.parse::<u16>().is_ok())
        .map_or(authority, |(host, _)| host);
    ensure!(
        !host.is_empty() && !authority.contains('/'),
        "TILO_ROOMS_LIVE_ORIGIN must be an https:// origin like https://live.rooms.example"
    );
    ensure!(
        relay_origin.is_some(),
        "TILO_ROOMS_LIVE_ORIGIN goes with TILO_ROOMS_RELAY_ORIGIN: it bypasses that ingress"
    );
    let path =
        |name: &str, default: &str| PathBuf::from(var(name).unwrap_or_else(|| default.into()));
    Ok(Some(LiveListener {
        host: host.to_owned(),
        origin,
        listen: parsed(
            var,
            "TILO_ROOMS_LIVE_LISTEN",
            SocketAddr::from(([0, 0, 0, 0], 9781)),
        )?,
        certificate: (
            path("TILO_ROOMS_LIVE_TLS_CERT", "/run/live-tls/tls.crt"),
            path("TILO_ROOMS_LIVE_TLS_KEY", "/run/live-tls/tls.key"),
        ),
    }))
}

/// What a persistent room seeds when `TILO_ROOMS_SEED_APPS` is unset.
pub const DEFAULT_SEED_APPS: [&str; 3] = ["so.tilo.chat", "so.tilo.notes", "so.tilo.canvas"];

/// Offset of a local room's plain HTTP port from its TLS port.
pub const LOCAL_PLAIN_OFFSET: u16 = 64;

fn parsed<T: std::str::FromStr>(
    var: impl Fn(&str) -> Option<String>,
    name: &str,
    default: T,
) -> Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    var(name).map_or(Ok(default), |value| {
        value.parse().with_context(|| format!("{name} is invalid"))
    })
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads the configuration through `lookup`, which answers an
    /// environment variable's value; an empty value counts as unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let var = |name: &str| lookup(name).filter(|value| !value.is_empty());
        let persistent = persistent(&var)?;
        let path =
            |name: &str, default: &str| PathBuf::from(var(name).unwrap_or_else(|| default.into()));
        let certificate = match (var("TILO_ROOMS_TLS_CERT"), var("TILO_ROOMS_TLS_KEY")) {
            (Some(cert), Some(key)) => Some((cert.into(), key.into())),
            (None, None) => None,
            _ => anyhow::bail!("TILO_ROOMS_TLS_CERT and TILO_ROOMS_TLS_KEY go together"),
        };
        let config = Self {
            api: parsed(
                var,
                "TILO_ROOMS_API",
                SocketAddr::from(([0, 0, 0, 0], 8790)),
            )?,
            tls: parsed(
                var,
                "TILO_ROOMS_TLS",
                SocketAddr::from(([0, 0, 0, 0], 9443)),
            )?,
            domain: var("TILO_ROOMS_DOMAIN").unwrap_or_else(|| "localhost".into()),
            public_port: var("TILO_ROOMS_PUBLIC_PORT")
                .map(|port| port.parse().context("TILO_ROOMS_PUBLIC_PORT is invalid"))
                .transpose()?,
            certificate,
            relay_binary: path("TILO_ROOMS_RELAY_BIN", "/usr/local/bin/tilo-relay"),
            assets: path("TILO_ROOMS_ASSETS", "/srv/tilo-room-assets"),
            state: path("TILO_ROOMS_STATE", "/run/tilo-rooms"),
            max_rooms: parsed(var, "TILO_ROOMS_MAX", 8)?,
            ttl: Duration::from_secs(parsed(var, "TILO_ROOMS_TTL_SECS", 1800)?),
            max_guests: parsed(
                var,
                "TILO_ROOMS_MAX_GUESTS",
                if persistent.is_some() { 300 } else { 4 },
            )?,
            port_base: parsed(var, "TILO_ROOMS_PORT_BASE", 9700)?,
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
            fetch_proxy: match var("TILO_ROOMS_FETCH_PROXY").as_deref().map(str::trim) {
                None | Some("1") => true,
                Some("0") => false,
                Some(other) => anyhow::bail!("TILO_ROOMS_FETCH_PROXY must be 0 or 1, not {other}"),
            },
            persistent,
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
            (1..=64).contains(&config.max_rooms)
                && config.max_guests
                    <= if config.persistent.is_some() {
                        10_000
                    } else {
                        16
                    },
            "TILO_ROOMS_MAX must be 1-64 and TILO_ROOMS_MAX_GUESTS at most 16 (10000 for a persistent room)"
        );
        ensure!(
            config.persistent.as_ref().is_none_or(|persistent| {
                persistent.relay_origin.is_some() || config.direct_ports || config.local
            }),
            "a persistent room is reached through the ingress or on its own port: set TILO_ROOMS_RELAY_ORIGIN (or TILO_ROOMS_DIRECT_PORTS=1, or TILO_ROOMS_LOCAL=1)"
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

    /// The persistent room's origin, which browsers join: the ingress's
    /// relay host when set, else the relay itself on its port.
    pub fn persistent_origin(&self) -> String {
        if self.local {
            return format!(
                "http://localhost:{}",
                self.port_base.saturating_add(LOCAL_PLAIN_OFFSET)
            );
        }
        if let Some(origin) = self
            .persistent
            .as_ref()
            .and_then(|persistent| persistent.relay_origin.clone())
        {
            return origin;
        }
        format!("https://{}:{}", self.domain, self.port_base)
    }

    /// The pages allowed to frame this service's pages
    /// (`Content-Security-Policy: frame-ancestors`).
    pub fn frame_ancestors(&self) -> String {
        self.site_origins.join(" ")
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

/// The relay's public origin and the Tilo app's web entry its links open
/// in, each checked for shape.
fn public_origins(
    var: &impl Fn(&str) -> Option<String>,
) -> Result<(Option<String>, Option<String>)> {
    let relay_origin =
        var("TILO_ROOMS_RELAY_ORIGIN").map(|origin| origin.trim_end_matches('/').to_owned());
    if let Some(origin) = &relay_origin {
        let host = origin
            .strip_prefix("https://")
            .or_else(|| origin.strip_prefix("http://"))
            .unwrap_or_default();
        ensure!(
            !host.is_empty() && !host.contains('/'),
            "TILO_ROOMS_RELAY_ORIGIN must be an origin like https://rooms.example"
        );
    }
    let web_entry = var("TILO_ROOMS_WEB_ENTRY");
    if let Some(entry) = &web_entry {
        let loopback = ["http://localhost", "http://127.0.0.1", "http://[::1]"]
            .iter()
            .any(|prefix| entry.starts_with(prefix));
        ensure!(
            (entry.starts_with("https://") || loopback) && entry.ends_with('/'),
            "TILO_ROOMS_WEB_ENTRY must be an https URL ending in /, like https://app.example/"
        );
    }
    Ok((relay_origin, web_entry))
}

/// The persistent room's settings, when `TILO_ROOMS_PERSISTENT` names one.
fn persistent(var: &impl Fn(&str) -> Option<String>) -> Result<Option<PersistentConfig>> {
    let Some(name) = var("TILO_ROOMS_PERSISTENT") else {
        return Ok(None);
    };
    ensure!(
        name.len() <= 32
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !name.starts_with('-'),
        "TILO_ROOMS_PERSISTENT must be a short lowercase name"
    );
    let list = |name: &str, separator: char| -> Vec<String> {
        var(name)
            .unwrap_or_default()
            .split(separator)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let keys = |name: &str| -> Result<Vec<String>> {
        let keys = list(name, ',');
        ensure!(
            keys.iter()
                .all(|key| tilo_relay_protocol::room_operator::hex32(key).is_some()),
            "{name} must list 64-character lowercase hex user public keys"
        );
        Ok(keys)
    };
    let (relay_origin, web_entry) = public_origins(var)?;
    let live = live_listener(var, relay_origin.as_deref())?;
    let max_spectators = parsed(var, "TILO_ROOMS_MAX_SPECTATORS", 1000_usize)?;
    ensure!(
        (1..=100_000).contains(&max_spectators),
        "TILO_ROOMS_MAX_SPECTATORS must be 1-100000"
    );
    let (guest_joins, spectator_joins) = join_rates(var)?;
    let token = PathBuf::from("/run/secrets/discord-bot-token");
    let moderation_key = PathBuf::from("/run/secrets/moderation-model-key");
    let moderation_model = moderation_model(var)?;
    let seed_apps = if var("TILO_ROOMS_SEED_APPS").is_some() {
        list("TILO_ROOMS_SEED_APPS", ',')
    } else {
        DEFAULT_SEED_APPS
            .iter()
            .map(|app| (*app).to_owned())
            .collect()
    };
    Ok(Some(PersistentConfig {
        name,
        shown_name: shown_name(var)?,
        description: description(var)?,
        chat_channels: chat_channels(var)?,
        feedback_channel: feedback_channel(var)?,
        strips: strips(var)?,
        discussions: discussions(var)?,
        trusted_proxies: trusted_proxies(var)?,
        country_header: var("TILO_ROOMS_COUNTRY_HEADER")
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty()),
        owners: keys("TILO_ROOMS_OWNERS")?,
        moderators: keys("TILO_ROOMS_MODERATORS")?,
        roles_file: var("TILO_ROOMS_ROLES_FILE")
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from),
        bots: list("TILO_ROOMS_BOTS", ';'),
        author_apps: author_apps(var, &seed_apps)?,
        seed_apps,
        layout: layout(var)?,
        admission: admission(var)?,
        wait_list_max: wait_list_max(var)?,
        contact: contact(var)?,
        inactive_after_days: inactive_after_days(var)?,
        attachments: AttachmentQuota {
            max_file: optional(var, "TILO_ROOMS_ATTACHMENT_MAX_MIB")?,
            per_member: optional(var, "TILO_ROOMS_ATTACHMENT_MEMBER_MIB")?,
            per_room: optional(var, "TILO_ROOMS_ATTACHMENT_ROOM_MIB")?,
        },
        root_file: var("TILO_ROOMS_ROOT_FILE")
            .map_or_else(|| PathBuf::from("/run/secrets/room-root"), PathBuf::from),
        discord_token_file: var("TILO_ROOMS_DISCORD_TOKEN_FILE")
            .map(PathBuf::from)
            .or_else(|| token.is_file().then_some(token)),
        moderation_key_file: var("TILO_ROOMS_MODERATION_KEY_FILE")
            .map(PathBuf::from)
            .or_else(|| moderation_key.is_file().then_some(moderation_key)),
        moderation_model,
        max_spectators,
        guest_joins,
        spectator_joins,
        relay_origin,
        web_entry,
        relay_plain: parsed(
            var,
            "TILO_ROOMS_RELAY_PLAIN",
            SocketAddr::from(([0, 0, 0, 0], 9780)),
        )?,
        live,
    }))
}

/// `TILO_ROOMS_TRUSTED_PROXIES`: the proxies whose `X-Forwarded-For` is believed.
fn trusted_proxies(var: &impl Fn(&str) -> Option<String>) -> Result<TrustedProxies> {
    var("TILO_ROOMS_TRUSTED_PROXIES")
        .unwrap_or_default()
        .parse::<TrustedProxies>()
        .map_err(anyhow::Error::msg)
        .context("TILO_ROOMS_TRUSTED_PROXIES is invalid")
}

/// What the room keeps of the files members post, in MiB, as the operator
/// set it; `None` leaves the relay's default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttachmentQuota {
    /// The largest file kept.
    pub max_file: Option<u64>,
    pub per_member: Option<u64>,
    /// Everyone's files together.
    pub per_room: Option<u64>,
}

/// `name` as a whole number, when it is set.
fn optional(var: &impl Fn(&str) -> Option<String>, name: &str) -> Result<Option<u64>> {
    var(name)
        .map(|value| {
            value
                .trim()
                .parse()
                .with_context(|| format!("{name} is a whole number of MiB"))
        })
        .transpose()
}

/// Who a persistent room's door lets in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Admission {
    /// Anyone, as a guest: the door makes them a member.
    #[default]
    Open,
    /// Only the room's members, such as one opening another tab. Anyone
    /// else joins with an invitation someone in the room made.
    Invite,
}

/// `TILO_ROOMS_ADMISSION`: `open` unless set, or `invite`.
fn admission(var: &impl Fn(&str) -> Option<String>) -> Result<Admission> {
    match var("TILO_ROOMS_ADMISSION").as_deref().map(str::trim) {
        None | Some("open") => Ok(Admission::Open),
        Some("invite") => Ok(Admission::Invite),
        Some(other) => anyhow::bail!(
            "TILO_ROOMS_ADMISSION is open (anyone joins as a guest) or invite (only with an invitation), not {other:?}"
        ),
    }
}

/// The most requests to be invited `TILO_ROOMS_WAIT_LIST_MAX` may keep.
const MAX_WAIT_LIST: usize = 1_000_000;

/// `TILO_ROOMS_WAIT_LIST_MAX`: 10000 unless set, up to [`MAX_WAIT_LIST`].
fn wait_list_max(var: &impl Fn(&str) -> Option<String>) -> Result<usize> {
    let max = parsed(var, "TILO_ROOMS_WAIT_LIST_MAX", 10_000_usize)?;
    ensure!(
        max <= MAX_WAIT_LIST,
        "TILO_ROOMS_WAIT_LIST_MAX must be 0-{MAX_WAIT_LIST}"
    );
    Ok(max)
}

/// `TILO_ROOMS_CONTACT`: an email address or an `https://` page, if set.
fn contact(var: &impl Fn(&str) -> Option<String>) -> Result<Option<String>> {
    let Some(contact) = var("TILO_ROOMS_CONTACT").map(|value| value.trim().to_owned()) else {
        return Ok(None);
    };
    if contact.is_empty() {
        return Ok(None);
    }
    if let Some(page) = contact.strip_prefix("https://") {
        ensure!(
            !page.is_empty()
                && contact.len() <= 2048
                && !contact.chars().any(|c| c.is_whitespace() || c.is_control()),
            "TILO_ROOMS_CONTACT is an email address or an https:// page"
        );
        return Ok(Some(contact));
    }
    tilo_relay_protocol::rooms::contact_address(&contact)
        .map(Some)
        .map_err(|_| anyhow::anyhow!("TILO_ROOMS_CONTACT is an email address or an https:// page"))
}

/// `TILO_ROOMS_LAYOUT`: `personal` unless set, or `shared`.
fn layout(var: &impl Fn(&str) -> Option<String>) -> Result<String> {
    let Some(layout) = var("TILO_ROOMS_LAYOUT") else {
        return Ok("personal".into());
    };
    let layout = layout.trim();
    ensure!(
        matches!(layout, "personal" | "shared"),
        "TILO_ROOMS_LAYOUT is personal (each member's windows stay on their device) or shared, not {layout:?}"
    );
    Ok(layout.to_owned())
}

/// The longest `TILO_ROOMS_INACTIVE_AFTER_DAYS` takes: ten years.
const MAX_INACTIVE_AFTER_DAYS: u64 = 3650;

/// `TILO_ROOMS_INACTIVE_AFTER_DAYS`: days without a session after which a
/// member's membership ends, 30 unless set. The hour-long horizon the room
/// once removed guests after is gone: `TILO_ROOMS_PRUNE_AFTER_MINS` is
/// refused, so a deployment that still sets it learns that it no longer
/// does what it says.
fn inactive_after_days(var: &impl Fn(&str) -> Option<String>) -> Result<u64> {
    ensure!(
        var("TILO_ROOMS_PRUNE_AFTER_MINS").is_none(),
        "TILO_ROOMS_PRUNE_AFTER_MINS is gone: membership persists, and \
         TILO_ROOMS_INACTIVE_AFTER_DAYS (days, 30 unless set) is when a member away that long stops being one"
    );
    let days = parsed(var, "TILO_ROOMS_INACTIVE_AFTER_DAYS", 30_u64)?;
    ensure!(
        (1..=MAX_INACTIVE_AFTER_DAYS).contains(&days),
        "TILO_ROOMS_INACTIVE_AFTER_DAYS must be 1-{MAX_INACTIVE_AFTER_DAYS}"
    );
    Ok(days)
}

/// The per-address join limits: guests' and spectators'.
fn join_rates(
    var: &impl Fn(&str) -> Option<String>,
) -> Result<(crate::limits::JoinRates, crate::limits::JoinRates)> {
    let defaults = crate::limits::JoinRates::GUESTS;
    let guest_joins = crate::limits::JoinRates {
        per_ten_minutes: parsed(
            var,
            "TILO_ROOMS_GUEST_JOINS_PER_10_MIN",
            defaults.per_ten_minutes,
        )?,
        per_day: parsed(var, "TILO_ROOMS_GUEST_JOINS_PER_DAY", defaults.per_day)?,
    };
    ensure!(
        (1..=100_000).contains(&guest_joins.per_ten_minutes)
            && (guest_joins.per_ten_minutes..=1_000_000).contains(&guest_joins.per_day),
        "TILO_ROOMS_GUEST_JOINS_PER_10_MIN must be 1-100000 and TILO_ROOMS_GUEST_JOINS_PER_DAY at least that, up to 1000000"
    );
    let spectator_joins = crate::limits::JoinRates {
        per_ten_minutes: parsed(
            var,
            "TILO_ROOMS_SPECTATES_PER_10_MIN",
            crate::limits::JoinRates::SPECTATORS.per_ten_minutes,
        )?,
        ..crate::limits::JoinRates::SPECTATORS
    };
    ensure!(
        (1..=100_000).contains(&spectator_joins.per_ten_minutes),
        "TILO_ROOMS_SPECTATES_PER_10_MIN must be 1-100000"
    );
    Ok((guest_joins, spectator_joins))
}

/// The name people see for the persistent room, `TILO_ROOMS_NAME`, when set.
fn shown_name(var: &impl Fn(&str) -> Option<String>) -> Result<Option<String>> {
    let Some(name) = var("TILO_ROOMS_NAME") else {
        return Ok(None);
    };
    let name = name.trim().to_owned();
    ensure!(
        (1..=80).contains(&name.chars().count()) && !name.chars().any(char::is_control),
        "TILO_ROOMS_NAME must be 1-80 characters"
    );
    Ok(Some(name))
}

/// What the directory says the room is, `TILO_ROOMS_DESCRIPTION`: the text
/// a public workspace's owner writes, held to the same bounds.
fn description(var: &impl Fn(&str) -> Option<String>) -> Result<String> {
    let Some(text) = var("TILO_ROOMS_DESCRIPTION") else {
        return Ok(String::new());
    };
    tilo_relay_protocol::directory::Listing {
        description: text,
        default_role: tilo_relay_protocol::rooms::RoomRole::Editor,
    }
    .checked()
    .map(|listing| listing.description)
    .map_err(|error| anyhow::anyhow!("TILO_ROOMS_DESCRIPTION: {error}"))
}

/// `TILO_ROOMS_AUTHOR_APPS`: the apps the room's members write through,
/// comma-separated app ids. Unset, `seed_apps`: a public room's apps are
/// there for its visitors to use, and its Chat for them to talk in. `none`,
/// none (an empty variable counts as unset here, as everywhere).
fn author_apps(var: &impl Fn(&str) -> Option<String>, seed_apps: &[String]) -> Result<Vec<String>> {
    let Some(apps) = var("TILO_ROOMS_AUTHOR_APPS") else {
        return Ok(seed_apps.to_vec());
    };
    if apps.trim() == "none" {
        return Ok(Vec::new());
    }
    let apps: Vec<String> = apps
        .split(',')
        .map(str::trim)
        .filter(|app| !app.is_empty())
        .map(str::to_owned)
        .collect();
    for app in &apps {
        ensure!(
            app.len() <= 128
                && app.contains('.')
                && app.split('.').all(|part| {
                    !part.is_empty()
                        && part.bytes().all(|b| {
                            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                        })
                }),
            "TILO_ROOMS_AUTHOR_APPS names apps by id, like so.tilo.chat, not {app:?}"
        );
    }
    Ok(apps)
}

/// `TILO_ROOMS_FEEDBACK_CHANNEL`: one channel name, as in
/// `TILO_ROOMS_CHAT_CHANNELS`.
fn feedback_channel(var: &impl Fn(&str) -> Option<String>) -> Result<Option<String>> {
    let Some(name) = var("TILO_ROOMS_FEEDBACK_CHANNEL")
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
    else {
        return Ok(None);
    };
    ensure!(
        name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "TILO_ROOMS_FEEDBACK_CHANNEL names a channel of up to 64 lowercase letters, digits and -, not {name:?}"
    );
    Ok(Some(name))
}

/// `TILO_ROOMS_CHAT_CHANNELS`: comma-separated channel names, each
/// lowercase letters, digits and `-` (the relay derives each channel's id
/// from its name, so two names may not share one). Unset or empty, the
/// chat opens with `general`.
fn chat_channels(var: &impl Fn(&str) -> Option<String>) -> Result<Vec<String>> {
    let channels: Vec<String> = var("TILO_ROOMS_CHAT_CHANNELS")
        .filter(|names| names.split(',').any(|name| !name.trim().is_empty()))
        .unwrap_or_else(|| "general".to_owned())
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    for name in &channels {
        ensure!(
            name.len() <= 64
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "TILO_ROOMS_CHAT_CHANNELS names channels of up to 64 lowercase letters, digits and -, not {name:?}"
        );
    }
    let mut unique = channels.clone();
    unique.sort_unstable();
    unique.dedup();
    ensure!(
        unique.len() == channels.len(),
        "TILO_ROOMS_CHAT_CHANNELS names a channel twice"
    );
    Ok(channels)
}

/// `TILO_ROOMS_DISCUSSIONS`: semicolon-separated `<address>=<title>`, each
/// a published text's address and the title its thread is named. Checked as
/// the relay checks an announcement, so a mistyped one stops the start
/// rather than the room's threads.
fn discussions(var: &impl Fn(&str) -> Option<String>) -> Result<Vec<String>> {
    let announced: Vec<String> = var("TILO_ROOMS_DISCUSSIONS")
        .unwrap_or_default()
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect();
    for entry in &announced {
        let (address, title) = entry.split_once('=').with_context(|| {
            format!("TILO_ROOMS_DISCUSSIONS takes ADDRESS=TITLE, not {entry:?}")
        })?;
        ensure!(
            address.trim().starts_with("http"),
            "TILO_ROOMS_DISCUSSIONS names a web address, not {address:?}"
        );
        ensure!(
            !title.trim().is_empty(),
            "TILO_ROOMS_DISCUSSIONS names each thread's title"
        );
    }
    Ok(announced)
}

/// `TILO_ROOMS_STRIPS`: comma-separated strip names, each 1-48
/// characters, none twice.
fn strips(var: &impl Fn(&str) -> Option<String>) -> Result<Vec<String>> {
    let strips: Vec<String> = var("TILO_ROOMS_STRIPS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    for name in &strips {
        ensure!(
            name.chars().count() <= 48 && !name.chars().any(char::is_control),
            "TILO_ROOMS_STRIPS names strips of up to 48 characters, not {name:?}"
        );
    }
    let mut unique = strips.clone();
    unique.sort_unstable();
    unique.dedup();
    ensure!(
        unique.len() == strips.len(),
        "TILO_ROOMS_STRIPS names a strip twice"
    );
    Ok(strips)
}

/// `TILO_ROOMS_MODERATION_MODEL`, a model id.
fn moderation_model(var: &impl Fn(&str) -> Option<String>) -> Result<Option<String>> {
    let moderation_model = var("TILO_ROOMS_MODERATION_MODEL");
    ensure!(
        moderation_model.as_deref().is_none_or(|model| {
            !model.is_empty()
                && model.len() <= 128
                && model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':' | '@'))
        }),
        "TILO_ROOMS_MODERATION_MODEL must be a model id like claude-haiku-5-5"
    );
    Ok(moderation_model)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(vars: &[(&str, &str)]) -> Result<Config> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        Config::from_lookup(|name| vars.get(name).cloned())
    }

    #[test]
    fn persistent_mode_reads_its_settings_and_defaults_to_two_hundred_guests() {
        let owner = "ab".repeat(32);
        let config = config(&[
            ("TILO_ROOMS_PERSISTENT", "plaza"),
            ("TILO_ROOMS_RELAY_ORIGIN", "https://rooms.example/"),
            (
                "TILO_ROOMS_SITE_ORIGINS",
                "https://site.example,https://app.example",
            ),
            ("TILO_ROOMS_TRUSTED_PROXIES", "10.42.0.0/16"),
            ("TILO_ROOMS_OWNERS", &owner),
            ("TILO_ROOMS_SEED_APPS", "so.tilo.chat"),
            ("TILO_ROOMS_BOTS", "so.tilo.bots.discord-bridge guild=1"),
            ("TILO_ROOMS_ROOT_FILE", "/secrets/root"),
            // The room API and the relay share one origin; the ingress
            // sends `/peer/` to the relay.
            ("TILO_ROOMS_PUBLIC_API", "https://rooms.example"),
            ("TILO_ROOMS_WEB_ENTRY", "https://app.example/"),
        ])
        .unwrap();
        assert_eq!(config.api_origin(), config.persistent_origin());
        let persistent = config.persistent.as_ref().unwrap();
        assert_eq!(persistent.name, "plaza");
        assert_eq!(persistent.owners, vec![owner]);
        assert!(persistent.moderators.is_empty());
        assert_eq!(persistent.seed_apps, vec!["so.tilo.chat".to_owned()]);
        assert_eq!(
            persistent.bots,
            vec!["so.tilo.bots.discord-bridge guild=1".to_owned()]
        );
        assert_eq!(persistent.root_file, PathBuf::from("/secrets/root"));
        assert_eq!(
            persistent.web_entry.as_deref(),
            Some("https://app.example/")
        );
        assert_eq!(persistent.max_spectators, 1000);
        assert_eq!(persistent.guest_joins, crate::limits::JoinRates::GUESTS);
        assert_eq!(
            persistent.spectator_joins,
            crate::limits::JoinRates::SPECTATORS
        );
        assert_eq!(
            persistent.relay_plain,
            SocketAddr::from(([0, 0, 0, 0], 9780))
        );
        assert!(
            persistent
                .trusted_proxies
                .contains("10.42.3.4".parse().unwrap())
        );
        assert_eq!(config.max_guests, 300);
        assert_eq!(config.persistent_origin(), "https://rooms.example");
        assert_eq!(
            config.frame_ancestors(),
            "https://site.example https://app.example"
        );
        assert!(persistent.live.is_none());
    }

    #[test]
    fn webtransport_can_bypass_an_http_only_ingress_on_its_own_origin() {
        let base = [
            ("TILO_ROOMS_PERSISTENT", "plaza"),
            ("TILO_ROOMS_RELAY_ORIGIN", "https://rooms.example"),
        ];
        let with = |extra: &[(&str, &str)]| {
            config(&[base.as_slice(), extra].concat()).map(|c| c.persistent.unwrap().live)
        };
        let live = with(&[("TILO_ROOMS_LIVE_ORIGIN", "https://live.rooms.example:9781/")])
            .unwrap()
            .unwrap();
        assert_eq!(live.origin, "https://live.rooms.example:9781");
        assert_eq!(live.host, "live.rooms.example");
        assert_eq!(live.listen, SocketAddr::from(([0, 0, 0, 0], 9781)));
        assert_eq!(
            live.certificate,
            (
                PathBuf::from("/run/live-tls/tls.crt"),
                PathBuf::from("/run/live-tls/tls.key")
            )
        );
        let moved = with(&[
            ("TILO_ROOMS_LIVE_ORIGIN", "https://live.example"),
            ("TILO_ROOMS_LIVE_LISTEN", "0.0.0.0:443"),
            ("TILO_ROOMS_LIVE_TLS_CERT", "/c"),
            ("TILO_ROOMS_LIVE_TLS_KEY", "/k"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            (moved.host.as_str(), moved.listen.port()),
            ("live.example", 443)
        );
        assert_eq!(
            moved.certificate,
            (PathBuf::from("/c"), PathBuf::from("/k"))
        );
        assert!(with(&[("TILO_ROOMS_LIVE_ORIGIN", "http://live.example")]).is_err());
        assert!(with(&[("TILO_ROOMS_LIVE_ORIGIN", "https://live.example/moq")]).is_err());
        assert!(
            config(&[
                ("TILO_ROOMS_PERSISTENT", "plaza"),
                ("TILO_ROOMS_LOCAL", "1"),
                ("TILO_ROOMS_LIVE_ORIGIN", "https://live.example"),
            ])
            .is_err()
        );
    }

    #[test]
    fn members_of_a_public_room_write_through_its_chat_unless_the_host_says_otherwise() {
        let base = [
            ("TILO_ROOMS_PERSISTENT", "plaza"),
            ("TILO_ROOMS_LOCAL", "1"),
        ];
        let defaults = config(&base).unwrap().persistent.unwrap();
        assert!(
            defaults.author_apps.iter().any(|app| app == "so.tilo.chat"),
            "{:?}",
            defaults.author_apps
        );
        assert_eq!(defaults.author_apps, defaults.seed_apps);
        let chat_only = config(&[base[0], base[1], ("TILO_ROOMS_SEED_APPS", "so.tilo.chat")])
            .unwrap()
            .persistent
            .unwrap();
        assert_eq!(chat_only.author_apps, ["so.tilo.chat"]);
        let configured = config(&[
            base[0],
            base[1],
            ("TILO_ROOMS_AUTHOR_APPS", " so.tilo.chat ,so.tilo.canvas"),
        ])
        .unwrap()
        .persistent
        .unwrap();
        assert_eq!(configured.author_apps, ["so.tilo.chat", "so.tilo.canvas"]);
        let none = config(&[base[0], base[1], ("TILO_ROOMS_AUTHOR_APPS", "none")])
            .unwrap()
            .persistent
            .unwrap();
        assert!(none.author_apps.is_empty());
        for bad in ["Chat", "so..chat", "so.tilo.chat/x"] {
            assert!(
                config(&[base[0], base[1], ("TILO_ROOMS_AUTHOR_APPS", bad)]).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_persistent_room_has_chat_notes_and_canvas_by_default() {
        let base = [
            ("TILO_ROOMS_PERSISTENT", "plaza"),
            ("TILO_ROOMS_LOCAL", "1"),
        ];
        let defaults = config(&base).unwrap().persistent.unwrap();
        assert_eq!(defaults.seed_apps, DEFAULT_SEED_APPS);
        assert_eq!(defaults.shown_name, None);
        let named = config(&[base[0], base[1], ("TILO_ROOMS_NAME", " Town Square ")]).unwrap();
        assert_eq!(
            named.persistent.unwrap().shown_name.as_deref(),
            Some("Town Square")
        );
        assert_eq!(defaults.chat_channels, ["general"]);
        let channels = config(&[
            base[0],
            base[1],
            ("TILO_ROOMS_CHAT_CHANNELS", "general, help,,show-and-tell"),
        ])
        .unwrap();
        assert_eq!(
            channels.persistent.unwrap().chat_channels,
            ["general", "help", "show-and-tell"]
        );
        for bad in ["General", "a_b", "a b", "help,help"] {
            assert!(
                config(&[base[0], base[1], ("TILO_ROOMS_CHAT_CHANNELS", bad)]).is_err(),
                "{bad}"
            );
        }
        assert!(defaults.strips.is_empty());
        let strips = config(&[
            base[0],
            base[1],
            ("TILO_ROOMS_STRIPS", " Main, Games night,,Quiet "),
        ])
        .unwrap();
        assert_eq!(
            strips.persistent.unwrap().strips,
            ["Main", "Games night", "Quiet"]
        );
        let long = "x".repeat(49);
        for bad in ["Main,Main", long.as_str()] {
            assert!(
                config(&[base[0], base[1], ("TILO_ROOMS_STRIPS", bad)]).is_err(),
                "{bad}"
            );
        }
        assert!(defaults.discussions.is_empty());
        let discussions = config(&[
            base[0],
            base[1],
            (
                "TILO_ROOMS_DISCUSSIONS",
                " https://tilo.so/blog/a-post=A post ;; https://tilo.so/blog/two=Second ",
            ),
        ])
        .unwrap();
        assert_eq!(
            discussions.persistent.unwrap().discussions,
            [
                "https://tilo.so/blog/a-post=A post",
                "https://tilo.so/blog/two=Second"
            ]
        );
        for bad in [
            "https://tilo.so/blog/a-post",
            "=A post",
            "https://tilo.so/blog/a-post=",
            "tilo.so/blog/a-post=A post",
        ] {
            assert!(
                config(&[base[0], base[1], ("TILO_ROOMS_DISCUSSIONS", bad)]).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_site_rooms_members_keep_their_windows_unless_the_host_shares_them() {
        let base = [
            ("TILO_ROOMS_PERSISTENT", "plaza"),
            ("TILO_ROOMS_LOCAL", "1"),
        ];
        assert_eq!(
            config(&base).unwrap().persistent.unwrap().layout,
            "personal"
        );
        let shared = config(&[base[0], base[1], ("TILO_ROOMS_LAYOUT", "shared")]).unwrap();
        assert_eq!(shared.persistent.unwrap().layout, "shared");
        assert!(config(&[base[0], base[1], ("TILO_ROOMS_LAYOUT", "public")]).is_err());
    }

    #[test]
    fn membership_ends_after_thirty_days_away_unless_the_host_says_otherwise() {
        let base = [
            ("TILO_ROOMS_PERSISTENT", "plaza"),
            ("TILO_ROOMS_LOCAL", "1"),
        ];
        assert_eq!(
            config(&base)
                .unwrap()
                .persistent
                .unwrap()
                .inactive_after_days,
            30
        );
        let week = config(&[base[0], base[1], ("TILO_ROOMS_INACTIVE_AFTER_DAYS", "7")]).unwrap();
        assert_eq!(week.persistent.unwrap().inactive_after_days, 7);
        let past = (MAX_INACTIVE_AFTER_DAYS + 1).to_string();
        for bad in ["0", "-1", "soon", past.as_str()] {
            assert!(
                config(&[base[0], base[1], ("TILO_ROOMS_INACTIVE_AFTER_DAYS", bad)]).is_err(),
                "{bad}"
            );
        }
        // The hour-long rule is gone, not silently ignored.
        assert!(config(&[base[0], base[1], ("TILO_ROOMS_PRUNE_AFTER_MINS", "60")]).is_err());
    }

    #[test]
    fn the_door_keeps_ten_thousand_waiting_addresses_and_names_a_contact_only_when_told() {
        let base = [
            ("TILO_ROOMS_PERSISTENT", "plaza"),
            ("TILO_ROOMS_LOCAL", "1"),
        ];
        let plain = config(&base).unwrap().persistent.unwrap();
        assert_eq!((plain.wait_list_max, plain.contact), (10_000, None));
        let set = config(&[
            base[0],
            base[1],
            ("TILO_ROOMS_WAIT_LIST_MAX", "0"),
            ("TILO_ROOMS_CONTACT", " Hello@Example.org "),
        ])
        .unwrap()
        .persistent
        .unwrap();
        assert_eq!(set.wait_list_max, 0);
        assert_eq!(set.contact.as_deref(), Some("hello@example.org"));
        let page = config(&[
            base[0],
            base[1],
            ("TILO_ROOMS_CONTACT", "https://example.org/privacy"),
        ])
        .unwrap();
        assert_eq!(
            page.persistent.unwrap().contact.as_deref(),
            Some("https://example.org/privacy")
        );
        for (name, bad) in [
            ("TILO_ROOMS_WAIT_LIST_MAX", "1000001"),
            ("TILO_ROOMS_WAIT_LIST_MAX", "many"),
            ("TILO_ROOMS_CONTACT", "write to us"),
            ("TILO_ROOMS_CONTACT", "http://example.org"),
        ] {
            assert!(
                config(&[base[0], base[1], (name, bad)]).is_err(),
                "{name}={bad}"
            );
        }
    }

    #[test]
    fn a_persistent_room_on_its_own_port_or_locally_names_that_origin() {
        let direct = config(&[
            ("TILO_ROOMS_PERSISTENT", "plaza"),
            ("TILO_ROOMS_DOMAIN", "rooms.example"),
            ("TILO_ROOMS_DIRECT_PORTS", "1"),
        ])
        .unwrap();
        assert_eq!(direct.persistent_origin(), "https://rooms.example:9700");
        let local = config(&[
            ("TILO_ROOMS_PERSISTENT", "plaza"),
            ("TILO_ROOMS_LOCAL", "1"),
        ])
        .unwrap();
        assert_eq!(local.persistent_origin(), "http://localhost:9764");
    }

    #[test]
    fn short_rooms_keep_their_limits_and_bad_values_are_refused() {
        let short = config(&[]).unwrap();
        assert_eq!(short.max_guests, 4);
        assert!(short.persistent.is_none());
        assert!(config(&[("TILO_ROOMS_MAX_GUESTS", "17")]).is_err());
        let ingress = ("TILO_ROOMS_RELAY_ORIGIN", "https://rooms.example");
        let plaza = ("TILO_ROOMS_PERSISTENT", "plaza");
        assert!(config(&[plaza, ingress, ("TILO_ROOMS_MAX_GUESTS", "10000")]).is_ok());
        assert!(config(&[plaza, ingress, ("TILO_ROOMS_MAX_GUESTS", "10001")]).is_err());
        assert!(config(&[plaza, ingress, ("TILO_ROOMS_OWNERS", "nothex")]).is_err());
        let moderated = config(&[
            plaza,
            ingress,
            ("TILO_ROOMS_MODERATION_KEY_FILE", "/secrets/model"),
            ("TILO_ROOMS_MODERATION_MODEL", "claude-sonnet-5-5"),
        ])
        .unwrap();
        let moderated = moderated.persistent.unwrap();
        assert_eq!(
            moderated.moderation_key_file,
            Some(PathBuf::from("/secrets/model"))
        );
        assert_eq!(
            moderated.moderation_model.as_deref(),
            Some("claude-sonnet-5-5")
        );
        assert!(config(&[plaza, ingress, ("TILO_ROOMS_MODERATION_MODEL", "a b")]).is_err());
        assert!(config(&[plaza, ingress, ("TILO_ROOMS_NAME", " ")]).is_err());
        assert!(config(&[("TILO_ROOMS_PERSISTENT", "../x"), ingress]).is_err());
        assert!(
            config(&[
                plaza,
                ingress,
                ("TILO_ROOMS_TRUSTED_PROXIES", "10.0.0.0/33")
            ])
            .is_err()
        );
        assert!(config(&[plaza, ingress, ("TILO_ROOMS_MAX_SPECTATORS", "0")]).is_err());
        let office = config(&[
            plaza,
            ingress,
            ("TILO_ROOMS_GUEST_JOINS_PER_10_MIN", "200"),
            ("TILO_ROOMS_GUEST_JOINS_PER_DAY", "2000"),
            ("TILO_ROOMS_SPECTATES_PER_10_MIN", "500"),
        ])
        .unwrap();
        let office = office.persistent.unwrap();
        assert_eq!(
            office.guest_joins,
            crate::limits::JoinRates {
                per_ten_minutes: 200,
                per_day: 2000
            }
        );
        assert_eq!(office.spectator_joins.per_ten_minutes, 500);
        assert_eq!(office.spectator_joins.per_day, usize::MAX);
        assert!(config(&[plaza, ingress, ("TILO_ROOMS_GUEST_JOINS_PER_10_MIN", "0")]).is_err());
        assert!(
            config(&[
                plaza,
                ingress,
                ("TILO_ROOMS_GUEST_JOINS_PER_10_MIN", "50"),
                ("TILO_ROOMS_GUEST_JOINS_PER_DAY", "10")
            ])
            .is_err(),
            "a day allows at least what ten minutes do"
        );
        assert!(config(&[plaza, ingress, ("TILO_ROOMS_SPECTATES_PER_10_MIN", "0")]).is_err());
        assert!(config(&[plaza, ("TILO_ROOMS_RELAY_ORIGIN", "https://a/b")]).is_err());
        assert!(
            config(&[
                plaza,
                ingress,
                ("TILO_ROOMS_WEB_ENTRY", "https://app.example")
            ])
            .is_err()
        );
        assert!(
            config(&[
                plaza,
                ingress,
                ("TILO_ROOMS_WEB_ENTRY", "http://app.example/")
            ])
            .is_err()
        );
        // A persistent room is reached through the ingress, directly or
        // locally, never through the short rooms' TLS front.
        assert!(config(&[plaza]).is_err());
    }
}

//! One address for everything a person can open.
//!
//! A link is `entry ["app/"] ["#" route]`. The path names only where the page
//! is served and whether it shows one app; the fragment names what to open.
//! The fragment never reaches a server, so neither the address nor the
//! secrets beside it land in access logs. This module is the only parser and
//! formatter for the grammar. The page reaches it through the runtime's wasm
//! exports instead of keeping a second implementation that could drift.
//! See `docs/architecture/web-routing.md`.

mod encode;
mod entry;
mod error;
mod json;
mod parse;

use std::fmt;

use uuid::Uuid;

pub use entry::{WebEntries, WebEntry};
pub use error::LinkError;
pub use json::{ModeJson, ResourceJson, RouteJson, ScopeJson, SpanJson, SpanUnitJson, TargetJson};

use crate::{ResourceRef, Span, SpanUnit};

/// The launcher's `--open-uri` bound, so a link that opens on the web also
/// opens natively.
pub const MAX_ROUTE_BYTES: usize = 8 * 1024;
pub const MAX_RECORD_ID_BYTES: usize = 1024;
pub const MAX_RESOURCE_ID_BYTES: usize = 2048;
pub const MAX_KIND_BYTES: usize = 128;
pub const MAX_VIEW_ID_BYTES: usize = 128;
pub const MAX_SECRET_BYTES: usize = 4096;
/// JavaScript's exact integer range: the page reads offsets as numbers.
pub const MAX_SPAN_OFFSET: u64 = (1 << 53) - 1;
const MAX_APP_ID_BYTES: usize = 255;
/// The store's spelling of a package's primary record, written `-` in links.
pub(crate) const PACKAGE_PRIMARY: &str = "@tilo:package-primary";

/// A package id as text (`so.tilo.chat`). Always the full id: no app gets a
/// shorter spelling, and the dot keeps it apart from the scope tokens.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AppId(String);

impl AppId {
    /// # Errors
    ///
    /// [`LinkError::BadApp`] unless the id is dot-separated, non-empty parts
    /// of ASCII letters, digits, `-` and `_`.
    pub fn new(id: impl Into<String>) -> Result<Self, LinkError> {
        let id = id.into();
        let parts_ok = id.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        });
        if parts_ok && id.contains('.') && id.len() <= MAX_APP_ID_BYTES {
            Ok(Self(id))
        } else {
            Err(LinkError::BadApp)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The record part: the package's primary record, or a prefixed record id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordSegment {
    Primary,
    Id(String),
}

impl RecordSegment {
    /// A record id as the store spells it. The package primary and the wire's
    /// empty record both become [`Self::Primary`], so it has one spelling.
    ///
    /// # Errors
    ///
    /// [`LinkError::BadRecord`] unless the id has a non-empty prefix ending in
    /// `:` and fits the store's bound.
    pub fn new(id: &str) -> Result<Self, LinkError> {
        if id.is_empty() || id == PACKAGE_PRIMARY {
            return Ok(Self::Primary);
        }
        let prefixed = id
            .split_once(':')
            .is_some_and(|(prefix, _)| !prefix.is_empty());
        if prefixed && id.len() <= MAX_RECORD_ID_BYTES && !id.contains('\0') {
            Ok(Self::Id(id.to_owned()))
        } else {
            Err(LinkError::BadRecord)
        }
    }

    /// The record part of a `record:<app>:<record>` provider: empty for the primary.
    #[must_use]
    pub fn provider_part(&self) -> &str {
        match self {
            Self::Primary => "",
            Self::Id(id) => id,
        }
    }
}

/// The origin of a room's host: `https://<authority>`, or `http://` for this
/// computer's loopback names, where local review relays listen. A loopback
/// host served over TLS (a test relay with its own certificate) is written
/// `https:<authority>`, so a link never changes the scheme it is reached by.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HostOrigin {
    origin: String,
    authority_at: usize,
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

fn split_authority(authority: &str) -> Result<(&str, Option<&str>), LinkError> {
    let (host, port) = match authority.strip_prefix("[::1]") {
        Some("") => ("[::1]", None),
        Some(rest) => (
            "[::1]",
            Some(rest.strip_prefix(':').ok_or(LinkError::BadHost)?),
        ),
        None => match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    if let Some(port) = port {
        let canonical = port
            .parse::<u16>()
            .ok()
            .filter(|number| *number != 0)
            .is_some_and(|number| number.to_string() == port);
        if !canonical {
            return Err(LinkError::BadHost);
        }
    }
    let name_ok = host == "[::1]"
        || (!host.is_empty()
            && host.len() <= 253
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            }));
    if name_ok {
        Ok((host, port))
    } else {
        Err(LinkError::BadHost)
    }
}

impl HostOrigin {
    /// The host as written after `/@` in a route.
    ///
    /// # Errors
    ///
    /// [`LinkError::BadHost`] for anything but a lowercase name with an
    /// optional port; [`LinkError::NonCanonical`] for an explicit default port.
    pub fn from_authority(written: &str) -> Result<Self, LinkError> {
        let (tls, authority) = match written.strip_prefix("https:") {
            Some(authority) => (true, authority),
            None => (false, written),
        };
        let (host, port) = split_authority(authority)?;
        let loopback = is_loopback(host);
        if tls && !loopback {
            return Err(LinkError::NonCanonical(
                "only this computer's hosts name their https",
            ));
        }
        let https = tls || !loopback;
        if port == Some(if https { "443" } else { "80" }) {
            return Err(LinkError::NonCanonical("leave out the default port"));
        }
        let scheme = if https { "https://" } else { "http://" };
        Ok(Self {
            origin: format!("{scheme}{authority}"),
            authority_at: scheme.len(),
        })
    }

    /// An origin as a locator announces it (`https://quinten.tilo.cloud`).
    ///
    /// # Errors
    ///
    /// Refuses plain http beyond this computer, paths, and default ports.
    pub fn new(origin: &str) -> Result<Self, LinkError> {
        if let Some(authority) = origin.strip_prefix("https://") {
            let (host, _) = split_authority(authority)?;
            if is_loopback(host) {
                Self::from_authority(&format!("https:{authority}"))
            } else {
                Self::from_authority(authority)
            }
        } else if let Some(authority) = origin.strip_prefix("http://") {
            let host = Self::from_authority(authority)?;
            if host.origin.starts_with("https://") {
                return Err(LinkError::BadEntry("plain http is only for this computer"));
            }
            Ok(host)
        } else {
            Err(LinkError::BadHost)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.origin
    }

    /// The host and port, without the scheme.
    #[must_use]
    pub fn authority(&self) -> &str {
        self.origin.get(self.authority_at..).unwrap_or("")
    }

    /// The part written after `/@`: the authority, prefixed `https:` for a
    /// loopback host reached over TLS.
    #[must_use]
    pub fn route(&self) -> String {
        let authority = self.authority();
        match split_authority(authority) {
            Ok((host, _)) if is_loopback(host) && self.origin.starts_with("https://") => {
                format!("https:{authority}")
            }
            _ => authority.to_owned(),
        }
    }
}

/// A workspace name for people to read in the link. Nothing resolves,
/// compares or routes on it; the page rewrites it once the room is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slug(String);

impl Slug {
    pub const MAX_LEN: usize = 48;

    /// # Errors
    ///
    /// [`LinkError::BadSlug`] unless 1–48 of `a-z`, `0-9` and `-`.
    pub fn parse(slug: &str) -> Result<Self, LinkError> {
        let ok = (1..=Self::MAX_LEN).contains(&slug.len())
            && slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if ok {
            Ok(Self(slug.to_owned()))
        } else {
            Err(LinkError::BadSlug)
        }
    }

    /// The slug for a workspace name: lowercase ASCII letters and digits,
    /// each run of anything else one `-`, trimmed. `None` when nothing is left.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let mut out = String::new();
        let mut gap = false;
        for c in name.chars() {
            if !c.is_ascii_alphanumeric() {
                gap = true;
                continue;
            }
            let dash = gap && !out.is_empty();
            if out.len().saturating_add(if dash { 2 } else { 1 }) > Self::MAX_LEN {
                break;
            }
            if dash {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            gap = false;
        }
        (!out.is_empty()).then_some(Self(out))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which room a link opens in.
#[derive(Clone, Debug, Eq)]
pub enum Scope {
    /// The person's own room (`/me`).
    Own,
    /// A workspace room on `host`.
    Room {
        host: HostOrigin,
        room: Uuid,
        slug: Option<Slug>,
    },
}

impl PartialEq for Scope {
    /// The slug is presentation: links that differ only in it are one link.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Own, Self::Own) => true,
            (
                Self::Room {
                    host: a, room: x, ..
                },
                Self::Room {
                    host: b, room: y, ..
                },
            ) => a == b && x == y,
            _ => false,
        }
    }
}

/// A [`ResourceRef`] without its label: what a link may carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceAddress {
    pub kind: String,
    pub id: String,
    pub span: Option<Span>,
}

impl ResourceAddress {
    #[must_use]
    pub fn from_ref(reference: &ResourceRef) -> Self {
        Self {
            kind: reference.kind.clone(),
            id: reference.id.clone(),
            span: reference.span,
        }
    }

    /// The reference with `label`, which comes from the live publisher.
    #[must_use]
    pub fn into_ref(self, label: String) -> ResourceRef {
        ResourceRef {
            kind: self.kind,
            id: self.id,
            label,
            span: self.span,
        }
    }

    /// Whether the id spells its own kind (`chat.channel:general`), so the
    /// link carries it as one segment.
    pub(crate) fn self_describing(&self) -> bool {
        !self.kind.contains(':')
            && self
                .id
                .strip_prefix(self.kind.as_str())
                .is_some_and(|rest| rest.starts_with(':'))
    }
}

/// Which mount presents the target: the desktop, or one app full tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Desktop,
    App,
}

/// A credential carried in the fragment. `Debug` and `Display` redact it,
/// so a stray `{:?}` in a log cannot leak it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// # Errors
    ///
    /// [`LinkError::BadSecret`] unless 1–4096 bytes of base64url.
    pub fn new(value: impl Into<String>) -> Result<Self, LinkError> {
        let value = value.into();
        let ok = (1..=MAX_SECRET_BYTES).contains(&value.len())
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if ok {
            Ok(Self(value))
        } else {
            Err(LinkError::BadSecret)
        }
    }

    /// The value, for the one consumer that redeems it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(‹redacted›)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("‹redacted›")
    }
}

/// What admits someone who is not yet a member. Only ever in the fragment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkSecrets {
    pub invite: Option<Secret>,
    pub ticket: Option<Secret>,
    pub token: Option<Secret>,
    /// The person (their user root, hex) whose new device this browser asks
    /// to become: one of their devices that holds the root approves it.
    pub pair: Option<Secret>,
    /// A machine waiting for its owner (its HTTPS origin, device key and
    /// install token; `web/machine-setup.mjs`): the root this browser holds
    /// certifies it and the page installs the certificate, once the person
    /// confirms.
    pub install: Option<Secret>,
}

impl LinkSecrets {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.invite.is_none()
            && self.ticket.is_none()
            && self.token.is_none()
            && self.pair.is_none()
            && self.install.is_none()
    }
}

/// Where a link points. Rendering it is the only way to make a link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkTarget {
    pub scope: Scope,
    pub app: Option<AppId>,
    pub record: Option<RecordSegment>,
    pub resource: Option<ResourceAddress>,
    /// A `ResourceViewSpec::id`; `None` is the primary view.
    pub view: Option<String>,
    /// The published text a reader arrived from (`?discuss=<slug>`), which
    /// the room keeps a discussion thread for. A link carrying it and naming
    /// no place of its own opens that thread (see `discuss_thread` in the
    /// strict reader); beside an app, record or item it only says where the
    /// reader came from, and the link points where it was written.
    pub discuss: Option<Slug>,
}

impl LinkTarget {
    #[must_use]
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            app: None,
            record: None,
            resource: None,
            view: None,
            discuss: None,
        }
    }

    /// The app and record of a `record:<app>:<record>` provider.
    ///
    /// # Errors
    ///
    /// [`LinkError::BadProvider`] for any other shape.
    pub fn from_provider(provider: &str) -> Result<(AppId, RecordSegment), LinkError> {
        let rest = provider
            .strip_prefix("record:")
            .ok_or(LinkError::BadProvider)?;
        let (app, record) = rest.split_once(':').ok_or(LinkError::BadProvider)?;
        Ok((AppId::new(app)?, RecordSegment::new(record)?))
    }

    /// The structural rules a target must meet to be written.
    ///
    /// # Errors
    ///
    /// The first rule broken.
    pub fn check(&self) -> Result<(), LinkError> {
        if self.record.is_some() && self.app.is_none() {
            return Err(LinkError::Incomplete("it names a document but no app"));
        }
        if self.resource.is_some() && self.record.is_none() {
            return Err(LinkError::Incomplete("it names an item but no document"));
        }
        if self.view.is_some() && self.record.is_none() {
            return Err(LinkError::Incomplete("it names a view but no document"));
        }
        if let Some(RecordSegment::Id(id)) = &self.record
            && RecordSegment::new(id)? != RecordSegment::Id(id.clone())
        {
            return Err(LinkError::NonCanonical("write the primary record as -"));
        }
        if let Some(view) = &self.view
            && (view.is_empty() || view.len() > MAX_VIEW_ID_BYTES)
        {
            return Err(LinkError::BadView);
        }
        if let Some(resource) = &self.resource {
            let kind_ok = (1..=MAX_KIND_BYTES).contains(&resource.kind.len())
                && !resource.kind.contains('\0');
            let id_ok = (1..=MAX_RESOURCE_ID_BYTES).contains(&resource.id.len())
                && !resource.id.contains('\0');
            if !kind_ok || !id_ok {
                return Err(LinkError::BadResource);
            }
            if let Some(span) = resource.span {
                let end = u64::try_from(span.end).map_err(|_| LinkError::BadSpan)?;
                if span.start > span.end || end > MAX_SPAN_OFFSET {
                    return Err(LinkError::BadSpan);
                }
            }
        }
        Ok(())
    }

    fn write_scope(&self, omit_host: Option<&HostOrigin>, out: &mut String) {
        match &self.scope {
            Scope::Own => out.push_str("/me"),
            Scope::Room { host, room, slug } => {
                if omit_host != Some(host) {
                    out.push_str("/@");
                    out.push_str(&host.route());
                }
                out.push_str("/w/");
                if let Some(slug) = slug {
                    out.push_str(slug.as_str());
                    out.push('~');
                }
                out.push_str(&room.hyphenated().to_string());
            }
        }
    }

    fn write_rest(&self, secrets: &LinkSecrets, out: &mut String) {
        if let Some(app) = &self.app {
            out.push('/');
            encode::push_segment(out, app.as_str());
        }
        match &self.record {
            Some(RecordSegment::Primary) => out.push_str("/-"),
            Some(RecordSegment::Id(id)) => {
                out.push('/');
                encode::push_segment(out, id);
            }
            None => {}
        }
        if let Some(resource) = &self.resource {
            out.push('/');
            if !resource.self_describing() {
                encode::push_segment(out, &resource.kind);
                out.push('/');
            }
            encode::push_segment(out, &resource.id);
        }
        let mut params: Vec<String> = Vec::new();
        if let Some(discuss) = &self.discuss {
            params.push(format!("discuss={}", discuss.as_str()));
        }
        if let Some(view) = &self.view {
            params.push(format!("view={}", encode::segment(view)));
        }
        if let Some(span) = self.resource.as_ref().and_then(|resource| resource.span) {
            let unit = match span.unit {
                SpanUnit::Chars => "chars",
                SpanUnit::Bytes => "bytes",
            };
            params.push(format!("{unit}={}-{}", span.start, span.end));
        }
        for (name, secret) in [
            ("invite", &secrets.invite),
            ("ticket", &secrets.ticket),
            ("token", &secrets.token),
            ("pair", &secrets.pair),
            ("install", &secrets.install),
        ] {
            if let Some(secret) = secret {
                params.push(format!("{name}={}", secret.expose()));
            }
        }
        if !params.is_empty() {
            out.push('?');
            out.push_str(&params.join("&"));
        }
    }
}

/// A target, the mount that presents it, and what admits a newcomer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub mode: Mode,
    pub target: LinkTarget,
    pub secrets: LinkSecrets,
}

impl Link {
    /// The link under `entry`.
    ///
    /// # Errors
    ///
    /// [`LinkError::TooLong`] past [`MAX_ROUTE_BYTES`], or a structural rule.
    pub fn to_url(&self, entry: &WebEntry) -> Result<String, LinkError> {
        self.target.check()?;
        let mut route = String::new();
        self.target.write_scope(Some(entry.origin()), &mut route);
        self.target.write_rest(&self.secrets, &mut route);
        if route.len() > MAX_ROUTE_BYTES {
            return Err(LinkError::TooLong);
        }
        let mut url = entry.base().to_owned();
        if self.mode == Mode::App {
            url.push_str("app/");
        }
        if !route.is_empty() {
            url.push('#');
            url.push_str(&route);
        }
        Ok(url)
    }

    /// `tilo://open#<route>` for an installed Tilo. The scope is always
    /// explicit, since no entry supplies one.
    ///
    /// # Errors
    ///
    /// As [`Self::to_url`].
    pub fn to_native(&self) -> Result<String, LinkError> {
        self.target.check()?;
        let mut route = String::new();
        self.target.write_scope(None, &mut route);
        self.target.write_rest(&self.secrets, &mut route);
        if route.len() > MAX_ROUTE_BYTES {
            return Err(LinkError::TooLong);
        }
        Ok(format!("tilo://open#{route}"))
    }

    /// The same link without secrets: what history and logs may hold.
    #[must_use]
    pub fn public(&self) -> Self {
        Self {
            mode: self.mode,
            target: self.target.clone(),
            secrets: LinkSecrets::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90";

    fn tilo() -> WebEntry {
        WebEntry::new("https://tilo.so/").unwrap()
    }

    fn room(host: &str) -> Scope {
        Scope::Room {
            host: HostOrigin::new(host).unwrap(),
            room: ROOM.parse().unwrap(),
            slug: None,
        }
    }

    fn plain(mode: Mode, target: LinkTarget) -> Link {
        Link {
            mode,
            target,
            secrets: LinkSecrets::default(),
        }
    }

    #[test]
    fn segments_encode_canonically() {
        let mut out = String::new();
        encode::push_segment(&mut out, "a/b c?d#é~:@");
        assert_eq!(out, "a%2Fb%20c%3Fd%23%C3%A9~:@");
        assert_eq!(encode::decode_segment(&out).unwrap(), "a/b c?d#é~:@");
        assert!(
            encode::decode_segment("%41").is_err(),
            "an unreserved byte must stay raw"
        );
        assert!(encode::decode_segment("%2f").is_err(), "hex is uppercase");
        assert!(encode::decode_segment("%C3").is_err(), "invalid UTF-8");
        assert!(encode::decode_segment("a b").is_err(), "a raw space");
    }

    #[test]
    fn slugs_derive_from_names_and_stay_short() {
        assert_eq!(
            Slug::from_name("Reading Group").unwrap().as_str(),
            "reading-group"
        );
        assert_eq!(
            Slug::from_name("  --Q3 / plans!! ").unwrap().as_str(),
            "q3-plans"
        );
        assert!(Slug::from_name("日本").is_none());
        let long = Slug::from_name(&"ab ".repeat(40)).unwrap();
        assert!(long.as_str().len() <= 48 && !long.as_str().ends_with('-'));
        assert!(Slug::parse("Reading").is_err());
        assert!(Slug::parse("").is_err());
    }

    #[test]
    fn ids_have_one_spelling() {
        assert!(AppId::new("so.tilo.chat").is_ok());
        assert!(AppId::new("chat").is_err(), "an app id always has a dot");
        assert!(AppId::new("so..chat").is_err());
        assert_eq!(
            RecordSegment::new("@tilo:package-primary").unwrap(),
            RecordSegment::Primary
        );
        assert_eq!(RecordSegment::new("").unwrap(), RecordSegment::Primary);
        assert!(RecordSegment::new("no-prefix").is_err());
        assert!(RecordSegment::new(":x").is_err());
        assert_eq!(
            LinkTarget::from_provider("record:so.tilo.chat:").unwrap(),
            (AppId::new("so.tilo.chat").unwrap(), RecordSegment::Primary)
        );
        assert_eq!(
            LinkTarget::from_provider("record:so.tilo.notes:uuid:1")
                .unwrap()
                .1,
            RecordSegment::Id("uuid:1".into())
        );
    }

    #[test]
    fn hosts_are_https_except_this_computer() {
        assert_eq!(
            HostOrigin::from_authority("quinten.tilo.cloud")
                .unwrap()
                .as_str(),
            "https://quinten.tilo.cloud"
        );
        assert_eq!(
            HostOrigin::from_authority("localhost:9700")
                .unwrap()
                .as_str(),
            "http://localhost:9700"
        );
        assert_eq!(
            HostOrigin::from_authority("[::1]:8080").unwrap().as_str(),
            "http://[::1]:8080"
        );
        assert!(
            HostOrigin::from_authority("tilo.so:443").is_err(),
            "default port is omitted"
        );
        let tls = HostOrigin::new("https://127.0.0.1:9443").unwrap();
        assert_eq!(tls.route(), "https:127.0.0.1:9443");
        assert_eq!(
            HostOrigin::from_authority("https:127.0.0.1:9443").unwrap(),
            tls
        );
        assert!(
            HostOrigin::from_authority("https:tilo.so").is_err(),
            "every other host is https already"
        );
        assert!(HostOrigin::from_authority("https:localhost:443").is_err());
        assert!(HostOrigin::from_authority("Tilo.so").is_err());
        assert!(
            HostOrigin::new("http://tilo.so").is_err(),
            "plain http leaves this computer"
        );
        assert!(
            WebEntry::new("https://tilo.so/open").is_err(),
            "an entry ends in /"
        );
        assert!(
            WebEntry::new("https://tilo.so/app/").is_err(),
            "app/ names the mode"
        );
        assert!(WebEntry::new("https://tilo.so/?x=1/").is_err());
    }

    #[test]
    fn formats_the_spec_examples() {
        let mut target = LinkTarget::new(room("https://quinten.tilo.cloud"));
        if let Scope::Room { slug, .. } = &mut target.scope {
            *slug = Slug::from_name("Reading Group");
        }
        target.app = Some(AppId::new("so.tilo.word-processor").unwrap());
        target.record =
            Some(RecordSegment::new("uuid:3e0d5a1c-7b2f-4e8a-9c6d-1a2b3c4d5e6f").unwrap());
        target.resource = Some(ResourceAddress {
            kind: "word_processor.block".into(),
            id: "b12".into(),
            span: Some(Span::chars(40, 96)),
        });
        let route = format!(
            "/@quinten.tilo.cloud/w/reading-group~{ROOM}/so.tilo.word-processor/uuid:3e0d5a1c-7b2f-4e8a-9c6d-1a2b3c4d5e6f/word_processor.block/b12?chars=40-96"
        );
        let link = plain(Mode::Desktop, target);
        assert_eq!(
            link.to_url(&tilo()).unwrap(),
            format!("https://tilo.so/#{route}")
        );
        assert_eq!(link.to_native().unwrap(), format!("tilo://open#{route}"));

        let calculator = LinkTarget {
            app: Some(AppId::new("so.tilo.calculator").unwrap()),
            ..LinkTarget::new(Scope::Own)
        };
        assert_eq!(
            plain(Mode::App, calculator).to_url(&tilo()).unwrap(),
            "https://tilo.so/app/#/me/so.tilo.calculator"
        );
        let same_origin = plain(Mode::Desktop, LinkTarget::new(room("https://tilo.so")));
        assert_eq!(
            same_origin.to_url(&tilo()).unwrap(),
            format!("https://tilo.so/#/w/{ROOM}")
        );
    }

    /// A reader arriving from a published text reaches its discussion
    /// thread: the chat's channel named by the text's slug.
    #[test]
    fn a_discuss_alone_opens_that_text_s_thread() {
        let room = ROOM;
        let arrived = Link::parse(
            &format!("https://tilo.so/#/w/{room}?discuss=quiet-software"),
            &WebEntries::new(vec![tilo()]),
        )
        .unwrap();
        let target = &arrived.target;
        assert_eq!(target.app.as_ref().map(AppId::as_str), Some("so.tilo.chat"));
        assert_eq!(target.record, Some(RecordSegment::Primary));
        let resource = target.resource.as_ref().unwrap();
        assert_eq!(resource.kind, "chat.channel");
        assert_eq!(resource.id, "c_quiet_software");
        let url = arrived.to_url(&tilo()).unwrap();
        assert!(
            url.ends_with("/so.tilo.chat/-/chat.channel/c_quiet_software?discuss=quiet-software"),
            "{url}"
        );
        assert_eq!(
            Link::parse(&url, &WebEntries::new(vec![tilo()]))
                .unwrap()
                .target,
            *target
        );
    }

    /// A reader arrives from a published text by `?discuss=<slug>`: the
    /// page keeps it in the link it canonically shows, and only that link
    /// carries it.
    #[test]
    fn a_discuss_parameter_names_the_published_text_a_reader_came_from() {
        let mut target = LinkTarget::new(room("https://tilo.so"));
        target.discuss = Some(Slug::parse("quiet-software").unwrap());
        let link = plain(Mode::Desktop, target);
        let url = link.to_url(&tilo()).unwrap();
        assert!(url.ends_with("?discuss=quiet-software"), "{url}");
        assert_eq!(
            link.to_native().unwrap(),
            format!("tilo://open#/@tilo.so/w/{ROOM}?discuss=quiet-software")
        );
        let read = Link::parse(&url, &WebEntries::new(vec![tilo()])).unwrap();
        assert_eq!(
            read.target.discuss.as_ref().map(Slug::as_str),
            Some("quiet-software")
        );
        let bare = plain(Mode::Desktop, LinkTarget::new(room("https://tilo.so")));
        assert!(!bare.to_url(&tilo()).unwrap().contains("discuss"));
        // Its spelling is presentation, as a room's slug is: not any text.
        assert!(
            Link::parse(
                &format!("https://tilo.so/#/w/{ROOM}?discuss=Quiet Software"),
                &WebEntries::new(vec![tilo()])
            )
            .is_err()
        );
        // And it comes before a view, its only place among the params.
        assert!(
            Link::parse(
                &format!("https://tilo.so/#/w/{ROOM}?view=x&discuss=a"),
                &WebEntries::new(vec![tilo()])
            )
            .is_err()
        );
        let ordered = plain(Mode::Desktop, {
            let mut target = LinkTarget::new(room("https://tilo.so"));
            target.app = Some(AppId::new("so.tilo.chat").unwrap());
            target.record = Some(RecordSegment::Primary);
            target.discuss = Some(Slug::parse("a-post").unwrap());
            target.view = Some("thread".into());
            target
        });
        let ordered = ordered.to_url(&tilo()).unwrap();
        assert!(
            ordered.ends_with("?discuss=a-post&view=thread"),
            "{ordered}"
        );
        let read = Link::parse(&ordered, &WebEntries::new(vec![tilo()])).unwrap();
        assert_eq!(
            read.target.discuss.as_ref().map(Slug::as_str),
            Some("a-post")
        );
        assert_eq!(read.target.view.as_deref(), Some("thread"));
    }

    #[test]
    fn structure_is_checked_before_writing() {
        let mut target = LinkTarget::new(Scope::Own);
        target.record = Some(RecordSegment::Primary);
        assert_eq!(target.check().unwrap_err().code(), "incomplete");
        target.app = Some(AppId::new("so.tilo.chat").unwrap());
        target.resource = Some(ResourceAddress {
            kind: "k".repeat(129),
            id: "x".into(),
            span: None,
        });
        assert_eq!(target.check().unwrap_err().code(), "bad-resource");
        target.resource = None;
        target.record = Some(RecordSegment::Id("@tilo:package-primary".into()));
        assert_eq!(target.check().unwrap_err().code(), "non-canonical");
    }

    /// Review focus R1.
    #[test]
    fn secrets_never_render_outside_the_fragment() {
        let link = Link {
            mode: Mode::App,
            target: LinkTarget {
                app: Some(AppId::new("so.tilo.chat").unwrap()),
                ..LinkTarget::new(Scope::Own)
            },
            secrets: LinkSecrets {
                invite: Some(Secret::new("SECRETinvite").unwrap()),
                ticket: None,
                token: Some(Secret::new("SECRETtoken").unwrap()),
                pair: None,
                install: None,
            },
        };
        let printed = format!(
            "{link:?} {} {}",
            link.secrets.invite.as_ref().unwrap(),
            link.secrets.token.as_ref().unwrap()
        );
        assert!(!printed.contains("SECRET"), "{printed}");
        let url = link.to_url(&tilo()).unwrap();
        let (before, fragment) = url.split_once('#').unwrap();
        assert!(!before.contains("SECRET"));
        assert!(fragment.ends_with("?invite=SECRETinvite&token=SECRETtoken"));
        assert!(!link.public().to_url(&tilo()).unwrap().contains("SECRET"));
        assert!(Secret::new("has space").is_err());
        assert!(Secret::new("").is_err());
    }
}

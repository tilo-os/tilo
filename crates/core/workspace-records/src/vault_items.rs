//! What a workspace keeps to sign in with: its vault's entries, the places each belongs to, and
//! the changes whoever keeps the vault writes.
//!
//! A login, a key, a code generator's seed, an ssh key or a plain value such as a card number is
//! an entry of the workspace's vault, bound to the places it belongs to: a site, a service, a
//! server. The kinds and the places are the core's own vocabulary, so the device daemon and the
//! workspace service name an entry without the accounts that sign in with it. What reads and
//! uses the entries ([`crate::vault`]) is here too; what keeps a closed vault on a device, and
//! the store that kept entries before, are the accounts provider's.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tilo_clock::unix_now;

/// The longest origin an item may belong to.
const MAX_ORIGIN_BYTES: usize = 2_048;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretId(pub String);

impl SecretId {
    /// A new entry's id: random, and spelled as a reference to a vault item names it
    /// ([`is_entry_id`]).
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }
}

/// Whether `text` is an entry's id: 32 lowercase hexadecimal digits, or an older entry's, the
/// same spelled as a UUID with its four hyphens. What names a vault entry from outside it (a
/// reference to a vault item) names it by this id.
#[must_use]
pub fn is_entry_id(text: &str) -> bool {
    let hex = |part: &str| {
        part.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    match text.len() {
        32 => hex(text),
        36 => {
            let parts: Vec<&str> = text.split('-').collect();
            parts.iter().map(|part| part.len()).eq([8, 4, 4, 4, 12])
                && parts.iter().all(|part| hex(part))
        }
        _ => false,
    }
}

impl Default for SecretId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretItemKind {
    #[default]
    Login,
    ApiKey,
    SecureNote,
    Wifi,
    Other,
}

/// One encrypted vault item. Even title, username, URLs and tags stay inside
/// the authenticated ciphertext because this metadata is often sensitive.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretItem {
    pub id: SecretId,
    pub kind: SecretItemKind,
    pub title: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub origins: BTreeSet<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    #[serde(default)]
    pub tags: BTreeSet<String>,
    pub created_unix: u64,
    pub updated_unix: u64,
}

impl std::fmt::Debug for SecretItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretItem")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl SecretItem {
    /// A list of items from its JSON form (a backup's), read by code the
    /// runtime holds once rather than every app that reads such a list.
    #[inline(never)]
    pub fn list_from_json(bytes: &[u8]) -> serde_json::Result<Vec<Self>> {
        serde_json::from_slice(bytes)
    }

    pub fn login(
        title: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        let now = unix_now();
        Self {
            id: SecretId::new(),
            kind: SecretItemKind::Login,
            title: title.into(),
            username: username.into(),
            password: password.into(),
            origins: BTreeSet::new(),
            notes: String::new(),
            fields: BTreeMap::new(),
            tags: BTreeSet::new(),
            created_unix: now,
            updated_unix: now,
        }
    }

    /// Add `origin` to the places the item belongs to, as the one spelling places are
    /// compared in.
    ///
    /// # Errors
    /// `origin` is not a web origin an item may belong to.
    pub fn add_origin(&mut self, origin: &str) -> Result<(), InvalidOrigin> {
        self.origins.insert(canonical_origin(origin)?);
        Ok(())
    }
}

/// The longest place an item belongs to.
const MAX_PLACE_BYTES: usize = 2_048;

/// Why an item was not kept, listed or used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemError {
    /// No item is kept under that id. Another person's item is the same
    /// answer: their vault is not this one.
    NotFound,
    /// The item belongs somewhere else than where it was asked for.
    WrongPlace,
    /// The item is not the kind of thing the use needs.
    WrongKind,
    /// A code generator's seed was asked for: it never leaves the vault.
    NeverReleased,
    /// The text names no place an item can belong to.
    InvalidPlace,
    /// The item is not one to keep, for the reason given.
    Invalid(&'static str),
    /// The kept item does not read.
    Damaged,
    /// The vault could not be read or written.
    Storage(String),
}

impl std::fmt::Display for ItemError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("That item is not kept here"),
            Self::WrongPlace => formatter.write_str("That item belongs somewhere else"),
            Self::WrongKind => {
                formatter.write_str("That item is not the kind of thing this needs")
            }
            Self::NeverReleased => {
                formatter.write_str("A code generator gives codes, never its seed")
            }
            Self::InvalidPlace => formatter.write_str(
                "Where an item belongs is a web address such as https://example.com, or a server such as imap://mail.example.com:993",
            ),
            Self::Invalid(reason) => formatter.write_str(reason),
            Self::Damaged => formatter.write_str("That item is damaged and cannot be used"),
            Self::Storage(reason) => write!(formatter, "The vault is unavailable: {reason}"),
        }
    }
}

impl std::error::Error for ItemError {}

/// Where an item belongs, in the one spelling places are compared in: a
/// site's or a service's origin (`https://example.com`), or a server by
/// scheme, host and port (`imap://mail.example.com:993`,
/// `ssh://git.example.com:22`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Place(String);

impl Place {
    /// The place `input` names. A web address is cut to its origin, so the
    /// address of a sign-in page names its site. A mail server names its
    /// port; an ssh server without one is on port 22.
    pub fn parse(input: &str) -> Result<Self, ItemError> {
        let text = input.trim();
        if text.is_empty()
            || text.len() > MAX_PLACE_BYTES
            || text
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(ItemError::InvalidPlace);
        }
        let (scheme, rest) = text.split_once("://").ok_or(ItemError::InvalidPlace)?;
        let scheme = scheme.to_ascii_lowercase();
        match scheme.as_str() {
            "https" | "http" => Self::origin(text),
            "imap" | "smtp" => Self::server_named(&scheme, rest, None),
            "ssh" => Self::server_named(&scheme, rest, Some(22)),
            _ => Err(ItemError::InvalidPlace),
        }
    }

    /// The server `host` answers at on `port`, spoken to as `scheme`
    /// (`imap`, `smtp`, `ssh`).
    pub fn server(scheme: &str, host: &str, port: u16) -> Result<Self, ItemError> {
        let host = host.trim();
        if host.contains(':') && !host.starts_with('[') {
            Self::parse(&format!("{scheme}://[{host}]:{port}"))
        } else {
            Self::parse(&format!("{scheme}://{host}:{port}"))
        }
    }

    fn origin(text: &str) -> Result<Self, ItemError> {
        let parsed = url::Url::parse(text).map_err(|_| ItemError::InvalidPlace)?;
        if parsed.host_str().is_none_or(str::is_empty)
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(ItemError::InvalidPlace);
        }
        Ok(Self(parsed.origin().ascii_serialization()))
    }

    fn server_named(
        scheme: &str,
        rest: &str,
        default_port: Option<u16>,
    ) -> Result<Self, ItemError> {
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        if rest.contains(['/', '@', '?', '#']) {
            return Err(ItemError::InvalidPlace);
        }
        let (host, port) = split_host_port(rest).ok_or(ItemError::InvalidPlace)?;
        if host.is_empty() {
            return Err(ItemError::InvalidPlace);
        }
        let host = url::Host::parse(&host).map_err(|_| ItemError::InvalidPlace)?;
        let port = match port {
            Some(port) if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) => {
                port.parse::<u16>()
                    .ok()
                    .filter(|port| *port != 0)
                    .ok_or(ItemError::InvalidPlace)?
            }
            Some(_) => return Err(ItemError::InvalidPlace),
            None => default_port.ok_or(ItemError::InvalidPlace)?,
        };
        Ok(Self(format!("{scheme}://{host}:{port}")))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `https`, `http`, `imap`, `smtp` or `ssh`.
    #[must_use]
    pub fn scheme(&self) -> &str {
        self.0.split_once("://").map_or("", |(scheme, _)| scheme)
    }

    /// Whether this is a site's or a service's origin, not a server's.
    #[must_use]
    pub fn is_web(&self) -> bool {
        matches!(self.scheme(), "https" | "http")
    }
}

impl std::fmt::Display for Place {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for Place {
    type Error = ItemError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}

impl From<Place> for String {
    fn from(place: Place) -> Self {
        place.0
    }
}

/// The host of `rest` and the port it names, when it names one. An IPv6
/// address is in brackets, as in a URL.
fn split_host_port(rest: &str) -> Option<(String, Option<&str>)> {
    if let Some(after) = rest.strip_prefix('[') {
        let (inside, tail) = after.split_once(']')?;
        let port = if tail.is_empty() {
            None
        } else {
            Some(tail.strip_prefix(':')?)
        };
        Some((format!("[{inside}]"), port))
    } else if let Some((host, port)) = rest.rsplit_once(':') {
        Some((host.to_owned(), Some(port)))
    } else {
        Some((rest.to_owned(), None))
    }
}

/// The one spelling a web origin an item may belong to is kept in: its scheme, host and port,
/// lowercased, with no user, password, path, query or fragment.
///
/// # Errors
/// `input` is not a web origin.
pub fn canonical_origin(input: &str) -> Result<String, InvalidOrigin> {
    let trimmed = input.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_ORIGIN_BYTES
        || trimmed.chars().any(char::is_control)
    {
        return Err(InvalidOrigin);
    }
    let parsed = url::Url::parse(trimmed).map_err(|_| InvalidOrigin)?;
    if !matches!(parsed.scheme(), "https" | "http")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(InvalidOrigin);
    }
    Ok(parsed.origin().ascii_serialization())
}

/// Why an origin is not one an item may belong to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidOrigin;

impl std::fmt::Display for InvalidOrigin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("The origin is not valid")
    }
}

impl std::error::Error for InvalidOrigin {}

/// One time an item was handed to something that signs in: when, where and
/// for whom. Never what was handed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseRecord {
    /// When, in seconds since the Unix epoch.
    pub at_unix: u64,
    /// The places it was used at.
    pub places: Vec<Place>,
    /// Who it was handed out for: the agent that named it, by the name its
    /// person knows it under, or empty for the person themselves.
    pub by: String,
    /// What it was used for: the use's own word, as a leave names it
    /// (the word a grant to always use it names). Empty where whoever took it said none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub using: String,
    /// What was done with it there, in one line: a web call's method and
    /// path. Never the item's secret, never what was sent, and nothing that
    /// follows a `?` or a `#` in an address.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub what: String,
    /// How its person allowed it, where whoever took it said so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<Allowance>,
}

/// How a use came to be allowed by the item's person.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Allowance {
    /// They were asked and allowed this one use.
    Once,
    /// A grant of theirs to always allow it covered it.
    Always,
}

impl Allowance {
    /// The word a person reads it by.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Always => "always",
        }
    }
}

/// A change made to the vault's entries, for whoever keeps the vault to write: the entry kept,
/// the entry forgotten, or the uses of an entry. A use changes only an entry's uses, never the
/// entry, so a use written while a member edits the entry loses neither.
#[derive(Clone, PartialEq, Eq)]
pub enum VaultChange {
    Keep(SecretItem),
    Forget(SecretId),
    Uses(SecretId, Vec<UseRecord>),
}

impl std::fmt::Debug for VaultChange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keep(item) => formatter.debug_tuple("Keep").field(&item.id).finish(),
            Self::Forget(id) => formatter.debug_tuple("Forget").field(id).finish(),
            Self::Uses(id, uses) => formatter
                .debug_tuple("Uses")
                .field(id)
                .field(&uses.len())
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_canonical_origins_are_kept() {
        assert_eq!(
            canonical_origin(" HTTPS://BÜCHER.example:443/path?q=1 ").unwrap(),
            "https://xn--bcher-kva.example"
        );
        assert_eq!(
            canonical_origin("http://example.com:8080/a").unwrap(),
            "http://example.com:8080"
        );
        for invalid in [
            "https://user:password@example.com",
            "https://example.com\nspoofed",
            "file:///etc/passwd",
            "https://",
        ] {
            assert_eq!(canonical_origin(invalid), Err(InvalidOrigin));
        }
    }
}

//! What a person keeps to sign in with, each item bound to where it belongs.
//!
//! An item is a login, a key, a code generator's seed, an ssh key or a plain
//! value such as a card number, sealed in the person's vault beside their
//! account credentials, with the places it may be used at. Nothing here hands an item back to whoever asks:
//! [`VaultItems::facts`] says what is safe to say of each (its kind, where it
//! belongs, when it was added and when it was last used), and
//! [`VaultItems::use_at`] hands the secret to the code that signs in, only at
//! a place the item names, and writes that use down first: when, where and
//! for whom, never what was handed ([`VaultItems::uses`]). An [`ItemId`]
//! names an item and is not its secret, so it may be held by code that must
//! never read one.
//!
//! Something that runs with nobody watching uses an item only where its
//! person said it always may ([`VaultItems::let_always`]): the leave names
//! who, for what and where, is sealed with the item, is read beside its uses
//! ([`VaultItems::standing`]) and is taken back by the person
//! ([`VaultItems::take_back`]). A leave hands nothing out: every use still
//! goes through [`VaultItems::use_as`] and is written down there.
//!
//! A place is compared exactly: the scheme, the host and the port. An item
//! kept for `https://example.com` is not used at `http://example.com`, at
//! `https://login.example.com` or at any other port.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tilo_clock::unix_now;
use zeroize::{Zeroize as _, Zeroizing};

use crate::{AccountId, Secret, SecretVault};

/// The vault entries that are items, apart from account credentials, the
/// password inventory and service credentials in the same sealed file.
const PREFIX: &str = "tilo-vault-item:";

/// The longest secret an item keeps: an ssh key fits, a file does not.
pub const MAX_SECRET_BYTES: usize = 16 * 1024;
/// The most places one item belongs to: a mail sign-in names two servers.
pub const MAX_PLACES: usize = 8;
const MAX_USERNAME_BYTES: usize = 320;
const MAX_PLACE_BYTES: usize = 2_048;
/// The shortest seed a code generator is given: 80 bits in base32.
const MIN_SEED_CHARS: usize = 16;

/// What kind of thing an item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemKind {
    /// A name and a password, for a site or a server.
    Login,
    /// A key a service takes with each request.
    Key,
    /// The seed codes for a second step are made from. It gives codes and is
    /// never handed out itself.
    CodeGenerator,
    /// A private ssh key.
    SshKey,
    /// A plain value that is not a sign-in: a card number, a passport
    /// number. It belongs to the places it may be filled into.
    Value,
}

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

/// The name of one item: what a reference to it carries. It is random, so
/// holding one says the holder was handed it, and it is not the secret.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(String);

impl ItemId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    /// The id `text` spells: 32 lowercase hexadecimal digits.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        (text.len() == 32
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then(|| Self(text.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What is safe to say of an item: everything a listing carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemFacts {
    pub id: ItemId,
    pub kind: ItemKind,
    pub places: Vec<Place>,
    /// When it was added, in seconds since the Unix epoch.
    pub added_unix: u64,
    /// When it was last handed to something that signs in.
    pub last_used_unix: Option<u64>,
}

/// An item to keep.
pub struct NewItem {
    pub kind: ItemKind,
    pub places: Vec<Place>,
    /// The name it signs in with, when it has one.
    pub username: String,
    pub secret: Zeroizing<String>,
}

impl NewItem {
    /// `secret` to keep as a `kind` that belongs to `places`, signing in as
    /// `username` when it has a name. The secret is wiped from memory when
    /// the item is dropped.
    #[must_use]
    pub fn new(kind: ItemKind, places: Vec<Place>, username: String, secret: String) -> Self {
        Self {
            kind,
            places,
            username,
            secret: Zeroizing::new(secret),
        }
    }
}

impl std::fmt::Debug for NewItem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NewItem")
            .field("kind", &self.kind)
            .field("places", &self.places)
            .finish_non_exhaustive()
    }
}

/// What the code that signs in is handed, at a place the item belongs to.
pub struct Released {
    pub kind: ItemKind,
    pub username: String,
    pub secret: Zeroizing<String>,
}

impl std::fmt::Debug for Released {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Released")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

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
}

/// The uses an item remembers: the newest of them.
pub const MAX_USES_KEPT: usize = 32;
const MAX_BY_CHARS: usize = 120;

/// The standing leaves an item keeps: the newest of them.
pub const MAX_STANDING: usize = 16;
const MAX_USING_CHARS: usize = 64;

/// A standing leave its person gave: `by` may use the item for `using` at
/// every one of `places` without them being asked again. It says who, for
/// what and where, never what the item holds, and hands nothing out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standing {
    /// Who may: an agent, by its id in the record that keeps it.
    pub by: String,
    /// The name its person knew it under when they gave the leave.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// What it may use the item for: the use's own word.
    pub using: String,
    /// The places the leave covers.
    pub places: Vec<Place>,
    /// When the person gave it, in seconds since the Unix epoch.
    pub since_unix: u64,
}

/// `text` as a leave keeps a name or a word: one line, bounded.
fn kept_word(text: &str, most: usize) -> String {
    text.trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(most)
        .collect()
}

/// One item as the vault seals it.
#[derive(Serialize, Deserialize)]
struct Sealed {
    kind: ItemKind,
    places: BTreeSet<Place>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    username: String,
    secret: String,
    added_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_used_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    uses: Vec<UseRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    standing: Vec<Standing>,
}

impl Drop for Sealed {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

impl Sealed {
    /// `item` as it is kept, or why it is not one to keep.
    fn checked(mut item: NewItem) -> Result<Self, ItemError> {
        let places: BTreeSet<Place> = std::mem::take(&mut item.places).into_iter().collect();
        if places.is_empty() {
            return Err(ItemError::Invalid("An item says where it belongs"));
        }
        if places.len() > MAX_PLACES {
            return Err(ItemError::Invalid(
                "An item belongs to at most eight places",
            ));
        }
        if item.secret.is_empty() {
            return Err(ItemError::Invalid("An item's secret cannot be empty"));
        }
        if item.secret.len() > MAX_SECRET_BYTES {
            return Err(ItemError::Invalid("That secret is too long to keep"));
        }
        let username = item.username.trim().to_owned();
        if username.len() > MAX_USERNAME_BYTES || username.chars().any(char::is_control) {
            return Err(ItemError::Invalid("That is not a name to sign in with"));
        }
        let secret = match item.kind {
            ItemKind::Login | ItemKind::Value => item.secret.to_string(),
            ItemKind::Key => {
                if !places.iter().all(Place::is_web) {
                    return Err(ItemError::Invalid(
                        "A key belongs to a service's web address",
                    ));
                }
                item.secret.to_string()
            }
            ItemKind::CodeGenerator => {
                if !places.iter().all(Place::is_web) {
                    return Err(ItemError::Invalid(
                        "A code generator belongs to a site's web address",
                    ));
                }
                canonical_seed(&item.secret).ok_or(ItemError::Invalid(
                    "A code generator's seed is the setup key a site shows: letters A to Z and digits 2 to 7",
                ))?
            }
            ItemKind::SshKey => {
                if !places.iter().all(|place| place.scheme() == "ssh") {
                    return Err(ItemError::Invalid("An ssh key belongs to an ssh server"));
                }
                if !(item.secret.trim_start().starts_with("-----BEGIN ")
                    && item.secret.contains("PRIVATE KEY-----"))
                {
                    return Err(ItemError::Invalid("That is not a private ssh key"));
                }
                item.secret.to_string()
            }
        };
        Ok(Self {
            kind: item.kind,
            places,
            username,
            secret,
            added_unix: unix_now(),
            last_used_unix: None,
            uses: Vec::new(),
            standing: Vec::new(),
        })
    }

    /// Whether a use that needs `kind` and sends the secret to every one of
    /// `places` is one this item allows: the one rule every use is held to.
    fn allows(&self, kind: Option<ItemKind>, places: &[Place]) -> Result<(), ItemError> {
        if places.is_empty() || !places.iter().all(|place| self.places.contains(place)) {
            return Err(ItemError::WrongPlace);
        }
        if kind.is_some_and(|kind| kind != self.kind) {
            return Err(ItemError::WrongKind);
        }
        if self.kind == ItemKind::CodeGenerator {
            return Err(ItemError::NeverReleased);
        }
        Ok(())
    }

    fn facts(&self, id: ItemId) -> ItemFacts {
        ItemFacts {
            id,
            kind: self.kind,
            places: self.places.iter().cloned().collect(),
            added_unix: self.added_unix,
            last_used_unix: self.last_used_unix,
        }
    }
}

/// A code generator's seed in one spelling, when `text` is one: base32
/// without its spaces, hyphens and padding.
fn canonical_seed(text: &str) -> Option<String> {
    let seed: String = text
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '-')
        .map(|character| character.to_ascii_uppercase())
        .collect();
    let seed = seed.trim_end_matches('=');
    (seed.len() >= MIN_SEED_CHARS
        && seed
            .chars()
            .all(|character| matches!(character, 'A'..='Z' | '2'..='7')))
    .then(|| seed.to_owned())
}

/// The items one person's vault keeps.
pub struct VaultItems {
    vault: SecretVault,
}

impl VaultItems {
    /// The items kept in the vault under the config root `root`; see
    /// [`SecretVault::open_person`] for what each root's custody is.
    pub fn open_in(root: &Path) -> Result<Self, ItemError> {
        let vault = SecretVault::open_person(Some(root));
        if let Some(error) = vault.recovery_error() {
            return Err(ItemError::Storage(error.to_owned()));
        }
        Ok(Self { vault })
    }

    fn key(id: &ItemId) -> AccountId {
        AccountId::new(format!("{PREFIX}{}", id.0))
    }

    fn sealed(&self, id: &ItemId) -> Result<Sealed, ItemError> {
        let entry = self.vault.get(&Self::key(id)).ok_or(ItemError::NotFound)?;
        let json = Zeroizing::new(entry.token.ok_or(ItemError::Damaged)?);
        serde_json::from_str(&json).map_err(|_| ItemError::Damaged)
    }

    fn write(&mut self, id: &ItemId, sealed: &Sealed) -> Result<(), ItemError> {
        let payload =
            Zeroizing::new(serde_json::to_string(sealed).map_err(|_| ItemError::Damaged)?);
        let key = Self::key(id);
        let previous = self.vault.get(&key);
        self.vault.put(
            &key,
            &Secret {
                password: String::new(),
                token: Some(payload.to_string()),
            },
        );
        if let Err(error) = self.vault.try_save() {
            match previous {
                Some(previous) => self.vault.put(&key, &previous),
                None => {
                    self.vault.remove(&key);
                }
            }
            return Err(ItemError::Storage(error));
        }
        Ok(())
    }

    /// What is safe to say of every item, oldest first.
    #[must_use]
    pub fn facts(&self) -> Vec<ItemFacts> {
        let mut facts: Vec<ItemFacts> = self
            .vault
            .entries_with_prefix(PREFIX)
            .into_iter()
            .filter_map(|(key, entry)| {
                let id = ItemId::parse(key.strip_prefix(PREFIX)?)?;
                let json = Zeroizing::new(entry.token?);
                let sealed: Sealed = serde_json::from_str(&json).ok()?;
                Some(sealed.facts(id))
            })
            .collect();
        facts.sort_by(|a, b| {
            a.added_unix
                .cmp(&b.added_unix)
                .then_with(|| a.id.cmp(&b.id))
        });
        facts
    }

    /// Keep `item`, answering what is safe to say of it, its id included.
    pub fn add(&mut self, item: NewItem) -> Result<ItemFacts, ItemError> {
        let sealed = Sealed::checked(item)?;
        let id = ItemId::new();
        self.write(&id, &sealed)?;
        Ok(sealed.facts(id))
    }

    /// Forget the item `id` names, answering whether one was kept.
    pub fn remove(&mut self, id: &ItemId) -> Result<bool, ItemError> {
        let key = Self::key(id);
        let Some(previous) = self.vault.get(&key) else {
            return Ok(false);
        };
        self.vault.remove(&key);
        if let Err(error) = self.vault.try_save() {
            self.vault.put(&key, &previous);
            return Err(ItemError::Storage(error));
        }
        Ok(true)
    }

    /// Hand the item `id` names to the code that signs in at `place`, for
    /// `by` ([`UseRecord::by`]).
    ///
    /// Refused anywhere the item does not belong, and for a code generator,
    /// whose seed never leaves. The use is written down before the secret is
    /// handed out: a use that cannot be recorded does not happen.
    pub fn use_at(&mut self, id: &ItemId, place: &Place, by: &str) -> Result<Released, ItemError> {
        self.release(id, None, std::slice::from_ref(place), by)
    }

    /// [`Self::use_at`] for a use that needs an item of `kind` and signs in
    /// at every one of `places`: a mail sign-in goes to the server that
    /// receives and to the one that sends, and belongs to both.
    pub fn use_as(
        &mut self,
        id: &ItemId,
        kind: ItemKind,
        places: &[Place],
        by: &str,
    ) -> Result<Released, ItemError> {
        self.release(id, Some(kind), places, by)
    }

    fn release(
        &mut self,
        id: &ItemId,
        kind: Option<ItemKind>,
        places: &[Place],
        by: &str,
    ) -> Result<Released, ItemError> {
        let mut sealed = self.sealed(id)?;
        sealed.allows(kind, places)?;
        let now = unix_now();
        sealed.last_used_unix = Some(now);
        sealed.uses.push(UseRecord {
            at_unix: now,
            places: places.to_vec(),
            by: by
                .trim()
                .chars()
                .filter(|character| !character.is_control())
                .take(MAX_BY_CHARS)
                .collect(),
        });
        let excess = sealed.uses.len().saturating_sub(MAX_USES_KEPT);
        if excess > 0 {
            sealed.uses = sealed.uses.split_off(excess);
        }
        self.write(id, &sealed)?;
        Ok(Released {
            kind: sealed.kind,
            username: std::mem::take(&mut sealed.username),
            secret: Zeroizing::new(std::mem::take(&mut sealed.secret)),
        })
    }

    /// The uses the item `id` names remembers, oldest first.
    pub fn uses(&self, id: &ItemId) -> Result<Vec<UseRecord>, ItemError> {
        let mut sealed = self.sealed(id)?;
        Ok(std::mem::take(&mut sealed.uses))
    }

    /// Whether the item `id` names could be used as `kind` at every one of
    /// `places`, answering what is safe to say of it when it could. What
    /// [`Self::use_as`] refuses is refused here the same; nothing is handed
    /// out and nothing is written down, so whoever must ask a person first
    /// asks only about a use that would be taken.
    pub fn fits(
        &self,
        id: &ItemId,
        kind: ItemKind,
        places: &[Place],
    ) -> Result<ItemFacts, ItemError> {
        let sealed = self.sealed(id)?;
        sealed.allows(Some(kind), places)?;
        Ok(sealed.facts(id.clone()))
    }

    /// Whether `by` may use the item `id` names for `using` at every one of
    /// `places` without its person being asked: one leave of theirs
    /// ([`Self::let_always`]) names that agent and that use and covers all
    /// of those places.
    pub fn stands(
        &self,
        id: &ItemId,
        places: &[Place],
        by: &str,
        using: &str,
    ) -> Result<bool, ItemError> {
        let sealed = self.sealed(id)?;
        Ok(!places.is_empty()
            && !by.is_empty()
            && sealed.standing.iter().any(|leave| {
                leave.by == by
                    && leave.using == using
                    && places.iter().all(|place| leave.places.contains(place))
            }))
    }

    /// Keep the person's standing leave: the agent `by`, which they know as
    /// `name`, may use the item `id` names for `using` at every one of
    /// `places` without being asked again. Only places the item belongs to
    /// are covered, and the leave hands nothing out.
    pub fn let_always(
        &mut self,
        id: &ItemId,
        places: &[Place],
        (by, name): (&str, &str),
        using: &str,
    ) -> Result<(), ItemError> {
        let mut sealed = self.sealed(id)?;
        if places.is_empty() || !places.iter().all(|place| sealed.places.contains(place)) {
            return Err(ItemError::WrongPlace);
        }
        let by = kept_word(by, MAX_BY_CHARS);
        let using = kept_word(using, MAX_USING_CHARS);
        if by.is_empty() || using.is_empty() {
            return Err(ItemError::Invalid(
                "A standing leave names who may use the item and for what",
            ));
        }
        let mut covered = places.to_vec();
        covered.sort();
        covered.dedup();
        sealed
            .standing
            .retain(|leave| !(leave.by == by && leave.using == using && leave.places == covered));
        sealed.standing.push(Standing {
            by,
            name: kept_word(name, MAX_BY_CHARS),
            using,
            places: covered,
            since_unix: unix_now(),
        });
        let excess = sealed.standing.len().saturating_sub(MAX_STANDING);
        if excess > 0 {
            sealed.standing = sealed.standing.split_off(excess);
        }
        self.write(id, &sealed)
    }

    /// The standing leaves the item `id` names keeps, oldest first.
    pub fn standing(&self, id: &ItemId) -> Result<Vec<Standing>, ItemError> {
        let mut sealed = self.sealed(id)?;
        Ok(std::mem::take(&mut sealed.standing))
    }

    /// Take back the standing leaves of the item `id` names: every one, or
    /// only those of the agent `by`. Answers how many were taken back; from
    /// then on that agent's next use asks its person again.
    pub fn take_back(&mut self, id: &ItemId, by: Option<&str>) -> Result<usize, ItemError> {
        let mut sealed = self.sealed(id)?;
        let before = sealed.standing.len();
        sealed
            .standing
            .retain(|leave| by.is_some_and(|by| leave.by != by));
        let taken = before.saturating_sub(sealed.standing.len());
        if taken > 0 {
            self.write(id, &sealed)?;
        }
        Ok(taken)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const PASSWORD: &str = "correct horse battery staple";

    fn root() -> PathBuf {
        std::env::temp_dir().join(format!("accounts-vault-items-{}", uuid::Uuid::new_v4()))
    }

    fn place(text: &str) -> Place {
        Place::parse(text).unwrap()
    }

    fn login(places: &[&str]) -> NewItem {
        NewItem {
            kind: ItemKind::Login,
            places: places.iter().map(|text| place(text)).collect(),
            username: "ada@example.test".into(),
            secret: Zeroizing::new(PASSWORD.into()),
        }
    }

    /// Every file under `dir` whose bytes hold `needle`.
    fn files_holding(dir: &Path, needle: &[u8]) -> Vec<PathBuf> {
        let mut holding = Vec::new();
        let mut waiting = vec![dir.to_path_buf()];
        while let Some(next) = waiting.pop() {
            for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
                let path = entry.path();
                match std::fs::metadata(&path) {
                    Ok(metadata) if metadata.is_dir() => waiting.push(path),
                    Ok(metadata) if metadata.is_file() => {
                        let bytes = std::fs::read(&path).unwrap();
                        if bytes.windows(needle.len()).any(|window| window == needle) {
                            holding.push(path);
                        }
                    }
                    _ => {}
                }
            }
        }
        holding
    }

    /// A place has one spelling, whatever was typed: a page's address names
    /// its site, a server names its port, and what names no place is refused.
    #[test]
    fn a_place_has_one_spelling() {
        for (typed, kept) in [
            (
                "https://WWW.Example.com/login?next=1",
                "https://www.example.com",
            ),
            ("https://example.com:443", "https://example.com"),
            ("https://example.com:8443/", "https://example.com:8443"),
            ("HTTP://example.com", "http://example.com"),
            ("imap://IMAP.Example.com:993", "imap://imap.example.com:993"),
            (
                "smtp://smtp.example.com:465/",
                "smtp://smtp.example.com:465",
            ),
            ("ssh://git.example.com", "ssh://git.example.com:22"),
            ("ssh://[::1]:2222", "ssh://[::1]:2222"),
            ("  https://example.com  ", "https://example.com"),
        ] {
            let parsed = place(typed);
            assert_eq!(parsed.as_str(), kept, "{typed}");
            // The kept spelling reads back as itself.
            assert_eq!(place(parsed.as_str()), parsed, "{typed}");
        }
        for typed in [
            "",
            "example.com",
            "ftp://example.com",
            "imap://mail.example.com",
            "imap://mail.example.com:0",
            "imap://mail.example.com:+993",
            "imap://mail.example.com:99999",
            "imap://mail.example.com:993/INBOX",
            "imap://ada@mail.example.com:993",
            "imap://:993",
            "https://ada:pw@example.com",
            "https://exa mple.com",
            "https://exa\nmple.com",
            "ssh://a:b:22",
        ] {
            assert_eq!(
                Place::parse(typed),
                Err(ItemError::InvalidPlace),
                "{typed:?}"
            );
        }
        assert_eq!(
            Place::server("imap", "Mail.Example.com", 993)
                .unwrap()
                .as_str(),
            "imap://mail.example.com:993"
        );
        assert_eq!(
            Place::server("ssh", "::1", 22).unwrap().as_str(),
            "ssh://[::1]:22"
        );
        assert!(place("https://example.com").is_web());
        assert!(!place("imap://mail.example.com:993").is_web());
        // A sealed place that is no place does not read.
        assert!(serde_json::from_str::<Place>("\"example.com\"").is_err());
    }

    /// An item is handed out only where it belongs, each use is written
    /// down, and a refused use leaves no trace of having happened.
    #[test]
    fn an_item_is_used_only_where_it_belongs() {
        let root = root();
        let mut items = VaultItems::open_in(&root).unwrap();
        let kept = items
            .add(login(&["https://www.linkedin.com/login"]))
            .unwrap();
        assert_eq!(kept.kind, ItemKind::Login);
        assert_eq!(kept.places, [place("https://www.linkedin.com")]);
        assert!(kept.added_unix > 0);
        assert_eq!(kept.last_used_unix, None);

        for elsewhere in [
            "https://linkedin.com",
            "http://www.linkedin.com",
            "https://www.linkedin.com:8443",
            "https://www.linkedin.com.attacker.example",
            "imap://www.linkedin.com:993",
        ] {
            assert_eq!(
                items.use_at(&kept.id, &place(elsewhere), "Helper").err(),
                Some(ItemError::WrongPlace),
                "{elsewhere}"
            );
        }
        assert_eq!(
            VaultItems::open_in(&root).unwrap().facts()[0].last_used_unix,
            None,
            "a refused use is not a use"
        );

        let released = items
            .use_at(
                &kept.id,
                &place("https://www.linkedin.com/checkpoint"),
                "Helper",
            )
            .unwrap();
        assert_eq!(released.kind, ItemKind::Login);
        assert_eq!(released.username, "ada@example.test");
        assert_eq!(released.secret.as_str(), PASSWORD);
        // The use is on disk, read by another opening of the same vault.
        let reopened = VaultItems::open_in(&root).unwrap().facts();
        assert_eq!(reopened.len(), 1);
        assert!(
            reopened[0]
                .last_used_unix
                .is_some_and(|used| used >= kept.added_unix)
        );

        // An id nobody was handed names nothing, and neither does a removed
        // item's.
        let unknown = ItemId::parse("0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(
            items
                .use_at(&unknown, &place("https://www.linkedin.com"), "Helper")
                .err(),
            Some(ItemError::NotFound)
        );
        assert!(items.remove(&kept.id).unwrap());
        assert!(!items.remove(&kept.id).unwrap());
        assert_eq!(
            items
                .use_at(&kept.id, &place("https://www.linkedin.com"), "Helper")
                .err(),
            Some(ItemError::NotFound)
        );
        assert!(VaultItems::open_in(&root).unwrap().facts().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// What is said of an item carries neither its secret nor its name, in
    /// a listing, in a debug print or in any file in the clear, and the
    /// vault's other entries are no items.
    #[test]
    fn what_is_said_of_an_item_is_only_what_is_safe() {
        let root = root();
        let adding = login(&[
            "imap://mail.example.test:993",
            "smtp://mail.example.test:465",
        ]);
        let printed = format!("{adding:?}");
        assert!(
            !printed.contains(PASSWORD) && !printed.contains("ada@"),
            "{printed}"
        );

        // An account credential and a password-inventory item share the
        // sealed file, and are not items.
        let mut vault = SecretVault::open_in(&root);
        vault.put(
            &AccountId::new("ada@example.test"),
            &Secret::password("an account's own"),
        );
        vault.try_save().unwrap();
        let mut inventory = crate::SecretStore::open_in(&root);
        inventory
            .put(&mut crate::SecretItem::login("Bank", "ada", "inventory"))
            .unwrap();

        let mut items = VaultItems::open_in(&root).unwrap();
        let kept = items.add(adding).unwrap();
        let facts = items.facts();
        assert_eq!(facts, std::slice::from_ref(&kept));
        let printed = format!("{facts:?}");
        assert!(
            !printed.contains(PASSWORD) && !printed.contains("ada@"),
            "{printed}"
        );
        assert_eq!(
            kept.places,
            [
                place("imap://mail.example.test:993"),
                place("smtp://mail.example.test:465")
            ]
        );

        let released = items
            .use_at(&kept.id, &place("smtp://mail.example.test:465"), "Helper")
            .unwrap();
        let printed = format!("{released:?}");
        assert!(
            !printed.contains(PASSWORD) && !printed.contains("ada@"),
            "{printed}"
        );

        assert!(
            crate::vault_dir_in(&root).join("vault.enc").exists(),
            "the items are in the person's one sealed vault"
        );
        for needle in [PASSWORD, "ada@example.test", "mail.example.test"] {
            let clear = files_holding(&root, needle.as_bytes());
            assert!(clear.is_empty(), "{needle} in the clear: {clear:?}");
        }
        // Adding an item took nothing of the vault's other entries.
        assert_eq!(
            SecretVault::open_in(&root)
                .get(&AccountId::new("ada@example.test"))
                .map(|secret| secret.password),
            Some("an account's own".to_owned())
        );
        assert_eq!(crate::SecretStore::open_in(&root).list().len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    const SEED: &str = "jbsw y3dp-ehpk 3pxp";
    const SSH: &str =
        "-----BEGIN OPENSSH PRIVATE KEY-----\nexample\n-----END OPENSSH PRIVATE KEY-----\n";

    fn item(kind: ItemKind, places: &[&str], secret: &str) -> NewItem {
        NewItem::new(
            kind,
            places.iter().map(|text| place(text)).collect(),
            String::new(),
            secret.into(),
        )
    }

    /// Each kind is kept as what it is and handed out where it belongs, but
    /// for a code generator, whose seed is never handed out.
    #[test]
    fn each_kind_is_kept_as_what_it_is() {
        let root = root();
        let mut items = VaultItems::open_in(&root).unwrap();
        let generator = items
            .add(item(
                ItemKind::CodeGenerator,
                &["https://example.com"],
                SEED,
            ))
            .unwrap();
        assert_eq!(
            items
                .use_at(&generator.id, &place("https://example.com"), "Helper")
                .err(),
            Some(ItemError::NeverReleased)
        );
        assert_eq!(
            VaultItems::open_in(&root).unwrap().facts()[0].last_used_unix,
            None
        );
        for (kind, belongs, used_at, secret) in [
            (
                ItemKind::Key,
                "https://api.example.com/v1",
                "https://api.example.com",
                "sk-live-key",
            ),
            (
                ItemKind::SshKey,
                "ssh://git.example.com",
                "ssh://git.example.com:22",
                SSH,
            ),
            (
                ItemKind::Value,
                "https://shop.example.com/checkout",
                "https://shop.example.com",
                "4111 1111 1111 1111",
            ),
        ] {
            let kept = items.add(item(kind, &[belongs], secret)).unwrap();
            assert_eq!(kept.kind, kind);
            let released = items.use_at(&kept.id, &place(used_at), "Helper").unwrap();
            assert_eq!(released.kind, kind);
            assert_eq!(released.secret.as_str(), secret);
            assert_eq!(
                items
                    .use_at(&kept.id, &place("https://other.example.com"), "Helper")
                    .err(),
                Some(ItemError::WrongPlace)
            );
        }
        assert_eq!(items.facts().len(), 4);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// What is not an item of its kind, or belongs where its kind does not,
    /// is refused before anything is kept.
    #[test]
    fn what_is_not_an_item_of_its_kind_is_refused() {
        let root = root();
        let mut items = VaultItems::open_in(&root).unwrap();
        let nowhere = NewItem {
            places: Vec::new(),
            ..login(&[])
        };
        let everywhere = login(&[
            "https://a.example",
            "https://b.example",
            "https://c.example",
            "https://d.example",
            "https://e.example",
            "https://f.example",
            "https://g.example",
            "https://h.example",
            "https://i.example",
        ]);
        let long = "x".repeat(MAX_SECRET_BYTES + 1);
        let web = &["https://example.com"];
        for refused in [
            nowhere,
            everywhere,
            item(ItemKind::Login, web, ""),
            item(ItemKind::Login, web, &long),
            item(ItemKind::CodeGenerator, web, "not a seed!"),
            item(ItemKind::CodeGenerator, web, "JBSWY3DP"),
            item(
                ItemKind::CodeGenerator,
                &["imap://mail.example.com:993"],
                SEED,
            ),
            item(ItemKind::Key, &["ssh://git.example.com"], "sk-live-key"),
            item(ItemKind::SshKey, &["https://git.example.com"], SSH),
            item(ItemKind::SshKey, &["ssh://git.example.com"], "hunter2"),
            NewItem {
                username: "ada\u{0}".into(),
                ..login(web)
            },
        ] {
            let printed = format!("{refused:?}");
            let refusal = items.add(refused).err();
            assert!(
                matches!(refusal, Some(ItemError::Invalid(_))),
                "{printed}: {refusal:?}"
            );
        }
        assert!(items.facts().is_empty(), "a refused item is not kept");
        assert!(
            !crate::vault_dir_in(&root).join("vault.enc").exists(),
            "a refused item writes nothing"
        );
    }

    /// A use that needs one kind at several places is refused unless the
    /// item is that kind and belongs to each of them, and every use an item
    /// was handed out for is remembered without what was handed.
    #[test]
    fn a_use_names_its_kind_and_every_place_and_is_remembered() {
        let root = root();
        let mut items = VaultItems::open_in(&root).unwrap();
        let both = [
            place("imap://mail.example.test:993"),
            place("smtp://mail.example.test:465"),
        ];
        let kept = items
            .add(login(&[
                "imap://mail.example.test:993",
                "smtp://mail.example.test:465",
            ]))
            .unwrap();
        let half = items.add(login(&["imap://mail.example.test:993"])).unwrap();

        assert_eq!(
            items
                .use_as(&half.id, ItemKind::Login, &both, "Helper")
                .err(),
            Some(ItemError::WrongPlace)
        );
        assert_eq!(
            items.use_as(&kept.id, ItemKind::Login, &[], "Helper").err(),
            Some(ItemError::WrongPlace)
        );
        assert_eq!(
            items.use_as(&kept.id, ItemKind::Key, &both, "Helper").err(),
            Some(ItemError::WrongKind)
        );
        assert!(items.uses(&kept.id).unwrap().is_empty());
        assert!(items.uses(&half.id).unwrap().is_empty());

        let released = items
            .use_as(&kept.id, ItemKind::Login, &both, " Helper\n")
            .unwrap();
        assert_eq!(released.secret.as_str(), PASSWORD);
        items.use_at(&kept.id, &both[0], "").unwrap();
        let uses = VaultItems::open_in(&root).unwrap().uses(&kept.id).unwrap();
        assert_eq!(uses.len(), 2);
        assert_eq!(uses[0].places, both);
        assert_eq!(uses[0].by, "Helper");
        assert!(uses[0].at_unix >= kept.added_unix);
        assert_eq!(uses[1].places, [both[0].clone()]);
        assert_eq!(uses[1].by, "");
        let said = format!("{uses:?}");
        assert!(!said.contains(PASSWORD) && !said.contains("ada@"), "{said}");

        // An item remembers its newest uses, not all it ever had.
        for _ in 0..40 {
            items.use_at(&kept.id, &both[1], "Helper").unwrap();
        }
        let uses = items.uses(&kept.id).unwrap();
        assert_eq!(uses.len(), MAX_USES_KEPT);
        assert!(uses.iter().all(|used| used.places == [both[1].clone()]));
        let unknown = ItemId::parse("0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(items.uses(&unknown).err(), Some(ItemError::NotFound));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Whether an item fits a use is said without using it: what a use
    /// would be refused for is refused here the same, nothing is handed out
    /// and nothing is written down.
    #[test]
    fn whether_an_item_fits_a_use_is_said_without_using_it() {
        let root = root();
        let mut items = VaultItems::open_in(&root).unwrap();
        let both = [
            place("imap://mail.example.test:993"),
            place("smtp://mail.example.test:465"),
        ];
        let elsewhere = place("imap://mail.attacker.example:993");
        let kept = items
            .add(login(&[
                "imap://mail.example.test:993",
                "smtp://mail.example.test:465",
            ]))
            .unwrap();
        let seed = items
            .add(item(
                ItemKind::CodeGenerator,
                &["https://example.test"],
                SEED,
            ))
            .unwrap();
        let unknown = ItemId::parse("0123456789abcdef0123456789abcdef").unwrap();

        assert_eq!(
            items.fits(&kept.id, ItemKind::Login, &both).unwrap().id,
            kept.id
        );
        for (id, kind, places, refusal) in [
            (&kept.id, ItemKind::Key, &both[..], ItemError::WrongKind),
            (
                &kept.id,
                ItemKind::Login,
                std::slice::from_ref(&elsewhere),
                ItemError::WrongPlace,
            ),
            (&kept.id, ItemKind::Login, &[][..], ItemError::WrongPlace),
            (
                &seed.id,
                ItemKind::CodeGenerator,
                &[place("https://example.test")][..],
                ItemError::NeverReleased,
            ),
            (&unknown, ItemKind::Login, &both[..], ItemError::NotFound),
        ] {
            assert_eq!(items.fits(id, kind, places).err(), Some(refusal));
        }
        assert!(items.uses(&kept.id).unwrap().is_empty());
        assert!(
            items
                .facts()
                .iter()
                .all(|facts| facts.last_used_unix.is_none())
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A standing leave is for one agent, one use and the places it names.
    /// It covers no place the item does not belong to, is kept once however
    /// often it is given, stays with the item across a reopening, and hands
    /// nothing out.
    #[test]
    fn a_standing_leave_is_for_one_agent_one_use_and_the_places_it_names() {
        let root = root();
        let mut items = VaultItems::open_in(&root).unwrap();
        let both = [
            place("imap://mail.example.test:993"),
            place("smtp://mail.example.test:465"),
        ];
        let elsewhere = place("imap://mail.attacker.example:993");
        let kept = items
            .add(login(&[
                "imap://mail.example.test:993",
                "smtp://mail.example.test:465",
            ]))
            .unwrap();
        let scout = ("scout", "Scout");
        assert!(!items.stands(&kept.id, &both, "scout", "add-mail").unwrap());

        // A leave covers only where the item belongs, and names who and
        // for what.
        for (places, who, using) in [
            (std::slice::from_ref(&elsewhere), scout, "add-mail"),
            (&[][..], scout, "add-mail"),
            (&both[..], ("", ""), "add-mail"),
            (&both[..], scout, " "),
        ] {
            assert!(
                items.let_always(&kept.id, places, who, using).is_err(),
                "{places:?} {who:?} {using:?}"
            );
        }
        assert!(items.standing(&kept.id).unwrap().is_empty());

        items
            .let_always(&kept.id, &both, scout, "add-mail")
            .unwrap();
        // Given twice it is kept once.
        let turned = [both[1].clone(), both[0].clone()];
        items
            .let_always(&kept.id, &turned, scout, "add-mail")
            .unwrap();
        let items = VaultItems::open_in(&root).unwrap();
        let leaves = items.standing(&kept.id).unwrap();
        assert_eq!(leaves.len(), 1, "{leaves:?}");
        assert_eq!(leaves[0].by, "scout");
        assert_eq!(leaves[0].name, "Scout");
        assert_eq!(leaves[0].using, "add-mail");
        assert_eq!(leaves[0].places, both);
        let said = format!("{leaves:?}");
        assert!(!said.contains(PASSWORD) && !said.contains("ada@"), "{said}");

        assert!(items.stands(&kept.id, &both, "scout", "add-mail").unwrap());
        assert!(
            items
                .stands(&kept.id, &both[..1], "scout", "add-mail")
                .unwrap(),
            "a leave for both servers covers one of them"
        );
        for (places, by, using) in [
            (&both[..], "scout-two", "add-mail"),
            (&both[..], "Scout", "add-mail"),
            (&both[..], "scout", "fill-page"),
            (&both[..], "", "add-mail"),
            (&[][..], "scout", "add-mail"),
            (std::slice::from_ref(&elsewhere), "scout", "add-mail"),
        ] {
            assert!(
                !items.stands(&kept.id, places, by, using).unwrap(),
                "{places:?} {by} {using}"
            );
        }
        // A leave is not a use: nothing was handed out for it.
        assert!(items.uses(&kept.id).unwrap().is_empty());
        assert_eq!(items.facts()[0].last_used_unix, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Its person takes a leave back, one agent's or every one. An item
    /// that is forgotten keeps none, and an item keeps its newest leaves,
    /// not all it ever had.
    #[test]
    fn a_leave_is_taken_back_and_goes_with_its_item() {
        let root = root();
        let mut items = VaultItems::open_in(&root).unwrap();
        let both = [
            place("imap://mail.example.test:993"),
            place("smtp://mail.example.test:465"),
        ];
        let kept = items
            .add(login(&[
                "imap://mail.example.test:993",
                "smtp://mail.example.test:465",
            ]))
            .unwrap();
        let scout = ("scout", "Scout");
        items
            .let_always(&kept.id, &both, scout, "add-mail")
            .unwrap();

        // Another agent's leaves, which are none, then this one's.
        assert_eq!(items.take_back(&kept.id, Some("scout-two")).unwrap(), 0);
        assert!(items.stands(&kept.id, &both, "scout", "add-mail").unwrap());
        assert_eq!(items.take_back(&kept.id, Some("scout")).unwrap(), 1);
        assert!(!items.stands(&kept.id, &both, "scout", "add-mail").unwrap());
        assert_eq!(items.take_back(&kept.id, None).unwrap(), 0);

        // Every leave of the item at once.
        items
            .let_always(&kept.id, &both, scout, "add-mail")
            .unwrap();
        items
            .let_always(&kept.id, &both[..1], ("other", "Other"), "add-mail")
            .unwrap();
        assert_eq!(items.take_back(&kept.id, None).unwrap(), 2);
        assert!(items.standing(&kept.id).unwrap().is_empty());

        // An item that is forgotten keeps none.
        items
            .let_always(&kept.id, &both, scout, "add-mail")
            .unwrap();
        assert!(items.remove(&kept.id).unwrap());
        assert_eq!(
            items.stands(&kept.id, &both, "scout", "add-mail").err(),
            Some(ItemError::NotFound)
        );
        assert_eq!(
            items.let_always(&kept.id, &both, scout, "add-mail").err(),
            Some(ItemError::NotFound)
        );

        // An item keeps its newest leaves.
        let many = items.add(login(&["imap://mail.example.test:993"])).unwrap();
        for agent in 0..(MAX_STANDING + 4) {
            let name = format!("agent-{agent}");
            items
                .let_always(&many.id, &both[..1], (&name, ""), "add-mail")
                .unwrap();
        }
        let leaves = items.standing(&many.id).unwrap();
        assert_eq!(leaves.len(), MAX_STANDING);
        assert_eq!(leaves[0].by, "agent-4");
        std::fs::remove_dir_all(root).unwrap();
    }
}

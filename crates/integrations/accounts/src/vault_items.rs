//! What a workspace keeps to sign in with, each item bound to where it belongs.
//!
//! An item is a login, a key, a code generator's seed, an ssh key or a plain
//! value such as a card number: an entry of the workspace's vault (Passwords'
//! [`SecretItem`]) that names the places it may be used at. Nothing here hands
//! an item back to whoever asks: [`VaultItems::facts`] says what is safe to say
//! of each (its kind, where it belongs, when it was added and when it was last
//! used), and [`VaultItems::use_at`] hands the secret to the code that signs
//! in, only at a place the item names, and writes that use down with it: when,
//! where and for whom, and what for where whoever takes it says so
//! ([`VaultItems::use_for`]), never what was handed ([`VaultItems::uses`]). An
//! [`ItemId`] names an item and is not its secret, so it may be held by code
//! that must never read one.
//!
//! Nothing here keeps anything. Whoever holds the vault's document reads its
//! entries into [`VaultItems::of_entries`] and writes back what changed
//! ([`VaultItems::take_changes`]) before a released secret leaves it.
//!
//! A place is compared exactly: the scheme, the host and the port. An item
//! kept for `https://example.com` is not used at `http://example.com`, at
//! `https://login.example.com` or at any other port.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tilo_clock::unix_now;
use zeroize::{Zeroize as _, Zeroizing};

use crate::{SecretItem, SecretItemKind};

mod codes;

pub use codes::{
    Algorithm, CODE_DIGITS, CODE_SECS, Code, CodeParams, FEWEST_DIGITS, FEWEST_PERIOD, MOST_DIGITS,
    MOST_PERIOD,
};

/// The longest secret an item keeps: an ssh key fits, a file does not.
pub const MAX_SECRET_BYTES: usize = 16 * 1024;
/// The most places one item belongs to: a mail sign-in names two servers.
pub const MAX_PLACES: usize = 8;
const MAX_USERNAME_BYTES: usize = 320;
/// The shortest seed a code generator is given: 80 bits in base32.
const MIN_SEED_CHARS: usize = 16;

#[cfg(not(target_arch = "wasm32"))]
mod kept_before;
mod key_in;
#[cfg(not(target_arch = "wasm32"))]
pub use kept_before::KeptBefore;

pub use key_in::{KeyIn, MIN_SENT_KEY_BYTES};

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
    /// A note kept as a secret: its text is the secret, and it belongs nowhere.
    Note,
    /// The password of a Wi-Fi network.
    Wifi,
}

pub use tilo_workspace_records::vault_items::{
    Allowance, ItemError, Place, UseRecord, VaultChange,
};

/// The name of one item: what a reference to it carries. It is random, so
/// holding one says the holder was handed it, and it is not the secret.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(String);

impl ItemId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    /// The id `text` spells: an entry's id, 32 lowercase hexadecimal digits or the same
    /// spelled as a UUID with its hyphens.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        crate::is_entry_id(text).then(|| Self(text.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A login as a page at a site it belongs to is offered it: what is safe to say of it, and the
/// name it signs in with, which only such a page is told. Its debug print leaves the name out.
#[derive(Clone, PartialEq, Eq)]
pub struct SiteLogin {
    pub facts: ItemFacts,
    pub username: String,
}

impl std::fmt::Debug for SiteLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteLogin")
            .field("facts", &self.facts)
            .finish_non_exhaustive()
    }
}

/// What is safe to say of an item: everything a listing carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemFacts {
    pub id: ItemId,
    pub kind: ItemKind,
    pub places: Vec<Place>,
    /// How a code generator makes its codes, for an item that is one.
    pub code: Option<CodeParams>,
    /// What it is called: the entry's title, as Passwords shows it.
    pub title: String,
    /// When it was added, in seconds since the Unix epoch.
    pub added_unix: u64,
    /// When it was last handed to something that signs in.
    pub last_used_unix: Option<u64>,
    /// The tags a person gave it, and the tags a keeper that moved an item here names its app,
    /// its purpose and the item it was kept as.
    pub tags: Vec<String>,
    /// The labels of the fields a person added to it, such as `PIN`: never their values.
    pub field_labels: Vec<String>,
    /// Whether it keeps a note.
    pub has_notes: bool,
    /// When its entry last changed, in seconds since the Unix epoch.
    pub changed_unix: u64,
    /// A login or Wi-Fi password that is shorter than 12 characters or misses a capital, a small
    /// letter or a digit.
    pub weak: bool,
    /// A login or Wi-Fi password another one of the vault keeps too.
    pub reused: bool,
}

/// What a person keeps with an item beside its secret, as Passwords shows it: what it is
/// called, a note, the fields they added (a PIN, a recovery code) and its tags. The note and
/// the fields' values are secrets as the item's own is: sealed with it, never listed.
#[derive(Default)]
pub struct Extras {
    pub title: String,
    pub notes: Zeroizing<String>,
    /// Each field by its label, with its value.
    pub fields: Vec<(String, Zeroizing<String>)>,
    pub tags: Vec<String>,
}

impl std::fmt::Debug for Extras {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Extras")
            .field("title", &self.title)
            .field("tags", &self.tags)
            .finish_non_exhaustive()
    }
}

/// The longest title, tag or field label an item keeps.
const MAX_LABEL_CHARS: usize = 200;
/// The most fields or tags one item keeps.
const MAX_EXTRAS: usize = 64;
/// The prefix of the fields the vault keeps of an item itself ([`KIND_FIELD`] and the rest),
/// which no person's field takes.
const OWN_FIELD_PREFIX: &str = "tilo.vault.";

impl Extras {
    /// Whether nothing is kept beside the secret.
    fn is_empty(&self) -> bool {
        self.title.trim().is_empty()
            && self.notes.is_empty()
            && self.fields.is_empty()
            && self.tags.is_empty()
    }

    /// `self` as it is kept, or why it is not: a label is one line, no field is the vault's
    /// own, and nothing is longer than an item keeps.
    fn checked(&self) -> Result<(), ItemError> {
        let one_line = |text: &str| {
            text.chars().count() <= MAX_LABEL_CHARS && !text.chars().any(char::is_control)
        };
        if !one_line(self.title.trim()) {
            return Err(ItemError::Invalid("That is not a title to keep"));
        }
        if self.fields.len() > MAX_EXTRAS || self.tags.len() > MAX_EXTRAS {
            return Err(ItemError::Invalid(
                "An item keeps at most 64 fields and 64 tags",
            ));
        }
        if self.notes.len() > MAX_SECRET_BYTES
            || self
                .fields
                .iter()
                .any(|(_, value)| value.len() > MAX_SECRET_BYTES)
        {
            return Err(ItemError::Invalid("That is too long to keep"));
        }
        for (label, _) in &self.fields {
            let label = label.trim();
            if label.is_empty() || !one_line(label) || label.starts_with(OWN_FIELD_PREFIX) {
                return Err(ItemError::Invalid("That is not a field's label"));
            }
        }
        if self.tags.iter().any(|tag| !one_line(tag.trim())) {
            return Err(ItemError::Invalid("That is not a tag"));
        }
        Ok(())
    }

    /// `self` written onto `entry`, which keeps its secret and the vault's own fields.
    fn dress(&self, entry: &mut SecretItem) {
        if !self.title.trim().is_empty() {
            self.title.trim().clone_into(&mut entry.title);
        }
        self.notes.as_str().clone_into(&mut entry.notes);
        for (label, value) in &self.fields {
            if !value.is_empty() {
                entry
                    .fields
                    .insert(label.trim().to_owned(), value.as_str().to_owned());
            }
        }
        entry.tags = self
            .tags
            .iter()
            .map(|tag| tag.trim().to_owned())
            .filter(|tag| !tag.is_empty())
            .collect();
    }
}

/// Whether a password a person chose is weak: shorter than 12 characters, or without a
/// capital, a small letter or a digit.
fn is_weak(password: &str) -> bool {
    password.chars().count() < 12
        || !password.chars().any(char::is_uppercase)
        || !password.chars().any(char::is_lowercase)
        || !password.chars().any(|character| character.is_ascii_digit())
}

/// An item to keep.
pub struct NewItem {
    pub kind: ItemKind,
    pub places: Vec<Place>,
    /// The name it signs in with, when it has one.
    pub username: String,
    /// How a code generator makes its codes, where the kind is one.
    pub code: CodeParams,
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
            code: CodeParams::default(),
            secret: Zeroizing::new(secret),
        }
    }

    /// `item` as a code generator makes its codes: the hash, the digits and the period its
    /// service's setup key named.
    #[must_use]
    pub fn with_code(mut self, code: CodeParams) -> Self {
        self.code = code;
        self
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

/// What an item is used for, written down with the use. Whoever takes the
/// item says it, and none of it is anything the item holds.
#[derive(Clone, Copy, Debug, Default)]
pub struct UsedFor<'a> {
    /// The use's own word (the word a grant to always use it names).
    pub using: &'a str,
    /// What is done with it, in one line.
    pub what: &'a str,
    /// How its person allowed it.
    pub allowed: Option<Allowance>,
}

/// Which secret of an item its person is shown or given: the item's own, its note, or one of
/// the fields they added to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part<'a> {
    /// The password, key or value itself; a note's text for a note kept with none.
    Secret,
    /// The note kept with it.
    Notes,
    /// The name it signs in with.
    Username,
    /// The field of this label.
    Field(&'a str),
}

/// The uses an item remembers: the newest of them.
pub const MAX_USES_KEPT: usize = 32;
const MAX_BY_CHARS: usize = 120;
const MAX_WHAT_CHARS: usize = 200;

const MAX_USING_CHARS: usize = 64;

/// `text` as a use keeps a name or a word: one line, bounded.
fn kept_word(text: &str, most: usize) -> String {
    text.trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(most)
        .collect()
}

/// The field of an entry that says which kind of item it is, where the entry's own kind does
/// not: a code generator, an ssh key or a plain value are all entries of another kind.
pub const KIND_FIELD: &str = "tilo.vault.kind";
/// The field of a key's entry that says how it rides in a request ([`KeyIn`]).
pub const SENT_IN_FIELD: &str = "tilo.vault.sent-in";
/// The field of an entry kept as one account's own sign-in, which goes when the account goes:
/// the account it was kept for (a mail or calendar account's id, or a model provider's).
pub const ACCOUNT_FIELD: &str = "tilo.vault.account";
/// The field of a code generator's entry that says which hash its codes are made with.
pub const ALGORITHM_FIELD: &str = "tilo.vault.algorithm";
/// The field of a code generator's entry that says how many digits its codes have.
pub const DIGITS_FIELD: &str = "tilo.vault.digits";
/// The field of a code generator's entry that says how many seconds one code stands.
pub const PERIOD_FIELD: &str = "tilo.vault.period";

/// One entry of the vault, as an item reads it.
struct Sealed {
    kind: ItemKind,
    places: BTreeSet<Place>,
    username: String,
    /// How a key rides in a request to its service, where it is no bearer
    /// token ([`KeyIn`]).
    sent_in: String,
    /// The account it was kept for as that account's own sign-in, or empty for an entry a
    /// member keeps ([`ACCOUNT_FIELD`]).
    account: String,
    /// How a code generator makes its codes; the defaults for every other item.
    code: CodeParams,
    secret: String,
    added_unix: u64,
    last_used_unix: Option<u64>,
    uses: Vec<UseRecord>,
}

impl Drop for Sealed {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

impl Sealed {
    /// How this code generator makes its codes.
    fn code_params(&self) -> CodeParams {
        self.code
    }

    /// `item` as it is kept, or why it is not one to keep.
    fn checked(mut item: NewItem) -> Result<Self, ItemError> {
        let places: BTreeSet<Place> = std::mem::take(&mut item.places).into_iter().collect();
        // An item may belong nowhere: a note, a Wi-Fi password, a licence code. Such an item is
        // never handed to anything that signs in, since every use names where it goes.
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
            ItemKind::Login | ItemKind::Value | ItemKind::Note | ItemKind::Wifi => {
                item.secret.to_string()
            }
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
            sent_in: String::new(),
            account: String::new(),
            code: item.code,
            secret,
            added_unix: unix_now(),
            last_used_unix: None,
            uses: Vec::new(),
        })
    }

    /// The item an entry of the vault is, with the uses kept beside it: every entry is one, a
    /// note and a Wi-Fi network included. `None` only for an entry whose kind does not read.
    fn of_entry(entry: &SecretItem, uses: &[UseRecord]) -> Option<Self> {
        let kind = match entry.fields.get(KIND_FIELD) {
            Some(kind) => serde_json::from_value(serde_json::Value::String(kind.clone())).ok()?,
            None => match entry.kind {
                SecretItemKind::Login => ItemKind::Login,
                SecretItemKind::ApiKey => ItemKind::Key,
                SecretItemKind::SecureNote => ItemKind::Note,
                SecretItemKind::Wifi => ItemKind::Wifi,
                SecretItemKind::Other => ItemKind::Value,
            },
        };
        let places: BTreeSet<Place> = entry
            .origins
            .iter()
            .filter_map(|origin| Place::parse(origin).ok())
            .collect();
        let code = CodeParams {
            algorithm: entry
                .fields
                .get(ALGORITHM_FIELD)
                .and_then(|word| Algorithm::parse(word))
                .unwrap_or_default(),
            digits: entry
                .fields
                .get(DIGITS_FIELD)
                .and_then(|text| text.parse().ok())
                .unwrap_or(CODE_DIGITS),
            period: entry
                .fields
                .get(PERIOD_FIELD)
                .and_then(|text| text.parse().ok())
                .unwrap_or(CODE_SECS),
        };
        Some(Self {
            kind,
            places,
            username: entry.username.clone(),
            sent_in: entry.fields.get(SENT_IN_FIELD).cloned().unwrap_or_default(),
            account: entry.fields.get(ACCOUNT_FIELD).cloned().unwrap_or_default(),
            code,
            secret: entry.password.clone(),
            added_unix: entry.created_unix,
            last_used_unix: uses.iter().map(|used| used.at_unix).max(),
            uses: uses.to_vec(),
        })
    }

    /// The entry of the vault this item is kept as: what Passwords shows of it too.
    fn entry(&self, id: &ItemId) -> SecretItem {
        let kind = match self.kind {
            ItemKind::Login => SecretItemKind::Login,
            ItemKind::Key => SecretItemKind::ApiKey,
            ItemKind::Note => SecretItemKind::SecureNote,
            ItemKind::Wifi => SecretItemKind::Wifi,
            ItemKind::CodeGenerator | ItemKind::SshKey | ItemKind::Value => SecretItemKind::Other,
        };
        let mut fields = std::collections::BTreeMap::new();
        if !matches!(
            self.kind,
            ItemKind::Login | ItemKind::Key | ItemKind::Note | ItemKind::Wifi
        ) {
            fields.insert(
                KIND_FIELD.to_owned(),
                serde_json::to_value(self.kind)
                    .ok()
                    .and_then(|kind| kind.as_str().map(str::to_owned))
                    .unwrap_or_default(),
            );
        }
        if !self.sent_in.is_empty() {
            fields.insert(SENT_IN_FIELD.to_owned(), self.sent_in.clone());
        }
        if !self.account.is_empty() {
            fields.insert(ACCOUNT_FIELD.to_owned(), self.account.clone());
        }
        if self.kind == ItemKind::CodeGenerator {
            fields.insert(
                ALGORITHM_FIELD.to_owned(),
                self.code.algorithm.word().to_owned(),
            );
            fields.insert(DIGITS_FIELD.to_owned(), self.code.digits.to_string());
            fields.insert(PERIOD_FIELD.to_owned(), self.code.period.to_string());
        }
        let title = self
            .places
            .iter()
            .next()
            .and_then(|place| place.as_str().split_once("://"))
            .map_or_else(String::new, |(_, rest)| rest.to_owned());
        SecretItem {
            id: crate::SecretId(id.as_str().to_owned()),
            kind,
            title,
            username: self.username.clone(),
            password: self.secret.clone(),
            origins: self
                .places
                .iter()
                .map(|place| place.as_str().to_owned())
                .collect(),
            notes: String::new(),
            fields,
            tags: BTreeSet::new(),
            created_unix: self.added_unix,
            updated_unix: unix_now(),
        }
    }

    /// Whether a use that needs `kind` and sends the secret to every one of
    /// `places` is one this item allows: the one rule every use is held to.
    fn allows(&self, kind: Option<ItemKind>, places: &[Place]) -> Result<(), ItemError> {
        if places.is_empty() || !places.iter().all(|place| self.places.contains(place)) {
            return Err(ItemError::WrongPlace);
        }
        // An entry an older Passwords kept with no secret (a note's text in its notes) has
        // nothing to hand out.
        if self.secret.is_empty() {
            return Err(ItemError::NotFound);
        }
        if kind.is_some_and(|kind| kind != self.kind) {
            return Err(ItemError::WrongKind);
        }
        if self.kind == ItemKind::CodeGenerator {
            return Err(ItemError::NeverReleased);
        }
        Ok(())
    }

    /// Write down that the item was used at `places` for `by` at `now`,
    /// keeping the newest uses.
    fn used(&mut self, now: u64, places: &[Place], by: &str) {
        self.used_for(now, places, by, UsedFor::default());
    }

    /// [`Self::used`], with what it was used for. The line that says what
    /// was done is kept up to a `?` or a `#`, so no query of an address is
    /// written, and it is not kept at all where it holds the item's own
    /// secret.
    fn used_for(&mut self, now: u64, places: &[Place], by: &str, purpose: UsedFor<'_>) {
        let what = purpose.what.split(['?', '#']).next().unwrap_or_default();
        let what = if self.secret.is_empty() || !what.contains(self.secret.as_str()) {
            kept_word(what, MAX_WHAT_CHARS)
        } else {
            String::new()
        };
        self.last_used_unix = Some(now);
        self.uses.push(UseRecord {
            at_unix: now,
            places: places.to_vec(),
            by: kept_word(by, MAX_BY_CHARS),
            using: kept_word(purpose.using, MAX_USING_CHARS),
            what,
            allowed: purpose.allowed,
        });
        let excess = self.uses.len().saturating_sub(MAX_USES_KEPT);
        if excess > 0 {
            self.uses = self.uses.split_off(excess);
        }
    }

    fn facts(&self, id: ItemId) -> ItemFacts {
        ItemFacts {
            id,
            kind: self.kind,
            places: self.places.iter().cloned().collect(),
            code: (self.kind == ItemKind::CodeGenerator).then_some(self.code),
            title: String::new(),
            added_unix: self.added_unix,
            last_used_unix: self.last_used_unix,
            tags: Vec::new(),
            field_labels: Vec::new(),
            has_notes: false,
            changed_unix: self.added_unix,
            weak: matches!(self.kind, ItemKind::Login | ItemKind::Wifi)
                && !self.secret.is_empty()
                && is_weak(&self.secret),
            reused: false,
        }
    }
}

/// `facts` with what `entry` keeps beside the item: its title, tags, the labels of its
/// fields, whether it keeps a note and when it last changed.
fn dressed(mut facts: ItemFacts, entry: &SecretItem) -> ItemFacts {
    facts.title.clone_from(&entry.title);
    facts.tags = entry.tags.iter().cloned().collect();
    facts.field_labels = entry
        .fields
        .keys()
        .filter(|label| !label.starts_with(OWN_FIELD_PREFIX))
        .cloned()
        .collect();
    facts.has_notes = !entry.notes.is_empty();
    facts.changed_unix = entry.updated_unix;
    facts
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

/// The items of a workspace's vault: its entries, each with the uses kept beside it, read as
/// what signs in where. Nothing here keeps anything: every change is answered as a
/// [`VaultChange`] ([`Self::take_changes`]), which whoever read the entries writes back to the
/// vault's document before what was released leaves it. A use that cannot be written down does
/// not happen.
#[derive(Default)]
pub struct VaultItems {
    entries: std::collections::BTreeMap<ItemId, (SecretItem, Vec<UseRecord>)>,
    changes: Vec<VaultChange>,
}

impl VaultItems {
    /// The items of the vault whose entries are `entries`, each with the uses kept beside it.
    /// An entry whose id names no item is passed over: Passwords keeps entries an item never
    /// is.
    pub fn of_entries(entries: impl IntoIterator<Item = (SecretItem, Vec<UseRecord>)>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .filter_map(|(entry, uses)| Some((ItemId::parse(&entry.id.0)?, (entry, uses))))
                .collect(),
            changes: Vec::new(),
        }
    }

    /// The changes made since the items were read, oldest first, to be written to the vault.
    pub fn take_changes(&mut self) -> Vec<VaultChange> {
        std::mem::take(&mut self.changes)
    }

    /// Every entry with the uses beside it, as they stand after the changes made: what reads
    /// back as these same items.
    #[must_use]
    pub fn kept(&self) -> Vec<(SecretItem, Vec<UseRecord>)> {
        self.entries.values().cloned().collect()
    }

    fn sealed(&self, id: &ItemId) -> Result<Sealed, ItemError> {
        let (entry, uses) = self.entries.get(id).ok_or(ItemError::NotFound)?;
        Sealed::of_entry(entry, uses).ok_or(ItemError::NotFound)
    }

    /// Keep `sealed` as the entry `id` names, whole.
    fn write(&mut self, id: &ItemId, sealed: &Sealed) {
        let mut entry = sealed.entry(id);
        if let Some((kept, _)) = self.entries.get(id) {
            // What Passwords keeps of the entry besides the item stays.
            entry.title.clone_from(&kept.title);
            entry.notes.clone_from(&kept.notes);
            entry.tags.clone_from(&kept.tags);
            for (name, value) in &kept.fields {
                entry
                    .fields
                    .entry(name.clone())
                    .or_insert_with(|| value.clone());
            }
        }
        self.changes.push(VaultChange::Keep(entry.clone()));
        self.entries
            .insert(id.clone(), (entry, sealed.uses.clone()));
    }

    /// Write down `sealed`'s uses, leaving its entry as it is.
    fn write_uses(&mut self, id: &ItemId, sealed: &Sealed) {
        if let Some((_, uses)) = self.entries.get_mut(id) {
            uses.clone_from(&sealed.uses);
        }
        self.changes.push(VaultChange::Uses(
            crate::SecretId(id.as_str().to_owned()),
            sealed.uses.clone(),
        ));
    }

    /// What is safe to say of every item, oldest first.
    #[must_use]
    pub fn facts(&self) -> Vec<ItemFacts> {
        let mut facts: Vec<ItemFacts> = self
            .entries
            .iter()
            .filter_map(|(id, (entry, uses))| {
                let mut facts = dressed(Sealed::of_entry(entry, uses)?.facts(id.clone()), entry);
                facts.reused = self.reused(id, entry);
                Some(facts)
            })
            .collect();
        facts.sort_by(|a, b| {
            a.added_unix
                .cmp(&b.added_unix)
                .then_with(|| a.id.cmp(&b.id))
        });
        facts
    }

    /// What is safe to say of the item `sealed` was written as, titled as its entry is: the
    /// entry's own title, which Passwords shows.
    fn written(&self, id: &ItemId, sealed: &Sealed) -> ItemFacts {
        let facts = sealed.facts(id.clone());
        match self.entries.get(id) {
            Some((entry, _)) => {
                let reused = self.reused(id, entry);
                ItemFacts {
                    reused,
                    ..dressed(facts, entry)
                }
            }
            None => facts,
        }
    }

    /// Whether another login or Wi-Fi network of the vault keeps the password `entry` keeps.
    fn reused(&self, id: &ItemId, entry: &SecretItem) -> bool {
        // A password a person chose: a login's or a Wi-Fi network's.
        let is_login = |entry: &SecretItem| {
            matches!(entry.kind, SecretItemKind::Login | SecretItemKind::Wifi)
                && !entry.fields.contains_key(KIND_FIELD)
        };
        is_login(entry)
            && !entry.password.is_empty()
            && self.entries.iter().any(|(other, (kept, _))| {
                other != id && is_login(kept) && kept.password == entry.password
            })
    }

    /// Keep `item` with what a person keeps beside it ([`Extras`]), answering what is safe to
    /// say of it, its id included: a key says where it rides in a request when `sent_in` does.
    /// Checked whole before anything is kept.
    pub fn add_with(
        &mut self,
        item: NewItem,
        sent_in: Option<&KeyIn>,
        extras: &Extras,
    ) -> Result<ItemFacts, ItemError> {
        extras.checked()?;
        let added = match sent_in {
            Some(sent_in) => self.add_key(item, sent_in)?,
            None => self.add(item)?,
        };
        if extras.is_empty() {
            return Ok(added);
        }
        if let Some((entry, _)) = self.entries.get_mut(&added.id) {
            extras.dress(entry);
            if let Some(VaultChange::Keep(kept)) = self.changes.last_mut() {
                extras.dress(kept);
            }
        }
        let sealed = self.sealed(&added.id)?;
        Ok(self.written(&added.id, &sealed))
    }

    /// Keep `item`, answering what is safe to say of it, its id included.
    pub fn add(&mut self, item: NewItem) -> Result<ItemFacts, ItemError> {
        let sealed = Sealed::checked(item)?;
        let id = ItemId::new();
        self.write(&id, &sealed);
        Ok(self.written(&id, &sealed))
    }

    /// Keep `item` as the entry `id` names, titled `title` where the vault has no entry at
    /// `id` yet: a password typed anew for an account replaces the one its entry kept, and
    /// the entry keeps its id, its title, what Passwords added to it, when it was added and
    /// its uses. Where `id` names no entry, or none is given, `item` is kept as a new one.
    pub fn keep(
        &mut self,
        id: Option<&ItemId>,
        item: NewItem,
        title: &str,
    ) -> Result<ItemFacts, ItemError> {
        let mut sealed = Sealed::checked(item)?;
        let kept = id.and_then(|id| Some((id, self.entries.get(id)?)));
        let id = match kept {
            Some((id, (entry, uses))) => {
                sealed.added_unix = entry.created_unix;
                sealed.uses.clone_from(uses);
                sealed.last_used_unix = uses.iter().map(|used| used.at_unix).max();
                id.clone()
            }
            None => ItemId::new(),
        };
        let fresh = !self.entries.contains_key(&id);
        self.write(&id, &sealed);
        if fresh
            && !title.trim().is_empty()
            && let Some((entry, _)) = self.entries.get_mut(&id)
        {
            title.trim().clone_into(&mut entry.title);
            if let Some(VaultChange::Keep(kept)) = self.changes.last_mut() {
                kept.title.clone_from(&entry.title);
            }
        }
        Ok(self.written(&id, &sealed))
    }

    /// Change the item `id` names to `item`, with what a person keeps beside it ([`Extras`]),
    /// keeping its id, when it was added and its uses: what Passwords writes as a person edits
    /// an item. Each secret part left empty stays as it was: its secret, the name it signs in
    /// with, its title, its note and the value of each field it names again. Where it belongs,
    /// its tags and which fields it has are `item`'s and `extras`'.
    ///
    /// # Errors
    /// No such item; an account's own sign-in, which changes with its account; or a change that
    /// is not one to keep, checked whole before anything is kept.
    pub fn change(
        &mut self,
        id: &ItemId,
        mut item: NewItem,
        sent_in: Option<&KeyIn>,
        extras: &Extras,
    ) -> Result<ItemFacts, ItemError> {
        extras.checked()?;
        let was = self.sealed(id)?;
        if !was.account.is_empty() {
            return Err(ItemError::Invalid(
                "An account's own sign-in changes with its account",
            ));
        }
        if item.secret.is_empty() {
            item.secret = Zeroizing::new(was.secret.clone());
        }
        if item.username.trim().is_empty() {
            item.username.clone_from(&was.username);
        }
        let mut sealed = Sealed::checked(item)?;
        sealed.added_unix = was.added_unix;
        sealed.uses.clone_from(&was.uses);
        sealed.last_used_unix = was.last_used_unix;
        match sent_in {
            Some(_) if sealed.kind != ItemKind::Key => {
                return Err(ItemError::Invalid(
                    "Only a key says how it is sent with a request",
                ));
            }
            Some(sent_in) => sealed.sent_in = sent_in.kept(),
            None if sealed.kind == ItemKind::Key => sealed.sent_in.clone_from(&was.sent_in),
            None => {}
        }
        let (kept, _) = self.entries.get(id).ok_or(ItemError::NotFound)?;
        let mut entry = sealed.entry(id);
        entry.title = if extras.title.trim().is_empty() {
            kept.title.clone()
        } else {
            extras.title.trim().to_owned()
        };
        entry.notes = if extras.notes.is_empty() {
            kept.notes.clone()
        } else {
            extras.notes.as_str().to_owned()
        };
        for (label, value) in &extras.fields {
            let label = label.trim();
            let value = if value.is_empty() {
                kept.fields.get(label).cloned().unwrap_or_default()
            } else {
                value.as_str().to_owned()
            };
            if !value.is_empty() {
                entry.fields.insert(label.to_owned(), value);
            }
        }
        entry.tags = extras
            .tags
            .iter()
            .map(|tag| tag.trim().to_owned())
            .filter(|tag| !tag.is_empty())
            .collect();
        self.changes.push(VaultChange::Keep(entry.clone()));
        self.entries
            .insert(id.clone(), (entry, sealed.uses.clone()));
        Ok(self.written(id, &sealed))
    }

    /// Change how the code generator `id` names makes its codes, and the name it signs in as,
    /// keeping its seed and the uses it remembers.
    ///
    /// # Errors
    /// No such item, or an item that is no code generator.
    pub fn retune(
        &mut self,
        id: &ItemId,
        username: &str,
        code: CodeParams,
    ) -> Result<ItemFacts, ItemError> {
        let mut sealed = self.sealed(id)?;
        if sealed.kind != ItemKind::CodeGenerator {
            return Err(ItemError::WrongKind);
        }
        username.trim().clone_into(&mut sealed.username);
        sealed.code = code;
        self.write(id, &sealed);
        Ok(self.written(id, &sealed))
    }

    /// Every login that belongs to `site`, oldest first: what a page at that site is offered
    /// to sign in with.
    #[must_use]
    pub fn logins_at(&self, site: &Place) -> Vec<SiteLogin> {
        self.facts()
            .into_iter()
            .filter(|facts| facts.kind == ItemKind::Login && facts.places.contains(site))
            .filter_map(|facts| {
                let username = self.sealed(&facts.id).ok()?.username.clone();
                Some(SiteLogin { facts, username })
            })
            .collect()
    }

    /// Keep `item` tagged with `tags`, which name the app that keeps it for itself and what for,
    /// answering what is safe to say of it. Where `replacing` names a login the vault keeps,
    /// `item` takes its place under the same id: its name and secret change, it belongs to
    /// `item`'s places as well as its own, and it keeps its title, tags and uses. Any other
    /// `replacing` is refused, so nothing kept elsewhere is overwritten by mistake. A new item is
    /// titled `title`, unless that is empty.
    pub fn keep_tagged(
        &mut self,
        tags: &[String],
        replacing: Option<&ItemId>,
        mut item: NewItem,
        title: &str,
    ) -> Result<ItemFacts, ItemError> {
        let before = match replacing {
            Some(id) => {
                let before = self.sealed(id)?;
                if before.kind != item.kind {
                    return Err(ItemError::WrongKind);
                }
                item.places.extend(before.places.iter().cloned());
                Some((id.clone(), before))
            }
            None => None,
        };
        let mut sealed = Sealed::checked(item)?;
        let id = match before {
            Some((id, before)) => {
                sealed.added_unix = before.added_unix;
                sealed.last_used_unix = before.last_used_unix;
                sealed.uses.clone_from(&before.uses);
                sealed.sent_in.clone_from(&before.sent_in);
                sealed.account.clone_from(&before.account);
                id
            }
            None => ItemId::new(),
        };
        let fresh = !self.entries.contains_key(&id);
        self.write(&id, &sealed);
        if let Some((entry, _)) = self.entries.get_mut(&id) {
            entry.tags.extend(tags.iter().cloned());
            if fresh && !title.trim().is_empty() {
                title.trim().clone_into(&mut entry.title);
            }
            let tagged = entry.clone();
            if let Some(VaultChange::Keep(kept)) = self.changes.last_mut() {
                *kept = tagged;
            }
        }
        Ok(self.written(&id, &sealed))
    }

    /// [`Self::add`] for a key that says where it rides in a request to its
    /// service ([`KeyIn`]). Only a key says so.
    pub fn add_key(&mut self, item: NewItem, sent_in: &KeyIn) -> Result<ItemFacts, ItemError> {
        if item.kind != ItemKind::Key {
            return Err(ItemError::Invalid(
                "Only a key says how it is sent with a request",
            ));
        }
        let mut sealed = Sealed::checked(item)?;
        sealed.sent_in = sent_in.kept();
        let id = ItemId::new();
        self.write(&id, &sealed);
        Ok(self.written(&id, &sealed))
    }

    /// Keep `item` as the own sign-in of `account` (a mail or calendar account's id, or a model
    /// provider's), answering what is safe to say of it. Where `replacing` names an entry kept
    /// for that same account, the item takes its place under the same id, with the uses it
    /// remembers; any other entry (one a member keeps, which the account was added with) stays
    /// as it is, and the item is kept as a new one.
    pub fn keep_for(
        &mut self,
        account: &str,
        item: NewItem,
        replacing: Option<&ItemId>,
    ) -> Result<ItemFacts, ItemError> {
        if account.trim().is_empty() {
            return Err(ItemError::Invalid("An account's sign-in names its account"));
        }
        let mut sealed = Sealed::checked(item)?;
        account.clone_into(&mut sealed.account);
        let id = match replacing.filter(|id| self.kept_for(id) == Some(account)) {
            Some(id) => {
                if let Ok(before) = self.sealed(id) {
                    sealed.added_unix = before.added_unix;
                    sealed.last_used_unix = before.last_used_unix;
                    sealed.uses.clone_from(&before.uses);
                }
                id.clone()
            }
            None => ItemId::new(),
        };
        self.write(&id, &sealed);
        Ok(self.written(&id, &sealed))
    }

    /// The account the entry `id` names was kept for as its own sign-in ([`Self::keep_for`]),
    /// or `None` for an entry a member keeps, and for none.
    #[must_use]
    pub fn kept_for(&self, id: &ItemId) -> Option<&str> {
        self.entries
            .get(id)
            .and_then(|(entry, _)| entry.fields.get(ACCOUNT_FIELD))
            .map(String::as_str)
            .filter(|account| !account.is_empty())
    }

    /// Forget the entry `id` names where it was kept as `account`'s own sign-in, answering
    /// whether it was: an entry a member keeps stays when an account that signed in with it
    /// goes.
    pub fn forget_for(&mut self, id: &ItemId, account: &str) -> bool {
        self.kept_for(id) == Some(account) && self.remove(id)
    }

    /// Put `secret` in place of the secret the item `id` names, keeping where it belongs, the
    /// name it signs in with and the uses it remembers: a provider's grant, renewed.
    pub fn renew(&mut self, id: &ItemId, secret: &str) -> Result<(), ItemError> {
        if secret.is_empty() {
            return Err(ItemError::Invalid("An item's secret cannot be empty"));
        }
        if secret.len() > MAX_SECRET_BYTES {
            return Err(ItemError::Invalid("That secret is too long to keep"));
        }
        let mut sealed = self.sealed(id)?;
        sealed.secret.zeroize();
        secret.clone_into(&mut sealed.secret);
        self.write(id, &sealed);
        Ok(())
    }

    /// Forget the item `id` names, answering whether one was kept.
    pub fn remove(&mut self, id: &ItemId) -> bool {
        if self.entries.remove(id).is_none() {
            return false;
        }
        self.changes
            .push(VaultChange::Forget(crate::SecretId(id.as_str().to_owned())));
        true
    }

    /// Hand the item `id` names to the code that signs in at `place`, for
    /// `by` ([`UseRecord::by`]).
    ///
    /// Refused anywhere the item does not belong, and for a code generator,
    /// whose seed never leaves. The use is written down with the release: whoever takes the
    /// secret writes [`Self::take_changes`] first.
    pub fn use_at(&mut self, id: &ItemId, place: &Place, by: &str) -> Result<Released, ItemError> {
        self.release(
            id,
            None,
            std::slice::from_ref(place),
            by,
            UsedFor::default(),
        )
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
        self.release(id, Some(kind), places, by, UsedFor::default())
    }

    /// [`Self::use_as`], with what the item is used for written down beside
    /// when, where and for whom ([`UsedFor`]): the use's word, what is done
    /// in one line, and how its person allowed it.
    pub fn use_for(
        &mut self,
        id: &ItemId,
        kind: ItemKind,
        places: &[Place],
        by: &str,
        purpose: UsedFor<'_>,
    ) -> Result<Released, ItemError> {
        self.release(id, Some(kind), places, by, purpose)
    }

    /// [`Self::use_for`] for whatever kind of sign-in the item is: what an account names as
    /// its sign-in is a password or the grant its provider's page left, and it signs in with
    /// either.
    pub fn use_as_kept(
        &mut self,
        id: &ItemId,
        places: &[Place],
        by: &str,
        purpose: UsedFor<'_>,
    ) -> Result<Released, ItemError> {
        self.release(id, None, places, by, purpose)
    }

    fn release(
        &mut self,
        id: &ItemId,
        kind: Option<ItemKind>,
        places: &[Place],
        by: &str,
        purpose: UsedFor<'_>,
    ) -> Result<Released, ItemError> {
        let mut sealed = self.sealed(id)?;
        sealed.allows(kind, places)?;
        sealed.used_for(unix_now(), places, by, purpose);
        self.write_uses(id, &sealed);
        Ok(Released {
            kind: sealed.kind,
            username: std::mem::take(&mut sealed.username),
            secret: Zeroizing::new(std::mem::take(&mut sealed.secret)),
        })
    }

    /// Hand `part` of the item `id` names to its own person, for them to see or paste, writing
    /// the use down as `by`'s with `purpose`: no place is signed in to, so none is named. A code
    /// generator's seed is never handed out, and a part the item does not keep is not found.
    pub fn show(
        &mut self,
        id: &ItemId,
        part: Part<'_>,
        by: &str,
        purpose: UsedFor<'_>,
    ) -> Result<Zeroizing<String>, ItemError> {
        let mut sealed = self.sealed(id)?;
        let (entry, _) = self.entries.get(id).ok_or(ItemError::NotFound)?;
        let value = match part {
            Part::Secret if sealed.kind == ItemKind::CodeGenerator => {
                return Err(ItemError::NeverReleased);
            }
            Part::Secret if entry.password.is_empty() && sealed.kind == ItemKind::Note => {
                entry.notes.clone()
            }
            Part::Secret => entry.password.clone(),
            Part::Notes => entry.notes.clone(),
            Part::Username => entry.username.clone(),
            Part::Field(label) if label.starts_with(OWN_FIELD_PREFIX) => String::new(),
            Part::Field(label) => entry.fields.get(label).cloned().unwrap_or_default(),
        };
        let value = Zeroizing::new(value);
        if value.is_empty() {
            return Err(ItemError::NotFound);
        }
        sealed.used_for(unix_now(), &[], by, purpose);
        self.write_uses(id, &sealed);
        Ok(value)
    }

    /// The uses the item `id` names remembers, oldest first.
    pub fn uses(&self, id: &ItemId) -> Result<Vec<UseRecord>, ItemError> {
        let mut sealed = self.sealed(id)?;
        Ok(std::mem::take(&mut sealed.uses))
    }

    /// Where the key `id` names rides in a request to its service: a bearer
    /// token unless it said otherwise when it was kept. Nothing is handed
    /// out and nothing is written down. Refused for an item that is no key,
    /// and for a key too short to be sent with a request an agent wrote
    /// ([`MIN_SENT_KEY_BYTES`]).
    pub fn key_in(&self, id: &ItemId) -> Result<KeyIn, ItemError> {
        let sealed = self.sealed(id)?;
        if sealed.kind != ItemKind::Key {
            return Err(ItemError::WrongKind);
        }
        if sealed.secret.len() < MIN_SENT_KEY_BYTES {
            return Err(ItemError::Invalid(
                "That key is too short to be sent with a request and kept out of its answer",
            ));
        }
        KeyIn::parse(&sealed.sent_in).map_err(|_| ItemError::Damaged)
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

    /// What is safe to say of the item `id` names, where it belongs to
    /// `place`: its kind among the rest. Refused anywhere else, as every use
    /// there is. Nothing is handed out and nothing is written down: it is
    /// for whoever must choose a use by the item's kind before it asks.
    pub fn belongs_at(&self, id: &ItemId, place: &Place) -> Result<ItemFacts, ItemError> {
        let sealed = self.sealed(id)?;
        if !sealed.places.contains(place) {
            return Err(ItemError::WrongPlace);
        }
        Ok(sealed.facts(id.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "correct horse battery staple";

    fn place(text: &str) -> Place {
        Place::parse(text).unwrap()
    }

    fn login(places: &[&str]) -> NewItem {
        NewItem {
            kind: ItemKind::Login,
            places: places.iter().map(|text| place(text)).collect(),
            username: "ada@example.test".into(),
            code: CodeParams::default(),
            secret: Zeroizing::new(PASSWORD.into()),
        }
    }

    /// A login an app keeps for itself carries the tags that name the app and what it is for,
    /// and a page at its site is offered it with its name and never its secret; replacing it
    /// changes the secret under the same id, keeps its tags, and adds the new site to its own.
    #[test]
    fn an_apps_login_is_tagged_and_offered_at_its_site() {
        let tags = [
            "app:so.tilo.browser".to_owned(),
            "purpose:sign-in".to_owned(),
        ];
        let mut items = VaultItems::default();
        let kept = items
            .keep_tagged(&tags, None, login(&["https://shop.example"]), "")
            .unwrap();
        assert_eq!(kept.tags, tags);
        assert_eq!(kept.title, "shop.example");
        let theirs = items.add(login(&["https://mail.example"])).unwrap();
        assert!(theirs.tags.is_empty());
        let at_shop = items.logins_at(&place("https://shop.example/login"));
        assert_eq!(
            at_shop.iter().map(|l| &l.facts.id).collect::<Vec<_>>(),
            [&kept.id]
        );
        assert_eq!(at_shop[0].facts.tags, tags, "a listing names its tags");
        assert_eq!(at_shop[0].username, "ada@example.test");
        assert!(!format!("{at_shop:?}").contains("ada@"));
        assert!(
            items
                .logins_at(&place("https://elsewhere.example"))
                .is_empty()
        );
        let written = items.take_changes();
        assert!(
            written.iter().any(|change| matches!(
                change,
                VaultChange::Keep(entry) if entry.id.0 == kept.id.as_str() && entry.tags.len() == 2
            )),
            "the entry is written with its tags"
        );

        let mut again = login(&["https://www.shop.example"]);
        again.username = "ada2@example.test".into();
        again.secret = Zeroizing::new("a newer one".into());
        let replaced = items.keep_tagged(&[], Some(&kept.id), again, "").unwrap();
        assert_eq!(replaced.id, kept.id);
        assert_eq!(replaced.tags, tags);
        assert_eq!(
            items.logins_at(&place("https://www.shop.example"))[0].username,
            "ada2@example.test"
        );
        assert_eq!(
            replaced.places,
            [
                place("https://shop.example"),
                place("https://www.shop.example")
            ]
        );
        let released = items
            .use_as_kept(
                &kept.id,
                &[place("https://www.shop.example")],
                "",
                UsedFor::default(),
            )
            .unwrap();
        assert_eq!(released.secret.as_str(), "a newer one");
        let none = ItemId::parse("0123456789abcdef0123456789abcdef").unwrap();
        assert!(matches!(
            items.keep_tagged(&tags, Some(&none), login(&["https://a.example"]), ""),
            Err(ItemError::NotFound)
        ));
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
        let mut items = VaultItems::default();
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
            VaultItems::of_entries(items.kept()).facts()[0].last_used_unix,
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
        let reopened = VaultItems::of_entries(items.kept()).facts();
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
        assert!(items.remove(&kept.id));
        assert!(!items.remove(&kept.id));
        assert_eq!(
            items
                .use_at(&kept.id, &place("https://www.linkedin.com"), "Helper")
                .err(),
            Some(ItemError::NotFound)
        );
        assert!(VaultItems::of_entries(items.kept()).facts().is_empty());
    }

    /// An account's own sign-in is an entry that names its account: kept again it takes the
    /// place of the one before with the uses it remembers, renewed it keeps where it belongs,
    /// and it goes only with its own account. A member's entry the account signed in with
    /// stays, and is never replaced in place.
    #[test]
    fn an_accounts_own_sign_in_goes_with_its_account_alone() {
        let mut items = VaultItems::default();
        let kept = items
            .keep_for(
                "ada@example.test",
                login(&["imap://mail.example.test:993"]),
                None,
            )
            .unwrap();
        assert_eq!(items.kept_for(&kept.id), Some("ada@example.test"));
        let place = place("imap://mail.example.test:993");
        items.use_at(&kept.id, &place, "").unwrap();

        // Kept again for the same account, the new password takes the old one's place.
        let mut again = login(&["imap://mail.example.test:993"]);
        again.secret = Zeroizing::new("a new one".into());
        let replaced = items
            .keep_for("ada@example.test", again, Some(&kept.id))
            .unwrap();
        assert_eq!(replaced.id, kept.id);
        assert!(replaced.last_used_unix.is_some(), "its uses stay");
        assert_eq!(
            items.use_at(&kept.id, &place, "").unwrap().secret.as_str(),
            "a new one"
        );

        // Renewed, it keeps its place and its name.
        items.renew(&kept.id, "renewed").unwrap();
        let renewed = items.use_at(&kept.id, &place, "").unwrap();
        assert_eq!(renewed.secret.as_str(), "renewed");
        assert_eq!(renewed.username, "ada@example.test");
        assert_eq!(
            VaultItems::of_entries(items.kept()).kept_for(&kept.id),
            Some("ada@example.test"),
            "the account it is kept for reads back from the entry"
        );

        // A member's own entry is never taken over: keeping for an account beside it is a new
        // entry, and the account going leaves it.
        let theirs = items.add(login(&["imap://mail.example.test:993"])).unwrap();
        let beside = items
            .keep_for(
                "ada@example.test",
                login(&["imap://mail.example.test:993"]),
                Some(&theirs.id),
            )
            .unwrap();
        assert_ne!(beside.id, theirs.id);
        assert!(!items.forget_for(&theirs.id, "ada@example.test"));
        assert!(!items.forget_for(&kept.id, "bob@example.test"));
        assert!(items.forget_for(&kept.id, "ada@example.test"));
        assert_eq!(items.facts().len(), 2);
        assert!(
            items
                .keep_for(" ", login(&["imap://mail.example.test:993"]), None)
                .is_err()
        );
    }

    /// What is said of an item carries neither its secret nor its name, in a listing or in a
    /// debug print; the vault's other entries are no items; and a use changes the entry's uses
    /// alone, never the entry a member may be editing.
    #[test]
    fn what_is_said_of_an_item_is_only_what_is_safe() {
        let adding = login(&[
            "imap://mail.example.test:993",
            "smtp://mail.example.test:465",
        ]);
        let printed = format!("{adding:?}");
        assert!(
            !printed.contains(PASSWORD) && !printed.contains("ada@"),
            "{printed}"
        );

        // A note and a Wi-Fi network of Passwords are items of their own kinds, listed with
        // the rest, and neither is handed to anything that signs in.
        let mut note = crate::SecretItem::login("Bank", "ada", "inventory");
        note.kind = SecretItemKind::SecureNote;
        note.origins.insert("https://bank.example".into());
        let mut wifi = crate::SecretItem::login("Home", "", "wifi-password");
        wifi.kind = SecretItemKind::Wifi;
        let (note_id, wifi_id) = (
            ItemId::parse(&note.id.0).unwrap(),
            ItemId::parse(&wifi.id.0).unwrap(),
        );
        let mut items = VaultItems::of_entries([(note, Vec::new()), (wifi, Vec::new())]);
        let listed: Vec<(ItemKind, String)> = items
            .facts()
            .into_iter()
            .map(|facts| (facts.kind, facts.title))
            .collect();
        assert_eq!(listed.len(), 2);
        assert!(listed.contains(&(ItemKind::Note, "Bank".to_owned())));
        assert!(listed.contains(&(ItemKind::Wifi, "Home".to_owned())));
        assert_eq!(
            items
                .use_at(&wifi_id, &place("https://bank.example"), "Helper")
                .err(),
            Some(ItemError::WrongPlace),
            "a Wi-Fi password belongs nowhere a use goes"
        );
        assert!(items.remove(&note_id) && items.remove(&wifi_id));
        items.take_changes();

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
        let changes = items.take_changes();
        let [VaultChange::Keep(entry)] = changes.as_slice() else {
            panic!("an added item is one kept entry: {changes:?}");
        };
        assert_eq!(entry.id.0, kept.id.as_str());
        assert_eq!(entry.kind, SecretItemKind::Login);
        assert_eq!(entry.username, "ada@example.test");
        assert_eq!(entry.password, PASSWORD);
        let printed = format!("{changes:?}");
        assert!(!printed.contains(PASSWORD), "{printed}");

        let released = items
            .use_at(&kept.id, &place("smtp://mail.example.test:465"), "Helper")
            .unwrap();
        let printed = format!("{released:?}");
        assert!(
            !printed.contains(PASSWORD) && !printed.contains("ada@"),
            "{printed}"
        );
        let changes = items.take_changes();
        assert!(
            matches!(changes.as_slice(), [VaultChange::Uses(id, uses)] if id.0 == kept.id.as_str() && uses.len() == 1),
            "a use is written down as the entry's uses alone: {changes:?}"
        );
        // The entries read back as the same items, with the use.
        let reread = VaultItems::of_entries(items.kept());
        assert_eq!(reread.facts().len(), 1);
        assert!(reread.facts()[0].last_used_unix.is_some());
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
        let mut items = VaultItems::default();
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
            VaultItems::of_entries(items.kept()).facts()[0].last_used_unix,
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
    }

    /// Its own person is shown an item's secret, its note or a field of it, wherever it belongs
    /// or none, and each showing is written down as a use; a code generator's seed is never
    /// shown, and a part the item does not keep is not found.
    #[test]
    fn its_person_is_shown_each_part_and_each_showing_is_a_use() {
        let mut items = VaultItems::default();
        let kept = items
            .add_with(
                item(ItemKind::Wifi, &[], "Home-Net-Pass-2024"),
                None,
                &Extras {
                    title: "Home".into(),
                    notes: Zeroizing::new("router in the hall".into()),
                    fields: vec![("network".into(), Zeroizing::new("home-5g".into()))],
                    tags: Vec::new(),
                },
            )
            .unwrap();
        let showing = UsedFor {
            using: "show",
            what: "Shown in Passwords",
            allowed: None,
        };
        for (part, said) in [
            (Part::Secret, "Home-Net-Pass-2024"),
            (Part::Notes, "router in the hall"),
            (Part::Field("network"), "home-5g"),
        ] {
            let shown = items.show(&kept.id, part, "Passwords", showing).unwrap();
            assert_eq!(shown.as_str(), said, "{part:?}");
        }
        let uses = items.uses(&kept.id).unwrap();
        assert_eq!(uses.len(), 3, "each showing is a use");
        assert!(
            uses.iter()
                .all(|used| used.places.is_empty() && used.by == "Passwords")
        );
        for part in [Part::Field("PIN"), Part::Field(KIND_FIELD)] {
            assert_eq!(
                items.show(&kept.id, part, "Passwords", showing).err(),
                Some(ItemError::NotFound),
                "{part:?}"
            );
        }
        let generator = items
            .add(item(
                ItemKind::CodeGenerator,
                &["https://example.com"],
                SEED,
            ))
            .unwrap();
        assert_eq!(
            items
                .show(&generator.id, Part::Secret, "Passwords", showing)
                .err(),
            Some(ItemError::NeverReleased)
        );
        assert_eq!(
            items.uses(&kept.id).unwrap().len(),
            3,
            "a refusal is no use"
        );
    }

    /// A person's edit changes an item in place: what they wrote anew is kept, each secret part
    /// they left empty stays, and the item keeps its id, when it was added and its uses.
    #[test]
    fn an_edit_keeps_what_it_leaves_empty_and_the_items_uses() {
        let mut items = VaultItems::default();
        let mut login = item(
            ItemKind::Login,
            &["https://bank.example"],
            "Old-Bank-Pass-2024",
        );
        login.username = "ada".into();
        let kept = items
            .add_with(
                login,
                None,
                &Extras {
                    title: "Bank".into(),
                    notes: Zeroizing::new("branch 12".into()),
                    fields: vec![
                        ("PIN".into(), Zeroizing::new("4711".into())),
                        ("TAN list".into(), Zeroizing::new("A1".into())),
                    ],
                    tags: vec!["money".into()],
                },
            )
            .unwrap();
        let showing = UsedFor {
            using: "show",
            what: "Shown in Passwords",
            allowed: None,
        };
        items
            .show(&kept.id, Part::Secret, "Passwords", showing)
            .unwrap();

        let changed = items
            .change(
                &kept.id,
                item(
                    ItemKind::Login,
                    &["https://bank.example", "https://app.bank.example"],
                    "",
                ),
                None,
                &Extras {
                    title: "My bank".into(),
                    notes: Zeroizing::new(String::new()),
                    fields: vec![("PIN".into(), Zeroizing::new(String::new()))],
                    tags: Vec::new(),
                },
            )
            .unwrap();
        assert_eq!(changed.id, kept.id);
        assert_eq!(changed.places.len(), 2);
        assert_eq!(
            changed.field_labels,
            ["PIN"],
            "a field not named again goes"
        );
        assert!(changed.tags.is_empty());
        for (part, said) in [
            (Part::Secret, "Old-Bank-Pass-2024"),
            (Part::Username, "ada"),
            (Part::Notes, "branch 12"),
            (Part::Field("PIN"), "4711"),
        ] {
            let shown = items.show(&kept.id, part, "Passwords", showing).unwrap();
            assert_eq!(shown.as_str(), said, "{part:?} stays as it was");
        }
        assert_eq!(items.uses(&kept.id).unwrap().len(), 5, "its uses stay");

        let mut renewed = item(ItemKind::Login, &[], "New-Bank-Pass-2025");
        renewed.username = "ada.l".into();
        items
            .change(&kept.id, renewed, None, &Extras::default())
            .unwrap();
        // A note left out stays, as a title or a field would.
        for (part, said) in [
            (Part::Secret, "New-Bank-Pass-2025"),
            (Part::Username, "ada.l"),
            (Part::Notes, "branch 12"),
        ] {
            let shown = items.show(&kept.id, part, "Passwords", showing).unwrap();
            assert_eq!(shown.as_str(), said, "{part:?}");
        }
        assert_eq!(
            items
                .change(
                    &ItemId::new(),
                    item(ItemKind::Login, &[], ""),
                    None,
                    &Extras::default()
                )
                .err(),
            Some(ItemError::NotFound)
        );
    }

    /// What is not an item of its kind, or belongs where its kind does not,
    /// is refused before anything is kept.
    #[test]
    fn what_is_not_an_item_of_its_kind_is_refused() {
        let mut items = VaultItems::default();
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
            items.take_changes().is_empty(),
            "a refused item writes nothing"
        );
    }

    /// A use that needs one kind at several places is refused unless the
    /// item is that kind and belongs to each of them, and every use an item
    /// was handed out for is remembered without what was handed.
    #[test]
    fn a_use_names_its_kind_and_every_place_and_is_remembered() {
        let mut items = VaultItems::default();
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
        let uses = VaultItems::of_entries(items.kept()).uses(&kept.id).unwrap();
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
    }

    /// Whether an item fits a use is said without using it: what a use
    /// would be refused for is refused here the same, nothing is handed out
    /// and nothing is written down.
    /// What an item was used for is written down beside when, where and for
    /// whom: the use's word, one line of what was done, and how its person
    /// allowed it. The line is kept up to a `?` or a `#`, so no query of an
    /// address is written, and a line that holds the secret is not kept.
    #[test]
    fn what_an_item_was_used_for_is_written_down_without_its_secret_or_a_query() {
        let mut items = VaultItems::default();
        let service = [place("https://api.example.test")];
        let kept = items.add(login(&["https://api.example.test"])).unwrap();
        let call = UsedFor {
            using: " tilo.vault/web-call\n",
            what: "GET /v1/balance?account=7&token=in-the-query#fragment-kept-nowhere",
            allowed: Some(Allowance::Once),
        };
        let released = items
            .use_for(&kept.id, ItemKind::Login, &service, "Scout", call)
            .unwrap();
        assert_eq!(released.secret.as_str(), PASSWORD);
        let holding = format!("POST /v1/{PASSWORD}/charges");
        let leaking = UsedFor {
            using: "tilo.vault/web-call",
            what: &holding,
            allowed: Some(Allowance::Always),
        };
        items
            .use_for(&kept.id, ItemKind::Login, &service, "Scout", leaking)
            .unwrap();
        items
            .use_as(&kept.id, ItemKind::Login, &service, "Scout")
            .unwrap();

        // Read again from the sealed file.
        let uses = VaultItems::of_entries(items.kept()).uses(&kept.id).unwrap();
        assert_eq!(uses.len(), 3);
        assert_eq!(uses[0].using, "tilo.vault/web-call");
        assert_eq!(uses[0].what, "GET /v1/balance");
        assert_eq!(uses[0].allowed, Some(Allowance::Once));
        assert_eq!(uses[0].by, "Scout");
        assert_eq!(uses[0].places, service);
        assert_eq!(uses[1].using, "tilo.vault/web-call");
        assert_eq!(uses[1].what, "", "a line that holds the secret is not kept");
        assert_eq!(uses[1].allowed, Some(Allowance::Always));
        assert_eq!((uses[2].using.as_str(), uses[2].what.as_str()), ("", ""));
        assert_eq!(uses[2].allowed, None);
        let said = format!("{uses:?}");
        for unwritten in [
            PASSWORD,
            "account=7",
            "in-the-query",
            "fragment-kept-nowhere",
        ] {
            assert!(!said.contains(unwritten), "{unwritten}: {said}");
        }

        // A use that says nothing more is sealed as it was before any did.
        let plain = serde_json::to_value(&uses[2]).unwrap();
        let mut named: Vec<&str> = plain
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        named.sort_unstable();
        assert_eq!(named, ["at_unix", "by", "places"]);
        let sealed = serde_json::to_value(&uses[0]).unwrap();
        assert_eq!(sealed["allowed"], "once");
        assert_eq!(Allowance::Once.word(), "once");
        assert_eq!(Allowance::Always.word(), "always");

        // What is longer than a line is cut to one.
        let long = "x".repeat(1_000);
        let wordy = UsedFor {
            using: &long,
            what: &long,
            allowed: None,
        };
        items
            .use_for(&kept.id, ItemKind::Login, &service, "", wordy)
            .unwrap();
        let uses = items.uses(&kept.id).unwrap();
        assert_eq!(uses[3].using.len(), MAX_USING_CHARS);
        assert_eq!(uses[3].what.len(), MAX_WHAT_CHARS);
    }

    #[test]
    fn whether_an_item_fits_a_use_is_said_without_using_it() {
        let mut items = VaultItems::default();
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
    }

    /// What an item is is said only where it belongs, and saying it uses
    /// nothing.
    #[test]
    fn what_an_item_is_is_said_only_where_it_belongs() {
        let mut items = VaultItems::default();
        let kept = items.add(login(&["https://example.com"])).unwrap();
        let said = items
            .belongs_at(&kept.id, &place("https://example.com"))
            .unwrap();
        assert_eq!(said.kind, ItemKind::Login);
        assert_eq!(said.id, kept.id);
        for elsewhere in [
            "https://login.example.com",
            "http://example.com",
            "https://example.com:8443",
        ] {
            assert_eq!(
                items.belongs_at(&kept.id, &place(elsewhere)).err(),
                Some(ItemError::WrongPlace),
                "{elsewhere}"
            );
        }
        let nobody = ItemId::parse(&"0".repeat(32)).unwrap();
        assert_eq!(
            items
                .belongs_at(&nobody, &place("https://example.com"))
                .err(),
            Some(ItemError::NotFound)
        );
        assert!(items.uses(&kept.id).unwrap().is_empty());
    }
}

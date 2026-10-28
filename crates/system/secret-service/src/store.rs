//! The store: the vault, sealed at rest under a key the person's own key
//! releases, and every operation on it gated by the caller's identity.
//!
//! # Locked and open
//!
//! The store is **locked** until its key can be derived. The key comes from
//! the person's own key — the identity the machine knows them by, whose
//! private half only their login releases (see [`UserKey`]) — so the vault
//! belongs to the person rather than to this machine: the same person on
//! another device opens the same store, and a home copied onto another
//! machine opens it too, because that is where the person's key travels.
//! Nothing about the login method reaches this module, and there is no
//! prompt: a store whose person key is unavailable stays locked and every
//! operation says so, until it appears.
//!
//! A vault that exists but cannot be opened (a different key or salt, or
//! damage) is never replaced: the store stays locked with the reason, and
//! the file is left as it is. Nor is it ever sealed under a second key: a
//! vault [`Locked::KeyChanged`] stays exactly as it was sealed.
//!
//! # Attribution
//!
//! Every operation names its caller. The store records each identity in a
//! ledger on first use, refuses a revoked one, and lets an identity see
//! only the items [`Owner::admits`] it.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tilo_seal::secret_service::CallerKey;
use tilo_seal::{Context, Label, SymmetricKey, envelope};
use tilo_user_identity::persist::{IdentityPaths, PersistError, load_device_key, read_signing_key};
use zeroize::Zeroizing;

use crate::app::AppId;
use crate::vault::{
    Access, AppRecord, Collection, Item, LOGIN_COLLECTION, Migration, Owner, SESSION_COLLECTION,
    SecretBytes, VAULT_VERSION, Vault,
};

/// The record a vault envelope is bound to.
const RECORD: &[u8] = b"vault-v1";
/// A ledger entry's `last_used` is written to disk at most this often.
const FLUSH_SECONDS: u64 = 300;
const MAX_LABEL: usize = 1024;
const MAX_ATTRIBUTES: usize = 64;
const MAX_ATTRIBUTE_TEXT: usize = 1024;
const MAX_SECRET: usize = 1 << 20;
const MAX_ITEMS: usize = 50_000;
const MAX_COLLECTIONS: usize = 256;
/// The length of a portal master secret.
pub const SANDBOX_SECRET_LEN: usize = 32;

/// Why the store is locked.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Locked {
    /// The person's key is not available yet, or this session cannot read
    /// it. The machine relays ciphertext only until the person signs in.
    #[error("the person's key is not available")]
    NoIdentity,
    /// The vault was sealed under another key: another person's key, or
    /// another salt.
    #[error("the vault belongs to another key")]
    KeyChanged,
    /// The vault file does not authenticate.
    #[error("the vault is damaged")]
    Corrupt,
    #[error("the vault cannot be read: {0}")]
    Unreadable(String),
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Locked(#[from] Locked),
    /// The caller was denied use of the store in Settings.
    #[error("access for this application was revoked")]
    Revoked,
    #[error("no such object")]
    NoSuchObject,
    #[error("invalid argument: {0}")]
    Invalid(&'static str),
    #[error("the vault could not be written: {0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Where the store's key comes from.
pub trait KeySource: Send + Sync {
    /// The key, or why it cannot be released now.
    fn release(&self) -> std::result::Result<SymmetricKey, Locked>;

    /// An equivalent source, for a store that keeps one handle for the
    /// vault's current seal and hands `release` to another. Every source is
    /// a handle onto something external — a file, a loaded secret — so this
    /// copies the handle, not key material.
    fn another(&self) -> Box<dyn KeySource>;

    /// The key a caller the store cannot examine proves it holds the
    /// device identity with ([`crate::app`]). A source with no device
    /// identity behind it has none, and such callers stay refused.
    fn caller_key(&self) -> std::result::Result<CallerKey, Locked> {
        Err(Locked::NoIdentity)
    }
}

/// The key derived from the person's own key, their salt and their public
/// identity (`tilo_seal::secret_service::user_store_key`).
///
/// The person's private key is the root this machine knows them by; it is
/// written into every session's identity tree, and only that person's login
/// releases it. The same key on another device derives the same store.
pub struct UserKey {
    paths: IdentityPaths,
    salt_path: PathBuf,
    /// The public half of the person's key, 32 bytes. It names the person in
    /// the derivation, so two people sharing a data directory never share a
    /// store. `None` on a machine no person has been set up on yet.
    identity: Option<[u8; 32]>,
}

impl UserKey {
    /// `paths` hold the person's key pair; `salt_path` is a file in the
    /// person's own data directory; `identity` is the public half of their
    /// key as the session knows it, or `None` when this machine knows no
    /// person.
    #[must_use]
    pub fn new(paths: IdentityPaths, salt_path: PathBuf, identity: Option<[u8; 32]>) -> Self {
        Self {
            paths,
            salt_path,
            identity,
        }
    }
}

impl UserKey {
    /// The person's private key, read with the owner-only checks every
    /// other reader of it applies.
    fn user_secret(&self) -> std::result::Result<Zeroizing<[u8; 32]>, Locked> {
        let key = read_signing_key(&self.paths.user_privkey).map_err(|error| match error {
            PersistError::Io(io) if io.kind() == io::ErrorKind::NotFound => Locked::NoIdentity,
            other => Locked::Unreadable(other.to_string()),
        })?;
        Ok(Zeroizing::new(key.to_bytes()))
    }
}

impl KeySource for UserKey {
    fn another(&self) -> Box<dyn KeySource> {
        Box::new(Self::new(
            self.paths.clone(),
            self.salt_path.clone(),
            self.identity,
        ))
    }

    fn release(&self) -> std::result::Result<SymmetricKey, Locked> {
        let identity = self.identity.ok_or(Locked::NoIdentity)?;
        let secret = self.user_secret()?;
        let salt = load_or_create_salt(&self.salt_path)
            .map_err(|error| Locked::Unreadable(format!("salt: {error}")))?;
        tilo_seal::secret_service::user_store_key(secret.as_ref(), &salt, &identity)
            .map_err(|error| Locked::Unreadable(error.to_string()))
    }
}

/// The key derived from the machine's device identity and the person's own
/// salt (`tilo_seal::secret_service::store_key`).
///
/// This build seals nothing with it. It answers two things still: a caller
/// the store cannot examine proves it holds this machine's identity with the
/// key ([`KeySource::caller_key`]), and a vault an earlier build sealed this
/// way is opened through it once and copied under the person's key, so a
/// machine upgraded in place does not lose what the person saved before.
#[derive(Clone)]
pub struct DeviceIdentityKey {
    paths: IdentityPaths,
    salt_path: PathBuf,
    account: String,
}

impl DeviceIdentityKey {
    /// `account` names the person (the numeric user id); `salt_path` is a
    /// file in the person's own data directory.
    #[must_use]
    pub fn new(paths: IdentityPaths, salt_path: PathBuf, account: &str) -> Self {
        Self {
            paths,
            salt_path,
            account: account.to_owned(),
        }
    }
}

impl DeviceIdentityKey {
    fn device_seed(&self) -> std::result::Result<Zeroizing<[u8; 32]>, Locked> {
        let device = load_device_key(&self.paths).map_err(|error| match error {
            PersistError::Io(io) if io.kind() == io::ErrorKind::NotFound => Locked::NoIdentity,
            other => Locked::Unreadable(other.to_string()),
        })?;
        Ok(Zeroizing::new(device.to_bytes()))
    }
}

impl KeySource for DeviceIdentityKey {
    fn another(&self) -> Box<dyn KeySource> {
        Box::new(Self::new(
            self.paths.clone(),
            self.salt_path.clone(),
            &self.account,
        ))
    }

    fn caller_key(&self) -> std::result::Result<CallerKey, Locked> {
        CallerKey::derive(self.device_seed()?.as_ref())
            .map_err(|error| Locked::Unreadable(error.to_string()))
    }

    fn release(&self) -> std::result::Result<SymmetricKey, Locked> {
        let seed = self.device_seed()?;
        let salt = load_or_create_salt(&self.salt_path)
            .map_err(|error| Locked::Unreadable(format!("salt: {error}")))?;
        tilo_seal::secret_service::store_key(seed.as_ref(), &salt, self.account.as_bytes())
            .map_err(|error| Locked::Unreadable(error.to_string()))
    }
}

/// The person's salt: minted once, never replaced. A salt that exists but
/// is unreadable or malformed is an error, because a new one would orphan
/// the vault.
fn load_or_create_salt(path: &Path) -> io::Result<Zeroizing<[u8; 32]>> {
    use std::io::Read;
    let read = |path: &Path| -> io::Result<Zeroizing<[u8; 32]>> {
        let mut bytes = Zeroizing::new(Vec::with_capacity(32));
        tilo_files::native::private::read(path)?
            .take(33)
            .read_to_end(&mut bytes)?;
        let mut salt = Zeroizing::new([0_u8; 32]);
        if bytes.len() != salt.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the salt has the wrong length",
            ));
        }
        salt.copy_from_slice(&bytes);
        Ok(salt)
    };
    match read(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        existing => return existing,
    }
    tilo_files::native::private::directory(path.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
    let mut fresh = Zeroizing::new([0_u8; 32]);
    tilo_seal::fill_random(fresh.as_mut()).map_err(io::Error::other)?;
    // `false`: a concurrent starter published first; use theirs.
    tilo_files::native::private::create_atomic(path, fresh.as_ref())?;
    read(path)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// An item's place in the vault.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ItemRef {
    pub collection: String,
    pub id: String,
}

/// What an item says about itself, never its secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemInfo {
    pub label: String,
    pub attributes: BTreeMap<String, String>,
    pub created: u64,
    pub modified: u64,
}

pub struct NewItem {
    pub label: String,
    pub attributes: BTreeMap<String, String>,
    pub secret: Vec<u8>,
    pub content_type: String,
}

/// One row of the ledger as Settings lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppInfo {
    pub id: AppId,
    pub name: String,
    pub access: Access,
    pub first_seen: u64,
    pub last_used: u64,
    pub uses: u64,
    pub items: u32,
}

/// An item of an older keyring, to be imported.
pub struct LegacyItem {
    pub label: String,
    pub attributes: BTreeMap<String, String>,
    pub secret: Vec<u8>,
    pub content_type: String,
    pub created: u64,
    pub modified: u64,
}

struct Open {
    key: SymmetricKey,
    vault: Vault,
    dirty: bool,
    now: u64,
}

#[derive(Default)]
struct Inner {
    open: Option<Open>,
}

pub struct Store {
    path: PathBuf,
    keys: Box<dyn KeySource>,
    /// The key every vault written from now on is sealed under. A vault the
    /// store opened under any other key (`previous`) is resealed under this
    /// one, once.
    key: Box<dyn KeySource>,
    /// The machine's device identity, which is what a caller the store
    /// cannot examine proves. Sealing the vault and proving a caller are
    /// different questions asked of different keys.
    caller: Box<dyn KeySource>,
    inner: Mutex<Inner>,
}

fn context() -> Context<'static> {
    Context::new(Label::SecretService, RECORD)
}

impl Store {
    /// A store over the vault file `path`, sealed and opened under `keys`.
    /// Nothing is read until first use.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, keys: Box<dyn KeySource>) -> Self {
        let caller = keys.another();
        Self::opening(path, keys.another(), keys, caller)
    }

    /// A store over `path` that seals vaults under `keys`, takes callers it
    /// cannot examine to `caller` (the device identity), and also opens a
    /// vault `previous` sealed under an earlier key. See
    /// [`Store::open_vault_keyed_by`].
    #[must_use]
    pub fn keyed_by(
        path: impl Into<PathBuf>,
        keys: Box<dyn KeySource>,
        previous: Box<dyn KeySource>,
        caller: Box<dyn KeySource>,
    ) -> Self {
        Self::opening(path, previous, keys, caller)
    }

    /// A store that also opens a vault `previous` sealed under an earlier
    /// key (`tilo_seal::secret_service::store_key`), and reseals it under
    /// `keys`' key on the first use that succeeds.
    ///
    /// The migration is a copy and a verify: the vault is opened with the
    /// earlier key, written whole under the current one beside it, and only
    /// a write that reads back is renamed over `path`. Nothing is rewritten
    /// in place, no collection or item id changes, and until the new file is
    /// whole the old one is the store. Until the person signs in, `previous`
    /// cannot release anything either, and the machine relays the ciphertext
    /// untouched.
    #[must_use]
    pub fn open_vault_keyed_by(
        path: impl Into<PathBuf>,
        keys: Box<dyn KeySource>,
        previous: Box<dyn KeySource>,
    ) -> Self {
        let caller = keys.another();
        Self::opening(path, previous, keys, caller)
    }

    fn opening(
        path: impl Into<PathBuf>,
        sealed_under: Box<dyn KeySource>,
        keys: Box<dyn KeySource>,
        caller: Box<dyn KeySource>,
    ) -> Self {
        Self {
            path: path.into(),
            key: sealed_under,
            keys,
            caller,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The key callers prove they hold the device identity with.
    pub fn caller_key(&self) -> std::result::Result<CallerKey, Locked> {
        self.caller.caller_key()
    }

    /// Why the store is locked, or `None` when it is open. Tries to open it.
    pub fn locked(&self) -> Option<Locked> {
        self.with_open(|_| Ok(()))
            .err()
            .and_then(|error| match error {
                StoreError::Locked(locked) => Some(locked),
                _ => None,
            })
    }

    fn open_now(&self) -> std::result::Result<Open, Locked> {
        let now = now();
        let sealed = match std::fs::read(&self.path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(Locked::Unreadable(error.to_string())),
        };
        let Some(sealed) = sealed else {
            // A fresh vault is written on the first change, not now:
            // opening must not create state.
            return Ok(Open {
                key: self.keys.release()?,
                vault: Vault::new(now),
                dirty: false,
                now,
            });
        };

        // The current key first: that is the vault the person owns.
        if let Some(vault) = Self::opens_with(&*self.keys, &sealed)? {
            return Ok(Open {
                key: self.keys.release()?,
                vault,
                dirty: false,
                now,
            });
        }
        // Then the key an earlier build sealed it under: copy it whole under
        // the current key, keep the old file until the new one reads back,
        // and switch. Every id survives: the plaintext is written as it was
        // read.
        if let Some(vault) = Self::opens_with(&*self.key, &sealed)? {
            let open = Open {
                key: self.keys.release()?,
                vault,
                dirty: false,
                now,
            };
            // A copy that cannot be written leaves the old vault as the
            // store, and says so: nothing was lost, nothing was switched.
            self.commit(&open).map_err(|error| match error {
                StoreError::Io(why) => Locked::Unreadable(format!("resealing the vault: {why}")),
                other => Locked::Unreadable(other.to_string()),
            })?;
            return Ok(open);
        }
        Err(Locked::KeyChanged)
    }

    /// The vault `sealed` holds under `source`'s key, or `None` when that
    /// key does not open it.
    fn opens_with(
        source: &dyn KeySource,
        sealed: &[u8],
    ) -> std::result::Result<Option<Vault>, Locked> {
        let key = match source.release() {
            Ok(key) => key,
            Err(Locked::NoIdentity) => return Ok(None),
            Err(other) => return Err(other),
        };
        let plain = match envelope::open(&key, context(), sealed) {
            Ok(plain) => plain,
            // Another key: not this source's vault.
            Err(tilo_seal::Error::WrongKey) => return Ok(None),
            Err(_) => return Err(Locked::Corrupt),
        };
        let vault: Vault = postcard::from_bytes(&plain).map_err(|_| Locked::Corrupt)?;
        if vault.version != VAULT_VERSION {
            return Err(Locked::Unreadable(format!(
                "vault version {} is not supported",
                vault.version
            )));
        }
        Ok(Some(vault))
    }

    fn with_open<R>(&self, work: impl FnOnce(&mut Open) -> Result<R>) -> Result<R> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| StoreError::Io("store lock poisoned".into()))?;
        if inner.open.is_none() {
            inner.open = Some(self.open_now()?);
        }
        let open = inner.open.as_mut().ok_or(Locked::NoIdentity)?;
        open.now = now();
        let result = work(open);
        if open.dirty {
            // A change that cannot be saved is reported, and stays pending
            // so the next operation tries again.
            self.commit(open)?;
            open.dirty = false;
        }
        drop(inner);
        result
    }

    /// Write the vault beside `path` under the open key, read the copy back,
    /// and only then rename it over the store.
    fn commit(&self, open: &Open) -> Result<()> {
        let plain = Zeroizing::new(
            postcard::to_allocvec(&open.vault.persistable())
                .map_err(|error| StoreError::Io(error.to_string()))?,
        );
        let sealed = envelope::seal(&open.key, context(), &plain)
            .map_err(|error| StoreError::Io(error.to_string()))?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| StoreError::Io("no parent directory".into()))?;
        tilo_files::native::private::directory(parent)
            .map_err(|error| StoreError::Io(error.to_string()))?;
        let written = self.write_beside(&sealed)?;
        let copied = std::fs::read(&written).map_err(|error| {
            let _ = std::fs::remove_file(&written);
            StoreError::Io(error.to_string())
        })?;
        // The copy must open under the key that sealed it and hold the same
        // vault, or it never replaces the store it was copied from.
        let round_trip = envelope::open(&open.key, context(), &copied)
            .ok()
            .and_then(|plain| postcard::from_bytes::<Vault>(&plain).ok())
            .is_some_and(|vault| vault == open.vault);
        if !round_trip {
            let _ = std::fs::remove_file(&written);
            return Err(StoreError::Io("the written vault did not read back".into()));
        }
        std::fs::rename(&written, &self.path).map_err(|error| {
            let _ = std::fs::remove_file(&written);
            StoreError::Io(error.to_string())
        })
    }

    /// `sealed` in a fresh file beside the store, so the store itself is
    /// never rewritten in place.
    fn write_beside(&self, sealed: &[u8]) -> Result<PathBuf> {
        let name = self
            .path
            .file_name()
            .ok_or_else(|| StoreError::Io("no file name".into()))?;
        let mut fresh = name.to_os_string();
        fresh.push(format!(".new-{}", random_id()?));
        let beside = self.path.with_file_name(fresh);
        tilo_files::native::private::write_atomic(&beside, sealed)
            .map_err(|error| StoreError::Io(error.to_string()))?;
        Ok(beside)
    }

    /// Record `caller` in the ledger and refuse it if it was revoked.
    fn admit(open: &mut Open, caller: &AppId) -> Result<()> {
        let now = open.now;
        let record = open.vault.apps.entry(caller.clone()).or_insert_with(|| {
            open.dirty = true;
            AppRecord {
                first_seen: now,
                last_used: now,
                uses: 0,
                access: Access::Allowed,
            }
        });
        if record.access == Access::Revoked {
            return Err(StoreError::Revoked);
        }
        record.uses = record.uses.saturating_add(1);
        if now.saturating_sub(record.last_used) >= FLUSH_SECONDS {
            open.dirty = true;
        }
        record.last_used = now;
        Ok(())
    }

    /// Every collection: its name and label.
    ///
    /// # Errors
    ///
    /// [`StoreError::Locked`] while locked.
    pub fn collections(&self) -> Result<Vec<(String, String)>> {
        self.with_open(|open| {
            Ok(open
                .vault
                .collections
                .iter()
                .map(|(name, c)| (name.clone(), c.label.clone()))
                .collect())
        })
    }

    /// The collection an alias names, or the alias if it is a collection.
    ///
    /// # Errors
    ///
    /// [`StoreError::NoSuchObject`] if neither exists.
    pub fn read_alias(&self, alias: &str) -> Result<String> {
        self.with_open(|open| {
            let name = open.vault.resolve(alias);
            open.vault
                .collections
                .contains_key(name)
                .then(|| name.to_owned())
                .ok_or(StoreError::NoSuchObject)
        })
    }

    /// Point `alias` at `collection`.
    ///
    /// # Errors
    ///
    /// [`StoreError::NoSuchObject`] for an unknown collection;
    /// [`StoreError::Invalid`] for a bad alias.
    pub fn set_alias(&self, caller: &AppId, alias: &str, collection: &str) -> Result<()> {
        valid_name(alias)?;
        self.with_open(|open| {
            Self::admit(open, caller)?;
            if !open.vault.collections.contains_key(collection) {
                return Err(StoreError::NoSuchObject);
            }
            open.vault
                .aliases
                .insert(alias.to_owned(), collection.to_owned());
            open.dirty = true;
            Ok(())
        })
    }

    /// Create a collection with `label` and optionally an alias.
    ///
    /// # Errors
    ///
    /// [`StoreError::Invalid`] for an over-long label or too many
    /// collections.
    pub fn create_collection(
        &self,
        caller: &AppId,
        label: &str,
        alias: Option<&str>,
    ) -> Result<String> {
        if label.len() > MAX_LABEL {
            return Err(StoreError::Invalid("label too long"));
        }
        if let Some(alias) = alias.filter(|alias| !alias.is_empty()) {
            valid_name(alias)?;
        }
        self.with_open(|open| {
            Self::admit(open, caller)?;
            if let Some(existing) = alias.and_then(|alias| open.vault.aliases.get(alias)) {
                return Ok(existing.clone());
            }
            if open.vault.collections.len() >= MAX_COLLECTIONS {
                return Err(StoreError::Invalid("too many collections"));
            }
            let name = random_id()?;
            open.vault.collections.insert(
                name.clone(),
                Collection {
                    label: label.to_owned(),
                    created: open.now,
                    modified: open.now,
                    transient: false,
                    items: BTreeMap::new(),
                },
            );
            if let Some(alias) = alias.filter(|alias| !alias.is_empty()) {
                open.vault.aliases.insert(alias.to_owned(), name.clone());
            }
            open.dirty = true;
            Ok(name)
        })
    }

    /// Delete a collection the caller can see every item of. The login and
    /// session collections cannot be deleted.
    ///
    /// # Errors
    ///
    /// [`StoreError::Invalid`] for a built-in collection or one holding
    /// items the caller may not see.
    pub fn delete_collection(&self, caller: &AppId, name: &str) -> Result<()> {
        if name == LOGIN_COLLECTION || name == SESSION_COLLECTION {
            return Err(StoreError::Invalid(
                "built-in collections cannot be deleted",
            ));
        }
        self.with_open(|open| {
            Self::admit(open, caller)?;
            let collection = open
                .vault
                .collections
                .get(name)
                .ok_or(StoreError::NoSuchObject)?;
            if collection
                .items
                .values()
                .any(|item| !item.owner.admits(caller))
            {
                return Err(StoreError::Invalid(
                    "the collection holds items of other applications",
                ));
            }
            open.vault.collections.remove(name);
            open.vault.aliases.retain(|_, target| target != name);
            open.dirty = true;
            Ok(())
        })
    }

    /// The items every attribute of `query` matches, among those `caller`
    /// may see, in `collection` or in all of them.
    ///
    /// # Errors
    ///
    /// [`StoreError::Locked`] or [`StoreError::Revoked`].
    pub fn search(
        &self,
        caller: &AppId,
        collection: Option<&str>,
        query: &BTreeMap<String, String>,
    ) -> Result<Vec<ItemRef>> {
        self.with_open(|open| {
            Self::admit(open, caller)?;
            let wanted = collection.map(|name| open.vault.resolve(name).to_owned());
            let mut found = Vec::new();
            for (name, collection) in &open.vault.collections {
                if wanted.as_ref().is_some_and(|wanted| wanted != name) {
                    continue;
                }
                for (id, item) in &collection.items {
                    let matches = query
                        .iter()
                        .all(|(key, value)| item.attributes.get(key) == Some(value));
                    if matches && item.owner.admits(caller) {
                        found.push(ItemRef {
                            collection: name.clone(),
                            id: id.clone(),
                        });
                    }
                }
            }
            Ok(found)
        })
    }

    /// Store an item, replacing the caller's visible item with the same
    /// attributes when `replace` is set.
    ///
    /// # Errors
    ///
    /// [`StoreError::Invalid`] for oversized input.
    pub fn create_item(
        &self,
        caller: &AppId,
        collection: &str,
        new: NewItem,
        replace: bool,
    ) -> Result<ItemRef> {
        check_item(&new.label, &new.attributes, new.secret.len())?;
        self.with_open(|open| {
            Self::admit(open, caller)?;
            let name = open.vault.resolve(collection).to_owned();
            let now = open.now;
            let target = open
                .vault
                .collections
                .get_mut(&name)
                .ok_or(StoreError::NoSuchObject)?;
            let existing = replace
                .then(|| {
                    target
                        .items
                        .iter()
                        .find(|(_, item)| {
                            item.owner.admits(caller) && item.attributes == new.attributes
                        })
                        .map(|(id, _)| id.clone())
                })
                .flatten();
            let id = if let Some(id) = existing {
                id
            } else {
                if target.items.len() >= MAX_ITEMS {
                    return Err(StoreError::Invalid("too many items"));
                }
                random_id()?
            };
            let created = target.items.get(&id).map_or(now, |item| item.created);
            target.items.insert(
                id.clone(),
                Item {
                    label: new.label,
                    attributes: new.attributes,
                    secret: SecretBytes::new(new.secret),
                    content_type: new.content_type,
                    created,
                    modified: now,
                    owner: Owner::App(caller.clone()),
                },
            );
            target.modified = now;
            open.dirty = !target.transient || open.dirty;
            Ok(ItemRef {
                collection: name,
                id,
            })
        })
    }

    /// The secret and content type of an item `caller` may see.
    ///
    /// # Errors
    ///
    /// [`StoreError::NoSuchObject`] for an item that does not exist or that
    /// the caller may not see (the two are not told apart).
    pub fn read_secret(&self, caller: &AppId, item: &ItemRef) -> Result<(SecretBytes, String)> {
        self.with_open(|open| {
            Self::admit(open, caller)?;
            let found = visible(&open.vault, caller, item)?;
            Ok((found.secret.clone(), found.content_type.clone()))
        })
    }

    /// Replace an item's secret.
    ///
    /// # Errors
    ///
    /// As [`Store::read_secret`].
    pub fn write_secret(
        &self,
        caller: &AppId,
        item: &ItemRef,
        secret: Vec<u8>,
        content_type: String,
    ) -> Result<()> {
        if secret.len() > MAX_SECRET {
            return Err(StoreError::Invalid("secret too long"));
        }
        self.with_open(|open| {
            Self::admit(open, caller)?;
            visible(&open.vault, caller, item)?;
            let now = open.now;
            let transient = open
                .vault
                .collections
                .get(&item.collection)
                .is_some_and(|c| c.transient);
            let found = open
                .vault
                .collections
                .get_mut(&item.collection)
                .and_then(|c| c.items.get_mut(&item.id))
                .ok_or(StoreError::NoSuchObject)?;
            found.secret = SecretBytes::new(secret);
            found.content_type = content_type;
            found.modified = now;
            // The writer now vouches for the value.
            found.owner = Owner::App(caller.clone());
            open.dirty = open.dirty || !transient;
            Ok(())
        })
    }

    /// Delete an item the caller may see.
    ///
    /// # Errors
    ///
    /// As [`Store::read_secret`].
    pub fn delete_item(&self, caller: &AppId, item: &ItemRef) -> Result<()> {
        self.with_open(|open| {
            Self::admit(open, caller)?;
            visible(&open.vault, caller, item)?;
            let transient = open
                .vault
                .collections
                .get(&item.collection)
                .is_some_and(|c| c.transient);
            if let Some(collection) = open.vault.collections.get_mut(&item.collection) {
                collection.items.remove(&item.id);
            }
            open.dirty = open.dirty || !transient;
            Ok(())
        })
    }

    /// What an item says about itself, to a caller who may see it.
    ///
    /// # Errors
    ///
    /// [`StoreError::NoSuchObject`] for an item that does not exist or that
    /// the caller may not see (the two are not told apart);
    /// [`StoreError::Revoked`] for a revoked caller.
    pub fn item_info(&self, caller: &AppId, item: &ItemRef) -> Result<ItemInfo> {
        self.with_open(|open| {
            Self::admit(open, caller)?;
            let found = visible(&open.vault, caller, item)?;
            Ok(ItemInfo {
                label: found.label.clone(),
                attributes: found.attributes.clone(),
                created: found.created,
                modified: found.modified,
            })
        })
    }

    /// Every item's place, for registering their D-Bus objects. Names no
    /// caller and returns no content.
    ///
    /// # Errors
    ///
    /// [`StoreError::Locked`] while locked.
    pub fn refs(&self) -> Result<Vec<ItemRef>> {
        self.with_open(|open| {
            Ok(open
                .vault
                .collections
                .iter()
                .flat_map(|(name, collection)| {
                    collection.items.keys().map(|id| ItemRef {
                        collection: name.clone(),
                        id: id.clone(),
                    })
                })
                .collect())
        })
    }

    /// A collection's label and times.
    ///
    /// # Errors
    ///
    /// [`StoreError::NoSuchObject`].
    pub fn collection_info(&self, name: &str) -> Result<(String, u64, u64)> {
        self.with_open(|open| {
            let found = open
                .vault
                .collections
                .get(open.vault.resolve(name))
                .ok_or(StoreError::NoSuchObject)?;
            Ok((found.label.clone(), found.created, found.modified))
        })
    }

    /// Every identity that used the store.
    ///
    /// # Errors
    ///
    /// [`StoreError::Locked`] while locked.
    pub fn apps(&self) -> Result<Vec<AppInfo>> {
        self.with_open(|open| {
            Ok(open
                .vault
                .apps
                .iter()
                .map(|(id, record)| AppInfo {
                    id: id.clone(),
                    name: id.display_name(),
                    access: record.access,
                    first_seen: record.first_seen,
                    last_used: record.last_used,
                    uses: record.uses,
                    items: open
                        .vault
                        .collections
                        .values()
                        .flat_map(|c| c.items.values())
                        .filter(|item| item.owner == Owner::App(id.clone()))
                        .count()
                        .try_into()
                        .unwrap_or(u32::MAX),
                })
                .collect())
        })
    }

    /// Allow or revoke an identity. A revoked identity sees nothing and
    /// stores nothing; its items stay until it is forgotten.
    ///
    /// # Errors
    ///
    /// [`StoreError::NoSuchObject`] for an identity the ledger lacks.
    pub fn set_access(&self, id: &AppId, access: Access) -> Result<()> {
        self.with_open(|open| {
            open.vault
                .apps
                .get_mut(id)
                .ok_or(StoreError::NoSuchObject)?
                .access = access;
            open.dirty = true;
            Ok(())
        })
    }

    /// Remove an identity: its ledger row, its items and its portal secret.
    /// The identity is recorded afresh if it asks again.
    ///
    /// # Errors
    ///
    /// [`StoreError::NoSuchObject`] for an identity the ledger lacks.
    pub fn forget_app(&self, id: &AppId) -> Result<()> {
        self.with_open(|open| {
            open.vault.apps.remove(id).ok_or(StoreError::NoSuchObject)?;
            let owner = Owner::App(id.clone());
            for collection in open.vault.collections.values_mut() {
                collection.items.retain(|_, item| item.owner != owner);
            }
            if let Some(package) = id.as_str().strip_prefix("sandbox:") {
                open.vault.sandbox_secrets.remove(package);
            }
            open.dirty = true;
            Ok(())
        })
    }

    /// The master secret of a sandboxed app, minted on first use. `app` is
    /// the package id the desktop vouched for; the caller of the portal is
    /// recorded as `sandbox:<app>` and may be revoked like any other.
    ///
    /// `legacy` is the secret an earlier portal store kept for this app, if
    /// the caller found one. It is adopted when the vault has none yet, so
    /// data an app sealed under it stays readable; once the vault holds a
    /// secret, `legacy` is ignored.
    ///
    /// # Errors
    ///
    /// [`StoreError::Revoked`] for a revoked app, [`StoreError::Invalid`]
    /// for a malformed package id.
    pub fn sandbox_secret(
        &self,
        app: &str,
        legacy: Option<&[u8]>,
    ) -> Result<Zeroizing<[u8; SANDBOX_SECRET_LEN]>> {
        let id = AppId::sandbox(app).ok_or(StoreError::Invalid("application id"))?;
        if app.is_empty() || app.len() > 255 || app.contains('/') {
            return Err(StoreError::Invalid("application id"));
        }
        self.with_open(|open| {
            Self::admit(open, &id)?;
            let mut out = Zeroizing::new([0_u8; SANDBOX_SECRET_LEN]);
            if let Some(existing) = open.vault.sandbox_secrets.get(app)
                && existing.expose().len() == SANDBOX_SECRET_LEN
            {
                out.copy_from_slice(existing.expose());
                return Ok(out);
            }
            match legacy.filter(|bytes| bytes.len() == SANDBOX_SECRET_LEN) {
                Some(bytes) => out.copy_from_slice(bytes),
                None => tilo_seal::fill_random(out.as_mut())
                    .map_err(|error| StoreError::Io(error.to_string()))?,
            }
            open.vault
                .sandbox_secrets
                .insert(app.to_owned(), SecretBytes::new(out.to_vec()));
            open.dirty = true;
            Ok(out)
        })
    }

    /// What the import of an older keyring did, if it ran.
    ///
    /// # Errors
    ///
    /// [`StoreError::Locked`] while locked.
    pub fn migration(&self) -> Result<Option<Migration>> {
        self.with_open(|open| Ok(open.vault.migration.clone()))
    }

    /// Import `items` once and record that `source` is done.
    ///
    /// Idempotent: a vault that already records a migration is left alone
    /// and `0` is returned; an item whose label and attributes match one
    /// imported before updates it instead of adding a second.
    ///
    /// # Errors
    ///
    /// [`StoreError::Invalid`] for oversized items.
    pub fn import(&self, source: &str, items: Vec<LegacyItem>) -> Result<u32> {
        for item in &items {
            check_item(&item.label, &item.attributes, item.secret.len())?;
        }
        self.with_open(|open| {
            if open.vault.migration.is_some() {
                return Ok(0);
            }
            let now = open.now;
            let login = open
                .vault
                .collections
                .get_mut(LOGIN_COLLECTION)
                .ok_or(StoreError::NoSuchObject)?;
            let mut imported = 0_u32;
            for legacy in items {
                let duplicate = login.items.iter().find_map(|(id, item)| {
                    (item.owner == Owner::Imported
                        && item.attributes == legacy.attributes
                        && item.label == legacy.label)
                        .then(|| id.clone())
                });
                let id = match duplicate {
                    Some(id) => id,
                    None => random_id()?,
                };
                login.items.insert(
                    id,
                    Item {
                        label: legacy.label,
                        attributes: legacy.attributes,
                        secret: SecretBytes::new(legacy.secret),
                        content_type: legacy.content_type,
                        created: legacy.created,
                        modified: legacy.modified,
                        owner: Owner::Imported,
                    },
                );
                imported = imported.saturating_add(1);
            }
            login.modified = now;
            open.vault.migration = Some(Migration {
                source: source.to_owned(),
                at: now,
                imported,
            });
            open.dirty = true;
            Ok(imported)
        })
    }
}

fn visible<'a>(vault: &'a Vault, caller: &AppId, item: &ItemRef) -> Result<&'a Item> {
    vault
        .collections
        .get(&item.collection)
        .and_then(|c| c.items.get(&item.id))
        .filter(|found| found.owner.admits(caller))
        .ok_or(StoreError::NoSuchObject)
}

fn valid_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then_some(()).ok_or(StoreError::Invalid("name"))
}

fn check_item(label: &str, attributes: &BTreeMap<String, String>, secret_len: usize) -> Result<()> {
    if label.len() > MAX_LABEL {
        return Err(StoreError::Invalid("label too long"));
    }
    if secret_len > MAX_SECRET {
        return Err(StoreError::Invalid("secret too long"));
    }
    if attributes.len() > MAX_ATTRIBUTES
        || attributes
            .iter()
            .any(|(k, v)| k.len() > MAX_ATTRIBUTE_TEXT || v.len() > MAX_ATTRIBUTE_TEXT)
    {
        return Err(StoreError::Invalid("attributes too large"));
    }
    Ok(())
}

/// 128 random bits as 32 hex digits.
fn random_id() -> Result<String> {
    let mut bytes = [0_u8; 16];
    tilo_seal::fill_random(&mut bytes).map_err(|error| StoreError::Io(error.to_string()))?;
    Ok(hex::encode(bytes))
}

#[cfg(test)]
mod tests;

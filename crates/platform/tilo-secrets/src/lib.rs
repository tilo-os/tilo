#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "in a test these are the assertion, not a hazard"
    )
)]
#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::future_not_send,
        reason = "browser futures hold JavaScript handles and run on the page's single thread"
    )
)]

//! App-scoped secret storage: one API on Linux, macOS, Windows and the web.
//!
//! A [`SecretStore`] belongs to one app ([`SecretStore::for_app`]); its keys
//! name that app's secrets and no other app's. [`SecretStore::memory`] keeps
//! them in the process, for tests.
//!
//! | Target | Backend | [`Protection`] |
//! |---|---|---|
//! | Linux | the freedesktop Secret Service over D-Bus (GNOME Keyring, `KWallet`) | [`Protection::OsKeychain`] |
//! | macOS | the login Keychain's generic passwords | [`Protection::OsKeychain`] |
//! | Windows | generic credentials in the user's Credential Manager (DPAPI-encrypted, this machine only) | [`Protection::OsKeychain`] |
//! | Web | AES-GCM under a non-extractable `CryptoKey`; key and ciphertext in `IndexedDB` | [`Protection::BrowserWrapped`] |
//! | Linux, macOS with `TILO_SECRETS_DIR` | sealed owner-only files in that directory | [`Protection::LocalFile`] |
//!
//! [`SECRETS_DIR_ENV`] is for machines that have no keychain to unlock: a
//! headless node or server. Every process given it keeps its secrets in that
//! directory instead of the system store; see the `files` module.
//!
//! `BrowserWrapped` protects secrets at rest (a copied profile, a disk
//! image), not against script running in the page's origin.
//!
//! The API is async because the browser's is. Its futures are `Send` on the
//! desktop and not in the browser, where everything runs on the page's
//! thread; spawn them on `tilo_rt::app()`, which accepts both. Desktop code
//! may use the
//! `*_blocking` methods; the web answers them once the host loaded the
//! page's secrets (`web::mirror`), and otherwise with [`Feature::Blocking`]
//! unsupported. On Linux the Secret Service item carries the attributes
//! `application = <app id>` and `purpose = <key>`, so an item stored earlier
//! by `secret-tool store application <app> purpose <key>` is found.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use zeroize::Zeroizing;

mod caps;
pub mod conformance;
#[cfg(any(target_os = "windows", test))]
mod credential;
#[cfg(target_os = "linux")]
mod device_proof;
#[cfg(unix)]
mod files;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
pub mod owner_migration;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub mod web;
#[cfg(target_os = "windows")]
mod windows;

pub use caps::{Availability, Feature, Unsupported, availability, available};
/// A process that holds the device identity proves it to
/// `tilo-secret-service`, which cannot read a non-dumpable process's
/// executable (see the module docs of `device_proof`).
#[cfg(target_os = "linux")]
pub use device_proof::{arm as arm_device_proof, prove as prove_device_caller};

/// Secret bytes: wiped from memory on drop and never printed by `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Zeroizing<Vec<u8>>);

impl Secret {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(Zeroizing::new(bytes.into()))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret({} bytes)", self.0.len())
    }
}

/// How the stored secrets are protected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protection {
    /// The operating system's keychain holds them, unlocked with the user's
    /// session.
    OsKeychain,
    /// Encrypted under a key the browser will not export; see the crate docs.
    BrowserWrapped,
    /// Sealed in owner-only files under a key file beside them: as private
    /// as the machine's disk and the Unix account that owns them.
    LocalFile,
    /// In this process only; gone when it exits.
    Memory,
}

/// Why a secret could not be read, stored or deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    /// No secret store answers on this session (no Secret Service running).
    Unavailable(String),
    /// This target cannot do what was asked; see [`availability`].
    Unsupported(Unsupported),
    /// The user dismissed the unlock prompt, or the store refused access.
    Denied,
    /// The key is empty or has control characters.
    InvalidKey,
    Failed(String),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "no secret store: {reason}"),
            Self::Unsupported(unsupported) => unsupported.fmt(f),
            Self::Denied => f.write_str("access to the secret store was refused"),
            Self::InvalidKey => f.write_str("a secret key must be non-empty text"),
            Self::Failed(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for SecretError {}

/// Options for [`SecretStore::put`].
#[derive(Clone, Debug, Default)]
pub struct PutOptions {
    /// What a keyring manager shows for the item. Defaults to the app id and
    /// key.
    pub label: Option<String>,
}

/// A synchronous secret store for one process: the desktop backends,
/// [`SecretStore::memory`], or a host's own. Every call names the app.
pub trait SecretBackend: Send + Sync {
    fn protection(&self) -> Protection;
    /// # Errors
    /// The store is unavailable or refused.
    fn get(&self, app: &str, key: &str) -> Result<Option<Secret>, SecretError>;
    /// Store `value`, replacing any secret under `key`.
    ///
    /// # Errors
    /// The store is unavailable or refused.
    fn put(&self, app: &str, key: &str, value: &[u8], label: &str) -> Result<(), SecretError>;
    /// Delete the secret under `key`; deleting a missing key succeeds.
    ///
    /// # Errors
    /// The store is unavailable or refused.
    fn delete(&self, app: &str, key: &str) -> Result<(), SecretError>;
    /// Whether the store answers at all on this session, without touching a
    /// secret.
    ///
    /// # Errors
    /// Why it does not.
    fn probe(&self) -> Result<(), SecretError> {
        Ok(())
    }
}

#[derive(Clone)]
enum Backend {
    Sync(Arc<dyn SecretBackend>),
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    Web,
}

/// One app's secrets.
#[derive(Clone)]
pub struct SecretStore {
    app: String,
    backend: Backend,
}

impl fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretStore")
            .field("app", &self.app)
            .field("protection", &self.protection())
            .finish_non_exhaustive()
    }
}

impl SecretStore {
    /// The app `app_id`'s secrets in this target's secret store, or in the
    /// thread's [`scope_thread`] store, or in the process's own after
    /// [`keep_in_memory_for_process`].
    pub fn for_app(app_id: &str) -> Self {
        Self {
            app: app_id.to_owned(),
            backend: THREAD_BACKEND
                .with(|scoped| scoped.borrow().clone())
                .or_else(|| PROCESS_BACKEND.get().cloned())
                .map_or_else(machine_backend, Backend::Sync),
        }
    }

    /// Secrets kept in this process, for tests. Each call is a fresh store.
    pub fn memory(app_id: &str) -> Self {
        Self::with_backend(app_id, Arc::new(MemoryBackend::default()))
    }

    /// The app `app_id`'s secrets in a host's own store.
    pub fn with_backend(app_id: &str, backend: Arc<dyn SecretBackend>) -> Self {
        Self {
            app: app_id.to_owned(),
            backend: Backend::Sync(backend),
        }
    }

    pub fn app_id(&self) -> &str {
        &self.app
    }

    pub fn protection(&self) -> Protection {
        match &self.backend {
            Backend::Sync(backend) => backend.protection(),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Backend::Web => Protection::BrowserWrapped,
        }
    }

    /// The secret under `key`, or `None`.
    ///
    /// # Errors
    /// The store is unavailable, refused, or the key is malformed.
    pub async fn get(&self, key: &str) -> Result<Option<Secret>, SecretError> {
        check_key(key)?;
        match &self.backend {
            Backend::Sync(backend) => {
                let (backend, app, key) = (backend.clone(), self.app.clone(), key.to_owned());
                off_thread(move || backend.get(&app, &key)).await
            }
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Backend::Web => web::get(&self.app, key).await,
        }
    }

    /// Store `value` under `key`, replacing any earlier secret.
    ///
    /// # Errors
    /// The store is unavailable, refused, or the key is malformed.
    pub async fn put(
        &self,
        key: &str,
        value: &[u8],
        options: PutOptions,
    ) -> Result<(), SecretError> {
        check_key(key)?;
        let label = options
            .label
            .unwrap_or_else(|| format!("{} {key}", self.app));
        match &self.backend {
            Backend::Sync(backend) => {
                let (backend, app, key) = (backend.clone(), self.app.clone(), key.to_owned());
                let value = Zeroizing::new(value.to_vec());
                off_thread(move || backend.put(&app, &key, &value, &label)).await
            }
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Backend::Web => web::put(&self.app, key, value).await,
        }
    }

    /// Delete the secret under `key`. Deleting a missing key succeeds.
    ///
    /// # Errors
    /// The store is unavailable, refused, or the key is malformed.
    pub async fn delete(&self, key: &str) -> Result<(), SecretError> {
        check_key(key)?;
        match &self.backend {
            Backend::Sync(backend) => {
                let (backend, app, key) = (backend.clone(), self.app.clone(), key.to_owned());
                off_thread(move || backend.delete(&app, &key)).await
            }
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Backend::Web => web::delete(&self.app, key).await,
        }
    }

    /// [`get`](Self::get), blocking the calling thread. Desktop code only.
    ///
    /// # Errors
    /// As `get`, and [`Feature::Blocking`] unsupported on the web.
    pub fn get_blocking(&self, key: &str) -> Result<Option<Secret>, SecretError> {
        check_key(key)?;
        self.sync()?.get(&self.app, key)
    }

    /// [`put`](Self::put), blocking the calling thread. Desktop code only.
    ///
    /// # Errors
    /// As `put`, and [`Feature::Blocking`] unsupported on the web.
    pub fn put_blocking(
        &self,
        key: &str,
        value: &[u8],
        options: PutOptions,
    ) -> Result<(), SecretError> {
        check_key(key)?;
        let label = options
            .label
            .unwrap_or_else(|| format!("{} {key}", self.app));
        self.sync()?.put(&self.app, key, value, &label)
    }

    /// [`delete`](Self::delete), blocking the calling thread. Desktop code
    /// only.
    ///
    /// # Errors
    /// As `delete`, and [`Feature::Blocking`] unsupported on the web.
    pub fn delete_blocking(&self, key: &str) -> Result<(), SecretError> {
        check_key(key)?;
        self.sync()?.delete(&self.app, key)
    }

    /// Whether the store answers on this session, blocking the calling
    /// thread. The web store always answers.
    ///
    /// # Errors
    /// Why it does not.
    pub fn probe_blocking(&self) -> Result<(), SecretError> {
        match &self.backend {
            Backend::Sync(backend) => backend.probe(),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Backend::Web => Ok(()),
        }
    }

    #[cfg_attr(
        not(all(target_arch = "wasm32", target_os = "unknown")),
        allow(
            clippy::unnecessary_wraps,
            reason = "the web store has no synchronous access"
        )
    )]
    fn sync(&self) -> Result<&dyn SecretBackend, SecretError> {
        match &self.backend {
            Backend::Sync(backend) => Ok(&**backend),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Backend::Web if web::mirrored() => Ok(&web::MirrorBackend),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Backend::Web => Err(SecretError::Unsupported(Unsupported::of(Feature::Blocking))),
        }
    }
}

impl SecretStore {
    /// The secret one item of a document needs for `purpose`: a database
    /// profile's password, a connection's SSH passphrase, an account's
    /// one-time-password seed. The document names the item by its id and
    /// never holds the secret; the secret stays in this store on this
    /// device.
    ///
    /// `purpose` is one of the app's own fixed names and contains no `/`;
    /// `item` is the item's id in the document.
    #[must_use]
    pub fn item(&self, purpose: &'static str, item: &str) -> ItemSecret {
        debug_assert!(
            !purpose.is_empty() && !purpose.contains('/'),
            "an item secret's purpose is a plain name: {purpose:?}"
        );
        ItemSecret {
            store: self.clone(),
            key: format!("{purpose}/{item}"),
            label: format!("{} {purpose} for {item}", self.app),
        }
    }
}

/// The secret of one document item in an app's [`SecretStore`]; see
/// [`SecretStore::item`].
#[derive(Clone, Debug)]
pub struct ItemSecret {
    store: SecretStore,
    key: String,
    label: String,
}

impl ItemSecret {
    /// The store key: `<purpose>/<item id>`.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The secret, or `None` when this device has none for the item.
    ///
    /// # Errors
    /// The store is unavailable or refused.
    pub async fn get(&self) -> Result<Option<Secret>, SecretError> {
        self.store.get(&self.key).await
    }

    /// Keep `value` as the item's secret, replacing any earlier one.
    ///
    /// # Errors
    /// The store is unavailable or refused.
    pub async fn put(&self, value: &[u8]) -> Result<(), SecretError> {
        let options = PutOptions {
            label: Some(self.label.clone()),
        };
        self.store.put(&self.key, value, options).await
    }

    /// Forget the item's secret, as when the item is deleted.
    ///
    /// # Errors
    /// The store is unavailable or refused.
    pub async fn delete(&self) -> Result<(), SecretError> {
        self.store.delete(&self.key).await
    }

    /// [`get`](Self::get), blocking the calling thread. Desktop code only.
    ///
    /// # Errors
    /// As `get`, and [`Feature::Blocking`] unsupported on the web.
    pub fn get_blocking(&self) -> Result<Option<Secret>, SecretError> {
        self.store.get_blocking(&self.key)
    }

    /// [`put`](Self::put), blocking the calling thread. Desktop code only.
    ///
    /// # Errors
    /// As `put`, and [`Feature::Blocking`] unsupported on the web.
    pub fn put_blocking(&self, value: &[u8]) -> Result<(), SecretError> {
        let options = PutOptions {
            label: Some(self.label.clone()),
        };
        self.store.put_blocking(&self.key, value, options)
    }

    /// [`delete`](Self::delete), blocking the calling thread. Desktop code
    /// only.
    ///
    /// # Errors
    /// As `delete`, and [`Feature::Blocking`] unsupported on the web.
    pub fn delete_blocking(&self) -> Result<(), SecretError> {
        self.store.delete_blocking(&self.key)
    }

    /// Whether this device holds the item's secret, for availability such
    /// as "secret present here". An unavailable store holds none.
    #[must_use]
    pub fn present_blocking(&self) -> bool {
        matches!(self.get_blocking(), Ok(Some(_)))
    }
}

/// Whether what this build puts in the system store stays readable by later
/// builds of the same program. On macOS only a Developer ID signature does
/// (see the `macos` backend): an ad-hoc or dev build's Keychain items belong
/// to that exact build, and a later one fails on them rather than ask. Always
/// true elsewhere.
#[must_use]
pub fn items_outlive_rebuilds() -> bool {
    #[cfg(target_os = "macos")]
    return macos::developer_id_signed();
    #[cfg(not(target_os = "macos"))]
    true
}

static PROCESS_BACKEND: std::sync::OnceLock<Arc<dyn SecretBackend>> = std::sync::OnceLock::new();

/// From now on, every [`SecretStore::for_app`] in this process shares one
/// in-memory store and never reaches the platform's (which may prompt, and
/// holds the user's real secrets). For harnesses that mount real apps
/// headless; stores created earlier keep their backend.
pub fn keep_in_memory_for_process() {
    PROCESS_BACKEND.get_or_init(|| Arc::new(MemoryBackend::default()));
}

thread_local! {
    static THREAD_BACKEND: std::cell::RefCell<Option<Arc<dyn SecretBackend>>> =
        const { std::cell::RefCell::new(None) };
}

/// Keeps a thread's stores in one backend until it is dropped: see
/// [`scope_thread`].
#[must_use = "the scope ends when the guard is dropped"]
pub struct SecretsScope {
    earlier: Option<Arc<dyn SecretBackend>>,
}

impl Drop for SecretsScope {
    fn drop(&mut self) {
        THREAD_BACKEND.with(|scoped| *scoped.borrow_mut() = self.earlier.take());
    }
}

/// From now until the guard drops, every [`SecretStore::for_app`] this
/// thread creates keeps its secrets in `backend`, never the platform's
/// store. A host gives a participant it runs on its own thread (an agent)
/// secrets apart from the host user's this way. A store keeps the backend
/// it was created with, wherever it is used later.
pub fn scope_thread(backend: Arc<dyn SecretBackend>) -> SecretsScope {
    let earlier = THREAD_BACKEND.with(|scoped| scoped.borrow_mut().replace(backend));
    SecretsScope { earlier }
}

/// Run `work` with this thread's [`scope_thread`] store lifted, then
/// restore it.
pub fn unscoped<R>(work: impl FnOnce() -> R) -> R {
    let _scope = SecretsScope {
        earlier: THREAD_BACKEND.with(|scoped| scoped.borrow_mut().take()),
    };
    work()
}

/// A fresh store that keeps secrets in this process: for [`scope_thread`],
/// or for a host whose apps' stores ([`SecretStore::with_backend`]) share one
/// process-lifetime map.
#[must_use]
pub fn memory_backend() -> Arc<dyn SecretBackend> {
    Arc::new(MemoryBackend::default())
}

fn check_key(key: &str) -> Result<(), SecretError> {
    if key.is_empty() || key.chars().any(char::is_control) {
        Err(SecretError::InvalidKey)
    } else {
        Ok(())
    }
}

/// Run a blocking store call on the app runtime's blocking pool, or inline
/// where there are no threads (an in-memory store in a browser).
async fn off_thread<T: Send + 'static>(
    call: impl FnOnce() -> Result<T, SecretError> + Send + 'static,
) -> Result<T, SecretError> {
    if matches!(
        tilo_rt::availability(tilo_rt::Feature::Threads),
        tilo_rt::Availability::Unsupported(_)
    ) {
        return call();
    }
    tilo_rt::app()
        .spawn_blocking(call)
        .await
        .unwrap_or_else(|error| {
            Err(SecretError::Failed(format!(
                "secret store task failed: {error}"
            )))
        })
}

/// Names the directory of a file-backed store that replaces the system
/// store for every process given it.
pub const SECRETS_DIR_ENV: &str = "TILO_SECRETS_DIR";

/// The store the machine provides: the file-backed one where
/// [`SECRETS_DIR_ENV`] names a directory, else the system's.
fn machine_backend() -> Backend {
    #[cfg(unix)]
    if let Some(dir) = std::env::var_os(SECRETS_DIR_ENV).filter(|dir| !dir.is_empty()) {
        static FILES: std::sync::OnceLock<Result<Arc<dyn SecretBackend>, SecretError>> =
            std::sync::OnceLock::new();
        let opened = FILES.get_or_init(|| {
            files::FileStore::open(std::path::Path::new(&dir))
                .map(|store| Arc::new(store) as Arc<dyn SecretBackend>)
        });
        return Backend::Sync(match opened {
            Ok(store) => store.clone(),
            Err(error) => Arc::new(Refused(error.clone())),
        });
    }
    system_backend()
}

/// A configured store that could not be opened: every call says why,
/// rather than falling back to another store.
#[cfg(unix)]
struct Refused(SecretError);

#[cfg(unix)]
impl SecretBackend for Refused {
    fn protection(&self) -> Protection {
        Protection::LocalFile
    }
    fn get(&self, _app: &str, _key: &str) -> Result<Option<Secret>, SecretError> {
        Err(self.0.clone())
    }
    fn put(&self, _app: &str, _key: &str, _value: &[u8], _label: &str) -> Result<(), SecretError> {
        Err(self.0.clone())
    }
    fn delete(&self, _app: &str, _key: &str) -> Result<(), SecretError> {
        Err(self.0.clone())
    }
    fn probe(&self) -> Result<(), SecretError> {
        Err(self.0.clone())
    }
}

#[cfg(target_os = "linux")]
fn system_backend() -> Backend {
    Backend::Sync(Arc::new(linux::SecretService))
}

/// A debug build inside `tools/with-test-environment.sh` (which cargo runs
/// every test binary in) never reaches the user's login Keychain, just as the
/// environment gives Linux tests no Secret Service. `TILO_SECRETS_SYSTEM_TEST`
/// is the explicit request to test the real store.
#[cfg(target_os = "macos")]
fn system_backend() -> Backend {
    if cfg!(debug_assertions)
        && std::env::var_os("TILO_TEST_ENVIRONMENT").is_some()
        && std::env::var_os("TILO_SECRETS_SYSTEM_TEST").is_none()
    {
        return Backend::Sync(Arc::new(UnsupportedBackend(
            "the test environment has no login Keychain",
        )));
    }
    Backend::Sync(Arc::new(macos::Keychain::new()))
}

/// As on macOS, a debug test build in the test environment never writes the
/// user's own Credential Manager unless `TILO_SECRETS_SYSTEM_TEST` asks.
#[cfg(target_os = "windows")]
fn system_backend() -> Backend {
    if cfg!(debug_assertions)
        && std::env::var_os("TILO_TEST_ENVIRONMENT").is_some()
        && std::env::var_os("TILO_SECRETS_SYSTEM_TEST").is_none()
    {
        return Backend::Sync(Arc::new(UnsupportedBackend(
            "the test environment has no Credential Manager",
        )));
    }
    Backend::Sync(Arc::new(windows::CredentialManager))
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn system_backend() -> Backend {
    Backend::Web
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    all(target_arch = "wasm32", target_os = "unknown")
)))]
fn system_backend() -> Backend {
    Backend::Sync(Arc::new(UnsupportedBackend(
        "this platform has no secret store backend",
    )))
}

/// No secret store, and why: a target without one, or a macOS or Windows
/// test environment.
#[cfg(not(any(
    target_os = "linux",
    all(target_arch = "wasm32", target_os = "unknown")
)))]
struct UnsupportedBackend(&'static str);

#[cfg(not(any(
    target_os = "linux",
    all(target_arch = "wasm32", target_os = "unknown")
)))]
impl SecretBackend for UnsupportedBackend {
    fn protection(&self) -> Protection {
        Protection::Memory
    }
    fn get(&self, _app: &str, _key: &str) -> Result<Option<Secret>, SecretError> {
        Err(SecretError::Unavailable(self.0.into()))
    }
    fn put(&self, _app: &str, _key: &str, _value: &[u8], _label: &str) -> Result<(), SecretError> {
        self.get("", "").map(drop)
    }
    fn delete(&self, _app: &str, _key: &str) -> Result<(), SecretError> {
        self.get("", "").map(drop)
    }
    fn probe(&self) -> Result<(), SecretError> {
        self.get("", "").map(drop)
    }
}

/// Secrets in a map, keyed by app and key.
#[derive(Default)]
struct MemoryBackend {
    secrets: Mutex<HashMap<(String, String), Secret>>,
}

impl SecretBackend for MemoryBackend {
    fn protection(&self) -> Protection {
        Protection::Memory
    }

    fn get(&self, app: &str, key: &str) -> Result<Option<Secret>, SecretError> {
        let secrets = self
            .secrets
            .lock()
            .map_err(|_| SecretError::Failed("poisoned".into()))?;
        Ok(secrets.get(&(app.to_owned(), key.to_owned())).cloned())
    }

    fn put(&self, app: &str, key: &str, value: &[u8], _label: &str) -> Result<(), SecretError> {
        self.secrets
            .lock()
            .map_err(|_| SecretError::Failed("poisoned".into()))?
            .insert((app.to_owned(), key.to_owned()), Secret::new(value));
        Ok(())
    }

    fn delete(&self, app: &str, key: &str) -> Result<(), SecretError> {
        self.secrets
            .lock()
            .map_err(|_| SecretError::Failed("poisoned".into()))?
            .remove(&(app.to_owned(), key.to_owned()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn the_memory_store_conforms() {
        conformance::check_store(&SecretStore::memory("so.tilo.secrets-test")).await;
    }

    #[test]
    fn the_memory_store_conforms_blocking() {
        conformance::check_store_blocking(&SecretStore::memory("so.tilo.secrets-test"));
    }

    #[test]
    fn apps_do_not_see_each_others_secrets() {
        let shared: Arc<dyn SecretBackend> = Arc::new(MemoryBackend::default());
        let a = SecretStore::with_backend("so.tilo.a", shared.clone());
        let b = SecretStore::with_backend("so.tilo.b", shared);
        a.put_blocking("token", b"a's", PutOptions::default())
            .unwrap();
        assert_eq!(b.get_blocking("token").unwrap(), None);
        assert_eq!(a.get_blocking("token").unwrap().unwrap().expose(), b"a's");
    }

    #[test]
    fn item_secrets_are_kept_per_purpose_and_item() {
        let store = SecretStore::memory("so.tilo.db-client");
        let alpha = store.item("password", "profile-alpha");
        let beta = store.item("password", "profile-beta");
        let alpha_key = store.item("key-passphrase", "profile-alpha");
        assert!(!alpha.present_blocking());
        alpha.put_blocking(b"hunter2").unwrap();
        assert!(alpha.present_blocking());
        assert!(!beta.present_blocking());
        assert!(!alpha_key.present_blocking());
        assert_eq!(alpha.key(), "password/profile-alpha");
        assert_eq!(
            store
                .get_blocking("password/profile-alpha")
                .unwrap()
                .unwrap()
                .expose(),
            b"hunter2"
        );
        alpha.delete_blocking().unwrap();
        assert_eq!(alpha.get_blocking().unwrap(), None);
    }

    #[test]
    fn secrets_never_print() {
        assert_eq!(
            format!("{:?}", Secret::new(b"hunter2".to_vec())),
            "Secret(7 bytes)"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn tests_never_reach_the_login_keychain() {
        assert!(
            std::env::var_os("TILO_TEST_ENVIRONMENT").is_some(),
            "run tests through cargo, whose runner (tools/cargo-runner.sh) starts \
             them in tools/with-test-environment.sh"
        );
        if std::env::var_os("TILO_SECRETS_SYSTEM_TEST").is_some() {
            return;
        }
        let store = SecretStore::for_app("so.tilo.secrets-isolation");
        assert!(matches!(
            store.put_blocking("probe", b"x", PutOptions::default()),
            Err(SecretError::Unavailable(_))
        ));
        assert!(matches!(
            store.probe_blocking(),
            Err(SecretError::Unavailable(_))
        ));
    }

    /// The real store is exercised only where a developer asks for it: it
    /// writes the login keychain (and may prompt).
    #[test]
    fn the_system_store_conforms_when_asked() {
        if std::env::var_os("TILO_SECRETS_SYSTEM_TEST").is_none() {
            return;
        }
        let store = SecretStore::for_app("so.tilo.secrets-conformance");
        store.probe_blocking().unwrap();
        conformance::check_store_blocking(&store);
    }
}

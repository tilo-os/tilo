//! The store of an app the launcher sandboxed: sealed files in the app's own directory, under a
//! key derived from the master secret the Secret portal (`org.freedesktop.portal.Secret`) keeps
//! for this app. The sandbox lets an app reach that portal when its manifest grants `secrets`,
//! and never the Secret Service itself. The key lives only in this process.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use tilo_seal::{Context, Label};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{Fd, OwnedObjectPath, OwnedValue, Value};
use zeroize::Zeroizing;

use super::{on_bus, unavailable};
use crate::files::FileStore;
use crate::{Protection, Secret, SecretBackend, SecretError};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
/// Where the store lies under the app's directory.
const STORE_DIR: &str = ".local/share/tilo-secrets";
/// A master secret is a key's worth of bytes; a longer answer is not one.
const MAX_SECRET_BYTES: u64 = 1024;

/// Opened on first use, so an app that never touches a secret never asks the portal. A failed
/// open is not kept: the next call asks again.
pub(crate) struct PortalStore {
    dir: PathBuf,
    opened: Mutex<Option<Arc<FileStore>>>,
}

impl PortalStore {
    pub(crate) fn in_app_dir(app_dir: &Path) -> Self {
        Self {
            dir: app_dir.join(STORE_DIR),
            opened: Mutex::default(),
        }
    }

    fn store(&self) -> Result<Arc<FileStore>, SecretError> {
        let mut opened = self
            .opened
            .lock()
            .map_err(|_| SecretError::Failed("secret store poisoned".into()))?;
        if let Some(store) = &*opened {
            return Ok(store.clone());
        }
        let master = on_bus(retrieve_master_secret)?;
        let key = tilo_seal::web::derive_key(
            &master,
            b"tilo-secrets",
            Context::new(Label::FileSecretStore, b"portal"),
        )
        .map_err(|error| SecretError::Failed(format!("the portal's secret: {error}")))?;
        let store = Arc::new(FileStore::with_key(&self.dir, key)?);
        *opened = Some(store.clone());
        Ok(store)
    }
}

impl SecretBackend for PortalStore {
    /// The key is held by the session's secret store, apart from the files.
    fn protection(&self) -> Protection {
        Protection::OsKeychain
    }
    fn get(&self, app: &str, key: &str) -> Result<Option<Secret>, SecretError> {
        self.store()?.get(app, key)
    }
    fn put(&self, app: &str, key: &str, value: &[u8], label: &str) -> Result<(), SecretError> {
        self.store()?.put(app, key, value, label)
    }
    fn delete(&self, app: &str, key: &str) -> Result<(), SecretError> {
        self.store()?.delete(app, key)
    }
    fn probe(&self) -> Result<(), SecretError> {
        self.store().map(drop)
    }
}

/// Ask the portal for this app's master secret: it writes the secret into a pipe this process
/// passes, then answers on the request object named by our token.
fn retrieve_master_secret() -> Result<Zeroizing<Vec<u8>>, SecretError> {
    let connection = Connection::session().map_err(unavailable)?;
    let sender = connection
        .unique_name()
        .ok_or_else(|| SecretError::Unavailable("no name on the session bus".into()))?
        .trim_start_matches(':')
        .replace('.', "_");
    static ASKED: AtomicU32 = AtomicU32::new(0);
    let asked = ASKED.fetch_add(1, Ordering::Relaxed);
    let token = format!("tilo_secrets_{}_{asked}", std::process::id());
    let handle = format!("{PORTAL_PATH}/request/{sender}/{token}");
    let request = Proxy::new(
        &connection,
        PORTAL,
        handle.as_str(),
        "org.freedesktop.portal.Request",
    )
    .map_err(unavailable)?;
    // Subscribed before the call, so the answer cannot pass unseen.
    let mut responses = request.receive_signal("Response").map_err(unavailable)?;
    let portal = Proxy::new(
        &connection,
        PORTAL,
        PORTAL_PATH,
        "org.freedesktop.portal.Secret",
    )
    .map_err(unavailable)?;
    let (mut read, write) = std::io::pipe()
        .map_err(|error| SecretError::Failed(format!("a pipe for the secret: {error}")))?;
    let options = HashMap::from([("handle_token", Value::from(token.as_str()))]);
    let _: OwnedObjectPath = portal
        .call("RetrieveSecret", &(Fd::from(&write), options))
        .map_err(unavailable)?;
    drop(write);
    let response = responses
        .next()
        .ok_or_else(|| SecretError::Unavailable("the Secret portal did not answer".into()))?;
    let (code, _): (u32, HashMap<String, OwnedValue>) = response
        .body()
        .deserialize()
        .map_err(|error| SecretError::Failed(format!("the Secret portal's answer: {error}")))?;
    if code != 0 {
        return Err(SecretError::Denied);
    }
    let mut secret = Zeroizing::new(Vec::new());
    (&mut read)
        .take(MAX_SECRET_BYTES)
        .read_to_end(&mut secret)
        .map_err(|error| SecretError::Failed(format!("reading the portal's secret: {error}")))?;
    Ok(secret)
}

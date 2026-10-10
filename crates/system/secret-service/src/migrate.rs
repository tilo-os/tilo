//! Bringing the items of an older keyring over, once.
//!
//! Until now GNOME Keyring held these secrets, unlocked at login with the
//! person's password. Tilo's store replaces it as the owner of
//! `org.freedesktop.secrets`, so what the person already keeps there must
//! come along. It can be read only while that keyring is unlocked, which
//! happens at a password login and never at a fingerprint or automatic one;
//! the import therefore waits for the first login where it can succeed, and
//! then records that it is done so that it never runs again.
//!
//! The keyring is read through the Secret Service API itself, from the
//! daemon the login session already started, and that daemon is stopped
//! afterwards: only one program may own the bus name, and it is the store.
//! The import runs before the store claims the name, which is why the name
//! is free for the old daemon to hold for that short while.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use tracing::{info, warn};
use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::store::{LegacyItem, Store};

/// What the older keyring is called in the record of the import.
pub const SOURCE_NAME: &str = "gnome-keyring";
const OLD_NAME: &str = "org.freedesktop.secrets";
const LOGIN_PATH: &str = "/org/freedesktop/secrets/aliases/default";
const ATTEMPT: Duration = Duration::from_secs(8);

/// Why items could not be read from the older keyring now.
#[derive(Debug, PartialEq, Eq)]
pub enum SourceError {
    /// There is no older keyring: nothing to bring over, ever.
    Absent,
    /// It exists but is locked, or its daemon is not running. Try again at
    /// the next login.
    Unavailable(String),
}

/// A place the person's earlier secrets were kept.
pub trait LegacySource {
    /// Every item of the login keyring.
    fn load(&self) -> impl Future<Output = Result<Vec<LegacyItem>, SourceError>> + Send;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Imported this many items.
    Imported(u32),
    /// An earlier run already did.
    AlreadyDone,
    /// Not now; the next start tries again.
    Deferred(String),
}

/// Import from `source` unless an earlier run did.
pub async fn migrate_once(store: &Store, source: &(impl LegacySource + Sync)) -> Outcome {
    match store.migration() {
        Ok(Some(_)) => return Outcome::AlreadyDone,
        Ok(None) => {}
        Err(error) => return Outcome::Deferred(error.to_string()),
    }
    match source.load().await {
        Ok(items) => match store.import(SOURCE_NAME, items) {
            Ok(imported) => Outcome::Imported(imported),
            Err(error) => Outcome::Deferred(error.to_string()),
        },
        // Nothing to bring over: recorded, so no later start asks again.
        Err(SourceError::Absent) => match store.import(SOURCE_NAME, Vec::new()) {
            Ok(_) => Outcome::Imported(0),
            Err(error) => Outcome::Deferred(error.to_string()),
        },
        Err(SourceError::Unavailable(reason)) => Outcome::Deferred(reason),
    }
}

/// GNOME Keyring as the login session left it.
pub struct GnomeKeyring {
    /// The person's `keyrings` directory.
    pub keyrings: PathBuf,
    /// `GNOME_KEYRING_CONTROL` as the session exported it.
    pub control: Option<PathBuf>,
}

impl GnomeKeyring {
    /// From the environment of a user service.
    #[must_use]
    pub fn from_environment() -> Self {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
            .unwrap_or_default();
        Self {
            keyrings: data.join("keyrings"),
            control: std::env::var_os("GNOME_KEYRING_CONTROL")
                .map(|dir| PathBuf::from(dir).join("control")),
        }
    }
}

impl LegacySource for GnomeKeyring {
    async fn load(&self) -> Result<Vec<LegacyItem>, SourceError> {
        if !self.keyrings.join("login.keyring").exists() {
            return Err(SourceError::Absent);
        }
        let control = self
            .control
            .as_ref()
            .filter(|path| path.exists())
            .ok_or_else(|| {
                SourceError::Unavailable("the login keyring daemon is not running".into())
            })?;
        tokio::time::timeout(ATTEMPT, read_from_daemon(control))
            .await
            .map_err(|_| SourceError::Unavailable("the keyring daemon did not answer".into()))?
    }
}

fn unavailable(error: impl std::fmt::Display) -> SourceError {
    SourceError::Unavailable(error.to_string())
}

/// Ask the running login daemon to serve the Secret Service on the session
/// bus, read the login collection, and stop it again.
async fn read_from_daemon(control: &std::path::Path) -> Result<Vec<LegacyItem>, SourceError> {
    let status = tokio::process::Command::new("gnome-keyring-daemon")
        .args(["--start", "--components=secrets"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env("GNOME_KEYRING_CONTROL", control.parent().unwrap_or(control))
        .status()
        .await
        .map_err(unavailable)?;
    if !status.success() {
        return Err(unavailable(
            "the keyring daemon refused to start its secrets component",
        ));
    }
    let connection = Connection::session().await.map_err(unavailable)?;
    let outcome = read_login_collection(&connection).await;
    stop_owner(&connection).await;
    outcome
}

async fn read_login_collection(connection: &Connection) -> Result<Vec<LegacyItem>, SourceError> {
    let bus = zbus::fdo::DBusProxy::new(connection)
        .await
        .map_err(unavailable)?;
    let name = zbus::names::BusName::try_from(OLD_NAME).map_err(unavailable)?;
    let mut waited = Duration::ZERO;
    while !bus
        .name_has_owner(name.clone())
        .await
        .map_err(unavailable)?
    {
        if waited >= Duration::from_secs(4) {
            return Err(unavailable(
                "the keyring daemon never claimed the Secret Service",
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        waited = waited.saturating_add(Duration::from_millis(100));
    }
    let service = zbus::Proxy::new(
        connection,
        OLD_NAME,
        "/org/freedesktop/secrets",
        "org.freedesktop.Secret.Service",
    )
    .await
    .map_err(unavailable)?;
    let (_, session): (OwnedValue, OwnedObjectPath) = service
        .call("OpenSession", &("plain", Value::from("")))
        .await
        .map_err(unavailable)?;
    let collection = zbus::Proxy::new(
        connection,
        OLD_NAME,
        LOGIN_PATH,
        "org.freedesktop.Secret.Collection",
    )
    .await
    .map_err(unavailable)?;
    let locked: bool = collection
        .get_property("Locked")
        .await
        .map_err(unavailable)?;
    if locked {
        return Err(unavailable("the login keyring is locked"));
    }
    let paths: Vec<OwnedObjectPath> = collection
        .get_property("Items")
        .await
        .map_err(unavailable)?;
    let mut items = Vec::with_capacity(paths.len());
    for path in paths {
        let item = zbus::Proxy::new(
            connection,
            OLD_NAME,
            path.as_str(),
            "org.freedesktop.Secret.Item",
        )
        .await
        .map_err(unavailable)?;
        let (_, _, value, content_type): (OwnedObjectPath, Vec<u8>, Vec<u8>, String) = item
            .call("GetSecret", &(&session,))
            .await
            .map_err(unavailable)?;
        let attributes: BTreeMap<String, String> = item
            .get_property::<std::collections::HashMap<String, String>>("Attributes")
            .await
            .map_err(unavailable)?
            .into_iter()
            .collect();
        items.push(LegacyItem {
            label: item.get_property("Label").await.map_err(unavailable)?,
            attributes,
            secret: value,
            content_type,
            created: item.get_property("Created").await.unwrap_or(0),
            modified: item.get_property("Modified").await.unwrap_or(0),
        });
    }
    Ok(items)
}

/// Stop the daemon that holds the old name, so the store can claim it.
async fn stop_owner(connection: &Connection) {
    let Ok(bus) = zbus::fdo::DBusProxy::new(connection).await else {
        return;
    };
    let Ok(name) = zbus::names::BusName::try_from(OLD_NAME) else {
        return;
    };
    let pid = match bus.get_connection_credentials(name.clone()).await {
        Ok(credentials) => credentials.process_id(),
        Err(_) => None,
    };
    if let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) {
        // SAFETY: `kill` takes no pointers; the pid is the daemon that owns
        // the name in this person's own session bus.
        let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
        if rc != 0 {
            warn!("could not stop the old keyring daemon");
        }
    }
    for _ in 0..40 {
        if !bus.name_has_owner(name.clone()).await.unwrap_or(false) {
            info!("the old keyring daemon released the Secret Service");
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    warn!("the old keyring daemon still holds the Secret Service");
}

#[cfg(test)]
mod tests;

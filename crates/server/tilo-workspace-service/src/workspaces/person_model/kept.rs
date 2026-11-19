//! A provider's key, read from a person's vault once for the asks of one
//! piece of work.
//!
//! Opening a vault reads a file and unseals it. A turn asks its model many
//! times one after the other, and between two asks the vault is nearly
//! always the file it was. So the key read for a provider is kept beside
//! what tells that file from the next one written, and the vault is opened
//! again only when that changed: a key its person removed or replaced is
//! gone, or new, at the very next ask. Nothing is kept of a vault that could
//! not be opened or that has no file, and nothing for another provider, or
//! another endpoint of it, than the one asked for.

use std::path::Path;
use std::sync::Mutex;

use tilo_lock::MutexRecover as _;

use super::super::person_settings::ProviderKeys;
use super::ModelRefusal;

/// What tells one written vault from the next: every save replaces the file
/// whole, as `accounts` itself tells them apart. None where no such file is
/// there, and then nothing is kept.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    inode: u64,
}

impl Stamp {
    fn of(root: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(accounts::vault_dir_in(root).join("vault.enc")).ok()?;
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            inode: std::os::unix::fs::MetadataExt::ino(&metadata),
        })
    }
}

/// What a vault kept for a provider when it was opened.
#[derive(Clone)]
enum Held {
    Key(zeroize::Zeroizing<String>),
    Nothing,
}

/// One opening of a vault: the file it was, the provider asked for and the
/// endpoint its key is bound to, and what the vault kept for it.
struct Opened {
    stamp: Stamp,
    provider: String,
    endpoint: String,
    held: Held,
}

/// The key one piece of work last took from its person's vault.
#[derive(Default)]
pub(super) struct Key {
    last: Mutex<Option<Opened>>,
    /// How often the vault was opened for this work.
    #[cfg(all(test, feature = "agents"))]
    opened: std::sync::atomic::AtomicU64,
}

impl Key {
    /// The key the vault under `root` keeps for `provider` at its endpoint's
    /// origin: none where it keeps none, or an empty one.
    pub(super) fn of(
        &self,
        root: &Path,
        provider: &agent_provider_config::ProviderProfile,
    ) -> Result<Option<zeroize::Zeroizing<String>>, ModelRefusal> {
        // Taken before the vault is opened: a save between the two leaves a
        // stamp older than what was taken, and the next ask opens it again.
        let stamp = Stamp::of(root);
        if let Some(held) = stamp.and_then(|stamp| self.held(stamp, provider)) {
            return Ok(match held {
                Held::Key(key) => Some(key),
                Held::Nothing => None,
            });
        }
        let key = ProviderKeys::of(root).get(provider).map_err(|error| {
            tracing::warn!(%error, "a person's vault was not read for their model");
            ModelRefusal::SettingsUnreadable
        })?;
        #[cfg(all(test, feature = "agents"))]
        self.opened
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *self.last.lock_recover() = stamp.map(|stamp| Opened {
            stamp,
            provider: provider.id.clone(),
            endpoint: provider.endpoint.clone(),
            held: key.clone().map_or(Held::Nothing, Held::Key),
        });
        Ok(key)
    }

    /// What was taken for `provider` from the vault that is still the file
    /// `stamp` tells.
    fn held(
        &self,
        stamp: Stamp,
        provider: &agent_provider_config::ProviderProfile,
    ) -> Option<Held> {
        self.last
            .lock_recover()
            .as_ref()
            .filter(|opened| {
                opened.stamp == stamp
                    && opened.provider == provider.id
                    && opened.endpoint == provider.endpoint
            })
            .map(|opened| opened.held.clone())
    }

    /// How often the vault was opened for this work.
    #[cfg(all(test, feature = "agents"))]
    pub(super) fn opened(&self) -> u64 {
        self.opened.load(std::sync::atomic::Ordering::Relaxed)
    }
}

//! A provider's key, taken from the workspace's vault once for the asks of one
//! piece of work.
//!
//! A turn asks its model many times one after the other, and between two asks
//! the key's entry is nearly always what it was. Taking the key out of the
//! vault writes a use into the entry; doing so for every ask of one piece of
//! work would write the same use again and again. So the key taken for a
//! provider is kept beside a mark of what its entry held, and every ask reads
//! the vault for that mark: the key is taken again, with its use, only when
//! the entry changed. A key its members removed or replaced is gone, or new,
//! at the very next ask. Nothing is kept for another provider, another
//! endpoint of it, or another entry than the one asked for.

use std::sync::Mutex;

use tilo_lock::MutexRecover as _;

use super::super::person_settings::{ProviderKeys, key_reference};
use super::ModelRefusal;

/// What one piece of work took from the vault for a provider: the provider, the endpoint its
/// key is bound to and the entry it names, a mark of what the entry held, and the key.
struct Taken {
    provider: String,
    endpoint: String,
    reference: String,
    mark: [u8; 32],
    key: zeroize::Zeroizing<String>,
}

/// The key one piece of work last took from the workspace's vault.
#[derive(Default)]
pub(super) struct Key {
    last: Mutex<Option<Taken>>,
    /// How often the key was taken out of the vault, a use written each time.
    #[cfg(all(test, feature = "agents"))]
    taken: std::sync::atomic::AtomicU64,
}

impl Key {
    /// The key the workspace's vault (`keys`) keeps for `provider` at its endpoint's origin:
    /// none where the provider names none, or the entry it names is gone or belongs elsewhere.
    pub(super) async fn of(
        &self,
        keys: ProviderKeys<'_>,
        provider: &agent_provider_config::ProviderProfile,
    ) -> Result<Option<zeroize::Zeroizing<String>>, ModelRefusal> {
        let unreadable = |error: String| {
            tracing::warn!(%error, "the workspace's vault was not read for its model");
            ModelRefusal::SettingsUnreadable
        };
        let Some(reference) = key_reference(provider) else {
            *self.last.lock_recover() = None;
            return Ok(None);
        };
        let Some(mark) = keys
            .vault()
            .sign_in_mark(&reference)
            .await
            .map_err(unreadable)?
        else {
            *self.last.lock_recover() = None;
            return Ok(None);
        };
        let reference = reference.to_string();
        if let Some(key) = self.held(provider, &reference, mark) {
            return Ok(Some(key));
        }
        let key = keys.get(provider).await.map_err(unreadable)?;
        #[cfg(all(test, feature = "agents"))]
        self.taken
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *self.last.lock_recover() = key.clone().map(|key| Taken {
            provider: provider.id.clone(),
            endpoint: provider.endpoint.clone(),
            reference,
            mark,
            key,
        });
        Ok(key)
    }

    /// What was taken for `provider` from the entry `reference` names, while it still holds
    /// what `mark` marks.
    fn held(
        &self,
        provider: &agent_provider_config::ProviderProfile,
        reference: &str,
        mark: [u8; 32],
    ) -> Option<zeroize::Zeroizing<String>> {
        self.last
            .lock_recover()
            .as_ref()
            .filter(|taken| {
                taken.mark == mark
                    && taken.reference == reference
                    && taken.provider == provider.id
                    && taken.endpoint == provider.endpoint
            })
            .map(|taken| taken.key.clone())
    }

    /// How often the key was taken out of the vault for this work.
    #[cfg(all(test, feature = "agents"))]
    pub(super) fn taken(&self) -> u64 {
        self.taken.load(std::sync::atomic::Ordering::Relaxed)
    }
}

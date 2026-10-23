//! The format-router. Owns the loaded set of provider modules and routes
//! detect / importAs / exportAs calls onto them. Shared by the Cap'n Proto
//! service and the in-process test harness.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use format_provider_abi::{CodecError, Detected};
use wasmi::Engine;

use crate::registry::{ProviderEntry, Registry};
use crate::runner::LoadedProvider;
use tilo_lock::RwLockRecover as _;

/// A provider as held by the router: the registry entry + its loaded module.
#[derive(Clone)]
pub struct ActiveProvider {
    pub entry: ProviderEntry,
    pub provider: LoadedProvider,
}

/// The in-memory provider catalogue. Cheap to clone; concurrent calls take
/// only a read-lock on the inner map.
#[derive(Clone)]
pub struct Router {
    engine: Engine,
    providers: Arc<RwLock<HashMap<String, ActiveProvider>>>,
}

impl Router {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            providers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    // A panic while the map was held leaves a complete provider set behind:
    // writers swap in a fully built map, never a partial one.
    fn providers_read(&self) -> RwLockReadGuard<'_, HashMap<String, ActiveProvider>> {
        self.providers.read_recover()
    }

    fn providers_write(&self) -> RwLockWriteGuard<'_, HashMap<String, ActiveProvider>> {
        self.providers.write_recover()
    }

    /// Replace the current provider set with `reg` — loads new modules,
    /// drops removed ones, reuses unchanged ones (matched by package, wasm
    /// path and limits). Used both on initial startup and on every inotify
    /// hot-reload.
    pub fn apply_registry(&self, reg: &Registry) -> anyhow::Result<()> {
        let mut next: HashMap<String, ActiveProvider> = HashMap::new();
        let current = self.providers_read().clone();
        for entry in &reg.providers {
            if let Some(existing) = current.get(&entry.package)
                && existing.entry.wasm == entry.wasm
                && existing.provider.limits() == reg.limits
            {
                // Same wasm file and budget → reuse the loaded module,
                // refresh the registry metadata.
                next.insert(
                    entry.package.clone(),
                    ActiveProvider {
                        entry: entry.clone(),
                        provider: existing.provider.clone(),
                    },
                );
                continue;
            }
            let provider = LoadedProvider::from_path(&self.engine, &entry.wasm, reg.limits)
                .map_err(|e| anyhow::anyhow!("loading provider {}: {e:#}", entry.package))?;
            next.insert(
                entry.package.clone(),
                ActiveProvider {
                    entry: entry.clone(),
                    provider,
                },
            );
        }
        *self.providers_write() = next;
        Ok(())
    }

    /// All loaded provider package ids, sorted for determinism.
    pub fn list(&self) -> Vec<String> {
        // Collect while the guard is live, then drop it before sorting the
        // (now independent) owned `Vec` — the lock isn't needed past this.
        let mut v: Vec<String> = self.providers_read().keys().cloned().collect();
        v.sort();
        v
    }

    pub fn get(&self, id: &str) -> Option<ActiveProvider> {
        self.providers_read().get(id).cloned()
    }

    /// Find the highest-tier provider that claims a MIME — used by `detect`
    /// when the caller passes a `hint`. Ties broken by package-id sort order
    /// for determinism.
    pub fn best_for_mime(&self, mime: &str) -> Option<ActiveProvider> {
        // Clone matches out while the guard is live, then drop it before
        // sorting the (now independent) owned candidates — the lock isn't
        // needed past this.
        let mut candidates: Vec<ActiveProvider> = self
            .providers_read()
            .values()
            .filter(|p| p.entry.mime == mime)
            .cloned()
            .collect();
        candidates.sort_by(|a, b| {
            b.entry
                .tier
                .cmp(&a.entry.tier)
                .then_with(|| a.entry.package.cmp(&b.entry.package))
        });
        candidates.into_iter().next()
    }

    /// `detect()`: try the hinted MIME first; if no provider matches, ask each
    /// loaded provider in package-id order and take the highest-scoring
    /// result. Returns `(detected, provider_id)` so the client knows which
    /// provider to route subsequent importAs calls to.
    pub fn detect(&self, head: &[u8], hint: &str) -> (Detected, String) {
        if !hint.is_empty()
            && let Some(p) = self.best_for_mime(hint)
            && let Ok(d) = p.provider.detect(head)
        {
            return (d, p.entry.package);
        }
        let providers = self.providers_read().clone();
        let mut keys: Vec<&String> = providers.keys().collect();
        keys.sort();
        let mut best: Option<(Detected, String)> = None;
        for k in keys {
            // `k` came from `providers.keys()` just above, so it's always present.
            let Some(p) = providers.get(k) else {
                continue;
            };
            if let Ok(d) = p.provider.detect(head)
                && best
                    .as_ref()
                    .is_none_or(|(b, _)| d.confidence.score > b.confidence.score)
            {
                best = Some((d, p.entry.package.clone()));
            }
        }
        best.unwrap_or_else(|| {
            (
                Detected {
                    confidence: format_provider_abi::Confidence {
                        tier: 0,
                        score: 0.0,
                    },
                    mime: None,
                },
                String::new(),
            )
        })
    }

    /// Import a document in a provider's native format and return canonical CRDT bytes.
    pub fn import_as(&self, bytes: &[u8], provider_id: &str) -> Result<Vec<u8>, CodecError> {
        let p = self
            .get(provider_id)
            .ok_or_else(|| CodecError::unsupported(format!("no provider: {provider_id}")))?;
        p.provider.import_doc(bytes)
    }

    /// Export canonical CRDT bytes to a provider's native format.
    pub fn export_as(&self, canonical: &[u8], provider_id: &str) -> Result<Vec<u8>, CodecError> {
        let p = self
            .get(provider_id)
            .ok_or_else(|| CodecError::unsupported(format!("no provider: {provider_id}")))?;
        p.provider.export_doc(canonical)
    }
}

/// Load + apply a registry file in one shot.
pub fn load_into(router: &Router, path: &Path) -> anyhow::Result<()> {
    let reg = Registry::load(path)?;
    router.apply_registry(&reg)
}

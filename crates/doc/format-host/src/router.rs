//! The format-router. Owns the loaded set of WASM components and routes
//! detect / importAs / exportAs calls onto them. Shared by the Cap'n Proto
//! service and the in-process test harness.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use format_provider_wit::CodecError;
use parking_lot::RwLock;
use wasmtime::Engine;

use crate::registry::{ProviderEntry, Registry};
use crate::runner::LoadedComponent;

/// A provider as held by the router: the registry entry + its loaded component.
#[derive(Clone)]
pub struct ActiveProvider {
    pub entry: ProviderEntry,
    pub component: LoadedComponent,
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

    /// Replace the current provider set with `reg` — loads new components,
    /// drops removed ones, reuses unchanged ones (matched by package + wasm
    /// path). Used both on initial startup and on every inotify hot-reload.
    ///
    /// # Errors
    ///
    /// Returns an error if loading a component from its WASM path fails.
    pub fn apply_registry(&self, reg: &Registry) -> anyhow::Result<()> {
        let mut next: HashMap<String, ActiveProvider> = HashMap::new();
        let current = self.providers.read().clone();
        for entry in &reg.providers {
            if let Some(existing) = current.get(&entry.package)
                && existing.entry.wasm == entry.wasm
            {
                // Same wasm file → reuse the loaded component, refresh
                // the registry metadata.
                next.insert(
                    entry.package.clone(),
                    ActiveProvider {
                        entry: entry.clone(),
                        component: existing.component.clone(),
                    },
                );
                continue;
            }
            let comp = LoadedComponent::from_path(&self.engine, &entry.wasm)
                .map_err(|e| anyhow::anyhow!("loading provider {}: {e:#}", entry.package))?;
            next.insert(
                entry.package.clone(),
                ActiveProvider {
                    entry: entry.clone(),
                    component: comp,
                },
            );
        }
        *self.providers.write() = next;
        Ok(())
    }

    /// All loaded provider package ids, sorted for determinism.
    pub fn list(&self) -> Vec<String> {
        // Collect while the guard is live, then drop it before sorting the
        // (now independent) owned `Vec` — the lock isn't needed past this.
        let mut v: Vec<String> = self.providers.read().keys().cloned().collect();
        v.sort();
        v
    }

    pub fn get(&self, id: &str) -> Option<ActiveProvider> {
        self.providers.read().get(id).cloned()
    }

    /// Find the highest-tier provider that claims a MIME — used by `detect`
    /// when the caller passes a `hint`. Ties broken by package-id sort order
    /// for determinism.
    pub fn best_for_mime(&self, mime: &str) -> Option<ActiveProvider> {
        // Clone matches out while the guard is live, then drop it before
        // sorting the (now independent) owned candidates — the lock isn't
        // needed past this.
        let mut candidates: Vec<ActiveProvider> = self
            .providers
            .read()
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
    pub fn detect(&self, head: &[u8], hint: &str) -> (format_provider_wit::Detected, String) {
        if !hint.is_empty()
            && let Some(p) = self.best_for_mime(hint)
            && let Ok(d) = p.component.detect(head)
        {
            return (
                format_provider_wit::Detected {
                    confidence: format_provider_wit::Confidence {
                        tier: d.confidence.tier,
                        score: d.confidence.score,
                    },
                    mime: d.mime,
                },
                p.entry.package,
            );
        }
        let providers = self.providers.read().clone();
        let mut keys: Vec<&String> = providers.keys().collect();
        keys.sort();
        let mut best: Option<(format_provider_wit::Detected, String, f32)> = None;
        for k in keys {
            // `k` came from `providers.keys()` just above, so it's always present.
            let Some(p) = providers.get(k) else {
                continue;
            };
            if let Ok(d) = p.component.detect(head) {
                let score = d.confidence.score;
                let detected = format_provider_wit::Detected {
                    confidence: format_provider_wit::Confidence {
                        tier: d.confidence.tier,
                        score: d.confidence.score,
                    },
                    mime: d.mime,
                };
                if best.as_ref().is_none_or(|(_, _, s)| score > *s) {
                    best = Some((detected, p.entry.package.clone(), score));
                }
            }
        }
        match best {
            Some((d, id, _)) => (d, id),
            None => (
                format_provider_wit::Detected {
                    confidence: format_provider_wit::Confidence {
                        tier: 0,
                        score: 0.0,
                    },
                    mime: None,
                },
                String::new(),
            ),
        }
    }

    /// Import a document in a provider's native format and return canonical CRDT bytes.
    ///
    /// # Errors
    ///
    /// Returns a `CodecError` if the provider is not found or if importing fails.
    pub fn import_as(&self, bytes: &[u8], provider_id: &str) -> Result<Vec<u8>, CodecError> {
        let p = self
            .get(provider_id)
            .ok_or_else(|| CodecError::unsupported(format!("no provider: {provider_id}")))?;
        p.component.import_doc(bytes)
    }

    /// Export canonical CRDT bytes to a provider's native format.
    ///
    /// # Errors
    ///
    /// Returns a `CodecError` if the provider is not found or if exporting fails.
    pub fn export_as(&self, canonical: &[u8], provider_id: &str) -> Result<Vec<u8>, CodecError> {
        let p = self
            .get(provider_id)
            .ok_or_else(|| CodecError::unsupported(format!("no provider: {provider_id}")))?;
        p.component.export_doc(canonical)
    }
}

/// Load + apply a registry file in one shot.
///
/// # Errors
///
/// Returns an error if loading or parsing the registry file fails, or if applying
/// the registry to the router fails.
pub fn load_into(router: &Router, path: &Path) -> anyhow::Result<()> {
    let reg = Registry::load(path)?;
    router.apply_registry(&reg)
}

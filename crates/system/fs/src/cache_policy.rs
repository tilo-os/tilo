//! The person's cache policy for this device: whether predicted files are
//! fetched ahead of their open, and how many bytes of fetched content the
//! content store keeps.
//!
//! Settings changes it through the storage socket
//! ([`runtime_fs::storage::StorageRequest::SetCachePolicy`]); the daemon
//! keeps it in its cache directory so it survives restarts, and applies it
//! to the predictive worker and the content store's file area at once.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use runtime_fs::storage::{CachePolicy, MIN_CACHE_BUDGET_BYTES};
use serde::{Deserialize, Serialize};

use crate::smart::Heuristics;
use crate::store::area::FileArea;
use tilo_lock::MutexRecover as _;

/// The file in the cache directory that holds the policy.
pub const CACHE_POLICY_FILE: &str = "cache-policy.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    prefetch: bool,
    budget_bytes: Option<u64>,
}

/// The policy in force, where it is kept, and what it governs.
pub struct CachePolicies {
    path: Option<PathBuf>,
    heuristics: Arc<Heuristics>,
    files: Arc<FileArea>,
    /// The area's budget before any policy: what `budget_bytes: None` means.
    default_budget: u64,
    current: Mutex<CachePolicy>,
}

impl CachePolicies {
    /// Load the policy kept at `path` (the default when there is none) and
    /// apply it. `None` keeps the policy in memory only.
    ///
    /// # Errors
    ///
    /// Returns an error when a kept policy cannot be read or is invalid; the
    /// daemon then starts with the default rather than guessing.
    pub fn load(
        path: Option<PathBuf>,
        heuristics: Arc<Heuristics>,
        files: Arc<FileArea>,
    ) -> (Self, Result<()>) {
        let default_budget = files.budget();
        let policies = Self {
            path,
            heuristics,
            files,
            default_budget,
            current: Mutex::new(CachePolicy::default()),
        };
        let loaded = policies.read_kept().and_then(|kept| {
            let Some(kept) = kept else {
                return Ok(());
            };
            validate(kept)?;
            policies.apply(kept);
            Ok(())
        });
        (policies, loaded)
    }

    fn read_kept(&self) -> Result<Option<CachePolicy>> {
        let Some(path) = &self.path else {
            return Ok(None);
        };
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let stored: Stored =
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        Ok(Some(CachePolicy {
            prefetch: stored.prefetch,
            budget_bytes: stored.budget_bytes,
        }))
    }

    /// The policy in force.
    #[must_use]
    pub fn current(&self) -> CachePolicy {
        *self.current.lock_recover()
    }

    /// Keep and apply `policy`. A smaller budget evicts down to it at once.
    ///
    /// # Errors
    ///
    /// Returns an error for a budget below [`MIN_CACHE_BUDGET_BYTES`] or
    /// when the policy cannot be kept; nothing changes then.
    pub fn set(&self, policy: CachePolicy) -> Result<CachePolicy> {
        validate(policy)?;
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec_pretty(&Stored {
                prefetch: policy.prefetch,
                budget_bytes: policy.budget_bytes,
            })?;
            tilo_files::native::atomic::write_creating_parent(path, &bytes)
                .with_context(|| format!("write {}", path.display()))?;
        }
        self.apply(policy);
        Ok(policy)
    }

    fn apply(&self, policy: CachePolicy) {
        *self.current.lock_recover() = policy;
        self.heuristics.set_prefetch_enabled(policy.prefetch);
        self.files
            .set_budget(policy.budget_bytes.unwrap_or(self.default_budget));
        self.files.evict_to_budget(&[]);
    }
}

fn validate(policy: CachePolicy) -> Result<()> {
    if let Some(budget) = policy.budget_bytes
        && budget < MIN_CACHE_BUDGET_BYTES
    {
        bail!(
            "a cache budget below {MIN_CACHE_BUDGET_BYTES} bytes would evict files as they are prefetched"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Hydrator;
    use crate::store::ContentStore;
    use crate::store::area::Retention;
    use iroh::endpoint::{Endpoint, presets};

    async fn fixture() -> (Endpoint, Arc<Heuristics>, Arc<FileArea>) {
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let files = Arc::clone(store.files());
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store));
        let heuristics = Arc::new(Heuristics::new(hydrator));
        (endpoint, heuristics, files)
    }

    #[tokio::test]
    async fn the_policy_is_applied_kept_and_restored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_POLICY_FILE);
        let (endpoint, heuristics, files) = fixture().await;
        let default_budget = files.budget();
        let (policies, loaded) = CachePolicies::load(
            Some(path.clone()),
            Arc::clone(&heuristics),
            Arc::clone(&files),
        );
        loaded.unwrap();
        assert_eq!(policies.current(), CachePolicy::default());

        // Two cached entries over a 1 GiB budget (sparse: the accounting is
        // what the budget reads): the older one goes as soon as the smaller
        // budget is set.
        let size = MIN_CACHE_BUDGET_BYTES / 2 + 1;
        for key in [[1; 32], [2; 32]] {
            files.ensure(&key, size, Retention::Cached, &[]).unwrap();
            files
                .mark_resident(&key, std::slice::from_ref(&(0..size)), false)
                .unwrap();
            files.touch(&key);
        }
        let smaller = CachePolicy {
            prefetch: false,
            budget_bytes: Some(MIN_CACHE_BUDGET_BYTES),
        };
        assert_eq!(policies.set(smaller).unwrap(), smaller);
        assert_eq!(files.budget(), MIN_CACHE_BUDGET_BYTES);
        assert!(files.entry(&[1; 32]).is_none() && files.entry(&[2; 32]).is_some());
        assert!(!heuristics.prefetch_enabled());

        // A restarted daemon applies the kept policy before serving.
        let (reborn_endpoint, reborn_heuristics, reborn_files) = fixture().await;
        let (restored, loaded) = CachePolicies::load(
            Some(path.clone()),
            Arc::clone(&reborn_heuristics),
            Arc::clone(&reborn_files),
        );
        loaded.unwrap();
        assert_eq!(restored.current(), smaller);
        assert!(!reborn_heuristics.prefetch_enabled());
        assert_eq!(reborn_files.budget(), MIN_CACHE_BUDGET_BYTES);

        // `None` is the store's own default again.
        policies.set(CachePolicy::default()).unwrap();
        assert_eq!(files.budget(), default_budget);
        assert!(heuristics.prefetch_enabled());
        endpoint.close().await;
        reborn_endpoint.close().await;
    }

    #[tokio::test]
    async fn a_budget_below_the_minimum_or_a_broken_file_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_POLICY_FILE);
        let (endpoint, heuristics, files) = fixture().await;
        let (policies, loaded) = CachePolicies::load(
            Some(path.clone()),
            Arc::clone(&heuristics),
            Arc::clone(&files),
        );
        loaded.unwrap();
        let budget = files.budget();
        assert!(
            policies
                .set(CachePolicy {
                    prefetch: false,
                    budget_bytes: Some(MIN_CACHE_BUDGET_BYTES - 1),
                })
                .is_err()
        );
        assert_eq!(policies.current(), CachePolicy::default());
        assert_eq!(files.budget(), budget);
        assert!(heuristics.prefetch_enabled());
        assert!(!path.exists(), "a refused policy is not kept");

        std::fs::write(&path, b"{\"prefetch\": false, \"budget_bytes\": 12}").unwrap();
        let (policies, loaded) =
            CachePolicies::load(Some(path), Arc::clone(&heuristics), Arc::clone(&files));
        assert!(loaded.is_err());
        assert_eq!(policies.current(), CachePolicy::default());
        assert!(heuristics.prefetch_enabled());
        endpoint.close().await;
    }
}

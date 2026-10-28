//! Conservative predictive hydration.
//!
//! Opening one file schedules the next visible siblings of the same type. The
//! FUSE layer supplies the candidates after applying its filtered-view ACL, so
//! this module can never turn an inaccessible sibling into locally cached
//! content. Work is bounded by file size, candidate count (at the caller), and
//! a small global concurrency limit; duplicate opens share one pending job.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use iroh_blobs::Hash;
use tokio::sync::mpsc;

use crate::hydration::Hydrator;
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

pub mod history;
pub mod kept_history;

/// Predictive reads are an optimization, not an instruction to fill the disk.
/// Larger files remain projected until explicitly opened or pinned.
pub const MAX_PREDICTIVE_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_CONCURRENT_PREFETCHES: usize = 2;
const PREFETCH_QUEUE_CAPACITY: usize = 32;
pub const DEFAULT_MAX_QUEUED_PREDICTIVE_BYTES: u64 = 128 * 1024 * 1024;

/// Runtime policy supplied by the system's network, power and storage-pressure
/// monitors. A false gate cancels queued/running speculation; foreground reads
/// are deliberately outside this policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent runtime gates are the policy's public contract"
)]
pub struct PredictivePolicy {
    pub enabled: bool,
    pub network_allows: bool,
    pub power_allows: bool,
    pub storage_allows: bool,
    pub max_queued_bytes: u64,
}

impl Default for PredictivePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            network_allows: true,
            power_allows: true,
            storage_allows: true,
            max_queued_bytes: DEFAULT_MAX_QUEUED_PREDICTIVE_BYTES,
        }
    }
}

impl PredictivePolicy {
    fn permits(self) -> bool {
        self.enabled
            && self.network_allows
            && self.power_allows
            && self.storage_allows
            && self.max_queued_bytes > 0
    }
}

#[derive(Clone, Copy)]
struct PrefetchJob {
    hash: Hash,
    generation: u64,
}

fn admit_pending(
    pending: &mut HashMap<Hash, u64>,
    hash: Hash,
    size: u64,
    policy: PredictivePolicy,
) -> bool {
    if !policy.permits() || size > MAX_PREDICTIVE_FILE_BYTES || pending.contains_key(&hash) {
        return false;
    }
    let queued_bytes = pending.values().copied().sum::<u64>();
    if queued_bytes.saturating_add(size) > policy.max_queued_bytes {
        return false;
    }
    pending.insert(hash, size);
    true
}

pub struct Heuristics {
    pending: Arc<Mutex<HashMap<Hash, u64>>>,
    /// What the worker obeys: the system's policy, with `enabled` cleared
    /// while the person has turned prediction off.
    policy: Arc<RwLock<PredictivePolicy>>,
    /// The latest policy from the network, power and storage monitors, and
    /// the person's own switch ([`Self::set_prefetch_enabled`]).
    inputs: Mutex<(PredictivePolicy, bool)>,
    generation: Arc<AtomicU64>,
    cancel: tokio::sync::watch::Sender<u64>,
    queue: mpsc::Sender<PrefetchJob>,
    /// Set by every open, cleared by [`Self::take_opened`]; the policy
    /// monitor samples power and storage only while files are opened.
    opened: Arc<std::sync::atomic::AtomicBool>,
    /// Signalled by every open, so the monitor resumes sampling at once.
    activity: Arc<tokio::sync::Notify>,
    /// Run whenever the person turns prediction off: what was learned for
    /// it is forgotten with it.
    prefetch_off: Mutex<Vec<PrefetchOffHook>>,
}

/// What [`Heuristics::on_prefetch_off`] runs.
pub type PrefetchOffHook = Box<dyn Fn() + Send + Sync>;

impl Heuristics {
    /// Construct the bounded predictive worker on the hydrator's runtime.
    pub fn new(hydrator: Arc<Hydrator>) -> Self {
        let (queue, mut jobs) = mpsc::channel::<PrefetchJob>(PREFETCH_QUEUE_CAPACITY);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let policy = Arc::new(RwLock::new(PredictivePolicy::default()));
        let generation = Arc::new(AtomicU64::new(0));
        let (cancel, _) = tokio::sync::watch::channel(0_u64);
        let runtime = hydrator.runtime_handle.clone();
        let worker_hydrator = hydrator;
        let worker_pending = pending.clone();
        let worker_policy = policy.clone();
        let worker_generation = generation.clone();
        let worker_cancel = cancel.clone();
        runtime.spawn(async move {
            let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PREFETCHES));
            while let Some(job) = jobs.recv().await {
                if job.generation != worker_generation.load(Ordering::Acquire)
                    || !worker_policy
                        .read_recover()
                        .permits()
                {
                    worker_pending
                        .lock_recover()
                        .remove(&job.hash);
                    continue;
                }
                let Ok(permit) = slots.clone().acquire_owned().await else {
                    worker_pending
                        .lock_recover()
                        .remove(&job.hash);
                    continue;
                };
                if job.generation != worker_generation.load(Ordering::Acquire)
                    || !worker_policy
                        .read_recover()
                        .permits()
                {
                    worker_pending
                        .lock_recover()
                        .remove(&job.hash);
                    continue;
                }
                let hydrator = worker_hydrator.clone();
                let pending = worker_pending.clone();
                let mut cancellation = worker_cancel.subscribe();
                // At most MAX_CONCURRENT_PREFETCHES child tasks exist; excess
                // work remains in the bounded channel rather than becoming an
                // unbounded task-per-open backlog.
                tokio::spawn(async move {
                    let _permit = permit;
                    if hydrator.sync_state(job.hash).await != fs_protocol::SyncState::Local {
                        // Best effort by design: a foreground read owns
                        // user-visible errors and retries; prediction must
                        // never make open fail.
                        tokio::select! {
                            biased;
                            _ = cancellation.wait_for(|generation| *generation != job.generation) => {}
                            _ = hydrator.prefetch(job.hash, MAX_PREDICTIVE_FILE_BYTES) => {}
                        }
                    }
                    pending
                        .lock_recover()
                        .remove(&job.hash);
                });
            }
        });
        Self {
            pending,
            policy,
            inputs: Mutex::new((PredictivePolicy::default(), true)),
            generation,
            cancel,
            queue,
            opened: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            activity: Arc::new(tokio::sync::Notify::new()),
            prefetch_off: Mutex::new(Vec::new()),
        }
    }

    /// Run `hook` each time the person turns prediction off, after the
    /// worker has stopped.
    pub fn on_prefetch_off(&self, hook: PrefetchOffHook) {
        self.prefetch_off.lock_recover().push(hook);
    }

    /// Whether a file was opened since the last call.
    pub fn take_opened(&self) -> bool {
        self.opened.swap(false, Ordering::AcqRel)
    }

    /// Wait for the next open; returns at once if one happened since the
    /// last [`Self::take_opened`] began waiting.
    pub async fn opened(&self) {
        self.activity.notified().await;
    }

    /// Replace the system's speculative policy. Tightening any gate
    /// invalidates every queued/running prediction; reopening policy starts
    /// with a fresh generation and never resurrects stale hints.
    pub fn set_policy(&self, policy: PredictivePolicy) {
        let mut inputs = self.inputs.lock_recover();
        inputs.0 = policy;
        self.apply(*inputs);
    }

    /// The person's switch for prediction, from Settings: off cancels what
    /// is queued or running and admits nothing more, whatever the system's
    /// gates say; on leaves those gates in charge.
    pub fn set_prefetch_enabled(&self, enabled: bool) {
        {
            let mut inputs = self.inputs.lock_recover();
            inputs.1 = enabled;
            self.apply(*inputs);
        }
        if !enabled {
            let hooks = self.prefetch_off.lock_recover();
            for hook in hooks.iter() {
                hook();
            }
        }
    }

    /// Whether the person allows prediction.
    #[must_use]
    pub fn prefetch_enabled(&self) -> bool {
        self.inputs.lock_recover().1
    }

    fn apply(&self, (system, enabled): (PredictivePolicy, bool)) {
        *self.policy.write_recover() = PredictivePolicy {
            enabled: system.enabled && enabled,
            ..system
        };
        let generation = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        self.pending.lock_recover().clear();
        let _ = self.cancel.send(generation);
    }

    #[must_use]
    pub fn policy(&self) -> PredictivePolicy {
        *self.policy.read_recover()
    }

    /// Current admitted speculative work as `(jobs, declared_bytes)`.
    ///
    /// This is intentionally a snapshot rather than a completion promise: a
    /// worker can finish immediately after the lock is released. It gives the
    /// daemon/diagnostics a bounded, truthful accounting surface without
    /// exposing the queue implementation.
    #[must_use]
    pub fn pending_work(&self) -> (usize, u64) {
        let pending = self.pending.lock_recover();
        (pending.len(), pending.values().copied().sum())
    }

    /// Called after a file inode has passed the real open-time ACL check.
    /// `candidates` must likewise come from the mount's filtered directory
    /// listing. Each tuple is `(manifest_hash, declared_file_size)`.
    pub fn on_open(&self, _ino: u64, candidates: Vec<(Hash, u64)>) {
        if !self.opened.swap(true, Ordering::AcqRel) {
            self.activity.notify_one();
        }
        let policy = self.policy();
        if !policy.permits() {
            return;
        }
        let generation = self.generation.load(Ordering::Acquire);
        for (hash, size) in candidates {
            if size > MAX_PREDICTIVE_FILE_BYTES {
                continue;
            }
            {
                let mut pending = self.pending.lock_recover();
                if !admit_pending(&mut pending, hash, size, policy) {
                    continue;
                }
            }
            if self
                .queue
                .try_send(PrefetchJob { hash, generation })
                .is_err()
            {
                // Queue pressure is a reason to skip speculation, never to
                // delay the foreground open that produced the hint.
                self.pending.lock_recover().remove(&hash);
            }
        }
    }

    /// Called when a directory is listed.
    pub fn on_readdir(&self, _ino: u64) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ContentStore;
    use bytes::Bytes;
    use iroh::endpoint::{Endpoint, presets};
    use iroh::protocol::Router;

    #[test]
    fn speculation_is_gated_and_budgeted() {
        {
            let base = PredictivePolicy::default();
            for policy in [
                PredictivePolicy {
                    enabled: false,
                    ..base
                },
                PredictivePolicy {
                    network_allows: false,
                    ..base
                },
                PredictivePolicy {
                    power_allows: false,
                    ..base
                },
                PredictivePolicy {
                    storage_allows: false,
                    ..base
                },
                PredictivePolicy {
                    max_queued_bytes: 0,
                    ..base
                },
            ] {
                let mut pending = HashMap::new();
                assert!(!admit_pending(&mut pending, Hash::from([1; 32]), 1, policy));
                assert!(pending.is_empty());
            }
        }
        {
            let policy = PredictivePolicy {
                max_queued_bytes: 10,
                ..PredictivePolicy::default()
            };
            let first = Hash::from([1; 32]);
            let second = Hash::from([2; 32]);
            let mut pending = HashMap::new();
            assert!(admit_pending(&mut pending, first, 6, policy));
            assert!(!admit_pending(&mut pending, first, 1, policy));
            assert!(!admit_pending(&mut pending, second, 5, policy));
            assert!(admit_pending(&mut pending, second, 4, policy));
            assert_eq!(pending.values().copied().sum::<u64>(), 10);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_persons_switch_overrides_the_system_gates() {
        let endpoint = Endpoint::builder(presets::Minimal)
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(
            endpoint.clone(),
            Arc::new(ContentStore::memory()),
        ));
        let heuristics = Heuristics::new(hydrator);
        let candidate = vec![(Hash::from([3; 32]), 10)];

        heuristics.set_prefetch_enabled(false);
        // A later sample from the monitors must not turn prediction back on.
        heuristics.set_policy(PredictivePolicy::default());
        assert!(!heuristics.prefetch_enabled());
        assert!(!heuristics.policy().enabled);
        heuristics.on_open(1, candidate.clone());
        assert_eq!(heuristics.pending_work(), (0, 0), "nothing is admitted");

        heuristics.set_prefetch_enabled(true);
        assert!(heuristics.policy().enabled);
        heuristics.set_policy(PredictivePolicy {
            network_allows: false,
            ..PredictivePolicy::default()
        });
        assert!(
            heuristics.prefetch_enabled() && !heuristics.policy().network_allows,
            "the switch leaves the system's gates in charge"
        );
        drop(heuristics);
        endpoint.close().await;
    }

    /// #4224. The policy monitor sleeps until a file is opened: the first
    /// open after a quiet interval wakes it, and opens are reported once
    /// per interval however many there were.
    #[tokio::test]
    async fn an_open_wakes_the_policy_monitor() {
        let endpoint = Endpoint::builder(presets::Minimal)
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(
            endpoint.clone(),
            Arc::new(ContentStore::memory()),
        ));
        let heuristics = Heuristics::new(hydrator);
        assert!(!heuristics.take_opened(), "nothing was opened yet");
        assert!(
            std::pin::pin!(heuristics.opened())
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                .is_pending(),
            "an idle mount does not wake the monitor"
        );
        heuristics.on_open(7, Vec::new());
        heuristics.on_open(8, Vec::new());
        tilo_test_env::within("the first open wakes the monitor", heuristics.opened()).await;
        assert!(heuristics.take_opened());
        assert!(!heuristics.take_opened(), "reported once");
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_prediction_hydrates_a_real_sibling_and_drains_accounting() {
        let server_endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let server_store = ContentStore::memory();
        let sibling = Bytes::from_static(b"the next image's exact bytes");
        let sibling_hash = server_store.write_content(&sibling).await.unwrap().unwrap();
        let router = Router::builder(server_endpoint.clone())
            .accept(iroh_blobs::ALPN, server_store.blobs_protocol(None))
            .spawn();

        let client_endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let client_store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(client_endpoint.clone(), client_store.clone()));
        hydrator.add_peer(server_endpoint.addr());
        let heuristics = Heuristics::new(hydrator.clone());

        assert_eq!(
            hydrator.sync_state(sibling_hash).await,
            fs_protocol::SyncState::Projected
        );
        heuristics.on_open(7, vec![(sibling_hash, sibling.len() as u64)]);
        assert_eq!(
            heuristics.pending_work(),
            (1, sibling.len() as u64),
            "the admitted prediction is observable and byte-bounded"
        );

        tilo_test_env::within("the open-triggered sibling prefetch", async {
            loop {
                if hydrator.sync_state(sibling_hash).await == fs_protocol::SyncState::Local
                    && heuristics.pending_work() == (0, 0)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert_eq!(
            hydrator
                .read(sibling_hash, 0, sibling.len() as u64)
                .await
                .unwrap(),
            sibling
        );

        drop(heuristics);
        router.shutdown().await.unwrap();
        client_endpoint.close().await;
        server_endpoint.close().await;
        client_store.store().shutdown().await.unwrap();
    }
}

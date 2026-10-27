//! Fail-closed planning for local collaborative-file eviction.
//!
//! A file payload is a manifest plus content-defined chunks.  Chunks and even
//! whole manifests may be shared by several records or retained versions, so
//! deleting "the blobs for one file" requires a complete, version-bound
//! reference catalog.  This module computes that set without mutating storage.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fs_protocol::{FileRecordId, FileVersionId};
use futures_util::stream::{self, StreamExt, TryStreamExt};
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::availability::{
    AvailabilityRegistry, EvictionBlocker, EvictionDecision, EvictionRequirements,
    RecordAvailability, RecordKind, pinned_in_snapshot,
};
use crate::clock::unix_now_ms;
use crate::{FileHistoryStore, Hydrator};
use fs_protocol::{AvailabilityPolicy, Residency};
use tilo_lock::MutexRecover as _;

/// Bound catalog I/O so opening Settings does not serialize one backend
/// status query per chunk across a large workspace.
const MAX_CONCURRENT_CATALOG_INSPECTIONS: usize = 8;

/// The one process-local bridge from durable eviction intent to backend GC.
/// Both FUSE controls and Settings use this coordinator so no caller can
/// silently replace exact-version receipts, retained-history checks, or the
/// exact absence observation with a Hydrator-lifetime approximation.
#[derive(Clone)]
pub struct EvictionCoordinator {
    hydrator: Arc<Hydrator>,
    registry: Arc<Mutex<AvailabilityRegistry>>,
    queue: Arc<tokio::sync::Mutex<EvictionQueue>>,
    history: Option<Arc<FileHistoryStore>>,
}

impl EvictionCoordinator {
    pub fn new(
        hydrator: Arc<Hydrator>,
        registry: Arc<Mutex<AvailabilityRegistry>>,
        queue: Arc<tokio::sync::Mutex<EvictionQueue>>,
        history: Option<Arc<FileHistoryStore>>,
    ) -> Self {
        Self {
            hydrator,
            registry,
            queue,
            history,
        }
    }

    /// Produce the same fail-closed dry run used immediately before GC.
    ///
    /// # Errors
    ///
    /// Returns an error when policy, history, or payload inspection cannot
    /// establish one coherent exact-version plan.
    pub async fn plan(
        &self,
        record_id: FileRecordId,
        mode: EvictionMode,
    ) -> anyhow::Result<EvictionPlan> {
        let records = self
            .registry
            .lock_recover()
            .records()
            .cloned()
            .collect::<Vec<_>>();
        let catalog = self.catalog_for_targets(&records, &[record_id]).await?;
        let registry = self.registry.lock_recover();
        if registry.records().cloned().collect::<Vec<_>>() != records {
            anyhow::bail!("availability changed during eviction planning");
        }
        let plan = plan_for_mode(&registry, &catalog, record_id, mode)?;
        drop(registry);
        Ok(plan)
    }

    /// Build one coherent payload catalog for multiple Settings rows, then
    /// evaluate each safe plan against that same captured registry generation.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry changes during catalog inspection or
    /// the history/payload catalog cannot be constructed safely.
    pub async fn batch_safe_plans(
        &self,
        targets: &[FileRecordId],
    ) -> anyhow::Result<HashMap<FileRecordId, Option<EvictionPlan>>> {
        let records = self
            .registry
            .lock_recover()
            .records()
            .cloned()
            .collect::<Vec<_>>();
        let catalog = self.catalog_for_targets(&records, targets).await?;
        let registry = self.registry.lock_recover();
        if registry.records().cloned().collect::<Vec<_>>() != records {
            anyhow::bail!("availability changed during eviction planning");
        }
        let plans = batch_plan_safe(&registry, &catalog, targets, unix_now_ms());
        drop(registry);
        Ok(plans)
    }

    /// Enqueue and freshly authorize one safe request. `expected_hash`, when
    /// supplied by Settings' snapshot token, binds the action to the exact
    /// immutable manifest it previewed.
    ///
    /// # Errors
    ///
    /// Returns an error if the exact version, policy roots, queue, or backend
    /// GC authorization cannot be verified and persisted.
    pub async fn prepare(
        &self,
        record_id: FileRecordId,
        mode: EvictionMode,
        existing_request: Option<EvictionRequestId>,
        expected_hash: Option<Hash>,
    ) -> anyhow::Result<(EvictionRequestId, EvictionPlan)> {
        let store = Arc::clone(&self.hydrator.store);
        let runtime = self.handoff_runtime();
        self.prepare_with_handoff(
            record_id,
            mode,
            existing_request,
            expected_hash,
            move |plan, existing_request, queue, registry, catalog, now_ms| {
                let request_id = match existing_request {
                    Some(request_id) => request_id,
                    None => queue.enqueue(plan, now_ms)?.id,
                };
                runtime.block_on(store.release_eviction_to_backend_gc(
                    queue,
                    request_id,
                    registry,
                    catalog,
                    now_ms,
                    EvictionRequirements::default(),
                ))?;
                Ok(request_id)
            },
        )
        .await
    }

    /// The runtime a handoff waits on the store through. The handoff holds
    /// the availability lock, which the daemon's workers take too; a worker
    /// blocked on it can leave the daemon's timers undriven, and the backend
    /// sweep waits on timers. The store's own runtime never takes that lock.
    #[must_use]
    pub fn handoff_runtime(&self) -> tokio::runtime::Handle {
        self.hydrator
            .store
            .runtime()
            .unwrap_or_else(|| self.hydrator.runtime_handle.clone())
    }

    /// As [`Self::prepare`], but makes the actual backend-GC handoff an
    /// explicit caller-supplied critical section. Storage uses this to retain
    /// its live namespace and dirty guards from validation through durable
    /// enqueue and tag release.
    ///
    /// The handoff deliberately runs while the availability and queue locks
    /// are held. It is not a preview-time hint. Those are synchronous guards,
    /// so the whole critical section runs on a blocking thread, where the
    /// handoff may wait on async store work with
    /// [`Self::handoff_runtime`]'s `block_on` — once, never nested.
    ///
    /// # Errors
    ///
    /// Returns the same planning and persistence errors as [`Self::prepare`],
    /// or an error from `handoff` when its live validation, persistence, or
    /// backend release fails.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "policy must remain stable from exact-plan revalidation through backend tag release"
    )]
    pub async fn prepare_with_handoff<F>(
        &self,
        record_id: FileRecordId,
        mode: EvictionMode,
        existing_request: Option<EvictionRequestId>,
        expected_hash: Option<Hash>,
        handoff: F,
    ) -> anyhow::Result<(EvictionRequestId, EvictionPlan)>
    where
        F: FnOnce(
                &EvictionPlan,
                Option<EvictionRequestId>,
                &mut EvictionQueue,
                &AvailabilityRegistry,
                &EvictionCatalog,
                u64,
            ) -> anyhow::Result<EvictionRequestId>
            + Send
            + 'static,
    {
        let records = self
            .registry
            .lock_recover()
            .records()
            .cloned()
            .collect::<Vec<_>>();
        if let Some(expected_hash) = expected_hash {
            let current = records
                .iter()
                .find(|record| record.record_id == record_id)
                .and_then(|record| record.current_version)
                .ok_or_else(|| anyhow::anyhow!("record has no exact current version"))?;
            let version = self
                .history
                .as_ref()
                .and_then(|history| history.snapshot(record_id))
                .and_then(|snapshot| {
                    snapshot
                        .versions
                        .into_iter()
                        .find(|version| version.id == current)
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("version history changed during eviction planning")
                })?;
            if version.content_hash != Some(fs_protocol::ContentHash(*expected_hash.as_bytes())) {
                anyhow::bail!("workspace changed since preview");
            }
        }
        let catalog = self.catalog_for_targets(&records, &[record_id]).await?;
        let registry = Arc::clone(&self.registry);
        let queue = Arc::clone(&self.queue);
        tokio::task::spawn_blocking(move || {
            let registry_guard = registry.lock_recover();
            if registry_guard.records().cloned().collect::<Vec<_>>() != records {
                anyhow::bail!("availability changed during eviction planning");
            }
            let plan = plan_for_mode(&registry_guard, &catalog, record_id, mode)?;
            let mut queue_guard = queue.blocking_lock();
            let request_id = handoff(
                &plan,
                existing_request,
                &mut queue_guard,
                &registry_guard,
                &catalog,
                unix_now_ms(),
            )?;
            Ok((request_id, plan))
        })
        .await
        .map_err(|_| anyhow::anyhow!("eviction handoff panicked"))?
    }

    /// Observe exact backend state once and atomically project completed work
    /// into durable local residency.
    ///
    /// # Errors
    ///
    /// Returns an error when the request or backend observation cannot be
    /// loaded/persisted. A newer record version is deliberately not changed.
    pub async fn observe_once(
        &self,
        record_id: FileRecordId,
        id: EvictionRequestId,
    ) -> anyhow::Result<bool> {
        let mut queue = self.queue.lock().await;
        let request = queue
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown eviction request"))?;
        let observation = self
            .hydrator
            .store
            .observe_eviction_request(&mut queue, id, unix_now_ms())
            .await?;
        drop(queue);
        let EvictionRequestState::Completed { observed_at_ms } = observation.state else {
            return Ok(false);
        };
        let mut registry = self.registry.lock_recover();
        if request.record_id != record_id
            || registry.get(record_id).is_none_or(|record| {
                record.current_version != Some(request.version_id)
                    // A later verified local hydration of the same version
                    // must not be erased by replaying an older completion.
                    || record.freshness.local_verified_at_ms.is_some_and(|at| at > observed_at_ms)
            })
        {
            return Ok(true);
        }
        let mut record = registry
            .get(record_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("evicted record disappeared"))?;
        record.residency = Residency::NotResident;
        record.freshness.local_verified_at_ms = None;
        registry.put(record)?;
        drop(registry);
        Ok(true)
    }

    pub fn spawn_observer(&self, record_id: FileRecordId, id: EvictionRequestId) {
        let coordinator = self.clone();
        self.hydrator.runtime_handle.spawn(async move {
            for _ in 0..=75 {
                match coordinator.observe_once(record_id, id).await {
                    Ok(true) | Err(_) => break,
                    Ok(false) => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
                }
            }
        });
    }

    async fn catalog_for_targets(
        &self,
        records: &[RecordAvailability],
        targets: &[FileRecordId],
    ) -> anyhow::Result<EvictionCatalog> {
        let targets = targets.iter().copied().collect::<HashSet<_>>();
        let history = self
            .history
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("durable version history is unavailable"))?;
        let mut keys = Vec::new();
        for record in records {
            if record.kind != RecordKind::File {
                continue;
            }
            let protects_current = targets.contains(&record.record_id)
                || record.residency != Residency::NotResident
                || pinned_in_snapshot(records, record.record_id);
            if protects_current && let Some(version) = record.current_version {
                keys.push((record.record_id, version));
            }
            keys.extend(
                record
                    .retained_versions
                    .iter()
                    .copied()
                    .map(|version| (record.record_id, version)),
            );
        }
        keys.sort_unstable();
        keys.dedup();
        let mut payload_keys = Vec::new();
        let mut empty = Vec::new();
        for (record_id, version_id) in keys {
            let version = history
                .snapshot(record_id)
                .and_then(|snapshot| {
                    snapshot
                        .versions
                        .into_iter()
                        .find(|version| version.id == version_id)
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("version history changed during eviction planning")
                })?;
            match version.content_hash {
                Some(hash) => payload_keys.push((record_id, version_id, Hash::from(hash.0))),
                None => empty.push((record_id, version_id)),
            }
        }
        let store = Arc::clone(&self.hydrator.store);
        let payloads = stream::iter(payload_keys)
            .map(|(record_id, version_id, hash)| {
                let store = Arc::clone(&store);
                async move {
                    store
                        .inspect_version_payload(record_id, version_id, hash)
                        .await
                }
            })
            .buffer_unordered(MAX_CONCURRENT_CATALOG_INSPECTIONS)
            .try_collect::<Vec<_>>()
            .await?;
        Ok(EvictionCatalog::with_empty_versions(payloads, empty)?)
    }
}

fn plan_for_mode(
    registry: &AvailabilityRegistry,
    catalog: &EvictionCatalog,
    record_id: FileRecordId,
    mode: EvictionMode,
) -> Result<EvictionPlan, PlanError> {
    match mode {
        EvictionMode::Safe => plan_safe_eviction(
            registry,
            catalog,
            record_id,
            unix_now_ms(),
            EvictionRequirements::default(),
        ),
        EvictionMode::DestructiveOverride => plan_destructive_eviction(
            registry,
            catalog,
            record_id,
            unix_now_ms(),
            EvictionRequirements::default(),
        ),
    }
}

/// One blob in a version payload, with locally verified byte accounting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlobExtent {
    pub hash: Hash,
    /// Distinguishes an absent blob from a partial blob whose stored byte
    /// count the backend cannot currently determine.
    pub locally_present: bool,
    /// Bytes verified locally. For a present partial blob, `None` means the
    /// byte count is unknown; consult `locally_present` to distinguish absence.
    pub resident_bytes: Option<u64>,
}

/// The exact immutable payload graph for one file version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionPayload {
    pub record_id: FileRecordId,
    pub version_id: FileVersionId,
    pub manifest: BlobExtent,
    pub chunks: Vec<BlobExtent>,
}

impl VersionPayload {
    fn blobs(&self) -> impl Iterator<Item = BlobExtent> + '_ {
        std::iter::once(self.manifest).chain(self.chunks.iter().copied())
    }
}

/// Complete reference information supplied by the version/history index.
///
/// Planning fails closed when any resident, pinned, or retained version is
/// absent. That is essential: an incomplete catalog cannot prove that a chunk
/// is unshared.
#[derive(Clone, Debug, Default)]
pub struct EvictionCatalog {
    payloads: HashMap<(FileRecordId, FileVersionId), VersionPayload>,
    empty_versions: HashSet<(FileRecordId, FileVersionId)>,
}

impl EvictionCatalog {
    /// Build a catalog containing only non-empty immutable payloads.
    pub fn new(payloads: impl IntoIterator<Item = VersionPayload>) -> Result<Self, PlanError> {
        Self::with_empty_versions(payloads, [])
    }

    /// Build a complete catalog including immutable versions that have no
    /// manifest because their exact content is empty. Empty versions protect
    /// no blobs, but naming them prevents fail-closed planning from confusing
    /// an intentional empty payload with a missing history entry.
    pub fn with_empty_versions(
        payloads: impl IntoIterator<Item = VersionPayload>,
        empty_versions: impl IntoIterator<Item = (FileRecordId, FileVersionId)>,
    ) -> Result<Self, PlanError> {
        let mut result = Self::default();
        for payload in payloads {
            let key = (payload.record_id, payload.version_id);
            if result.payloads.insert(key, payload).is_some() {
                return Err(PlanError::DuplicateVersion {
                    record_id: key.0,
                    version_id: key.1,
                });
            }
        }
        for key in empty_versions {
            if result.payloads.contains_key(&key) || !result.empty_versions.insert(key) {
                return Err(PlanError::DuplicateVersion {
                    record_id: key.0,
                    version_id: key.1,
                });
            }
        }
        Ok(result)
    }

    fn get(&self, record_id: FileRecordId, version_id: FileVersionId) -> Option<&VersionPayload> {
        self.payloads.get(&(record_id, version_id))
    }

    fn is_empty(&self, record_id: FileRecordId, version_id: FileVersionId) -> bool {
        self.empty_versions.contains(&(record_id, version_id))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvictionMode {
    Safe,
    /// The caller explicitly accepts that the target's only known copy may be
    /// lost. Shared blobs and retained history remain protected.
    DestructiveOverride,
}

pub const EVICTION_QUEUE_SCHEMA_VERSION: u16 = 1;
pub const MAX_EVICTION_REQUESTS: usize = 4_096;

/// Stable idempotency key derived from the exact immutable eviction request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EvictionRequestId(pub Uuid);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum EvictionRequestState {
    /// Persisted intent, not a claim that backend deletion has started.
    AwaitingBackendGc,
    /// Every requested hash was observed `NotFound` in one complete backend
    /// observation. This is the only successful terminal state.
    Completed { observed_at_ms: u64 },
}

/// Durable immutable request plus its monotonic observation state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvictionRequest {
    pub id: EvictionRequestId,
    pub record_id: FileRecordId,
    pub version_id: FileVersionId,
    pub mode: EvictionMode,
    pub requested_hashes: Vec<Hash>,
    pub requested_at_ms: u64,
    pub state: EvictionRequestState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnqueueResult {
    pub id: EvictionRequestId,
    /// False means the identical durable request already existed.
    pub inserted: bool,
}

/// One authoritative status sample returned by the blob backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedBlobState {
    Absent,
    Present { resident_bytes: Option<u64> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvictionObservation {
    pub id: EvictionRequestId,
    pub state: EvictionRequestState,
    pub remaining_hashes: Vec<Hash>,
    pub observed_remaining_bytes: u64,
    pub unaccounted_remaining_blobs: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvictionQueueDocument {
    schema_version: u16,
    requests: Vec<EvictionRequest>,
}

/// Crash-safe ledger for explicit eviction intent and observed completion.
///
/// The ledger does not trigger deletion by itself. The content-store bridge
/// freshly reauthorizes a pending request, releases only its deterministic
/// service-owned tags, and lets iroh-blobs' configured background GC collect
/// them. This ledger records completion only after exact backend observation.
pub struct EvictionQueue {
    path: PathBuf,
    requests: BTreeMap<EvictionRequestId, EvictionRequest>,
}

#[derive(Debug, Error)]
pub enum EvictionQueueError {
    #[error("eviction queue I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("eviction queue is malformed: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("unsupported eviction queue schema version {0}")]
    UnsupportedVersion(u16),
    #[error("duplicate eviction request {0:?}")]
    DuplicateRequest(EvictionRequestId),
    #[error("eviction request {0:?} has a non-canonical id")]
    NonCanonicalRequestId(EvictionRequestId),
    #[error("eviction request {0:?} contains duplicate or unsorted hashes")]
    NonCanonicalHashes(EvictionRequestId),
    #[error("unknown eviction request {0:?}")]
    UnknownRequest(EvictionRequestId),
    #[error("backend observation does not exactly cover request {0:?}")]
    IncompleteObservation(EvictionRequestId),
    #[error("eviction request {0:?} is no longer awaiting backend GC")]
    RequestNotPending(EvictionRequestId),
    #[error("eviction queue contains too many requests")]
    TooManyRequests,
    #[error("fresh eviction plan no longer matches request {0:?}")]
    FreshPlanMismatch(EvictionRequestId),
}

#[derive(Debug, Error)]
pub enum BackendGcAuthorizationError {
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Queue(#[from] EvictionQueueError),
}

impl EvictionQueue {
    /// Open and fully validate a durable eviction queue.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, EvictionQueueError> {
        let path = path.into();
        if !path.exists() {
            return Ok(Self {
                path,
                requests: BTreeMap::new(),
            });
        }
        let document: EvictionQueueDocument = serde_json::from_slice(&fs::read(&path)?)?;
        if document.schema_version != EVICTION_QUEUE_SCHEMA_VERSION {
            return Err(EvictionQueueError::UnsupportedVersion(
                document.schema_version,
            ));
        }
        let mut requests = BTreeMap::new();
        for request in document.requests {
            if requests.len() >= MAX_EVICTION_REQUESTS {
                return Err(EvictionQueueError::TooManyRequests);
            }
            validate_request(&request)?;
            let id = request.id;
            if requests.insert(id, request).is_some() {
                return Err(EvictionQueueError::DuplicateRequest(id));
            }
        }
        Ok(Self { path, requests })
    }

    pub fn get(&self, id: EvictionRequestId) -> Option<&EvictionRequest> {
        self.requests.get(&id)
    }

    pub fn requests(&self) -> impl Iterator<Item = &EvictionRequest> {
        self.requests.values()
    }

    /// Durably enqueue only the blobs approved by an exact dry-run plan.
    /// Repeating an identical request is idempotent across process restarts.
    pub fn enqueue(
        &mut self,
        plan: &EvictionPlan,
        requested_at_ms: u64,
    ) -> Result<EnqueueResult, EvictionQueueError> {
        let requested_hashes = plan_requested_hashes(plan);
        let id = request_id(
            plan.record_id,
            plan.version_id,
            plan.mode,
            &requested_hashes,
        );
        if let Some(existing) = self.requests.get(&id) {
            if matches!(existing.state, EvictionRequestState::Completed { .. }) {
                let previous = existing.clone();
                let Some(request) = self.requests.get_mut(&id) else {
                    return Err(EvictionQueueError::UnknownRequest(id));
                };
                request.requested_at_ms = requested_at_ms;
                request.state = EvictionRequestState::AwaitingBackendGc;
                if let Err(error) = self.persist() {
                    self.requests.insert(id, previous);
                    return Err(error);
                }
                return Ok(EnqueueResult {
                    id,
                    inserted: false,
                });
            }
            return Ok(EnqueueResult {
                id,
                inserted: false,
            });
        }
        let pruned = if self.requests.len() >= MAX_EVICTION_REQUESTS {
            let completed = self
                .requests
                .values()
                .filter(|request| matches!(request.state, EvictionRequestState::Completed { .. }))
                .min_by_key(|request| (request.requested_at_ms, request.id))
                .map(|request| request.id);
            if let Some(completed) = completed {
                self.requests.remove(&completed)
            } else {
                return Err(EvictionQueueError::TooManyRequests);
            }
        } else {
            None
        };
        let request = EvictionRequest {
            id,
            record_id: plan.record_id,
            version_id: plan.version_id,
            mode: plan.mode,
            requested_hashes,
            requested_at_ms,
            state: EvictionRequestState::AwaitingBackendGc,
        };
        self.requests.insert(id, request);
        if let Err(error) = self.persist() {
            self.requests.remove(&id);
            if let Some(pruned) = pruned {
                self.requests.insert(pruned.id, pruned);
            }
            return Err(error);
        }
        Ok(EnqueueResult { id, inserted: true })
    }

    /// Reauthorize a queued request against a freshly computed exact plan.
    ///
    /// A backend GC bridge must call this immediately before making hashes
    /// collectible, while holding whatever higher-level lock keeps the
    /// availability registry and version catalog stable through that handoff.
    /// A record version change, policy refusal, or newly shared safety root
    /// either prevents construction of `fresh_plan` or changes its exact hash
    /// set, so stale enqueue-time authority is never enough by itself.
    ///
    /// This method does not delete, unprotect, or mark anything complete.
    /// Completion still requires one exact all-absent backend observation.
    fn revalidate_for_backend_gc(
        &self,
        id: EvictionRequestId,
        fresh_plan: &EvictionPlan,
    ) -> Result<Vec<Hash>, EvictionQueueError> {
        let request = self
            .requests
            .get(&id)
            .ok_or(EvictionQueueError::UnknownRequest(id))?;
        if !matches!(request.state, EvictionRequestState::AwaitingBackendGc) {
            return Err(EvictionQueueError::RequestNotPending(id));
        }
        if request.record_id != fresh_plan.record_id
            || request.version_id != fresh_plan.version_id
            || request.mode != fresh_plan.mode
            || request.requested_hashes != plan_requested_hashes(fresh_plan)
        {
            return Err(EvictionQueueError::FreshPlanMismatch(id));
        }
        Ok(request.requested_hashes.clone())
    }

    /// Recompute all policy and shared-root decisions before a backend handoff.
    ///
    /// The returned hashes are authorized only while the caller keeps
    /// `registry` and `catalog` stable. They are not evidence of deletion;
    /// [`ContentStore::observe_eviction_request`](crate::store::ContentStore::observe_eviction_request)
    /// remains the sole completion path.
    ///
    /// # Errors
    ///
    /// Returns an error when the request is unknown or no longer pending, the
    /// fresh safety plan is denied, or its immutable request fields changed.
    pub fn authorize_backend_gc(
        &self,
        id: EvictionRequestId,
        registry: &AvailabilityRegistry,
        catalog: &EvictionCatalog,
        now_ms: u64,
        requirements: EvictionRequirements,
    ) -> Result<Vec<Hash>, BackendGcAuthorizationError> {
        let request = self
            .requests
            .get(&id)
            .ok_or(EvictionQueueError::UnknownRequest(id))?;
        if !matches!(request.state, EvictionRequestState::AwaitingBackendGc) {
            return Err(EvictionQueueError::RequestNotPending(id).into());
        }
        let fresh_plan = match request.mode {
            EvictionMode::Safe => {
                plan_safe_eviction(registry, catalog, request.record_id, now_ms, requirements)?
            }
            EvictionMode::DestructiveOverride => plan_destructive_eviction(
                registry,
                catalog,
                request.record_id,
                now_ms,
                requirements,
            )?,
        };
        Ok(self.revalidate_for_backend_gc(id, &fresh_plan)?)
    }

    /// Record one all-or-nothing backend observation. Partial samples never
    /// change durable state, and completed requests remain completed.
    pub(crate) fn record_observation(
        &mut self,
        id: EvictionRequestId,
        mut states: Vec<(Hash, ObservedBlobState)>,
        observed_at_ms: u64,
    ) -> Result<EvictionObservation, EvictionQueueError> {
        let request = self
            .requests
            .get(&id)
            .ok_or(EvictionQueueError::UnknownRequest(id))?;
        states.sort_by_key(|(hash, _)| *hash.as_bytes());
        if states.len() != request.requested_hashes.len()
            || states
                .iter()
                .zip(&request.requested_hashes)
                .any(|((observed, _), expected)| observed != expected)
        {
            return Err(EvictionQueueError::IncompleteObservation(id));
        }

        let mut remaining_hashes = Vec::new();
        let mut observed_remaining_bytes = 0u64;
        let mut unaccounted_remaining_blobs = 0usize;
        for (hash, state) in states {
            if let ObservedBlobState::Present { resident_bytes } = state {
                remaining_hashes.push(hash);
                match resident_bytes {
                    Some(bytes) => {
                        observed_remaining_bytes = observed_remaining_bytes.saturating_add(bytes);
                    }
                    None => {
                        unaccounted_remaining_blobs = unaccounted_remaining_blobs.saturating_add(1);
                    }
                }
            }
        }

        let previous = request.state;
        let state = if remaining_hashes.is_empty() {
            match previous {
                EvictionRequestState::Completed { .. } => previous,
                EvictionRequestState::AwaitingBackendGc => {
                    EvictionRequestState::Completed { observed_at_ms }
                }
            }
        } else {
            previous
        };
        if state != previous {
            let Some(request) = self.requests.get_mut(&id) else {
                return Err(EvictionQueueError::UnknownRequest(id));
            };
            request.state = state;
            if let Err(error) = self.persist() {
                let Some(request) = self.requests.get_mut(&id) else {
                    return Err(EvictionQueueError::UnknownRequest(id));
                };
                request.state = previous;
                return Err(error);
            }
        }
        Ok(EvictionObservation {
            id,
            state,
            remaining_hashes,
            observed_remaining_bytes,
            unaccounted_remaining_blobs,
        })
    }

    fn persist(&self) -> Result<(), EvictionQueueError> {
        let document = EvictionQueueDocument {
            schema_version: EVICTION_QUEUE_SCHEMA_VERSION,
            requests: self.requests.values().cloned().collect(),
        };
        let bytes = serde_json::to_vec_pretty(&document)?;
        tilo_files::native::atomic::write_creating_parent(&self.path, &bytes)?;
        Ok(())
    }
}

fn plan_requested_hashes(plan: &EvictionPlan) -> Vec<Hash> {
    let mut hashes: Vec<_> = plan
        .reclaimable
        .iter()
        .filter(|blob| blob.locally_present)
        .map(|blob| blob.hash)
        .collect();
    hashes.sort_by_key(|hash| *hash.as_bytes());
    hashes.dedup();
    hashes
}

fn request_id(
    record_id: FileRecordId,
    version_id: FileVersionId,
    mode: EvictionMode,
    hashes: &[Hash],
) -> EvictionRequestId {
    let mut digest_builder = blake3::Hasher::new();
    digest_builder.update(b"tilo.collaborative-fs.eviction-request.v1\0");
    digest_builder.update(record_id.0.as_bytes());
    digest_builder.update(version_id.0.as_bytes());
    digest_builder.update(&[match mode {
        EvictionMode::Safe => 0,
        EvictionMode::DestructiveOverride => 1,
    }]);
    for hash in hashes {
        digest_builder.update(hash.as_bytes());
    }
    let digest = digest_builder.finalize();
    let mut bytes = [0u8; 16];
    let Some(prefix) = digest.as_bytes().get(..16) else {
        return EvictionRequestId(Uuid::nil());
    };
    bytes.copy_from_slice(prefix);
    EvictionRequestId(Uuid::from_bytes(bytes))
}

fn validate_request(request: &EvictionRequest) -> Result<(), EvictionQueueError> {
    if request
        .requested_hashes
        .windows(2)
        .any(|window| matches!(window, [left, right] if left.as_bytes() >= right.as_bytes()))
    {
        return Err(EvictionQueueError::NonCanonicalHashes(request.id));
    }
    if request.id
        != request_id(
            request.record_id,
            request.version_id,
            request.mode,
            &request.requested_hashes,
        )
    {
        return Err(EvictionQueueError::NonCanonicalRequestId(request.id));
    }
    Ok(())
}

/// An immutable dry-run result. No storage operation occurs while producing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvictionPlan {
    pub record_id: FileRecordId,
    pub version_id: FileVersionId,
    pub mode: EvictionMode,
    /// Locally present target blobs that no protected version references.
    pub reclaimable: Vec<BlobExtent>,
    /// Target blobs kept because another resident/pinned/retained version uses them.
    pub shared_and_protected: Vec<BlobExtent>,
    /// Target blobs that are already absent locally.
    pub already_absent: Vec<Hash>,
    pub reclaimable_bytes: u64,
    /// Number of present reclaimable blobs omitted from `reclaimable_bytes`
    /// because the backend reported an unknown partial size.
    pub unaccounted_reclaimable_blobs: usize,
    pub protected_shared_bytes: u64,
    pub unaccounted_protected_shared_blobs: usize,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("availability policy denied eviction: {0:?}")]
    Denied(EvictionBlocker),
    #[error("target record has no exact current version")]
    TargetVersionUnknown,
    #[error("payload catalog is missing target version {version_id:?} for {record_id:?}")]
    TargetPayloadMissing {
        record_id: FileRecordId,
        version_id: FileVersionId,
    },
    #[error("protected record {record_id:?} has no exact current version")]
    ProtectedCurrentVersionUnknown { record_id: FileRecordId },
    #[error("payload catalog is missing protected version {version_id:?} for {record_id:?}")]
    ProtectedPayloadMissing {
        record_id: FileRecordId,
        version_id: FileVersionId,
    },
    #[error("payload catalog contains duplicate version {version_id:?} for {record_id:?}")]
    DuplicateVersion {
        record_id: FileRecordId,
        version_id: FileVersionId,
    },
    #[error("blob {hash} has conflicting resident byte counts")]
    ConflictingBlobAccounting { hash: Hash },
    #[error("absent blob {hash} claims resident bytes")]
    InvalidBlobAccounting { hash: Hash },
    #[error("availability ancestry is invalid")]
    InvalidAncestry,
    #[error("availability decision did not match the requested eviction mode")]
    DecisionModeMismatch,
}

/// Plan a non-destructive eviction after all availability and reference checks.
pub fn plan_safe_eviction(
    registry: &AvailabilityRegistry,
    catalog: &EvictionCatalog,
    record_id: FileRecordId,
    now_ms: u64,
    requirements: EvictionRequirements,
) -> Result<EvictionPlan, PlanError> {
    plan(
        registry,
        catalog,
        record_id,
        now_ms,
        requirements,
        EvictionMode::Safe,
    )
}

/// Plan an explicitly destructive removal. This bypasses replica/pin checks
/// only for the target current version; it never bypasses shared/history
/// reference protection or exact-version validation.
pub fn plan_destructive_eviction(
    registry: &AvailabilityRegistry,
    catalog: &EvictionCatalog,
    record_id: FileRecordId,
    now_ms: u64,
    requirements: EvictionRequirements,
) -> Result<EvictionPlan, PlanError> {
    plan(
        registry,
        catalog,
        record_id,
        now_ms,
        requirements,
        EvictionMode::DestructiveOverride,
    )
}

fn plan(
    registry: &AvailabilityRegistry,
    catalog: &EvictionCatalog,
    record_id: FileRecordId,
    now_ms: u64,
    requirements: EvictionRequirements,
    mode: EvictionMode,
) -> Result<EvictionPlan, PlanError> {
    let destructive = mode == EvictionMode::DestructiveOverride;
    match registry.eviction_decision(record_id, now_ms, requirements, destructive) {
        EvictionDecision::Denied(blocker) => return Err(PlanError::Denied(blocker)),
        EvictionDecision::Allowed if destructive => return Err(PlanError::DecisionModeMismatch),
        EvictionDecision::AllowedDestructiveOverride if !destructive => {
            return Err(PlanError::DecisionModeMismatch);
        }
        EvictionDecision::Allowed | EvictionDecision::AllowedDestructiveOverride => {}
    }

    let target_record = registry
        .get(record_id)
        .ok_or(PlanError::Denied(EvictionBlocker::UnknownRecord))?;
    let target_version = target_record
        .current_version
        .ok_or(PlanError::TargetVersionUnknown)?;
    let target = catalog.get(record_id, target_version);
    if target.is_none() && !catalog.is_empty(record_id, target_version) {
        return Err(PlanError::TargetPayloadMissing {
            record_id,
            version_id: target_version,
        });
    }

    let mut protected = HashSet::new();
    for record in registry.records() {
        if record.kind != RecordKind::File {
            continue;
        }

        // A current payload is protected when it belongs to another locally
        // resident record, or when pin policy promises it should remain local.
        let protects_current = record.record_id != record_id
            && (record.residency != Residency::NotResident
                || registry
                    .effective_policy(record.record_id)
                    .map_err(|_| PlanError::InvalidAncestry)?
                    == Some(AvailabilityPolicy::Pinned));
        if protects_current {
            let version_id =
                record
                    .current_version
                    .ok_or(PlanError::ProtectedCurrentVersionUnknown {
                        record_id: record.record_id,
                    })?;
            add_protected_payload(&mut protected, catalog, record.record_id, version_id)?;
        }

        // Retained history remains protected even for the target during an
        // explicit destructive current-version eviction.
        for &version_id in &record.retained_versions {
            add_protected_payload(&mut protected, catalog, record.record_id, version_id)?;
        }
    }

    let target_blobs = match target {
        Some(target) => canonical_blobs(target.blobs())?,
        None => Vec::new(),
    };
    let mut reclaimable = Vec::new();
    let mut shared_and_protected = Vec::new();
    let mut already_absent = Vec::new();
    for extent in target_blobs {
        match (extent.locally_present, protected.contains(&extent.hash)) {
            (false, _) => already_absent.push(extent.hash),
            (true, true) => shared_and_protected.push(extent),
            (true, false) => reclaimable.push(extent),
        }
    }
    sort_extents(&mut reclaimable);
    sort_extents(&mut shared_and_protected);
    already_absent.sort_by_key(|hash| *hash.as_bytes());

    Ok(EvictionPlan {
        record_id,
        version_id: target_version,
        mode,
        reclaimable_bytes: sum_bytes(&reclaimable),
        unaccounted_reclaimable_blobs: reclaimable
            .iter()
            .filter(|extent| extent.resident_bytes.is_none())
            .count(),
        protected_shared_bytes: sum_bytes(&shared_and_protected),
        unaccounted_protected_shared_blobs: shared_and_protected
            .iter()
            .filter(|extent| extent.resident_bytes.is_none())
            .count(),
        reclaimable,
        shared_and_protected,
        already_absent,
    })
}

/// Near-linear multi-target equivalent of repeated `plan_safe_eviction`.
/// Protected roots are catalogued exactly once; each target then only walks
/// its own immutable payload and subtracts its own current-root contribution.
fn batch_plan_safe(
    registry: &AvailabilityRegistry,
    catalog: &EvictionCatalog,
    targets: &[FileRecordId],
    now_ms: u64,
) -> HashMap<FileRecordId, Option<EvictionPlan>> {
    let requirements = EvictionRequirements::default();
    let mut protected_counts = HashMap::<Hash, usize>::new();
    let mut own_current = HashMap::<FileRecordId, HashSet<Hash>>::new();
    let mut roots_valid = true;
    for record in registry.records() {
        if record.kind != RecordKind::File {
            continue;
        }
        let current_is_protected = if let Ok(policy) = registry.effective_policy(record.record_id) {
            record.residency != Residency::NotResident || policy == Some(AvailabilityPolicy::Pinned)
        } else {
            roots_valid = false;
            true
        };
        if current_is_protected {
            let Some(version) = record.current_version else {
                roots_valid = false;
                continue;
            };
            match payload_hashes(catalog, record.record_id, version) {
                Ok(hashes) => {
                    for hash in &hashes {
                        let count = protected_counts.entry(*hash).or_default();
                        *count = count.saturating_add(1);
                    }
                    own_current.insert(record.record_id, hashes);
                }
                Err(_) => roots_valid = false,
            }
        }
        for &version in &record.retained_versions {
            match payload_hashes(catalog, record.record_id, version) {
                Ok(hashes) => {
                    for hash in hashes {
                        let count = protected_counts.entry(hash).or_default();
                        *count = count.saturating_add(1);
                    }
                }
                Err(_) => roots_valid = false,
            }
        }
    }
    targets
        .iter()
        .copied()
        .map(|record_id| {
            let plan = if roots_valid {
                batch_target_plan(
                    registry,
                    catalog,
                    record_id,
                    &protected_counts,
                    own_current.get(&record_id),
                    requirements,
                    now_ms,
                )
                .ok()
            } else {
                None
            };
            (record_id, plan)
        })
        .collect()
}

fn payload_hashes(
    catalog: &EvictionCatalog,
    record_id: FileRecordId,
    version_id: FileVersionId,
) -> Result<HashSet<Hash>, PlanError> {
    if catalog.is_empty(record_id, version_id) {
        return Ok(HashSet::new());
    }
    Ok(catalog
        .get(record_id, version_id)
        .ok_or(PlanError::ProtectedPayloadMissing {
            record_id,
            version_id,
        })?
        .blobs()
        .map(|blob| blob.hash)
        .collect())
}

fn batch_target_plan(
    registry: &AvailabilityRegistry,
    catalog: &EvictionCatalog,
    record_id: FileRecordId,
    protected_counts: &HashMap<Hash, usize>,
    own_current: Option<&HashSet<Hash>>,
    requirements: EvictionRequirements,
    now_ms: u64,
) -> Result<EvictionPlan, PlanError> {
    if !matches!(
        registry.eviction_decision(record_id, now_ms, requirements, false),
        EvictionDecision::Allowed
    ) {
        return Err(PlanError::Denied(EvictionBlocker::UnknownRecord));
    }
    let record = registry
        .get(record_id)
        .ok_or(PlanError::Denied(EvictionBlocker::UnknownRecord))?;
    let version_id = record
        .current_version
        .ok_or(PlanError::TargetVersionUnknown)?;
    let target = if catalog.is_empty(record_id, version_id) {
        Vec::new()
    } else {
        canonical_blobs(
            catalog
                .get(record_id, version_id)
                .ok_or(PlanError::TargetPayloadMissing {
                    record_id,
                    version_id,
                })?
                .blobs(),
        )?
    };
    let mut reclaimable = Vec::new();
    let mut shared_and_protected = Vec::new();
    let mut already_absent = Vec::new();
    for extent in target {
        if !extent.locally_present {
            already_absent.push(extent.hash);
            continue;
        }
        let own = usize::from(own_current.is_some_and(|hashes| hashes.contains(&extent.hash)));
        if protected_counts.get(&extent.hash).copied().unwrap_or(0) > own {
            shared_and_protected.push(extent);
        } else {
            reclaimable.push(extent);
        }
    }
    sort_extents(&mut reclaimable);
    sort_extents(&mut shared_and_protected);
    already_absent.sort_by_key(|hash| *hash.as_bytes());
    Ok(EvictionPlan {
        record_id,
        version_id,
        mode: EvictionMode::Safe,
        reclaimable_bytes: sum_bytes(&reclaimable),
        unaccounted_reclaimable_blobs: reclaimable
            .iter()
            .filter(|extent| extent.resident_bytes.is_none())
            .count(),
        protected_shared_bytes: sum_bytes(&shared_and_protected),
        unaccounted_protected_shared_blobs: shared_and_protected
            .iter()
            .filter(|extent| extent.resident_bytes.is_none())
            .count(),
        reclaimable,
        shared_and_protected,
        already_absent,
    })
}

fn add_protected_payload(
    protected: &mut HashSet<Hash>,
    catalog: &EvictionCatalog,
    record_id: FileRecordId,
    version_id: FileVersionId,
) -> Result<(), PlanError> {
    if catalog.is_empty(record_id, version_id) {
        return Ok(());
    }
    let payload = catalog
        .get(record_id, version_id)
        .ok_or(PlanError::ProtectedPayloadMissing {
            record_id,
            version_id,
        })?;
    protected.extend(payload.blobs().map(|blob| blob.hash));
    Ok(())
}

fn canonical_blobs(
    blobs: impl IntoIterator<Item = BlobExtent>,
) -> Result<Vec<BlobExtent>, PlanError> {
    let mut by_hash = HashMap::new();
    for blob in blobs {
        if !blob.locally_present && blob.resident_bytes.is_some() {
            return Err(PlanError::InvalidBlobAccounting { hash: blob.hash });
        }
        let accounting = (blob.locally_present, blob.resident_bytes);
        match by_hash.insert(blob.hash, accounting) {
            Some(previous) if previous != accounting => {
                return Err(PlanError::ConflictingBlobAccounting { hash: blob.hash });
            }
            _ => {}
        }
    }
    Ok(by_hash
        .into_iter()
        .map(|(hash, (locally_present, resident_bytes))| BlobExtent {
            hash,
            locally_present,
            resident_bytes,
        })
        .collect())
}

fn sort_extents(extents: &mut [BlobExtent]) {
    extents.sort_by_key(|extent| *extent.hash.as_bytes());
}

fn sum_bytes(extents: &[BlobExtent]) -> u64 {
    extents
        .iter()
        .filter_map(|extent| extent.resident_bytes)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::{RecordAvailability, ReplicaObservation};
    use collab_types::Ed25519PublicKey;
    use ed25519_dalek::SigningKey;
    use fs_protocol::{ContentHash, FileActor};
    use getrandom::{SysRng, rand_core::UnwrapErr};
    use tempfile::TempDir;

    fn hash(byte: u8) -> Hash {
        Hash::from([byte; 32])
    }

    fn payload(
        record_id: FileRecordId,
        version_id: FileVersionId,
        manifest: (u8, Option<u64>),
        chunks: &[(u8, Option<u64>)],
    ) -> VersionPayload {
        VersionPayload {
            record_id,
            version_id,
            manifest: BlobExtent {
                hash: hash(manifest.0),
                locally_present: manifest.1.is_some(),
                resident_bytes: manifest.1,
            },
            chunks: chunks
                .iter()
                .map(|&(byte, resident_bytes)| BlobExtent {
                    hash: hash(byte),
                    locally_present: resident_bytes.is_some(),
                    resident_bytes,
                })
                .collect(),
        }
    }

    fn resident_file(id: FileRecordId, version: FileVersionId, now: u64) -> RecordAvailability {
        let mut record = RecordAvailability::new(id, RecordKind::File);
        record.current_version = Some(version);
        record.residency = Residency::Resident { bytes: 30 };
        let device = SigningKey::generate(&mut UnwrapErr(SysRng));
        let actor = FileActor {
            person: Ed25519PublicKey(device.verifying_key().to_bytes()),
            device: Ed25519PublicKey(device.verifying_key().to_bytes()),
        };
        let request = crate::replica_ack::ReplicaAckRequestV1::new(
            uuid::Uuid::new_v4(),
            fs_protocol::ContentRevisionV1 {
                schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                record_id: id,
                version_id: version,
                content_hash: Some(ContentHash([6; 32])),
                byte_len: 30,
                metadata_hash: ContentHash([7; 32]),
            },
        )
        .unwrap();
        let receipt =
            crate::replica_ack::ReplicaAckV1::signed(&request, actor, now, &device).unwrap();
        record.replica_observations = vec![ReplicaObservation {
            protocol_version: receipt.protocol_version,
            workspace_id: receipt.workspace_id,
            record_id: receipt.revision.record_id,
            person: receipt.actor.person,
            device: receipt.actor.device,
            version_id: receipt.revision.version_id,
            content_hash: receipt.revision.content_hash,
            byte_len: receipt.revision.byte_len,
            metadata_hash: receipt.revision.metadata_hash,
            nonce: receipt.nonce,
            issued_at_ms: receipt.issued_at_ms,
            expires_at_ms: receipt.expires_at_ms,
            observed_at_ms: now,
            signature: receipt.signature,
        }];
        record.replicas = crate::availability::ReplicaEvidence {
            reachable: 1,
            durable: 1,
            acknowledged_version: Some(version),
        };
        record.freshness.replicas_observed_at_ms = Some(now);
        record
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn eviction_plans_protect_shared_and_retained_blobs_and_fail_closed() {
        {
            let temp = TempDir::new().unwrap();
            let mut registry =
                AvailabilityRegistry::open(temp.path().join("availability.json")).unwrap();
            let now = unix_now_ms();
            let record = FileRecordId::new();
            let version = FileVersionId::new();
            registry.put(resident_file(record, version, now)).unwrap();
            let catalog = EvictionCatalog::with_empty_versions([], [(record, version)]).unwrap();

            let plan = plan_safe_eviction(
                &registry,
                &catalog,
                record,
                now,
                EvictionRequirements::default(),
            )
            .unwrap();
            assert!(plan.reclaimable.is_empty());
            assert!(plan.shared_and_protected.is_empty());
            assert!(plan.already_absent.is_empty());
            assert_eq!(plan.reclaimable_bytes, 0);
        }
        {
            let temp = TempDir::new().unwrap();
            let mut registry =
                AvailabilityRegistry::open(temp.path().join("availability.json")).unwrap();
            let now = 1_000;
            let target = FileRecordId::new();
            let target_version = FileVersionId::new();
            let other = FileRecordId::new();
            let other_version = FileVersionId::new();
            registry
                .put(resident_file(target, target_version, now))
                .unwrap();
            registry
                .put(resident_file(other, other_version, now))
                .unwrap();
            let catalog = EvictionCatalog::with_empty_versions(
                [payload(target, target_version, (1, Some(4)), &[])],
                [(other, other_version)],
            )
            .unwrap();

            let plan = plan_safe_eviction(
                &registry,
                &catalog,
                target,
                now,
                EvictionRequirements::default(),
            )
            .unwrap();
            assert_eq!(plan.reclaimable.len(), 1);
            assert!(plan.shared_and_protected.is_empty());
        }
        {
            let temp = TempDir::new().unwrap();
            let mut registry =
                AvailabilityRegistry::open(temp.path().join("availability.json")).unwrap();
            let now = 1_000;
            let target_id = FileRecordId::new();
            let target_version = FileVersionId::new();
            let other_id = FileRecordId::new();
            let other_version = FileVersionId::new();
            registry
                .put(resident_file(target_id, target_version, now))
                .unwrap();
            registry
                .put(resident_file(other_id, other_version, now))
                .unwrap();
            let catalog = EvictionCatalog::new([
                payload(
                    target_id,
                    target_version,
                    (1, Some(4)),
                    &[(2, Some(10)), (3, Some(20))],
                ),
                payload(
                    other_id,
                    other_version,
                    (1, Some(4)),
                    &[(2, Some(10)), (4, Some(7))],
                ),
            ])
            .unwrap();

            let plan = plan_safe_eviction(
                &registry,
                &catalog,
                target_id,
                now,
                EvictionRequirements::default(),
            )
            .unwrap();
            assert_eq!(
                plan.reclaimable,
                vec![BlobExtent {
                    hash: hash(3),
                    locally_present: true,
                    resident_bytes: Some(20)
                }]
            );
            assert_eq!(plan.reclaimable_bytes, 20);
            assert_eq!(plan.protected_shared_bytes, 14);
            assert_eq!(plan.shared_and_protected.len(), 2);
        }
        {
            let temp = TempDir::new().unwrap();
            let mut registry =
                AvailabilityRegistry::open(temp.path().join("availability.json")).unwrap();
            let now = 1_000;
            let target_id = FileRecordId::new();
            let target_version = FileVersionId::new();
            let other_id = FileRecordId::new();
            let other_version = FileVersionId::new();
            registry
                .put(resident_file(target_id, target_version, now))
                .unwrap();
            registry
                .put(resident_file(other_id, other_version, now))
                .unwrap();
            let catalog = EvictionCatalog::new([payload(
                target_id,
                target_version,
                (1, Some(4)),
                &[(2, Some(10))],
            )])
            .unwrap();

            assert_eq!(
                plan_safe_eviction(
                    &registry,
                    &catalog,
                    target_id,
                    now,
                    EvictionRequirements::default(),
                ),
                Err(PlanError::ProtectedPayloadMissing {
                    record_id: other_id,
                    version_id: other_version,
                })
            );
        }
        {
            let child_id = FileRecordId::new();
            let missing = FileRecordId::new();
            let mut child = RecordAvailability::new(child_id, RecordKind::File);
            child.parent = Some(missing);
            assert!(pinned_in_snapshot(&[child.clone()], child_id));

            child.parent = Some(child_id);
            assert!(pinned_in_snapshot(&[child], child_id));
        }
        {
            let temp = TempDir::new().unwrap();
            let mut registry =
                AvailabilityRegistry::open(temp.path().join("availability.json")).unwrap();
            let now = unix_now_ms();
            let first = FileRecordId::new();
            let first_version = FileVersionId::new();
            let second = FileRecordId::new();
            let second_version = FileVersionId::new();
            registry
                .put(resident_file(first, first_version, now))
                .unwrap();
            registry
                .put(resident_file(second, second_version, now))
                .unwrap();
            let catalog = EvictionCatalog::new([
                payload(
                    first,
                    first_version,
                    (1, Some(4)),
                    &[(7, Some(9)), (8, None)],
                ),
                payload(
                    second,
                    second_version,
                    (2, Some(4)),
                    &[(7, Some(9)), (9, None)],
                ),
            ])
            .unwrap();
            let batch = batch_plan_safe(&registry, &catalog, &[first, second], now);
            for record in [first, second] {
                assert_eq!(
                    batch.get(&record).and_then(Option::as_ref),
                    Some(
                        &plan_safe_eviction(
                            &registry,
                            &catalog,
                            record,
                            now,
                            EvictionRequirements::default()
                        )
                        .unwrap()
                    )
                );
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn dry_runs_and_the_destructive_api_account_every_refusal_and_size() {
        {
            let temp = TempDir::new().unwrap();
            let mut registry =
                AvailabilityRegistry::open(temp.path().join("availability.json")).unwrap();
            let id = FileRecordId::new();
            let version = FileVersionId::new();
            let mut record = resident_file(id, version, 1_000);
            record.policy = AvailabilityPolicy::Pinned;
            registry.put(record).unwrap();
            let catalog = EvictionCatalog::new([payload(id, version, (1, Some(4)), &[])]).unwrap();

            assert_eq!(
                plan_safe_eviction(
                    &registry,
                    &catalog,
                    id,
                    1_000,
                    EvictionRequirements::default(),
                ),
                Err(PlanError::Denied(EvictionBlocker::Pinned))
            );
        }
        {
            let temp = TempDir::new().unwrap();
            let mut registry =
                AvailabilityRegistry::open(temp.path().join("availability.json")).unwrap();
            let id = FileRecordId::new();
            let current = FileVersionId::new();
            let retained = FileVersionId::new();
            let mut record = resident_file(id, current, 1_000);
            record.policy = AvailabilityPolicy::Pinned;
            record.retained_versions.push(retained);
            registry.put(record).unwrap();
            let catalog = EvictionCatalog::new([
                payload(id, current, (1, Some(4)), &[(2, Some(10)), (3, Some(20))]),
                payload(id, retained, (4, Some(4)), &[(2, Some(10))]),
            ])
            .unwrap();

            let plan = plan_destructive_eviction(
                &registry,
                &catalog,
                id,
                1_000,
                EvictionRequirements::default(),
            )
            .unwrap();
            assert_eq!(plan.mode, EvictionMode::DestructiveOverride);
            assert_eq!(
                plan.shared_and_protected,
                vec![BlobExtent {
                    hash: hash(2),
                    locally_present: true,
                    resident_bytes: Some(10)
                }]
            );
            assert_eq!(plan.reclaimable_bytes, 24);
        }
        {
            let temp = TempDir::new().unwrap();
            let mut registry =
                AvailabilityRegistry::open(temp.path().join("availability.json")).unwrap();
            let id = FileRecordId::new();
            let version = FileVersionId::new();
            registry.put(resident_file(id, version, 1_000)).unwrap();
            let catalog = EvictionCatalog::new([payload(
                id,
                version,
                (1, Some(4)),
                &[(2, None), (3, Some(20)), (3, Some(20))],
            )])
            .unwrap();

            let plan = plan_safe_eviction(
                &registry,
                &catalog,
                id,
                1_000,
                EvictionRequirements::default(),
            )
            .unwrap();
            assert_eq!(plan.reclaimable_bytes, 24);
            assert_eq!(plan.reclaimable.len(), 2);
            assert_eq!(plan.already_absent, vec![hash(2)]);
        }
        {
            let temp = TempDir::new().unwrap();
            let mut registry =
                AvailabilityRegistry::open(temp.path().join("availability.json")).unwrap();
            let id = FileRecordId::new();
            let version = FileVersionId::new();
            registry.put(resident_file(id, version, 1_000)).unwrap();
            let catalog = EvictionCatalog::new([VersionPayload {
                record_id: id,
                version_id: version,
                manifest: BlobExtent {
                    hash: hash(1),
                    locally_present: true,
                    resident_bytes: Some(4),
                },
                chunks: vec![BlobExtent {
                    hash: hash(2),
                    locally_present: true,
                    resident_bytes: None,
                }],
            }])
            .unwrap();

            let plan = plan_safe_eviction(
                &registry,
                &catalog,
                id,
                1_000,
                EvictionRequirements::default(),
            )
            .unwrap();
            assert!(plan.already_absent.is_empty());
            assert_eq!(plan.reclaimable.len(), 2);
            assert_eq!(plan.reclaimable_bytes, 4);
            assert_eq!(plan.unaccounted_reclaimable_blobs, 1);
        }
        {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("evictions.json");
            let plan = queued_plan(FileRecordId::new(), FileVersionId::new());
            let mut queue = EvictionQueue::open(&path).unwrap();
            let id = queue.enqueue(&plan, 100).unwrap().id;

            let observation = queue
                .record_observation(
                    id,
                    vec![
                        (
                            hash(7),
                            ObservedBlobState::Present {
                                resident_bytes: None,
                            },
                        ),
                        (
                            hash(8),
                            ObservedBlobState::Present {
                                resident_bytes: Some(12),
                            },
                        ),
                    ],
                    200,
                )
                .unwrap();
            assert_eq!(observation.state, EvictionRequestState::AwaitingBackendGc);
            assert_eq!(observation.remaining_hashes, vec![hash(7), hash(8)]);
            assert_eq!(observation.observed_remaining_bytes, 12);
            assert_eq!(observation.unaccounted_remaining_blobs, 1);
            assert_eq!(
                EvictionQueue::open(&path).unwrap().get(id).unwrap().state,
                EvictionRequestState::AwaitingBackendGc
            );
        }
    }

    fn queued_plan(record_id: FileRecordId, version_id: FileVersionId) -> EvictionPlan {
        EvictionPlan {
            record_id,
            version_id,
            mode: EvictionMode::Safe,
            reclaimable: vec![
                BlobExtent {
                    hash: hash(8),
                    locally_present: true,
                    resident_bytes: Some(80),
                },
                BlobExtent {
                    hash: hash(7),
                    locally_present: true,
                    resident_bytes: None,
                },
            ],
            shared_and_protected: Vec::new(),
            already_absent: Vec::new(),
            reclaimable_bytes: 80,
            unaccounted_reclaimable_blobs: 1,
            protected_shared_bytes: 0,
            unaccounted_protected_shared_blobs: 0,
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn eviction_queue_is_durable_bounded_and_hands_off_only_fresh_exact_plans() {
        {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("cache/evictions.json");
            let record_id = FileRecordId::new();
            let version_id = FileVersionId::new();
            let plan = queued_plan(record_id, version_id);

            let first = {
                let mut queue = EvictionQueue::open(&path).unwrap();
                queue.enqueue(&plan, 100).unwrap()
            };
            assert!(first.inserted);

            let mut reopened = EvictionQueue::open(&path).unwrap();
            let second = reopened.enqueue(&plan, 999).unwrap();
            assert_eq!(second.id, first.id);
            assert!(!second.inserted);
            assert_eq!(reopened.requests().count(), 1);
            let request = reopened.get(first.id).unwrap();
            assert_eq!(request.requested_at_ms, 100);
            assert_eq!(request.requested_hashes, vec![hash(7), hash(8)]);
            assert_eq!(request.state, EvictionRequestState::AwaitingBackendGc);
        }
        {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("evictions.json");
            let plan = queued_plan(FileRecordId::new(), FileVersionId::new());
            let mut queue = EvictionQueue::open(&path).unwrap();
            let first = queue.enqueue(&plan, 100).unwrap();
            let absent = plan_requested_hashes(&plan)
                .into_iter()
                .map(|hash| (hash, ObservedBlobState::Absent))
                .collect();
            queue.record_observation(first.id, absent, 101).unwrap();

            let second = queue.enqueue(&plan, 200).unwrap();
            assert_eq!(second.id, first.id);
            assert!(!second.inserted);
            let request = queue.get(first.id).unwrap();
            assert_eq!(request.requested_at_ms, 200);
            assert_eq!(request.state, EvictionRequestState::AwaitingBackendGc);

            drop(queue);
            let reopened = EvictionQueue::open(path).unwrap();
            assert_eq!(
                reopened.get(first.id).unwrap().state,
                EvictionRequestState::AwaitingBackendGc
            );
        }
        {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("evictions.json");
            let mut queue = EvictionQueue::open(&path).unwrap();
            for value in 1..=MAX_EVICTION_REQUESTS as u128 {
                let id = EvictionRequestId(Uuid::from_u128(value));
                queue.requests.insert(
                    id,
                    EvictionRequest {
                        id,
                        record_id: FileRecordId::new(),
                        version_id: FileVersionId::new(),
                        mode: EvictionMode::Safe,
                        requested_hashes: Vec::new(),
                        requested_at_ms: u64::try_from(value).unwrap(),
                        state: EvictionRequestState::AwaitingBackendGc,
                    },
                );
            }
            let plan = queued_plan(FileRecordId::new(), FileVersionId::new());
            assert!(matches!(
                queue.enqueue(&plan, 10_000),
                Err(EvictionQueueError::TooManyRequests)
            ));
            let oldest = EvictionRequestId(Uuid::from_u128(1));
            queue.requests.get_mut(&oldest).unwrap().state =
                EvictionRequestState::Completed { observed_at_ms: 2 };
            let inserted = queue.enqueue(&plan, 10_000).unwrap();
            assert!(inserted.inserted);
            assert_eq!(queue.requests.len(), MAX_EVICTION_REQUESTS);
            assert!(!queue.requests.contains_key(&oldest));
        }
        {
            let temp = TempDir::new().unwrap();
            let record_id = FileRecordId::new();
            let version_id = FileVersionId::new();
            let plan = queued_plan(record_id, version_id);
            let mut queue = EvictionQueue::open(temp.path().join("evictions.json")).unwrap();
            let id = queue.enqueue(&plan, 100).unwrap().id;

            assert_eq!(
                queue.revalidate_for_backend_gc(id, &plan).unwrap(),
                vec![hash(7), hash(8)]
            );

            let mut newly_shared = plan.clone();
            let protected = newly_shared.reclaimable.remove(0);
            newly_shared.shared_and_protected.push(protected);
            assert!(matches!(
                queue.revalidate_for_backend_gc(id, &newly_shared),
                Err(EvictionQueueError::FreshPlanMismatch(found)) if found == id
            ));

            let changed_version = queued_plan(record_id, FileVersionId::new());
            assert!(matches!(
                queue.revalidate_for_backend_gc(id, &changed_version),
                Err(EvictionQueueError::FreshPlanMismatch(found)) if found == id
            ));

            let mut changed_mode = plan.clone();
            changed_mode.mode = EvictionMode::DestructiveOverride;
            assert!(matches!(
                queue.revalidate_for_backend_gc(id, &changed_mode),
                Err(EvictionQueueError::FreshPlanMismatch(found)) if found == id
            ));
        }
        {
            let temp = TempDir::new().unwrap();
            let now = 1_000;
            let target_id = FileRecordId::new();
            let target_version = FileVersionId::new();
            let other_id = FileRecordId::new();
            let other_version = FileVersionId::new();
            let mut registry =
                AvailabilityRegistry::open(temp.path().join("availability.json")).unwrap();
            registry
                .put(resident_file(target_id, target_version, now))
                .unwrap();
            let mut other = resident_file(other_id, other_version, now);
            other.residency = Residency::NotResident;
            registry.put(other.clone()).unwrap();
            let catalog = EvictionCatalog::new([
                payload(target_id, target_version, (1, Some(4)), &[(2, Some(10))]),
                payload(other_id, other_version, (3, Some(4)), &[(2, Some(10))]),
            ])
            .unwrap();
            let plan = plan_safe_eviction(
                &registry,
                &catalog,
                target_id,
                now,
                EvictionRequirements::default(),
            )
            .unwrap();
            let mut queue = EvictionQueue::open(temp.path().join("evictions.json")).unwrap();
            let id = queue.enqueue(&plan, now).unwrap().id;

            assert_eq!(
                queue
                    .authorize_backend_gc(
                        id,
                        &registry,
                        &catalog,
                        now,
                        EvictionRequirements::default(),
                    )
                    .unwrap(),
                vec![hash(1), hash(2)]
            );

            other.residency = Residency::Resident { bytes: 14 };
            registry.put(other).unwrap();
            assert!(matches!(
                queue.authorize_backend_gc(
                    id,
                    &registry,
                    &catalog,
                    now,
                    EvictionRequirements::default(),
                ),
                Err(BackendGcAuthorizationError::Queue(
                    EvictionQueueError::FreshPlanMismatch(found)
                )) if found == id
            ));
        }
        {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("evictions.json");
            let plan = queued_plan(FileRecordId::new(), FileVersionId::new());
            let mut queue = EvictionQueue::open(&path).unwrap();
            let id = queue.enqueue(&plan, 100).unwrap().id;

            assert!(matches!(
                queue.record_observation(
                    id,
                    vec![(hash(7), ObservedBlobState::Absent)],
                    200
                ),
                Err(EvictionQueueError::IncompleteObservation(found)) if found == id
            ));
            assert_eq!(
                queue.get(id).unwrap().state,
                EvictionRequestState::AwaitingBackendGc
            );

            let completed = queue
                .record_observation(
                    id,
                    vec![
                        (hash(7), ObservedBlobState::Absent),
                        (hash(8), ObservedBlobState::Absent),
                    ],
                    300,
                )
                .unwrap();
            assert_eq!(
                completed.state,
                EvictionRequestState::Completed {
                    observed_at_ms: 300
                }
            );
            let mut reopened = EvictionQueue::open(&path).unwrap();
            assert_eq!(reopened.get(id).unwrap().state, completed.state);

            // Completion is a historical fact and never gets a later timestamp.
            let repeated = reopened
                .record_observation(
                    id,
                    vec![
                        (hash(7), ObservedBlobState::Absent),
                        (hash(8), ObservedBlobState::Absent),
                    ],
                    999,
                )
                .unwrap();
            assert_eq!(repeated.state, completed.state);
            assert!(matches!(
                reopened.revalidate_for_backend_gc(id, &plan),
                Err(EvictionQueueError::RequestNotPending(found)) if found == id
            ));
        }
    }
}

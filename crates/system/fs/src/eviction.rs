//! Fail-closed planning for local collaborative-file eviction.
//!
//! A file's content is a manifest plus content-defined chunks, named by the manifest's hash, and
//! chunks and manifests may be shared by several files, so deleting "the blobs for one file" needs
//! a complete reference catalog of the contents kept here. This module computes that set without
//! mutating storage.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fs_protocol::FileDocumentId;
use futures_util::stream::{self, StreamExt, TryStreamExt};
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::Hydrator;
use crate::availability::{
    AvailabilityRegistry, DocumentAvailability, EntryKind, EvictionBlocker, EvictionDecision,
    EvictionRequirements, pinned_in_snapshot,
};
use crate::clock::unix_now_ms;
use fs_protocol::{AvailabilityPolicy, Residency};
use tilo_lock::MutexRecover as _;

/// Bound catalog I/O so opening Settings does not serialize one store status query per chunk
/// across a large workspace.
const MAX_CONCURRENT_CATALOG_INSPECTIONS: usize = 8;

/// The one process-local bridge from durable eviction intent to removal from the store, so no
/// caller can silently replace exact-content receipts or the exact absence
/// observation with a Hydrator-lifetime approximation.
#[derive(Clone)]
pub struct EvictionCoordinator {
    hydrator: Arc<Hydrator>,
    registry: Arc<Mutex<AvailabilityRegistry>>,
    queue: Arc<tokio::sync::Mutex<EvictionQueue>>,
}

impl EvictionCoordinator {
    pub fn new(
        hydrator: Arc<Hydrator>,
        registry: Arc<Mutex<AvailabilityRegistry>>,
        queue: Arc<tokio::sync::Mutex<EvictionQueue>>,
    ) -> Self {
        Self {
            hydrator,
            registry,
            queue,
        }
    }

    /// The same fail-closed dry run used immediately before removal.
    pub async fn plan(
        &self,
        document_id: FileDocumentId,
        mode: EvictionMode,
    ) -> anyhow::Result<EvictionPlan> {
        let records = self
            .registry
            .lock_recover()
            .records()
            .cloned()
            .collect::<Vec<_>>();
        let catalog = self.catalog_for_targets(&records, &[document_id]).await?;
        let registry = self.registry.lock_recover();
        if registry.records().cloned().collect::<Vec<_>>() != records {
            anyhow::bail!("availability changed during eviction planning");
        }
        let plan = plan_for_mode(&registry, &catalog, document_id, mode)?;
        drop(registry);
        Ok(plan)
    }

    /// Build one coherent payload catalog for multiple Settings rows, then evaluate each safe
    /// plan against that same captured registry generation.
    pub async fn batch_safe_plans(
        &self,
        targets: &[FileDocumentId],
    ) -> anyhow::Result<HashMap<FileDocumentId, Option<EvictionPlan>>> {
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

    /// Enqueue and freshly authorize one safe request. `expected_hash`, when supplied by
    /// Settings' snapshot token, binds the action to the exact immutable manifest it previewed.
    pub async fn prepare(
        &self,
        document_id: FileDocumentId,
        mode: EvictionMode,
        existing_request: Option<EvictionRequestId>,
        expected_hash: Option<Hash>,
    ) -> anyhow::Result<(EvictionRequestId, EvictionPlan)> {
        let store = Arc::clone(&self.hydrator.store);
        let runtime = self.handoff_runtime();
        self.prepare_with_handoff(
            document_id,
            mode,
            existing_request,
            expected_hash,
            move |plan, existing_request, queue, registry, catalog, now_ms| {
                let request_id = match existing_request {
                    Some(request_id) => request_id,
                    None => queue.enqueue(plan, now_ms)?.id,
                };
                runtime.block_on(store.evict_request(
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

    /// The runtime a handoff waits on the store through. The handoff holds the availability
    /// lock, which the daemon's workers take too; a worker blocked on it can leave the daemon's
    /// timers undriven. The store has no runtime of its own, so this is the daemon's.
    #[must_use]
    pub fn handoff_runtime(&self) -> tokio::runtime::Handle {
        self.hydrator.runtime_handle.clone()
    }

    /// As [`Self::prepare`], but makes the actual removal an explicit caller-supplied critical
    /// section, so storage can keep its live namespace and dirty guards from validation through the
    /// durable enqueue and the removal itself.
    ///
    /// The handoff runs while the availability and queue locks are held. Those are synchronous
    /// guards, so the whole section runs on a blocking thread, where the handoff may wait on
    /// async store work with [`Self::handoff_runtime`]'s `block_on`, once and never nested.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "policy must remain stable from exact-plan revalidation through removal"
    )]
    pub async fn prepare_with_handoff<F>(
        &self,
        document_id: FileDocumentId,
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
                .find(|record| record.document_id == document_id)
                .and_then(current_content)
                .ok_or_else(|| anyhow::anyhow!("the file has no content to free"))?;
            if current != expected_hash {
                anyhow::bail!("workspace changed since preview");
            }
        }
        let catalog = self.catalog_for_targets(&records, &[document_id]).await?;
        let registry = Arc::clone(&self.registry);
        let queue = Arc::clone(&self.queue);
        tokio::task::spawn_blocking(move || {
            let registry_guard = registry.lock_recover();
            if registry_guard.records().cloned().collect::<Vec<_>>() != records {
                anyhow::bail!("availability changed during eviction planning");
            }
            let plan = plan_for_mode(&registry_guard, &catalog, document_id, mode)?;
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

    /// Observe exact store state once and atomically project completed work into durable local
    /// residency. A record whose content changed since is deliberately not changed.
    pub async fn observe_once(
        &self,
        document_id: FileDocumentId,
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
        if request.document_id != document_id
            || registry.get(document_id).is_none_or(|record| {
                current_content(record) != Some(request.content_hash)
                    // A later verified local hydration of the same content
                    // must not be erased by replaying an older completion.
                    || record.freshness.local_verified_at_ms.is_some_and(|at| at > observed_at_ms)
            })
        {
            return Ok(true);
        }
        let mut record = registry
            .get(document_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("evicted record disappeared"))?;
        record.residency = Residency::NotResident;
        record.freshness.local_verified_at_ms = None;
        registry.put(record)?;
        drop(registry);
        Ok(true)
    }

    pub fn spawn_observer(&self, document_id: FileDocumentId, id: EvictionRequestId) {
        let coordinator = self.clone();
        self.hydrator.runtime_handle.spawn(async move {
            for _ in 0..=75 {
                match coordinator.observe_once(document_id, id).await {
                    Ok(true) | Err(_) => break,
                    Ok(false) => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
                }
            }
        });
    }

    async fn catalog_for_targets(
        &self,
        records: &[DocumentAvailability],
        targets: &[FileDocumentId],
    ) -> anyhow::Result<EvictionCatalog> {
        let targets = targets.iter().copied().collect::<HashSet<_>>();
        let mut contents = Vec::new();
        for record in records {
            if record.kind != EntryKind::File {
                continue;
            }
            let protects_current = targets.contains(&record.document_id)
                || record.residency != Residency::NotResident
                || pinned_in_snapshot(records, record.document_id);
            if protects_current && let Some(hash) = current_content(record) {
                contents.push(hash);
            }
        }
        contents.sort_unstable_by_key(|hash| *hash.as_bytes());
        contents.dedup();
        let store = Arc::clone(&self.hydrator.store);
        let payloads = stream::iter(contents)
            .map(|hash| {
                let store = Arc::clone(&store);
                async move { store.inspect_content_payload(hash).await }
            })
            .buffer_unordered(MAX_CONCURRENT_CATALOG_INSPECTIONS)
            .try_collect::<Vec<_>>()
            .await?;
        Ok(EvictionCatalog::new(payloads))
    }
}

/// A record's current content, by the hash its blobs are kept under.
fn current_content(record: &DocumentAvailability) -> Option<Hash> {
    record.current_hash.map(|hash| Hash::from(hash.0))
}

fn plan_for_mode(
    registry: &AvailabilityRegistry,
    catalog: &EvictionCatalog,
    document_id: FileDocumentId,
    mode: EvictionMode,
) -> Result<EvictionPlan, PlanError> {
    match mode {
        EvictionMode::Safe => plan_safe_eviction(
            registry,
            catalog,
            document_id,
            unix_now_ms(),
            EvictionRequirements::default(),
        ),
        EvictionMode::DestructiveOverride => plan_destructive_eviction(
            registry,
            catalog,
            document_id,
            unix_now_ms(),
            EvictionRequirements::default(),
        ),
    }
}

/// One blob of a content, with locally verified byte accounting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlobExtent {
    pub hash: Hash,
    /// Distinguishes an absent blob from a partial blob whose stored byte count the store
    /// cannot determine.
    pub locally_present: bool,
    /// Bytes verified locally. For a present partial blob, `None` means the count is unknown;
    /// `locally_present` tells absence from a partial blob.
    pub resident_bytes: Option<u64>,
}

/// The blobs of one content: its manifest, whose hash names the content, and its chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentPayload {
    pub manifest: BlobExtent,
    pub chunks: Vec<BlobExtent>,
}

impl ContentPayload {
    fn blobs(&self) -> impl Iterator<Item = BlobExtent> + '_ {
        std::iter::once(self.manifest).chain(self.chunks.iter().copied())
    }
}

/// The blobs of every content a plan must consider, by content hash.
///
/// Planning fails closed when any resident or pinned content is absent: an incomplete catalog
/// cannot prove that a chunk is unshared.
#[derive(Clone, Debug, Default)]
pub struct EvictionCatalog {
    payloads: HashMap<Hash, ContentPayload>,
}

impl EvictionCatalog {
    pub fn new(payloads: impl IntoIterator<Item = ContentPayload>) -> Self {
        Self {
            payloads: payloads
                .into_iter()
                .map(|payload| (payload.manifest.hash, payload))
                .collect(),
        }
    }

    fn get(&self, hash: Hash) -> Option<&ContentPayload> {
        self.payloads.get(&hash)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvictionMode {
    Safe,
    /// The caller explicitly accepts that the target's only known copy may be
    /// lost. Shared blobs remain protected.
    DestructiveOverride,
}

pub const EVICTION_QUEUE_SCHEMA_VERSION: u16 = 3;
pub const MAX_EVICTION_REQUESTS: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EvictionRequestId(pub Uuid);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum EvictionRequestState {
    AwaitingEviction,
    /// Every requested hash was observed `NotFound` in one complete store observation; the only
    /// successful terminal state.
    Completed {
        observed_at_ms: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvictionRequest {
    pub id: EvictionRequestId,
    pub document_id: FileDocumentId,
    /// The content freed, by its hash.
    pub content_hash: Hash,
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

/// Crash-safe log for explicit eviction intent and observed completion.
///
/// The log does not trigger deletion by itself: the content-store bridge reauthorizes a
/// pending request and removes exactly its approved blobs from the store. Completion is recorded
/// only after one observation finds them all absent.
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
    #[error("store observation does not exactly cover request {0:?}")]
    IncompleteObservation(EvictionRequestId),
    #[error("eviction request {0:?} is no longer awaiting eviction")]
    RequestNotPending(EvictionRequestId),
    #[error("eviction queue contains too many requests")]
    TooManyRequests,
    #[error("fresh eviction plan no longer matches request {0:?}")]
    FreshPlanMismatch(EvictionRequestId),
}

#[derive(Debug, Error)]
pub enum EvictionAuthorizationError {
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Queue(#[from] EvictionQueueError),
}

impl EvictionQueue {
    /// Open the saved queue, or an empty one when there is none or it is written in an older
    /// format: requests of an older format are dropped, and the next enqueue replaces the file.
    /// A format newer than this build's is refused.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, EvictionQueueError> {
        let path = path.into();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        let format = (!bytes.is_empty())
            .then(|| serde_json::from_slice::<crate::availability::SchemaVersion>(&bytes))
            .transpose()?
            .map(|format| format.schema_version);
        if format.is_some_and(|version| version > EVICTION_QUEUE_SCHEMA_VERSION) {
            return Err(EvictionQueueError::UnsupportedVersion(
                format.unwrap_or_default(),
            ));
        }
        if format != Some(EVICTION_QUEUE_SCHEMA_VERSION) {
            return Ok(Self {
                path,
                requests: BTreeMap::new(),
            });
        }
        let document: EvictionQueueDocument = serde_json::from_slice(&bytes)?;
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

    /// Durably enqueue only the blobs approved by an exact dry-run plan; repeating an identical
    /// request is idempotent across restarts.
    pub fn enqueue(
        &mut self,
        plan: &EvictionPlan,
        requested_at_ms: u64,
    ) -> Result<EnqueueResult, EvictionQueueError> {
        let requested_hashes = plan_requested_hashes(plan);
        let id = request_id(
            plan.document_id,
            plan.content_hash,
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
                request.state = EvictionRequestState::AwaitingEviction;
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
            document_id: plan.document_id,
            content_hash: plan.content_hash,
            mode: plan.mode,
            requested_hashes,
            requested_at_ms,
            state: EvictionRequestState::AwaitingEviction,
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

    /// Reauthorize a queued request against a freshly computed exact plan, immediately before its
    /// hashes become collectible, while holding whatever lock keeps the availability registry and
    /// content catalog stable through the handoff. A change of the file's content, a policy refusal, or
    /// newly shared safety root changes the plan, so enqueue-time authority is never enough.
    ///
    /// Deletes and unprotects nothing; completion still requires one all-absent observation.
    fn revalidate_eviction(
        &self,
        id: EvictionRequestId,
        fresh_plan: &EvictionPlan,
    ) -> Result<Vec<Hash>, EvictionQueueError> {
        let request = self
            .requests
            .get(&id)
            .ok_or(EvictionQueueError::UnknownRequest(id))?;
        if !matches!(request.state, EvictionRequestState::AwaitingEviction) {
            return Err(EvictionQueueError::RequestNotPending(id));
        }
        if request.document_id != fresh_plan.document_id
            || request.content_hash != fresh_plan.content_hash
            || request.mode != fresh_plan.mode
            || request.requested_hashes != plan_requested_hashes(fresh_plan)
        {
            return Err(EvictionQueueError::FreshPlanMismatch(id));
        }
        Ok(request.requested_hashes.clone())
    }

    /// Recompute all policy and shared-root decisions before a removal.
    ///
    /// The returned hashes are authorized only while the caller keeps `registry` and `catalog`
    /// stable; they are not evidence of deletion, and
    /// [`ContentStore::observe_eviction_request`](crate::store::ContentStore::observe_eviction_request)
    /// remains the only completion path.
    pub fn authorize_eviction(
        &self,
        id: EvictionRequestId,
        registry: &AvailabilityRegistry,
        catalog: &EvictionCatalog,
        now_ms: u64,
        requirements: EvictionRequirements,
    ) -> Result<Vec<Hash>, EvictionAuthorizationError> {
        let request = self
            .requests
            .get(&id)
            .ok_or(EvictionQueueError::UnknownRequest(id))?;
        if !matches!(request.state, EvictionRequestState::AwaitingEviction) {
            return Err(EvictionQueueError::RequestNotPending(id).into());
        }
        // Either mode releases only what the person explicitly gave up.
        let fresh_plan = plan_destructive_eviction(
            registry,
            catalog,
            request.document_id,
            now_ms,
            requirements,
        )?;
        Ok(self.revalidate_eviction(id, &fresh_plan)?)
    }

    /// Record one all-or-nothing store observation. Partial samples never change durable state,
    /// and completed requests stay completed.
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
                EvictionRequestState::AwaitingEviction => {
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
    document_id: FileDocumentId,
    content_hash: Hash,
    mode: EvictionMode,
    hashes: &[Hash],
) -> EvictionRequestId {
    let mut digest_builder = blake3::Hasher::new();
    digest_builder.update(b"tilo.collaborative-fs.eviction-request.v3\0");
    digest_builder.update(document_id.0.as_bytes());
    digest_builder.update(content_hash.as_bytes());
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
            request.document_id,
            request.content_hash,
            request.mode,
            &request.requested_hashes,
        )
    {
        return Err(EvictionQueueError::NonCanonicalRequestId(request.id));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvictionPlan {
    pub document_id: FileDocumentId,
    /// The content freed, by its hash.
    pub content_hash: Hash,
    pub mode: EvictionMode,
    pub reclaimable: Vec<BlobExtent>,
    /// Target blobs kept because another resident or pinned file's content uses them.
    pub shared_and_protected: Vec<BlobExtent>,
    pub already_absent: Vec<Hash>,
    pub reclaimable_bytes: u64,
    /// Number of present reclaimable blobs left out of `reclaimable_bytes` because the store
    /// reported an unknown partial size.
    pub unaccounted_reclaimable_blobs: usize,
    pub protected_shared_bytes: u64,
    pub unaccounted_protected_shared_blobs: usize,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("availability policy denied eviction: {0:?}")]
    Denied(EvictionBlocker),
    #[error("the file has no content to free")]
    TargetHasNoContent,
    #[error("payload catalog is missing the target content {hash}")]
    TargetPayloadMissing { hash: Hash },
    #[error("payload catalog is missing the protected content {hash}")]
    ProtectedPayloadMissing { hash: Hash },
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
///
/// Nothing proves another device holds a copy since the tree became the record, so this mode
/// always refuses; callers that mean to lose the bytes say so with
/// [`plan_destructive_eviction`].
pub fn plan_safe_eviction(
    registry: &AvailabilityRegistry,
    catalog: &EvictionCatalog,
    document_id: FileDocumentId,
    now_ms: u64,
    requirements: EvictionRequirements,
) -> Result<EvictionPlan, PlanError> {
    plan(
        registry,
        catalog,
        document_id,
        now_ms,
        requirements,
        EvictionMode::Safe,
    )
}

/// Plan an explicitly destructive removal. It bypasses replica and pin checks only for the
/// target's current content; shared references and exact-content validation always apply.
pub fn plan_destructive_eviction(
    registry: &AvailabilityRegistry,
    catalog: &EvictionCatalog,
    document_id: FileDocumentId,
    now_ms: u64,
    requirements: EvictionRequirements,
) -> Result<EvictionPlan, PlanError> {
    plan(
        registry,
        catalog,
        document_id,
        now_ms,
        requirements,
        EvictionMode::DestructiveOverride,
    )
}

fn plan(
    registry: &AvailabilityRegistry,
    catalog: &EvictionCatalog,
    document_id: FileDocumentId,
    now_ms: u64,
    requirements: EvictionRequirements,
    mode: EvictionMode,
) -> Result<EvictionPlan, PlanError> {
    let destructive = mode == EvictionMode::DestructiveOverride;
    match registry.eviction_decision(document_id, now_ms, requirements, destructive) {
        EvictionDecision::Denied(blocker) => return Err(PlanError::Denied(blocker)),
        EvictionDecision::Allowed if destructive => return Err(PlanError::DecisionModeMismatch),
        EvictionDecision::AllowedDestructiveOverride if !destructive => {
            return Err(PlanError::DecisionModeMismatch);
        }
        EvictionDecision::Allowed | EvictionDecision::AllowedDestructiveOverride => {}
    }

    let target_document = registry
        .get(document_id)
        .ok_or(PlanError::Denied(EvictionBlocker::UnknownDocument))?;
    let content_hash = current_content(target_document).ok_or(PlanError::TargetHasNoContent)?;
    let target = catalog
        .get(content_hash)
        .ok_or(PlanError::TargetPayloadMissing { hash: content_hash })?;

    let mut protected = HashSet::new();
    for record in registry.records() {
        if record.kind != EntryKind::File {
            continue;
        }

        // A current payload is protected when it belongs to another locally
        // resident record, or when pin policy promises it should remain local.
        let protects_current = record.document_id != document_id
            && (record.residency != Residency::NotResident
                || registry
                    .effective_policy(record.document_id)
                    .map_err(|_| PlanError::InvalidAncestry)?
                    == Some(AvailabilityPolicy::Pinned));
        if protects_current && let Some(hash) = current_content(record) {
            add_protected_payload(&mut protected, catalog, hash)?;
        }
    }

    let target_blobs = canonical_blobs(target.blobs())?;
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
        document_id,
        content_hash,
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

/// Near-linear multi-target equivalent of repeated `plan_safe_eviction`: protected roots are
/// catalogued once, and each target then walks only its own immutable payload.
fn batch_plan_safe(
    registry: &AvailabilityRegistry,
    catalog: &EvictionCatalog,
    targets: &[FileDocumentId],
    now_ms: u64,
) -> HashMap<FileDocumentId, Option<EvictionPlan>> {
    let requirements = EvictionRequirements::default();
    let mut protected_counts = HashMap::<Hash, usize>::new();
    let mut own_current = HashMap::<FileDocumentId, HashSet<Hash>>::new();
    let mut roots_valid = true;
    for record in registry.records() {
        if record.kind != EntryKind::File {
            continue;
        }
        let current_is_protected = if let Ok(policy) = registry.effective_policy(record.document_id)
        {
            record.residency != Residency::NotResident || policy == Some(AvailabilityPolicy::Pinned)
        } else {
            roots_valid = false;
            true
        };
        if current_is_protected && let Some(hash) = current_content(record) {
            match payload_hashes(catalog, hash) {
                Ok(hashes) => {
                    for hash in &hashes {
                        let count = protected_counts.entry(*hash).or_default();
                        *count = count.saturating_add(1);
                    }
                    own_current.insert(record.document_id, hashes);
                }
                Err(_) => roots_valid = false,
            }
        }
    }
    targets
        .iter()
        .copied()
        .map(|document_id| {
            let plan = if roots_valid {
                batch_target_plan(
                    registry,
                    catalog,
                    document_id,
                    &protected_counts,
                    own_current.get(&document_id),
                    requirements,
                    now_ms,
                )
                .ok()
            } else {
                None
            };
            (document_id, plan)
        })
        .collect()
}

fn payload_hashes(catalog: &EvictionCatalog, hash: Hash) -> Result<HashSet<Hash>, PlanError> {
    Ok(catalog
        .get(hash)
        .ok_or(PlanError::ProtectedPayloadMissing { hash })?
        .blobs()
        .map(|blob| blob.hash)
        .collect())
}

fn batch_target_plan(
    registry: &AvailabilityRegistry,
    catalog: &EvictionCatalog,
    document_id: FileDocumentId,
    protected_counts: &HashMap<Hash, usize>,
    own_current: Option<&HashSet<Hash>>,
    requirements: EvictionRequirements,
    now_ms: u64,
) -> Result<EvictionPlan, PlanError> {
    if !matches!(
        registry.eviction_decision(document_id, now_ms, requirements, false),
        EvictionDecision::Allowed
    ) {
        return Err(PlanError::Denied(EvictionBlocker::UnknownDocument));
    }
    let record = registry
        .get(document_id)
        .ok_or(PlanError::Denied(EvictionBlocker::UnknownDocument))?;
    let content_hash = current_content(record).ok_or(PlanError::TargetHasNoContent)?;
    let target = canonical_blobs(
        catalog
            .get(content_hash)
            .ok_or(PlanError::TargetPayloadMissing { hash: content_hash })?
            .blobs(),
    )?;
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
        document_id,
        content_hash,
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
    hash: Hash,
) -> Result<(), PlanError> {
    let payload = catalog
        .get(hash)
        .ok_or(PlanError::ProtectedPayloadMissing { hash })?;
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
    use crate::availability::DocumentAvailability;
    use crate::store::StoreEvictionError;
    use bytes::Bytes;
    use tempfile::TempDir;

    fn hash(byte: u8) -> Hash {
        Hash::from([byte; 32])
    }

    /// The payload of the content whose manifest is `hash(manifest.0)`.
    fn payload(manifest: (u8, Option<u64>), chunks: &[(u8, Option<u64>)]) -> ContentPayload {
        ContentPayload {
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

    /// A file kept here whose content is the one with manifest `hash(content)`.
    fn resident_file(id: FileDocumentId, content: u8, now: u64) -> DocumentAvailability {
        let content = Some(fs_protocol::ContentHash([content; 32]));
        let mut record = DocumentAvailability::new(id, EntryKind::File);
        record.current_hash = content;
        record.residency = Residency::Resident { bytes: 30 };
        record.replicas = crate::availability::ReplicaEvidence {
            reachable: 0,
            durable: 1,
            acknowledged_hash: content,
        };
        record.freshness.replicas_observed_at_ms = Some(now);
        record
    }

    /// A person frees space on a device: only the target's own bytes are given back, a blob
    /// another file still needs is protected, and every refusal on the way — an empty file, a
    /// file it cannot account for, a pin, a plan that went stale — removes nothing.
    #[tokio::test]
    #[cfg(feature = "filesystem")]
    #[allow(
        clippy::too_many_lines,
        reason = "one journey through freeing space, read top to bottom"
    )]
    async fn freeing_space_gives_back_only_the_targets_own_bytes() {
        use crate::store::ContentStore;

        let temp = TempDir::new().unwrap();
        let store = ContentStore::open(temp.path().join("blobs")).await.unwrap();
        let target_bytes = Bytes::from(vec![0x5a; 2 * 1024 * 1024]);
        let target_only = store.ingest(target_bytes.clone()).await.unwrap();
        let target_path = temp
            .path()
            .join("blobs/files/data")
            .join(hex::encode(target_only.as_bytes()));
        assert!(target_path.exists());
        let shared = store.ingest(Bytes::from_static(b"shared")).await.unwrap();
        let other_only = store
            .ingest(Bytes::from_static(b"other only"))
            .await
            .unwrap();

        let target_id = FileDocumentId::new();
        let other_id = FileDocumentId::new();
        let registry_path = temp.path().join("availability.json");
        let mut registry = AvailabilityRegistry::open(&registry_path).unwrap();
        let mut target = DocumentAvailability::new(target_id, EntryKind::File);
        target.current_hash = Some(fs_protocol::ContentHash(*target_only.as_bytes()));
        target.residency = Residency::Resident {
            bytes: target_bytes.len() as u64 + 6,
        };
        registry.put(target).unwrap();
        let mut other = DocumentAvailability::new(other_id, EntryKind::File);
        other.current_hash = Some(fs_protocol::ContentHash(*other_only.as_bytes()));
        other.residency = Residency::Resident { bytes: 16 };
        registry.put(other.clone()).unwrap();
        let extent = |hash, bytes| BlobExtent {
            hash,
            locally_present: true,
            resident_bytes: Some(bytes),
        };
        let catalog = EvictionCatalog::new([
            ContentPayload {
                manifest: extent(target_only, target_bytes.len() as u64),
                chunks: vec![extent(shared, 6)],
            },
            ContentPayload {
                manifest: extent(other_only, 10),
                chunks: vec![extent(shared, 6)],
            },
        ]);

        // An empty file has no bytes to free.
        let empty = FileDocumentId::new();
        let mut empty_record = DocumentAvailability::new(empty, EntryKind::File);
        empty_record.current_hash = None;
        registry.put(empty_record).unwrap();
        assert_eq!(
            plan_destructive_eviction(
                &registry,
                &catalog,
                empty,
                1,
                EvictionRequirements::default(),
            ),
            Err(PlanError::TargetHasNoContent),
            "an empty file has no bytes to free"
        );

        // The target's own blob goes; the blob the other file shares stays.
        let plan = plan_destructive_eviction(
            &registry,
            &catalog,
            target_id,
            1,
            EvictionRequirements::default(),
        )
        .unwrap();
        assert_eq!(plan.mode, EvictionMode::DestructiveOverride);
        assert_eq!(
            plan.reclaimable
                .iter()
                .map(|blob| blob.hash)
                .collect::<Vec<_>>(),
            vec![target_only]
        );
        assert_eq!(
            plan.shared_and_protected
                .iter()
                .map(|blob| blob.hash)
                .collect::<Vec<_>>(),
            vec![shared]
        );

        // Another file with the very same content keeps all of it.
        let twin_temp = TempDir::new().unwrap();
        let mut twin_registry =
            AvailabilityRegistry::open(twin_temp.path().join("availability.json")).unwrap();
        let twin_target = DocumentAvailability::new(FileDocumentId::new(), EntryKind::File);
        let mut twin_target = twin_target;
        twin_target.current_hash = Some(fs_protocol::ContentHash(*target_only.as_bytes()));
        twin_target.residency = Residency::Resident { bytes: 13 };
        let twin_target_id = twin_target.document_id;
        twin_registry.put(twin_target).unwrap();
        let twin = FileDocumentId::new();
        let mut twin_record = DocumentAvailability::new(twin, EntryKind::File);
        twin_record.current_hash = Some(fs_protocol::ContentHash(*target_only.as_bytes()));
        twin_record.residency = Residency::Resident { bytes: 7 };
        twin_registry.put(twin_record).unwrap();
        let plan = plan_destructive_eviction(
            &twin_registry,
            &catalog,
            twin_target_id,
            1,
            EvictionRequirements::default(),
        )
        .unwrap();
        assert!(
            plan.reclaimable.is_empty(),
            "a twin keeps every blob of the content it shares"
        );
        let mut protected: Vec<_> = plan
            .shared_and_protected
            .iter()
            .map(|blob| blob.hash)
            .collect();
        protected.sort();
        let mut expected = vec![target_only, shared];
        expected.sort();
        assert_eq!(protected, expected, "the twin's whole content is protected");
        assert!(
            twin_registry
                .get(twin)
                .is_some_and(|record| record.residency == Residency::Resident { bytes: 7 })
        );

        // A file whose content the device cannot inspect is a refusal, not a guess.
        let gap_temp = TempDir::new().unwrap();
        let mut gap_registry =
            AvailabilityRegistry::open(gap_temp.path().join("availability.json")).unwrap();
        let own = FileDocumentId::new();
        let sharing = FileDocumentId::new();
        gap_registry.put(resident_file(own, 1, 1_000)).unwrap();
        gap_registry.put(resident_file(sharing, 5, 1_000)).unwrap();
        let gap_catalog = EvictionCatalog::new([payload((1, Some(4)), &[(2, Some(10))])]);
        assert_eq!(
            plan_destructive_eviction(
                &gap_registry,
                &gap_catalog,
                own,
                1_000,
                EvictionRequirements::default(),
            ),
            Err(PlanError::ProtectedPayloadMissing { hash: hash(5) })
        );

        // A pin is never given up without the person saying so.
        let pin_temp = TempDir::new().unwrap();
        let mut pin_registry =
            AvailabilityRegistry::open(pin_temp.path().join("availability.json")).unwrap();
        let pinned = FileDocumentId::new();
        let mut pinned_record = resident_file(pinned, 9, 1_000);
        pinned_record.policy = AvailabilityPolicy::Pinned;
        pin_registry.put(pinned_record).unwrap();
        let pinned_catalog = EvictionCatalog::new([payload((9, Some(4)), &[])]);
        assert_eq!(
            plan_safe_eviction(
                &pin_registry,
                &pinned_catalog,
                pinned,
                1_000,
                EvictionRequirements::default(),
            ),
            Err(PlanError::Denied(EvictionBlocker::Pinned))
        );

        // A safe batch hands nothing off for a content no replica acknowledged.
        let first = FileDocumentId::new();
        let second = FileDocumentId::new();
        let mut batch_registry =
            AvailabilityRegistry::open(temp.path().join("batch.json")).unwrap();
        batch_registry
            .put(resident_file(first, 1, unix_now_ms()))
            .unwrap();
        batch_registry
            .put(resident_file(second, 2, unix_now_ms()))
            .unwrap();
        let batch_catalog = EvictionCatalog::new([
            payload((1, Some(4)), &[(7, Some(9)), (8, None)]),
            payload((2, Some(4)), &[(7, Some(9)), (9, None)]),
        ]);
        let batch = batch_plan_safe(
            &batch_registry,
            &batch_catalog,
            &[first, second],
            unix_now_ms(),
        );
        for record in [first, second] {
            assert!(
                batch.get(&record).and_then(Option::as_ref).is_none(),
                "a safe batch refuses a record it cannot prove safe"
            );
        }

        // A file is pinned in a snapshot even when its parent is gone or is itself.
        let child_id = FileDocumentId::new();
        let gone = FileDocumentId::new();
        let mut child = DocumentAvailability::new(child_id, EntryKind::File);
        child.parent = Some(gone);
        assert!(pinned_in_snapshot(&[child.clone()], child_id));
        child.parent = Some(child_id);
        assert!(pinned_in_snapshot(&[child], child_id));

        // The queue frees exactly the authorized blob and its completion survives a restart.
        let fresh = plan_destructive_eviction(
            &registry,
            &catalog,
            target_id,
            1,
            EvictionRequirements::default(),
        )
        .unwrap();
        let mut queue = EvictionQueue::open(temp.path().join("evictions.json")).unwrap();
        let id = queue.enqueue(&fresh, 1).unwrap().id;
        store
            .evict_request(
                &queue,
                id,
                &registry,
                &catalog,
                2,
                EvictionRequirements::default(),
            )
            .await
            .unwrap();
        assert!(!store.has(target_only).await);
        assert!(
            !target_path.exists(),
            "eviction removes the whole blob's file"
        );
        assert_eq!(
            store.local(shared).await,
            Some(Bytes::from_static(b"shared"))
        );
        assert_eq!(
            store.local(other_only).await,
            Some(Bytes::from_static(b"other only"))
        );
        let observation = store
            .observe_eviction_request(&mut queue, id, 3)
            .await
            .unwrap();
        assert!(matches!(
            observation.state,
            EvictionRequestState::Completed { .. }
        ));
        assert!(matches!(
            EvictionQueue::open(temp.path().join("evictions.json"))
                .unwrap()
                .get(id)
                .unwrap()
                .state,
            EvictionRequestState::Completed { .. }
        ));

        // A plan that went stale removes nothing: the other file now keeps the shared blob.
        let stale_temp = TempDir::new().unwrap();
        let stale_store = ContentStore::open(stale_temp.path().join("blobs"))
            .await
            .unwrap();
        let becomes_shared = stale_store
            .ingest(Bytes::from_static(b"becomes shared"))
            .await
            .unwrap();
        let other_root = stale_store
            .ingest(Bytes::from_static(b"other root"))
            .await
            .unwrap();
        let mut stale_registry =
            AvailabilityRegistry::open(stale_temp.path().join("availability.json")).unwrap();
        let stale_target = DocumentAvailability::new(FileDocumentId::new(), EntryKind::File);
        let mut stale_target = stale_target;
        stale_target.current_hash = Some(fs_protocol::ContentHash(*becomes_shared.as_bytes()));
        stale_target.residency = Residency::Resident { bytes: 14 };
        let stale_target_id = stale_target.document_id;
        stale_registry.put(stale_target).unwrap();
        let mut stale_other = DocumentAvailability::new(FileDocumentId::new(), EntryKind::File);
        stale_other.current_hash = Some(fs_protocol::ContentHash(*other_root.as_bytes()));
        stale_other.residency = Residency::NotResident;
        let stale_other_id = stale_other.document_id;
        stale_registry.put(stale_other.clone()).unwrap();
        let stale_catalog = EvictionCatalog::new([
            ContentPayload {
                manifest: extent(becomes_shared, 14),
                chunks: Vec::new(),
            },
            ContentPayload {
                manifest: extent(other_root, 10),
                chunks: vec![extent(becomes_shared, 14)],
            },
        ]);
        let stale = plan_destructive_eviction(
            &stale_registry,
            &stale_catalog,
            stale_target_id,
            1,
            EvictionRequirements::default(),
        )
        .unwrap();
        let mut stale_queue =
            EvictionQueue::open(stale_temp.path().join("evictions.json")).unwrap();
        let stale_id = stale_queue.enqueue(&stale, 1).unwrap().id;
        stale_other.residency = Residency::Resident { bytes: 24 };
        stale_registry.put(stale_other).unwrap();
        assert!(stale_registry.get(stale_other_id).is_some());
        assert!(matches!(
            stale_store
                .evict_request(
                    &stale_queue,
                    stale_id,
                    &stale_registry,
                    &stale_catalog,
                    2,
                    EvictionRequirements::default(),
                )
                .await,
            Err(StoreEvictionError::Authorization(_))
        ));
        assert!(
            stale_store.has(becomes_shared).await,
            "a refused eviction removes nothing"
        );
        assert!(store.has(shared).await);
    }

    /// The queue is bounded, keeps its history across a restart, and hands off only a fresh,
    /// complete observation of the exact plan it was asked about.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one journey through the queue's refusals, read top to bottom"
    )]
    fn the_eviction_queue_is_bounded_fresh_and_durable() {
        {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("cache/evictions.json");
            let document_id = FileDocumentId::new();
            let plan = queued_plan(document_id, hash(42));

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
            assert_eq!(request.state, EvictionRequestState::AwaitingEviction);
        }
        {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("evictions.json");
            let plan = queued_plan(FileDocumentId::new(), hash(42));
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
            assert_eq!(request.state, EvictionRequestState::AwaitingEviction);

            drop(queue);
            let reopened = EvictionQueue::open(path).unwrap();
            assert_eq!(
                reopened.get(first.id).unwrap().state,
                EvictionRequestState::AwaitingEviction
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
                        document_id: FileDocumentId::new(),
                        content_hash: hash(42),
                        mode: EvictionMode::Safe,
                        requested_hashes: Vec::new(),
                        requested_at_ms: u64::try_from(value).unwrap(),
                        state: EvictionRequestState::AwaitingEviction,
                    },
                );
            }
            let plan = queued_plan(FileDocumentId::new(), hash(42));
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
            let document_id = FileDocumentId::new();
            let plan = queued_plan(document_id, hash(42));
            let mut queue = EvictionQueue::open(temp.path().join("evictions.json")).unwrap();
            let id = queue.enqueue(&plan, 100).unwrap().id;

            assert_eq!(
                queue.revalidate_eviction(id, &plan).unwrap(),
                vec![hash(7), hash(8)]
            );

            let mut newly_shared = plan.clone();
            let protected = newly_shared.reclaimable.remove(0);
            newly_shared.shared_and_protected.push(protected);
            assert!(matches!(
                queue.revalidate_eviction(id, &newly_shared),
                Err(EvictionQueueError::FreshPlanMismatch(found)) if found == id
            ));

            let changed_content = queued_plan(document_id, hash(43));
            assert!(matches!(
                queue.revalidate_eviction(id, &changed_content),
                Err(EvictionQueueError::FreshPlanMismatch(found)) if found == id
            ));

            let mut changed_mode = plan.clone();
            changed_mode.mode = EvictionMode::DestructiveOverride;
            assert!(matches!(
                queue.revalidate_eviction(id, &changed_mode),
                Err(EvictionQueueError::FreshPlanMismatch(found)) if found == id
            ));
        }
        {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("evictions.json");
            let plan = queued_plan(FileDocumentId::new(), hash(42));
            let mut queue = EvictionQueue::open(&path).unwrap();
            let id = queue.enqueue(&plan, 100).unwrap().id;

            assert!(matches!(
                queue.record_observation(id, vec![(hash(7), ObservedBlobState::Absent)], 200),
                Err(EvictionQueueError::IncompleteObservation(found)) if found == id
            ));
            assert_eq!(
                queue.get(id).unwrap().state,
                EvictionRequestState::AwaitingEviction
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
                reopened.revalidate_eviction(id, &plan),
                Err(EvictionQueueError::RequestNotPending(found)) if found == id
            ));
        }
    }

    fn queued_plan(document_id: FileDocumentId, content_hash: Hash) -> EvictionPlan {
        EvictionPlan {
            document_id,
            content_hash,
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
}

//! Canonical sync-storage accounting and conservative local hydration eviction.
//!
//! This authority reasons over CRDT node identities and content hashes, never
//! host paths.  Consequently symlinks cannot escape the workspace namespace.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use iroh_blobs::Hash;
use runtime_fs::storage::{
    CacheFreed, EvictionReport, EvictionSelection, ResidencySplit, ResidencySummary,
    STORAGE_FRAMING, StorageAggregate, StorageRequest, StorageResponse, SyncStorageSnapshot,
    SyncedFileState, WorkspaceResidency,
};

use crate::availability::{
    AvailabilityRegistry, RecordAvailability, RecordKind, pinned_in_snapshot,
};
use crate::clock::unix_now_ms;
use crate::eviction::{EvictionCoordinator, EvictionPlan, EvictionQueue, EvictionRequestId};
use crate::namespace::{NodeKind, ROOT_INO};
use crate::store::area::Retention;
use crate::{FileHistoryStore, Hydrator, Namespace};
use tilo_lock::MutexRecover as _;

#[derive(Clone)]
pub struct StorageAuthority {
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    dirty: Arc<Mutex<crate::fs::WriteStages>>,
    /// Settings shares the same durable authority as FUSE.  In particular,
    /// it never treats a daemon-lifetime hydration source as durability.
    availability: Option<Arc<Mutex<AvailabilityRegistry>>>,
    eviction_queue: Option<Arc<tokio::sync::Mutex<EvictionQueue>>>,
    history: Option<Arc<FileHistoryStore>>,
    eviction: Arc<tokio::sync::Mutex<()>>,
    workspace: String,
    #[cfg(target_os = "linux")]
    external_history: Option<Arc<crate::external_history::ExternalHistoryFactory>>,
    /// The person's prefetch switch and cache budget; `None` answers the
    /// policy requests with an error.
    cache_policy: Option<Arc<crate::cache_policy::CachePolicies>>,
}

/// Serve the narrow storage protocol.  The caller owns socket placement and
/// permissions; no document bytes or mutation operations are exposed here.
pub async fn serve(listener: tokio::net::UnixListener, authority: StorageAuthority) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            continue;
        };
        let authority = authority.clone();
        tokio::spawn(async move {
            // A close, a malformed or oversized frame, or a failed write
            // ends the connection.
            while let Ok(Some(request)) = STORAGE_FRAMING
                .read_value_async::<_, StorageRequest>(&mut stream)
                .await
            {
                let response = match request {
                    StorageRequest::BindExternalSource | StorageRequest::BindExternalReadSource => {
                        #[cfg(target_os = "linux")]
                        let read = matches!(request, StorageRequest::BindExternalReadSource);
                        #[cfg(target_os = "linux")]
                        if let Some(factory) = authority.external_history.as_ref().filter(|_| {
                            // Routing admission prevents unprivileged clients from
                            // occupying enrollment workers. Actual root/helper
                            // authority still requires mutual kernel QPE below.
                            stream.peer_cred().is_ok_and(|peer| peer.uid() == 0)
                        }) {
                            let factory = Arc::clone(factory);
                            if STORAGE_FRAMING
                                .write_value_async(
                                    &mut stream,
                                    &if read {
                                        StorageResponse::ExternalReadSourceBinding { schema: 1 }
                                    } else {
                                        StorageResponse::ExternalSourceBinding
                                    },
                                )
                                .await
                                .is_err()
                            {
                                return;
                            }
                            let Ok(stream) = stream.into_std() else {
                                return;
                            };
                            if stream.set_nonblocking(false).is_err() {
                                return;
                            }
                            // Native proof/descriptor exchange is a bounded owned
                            // blocking worker, never the async service reactor.
                            let _ = tokio::task::spawn_blocking(move || {
                                if read {
                                    factory.serve_read_enrollment(stream)
                                } else {
                                    factory.serve_enrollment(stream)
                                }
                            })
                            .await;
                            return;
                        }
                        StorageResponse::Error(
                            "Certified personal external-source enrollment unavailable".into(),
                        )
                    }
                    StorageRequest::Snapshot => {
                        StorageResponse::Snapshot(authority.snapshot().await)
                    }
                    StorageRequest::Evict { selected } => match authority.evict(selected).await {
                        Ok(report) => StorageResponse::Evicted(report),
                        Err(error) => StorageResponse::Error(error.to_string()),
                    },
                    StorageRequest::CachePolicy => authority.cache_policy(None).await,
                    StorageRequest::SetCachePolicy(policy) => {
                        authority.cache_policy(Some(policy)).await
                    }
                    StorageRequest::Residency => {
                        StorageResponse::Residency(authority.residency().await)
                    }
                    StorageRequest::FreeCache { workspace } => {
                        match authority.free_cache(&workspace).await {
                            Ok(freed) => StorageResponse::CacheFreed(freed),
                            Err(error) => StorageResponse::Error(error.to_string()),
                        }
                    }
                    StorageRequest::KeepHere { workspace, keep } => {
                        match authority.keep_here(&workspace, keep) {
                            Ok(()) => StorageResponse::Kept { keep },
                            Err(error) => StorageResponse::Error(error.to_string()),
                        }
                    }
                };
                if STORAGE_FRAMING
                    .write_value_async(&mut stream, &response)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
    }
}

#[derive(Clone)]
struct FileBlobs {
    state: SyncedFileState,
}

/// A live file as seen through the namespace, before residency is looked up.
struct LogicalFile {
    identity: String,
    inode: u64,
    path: String,
    logical_bytes: u64,
    hash: Option<Hash>,
    record_id: fs_protocol::FileRecordId,
    mtime: u64,
}

impl StorageAuthority {
    /// Install only the concrete certified personal daemon factory. An absent
    /// context stays an explicit unavailable response on the existing socket.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn with_external_history(
        mut self,
        factory: Option<Arc<crate::external_history::ExternalHistoryFactory>>,
    ) -> Self {
        self.external_history = factory;
        self
    }
    pub fn new(
        namespace: Arc<Mutex<Namespace>>,
        hydrator: Arc<Hydrator>,
        dirty: Arc<Mutex<crate::fs::WriteStages>>,
        availability: Option<Arc<Mutex<AvailabilityRegistry>>>,
        eviction_queue: Option<Arc<tokio::sync::Mutex<EvictionQueue>>>,
        history: Option<Arc<FileHistoryStore>>,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            namespace,
            hydrator,
            dirty,
            availability,
            eviction_queue,
            history,
            eviction: Arc::new(tokio::sync::Mutex::new(())),
            workspace: workspace.into(),
            #[cfg(target_os = "linux")]
            external_history: None,
            cache_policy: None,
        }
    }

    /// Answer the cache-policy requests from `policies`.
    #[must_use]
    pub fn with_cache_policy(mut self, policies: Arc<crate::cache_policy::CachePolicies>) -> Self {
        self.cache_policy = Some(policies);
        self
    }

    /// The policy in force, after replacing it with `set` if given. Setting
    /// a smaller budget evicts at once, so it runs off the async workers.
    async fn cache_policy(&self, set: Option<runtime_fs::storage::CachePolicy>) -> StorageResponse {
        let Some(policies) = self.cache_policy.clone() else {
            return StorageResponse::Error("this mount keeps no cache policy".into());
        };
        let answer = tokio::task::spawn_blocking(move || match set {
            Some(policy) => policies.set(policy),
            None => Ok(policies.current()),
        })
        .await;
        match answer {
            Ok(Ok(policy)) => StorageResponse::CachePolicy(policy),
            Ok(Err(error)) => StorageResponse::Error(format!("{error:#}")),
            Err(error) => StorageResponse::Error(error.to_string()),
        }
    }

    /// The live files the namespace knows about, filtered to regular files.
    fn logical_files(&self) -> Vec<LogicalFile> {
        let mut ns = self.namespace.lock_recover();
        ns.live_node_ids()
            .into_iter()
            .filter_map(|identity| {
                let entry = ns.entry_for_node(&identity)?;
                if entry.kind != NodeKind::File {
                    return None;
                }
                let inode = ns.inode_for_node(&identity)?;
                let path = ns.path_for(inode)?;
                let record_id = ns.record_id_for(inode)?;
                Some(LogicalFile {
                    identity,
                    inode,
                    path,
                    logical_bytes: entry.size,
                    hash: entry.hash,
                    record_id,
                    mtime: entry.mtime,
                })
            })
            .collect()
    }

    /// The accounting entry for one logical file: content-less files (never
    /// written) report their logical size as already hydrated; the rest ask
    /// the hydrator for real residency.
    async fn file_blobs(
        &self,
        file: LogicalFile,
        dirty: &HashSet<u64>,
        records: &[RecordAvailability],
        plans: &HashMap<fs_protocol::FileRecordId, Option<EvictionPlan>>,
    ) -> FileBlobs {
        let LogicalFile {
            identity,
            inode,
            path,
            logical_bytes,
            hash,
            record_id,
            mtime,
        } = file;
        let is_dirty = dirty.contains(&inode);
        let record = records.iter().find(|record| record.record_id == record_id);
        let is_pinned = pinned_in_snapshot(records, record_id);
        let Some(hash) = hash else {
            return FileBlobs {
                state: SyncedFileState {
                    workspace: self.workspace.clone(),
                    app: app_from_path(&path),
                    logical_path: path,
                    identity,
                    inode,
                    version: format!("namespace-{mtime}"),
                    logical_bytes,
                    hydrated_bytes: logical_bytes,
                    remote_only: false,
                    dirty: is_dirty,
                    pinned: is_pinned,
                    in_flight: false,
                    remotely_durable: false,
                    reclaimable_bytes: 0,
                },
            };
        };
        let residence = self.hydrator.residency(hash).await;
        // A signed `ReplicaAckV1` persisted in AvailabilityRegistry is the
        // only durability proof Settings may project. Hydrator peer/source
        // state is intentionally only transport telemetry and resets at exit.
        let durable = record.is_some_and(has_fresh_signed_ack);
        let in_flight =
            residence.in_flight || record.is_some_and(|record| record.transfer.phase.is_active());
        let reclaimable = if !is_dirty && !is_pinned && !in_flight && durable {
            plans
                .get(&record_id)
                .and_then(Option::as_ref)
                .map_or(0, |plan| plan.reclaimable_bytes)
        } else {
            0
        };
        FileBlobs {
            state: SyncedFileState {
                workspace: self.workspace.clone(),
                app: app_from_path(&path),
                logical_path: path,
                identity,
                inode,
                version: hex::encode(hash.as_bytes()),
                logical_bytes,
                hydrated_bytes: residence.hydrated_bytes,
                remote_only: residence.remote_only,
                dirty: is_dirty,
                pinned: is_pinned,
                in_flight,
                remotely_durable: durable,
                reclaimable_bytes: reclaimable,
            },
        }
    }

    pub async fn snapshot(&self) -> SyncStorageSnapshot {
        let logical = self.logical_files();
        let online = !self.hydrator.known_peers().is_empty();
        let dirty = self.dirty_inodes();
        let records = self.records();
        // One captured registry generation and one superset history catalog
        // serve every row. A failed/changed catalog yields no reclaimable UI
        // rows rather than re-planning files independently.
        // Only rows that can plausibly be allowed enter the expensive exact
        // payload catalog.  The coordinator still adds every resident, pinned,
        // and retained root needed to prove shared-chunk safety.
        let target_ids = snapshot_candidate_ids(&logical, &dirty, &records, unix_now_ms());
        let plans = match self.coordinator() {
            Ok(coordinator) => coordinator
                .batch_safe_plans(&target_ids)
                .await
                .unwrap_or_default(),
            Err(_) => HashMap::new(),
        };

        let mut details = Vec::with_capacity(logical.len());
        for file in logical {
            details.push(self.file_blobs(file, &dirty, &records, &plans).await);
        }

        let files = details.into_iter().map(|f| f.state).collect::<Vec<_>>();
        SyncStorageSnapshot {
            online,
            workspaces: aggregate(files.iter().map(|f| (&f.workspace, f))),
            apps: aggregate(files.iter().filter_map(|f| f.app.as_ref().map(|a| (a, f)))),
            files,
        }
    }

    fn dirty_inodes(&self) -> HashSet<u64> {
        self.dirty.lock_recover().keys().copied().collect()
    }

    fn records(&self) -> Vec<RecordAvailability> {
        self.availability
            .as_ref()
            .map_or_else(Vec::new, |registry| {
                registry.lock_recover().records().cloned().collect()
            })
    }

    fn root_record(&self) -> Option<fs_protocol::FileRecordId> {
        self.namespace.lock_recover().record_id_for(ROOT_INO)
    }

    fn check_workspace(&self, workspace: &str) -> Result<()> {
        anyhow::ensure!(
            workspace == self.workspace,
            "this device mounts no workspace named {workspace}"
        );
        Ok(())
    }

    /// What this device holds of the workspace, split by what losing the
    /// device would cost ([`classify`]). It is answered to the person's own
    /// devices, so it carries counts and never a path.
    pub async fn residency(&self) -> ResidencySummary {
        let logical = self.logical_files();
        let dirty = self.dirty_inodes();
        let records = self.records();
        let by_id: HashMap<_, _> = records
            .iter()
            .map(|record| (record.record_id, record))
            .collect();
        // A pin reaches every file below the folder it is set on, whether
        // or not the file has a record of its own yet.
        let pinned: HashSet<u64> = {
            let mut ns = self.namespace.lock_recover();
            logical
                .iter()
                .filter(|file| kept_by_policy(&mut ns, file.inode, &by_id))
                .map(|file| file.inode)
                .collect()
        };
        // The same receipts the cache budget gives up content for first.
        let held_elsewhere = self
            .availability
            .as_ref()
            .map(|registry| registry.lock_recover().held_elsewhere())
            .unwrap_or_default();
        let files = self.hydrator.store.files();
        let mut split = ResidencySplit::default();
        for file in logical {
            let unsaved = dirty.contains(&file.inode);
            let facts = match file.hash {
                // Never written: whatever it holds is staged here.
                None => FileFacts {
                    local_bytes: file.logical_bytes,
                    unsaved,
                    ..FileFacts::default()
                },
                Some(hash) => {
                    let residence = self.hydrator.residency(hash).await;
                    FileFacts {
                        local_bytes: residence.hydrated_bytes,
                        unsaved,
                        pinned: pinned.contains(&file.inode),
                        cached: files
                            .entry(hash.as_bytes())
                            .is_some_and(|entry| entry.retention == Retention::Cached),
                        acknowledged: held_elsewhere.contains(hash.as_bytes()),
                        fetched: residence.remote_source.is_some() || residence.remotely_durable,
                    }
                }
            };
            add_to(&mut split, &facts);
        }
        let kept_here = self.root_record().is_some_and(|root| {
            by_id
                .get(&root)
                .is_some_and(|record| record.policy == fs_protocol::AvailabilityPolicy::Pinned)
        });
        ResidencySummary {
            workspaces: vec![WorkspaceResidency {
                workspace: self.workspace.clone(),
                kept_here,
                split,
            }],
        }
    }

    /// Drop the workspace's cache: the content this device fetched from
    /// another one and keeps only to open it again quickly. What is being
    /// written or fetched right now stays; pinned and local-only content is
    /// never cache. A dropped file downloads again when it is opened.
    ///
    /// # Errors
    ///
    /// Returns an error when `workspace` is not this one or an entry's
    /// removal cannot be recorded.
    pub async fn free_cache(&self, workspace: &str) -> Result<CacheFreed> {
        self.check_workspace(workspace)?;
        let _exclusive = self.eviction.lock().await;
        let dirty = self.dirty_inodes();
        let mut keys = self
            .logical_files()
            .into_iter()
            .filter(|file| !dirty.contains(&file.inode))
            .filter_map(|file| file.hash)
            .filter(|hash| !self.hydrator.is_fetching(*hash))
            .map(|hash| *hash.as_bytes())
            .collect::<Vec<_>>();
        keys.sort_unstable();
        keys.dedup();
        let files = Arc::clone(self.hydrator.store.files());
        tokio::task::spawn_blocking(move || drop_cached(&files, &keys)).await?
    }

    /// Keep the whole workspace on this device ("Keep offline" on its root),
    /// or stop keeping it. Pinned content is fetched in the background.
    ///
    /// # Errors
    ///
    /// Returns an error when `workspace` is not this one, or the policy
    /// cannot be recorded.
    pub fn keep_here(&self, workspace: &str, keep: bool) -> Result<()> {
        self.check_workspace(workspace)?;
        let registry = self
            .availability
            .as_ref()
            .ok_or_else(|| anyhow!("durable availability registry is unavailable"))?;
        let root = self
            .root_record()
            .ok_or_else(|| anyhow!("the workspace has no root record"))?;
        let policy = if keep {
            fs_protocol::AvailabilityPolicy::Pinned
        } else {
            fs_protocol::AvailabilityPolicy::Automatic
        };
        registry
            .lock_recover()
            .set_policy_along(&[(root, RecordKind::Directory, None)], policy)?;
        Ok(())
    }

    /// # Errors
    ///
    /// Safe eviction uses the durable, exact-version record plane.  It can
    /// queue while offline only when a fresh persisted signed receipt already
    /// proves an eligible replica; it never promotes a live Hydrator peer to
    /// durability and never directly deletes a blob.
    pub async fn evict(&self, selected: Vec<EvictionSelection>) -> Result<EvictionReport> {
        let _exclusive = self.eviction.lock().await;
        let mut report = EvictionReport::default();
        for request in selected {
            let Some(file) = self
                .logical_files()
                .into_iter()
                .find(|file| file.identity == request.identity && file.inode == request.inode)
            else {
                report
                    .skipped
                    .push(format!("{} changed since preview", request.identity));
                continue;
            };
            if file
                .hash
                .map(|hash| hex::encode(hash.as_bytes()))
                .as_deref()
                != Some(&request.version)
            {
                report
                    .skipped
                    .push(format!("{} changed since preview", file.path));
                continue;
            }
            let still_dirty = self.dirty.lock_recover().contains_key(&file.inode);
            if still_dirty {
                report
                    .skipped
                    .push(format!("{} has unsaved changes", file.path));
                continue;
            }
            let Some(expected_hash) = file.hash else {
                report
                    .skipped
                    .push(format!("{} changed since preview", file.path));
                continue;
            };
            match self
                .enqueue_canonical_eviction(
                    file.record_id,
                    expected_hash,
                    file.identity.clone(),
                    file.inode,
                )
                .await
            {
                Ok((request_id, plan)) => {
                    if self.observe_once(file.record_id, request_id).await? {
                        report.evicted_files = report.evicted_files.saturating_add(1);
                        report.freed_bytes =
                            report.freed_bytes.saturating_add(plan.reclaimable_bytes);
                    } else {
                        report.queued_files = report.queued_files.saturating_add(1);
                        self.spawn_observer(file.record_id, request_id);
                    }
                }
                Err(error) => report.skipped.push(format!("{}: {error}", file.path)),
            }
        }
        Ok(report)
    }

    async fn enqueue_canonical_eviction(
        &self,
        record_id: fs_protocol::FileRecordId,
        expected_hash: Hash,
        identity: String,
        inode: u64,
    ) -> Result<(EvictionRequestId, EvictionPlan)> {
        let namespace = Arc::clone(&self.namespace);
        let dirty = Arc::clone(&self.dirty);
        let store = Arc::clone(&self.hydrator.store);
        let coordinator = self.coordinator()?;
        let runtime = coordinator.handoff_runtime();
        coordinator
            .prepare_with_handoff(
                record_id,
                crate::eviction::EvictionMode::Safe,
                None,
                Some(expected_hash),
                move |plan, existing_request, queue, registry, catalog, now_ms| {
                    // `write` establishes dirtiness before it can publish a
                    // replacement namespace revision. Holding both guards
                    // therefore prevents either local or remote mutation from
                    // slipping between token validation and backend GC.
                    let dirty = dirty.lock_recover();
                    let mut namespace = namespace.lock_recover();
                    validate_live_eviction_token(
                        &mut namespace,
                        &dirty,
                        &identity,
                        inode,
                        record_id,
                        expected_hash,
                    )?;
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
                        crate::availability::EvictionRequirements::default(),
                    ))?;
                    Ok(request_id)
                },
            )
            .await
    }

    async fn observe_once(
        &self,
        record_id: fs_protocol::FileRecordId,
        request_id: EvictionRequestId,
    ) -> Result<bool> {
        self.coordinator()?
            .observe_once(record_id, request_id)
            .await
    }

    fn spawn_observer(&self, record_id: fs_protocol::FileRecordId, request_id: EvictionRequestId) {
        if let Ok(coordinator) = self.coordinator() {
            coordinator.spawn_observer(record_id, request_id);
        }
    }

    fn coordinator(&self) -> Result<EvictionCoordinator> {
        Ok(EvictionCoordinator::new(
            Arc::clone(&self.hydrator),
            self.availability
                .as_ref()
                .ok_or_else(|| anyhow!("durable availability registry is unavailable"))?
                .clone(),
            self.eviction_queue
                .as_ref()
                .ok_or_else(|| anyhow!("durable eviction queue is unavailable"))?
                .clone(),
            Some(
                self.history
                    .as_ref()
                    .ok_or_else(|| anyhow!("durable version history is unavailable"))?
                    .clone(),
            ),
        ))
    }
}

fn app_from_path(path: &str) -> Option<String> {
    let mut parts = path.trim_start_matches('/').split('/');
    match (parts.next(), parts.next()) {
        (Some("apps" | "app"), Some(app)) if !app.is_empty() => Some(app.to_string()),
        _ => None,
    }
}

fn has_fresh_signed_ack(record: &RecordAvailability) -> bool {
    has_fresh_signed_ack_at(record, unix_now_ms())
}

fn has_fresh_signed_ack_at(record: &RecordAvailability, now: u64) -> bool {
    let requirements = crate::availability::EvictionRequirements::default();
    let Some(version) = record.current_version else {
        return false;
    };
    record.replicas.acknowledged_version == Some(version)
        && record.freshness.replicas_observed_at_ms.is_some_and(|at| {
            at <= now && now.saturating_sub(at) <= requirements.maximum_replica_age_ms
        })
        && u32::try_from(
            record
                .replica_observations
                .iter()
                .filter(|receipt| {
                    receipt.version_id == version
                        && receipt.issued_at_ms <= now
                        && receipt.expires_at_ms >= now
                })
                .count(),
        )
        .unwrap_or(u32::MAX)
            >= requirements.minimum_durable
}

/// What one file's bytes here are, by what losing this device would cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Holding {
    /// Kept by the person's choice, and held elsewhere too.
    Pinned,
    /// Kept, and held elsewhere too.
    Replicated,
    /// Held by no other device this one knows of.
    OnlyHere,
    /// Fetched from elsewhere; dropped again when space is short.
    Cache,
}

/// What [`classify`] knows of one file on this device.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent fact from its own source, not a state machine"
)]
#[derive(Clone, Copy, Debug, Default)]
struct FileFacts {
    /// Its bytes on this device.
    local_bytes: u64,
    /// It has changes not yet saved as a version.
    unsaved: bool,
    /// The person keeps it here ("Keep offline", itself or a folder above).
    pinned: bool,
    /// The content store holds it as cache.
    cached: bool,
    /// Another device signed a receipt for its current version
    /// ([`AvailabilityRegistry::held_elsewhere`]). An expired one counts: a
    /// content id is immutable, and this only says where else the bytes are,
    /// it never permits dropping them (safe eviction asks for fresh ones).
    acknowledged: bool,
    /// Its bytes arrived from a peer while this daemon ran.
    fetched: bool,
}

/// Where a file's bytes here stand. Another device is known to hold it when
/// it signed a receipt for the current version, or when the bytes came from
/// a peer (cache, or fetched in this run); without either, and while it has
/// unsaved changes, it is only here, pinned or not, because that is what a
/// wiped device would lose. `None` when it has no bytes here.
fn classify(facts: &FileFacts) -> Option<Holding> {
    if facts.local_bytes == 0 {
        return None;
    }
    let elsewhere = facts.acknowledged || facts.cached || facts.fetched;
    Some(if facts.unsaved || !elsewhere {
        Holding::OnlyHere
    } else if facts.pinned {
        Holding::Pinned
    } else if facts.cached {
        Holding::Cache
    } else {
        Holding::Replicated
    })
}

fn add_to(split: &mut ResidencySplit, facts: &FileFacts) {
    let bytes = facts.local_bytes;
    let total = match classify(facts) {
        None => return,
        Some(Holding::Pinned) => &mut split.pinned_bytes,
        Some(Holding::Replicated) => &mut split.replicated_bytes,
        Some(Holding::Cache) => &mut split.cache_bytes,
        Some(Holding::OnlyHere) => {
            split.only_here_files = split.only_here_files.saturating_add(1);
            &mut split.only_here_bytes
        }
    };
    *total = total.saturating_add(bytes);
}

/// Whether "Keep offline" is set on the inode `ino` or a folder above it.
fn kept_by_policy(
    ns: &mut Namespace,
    ino: u64,
    records: &HashMap<fs_protocol::FileRecordId, &RecordAvailability>,
) -> bool {
    let mut cursor = Some(ino);
    // A namespace is a tree; the bound only keeps a corrupt one finite.
    for _ in 0..MAX_POLICY_DEPTH {
        let Some(current) = cursor else {
            return false;
        };
        if ns
            .record_id_for(current)
            .and_then(|id| records.get(&id))
            .is_some_and(|record| record.policy == fs_protocol::AvailabilityPolicy::Pinned)
        {
            return true;
        }
        cursor = ns.parent_ino(current);
    }
    false
}

/// Deepest folder nesting [`kept_by_policy`] walks.
const MAX_POLICY_DEPTH: usize = 4096;

/// Remove the cached entries among `keys` from the content store's file
/// area. Entries of any other retention stay.
fn drop_cached(files: &crate::store::area::FileArea, keys: &[[u8; 32]]) -> Result<CacheFreed> {
    let mut freed = CacheFreed::default();
    for key in keys {
        let Some(entry) = files.entry(key) else {
            continue;
        };
        if entry.retention != Retention::Cached {
            continue;
        }
        files.remove(key)?;
        freed.files = freed.files.saturating_add(1);
        freed.freed_bytes = freed.freed_bytes.saturating_add(entry.resident);
    }
    Ok(freed)
}

fn snapshot_candidate_ids(
    logical: &[LogicalFile],
    dirty: &HashSet<u64>,
    records: &[RecordAvailability],
    now: u64,
) -> Vec<fs_protocol::FileRecordId> {
    logical
        .iter()
        .filter(|file| {
            file.hash.is_some()
                && !dirty.contains(&file.inode)
                && records
                    .iter()
                    .find(|record| record.record_id == file.record_id)
                    .is_some_and(|record| {
                        !record.transfer.phase.is_active()
                            && !pinned_in_snapshot(records, file.record_id)
                            && has_fresh_signed_ack_at(record, now)
                    })
        })
        .map(|file| file.record_id)
        .collect()
}

/// Verify the live namespace token at the destructive authorization boundary,
/// after all catalog I/O.  Availability/history can lag namespace CRDT
/// application, so neither is a substitute for this check.
fn validate_live_eviction_token<Stage>(
    namespace: &mut Namespace,
    dirty: &HashMap<u64, Stage>,
    identity: &str,
    inode: u64,
    record_id: fs_protocol::FileRecordId,
    expected_hash: Hash,
) -> Result<()> {
    if dirty.contains_key(&inode) {
        anyhow::bail!("workspace changed during eviction authorization");
    }
    let matches = namespace.inode_for_node(identity) == Some(inode)
        && namespace.record_id_for(inode) == Some(record_id)
        && namespace
            .entry_for_node(identity)
            .is_some_and(|entry| entry.hash == Some(expected_hash));
    if !matches {
        anyhow::bail!("workspace changed during eviction authorization");
    }
    Ok(())
}

fn aggregate<'a>(
    items: impl Iterator<Item = (&'a String, &'a SyncedFileState)>,
) -> Vec<StorageAggregate> {
    let mut out = HashMap::<String, StorageAggregate>::new();
    for (owner, file) in items {
        let item = out
            .entry(owner.clone())
            .or_insert_with(|| StorageAggregate {
                owner: owner.clone(),
                ..StorageAggregate::default()
            });
        item.logical_bytes = item.logical_bytes.saturating_add(file.logical_bytes);
        item.local_bytes = item.local_bytes.saturating_add(file.hydrated_bytes);
        item.remote_only_bytes = item
            .remote_only_bytes
            .saturating_add(file.logical_bytes.saturating_sub(file.hydrated_bytes));
        item.reclaimable_bytes = item
            .reclaimable_bytes
            .saturating_add(file.reclaimable_bytes);
        item.files = item.files.saturating_add(1);
    }
    let mut out = out.into_values().collect::<Vec<_>>();
    out.sort_by(|a, b| a.owner.cmp(&b.owner));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::ROOT_INO;

    #[tokio::test]
    async fn storage_frames_are_little_endian_length_then_postcard() {
        let (mut client, mut server) = tokio::net::UnixStream::pair().unwrap();
        let mut wire = Vec::new();
        STORAGE_FRAMING
            .write_value_async(&mut wire, &StorageResponse::Error("no".into()))
            .await
            .unwrap();
        assert_eq!(wire, [4, 0, 0, 0, 2, 2, b'n', b'o']);

        tokio::io::AsyncWriteExt::write_all(&mut client, &[1, 0, 0, 0, 0])
            .await
            .unwrap();
        drop(client);
        let request: Option<StorageRequest> =
            STORAGE_FRAMING.read_value_async(&mut server).await.unwrap();
        assert!(matches!(request, Some(StorageRequest::Snapshot)));
        let closed: Option<StorageRequest> =
            STORAGE_FRAMING.read_value_async(&mut server).await.unwrap();
        assert!(closed.is_none());
    }

    /// Settings' client sets and reads the cache policy through the socket,
    /// and the daemon applies it to the predictive worker and the cache.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_cache_policy_is_set_and_read_through_the_socket() {
        use runtime_fs::storage::{CachePolicy, MIN_CACHE_BUDGET_BYTES, StorageClient};
        let dir = tempfile::tempdir().unwrap();
        let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::Minimal)
            .await
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(
            endpoint.clone(),
            Arc::new(crate::store::ContentStore::memory()),
        ));
        let heuristics = Arc::new(crate::smart::Heuristics::new(Arc::clone(&hydrator)));
        let (policies, loaded) = crate::cache_policy::CachePolicies::load(
            Some(dir.path().join(crate::cache_policy::CACHE_POLICY_FILE)),
            Arc::clone(&heuristics),
            Arc::clone(hydrator.store.files()),
        );
        loaded.unwrap();
        let authority = StorageAuthority::new(
            Arc::new(Mutex::new(Namespace::new("storage").unwrap())),
            Arc::clone(&hydrator),
            Arc::new(Mutex::new(HashMap::new())),
            None,
            None,
            None,
            "work",
        )
        .with_cache_policy(Arc::new(policies));
        let socket = dir.path().join("storage.sock");
        let server = tokio::spawn(serve(
            tokio::net::UnixListener::bind(&socket).unwrap(),
            authority,
        ));

        let wanted = CachePolicy {
            prefetch: false,
            budget_bytes: Some(MIN_CACHE_BUDGET_BYTES * 4),
        };
        let (applied, read, refused) = tokio::task::spawn_blocking(move || {
            let mut client = StorageClient::connect(&socket).unwrap();
            let applied = client.set_cache_policy(wanted).unwrap();
            let read = client.cache_policy().unwrap();
            let refused = client.set_cache_policy(CachePolicy {
                prefetch: true,
                budget_bytes: Some(1),
            });
            (applied, read, refused)
        })
        .await
        .unwrap();
        assert_eq!((applied, read), (wanted, wanted));
        assert!(refused.is_err(), "a budget below the minimum is refused");
        assert!(!heuristics.prefetch_enabled());
        assert_eq!(hydrator.store.files().budget(), MIN_CACHE_BUDGET_BYTES * 4);

        server.abort();
        endpoint.close().await;
    }

    #[tokio::test]
    async fn storage_frames_over_sixteen_mebibytes_are_refused() {
        let oversized = (16_u32 * 1024 * 1024 + 1).to_le_bytes();
        assert!(matches!(
            STORAGE_FRAMING
                .read_value_async::<_, StorageRequest>(&mut &oversized[..])
                .await,
            Err(wire_frame::FrameError::TooLarge(_))
        ));
        let truncated = [2, 0, 0, 0, 1];
        assert!(matches!(
            STORAGE_FRAMING
                .read_value_async::<_, StorageRequest>(&mut &truncated[..])
                .await,
            Err(wire_frame::FrameError::Truncated)
        ));
    }

    /// Bytes another device is known to hold are kept, cache or pinned;
    /// the rest, and anything unsaved, are only here, pinned or not.
    #[test]
    fn local_bytes_are_split_by_what_losing_the_device_costs() {
        let held = |facts: FileFacts| FileFacts {
            local_bytes: 10,
            ..facts
        };
        let cases = [
            (FileFacts::default(), None),
            (held(FileFacts::default()), Some(Holding::OnlyHere)),
            (
                held(FileFacts {
                    pinned: true,
                    ..FileFacts::default()
                }),
                Some(Holding::OnlyHere),
            ),
            (
                held(FileFacts {
                    acknowledged: true,
                    ..FileFacts::default()
                }),
                Some(Holding::Replicated),
            ),
            (
                held(FileFacts {
                    fetched: true,
                    ..FileFacts::default()
                }),
                Some(Holding::Replicated),
            ),
            (
                held(FileFacts {
                    cached: true,
                    ..FileFacts::default()
                }),
                Some(Holding::Cache),
            ),
            (
                held(FileFacts {
                    cached: true,
                    pinned: true,
                    ..FileFacts::default()
                }),
                Some(Holding::Pinned),
            ),
            (
                held(FileFacts {
                    acknowledged: true,
                    pinned: true,
                    ..FileFacts::default()
                }),
                Some(Holding::Pinned),
            ),
            (
                held(FileFacts {
                    acknowledged: true,
                    cached: true,
                    pinned: true,
                    unsaved: true,
                    ..FileFacts::default()
                }),
                Some(Holding::OnlyHere),
            ),
        ];
        let mut split = ResidencySplit::default();
        for (facts, holding) in cases {
            assert_eq!(classify(&facts), holding, "{facts:?}");
            add_to(&mut split, &facts);
        }
        assert_eq!(
            split,
            ResidencySplit {
                pinned_bytes: 20,
                replicated_bytes: 20,
                only_here_bytes: 30,
                only_here_files: 3,
                cache_bytes: 10,
            }
        );
        assert_eq!(split.total_bytes(), 80);
    }

    /// Freeing the cache removes cached entries and nothing kept.
    #[test]
    fn freeing_the_cache_drops_only_cached_entries() {
        let dir = tempfile::tempdir().unwrap();
        let area = crate::store::area::FileArea::open(dir.path(), u64::MAX).unwrap();
        let (cached, local, pinned) = ([1; 32], [2; 32], [3; 32]);
        area.adopt_bytes(&cached, &[7; 100], Retention::Cached, &[])
            .unwrap();
        area.adopt_bytes(&local, &[7; 200], Retention::LocalOnly, &[])
            .unwrap();
        area.adopt_bytes(&pinned, &[7; 300], Retention::Pinned, &[])
            .unwrap();
        let freed = drop_cached(&area, &[cached, local, pinned, [4; 32]]).unwrap();
        assert_eq!(
            freed,
            CacheFreed {
                files: 1,
                freed_bytes: 100
            }
        );
        assert!(area.entry(&cached).is_none());
        assert!(area.entry(&local).is_some() && area.entry(&pinned).is_some());
    }

    /// Through the storage socket's authority: a workspace's residency, its
    /// cache freed, and the whole of it kept here, each refused for a
    /// workspace this device does not mount.
    #[tokio::test]
    async fn the_authority_reports_frees_and_keeps_a_workspace() {
        use iroh::endpoint::{Endpoint, presets};
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(crate::ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), Arc::clone(&store)));
        let namespace = Arc::new(Mutex::new(Namespace::new("residency").unwrap()));
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        // Three large files, stored the way an authored write stores them;
        // one is then cache, as if it had been fetched, and a host signs a
        // receipt for another.
        let size = crate::content::FILE_BACKED_MIN + 1;
        let mut hashes = Vec::new();
        let mut inodes = Vec::new();
        for (name, byte) in [
            ("authored.bin", 1_u8),
            ("fetched.bin", 2),
            ("backed-up.bin", 3),
        ] {
            let hash = store
                .write_content(&vec![byte; usize::try_from(size).unwrap()])
                .await
                .unwrap()
                .unwrap();
            inodes.push(
                namespace
                    .lock()
                    .unwrap()
                    .create_file(ROOT_INO, name, Some(&hash), size, 1)
                    .unwrap(),
            );
            hashes.push(hash);
        }
        store
            .files()
            .set_retention(hashes[1].as_bytes(), Retention::Cached)
            .unwrap();
        let backed_up = namespace.lock().unwrap().record_id_for(inodes[2]).unwrap();
        let version = fs_protocol::FileVersionId::new();
        let mut record = RecordAvailability::new(backed_up, RecordKind::File);
        record.current_version = Some(version);
        crate::availability::test_receipts::acknowledge(
            &mut record,
            version,
            fs_protocol::ContentHash(*hashes[2].as_bytes()),
            unix_now_ms(),
        );
        registry.lock().unwrap().put(record).unwrap();
        let authority = StorageAuthority::new(
            namespace,
            hydrator,
            Arc::new(Mutex::new(crate::fs::WriteStages::new())),
            Some(registry),
            None,
            None,
            "notes",
        );

        let summary = authority.residency().await;
        let [workspace] = summary.workspaces.as_slice() else {
            panic!("one workspace: {summary:?}");
        };
        assert_eq!(workspace.workspace, "notes");
        assert!(!workspace.kept_here);
        assert_eq!(workspace.split.only_here_files, 1);
        assert!(workspace.split.only_here_bytes >= size);
        assert!(workspace.split.cache_bytes >= size);
        assert!(
            (size..2 * size).contains(&workspace.split.replicated_bytes),
            "a receipt for the current version is another device holding it: {workspace:?}"
        );
        assert_eq!(workspace.split.pinned_bytes, 0);

        assert!(authority.keep_here("elsewhere", true).is_err());
        authority.keep_here("notes", true).unwrap();
        let summary = authority.residency().await;
        let workspace = &summary.workspaces[0];
        assert!(workspace.kept_here);
        assert!(
            workspace.split.pinned_bytes >= 2 * size
                && workspace.split.cache_bytes == 0
                && workspace.split.replicated_bytes == 0,
            "kept content another device holds is pinned: {workspace:?}"
        );
        assert_eq!(
            workspace.split.only_here_files, 1,
            "keeping it here does not make authored content safe"
        );
        authority.keep_here("notes", false).unwrap();

        assert!(authority.free_cache("elsewhere").await.is_err());
        let freed = authority.free_cache("notes").await.unwrap();
        assert_eq!(freed.files, 1);
        assert!(freed.freed_bytes >= size);
        assert!(store.files().entry(hashes[1].as_bytes()).is_none());
        assert!(
            store.files().entry(hashes[0].as_bytes()).is_some(),
            "authored content is never cache"
        );
        let summary = authority.residency().await;
        assert_eq!(summary.workspaces[0].split.cache_bytes, 0);
        assert!(!summary.workspaces[0].kept_here);

        endpoint.close().await;
    }

    #[test]
    fn storage_attribution_and_candidates_fail_closed() {
        // Phase `only_explicit_app_namespaces_are_attributed`.
        {
            assert_eq!(
                app_from_path("/apps/org.tilo.Notes/a"),
                Some("org.tilo.Notes".into())
            );
            assert_eq!(app_from_path("/personal/notes"), None);
            assert_eq!(app_from_path("/apps"), None);
        }
        // Phase `broken_or_cyclic_policy_ancestry_is_pinned_fail_closed`.
        {
            let record = fs_protocol::FileRecordId::new();
            let missing_parent = fs_protocol::FileRecordId::new();
            let mut row = RecordAvailability::new(record, crate::availability::RecordKind::File);
            row.parent = Some(missing_parent);
            assert!(pinned_in_snapshot(&[row], record));
        }
        // Phase `snapshot_candidates_skip_rows_that_cannot_be_reclaimed`.
        {
            let record_id = fs_protocol::FileRecordId::new();
            let logical = vec![LogicalFile {
                identity: "candidate".into(),
                inode: 7,
                path: "/candidate".into(),
                logical_bytes: 1,
                hash: Some(Hash::from([1; 32])),
                record_id,
                mtime: 0,
            }];
            let mut dirty = HashSet::new();
            dirty.insert(7);
            let mut record =
                RecordAvailability::new(record_id, crate::availability::RecordKind::File);
            record.current_version = Some(fs_protocol::FileVersionId::new());
            assert!(snapshot_candidate_ids(&logical, &dirty, &[record], unix_now_ms()).is_empty());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn reclamation_tokens_and_leases_block_racing_mutation() {
        // Phase `snapshot_selection_carries_all_race_tokens`.
        {
            let file = SyncedFileState {
                workspace: "work".into(),
                app: None,
                logical_path: "/a".into(),
                identity: "stable-node".into(),
                inode: 9,
                version: "version-a".into(),
                logical_bytes: 1,
                hydrated_bytes: 1,
                remote_only: false,
                dirty: false,
                pinned: false,
                in_flight: false,
                remotely_durable: true,
                reclaimable_bytes: 1,
            };
            let selection = EvictionSelection::from(&file);
            assert_eq!(selection.identity, file.identity);
            assert_eq!(selection.inode, file.inode);
            assert_eq!(selection.version, file.version);
        }
        // Phase `final_live_token_rejects_a_namespace_or_dirty_change`.
        {
            let namespace = Arc::new(Mutex::new(Namespace::new("storage-token").unwrap()));
            let dirty = Arc::new(Mutex::new(HashMap::<u64, Vec<u8>>::new()));
            let expected = Hash::from([7; 32]);
            let inode = namespace
                .lock()
                .unwrap()
                .create_file(ROOT_INO, "report.txt", Some(&expected), 1, 1)
                .unwrap();
            let (identity, record_id) = {
                let namespace = namespace.lock().unwrap();
                (
                    namespace.id_for_ino(inode).unwrap().to_owned(),
                    namespace.record_id_for(inode).unwrap(),
                )
            };
            assert!(
                validate_live_eviction_token(
                    &mut namespace.lock().unwrap(),
                    &dirty.lock().unwrap(),
                    &identity,
                    inode,
                    record_id,
                    expected,
                )
                .is_ok()
            );

            let replacement = Hash::from([8; 32]);
            assert!(
                namespace
                    .lock()
                    .unwrap()
                    .set_content(inode, Some(&replacement), 1, 2)
            );
            assert!(
                validate_live_eviction_token(
                    &mut namespace.lock().unwrap(),
                    &dirty.lock().unwrap(),
                    &identity,
                    inode,
                    record_id,
                    expected,
                )
                .is_err()
            );

            assert!(
                namespace
                    .lock()
                    .unwrap()
                    .set_content(inode, Some(&expected), 1, 3)
            );
            dirty.lock().unwrap().insert(inode, Vec::new());
            assert!(
                validate_live_eviction_token(
                    &mut namespace.lock().unwrap(),
                    &dirty.lock().unwrap(),
                    &identity,
                    inode,
                    record_id,
                    expected,
                )
                .is_err()
            );
        }
        // Phase `live_authorization_lease_blocks_mutation_after_validation`.
        {
            let namespace = Arc::new(Mutex::new(Namespace::new("storage-lease").unwrap()));
            let dirty = Arc::new(Mutex::new(HashMap::<u64, Vec<u8>>::new()));
            let expected = Hash::from([9; 32]);
            let inode = namespace
                .lock()
                .unwrap()
                .create_file(ROOT_INO, "lease.txt", Some(&expected), 1, 1)
                .unwrap();
            let (identity, record_id) = {
                let namespace = namespace.lock().unwrap();
                (
                    namespace.id_for_ino(inode).unwrap().to_owned(),
                    namespace.record_id_for(inode).unwrap(),
                )
            };

            // This is the exact lease held by `prepare_with_handoff` while it
            // persists the request and performs backend tag release.
            let dirty_lease = dirty.lock().unwrap();
            let mut namespace_lease = namespace.lock().unwrap();
            assert!(
                validate_live_eviction_token(
                    &mut namespace_lease,
                    &dirty_lease,
                    &identity,
                    inode,
                    record_id,
                    expected,
                )
                .is_ok()
            );
            assert!(namespace.try_lock().is_err());
            assert!(dirty.try_lock().is_err());
            drop(namespace_lease);
            drop(dirty_lease);
            assert!(namespace.try_lock().is_ok());
            assert!(dirty.try_lock().is_ok());
        }
    }
}

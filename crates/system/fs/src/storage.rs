//! Canonical sync-storage accounting and conservative local hydration eviction.
//!
//! This authority reasons over CRDT node identities and content hashes, never
//! host paths.  Consequently symlinks cannot escape the workspace namespace.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use iroh_blobs::Hash;
use runtime_fs::storage::{
    EvictionReport, EvictionSelection, STORAGE_FRAMING, StorageAggregate, StorageRequest,
    StorageResponse, SyncStorageSnapshot, SyncedFileState,
};

use crate::availability::{AvailabilityRegistry, RecordAvailability, pinned_in_snapshot};
use crate::clock::unix_now_ms;
use crate::eviction::{EvictionCoordinator, EvictionPlan, EvictionQueue, EvictionRequestId};
use crate::namespace::NodeKind;
use crate::{FileHistoryStore, Hydrator, Namespace};

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
        let mut ns = self
            .namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let dirty = self
            .dirty
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .copied()
            .collect::<HashSet<_>>();
        let records = self
            .availability
            .as_ref()
            .map_or_else(Vec::new, |registry| {
                registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .records()
                    .cloned()
                    .collect()
            });
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
            let still_dirty = self
                .dirty
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&file.inode);
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
                    let dirty = dirty
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let mut namespace = namespace
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
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

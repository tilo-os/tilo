//! Durable, headless full-workspace hydration for an always-on peer.
//!
//! The replica walks the authenticated namespace graph, not its cache
//! directory.  A ready status therefore proves that every currently live
//! manifest, every manifest chunk, and every referenced thumbnail is present
//! in the content-addressed store.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use iroh_base::EndpointAddr;
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};

use crate::content::Manifest;
use crate::{Hydrator, Namespace};

/// A full replica is deliberately bounded even for an authorised writer. The
/// namespace cap alone does not constrain an adversarial manifest's vector or
/// claimed chunk lengths.
const MAX_REPLICA_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_REPLICA_CHUNKS_PER_RECORD: usize = 100_000;
const MAX_REPLICA_CHUNK_BYTES: u32 = crate::content::MAX_CHUNK_LEN;
const MAX_REPLICA_THUMBNAIL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_REPLICA_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024 * 1024;
/// A generation lease intentionally covers only a small physical deletion
/// batch. This keeps foreground namespace/FUSE mutations responsive even when
/// a long-offline replica has accumulated a large obsolete cache.
const MAX_GC_BATCH_BLOBS: usize = 16;

/// Transferable configuration for one always-on home peer.
///
/// `ticket` and `capability_json` identify a whole workspace and its existing capability
/// policy.  Nothing is configured per file: the replica follows future
/// namespace changes automatically.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HomePeerBundle {
    pub ticket: String,
    pub cache_dir: std::path::PathBuf,
    /// Exact JSON capability list exported from the existing member. Keeping
    /// it in this one file means transferring a bundle never leaves a home
    /// peer with a host-local path back to the originating desktop.
    pub capability_json: String,
    /// Governor-signed workspace authority. `capability_json` is retained
    /// only to read legacy bundles; workers require this signed envelope.
    #[serde(default)]
    pub authority: Option<crate::workspace_authority::WorkspaceAuthorityEnvelope>,
    #[serde(default)]
    pub principals: Vec<String>,
    /// Shell/relay workspace shards backed by this filesystem workspace.
    /// A desktop may attach one filesystem tree to several collaboration
    /// rooms, so this explicit mapping is authoritative for relay binding.
    #[serde(default, with = "relay_workspace_ids_serde")]
    pub relay_workspace_ids: Vec<u64>,
}

mod relay_workspace_ids_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(ids: &[u64], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ids.iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<String>::deserialize(deserializer)?
            .into_iter()
            .map(|value| value.parse().map_err(serde::de::Error::custom))
            .collect()
    }
}

/// The one configuration file installed on a home peer.
///
/// Tilo currently has one global Files namespace and one certified filesystem
/// transport identity per device. Shell rooms are mapped through
/// [`HomePeerBundle::relay_workspace_ids`]; configuring several filesystem
/// bundles would bind competing endpoints under that same certified `NodeId`.
/// Reject that unsafe topology before any worker starts.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HomePeerSet {
    pub status_file: std::path::PathBuf,
    pub workspaces: Vec<HomePeerBundle>,
}

/// Validate the topology before binding an authenticated filesystem endpoint.
///
/// # Errors
///
/// Returns an error unless exactly one global filesystem workspace is configured.
pub fn validate_home_peer_set(bundle: &HomePeerSet) -> anyhow::Result<()> {
    anyhow::ensure!(
        bundle.workspaces.len() == 1,
        "a home peer requires exactly one global Files workspace bundle"
    );
    Ok(())
}

/// A machine-readable proof of the latest hydration pass.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FullReplicaStatus {
    /// `ready`, `syncing`, or `failed`.
    pub state: String,
    /// True only when this pass found every current content root resident.
    pub full_content_replication: bool,
    pub records: u64,
    pub manifests: u64,
    pub chunks: u64,
    pub thumbnails: u64,
    /// BLAKE3 digest of the exact durable namespace generation whose live
    /// content roots were verified by this readiness pass.
    #[serde(default)]
    pub namespace_generation: Option<String>,
    /// Authenticated filesystem endpoints that serve the complete content
    /// proof. They are published only with a ready proof, so discovery never
    /// substitutes a relay-only endpoint for the blob/sync protocols.
    #[serde(default)]
    pub filesystem_endpoints: Vec<EndpointAddr>,
    /// Milliseconds since Unix epoch, refreshed by every successful atomic
    /// status write. Consumers fail closed when this heartbeat expires.
    pub updated_at_ms: u64,
    pub error: Option<String>,
}

impl FullReplicaStatus {
    pub fn syncing() -> Self {
        Self {
            state: "syncing".into(),
            full_content_replication: false,
            records: 0,
            manifests: 0,
            chunks: 0,
            thumbnails: 0,
            namespace_generation: None,
            filesystem_endpoints: Vec::new(),
            updated_at_ms: 0,
            error: None,
        }
    }

    fn failed(mut self, error: &impl std::fmt::Display) -> Self {
        self.state = "failed".into();
        self.error = Some(error.to_string());
        self.full_content_replication = false;
        self
    }
}

/// Combine per-workspace proofs. One incomplete worker keeps the whole home
/// peer non-ready; counters saturate rather than wrapping on a large bundle.
pub fn aggregate_status(states: &[FullReplicaStatus]) -> FullReplicaStatus {
    let mut aggregate = FullReplicaStatus::syncing();
    for status in states {
        aggregate.records = aggregate.records.saturating_add(status.records);
        aggregate.manifests = aggregate.manifests.saturating_add(status.manifests);
        aggregate.chunks = aggregate.chunks.saturating_add(status.chunks);
        aggregate.thumbnails = aggregate.thumbnails.saturating_add(status.thumbnails);
    }
    aggregate.full_content_replication =
        !states.is_empty() && states.iter().all(|status| status.full_content_replication);
    aggregate.state = if aggregate.full_content_replication {
        "ready"
    } else if states.iter().any(|status| status.state == "failed") {
        "failed"
    } else {
        "syncing"
    }
    .into();
    aggregate.error = states.iter().find_map(|status| status.error.clone());
    if aggregate.full_content_replication {
        if states.len() == 1
            && let Some(status) = states.first()
        {
            aggregate
                .namespace_generation
                .clone_from(&status.namespace_generation);
        }
        for status in states {
            for endpoint in &status.filesystem_endpoints {
                if !aggregate.filesystem_endpoints.contains(endpoint) {
                    aggregate.filesystem_endpoints.push(endpoint.clone());
                }
            }
        }
        // Residency and endpoint availability are distinct facts. A complete
        // durable replica remains ready while its transport endpoint is
        // restarting; relay advertisement separately requires a fresh,
        // serving endpoint before it exposes full-content service.
    }
    aggregate
}

/// Atomically write the status consumed by `tilo-relay` and status tooling.
///
/// # Errors
///
/// Returns an error when serialization or atomic persistence fails.
pub fn save_status(path: &Path, status: &FullReplicaStatus) -> Result<()> {
    let mut status = status.clone();
    status.updated_at_ms = tilo_clock::unix_now_ms();
    let bytes = serde_json::to_vec_pretty(&status)?;
    tilo_files::native::atomic::write(path, &bytes)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// The lock a running replica holds beside its status file for its whole
/// life, so a consumer can tell the status of a live replica from one a
/// replica left behind when it exited ([`replica_is_running`]). The kernel
/// releases it however the process ends, crash included.
///
/// This replaces restamping the status every second to keep it recent
///: the replica now writes its status only when the status
/// changes, and sleeps in between.
#[derive(Debug)]
pub struct ReplicaLiveness {
    _lock: advisory_lock::FileLock,
}

impl ReplicaLiveness {
    /// Take the liveness lock for the status file at `status`.
    ///
    /// # Errors
    ///
    /// The lock file cannot be created, or another running replica holds it.
    pub fn hold(status: &Path) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = liveness_path(status);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        // Readable by the relay, which only ever takes a shared lock.
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o644)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        match advisory_lock::FileLock::try_exclusive(lock) {
            Ok(lock) => Ok(Self { _lock: lock }),
            Err(advisory_lock::TryLockError::WouldBlock) => {
                anyhow::bail!("another replica is running for {}", status.display())
            }
            Err(advisory_lock::TryLockError::Error(error)) => {
                Err(error).with_context(|| format!("lock {}", path.display()))
            }
        }
    }
}

/// Whether a running replica holds the liveness lock of the status file at
/// `status` ([`ReplicaLiveness`]). `false` when no replica ever ran, when
/// the one that did has exited, or when the lock cannot be inspected.
#[must_use]
pub fn replica_is_running(status: &Path) -> bool {
    let Ok(lock) = std::fs::File::open(liveness_path(status)) else {
        return false;
    };
    matches!(
        advisory_lock::FileLock::try_shared(lock),
        Err(advisory_lock::TryLockError::WouldBlock)
    )
}

fn liveness_path(status: &Path) -> std::path::PathBuf {
    let mut name = status.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    status.with_file_name(name)
}

/// Read the last complete status record, if any.
///
/// # Errors
///
/// Returns an error when the existing status cannot be read or decoded.
pub fn load_status(path: &Path) -> Result<Option<FullReplicaStatus>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&std::fs::read(path)?)?))
}

/// Hydrate and verify the entire current live namespace.
///
/// This is intentionally bounded by the namespace's own live-record limit;
/// accepting an unbounded adversarial document would turn a home peer into a
/// memory/disk exhaustion primitive.  A bound violation leaves the status
/// non-ready and retries on the next pass after the namespace is repaired.
pub async fn hydrate_full_workspace(
    namespace: &Arc<Mutex<Namespace>>,
    hydrator: &Hydrator,
) -> FullReplicaStatus {
    let mut status = FullReplicaStatus::syncing();
    let root_scan = namespace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .live_record_blob_roots(crate::namespace::MAX_LIVE_RECORD_BLOB_SCAN);
    let roots = match root_scan {
        Ok(roots) => roots,
        Err(error) => return status.failed(&error),
    };
    status.records = u64::try_from(roots.len()).unwrap_or(u64::MAX);

    let mut hydrated_bytes = 0u64;
    for roots in roots {
        if let Some(manifest_hash) = roots.manifest {
            status.manifests = status.manifests.saturating_add(1);
            let manifest_bytes = match hydrator
                .read_blob(
                    manifest_hash,
                    0,
                    MAX_REPLICA_MANIFEST_BYTES.saturating_add(1),
                )
                .await
            {
                Ok(bytes) => bytes,
                Err(error) => return status.failed(&error),
            };
            if u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX) > MAX_REPLICA_MANIFEST_BYTES
            {
                return status.failed(&anyhow::anyhow!("manifest exceeds replica byte budget"));
            }
            let manifest = match Manifest::decode(&manifest_bytes) {
                Ok(manifest) => manifest,
                Err(error) => return status.failed(&error),
            };
            if manifest.chunks().len() > MAX_REPLICA_CHUNKS_PER_RECORD {
                return status.failed(&anyhow::anyhow!("manifest exceeds replica chunk budget"));
            }
            for chunk in manifest.chunks().iter().copied() {
                if chunk.len == 0 || chunk.len > MAX_REPLICA_CHUNK_BYTES {
                    return status.failed(&anyhow::anyhow!("manifest has an invalid chunk length"));
                }
                hydrated_bytes = match hydrated_bytes.checked_add(u64::from(chunk.len)) {
                    Some(total) if total <= MAX_REPLICA_TOTAL_BYTES => total,
                    _ => {
                        return status
                            .failed(&anyhow::anyhow!("workspace exceeds replica byte budget"));
                    }
                };
                let chunk_hash = Hash::from(chunk.hash);
                let chunk_bytes = match hydrator
                    .read_blob(chunk_hash, 0, u64::from(chunk.len))
                    .await
                {
                    Ok(bytes) => bytes,
                    Err(error) => return status.failed(&error),
                };
                if chunk_bytes.len() != chunk.len as usize {
                    return status.failed(&anyhow::anyhow!("durable chunk length mismatch"));
                }
                // `read_blob` returning bytes is not enough proof if a future
                // store backend changes cache behavior; check the durable
                // content-addressed store explicitly.
                if !hydrator.store.has(chunk_hash).await {
                    return status
                        .failed(&anyhow::anyhow!("chunk was not retained after hydration"));
                }
                status.chunks = status.chunks.saturating_add(1);
            }
            if !hydrator.store.has(manifest_hash).await {
                return status.failed(&anyhow::anyhow!(
                    "manifest was not retained after hydration"
                ));
            }
        }
        if let Some(thumbnail_hash) = roots.thumbnail {
            let thumbnail = match hydrator
                .read_blob(
                    thumbnail_hash,
                    0,
                    MAX_REPLICA_THUMBNAIL_BYTES.saturating_add(1),
                )
                .await
            {
                Ok(bytes) => bytes,
                Err(error) => return status.failed(&error),
            };
            if u64::try_from(thumbnail.len()).unwrap_or(u64::MAX) > MAX_REPLICA_THUMBNAIL_BYTES {
                return status.failed(&anyhow::anyhow!("thumbnail exceeds replica byte budget"));
            }
            if !hydrator.store.has(thumbnail_hash).await {
                return status.failed(&anyhow::anyhow!(
                    "thumbnail was not retained after hydration"
                ));
            }
            status.thumbnails = status.thumbnails.saturating_add(1);
        }
    }
    status.state = "ready".into();
    status.full_content_replication = true;
    status
}

/// Release blobs which are no longer reachable from the current namespace.
///
/// Home-peer caches are per-workspace, so their blob store contains only this
/// workspace's retained content. The live set includes manifests, chunks and
/// thumbnails; every other owned blob is an obsolete version left by a
/// replacement, deletion or rename. Immutable history retains the signed
/// version graph and metadata, but does not pin every historical payload on a
/// bounded home server forever; an explicit retention/export feature can hold
/// selected historical bytes separately. Callers invoke this only after a
/// stable hydration pass and withdraw readiness if the pass changes
/// underneath it.
///
/// # Errors
///
/// Returns an error when namespace roots, retained manifests, blob listing, or
/// a bounded deletion batch cannot be read or updated.
pub async fn prune_obsolete_workspace_content(
    namespace: &Arc<Mutex<Namespace>>,
    hydrator: &Hydrator,
    _history: Option<&crate::FileHistoryStore>,
) -> Result<u64> {
    let (roots, generation) = {
        let mut namespace = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let roots =
            namespace.live_record_blob_roots(crate::namespace::MAX_LIVE_RECORD_BLOB_SCAN)?;
        (roots, namespace.heads())
    };
    let mut live = HashSet::new();
    for roots in roots {
        if let Some(manifest_hash) = roots.manifest {
            retain_manifest_tree(&mut live, hydrator, manifest_hash).await?;
        }
        if let Some(thumbnail_hash) = roots.thumbnail {
            live.insert(thumbnail_hash);
        }
    }
    let present = hydrator.store.store().blobs().list().hashes().await?;
    let obsolete = present
        .into_iter()
        .filter(|hash| !live.contains(hash))
        .collect::<Vec<_>>();
    if obsolete.is_empty() {
        return Ok(0);
    }

    let mut removed = 0_u64;
    for hashes in obsolete.chunks(MAX_GC_BATCH_BLOBS) {
        // Namespace roots and each physical deletion batch form one generation
        // lease: the namespace lock is held from the generation check through
        // the physical deletion. A changed generation terminates this sweep;
        // the next pass recomputes roots before releasing anything else. The
        // lease is a std::Mutex guard, which cannot be held across an await,
        // so the whole lease runs on a blocking thread that waits on the
        // store. It waits through the store's own runtime: this runtime's
        // workers may be blocked on the lease, and the eviction's timers and
        // the store's actors must not depend on them.
        let selected = hashes.iter().copied().collect::<HashSet<_>>();
        let store = Arc::clone(&hydrator.store);
        let namespace = Arc::clone(namespace);
        let runtime = store
            .runtime()
            .unwrap_or_else(tokio::runtime::Handle::current);
        let generation = generation.clone();
        let batch_removed = tokio::task::spawn_blocking(move || -> Result<Option<u64>> {
            let mut namespace = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if namespace.heads() != generation {
                return Ok(None);
            }
            let batch_removed = runtime.block_on(store.evict_blobs(&selected))?;
            // Keep the namespace generation lease through the physical
            // deletion. This also makes the lease explicit at the point
            // it is released rather than accidentally shortening it.
            let lease_held = namespace.heads() == generation;
            drop(namespace);
            Ok(lease_held.then_some(batch_removed))
        })
        .await
        .map_err(|_| anyhow::anyhow!("replica GC worker panicked"))??;
        let Some(batch_removed) = batch_removed else {
            break;
        };
        removed = removed.saturating_add(batch_removed);
        // A large stale cache must not monopolize the mount's namespace lock.
        tokio::task::yield_now().await;
    }
    Ok(removed)
}

async fn retain_manifest_tree(
    live: &mut HashSet<Hash>,
    hydrator: &Hydrator,
    manifest_hash: Hash,
) -> Result<()> {
    live.insert(manifest_hash);
    let bytes = hydrator
        .read_blob(
            manifest_hash,
            0,
            MAX_REPLICA_MANIFEST_BYTES.saturating_add(1),
        )
        .await?;
    anyhow::ensure!(
        u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= MAX_REPLICA_MANIFEST_BYTES,
        "retained manifest exceeds replica byte budget"
    );
    let manifest = Manifest::decode(&bytes)?;
    anyhow::ensure!(
        manifest.chunks().len() <= MAX_REPLICA_CHUNKS_PER_RECORD,
        "retained manifest exceeds replica chunk budget"
    );
    for chunk in manifest.chunks() {
        anyhow::ensure!(
            chunk.len > 0 && chunk.len <= MAX_REPLICA_CHUNK_BYTES,
            "retained manifest has an invalid chunk length"
        );
        live.insert(Hash::from(chunk.hash));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
    const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

    /// A replica is running exactly while it holds its liveness
    /// lock, and two replicas cannot serve one status file.
    #[test]
    fn a_replica_is_running_while_it_holds_its_liveness_lock() {
        let temp = tempfile::tempdir().unwrap();
        let status = temp.path().join("full-replica").join("status.json");
        assert!(!replica_is_running(&status), "no replica ever ran");
        let alive = ReplicaLiveness::hold(&status).unwrap();
        assert!(replica_is_running(&status));
        assert!(
            ReplicaLiveness::hold(&status).is_err(),
            "a second replica must not take over a running one's status"
        );
        drop(alive);
        assert!(!replica_is_running(&status), "the replica exited");
        let _again = ReplicaLiveness::hold(&status).unwrap();
        assert!(
            replica_is_running(&status),
            "a restarted replica is running"
        );
    }
    use crate::ContentStore;
    use crate::content::{ChunkRef, Manifest};
    use crate::namespace::ROOT_INO;
    use iroh::endpoint::{Endpoint, presets};
    use iroh::protocol::Router;
    use std::time::Duration;

    #[tokio::test]
    async fn readiness_requires_manifest_chunks_and_thumbnail() {
        let store = Arc::new(ContentStore::memory());
        let chunk = store.ingest("payload").await.unwrap();
        let manifest = Manifest::new(
            crate::content::ChunkTier::Small,
            vec![ChunkRef {
                hash: *chunk.as_bytes(),
                len: 7,
            }],
        );
        let manifest = store.ingest(manifest.encode()).await.unwrap();
        let thumbnail = store.ingest("thumb").await.unwrap();
        let mut namespace = Namespace::new("replica-test").unwrap();
        let file = namespace
            .create_file(ROOT_INO, "a", Some(&manifest), 7, 0)
            .unwrap();
        assert!(namespace.set_thumb(file, &thumbnail));
        let namespace = Arc::new(Mutex::new(namespace));
        let (endpoint, _) = crate::sync::bind_ephemeral_endpoint().await.unwrap();
        let hydrator = Hydrator::new(endpoint.clone(), store);

        let ready = hydrate_full_workspace(&namespace, &hydrator).await;
        assert_eq!(ready.state, "ready");
        assert!(ready.full_content_replication);
        assert_eq!((ready.manifests, ready.chunks, ready.thumbnails), (1, 1, 1));
        endpoint.close().await;
    }

    #[tokio::test]
    async fn readiness_rejects_an_oversized_manifest_claim_before_advertising() {
        let store = Arc::new(ContentStore::memory());
        let oversized = store
            .ingest(
                Manifest::new(
                    crate::content::ChunkTier::Small,
                    vec![ChunkRef {
                        hash: [7; 32],
                        len: MAX_REPLICA_CHUNK_BYTES.saturating_add(1),
                    }],
                )
                .encode(),
            )
            .await
            .unwrap();
        let mut namespace = Namespace::new("replica-budget").unwrap();
        namespace
            .create_file(ROOT_INO, "oversized", Some(&oversized), 1, 0)
            .unwrap();
        let namespace = Arc::new(Mutex::new(namespace));
        let (endpoint, _) = crate::sync::bind_ephemeral_endpoint().await.unwrap();
        let hydrator = Hydrator::new(endpoint.clone(), store);

        let status = hydrate_full_workspace(&namespace, &hydrator).await;
        assert_eq!(status.state, "failed");
        assert!(!status.full_content_replication);
        endpoint.close().await;
    }

    #[test]
    fn replica_topology_and_relay_ids_are_validated() {
        // Phase `aggregate_is_ready_only_when_every_workspace_is_complete`.
        {
            let mut ready = FullReplicaStatus::syncing();
            ready.state = "ready".into();
            ready.full_content_replication = true;
            ready.records = u64::MAX;
            ready.namespace_generation = Some("generation-a".into());
            let aggregate = aggregate_status(&[ready.clone(), ready.clone()]);
            assert!(aggregate.full_content_replication);
            assert_eq!(aggregate.state, "ready");
            assert_eq!(aggregate.records, u64::MAX);
            assert_eq!(aggregate.namespace_generation, None);

            let aggregate = aggregate_status(&[ready.clone()]);
            assert_eq!(
                aggregate.namespace_generation.as_deref(),
                Some("generation-a")
            );

            let aggregate = aggregate_status(&[ready, FullReplicaStatus::syncing()]);
            assert!(!aggregate.full_content_replication);
            assert_eq!(aggregate.state, "syncing");
            assert_eq!(aggregate.namespace_generation, None);
        }
        // Phase `global_files_topology_rejects_multiple_endpoint_workers`.
        {
            let workspace = HomePeerBundle {
                ticket: "not-decoded-by-topology-validation".into(),
                cache_dir: std::path::PathBuf::from("/tmp/files"),
                capability_json: "[]".into(),
                authority: None,
                principals: Vec::new(),
                relay_workspace_ids: vec![7, 8],
            };
            let one = HomePeerSet {
                status_file: std::path::PathBuf::from("/tmp/status"),
                workspaces: vec![workspace.clone()],
            };
            assert!(validate_home_peer_set(&one).is_ok());
            let many = HomePeerSet {
                status_file: one.status_file,
                workspaces: vec![workspace.clone(), workspace],
            };
            assert!(validate_home_peer_set(&many).is_err());
        }
        // Phase `relay_workspace_ids_round_trip_high_bits`.
        {
            let high = (i64::MAX as u64) + 42;
            let bundle = HomePeerSet {
                status_file: std::path::PathBuf::from("/tmp/status"),
                workspaces: vec![HomePeerBundle {
                    ticket: "opaque".into(),
                    cache_dir: std::path::PathBuf::from("/tmp/cache"),
                    capability_json: "[]".into(),
                    authority: None,
                    principals: Vec::new(),
                    relay_workspace_ids: vec![high],
                }],
            };
            let text = toml::to_string(&bundle).unwrap();
            assert!(text.contains(&format!("relay_workspace_ids = [\"{high}\"]")));
            assert_eq!(
                toml::from_str::<HomePeerSet>(&text).unwrap().workspaces[0].relay_workspace_ids,
                vec![high]
            );
        }
    }

    // The discovery address carried by a ready proof must be the endpoint
    // serving iroh-blobs, rather than a separately-running relay listener.
    // Once the owner goes away, a collaborator still reads the retained blob
    // through the address that the ready proof publishes.
    #[tokio::test]
    async fn ready_endpoint_serves_blob_after_owner_is_offline() {
        let owner_endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let owner_store = ContentStore::memory();
        let bytes = b"home peer survives owner shutdown";
        let hash = owner_store.write_content(bytes).await.unwrap().unwrap();
        let owner_router = Router::builder(owner_endpoint.clone())
            .accept(iroh_blobs::ALPN, owner_store.blobs_protocol(None))
            .spawn();

        let (nas_endpoint, _) = crate::sync::bind_ephemeral_endpoint().await.unwrap();
        let nas_store = Arc::new(ContentStore::memory());
        let nas_hydrator = Hydrator::new(nas_endpoint.clone(), Arc::clone(&nas_store));
        nas_hydrator.add_peer(owner_endpoint.addr());
        assert_eq!(
            nas_hydrator
                .read(hash, 0, bytes.len() as u64)
                .await
                .unwrap(),
            bytes
        );
        let nas_router = Router::builder(nas_endpoint.clone())
            .accept(iroh_blobs::ALPN, nas_store.blobs_protocol(None))
            .spawn();

        let mut status = FullReplicaStatus::syncing();
        status.state = "ready".into();
        status.full_content_replication = true;
        status.filesystem_endpoints.push(nas_endpoint.addr());
        let discovered = aggregate_status(&[status]);
        assert_eq!(discovered.filesystem_endpoints.len(), 1);

        owner_router.shutdown().await.unwrap();
        owner_endpoint.close().await;

        let reader_endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let reader_store = Arc::new(ContentStore::memory());
        let reader = Hydrator::new(reader_endpoint.clone(), reader_store);
        reader.add_peer(discovered.filesystem_endpoints[0].clone());
        let read = reader.read(hash, 0, bytes.len() as u64).await.unwrap();
        assert_eq!(read.as_slice(), bytes);

        nas_router.shutdown().await.unwrap();
        nas_endpoint.close().await;
        reader_endpoint.close().await;
    }

    async fn owner_manifest_source(bytes: &[u8]) -> (Endpoint, Router, Hash, Hash) {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = ContentStore::memory();
        let chunk = store.ingest(bytes.to_vec()).await.unwrap();
        let manifest = store
            .ingest(
                Manifest::new(
                    crate::content::ChunkTier::Small,
                    vec![ChunkRef {
                        hash: *chunk.as_bytes(),
                        len: u32::try_from(bytes.len()).unwrap(),
                    }],
                )
                .encode(),
            )
            .await
            .unwrap();
        let router = Router::builder(endpoint.clone())
            .accept(iroh_blobs::ALPN, store.blobs_protocol(None))
            .spawn();
        (endpoint, router, chunk, manifest)
    }

    fn verify_targeted_revocation(
        authority: &crate::workspace_authority::WorkspaceAuthority,
        workspace: uuid::Uuid,
        governor: &ed25519_dalek::SigningKey,
        collaborator: crate::IdentityKey,
    ) {
        use collab_types::{Access, CapOverride, Effect, Scope, Subject};
        authority
            .apply(
                crate::workspace_authority::WorkspaceAuthorityEnvelope::sign(
                    workspace,
                    1,
                    vec![CapOverride::new(
                        Subject::User(collab_types::Ed25519PublicKey(collaborator)),
                        Scope::FsPath("/".into()),
                        Effect::Grant,
                        Access::Read,
                    )],
                    false,
                    governor,
                )
                .unwrap(),
            )
            .unwrap();
        assert!(
            authority
                .handle()
                .permits(&collaborator, "/shared", crate::Access::Read)
        );
        authority
            .apply(
                crate::workspace_authority::WorkspaceAuthorityEnvelope::sign(
                    workspace,
                    2,
                    Vec::new(),
                    true,
                    governor,
                )
                .unwrap(),
            )
            .unwrap();
        assert!(
            !authority
                .handle()
                .permits(&collaborator, "/shared", crate::Access::Read)
        );
    }

    #[tokio::test]
    async fn two_user_owner_offline_revoke_delete_and_restart_lifecycle() {
        use crate::IdentityKey;

        let temp = tempfile::tempdir().unwrap();
        let bytes = b"owner-created bytes";
        let (owner_endpoint, owner_router, chunk, manifest) = owner_manifest_source(bytes).await;

        let nas_store = Arc::new(
            ContentStore::open_with_gc_interval(
                temp.path().join("blobs"),
                Duration::from_millis(10),
            )
            .await
            .unwrap(),
        );
        let (nas_endpoint, _) = crate::sync::bind_ephemeral_endpoint().await.unwrap();
        let nas_hydrator = Hydrator::new(nas_endpoint.clone(), nas_store.clone());
        nas_hydrator.add_peer(owner_endpoint.addr());
        let mut namespace = Namespace::new("lifecycle").unwrap();
        let file = namespace
            .create_file(
                ROOT_INO,
                "shared",
                Some(&manifest),
                u64::try_from(bytes.len()).unwrap(),
                0,
            )
            .unwrap();
        let namespace = Arc::new(Mutex::new(namespace));
        assert!(
            hydrate_full_workspace(&namespace, &nas_hydrator)
                .await
                .full_content_replication
        );

        let nas_router = Router::builder(nas_endpoint.clone())
            .accept(iroh_blobs::ALPN, nas_store.blobs_protocol(None))
            .spawn();
        owner_router.shutdown().await.unwrap();
        owner_endpoint.close().await;
        let reader_endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let reader = Hydrator::new(reader_endpoint.clone(), Arc::new(ContentStore::memory()));
        assert_eq!(
            reader
                .fetch_from(chunk, nas_endpoint.addr())
                .await
                .unwrap()
                .as_ref(),
            bytes
        );

        let governor = ed25519_dalek::SigningKey::from_bytes(&[0x71; 32]);
        let collaborator: IdentityKey = [0x72; 32];
        let workspace = uuid::Uuid::new_v4();
        let authority = crate::workspace_authority::WorkspaceAuthority::open(
            temp.path().join("authority.bin"),
            workspace,
            governor.verifying_key().to_bytes(),
            collaborator,
            Vec::new(),
        )
        .unwrap();
        verify_targeted_revocation(&authority, workspace, &governor, collaborator);

        assert!(namespace.lock().unwrap().remove(file));
        tokio::time::timeout(
            HANG_BOUND,
            prune_obsolete_workspace_content(&namespace, &nas_hydrator, None),
        )
        .await
        .expect("obsolete blob prune must be bounded")
        .unwrap();
        assert!(!nas_store.has(chunk).await);
        assert!(!nas_store.has(manifest).await);
        nas_store.store().shutdown().await.unwrap();
        drop(nas_hydrator);
        drop(nas_store);
        let restarted = ContentStore::open_with_gc_interval(
            temp.path().join("blobs"),
            Duration::from_millis(10),
        )
        .await
        .unwrap();
        assert!(!restarted.has(chunk).await);
        assert!(!restarted.has(manifest).await);
        nas_router.shutdown().await.unwrap();
        nas_endpoint.close().await;
        reader_endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn batched_gc_yields_for_namespace_mutations() {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        for index in 0..MAX_GC_BATCH_BLOBS {
            store.ingest(format!("obsolete-{index}")).await.unwrap();
        }
        let hydrator = Hydrator::new(endpoint.clone(), Arc::clone(&store));
        let namespace = Arc::new(Mutex::new(Namespace::new("gc-yield").unwrap()));
        let gc = prune_obsolete_workspace_content(&namespace, &hydrator, None);
        let mutation = async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            namespace
                .lock()
                .unwrap()
                .create_file(ROOT_INO, "arrived-during-gc", None, 0, 1)
                .unwrap();
        };
        let (gc, ()) =
            tokio::time::timeout(Duration::from_secs(6), async { tokio::join!(gc, mutation) })
                .await
                .expect("large stale cache must not monopolize namespace mutations");
        gc.unwrap();
        assert!(
            namespace
                .lock()
                .unwrap()
                .entry_at("arrived-during-gc")
                .is_some()
        );
        endpoint.close().await;
    }
}

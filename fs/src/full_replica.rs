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
    #[serde(default)]
    pub relay_workspace_ids: Vec<u64>,
}

/// The one configuration file installed on a home peer. Each workspace runs
/// independently under the same enrolled device identity; the common status
/// becomes ready only when every worker has completed its current namespace.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HomePeerSet {
    pub status_file: std::path::PathBuf,
    pub workspaces: Vec<HomePeerBundle>,
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
            filesystem_endpoints: Vec::new(),
            updated_at_ms: 0,
            error: None,
        }
    }

    fn failed(mut self, error: anyhow::Error) -> Self {
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
        for status in states {
            for endpoint in &status.filesystem_endpoints {
                if !aggregate.filesystem_endpoints.contains(endpoint) {
                    aggregate.filesystem_endpoints.push(endpoint.clone());
                }
            }
        }
        // A content-ready claim without a serving endpoint is not useful to
        // an offline collaborator and must not be advertised.
        if aggregate.filesystem_endpoints.is_empty() {
            aggregate.full_content_replication = false;
            aggregate.state = "syncing".into();
        }
    }
    aggregate
}

/// Atomically write the status consumed by `qos-relay` and status tooling.
pub fn save_status(path: &Path, status: &FullReplicaStatus) -> Result<()> {
    let mut status = status.clone();
    status.updated_at_ms = unix_now_ms();
    let bytes = serde_json::to_vec_pretty(&status)?;
    atomic_file::write(path, &bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Read the last complete status record, if any.
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
    let roots = match namespace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .live_record_blob_roots(crate::namespace::MAX_LIVE_RECORD_BLOB_SCAN)
    {
        Ok(roots) => roots,
        Err(error) => return status.failed(error.into()),
    };
    status.records = u64::try_from(roots.len()).unwrap_or(u64::MAX);

    for roots in roots {
        if let Some(manifest_hash) = roots.manifest {
            status.manifests = status.manifests.saturating_add(1);
            let manifest_bytes = match hydrator.read_blob(manifest_hash, 0, u64::MAX).await {
                Ok(bytes) => bytes,
                Err(error) => return status.failed(error),
            };
            let manifest = match Manifest::decode(&manifest_bytes) {
                Ok(manifest) => manifest,
                Err(error) => return status.failed(error.into()),
            };
            for chunk in manifest.chunks {
                let chunk_hash = Hash::from(chunk.hash);
                if let Err(error) = hydrator
                    .read_blob(chunk_hash, 0, u64::from(chunk.len))
                    .await
                {
                    return status.failed(error);
                }
                // `read_blob` returning bytes is not enough proof if a future
                // store backend changes cache behavior; check the durable
                // content-addressed store explicitly.
                if !hydrator.store.has(chunk_hash).await {
                    return status
                        .failed(anyhow::anyhow!("chunk was not retained after hydration"));
                }
                status.chunks = status.chunks.saturating_add(1);
            }
            if !hydrator.store.has(manifest_hash).await {
                return status.failed(anyhow::anyhow!("manifest was not retained after hydration"));
            }
        }
        if let Some(thumbnail_hash) = roots.thumbnail {
            if let Err(error) = hydrator.read_blob(thumbnail_hash, 0, u64::MAX).await {
                return status.failed(error);
            }
            if !hydrator.store.has(thumbnail_hash).await {
                return status.failed(anyhow::anyhow!(
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
/// replacement, deletion or rename. Callers invoke this only after a stable
/// hydration pass and withdraw readiness if the pass changes underneath it.
pub async fn prune_obsolete_workspace_content(
    namespace: &Arc<Mutex<Namespace>>,
    hydrator: &Hydrator,
) -> Result<u64> {
    let roots = namespace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .live_record_blob_roots(crate::namespace::MAX_LIVE_RECORD_BLOB_SCAN)?;
    let mut live = HashSet::new();
    for roots in roots {
        if let Some(manifest_hash) = roots.manifest {
            live.insert(manifest_hash);
            let bytes = hydrator.read_blob(manifest_hash, 0, u64::MAX).await?;
            for chunk in Manifest::decode(&bytes)?.chunks {
                live.insert(Hash::from(chunk.hash));
            }
        }
        if let Some(thumbnail_hash) = roots.thumbnail {
            live.insert(thumbnail_hash);
        }
    }
    let present = hydrator.store.store().blobs().list().hashes().await?;
    let obsolete = present
        .into_iter()
        .filter(|hash| !live.contains(hash))
        .collect::<HashSet<_>>();
    hydrator.store.evict_blobs(&obsolete).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContentStore;
    use crate::content::{ChunkRef, Manifest};
    use crate::namespace::ROOT_INO;
    use iroh::endpoint::{Endpoint, presets};
    use iroh::protocol::Router;

    #[tokio::test]
    async fn readiness_requires_manifest_chunks_and_thumbnail() {
        let store = Arc::new(ContentStore::memory());
        let chunk = store.ingest("payload").await.unwrap();
        let manifest = Manifest {
            chunks: vec![ChunkRef {
                hash: *chunk.as_bytes(),
                len: 7,
            }],
        };
        let manifest = store.ingest(manifest.encode().unwrap()).await.unwrap();
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

    #[test]
    fn aggregate_is_ready_only_when_every_workspace_is_complete() {
        let mut ready = FullReplicaStatus::syncing();
        ready.state = "ready".into();
        ready.full_content_replication = true;
        ready.records = u64::MAX;
        let aggregate = aggregate_status(&[ready.clone(), ready.clone()]);
        assert!(aggregate.full_content_replication);
        assert_eq!(aggregate.state, "ready");
        assert_eq!(aggregate.records, u64::MAX);

        let aggregate = aggregate_status(&[ready, FullReplicaStatus::syncing()]);
        assert!(!aggregate.full_content_replication);
        assert_eq!(aggregate.state, "syncing");
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
        let read = reader
            .fetch_from(hash, discovered.filesystem_endpoints[0].clone())
            .await
            .unwrap();
        assert_eq!(read.as_ref(), bytes);

        nas_router.shutdown().await.unwrap();
        nas_endpoint.close().await;
        reader_endpoint.close().await;
    }
}

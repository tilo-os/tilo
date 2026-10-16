//! Authorising blob requests from an index instead of a workspace scan.
//!
//! Possessing a hash never grants access: a blob is served only while a
//! live node the requester may read names it, as its file content (a
//! manifest, or a small file's only blob), its thumbnail, or a chunk of a
//! readable manifest. The rule is unchanged; what changes is its cost.
//!
//! - The live `root → owners` map is rebuilt only when the namespace
//!   changes, detected by comparing Automerge heads: an unchanged namespace
//!   costs one head comparison per request, and the index answers exactly
//!   what a fresh scan would.
//! - Owners carry their path and record, so path grants (the live ACL) and
//!   record grants (the live access registry) are still evaluated per
//!   request: a revocation applies to the next request.
//! - A chunk is authorised through a manifest named in the same request
//!   (the batch client sends the manifests of its chunks with empty
//!   ranges), so one lookup covers a whole batch. Requests without that
//!   context, from older peers, fall back to searching the readable
//!   manifests, whose decoded chunk sets are cached.
//! - No namespace lock is held while decoding or searching, and requests
//!   are checked concurrently.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use fs_protocol::FileRecordId;
use iroh_blobs::Hash;

use crate::acl::IdentityKey;
use crate::namespace::{LiveBlobOwner, Namespace};
use crate::store::ContentStore;
use crate::sync::PeerPolicy;
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

/// Chunk entries held by the decoded-manifest cache (about 40 bytes each).
const MANIFEST_CHUNK_BUDGET: u64 = 2 * 1024 * 1024;
/// Largest manifest decoded for authorisation (about 116k chunks, a
/// multi-gigabyte file).
const MAX_AUTH_MANIFEST_BYTES: usize = 8 * 1024 * 1024;

struct Snapshot {
    heads: Vec<automerge::ChangeHash>,
    /// Manifest and thumbnail hashes to the live nodes naming them.
    roots: HashMap<Hash, Vec<Owner>>,
}

#[derive(Clone)]
struct Owner {
    path: String,
    record_id: Option<FileRecordId>,
    /// Whether the owner names this hash as file content (a manifest)
    /// rather than as a thumbnail; only manifests authorise chunks.
    manifest: bool,
}

/// Who is asking, as the blob provider knows them on one connection.
pub(crate) struct Requester<'a> {
    pub policy: &'a PeerPolicy,
    pub device: IdentityKey,
    pub person: Option<IdentityKey>,
    /// Records the device holds a live record grant for, when the
    /// workspace serves record-scoped shares.
    pub readable_records: Option<BTreeSet<FileRecordId>>,
}

impl Requester<'_> {
    fn may_read(&self, owner: &Owner) -> bool {
        let by_path = self.policy.as_ref().is_some_and(|acl| {
            acl.permits_peer(&self.device, &owner.path, crate::acl::Access::Read)
                || self.person.is_some_and(|person| {
                    acl.permits_peer(&person, &owner.path, crate::acl::Access::Read)
                })
        });
        by_path
            || owner.record_id.is_some_and(|record| {
                self.readable_records
                    .as_ref()
                    .is_some_and(|readable| readable.contains(&record))
            })
    }
}

/// The blob provider's authorisation index for one workspace.
pub(crate) struct BlobAccessIndex {
    namespace: Arc<Mutex<Namespace>>,
    snapshot: RwLock<Option<Arc<Snapshot>>>,
    rebuild: tokio::sync::Mutex<()>,
    manifests: moka::sync::Cache<Hash, Arc<HashSet<Hash>>>,
}

impl BlobAccessIndex {
    pub(crate) fn new(namespace: Arc<Mutex<Namespace>>) -> Self {
        Self {
            namespace,
            snapshot: RwLock::new(None),
            rebuild: tokio::sync::Mutex::new(()),
            manifests: moka::sync::Cache::builder()
                .max_capacity(MANIFEST_CHUNK_BUDGET)
                .weigher(|_, chunks: &Arc<HashSet<Hash>>| {
                    u32::try_from(chunks.len().max(1)).unwrap_or(u32::MAX)
                })
                .build(),
        }
    }

    fn heads(&self) -> Vec<automerge::ChangeHash> {
        self.namespace.lock_recover().heads()
    }

    fn cached(&self, heads: &[automerge::ChangeHash]) -> Option<Arc<Snapshot>> {
        self.snapshot
            .read_recover()
            .as_ref()
            .filter(|snapshot| snapshot.heads == heads)
            .cloned()
    }

    /// The index for the namespace as it is now.
    async fn current(&self) -> Arc<Snapshot> {
        if let Some(snapshot) = self.cached(&self.heads()) {
            return snapshot;
        }
        // One rebuild at a time; concurrent requests wait for it.
        let _rebuild = self.rebuild.lock().await;
        let (heads, owners) = {
            let mut namespace = self.namespace.lock_recover();
            let heads = namespace.heads();
            if let Some(snapshot) = self.cached(&heads) {
                return snapshot;
            }
            (heads, namespace.live_blob_owners())
        };
        let snapshot = Arc::new(Snapshot {
            heads,
            roots: index_owners(owners),
        });
        *self.snapshot.write_recover() = Some(Arc::clone(&snapshot));
        snapshot
    }

    /// The chunk set of a locally stored manifest, decoded once.
    async fn manifest_chunks(
        &self,
        store: &ContentStore,
        manifest: Hash,
    ) -> Option<Arc<HashSet<Hash>>> {
        if let Some(chunks) = self.manifests.get(&manifest) {
            return Some(chunks);
        }
        let bytes = store.local(manifest).await?;
        if bytes.len() > MAX_AUTH_MANIFEST_BYTES {
            return None;
        }
        let decoded = crate::content::Manifest::decode(&bytes).ok()?;
        let chunks: Arc<HashSet<Hash>> = Arc::new(
            decoded
                .chunks()
                .iter()
                .map(|chunk| Hash::from(chunk.hash))
                .collect(),
        );
        self.manifests.insert(manifest, Arc::clone(&chunks));
        Some(chunks)
    }

    /// Whether `requester` may receive the parts of the content `root` a
    /// `GET` names: `children` are the non-empty positions of its
    /// `RangeSpecSeq`. The root must be a readable file version (a
    /// manifest); its chunks are then readable with it, and a request for
    /// any position that is not a chunk (the header or length entries of
    /// the manifest) is refused.
    pub(crate) async fn authorize_manifest_children(
        &self,
        store: &ContentStore,
        requester: &Requester<'_>,
        root: Hash,
        children: &[u64],
    ) -> bool {
        let snapshot = self.current().await;
        let readable = snapshot.roots.get(&root).is_some_and(|owners| {
            owners
                .iter()
                .any(|owner| owner.manifest && requester.may_read(owner))
        });
        if !readable {
            return false;
        }
        let Some(bytes) = store.local(root).await else {
            return false;
        };
        if bytes.len() > MAX_AUTH_MANIFEST_BYTES {
            return false;
        }
        let Ok(manifest) = crate::content::Manifest::decode(&bytes) else {
            return false;
        };
        children
            .iter()
            .all(|offset| *offset == 0 || manifest.chunk_of_child(*offset).is_some())
    }

    /// Whether `requester` may receive every blob in `wanted`. `context`
    /// lists further hashes named in the same request (with nothing asked
    /// of them): readable manifests among them authorise their chunks.
    pub(crate) async fn authorize(
        &self,
        store: &ContentStore,
        requester: &Requester<'_>,
        wanted: &[Hash],
        context: &[Hash],
    ) -> bool {
        if wanted.is_empty() {
            return true;
        }
        let snapshot = self.current().await;
        let readable_root = |hash: &Hash| {
            snapshot
                .roots
                .get(hash)
                .is_some_and(|owners| owners.iter().any(|owner| requester.may_read(owner)))
        };
        let readable_manifest = |hash: &Hash| {
            snapshot.roots.get(hash).is_some_and(|owners| {
                owners
                    .iter()
                    .any(|owner| owner.manifest && requester.may_read(owner))
            })
        };
        let mut context_manifests = Vec::new();
        for hash in context.iter().chain(wanted) {
            if readable_manifest(hash) && !context_manifests.contains(hash) {
                context_manifests.push(*hash);
            }
        }
        let mut pending: Vec<Hash> = wanted
            .iter()
            .copied()
            .filter(|hash| !readable_root(hash))
            .collect();
        if pending.is_empty() {
            return true;
        }
        for manifest in context_manifests {
            if let Some(chunks) = self.manifest_chunks(store, manifest).await {
                pending.retain(|hash| !chunks.contains(hash));
                if pending.is_empty() {
                    return true;
                }
            }
        }
        // No context named the remaining chunks: search every readable
        // manifest, as a request from a peer without batch context needs.
        let mut searched = HashSet::new();
        for (hash, owners) in &snapshot.roots {
            if pending.is_empty() {
                break;
            }
            if searched.insert(*hash)
                && owners
                    .iter()
                    .any(|owner| owner.manifest && requester.may_read(owner))
                && let Some(chunks) = self.manifest_chunks(store, *hash).await
            {
                pending.retain(|chunk| !chunks.contains(chunk));
            }
        }
        pending.is_empty()
    }
}

fn index_owners(owners: Vec<LiveBlobOwner>) -> HashMap<Hash, Vec<Owner>> {
    let mut roots: HashMap<Hash, Vec<Owner>> = HashMap::with_capacity(owners.len());
    for owner in owners {
        if let Some(manifest) = owner.manifest {
            roots.entry(manifest).or_default().push(Owner {
                path: owner.path.clone(),
                record_id: owner.record_id,
                manifest: true,
            });
        }
        if let Some(thumbnail) = owner.thumbnail {
            roots.entry(thumbnail).or_default().push(Owner {
                path: owner.path,
                record_id: owner.record_id,
                manifest: false,
            });
        }
    }
    roots
}

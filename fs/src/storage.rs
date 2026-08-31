//! Canonical sync-storage accounting and conservative local hydration eviction.
//!
//! This authority reasons over CRDT node identities and content hashes, never
//! host paths.  Consequently symlinks cannot escape the workspace namespace.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Result, anyhow};
use iroh_blobs::Hash;
use runtime_fs::storage::{
    EvictionReport, EvictionSelection, StorageAggregate, StorageRequest, StorageResponse,
    SyncStorageSnapshot, SyncedFileState,
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::namespace::NodeKind;
use crate::{ContentStore, Hydrator, Namespace};

#[derive(Clone)]
pub struct StorageAuthority {
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    store: Arc<ContentStore>,
    dirty: Arc<Mutex<HashMap<u64, Vec<u8>>>>,
    pinned: Arc<RwLock<HashSet<String>>>,
    eviction: Arc<tokio::sync::Mutex<()>>,
    workspace: String,
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
            loop {
                let request: StorageRequest = match read_async_frame(&mut stream).await {
                    Ok(request) => request,
                    Err(_) => break,
                };
                let response = match request {
                    StorageRequest::Snapshot => {
                        StorageResponse::Snapshot(authority.snapshot().await)
                    }
                    StorageRequest::Evict { selected } => match authority.evict(selected).await {
                        Ok(report) => StorageResponse::Evicted(report),
                        Err(error) => StorageResponse::Error(error.to_string()),
                    },
                };
                if write_async_frame(&mut stream, &response).await.is_err() {
                    break;
                }
            }
        });
    }
}

const MAX_FRAME: usize = 16 * 1024 * 1024;

async fn read_async_frame<T: DeserializeOwned>(
    stream: &mut tokio::net::UnixStream,
) -> std::io::Result<T> {
    let len = stream.read_u32_le().await? as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::other("storage frame too large"));
    }
    let mut data = vec![0; len];
    stream.read_exact(&mut data).await?;
    postcard::from_bytes(&data).map_err(std::io::Error::other)
}

// `T: Sync` so `&T` is `Send` — required for the future this returns to be
// usable from the `tokio::spawn`ed connection task in `serve`.
async fn write_async_frame<T: Serialize + Sync>(
    stream: &mut tokio::net::UnixStream,
    value: &T,
) -> std::io::Result<()> {
    let data = postcard::to_allocvec(value).map_err(std::io::Error::other)?;
    if data.len() > MAX_FRAME {
        return Err(std::io::Error::other("storage frame too large"));
    }
    let len = u32::try_from(data.len())
        .map_err(|_| std::io::Error::other("storage frame too large to length-prefix"))?;
    stream.write_u32_le(len).await?;
    stream.write_all(&data).await
}

#[derive(Clone)]
struct FileBlobs {
    state: SyncedFileState,
    blobs: Vec<Hash>,
}

/// A live file as seen through the namespace, before residency is looked up.
struct LogicalFile {
    identity: String,
    inode: u64,
    path: String,
    logical_bytes: u64,
    hash: Option<Hash>,
    mtime: u64,
}

impl StorageAuthority {
    pub fn new(
        namespace: Arc<Mutex<Namespace>>,
        hydrator: Arc<Hydrator>,
        store: Arc<ContentStore>,
        dirty: Arc<Mutex<HashMap<u64, Vec<u8>>>>,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            namespace,
            hydrator,
            store,
            dirty,
            pinned: Arc::new(RwLock::new(HashSet::new())),
            eviction: Arc::new(tokio::sync::Mutex::new(())),
            workspace: workspace.into(),
        }
    }

    pub fn set_pinned(&self, identity: impl Into<String>, pinned: bool) {
        let identity = identity.into();
        let mut pins = self
            .pinned
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pinned {
            pins.insert(identity);
        } else {
            pins.remove(&identity);
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
                Some(LogicalFile {
                    identity,
                    inode,
                    path,
                    logical_bytes: entry.size,
                    hash: entry.hash,
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
        online: bool,
        dirty: &HashSet<u64>,
        pinned: &HashSet<String>,
    ) -> FileBlobs {
        let LogicalFile {
            identity,
            inode,
            path,
            logical_bytes,
            hash,
            mtime,
        } = file;
        let is_dirty = dirty.contains(&inode);
        let is_pinned = pinned.contains(&identity);
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
                blobs: Vec::new(),
            };
        };
        let residence = self.hydrator.residency(hash).await;
        let reclaimable = eligible(Eligibility {
            online,
            bytes: residence.hydrated_bytes,
            dirty: is_dirty,
            pinned: is_pinned,
            in_flight: residence.in_flight,
            durable: residence.remotely_durable,
        });
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
                in_flight: residence.in_flight,
                remotely_durable: residence.remotely_durable,
                reclaimable_bytes: if reclaimable {
                    residence.hydrated_bytes
                } else {
                    0
                },
            },
            blobs: residence.blobs,
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
        let pinned = self
            .pinned
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();

        let mut details = Vec::with_capacity(logical.len());
        for file in logical {
            details.push(self.file_blobs(file, online, &dirty, &pinned).await);
        }
        zero_out_shared_blob_reclaim(&mut details);

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
    /// Returns an error if the mount is offline, a selected file's version can't be parsed,
    /// the workspace changed underneath the selection during revalidation, or eviction fails
    /// against the underlying store.
    pub async fn evict(&self, selected: Vec<EvictionSelection>) -> Result<EvictionReport> {
        let _exclusive = self.eviction.lock().await;
        if self.hydrator.known_peers().is_empty() {
            return Err(anyhow!("offline: local synced content was not changed"));
        }
        let snapshot = self.snapshot().await;
        let mut report = EvictionReport::default();
        let mut selected_blobs = HashSet::new();
        let mut accepted = Vec::new();

        for request in selected {
            let Some(file) = snapshot
                .files
                .iter()
                .find(|file| matches_selection(file, &request))
            else {
                report
                    .skipped
                    .push(format!("{} changed since preview", request.identity));
                continue;
            };
            if file.reclaimable_bytes == 0 {
                report.skipped.push(format!(
                    "{} is no longer safely reclaimable",
                    file.logical_path
                ));
                continue;
            }
            let hash = parse_hash(&file.version)?;
            let residence = self.hydrator.residency(hash).await;
            let Some(peer) = residence.remote_source.clone() else {
                report.skipped.push(format!(
                    "{} has no single verified remote source",
                    file.logical_path
                ));
                continue;
            };
            // Verify every component just before eviction.  Content addressing
            // makes this a version proof, not merely a peer liveness ping.
            let mut verified = true;
            for blob in &residence.blobs {
                if self
                    .hydrator
                    .verify_remote_copy(*blob, peer.clone())
                    .await
                    .is_err()
                {
                    verified = false;
                    break;
                }
            }
            if !verified {
                report.skipped.push(format!(
                    "{} could not be verified remotely",
                    file.logical_path
                ));
                continue;
            }
            selected_blobs.extend(residence.blobs);
            accepted.push(file.clone());
        }

        // Final identity/inode/version/dirty revalidation after network I/O.
        for file in &accepted {
            let current = {
                let ns = self
                    .namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                ns.entry_for_node(&file.identity)
                    .and_then(|entry| entry.hash)
            };
            let still_dirty = self
                .dirty
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&file.inode);
            let still_pinned = self
                .pinned
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&file.identity);
            if current.map(|h| hex::encode(h.as_bytes())).as_deref() != Some(&file.version)
                || still_dirty
                || still_pinned
            {
                return Err(anyhow!(
                    "workspace changed during revalidation; nothing was evicted"
                ));
            }
        }
        self.store.evict_blobs(&selected_blobs).await?;
        for file in &accepted {
            let current = self.hydrator.residency(parse_hash(&file.version)?).await;
            report.freed_bytes = report.freed_bytes.saturating_add(
                file.reclaimable_bytes
                    .saturating_sub(current.hydrated_bytes),
            );
            if current.hydrated_bytes == 0 {
                report.evicted_files = report.evicted_files.saturating_add(1);
            }
        }
        Ok(report)
    }
}

/// Eviction-eligibility inputs, grouped so the call site names each flag
/// rather than passing an easily-transposed run of `bool`s.
#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is an independent, named eligibility condition; that's exactly what \
              this struct exists to make transposition-proof at the call site instead of a run \
              of positional bool params (see the doc comment above)"
)]
struct Eligibility {
    online: bool,
    bytes: u64,
    dirty: bool,
    pinned: bool,
    in_flight: bool,
    durable: bool,
}

fn eligible(e: Eligibility) -> bool {
    e.online && e.bytes > 0 && !e.dirty && !e.pinned && !e.in_flight && e.durable
}

/// A chunk used by any other logical file must remain local. Being
/// conservative here avoids misleading per-file reclaim estimates.
fn zero_out_shared_blob_reclaim(details: &mut [FileBlobs]) {
    let mut refs = HashMap::<Hash, usize>::new();
    for file in details.iter() {
        let mut seen = HashSet::new();
        for hash in file.blobs.iter().copied() {
            if seen.insert(hash) {
                let count = refs.entry(hash).or_default();
                *count = count.saturating_add(1);
            }
        }
    }
    for file in details.iter_mut() {
        if file
            .blobs
            .iter()
            .any(|hash| refs.get(hash).copied().unwrap_or(0) > 1)
        {
            file.state.reclaimable_bytes = 0;
        }
    }
}

fn matches_selection(file: &SyncedFileState, request: &EvictionSelection) -> bool {
    file.identity == request.identity
        && file.inode == request.inode
        && file.version == request.version
}

fn app_from_path(path: &str) -> Option<String> {
    let mut parts = path.trim_start_matches('/').split('/');
    match (parts.next(), parts.next()) {
        (Some("apps" | "app"), Some(app)) if !app.is_empty() => Some(app.to_string()),
        _ => None,
    }
}

fn parse_hash(hex_hash: &str) -> Result<Hash> {
    let raw = hex::decode(hex_hash)?;
    let array: [u8; 32] = raw
        .try_into()
        .map_err(|_| anyhow!("invalid content version"))?;
    Ok(Hash::from(array))
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

    #[test]
    fn dirty_pinned_inflight_and_offline_are_never_reclaimable() {
        let base = || Eligibility {
            online: true,
            bytes: 42,
            dirty: false,
            pinned: false,
            in_flight: false,
            durable: true,
        };
        assert!(eligible(base()));
        assert!(!eligible(Eligibility {
            online: false,
            ..base()
        }));
        assert!(!eligible(Eligibility {
            dirty: true,
            ..base()
        }));
        assert!(!eligible(Eligibility {
            pinned: true,
            ..base()
        }));
        assert!(!eligible(Eligibility {
            in_flight: true,
            ..base()
        }));
        assert!(!eligible(Eligibility {
            durable: false,
            ..base()
        }));
    }

    #[test]
    fn only_explicit_app_namespaces_are_attributed() {
        assert_eq!(
            app_from_path("/apps/org.qos.Notes/a"),
            Some("org.qos.Notes".into())
        );
        assert_eq!(app_from_path("/personal/notes"), None);
        assert_eq!(app_from_path("/apps"), None);
    }

    #[test]
    fn preview_identity_inode_and_version_are_all_race_tokens() {
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
        let mut request = EvictionSelection::from(&file);
        assert!(matches_selection(&file, &request));
        request.version = "version-b".into();
        assert!(!matches_selection(&file, &request));
        request.version = file.version.clone();
        request.inode += 1;
        assert!(!matches_selection(&file, &request));
        request.inode = file.inode;
        request.identity = "replacement-node".into();
        assert!(!matches_selection(&file, &request));
    }
}

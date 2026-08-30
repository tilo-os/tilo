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

async fn write_async_frame<T: Serialize>(
    stream: &mut tokio::net::UnixStream,
    value: &T,
) -> std::io::Result<()> {
    let data = postcard::to_allocvec(value).map_err(std::io::Error::other)?;
    if data.len() > MAX_FRAME {
        return Err(std::io::Error::other("storage frame too large"));
    }
    stream.write_u32_le(data.len() as u32).await?;
    stream.write_all(&data).await
}

#[derive(Clone)]
struct FileBlobs {
    state: SyncedFileState,
    blobs: Vec<Hash>,
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
        let mut pins = self.pinned.write().unwrap();
        if pinned {
            pins.insert(identity);
        } else {
            pins.remove(&identity);
        }
    }

    pub async fn snapshot(&self) -> SyncStorageSnapshot {
        let logical = {
            let mut ns = self.namespace.lock().unwrap();
            ns.live_node_ids()
                .into_iter()
                .filter_map(|identity| {
                    let entry = ns.entry_for_node(&identity)?;
                    if entry.kind != NodeKind::File {
                        return None;
                    }
                    let inode = ns.inode_for_node(&identity)?;
                    let path = ns.path_for(inode)?;
                    Some((identity, inode, path, entry.size, entry.hash, entry.mtime))
                })
                .collect::<Vec<_>>()
        };

        let online = !self.hydrator.known_peers().is_empty();
        let dirty = self
            .dirty
            .lock()
            .unwrap()
            .keys()
            .copied()
            .collect::<HashSet<_>>();
        let pinned = self.pinned.read().unwrap().clone();
        let mut details = Vec::new();
        for (identity, inode, path, logical_bytes, hash, mtime) in logical {
            let Some(hash) = hash else {
                let is_dirty = dirty.contains(&inode);
                let is_pinned = pinned.contains(&identity);
                details.push(FileBlobs {
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
                });
                continue;
            };
            let residence = self.hydrator.residency(hash).await;
            let app = app_from_path(&path);
            let is_dirty = dirty.contains(&inode);
            let is_pinned = pinned.contains(&identity);
            let reclaimable = eligible(
                online,
                residence.hydrated_bytes,
                is_dirty,
                is_pinned,
                residence.in_flight,
                residence.remotely_durable,
            );
            details.push(FileBlobs {
                state: SyncedFileState {
                    workspace: self.workspace.clone(),
                    app,
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
            });
        }

        // A chunk used by any other logical file must remain local.  Being
        // conservative here avoids misleading per-file reclaim estimates.
        let mut refs = HashMap::<Hash, usize>::new();
        for file in &details {
            for hash in file.blobs.iter().copied().collect::<HashSet<_>>() {
                *refs.entry(hash).or_default() += 1;
            }
        }
        for file in &mut details {
            if file
                .blobs
                .iter()
                .any(|hash| refs.get(hash).copied().unwrap_or(0) > 1)
            {
                file.state.reclaimable_bytes = 0;
            }
        }

        let files = details.into_iter().map(|f| f.state).collect::<Vec<_>>();
        SyncStorageSnapshot {
            online,
            workspaces: aggregate(files.iter().map(|f| (&f.workspace, f))),
            apps: aggregate(files.iter().filter_map(|f| f.app.as_ref().map(|a| (a, f)))),
            files,
        }
    }

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
                let ns = self.namespace.lock().unwrap();
                ns.entry_for_node(&file.identity)
                    .and_then(|entry| entry.hash)
            };
            if current.map(|h| hex::encode(h.as_bytes())).as_deref() != Some(&file.version)
                || self.dirty.lock().unwrap().contains_key(&file.inode)
                || self.pinned.read().unwrap().contains(&file.identity)
            {
                return Err(anyhow!(
                    "workspace changed during revalidation; nothing was evicted"
                ));
            }
        }
        self.store.evict_blobs(&selected_blobs).await?;
        for file in &accepted {
            let current = self.hydrator.residency(parse_hash(&file.version)?).await;
            report.freed_bytes += file
                .reclaimable_bytes
                .saturating_sub(current.hydrated_bytes);
            if current.hydrated_bytes == 0 {
                report.evicted_files += 1;
            }
        }
        Ok(report)
    }
}

fn eligible(
    online: bool,
    bytes: u64,
    dirty: bool,
    pinned: bool,
    in_flight: bool,
    durable: bool,
) -> bool {
    online && bytes > 0 && !dirty && !pinned && !in_flight && durable
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
        item.logical_bytes += file.logical_bytes;
        item.local_bytes += file.hydrated_bytes;
        item.remote_only_bytes += file.logical_bytes.saturating_sub(file.hydrated_bytes);
        item.reclaimable_bytes += file.reclaimable_bytes;
        item.files += 1;
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
        assert!(eligible(true, 42, false, false, false, true));
        assert!(!eligible(false, 42, false, false, false, true));
        assert!(!eligible(true, 42, true, false, false, true));
        assert!(!eligible(true, 42, false, true, false, true));
        assert!(!eligible(true, 42, false, false, true, true));
        assert!(!eligible(true, 42, false, false, false, false));
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

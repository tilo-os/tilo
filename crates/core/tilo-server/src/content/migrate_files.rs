//! Prepare encrypted replacements for legacy Files versions in their original workspace.
//! The new addresses become visible together through a signed Files index change.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::SystemTime;

use automerge::AutoCommit;
use automerge::transaction::CommitOptions;
use collab_types::{ContentRetention, EncryptionMetadata, WorkspaceId, file_content_artifact};
use fs_protocol::tree::{FILES_APP, chunk_document_id};
use key_group::ContentKeys;
use tilo_content::{ContentStore, WorkspaceStores};
use tilo_peer_state::document_frame::DocumentRoute;

use super::branch::{
    StoredVersion, file_versions, inspect_version, seal_version, set_file_aliases, tree_chunk,
    tree_of,
};
use crate::workspaces::Workspaces;

type Document = ((String, String), Vec<u8>);
type Versions = BTreeMap<String, StoredVersion>;

/// Limit one attempt so that a large archive makes progress without needing
/// to duplicate every file before the first index update.
const BATCH: usize = 32;

pub(super) async fn migrate(
    workspaces: &Workspaces,
    stores: &WorkspaceStores,
    workspace: &str,
) -> Result<(), String> {
    while migrate_batch(workspaces, stores, workspace).await? {}
    Ok(())
}

async fn migrate_batch(
    workspaces: &Workspaces,
    stores: &WorkspaceStores,
    workspace: &str,
) -> Result<bool, String> {
    let Some(peer) = workspaces.state(workspace).await else {
        return Ok(false);
    };
    let routes: BTreeSet<_> = peer
        .plaintext_routes()
        .into_iter()
        .chain(peer.sealed_routes())
        .filter(|(app, doc)| app == FILES_APP && tree_chunk(doc).is_some())
        .collect();
    drop(peer);
    let index = chunk_document_id(0);
    if !routes.contains(&(FILES_APP.to_owned(), index.clone())) {
        return Ok(false);
    }
    let snapshot = workspaces
        .whole_documents(workspace, Some(&routes))
        .await
        .map_err(|error| error.to_string())?;
    if !snapshot.is_whole() || !snapshot.private.is_empty() {
        return Ok(false);
    }
    let store = stores.get(workspace)?;
    let (snapshot, versions) = tokio::task::spawn_blocking({
        let store = store.clone();
        move || {
            let versions = legacy_versions(&store, &snapshot.documents)?;
            Ok::<_, String>((snapshot, versions))
        }
    })
    .await
    .map_err(|error| error.to_string())??;
    if versions.is_empty() {
        return Ok(false);
    }
    let route = DocumentRoute {
        app_id: FILES_APP.to_owned(),
        cluster_id: workspace.to_owned(),
        document: index.clone(),
    };
    let Some(keys) = workspaces.own_route_keys(&route, true).await else {
        return Ok(false);
    };
    let keys = keys?;
    let id = WorkspaceId::parse(workspace).ok_or("invalid Files workspace")?;
    let (snapshot, keys, changes) = tokio::task::spawn_blocking(move || {
        let Some((_, bytes)) = snapshot
            .documents
            .iter()
            .find(|((app, doc), _)| app == FILES_APP && doc == &index)
        else {
            return Err("the Files index did not arrive".to_owned());
        };
        let changes = rewrite_index(&store, id, &keys, versions, bytes)?;
        Ok((snapshot, keys, changes))
    })
    .await
    .map_err(|error| error.to_string())??;
    workspaces
        .publish_files_migration(workspace, &snapshot, &keys, changes)
        .await
        .map_err(|error| error.to_string())
}

/// Which plaintext versions have a durable, byte-equivalent encrypted copy.
/// A repeated address elsewhere in the workspace still keeps its own copy.
pub(super) async fn retired_sources(
    workspaces: &Workspaces,
    stores: &Arc<WorkspaceStores>,
    workspace: &str,
    documents: &[Document],
    before: SystemTime,
) -> Option<BTreeSet<String>> {
    let id = WorkspaceId::parse(workspace)?;
    let index = chunk_document_id(0);
    if !documents
        .iter()
        .any(|((app, document), _)| app == FILES_APP && document == &index)
    {
        return None;
    }
    let (store, documents, workspace) = (
        stores.get(workspace).ok()?,
        documents.to_vec(),
        workspace.to_owned(),
    );
    let stores = Arc::clone(stores);
    let (store, workspace, candidates) = tokio::task::spawn_blocking(move || {
        let tree = tree_of(&documents)?;
        let sources = file_versions(&documents).ok()?;
        let destinations: BTreeSet<String> = sources
            .iter()
            .map(|source| (source.as_str(), tree.file_hash(source)))
            .filter(|(source, replacement)| source != replacement)
            .map(|(_, replacement)| replacement.to_owned())
            .collect();
        let mut candidates: Vec<_> = sources
            .into_iter()
            .filter_map(|source| {
                let replacement = tree.file_hash(&source);
                if replacement == source
                    || destinations.contains(&source)
                    || !store.has(&source)
                    || !stores.written_before(&workspace, replacement, before)
                {
                    return None;
                }
                let replacement = replacement.to_owned();
                Some((source, replacement))
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let shared = super::collection::named_by_matching(
            &documents,
            &|(app, document)| app != FILES_APP || tree_chunk(document).is_none(),
            &|hash| store.read(hash),
        )
        .ok()?;
        candidates.retain(|(source, _)| !shared.contains(source));
        Some((store, workspace, candidates))
    })
    .await
    .ok()??;
    if candidates.is_empty() {
        return None;
    }
    let route = DocumentRoute {
        app_id: FILES_APP.to_owned(),
        cluster_id: workspace,
        document: index,
    };
    let keys = workspaces.own_route_keys(&route, false).await?.ok()?;
    tokio::task::spawn_blocking(move || {
        candidates
            .into_iter()
            .filter(|(source, replacement)| is_sealed_copy(&store, id, &keys, source, replacement))
            .map(|(source, _)| source)
            .collect()
    })
    .await
    .ok()
}

fn is_sealed_copy(
    store: &ContentStore,
    workspace: WorkspaceId,
    keys: &ContentKeys,
    source: &str,
    replacement: &str,
) -> bool {
    let Ok(Some(old)) = inspect_version(store, source) else {
        return false;
    };
    let Some(new) = inspect_version(store, replacement)
        .ok()
        .flatten()
        .and_then(|version| version.manifest)
    else {
        return false;
    };
    let Some(sealed) = new.sealed() else {
        return false;
    };
    if sealed.group != *keys.workspace().as_uuid().as_bytes()
        || old.manifest.as_ref().is_some_and(|old| {
            old.sealed().is_some()
                || old.tier() != new.tier()
                || old.chunks().len() != new.chunks().len()
        })
    {
        return false;
    }
    let metadata = EncryptionMetadata::content_group(
        workspace,
        keys.workspace(),
        sealed.epoch,
        ContentRetention::Delta,
    );
    let mut raw = blake3::Hasher::new();
    for (i, chunk) in new.chunks().iter().enumerate() {
        let hash = hex::encode(chunk.hash);
        if store.size(&hash) != Some(new.stored_len(chunk)) {
            return false;
        }
        let Some(clear) = store.read(&hash).and_then(|bytes| {
            keys.open_content(file_content_artifact(workspace), &metadata, &bytes)
                .ok()
        }) else {
            return false;
        };
        if clear.len() != chunk.len as usize {
            return false;
        }
        if let Some(old) = &old.manifest {
            let Some(part) = old.chunks().get(i) else {
                return false;
            };
            if part.len != chunk.len || &part.hash != blake3::hash(&clear).as_bytes() {
                return false;
            }
        } else {
            raw.update(&clear);
        }
    }
    old.manifest.is_some() || raw.finalize().to_hex().as_str() == source
}

fn legacy_versions(store: &ContentStore, documents: &[Document]) -> Result<Versions, String> {
    let mut versions = BTreeMap::new();
    let Some(tree) = tree_of(documents) else {
        return Ok(versions);
    };
    for original in file_versions(documents)? {
        let address = tree.file_hash(&original);
        if versions.contains_key(address) {
            continue;
        }
        let Some(version) = inspect_version(store, address)? else {
            continue;
        };
        if version.manifest.as_ref().is_some_and(|manifest| {
            manifest.sealed().is_some()
                || manifest
                    .chunks()
                    .iter()
                    .any(|part| !store.has(&hex::encode(part.hash)))
        }) {
            continue;
        }
        // The source address is the historical version. Branch aliases
        // already resolve to sealed content and are skipped above.
        if original == address {
            versions.insert(original, version);
        }
        if versions.len() == BATCH {
            break;
        }
    }
    Ok(versions)
}

fn rewrite_index(
    store: &ContentStore,
    workspace: WorkspaceId,
    keys: &ContentKeys,
    versions: Versions,
    index: &[u8],
) -> Result<Vec<u8>, String> {
    let mut doc = AutoCommit::load(index).map_err(|error| error.to_string())?;
    let heads = doc.get_heads();
    let mut aliases = BTreeMap::new();
    for (address, version) in versions {
        let sealed = seal_version(
            (store, store),
            (&address, version),
            (workspace, workspace),
            (None, keys),
        )?;
        aliases.insert(address, sealed);
    }
    set_file_aliases(&mut doc, &aliases)?;
    // The index change is the framework's storage migration of the record: no
    // person asked for it, so it names the migration and admission decides it as
    // the device that signs it.
    doc.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Migrate.change_message()),
    );
    Ok(doc.save_after(&heads))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming as _;

    #[test]
    fn a_legacy_manifest_requires_all_replacement_chunks_before_retirement() {
        let store = ContentStore::memory();
        let workspace = WorkspaceId::new();
        let keys = key_group::KeyGroup::create(
            workspace,
            key_group::MemberKey::generate("member").unwrap(),
        )
        .unwrap()
        .content_keys()
        .unwrap();
        let mut keep = BTreeSet::new();
        let parts = [b"the beginning".as_slice(), b"the rest"].map(|clear| {
            let address = store.put_public(clear).unwrap();
            let mut hash = [0; blake3::OUT_LEN];
            hex::decode_to_slice(&address, &mut hash).unwrap();
            keep.insert(address);
            fs_content::ChunkRef {
                hash,
                len: u32::try_from(clear.len()).unwrap(),
            }
        });
        let manifest = fs_content::Manifest::new(fs_content::ChunkTier::Small, parts.to_vec());
        let original = store.put_public(&manifest.encode()).unwrap();
        let version = inspect_version(&store, &original).unwrap().unwrap();
        let replacement = seal_version(
            (&store, &store),
            (&original, version),
            (workspace, workspace),
            (None, &keys),
        )
        .unwrap();
        assert!(is_sealed_copy(
            &store,
            workspace,
            &keys,
            &original,
            &replacement
        ));

        keep.extend([original.clone(), replacement.clone()]);
        store.collect_unreferenced(&keep).unwrap();
        assert!(!is_sealed_copy(
            &store,
            workspace,
            &keys,
            &original,
            &replacement
        ));
    }

    #[test]
    fn an_existing_files_index_preserves_history_when_plaintext_is_sealed() {
        let store = ContentStore::memory();
        let workspace = WorkspaceId::new();
        let keys = key_group::KeyGroup::create(
            workspace,
            key_group::MemberKey::generate("member").unwrap(),
        )
        .unwrap()
        .content_keys()
        .unwrap();
        let original = b"The previous version is still readable";
        let hash = store.put_public(original).unwrap();
        let later = b"The current version is also readable";
        let later_hash = store.put_public(later).unwrap();
        let mut namespace = tilo_fs::Namespace::new("existing Files").unwrap();
        let ino = namespace
            .create_file(
                tilo_fs::ROOT_INO,
                "report.txt",
                Some(&hash.parse().unwrap()),
                original.len() as u64,
                1,
            )
            .unwrap();
        assert!(!namespace.document_writes(&BTreeMap::new()).is_empty());
        assert!(namespace.set_content(
            ino,
            Some(&later_hash.parse().unwrap()),
            later.len() as u64,
            2
        ));
        let mut docs: Vec<Document> = namespace
            .document_writes(&BTreeMap::new())
            .into_iter()
            .map(|write| {
                let mut doc = AutoCommit::new();
                doc.load_incoming(&write.changes).unwrap();
                (
                    (FILES_APP.to_owned(), chunk_document_id(write.chunk)),
                    doc.save(),
                )
            })
            .collect();

        let versions = legacy_versions(&store, &docs).unwrap();
        assert!(versions.contains_key(&hash));
        let (_, bytes) = docs
            .iter_mut()
            .find(|((_, doc), _)| doc == &chunk_document_id(0))
            .unwrap();
        let changes = rewrite_index(&store, workspace, &keys, versions, bytes).unwrap();
        let mut doc = AutoCommit::load(bytes).unwrap();
        let before = doc.get_heads();
        doc.load_incoming(&changes).unwrap();
        // The migration's change names itself: the framework's storage
        // migration of the record, which admission decides as the device that
        // signs it. An unnamed change is refused by every receiver.
        let added = doc.get_changes(&before);
        assert_eq!(added.len(), 1);
        let names = tilo_actions::FrameworkChange::Migrate.change_message();
        assert_eq!(added[0].message(), Some(names.as_str()));
        *bytes = doc.save();

        let tree = tree_of(&docs).unwrap();
        for (address, expected) in [
            (&hash, original.as_slice()),
            (&later_hash, later.as_slice()),
        ] {
            let replacement = tree.file_hash(address);
            assert_ne!(replacement, address);
            let manifest = fs_content::Manifest::decode(&store.read(replacement).unwrap()).unwrap();
            let sealed = manifest.sealed().unwrap();
            let body = store.read(&hex::encode(manifest.chunks()[0].hash)).unwrap();
            let metadata = EncryptionMetadata::content_group(
                workspace,
                workspace,
                sealed.epoch,
                ContentRetention::Delta,
            );
            assert_eq!(
                &*keys
                    .open_content(file_content_artifact(workspace), &metadata, &body)
                    .unwrap(),
                expected,
            );
        }
        assert!(legacy_versions(&store, &docs).unwrap().is_empty());
        assert!(is_sealed_copy(
            &store,
            workspace,
            &keys,
            &hash,
            tree.file_hash(&hash),
        ));
        assert!(!is_sealed_copy(
            &store,
            workspace,
            &keys,
            &hash,
            tree.file_hash(&later_hash),
        ));
    }
}

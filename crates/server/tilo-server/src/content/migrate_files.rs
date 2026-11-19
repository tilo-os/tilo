//! Prepare encrypted replacements for legacy Files versions in their original workspace.
//! The new addresses become visible together through a signed Files index change.

use std::collections::{BTreeMap, BTreeSet};

use automerge::AutoCommit;
use collab_types::WorkspaceId;
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
    Ok(doc.save_after(&heads))
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{ContentRetention, EncryptionMetadata, file_content_artifact};

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
        let mut namespace = fs::Namespace::new("existing Files").unwrap();
        let ino = namespace
            .create_file(
                fs::ROOT_INO,
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
                doc.load_incremental(&write.changes).unwrap();
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
        doc.load_incremental(&changes).unwrap();
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
    }
}

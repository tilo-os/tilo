//! Copy the content a workspace's branch needs. Current Files payloads must be complete.
//! Older versions go with the branch when they are still stored. Sealed Files versions are
//! encrypted with the branch's keys, and the tree index maps their original addresses to the
//! branch's addresses so history can bring them back.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ROOT, ReadDoc as _};
use collab_types::{ContentRetention, EncryptionMetadata, WorkspaceId, file_content_artifact};
use fs::content::Manifest;
use fs_protocol::tree::chunk_of_record as tree_chunk;
use key_group::ContentKeys;
use tilo_content::{ContentStore, WorkspaceStores};

/// The workspaces' stores, as a branch's content is copied between them.
pub struct StoresBranches(pub Arc<WorkspaceStores>);

impl crate::workspaces::BranchContent for StoresBranches {
    fn copy(
        &self,
        source: &str,
        branch: &str,
        records: &[((String, String), Vec<u8>)],
    ) -> Result<(), String> {
        let from = self.0.get(source)?;
        let to = self.0.get(branch)?;
        // The files the tree names now are the branch's: each must be here
        // whole, or the branch is not made. A file the host is still gathering
        // from the devices that wrote it would be a name in the branch whose
        // bytes no member of it ever serves.
        let gathering = not_whole_yet(&from, records);
        if gathering > 0 {
            tracing::debug!(%source, %branch, gathering, "a branch waits for files its source's host is still gathering");
            return Err(format!(
                "{gathering} pieces of the files its tree names are not on this host yet; \
                 it is gathering them from the devices that wrote them"
            ));
        }
        let named = super::collection::named_by(records, &|address| from.read(address))?;
        let (count, mut copied, mut lacking) = (named.len(), 0_usize, 0_usize);
        for address in named {
            if to.has(&address) {
                continue;
            }
            // Named but not kept here: the source answers it no more than the branch.
            let Some(mut kept) = from.open(&address) else {
                lacking = lacking.saturating_add(1);
                continue;
            };
            copied = copied.saturating_add(1);
            let stored = to
                .put_reader(&mut kept, "application/octet-stream")
                .map_err(|error| format!("{address}: {error}"))?;
            if stored.hash != address {
                return Err(format!(
                    "{address}: the source keeps other bytes under it ({})",
                    stored.hash
                ));
            }
        }
        // How much of the source's Files the branch took, in counts alone: what
        // tells a branch made before the host held a file from one whose copy
        // lost it. A file's name is the person's, never the host's log's.
        tracing::debug!(
            %source,
            %branch,
            tree_entries = ?tree_entries(records),
            named = count,
            copied,
            not_kept_here = lacking,
            "a branch took its source's files"
        );
        Ok(())
    }

    fn has_sealed_files(
        &self,
        source: &str,
        documents: &[((String, String), Vec<u8>)],
    ) -> Result<bool, String> {
        let Some(tree) = tree_of(documents) else {
            return Ok(false);
        };
        let source = self.0.get(source)?;
        Ok(file_versions(documents)?.iter().any(|hash| {
            source
                .read(tree.file_hash(hash))
                .and_then(|bytes| Manifest::decode(&bytes).ok())
                .is_some_and(|manifest| manifest.sealed().is_some())
        }))
    }

    fn copy_rekeyed(
        &self,
        source: &str,
        branch: &str,
        documents: &mut [((String, String), Vec<u8>)],
        keys: (&ContentKeys, &ContentKeys),
    ) -> Result<(), String> {
        self.copy(source, branch, documents)?;
        let Some(tree) = tree_of(documents) else {
            return Ok(());
        };
        let from = WorkspaceId::parse(source).ok_or("invalid source workspace")?;
        let to = WorkspaceId::parse(branch).ok_or("invalid branch workspace")?;
        let store = self.0.get(branch)?;
        let current: BTreeSet<_> = crate::files_content::files_named(&tree)
            .into_iter()
            .collect();
        let mut translated = BTreeMap::new();
        let mut kept = BTreeMap::new();
        for original in file_versions(documents)? {
            let source_hash = tree.file_hash(&original);
            if !kept.contains_key(source_hash) {
                let Some(bytes) = store.read(source_hash) else {
                    continue;
                };
                let Ok(manifest) = Manifest::decode(&bytes) else {
                    continue;
                };
                if manifest.sealed().is_none()
                    || (!current.contains(source_hash)
                        && manifest
                            .chunks()
                            .iter()
                            .any(|part| !store.has(&hex::encode(part.hash))))
                {
                    continue;
                }
                let replacement = rekey(&store, &manifest, (from, to), keys)?;
                kept.insert(source_hash.to_owned(), replacement);
            }
            if let Some(replacement) = kept.get(source_hash) {
                translated.insert(original, replacement.clone());
            }
        }
        translated.extend(kept);
        for ((app, document), bytes) in documents {
            if app != fs_protocol::tree::FILES_APP {
                continue;
            }
            match tree_chunk(document) {
                Some(0) => write_file_aliases(bytes, &translated)?,
                Some(_) => replace_file_hashes(bytes, &translated)?,
                None => {}
            }
        }
        Ok(())
    }

    fn discard(&self, branch: &str) -> Result<(), String> {
        self.0.remove(branch)
    }
}

/// Every version address ever entered in a chunk of the Files tree.
fn file_versions(documents: &[((String, String), Vec<u8>)]) -> Result<BTreeSet<String>, String> {
    use automerge::legacy::{Key, OpType};
    use fs_protocol::tree::{FILES_APP, HASH_KEY, is_file_content_hash};

    let mut versions = BTreeSet::new();
    for ((app, id), bytes) in documents {
        if app != FILES_APP || !matches!(tree_chunk(id), Some(chunk) if chunk != 0) {
            continue;
        }
        let mut document = AutoCommit::load(bytes).map_err(|error| error.to_string())?;
        for change in document.get_changes(&[]) {
            for operation in change.decode().operations {
                if matches!(&operation.key, Key::Map(key) if matches!(key.as_str(), HASH_KEY | "deleted_hash"))
                    && let OpType::Put(automerge::ScalarValue::Str(hash)) = operation.action
                    && is_file_content_hash(hash.as_str())
                {
                    versions.insert(hash.to_string());
                }
            }
        }
    }
    Ok(versions)
}

fn write_file_aliases(
    bytes: &mut Vec<u8>,
    translated: &BTreeMap<String, String>,
) -> Result<(), String> {
    use automerge::{ObjType, Value};
    use fs_protocol::tree::FILE_VERSION_ALIASES_KEY;

    if translated.is_empty() {
        return Ok(());
    }
    let mut document = AutoCommit::load(bytes).map_err(|error| error.to_string())?;
    let aliases = match document
        .get(&ROOT, FILE_VERSION_ALIASES_KEY)
        .map_err(|error| error.to_string())?
    {
        Some((Value::Object(ObjType::Map), aliases)) => aliases,
        _ => document
            .put_object(&ROOT, FILE_VERSION_ALIASES_KEY, ObjType::Map)
            .map_err(|error| error.to_string())?,
    };
    for (before, after) in translated {
        document
            .put(&aliases, before, after.as_str())
            .map_err(|error| error.to_string())?;
    }
    *bytes = document.save();
    Ok(())
}

fn rekey(
    store: &ContentStore,
    source: &Manifest,
    (from, to): (WorkspaceId, WorkspaceId),
    (source_keys, branch_keys): (&ContentKeys, &ContentKeys),
) -> Result<String, String> {
    let old = source.sealed().ok_or("the source file is not sealed")?;
    let group = WorkspaceId::from_u128(u128::from_be_bytes(old.group));
    let metadata =
        EncryptionMetadata::content_group(from, group, old.epoch, ContentRetention::Delta);
    let from = file_content_artifact(from);
    let to = file_content_artifact(to);
    let mut chunks = Vec::with_capacity(source.chunks().len());
    let mut overhead = None;
    for part in source.chunks() {
        let bytes = store
            .read(&hex::encode(part.hash))
            .ok_or("a source file chunk is missing")?;
        let clear = source_keys
            .open_content(from, &metadata, &bytes)
            .map_err(|error| error.to_string())?;
        if clear.len() != part.len as usize {
            return Err("a source file chunk has another length".into());
        }
        let (_, sealed) = branch_keys
            .seal_content(to, ContentRetention::Delta, &clear)
            .map_err(|error| error.to_string())?;
        let extra = u32::try_from(sealed.len().saturating_sub(clear.len()))
            .map_err(|error| error.to_string())?;
        if *overhead.get_or_insert(extra) != extra {
            return Err("inconsistent sealed file chunk length".into());
        }
        let mut chunk = *part;
        let stored = store
            .put_public(&sealed)
            .map_err(|error| error.to_string())?;
        hex::decode_to_slice(stored, &mut chunk.hash).map_err(|error| error.to_string())?;
        chunks.push(chunk);
    }
    let manifest = Manifest::new_sealed(
        source.tier(),
        chunks,
        fs::content::SealedChunks {
            group: *branch_keys.workspace().as_uuid().as_bytes(),
            epoch: branch_keys.epoch(),
            overhead: overhead.ok_or("empty sealed manifest")?,
        },
    );
    store
        .put_public(&manifest.encode())
        .map_err(|error| error.to_string())
}

fn replace_file_hashes(
    bytes: &mut Vec<u8>,
    translated: &BTreeMap<String, String>,
) -> Result<(), String> {
    use fs_protocol::tree::{HASH_KEY, NODES_KEY};
    let mut document = AutoCommit::load(bytes).map_err(|error| error.to_string())?;
    let Some((automerge::Value::Object(automerge::ObjType::Map), nodes)) = document
        .get(&ROOT, NODES_KEY)
        .map_err(|error| error.to_string())?
    else {
        return Ok(());
    };
    let ids: Vec<_> = document.keys(&nodes).collect();
    for id in ids {
        let Some((automerge::Value::Object(automerge::ObjType::Map), node)) = document
            .get(&nodes, &id)
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        let Some((automerge::Value::Scalar(hash), _)) = document
            .get(&node, HASH_KEY)
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        if let Some(new) = hash.as_str().and_then(|old| translated.get(old)) {
            document
                .put(&node, HASH_KEY, new.as_str())
                .map_err(|error| error.to_string())?;
        }
    }
    *bytes = document.save();
    Ok(())
}

/// The Files tree among `records`, read: its own record and each chunk of it the branch took.
/// `None` when the branch took none of it.
fn tree_of(records: &[((String, String), Vec<u8>)]) -> Option<fs_protocol::tree::TreeChunks> {
    let mut tree = fs_protocol::tree::TreeChunks::default();
    let mut read = false;
    for ((app, record), document) in records {
        if app == fs_protocol::tree::FILES_APP {
            read |= tree.read_resource(record, document).unwrap_or(false);
        }
    }
    read.then_some(tree)
}

/// How many pieces of the files the tree among `records` names now `store`
/// lacks: each file's manifest, and every chunk of a manifest it keeps, as the
/// host gathers them ([`crate::files_content`]). What only an earlier version of
/// a record named is not counted: it may be long collected.
fn not_whole_yet(
    store: &tilo_content::ContentStore,
    records: &[((String, String), Vec<u8>)],
) -> usize {
    let Some(tree) = tree_of(records) else {
        return 0;
    };
    crate::files_content::files_named(&tree)
        .iter()
        .map(|manifest| match store.read(manifest) {
            None => 1,
            Some(listed) => fs::content::Manifest::decode(&listed).map_or(0, |manifest| {
                manifest
                    .chunks()
                    .iter()
                    .filter(|chunk| !store.has(&hex::encode(chunk.hash)))
                    .count()
            }),
        })
        .sum()
}

/// How many entries the Files tree among `records` holds: `None` when the
/// branch took no tree, or it does not read.
fn tree_entries(records: &[((String, String), Vec<u8>)]) -> Option<usize> {
    tree_of(records).map(|tree| tree.tree().nodes.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::BranchContent;
    use fs::ROOT_INO;
    use fs_protocol::tree::{FILES_APP, chunk_document_id};

    fn snapshot(namespace: &mut fs::Namespace) -> Vec<((String, String), Vec<u8>)> {
        namespace
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
            .collect()
    }

    fn put_file(store: &ContentStore, keys: &ContentKeys, data: &[u8]) -> String {
        let (plain, _) = fs::content::build(data, None);
        let (_, payload) = keys
            .seal_content(
                file_content_artifact(keys.workspace()),
                ContentRetention::Delta,
                data,
            )
            .unwrap();
        let stored = store.put_public(&payload).unwrap();
        let mut chunk = plain.chunks()[0];
        hex::decode_to_slice(stored, &mut chunk.hash).unwrap();
        let manifest = Manifest::new_sealed(
            plain.tier(),
            vec![chunk],
            fs::content::SealedChunks {
                group: *keys.workspace().as_uuid().as_bytes(),
                epoch: keys.epoch(),
                overhead: u32::try_from(payload.len() - data.len()).unwrap(),
            },
        );
        store
            .put_public(&manifest.encode())
            .unwrap()
            .parse()
            .unwrap()
    }

    #[test]
    fn a_branch_opens_an_earlier_file_version_with_its_own_keys() {
        let dir = tempfile::tempdir().unwrap();
        let stores = Arc::new(WorkspaceStores::in_dir(
            dir.path(),
            tilo_content::ContentLimits::default(),
        ));
        let source = WorkspaceId::new();
        let branch = WorkspaceId::new();
        let keys_for = |id| {
            key_group::KeyGroup::create(id, key_group::MemberKey::generate("member").unwrap())
                .unwrap()
                .content_keys()
                .unwrap()
        };
        let (from, to) = (keys_for(source), keys_for(branch));
        let source = source.to_string();
        let branch = branch.to_string();
        let store = stores.get(&source).unwrap();
        let (earlier, later) = (b"earlier version", b"current version");
        let old = put_file(&store, &from, earlier).parse().unwrap();
        let new = put_file(&store, &from, later).parse().unwrap();

        let mut namespace = fs::Namespace::new("source").unwrap();
        let ino = namespace
            .create_file(ROOT_INO, "report", Some(&old), earlier.len() as u64, 1)
            .unwrap();
        let before = snapshot(&mut namespace);
        assert!(namespace.set_content(ino, Some(&new), later.len() as u64, 2));
        let mut copied = snapshot(&mut namespace);

        let copier = StoresBranches(stores.clone());
        assert!(copier.has_sealed_files(&source, &copied).unwrap());
        copier
            .copy_rekeyed(&source, &branch, &mut copied, (&from, &to))
            .unwrap();

        // The branch's Files index is live when a reader brings back the earlier version.
        let restored = copied
            .into_iter()
            .filter(|((_, id), _)| tree_chunk(id) == Some(0))
            .chain(
                before
                    .into_iter()
                    .filter(|((_, id), _)| tree_chunk(id) != Some(0)),
            )
            .map(|((_, id), bytes)| (tree_chunk(&id).unwrap(), bytes))
            .collect();
        let mut namespace = fs::Namespace::new("branch").unwrap();
        namespace
            .take_published_documents(restored, &BTreeMap::new())
            .unwrap();
        let ino = namespace.lookup(ROOT_INO, "report").unwrap().ino.0;
        let hash = namespace.hash_for(ino).unwrap();
        assert_ne!(hash, old);

        let store = stores.get(&branch).unwrap();
        let bytes = store.read(&hash.to_hex()).unwrap();
        let manifest = Manifest::decode(&bytes).unwrap();
        let metadata = EncryptionMetadata::content_key(to.epoch(), ContentRetention::Delta);
        let payload = store.read(&hex::encode(manifest.chunks()[0].hash)).unwrap();
        let artifact = file_content_artifact(to.workspace());
        assert_eq!(
            &*to.open_content(artifact, &metadata, &payload).unwrap(),
            earlier
        );
        assert!(from.open_content(artifact, &metadata, &payload).is_err());
    }
}

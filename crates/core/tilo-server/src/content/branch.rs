//! Copy the content a workspace's branch needs. Current Files payloads must be complete.
//! Older versions go with the branch when they are still stored. Files versions are
//! encrypted with the branch's keys, and the tree index maps their original addresses to the
//! branch's addresses so history can bring them back.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Read as _};
use std::sync::Arc;

use crate::workspaces::BranchFileKeys;
use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, ROOT, ReadDoc as _};
use collab_types::{ContentRetention, EncryptionMetadata, WorkspaceId, file_content_artifact};
use fs_protocol::tree::FILES_APP;
pub(super) use fs_protocol::tree::chunk_of_record as tree_chunk;
use key_group::ContentKeys;
use tilo_content::{ContentStore, WorkspaceStores};
use tilo_fs::content::Manifest;

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
        let to = self.0.get(branch)?;
        copy_assets((source, branch), (&from, &to), records, &BTreeSet::new())
    }

    fn file_keys(
        &self,
        source: &str,
        documents: &[((String, String), Vec<u8>)],
    ) -> Result<BranchFileKeys, String> {
        let Some(tree) = tree_of(documents) else {
            return Ok(BranchFileKeys::None);
        };
        let source = self.0.get(source)?;
        let mut needed = BranchFileKeys::None;
        for hash in file_versions(documents)? {
            if let Some(version) = inspect_version(&source, tree.file_hash(&hash))? {
                if version
                    .manifest
                    .is_some_and(|manifest| manifest.sealed().is_some())
                {
                    return Ok(BranchFileKeys::SourceAndDestination);
                }
                needed = BranchFileKeys::Destination;
            }
        }
        Ok(needed)
    }

    fn copy_rekeyed(
        &self,
        source: &str,
        branch: &str,
        documents: &mut [((String, String), Vec<u8>)],
        keys: (Option<&ContentKeys>, &ContentKeys),
    ) -> Result<(), String> {
        let Some(tree) = tree_of(documents) else {
            return self.copy(source, branch, documents);
        };
        let from = WorkspaceId::parse(source).ok_or("invalid source workspace")?;
        let to = WorkspaceId::parse(branch).ok_or("invalid branch workspace")?;
        let original_store = self.0.get(source)?;
        let branch_store = self.0.get(branch)?;
        let gathering = not_whole_yet(&original_store, documents);
        if gathering > 0 {
            return Err(format!(
                "{gathering} pieces of the files its documents name have yet to arrive"
            ));
        }
        let current = current_file_versions(&tree);
        let mut translated = BTreeMap::new();
        let mut kept = BTreeMap::new();
        let mut omitted = BTreeSet::new();
        for original in file_versions(documents)? {
            let source_hash = tree.file_hash(&original);
            if !kept.contains_key(source_hash) {
                let Some(version) = inspect_version(&original_store, source_hash)? else {
                    continue;
                };
                omitted.insert(source_hash.to_owned());
                if let Some(manifest) = &version.manifest {
                    omitted.extend(manifest.chunks().iter().map(|part| hex::encode(part.hash)));
                    if !current.contains(source_hash)
                        && manifest
                            .chunks()
                            .iter()
                            .any(|part| !original_store.has(&hex::encode(part.hash)))
                    {
                        continue;
                    }
                }
                let replacement = seal_version(
                    (&original_store, &branch_store),
                    (source_hash, version),
                    (from, to),
                    keys,
                )?;
                kept.insert(source_hash.to_owned(), replacement);
            }
            if let Some(replacement) = kept.get(source_hash) {
                omitted.insert(original.clone());
                translated.insert(original, replacement.clone());
            }
        }
        translated.extend(kept);
        copy_assets(
            (source, branch),
            (&original_store, &branch_store),
            documents,
            &omitted,
        )?;
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

fn copy_assets(
    (source, branch): (&str, &str),
    (from, to): (&ContentStore, &ContentStore),
    documents: &[((String, String), Vec<u8>)],
    omitted: &BTreeSet<String>,
) -> Result<(), String> {
    let mut named = super::collection::named_by(documents, &|address| {
        (!omitted.contains(address))
            .then(|| from.read(address))
            .flatten()
    })?;
    if !omitted.is_empty() {
        let attachments = super::collection::named_by_matching(
            documents,
            &|(app, _)| app != FILES_APP,
            &|address| from.read(address),
        )?;
        named.retain(|hash| !omitted.contains(hash));
        named.extend(attachments);
    }
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
        tree_entries = ?tree_entries(documents),
        named = count,
        copied,
        not_kept_here = lacking,
        "a branch took its source's files"
    );
    Ok(())
}

/// Every version address ever entered in a chunk of the Files tree.
pub(super) fn file_versions(
    documents: &[((String, String), Vec<u8>)],
) -> Result<BTreeSet<String>, String> {
    use automerge::legacy::{Key, OpType};
    use fs_protocol::tree::{HASH_KEY, is_file_content_hash};

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
    if translated.is_empty() {
        return Ok(());
    }
    let mut document = AutoCommit::load(bytes).map_err(|error| error.to_string())?;
    set_file_aliases(&mut document, translated)?;
    commit_readdressed(&mut document);
    *bytes = document.save();
    Ok(())
}

/// Commit what the framework re-addressed in a branch's copied record as the
/// storage migration it is: no person's action produced it, so it names the
/// migration, and admission decides it as the device that signs it.
fn commit_readdressed(document: &mut AutoCommit) {
    document.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Migrate.change_message()),
    );
}

pub(super) fn set_file_aliases(
    document: &mut AutoCommit,
    translated: &BTreeMap<String, String>,
) -> Result<(), String> {
    use automerge::{ObjType, Value};
    use fs_protocol::tree::FILE_VERSION_ALIASES_KEY;

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
    Ok(())
}

/// A stored file version is a manifest or a legacy file stored directly by its content hash.
pub(super) struct StoredVersion {
    len: u64,
    pub(super) manifest: Option<Manifest>,
}

pub(super) fn inspect_version(
    store: &ContentStore,
    address: &str,
) -> Result<Option<StoredVersion>, String> {
    let Some(len) = store.size(address).filter(|len| *len > 0) else {
        return Ok(None);
    };
    let Some(reader) = store.open(address) else {
        return Ok(None);
    };
    let mut reader = BufReader::new(reader);
    let mut header = [0; blake3::OUT_LEN];
    let manifest = if len.is_multiple_of(header.len() as u64) {
        reader
            .read_exact(&mut header)
            .map_err(|error| error.to_string())?;
        if fs_content::is_manifest(&header) {
            let mut bytes = header.to_vec();
            reader
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            Manifest::decode(&bytes).ok()
        } else {
            None
        }
    } else {
        None
    };
    Ok(Some(StoredVersion { len, manifest }))
}

pub(super) fn seal_version(
    (source_store, destination): (&ContentStore, &ContentStore),
    (address, version): (&str, StoredVersion),
    (from, to): (WorkspaceId, WorkspaceId),
    (source_keys, branch_keys): (Option<&ContentKeys>, &ContentKeys),
) -> Result<String, String> {
    let open = || {
        source_store
            .open(address)
            .ok_or("the source file is missing")
    };
    let (manifest, mut inline) = if let Some(manifest) = version.manifest {
        (manifest, None)
    } else {
        let (manifest, _) =
            tilo_fs::content::build_reader(BufReader::new(open()?), version.len, None)
                .map_err(|error| error.to_string())?;
        (manifest, Some(BufReader::new(open()?)))
    };
    let old = manifest.sealed().map(|old| {
        let group = WorkspaceId::from_u128(u128::from_be_bytes(old.group));
        EncryptionMetadata::content_group(from, group, old.epoch, ContentRetention::Delta)
    });
    let from = file_content_artifact(from);
    let to = file_content_artifact(to);
    let mut chunks = Vec::with_capacity(manifest.chunks().len());
    let mut overhead = None;
    for part in manifest.chunks() {
        let payload = match inline.as_mut() {
            Some(reader) => {
                let mut payload = vec![0; part.len as usize];
                reader
                    .read_exact(&mut payload)
                    .map_err(|error| error.to_string())?;
                if blake3::hash(&payload).as_bytes() != &part.hash {
                    return Err("the source file changed while it was copied".into());
                }
                payload
            }
            None => source_store
                .read(&hex::encode(part.hash))
                .ok_or("a source file chunk is missing")?,
        };
        let opened;
        let clear = if let Some(metadata) = &old {
            opened = source_keys
                .ok_or("the source's Files keys are unavailable")?
                .open_content(from, metadata, &payload)
                .map_err(|error| error.to_string())?;
            opened.as_slice()
        } else {
            &payload
        };
        if clear.len() != part.len as usize {
            return Err("a source file chunk has another length".into());
        }
        let (_, sealed) = branch_keys
            .seal_content(to, ContentRetention::Delta, clear)
            .map_err(|error| error.to_string())?;
        let extra = u32::try_from(sealed.len().saturating_sub(clear.len()))
            .map_err(|error| error.to_string())?;
        if *overhead.get_or_insert(extra) != extra {
            return Err("inconsistent sealed file chunk length".into());
        }
        let mut chunk = *part;
        let stored = destination
            .put_public(&sealed)
            .map_err(|error| error.to_string())?;
        hex::decode_to_slice(stored, &mut chunk.hash).map_err(|error| error.to_string())?;
        chunks.push(chunk);
    }
    if let Some(mut reader) = inline
        && reader
            .read(&mut [0_u8; 1])
            .map_err(|error| error.to_string())?
            != 0
    {
        return Err("the source file changed while it was copied".into());
    }
    let overhead = if let Some(overhead) = overhead {
        overhead
    } else {
        let (_, sealed) = branch_keys
            .seal_content(to, ContentRetention::Delta, &[])
            .map_err(|error| error.to_string())?;
        u32::try_from(sealed.len()).map_err(|error| error.to_string())?
    };
    let manifest = Manifest::new_sealed(
        manifest.tier(),
        chunks,
        tilo_fs::content::SealedChunks {
            group: *branch_keys.workspace().as_uuid().as_bytes(),
            epoch: branch_keys.epoch(),
            overhead,
        },
    );
    destination
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
    commit_readdressed(&mut document);
    *bytes = document.save();
    Ok(())
}

/// The Files tree among `records`, read: its own record and each chunk of it the branch took.
/// `None` when the branch took none of it.
pub(super) fn tree_of(
    documents: &[((String, String), Vec<u8>)],
) -> Option<fs_protocol::tree::TreeChunks> {
    let mut tree = fs_protocol::tree::TreeChunks::default();
    let mut read = false;
    for ((app, record), document) in documents {
        if app == fs_protocol::tree::FILES_APP {
            read |= tree.read_resource(record, document).unwrap_or(false);
        }
    }
    read.then_some(tree)
}

/// How many pieces of the files the tree among `records` names now `store`
/// lacks: each file's manifest, and every chunk of a manifest it keeps, as the
/// host gathers them ([`crate::kept_content`]). What only an earlier version of
/// a record named is not counted: it may be long collected.
fn not_whole_yet(
    store: &tilo_content::ContentStore,
    records: &[((String, String), Vec<u8>)],
) -> usize {
    let Some(tree) = tree_of(records) else {
        return 0;
    };
    current_file_versions(&tree)
        .iter()
        .map(|hash| match inspect_version(store, hash) {
            Ok(Some(StoredVersion {
                manifest: Some(manifest),
                ..
            })) => manifest
                .chunks()
                .iter()
                .filter(|chunk| !store.has(&hex::encode(chunk.hash)))
                .count(),
            Ok(Some(_)) => 0,
            Ok(None) if store.size(hash) == Some(0) => 0,
            Ok(None) | Err(_) => 1,
        })
        .sum()
}

fn current_file_versions(tree: &fs_protocol::tree::TreeChunks) -> BTreeSet<String> {
    use fs_protocol::files::FileNodeKind;
    use fs_protocol::tree::TRASH_ID;

    tree.listed()
        .into_iter()
        .filter_map(|chunk| tree.chunk(chunk))
        .flat_map(|nodes| nodes.iter())
        .filter(|(_, node)| {
            node.kind == FileNodeKind::File && node.parent.as_deref() != Some(TRASH_ID)
        })
        .filter_map(|(_, node)| node.hash.as_deref())
        .map(|hash| tree.file_hash(hash).to_ascii_lowercase())
        .collect()
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
    use fs_protocol::tree::{FILES_APP, chunk_document_id};
    use tilo_automerge::ApplyIncoming as _;
    use tilo_fs::ROOT_INO;

    fn snapshot(namespace: &mut tilo_fs::Namespace) -> Vec<((String, String), Vec<u8>)> {
        namespace
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
            .collect()
    }

    fn put_file(store: &ContentStore, keys: &ContentKeys, data: &[u8]) -> String {
        let (plain, _) = tilo_fs::content::build(data, None);
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
            tilo_fs::content::SealedChunks {
                group: *keys.workspace().as_uuid().as_bytes(),
                epoch: keys.epoch(),
                overhead: u32::try_from(payload.len().checked_sub(data.len()).unwrap()).unwrap(),
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

        let mut namespace = tilo_fs::Namespace::new("source").unwrap();
        let ino = namespace
            .create_file(ROOT_INO, "report", Some(&old), earlier.len() as u64, 1)
            .unwrap();
        let before = snapshot(&mut namespace);
        assert!(namespace.set_content(ino, Some(&new), later.len() as u64, 2));
        let mut copied = snapshot(&mut namespace);

        let transfer = StoresBranches(stores.clone());
        assert_eq!(
            transfer.file_keys(&source, &copied).unwrap(),
            BranchFileKeys::SourceAndDestination
        );
        transfer
            .copy_rekeyed(&source, &branch, &mut copied, (Some(&from), &to))
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
        let mut namespace = tilo_fs::Namespace::new("branch").unwrap();
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

    #[test]
    fn branching_plaintext_files_only_publishes_sealed_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let stores = Arc::new(WorkspaceStores::in_dir(
            dir.path(),
            tilo_content::ContentLimits::default(),
        ));
        let source = WorkspaceId::new().to_string();
        let branch = WorkspaceId::new();
        let keys =
            key_group::KeyGroup::create(branch, key_group::MemberKey::generate("member").unwrap())
                .unwrap()
                .content_keys()
                .unwrap();
        let clear = b"a version stored in the clear";
        let original = stores.get(&source).unwrap();
        let raw = original.put_public(clear).unwrap();
        let mut namespace = tilo_fs::Namespace::new("source").unwrap();
        namespace
            .create_file(
                ROOT_INO,
                "report",
                Some(&raw.parse().unwrap()),
                clear.len() as u64,
                1,
            )
            .unwrap();
        let (empty, _) = tilo_fs::content::build(b"", None);
        let empty_hash = original.put_public(&empty.encode()).unwrap();
        namespace
            .create_file(ROOT_INO, "empty", Some(&empty_hash.parse().unwrap()), 0, 1)
            .unwrap();
        let mut copied = snapshot(&mut namespace);
        let transfer = StoresBranches(stores.clone());
        assert_eq!(
            transfer.file_keys(&source, &copied).unwrap(),
            BranchFileKeys::Destination
        );
        transfer
            .copy_rekeyed(&source, &branch.to_string(), &mut copied, (None, &keys))
            .unwrap();

        let store = stores.get(&branch.to_string()).unwrap();
        assert!(original.has(&raw));
        assert!(
            !store.has(&raw),
            "the branch copied no plaintext file payload"
        );
        let tree = tree_of(&copied).unwrap();
        let empty = Manifest::decode(&store.read(tree.file_hash(&empty_hash)).unwrap()).unwrap();
        assert!(empty.sealed().is_some());
        assert_eq!(empty.total_len(), 0);
        let hash = tree.file_hash(&raw);
        let manifest = Manifest::decode(&store.read(hash).unwrap()).unwrap();
        assert!(manifest.sealed().is_some());
        let payload = store.read(&hex::encode(manifest.chunks()[0].hash)).unwrap();
        let metadata = EncryptionMetadata::content_key(keys.epoch(), ContentRetention::Delta);
        assert_eq!(
            &*keys
                .open_content(file_content_artifact(branch), &metadata, &payload)
                .unwrap(),
            clear
        );
    }
}

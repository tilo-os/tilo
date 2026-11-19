//! A branch of a workspace takes the content its records name
//! ([`crate::workspaces::BranchContent`]): each item is copied from the source's
//! store into the branch's as the bytes the source keeps under its address,
//! sealed content as its ciphertext, so the reference a record holds (its
//! address, and the key of sealed content) reads the same in the branch. What
//! the branch takes is what a pass over its records keeps
//! ([`super::collection::named_by`]): every address any version of them names,
//! and the chunks of the files those are. The files the workspace's tree names
//! now must all be here, each manifest and every chunk, or no branch is made: a
//! file the host is still gathering would never reach the branch. What only an
//! earlier version named is copied when it is here, and may be long collected.

use std::collections::BTreeMap;
use std::sync::Arc;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ROOT, ReadDoc as _};
use collab_types::{ContentRetention, EncryptionMetadata, WorkspaceId, file_content_artifact};
use fs::content::Manifest;
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
        Ok(crate::files_content::files_named(&tree).iter().any(|hash| {
            source
                .read(hash)
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
        let mut translated = BTreeMap::new();
        for hash in crate::files_content::files_named(&tree) {
            let Some(bytes) = store.read(&hash) else {
                continue;
            };
            let Ok(manifest) = Manifest::decode(&bytes) else {
                continue;
            };
            if manifest.sealed().is_some() {
                let replacement = rekey(&store, &manifest, (from, to), keys)?;
                translated.insert(hash, replacement);
            }
        }
        for ((app, document), bytes) in documents {
            if app == fs_protocol::tree::FILES_APP
                && fs_protocol::tree::chunk_of_record(document).is_some()
            {
                replace_file_hashes(bytes, &translated)?;
            }
        }
        Ok(())
    }

    fn discard(&self, branch: &str) -> Result<(), String> {
        self.0.remove(branch)
    }
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

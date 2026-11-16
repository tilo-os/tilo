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

use std::sync::Arc;

use tilo_content::WorkspaceStores;

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

    fn discard(&self, branch: &str) -> Result<(), String> {
        self.0.remove(branch)
    }
}

/// The Files tree among `records`, read; `None` when the branch took none, or
/// it does not read.
fn tree_of(records: &[((String, String), Vec<u8>)]) -> Option<fs_protocol::tree::Tree> {
    let tree = fs_protocol::tree::record_id();
    records
        .iter()
        .find(|((app, record), _)| app == fs_protocol::tree::FILES_APP && *record == tree)
        .and_then(|(_, document)| fs_protocol::tree::Tree::of_record(document).ok())
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
    tree_of(records).map(|tree| tree.nodes.len())
}

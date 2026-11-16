//! A branch of a workspace takes the content its records name
//! ([`crate::workspaces::BranchContent`]): each item is copied from the source's
//! store into the branch's as the bytes the source keeps under its address,
//! sealed content as its ciphertext, so the reference a record holds (its
//! address, and the key of sealed content) reads the same in the branch. What
//! the branch takes is what a pass over its records keeps
//! ([`super::collection::named_by`]): every address any version of them names,
//! and the chunks of the files those are.

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
}

/// How many entries the Files tree among `records` holds: `None` when the
/// branch took no tree, or it does not read.
fn tree_entries(records: &[((String, String), Vec<u8>)]) -> Option<usize> {
    let tree = fs_protocol::tree::record_id();
    records
        .iter()
        .find(|((app, record), _)| app == fs_protocol::tree::FILES_APP && *record == tree)
        .and_then(|(_, document)| fs_protocol::tree::Tree::of_record(document).ok())
        .map(|tree| tree.nodes.len())
}

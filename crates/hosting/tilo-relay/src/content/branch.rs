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
        for address in named {
            if to.has(&address) {
                continue;
            }
            // Named but not kept here: the source answers it no more than the branch.
            let Some(mut kept) = from.open(&address) else {
                continue;
            };
            let copied = to
                .put_reader(&mut kept, "application/octet-stream")
                .map_err(|error| format!("{address}: {error}"))?;
            if copied.hash != address {
                return Err(format!(
                    "{address}: the source keeps other bytes under it ({})",
                    copied.hash
                ));
            }
        }
        Ok(())
    }
}

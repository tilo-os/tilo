//! The records of a workspace's file tree that an earlier build wrote without naming them.
//!
//! A computer signed in with `tilo login` mounts the workspace's folder, and its daemon publishes
//! the folder's tree as Files' records: the tree's own record and one record per chunk of it.
//! Builds before the mount named each record wrote them without the identity every reader checks
//! before it opens a record (its id, kind, home and workspace), so every reader refuses them
//! ("record identity does not match its binding"): Files shows nothing of the tree and the search
//! index lists none of it. A mount on this build names a record when it next writes it, but a
//! tree nobody writes to again stays unreadable for every member.
//!
//! So the service that governs such a workspace names each of those records once as it opens it,
//! before any member's session reaches it. The name is the founding every founder of the record
//! makes on this build ([`identity::founding_for_unnamed`]), written as the governor's own write
//! through its normal authoring path, so members admit it as any write of the governor. It is the
//! same change whoever makes it, so a mount on this build naming the same record at the same time
//! writes the same identity, and the two merge as one. A record that has an identity already is
//! left alone: nothing here rewrites what a record says it is.

use automerge::AutoCommit;
use collab_types::document::identity;
use collab_types::document::{DocumentId, ResourceType};
use neighbour_wire::DocumentSync;
use tilo_automerge::ApplyIncoming as _;

use super::Workspaces;

/// The app whose records a workspace's tree is.
const FILES_APP: &str = tilo_app_id::apps::FILE_EXPLORER;

/// Whether `record` is one of a workspace's tree: its own record, or a chunk of it.
fn tree_record(record: &str) -> bool {
    let tree = collab_types::person_document_id(collab_types::FILES_TREE_KIND);
    record == tree
        || matches!(
            neighbour_wire::document_sync(record),
            DocumentSync::Chunk { base, .. } if base == tree
        )
}

impl Workspaces {
    /// Name every unnamed record of the tree of each workspace governed here. What could not be
    /// named is logged and tried again when the service next opens.
    pub(super) async fn name_kept_trees(&self) {
        let states: Vec<_> = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect();
        for state in states {
            let workspace = {
                let governed = state.lock().await;
                // A member's copy takes the names from the server that governs it.
                if governed.member.is_some() {
                    continue;
                }
                governed.workspace().to_owned()
            };
            match Box::pin(self.name_kept_tree(&workspace)).await {
                Ok(0) => {}
                Ok(named) => {
                    tracing::info!(%workspace, named, "the workspace's file tree names its records");
                }
                Err(error) => {
                    tracing::error!(%workspace, %error, "the workspace's file tree was not named");
                }
            }
        }
    }

    /// [`Self::name_kept_trees`] for `workspace`: how many records it named.
    pub(super) async fn name_kept_tree(&self, workspace: &str) -> Result<usize, String> {
        let records: Vec<String> = self
            .own_listing(workspace)?
            .into_iter()
            .filter(|(app, record)| app == FILES_APP && tree_record(record))
            .map(|(_, record)| record)
            .collect();
        let kind = ResourceType::new(collab_types::FILES_TREE_KIND);
        let governor = self.device_cert().clone();
        let mut named: usize = 0;
        for record in records {
            let id = DocumentId::parse(&record)
                .ok_or_else(|| format!("{record} names no resource of the tree"))?;
            let Some(bytes) = self
                .resource_document(workspace, FILES_APP, &record)
                .await
                .map_err(|error| error.to_string())?
            else {
                continue;
            };
            let held = AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
            if identity::founding_for_unnamed(&held, &id, &kind, workspace).is_none() {
                continue;
            }
            let (kind, home) = (kind.clone(), workspace.to_owned());
            let wrote = Box::pin(self.author_workspace_document(
                workspace,
                &governor,
                (FILES_APP, &record),
                move |doc| {
                    let Some(mut founding) = identity::founding_for_unnamed(doc, &id, &kind, &home)
                    else {
                        return Ok(false);
                    };
                    doc.merge_incoming(&mut founding)
                        .map_err(|error| error.to_string())?;
                    Ok(true)
                },
            ))
            .await
            .map_err(|error| error.to_string())?;
            named = named.saturating_add(usize::from(wrote));
        }
        Ok(named)
    }
}

#[cfg(test)]
#[path = "tree_identity_tests.rs"]
mod tests;

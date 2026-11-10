//! What the host changed, published as the member, and the workspace's
//! record index kept with it.
//!
//! A workspace holds a record by naming it in its app's index
//! ([`ui::record::index`]): the document every window of the app reads its
//! workspace's records from, and what a person's search finds them by. A
//! window names the record it opens there itself; a record the host makes
//! or changes for someone (an agent's, with no window open) is named there
//! by the host, as the same act, under what its app calls it. Every change
//! the host publishes passes [`Worker::commit`], so that is where it names
//! the record: one place, whoever asked for the change.

use collab_types::DocumentMutation;
use tilo_peer_state::document_frame::DocumentRoute;

use super::{COMMIT_WAIT, Needs, Worker, bytes_of, is_part};

/// The record at `route` as the session's replica holds it, with `written`
/// (a change of it the replica may not have taken in yet) merged in, and
/// whether the replica holds it at all.
fn document(
    peer: &tilo_session_host::PeerHandle,
    route: &DocumentRoute,
    written: Option<&[u8]>,
) -> (automerge::AutoCommit, bool) {
    let held = peer
        .engine()
        .record(&route.cluster_id, &route.app_id, &route.record);
    let (mut doc, held) = match held {
        Ok(Some(bytes)) => (
            automerge::AutoCommit::load(&bytes).unwrap_or_else(|error| {
                tracing::warn!(record = %route.record, %error, "workspace actions: a record does not load");
                automerge::AutoCommit::new()
            }),
            true,
        ),
        _ => (automerge::AutoCommit::new(), false),
    };
    if let Some(bytes) = written
        && let Err(error) = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes)
    {
        tracing::warn!(record = %route.record, %error, "workspace actions: a change does not load");
    }
    (doc, held)
}

impl Worker {
    /// Publish `mutation` as the member: sealed with the record's channel
    /// keys, or held until a participant admits the member to it.
    pub(super) fn commit(
        &mut self,
        workspace_id: u64,
        route: &DocumentRoute,
        mutation: &DocumentMutation,
    ) -> Result<(), String> {
        let workspace = self
            .workspaces
            .get_mut(&workspace_id)
            .ok_or("the workspace was not joined")?;
        workspace.peer.publish(route.clone(), mutation)?;
        // The workspace holds the record by its index naming it, as a
        // window opening the record names it there.
        self.index_record(workspace_id, route, &bytes_of(mutation));
        Ok(())
    }

    /// [`Self::commit`], done only once the host holds the change durably
    /// (the session's barrier): handing it to the session, possibly to wait
    /// for keys, is not a save.
    pub(super) fn commit_durably(
        &mut self,
        workspace_id: u64,
        route: &DocumentRoute,
        mutation: &DocumentMutation,
    ) -> Result<(), String> {
        self.commit(workspace_id, route, mutation)?;
        let saved = self
            .workspaces
            .get(&workspace_id)
            .ok_or("the workspace was not joined")?
            .peer
            .barrier()?;
        match saved.recv_timeout(COMMIT_WAIT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(reason)) => Err(format!("the host did not save the change: {reason}")),
            Err(_) => Err("the host has not acknowledged saving the change".into()),
        }
    }

    /// What the record at `route` is called in its app's index of the
    /// workspace, when the index names it.
    pub(super) fn record_name(&self, workspace_id: u64, route: &DocumentRoute) -> Option<String> {
        let workspace = self.workspaces.get(&workspace_id)?;
        let checkout = ui::record::identity(&document(&workspace.peer, route, None).0)?;
        let index = DocumentRoute {
            record: String::new(),
            ..route.clone()
        };
        ui::record::index::name_of(&document(&workspace.peer, &index, None).0, &checkout.id)
    }

    /// Name the record at `route` in its app's index of the workspace,
    /// under what its app calls it now, unless the index already does. A
    /// record of the workspace itself (its desk, its emoji), the index, and
    /// a part of a record's chain are no records of their own there.
    pub(super) fn index_record(
        &mut self,
        workspace_id: u64,
        route: &DocumentRoute,
        written: &[u8],
    ) {
        if route.record.is_empty()
            || is_part(&route.record)
            || collab_types::workspace_record(&route.app_id, &route.record).is_some()
            || self.catalogue.get(&route.app_id).is_none()
        {
            return;
        }
        if let Err(reason) = self.name_in_index(workspace_id, route, written) {
            tracing::warn!(app = %route.app_id, record = %route.record, %reason,
                "workspace actions: a record is not named in its workspace's index");
        }
    }

    fn name_in_index(
        &mut self,
        workspace_id: u64,
        route: &DocumentRoute,
        written: &[u8],
    ) -> Result<(), String> {
        let index_route = DocumentRoute {
            record: String::new(),
            ..route.clone()
        };
        let link = self.shared.link.get().cloned();
        let mut record = {
            let workspace = self
                .workspaces
                .get(&workspace_id)
                .ok_or("the workspace was not joined")?;
            document(&workspace.peer, route, Some(written)).0
        };
        let Some(checkout) = ui::record::identity(&record) else {
            return Ok(());
        };
        // What the app calls the record is the app's to say: read in its
        // confined worker, as the record's bytes are anyone's.
        let name = match self.titles.title(&route.app_id, record.save()) {
            Ok(name) => name,
            Err(reason) => {
                tracing::info!(app = %route.app_id, %reason,
                    "workspace actions: a record's name is not read");
                None
            }
        };
        let workspace = self
            .workspaces
            .get_mut(&workspace_id)
            .ok_or("the workspace was not joined")?;
        // The index is written over what the workspace holds of it, never
        // beside it: its keys first (made when the app has no index yet in
        // this workspace), then everything that arrived under them.
        if let Some(link) = &link
            && let Some(keys) = link.host_keys(&index_route, true)
        {
            workspace
                .peer
                .install_channel_keys(index_route.clone(), keys?)?;
        }
        workspace
            .wait_for(
                workspace_id,
                Needs::Act {
                    app_id: &route.app_id,
                    record: "",
                },
                link.as_deref(),
            )
            .map_err(|error| error.to_string())?;
        let (mut index, held) = document(&workspace.peer, &index_route, None);
        let _ = index.save();
        let named = ui::record::index::reference(&mut index, &checkout.id, &checkout.kind);
        let renamed = name.is_some()
            && ui::record::index::set_name(&mut index, &checkout.id, name.as_deref());
        if !named && !renamed {
            return Ok(());
        }
        let change = index.save_incremental();
        if change.is_empty() {
            return Ok(());
        }
        // An index the workspace holds nothing of yet starts as the whole
        // record: a reader holding none of it reads it from that, where a
        // change alone would wait for a base no one sends.
        let mutation = if held {
            DocumentMutation::Incremental(change)
        } else {
            DocumentMutation::Snapshot(index.save())
        };
        // Durably: the next name is written over this one, read back from the
        // session's replica.
        self.commit_durably(workspace_id, &index_route, &mutation)
    }
}

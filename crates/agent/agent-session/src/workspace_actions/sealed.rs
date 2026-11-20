//! What a workspace waits for before it can read or act: the routes whose documents arrived and
//! open only once the member holds their keys, or has the surface they were written with and their
//! workspace's governance.

use std::collections::{BTreeSet, HashSet};

use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;

use super::naming::is_part;
use super::{Needs, opened};

/// What a workspace's earlier waits for keys gave up on.
#[derive(Default)]
pub(super) struct GaveUp {
    pub(super) listed: bool,
    /// Records a listing by name waited for in vain.
    pub(super) records: HashSet<DocumentRoute>,
}

/// The routes of `workspace_id` in `routes` still undecided for `needs`:
/// documents that arrived and open only once the member holds their keys, or
/// has the surface they were written with and their workspace's governance.
pub(super) fn sealed_for(
    routes: &BTreeSet<DocumentRoute>,
    workspace_id: WorkspaceId,
    needs: Needs<'_>,
    gave_up: &GaveUp,
) -> Vec<DocumentRoute> {
    // A part of a chain is held beside a record of its app that keeps it:
    // reading or acting on it needs that record open too, and any other
    // part of the same chain, which the request's host sends beside it.
    let named = |route: &DocumentRoute, app_id: &str, record: &str| {
        let same_chain = match (opened::chain_of(record), opened::chain_of(&route.document)) {
            (Some(asked), Some(held)) => asked == held,
            _ => false,
        };
        route.app_id == app_id
            && (route.document == record
                || same_chain
                || (is_part(record) && !is_part(&route.document)))
    };
    routes
        .iter()
        .filter(|route| {
            WorkspaceId::parse(&route.cluster_id) == Some(workspace_id)
                && match needs {
                    Needs::Joined => false,
                    Needs::Workspace => !gave_up.listed,
                    Needs::Read { app_id, record } => {
                        named(route, app_id, record) && !gave_up.records.contains(*route)
                    }
                    Needs::Act { app_id, record } => named(route, app_id, record),
                }
        })
        .cloned()
        .collect()
}

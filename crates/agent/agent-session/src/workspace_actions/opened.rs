//! The records the workspace sends this service only when it opens them.
//!
//! A public workspace — one listed in its host's directory, which anyone may
//! walk into — replays a joining device only its governance and its shared
//! desktop, and sends every other record once that device opens it, saying so
//! in the [`tilo_session_client::Event::HostReady`] that ends the replay. So a
//! record this service holds none of is not one the workspace lacks: it is
//! asked for ([`Cmd::OpenRoute`](tilo_session_client::Cmd::OpenRoute), which asks the host and delivers
//! what arrives as an ordinary change), and it is absent only once the host
//! answered it holds none of it ([`tilo_session_client::Event::DocumentOpened`]
//! with no frames). Without that, every read and action about a record the
//! host had not replayed was refused as one the workspace has none of, and a
//! member's own records could neither be read nor acted on in the workspace
//! they wrote them in.

use super::keys::{Keys, invoke_keys};
use super::{DocumentRoute, Needs, Outcome, Worker, Workspace};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, APPLICATION_START_ACTION,
    ActionCallError, InvokeRequest, application_provider, application_provider_resource,
    provider_workspace,
};
use tilo_server_protocol::WorkspaceId;
use tilo_session_client::{Cmd, Outbox};

/// The chain `record` is a part of, when it is a part of one.
pub(super) fn chain_of(record: &str) -> Option<ui::resource::DocumentId> {
    let part = ui::resource::DocumentId::parse(record)?;
    part.chain_part().map(|(chain, _)| chain)
}

/// The records a request naming `record` of `app_id` asks the workspace's
/// host for, in the workspace whose channels are `cluster_id`: the record
/// itself, and for a part of a chain the chain's head
/// ([`ui::resource::DocumentId::chain_head`]), which names whichever part is
/// the newest rather than a guess at its oldest, beside the record a part
/// is kept with.
fn asked_routes(cluster_id: &str, app_id: &str, record: &str) -> Vec<DocumentRoute> {
    let route = |record: &str| DocumentRoute {
        app_id: app_id.to_owned(),
        cluster_id: cluster_id.to_owned(),
        document: record.to_owned(),
    };
    let mut routes = vec![route(record)];
    if let Some(chain) = chain_of(record) {
        // A write to a chain belongs in its newest part, and a device that
        // holds none cannot know which that is.
        routes.push(route(chain.chain_head().as_str()));
        // The app's own record of the workspace: what a part is held with.
        routes.push(route(""));
    }
    routes
}

/// The chunks of the tree `route` names, as its own record `index` lists them:
/// the records a member reads a workspace's folder with, beside the tree's own
/// record. None for a record that is no tree's own.
fn tree_chunks(index: Option<&[u8]>, route: &DocumentRoute) -> Vec<DocumentRoute> {
    if fs_protocol::tree::chunk_of_record(&route.document) != Some(0) {
        return Vec::new();
    }
    let Some(index) = index else {
        return Vec::new();
    };
    let Ok(doc) = automerge::AutoCommit::load(index) else {
        return Vec::new();
    };
    fs_protocol::tree::listed_chunks(&doc)
        .into_iter()
        .map(|chunk| DocumentRoute {
            document: fs_protocol::tree::chunk_document_id(chunk),
            ..route.clone()
        })
        .collect()
}

impl Workspace {
    /// Whether this member's replica holds `route`: readable, or sealed and waiting for the
    /// keys it opens with.
    pub(super) fn holds(&self, route: &DocumentRoute) -> bool {
        self.peer
            .engine()
            .documents(&route.cluster_id)
            .is_ok_and(|records| {
                records
                    .iter()
                    .any(|(app, record)| *app == route.app_id && *record == route.document)
            })
    }

    /// The routes reading (or acting on) `needs` reaches: the record it names
    /// ([`asked_routes`]) and, for a workspace's tree's own record, the chunks
    /// that record names. A workspace's folder is read record by record, so a
    /// request reading it reads every one of those records.
    pub(super) fn routes(&self, workspace_id: WorkspaceId, needs: Needs<'_>) -> Vec<DocumentRoute> {
        let (Needs::Read { app_id, record } | Needs::Act { app_id, record }) = needs else {
            return Vec::new();
        };
        let Some(cluster_id) = self.cluster_id(workspace_id) else {
            return Vec::new();
        };
        let mut routes = asked_routes(&cluster_id, app_id, record);
        if fs_protocol::tree::chunk_of_record(record) == Some(0)
            && let Some(tree) = routes.first()
        {
            // The tree's own record names its chunks: read from the replica,
            // which holds it once the request waited for it.
            let index = self
                .peer
                .engine()
                .record(&cluster_id, app_id, record)
                .ok()
                .flatten();
            routes.extend(tree_chunks(index.as_deref(), tree));
        }
        routes
    }

    /// Ask the workspace's host for the records `needs` reads that this
    /// member holds none of, and say whether an answer is still owed: the
    /// host's answer is the session's change that wakes the request to look
    /// again, and the wait is the one the request's keys get, so a host that
    /// never answers ends it. A workspace that sends its records when they
    /// are opened sends nothing else, so a record not here is one the
    /// workspace may still hold: it is asked for rather than taken as absent,
    /// and is absent only once the host answered it holds none of it
    /// ([`tilo_session_client::Event::DocumentOpened`] with no frames).
    pub(super) fn ask(&mut self, workspace_id: WorkspaceId, needs: Needs<'_>) -> bool {
        if self.when_opened {
            return self.ask_for(workspace_id, needs);
        }
        // A replay sent what the workspace held when this member joined. A chunk its tree
        // lists that is not here was made since (a branch's, refounded as it was made), and
        // reaches this member only asked: read as absent, a write would found it again.
        let chunks = self
            .routes(workspace_id, needs)
            .into_iter()
            .filter(|route| {
                fs_protocol::tree::chunk_of_record(&route.document).is_some_and(|chunk| chunk > 0)
            })
            .collect();
        self.ask_routes(workspace_id, chunks)
    }

    /// [`Self::ask`] whether or not the workspace replayed everything it held when this member
    /// joined: a record made after that is sent to a member only once it asked for it.
    pub(super) fn ask_for(&mut self, workspace_id: WorkspaceId, needs: Needs<'_>) -> bool {
        let routes = self.routes(workspace_id, needs);
        self.ask_routes(workspace_id, routes)
    }

    /// Ask for each of `routes` this member holds none of and has not asked for, and say
    /// whether an answer is still owed for any of them.
    pub(super) fn ask_routes(
        &mut self,
        workspace_id: WorkspaceId,
        routes: Vec<DocumentRoute>,
    ) -> bool {
        let mut owing = false;
        for route in routes {
            if !self.holds(&route)
                && !self.answered.contains(&route)
                && !self.asked.contains(&route)
            {
                // A record the host sends only to a device that opened it (a chunk of a
                // tree) is opened as one; any other is asked for by its route.
                let ask = if tilo_actions::document_sync::document_sync(&route.document)
                    == tilo_actions::document_sync::DocumentSync::Eager
                {
                    Cmd::OpenRoute(route.clone())
                } else {
                    Cmd::OpenDocumentAt(workspace_id, route.clone())
                };
                match self.peer.send(ask) {
                    Ok(()) => {
                        tracing::debug!(
                            app = %route.app_id,
                            record = %route.document,
                            "workspace actions: asked the workspace for a record this member holds none of"
                        );
                        self.asked.insert(route.clone());
                    }
                    Err(error) => tracing::warn!(
                        %error,
                        app = %route.app_id,
                        record = %route.document,
                        "workspace actions: a record the workspace may hold was not asked for"
                    ),
                }
            }
            owing = owing || self.asked.contains(&route);
        }
        owing
    }
}

impl Worker {
    /// In a workspace that sends its documents only once they are opened,
    /// ask it for the chains kept beside the document `needs` names: the head
    /// of each chain of its app that the workspace lists a part of. An app
    /// keeps its chains (Tasks' issues, Chat's messages) in parts beside its
    /// own document, and the workspace sends a part only to a device that
    /// asks for its chain's head, which it answers with the newest part: the
    /// document is handed that ([`super::catch_up::hold_parts`]). Without it
    /// a document mounted here held its chains empty, so an agent listed,
    /// read and acted on no issue and no message of it. Whether an answer is
    /// still owed.
    pub(super) fn ask_chains(&mut self, workspace_id: WorkspaceId, needs: Needs<'_>) -> bool {
        let (Needs::Read { app_id, .. } | Needs::Act { app_id, .. }) = needs else {
            return false;
        };
        let Some(workspace) = self.workspaces.get_mut(&workspace_id) else {
            return false;
        };
        if !workspace.when_opened {
            return false;
        }
        let Some(cluster_id) = workspace.cluster_id(workspace_id) else {
            return false;
        };
        let state = workspace.state();
        let mut heads: Vec<DocumentRoute> = Vec::new();
        for listed in self.documents(Some(workspace_id), &state, &cluster_id) {
            if listed.app_id != app_id {
                continue;
            }
            let Some(chain) = chain_of(&listed.document) else {
                continue;
            };
            let head = DocumentRoute {
                document: chain.chain_head().as_str().to_owned(),
                ..listed
            };
            if !heads.contains(&head) {
                heads.push(head);
            }
        }
        let Some(workspace) = self.workspaces.get_mut(&workspace_id) else {
            return false;
        };
        workspace.ask_routes(workspace_id, heads)
    }

    /// What running `request` waits for: the record it acts on
    /// ([`invoke_keys`]), and for an open of an app, the record the open
    /// shows, which the workspace may hold and send only when asked. That is
    /// the record an open on the layout names, and for an app that keeps one
    /// record per workspace (Chat) the open of the app itself, whose record
    /// is that one: asked for and waited on before the open runs, so the open
    /// finds the record people use rather than taking the workspace for one
    /// without it. Waited on as a read: a record the workspace holds none of
    /// is no reason to refuse the open.
    pub(super) fn invoke_keys(&self, request: &InvokeRequest) -> Keys {
        // A file opened in Files is put into the workspace's tree, which is
        // read before it is written: asked for, and waited on until it opens
        // with the host's keys, as a read, since a workspace whose tree no one
        // wrote yet is founded by the put.
        if puts_into_the_tree(request) {
            return Keys::Record {
                app_id: tilo_fs::tree_document::FILES_APP.to_owned(),
                record: tilo_fs::tree_document::document_id(),
                act: false,
            };
        }
        match self.opened_resource(request) {
            Some((app_id, record)) => Keys::Record {
                app_id,
                record,
                act: false,
            },
            None => invoke_keys(request),
        }
    }

    /// The app and record an open of an app in `request` shows, when the
    /// record is one the workspace may already hold.
    pub(super) fn opened_resource(&self, request: &InvokeRequest) -> Option<(String, String)> {
        let target = &request.target;
        if let Some((_, app)) = application_provider(&target.provider) {
            return (target.address.action == APPLICATION_OPEN_ACTION).then(|| {
                let record = application_provider_resource(&target.provider).unwrap_or_default();
                (app.to_owned(), record.to_owned())
            });
        }
        provider_workspace(&target.provider)?;
        let app = target
            .address
            .resource
            .strip_prefix(APPLICATION_RESOURCE_PREFIX)?;
        let opens = target.address.action == APPLICATION_OPEN_ACTION
            || target.address.action == APPLICATION_START_ACTION;
        let keeps = self
            .catalogue
            .get(app)
            .is_some_and(ui::application::ApplicationDefinition::keeps_workspace_resource);
        (opens && keeps).then(|| (app.to_owned(), String::new()))
    }

    /// Whether the workspace `workspace_id` holds `route`, as far as its host
    /// answered: `Ok(false)` only for a record the host said it holds none of,
    /// or in a workspace that replayed everything it holds. A record the
    /// workspace may still send is asked for, and refused as on its way, so
    /// what an open would found is never founded over a record the workspace
    /// already has.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn held_or_owed(
        &mut self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
    ) -> Result<bool, Outcome> {
        let workspace = self.workspaces.get_mut(&workspace_id).ok_or_else(|| {
            Outcome::Rejected(ActionCallError::Unavailable {
                reason: "the workspace was not joined".into(),
            })
        })?;
        // Another person's own: the host never sends it here, so nothing waits for it.
        if workspace.not_yours.contains(route) {
            return Err(Outcome::Rejected(ActionCallError::NotPermitted));
        }
        // Held, or sent by the host under the route of what it names.
        if workspace.holds(route)
            || (workspace.answered.contains(route) && !workspace.absent.contains(route))
        {
            return Ok(true);
        }
        let needs = Needs::Read {
            app_id: &route.app_id,
            record: &route.document,
        };
        // Asked for now: the same call goes through once it arrived, which the caller waits for
        // as the workspace's state rather than a reason to read.
        if workspace.ask(workspace_id, needs) {
            return Err(Outcome::Rejected(ActionCallError::NotReady));
        }
        Ok(false)
    }

    /// The record at `route`, saved, when this host holds it, with the heads it stands at;
    /// `None` for one the workspace holds none of. A record the workspace may still send is
    /// refused as on its way, as [`Self::held_or_owed`] refuses it, `act` waiting for its keys
    /// every time as a write to it needs them. The heads are what a follower of the record
    /// asks the changes after, next time.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn saved_here(
        &mut self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
        act: bool,
    ) -> Result<Option<super::Saved>, Outcome> {
        let held = self.refreshed_for(workspace_id, route, act)?;
        match held.document(route) {
            Ok(saved) => Ok(Some(saved)),
            Err(_) if !self.held_or_owed(workspace_id, route)? => Ok(None),
            Err(why) => Err(Outcome::Rejected(ActionCallError::Unavailable {
                reason: why,
            })),
        }
    }

    /// The workspace as this member holds it, with what the document at
    /// `route` needs to be read (or, with `act`, changed) held first.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome is what the request is answered with"
    )]
    pub(super) fn refreshed_for(
        &mut self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
        act: bool,
    ) -> Result<super::WorkspaceState, Outcome> {
        let app_id = route.app_id.as_str();
        let record = route.document.as_str();
        let needs = if act {
            Needs::Act { app_id, record }
        } else {
            Needs::Read { app_id, record }
        };
        self.refresh(workspace_id, needs).map_err(Outcome::Rejected)
    }
}

/// Whether `request` opens files in Files: what puts them into the
/// workspace's tree ([`super::tree_put`]), one file or many at once.
fn puts_into_the_tree(request: &InvokeRequest) -> bool {
    let target = &request.target;
    if provider_workspace(&target.provider).is_none() {
        return false;
    }
    // One of Files' verbs by a path in the workspace reads the tree and writes it, as a put
    // does, and one of its reads by a path reads it.
    if super::tree_put::acts_on_tree(target) {
        return true;
    }
    let arguments = super::invoke_arguments(&request.argument);
    target.address.action == APPLICATION_OPEN_ACTION
        && target
            .address
            .resource
            .strip_prefix(APPLICATION_RESOURCE_PREFIX)
            == Some(tilo_fs::tree_document::FILES_APP)
        && (arguments.contains_key("files")
            || super::opened_file::OpenedFile::named(&arguments).is_ok_and(|file| file.is_some()))
}

#[cfg(test)]
mod tests {
    use super::{asked_routes, tree_chunks};
    use crate::workspace_actions::{GaveUp, Needs, sealed_for};
    use std::collections::BTreeSet;
    use tilo_peer_state::document_frame::DocumentRoute;
    use tilo_server_protocol::WorkspaceId;

    const WORKSPACE: &str = "6f1c1b62-4f5e-4b8a-9b52-1f8a3f0f3a11";

    fn route(app: &str, record: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: app.into(),
            cluster_id: WORKSPACE.into(),
            document: record.into(),
        }
    }

    /// The chat's chain: the record its numbered parts hang off.
    fn chain() -> ui::resource::DocumentId {
        ui::resource::DocumentId::derived(
            "messages",
            &ui::resource::DocumentId::workspace_default(),
        )
    }

    #[test]
    fn a_record_is_asked_for_by_its_own_id() {
        assert_eq!(
            asked_routes(WORKSPACE, "so.tilo.notes", "note"),
            [route("so.tilo.notes", "note")],
            "a record is sent when it is opened, and nothing else is asked for"
        );
    }

    #[test]
    fn a_trees_own_record_names_the_chunks_read_with_it() {
        let tree = route(
            fs_protocol::tree::FILES_APP,
            &fs_protocol::tree::document_id(),
        );
        assert!(
            tree_chunks(None, &tree).is_empty(),
            "nothing to read before the tree's own record is here"
        );
        let mut index = automerge::AutoCommit::new();
        for chunk in [1, 2] {
            fs_protocol::tree::list_chunk(&mut index, chunk).unwrap();
        }
        let index = index.save();
        assert_eq!(
            tree_chunks(Some(&index), &tree),
            [1, 2].map(|chunk| route(
                fs_protocol::tree::FILES_APP,
                &fs_protocol::tree::chunk_document_id(chunk)
            )),
            "the chunks the tree's own record lists are read with it"
        );
        assert!(
            tree_chunks(Some(&index), &route("so.tilo.chat", "")).is_empty(),
            "another app's record names no chunks"
        );
    }

    #[test]
    fn a_chain_is_asked_for_by_its_head_not_its_first_part() {
        let part = chain().part(2);
        let asked = asked_routes(WORKSPACE, "so.tilo.chat", part.as_str());
        assert_eq!(
            asked,
            [
                route("so.tilo.chat", part.as_str()),
                route("so.tilo.chat", chain().chain_head().as_str()),
                route("so.tilo.chat", ""),
            ],
            "a part is asked for beside its chain's head, whichever part that is, \
             and the record it is kept with"
        );
        assert!(
            !asked
                .iter()
                .any(|asked| asked.document == chain().part(0).as_str()),
            "the chain's first part is a guess at what it holds, not its head"
        );
    }

    #[test]
    fn reading_a_chain_waits_for_every_part_of_it() {
        let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
        let head = route("so.tilo.chat", chain().part(3).as_str());
        let asked = route("so.tilo.chat", chain().part(1).as_str());
        let records = route("so.tilo.chat", "");
        let routes = BTreeSet::from([
            head.clone(),
            records.clone(),
            route("so.tilo.notes", "note"),
        ]);
        let mut waited = sealed_for(
            &routes,
            workspace,
            Needs::Read {
                app_id: "so.tilo.chat",
                record: chain().part(1).as_str(),
            },
            &GaveUp::default(),
        );
        waited.sort_by(|a, b| a.document.cmp(&b.document));
        assert_eq!(
            waited,
            [records, head],
            "the head the host sends arrives on its own part's route, and is waited for"
        );
        assert_eq!(
            sealed_for(
                &routes,
                workspace,
                Needs::Act {
                    app_id: "so.tilo.chat",
                    record: asked.document.as_str(),
                },
                &GaveUp::default(),
            )
            .len(),
            2,
            "acting on a part waits for its chain too"
        );
    }
}

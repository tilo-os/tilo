//! The records the workspace sends this service only when it opens them.
//!
//! A public workspace — one listed in its host's directory, which anyone may
//! walk into — replays a joining device only its governance and its shared
//! desktop, and sends every other record once that device opens it, saying so
//! in the [`tilo_session_client::Event::HostReady`] that ends the replay. So a
//! record this service holds none of is not one the workspace lacks: it is
//! asked for ([`Cmd::OpenRoute`](tilo_session_client::Cmd::OpenRoute), which asks the host and delivers
//! what arrives as an ordinary change), and it is absent only once the host
//! answered it holds none of it ([`tilo_session_client::Event::RecordOpened`]
//! with no frames). Without that, every read and action about a record the
//! host had not replayed was refused as one the workspace has none of, and a
//! member's own records could neither be read nor acted on in the workspace
//! they wrote them in.

use super::keys::{Keys, invoke_keys};
use super::{DocumentRoute, Needs, Outcome, Worker, Workspace};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, APPLICATION_START_ACTION,
    ActionHostError, InvokeRequest, application_provider, application_provider_record,
    provider_workspace,
};
use tilo_relay_protocol::WorkspaceId;
use tilo_session_client::{Cmd, Outbox};

/// The chain `record` is a part of, when it is a part of one.
pub(super) fn chain_of(record: &str) -> Option<ui::record::RecordId> {
    let part = ui::record::RecordId::parse(record)?;
    part.chain_part().map(|(chain, _)| chain)
}

/// The records a request naming `record` of `app_id` asks the workspace's
/// host for, in the workspace whose channels are `cluster_id`: the record
/// itself, and for a part of a chain the chain's head
/// ([`ui::record::RecordId::chain_head`]), which names whichever part is
/// the newest rather than a guess at its oldest, beside the record a part
/// is kept with.
fn asked_routes(cluster_id: &str, app_id: &str, record: &str) -> Vec<DocumentRoute> {
    let route = |record: &str| DocumentRoute {
        app_id: app_id.to_owned(),
        cluster_id: cluster_id.to_owned(),
        record: record.to_owned(),
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

impl Workspace {
    /// Whether this member's replica holds `route`: readable, or sealed and waiting for the
    /// keys it opens with.
    pub(super) fn holds(&self, route: &DocumentRoute) -> bool {
        self.peer
            .engine()
            .records(&route.cluster_id)
            .is_ok_and(|records| {
                records
                    .iter()
                    .any(|(app, record)| *app == route.app_id && *record == route.record)
            })
    }

    /// Ask the workspace's host for the records `needs` reads that this
    /// member holds none of, and say whether an answer is still owed: the
    /// host's answer is the session's change that wakes the request to look
    /// again, and the wait is the one the request's keys get, so a host that
    /// never answers ends it. A workspace that sends its records when they
    /// are opened sends nothing else, so a record not here is one the
    /// workspace may still hold: it is asked for rather than taken as absent,
    /// and is absent only once the host answered it holds none of it
    /// ([`tilo_session_client::Event::RecordOpened`] with no frames).
    pub(super) fn ask(&mut self, workspace_id: WorkspaceId, needs: Needs<'_>) -> bool {
        if !self.when_opened {
            return false;
        }
        self.ask_for(workspace_id, needs)
    }

    /// [`Self::ask`] whether or not the workspace replayed everything it held when this member
    /// joined: a record made after that is sent to a member only once it asked for it.
    pub(super) fn ask_for(&mut self, workspace_id: WorkspaceId, needs: Needs<'_>) -> bool {
        let (Needs::Read { app_id, record } | Needs::Act { app_id, record }) = needs else {
            return false;
        };
        let Some(cluster_id) = self.cluster_id(workspace_id) else {
            return false;
        };
        let mut owing = false;
        for route in asked_routes(&cluster_id, app_id, record) {
            if !self.holds(&route)
                && !self.answered.contains(&route)
                && !self.asked.contains(&route)
            {
                match self.peer.send(Cmd::OpenRoute(route.clone())) {
                    Ok(()) => {
                        tracing::debug!(
                            app = %route.app_id,
                            record = %route.record,
                            "workspace actions: asked the workspace for a record this member holds none of"
                        );
                        self.asked.insert(route.clone());
                    }
                    Err(error) => tracing::warn!(
                        %error,
                        app = %route.app_id,
                        record = %route.record,
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
                app_id: tilo_fs::tree_record::FILES_APP.to_owned(),
                record: tilo_fs::tree_record::record(),
                act: false,
            };
        }
        // An app's open of the workspace's own provider shows its record on the layout.
        let on_layout = application_provider(&request.target.provider).is_some();
        match self.opened_record(request) {
            Some((app_id, record)) if on_layout => Keys::Shown { app_id, record },
            Some((app_id, record)) => Keys::Record {
                app_id,
                record,
                act: false,
            },
            None => invoke_keys(request),
        }
    }

    /// Whether the layout of `workspace_id` that `keys` shows a record on has opened here, or the
    /// workspace's host answered it holds none. The layout is asked for whatever the workspace
    /// replayed: a page writes it once it opens the workspace, often after this member joined,
    /// and the host sends a record made since only to a member that asked for it. Then it is
    /// handed its keys as the record a request reads is ([`Worker::opened`]).
    pub(super) fn layout_opened(&mut self, workspace_id: WorkspaceId, keys: &Keys) -> bool {
        if !matches!(keys, Keys::Shown { .. }) {
            return true;
        }
        let Some(workspace) = self.workspaces.get_mut(&workspace_id) else {
            return true;
        };
        let Some(cluster_id) = workspace.cluster_id(workspace_id) else {
            return true;
        };
        let layout = crate::participant::desktop_route(&cluster_id);
        let needs = Needs::Read {
            app_id: &layout.app_id,
            record: &layout.record,
        };
        if workspace.ask_for(workspace_id, needs) {
            return false;
        }
        self.opened(workspace_id, needs)
    }

    /// The app and record an open of an app in `request` shows, when the
    /// record is one the workspace may already hold.
    fn opened_record(&self, request: &InvokeRequest) -> Option<(String, String)> {
        let target = &request.target;
        if let Some((_, app)) = application_provider(&target.provider) {
            return (target.address.action == APPLICATION_OPEN_ACTION).then(|| {
                let record = application_provider_record(&target.provider).unwrap_or_default();
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
            .is_some_and(ui::application::ApplicationDefinition::keeps_workspace_record);
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
            Outcome::Rejected(ActionHostError::Unavailable {
                reason: "the workspace was not joined".into(),
            })
        })?;
        // Another person's own: the host never sends it here, so nothing waits for it.
        if workspace.not_yours.contains(route) {
            return Err(Outcome::Rejected(ActionHostError::NotPermitted));
        }
        // Held, or sent by the host under the route of what it names.
        if workspace.holds(route)
            || (workspace.answered.contains(route) && !workspace.absent.contains(route))
        {
            return Ok(true);
        }
        let needs = Needs::Read {
            app_id: &route.app_id,
            record: &route.record,
        };
        // Asked for now: the same call goes through once it arrived, which the caller waits for
        // as the workspace's state rather than a reason to read.
        if workspace.ask(workspace_id, needs) {
            return Err(Outcome::Rejected(ActionHostError::NotReady));
        }
        Ok(false)
    }
}

/// Whether `request` opens a file in Files: what puts it into the workspace's
/// tree ([`super::tree_put`]).
fn puts_into_the_tree(request: &InvokeRequest) -> bool {
    let target = &request.target;
    provider_workspace(&target.provider).is_some()
        && target.address.action == APPLICATION_OPEN_ACTION
        && target
            .address
            .resource
            .strip_prefix(APPLICATION_RESOURCE_PREFIX)
            == Some(tilo_fs::tree_record::FILES_APP)
        && super::opened_file::OpenedFile::named(&super::invoke_arguments(&request.argument))
            .is_ok_and(|file| file.is_some())
}

#[cfg(test)]
mod tests {
    use super::asked_routes;
    use crate::workspace_actions::{GaveUp, Needs, sealed_for};
    use std::collections::BTreeSet;
    use tilo_peer_state::document_frame::DocumentRoute;
    use tilo_relay_protocol::WorkspaceId;

    const WORKSPACE: &str = "6f1c1b62-4f5e-4b8a-9b52-1f8a3f0f3a11";

    fn route(app: &str, record: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: app.into(),
            cluster_id: WORKSPACE.into(),
            record: record.into(),
        }
    }

    /// The chat's chain: the record its numbered parts hang off.
    fn chain() -> ui::record::RecordId {
        ui::record::RecordId::derived("messages", &ui::record::RecordId::workspace_default())
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
                .any(|asked| asked.record == chain().part(0).as_str()),
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
        waited.sort_by(|a, b| a.record.cmp(&b.record));
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
                    record: asked.record.as_str(),
                },
                &GaveUp::default(),
            )
            .len(),
            2,
            "acting on a part waits for its chain too"
        );
    }
}

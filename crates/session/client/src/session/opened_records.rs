//! The records this session opened: those a workspace sends a device only
//! once it asks ([`Task::open_resource`]), and the host's answer that it sent
//! all it holds of one ([`Task::resource_opened`]). In a workspace that sends
//! every record when opened (a public one), each route this session opens
//! is asked for that way ([`Task::documents_when_opened`]).

use super::{DocumentRoute, Envelope, ServerClient, Task};
use tilo_server_protocol::WorkspaceId;

impl Task {
    /// Open the on-demand record `route` of `workspace_id`, and ask for its
    /// keys at the same time. Returns `false` when the session could not
    /// take the request.
    pub(super) fn open_resource(
        &mut self,
        workspace_id: WorkspaceId,
        route: DocumentRoute,
        client: Option<&ServerClient>,
    ) -> bool {
        let channel = neighbour_wire::app_document_channel(&route.app_id, &route.document);
        // A chain's head names a part and holds nothing of its own: the
        // part's keys are asked for as it arrives, like a replayed head's.
        if !matches!(
            neighbour_wire::document_sync(&route.document),
            neighbour_wire::DocumentSync::ChainHead { .. }
        ) {
            self.open_channel(&route);
        }
        let own = self.own_channel(&route);
        self.opened_records.insert((workspace_id, channel), route);
        client.is_none_or(|client| {
            client
                .send(Envelope::OpenDocument {
                    workspace_id,
                    channel,
                    own,
                })
                .is_ok()
        })
    }

    /// Open `route` as a record ([`Self::open_resource`]) unless this session
    /// opened it already; the shared desktop is replayed to every device
    /// and is never asked for.
    pub(super) fn open_record_once(
        &mut self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
        client: Option<&ServerClient>,
    ) -> bool {
        let channel = neighbour_wire::app_document_channel(&route.app_id, &route.document);
        if self.opened_records.contains_key(&(workspace_id, channel))
            || (route.app_id == tilo_desktop_model::ownership::DESKTOP_APP
                && route.document == tilo_desktop_model::ownership::DESKTOP_DOCUMENT)
        {
            return true;
        }
        self.open_resource(workspace_id, route.clone(), client)
    }

    /// `workspace_id`'s host said it sends records only when they are opened
    /// (its `Ready`): every route opened in it so far is opened as a record.
    pub(super) fn documents_when_opened(
        &mut self,
        workspace_id: WorkspaceId,
        client: Option<&ServerClient>,
    ) {
        if !self.when_opened.insert(workspace_id) {
            return;
        }
        let routes: Vec<DocumentRoute> = self
            .opened_routes
            .iter()
            .filter(|route| route.workspace_id().ok() == Some(workspace_id))
            .cloned()
            .collect();
        for route in routes {
            self.open_record_once(workspace_id, &route, client);
        }
    }

    /// `route` as this session's person's own record, which the host is told when
    /// the record is opened: another person's own is answered as not theirs, written or not.
    pub(super) fn own_channel(
        &self,
        route: &DocumentRoute,
    ) -> Option<tilo_server_protocol::OwnedChannel> {
        tilo_server_protocol::OwnedChannel::recognise(
            &route.app_id,
            &route.document,
            self.config.cert.user_public_key,
        )
    }

    /// Open every record this session opened again on a new subscription.
    /// Returns `false` when the session could not take the requests.
    pub(super) fn reopen_records(&self, client: &ServerClient) -> bool {
        let mut sent_ok = true;
        for ((workspace_id, channel), route) in &self.opened_records {
            sent_ok &= client
                .send(Envelope::OpenDocument {
                    workspace_id: *workspace_id,
                    channel: *channel,
                    own: self.own_channel(route),
                })
                .is_ok();
        }
        sent_ok
    }

    /// The host answered a record this page opened: it sent everything it holds of it, or it is
    /// not this page's.
    pub(super) fn resource_opened(
        &mut self,
        workspace_id: WorkspaceId,
        channel: [u8; 32],
        answer: tilo_server_protocol::DocumentAnswer,
    ) {
        // Its frames are in the replica before anyone hears the record opened.
        self.keep_arrived();
        if let Some(route) = self.opened_records.get(&(workspace_id, channel)) {
            self.reports.emit(&crate::Event::DocumentOpened {
                ws: workspace_id,
                channel,
                route: Some(route.clone()),
                answer,
            });
        }
    }
}

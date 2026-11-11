//! The records this session opened: those a workspace sends a device only
//! once it asks ([`Task::open_record`]), and the host's answer that it sent
//! all it holds of one ([`Task::record_opened`]). In a workspace that sends
//! every record when opened (a public one), each route this session opens
//! is asked for that way ([`Task::records_when_opened`]).

use super::{DocumentRoute, Envelope, PeerEvent, RelayClient, Task};

impl Task {
    /// Open the on-demand record `route` of `workspace_id`, and ask for its
    /// keys at the same time. Returns `false` when the session could not
    /// take the request.
    pub(super) fn open_record(
        &mut self,
        workspace_id: u64,
        route: DocumentRoute,
        client: Option<&RelayClient>,
    ) -> bool {
        let channel = neighbour_wire::app_document_channel(&route.app_id, &route.record);
        self.open_channel(&route);
        self.opened_records.insert(channel, (workspace_id, route));
        client.is_none_or(|client| {
            client
                .send(Envelope::OpenRecord {
                    workspace_id,
                    channel,
                })
                .is_ok()
        })
    }

    /// Open `route` as a record ([`Self::open_record`]) unless this session
    /// opened it already; the shared desktop is replayed to every device
    /// and is never asked for.
    pub(super) fn open_record_once(
        &mut self,
        workspace_id: u64,
        route: &DocumentRoute,
        client: Option<&RelayClient>,
    ) -> bool {
        let channel = neighbour_wire::app_document_channel(&route.app_id, &route.record);
        if self.opened_records.contains_key(&channel)
            || (route.app_id == tilo_desktop_model::ownership::DESKTOP_APP
                && route.record == tilo_desktop_model::ownership::DESKTOP_RECORD)
        {
            return true;
        }
        self.open_record(workspace_id, route.clone(), client)
    }

    /// `workspace_id`'s host said it sends records only when they are opened
    /// (its `Ready`): every route opened in it so far is opened as a record.
    pub(super) fn records_when_opened(&mut self, workspace_id: u64, client: Option<&RelayClient>) {
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

    /// The host sent everything it holds of a record this page opened.
    pub(super) fn record_opened(&mut self, workspace_id: u64, channel: [u8; 32], frames: u32) {
        if let Some(route) = self.owed_replays.remove(&channel) {
            let artifact =
                collab_types::document_artifact_id(&route.app_id, &route.cluster_id, &route.record);
            let given_up = self
                .held_inbound
                .get_mut(&route)
                .map(|held| held.give_up_owed(artifact))
                .unwrap_or_default();
            for owed in given_up {
                let error = format!("the host's replay has no document {owed}");
                self.refused = (self.refused.0.saturating_add(1), error.clone());
                self.emit(&PeerEvent::DocumentRejected(error));
            }
        }
        if let Some((_, route)) = self.opened_records.get(&channel) {
            self.emit(&PeerEvent::RecordOpened {
                workspace_id,
                route: route.clone(),
                frames,
            });
        }
    }
}

//! The records the workspace sends this service only when it opens them.
//!
//! A public workspace — one listed in its host's directory, which anyone may
//! walk into — replays a joining device only its governance and its shared
//! desktop, and sends every other record once that device opens it, saying so
//! in the [`tilo_session_client::Event::HostReady`] that ends the replay. So a
//! record this service holds none of is not one the workspace lacks: it is
//! asked for ([`PeerHandle::open_route`], which asks the host and delivers
//! what arrives as an ordinary change), and it is absent only once the host
//! answered it holds none of it ([`tilo_session_client::Event::RecordOpened`]
//! with no frames). Without that, every read and action about a record the
//! host had not replayed was refused as one the workspace has none of, and a
//! member's own records could neither be read nor acted on in the workspace
//! they wrote them in.

use super::{DocumentRoute, Needs, Workspace};

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
    pub(super) fn ask(&mut self, workspace_id: u64, needs: Needs<'_>) -> bool {
        if !self.when_opened {
            return false;
        }
        let (Needs::Read { app_id, record } | Needs::Act { app_id, record }) = needs else {
            return false;
        };
        let Some(cluster_id) = self.cluster_id(workspace_id) else {
            return false;
        };
        let mut owing = false;
        for route in asked_routes(&cluster_id, app_id, record) {
            if !self.holds(&route) && !self.absent.contains(&route) && !self.asked.contains(&route)
            {
                match self.peer.open_route(route.clone()) {
                    Ok(()) => {
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

#[cfg(test)]
mod tests {
    use super::asked_routes;
    use crate::workspace_actions::{GaveUp, Needs, sealed_for};
    use std::collections::BTreeSet;
    use tilo_peer_state::document_frame::DocumentRoute;

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
        let workspace = tilo_relay_protocol::room_workspace_id(WORKSPACE).unwrap();
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

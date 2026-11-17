//! The same record of two workspaces, opened on one session: each workspace's
//! answer names the record of that workspace.
//!
//! A record's retained channel is the digest of its app and its id; which
//! workspace it is of is the session's to keep. A session that kept what it
//! opened by channel alone kept one record per channel, the one opened last:
//! a host's device, whose one session serves every workspace it hosts, opened
//! the layout of two workspaces and heard the first one's answer as the
//! second's, so whoever waited for the first never heard it answered; and
//! after a reconnect only the last was opened again. A workspace its owner
//! deleted is followed no more, and its records are not opened again.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
use tilo_peer_state::owner::testing::{LateAdmission, late_admission_at};
use tilo_relay_protocol::DocumentAnswer;

#[test]
fn the_same_record_opened_in_two_workspaces_is_answered_as_each_ones_own() {
    let LateAdmission {
        bootstrap,
        device_key,
        ..
    } = late_admission_at(web_time::SystemTime::now());
    let host = bootstrap.host;
    let config = PeerConfig {
        host: tilo_relay_client::Host::through(
            iroh_base::EndpointAddr::new(
                iroh_base::EndpointId::from_bytes(&host.device_public_key).unwrap(),
            ),
            None,
        ),
        cert: bootstrap.device_cert,
        host_cert: host,
        revocations: RevocationList::new(),
        entering: None,
    };
    let (mut task, link, _commands) = session_task(
        config,
        Arc::new(device_key),
        Arc::new(crate::MemoryPeerStore::default()),
        None,
        false,
    );
    let events = link.events();
    let layout = |workspace: WorkspaceId| DocumentRoute {
        app_id: DESKTOP_APP.to_owned(),
        cluster_id: workspace.to_string(),
        document: DESKTOP_DOCUMENT.to_owned(),
    };
    let (first, second) = (WorkspaceId::from_u128(1), WorkspaceId::from_u128(2));
    // Both are asked for before either is answered, as two workspaces' requests
    // arriving together ask.
    assert!(task.open_resource(first, layout(first), None));
    assert!(task.open_resource(second, layout(second), None));
    assert_eq!(
        task.opened_records.len(),
        2,
        "each is opened again after a reconnect"
    );

    let channel = neighbour_wire::app_document_channel(DESKTOP_APP, DESKTOP_DOCUMENT);
    for workspace in [first, second] {
        task.resource_opened(workspace, channel, DocumentAnswer::Sent { frames: 0 });
    }
    assert_eq!(
        answered(&events),
        [(first, layout(first)), (second, layout(second))],
        "each workspace hears its own layout answered"
    );

    // The first workspace's owner deletes it: the session opens its layout on
    // no later subscription, and the second's as before.
    task.inbound(Envelope::NotAuthorised {
        workspace_id: first,
        refusal: tilo_relay_protocol::refusal::Refusal::WorkspaceDeleted,
    });
    assert_eq!(task.opened_records.len(), 1);
    for workspace in [first, second] {
        task.resource_opened(workspace, channel, DocumentAnswer::Sent { frames: 0 });
    }
    assert_eq!(answered(&events), [(second, layout(second))]);
}

/// The records `events` heard answered since it was last asked, each with the
/// workspace the host answered for.
fn answered(events: &crate::Events) -> Vec<(WorkspaceId, DocumentRoute)> {
    events
        .drain()
        .into_iter()
        .filter_map(|event| match event {
            crate::Event::DocumentOpened {
                ws,
                route: Some(route),
                ..
            } => Some((ws, route)),
            _ => None,
        })
        .collect()
}

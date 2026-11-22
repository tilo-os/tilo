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
//! deleted, or that took this person out, is followed no more, and its records
//! are not opened again.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
use tilo_peer_state::owner::testing::{LateAdmission, late_admission_at};
use tilo_server_protocol::DocumentAnswer;

/// A session of a device the workspace's server admitted late, as a page's is.
fn session() -> (Task, Link, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let LateAdmission {
        bootstrap,
        device_key,
        ..
    } = late_admission_at(web_time::SystemTime::now());
    let host = bootstrap.host;
    let config = PeerConfig {
        host: tilo_server_client::ServerAddress::through(
            iroh_base::EndpointAddr::new(
                iroh_base::EndpointId::from_bytes(&host.device_public_key).unwrap(),
            ),
            None,
        ),
        cert: bootstrap.device_cert,
        server_cert: host,
        revocations: RevocationList::new(),
        entering: None,
    };
    session_task(
        config,
        Arc::new(device_key),
        Arc::new(crate::MemoryPeerStore::default()),
        None,
        false,
    )
}

#[test]
fn the_same_record_opened_in_two_workspaces_is_answered_as_each_ones_own() {
    let (mut task, link, _commands) = session();
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

    let channel = neighbour_wire::app_document_group(DESKTOP_APP, DESKTOP_DOCUMENT);
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
        refusal: tilo_server_protocol::refusal::Refusal::WorkspaceDeleted,
    });
    assert_eq!(task.opened_records.len(), 1);
    for workspace in [first, second] {
        task.resource_opened(workspace, channel, DocumentAnswer::Sent { frames: 0 });
    }
    assert_eq!(answered(&events), [(second, layout(second))]);
}

/// A person taken out of a workspace is let into it no more: the session follows it no longer,
/// and once it holds no other workspace it ends as not admitted, which the page turns them away
/// on. A permission they could still ask for stays a notice.
#[test]
fn a_person_taken_out_of_a_workspace_is_told_they_are_no_longer_a_member() {
    use tilo_server_protocol::refusal::Refusal;

    let (mut task, link, _commands) = session();
    let (lounge, own) = (WorkspaceId::from_u128(1), WorkspaceId::from_u128(2));
    task.workspaces.extend([lounge, own]);

    task.inbound(Envelope::NotAuthorised {
        workspace_id: own,
        refusal: Refusal::NotGranted("invite".to_owned()),
    });
    let status = link.status();
    assert_ne!(status.phase, TransportPhase::Failed);
    assert!(
        status
            .failures
            .iter()
            .any(|failure| failure.code == "permission-required")
    );

    task.inbound(Envelope::NotAuthorised {
        workspace_id: lounge,
        refusal: Refusal::NotAMember("removed".to_owned()),
    });
    assert!(
        !task.workspaces.contains(&lounge),
        "the Lounge is followed no more"
    );
    assert_ne!(
        link.status().phase,
        TransportPhase::Failed,
        "another workspace still holds the session"
    );

    task.inbound(Envelope::NotAuthorised {
        workspace_id: own,
        refusal: Refusal::NotAMember("removed".to_owned()),
    });
    let status = link.status();
    assert_eq!(
        status.phase,
        TransportPhase::Failed,
        "nothing is left to follow"
    );
    assert!(
        status
            .failures
            .iter()
            .any(|failure| failure.code == "not-admitted")
    );
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

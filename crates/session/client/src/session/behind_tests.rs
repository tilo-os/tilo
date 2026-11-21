//! A document this device is behind in is asked of its workspace again after a bounded time,
//! and said: it waits for nothing in silence.
//!
//! A device's replica refuses a compaction that covers writes it does not hold, and a device
//! that could not take it with its keys either reads nothing written to the document after it.
//! Until a compaction of the document is taken, the session says so where it says what it holds
//! undecided, and asks its workspace for the document again: after a quarter of a minute, then
//! twice as long each time, never longer than five minutes.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;
use tilo_peer_state::owner::testing::{LateAdmission, late_admission_at};

const WHY: &str = "sealed compaction covers writes this replica has not admitted";

#[test]
fn a_document_this_device_is_behind_in_is_asked_for_again_until_its_compaction_is_taken() {
    let LateAdmission {
        bootstrap,
        device_key,
        ..
    } = late_admission_at(web_time::SystemTime::now());
    let server = bootstrap.host;
    let config = PeerConfig {
        host: tilo_server_client::ServerAddress::through(
            iroh_base::EndpointAddr::new(
                iroh_base::EndpointId::from_bytes(&server.device_public_key).unwrap(),
            ),
            None,
        ),
        cert: bootstrap.device_cert,
        server_cert: server,
        revocations: RevocationList::new(),
        entering: None,
    };
    let (mut task, _link, _commands) = session_task(
        config,
        Arc::new(device_key),
        Arc::new(crate::MemoryPeerStore::default()),
        None,
        false,
    );
    let workspace = WorkspaceId::from_u128(1);
    let conversation = DocumentRoute {
        app_id: "so.tilo.chat".to_owned(),
        cluster_id: workspace.to_string(),
        document: "messages".to_owned(),
    };
    let asked = |task: &mut Task| {
        std::mem::take(&mut task.replays)
            .into_iter()
            .collect::<Vec<_>>()
    };
    // What its workspace names the document by when it answers.
    let named = neighbour_wire::app_document_group(&conversation.app_id, &conversation.document);

    // Refused at second one: said at once, asked for again a quarter of a minute later and
    // not before.
    task.fell_behind(&conversation, WHY, 1_000);
    assert!(
        task.describe_keys().contains("behind a compaction"),
        "{}",
        task.describe_keys()
    );
    task.ask_behind_again(1_000 + BEHIND_ASKED_AFTER_MS - 1);
    assert!(asked(&mut task).is_empty());
    task.ask_behind_again(1_000 + BEHIND_ASKED_AFTER_MS);
    assert_eq!(asked(&mut task), std::slice::from_ref(&conversation));

    // The workspace sent the document again and the compaction was refused again: it goes on
    // asking, each time after twice as long and never after longer than the bound.
    let mut now = 1_000 + BEHIND_ASKED_AFTER_MS;
    let mut waits = Vec::new();
    for _ in 0..8 {
        task.fell_behind(&conversation, WHY, now);
        task.answered_behind(workspace, named);
        let due = task.behind[&conversation].next_ms;
        waits.push(due - now);
        task.ask_behind_again(due - 1);
        assert!(asked(&mut task).is_empty());
        task.ask_behind_again(due);
        assert_eq!(asked(&mut task), std::slice::from_ref(&conversation));
        now = due;
    }
    assert!(waits.windows(2).all(|pair| pair[0] <= pair[1]), "{waits:?}");
    assert_eq!(
        waits.first(),
        Some(&(2 * BEHIND_ASKED_AFTER_MS)),
        "{waits:?}"
    );
    assert_eq!(
        waits.last(),
        Some(&BEHIND_ASKED_AT_MOST_EVERY_MS),
        "{waits:?}"
    );

    // A compaction of it is taken: nothing is asked any more, and nothing is said of it.
    task.caught_up(&conversation);
    task.ask_behind_again(u64::MAX);
    assert!(asked(&mut task).is_empty());
    assert!(!task.describe_keys().contains("behind a compaction"));

    // One the workspace sends again with no compaction refused this time is behind no more
    // either: what was refused is no longer what the workspace holds.
    task.fell_behind(&conversation, WHY, 0);
    task.ask_behind_again(BEHIND_ASKED_AFTER_MS);
    assert_eq!(asked(&mut task), std::slice::from_ref(&conversation));
    task.answered_behind(workspace, named);
    task.ask_behind_again(u64::MAX);
    assert!(asked(&mut task).is_empty());
}

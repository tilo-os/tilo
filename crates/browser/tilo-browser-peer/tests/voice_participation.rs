//! A browser session's part in a governed call, through the session the
//! page runs (`PeerHandle`): it lists the room's calls, joins one and keeps
//! the participation across a reconnect, and leaving ends its grant: the
//! others' roster shrinks and its token admits no carrier.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "support/call.rs"]
mod call;
mod support;

// This binary re-execs as the automerge worker (support is shared with crates that lack the dep).
tilo_automerge_isolate::test_worker!();

use tilo_browser_peer::{Host, PeerEvent, PeerHandle, VoiceEvent};
use tilo_moq::client::Status;
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::voice::{CallOperation, VoiceGrant};

use call::{GovernedCall, carrier, native, operate, until};
use support::{ready, start_peer, wait_for};

/// The next call event of `peer` that `pick` accepts.
async fn voice<T>(
    peer: &PeerHandle,
    pending: &mut Vec<PeerEvent>,
    what: &str,
    mut pick: impl FnMut(&VoiceEvent) -> Option<T>,
) -> T {
    wait_for(what, || {
        pending.extend(peer.drain());
        let found = pending
            .iter()
            .enumerate()
            .find_map(|(index, event)| match event {
                PeerEvent::Voice(event) => pick(event).map(|value| (index, value)),
                _ => None,
            });
        let found = found.map(|(index, value)| {
            pending.drain(..=index);
            value
        });
        async move { found }
    })
    .await
}

fn granted(event: &VoiceEvent) -> Option<VoiceGrant> {
    match event {
        VoiceEvent::Granted { grant, .. } => Some((**grant).clone()),
        _ => None,
    }
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay fixture is consumed by shutdown after the final grant assertion; dropping it at Clippy's suggested location uses a moved value"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_browser_session_lists_joins_keeps_and_leaves_a_governed_call() {
    let room = GovernedCall::start().await;
    let workspace_id = room.workspace_id;
    let (_bob, page) = room.invite("bob-browser").await;
    let peer = start_peer(
        Host::Https(room.https()),
        page.cert.clone(),
        room.host.cert.clone(),
        page.key.clone(),
    );
    peer.join(workspace_id).unwrap();
    ready(&[&peer]).await;
    let mut pending = Vec::new();

    // The room lists its call: active, on the chat's voice channel, empty.
    peer.voice_calls(workspace_id).unwrap();
    let calls = voice(&peer, &mut pending, "the call list", |event| match event {
        VoiceEvent::Calls { result, .. } => Some(result.clone()),
        _ => None,
    })
    .await
    .unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].state, room.call);
    // Exactly what the shared voice controls look up to join
    // (`voice-controls.mjs`): Chat's app, its record, and the voice
    // channel's own id, the one the chat minted, active.
    assert!(calls[0].state.active);
    assert_eq!(calls[0].app_id, call::CHAT);
    assert_eq!(calls[0].record_id, "");
    assert_eq!(calls[0].channel_id, room.channel);
    assert!(calls[0].participants.is_empty());

    // Alice joins from her laptop, Bob through his page's session.
    room.owner
        .send(Envelope::VoiceJoin {
            workspace_id,
            call: room.call.call,
            participation: None,
        })
        .unwrap();
    let alice_alone = native(&room.owner, |envelope| match envelope {
        Envelope::VoiceGranted { grant, .. } => Some(*grant),
        _ => None,
    })
    .await;
    peer.voice_join(workspace_id, room.call.call).unwrap();
    let bob = voice(&peer, &mut pending, "Bob's grant", granted).await;
    assert_eq!(bob.participants.len(), 2);
    assert_eq!(
        bob.subscriptions,
        vec![alice_alone.publication.clone().unwrap()]
    );
    let alice = native(&room.owner, |envelope| match envelope {
        Envelope::VoiceGranted { grant, .. } if grant.participants.len() == 2 => Some(*grant),
        _ => None,
    })
    .await;
    assert_eq!(alice.subscriptions, vec![bob.publication.clone().unwrap()]);
    // The page watches the list since it first asked: each join pushed a
    // new one, so the list naming Bob may follow one with Alice alone.
    peer.voice_calls(workspace_id).unwrap();
    let listed = voice(
        &peer,
        &mut pending,
        "the call list with Bob",
        |event| match event {
            VoiceEvent::Calls {
                result: Ok(calls), ..
            } if calls[0].participants.len() == 2 => Some(calls.clone()),
            _ => None,
        },
    )
    .await;
    assert_eq!(listed[0].participants.len(), 2);

    // Bob's grant admits his carrier.
    let bob_carrier = carrier(&bob, 1);
    until("Bob's carrier", &bob_carrier, |status| {
        status == Status::Live
    })
    .await;
    bob_carrier.close();

    // A dropped session resumes the same participation: the host keeps Bob
    // in the call and answers the resumed join with his grant.
    peer.reconnect().unwrap();
    let resumed = voice(&peer, &mut pending, "Bob's resumed grant", granted).await;
    assert_eq!(resumed.participation, bob.participation);
    assert_eq!(resumed.participants.len(), 2);

    // Bob leaves: Alice's roster shrinks and his token admits no carrier.
    peer.voice_leave(workspace_id).unwrap();
    let alone_again = native(&room.owner, |envelope| match envelope {
        Envelope::VoiceGranted { grant, .. } if grant.participants.len() == 1 => Some(*grant),
        _ => None,
    })
    .await;
    assert!(alone_again.subscriptions.is_empty());
    let stale = carrier(&resumed, 1);
    until("Bob's stale carrier", &stale, |status| {
        matches!(status, Status::Failed | Status::Denied | Status::Closed)
    })
    .await;

    // Bob joins again; when Alice ends the call his session reports it ended.
    peer.voice_join(workspace_id, room.call.call).unwrap();
    voice(&peer, &mut pending, "Bob's second grant", granted).await;
    operate(
        &room.owner,
        workspace_id,
        2,
        CallOperation::End {
            call: room.call.call,
            epoch: room.call.epoch,
        },
    )
    .await
    .unwrap();
    let reason = voice(
        &peer,
        &mut pending,
        "the end of Bob's call",
        |event| match event {
            VoiceEvent::Ended { reason, .. } => Some(reason.clone()),
            _ => None,
        },
    )
    .await;
    assert!(reason.contains("ended"), "{reason}");
    peer.voice_calls(workspace_id).unwrap();
    let ended = voice(
        &peer,
        &mut pending,
        "the call list after the end",
        |event| match event {
            VoiceEvent::Calls {
                result: Ok(calls), ..
            } if !calls[0].state.active => Some(calls.clone()),
            _ => None,
        },
    )
    .await;
    assert!(!ended[0].state.active);
    assert!(ended[0].participants.is_empty());
    peer.close();
    room.shutdown().await;
}

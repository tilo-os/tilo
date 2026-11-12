//! A call in a workspace the host governs, between the workspace's owner on a native
//! iroh device and an invited person in a browser session: both get their
//! grants from the host, see each other in the roster, and lose their voice
//! when the owner denies it or ends the call.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::voice::{CallOperation, Directions};

use crate::call::{GovernedCall, browser, granted, native, operate};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invited_browser_and_the_owners_device_take_part_in_a_governed_call() {
    // Alice starts a call on her chat's voice channel; only the workspace's
    // owner may. Bob, a different person, redeems her invitation with his
    // browser's device (over iroh: pages have no redemption route yet).
    let workspace = GovernedCall::start().await;
    let (bob, page) = workspace.invite("bob-browser").await;
    let alice_link = &workspace.owner;
    let started = &workspace.call;
    alice_link
        .send(Envelope::VoiceJoin {
            workspace_id: workspace.workspace_id,
            call: started.call,
            participation: None,
        })
        .unwrap();
    let alone = native(alice_link, granted).await;
    assert_eq!(alone.participants.len(), 1);
    assert!(alone.directions.publish && alone.directions.subscribe);

    // Bob's browser joins the host's session; as a guest it may not end the call.
    let page_link = workspace.session(&page.cert, &page.key).await;
    page_link
        .send(Envelope::Subscribe {
            workspace_id: workspace.workspace_id,
        })
        .unwrap();
    browser(&page_link, |envelope| match envelope {
        Envelope::Ready { .. } => Some(()),
        Envelope::NotAuthorised { reason, .. } => panic!("browser not admitted: {reason}"),
        _ => None,
    })
    .await;
    page_link
        .send(Envelope::VoiceCall {
            workspace_id: workspace.workspace_id,
            request: [2; 16],
            operation: CallOperation::End {
                call: started.call,
                epoch: started.epoch,
            },
        })
        .unwrap();
    let intruder = browser(&page_link, |envelope| match envelope {
        Envelope::VoiceCallResult { result, .. } => Some(result),
        _ => None,
    })
    .await;
    assert!(intruder.is_err(), "a guest cannot end the call");
    page_link
        .send(Envelope::VoiceJoin {
            workspace_id: workspace.workspace_id,
            call: started.call,
            participation: None,
        })
        .unwrap();
    let bob_grant = browser(&page_link, granted).await;
    assert!(bob_grant.directions.publish && bob_grant.directions.subscribe);
    let alice_grant = native(alice_link, |envelope| {
        granted(envelope).filter(|grant| grant.participants.len() == 2)
    })
    .await;
    assert_eq!(alice_grant.call, started.call);
    assert_eq!(
        bob_grant.participants.len(),
        2,
        "each side sees the whole roster"
    );

    // Alice mutes Bob: his next grant may only hear, and Alice sees him as
    // one who does not publish.
    let muted = operate(
        alice_link,
        workspace.workspace_id,
        3,
        CallOperation::SetDenial {
            call: started.call,
            epoch: started.epoch,
            person: bob.verifying_key().to_bytes(),
            denied: Directions {
                publish: true,
                subscribe: false,
            },
        },
    )
    .await
    .unwrap();
    assert!(muted.epoch > started.epoch);
    let listening = browser(&page_link, |envelope| {
        granted(envelope).filter(|grant| !grant.directions.publish)
    })
    .await;
    assert!(listening.directions.subscribe);
    let speaking = native(alice_link, |envelope| {
        granted(envelope).filter(|grant| {
            grant
                .participants
                .iter()
                .any(|participant| !participant.publishes)
        })
    })
    .await;
    assert!(
        speaking
            .participants
            .iter()
            .any(|participant| participant.user_public_key == bob.verifying_key().to_bytes()),
        "Alice sees Bob, silenced"
    );

    // Alice ends the call: Bob is told, and joining again is refused.
    operate(
        alice_link,
        workspace.workspace_id,
        4,
        CallOperation::End {
            call: started.call,
            epoch: muted.epoch,
        },
    )
    .await
    .unwrap();
    browser(&page_link, |envelope| match envelope {
        Envelope::VoiceRefused { .. } => Some(()),
        _ => None,
    })
    .await;
    page_link
        .send(Envelope::VoiceJoin {
            workspace_id: workspace.workspace_id,
            call: started.call,
            participation: None,
        })
        .unwrap();
    let refused = browser(&page_link, |envelope| match envelope {
        Envelope::VoiceRefused { reason, .. } => Some(reason),
        Envelope::VoiceGranted { .. } => panic!("an ended call admits no one"),
        _ => None,
    })
    .await;
    assert!(refused.contains("ended"), "{refused}");
    workspace.shutdown().await;
}

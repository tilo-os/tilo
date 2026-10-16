//! A call in a room the host governs, between the room's owner on a native
//! iroh device and an invited person in a browser session: both get their
//! grants from the host, hear each other over its `moq-lite` voice carrier,
//! and lose their voice when the owner denies it or ends the call.
//!
//! The browser's session is the page's own HTTPS carrier; its voice carrier
//! is the native `moq-lite` client, dialling the same WebTransport endpoint a
//! page would.

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

#[path = "support/call.rs"]
mod call;
mod support;

// This binary re-execs as the automerge worker (support is shared with crates that lack the dep).
tilo_automerge_isolate::test_worker!();

use tilo_browser_peer::HttpsLink;
use tilo_moq::client::Status;
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::voice::{CallOperation, Directions};
use tilo_user_identity::RevocationList;

use call::{GovernedCall, browser, carrier, granted, hears, native, operate, until};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invited_browser_and_the_owners_device_talk_in_a_governed_call() {
    // Alice starts a call on her chat's voice channel; only the room's
    // owner may. Bob, a different person, redeems her invitation with his
    // browser's device (over iroh: pages have no redemption route yet).
    let room = GovernedCall::start().await;
    let (bob, page) = room.invite("bob-browser").await;
    let alice_link = &room.owner;
    let started = &room.call;
    alice_link
        .send(Envelope::VoiceJoin {
            workspace_id: room.workspace_id,
            call: started.call,
            participation: None,
        })
        .unwrap();
    let alone = native(alice_link, granted).await;
    assert_eq!(alone.participants.len(), 1);
    assert!(alone.subscriptions.is_empty());

    // Bob's browser joins over HTTPS; as a guest it may not end the call.
    let mut page_link = HttpsLink::connect(
        room.https(),
        &page.cert,
        room.host.cert.device_public_key,
        &RevocationList::new(),
        &page.key,
    )
    .await
    .unwrap();
    assert!(page_link.voice(), "the host advertises calls");
    page_link
        .send(Envelope::Subscribe {
            workspace_id: room.workspace_id,
        })
        .unwrap();
    browser(&mut page_link, |envelope| match envelope {
        Envelope::Ready { .. } => Some(()),
        Envelope::NotAuthorised { reason, .. } => panic!("browser not admitted: {reason}"),
        _ => None,
    })
    .await;
    page_link
        .send(Envelope::VoiceCall {
            workspace_id: room.workspace_id,
            request: [2; 16],
            operation: CallOperation::End {
                call: started.call,
                epoch: started.epoch,
            },
        })
        .unwrap();
    let intruder = browser(&mut page_link, |envelope| match envelope {
        Envelope::VoiceCallResult { result, .. } => Some(result),
        _ => None,
    })
    .await;
    assert!(intruder.is_err(), "a guest cannot end the call");
    page_link
        .send(Envelope::VoiceJoin {
            workspace_id: room.workspace_id,
            call: started.call,
            participation: None,
        })
        .unwrap();
    let bob_grant = browser(&mut page_link, granted).await;
    let alice_grant = native(alice_link, |envelope| {
        granted(envelope).filter(|grant| grant.participants.len() == 2)
    })
    .await;
    let alice_voice = alice_grant.publication.clone().unwrap();
    let bob_voice = bob_grant.publication.clone().unwrap();
    assert_eq!(bob_grant.subscriptions, vec![alice_voice.clone()]);
    assert_eq!(alice_grant.subscriptions, vec![bob_voice.clone()]);

    // Both carriers are admitted by the host's grants; audio flows both ways.
    let alice_carrier = carrier(&alice_grant, 1);
    let bob_carrier = carrier(&bob_grant, 1);
    until("Alice's carrier", &alice_carrier, |status| {
        status == Status::Live
    })
    .await;
    until("Bob's carrier", &bob_carrier, |status| {
        status == Status::Live
    })
    .await;
    alice_carrier.enable_publication().unwrap();
    bob_carrier.enable_publication().unwrap();
    hears(
        "Bob hears Alice",
        &alice_carrier,
        &bob_carrier,
        &alice_voice,
    )
    .await;
    hears("Alice hears Bob", &bob_carrier, &alice_carrier, &bob_voice).await;

    // A forged grant is not admitted.
    let mut forged = bob_grant.clone();
    forged.token = vec![0; 32];
    let forged = carrier(&forged, 2);
    until("the forged carrier", &forged, |status| {
        matches!(status, Status::Failed | Status::Denied | Status::Closed)
    })
    .await;

    // Alice mutes Bob: his grant loses its publication and the host closes
    // the carrier that published.
    let muted = operate(
        alice_link,
        room.workspace_id,
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
    let listening = browser(&mut page_link, |envelope| {
        granted(envelope).filter(|grant| !grant.directions.publish)
    })
    .await;
    assert!(listening.publication.is_none());
    assert!(listening.directions.subscribe);
    until("Bob's publishing carrier closes", &bob_carrier, |status| {
        status != Status::Live
    })
    .await;
    // The roster changed for Alice too: nobody left for her to hear. Each
    // side follows its new grant with a new carrier.
    let speaking = native(alice_link, |envelope| {
        granted(envelope).filter(|grant| grant.subscriptions.is_empty())
    })
    .await;
    assert_eq!(speaking.publication.as_deref(), Some(alice_voice.as_str()));
    alice_carrier.close();
    let alice_speaker = carrier(&speaking, 2);
    let bob_listener = carrier(&listening, 2);
    until("Alice's speaking carrier", &alice_speaker, |status| {
        status == Status::Live
    })
    .await;
    until("Bob's listening carrier", &bob_listener, |status| {
        status == Status::Live
    })
    .await;
    alice_speaker.enable_publication().unwrap();
    hears(
        "Bob still hears Alice",
        &alice_speaker,
        &bob_listener,
        &alice_voice,
    )
    .await;

    // Alice ends the call: Bob is told and his carrier is closed.
    operate(
        alice_link,
        room.workspace_id,
        4,
        CallOperation::End {
            call: started.call,
            epoch: muted.epoch,
        },
    )
    .await
    .unwrap();
    browser(&mut page_link, |envelope| match envelope {
        Envelope::VoiceRefused { .. } => Some(()),
        _ => None,
    })
    .await;
    until(
        "Bob's carrier closes with the call",
        &bob_listener,
        |status| status != Status::Live,
    )
    .await;
    page_link
        .send(Envelope::VoiceJoin {
            workspace_id: room.workspace_id,
            call: started.call,
            participation: None,
        })
        .unwrap();
    let refused = browser(&mut page_link, |envelope| match envelope {
        Envelope::VoiceRefused { reason, .. } => Some(reason),
        Envelope::VoiceGranted { .. } => panic!("an ended call admits no one"),
        _ => None,
    })
    .await;
    assert!(refused.contains("ended"), "{refused}");
    room.shutdown().await;
}

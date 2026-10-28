//! A person's own room, which a host carries for them and nobody governs:
//! one of their browsers stewards its channels, holding the keys and
//! admitting their other devices, and the host only ever carries
//! ciphertext. Carol is a member of a host's Lounge and has nothing but
//! browsers. Her laptop's page writes a note in her own room; her phone's
//! page joins it, is admitted by the laptop's page and reads the note; and
//! the two pages hear each other in a call there. Then the laptop's page
//! closes, and her phone's page takes over and admits her tablet's.

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

// This binary re-execs as the automerge worker (the host's rooms use it).
tilo_automerge_isolate::test_worker!();

use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::{DocumentRoute, Host, PeerEvent, PeerHandle, VoiceEvent};
use tilo_relay_protocol::voice::{CallOperation, VoiceGrant};
use tilo_user_identity::DeviceCapabilities;

use call::{CHAT, enter, invite};
use support::{TestHost, enrol, next_document, ready, start_peer, start_room_host, wait_for};

/// The next voice event of `page` that `pick` takes.
async fn voice<T>(page: &PeerHandle, mut pick: impl FnMut(VoiceEvent) -> Option<T>) -> T {
    let mut found = None;
    wait_for("a voice event", || {
        for event in page.drain() {
            if found.is_none()
                && let PeerEvent::Voice(event) = event
            {
                found = pick(event);
            }
        }
        let done = found.is_some().then_some(());
        async move { done }
    })
    .await;
    found.unwrap()
}

/// The next document `page` receives, or a failure telling where every
/// page's channels stand.
async fn document(page: &PeerHandle, pages: &[&PeerHandle]) -> DocumentMutation {
    let mut events = Vec::new();
    let wait = std::time::Duration::from_secs(90);
    match tokio::time::timeout(wait, next_document(page, &mut events)).await {
        Ok(document) => document,
        Err(_) => {
            let states: Vec<String> = pages
                .iter()
                .map(|page| {
                    let status = page.status();
                    format!("{}\nfailures: {:?}", status.channels, status.failures)
                })
                .collect();
            panic!("no document arrived; channels:\n{}", states.join("\n---\n"));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_browsers_steward_their_own_room_and_call_there() {
    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    let host = enrol(&alice, "lounge-host", DeviceCapabilities::host());
    let alice_laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let hosted = start_room_host(&alice, &host, state.path(), false).await;
    let https = Host::Https(hosted.https());
    let relay = &hosted.relay;

    // Carol joins the Lounge: the host trusts her from then on.
    let lounge = relay.rooms().unwrap().create("Lounge").await.unwrap();
    enter(relay, &alice_laptop.cert, &lounge.room).await;
    let carol = SigningKey::generate(&mut UnwrapErr(SysRng));
    let laptop = enrol(&carol, "carol-laptop-browser", DeviceCapabilities::member());
    let phone = enrol(&carol, "carol-phone-browser", DeviceCapabilities::member());
    invite(relay, &alice_laptop.cert, &lounge.room, &carol, &laptop).await;

    let own = tilo_relay::own_room(&carol.verifying_key().to_bytes());
    let workspace_id = tilo_relay::room_workspace_id(&own).unwrap();
    let route = DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: own.clone(),
        record: "note-1".into(),
    };

    // Her laptop's page is the first of her devices there: it stewards the
    // room, and its note goes out sealed.
    let first = start_peer(
        https.clone(),
        laptop.cert.clone(),
        host.cert.clone(),
        laptop.key.clone(),
    );
    first.join(workspace_id).unwrap();
    ready(&[&first]).await;
    let note = DocumentMutation::Snapshot(b"carol's own note".to_vec());
    first.publish(route.clone(), &note).unwrap();
    let receipt = first.barrier().unwrap();
    wait_for("the note to be durable", || async {
        receipt.try_recv().ok()
    })
    .await
    .expect("the host keeps the note");

    // Her phone's page joins, asks the laptop's page for the keys, and
    // reads the note.
    let second = start_peer(
        https.clone(),
        phone.cert.clone(),
        host.cert.clone(),
        phone.key.clone(),
    );
    second.join(workspace_id).unwrap();
    ready(&[&second]).await;
    second.open_route(route.clone()).unwrap();
    assert_eq!(
        document(&second, &[&first, &second]).await,
        note,
        "the phone reads what the laptop wrote, with keys only her devices hold"
    );
    assert!(
        first.status().channels.contains("stewarded by"),
        "{}",
        first.status().channels
    );

    // A call in her own room: both pages open it and hear each other.
    let open = CallOperation::Open {
        app_id: CHAT.into(),
        record_id: String::new(),
        channel_id: "voice".into(),
    };
    first
        .voice_call(workspace_id, [1; 16], open.clone())
        .unwrap();
    let call = voice(&first, |event| match event {
        VoiceEvent::CallResult { result, .. } => Some(result.expect("the call opens").call),
        _ => None,
    })
    .await;
    first.voice_join(workspace_id, call).unwrap();
    second.voice_join(workspace_id, call).unwrap();
    let grant: VoiceGrant = voice(&second, |event| match event {
        VoiceEvent::Granted { grant, .. } if grant.participants.len() == 2 => Some(*grant),
        VoiceEvent::Ended { reason, .. } => panic!("the call ended: {reason}"),
        _ => None,
    })
    .await;
    assert!(grant.directions.publish && grant.directions.subscribe);

    // The laptop's page closes. Her tablet's page joins and asks; the
    // steward does not answer, so her phone's page, which holds the keys,
    // takes the channel over and admits the tablet.
    first.close();
    let tablet = enrol(&carol, "carol-tablet-browser", DeviceCapabilities::member());
    let third = start_peer(
        https.clone(),
        tablet.cert.clone(),
        host.cert.clone(),
        tablet.key.clone(),
    );
    third.join(workspace_id).unwrap();
    ready(&[&third]).await;
    third.open_route(route.clone()).unwrap();
    assert_eq!(
        document(&third, &[&second, &third]).await,
        note,
        "the tablet reads the note with keys from the phone, the laptop gone"
    );
    let phone_key = hex::encode(&phone.cert.device_public_key[..4]);
    assert!(
        third
            .status()
            .channels
            .contains(&format!("stewarded by {phone_key}")),
        "{}",
        third.status().channels
    );

    let TestHost { relay, web } = hosted;
    drop(web);
    relay.shutdown().await;
}

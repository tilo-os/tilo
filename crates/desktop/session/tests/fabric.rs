//! The person's device fabric lives on the session: a copy made on one device reaches the other,
//! and an action reaches the one device it is addressed to and no other.
//! The sessions here run on this machine's loopback with the real peer actor and real certificates
//! of one person, so what is checked is the transport and the link over it together, not a
//! stand-in link.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_network_actor::DeviceIdentity;
use tilo_session::platform::fabric::Continuity;
use tilo_session::platform::fabric::clock::{
    ClockSyncState, fabric_monotonic_us, hold_fabric_clock_sync,
};
use tilo_session::platform::fabric::{
    ClipboardEntry, ClipboardPayload, DeviceId, FabricStamp, PersonalFabricAction,
    PersonalFabricActionEnvelope,
};
use tilo_session::platform::fabric::{FabricLink as _, SharedLink};
use tilo_session::sync::{Client, Entry, OwnPresence};
use tilo_user_identity::DeviceCapabilities;

/// One pass of the session's loop, as the session runs it on its own clock.
const PASS: Duration = Duration::from_millis(20);

/// One of the person's devices, on its own loopback endpoint, taking part in the collaboration
/// app wire as its certified self.
struct Device {
    client: Client,
    id: DeviceId,
}

impl Device {
    /// This device's way onto the person's fabric.
    fn fabric(&self) -> SharedLink {
        self.client
            .fabric()
            .expect("a certified device's session keeps a fabric")
    }
}

/// The person's root key: every device's certificate chains to it.
fn person() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[21; 32])
}

/// The person's continuity switches as their settings leave them: nothing turned off, so media
/// moves between their devices.
fn permitted() -> Continuity {
    let continuity = Continuity::default();
    let document: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({"continuity": {"enabled": true, "media": true}}))
            .expect("a settings document");
    continuity.read(&document);
    continuity
}

/// One of the person's devices, certified by `person` , on its own key.
fn device(person: &ed25519_dalek::SigningKey, seed: u8, label: &str) -> Device {
    device_with(person, seed, label, test_server::loopback(), None)
}

/// Discovery on this machine's loopback instead of the network the session runs on: what a
/// local-network test needs to hear only its own devices.
fn loopback_discovery() -> tilo_endpoint_reachability::Nearby {
    tilo_endpoint_reachability::Nearby::on(Arc::new(|| {}), [std::net::Ipv4Addr::LOCALHOST])
}

/// [`device`], over the reach and local discovery it is given.
fn device_with(
    person: &ed25519_dalek::SigningKey,
    seed: u8,
    label: &str,
    relays: tilo_endpoint_reachability::EndpointRelays,
    lan: Option<tilo_endpoint_reachability::Nearby>,
) -> Device {
    let key = test_server::signing_key(seed);
    let cert = test_server::certified(person, &key, label, DeviceCapabilities::member());
    let client = Client::start(
        Entry {
            identity: Some(DeviceIdentity::kept(cert, key)),
            own: OwnPresence::default(),
            hosts: Vec::new(),
            continuity: permitted(),
            wake: Arc::new(|| {}),
            relays,
            lan,
        },
        None,
        None,
        Vec::new(),
    )
    .expect("a loopback endpoint binds");
    // The fabric names a device by the key its session proves.
    let id = DeviceId::new(hex::encode(client.publisher().as_bytes())).expect("a device id");
    Device { client, id }
}

/// One pass of each session's loop, as the session's own loop makes it.
fn pass(devices: &[&Device]) {
    for device in devices {
        device.client.follow();
    }
}

/// Run the sessions' passes until `done` holds: whether it held before the suite's hang bound ran
/// out.
fn until(devices: &[&Device], mut done: impl FnMut() -> bool) -> bool {
    // The bound is the suite's own, so a busy machine slows the passes instead of failing them.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        pass(devices);
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(PASS);
    }
}

/// Whether `here` and `there` each see the other's session up.
fn both_online(here: &Device, there: &Device) -> bool {
    here.fabric().online_device_ids().contains(&there.id)
        && there.fabric().online_device_ids().contains(&here.id)
}

#[test]
fn a_remote_audio_route_gets_the_clock_it_plays_the_other_device_by() {
    let person = person();
    let laptop = device(&person, 1, "Laptop");
    let session = device(&person, 2, "Session");
    laptop.client.connect(session.client.endpoint_addr());
    assert!(
        until(&[&laptop, &session], || both_online(&laptop, &session)),
        "the two devices' sessions came up"
    );

    // What a route playing the shell's sound holds while it runs: until the shell answers, its audio
    // may not start.
    let clock = hold_fabric_clock_sync(session.id.clone());
    assert_eq!(
        clock.state(),
        ClockSyncState::Syncing,
        "nothing is known of the session's clock yet"
    );
    assert!(
        until(&[&laptop, &session], || clock.state()
            == ClockSyncState::Ready),
        "the session answered a probe, so the route left its syncing clock"
    );
    assert!(
        until(&[&laptop, &session], || {
            clock.estimate().is_some_and(|estimate| estimate.synced)
        }),
        "enough round trips to follow the session's clock"
    );
    // Both sessions run on this machine, so the shell's clock is this one's,
    // and what plays there is scheduled from the same line.
    let now_us = fabric_monotonic_us();
    let offset_us = clock
        .estimate()
        .and_then(|estimate| estimate.offset_us(now_us))
        .expect("an estimate of the session's clock");
    assert!(
        offset_us.abs() < 1_000,
        "the session's clock is this device's: {offset_us} us off"
    );
}

/// Two of the person's devices on one local network, whose server is unreachable and which ask no
/// directory: each is told only the other's key, finds it on the network, and what is copied on one
/// reaches the other. Each lists the other as nearby; a device of someone else's, heard on the same
/// network, is neither dialled nor named. The three discover on this machine's loopback, so the
/// test hears only them and needs no multicast on the network it runs on.
#[test]
fn the_persons_devices_on_one_network_sync_without_a_relay() {
    let unreachable = || {
        tilo_endpoint_reachability::EndpointRelays::for_choice(&settings_store::relay::Relays::Own(
            "http://127.0.0.1:1".into(),
        ))
    };
    let person = person();
    let laptop = device_with(
        &person,
        1,
        "Laptop",
        unreachable(),
        Some(loopback_discovery()),
    );
    let session = device_with(
        &person,
        2,
        "Session",
        unreachable(),
        Some(loopback_discovery()),
    );
    let stranger = device_with(
        &ed25519_dalek::SigningKey::from_bytes(&[22; 32]),
        3,
        "Stranger",
        unreachable(),
        Some(loopback_discovery()),
    );
    laptop
        .client
        .connect(iroh::EndpointAddr::new(session.client.publisher()));

    assert!(
        until(&[&laptop, &session, &stranger], || both_online(
            &laptop, &session
        )),
        "the two devices found each other on the local network"
    );
    let now = tilo_clock::unix_now_ms();
    let text = "copied with no server";
    let copy = ClipboardEntry::shareable(
        ClipboardPayload::Text(text.to_owned()),
        laptop.id.clone(),
        now,
        FabricStamp {
            counter: now,
            device: laptop.id.clone(),
        },
    )
    .expect("a shareable copy");
    laptop.fabric().share_clipboard(vec![copy]);
    assert!(
        until(&[&laptop, &session, &stranger], || {
            session
                .fabric()
                .devices()
                .merged_clipboard()
                .entries()
                .any(|entry| entry.payload == ClipboardPayload::Text(text.to_owned()))
        }),
        "the copy reached the session over the local network"
    );
    assert!(
        until(&[&laptop, &session, &stranger], || {
            laptop.fabric().nearby_device_ids().contains(&session.id)
                && session.fabric().nearby_device_ids().contains(&laptop.id)
        }),
        "each lists the other as nearby"
    );
    for device in [&laptop, &session] {
        assert!(
            !device.fabric().nearby_device_ids().contains(&stranger.id)
                && !device.fabric().online_device_ids().contains(&stranger.id),
            "someone else's device on the network is nobody here"
        );
    }
}

#[test]
fn an_action_reaches_the_other_session() {
    let person = person();
    let laptop = device(&person, 1, "Laptop");
    let session = device(&person, 2, "Session");
    laptop.client.connect(session.client.endpoint_addr());

    assert!(
        until(&[&laptop, &session], || both_online(&laptop, &session)),
        "the two devices' sessions came up"
    );

    let request = laptop
        .fabric()
        .try_send_personal_fabric_action(
            &session.id,
            PersonalFabricAction::RemoteAudioCancel {
                session_id: [1; 16],
            },
        )
        .expect("the action is sent to the session");

    let mut heard: Vec<PersonalFabricActionEnvelope> = Vec::new();
    let arrived = until(&[&laptop, &session], || {
        heard.extend(session.client.fabric_actions());
        heard.iter().any(|envelope| envelope.request_id == request)
    });

    assert!(arrived, "the action reached the session");
    let envelope = heard
        .iter()
        .find(|envelope| envelope.request_id == request)
        .expect("the action the session heard");
    assert_eq!(envelope.source, laptop.id, "the laptop sent the action");
    assert_eq!(envelope.target, session.id, "the session was addressed");
    assert_eq!(
        envelope.action,
        PersonalFabricAction::RemoteAudioCancel {
            session_id: [1; 16]
        },
        "the action arrives as it was sent"
    );
}

#[test]
fn an_action_addressed_to_a_third_device_is_not_delivered() {
    let person = person();
    let laptop = device(&person, 1, "Laptop");
    let session = device(&person, 2, "Session");
    let pod = device(&person, 3, "Pod");
    laptop.client.connect(session.client.endpoint_addr());
    laptop.client.connect(pod.client.endpoint_addr());

    assert!(
        until(&[&laptop, &session, &pod], || {
            both_online(&laptop, &session) && both_online(&laptop, &pod)
        }),
        "the laptop's sessions with the session and the pod came up"
    );

    let request = laptop
        .fabric()
        .try_send_personal_fabric_action(
            &pod.id,
            PersonalFabricAction::RemoteAudioCancel {
                session_id: [1; 16],
            },
        )
        .expect("the action is sent to the pod");

    let mut at_shell: Vec<PersonalFabricActionEnvelope> = Vec::new();
    let mut at_pod: Vec<PersonalFabricActionEnvelope> = Vec::new();
    let arrived = until(&[&laptop, &session, &pod], || {
        at_shell.extend(session.client.fabric_actions());
        at_pod.extend(pod.client.fabric_actions());
        at_pod.iter().any(|envelope| envelope.request_id == request)
    });

    assert!(arrived, "the pod was handed the action addressed to it");
    // Whatever the transport did with the frame would have happened by now: the shell's session with
    // the laptop runs on the same passes.
    for _ in 0..5 {
        pass(&[&laptop, &session, &pod]);
        at_shell.extend(session.client.fabric_actions());
        at_pod.extend(pod.client.fabric_actions());
        std::thread::sleep(PASS);
    }
    assert!(
        at_shell.is_empty(),
        "the session was handed no action addressed to the pod: {at_shell:?}"
    );
    assert_eq!(at_pod.len(), 1, "the pod was handed the action once");
    assert_eq!(at_pod[0].source, laptop.id);
    assert_eq!(at_pod[0].target, pod.id);
}

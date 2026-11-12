//! The person's device fabric lives on the session: a copy made on one device reaches the other,
//! and an action reaches the one device it is addressed to and no other.
//! The sessions here run on this machine's loopback with the real peer actor and real certificates
//! of one person, so what is checked is the transport and the fabric runtime together, not a
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

use tilo_network_actor::HostIdentity;
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
/// plane as its certified self.
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
    kept_device(person, seed, label, None)
}

/// [`device`], keeping its fabric under `kept` between runs as a session does.
fn kept_device(
    person: &ed25519_dalek::SigningKey,
    seed: u8,
    label: &str,
    kept: Option<&std::path::Path>,
) -> Device {
    let key = test_relay::signing_key(seed);
    let cert = test_relay::certified(person, &key, label, DeviceCapabilities::member());
    let client = Client::start(
        Entry {
            identity: Some(HostIdentity::kept(cert, key)),
            own: OwnPresence::default(),
            hosts: Vec::new(),
            continuity: permitted(),
            wake: Arc::new(|| {}),
            fabric: kept.map(std::path::Path::to_path_buf),
            relays: test_relay::loopback(),
        },
        None,
        None,
        Vec::new(),
        None,
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
    let desk = device(&person, 2, "Desk");
    laptop.client.connect(desk.client.endpoint_addr());
    assert!(
        until(&[&laptop, &desk], || both_online(&laptop, &desk)),
        "the two devices' sessions came up"
    );

    // What a route playing the desk's sound holds while it runs: until the desk answers, its audio
    // may not start.
    let clock = hold_fabric_clock_sync(desk.id.clone());
    assert_eq!(
        clock.state(),
        ClockSyncState::Syncing,
        "nothing is known of the desk's clock yet"
    );
    assert!(
        until(&[&laptop, &desk], || clock.state() == ClockSyncState::Ready),
        "the desk answered a probe, so the route left its syncing clock"
    );
    assert!(
        until(&[&laptop, &desk], || {
            clock.estimate().is_some_and(|estimate| estimate.synced)
        }),
        "enough round trips to follow the desk's clock"
    );
    // Both sessions run on this machine, so the desk's clock is this one's,
    // and what plays there is scheduled from the same line.
    let now_us = fabric_monotonic_us();
    let offset_us = clock
        .estimate()
        .and_then(|estimate| estimate.offset_us(now_us))
        .expect("an estimate of the desk's clock");
    assert!(
        offset_us.abs() < 1_000,
        "the desk's clock is this device's: {offset_us} us off"
    );
}

/// A device whose session starts again goes on from the revision it last said, so its other
/// devices take what it says now over what they kept of it; a record said again from the first
/// revision was refused as a conflict ("same device revision carried conflicting state", box of
/// e2e-branch after both boxes rebooted). A machine copied from it, under a key of its own,
/// starts a line of its own and does not take up the one it was copied with.
#[test]
fn a_device_that_starts_again_continues_its_own_revisions() {
    let person = person();
    let kept = tempfile::tempdir().expect("a state directory");
    let desk = device(&person, 2, "Desk");
    let laptop = kept_device(&person, 1, "Laptop", Some(kept.path()));
    let id = laptop.id.clone();
    laptop.client.connect(desk.client.endpoint_addr());
    assert!(
        until(&[&laptop, &desk], || both_online(&laptop, &desk)),
        "the two devices' sessions came up"
    );
    let held = |desk: &Device| {
        desk.fabric()
            .device_fabric()
            .snapshots()
            .get(&id)
            .map(|record| record.presence.stamp.clone())
    };
    let before = held(&desk).expect("the desk holds what the laptop said");
    drop(laptop);

    let laptop = kept_device(&person, 1, "Laptop", Some(kept.path()));
    laptop.client.connect(desk.client.endpoint_addr());
    assert!(
        until(&[&laptop, &desk], || {
            held(&desk).is_some_and(|now| now.is_newer_than(&before))
        }),
        "the desk took what the laptop said after starting again: it still holds {before:?}"
    );

    let copy = kept_device(&person, 3, "Branch", Some(kept.path()));
    assert!(
        !copy.fabric().device_fabric().snapshots().contains_key(&id),
        "a machine with a key of its own does not take up the laptop's kept fabric"
    );
}

#[test]
fn a_clipboard_entry_set_on_one_session_reaches_the_other() {
    let person = person();
    let laptop = device(&person, 1, "Laptop");
    let desk = device(&person, 2, "Desk");
    laptop.client.connect(desk.client.endpoint_addr());

    assert!(
        until(&[&laptop, &desk], || both_online(&laptop, &desk)),
        "the two devices' sessions came up"
    );

    let now = tilo_clock::unix_now_ms();
    let text = "a line copied on the laptop";
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
        until(&[&laptop, &desk], || {
            desk.fabric()
                .device_fabric()
                .merged_clipboard()
                .entries()
                .any(|entry| {
                    entry.origin == laptop.id
                        && entry.payload == ClipboardPayload::Text(text.to_owned())
                })
        }),
        "the copy made on the laptop reached the desk"
    );
}

#[test]
fn a_wake_reaches_the_other_session() {
    let person = person();
    let laptop = device(&person, 1, "Laptop");
    let desk = device(&person, 2, "Desk");
    // Not in the session at all: a wake names a device of the person's by its fabric id, so the
    // waker need not have a session with it.
    let pod = device(&person, 3, "Pod");
    laptop.client.connect(desk.client.endpoint_addr());

    assert!(
        until(&[&laptop, &desk], || both_online(&laptop, &desk)),
        "the two devices' sessions came up"
    );

    let request = laptop
        .fabric()
        .try_send_personal_fabric_action(
            &desk.id,
            PersonalFabricAction::WakeDevice {
                target: pod.id.clone(),
            },
        )
        .expect("the wake is sent to the desk");

    let mut heard: Vec<PersonalFabricActionEnvelope> = Vec::new();
    let arrived = until(&[&laptop, &desk], || {
        heard.extend(desk.client.fabric_actions());
        heard.iter().any(|envelope| envelope.request_id == request)
    });

    assert!(arrived, "the wake reached the desk");
    let envelope = heard
        .iter()
        .find(|envelope| envelope.request_id == request)
        .expect("the wake the desk heard");
    assert_eq!(envelope.source, laptop.id, "the laptop sent the wake");
    assert_eq!(envelope.target, desk.id, "the desk was addressed");
    assert_eq!(
        envelope.action,
        PersonalFabricAction::WakeDevice { target: pod.id },
        "the wake still names the device to wake"
    );
}

#[test]
fn an_action_addressed_to_a_third_device_is_not_delivered() {
    let person = person();
    let laptop = device(&person, 1, "Laptop");
    let desk = device(&person, 2, "Desk");
    let pod = device(&person, 3, "Pod");
    laptop.client.connect(desk.client.endpoint_addr());
    laptop.client.connect(pod.client.endpoint_addr());

    assert!(
        until(&[&laptop, &desk, &pod], || {
            both_online(&laptop, &desk) && both_online(&laptop, &pod)
        }),
        "the laptop's sessions with the desk and the pod came up"
    );

    let request = laptop
        .fabric()
        .try_send_personal_fabric_action(
            &pod.id,
            PersonalFabricAction::WakeDevice {
                target: desk.id.clone(),
            },
        )
        .expect("the wake is sent to the pod");

    let mut at_desk: Vec<PersonalFabricActionEnvelope> = Vec::new();
    let mut at_pod: Vec<PersonalFabricActionEnvelope> = Vec::new();
    let arrived = until(&[&laptop, &desk, &pod], || {
        at_desk.extend(desk.client.fabric_actions());
        at_pod.extend(pod.client.fabric_actions());
        at_pod.iter().any(|envelope| envelope.request_id == request)
    });

    assert!(arrived, "the pod was handed the action addressed to it");
    // Whatever the transport did with the frame would have happened by now: the desk's session with
    // the laptop runs on the same passes.
    for _ in 0..5 {
        pass(&[&laptop, &desk, &pod]);
        at_desk.extend(desk.client.fabric_actions());
        at_pod.extend(pod.client.fabric_actions());
        std::thread::sleep(PASS);
    }
    assert!(
        at_desk.is_empty(),
        "the desk was handed no action addressed to the pod: {at_desk:?}"
    );
    assert_eq!(at_pod.len(), 1, "the pod was handed the action once");
    assert_eq!(at_pod[0].source, laptop.id);
    assert_eq!(at_pod[0].target, pod.id);
}

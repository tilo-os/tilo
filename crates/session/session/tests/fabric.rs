//! The person's device fabric lives on the session: a copy made on one
//! device reaches the other, and an action reaches the one device it is
//! addressed to and no other.
//!
//! The sessions here run on this machine's loopback with the real peer
//! actor and real certificates of one person, so what is checked is the
//! transport and the fabric runtime together, not a stand-in link.

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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_device_fabric::link::{FabricLink as _, SharedLink};
use tilo_device_fabric::{
    ClipboardEntry, ClipboardPayload, DeviceId, FabricStamp, PersonalFabricAction,
    PersonalFabricActionEnvelope,
};
use tilo_network_actor::HostIdentity;
use tilo_session::fabric::MediaContinuity;
use tilo_session::sync::{Client, OwnPresence};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

/// How long the loopback sessions have to come up and carry what they carry.
const DEADLINE: Duration = Duration::from_secs(30);
/// One pass of the session's loop, as the session runs it on its own clock.
const PASS: Duration = Duration::from_millis(20);

/// One of the person's devices, on its own loopback endpoint, taking part
/// in the collaboration plane as its certified self.
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

/// One of the person's devices, certified by `person`, on its own key.
fn device(person: &ed25519_dalek::SigningKey, seed: u8, label: &str) -> Device {
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, person).expect("the person signs their own device");
    let client = Client::start(
        Some(HostIdentity {
            device_cert: cert,
            device_signing_key: key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        }),
        None,
        OwnPresence::default(),
        Vec::new(),
        MediaContinuity::default(),
        Arc::new(|| {}),
        tilo_session::ChannelLink::new(),
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

/// Run the sessions' passes until `done` holds, or the deadline passes:
/// whether `done` held.
fn until(devices: &[&Device], mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + DEADLINE;
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
    // Not in the session at all: a wake names a device of the person's by
    // its fabric id, so the waker need not have a session with it.
    let pod = device(&person, 3, "Pod");
    laptop.client.connect(desk.client.endpoint_addr());

    assert!(
        until(&[&laptop, &desk], || both_online(&laptop, &desk)),
        "the two devices' sessions came up"
    );

    let request = laptop
        .fabric()
        .send_personal_fabric_action(
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
        .send_personal_fabric_action(
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
    // Whatever the transport did with the frame would have happened by now:
    // the desk's session with the laptop runs on the same passes.
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

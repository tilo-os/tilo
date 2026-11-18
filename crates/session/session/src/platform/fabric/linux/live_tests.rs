#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::sync::Arc;
use std::time::Instant;

use tilo_network_actor::PersonalFabricPayload;
use tilo_network_actor::fabric::link::FabricLink as _;
use tilo_network_actor::fabric::{ClipboardPayload, FabricStamp, OwnerSession};
use tilo_user_identity::{DeviceCert, RevocationEntry};

use super::super::{Continuity, Reach, SessionFabric};
use super::*;

/// One of the person's devices: its certificate, its endpoint and its session's link, and the
/// frames that link sends.
struct Device {
    cert: DeviceCert,
    id: DeviceId,
    endpoint: EndpointId,
    link: Arc<SessionFabric>,
    frames: Box<dyn FnMut() -> Vec<PersonalFabricPayload>>,
}

impl Device {
    fn new(seed: u8, label: &str) -> Self {
        let person = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
        let id = DeviceId::new(hex::encode(cert.device_public_key)).expect("a device id");
        let endpoint = EndpointId::from_bytes(&cert.device_public_key).expect("an endpoint");
        let (reach, sent) = Reach::listened(cert.user_public_key, key, Continuity::default());
        let link = SessionFabric::new(id.clone(), label.to_owned(), None, reach);
        Self {
            cert,
            id,
            endpoint,
            link,
            frames: Box::new(sent),
        }
    }

    /// The frames this device's link sent since the last call.
    fn sent(&mut self) -> Vec<PersonalFabricPayload> {
        (self.frames)()
    }

    /// `other` proved itself on its session with this device.
    fn meets(&self, other: &Self) {
        self.link
            .sibling(other.endpoint, std::slice::from_ref(&other.cert), &[]);
    }

    /// Take what `from` sent since the last call, as its session with this device carries it.
    fn hears(&self, from: &mut Self) {
        for payload in from.sent() {
            self.link.arrived(from.endpoint, &payload, Instant::now());
        }
    }
}

/// What a device says of itself while it is there reaches the person's other device when their
/// session starts and whenever it changes, and holds only while the session lasts.
#[test]
fn a_devices_presence_holds_while_its_session_lasts() {
    let mut laptop = Device::new(0x21, "Laptop");
    let desktop = Device::new(0x22, "Desktop");

    desktop.meets(&laptop);
    laptop.meets(&desktop);
    desktop.hears(&mut laptop);
    let seen = desktop.link.devices();
    let there = seen.get(&laptop.id).expect("the desktop knows the laptop");
    assert!(there.online);
    assert_eq!(there.about.label, "Laptop");
    assert!(!there.presence.session.locked);

    // The person locks the laptop: the desktop hears it on the next announcement.
    laptop.link.publish_owner_session(OwnerSession {
        locked: true,
        changed_unix_ms: 7,
        active_unix_ms: 0,
    });
    laptop.link.announce();
    desktop.hears(&mut laptop);
    assert!(
        desktop
            .link
            .devices()
            .get(&laptop.id)
            .is_some_and(|device| device.presence.session.locked)
    );
    // Nothing changed: nothing more is said.
    laptop.link.announce();
    assert!(laptop.sent().is_empty());

    desktop.link.sibling_left(laptop.endpoint);
    let seen = desktop.link.devices();
    let gone = seen.get(&laptop.id).expect("still the person's device");
    assert!(!gone.online);
    assert!(
        !gone.presence.session.locked,
        "what it said held only while it was there"
    );
    assert!(desktop.link.online_device_ids().is_empty());
}

/// A frame from an endpoint that proved no certificate of the person's names no device and
/// changes nothing.
#[test]
fn a_stranger_says_nothing_of_the_persons_devices() {
    let desktop = Device::new(0x22, "Desktop");
    let mut stranger = Device::new(0x31, "Stranger");

    stranger.link.share_wake_routes(Vec::new());
    stranger.link.publish_owner_session(OwnerSession {
        locked: true,
        changed_unix_ms: 1,
        active_unix_ms: 0,
    });
    stranger.link.announce();
    desktop.hears(&mut stranger);

    assert!(desktop.link.devices().get(&stranger.id).is_none());
    assert!(desktop.link.online_device_ids().is_empty());
}

/// The copies a device offers reach the person's other device, and a copy it says another device
/// made is refused.
#[test]
fn a_copy_reaches_the_persons_other_device_and_a_forged_one_does_not() {
    let mut laptop = Device::new(0x21, "Laptop");
    let desktop = Device::new(0x22, "Desktop");
    desktop.meets(&laptop);
    let copy = |origin: &DeviceId, text: &str| {
        ClipboardEntry::shareable(
            ClipboardPayload::Text(text.to_owned()),
            origin.clone(),
            5,
            FabricStamp {
                counter: 1,
                device: origin.clone(),
            },
        )
        .expect("a shareable copy")
    };

    laptop.link.share_clipboard(vec![copy(&laptop.id, "hello")]);
    laptop.link.announce();
    desktop.hears(&mut laptop);
    let history = desktop.link.devices().merged_clipboard();
    let copies: Vec<_> = history.entries().map(|entry| &entry.payload).collect();
    assert_eq!(copies, vec![&ClipboardPayload::Text("hello".to_owned())]);

    // The laptop's link refuses to offer the desktop's copy as its own; a frame that does is
    // refused where it arrives.
    let forged = vec![copy(&desktop.id, "not the laptop's")];
    desktop.link.arrived(
        laptop.endpoint,
        &PersonalFabricPayload::Clipboard(forged),
        Instant::now(),
    );
    assert_eq!(
        desktop
            .link
            .devices()
            .get(&laptop.id)
            .map(|device| device.clipboard.len()),
        Some(1)
    );
}

/// An action reaches the device it is addressed to once: the same envelope again is refused.
#[test]
fn an_action_is_delivered_once() {
    let mut laptop = Device::new(0x21, "Laptop");
    let mut desktop = Device::new(0x22, "Desktop");
    desktop.meets(&laptop);
    laptop.meets(&desktop);
    let _ = desktop.sent();
    let _ = laptop.sent();

    laptop
        .link
        .try_send_personal_fabric_action(&desktop.id, PersonalFabricAction::InputLeaseAsk)
        .expect("the desktop is on a session with the laptop");
    let sent = laptop.sent();
    for payload in sent.iter().chain(sent.iter()) {
        desktop
            .link
            .arrived(laptop.endpoint, payload, Instant::now());
    }
    let actions = desktop.link.take_actions();
    assert_eq!(actions.len(), 1);
    assert_eq!(
        actions.first().map(|action| &action.source),
        Some(&laptop.id)
    );
    assert!(
        desktop.sent().is_empty(),
        "taking an action says nothing back"
    );
}

/// A device the person revoked is no sibling: what it says is not taken.
#[test]
fn a_revoked_device_is_no_sibling() {
    let person = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
    let laptop = Device::new(0x21, "Laptop");
    let mut live = Live::new(DeviceId::new("desktop").expect("a device id"));
    // The session takes the revocations the transport verified; the signature is not this
    // check's to make again.
    let revoked = RevocationEntry {
        user_public_key: person.verifying_key().to_bytes(),
        revoked_device_public_key: laptop.cert.device_public_key,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };

    assert_eq!(
        live.sibling(laptop.endpoint, std::slice::from_ref(&laptop.cert), &[]),
        Some(laptop.id.clone())
    );
    assert_eq!(
        live.sibling(
            laptop.endpoint,
            std::slice::from_ref(&laptop.cert),
            std::slice::from_ref(&revoked)
        ),
        None
    );
    assert_eq!(live.device_behind(laptop.endpoint), None);
    assert!(
        live.presence(laptop.endpoint, DevicePresence::default())
            .is_err()
    );
}

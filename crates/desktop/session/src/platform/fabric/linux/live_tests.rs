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
        let (reach, sent) = Reach::listened(cert.user_public_key, Continuity::default());
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

/// A device of the person's is online on another once it said it is there on their session, and
/// only while the session lasts.
#[test]
fn a_device_is_online_while_its_session_lasts() {
    let mut laptop = Device::new(0x21, "Laptop");
    let desktop = Device::new(0x22, "Desktop");

    desktop.meets(&laptop);
    assert!(
        desktop.link.online_device_ids().is_empty(),
        "proved, but it said nothing yet"
    );
    laptop.meets(&desktop);
    desktop.hears(&mut laptop);
    let seen = desktop.link.devices();
    let there = seen.get(&laptop.id).expect("the desktop knows the laptop");
    assert!(there.online);
    assert_eq!(there.about.label, "Laptop");

    desktop.link.sibling_left(laptop.endpoint);
    let seen = desktop.link.devices();
    let gone = seen.get(&laptop.id).expect("still the person's device");
    assert!(!gone.online);
    assert!(desktop.link.online_device_ids().is_empty());
}

/// A frame from an endpoint that proved no certificate of the person's names no device and
/// changes nothing.
#[test]
fn a_stranger_says_nothing_of_the_persons_devices() {
    let desktop = Device::new(0x22, "Desktop");
    let stranger = Device::new(0x31, "Stranger");

    desktop.link.arrived(
        stranger.endpoint,
        &PersonalFabricPayload::Here,
        Instant::now(),
    );

    assert!(desktop.link.devices().get(&stranger.id).is_none());
    assert!(desktop.link.online_device_ids().is_empty());
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
        .try_send_personal_fabric_action(
            &desktop.id,
            PersonalFabricAction::RemoteAudioCancel {
                session_id: [1; 16],
            },
        )
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
    assert!(live.here(laptop.endpoint).is_err());
}

//! The person's devices driven through the fabric's published actions, each device's providers
//! wired to the others by the in-memory link: the device list, the lock that follows the person,
//! waking a sleeping device, and unlocking and enrolling a disk at boot.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::time::Instant;

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::ActionCallError;
use tilo_fabric::testing::{Person, TestDevice, uncertified};
use tilo_fabric::wake::WakeEvent;
use tilo_fabric::{
    Answer, BootRequest, DeviceAbout, Empty, Followed, Hello, Ipv4Subnet, ListedDevice,
    OwnerReading, PROVIDER, Packed, Polled, Probe, SayLock, UNLOCK_SCOPE, UnlockAsk, UnlockReading,
    WakeInterfaceKind, WakeRoute, Woken, pack, unpack,
};
use tilo_seal::SecretKey;
use tilo_unlock_protocol::{
    BootId, EnrolReply, EnrolRequest, Half, PROTOCOL_VERSION, SiblingFrame, SlotId, UnlockHello,
    UnlockRequest, open_release,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

fn subnet(text: &str) -> Ipv4Subnet {
    let (address, prefix) = text.split_once('/').unwrap();
    Ipv4Subnet::containing(address.parse().unwrap(), prefix.parse().unwrap()).unwrap()
}

fn route(mac: u8, on: &str) -> WakeRoute {
    WakeRoute {
        mac: [0x52, 0x54, 0, 0, 0, mac],
        kind: WakeInterfaceKind::Ethernet,
        magic_packet: true,
        subnet: Some(subnet(on)),
    }
}

fn now_ms() -> u64 {
    tilo_clock::unix_now_ms()
}

fn say_lock(
    device: &TestDevice,
    locked: bool,
    input_unix_ms: u64,
    change: Option<&str>,
) -> Followed {
    let said: SayLock = serde_json::from_value(json!({
        "locked": locked,
        "input_unix_ms": input_unix_ms,
        "in_use": true,
        "change": change,
    }))
    .unwrap();
    device
        .here(PROVIDER, "owner", "say-lock", said)
        .expect("the session here says its lock")
}

fn tick(devices: &[&TestDevice]) {
    for device in devices {
        device.fabric.tick(Instant::now());
    }
}

#[test]
fn the_device_list_is_this_device_first_then_online_then_by_name() {
    let person = Person::new();
    let laptop = person.device(2, "Laptop", true);
    let tower = person.device(3, "Tower", false);
    let phone = person.device(4, "Anna's phone", false);
    let tv = person.device(5, "Television", false);
    laptop.interfaces.set(vec![route(2, "10.0.0.2/24")]);
    tower.interfaces.set(vec![route(3, "10.0.0.3/24")]);
    Person::connect(&[&laptop, &tower, &tv]);
    tick(&[&laptop, &tower, &tv]);
    laptop.fabric.heard_nearby([tower.id].into_iter().collect());

    let listed: Vec<ListedDevice> = laptop.read(PROVIDER, "devices");
    let labels: Vec<&str> = listed.iter().map(|row| row.label.as_str()).collect();
    // The phone never came online here, but it keeps what it is in the person's store.
    assert_eq!(labels, ["Laptop", "Television", "Tower", "Anna's phone"]);
    let row = |label: &str| listed.iter().find(|row| row.label == label).unwrap();
    assert!(row("Laptop").local && row("Laptop").online);
    assert!(row("Tower").online && row("Tower").nearby && row("Tower").wakeable);
    assert!(row("Television").online && !row("Television").nearby);
    assert!(!row("Anna's phone").online);
    assert!(!row("Television").wakeable, "it said no way to wake it");
    drop(phone);

    // Another device reads the same devices, itself first.
    let from_tv: Vec<ListedDevice> = tv.read(PROVIDER, "devices");
    assert_eq!(
        from_tv.first().map(|row| row.label.as_str()),
        Some("Television")
    );
    assert_eq!(from_tv.len(), 4);

    // What a device says of itself is only another device's to tell.
    let refused = laptop.here::<Value>(
        PROVIDER,
        "presence",
        "tell",
        json!({ "presence": { "wake_routes": [], "session": { "locked": false, "changed_unix_ms": 0, "active_unix_ms": 0 }, "display": null } }),
    );
    assert_eq!(refused.unwrap_err(), ActionCallError::NotPermitted);
}

#[test]
fn what_a_device_is_lives_in_the_persons_store() {
    let person = Person::new();
    let laptop = person.device(2, "Laptop", true);
    let facts: Vec<(tilo_fabric::DeviceId, DeviceAbout)> =
        tilo_fabric::read_facts(laptop.facts.as_ref(), tilo_fabric::ABOUT);
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].0, laptop.id);
    assert_eq!(facts[0].1.label, "Laptop");
}

#[test]
fn the_lock_follows_the_person_across_their_devices() {
    let person = Person::new();
    let laptop = person.device(2, "Laptop", true);
    let phone = person.device(3, "Phone", false);
    laptop.interfaces.set(vec![route(2, "10.0.0.2/24")]);
    phone.interfaces.set(vec![route(3, "10.0.0.3/24")]);
    Person::connect(&[&laptop, &phone]);
    tick(&[&laptop, &phone]);
    let earlier = now_ms() - 60_000;
    say_lock(&laptop, false, earlier, None);
    say_lock(&phone, false, earlier, None);

    // The idle timer's lock is not followed.
    say_lock(&phone, true, earlier, Some("idle"));
    assert!(laptop.seat.inputs("lock#follow").is_empty());
    say_lock(&phone, false, earlier, Some("person"));

    // The person locks the phone: the laptop, not used since, locks too.
    say_lock(&phone, true, earlier, Some("person"));
    assert_eq!(
        laptop.seat.inputs("lock#follow"),
        [json!({ "locked": true })]
    );
    let reading: OwnerReading = laptop.read(PROVIDER, "owner");
    assert!(reading.followed_lock);
    assert_eq!(reading.devices.len(), 1);
    assert!(reading.devices[0].locked && reading.devices[0].nearby);

    // The laptop's session says it locked; the person unlocks the phone beside it, and the laptop,
    // in use when it followed, unlocks too.
    say_lock(&laptop, true, earlier, None);
    let answered = say_lock(&phone, false, now_ms() + 5_000, Some("person"));
    assert_eq!(
        answered.followed, None,
        "the phone follows nothing of its own unlock"
    );
    assert_eq!(
        laptop.seat.inputs("lock#follow"),
        [json!({ "locked": true }), json!({ "locked": false })]
    );

    // Idle time on the laptop counts from the phone's input.
    let reading: OwnerReading = laptop.read(PROVIDER, "owner");
    assert!(
        reading
            .used_nearby_unix_ms
            .is_some_and(|used| used > earlier)
    );
}

#[test]
fn a_lock_elsewhere_is_not_followed_where_the_person_works_after_it() {
    let person = Person::new();
    let laptop = person.device(2, "Laptop", true);
    let phone = person.device(3, "Phone", false);
    Person::connect(&[&laptop, &phone]);
    say_lock(&phone, false, now_ms() - 60_000, None);
    say_lock(&phone, true, now_ms() - 60_000, Some("person"));
    // The laptop's session says the person typed after the phone locked.
    let followed = say_lock(&laptop, false, now_ms() + 1_000, None);
    assert_eq!(followed.followed, None);
    assert!(laptop.seat.inputs("lock#follow").is_empty());
}

#[test]
fn a_device_without_a_session_cannot_follow_a_lock() {
    let person = Person::new();
    let laptop = person.device(2, "Laptop", true);
    let phone = person.device(3, "Phone", false);
    Person::connect(&[&laptop, &phone]);
    laptop.seat.detach(true);
    say_lock(&laptop, false, now_ms() - 60_000, None);
    let followed = say_lock(&phone, true, now_ms() - 60_000, Some("person"));
    assert_eq!(followed.followed, None);
    // The rule decided to follow; the seat refused it, as a device with no session does.
    assert!(laptop.seat.calls().is_empty());
}

#[test]
fn a_sleeping_device_is_woken_here_or_by_a_device_on_its_network() {
    let person = Person::new();
    let phone = person.device(2, "Phone", true);
    let tower = person.device(3, "Tower", false);
    let server = person.device(4, "Server", false);
    phone.interfaces.set(vec![route(2, "192.168.1.2/24")]);
    tower.interfaces.set(vec![route(3, "10.0.0.3/24")]);
    server.interfaces.set(vec![route(4, "10.0.0.4/24")]);
    Person::connect(&[&phone, &tower, &server]);
    tick(&[&phone, &tower, &server]);
    tower.sleep(&[&phone, &server]);

    // Refusals name what is wrong.
    let unknown = phone.here::<Woken>(PROVIDER, "wake", "wake", json!({ "device": "Fridge" }));
    assert!(matches!(unknown, Err(ActionCallError::Refused { .. })));
    let itself = phone.here::<Woken>(PROVIDER, "wake", "wake", json!({ "device": "Phone" }));
    assert!(matches!(itself, Err(ActionCallError::Refused { .. })));

    // The phone is not on the tower's network: the server is, and sends the packet.
    let woken: Woken = phone
        .here(PROVIDER, "wake", "wake", json!({ "device": "Tower" }))
        .unwrap();
    assert_eq!(
        woken,
        Woken {
            waker: "Server".into(),
            asked: true,
            subnet: "10.0.0.0/24".into(),
        }
    );
    assert!(phone.packets.sent().is_empty());
    assert_eq!(
        server.packets.sent(),
        [([0x52, 0x54, 0, 0, 0, 3], subnet("10.0.0.0/24"))]
    );

    // The tower comes back: the server sees it rejoin and tells the phone.
    Person::connect(&[&phone, &tower, &server]);
    tick(&[&server, &phone]);
    let wakes: Vec<WakeEvent> = phone.read(PROVIDER, "wake");
    let outcomes: Vec<Value> = wakes
        .iter()
        .map(|event| serde_json::to_value(event.outcome).unwrap())
        .collect();
    assert!(
        outcomes.contains(&json!("rejoined")),
        "the phone hears the tower is awake: {outcomes:?}"
    );
    assert!(wakes.iter().all(|event| event.label == "Tower"));

    // The server, on the tower's network, wakes it itself.
    tower.sleep(&[&phone, &server]);
    let woken: Woken = server
        .here(PROVIDER, "wake", "wake", json!({ "device": "Tower" }))
        .unwrap();
    assert!(!woken.asked);
    assert_eq!(server.packets.sent().len(), 2);
}

#[test]
fn only_the_persons_devices_ask_a_wake_or_a_probe() {
    let person = Person::new();
    let phone = person.device(2, "Phone", true);
    let nonce = serde_json::to_value(Probe { nonce: [1; 16] }).unwrap();
    let refused = phone.here::<Value>(PROVIDER, "wake", "probe", nonce.clone());
    assert_eq!(refused.unwrap_err(), ActionCallError::NotPermitted);
    let tower = person.device(3, "Tower", false);
    let answered: Value = tower
        .ask(&phone.id, PROVIDER, "wake", "probe", nonce.clone())
        .unwrap();
    assert_eq!(answered, nonce);
}

// ---- disk unlock

fn slot() -> SlotId {
    SlotId {
        volume_uuid: [5; 16],
        keyslot: 2,
    }
}

fn half() -> Half {
    Half::from_bytes([6; 32])
}

/// One early boot of an enrolled device: its boot key, the certificate it holds, its ephemeral key.
struct Boot {
    key: SigningKey,
    cert: DeviceCert,
    ephemeral: SecretKey,
    id: BootId,
}

impl Boot {
    fn hello(&self) -> String {
        pack(&UnlockHello {
            version: PROTOCOL_VERSION,
            boot_cert: self.cert.clone(),
        })
        .unwrap()
    }

    fn frame(packed: &Packed) -> SiblingFrame {
        unpack(&packed.frame).unwrap()
    }

    /// Ask `sibling` to unlock, as the boot key alone: the answer to the challenge, and the
    /// request it signed.
    fn ask(
        &self,
        link: &tilo_fabric::testing::Link,
        sibling: &TestDevice,
    ) -> (SiblingFrame, BootRequest, UnlockRequest) {
        let boot = self.key.verifying_key().to_bytes();
        let challenge = match Self::frame(
            &uncertified(
                link,
                boot,
                &sibling.id,
                UNLOCK_SCOPE,
                "unlock",
                "hello",
                Hello {
                    hello: self.hello(),
                },
            )
            .unwrap(),
        ) {
            SiblingFrame::Challenge(challenge) => challenge,
            other => return (other, self.request_with(None), self.unsigned()),
        };
        let request = UnlockRequest::sign(
            &self.key,
            &self.cert,
            slot(),
            &challenge,
            self.id,
            &self.ephemeral.public_key().unwrap(),
            None,
        )
        .unwrap();
        let asked = self.request_with(Some(&request));
        let frame = Self::frame(
            &uncertified(
                link,
                boot,
                &sibling.id,
                UNLOCK_SCOPE,
                "unlock",
                "request",
                asked.clone(),
            )
            .unwrap(),
        );
        (frame, asked, request)
    }

    fn unsigned(&self) -> UnlockRequest {
        UnlockRequest {
            version: PROTOCOL_VERSION,
            boot_id: self.id,
            slot: slot(),
            ephemeral_public: [0; 32],
            nonce: [0; 32],
            attestation: None,
            signature: [0; 64],
        }
    }

    fn request_with(&self, request: Option<&UnlockRequest>) -> BootRequest {
        BootRequest {
            hello: self.hello(),
            request: pack(&request.cloned().unwrap_or_else(|| self.unsigned())).unwrap(),
        }
    }

    fn poll(
        &self,
        link: &tilo_fabric::testing::Link,
        sibling: &TestDevice,
        asked: &BootRequest,
    ) -> SiblingFrame {
        let boot = self.key.verifying_key().to_bytes();
        Self::frame(
            &uncertified(
                link,
                boot,
                &sibling.id,
                UNLOCK_SCOPE,
                "unlock",
                "poll",
                asked.clone(),
            )
            .unwrap(),
        )
    }

    fn open(
        &self,
        sibling: &TestDevice,
        request: &UnlockRequest,
        frame: &SiblingFrame,
    ) -> [u8; 32] {
        let SiblingFrame::Released(release) = frame else {
            panic!("expected a release, got {frame:?}");
        };
        let digest = request
            .digest(&self.cert, &sibling.key.verifying_key().to_bytes())
            .unwrap();
        *open_release(&self.ephemeral, &digest, release)
            .unwrap()
            .expose()
    }
}

/// The laptop, without the person's root, enrols its boot key through the phone, which holds it.
fn enrolled(person: &Person, phone: &TestDevice, laptop: &TestDevice) -> Boot {
    let key = SigningKey::from_bytes(&[30; 32]);
    let request = EnrolRequest::new(&key, slot(), None, None, half()).unwrap();
    let asked: Polled = laptop
        .ask(
            &phone.id,
            UNLOCK_SCOPE,
            "enrol",
            "ask",
            Packed {
                frame: pack(&request).unwrap(),
            },
        )
        .unwrap();
    assert!(asked.reply.is_none(), "the person is asked first");
    let asks: Vec<UnlockAsk> = phone.read(PROVIDER, "unlock-asks");
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].device, "Laptop");
    let _: Value = phone
        .here(
            PROVIDER,
            "unlock",
            "answer",
            Answer {
                ask: asks[0].ask.clone(),
                allow: true,
            },
        )
        .unwrap();
    let polled: Polled = laptop
        .ask(&phone.id, UNLOCK_SCOPE, "enrol", "poll", Empty {})
        .unwrap();
    let reply: EnrolReply = unpack(&polled.reply.expect("answered")).unwrap();
    let EnrolReply::Enrolled(response) = reply else {
        panic!("expected an enrolment, got {reply:?}");
    };
    assert_eq!(
        response.cert.device_public_key,
        key.verifying_key().to_bytes()
    );
    let _ = person;
    Boot {
        key,
        cert: response.cert,
        ephemeral: SecretKey::from_bytes([40; 32]),
        id: BootId([7; 16]),
    }
}

#[test]
fn a_boot_receives_the_half_the_person_allowed_and_hears_it_again() {
    let person = Person::new();
    let phone = person.device(2, "Phone", true);
    let laptop = person.device(3, "Laptop", false);
    Person::connect(&[&phone, &laptop]);
    let boot = enrolled(&person, &phone, &laptop);

    let (frame, asked, request) = boot.ask(&person.link, &phone);
    let SiblingFrame::Pending { approval_code } = frame else {
        panic!("expected the person to be asked, got {frame:?}");
    };
    let asks: Vec<UnlockAsk> = phone.read(PROVIDER, "unlock-asks");
    assert_eq!(asks.len(), 1);
    assert_eq!(
        asks[0].approval_code.as_deref(),
        Some(approval_code.as_str())
    );
    assert_eq!(asks[0].device, "Laptop");
    assert!(matches!(
        boot.poll(&person.link, &phone, &asked),
        SiblingFrame::Pending { .. }
    ));

    let _: Value = phone
        .here(
            PROVIDER,
            "unlock",
            "answer",
            Answer {
                ask: asks[0].ask.clone(),
                allow: true,
            },
        )
        .unwrap();
    let released = boot.poll(&person.link, &phone, &asked);
    assert_eq!(boot.open(&phone, &request, &released), *half().expose());

    // The same boot asks again (its stream was lost): the kept answer releases it, unasked.
    let (again, _, request) = boot.ask(&person.link, &phone);
    assert_eq!(boot.open(&phone, &request, &again), *half().expose());
    let asks: Vec<UnlockAsk> = phone.read(PROVIDER, "unlock-asks");
    assert!(asks.is_empty());
    let reading: UnlockReading = phone.read(PROVIDER, "unlock");
    assert!(reading.key_holder);
    assert_eq!(reading.enrolled.len(), 1);
    assert_eq!(
        reading.answers, 1,
        "the answer is kept for the person's devices"
    );
}

#[test]
fn a_denied_boot_hears_the_denial_without_a_second_question() {
    let person = Person::new();
    let phone = person.device(2, "Phone", true);
    let laptop = person.device(3, "Laptop", false);
    let boot = enrolled(&person, &phone, &laptop);
    let (_, asked, _) = boot.ask(&person.link, &phone);
    let asks: Vec<UnlockAsk> = phone.read(PROVIDER, "unlock-asks");
    let _: Value = phone
        .here(
            PROVIDER,
            "unlock",
            "answer",
            Answer {
                ask: asks[0].ask.clone(),
                allow: false,
            },
        )
        .unwrap();
    assert_eq!(
        boot.poll(&person.link, &phone, &asked),
        SiblingFrame::Denied
    );
    let (again, _, _) = boot.ask(&person.link, &phone);
    assert_eq!(again, SiblingFrame::Denied);
    let asks: Vec<UnlockAsk> = phone.read(PROVIDER, "unlock-asks");
    assert!(asks.is_empty());
}

#[test]
fn a_boot_that_went_away_takes_its_question_with_it() {
    let person = Person::new();
    let phone = person.device(2, "Phone", true);
    let laptop = person.device(3, "Laptop", false);
    let boot = enrolled(&person, &phone, &laptop);
    let (_, asked, _) = boot.ask(&person.link, &phone);
    let boot_key = boot.key.verifying_key().to_bytes();
    let _: Value = uncertified(
        &person.link,
        boot_key,
        &phone.id,
        UNLOCK_SCOPE,
        "unlock",
        "withdraw",
        asked,
    )
    .unwrap();
    let asks: Vec<UnlockAsk> = phone.read(PROVIDER, "unlock-asks");
    assert!(asks.is_empty());
}

#[test]
fn a_boot_that_is_not_the_persons_or_asks_no_key_holder_is_refused() {
    let person = Person::new();
    let phone = person.device(2, "Phone", true);
    let laptop = person.device(3, "Laptop", false);
    let boot = enrolled(&person, &phone, &laptop);

    // The laptop holds no root: it releases nothing.
    let (frame, _, _) = boot.ask(&person.link, &laptop);
    assert!(matches!(frame, SiblingFrame::Refused(_)), "{frame:?}");

    // A boot key someone else's root certified.
    let stranger = SigningKey::from_bytes(&[99; 32]);
    let key = SigningKey::from_bytes(&[31; 32]);
    let mut cert = DeviceCert {
        user_public_key: stranger.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Boot".into(),
        capabilities: DeviceCapabilities::nothing(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &stranger).unwrap();
    let foreign = Boot {
        key,
        cert,
        ephemeral: SecretKey::from_bytes([41; 32]),
        id: BootId([8; 16]),
    };
    let (frame, _, _) = foreign.ask(&person.link, &phone);
    assert!(matches!(frame, SiblingFrame::Refused(_)), "{frame:?}");
    let asks: Vec<UnlockAsk> = phone.read(PROVIDER, "unlock-asks");
    assert!(asks.is_empty(), "nobody is asked for a stranger's boot");

    // A device without the root answers an enrolment with who can.
    let request = EnrolRequest::new(
        &SigningKey::from_bytes(&[32; 32]),
        slot(),
        None,
        None,
        half(),
    )
    .unwrap();
    let polled: Polled = phone
        .ask(
            &laptop.id,
            UNLOCK_SCOPE,
            "enrol",
            "ask",
            Packed {
                frame: pack(&request).unwrap(),
            },
        )
        .unwrap();
    let reply: EnrolReply = unpack(&polled.reply.unwrap()).unwrap();
    assert_eq!(reply, EnrolReply::NotKeyHolder);
}

#[test]
fn the_root_holder_enrols_its_own_boot_key_for_an_unattended_unlock() {
    let person = Person::new();
    let phone = person.device(2, "Phone", true);
    let laptop = person.device(3, "Laptop", false);
    let request = EnrolRequest::new(
        &SigningKey::from_bytes(&[33; 32]),
        slot(),
        None,
        Some([9; 32]),
        half(),
    )
    .unwrap();
    let enrolled: Packed = phone
        .here(
            PROVIDER,
            "unlock",
            "enrol-boot-key",
            Packed {
                frame: pack(&request).unwrap(),
            },
        )
        .unwrap();
    let reply: EnrolReply = unpack(&enrolled.frame).unwrap();
    let EnrolReply::Enrolled(response) = reply else {
        panic!("expected an enrolment, got {reply:?}");
    };
    // Every other device of the person's holds the half once the enrolment reaches it.
    let siblings: Vec<[u8; 32]> = response
        .siblings
        .iter()
        .map(|sibling| sibling.device_public_key)
        .collect();
    assert_eq!(siblings, [laptop.key.verifying_key().to_bytes()]);
    // The laptop reads the enrolment and the root-signed unattended acceptance from the store.
    let reading: UnlockReading = laptop.read(PROVIDER, "unlock");
    assert!(!reading.key_holder);
    assert_eq!(reading.enrolled.len(), 1);
    assert!(reading.enrolled[0].unattended);
    assert_eq!(reading.enrolled[0].device, "Phone");

    // Only the person here enrols a key with it.
    let refused = laptop.ask::<Packed>(
        &phone.id,
        PROVIDER,
        "unlock",
        "enrol-boot-key",
        Packed {
            frame: pack(&request).unwrap(),
        },
    );
    assert_eq!(refused.unwrap_err(), ActionCallError::NotPermitted);
}

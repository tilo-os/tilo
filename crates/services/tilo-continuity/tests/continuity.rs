//! What continues across the person's devices, driven through each service's published actions,
//! with each device's providers wired to the others by the fabric's in-memory link and the
//! session on each device a recorder.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Value, json};
use tilo_actions::remote::ActionCallError;
use tilo_continuity::Switches;
use tilo_continuity::accessories::{self, Accessories, Bluetooth, Notice, Stayed, WAIT};
use tilo_continuity::clipboard::{self, Clipboard, ClipboardEntry, ClipboardPayload, Copies};
use tilo_continuity::notifications::{self, Mirror, Projected};
use tilo_continuity::wifi::{self, Credentials, Password, Wifi, WifiCredential};
use tilo_fabric::testing::{Person, TestDevice};
use tilo_fabric::{DeviceId, FabricStamp};

/// The person's shared passphrases, as their store keeps them: one per network.
#[derive(Default)]
struct Kept(Mutex<BTreeMap<[u8; 32], WifiCredential>>);

impl Credentials for Kept {
    fn keep(&self, credential: &WifiCredential) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .insert(credential.network_id, credential.clone());
        Ok(())
    }

    fn all(&self) -> Vec<WifiCredential> {
        self.0.lock().unwrap().values().cloned().collect()
    }
}

/// A machine's Bluetooth: what is connected and bonded here, and what was done to it. Work runs
/// at once unless held, when it waits for [`FakeBluetooth::release_held`].
#[derive(Default)]
struct FakeBluetooth {
    connected: Mutex<BTreeMap<String, String>>,
    paired: Mutex<BTreeSet<String>>,
    done: Mutex<Vec<String>>,
    hold: Mutex<bool>,
    held: Mutex<VecDeque<Box<dyn FnOnce() + Send>>>,
}

impl FakeBluetooth {
    fn with(connected: &[(&str, &str)], paired: &[&str]) -> Arc<Self> {
        let bluetooth = Self::default();
        for (address, name) in connected {
            bluetooth
                .connected
                .lock()
                .unwrap()
                .insert((*address).to_owned(), (*name).to_owned());
            bluetooth
                .paired
                .lock()
                .unwrap()
                .insert((*address).to_owned());
        }
        for address in paired {
            bluetooth
                .paired
                .lock()
                .unwrap()
                .insert((*address).to_owned());
        }
        Arc::new(bluetooth)
    }

    fn done(&self) -> Vec<String> {
        self.done.lock().unwrap().clone()
    }
}

impl Bluetooth for FakeBluetooth {
    fn connected_name(&self, address: &str) -> Result<String, String> {
        self.connected
            .lock()
            .unwrap()
            .get(address)
            .cloned()
            .ok_or_else(|| "not connected".to_owned())
    }

    fn paired(&self, address: &str) -> bool {
        self.paired.lock().unwrap().contains(address)
    }

    fn disconnect(&self, address: &str) -> Result<(), String> {
        self.done
            .lock()
            .unwrap()
            .push(format!("disconnect {address}"));
        self.connected.lock().unwrap().remove(address);
        Ok(())
    }

    fn connect(&self, address: &str) -> Result<(), String> {
        self.done.lock().unwrap().push(format!("connect {address}"));
        self.connected
            .lock()
            .unwrap()
            .insert(address.to_owned(), "Headphones".to_owned());
        Ok(())
    }

    fn run(&self, work: Box<dyn FnOnce() + Send>) {
        if *self.hold.lock().unwrap() {
            self.held.lock().unwrap().push_back(work);
        } else {
            work();
        }
    }
}

fn copy(device: &TestDevice, text: &str, at: u64) -> ClipboardEntry {
    ClipboardEntry::shareable(
        ClipboardPayload::Text(text.to_owned()),
        device.id.clone(),
        at,
        FabricStamp {
            counter: at,
            device: device.id.clone(),
        },
    )
    .unwrap()
}

fn texts(copies: &Copies) -> Vec<String> {
    copies
        .entries
        .iter()
        .map(|entry| match &entry.payload {
            ClipboardPayload::Text(text) => text.clone(),
            ClipboardPayload::Binary { mime, .. } => mime.clone(),
        })
        .collect()
}

fn clipboard_on(device: &TestDevice) {
    let service = Clipboard::new(&device.fabric);
    device
        .link
        .mount(&device.id, clipboard::PROVIDER, service.provider());
}

#[test]
fn copies_made_here_join_the_clipboard_of_the_persons_other_devices() {
    let person = Person::new();
    let laptop = person.device(2, "Laptop", true);
    let phone = person.device(3, "Phone", false);
    clipboard_on(&laptop);
    clipboard_on(&phone);

    // A copy offered before the phone is here reaches it when it arrives.
    let _: Value = laptop
        .here(
            clipboard::PROVIDER,
            "clipboard",
            "offer",
            Copies {
                entries: vec![copy(&laptop, "hello", 1)],
            },
        )
        .unwrap();
    Person::connect(&[&laptop, &phone]);
    let history: Copies = phone.read(clipboard::PROVIDER, "history");
    assert_eq!(texts(&history), ["hello"]);
    let set = phone.seat.inputs("clipboard#set");
    assert_eq!(set.len(), 1, "the phone's session shows the laptop's copy");

    // The phone's copy joins too, newest first.
    let _: Value = phone
        .here(
            clipboard::PROVIDER,
            "clipboard",
            "offer",
            Copies {
                entries: vec![copy(&phone, "world", 2)],
            },
        )
        .unwrap();
    let history: Copies = laptop.read(clipboard::PROVIDER, "history");
    assert_eq!(texts(&history), ["world", "hello"]);

    // A copy that claims to be another device's, or whose content is not what its id names, is
    // refused.
    let forged = Copies {
        entries: vec![copy(&phone, "not mine", 3)],
    };
    let refused = laptop.here::<Value>(clipboard::PROVIDER, "clipboard", "offer", forged);
    assert!(matches!(
        refused,
        Err(ActionCallError::InvalidRequest { .. })
    ));
    let mut tampered = copy(&laptop, "safe", 4);
    tampered.payload = ClipboardPayload::Text("changed".to_owned());
    let refused = laptop.ask::<Value>(
        &phone.id,
        clipboard::PROVIDER,
        "clipboard",
        "offered",
        Copies {
            entries: vec![tampered],
        },
    );
    assert!(matches!(
        refused,
        Err(ActionCallError::InvalidRequest { .. })
    ));

    // A device that leaves takes its copies with it.
    laptop.sleep(&[&phone]);
    let history: Copies = phone.read(clipboard::PROVIDER, "history");
    assert_eq!(texts(&history), ["world"]);
}

fn mirror_on(device: &TestDevice, switches: &Switches) {
    let mirror = Mirror::new(device.fabric.clone(), switches.clone());
    device
        .link
        .mount(&device.id, notifications::PROVIDER, mirror.provider());
}

fn project(device: &TestDevice, daemon_id: u32, previews: bool) -> Projected {
    device
        .here(
            notifications::PROVIDER,
            "notifications",
            "project",
            json!({
                "daemon_id": daemon_id,
                "log_id": u64::from(daemon_id) + 100,
                "app_id": "so.tilo.mail",
                "app_name": "Mail",
                "title": "New mail",
                "body": "Lunch?",
                "actions": [["open", "Open"]],
                "urgency": 0,
                "dismiss_after_ms": 60_000,
                "previews": previews,
            }),
        )
        .unwrap()
}

#[test]
fn a_notification_here_shows_on_the_persons_other_devices_and_answers_back() {
    let person = Person::new();
    let laptop = person.device(2, "Laptop", true);
    let phone = person.device(3, "Phone", false);
    let switches = Switches::unset();
    mirror_on(&laptop, &switches);
    mirror_on(&phone, &switches);
    Person::connect(&[&laptop, &phone]);
    phone
        .seat
        .answer("notifications#show-remote", json!({ "log_id": 9 }));

    // Without previews the body stays here.
    let projected = project(&laptop, 1, false);
    assert!(projected.id.is_some());
    let shown = phone.seat.inputs("notifications#show-remote");
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0]["device"], json!("Laptop"));
    assert_eq!(shown[0]["source"], json!(laptop.id));
    assert_eq!(shown[0]["notification"]["body"], json!(""));
    assert_eq!(shown[0]["notification"]["body_redacted"], json!(true));

    // The person chooses "Open" on the phone: the laptop's app is told.
    let _: Value = phone
        .here(
            notifications::PROVIDER,
            "notifications",
            "choose",
            json!({ "log_id": 9, "action": "open" }),
        )
        .unwrap();
    assert_eq!(
        laptop.seat.inputs("notifications#answered"),
        [json!({ "daemon_id": 1, "action": "open" })]
    );
    // Answered once, it is no longer another device's to answer.
    let again = phone.here::<Value>(
        notifications::PROVIDER,
        "notifications",
        "choose",
        json!({ "log_id": 9 }),
    );
    assert!(matches!(again, Err(ActionCallError::Refused { .. })));

    // An action the app did not offer runs nothing.
    let id = projected.id.unwrap();
    let refused = phone.ask::<Value>(
        &laptop.id,
        notifications::PROVIDER,
        "notifications",
        "answer",
        json!({ "id": id, "action": "delete-everything" }),
    );
    assert!(matches!(refused, Err(ActionCallError::Refused { .. })));

    // The person is done with it here: the phone takes its copy away.
    let _: Value = laptop
        .here(
            notifications::PROVIDER,
            "notifications",
            "retire",
            json!({ "daemon_id": 1 }),
        )
        .unwrap();
    assert_eq!(
        phone.seat.inputs("notifications#retire-remote"),
        [json!({ "source": laptop.id, "id": id })]
    );

    // With previews the body travels; a row no longer shown here is retired everywhere.
    project(&laptop, 2, true);
    let shown = phone.seat.inputs("notifications#show-remote");
    assert_eq!(shown[1]["notification"]["body"], json!("Lunch?"));
    let _: Value = laptop
        .here(
            notifications::PROVIDER,
            "notifications",
            "prune",
            json!({ "shown": [] }),
        )
        .unwrap();
    assert_eq!(phone.seat.inputs("notifications#retire-remote").len(), 2);
}

#[test]
fn notifications_stay_here_with_the_switch_off() {
    let person = Person::new();
    let laptop = person.device(2, "Laptop", true);
    let phone = person.device(3, "Phone", false);
    let switches = Switches::unset();
    mirror_on(&laptop, &switches);
    mirror_on(&phone, &switches);
    Person::connect(&[&laptop, &phone]);
    let mut settings = serde_json::Map::new();
    settings.insert("continuity".into(), json!({ "notifications": false }));
    switches.read(&settings);
    assert_eq!(project(&laptop, 1, true).id, None);
    assert!(phone.seat.inputs("notifications#show-remote").is_empty());
}

#[test]
fn a_device_without_a_session_cannot_show_another_devices_notification() {
    let person = Person::new();
    let laptop = person.device(2, "Laptop", true);
    let phone = person.device(3, "Phone", false);
    let switches = Switches::unset();
    mirror_on(&laptop, &switches);
    mirror_on(&phone, &switches);
    Person::connect(&[&laptop, &phone]);
    phone.seat.detach(true);
    assert!(project(&laptop, 1, true).id.is_some());
    assert!(phone.seat.calls().is_empty());
    // Nothing is shown there, so nothing there answers it.
    let choose = phone.here::<Value>(
        notifications::PROVIDER,
        "notifications",
        "choose",
        json!({ "log_id": 0 }),
    );
    assert!(matches!(choose, Err(ActionCallError::Refused { .. })));
}

fn wifi_on(device: &TestDevice, kept: &Arc<Kept>, switches: &Switches) {
    let wifi = Wifi::new(
        device.fabric.clone(),
        device.key.clone(),
        kept.clone(),
        switches.clone(),
    );
    device
        .link
        .mount(&device.id, wifi::PROVIDER, wifi.provider());
}

#[test]
fn a_passphrase_shared_by_one_device_joins_the_network_on_another() {
    let person = Person::new();
    let laptop = person.device(2, "Laptop", true);
    let phone = person.device(3, "Phone", false);
    let kept = Arc::new(Kept::default());
    let switches = Switches::unset();
    wifi_on(&laptop, &kept, &switches);
    wifi_on(&phone, &kept, &switches);

    let shared: Value = laptop
        .here(
            wifi::PROVIDER,
            "wifi",
            "share",
            json!({ "ssid": "Home", "security": "wpa3", "password": "correct horse" }),
        )
        .unwrap();
    assert_eq!(shared, json!({ "ssid": "Home" }));
    // Only ciphertext is kept.
    let credential = kept.all().pop().unwrap();
    assert!(
        !credential
            .sealed_secret
            .windows(b"correct horse".len())
            .any(|window| window == b"correct horse")
    );

    let password: Password = phone
        .here(
            wifi::PROVIDER,
            "wifi",
            "password",
            json!({ "ssid": "Home", "security": "wpa3" }),
        )
        .unwrap();
    assert_eq!(password.password.as_deref(), Some("correct horse"));
    let other: Password = phone
        .here(
            wifi::PROVIDER,
            "wifi",
            "password",
            json!({ "ssid": "Home", "security": "wpa2" }),
        )
        .unwrap();
    assert_eq!(
        other.password, None,
        "the security mode is part of the network"
    );

    // A device certified after the share holds no envelope until a device shares it again.
    let tablet = person.device(4, "Tablet", false);
    wifi_on(&tablet, &kept, &switches);
    let none: Password = tablet
        .here(
            wifi::PROVIDER,
            "wifi",
            "password",
            json!({ "ssid": "Home", "security": "wpa3" }),
        )
        .unwrap();
    assert_eq!(none.password, None);

    // With the switch off nothing is shared or read.
    let mut settings = serde_json::Map::new();
    settings.insert("continuity".into(), json!({ "wifi_settings": false }));
    switches.read(&settings);
    let off: Password = phone
        .here(
            wifi::PROVIDER,
            "wifi",
            "password",
            json!({ "ssid": "Home", "security": "wpa3" }),
        )
        .unwrap();
    assert_eq!(off.password, None);
    let refused = laptop.here::<Value>(
        wifi::PROVIDER,
        "wifi",
        "share",
        json!({ "ssid": "Cafe", "security": "wpa2", "password": "pastry" }),
    );
    assert!(matches!(refused, Err(ActionCallError::Refused { .. })));
}

const HEADPHONES: &str = "AA:BB:CC:DD:EE:FF";

fn accessories_on(device: &TestDevice, bluetooth: &Arc<FakeBluetooth>) -> Accessories {
    let service = Accessories::new(device.fabric.clone(), bluetooth.clone());
    device
        .link
        .mount(&device.id, accessories::PROVIDER, service.provider());
    service
}

fn notices(device: &TestDevice) -> Vec<Notice> {
    device
        .seat
        .inputs("notices#tell")
        .into_iter()
        .map(|notice| serde_json::from_value(notice).unwrap())
        .collect()
}

fn move_to(from: &TestDevice, to: &DeviceId) {
    let _: Value = from
        .here(
            accessories::PROVIDER,
            "accessories",
            "move",
            json!({ "address": HEADPHONES, "device": to }),
        )
        .unwrap();
}

#[test]
fn an_accessory_moves_to_a_device_that_holds_its_bond() {
    let person = Person::new();
    let phone = person.device(2, "Phone", true);
    let laptop = person.device(3, "Laptop", false);
    let phone_bt = FakeBluetooth::with(&[(HEADPHONES, "Headphones")], &[]);
    let laptop_bt = FakeBluetooth::with(&[], &[HEADPHONES]);
    accessories_on(&phone, &phone_bt);
    accessories_on(&laptop, &laptop_bt);
    Person::connect(&[&phone, &laptop]);

    move_to(&phone, &laptop.id);
    assert_eq!(phone_bt.done(), [format!("disconnect {HEADPHONES}")]);
    assert_eq!(laptop_bt.done(), [format!("connect {HEADPHONES}")]);
    assert_eq!(
        notices(&phone),
        [
            Notice::Moving {
                accessory: "Headphones".into(),
                device: "Laptop".into(),
            },
            Notice::Moved {
                accessory: "Headphones".into(),
                device: "Laptop".into(),
            },
        ]
    );
    assert_eq!(
        notices(&laptop),
        [Notice::Taken {
            accessory: "Headphones".into(),
            device: "Phone".into(),
        }]
    );
    let moves: Vec<Value> = phone.read(accessories::PROVIDER, "moves");
    assert!(moves.is_empty(), "the move is over on both sides");
}

#[test]
fn an_accessory_the_destination_has_no_bond_with_stays() {
    let person = Person::new();
    let phone = person.device(2, "Phone", true);
    let laptop = person.device(3, "Laptop", false);
    let phone_bt = FakeBluetooth::with(&[(HEADPHONES, "Headphones")], &[]);
    let laptop_bt = FakeBluetooth::with(&[], &[]);
    accessories_on(&phone, &phone_bt);
    accessories_on(&laptop, &laptop_bt);
    Person::connect(&[&phone, &laptop]);

    move_to(&phone, &laptop.id);
    assert!(phone_bt.done().is_empty(), "it never let go");
    assert!(laptop_bt.done().is_empty());
    assert_eq!(
        notices(&phone).last(),
        Some(&Notice::Stayed {
            accessory: "Headphones".into(),
            why: Stayed::NotPairedThere {
                device: "Laptop".into(),
            },
        })
    );

    // Nor does one move to a device that is not here.
    laptop.sleep(&[&phone]);
    move_to(&phone, &laptop.id);
    assert_eq!(
        notices(&phone).last(),
        Some(&Notice::Stayed {
            accessory: HEADPHONES.into(),
            why: Stayed::TargetAway,
        })
    );
}

#[test]
fn a_move_the_destination_leaves_or_that_takes_too_long_is_given_up() {
    let person = Person::new();
    let phone = person.device(2, "Phone", true);
    let laptop = person.device(3, "Laptop", false);
    let phone_bt = FakeBluetooth::with(&[(HEADPHONES, "Headphones")], &[]);
    let laptop_bt = FakeBluetooth::with(&[], &[HEADPHONES]);
    let on_phone = accessories_on(&phone, &phone_bt);
    let on_laptop = accessories_on(&laptop, &laptop_bt);
    Person::connect(&[&phone, &laptop]);

    // The laptop is slow to check its bond, and the move runs out of time.
    *laptop_bt.hold.lock().unwrap() = true;
    move_to(&phone, &laptop.id);
    assert_eq!(on_phone.moves().len(), 1);
    assert_eq!(on_laptop.moves().len(), 1);
    on_phone.tick(Instant::now() + WAIT);
    assert!(on_phone.moves().is_empty());
    assert!(on_laptop.moves().is_empty(), "the laptop gives it up too");
    assert_eq!(
        notices(&phone).last(),
        Some(&Notice::Stayed {
            accessory: "Headphones".into(),
            why: Stayed::TimedOut,
        })
    );

    // The laptop leaves in the middle of a move.
    move_to(&phone, &laptop.id);
    laptop.sleep(&[&phone]);
    on_phone.tick(Instant::now());
    assert!(on_phone.moves().is_empty());
    assert_eq!(
        notices(&phone).last(),
        Some(&Notice::Stayed {
            accessory: "Headphones".into(),
            why: Stayed::TargetAway,
        })
    );
    assert!(phone_bt.done().is_empty(), "the phone never let go");
}

//! A device of the person that holds none of their identity's key adds a
//! device: the device holding the key asks the person, over the real
//! personal-fabric wire between two certified sessions, and signs only what
//! they approved; a revoked device is refused before anyone is asked.

use super::tests::{authenticated_personal_peer, personal_workspaces_network_for_test};
use super::{NetCmd, PersonalFabricPayload, ShellNetwork};
use crate::shell::device_certification::{self as certification, Keys};
use crate::shell::device_fabric::PersonalFabricActionEnvelope;
use crate::shell::device_pairing;
use crate::shell::personal_continuity::DeviceId;
use std::sync::{Arc, mpsc as std_mpsc};
use tilo_identity_host::certification_ceremony::{IdentityKey, pairing_code};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, verify_device_cert};
use tokio::sync::mpsc;

const SEED: [u8; 32] = [0xC1; 32];

/// The person's key as a device holds it: the whole key, or none of it.
struct Held {
    whole: bool,
    revoked: Vec<[u8; 32]>,
}

impl Keys for Held {
    fn key(&self, person: [u8; 32]) -> Result<Box<dyn IdentityKey>, String> {
        let (single, shares, secret): (bool, (u16, u16, u16), &[u8]) = if self.whole {
            (true, (1, 1, 1), &SEED)
        } else {
            (false, (1, 1, 0), &[])
        };
        let record = serde_json::json!({
            "display_name": "Fixture", "avatar_color": [1, 2, 3],
            "created_at": "2026-10-22T00:00:00Z", "schema_version": 1,
            "threshold_identity": {
                "single_device": single, "group_public_key": person, "threshold": shares.0,
                "total_shares": shares.1, "share_index": shares.2, "secret_bytes": secret,
            },
        });
        runtime_identity::load_from_bytes(&serde_json::to_vec(&record).unwrap())
            .map(|key| Box::new(key) as Box<dyn IdentityKey>)
            .map_err(|error| error.to_string())
    }

    fn revoked(&self, _: [u8; 32], device: &[u8; 32]) -> bool {
        self.revoked.contains(device)
    }
}

struct Device {
    network: ShellNetwork,
    commands: mpsc::Receiver<NetCmd>,
    key: iroh::SecretKey,
    woken: std_mpsc::Receiver<()>,
}

impl Device {
    fn id(&self) -> DeviceId {
        DeviceId::new(hex::encode(self.key.public().as_bytes())).unwrap()
    }
}

/// Two certified devices of the person `root`, each with a route to the
/// other: `courier` holds none of the key, `holder` what `held` says. Each
/// test names its own devices (`seed`), as their state is the process's.
fn pair_of_devices(root: &ed25519_dalek::SigningKey, seed: u8, held: Held) -> (Device, Device) {
    let device = |seed: u8, keys: Arc<dyn Keys>| {
        let key = iroh::SecretKey::from_bytes(&[seed; 32]);
        let (mut network, commands) = personal_workspaces_network_for_test(
            root,
            &key,
            crate::shell::state::WorkspaceTree::default(),
        );
        let id = DeviceId::new(hex::encode(key.public().as_bytes())).unwrap();
        network.personal_continuity_device = Some(id.clone());
        certification::set_keys(&id, keys);
        let (wake, woken) = std_mpsc::channel();
        let wake = std::sync::Mutex::new(wake);
        network.set_wake(Arc::new(move || {
            let _ = wake.lock().unwrap().send(());
        }));
        Device {
            network,
            commands,
            key,
            woken,
        }
    };
    let courier = device(
        seed,
        Arc::new(Held {
            whole: false,
            revoked: Vec::new(),
        }),
    );
    let holder = device(seed.wrapping_add(1), Arc::new(held));
    for (from, to) in [(&courier, &holder), (&holder, &courier)] {
        from.network
            .registered_home_device_ids
            .lock()
            .unwrap()
            .insert(*to.key.public().as_bytes());
        authenticated_personal_peer(&from.network, root, &to.key);
    }
    (courier, holder)
}

/// Carry the next certification message the device `from` sends (on
/// `commands`) to `to` over the personal-fabric wire, waiting for it.
fn carry(commands: &mut mpsc::Receiver<NetCmd>, from: &DeviceId, to: &Device) {
    loop {
        let NetCmd::PublishPersonalFabricTarget {
            target,
            payload: PersonalFabricPayload::Action(bytes),
            ..
        } = commands
            .blocking_recv()
            .expect("the sending device keeps its transport")
        else {
            continue;
        };
        assert_eq!(target, to.key.public());
        let route = to
            .network
            .current_personal_route(from)
            .expect("the receiver has a route to the sender");
        to.network.dispatch_personal_fabric_action(
            route.endpoint_id,
            *route.endpoint_id.as_bytes(),
            route,
            &bytes,
        );
        let received = to.network.take_personal_fabric_actions();
        assert_eq!(received.len(), 1, "the wire admits the certified message");
        let envelope: &PersonalFabricActionEnvelope = &received[0].envelope;
        assert!(certification::handle_action(&to.network, envelope));
        return;
    }
}

fn proposal(root: &ed25519_dalek::SigningKey, label: &str) -> DeviceCert {
    DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: ed25519_dalek::SigningKey::from_bytes(&[0xC4; 32])
            .verifying_key()
            .to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
    }
}

const SERVICE: [u8; 32] = [0xC5; 32];

#[test]
fn the_key_holder_asks_its_person_and_certifies_what_they_approved() {
    let root = ed25519_dalek::SigningKey::from_bytes(&SEED);
    let (mut courier, mut holder) = pair_of_devices(
        &root,
        0x10,
        Held {
            whole: true,
            revoked: Vec::new(),
        },
    );
    let wanted = proposal(&root, "New laptop");
    let (courier_id, holder_id) = (courier.id(), holder.id());
    std::thread::scope(|scope| {
        let network = &courier.network;
        let gathering =
            scope.spawn(|| certification::certify(network, wanted.clone(), SERVICE, 1, 1));
        carry(&mut courier.commands, &courier_id, &holder);
        holder
            .woken
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("the key holder takes the ask");
        // What its notification shows, and `tilo-identity asking` lists
        // through the same action.
        let listed = device_pairing::asking(&holder.network);
        let asking = listed["asking"].as_array().unwrap();
        assert_eq!(asking.len(), 1, "the person is asked on the key holder");
        assert_eq!(asking[0]["label"], "New laptop");
        assert_eq!(asking[0]["device"], hex::encode(wanted.device_public_key));
        assert_eq!(
            asking[0]["code"],
            pairing_code(wanted.device_public_key, SERVICE)
        );
        assert_eq!(
            device_pairing::asking(&courier.network)["asking"],
            serde_json::json!([])
        );

        let ask = asking[0]["ask"].as_str().unwrap();
        let answered = device_pairing::answer(&holder.network, ask, true).unwrap();
        assert_eq!(answered["label"], "New laptop");
        assert!(
            device_pairing::answer(&holder.network, ask, true).is_err(),
            "an ask is answered once"
        );
        carry(&mut holder.commands, &holder_id, &courier);
        let cert = gathering.join().unwrap().expect("certified");
        verify_device_cert(&cert).unwrap();
        assert_eq!(cert.user_public_key, root.verifying_key().to_bytes());
        assert_eq!(cert.device_label, "New laptop");
    });
    assert!(certification::asking(&holder.network).is_empty());
}

#[test]
fn a_revoked_device_is_refused_without_asking_anyone() {
    let root = ed25519_dalek::SigningKey::from_bytes(&SEED);
    let wanted = proposal(&root, "Lost phone");
    let (mut courier, mut holder) = pair_of_devices(
        &root,
        0x20,
        Held {
            whole: true,
            revoked: vec![wanted.device_public_key],
        },
    );
    let (courier_id, holder_id) = (courier.id(), holder.id());
    std::thread::scope(|scope| {
        let network = &courier.network;
        let gathering =
            scope.spawn(|| certification::certify(network, wanted.clone(), SERVICE, 1, 1));
        carry(&mut courier.commands, &courier_id, &holder);
        holder
            .woken
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("the key holder takes the ask");
        assert!(certification::asking(&holder.network).is_empty());
        certification::pump(&holder.network);
        carry(&mut holder.commands, &holder_id, &courier);
        let error = gathering.join().unwrap().expect_err("refused");
        assert!(error.contains("removed"), "{error}");
    });
}

#[test]
fn devices_without_the_key_say_so_and_the_person_hears_what_is_needed() {
    let root = ed25519_dalek::SigningKey::from_bytes(&SEED);
    let (mut courier, mut holder) = pair_of_devices(
        &root,
        0x30,
        Held {
            whole: false,
            revoked: Vec::new(),
        },
    );
    let wanted = proposal(&root, "Tablet");
    let (courier_id, holder_id) = (courier.id(), holder.id());
    std::thread::scope(|scope| {
        let network = &courier.network;
        let gathering =
            scope.spawn(|| certification::certify(network, wanted.clone(), SERVICE, 1, 1));
        carry(&mut courier.commands, &courier_id, &holder);
        holder
            .woken
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("the device takes the ask");
        certification::pump(&holder.network);
        carry(&mut holder.commands, &holder_id, &courier);
        let error = gathering.join().unwrap().expect_err("nobody holds the key");
        assert!(error.contains("can approve it"), "{error}");
    });
}

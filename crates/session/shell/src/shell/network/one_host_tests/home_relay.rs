//! A home relay hosts a person's workspace only when they chose it for that
//! workspace (Settings, Devices & Sync). Choosing it adds the relay as a
//! hosting member of the room: the room keeps its id and stays governed by
//! the device that made it, and the relay replicates and serves it, so the
//! person's other device syncs through the relay while that device is off.
//! A workspace nobody chose this for never reaches the relay, reachable as
//! it is, and turning the choice off takes the relay off again.

use std::path::Path;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use super::{Device, Person, Wakes, hosted_workspace, scratch, until};
use crate::shell::state::RoomId;

/// The name Ada's home relay goes by: an identifier, not copy.
const RELAY_LABEL: &str = "ada-home-relay";

/// Ada's home relay, running in this test.
pub(super) struct HomeRelay {
    pub(super) relay: tilo_relay::Relay,
    pub(super) state: std::path::PathBuf,
    pub(super) runtime: tokio::runtime::Runtime,
}

impl HomeRelay {
    /// Start `person`'s home relay with its state under `dir`, and register
    /// it with their devices as their enrolled home relays are registered.
    pub(super) fn start(person: &Person, dir: &Path, env: &mut tilo_test_env::ScopedEnv) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let key = SigningKey::from_bytes(&[0x5E; 32]);
        let cert = person.certify(
            &key,
            RELAY_LABEL,
            tilo_user_identity::cloud_enrollment::host_capabilities(),
        );
        let state = dir.join("relay");
        let relay = runtime
            // On this machine only, as the devices are: a test's relay never
            // reaches the public relays, where another run's relay of the
            // same key would take its place.
            .block_on(tilo_relay::Relay::start_with_relay_mode(
                tilo_relay::RelayConfig {
                    state_root: state.clone(),
                    ..tilo_relay::RelayConfig::default()
                },
                tilo_relay::daemon::RelayIdentity {
                    user_public_key: person.user(),
                    device_signing_key: key,
                    device_cert: cert.clone(),
                    revocations: RevocationList::new(),
                },
                iroh::RelayMode::Disabled,
            ))
            .unwrap();
        let addr = relay.node_addr();
        let registry = dir.join("home-peers.json");
        let registration = tilo_relay_protocol::HomePeerRegistration {
            schema_version: tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
            device_node_id: hex::encode(cert.device_public_key),
            relay_service_node_id: hex::encode(addr.id.as_bytes()),
            relay_endpoint: Some(addr),
            label: RELAY_LABEL.into(),
            user_public_key: hex::encode(person.user()),
            device_cert: Some(cert),
            relay_workspace_ids: vec![super::super::personal_continuity_topic_id(&person.user())],
            environments: tilo_relay_protocol::RelayEnvironments::default(),
        };
        std::fs::write(&registry, serde_json::to_vec(&vec![registration]).unwrap()).unwrap();
        env.set("TILO_HOME_PEERS_PATH", &registry);
        Self {
            relay,
            state,
            runtime,
        }
    }

    /// The bodies the relay retains for `room`; `None` while it does not
    /// host the room.
    pub(super) fn retained(&self, room: RoomId) -> Option<usize> {
        let workspace = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
        self.runtime
            .block_on(self.relay.room_records().retained(workspace))
            .ok()
            .map(|bodies| bodies.len())
    }

    /// Whether any file the relay keeps contains `needle`.
    pub(super) fn keeps(&self, needle: &[u8]) -> bool {
        holds(&self.state, needle)
    }
}

/// The home relay `device`'s Devices & Sync names, and each workspace it
/// hosts with whether the relay hosts it too.
fn on_relay(device: &Device) -> (Option<String>, Vec<(RoomId, bool)>) {
    let hosting = device
        .net()
        .home_relay_hosting()
        .unwrap_or_else(|error| panic!("the home relay hosting is read: {error}"));
    (
        hosting.relay,
        hosting
            .workspaces
            .into_iter()
            .map(|workspace| (RoomId::parse(&workspace.room).unwrap(), workspace.on_relay))
            .collect(),
    )
}

pub(super) fn holds(dir: &Path, needle: &[u8]) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let path = entry.path();
            if path.is_dir() {
                holds(&path, needle)
            } else {
                std::fs::read(&path)
                    .unwrap_or_default()
                    .windows(needle.len())
                    .any(|window| window == needle)
            }
        })
}

#[test]
#[serial_test::serial]
#[allow(
    clippy::too_many_lines,
    reason = "one journey: the choice, both devices' absences and the choice undone"
)]
fn a_home_relay_hosts_only_the_workspace_chosen_for_it_and_carries_it_while_its_device_is_off() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(61);
    home.env().set("TILO_USER_ID", ada.id());
    let mut env = tilo_test_env::ScopedEnv::new();
    let relay = HomeRelay::start(&ada, dir.path(), &mut env);
    let mut first = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A31,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let mut second = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("laptop"),
        0x0A32,
        DeviceCapabilities::member(),
        &wakes,
    );

    // Two workspaces on the desktop, both on its own host, the relay
    // reachable all along.
    let (_, personal) = hosted_workspace(&first, &[&second], "Personal");
    let (_, scratchpad) = hosted_workspace(&first, &[&second], "Scratch");
    second.reach(&first);
    until(
        &[&first, &second],
        "the laptop to hold both workspaces",
        || (second.holds(personal) && second.holds(scratchpad)).then_some(()),
    );
    for room in [personal, scratchpad] {
        first.open_notes_first(room);
    }
    first.write_note(scratchpad, "scratch only on Ada's own devices");
    until(&[&first, &second], "the scratch note on the laptop", || {
        (second.note(scratchpad).as_deref() == Some("scratch only on Ada's own devices"))
            .then_some(())
    });
    assert_eq!(
        relay.retained(personal),
        None,
        "nothing reaches the relay before Ada chooses it"
    );

    // Devices & Sync shows each workspace the desktop hosts, none of them
    // on the relay yet.
    assert_eq!(
        on_relay(&first),
        (
            Some(RELAY_LABEL.to_owned()),
            vec![(personal, false), (scratchpad, false)]
        )
    );

    // Ada chooses her home relay for the personal workspace.
    first
        .net()
        .set_home_relay_hosting(personal, true)
        .unwrap_or_else(|error| panic!("the relay is added as a host: {error}"));
    assert_eq!(
        on_relay(&first).1,
        [(personal, true), (scratchpad, false)],
        "the personal workspace's hosts name the relay"
    );
    first.write_note(personal, "kept on the relay too");
    until(
        &[&first, &second],
        "the relay to host the personal workspace",
        || {
            (relay.retained(personal).is_some_and(|bodies| bodies > 0)
                && second.note(personal).as_deref() == Some("kept on the relay too"))
            .then_some(())
        },
    );
    assert!(first.workspace_rooms().contains(&personal), "same room");
    assert!(second.workspace_rooms().contains(&personal), "same room");
    let hosted: std::collections::BTreeSet<_> = first.hosted_rooms().into_iter().collect();
    assert_eq!(
        hosted,
        [personal, scratchpad].into_iter().collect(),
        "the desktop still hosts and governs the personal workspace"
    );

    // The desktop goes off; the laptop keeps syncing with the relay.
    first.go_offline();
    let before = relay.retained(personal).unwrap();
    second.write_note(personal, "written on the laptop while the desktop was off");
    until(&[&second], "the relay to take the laptop's edit", || {
        (relay.retained(personal).unwrap_or(0) > before).then_some(())
    });

    // With the laptop off too, the desktop catches up from the relay.
    second.go_offline();
    first.come_back();
    until(&[&first], "the desktop to catch up from the relay", || {
        (first.note(personal).as_deref() == Some("written on the laptop while the desktop was off"))
            .then_some(())
    });
    second.come_back();
    until(&[&first, &second], "the laptop to be back in step", || {
        (second.holds(personal)
            && second.note(personal).as_deref()
                == Some("written on the laptop while the desktop was off"))
        .then_some(())
    });

    // The workspace nobody chose this for never reached the relay.
    assert_eq!(relay.retained(scratchpad), None);
    assert!(
        !relay.keeps(b"scratch only on Ada's own devices"),
        "no file of the relay holds the scratch note"
    );

    // Turning the choice off takes the relay off as a host.
    first
        .net()
        .set_home_relay_hosting(personal, false)
        .unwrap_or_else(|error| panic!("the relay is removed as a host: {error}"));
    assert_eq!(
        on_relay(&first).1,
        [(personal, false), (scratchpad, false)],
        "its hosts no longer name the relay"
    );
    until(&[&first, &second], "the relay to stop hosting", || {
        relay.retained(personal).is_none().then_some(())
    });
    assert!(first.workspace_rooms().contains(&personal));
    assert!(first.holds(personal) && second.holds(personal));
    drop((first, second, relay, env));
}

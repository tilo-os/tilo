use super::*;

use super::device::{DHCP, STATIC};

const WORKSPACE: &str = "0b7e7dfa-3ad5-4b43-9d6c-5f0d7c1f6a11";

fn device() -> Id {
    device_id(&[7; 32])
}

/// The address a server enrols: its own endpoint, with no address of its own
/// yet, as `node_addr` spells one.
fn endpoint() -> String {
    let key = iroh_base::SecretKey::from_bytes(&[7; 32]);
    serde_json::to_string(&iroh_base::EndpointAddr::from_parts(
        key.public(),
        std::iter::empty(),
    ))
    .unwrap()
}

fn enrolled() -> Replica<Servers> {
    let mut record = Replica::<Servers>::new();
    record
        .apply(Servers::ENROLL_SERVER.with((
            device().as_str().to_owned(),
            "Attic".into(),
            endpoint(),
        )))
        .unwrap();
    record
}

/// The record answers a read with what each server is set to, so an agent or
/// a script with no window open reads a person's servers as any record.
#[test]
fn the_record_answers_a_read_with_what_each_server_is_set_to() {
    use tilo_resource::Resource as _;
    const { assert!(<Servers as tilo_resource::Resource>::READABLE) };
    let mut record = enrolled();
    record
        .apply(Servers::SET_UPDATES.with((device(), "stable".into())))
        .unwrap();
    let read = Servers::read_content(&record.state().unwrap(), record.doc(), None).unwrap();
    let said = read.to_string();
    assert!(read.is_object(), "{said}");
    assert!(said.contains("Attic"), "the server's name is read: {said}");
    assert!(said.contains("stable"), "and what it is set to: {said}");
}

#[test]
fn a_servers_configuration_is_typed_actions_with_history() {
    let mut record = enrolled();
    record
        .apply(Servers::HOLD_WORKSPACE.with((device(), WORKSPACE.into(), KEY_HOLDER.into())))
        .unwrap();
    record
        .apply(Servers::SET_APPETITE.with((device(), 500, true, 2)))
        .unwrap();
    record
        .apply(Servers::SET_UPDATES.with((device(), "stable".into())))
        .unwrap();
    record
        .apply(Servers::SET_MAINTENANCE_WINDOW.with((device(), 180, 60)))
        .unwrap();
    record
        .apply(Servers::SET_UNLOCK_POLICY.with((device(), "tpm".into())))
        .unwrap();
    let state = record.state().unwrap();
    let server = state.active(&[7; 32]).unwrap();
    assert_eq!(server.label, "Attic");
    assert_eq!(server.holding(WORKSPACE), Some(KEY_HOLDER));
    assert_eq!(
        (server.storage_gb, server.compute, server.machines),
        (500, true, 2)
    );
    assert_eq!(server.updates, "stable");
    assert_eq!(
        (server.maintenance_start, server.maintenance_minutes),
        (180, 60)
    );
    assert_eq!(server.unlock, "tpm");
    assert_eq!(record.log().len(), 6, "each change is one entry of history");

    record
        .apply(Servers::HOLD_WORKSPACE.with((device(), WORKSPACE.into(), STORE.into())))
        .unwrap();
    assert_eq!(
        record
            .state()
            .unwrap()
            .active(&[7; 32])
            .unwrap()
            .holding(WORKSPACE),
        Some(STORE)
    );
    record
        .apply(Servers::RELEASE_WORKSPACE.with((device(), WORKSPACE.into())))
        .unwrap();
    assert_eq!(
        record
            .state()
            .unwrap()
            .active(&[7; 32])
            .unwrap()
            .holding(WORKSPACE),
        None
    );
}

#[test]
fn a_change_the_record_cannot_mean_is_refused() {
    let mut record = enrolled();
    for refused in [
        Servers::HOLD_WORKSPACE.with((device(), "not-a-workspace".into(), STORE.into())),
        Servers::HOLD_WORKSPACE.with((device(), WORKSPACE.into(), "owner".into())),
        Servers::SET_UPDATES.with((device(), "nightly-ish".into())),
        Servers::SET_MAINTENANCE_WINDOW.with((device(), 24 * 60, 10)),
        Servers::SET_UNLOCK_POLICY.with((device(), "none".into())),
        Servers::RENAME_SERVER.with((device(), " ".into())),
        Servers::RENAME_SERVER.with((device_id(&[8; 32]), "Other".into())),
        Servers::ENROLL_SERVER.with(("attic".into(), "Attic".into(), endpoint())),
        Servers::ENROLL_SERVER.with((
            device().as_str().to_owned(),
            "Attic".into(),
            "not-an-address".into(),
        )),
        Servers::SET_HOSTNAME.with((device(), "-nas".into())),
        Servers::USE_DISK.with((device(), "ROOT1".into(), "spare".into(), String::new())),
        Servers::USE_DISK.with((device(), " ".into(), "data".into(), String::new())),
        Servers::USE_DISK.with((device(), "DATA1".into(), "data".into(), "OTHER".into())),
        Servers::SET_INTERFACE.with((
            device(),
            "enp2".into(),
            "static".into(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        )),
        Servers::SET_INTERFACE.with((
            device(),
            "enp2".into(),
            "dhcp".into(),
            "10.0.0.5/24".into(),
            String::new(),
            String::new(),
            String::new(),
        )),
        Servers::SET_INTERFACE.with((
            device(),
            "enp2".into(),
            "dhcp".into(),
            String::new(),
            String::new(),
            String::new(),
            "bond0".into(),
        )),
        Servers::REPORT_DEVICE_CONFIG.with((
            device(),
            "revision".into(),
            "applied".into(),
            String::new(),
        )),
    ] {
        assert!(record.apply(refused).is_err());
    }
    assert_eq!(record.log().len(), 1, "nothing refused is history");
}

/// The machine an entry configures: three drives, a bond of two interfaces, a
/// tagged interface with a static address, and a hostname.
fn configured() -> Replica<Servers> {
    let mut servers = enrolled();
    for (identity, role) in [
        ("ROOT1", "system"),
        ("DATA1", "data"),
        ("MIRROR1", "mirror"),
    ] {
        servers
            .apply(Servers::USE_DISK.with((device(), identity.into(), role.into(), String::new())))
            .unwrap();
    }
    servers
        .apply(Servers::SET_INTERFACE.with((
            device(),
            "enp2".into(),
            STATIC.into(),
            "10.0.0.5/24".into(),
            "10.0.0.1".into(),
            "1.1.1.1,9.9.9.9".into(),
            String::new(),
        )))
        .unwrap();
    servers
        .apply(Servers::SET_INTERFACE.with((
            device(),
            "enp3.20".into(),
            DHCP.into(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        )))
        .unwrap();
    for (name, bond) in [("bond0", ""), ("eno1", "bond0")] {
        servers
            .apply(Servers::SET_INTERFACE.with((
                device(),
                name.into(),
                DHCP.into(),
                String::new(),
                String::new(),
                String::new(),
                bond.into(),
            )))
            .unwrap();
    }
    servers
        .apply(Servers::SET_HOSTNAME.with((device(), "nas".into())))
        .unwrap();
    servers
}

#[test]
fn a_machines_own_configuration_is_typed_actions_with_history() {
    let mut servers = configured();
    let status = Servers::REPORT_DEVICE_CONFIG.with((
        device(),
        "0123456789abcdef0123456789abcdef".into(),
        "applied".into(),
        "hostname, network".into(),
    ));
    servers.apply(status).unwrap();
    let server = servers.state().unwrap().active(&[7; 32]).cloned().unwrap();
    assert_eq!(server.device.hostname, "nas");
    let roles = server
        .device
        .disks
        .iter()
        .map(|(id, disk)| (id.as_str().to_owned(), disk.role.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        roles,
        [
            ("ROOT1".to_owned(), "system".to_owned()),
            ("DATA1".to_owned(), "data".to_owned()),
            ("MIRROR1".to_owned(), "mirror".to_owned()),
        ]
    );
    let enp2 = server
        .device
        .network
        .get(&Id::from_raw("enp2"))
        .cloned()
        .unwrap();
    assert_eq!(
        (enp2.addressing.as_str(), enp2.addresses.as_str()),
        (STATIC, "10.0.0.5/24")
    );
    assert_eq!(
        server
            .device
            .network
            .get(&Id::from_raw("eno1"))
            .map(|eno1| eno1.bond.clone()),
        Some("bond0".to_owned())
    );
    assert_eq!(
        server
            .device
            .network
            .get(&Id::from_raw("enp3.20"))
            .map(|i| i.vlan),
        Some(20)
    );
    assert_eq!(server.device.status.state, "applied");
    assert_eq!(
        server.device.status.revision,
        "0123456789abcdef0123456789abcdef"
    );
}

/// A drive another role would erase takes the drive's name to confirm, and
/// what a machine needs stays: the disk it boots from, and an interface others
/// are bonded to.
#[test]
fn a_machine_keeps_what_it_boots_from_and_a_drive_is_erased_only_when_named() {
    let mut servers = configured();

    // Another role erases the drive, so it takes the drive's name to confirm.
    assert!(
        servers
            .apply(Servers::USE_DISK.with((
                device(),
                "DATA1".into(),
                "mirror".into(),
                String::new(),
            )))
            .is_err()
    );
    servers
        .apply(Servers::USE_DISK.with((device(), "DATA1".into(), "mirror".into(), "DATA1".into())))
        .unwrap();
    // The disk the system boots from stays, and one that others are bonded to
    // stays until they let go.
    assert!(
        servers
            .apply(Servers::RELEASE_DISK.with((device(), "ROOT1".into())))
            .is_err()
    );
    assert!(
        servers
            .apply(Servers::RELEASE_INTERFACE.with((device(), "bond0".into())))
            .is_err()
    );
    servers
        .apply(Servers::RELEASE_INTERFACE.with((device(), "eno1".into())))
        .unwrap();
    servers
        .apply(Servers::RELEASE_INTERFACE.with((device(), "bond0".into())))
        .unwrap();
    servers
        .apply(Servers::RELEASE_DISK.with((device(), "MIRROR1".into())))
        .unwrap();
    let server = servers.state().unwrap().active(&[7; 32]).cloned().unwrap();
    assert!(server.device.network.get(&Id::from_raw("bond0")).is_none());
    assert!(server.device.disks.get(&Id::from_raw("MIRROR1")).is_none());
    assert_eq!(
        server
            .device
            .disks
            .get(&Id::from_raw("DATA1"))
            .map(|d| d.role.clone()),
        Some("mirror".to_owned())
    );
}

#[test]
fn a_retired_server_holds_nothing_and_is_not_taken_in_again() {
    let mut record = enrolled();
    record
        .apply(Servers::HOLD_WORKSPACE.with((device(), WORKSPACE.into(), STORE.into())))
        .unwrap();
    record.apply(Servers::RETIRE_SERVER.with(device())).unwrap();
    assert!(record.state().unwrap().active(&[7; 32]).is_none());
    record
        .apply(Servers::ENROLL_SERVER.with((
            device().as_str().to_owned(),
            "Attic again".into(),
            endpoint(),
        )))
        .unwrap();
    assert!(record.state().unwrap().active(&[7; 32]).is_none());
    assert!(
        record
            .apply(Servers::HOLD_WORKSPACE.with((device(), WORKSPACE.into(), STORE.into())))
            .is_err()
    );
}

#[test]
fn each_server_of_a_person_finds_the_same_admin_workspace() {
    let ana = admin_workspace_id(&[1; 32]);
    assert_eq!(ana, admin_workspace_id(&[1; 32]));
    assert_ne!(ana, admin_workspace_id(&[2; 32]));
    assert!(uuid::Uuid::parse_str(&ana).is_ok());
}

/// A server keeps the address it enrolled, and enrolling again moves it; the
/// first name it was given stays.
#[test]
fn a_server_keeps_the_address_it_enrolled() {
    let mut servers = enrolled();
    let first = endpoint();
    assert_eq!(
        servers
            .state()
            .unwrap()
            .servers
            .get(&device())
            .unwrap()
            .endpoint,
        first
    );
    let moved = serde_json::to_string(&iroh_base::EndpointAddr::from_parts(
        iroh_base::SecretKey::from_bytes(&[9; 32]).public(),
        std::iter::empty(),
    ))
    .unwrap();
    assert_ne!(moved, first);
    servers
        .apply(Servers::ENROLL_SERVER.with((
            device().as_str().to_owned(),
            "Attic".into(),
            moved.clone(),
        )))
        .unwrap();
    let server = servers
        .state()
        .unwrap()
        .servers
        .get(&device())
        .cloned()
        .unwrap();
    assert_eq!(server.endpoint, moved);
    assert_eq!(server.label, "Attic");
}

const PLAN: &str = "0123456789abcdef0123456789abcdef";
const NONCE: &str = "00112233445566778899aabbccddeeff";

/// A machine planned to install to `disk`, leaving exactly `open` open, as the
/// plan document both sides read.
fn planned(disk: &str, open: &str) -> Replica<Servers> {
    use install_profile_model::plan::{DiskRule, Network, Plan, SCHEMA, Token};
    let open: Vec<String> = open
        .split(',')
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    let left_open = |name: &str| open.iter().any(|choice| choice == name);
    let deliver = |name: &str, made: &str| {
        if left_open(name) {
            String::new()
        } else {
            made.to_owned()
        }
    };
    let plan = Plan {
        schema: SCHEMA,
        plan: PLAN.to_owned(),
        revision: 1,
        destination: deliver("destination", "tilo-server"),
        name: deliver("name", "cupboard"),
        network: if left_open("network") {
            Network::default()
        } else {
            Network {
                addressing: "dhcp".into(),
                ..Network::default()
            }
        },
        disk: if left_open("disk") {
            DiskRule::default()
        } else {
            DiskRule::decode(disk).unwrap()
        },
        unlock: deliver("unlock", "tpm"),
        updates: deliver("updates", "stable"),
        hosting: Vec::new(),
        open: open.clone(),
        token: Token {
            nonce: NONCE.to_owned(),
            expires_at_ns: 1_700_000_000_000_000_000,
        },
    };
    let plan: Plan = serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
    let mut servers = Replica::<Servers>::new();
    servers
        .apply(
            Servers::PLAN_MACHINE
                .with((serde_json::to_string(&plan).unwrap(), "cupboard".to_owned())),
        )
        .unwrap();
    servers
}

/// Two drives the machine reported: a rule that picks neither has to ask.
fn two_drives() -> String {
    serde_json::json!([
        {"model": "QEMU HARDDISK", "identity": "serial-a", "bytes": 40_000_000_000_u64,
         "holds_data": false, "boots": false},
        {"model": "QEMU HARDDISK", "identity": "serial-b", "bytes": 40_000_000_000_u64,
         "holds_data": true, "boots": true},
    ])
    .to_string()
}

/// The machine's drive rule matched two drives, so it enrolled and asked; the
/// person's device answers with one of the drives the machine reported, never
/// one it did not, and only then may the installation finish.
#[test]
fn a_machine_asks_the_drive_it_could_not_settle_and_takes_the_answer() {
    let mut servers = planned("empty", "");
    let machine = device_id(&[9; 32]);
    let ask = serde_json::json!({
        "disk": "empty matched 2 drives on this machine, not a drive without data"
    })
    .to_string();
    servers
        .apply(Servers::BEGIN_INSTALL.with((
            machine.as_str().to_owned(),
            NONCE.to_owned(),
            two_drives(),
            ask,
        )))
        .unwrap();
    let entry = servers
        .state()
        .unwrap()
        .servers
        .get(&machine)
        .cloned()
        .unwrap();
    assert_eq!(entry.plan.status, plan::INSTALLING);
    assert_eq!(
        entry.plan.drives.len(),
        2,
        "the drives it reported are kept"
    );
    assert!(
        entry
            .plan
            .question("disk")
            .is_some_and(|asked| asked.reason.contains("2 drives")),
        "the question carries the reason: {entry:?}"
    );

    assert!(
        servers
            .apply(Servers::FINISH_INSTALL.with(machine.clone()))
            .is_err(),
        "a machine still waiting for an answer did not finish"
    );
    assert!(
        servers
            .apply(Servers::ANSWER_MACHINE_CHOICE.with((
                machine.clone(),
                "disk".into(),
                "serial:serial-z".into(),
            )))
            .is_err(),
        "an answer names a drive the machine reported"
    );
    assert!(
        servers
            .apply(Servers::ANSWER_MACHINE_CHOICE.with((
                machine.clone(),
                "disk".into(),
                "empty".into(),
            )))
            .is_err(),
        "an answered drive names its serial or WWN, not another rule"
    );

    servers
        .apply(Servers::ANSWER_MACHINE_CHOICE.with((
            machine.clone(),
            "disk".into(),
            "serial:serial-a".into(),
        )))
        .unwrap();
    let entry = servers
        .state()
        .unwrap()
        .servers
        .get(&machine)
        .cloned()
        .unwrap();
    assert_eq!(entry.plan.disk, "serial:serial-a");
    assert!(entry.plan.ask.is_empty(), "the question is answered");
    servers
        .apply(Servers::FINISH_INSTALL.with(machine.clone()))
        .unwrap();
    let entry = servers
        .state()
        .unwrap()
        .servers
        .get(&machine)
        .cloned()
        .unwrap();
    assert_eq!(entry.plan.status, plan::INSTALLED);
}

/// A plan's other choices are answered the same way; the machine's network and
/// the workspaces it holds are set on its entry, not guessed from an answer.
#[test]
fn the_other_open_choices_are_answered_on_the_persons_devices() {
    let mut servers = planned("largest", "name,unlock,updates");
    let machine = device_id(&[9; 32]);
    servers
        .apply(Servers::BEGIN_INSTALL.with((
            machine.as_str().to_owned(),
            NONCE.to_owned(),
            two_drives(),
            String::new(),
        )))
        .unwrap();
    for (choice, value) in [
        ("name", "attic"),
        ("unlock", "owner-device"),
        ("updates", "dev"),
    ] {
        servers
            .apply(Servers::ANSWER_MACHINE_CHOICE.with((
                machine.clone(),
                choice.into(),
                value.into(),
            )))
            .unwrap();
    }
    let entry = servers
        .state()
        .unwrap()
        .servers
        .get(&machine)
        .cloned()
        .unwrap();
    assert_eq!(entry.label, "attic");
    assert_eq!(entry.plan.unlock, "owner-device");
    assert_eq!(entry.plan.updates, "dev");
    assert!(entry.plan.open_choices().is_empty(), "{entry:?}");

    for (choice, value) in [
        ("destination", "tilo-desktop"),
        ("unlock", "pin"),
        ("updates", "nightly"),
        ("network", "dhcp"),
        ("hosting", "store"),
        ("purpose", "server"),
    ] {
        assert!(
            servers
                .apply(Servers::ANSWER_MACHINE_CHOICE.with((
                    machine.clone(),
                    choice.into(),
                    value.into(),
                )))
                .is_err(),
            "answering {choice} with {value:?} is refused"
        );
    }
}

/// A machine whose token no plan answers is not taken in.
#[test]
fn a_machine_with_an_unknown_token_is_refused() {
    let mut servers = planned("empty", "");
    assert!(
        servers
            .apply(Servers::BEGIN_INSTALL.with((
                device_id(&[9; 32]).as_str().to_owned(),
                "ffffffffffffffffffffffffffffffff".into(),
                two_drives(),
                String::new(),
            )))
            .is_err()
    );
}

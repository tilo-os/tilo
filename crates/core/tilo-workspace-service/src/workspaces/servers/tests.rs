mod keeping;

use super::*;
use tilo_resource::Replica;

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
    let mut replica = Replica::<Servers>::new();
    replica
        .apply(Servers::ENROLL_SERVER.with((
            device().as_str().to_owned(),
            "Attic".into(),
            endpoint(),
        )))
        .unwrap();
    replica
}

/// The replica answers a read with what each server is set to, so an agent or
/// a script with no window open reads a person's servers as any replica.
#[test]
fn the_document_answers_a_read_with_what_each_server_is_set_to() {
    use tilo_resource::Resource as _;
    const { assert!(<Servers as tilo_resource::Resource>::READABLE) };
    let mut replica = enrolled();
    replica
        .apply(Servers::SET_UPDATES.with((device(), "stable".into())))
        .unwrap();
    let read = Servers::read_content(&replica.state().unwrap(), replica.doc(), None).unwrap();
    let said = read.to_string();
    assert!(read.is_object(), "{said}");
    assert!(said.contains("Attic"), "the server's name is read: {said}");
    assert!(said.contains("stable"), "and what it is set to: {said}");
}

#[test]
fn a_servers_configuration_is_typed_actions_with_history() {
    let mut replica = enrolled();
    replica
        .apply(Servers::SET_APPETITE.with((device(), 500, true, 2)))
        .unwrap();
    replica
        .apply(Servers::SET_UPDATES.with((device(), "stable".into())))
        .unwrap();
    replica
        .apply(Servers::SET_MAINTENANCE_WINDOW.with((device(), 180, 60)))
        .unwrap();
    replica
        .apply(Servers::SET_UNLOCK_POLICY.with((device(), "tpm".into())))
        .unwrap();
    let state = replica.state().unwrap();
    let server = state.active(&[7; 32]).unwrap();
    assert_eq!(server.label, "Attic");
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
    assert_eq!(
        replica.log().len(),
        5,
        "each change is one entry of history"
    );
}

/// A server's entry says where its packet relay is reached and whether it serves its own
/// web UI: empty for the default, on, and `off` once its person turns it off.
#[test]
fn an_entry_names_its_relay_and_whether_it_serves_web() {
    let mut replica = enrolled();
    // A URL keeps the form its parser gives it: a bare host ends in its root path.
    let relay = "http://10.0.0.5:9782/";
    replica
        .apply(Servers::SET_RELAY_URL.with((device(), relay.into())))
        .unwrap();
    replica
        .apply(Servers::SET_WEB.with((device(), WEB_SERVED[1].into())))
        .unwrap();
    let state = replica.state().unwrap();
    let server = state.active(&[7; 32]).unwrap();
    assert_eq!(server.relay, relay);
    assert_eq!(server.web, WEB_SERVED[1]);
    drop(state);
    replica
        .apply(Servers::SET_WEB.with((device(), WEB_SERVED[0].into())))
        .unwrap();
    let state = replica.state().unwrap();
    assert_eq!(
        state.active(&[7; 32]).unwrap().web,
        "",
        "serving again is the default, which the entry does not spell out"
    );
    assert!(
        replica
            .apply(Servers::SET_RELAY_URL.with((device(), "not a url".into())))
            .is_err(),
        "a relay is a URL"
    );
}

/// Every device founds the document with the same change, so two of the
/// person's devices that each write a server into it before they see one
/// another write one document holding both, not two competing roots.
#[test]
fn two_devices_that_each_write_the_document_first_merge_into_one() {
    let mut left = Replica::<Servers>::from_doc(genesis(WORKSPACE).unwrap());
    let mut right = Replica::<Servers>::from_doc(genesis(WORKSPACE).unwrap());
    assert_eq!(
        left.doc().clone().get_heads(),
        right.doc().clone().get_heads(),
        "the genesis is one change, identical on every device"
    );

    // Each device writes under its own actor, as the document's author does
    // when the workspace hands it a copy.
    left.doc_mut()
        .set_actor(automerge::ActorId::from([1; 32].to_vec()));
    left.apply(Servers::ENROLL_SERVER.with((
        device_id(&[7; 32]).as_str().to_owned(),
        "Attic".into(),
        endpoint(),
    )))
    .unwrap();
    right
        .doc_mut()
        .set_actor(automerge::ActorId::from([2; 32].to_vec()));
    right
        .apply(Servers::ENROLL_SERVER.with((
            device_id(&[9; 32]).as_str().to_owned(),
            "Cellar".into(),
            endpoint(),
        )))
        .unwrap();

    // What the workspace hands one device's copy once the two meet: the
    // other's whole document, merged in.
    let mut merged = Replica::<Servers>::load(&left.save()).unwrap();
    tilo_automerge::ApplyIncoming::load_incoming(merged.doc_mut(), &right.save())
        .expect("the other device's copy merges");
    let state = merged.state().unwrap();
    assert_eq!(state.servers.len(), 2, "both servers are in one document");
    assert_eq!(state.active(&[7; 32]).unwrap().label, "Attic");
    assert_eq!(state.active(&[9; 32]).unwrap().label, "Cellar");
}

/// The genesis is derived from the document's identity alone: another workspace
/// has another genesis, and no plan or server is in it.
#[test]
fn the_genesis_is_empty_and_named_by_its_workspace() {
    let mut one = genesis(WORKSPACE).unwrap();
    let mut same = genesis(WORKSPACE).unwrap();
    assert_eq!(one.get_heads(), same.get_heads());
    let mut other = genesis("0b7e7dfa-3ad5-4b43-9d6c-5f0d7c1f6a22").unwrap();
    assert_ne!(one.get_heads(), other.get_heads());
    let replica = Replica::<Servers>::load(&one.save()).unwrap();
    assert!(replica.state().unwrap().servers.is_empty());
}

#[test]
fn a_change_the_document_cannot_mean_is_refused() {
    let mut replica = enrolled();
    for refused in [
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
        assert!(replica.apply(refused).is_err());
    }
    assert_eq!(replica.log().len(), 1, "nothing refused is history");
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

/// A link addressed in both families keeps a default route for each at once:
/// the IPv4 gateway and the IPv6 one in the interface's one setting.
#[test]
fn a_dual_stack_interface_keeps_a_gateway_for_each_family() {
    let mut servers = enrolled();
    servers
        .apply(Servers::SET_INTERFACE.with((
            device(),
            "enp3s0".into(),
            STATIC.into(),
            "65.108.204.148/26,2a01:4f9:1a:a083::2/64".into(),
            "65.108.204.129,fe80::1".into(),
            "185.12.64.1,185.12.64.2".into(),
            String::new(),
        )))
        .unwrap();
    let server = servers.state().unwrap().active(&[7; 32]).cloned().unwrap();
    let interface = server
        .device
        .network
        .get(&Id::from_raw("enp3s0"))
        .cloned()
        .expect("the interface is set up");
    assert_eq!(
        interface.addresses,
        "65.108.204.148/26,2a01:4f9:1a:a083::2/64"
    );
    assert_eq!(interface.gateway, "65.108.204.129,fe80::1");
    // A second gateway of one family is refused whole.
    assert!(
        servers
            .apply(Servers::SET_INTERFACE.with((
                device(),
                "enp3s0".into(),
                STATIC.into(),
                "65.108.204.148/26,2a01:4f9:1a:a083::2/64".into(),
                "65.108.204.129,65.108.204.130".into(),
                "185.12.64.1".into(),
                String::new(),
            )))
            .is_err()
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

/// The revocation of the device key `device` a root signs, as JSON.
fn revocation_of(device: [u8; 32]) -> String {
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: device,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    serde_json::to_string(&entry).unwrap()
}

#[test]
fn a_retired_server_holds_nothing_and_is_not_taken_in_again() {
    let mut replica = enrolled();
    assert!(
        replica
            .apply(Servers::RETIRE_SERVER.with(device()))
            .is_err(),
        "a server is retired only once it was asked with its revocation"
    );
    for wrong in [String::new(), revocation_of([8; 32])] {
        assert!(
            replica
                .apply(Servers::ASK_RETIREMENT.with((device(), wrong)))
                .is_err(),
            "a server is asked to retire only with its own key's revocation"
        );
    }
    replica
        .apply(Servers::ASK_RETIREMENT.with((device(), revocation_of([7; 32]))))
        .unwrap();
    replica
        .apply(Servers::RETIRE_SERVER.with(device()))
        .unwrap();
    assert!(replica.state().unwrap().active(&[7; 32]).is_none());
    replica
        .apply(Servers::ENROLL_SERVER.with((
            device().as_str().to_owned(),
            "Attic again".into(),
            endpoint(),
        )))
        .unwrap();
    assert!(replica.state().unwrap().active(&[7; 32]).is_none());
    assert!(
        replica
            .apply(Servers::SET_APPETITE.with((device(), 500, true, 2)))
            .is_err()
    );
}

#[test]
fn each_server_of_a_person_finds_the_same_admin_workspace() {
    let ana = person_workspace_id(&[1; 32]);
    assert_eq!(ana, person_workspace_id(&[1; 32]));
    assert_ne!(ana, person_workspace_id(&[2; 32]));
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
    // The person's devices read the waiting entry as they read any entry, and
    // see the machine's real drives and the question it asked there.
    {
        use tilo_resource::Resource as _;
        let read = Servers::read_content(&servers.state().unwrap(), servers.doc(), None).unwrap();
        let said = read.to_string();
        assert!(said.contains("serial-a"), "the drives are read: {said}");
        assert!(said.contains("2 drives"), "and the question: {said}");
    }

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

/// A plan's other choices are answered the same way; the machine's network is
/// set on its entry and the workspaces it holds in each workspace, not guessed
/// from an answer.
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

/// The machine's own drives, as it reads them off its hardware: two empty
/// ones, so the plan's `empty` rule settles neither.
fn empty_drives() -> Vec<install_profile_model::plan::Drive> {
    vec![
        install_profile_model::plan::Drive {
            model: "QEMU HARDDISK".into(),
            identity: "serial-a".into(),
            bytes: 40_000_000_000,
            holds_data: false,
            boots: false,
        },
        install_profile_model::plan::Drive {
            model: "QEMU HARDDISK".into(),
            identity: "serial-b".into(),
            bytes: 40_000_000_000,
            holds_data: false,
            boots: false,
        },
    ]
}

/// A machine that enrolled with a plan's token is one of the person's
/// devices, and reads its plan from its waiting entry: it reports the drives
/// it really has, asks what it cannot settle, waits while another of the
/// person's devices answers in the entry, and then finishes.
#[test]
fn a_machine_reads_its_plan_from_its_entry_reports_its_drives_and_finishes() {
    let mut servers = planned("empty", "");
    let drives = empty_drives();
    let machine = device_id(&[9; 32]);

    // It reads its plan from the waiting entry, as the person's devices keep
    // it, before it has a device key of its own.
    let planned_entry = servers
        .state()
        .unwrap()
        .planned_by_nonce(NONCE)
        .map(|(_, server)| server.clone())
        .expect("the token names the planned machine");
    let read = planned_entry.install_plan().unwrap();
    assert_eq!(read.plan, PLAN);
    assert_eq!(read.name, "cupboard");
    assert_eq!(read.destination, "tilo-server");
    assert_eq!(read.disk.encode(), "empty");

    // It reports its two empty drives, and asks which one, because the plan's
    // rule matches neither.
    let machine::Step::Enlist { nonce, report } = machine::step(&planned_entry, &drives).unwrap()
    else {
        panic!("a planned machine enlists");
    };
    assert_eq!(nonce, NONCE);
    let asked: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&report.ask).unwrap();
    assert!(
        asked
            .get("disk")
            .is_some_and(|reason| reason.contains("2 drives")),
        "the machine asks about its disk: {asked:?}"
    );
    servers
        .apply(Servers::BEGIN_INSTALL.with((
            machine.as_str().to_owned(),
            nonce,
            report.drives,
            report.ask,
        )))
        .unwrap();

    let entry = servers
        .state()
        .unwrap()
        .servers
        .get(&machine)
        .cloned()
        .unwrap();
    assert_eq!(
        machine::step(&entry, &drives).unwrap(),
        machine::Step::Wait(vec!["disk".to_owned()]),
        "it waits while the question is open"
    );

    // Another of the person's devices answers in the entry.
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
    let machine::Step::Install(plan) = machine::step(&entry, &drives).unwrap() else {
        panic!("an answered machine installs: {entry:?}");
    };
    assert_eq!(
        plan.disk.encode(),
        "serial:serial-a",
        "with the answered drive"
    );
    assert_eq!(plan.name, "cupboard", "and the name it was planned under");
    assert_eq!(plan.unlock, "tpm");
    assert_eq!(plan.network.addressing, "dhcp");

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
    assert_eq!(
        machine::step(&entry, &drives).unwrap(),
        machine::Step::Finished
    );
    assert_eq!(
        entry.install_plan().unwrap().disk.encode(),
        "serial:serial-a",
        "the installed entry still says what it was installed from"
    );
}

/// A plan open on its network or on the workspaces it holds is not asked
/// about: the person's devices set those up on the machine's entry, so nothing
/// waits for an answer.
#[test]
fn a_machine_asks_nothing_about_what_its_entry_sets_up() {
    let servers = planned("largest", "network,hosting");
    let mut drives = empty_drives();
    drives[1].bytes = 2_000_000_000_000;
    let report = machine::report(
        servers
            .state()
            .unwrap()
            .planned_by_nonce(NONCE)
            .map(|(_, server)| server)
            .unwrap(),
        &drives,
    )
    .unwrap();
    assert!(report.ask.is_empty(), "nothing is asked: {report:?}");
}

/// A rule that matches exactly one of the machine's drives is settled by the
/// machine itself; the plan's other open choices are asked instead.
#[test]
fn a_machine_settles_its_disk_when_the_rule_names_one_drive() {
    let servers = planned("largest", "name");
    let mut drives = empty_drives();
    drives[1].bytes = 2_000_000_000_000;
    let report = machine::report(
        servers
            .state()
            .unwrap()
            .planned_by_nonce(NONCE)
            .map(|(_, server)| server)
            .unwrap(),
        &drives,
    )
    .unwrap();
    let asked: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&report.ask).unwrap();
    assert!(
        !asked.contains_key("disk"),
        "the rule settled the disk: {asked:?}"
    );
    assert!(
        asked.contains_key("name"),
        "the plan's open name is asked: {asked:?}"
    );
}

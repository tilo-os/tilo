//! A delegated agent as a governed member: admitted only by the governor,
//! only within its delegation; acting as its person, never above Editor;
//! attributed to its own key; and dead the moment its delegation ends or the
//! governor withdraws it, while its history stays on the roster.

use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use tilo_relay_protocol::agents::{AgentDelegation, AgentScope};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use web_time::{Duration, SystemTime};

use crate::{StatePeer, TransportOrigin};

const ROOT_SEED: [u8; 32] = [1; 32];

fn ns(at: SystemTime) -> u64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap()
}

const fn no_capabilities() -> DeviceCapabilities {
    DeviceCapabilities {
        can_own_workspaces: false,
        can_admit_devices: false,
        can_act_as_host: false,
        can_read_all_workspaces: false,
        foreign: false,
        can_administer_devices: false,
    }
}

/// An agent device of `person`: its key and its certificate, as an approver
/// issues them (no capabilities).
fn agent(person: &SigningKey, seed: u8) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("Agent: Scribe {seed}"),
        capabilities: no_capabilities(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, person).unwrap();
    (key, cert)
}

/// `person`'s delegation of `device` to `room` until `expires`.
fn delegation(
    person: &SigningKey,
    device: [u8; 32],
    room: &str,
    issued: SystemTime,
    expires: SystemTime,
) -> AgentDelegation {
    AgentDelegation::sign(
        person,
        device,
        "scribe",
        "Scribe",
        [10, 20, 30],
        AgentScope {
            rooms: vec![room.into()],
            apps: Vec::new(),
            may_confirm: false,
        },
        ns(issued),
        ns(expires),
    )
    .unwrap()
}

/// An edit of the seeded record by `key`, as `writer` publishes it.
fn edit(writer: &mut StatePeer, key: &SigningKey, now: SystemTime) -> crate::NeighbourMsg {
    let mut doc = AutoCommit::load(&writer.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let before = doc.get_heads();
    doc.put(ROOT, "by agent", true).unwrap();
    doc.commit();
    writer
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), key, now)
        .unwrap()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario from admission to expiry: its steps and assertions stay together"
)]
fn a_delegated_agent_acts_as_its_person_within_its_window_and_never_after() {
    let (mut governor, authority, other_device, _, now) = crate::admission_tests::seeded();
    let room = governor.room().to_owned();
    let person = SigningKey::from_bytes(&ROOT_SEED);
    let (key, cert) = agent(&person, 8);
    let device = cert.device_public_key;
    let until = now + Duration::from_secs(10);
    let granted = delegation(&person, device, &room, now - Duration::from_secs(1), until);
    let mut member = governor.clone();

    // Only the pinned authority admits, and only within the delegation.
    assert!(
        governor
            .admit_agent(cert.clone(), granted.clone(), &other_device, now)
            .is_err()
    );
    let elsewhere = delegation(
        &person,
        device,
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        now - Duration::from_secs(1),
        until,
    );
    assert!(
        governor
            .admit_agent(cert.clone(), elsewhere, &authority, now)
            .unwrap_err()
            .contains("not delegated this room")
    );
    let lapsed = delegation(&person, device, &room, now - Duration::from_secs(10), now);
    assert!(
        governor
            .admit_agent(cert.clone(), lapsed, &authority, now)
            .unwrap_err()
            .contains("not current")
    );
    let (_, someone_else) = agent(&person, 9);
    assert!(
        governor
            .admit_agent(someone_else, granted.clone(), &authority, now)
            .unwrap_err()
            .contains("another device")
    );
    let mut powerful = cert.clone();
    powerful.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut powerful, &person).unwrap();
    assert!(
        governor
            .admit_agent(powerful, granted.clone(), &authority, now)
            .is_err()
    );
    let stranger = SigningKey::from_bytes(&[33; 32]);
    let (_, foreign) = agent(&stranger, 10);
    let foreign_grant = delegation(
        &stranger,
        foreign.device_public_key,
        &room,
        now - Duration::from_secs(1),
        until,
    );
    assert!(
        governor
            .admit_agent(foreign, foreign_grant, &authority, now)
            .unwrap_err()
            .contains("owner's agents")
    );
    assert!(governor.governed_agents(now).unwrap().is_empty());

    assert!(
        governor
            .admit_agent(cert.clone(), granted.clone(), &authority, now)
            .unwrap()
    );
    assert!(
        !governor
            .admit_agent(cert, granted, &authority, now)
            .unwrap(),
        "admitting the same delegation again changes nothing"
    );

    // Every member learns the agent from governance: it acts as its
    // person, but never governs.
    assert!(
        member
            .receive_from(
                TransportOrigin::RelayDelta(authority.verifying_key().to_bytes()),
                governor.room_message(),
                now,
            )
            .unwrap()
            .metadata_changed
    );
    let (role, _, acting_for) = member.permissions(device, now).unwrap();
    assert_eq!(role, collab_types::Role::Editor);
    assert_eq!(acting_for, hex::encode(person.verifying_key().to_bytes()));
    let readers = member.governed_access(now).unwrap();
    assert!(readers.member_devices.contains(&device));
    assert_eq!(
        member.governed_agents(now).unwrap(),
        vec![crate::GovernedAgent {
            device_public_key: device,
            user_public_key: person.verifying_key().to_bytes(),
            agent_id: "scribe".into(),
            label: "Scribe".into(),
            color: [10, 20, 30],
            expires_at_ns: ns(until),
            withdrawn_at_ns: None,
            acting: true,
        }]
    );

    // Its edit is signed by its own key, and the governor admits it.
    let mut own = member.clone();
    let message = edit(&mut own, &key, now);
    let late = edit(&mut own, &key, now);
    assert!(
        governor
            .receive_from(TransportOrigin::RelayDelta(device), message, now)
            .unwrap()
            .changed
    );

    // Its delegation ends: no reader, no writer, still on the roster.
    let after = until + Duration::from_secs(1);
    assert!(
        member
            .permissions(device, after)
            .unwrap_err()
            .contains("not current")
    );
    assert!(
        !member
            .governed_access(after)
            .unwrap()
            .member_devices
            .contains(&device)
    );
    assert!(!member.governed_agents(after).unwrap()[0].acting);
    assert!(
        governor
            .receive_from(TransportOrigin::RelayDelta(device), late, after)
            .is_err(),
        "an agent past its delegation writes nothing"
    );

    // The roster survives a restore.
    let restored = StatePeer::restore(
        &member.durable().unwrap(),
        member.room(),
        member.authority().clone(),
        vec![],
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    assert_eq!(
        restored.permissions(device, now).unwrap().0,
        collab_types::Role::Editor
    );
    assert!(restored.permissions(device, after).is_err());
}

#[test]
fn a_withdrawn_agent_is_dead_at_once_and_never_readmitted() {
    let (mut governor, authority, other_device, _, now) = crate::admission_tests::seeded();
    let room = governor.room().to_owned();
    let person = SigningKey::from_bytes(&ROOT_SEED);
    let (key, cert) = agent(&person, 8);
    let device = cert.device_public_key;
    let granted = delegation(
        &person,
        device,
        &room,
        now - Duration::from_secs(1),
        now + Duration::from_secs(3600),
    );
    governor
        .admit_agent(cert.clone(), granted, &authority, now)
        .unwrap();
    let mut member = governor.clone();
    let before = edit(&mut member, &key, now);

    assert!(governor.withdraw_agent(device, &other_device, now).is_err());
    let withdrawn = now + Duration::from_secs(5);
    assert!(
        governor
            .withdraw_agent(device, &authority, withdrawn)
            .unwrap()
    );
    assert!(
        !governor
            .withdraw_agent(device, &authority, withdrawn)
            .unwrap()
    );
    assert!(
        governor
            .permissions(device, withdrawn)
            .unwrap_err()
            .contains("withdrawn")
    );
    assert!(
        !governor
            .governed_access(withdrawn)
            .unwrap()
            .member_devices
            .contains(&device)
    );
    let roster = governor.governed_agents(withdrawn).unwrap();
    assert_eq!(roster[0].withdrawn_at_ns, Some(ns(withdrawn)));
    assert!(!roster[0].acting);
    assert_eq!(
        governor.governed_agent(&device).map(|(_, _, at)| at),
        Some(Some(ns(withdrawn)))
    );
    // What it wrote before the withdrawal is still its own.
    assert!(
        governor
            .receive_from(TransportOrigin::RelayDelta(device), before, now)
            .unwrap()
            .changed
    );
    let after = edit(&mut member, &key, withdrawn);
    assert!(
        governor
            .receive_from(TransportOrigin::RelayDelta(device), after, withdrawn)
            .is_err()
    );

    // Withdrawal is final: not even a newer delegation brings it back.
    let renewed = delegation(
        &person,
        device,
        &room,
        withdrawn,
        withdrawn + Duration::from_secs(3600),
    );
    assert!(
        governor
            .admit_agent(cert.clone(), renewed, &authority, withdrawn)
            .unwrap_err()
            .contains("withdrawn")
    );
    // Nor does admitting its certificate as an ordinary device.
    assert!(
        !governor
            .admit_certificate(cert, &authority, withdrawn)
            .unwrap()
    );
    assert!(governor.permissions(device, withdrawn).is_err());
}

#[test]
fn members_and_agents_never_trade_places() {
    let (mut governor, authority, _, member_cert, now) = crate::admission_tests::seeded();
    let room = governor.room().to_owned();
    let person = SigningKey::from_bytes(&ROOT_SEED);
    let window = |device| {
        delegation(
            &person,
            device,
            &room,
            now - Duration::from_secs(1),
            now + Duration::from_secs(60),
        )
    };
    // A person's ordinary device, admitted as one, never becomes an agent.
    let (_, plain) = agent(&person, 12);
    assert!(
        governor
            .admit_certificate(plain.clone(), &authority, now)
            .unwrap()
    );
    assert!(
        governor
            .admit_agent(
                plain.clone(),
                window(plain.device_public_key),
                &authority,
                now
            )
            .unwrap_err()
            .contains("member, not an agent")
    );
    // Nor is a member withdrawn as an agent.
    assert!(
        governor
            .withdraw_agent(member_cert.device_public_key, &authority, now)
            .unwrap_err()
            .contains("member, not an agent")
    );
    // Another certificate under a member's key is a conflict.
    let mut as_agent = member_cert.clone();
    as_agent.capabilities = no_capabilities();
    sign_device_cert(&mut as_agent, &person).unwrap();
    assert!(
        governor
            .admit_agent(
                as_agent,
                window(member_cert.device_public_key),
                &authority,
                now
            )
            .unwrap_err()
            .contains("conflict")
    );
    assert_eq!(
        governor
            .permissions(member_cert.device_public_key, now)
            .unwrap()
            .0,
        collab_types::Role::Owner,
        "the person's own device keeps its role"
    );
}

#[test]
fn identities_without_an_agent_keep_their_bytes() {
    let (_, cert) = agent(&SigningKey::from_bytes(&ROOT_SEED), 8);
    let identity = crate::state::GovernedIdentity {
        device_cert: cert,
        revocations: vec![],
        admission: None,
        admissions: vec![],
        workspace_delegations: vec![],
        agent: None,
    };
    let bytes = serde_json::to_string(&identity).unwrap();
    assert!(
        !bytes.contains("agent\""),
        "an ordinary identity's governed bytes are unchanged: {bytes}"
    );
    let back: crate::state::GovernedIdentity = serde_json::from_str(&bytes).unwrap();
    assert!(back.agent.is_none());
}

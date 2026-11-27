//! A delegated agent as a member: admitted only by the owner,
//! only within its delegation; acting as its person, never above Editor;
//! attributed to its own key; and dead the moment its delegation ends or an
//! administrator withdraws it, while its history stays on the roster.

use crate::join::WorkspaceId;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use tilo_workspace_join::delegation::{DelegateScope, Delegation};
use web_time::{Duration, SystemTime};

use crate::state::{DelegateInactive, MembershipError};
use crate::{StatePeer, TransportOrigin};

const ROOT_SEED: [u8; 32] = [1; 32];
/// The agent every delegation here names.
const AGENT: &str = "scribe";

use crate::fixture::ns;

const fn no_capabilities() -> DeviceCapabilities {
    DeviceCapabilities::nothing()
}

/// An agent device of `person`: its key and its certificate, as an approver
/// issues them (no capabilities).
fn agent(person: &SigningKey, seed: u8) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let cert = crate::fixture::labelled(
        person,
        &key,
        &format!("Agent: Scribe {seed}"),
        no_capabilities(),
    );
    (key, cert)
}

/// `person`'s delegation of `device` to `workspace` until `expires`.
fn delegation(
    person: &SigningKey,
    device: [u8; 32],
    workspace: &str,
    issued: SystemTime,
    expires: SystemTime,
) -> Delegation {
    Delegation::sign(
        person,
        device,
        tilo_workspace_join::delegation::Delegate::Agent {
            agent_id: AGENT.into(),
        },
        "Scribe",
        [10, 20, 30],
        DelegateScope {
            workspaces: vec![WorkspaceId::parse(workspace).unwrap()],
            apps: Vec::new(),
        },
        ns(issued),
        ns(expires),
    )
    .unwrap()
}

/// An edit of the seeded record by the agent whose key and certificate are `agent`, acting for
/// its person, as `writer` publishes it. An agent's own key writes as the agent, never as its
/// person, and the change keeps the invocation that key signed, as every agent with a key of its
/// own asks under it.
fn edit(
    writer: &mut StatePeer,
    (key, cert): (&SigningKey, &DeviceCert),
    now: SystemTime,
) -> crate::NeighbourMsg {
    use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget, SignedCall};
    use tilo_actions::{
        ActionAddress, ActionContext, ActionInvoker, Did, EditOp, FieldEdit, InvocationId, PathStep,
    };
    let mut doc = AutoCommit::load(&writer.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let before = doc.get_heads();
    doc.put(ROOT, "by agent", cert.device_label.as_str())
        .unwrap();
    let invocation = InvocationId::new();
    let mut request = InvokeRequest {
        asks: false,
        invocation,
        target: RemoteActionTarget {
            provider: "test.app".into(),
            address: ActionAddress::new("tab:1", "field-edit"),
        },
        argument: InvokeArgument::Text("by agent".into()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    request.signed = Some(Box::new(
        tilo_server_protocol::invocation::sign_invocation_now(
            writer.workspace_id(),
            &request,
            cert,
            key,
            ns(now),
        )
        .unwrap(),
    ));
    let context = ActionContext {
        what: Some(Did::Edit(FieldEdit {
            ty: "fixture".into(),
            field: "by agent".into(),
            path: vec![PathStep::Field("by agent".into())],
            op: EditOp::Set,
            old: None,
            new: None,
            args: std::collections::BTreeMap::new(),
        })),
        source_invocation: Some(invocation),
        signed: SignedCall::of(&request).map(Box::new),
        ..ActionContext::for_resource("tab:1", "fixture", "fixture")
    }
    .with_invoker(
        ActionInvoker::agent(
            AGENT,
            tilo_member_types::PersonId::from_root(cert.user_public_key),
        )
        .acting_for(tilo_member_types::PersonId::from_root(cert.user_public_key)),
    );
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(context.change_message().unwrap()),
    );
    writer
        .publication_mut()
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), key, now)
        .unwrap()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario from admission to expiry: its steps and assertions stay together"
)]
fn a_delegated_agent_acts_as_its_person_within_its_window_and_never_after() {
    let (mut owner_copy, authority, own_device, _, now) = crate::admission_tests::seeded();
    // A device the workspace has no certificate for: not one of the owner's.
    let other_device = SigningKey::from_bytes(&[0x5a; 32]);
    let workspace = owner_copy.workspace_id().to_owned();
    let person = SigningKey::from_bytes(&ROOT_SEED);
    let (key, cert) = agent(&person, 8);
    let agent_cert = cert.clone();
    let device = cert.device_public_key;
    let until = now + Duration::from_secs(10);
    let granted = delegation(
        &person,
        device,
        &workspace,
        now - Duration::from_secs(1),
        until,
    );
    let mut member = owner_copy.clone();

    // Only the agent writes its own row, and only within the delegation.
    assert!(
        owner_copy
            .governance_mut()
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
        owner_copy
            .governance_mut()
            .admit_agent(cert.clone(), elsewhere, &key, now)
            .unwrap_err()
            .contains("not delegated this workspace")
    );
    let lapsed = delegation(
        &person,
        device,
        &workspace,
        now - Duration::from_secs(10),
        now,
    );
    assert!(
        owner_copy
            .governance_mut()
            .admit_agent(cert.clone(), lapsed, &key, now)
            .unwrap_err()
            .contains("not current")
    );
    let (someone_key, someone_else) = agent(&person, 9);
    assert!(
        owner_copy
            .governance_mut()
            .admit_agent(someone_else, granted.clone(), &someone_key, now)
            .unwrap_err()
            .contains("another device")
    );
    let mut powerful = cert.clone();
    powerful.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut powerful, &person).unwrap();
    assert!(
        owner_copy
            .governance_mut()
            .admit_agent(powerful, granted.clone(), &key, now)
            .is_err()
    );
    let stranger = SigningKey::from_bytes(&[33; 32]);
    let (foreign_key, foreign) = agent(&stranger, 10);
    let foreign_grant = delegation(
        &stranger,
        foreign.device_public_key,
        &workspace,
        now - Duration::from_secs(1),
        until,
    );
    assert!(
        owner_copy
            .governance_mut()
            .admit_agent(foreign, foreign_grant, &foreign_key, now)
            .unwrap_err()
            .contains("only an agent of one of the workspace's members is admitted")
    );
    assert!(
        owner_copy
            .governance()
            .delegated_agents(now)
            .unwrap()
            .is_empty()
    );

    assert!(
        owner_copy
            .governance_mut()
            .admit_agent(cert.clone(), granted.clone(), &authority, now)
            .is_err(),
        "no other device writes the agent's row"
    );
    assert!(
        owner_copy
            .governance_mut()
            .admit_agent(cert.clone(), granted.clone(), &key, now)
            .unwrap()
    );
    assert!(
        !owner_copy
            .governance_mut()
            .admit_agent(cert, granted, &key, now)
            .unwrap(),
        "admitting the same delegation again changes nothing"
    );

    // Every member learns the agent from governance: it acts as its
    // person, but never administers.
    assert!(
        member
            .receive_from(
                crate::fixture::origin(&authority),
                owner_copy.workspace_message(),
                now,
            )
            .unwrap()
            .metadata_changed
    );
    let authority_of_agent = member.permissions(device, now).unwrap();
    assert_eq!(
        member.role(&device, now).unwrap(),
        collab_types::Role::Editor
    );
    assert_eq!(
        authority_of_agent.person_id(),
        Some(hex::encode(person.verifying_key().to_bytes()).as_str())
    );
    // Its person owns the workspace; the delegation bounds it to an Editor's
    // grants: it changes the workspace's records, and administers nothing.
    let workspace_node = tilo_action_policy::TreePath::workspace(&workspace);
    let governance = tilo_action_policy::roles::governance_action(
        &workspace,
        tilo_action_policy::roles::governance::SET_ROLE,
    );
    assert!(authority_of_agent.allows(
        &workspace_node,
        tilo_action_policy::ActionKind::Undoable,
        now
    ));
    assert!(!authority_of_agent.allows(
        &governance,
        tilo_action_policy::roles::GOVERNANCE_KIND,
        now
    ));
    assert!(
        member
            .permissions(own_device.verifying_key().to_bytes(), now)
            .unwrap()
            .allows(&governance, tilo_action_policy::roles::GOVERNANCE_KIND, now),
        "the person's own device administers"
    );
    let readers = member.governance().member_list_access(now).unwrap();
    assert!(readers.member_devices.contains(&device));
    assert_eq!(
        member.governance().delegated_agents(now).unwrap(),
        vec![crate::DelegatedAgent {
            device_public_key: device,
            user_public_key: person.verifying_key().to_bytes(),
            agent_id: AGENT.into(),
            label: "Scribe".into(),
            color: [10, 20, 30],
            expires_at_ns: ns(until),
            withdrawn_at_ns: None,
            acting: true,
        }]
    );

    // Its edit is signed by its own key, and the host admits it.
    let mut own = member.clone();
    let message = edit(&mut own, (&key, &agent_cert), now);
    let late = edit(&mut own, (&key, &agent_cert), now);
    assert!(
        owner_copy
            .receive_from(TransportOrigin::ServerDelta(device), message, now)
            .unwrap()
            .changed
    );

    // Its delegation ends: no reader, no writer, still on the roster.
    let after = until + Duration::from_secs(1);
    assert!(matches!(
        member.permissions(device, after),
        Err(MembershipError::Delegate(DelegateInactive::Delegation))
    ));
    assert!(
        !member
            .governance()
            .member_list_access(after)
            .unwrap()
            .member_devices
            .contains(&device)
    );
    assert!(!member.governance().delegated_agents(after).unwrap()[0].acting);
    assert!(
        owner_copy
            .receive_from(TransportOrigin::ServerDelta(device), late, after)
            .is_err(),
        "an agent past its delegation writes nothing"
    );

    // The roster survives a restore.
    let restored = StatePeer::restore(
        &member.durable().unwrap(),
        member.workspace_id(),
        member.owner_certificates()[0].clone(),
        vec![],
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    assert_eq!(
        restored.role(&device, now).unwrap(),
        collab_types::Role::Editor
    );
    assert!(restored.permissions(device, after).is_err());
}

tilo_test_env::scenario! {
    fn an_agent_is_admitted_and_withdrawn_only_as_an_agent;

    phase a_withdrawn_agent_is_dead_at_once_and_never_readmitted;
    {
        let (mut owner_copy, authority, _, _, now) = crate::admission_tests::seeded();
        // A device the workspace has no certificate for: not one of the owner's.
        let other_device = SigningKey::from_bytes(&[0x5a; 32]);
        let workspace = owner_copy.workspace_id().to_owned();
        let person = SigningKey::from_bytes(&ROOT_SEED);
        let (key, cert) = agent(&person, 8);
        let device = cert.device_public_key;
        let granted = delegation(
            &person,
            device,
            &workspace,
            now - Duration::from_secs(1),
            now + Duration::from_secs(3600),
        );
        owner_copy
            .governance_mut()
            .admit_agent(cert.clone(), granted, &key, now)
            .unwrap();
        let mut member = owner_copy.clone();
        let before = edit(&mut member, (&key, &cert), now);
        // What it wrote and the host took in before the withdrawal stays
        // its own.
        assert!(
            owner_copy
                .receive_from(TransportOrigin::ServerDelta(device), before, now)
                .unwrap()
                .changed
        );
        let unseen = edit(&mut member, (&key, &cert), now);

        assert!(
            owner_copy
                .governance_mut()
                .withdraw_agent(device, &other_device, now)
                .is_err()
        );
        let withdrawn = now + Duration::from_secs(5);
        assert!(
            owner_copy
                .governance_mut()
                .withdraw_agent(device, &authority, withdrawn)
                .unwrap()
        );
        assert!(
            !owner_copy
                .governance_mut()
                .withdraw_agent(device, &authority, withdrawn)
                .unwrap()
        );
        assert!(matches!(
            owner_copy.permissions(device, withdrawn),
            Err(MembershipError::Delegate(DelegateInactive::Withdrawn))
        ));
        assert!(
            !owner_copy
                .governance()
                .member_list_access(withdrawn)
                .unwrap()
                .member_devices
                .contains(&device)
        );
        let roster = owner_copy.governance().delegated_agents(withdrawn).unwrap();
        assert_eq!(roster[0].withdrawn_at_ns, Some(ns(withdrawn)));
        assert!(!roster[0].acting);
        assert_eq!(
            owner_copy
                .governance()
                .listed_delegate(&device)
                .map(|(_, _, at)| at),
            Some(Some(ns(withdrawn)))
        );
        // A write on the governance from before the withdrawal that the
        // withdrawal did not take is judged as written after it.
        assert!(
            owner_copy
                .receive_from(TransportOrigin::ServerDelta(device), unseen, withdrawn)
                .is_err()
        );
        let after = edit(&mut member, (&key, &cert), withdrawn);
        assert!(
            owner_copy
                .receive_from(TransportOrigin::ServerDelta(device), after, withdrawn)
                .is_err()
        );

        // Withdrawal is final: not even a newer delegation brings it back.
        let renewed = delegation(
            &person,
            device,
            &workspace,
            withdrawn,
            withdrawn + Duration::from_secs(3600),
        );
        assert!(
            owner_copy
                .governance_mut()
                .admit_agent(cert.clone(), renewed, &key, withdrawn)
                .unwrap_err()
                .contains("withdrawn")
        );
        // Nor does admitting its certificate as an ordinary device.
        assert!(
            !owner_copy
                .governance_mut().admit_member_device(&cert, &key, withdrawn)
                .unwrap()
        );
        assert!(owner_copy.permissions(device, withdrawn).is_err());
    }

    phase members_and_agents_never_trade_places;
    {
        let (mut owner_copy, authority, member_key, member_cert, now) =
            crate::admission_tests::seeded();
        let workspace = owner_copy.workspace_id().to_owned();
        let person = SigningKey::from_bytes(&ROOT_SEED);
        let window = |device| {
            delegation(
                &person,
                device,
                &workspace,
                now - Duration::from_secs(1),
                now + Duration::from_secs(60),
            )
        };
        // A person's ordinary device, admitted as one, never becomes an agent.
        let (plain_key, plain) = agent(&person, 12);
        assert!(
            owner_copy
                .governance_mut().admit_member_device(&plain, &plain_key, now)
                .unwrap()
        );
        assert!(
            owner_copy
                .governance_mut()
                .admit_agent(
                    plain.clone(),
                    window(plain.device_public_key),
                    &plain_key,
                    now
                )
                .unwrap_err()
                .contains("member, not an agent")
        );
        // Nor is a member withdrawn as an agent.
        assert!(
            owner_copy
                .governance_mut()
                .withdraw_agent(member_cert.device_public_key, &authority, now)
                .unwrap_err()
                .contains("member, not an agent")
        );
        // Another certificate under a member's key is a conflict.
        let mut as_agent = member_cert.clone();
        as_agent.capabilities = no_capabilities();
        sign_device_cert(&mut as_agent, &person).unwrap();
        assert!(
            owner_copy
                .governance_mut()
                .admit_agent(
                    as_agent,
                    window(member_cert.device_public_key),
                    &member_key,
                    now
                )
                .unwrap_err()
                .contains("conflict")
        );
        assert_eq!(
            owner_copy.role(&member_cert.device_public_key, now).unwrap(),
            collab_types::Role::Owner,
            "the person's own device keeps its role"
        );
    }

    phase identities_without_an_agent_keep_their_bytes;
    {
        let (_, cert) = agent(&SigningKey::from_bytes(&ROOT_SEED), 8);
        let identity = crate::state::DeviceRow {
            device_cert: cert,
            revocations: vec![],
            admission: None,
            admissions: vec![],
            delegate: None,
            workspace_delegations: vec![],
            removed: false,
            entry: None,
        };
        let bytes = identity.encode().unwrap();
        let back = crate::state::DeviceRow::decode(&bytes).unwrap();
        assert!(back.delegate.is_none());
        assert_eq!(
            back.encode().unwrap(),
            bytes,
            "an ordinary identity's row bytes read and write back alike"
        );
    }
}

/// A key `person`'s root granted the agent `AGENT` in `workspace`, as each of their devices
/// that runs it makes one: the agent by its id in the certificate.
fn agent_key(
    person: &SigningKey,
    seed: u8,
    workspace: &str,
    now: SystemTime,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let id = *WorkspaceId::parse(workspace).unwrap().as_uuid().as_bytes();
    let mut cert = DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("Scribe on device {seed}"),
        capabilities: DeviceCapabilities::agent_grant(tilo_user_identity::AgentScope::agent_in(
            id, AGENT,
        )),
        issued_at_ns: ns(now.checked_sub(Duration::from_secs(1)).unwrap()),
        expires_at_ns: Some(ns(now.checked_add(Duration::from_secs(3600)).unwrap())),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, person).unwrap();
    (key, cert)
}

#[test]
fn one_agent_on_two_devices_is_one_agent_in_permissions_and_in_history() {
    // The person owns the workspace; their devices hold its keys.
    let (mut peer, authority, _, _, now) = crate::admission_tests::seeded();
    let workspace = peer.workspace_id().to_owned();
    let person = SigningKey::from_bytes(&ROOT_SEED);
    // The same agent of the person's, on their laptop and on their phone: a key each, which
    // redeems its person's invitation as every key of theirs does.
    let laptop = agent_key(&person, 8, &workspace, now);
    let phone = agent_key(&person, 9, &workspace, now);
    for (seed, (key, cert)) in [(80, &laptop), (90, &phone)] {
        assert!(
            crate::admission_tests::redeem_key(&mut peer, &authority, (key, cert), now, seed),
            "{}",
            cert.device_label
        );
    }
    // A key of the same agent whose grant names another workspace alone.
    let elsewhere = agent_key(&person, 10, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", now);
    let admitted = crate::admission_tests::redeem_key(
        &mut peer,
        &authority,
        (&elsewhere.0, &elsewhere.1),
        now,
        100,
    );

    // Permissions: both keys are the one agent, for its person, and never the person.
    let one = tilo_member_types::MemberId::agent(
        AGENT.to_owned(),
        hex::encode(person.verifying_key().to_bytes()).as_str(),
    );
    for (_, cert) in [&laptop, &phone] {
        let access = peer.permissions(cert.device_public_key, now).unwrap();
        assert_eq!(access.acting.member_id(), Some(&one), "{:?}", access.acting);
        assert_eq!(
            peer.role(&cert.device_public_key, now).unwrap(),
            collab_types::Role::Editor,
            "its person owns the workspace; the agent governs nothing"
        );
    }

    // The certificate's grant bounds where the agent acts: a key granted another workspace alone
    // is not listed here, and acts in none.
    assert!(
        !admitted,
        "a key granted another workspace is not listed here"
    );
    assert!(
        peer.permissions(elsewhere.1.device_public_key, now)
            .is_err(),
        "a grant for another workspace reaches none here"
    );

    // History: what either key writes as that agent is taken as that agent's.
    let seeded = AutoCommit::load(&peer.document("test.app", "tab:1").unwrap())
        .unwrap()
        .get_heads();
    for (key, cert) in [&laptop, &phone] {
        let mut own = peer.clone();
        let message = edit(&mut own, (key, cert), now);
        assert!(
            peer.receive_from(
                TransportOrigin::ServerDelta(cert.device_public_key),
                message,
                now
            )
            .unwrap()
            .changed,
            "{}",
            cert.device_label
        );
    }
    let mut doc = AutoCommit::load(&peer.document("test.app", "tab:1").unwrap()).unwrap();
    let invokers: Vec<_> = doc
        .get_changes(&seeded)
        .into_iter()
        .filter_map(|change| change.message().map(str::to_owned))
        .filter_map(|message| tilo_actions::ActionContext::from_change_message(&message))
        .filter_map(|context| context.invoker)
        .collect();
    assert_eq!(invokers.len(), 2, "{invokers:?}");
    assert!(
        invokers
            .iter()
            .all(|invoker| invoker.acting.member_id() == Some(&one)),
        "{invokers:?}"
    );
}

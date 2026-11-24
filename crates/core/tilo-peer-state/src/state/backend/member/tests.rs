use super::*;
use crate::StatePeer;
use crate::state::GovernedIdentity;
use crate::state::now_ns;
use crate::verified_certificate::VerifiedCertificate;
use automerge::ROOT;
use automerge::ReadDoc;
use automerge::transaction::Transactable;
use ed25519_dalek::SigningKey;
use tilo_action_policy::Grant;
use tilo_action_policy::{ActionKind, Answer, Grantee, KindSet, TreePath, roles};
use web_time::Duration;

/// The person tests name the workspace's owner, and the device they sign with.
fn owner() -> (SigningKey, tilo_user_identity::DeviceCert) {
    crate::members::op::tests_support::device(200)
}
/// Sign `change` into the workspace as its owner: the test owner's device, or
/// the seeded device of the person the owner row names instead.
fn sign_as_owner(peer: &mut StatePeer, change: tilo_action_policy::Change) {
    let named = peer
        .core
        .workspace_doc
        .as_deref()
        .and_then(|doc| doc.get(ROOT, "owner").ok().flatten())
        .and_then(|(value, _)| value.into_string().ok());
    let signer = if named.as_deref() == Some(hex::encode(owner().1.user_public_key).as_str()) {
        owner()
    } else {
        let key = SigningKey::from_bytes(&[2; 32]);
        let cert = peer.core.certificates[&key.verifying_key().to_bytes()]
            .as_ref()
            .clone();
        (key, cert)
    };
    let workspace = peer.core.workspace_doc.as_mut().unwrap();
    crate::members::op::tests_support::sign_into(workspace, &signer, 1_000, vec![change]);
    workspace.commit();
}
fn put_member(peer: &mut StatePeer, person: &str, role: Role) {
    let doc = peer.core.workspace_doc.as_deref().unwrap();
    let member = crate::members::replay_declared(doc)
        .members
        .contains_key(person);
    let person = person.to_owned();
    let change = if member {
        tilo_action_policy::Change::SetRole { person, role }
    } else {
        tilo_action_policy::Change::AddMember { person, role }
    };
    sign_as_owner(peer, change);
}
fn put_grant(peer: &mut StatePeer, grant: &Grant) {
    sign_as_owner(peer, tilo_action_policy::Change::Grant(grant.clone()));
}
fn fixture() -> (StatePeer, [u8; 32], SystemTime) {
    let (mut peer, _, _, cert, now) = crate::admission_tests::seeded();
    let workspace = peer.core.workspace_doc.as_mut().unwrap();
    workspace
        .put(ROOT, "owner", hex::encode(owner().1.user_public_key))
        .unwrap();
    workspace.commit();
    put_member(&mut peer, &hex::encode(cert.user_public_key), Role::Editor);
    (peer, cert.device_public_key, now)
}
tilo_test_env::scenario! {
    fn a_devices_current_membership_decides_what_it_may;

    phase current_member_rechecks_removal;
    {
        let (mut peer, source, now) = fixture();
        let expected = peer.permissions(source, now).unwrap();
        assert_eq!(peer.role(&source, now).unwrap(), Role::Editor);
        // A blocked member is no longer current.
        let block = expected.policy.block(
            expected.person_id().unwrap(),
            Grantee::person("a different person"),
        );
        put_grant(&mut peer, &block);
        assert!(peer.permissions(source, now).is_err());
    }

    phase a_device_whose_row_does_not_prove_it_holds_no_role;
    {
        let (base, source, now) = fixture();
        let mut cases = Vec::new();
        let mut malformed = base.clone();
        replace_row(&mut malformed, &source, b"{bad json".to_vec());
        cases.push(malformed);
        let mut absent = base.clone();
        let workspace = absent.core.workspace_doc.as_mut().unwrap();
        let ids = workspace
            .get(ROOT, "peer_identities_v1")
            .unwrap()
            .unwrap()
            .1;
        workspace.delete(&ids, hex::encode(source)).unwrap();
        cases.push(absent);
        let mut mismatch = base.clone();
        let mut identity = row(&mismatch, &source);
        identity.device_cert.device_label.push_str("changed");
        replace_row(&mut mismatch, &source, identity.encode().unwrap());
        cases.push(mismatch);
        let root = SigningKey::from_bytes(&[1; 32]);
        let mut revoked = tilo_user_identity::RevocationEntry {
            user_public_key: base.owner(),
            revoked_device_public_key: source,
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Compromised,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut revoked, &root).unwrap();
        for mode in 0..4 {
            let mut candidate = base.clone();
            let mut identity = row(&candidate, &source);
            identity.revocations = vec![revoked.clone()];
            if mode == 1 {
                identity.revocations[0].user_public_key = [88; 32];
            }
            if mode == 2 {
                identity.revocations[0].signature[0] ^= 1;
            }
            if mode == 3 {
                let mut bad = revoked.clone();
                bad.signature[0] ^= 1;
                identity.revocations.push(bad);
            }
            replace_row(&mut candidate, &source, identity.encode().unwrap());
            cases.push(candidate);
        }
        let mut missing = base;
        missing.core.workspace_doc = None;
        cases.push(missing);
        for peer in cases {
            assert!(peer.role(&source, now).is_err());
            assert!(peer.role(&[66; 32], now).is_err());
            assert!(peer.role(&source, SystemTime::UNIX_EPOCH).is_err());
        }
    }

    phase current_member_preserves_restriction_role_semantics_and_current_certificate_expiry;
    {
        let (mut restricted, source, now) = fixture();
        let person = hex::encode(row(&restricted, &source).device_cert.user_public_key);
        // A deny on one app narrows what the member may do there; it is no block.
        let deny = Grant::new(
            "denial",
            Grantee::person(person.as_str()),
            TreePath::workspace(restricted.workspace_id()).app("so.tilo.notes"),
            KindSet::ALL,
            Answer::Deny,
            Grantee::person("a different person"),
        );
        put_grant(&mut restricted, &deny);
        assert_eq!(restricted.role(&source, now).unwrap(), Role::Editor);
        let (mut expiring, source, now) = fixture();
        let mut identity = row(&expiring, &source);
        identity.device_cert.expires_at_ns = Some(now_ns(now) + 1);
        tilo_user_identity::sign_device_cert(
            &mut identity.device_cert,
            &SigningKey::from_bytes(&[1; 32]),
        )
        .unwrap();
        expiring.core.certificates.insert(
            source,
            VerifiedCertificate::historical_typed(identity.device_cert.clone()).unwrap(),
        );
        replace_row(&mut expiring, &source, identity.encode().unwrap());
        assert_eq!(expiring.role(&source, now).unwrap(), Role::Editor);
        assert!(
            expiring
                .role(&source, now + Duration::from_secs(1))
                .is_err()
        );
    }

    phase a_foreign_device_acts_with_at_most_what_such_a_device_may;
    {
        let (mut peer, source, now) = fixture();
        let person = hex::encode(row(&peer, &source).device_cert.user_public_key);
        let workspace = peer.core.workspace_doc.as_mut().unwrap();
        workspace.put(ROOT, "owner", person.as_str()).unwrap();
        workspace.commit();
        assert_eq!(peer.role(&source, now).unwrap(), Role::Owner);

        make_foreign(&mut peer, &source);
        assert_foreign(&peer, source, now, Role::Owner);
        let content = TreePath::workspace(peer.workspace_id())
            .app("app")
            .resource("record");
        assert!(
            peer.permissions(source, now)
                .unwrap()
                .allows(&content, ActionKind::Undoable, now)
        );

        // Reaching outside the workspace still asks first, whoever holds it.
        assert!(
            !peer
                .permissions(source, now)
                .unwrap()
                .allows(&content, ActionKind::Outside, now)
        );
        assert!(
            peer.check_document_read(source, "app", "record", now)
                .is_ok()
        );
    }

    phase a_foreign_device_is_not_bounded_in_the_workspace_made_on_it;
    {
        let (mut peer, source, now) = fixture();
        let person = hex::encode(row(&peer, &source).device_cert.user_public_key);
        let workspace = peer.core.workspace_doc.as_mut().unwrap();
        workspace.put(ROOT, "owner", person.as_str()).unwrap();
        workspace.commit();
        make_foreign(&mut peer, &source);
        assert!(peer.permissions(source, now).unwrap().acting.foreign_device);

        peer.core.origin = source;
        let member = peer.permissions(source, now).unwrap();
        assert_eq!(peer.role(&source, now).unwrap(), Role::Owner);
        assert!(!member.acting.foreign_device);
    }

    phase a_read_deny_below_the_workspace_refuses_only_what_it_names;
    // Read is a kind like any other: a Deny on Read below the workspace answers Deny
    // for exactly the nodes under it, the most specific grant winning, while the
    // workspace and every other record stay readable.
    {
        let (mut peer, source, now) = fixture();
        let person = hex::encode(row(&peer, &source).device_cert.user_public_key);
        put_member(&mut peer, &person, Role::Viewer);
        let workspace_id = peer.workspace_id().to_owned();
        let read_grant = |id: &str, node: TreePath, answer: Answer| {
            Grant::new(
                id,
                Grantee::person(person.as_str()),
                node,
                KindSet::READ,
                answer,
                Grantee::person("a different person"),
            )
        };
        for grant in [
            read_grant(
                "payroll-hidden",
                collab_types::document_node(&workspace_id, "app", "payroll"),
                Answer::Deny,
            ),
            read_grant(
                "finance-hidden",
                TreePath::workspace(&workspace_id).app("finance"),
                Answer::Deny,
            ),
            read_grant(
                "finance-summary",
                collab_types::document_node(&workspace_id, "finance", "summary"),
                Answer::Allow,
            ),
        ] {
            put_grant(&mut peer, &grant);
        }
        for (app, record, readable) in [
            ("app", "payroll", false),
            ("app", "notes", true),
            ("other.app", "payroll", true),
            ("finance", "log", false),
            ("finance", "summary", true),
        ] {
            assert_eq!(
                peer.check_document_read(source, app, record, now).is_ok(),
                readable,
                "{app}/{record}"
            );
        }
    }

    phase the_earlier_private_singletons_can_neither_be_read_nor_give_keys;
    {
        let (mut peer, source, now) = fixture();
        let person = hex::encode(row(&peer, &source).device_cert.user_public_key);
        let workspace = peer.core.workspace_doc.as_mut().unwrap();
        workspace.put(ROOT, "owner", person.as_str()).unwrap();
        workspace.commit();
        for (app, record) in [
            ("so.tilo.tube", roles::WORKSPACE_TUBE_HISTORY_DOCUMENT),
            ("so.tilo.messages", roles::WORKSPACE_MESSAGE_DRAFTS_DOCUMENT),
        ] {
            assert!(
                peer.check_document_read(source, app, record, now).is_err(),
                "workspace ownership cannot disclose private data with no verifiable owner"
            );
            assert!(
                !peer.document_key_holders(app, record, now).unwrap().contains(&source),
                "an editing grant cannot restore the old group's read access"
            );
        }
        assert!(
            peer.check_document_read(
                source,
                "so.tilo.chat",
                roles::WORKSPACE_CHAT_DRAFTS_DOCUMENT,
                now,
            ).is_ok(),
            "the workspace's channel drafts remain shared by role"
        );
    }

    phase each_person_writes_their_own_view_of_the_layout_and_no_one_elses;
    // Each person's view of the workspace's layout is an ordinary record of the workspace that
    // every member reads; the workspace's policy lets its person change it whatever their role,
    // and nobody else, an agent acting for them as far as its person may.
    {
        let (mut peer, source, now) = fixture();
        let person = hex::encode(row(&peer, &source).device_cert.user_public_key);
        let other = "ad".repeat(32);
        put_member(&mut peer, &other, Role::Editor);
        let workspace_id = peer.workspace_id().to_owned();
        let view = |of: &str| {
            collab_types::document_node(
                &workspace_id,
                roles::LAYOUT_APP,
                &roles::person_view_record(of),
            )
        };
        let layout = collab_types::document_node(&workspace_id, roles::LAYOUT_APP, "desktop");
        let cx = tilo_action_policy::Context::at_time(now);
        for role in [Role::Editor, Role::Viewer] {
            put_member(&mut peer, &person, role);
            let me = peer.permissions(source, now).unwrap();
            assert_eq!(
                me.answer(&view(&person), ActionKind::Undoable, &cx),
                Answer::Allow,
                "{role:?}, on their own view"
            );
            assert_eq!(
                me.answer(&view(&other), ActionKind::Undoable, &cx),
                Answer::Deny,
                "{role:?}, on another member's view"
            );
            assert!(
                peer.check_document_read(
                    source,
                    roles::LAYOUT_APP,
                    &roles::person_view_record(&other),
                    now
                )
                .is_ok(),
                "every member reads every view"
            );
        }
        put_member(&mut peer, &person, Role::Editor);
        let me = peer.permissions(source, now).unwrap();
        assert_eq!(
            me.answer(&layout, ActionKind::Undoable, &cx),
            Answer::Allow,
            "the layout itself is every Editor's"
        );
        let granted = tilo_action_policy::Acting::handed_agent("assistant", person.as_str())
            .handed_preset(&workspace_id);
        let agent = peer.permissions(source, now).unwrap().acting_as(
            tilo_action_policy::Actor::Member(tilo_action_policy::MemberId::agent("assistant")),
            granted,
        );
        assert_ne!(
            agent.answer(&view(&person), ActionKind::Undoable, &cx),
            Answer::Deny,
            "the person's agent, on its person's view"
        );
        assert_eq!(
            agent.answer(&view(&other), ActionKind::Undoable, &cx),
            Answer::Deny,
            "the person's agent, on another member's view"
        );
    }
}

/// The group the person's two devices read the record with, and the first device's keys.
fn group_for_two_devices(
    workspace: collab_types::WorkspaceId,
    first: &mut key_group::KeyGroup,
    second_device: [u8; 32],
) -> (key_group::KeyGroup, key_group::ContentKeys) {
    let second_key = key_group::MemberKey::generate(second_device).unwrap();
    let welcome = first
        .add_member(second_key.fresh_key_package().unwrap())
        .unwrap();
    let second_group =
        key_group::KeyGroup::process_welcome(workspace, &welcome, second_key).unwrap();
    (second_group, first.content_keys().unwrap())
}

/// Opens what `keys` sealed.
fn unseal_with(
    keys: &key_group::ContentKeys,
) -> impl FnMut(&collab_types::SignedOp) -> Result<Vec<u8>, String> + '_ {
    move |op| {
        keys.open_content(op.artifact, op.encryption.as_ref().unwrap(), &op.payload)
            .map(|bytes| bytes.to_vec())
            .map_err(|error| error.to_string())
    }
}

#[test]
fn a_viewers_drafts_are_signed_and_sealed_for_their_devices() {
    use crate::state::document_capture::{KeptDocument, PeerDocumentReadError};
    use collab_types::{ContentRetention, DocumentMutation};

    const APP: &str = "so.tilo.messages";
    let (mut peer, owner_key, member_key, member, second, now) =
        crate::admission_tests::seeded_editor_with_second_device();
    let person = hex::encode(member.user_public_key);
    put_member(&mut peer, &person, Role::Viewer);
    peer.select_exact_admissions().unwrap();
    let owner = owner_key.verifying_key().to_bytes();
    let device = member.device_public_key;
    let drafts = collab_types::document::DocumentId::of_member(&person, "messages.drafts")
        .unwrap()
        .as_str()
        .to_owned();
    let route = (APP, drafts.as_str());
    let workspace = collab_types::WorkspaceId::parse(peer.workspace_id()).unwrap();
    let group = neighbour_wire::KeyScope::document(workspace, route.0, route.1, None)
        .unwrap()
        .group();
    let mut first_group =
        key_group::KeyGroup::create(group, key_group::MemberKey::generate(device).unwrap())
            .unwrap();
    let (second_group, keys) =
        group_for_two_devices(group, &mut first_group, second.device_public_key);
    assert_eq!(
        peer.document_key_holders(APP, &drafts, now).unwrap(),
        [device, second.device_public_key].into(),
        "the group follows the person to both devices and excludes the owner"
    );
    let mut written = automerge::AutoCommit::new();
    written.put(ROOT, "body", "see you at noon").unwrap();
    let frame = DocumentMutation::Snapshot(written.save()).encode().unwrap();
    assert!(
        peer.draft_write(route, None, &frame, None, device, 0, now)
            .is_err()
    );
    let artifact = collab_types::document_artifact_id(APP, peer.workspace_id(), &drafts);
    let draft = |key: &SigningKey| {
        let mut seal = |plaintext: &[u8]| {
            keys.seal_content(artifact, ContentRetention::of_frame(plaintext), plaintext)
                .map_err(|error| error.to_string())
        };
        peer.draft_write(
            route,
            None,
            &frame,
            Some(&mut seal),
            key.verifying_key().to_bytes(),
            0,
            now,
        )
    };
    // The workspace owner cannot found another person's record: their device's own grants refuse
    // it before it is sealed, as every device that opens it would.
    let refused = draft(&owner_key).unwrap_err();
    assert!(refused.contains("not granted"), "{refused}");
    let mut own = draft(&member_key).unwrap();
    collab_types::sign_op(&mut own, &member_key).unwrap();
    peer.admit_new_local_document(APP, &drafts, own, now)
        .unwrap();

    let second_keys = second_group.content_keys().unwrap();
    let mut unseal = unseal_with(&second_keys);
    let mut held = KeptDocument::default();
    assert!(matches!(
        peer.open_document(owner, route, now, usize::MAX, Some(&mut unseal), &mut held),
        Err(PeerDocumentReadError::Denied)
    ));
    let own_drafts = peer
        .open_document(
            second.device_public_key,
            route,
            now,
            usize::MAX,
            Some(&mut unseal),
            &mut held,
        )
        .expect("the other Viewer device reads what their first device kept");
    assert_eq!(
        own_drafts
            .get(ROOT, "body")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok())
            .as_deref(),
        Some("see you at noon")
    );

    sign_as_owner(
        &mut peer,
        tilo_action_policy::Change::RemoveMember { person },
    );
    assert!(peer.check_document_read(owner, APP, &drafts, now).is_err());
    assert!(
        peer.document_key_holders(APP, &drafts, now)
            .unwrap()
            .is_empty(),
        "retained ownership cannot fall back to shared readership after removal"
    );
}

fn row(peer: &StatePeer, source: &[u8; 32]) -> GovernedIdentity {
    let workspace = peer.admitted_workspace().unwrap();
    let identities = workspace
        .get(ROOT, "peer_identities_v1")
        .unwrap()
        .unwrap()
        .1;
    let raw = workspace
        .get(&identities, hex::encode(source))
        .unwrap()
        .unwrap()
        .0;
    GovernedIdentity::decode(raw.as_bytes().unwrap()).unwrap()
}
fn replace_row(peer: &mut StatePeer, source: &[u8; 32], bytes: Vec<u8>) {
    let workspace = peer.core.workspace_doc.as_mut().unwrap();
    let identities = workspace
        .get(ROOT, "peer_identities_v1")
        .unwrap()
        .unwrap()
        .1;
    workspace
        .put(
            &identities,
            hex::encode(source),
            automerge::ScalarValue::Bytes(bytes),
        )
        .unwrap();
    workspace.commit();
}
fn make_foreign(peer: &mut StatePeer, source: &[u8; 32]) {
    let mut identity = row(peer, source);
    identity.device_cert.capabilities.foreign = true;
    tilo_user_identity::sign_device_cert(
        &mut identity.device_cert,
        &SigningKey::from_bytes(&[1; 32]),
    )
    .unwrap();
    peer.core.certificates.insert(
        *source,
        VerifiedCertificate::historical_typed(identity.device_cert.clone()).unwrap(),
    );
    replace_row(peer, source, identity.encode().unwrap());
}
fn assert_foreign(peer: &StatePeer, source: [u8; 32], now: SystemTime, role: Role) {
    let permissions = peer.permissions(source, now).unwrap();
    assert_eq!(peer.role(&source, now).unwrap(), role);
    assert!(permissions.acting.foreign_device);
    let invite = roles::governance_action(peer.workspace_id(), roles::governance::ADD_MEMBER);
    assert!(!permissions.allows(&invite, ActionKind::Undoable, now));
}

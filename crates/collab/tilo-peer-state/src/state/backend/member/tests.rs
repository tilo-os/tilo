use super::*;
use crate::StatePeer;
use crate::state::now_ns;
use crate::verified_certificate::VerifiedCertificate;
use automerge::ROOT;
use automerge::ReadDoc;
use automerge::transaction::Transactable;
use ed25519_dalek::SigningKey;
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
#[test]
fn current_member_rechecks_removal() {
    let (mut peer, source, now) = fixture();
    let expected = peer.permissions(source, now).unwrap();
    assert_eq!(expected.role, Role::Editor);
    // A blocked member is no longer current.
    let block = expected
        .policy
        .block(&expected.person, Grantee::person("a different person"));
    put_grant(&mut peer, &block);
    assert!(peer.permissions(source, now).is_err());
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
#[test]
fn a_device_whose_row_does_not_prove_it_holds_no_role() {
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

#[test]
fn current_member_preserves_restriction_role_semantics_and_current_certificate_expiry() {
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
    let legacy = peer.permissions(source, now).unwrap();
    assert_eq!(legacy.role, role);
    assert!(legacy.foreign);
    let invite = roles::governance_action(peer.workspace_id(), roles::governance::INVITE);
    assert!(!legacy.allows(&invite, ActionKind::Undoable, now));
}
#[test]
fn a_foreign_device_acts_with_at_most_the_workspace_bound() {
    let (mut peer, source, now) = fixture();
    let person = hex::encode(row(&peer, &source).device_cert.user_public_key);
    let workspace = peer.core.workspace_doc.as_mut().unwrap();
    workspace.put(ROOT, "owner", person.as_str()).unwrap();
    workspace.commit();
    assert_eq!(peer.permissions(source, now).unwrap().role, Role::Owner);

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

    // The workspace narrows what a foreign device may do to reading.
    let workspace_id = peer.workspace_id().to_owned();
    put_grant(
        &mut peer,
        &Grant::new(
            "foreign-device-read",
            Grantee::ForeignDevice,
            TreePath::workspace(&workspace_id),
            KindSet::UNDOABLE.union(KindSet::OUTSIDE),
            Answer::Deny,
            Grantee::person(person.as_str()),
        ),
    );
    assert_foreign(&peer, source, now, Role::Owner);
    assert!(
        !peer
            .permissions(source, now)
            .unwrap()
            .allows(&content, ActionKind::Undoable, now)
    );
    assert!(
        peer.check_document_read(source, "app", "record", now)
            .is_ok()
    );
}
#[test]
fn a_foreign_device_is_not_bounded_in_the_workspace_made_on_it() {
    let (mut peer, source, now) = fixture();
    let person = hex::encode(row(&peer, &source).device_cert.user_public_key);
    let workspace = peer.core.workspace_doc.as_mut().unwrap();
    workspace.put(ROOT, "owner", person.as_str()).unwrap();
    workspace.commit();
    make_foreign(&mut peer, &source);
    assert!(peer.permissions(source, now).unwrap().foreign);

    peer.core.origin = source;
    let member = peer.permissions(source, now).unwrap();
    assert_eq!(member.role, Role::Owner);
    assert!(!member.foreign);
}
/// Read is a kind like any other: a Deny on Read below the workspace answers Deny
/// for exactly the nodes under it, the most specific grant winning, while the
/// workspace and every other record stay readable.
#[test]
fn a_read_deny_below_the_workspace_refuses_only_what_it_names() {
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
        ("finance", "ledger", false),
        ("finance", "summary", true),
    ] {
        assert_eq!(
            peer.check_document_read(source, app, record, now).is_ok(),
            readable,
            "{app}/{record}"
        );
    }
}
/// A participant withholds a record's channel from exactly the member devices
/// that may not read the record, by the same decision
/// as [`StatePeer::check_document_read`]: a Read Deny naming one record
/// withholds that record's channel alone, a Deny over an app withholds every
/// record of it a device proved the route of, a record reopened by a more
/// specific Allow is withheld from nobody, and a channel whose record nobody
/// named is withheld from whoever the app's Deny keeps out, and no one else.
#[test]
fn a_read_deny_on_a_record_withholds_only_that_records_channel() {
    let (mut peer, source, now) = fixture();
    let person = hex::encode(row(&peer, &source).device_cert.user_public_key);
    put_member(&mut peer, &person, Role::Viewer);
    let workspace_id = peer.workspace_id().to_owned();
    // Every member device of the fixture is this person's: the Deny names
    // the person, so it withholds the record from all of them.
    let devices: std::collections::BTreeSet<[u8; 32]> = peer
        .governance()
        .governed_access(now)
        .unwrap()
        .member_devices
        .into_iter()
        .collect();
    assert!(devices.contains(&source));
    let open = peer.governance().withheld_record_readers(now, &[]).unwrap();
    assert!(open.withheld.is_empty());
    // A channel nobody named may be anyone's own record: no one reads it.
    assert_eq!(open.unrouted, devices);
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
    let routes = [
        collab_types::ChannelRoute::new("finance", "q3"),
        collab_types::ChannelRoute::new("finance", "summary"),
        collab_types::ChannelRoute::new("app", "notes"),
    ];
    for known in [&[][..], &routes[..]] {
        let readers = peer
            .governance()
            .withheld_record_readers(now, known)
            .unwrap();
        let payroll = collab_types::document_channel_id("app", &workspace_id, "payroll");
        assert_eq!(readers.withheld.get(&payroll), Some(&devices));
        assert!(
            peer.check_document_read(source, "app", "payroll", now)
                .is_err()
        );
        assert_eq!(readers.unrouted, devices);
        let q3 = collab_types::document_channel_id("finance", &workspace_id, "q3");
        assert!(
            peer.check_document_read(source, "finance", "q3", now)
                .is_err()
        );
        assert_eq!(
            readers.withheld.get(&q3),
            (!known.is_empty()).then_some(&devices)
        );
        for (app, record) in [("app", "notes"), ("finance", "summary")] {
            assert!(peer.check_document_read(source, app, record, now).is_ok());
            assert!(
                readers
                    .withheld
                    .get(&collab_types::document_channel_id(
                        app,
                        &workspace_id,
                        record
                    ))
                    .is_none_or(std::collections::BTreeSet::is_empty),
                "{app}/{record}"
            );
        }
        for (channel, withheld) in &readers.withheld {
            assert!(*channel == payroll || *channel == q3 || withheld.is_empty());
        }
    }
}

/// A member's own record in the workspace is theirs alone: another member,
/// even the workspace's owner, neither reads nor changes it, and a steward
/// withholds its channel from every device but that person's, while the
/// person reads and changes their own whatever their role.
#[test]
fn another_member_neither_reads_nor_writes_a_members_own_record() {
    let (mut peer, source, now) = fixture();
    let person = hex::encode(row(&peer, &source).device_cert.user_public_key);
    let workspace = peer.core.workspace_doc.as_mut().unwrap();
    workspace.put(ROOT, "owner", person.as_str()).unwrap();
    workspace.commit();
    let workspace_id = peer.workspace_id().to_owned();
    let theirs = collab_types::member_record_id(&"ad".repeat(32), "chat.drafts");
    let own = collab_types::member_record_id(&person, "chat.drafts");
    let member = peer.permissions(source, now).unwrap();
    assert_eq!(
        member.role,
        Role::Owner,
        "the owner, who reads everything else"
    );
    for (record, theirs_alone) in [(&theirs, false), (&own, true)] {
        let node = collab_types::document_node(&workspace_id, "so.tilo.chat", record);
        assert_eq!(
            peer.check_document_read(source, "so.tilo.chat", record, now)
                .is_ok(),
            theirs_alone,
            "{record}"
        );
        assert_eq!(
            member.allows(&node, ActionKind::Undoable, now),
            theirs_alone,
            "{record}"
        );
    }
    assert!(
        peer.check_document_read(source, "so.tilo.chat", "tab:0000000000000001", now)
            .is_ok()
    );

    let devices: std::collections::BTreeSet<[u8; 32]> = peer
        .governance()
        .governed_access(now)
        .unwrap()
        .member_devices
        .into_iter()
        .collect();
    let routes = [
        collab_types::ChannelRoute::new("so.tilo.chat", theirs.as_str()),
        collab_types::ChannelRoute::new("so.tilo.chat", own.as_str()),
    ];
    let readers = peer
        .governance()
        .withheld_record_readers(now, &routes)
        .unwrap();
    assert_eq!(
        readers.withheld.get(&collab_types::document_channel_id(
            "so.tilo.chat",
            &workspace_id,
            &theirs
        )),
        Some(&devices)
    );
    assert!(
        readers
            .withheld
            .get(&collab_types::document_channel_id(
                "so.tilo.chat",
                &workspace_id,
                &own
            ))
            .is_some_and(std::collections::BTreeSet::is_empty)
    );

    // Their role does not decide their own record: a viewer still writes it.
    let workspace = peer.core.workspace_doc.as_mut().unwrap();
    workspace
        .put(ROOT, "owner", hex::encode(owner().1.user_public_key))
        .unwrap();
    workspace.commit();
    put_member(&mut peer, &person, Role::Viewer);
    let viewer = peer.permissions(source, now).unwrap();
    assert_eq!(viewer.role, Role::Viewer);
    let own_node = collab_types::document_node(&workspace_id, "so.tilo.chat", &own);
    assert!(viewer.allows(&own_node, ActionKind::Undoable, now));
}

/// A channel's id does not say whose own record it is, so until a device
/// names the record no member is admitted to it: another member asking
/// without the route is refused, and once named it is the person's alone.
#[test]
fn a_members_own_channel_admits_no_one_until_its_record_is_named() {
    let (mut peer, source, now) = fixture();
    let person = hex::encode(row(&peer, &source).device_cert.user_public_key);
    put_member(&mut peer, &person, Role::Viewer);
    let workspace_id = peer.workspace_id().to_owned();
    let theirs = collab_types::member_record_id(&"ad".repeat(32), "chat.drafts");
    let channel = collab_types::document_channel_id("so.tilo.chat", &workspace_id, &theirs);
    let readers = |route: Option<&collab_types::ChannelRoute>| {
        peer.governance()
            .channel_readers(now, channel, route)
            .unwrap()
    };
    assert!(
        readers(None).is_none(),
        "another member asking without naming the record is refused"
    );
    let named = collab_types::ChannelRoute::new("so.tilo.chat", theirs.as_str());
    assert!(!readers(Some(&named)).unwrap().contains(&source));
    // An ordinary record is everyone's once named.
    let notes = collab_types::ChannelRoute::new("so.tilo.chat", "tab:0000000000000001");
    let notes_channel = notes.channel(&workspace_id);
    assert!(
        peer.governance()
            .channel_readers(now, notes_channel, Some(&notes))
            .unwrap()
            .unwrap()
            .contains(&source)
    );
}

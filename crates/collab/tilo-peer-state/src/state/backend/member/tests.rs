use super::*;
use crate::StatePeer;
use crate::state::err;
use crate::state::now_ns;
use crate::verified_certificate::VerifiedCertificate;
use automerge::ObjType;
use automerge::ROOT;
use automerge::ReadDoc;
use automerge::transaction::Transactable;
use collab_types::Ed25519PublicKey;
use collab_types::caps::Principal;
use ed25519_dalek::SigningKey;
use tilo_action_policy::{
    ActionKind, Answer, GrantId, KindSet, Lifetime, Subject, TreePath, roles,
};
use tilo_automerge::ApplyIncoming;
use web_time::Duration;

fn put_member(peer: &mut StatePeer, person: &str, role: Role) {
    let workspace = peer.core.workspace_doc.as_mut().unwrap();
    let members = workspace.get(ROOT, "members").unwrap().unwrap().1;
    workspace
        .put(
            &members,
            person,
            automerge::ScalarValue::Bytes(serde_json::to_vec(&role).unwrap()),
        )
        .unwrap();
    workspace.commit();
}
fn put_grant(peer: &mut StatePeer, grant: &Grant) {
    let workspace = peer.core.workspace_doc.as_mut().unwrap();
    let grants = workspace.get(ROOT, "grants").unwrap().unwrap().1;
    workspace
        .put(
            &grants,
            grant.id.as_str(),
            automerge::ScalarValue::Bytes(serde_json::to_vec(grant).unwrap()),
        )
        .unwrap();
    workspace.commit();
}
fn fixture() -> (StatePeer, [u8; 32], SystemTime) {
    let (mut peer, _, _, cert, now) = crate::admission_tests::seeded();
    let workspace = peer.core.workspace_doc.as_mut().unwrap();
    workspace.put(ROOT, "owner", "a different person").unwrap();
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
        .block(&expected.person, Subject::person("a different person"));
    put_grant(&mut peer, &block);
    assert!(peer.permissions(source, now).is_err());
}
// Literal pre-extraction role body, with the historical projection oracle.
fn original_role(peer: &StatePeer, source: &[u8; 32], now: SystemTime) -> Result<Role, String> {
    let cert = peer
        .certificate(source, now)
        .map_err(|error| error.to_string())?;
    let workspace = peer
        .core
        .workspace_doc
        .as_ref()
        .ok_or("workspace governance missing")?;
    let (_, workspace_policy) = crate::workspace::extract_governed_fields(workspace);
    let identities = crate::workspace::governance_of(Some(workspace)).identities;
    let policy = crate::cached_document::PolicyData {
        owner: workspace_policy.owner.clone(),
        members: workspace_policy.members.clone(),
        joined: workspace_policy.joined.clone(),
        grants: workspace_policy.grants.clone(),
        identities,
        damaged: crate::workspace::policy::Damaged::default(),
    };
    // A root-signed certificate must also be the certificate admitted by workspace governance.
    let identity: GovernedIdentity = serde_json::from_slice(
        policy
            .identities
            .get(&hex::encode(source))
            .ok_or("device is absent from governed roster")?,
    )
    .map_err(|error| {
        crate::state::MembershipError::MalformedIdentity(std::sync::Arc::new(error)).to_string()
    })?;
    if identity.device_cert != *cert {
        return Err("governed certificate mismatch".into());
    }
    let mut revoked = peer.core.revocations.clone();
    for entry in identity.revocations {
        if entry.user_public_key != cert.user_public_key {
            return Err("revocation root mismatch".into());
        }
        revoked.insert(entry).map_err(err)?;
    }
    if revoked.is_revoked(&cert.user_public_key, source) {
        return Err("device revoked by workspace".into());
    }
    let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(cert.user_public_key));
    workspace_policy
        .active_role(person.id())
        .ok_or_else(|| "person has no workspace membership".into())
}

fn compare_native(peer: &StatePeer, source: [u8; 32], now: SystemTime) {
    assert_eq!(peer.role(&source, now), original_role(peer, &source, now));
}
#[test]
fn current_member_preserves_malformed_container_and_conflict_winner_semantics() {
    for kind in [ObjType::List, ObjType::Text, ObjType::Map] {
        let (mut peer, source, now) = fixture();
        let owner = hex::encode(peer.owner());
        let workspace = peer.core.workspace_doc.as_mut().unwrap();
        let object = workspace.put_object(ROOT, "members", kind).unwrap();
        if kind == ObjType::List {
            workspace.insert(&object, 0, "not a map entry").unwrap();
        } else if kind == ObjType::Text {
            workspace
                .splice_text(&object, 0, 0, "not a map entry")
                .unwrap();
        } else {
            workspace
                .put(
                    &object,
                    "invalid",
                    automerge::ScalarValue::Bytes(b"not json".to_vec()),
                )
                .unwrap();
            workspace
                .put(
                    &object,
                    owner.as_str(),
                    automerge::ScalarValue::Bytes(serde_json::to_vec(&Role::Viewer).unwrap()),
                )
                .unwrap();
        }
        workspace.commit();
        compare_native(&peer, source, now);
    }
    let (mut peer, source, now) = fixture();
    peer.core
        .workspace_doc
        .as_mut()
        .unwrap()
        .put(ROOT, "members", false)
        .unwrap();
    compare_native(&peer, source, now);
    for map_wins in [false, true] {
        let (mut peer, source, now) = fixture();
        let doc = peer.admitted_workspace().unwrap();
        let mut valid = doc
            .clone()
            .with_actor(automerge::ActorId::from(vec![if map_wins { 3 } else { 1 }]));
        let mut invalid = doc.clone().with_actor(automerge::ActorId::from(vec![2]));
        let members = valid.put_object(ROOT, "members", ObjType::Map).unwrap();
        valid
            .put(
                &members,
                hex::encode(peer.owner()),
                automerge::ScalarValue::Bytes(serde_json::to_vec(&Role::Commenter).unwrap()),
            )
            .unwrap();
        valid.commit();
        invalid.put_object(ROOT, "members", ObjType::List).unwrap();
        invalid.commit();
        valid.merge_incoming(&mut invalid).unwrap();
        assert_eq!(valid.get_all(ROOT, "members").unwrap().len(), 2);
        peer.core.workspace_doc = Some(valid.into());
        compare_native(&peer, source, now);
    }
}
#[test]
fn current_member_copies_all_grant_fields() {
    let (mut peer, source, now) = fixture();
    let workspace = peer.workspace_id().to_owned();
    let long = "owned grant string ".repeat(32);
    let subjects = [
        Subject::Role(Role::Viewer),
        Subject::Person(long.clone()),
        Subject::App(long.clone()),
        Subject::Agent(long.clone()),
        Subject::Task(long.clone()),
        Subject::Shortcut(long.clone()),
        Subject::ForeignDevice,
    ];
    let nodes = [
        TreePath::workspace(&workspace),
        TreePath::workspace(&workspace).app(long.clone()),
        TreePath::workspace(&workspace)
            .app(long.clone())
            .kind(long.clone())
            .resource(long.clone())
            .action(long.clone()),
        TreePath::workspace(&workspace).rest().field(&long),
        TreePath::workspace(&workspace).any(tilo_action_policy::Level::App),
        roles::workspace_resource(&workspace),
    ];
    let lifetimes = [Lifetime::UntilRevoked, Lifetime::Until(u64::MAX)];
    for (i, node) in nodes.into_iter().enumerate() {
        let grant = Grant {
            id: GrantId::new(format!("extra{i:02}{long}")),
            subject: subjects[i % subjects.len()].clone(),
            node,
            kinds: KindSet::READ_UNDOABLE,
            answer: Answer::Ask,
            lifetime: lifetimes[i % lifetimes.len()].clone(),
            granted_by: Subject::Person(long.clone()),
        };
        put_grant(&mut peer, &grant);
    }
    compare_native(&peer, source, now);
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
    serde_json::from_slice(raw.as_bytes().unwrap()).unwrap()
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
fn shared_current_member_retains_original_certificate_json_and_revocation_error_order() {
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
    replace_row(
        &mut mismatch,
        &source,
        serde_json::to_vec(&identity).unwrap(),
    );
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
        replace_row(
            &mut candidate,
            &source,
            serde_json::to_vec(&identity).unwrap(),
        );
        cases.push(candidate);
    }
    let mut missing = base;
    missing.core.workspace_doc = None;
    cases.push(missing);
    for peer in cases {
        assert_eq!(peer.role(&source, now), original_role(&peer, &source, now));
        assert_eq!(
            peer.role(&[66; 32], now),
            original_role(&peer, &[66; 32], now)
        );
        assert_eq!(
            peer.role(&source, SystemTime::UNIX_EPOCH),
            original_role(&peer, &source, SystemTime::UNIX_EPOCH)
        );
    }
}

#[test]
fn current_member_preserves_restriction_role_semantics_and_current_certificate_expiry() {
    let (mut restricted, source, now) = fixture();
    let person = hex::encode(row(&restricted, &source).device_cert.user_public_key);
    // A deny on one app narrows what the member may do there; it is no block.
    let deny = Grant::new(
        "denial",
        Subject::person(person.as_str()),
        TreePath::workspace(restricted.workspace_id()).app("so.tilo.notes"),
        KindSet::ALL,
        Answer::Deny,
        Subject::person("a different person"),
    );
    put_grant(&mut restricted, &deny);
    assert_eq!(restricted.role(&source, now).unwrap(), Role::Editor);
    compare_native(&restricted, source, now);
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
    replace_row(
        &mut expiring,
        &source,
        serde_json::to_vec(&identity).unwrap(),
    );
    compare_native(&expiring, source, now);
    compare_native(&expiring, source, now + Duration::from_nanos(1));
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
    replace_row(peer, source, serde_json::to_vec(&identity).unwrap());
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
            Subject::ForeignDevice,
            TreePath::workspace(&workspace_id),
            KindSet::UNDOABLE.union(KindSet::OUTSIDE),
            Answer::Deny,
            Subject::person(person.as_str()),
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
            Subject::person(person.as_str()),
            node,
            KindSet::READ,
            answer,
            Subject::person("a different person"),
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
            Subject::person(person.as_str()),
            node,
            KindSet::READ,
            answer,
            Subject::person("a different person"),
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
        .put(ROOT, "owner", "b0".repeat(32).as_str())
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

/// A person shut out by a row of their own that did not read back is told
/// their access could not be read, not that they are a stranger here.
#[test]
fn a_damaged_row_says_the_access_could_not_be_read() {
    use super::absence;
    use crate::state::MembershipError;
    use crate::workspace::policy::Damaged;

    let damaged = Damaged {
        grants: 1,
        ..Damaged::default()
    };
    assert!(
        matches!(absence(&damaged, "them"), MembershipError::UnreadableAccess),
        "an unreadable grant could have been theirs"
    );
    let mut damaged = Damaged::default();
    damaged.members.insert("them".to_owned());
    assert!(
        matches!(absence(&damaged, "them"), MembershipError::UnreadableAccess),
        "their own member row did not read back"
    );
    assert!(
        matches!(
            absence(&damaged, "someone else"),
            MembershipError::NoMembership
        ),
        "someone else's rows read back, so they hold no role"
    );
    assert!(matches!(
        absence(&Damaged::default(), "them"),
        MembershipError::NoMembership
    ));
}

//! Governance is decoded once per workspace document state, not once per message.
use super::*;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn device(root: &SigningKey, seed: u8) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{seed}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// The governor `a` holding `records` seeded documents, and a fresh member
/// replica with the same pinned authority.
fn governor_and_replica(records: usize) -> (StatePeer, StatePeer, SigningKey, SystemTime) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap();
    let (a, ac) = device(&root, 2);
    let (_, bc) = device(&root, 3);
    let roster = vec![ac.clone(), bc];
    let mut governor = StatePeer::new(
        WORKSPACE,
        ac.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    let documents = (0..records)
        .map(|index| {
            let mut document = AutoCommit::new();
            document
                .put(ROOT, "value", i64::try_from(index).unwrap())
                .unwrap();
            document.commit();
            SeedDocument {
                app_id: "test.app".into(),
                record_id: format!("tab:{index}"),
                kind: "test".into(),
                bytes: document.save(),
            }
        })
        .collect();
    governor
        .install_seed(
            SeedState {
                workspace_changes: seed_workspace(WORKSPACE, &ac, &roster).unwrap(),
                documents,
            },
            &a,
            now,
        )
        .unwrap();
    let replica = StatePeer::new(WORKSPACE, ac, roster, RevocationList::new(), now).unwrap();
    (governor, replica, a, now)
}

#[test]
fn authorize_document_decodes_capabilities_at_most_once_per_document_state() {
    const MESSAGES: usize = 10;
    let (mut governor, mut replica, a, now) = governor_and_replica(MESSAGES);
    let source = a.verifying_key().to_bytes();
    let messages = governor
        .publication_mut()
        .signed_snapshots(&a, now)
        .unwrap();
    assert_eq!(messages.len(), MESSAGES);
    replica
        .receive(source, governor.workspace_message(), now)
        .unwrap();
    // Counted without touching the cache under test.
    let identities = workspace::governance_of(replica.core.workspace_doc.as_deref())
        .identities
        .len();
    assert_eq!(identities, 2, "both roster devices are governed identities");
    crate::workspace::POLICY_READS.with(|count| count.set(0));
    crate::cached_document::IDENTITY_DECODES.with(|count| count.set(0));
    for message in messages {
        // The production admission path: role lookup, then capability decision.
        let outcome = replica.receive(source, message, now).unwrap();
        assert!(outcome.changed && outcome.rejected == 0);
    }
    assert_eq!(replica.rows(), governor.rows());
    // Taking in the governance may have read it already.
    assert!(
        crate::workspace::POLICY_READS.with(std::cell::Cell::get) <= 1,
        "capabilities are decoded once for the governance state, not per message"
    );
    assert!(
        crate::cached_document::IDENTITY_DECODES.with(std::cell::Cell::get) <= identities,
        "each identity row is decoded once for the governance state, not per message"
    );
}

/// The governor writes `receipts` into its workspace document, as an owner commit
/// does, and returns the workspace message that carries them.
fn publish_receipts(governor: &mut StatePeer, receipts: &[(&str, &[u8])]) -> NeighbourMsg {
    let workspace = governor.core.workspace_doc.as_deref_mut().unwrap();
    let ledger = match workspace.get(ROOT, "effect_receipts_v1").unwrap() {
        Some((_, object)) => object,
        None => workspace
            .put_object(ROOT, "effect_receipts_v1", ObjType::Map)
            .unwrap(),
    };
    // The governor's own slice: who recorded them is not what is measured.
    let object = match workspace.get(&ledger, "governor").unwrap() {
        Some((_, object)) => object,
        None => workspace
            .put_object(&ledger, "governor", ObjType::Map)
            .unwrap(),
    };
    for (key, bytes) in receipts {
        workspace
            .put(&object, *key, automerge::ScalarValue::Bytes(bytes.to_vec()))
            .unwrap();
    }
    workspace.commit();
    governor.workspace_message()
}

fn receipt_decodes() -> usize {
    crate::workspace::RECEIPT_DECODES.with(std::cell::Cell::get)
}

#[test]
fn sync_resp_decodes_each_receipt_once() {
    let (mut governor, mut replica, a, now) = governor_and_replica(1);
    let source = a.verifying_key().to_bytes();
    replica
        .receive(source, governor.workspace_message(), now)
        .unwrap();
    // Spaced JSON: a re-encoding would not reproduce these bytes.
    let first: Vec<(String, Vec<u8>)> = (0..5)
        .map(|n| {
            (
                format!("invocation-{n}"),
                format!(r#"{{ "outcome": "done", "n": {n} }}"#).into_bytes(),
            )
        })
        .collect();
    let rows: Vec<(&str, &[u8])> = first
        .iter()
        .map(|(key, bytes)| (key.as_str(), bytes.as_slice()))
        .collect();
    let message = publish_receipts(&mut governor, &rows);
    crate::workspace::RECEIPT_DECODES.with(|count| count.set(0));
    replica.receive(source, message, now).unwrap();
    assert_eq!(receipt_decodes(), first.len(), "each new receipt once");
    for (key, bytes) in &first {
        assert_eq!(
            replica.core.receipts.get(key),
            Some(bytes),
            "receipts are recorded as received, never re-encoded"
        );
    }

    // A later sync restating every admitted receipt decodes only the new one.
    let message = publish_receipts(&mut governor, &[("invocation-5", br#"{"outcome":"late"}"#)]);
    crate::workspace::RECEIPT_DECODES.with(|count| count.set(0));
    replica.receive(source, message, now).unwrap();
    assert_eq!(receipt_decodes(), 1, "only the new receipt is decoded");
    assert_eq!(replica.core.receipts.len(), first.len() + 1);

    // A ledger row an older version stored re-encoded is still the same
    // evidence: it is compared by value once and then kept as received.
    let legacy: serde_json::Value = serde_json::from_slice(&first[0].1).unwrap();
    replica
        .core
        .receipts
        .insert(first[0].0.clone(), serde_json::to_vec(&legacy).unwrap());
    let message = publish_receipts(
        &mut governor,
        &[("invocation-6", br#"{"outcome":"later"}"#)],
    );
    replica.receive(source, message, now).unwrap();
    assert_eq!(replica.core.receipts.get(&first[0].0), Some(&first[0].1));
    crate::workspace::RECEIPT_DECODES.with(|count| count.set(0));
    replica
        .receive(source, governor.workspace_message(), now)
        .unwrap();
    assert_eq!(receipt_decodes(), 0, "an unchanged sync decodes nothing");

    // Rewriting admitted evidence is refused, even by the governor.
    let rewrite = publish_receipts(
        &mut governor,
        &[("invocation-0", br#"{"outcome":"undone"}"#)],
    );
    let before = replica.core.receipts.clone();
    assert_eq!(
        replica.receive(source, rewrite, now).unwrap_err(),
        "effect receipt is immutable"
    );
    assert_eq!(replica.core.receipts, before);
}

/// A device certified by its own person's root, both keys derived from `n`.
fn person_device(n: usize) -> (SigningKey, DeviceCert) {
    let seed = |kind: u8| {
        let mut bytes = [kind; 32];
        bytes[..8].copy_from_slice(&u64::try_from(n).unwrap().to_le_bytes());
        bytes
    };
    let root = SigningKey::from_bytes(&seed(0x40));
    let key = SigningKey::from_bytes(&seed(0x80));
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{n}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

/// A workspace of `people` editors and its owner, whose grants keep the first
/// person from `named` records: the governed peer, and the people's devices.
fn crowded_workspace(people: usize, named: usize, now: SystemTime) -> (StatePeer, Vec<DeviceCert>) {
    let (owner_key, owner) = person_device(people);
    let people: Vec<DeviceCert> = (0..people).map(|n| person_device(n).1).collect();
    let roster: Vec<DeviceCert> = std::iter::once(owner.clone())
        .chain(people.iter().cloned())
        .collect();
    let mut doc =
        crate::workspace::candidate(None, &seed_workspace(WORKSPACE, &owner, &roster).unwrap())
            .unwrap();
    let members = doc.get(ROOT, "members").unwrap().unwrap().1;
    for cert in &people {
        doc.put(
            &members,
            hex::encode(cert.user_public_key).as_str(),
            automerge::ScalarValue::Bytes(serde_json::to_vec(&Role::Editor).unwrap()),
        )
        .unwrap();
    }
    let hidden = hex::encode(people[0].user_public_key);
    let grants = doc.get(ROOT, "grants").unwrap().unwrap().1;
    for n in 0..named {
        let grant = Grant::new(
            format!("hidden-{n}"),
            Subject::person(hidden.as_str()),
            collab_types::document_node(WORKSPACE, "so.tilo.chat", &format!("tab:{n}")),
            tilo_action_policy::KindSet::READ,
            tilo_action_policy::Answer::Deny,
            Subject::person(hex::encode(owner.user_public_key)),
        );
        doc.put(
            &grants,
            grant.id.as_str(),
            automerge::ScalarValue::Bytes(serde_json::to_vec(&grant).unwrap()),
        )
        .unwrap();
    }
    let workspace_changes = doc
        .get_changes(&[])
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    let mut peer = StatePeer::new(WORKSPACE, owner, roster, RevocationList::new(), now).unwrap();
    peer.install_seed(
        SeedState {
            workspace_changes,
            documents: vec![],
        },
        &owner_key,
        now,
    )
    .unwrap();
    (peer, people)
}

/// A page in a workspace of hundreds of people, whose grants name many
/// records, asks who reads a channel for every frame the channel takes in
/// (thousands while it arrives). Each answer builds the workspace's policy a
/// fixed number of times and verifies no certificate again: before, it built
/// the policy once per member device per named record and verified every
/// member's certificate, which held a page's only thread for minutes before it
/// was admitted to such a workspace.
#[test]
fn a_channels_readers_cost_no_more_with_the_workspaces_people_and_named_records() {
    const PEOPLE: usize = 300;
    const FRAMES: usize = 10;
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap();
    // The first person may not read any of the 20 records the grants name.
    let (peer, people) = crowded_workspace(PEOPLE, 20, now);

    let named = collab_types::document_channel_id("so.tilo.chat", WORKSPACE, "tab:0");
    let scope = collab_types::workspace_scope(WORKSPACE);
    let readers = |channel| {
        peer.governance()
            .channel_readers(now, channel, None)
            .unwrap()
            .unwrap()
    };
    let everyone = readers(scope);
    assert_eq!(
        everyone.len(),
        PEOPLE + 1,
        "every member device reads the workspace"
    );
    let record = readers(named);
    assert_eq!(
        record.len(),
        PEOPLE,
        "a Read Deny keeps one person's device out"
    );
    assert!(!record.contains(&people[0].device_public_key));
    // A person's own record is read by that person's device alone.
    let drafts = collab_types::ChannelRoute::new(
        "so.tilo.chat",
        collab_types::member_record_id(&hex::encode(people[1].user_public_key), "chat.drafts")
            .as_str(),
    );
    let own = || {
        peer.governance()
            .channel_readers(now, drafts.channel(WORKSPACE), Some(&drafts))
            .unwrap()
            .unwrap()
    };
    assert_eq!(own(), BTreeSet::from([people[1].device_public_key]));

    crate::cached_document::WORKSPACE_POLICIES.with(|count| count.set(0));
    crate::state::CERTIFICATE_VERIFICATIONS.with(|count| count.set(0));
    for _ in 0..FRAMES {
        assert_eq!(readers(named), record);
        assert_eq!(readers(scope), everyone);
        assert_eq!(own().len(), 1);
    }
    assert!(
        crate::cached_document::WORKSPACE_POLICIES.with(std::cell::Cell::get) <= 5 * FRAMES,
        "the policy is built a fixed number of times per question, not per device or record"
    );
    assert_eq!(
        crate::state::CERTIFICATE_VERIFICATIONS.with(std::cell::Cell::get),
        0,
        "each member's certificate is verified once for the governance state"
    );
}

/// What a newcomer's page does with a workspace's governance before it shows
/// the desktop: take in the governance its host replays, list the members and
/// their devices, decide who reads the workspace's channels, and keep the
/// replica with its security basis. Each row is decoded and its certificate
/// checked once for all of these, and the whole-governance reads and replays
/// are as many among 300 people as among 30.
#[test]
fn a_newcomer_reads_each_members_identity_once_however_many_people() {
    let work = |people: usize| {
        let now = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(2000))
            .unwrap();
        let (governor, _) = crowded_workspace(people, 0, now);
        let owner = governor
            .core
            .certificates
            .values()
            .find(|cert| cert.user_public_key == governor.core.owner)
            .unwrap()
            .as_ref()
            .clone();
        let (_, newcomer) = person_device(people + 1);
        let mut page = StatePeer::from_join(
            WORKSPACE,
            owner.clone(),
            vec![owner.clone(), newcomer],
            &[],
            now,
        )
        .unwrap();
        let message = governor.workspace_message();
        let _ = crate::governance_work::take();
        page.receive_from_report(
            TransportOrigin::RelayDelta(owner.device_public_key),
            message,
            now,
        )
        .unwrap();
        page.governance().governed_members(now).unwrap();
        let access = page.governance().governed_access(now).unwrap();
        assert_eq!(
            access.member_devices.len(),
            people + 1,
            "every member's device"
        );
        page.governance()
            .channel_readers(now, collab_types::workspace_scope(WORKSPACE), None)
            .unwrap();
        page.security_basis().unwrap().unwrap();
        page.durable().unwrap();
        crate::governance_work::take()
    };
    let (few, many) = (work(30), work(300));
    for (people, work) in [(30_u64, few), (300, many)] {
        assert_eq!(
            work.identities_decoded,
            people + 1,
            "{people} people: each identity row is decoded once: {work:?}"
        );
        assert!(
            work.signatures_verified <= people + 2,
            "{people} people: each member's certificate is checked once: {work:?}"
        );
    }
    assert_eq!(
        (few.governance_reads, few.replays),
        (many.governance_reads, many.replays),
        "the governance is read as often among 300 people as among 30"
    );
}

use super::*;
use collab_types::caps::{CapOverride, Expiry, Scope, Subject, Verb};
use qos_peer_state::{NeighbourMsg, StatePeer, TransportOrigin};
use ui::automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
mod oracle;
fn category(error: anyhow::Error) -> PlanningFailure {
    error
        .downcast_ref::<PlanningFailure>()
        .copied()
        .unwrap_or(PlanningFailure::Unavailable)
}
fn original(
    peer: &StatePeer,
    workspace: u64,
    host: &DeviceCert,
    session: &VerifiedSession,
    now: SystemTime,
) -> Result<Digest> {
    oracle::inspect(peer, workspace, session.device_certificate(), host, now).map_err(category)
}
fn compare(
    peer: &StatePeer,
    workspace: u64,
    host: &DeviceCert,
    session: &VerifiedSession,
    now: SystemTime,
) -> Result<Digest> {
    let expected = original(peer, workspace, host, session, now);
    let got = DeliveryPolicyBasis::inspect_unqualified(peer, workspace, host, session, now)
        .map(|v| v.policy_digest());
    assert_eq!(got, expected);
    got
}
fn document(peer: &mut StatePeer, f: &Fixture, now: SystemTime) {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "text", "real planner record").unwrap();
    peer.publish_document(
        "test.planner",
        "record:1",
        doc.save(),
        &f.owner.identity.native,
        now,
    )
    .unwrap();
    assert!(!peer.rows().is_empty());
}
fn capability(peer: &mut StatePeer, f: &Fixture, bytes: Vec<u8>, now: SystemTime) {
    let mut room = peer.admitted_room().unwrap().clone();
    let caps = room.get(ROOT, "capabilities").unwrap().unwrap().1;
    room.put(&caps, "planner-fixture", bytes).unwrap();
    let changes = room
        .get_changes(&[])
        .iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    peer.receive_from(
        TransportOrigin::RelayDelta(f.owner.identity.native_cert.device_public_key),
        NeighbourMsg::SyncResp {
            room: peer.room().into(),
            changes,
        },
        now,
    )
    .unwrap();
}
#[test]
fn planner_policy_matches_literal_original_with_actual_distinct_host_and_device() {
    let (f, session) = fixture();
    let now = SystemTime::now();
    let peer = f.owner.peer();
    let host = f.owner.host_certificate();
    let workspace = f.owner.workspace_id();
    assert_ne!(
        host.device_public_key,
        session.device_certificate().device_public_key
    );
    assert_ne!(host.device_label, session.device_certificate().device_label);
    let digest = compare(peer, workspace, host, &session, now).unwrap();
    let swapped =
        oracle::inspect(peer, workspace, host, session.device_certificate(), now).unwrap();
    assert_ne!(
        digest, swapped,
        "legacy inspect is device then host, shared entry is host then device"
    );
    let mut source = peer.clone();
    document(&mut source, &f, now);
    for expiry in [Expiry::AfterUse, Expiry::EndOfSession] {
        let cap = CapOverride::grant(
            Subject::Role(collab_types::Role::Editor),
            Scope::Workspace,
            Verb::Read,
        )
        .expiring(expiry);
        capability(&mut source, &f, serde_json::to_vec(&cap).unwrap(), now);
        assert!(compare(&source, workspace, host, &session, now).is_ok());
    }
}
#[test]
fn planner_policy_preserves_actual_error_categories_and_policy_before_denial() {
    let (f, session) = fixture();
    let (other, _) = fixture();
    let now = SystemTime::now();
    let host = f.owner.host_certificate();
    let workspace = f.owner.workspace_id();
    let mut unrelated = other.owner.peer().clone();
    document(&mut unrelated, &other, now);
    assert_eq!(
        compare(&unrelated, workspace, host, &session, now),
        Err(PlanningFailure::CurrentDenied)
    );
    for (bytes, expected) in [
        (b"not-json".to_vec(), PlanningFailure::Unavailable),
        (
            serde_json::to_vec(&CapOverride::grant(
                Subject::Role(collab_types::Role::Editor),
                Scope::App("unsupported".into()),
                Verb::Read,
            ))
            .unwrap(),
            PlanningFailure::UnsupportedPolicy,
        ),
        (
            vec![b' '; 1024 * 1024 + 1],
            PlanningFailure::Capacity(PlanningCapacity::Bytes),
        ),
    ] {
        let mut peer = unrelated.clone();
        capability(&mut peer, &other, bytes, now);
        assert_eq!(
            compare(&peer, workspace, host, &session, now),
            Err(expected)
        );
    }
    let missing = StatePeer::new(
        f.owner.peer().room(),
        f.owner.peer().authority().clone(),
        f.owner.peer().certificates(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert_eq!(
        compare(&missing, workspace, host, &session, now),
        Err(PlanningFailure::Unavailable)
    );
}
#[test]
fn planner_policy_preserves_exact_final_mebibyte_limit_and_denial_before_final_count() {
    let (f, session) = fixture();
    let now = SystemTime::now();
    let workspace = f.owner.workspace_id();
    let peer = f.owner.peer();
    let mut host = f.owner.host_certificate().clone();
    let old = host.device_label.len();
    assert!(old < 128);
    let size = oracle::encoded(peer, workspace, session.device_certificate(), &host, now)
        .unwrap()
        .len();
    let length = 1024 * 1024 - (size - old) - 2;
    host.device_label = "h".repeat(length);
    tilo_user_identity::sign_device_cert(&mut host, &f.owner.identity.root).unwrap();
    assert_eq!(
        oracle::encoded(peer, workspace, session.device_certificate(), &host, now)
            .unwrap()
            .len(),
        1024 * 1024
    );
    assert!(compare(peer, workspace, &host, &session, now).is_ok());
    host.device_label.push('h');
    tilo_user_identity::sign_device_cert(&mut host, &f.owner.identity.root).unwrap();
    assert_eq!(
        compare(peer, workspace, &host, &session, now),
        Err(PlanningFailure::Capacity(PlanningCapacity::Bytes))
    );
    let (other, _) = fixture();
    let now = SystemTime::now();
    let mut unrelated = other.owner.peer().clone();
    document(&mut unrelated, &other, now);
    assert_eq!(
        compare(&unrelated, workspace, &host, &session, now),
        Err(PlanningFailure::CurrentDenied)
    );
}

use super::*;
use crate::mesh::MeshAction;
use tilo_user_identity::machine_mesh::{BROKER_ACTIONS_SCOPE, MeshRequest, sign_request};
fn linked() -> (tempfile::TempDir, tempfile::TempDir, MeshRequest) {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let ra = SigningKey::from_bytes(&[7; 32]);
    let rb = SigningKey::from_bytes(&[9; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut sa = Store::open(a.path(), "a").unwrap();
    let mut sb = Store::open(b.path(), "b").unwrap();
    for (store, root) in [(&mut sa, &ra), (&mut sb, &rb)] {
        let r = super::tests::approved(store.inspect(10).unwrap(), root, &device);
        store.apply(&r, 11).unwrap();
    }
    let mut request = MeshRequest {
        version: 1,
        kind: "link".into(),
        link_id: "link-1".into(),
        source: sa.mesh_inspect(20).unwrap(),
        target: sb.mesh_inspect(20).unwrap(),
        allowed_actions: vec![BROKER_ACTIONS_SCOPE.into()],
        source_signature: vec![],
        target_signature: vec![],
    };
    sign_request(&mut request, &ra, &rb).unwrap();
    (a, b, request)
}

/// Same user root on both machines so the source host can also be admitted as a
/// member on the target and exercise ordinary broker permissions over the link.
fn linked_same_root() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    MeshRequest,
    SigningKey,
) {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device_a = SigningKey::from_bytes(&[8; 32]);
    let device_b = SigningKey::from_bytes(&[10; 32]);
    let mut sa = Store::open(a.path(), "a").unwrap();
    let mut sb = Store::open(b.path(), "b").unwrap();
    let enroll_a = super::tests::approved(sa.inspect(10).unwrap(), &root, &device_a);
    sa.apply(&enroll_a, 11).unwrap();
    let enroll_b = super::tests::approved(sb.inspect(10).unwrap(), &root, &device_b);
    sb.apply(&enroll_b, 11).unwrap();
    // Source host key is admitted on the target with ordinary member rights.
    let source_host = sa.key();
    let admit_source = super::tests::approved(sb.inspect(12).unwrap(), &root, &source_host);
    sb.apply(&admit_source, 13).unwrap();
    let mut request = MeshRequest {
        version: 1,
        kind: "link".into(),
        link_id: "link-1".into(),
        source: sa.mesh_inspect(20).unwrap(),
        target: sb.mesh_inspect(20).unwrap(),
        allowed_actions: vec![BROKER_ACTIONS_SCOPE.into()],
        source_signature: vec![],
        target_signature: vec![],
    };
    sign_request(&mut request, &root, &root).unwrap();
    (a, b, request, root)
}

#[test]
fn mesh_bilateral_replay_substitution_and_durable_unlink() {
    let (a, b, r) = linked();
    let mut sa = Store::open(a.path(), "a").unwrap();
    let mut sb = Store::open(b.path(), "b").unwrap();
    let before = sa.state.bound.as_ref().unwrap().peer.clone();
    let room = sa.state.room.clone();
    let mut bad = r.clone();
    bad.target.machine_id = "c".into();
    assert!(sa.mesh_apply(&bad, 21).is_err());
    bad = r.clone();
    bad.target_signature = r.source_signature.clone();
    assert!(sa.mesh_apply(&bad, 21).is_err());
    sa.mesh_apply(&r, 21).unwrap();
    sb.mesh_apply(&r, 21).unwrap();
    sa.mesh_apply(&r, 22).unwrap();
    assert_eq!(sa.state.bound.as_ref().unwrap().peer, before);
    assert_eq!(sa.state.room, room);
    drop(sa);
    let mut sa = Store::open(a.path(), "a").unwrap();
    assert_eq!(
        sa.mesh_status(22).unwrap()["links"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        sa.mesh_status(22).unwrap()["capabilities"]["remote_actions"],
        serde_json::json!([BROKER_ACTIONS_SCOPE])
    );
    let mut unlink = r.clone();
    unlink.kind = "unlink".into();
    unlink.source = sa.mesh_inspect(30).unwrap();
    unlink.target = sb.mesh_inspect(30).unwrap();
    sign_request(
        &mut unlink,
        &SigningKey::from_bytes(&[7; 32]),
        &SigningKey::from_bytes(&[9; 32]),
    )
    .unwrap();
    sa.mesh_apply(&unlink, 31).unwrap();
    drop(sa);
    let mut sa = Store::open(a.path(), "a").unwrap();
    assert_eq!(sa.mesh_status(32).unwrap()["links"][0]["active"], false);
    assert_eq!(
        sa.mesh_status(32).unwrap()["capabilities"]["remote_actions"],
        serde_json::json!([])
    );
    let persisted = std::fs::read(a.path().join("state.json")).unwrap();
    let mut repeated = unlink.clone();
    repeated.source.nonce = [99; 32];
    sign_request(
        &mut repeated,
        &SigningKey::from_bytes(&[7; 32]),
        &SigningKey::from_bytes(&[9; 32]),
    )
    .unwrap();
    sa.mesh_apply(&repeated, 32).unwrap();
    assert_eq!(
        persisted,
        std::fs::read(a.path().join("state.json")).unwrap()
    );

    assert!(sa.mesh_apply(&r, 33).is_err());
    assert_eq!(sa.mesh_status(33).unwrap()["links"][0]["active"], false);
}
#[test]
fn mesh_rejects_wrong_roots_self_links_expiry_and_supports_refreshed_retry() {
    let (a, b, r) = linked();
    let mut sa = Store::open(a.path(), "a").unwrap();
    let mut sb = Store::open(b.path(), "b").unwrap();
    let ra = SigningKey::from_bytes(&[7; 32]);
    let rb = SigningKey::from_bytes(&[9; 32]);
    let mut wrong = r.clone();
    wrong.target = wrong.source.clone();
    sign_request(&mut wrong, &ra, &ra).unwrap();
    assert!(sa.mesh_apply(&wrong, 21).is_err());
    wrong = r.clone();
    wrong.source.root_public_key = rb.verifying_key().to_bytes();
    assert!(sign_request(&mut wrong, &ra, &rb).is_err());
    wrong = r.clone();
    wrong.allowed_actions.push("writer_insert".into());
    sign_request(&mut wrong, &ra, &rb).unwrap();
    assert!(sa.mesh_apply(&wrong, 21).is_err());
    assert!(sa.mesh_apply(&r, 300_000_000_021).is_err());
    sa.mesh_apply(&r, 21).unwrap();
    let mut fresh = r.clone();
    fresh.source = sa.mesh_inspect(300_000_000_100).unwrap();
    fresh.target = sb.mesh_inspect(300_000_000_100).unwrap();
    sign_request(&mut fresh, &ra, &rb).unwrap();
    sa.mesh_apply(&fresh, 300_000_000_101).unwrap();
    sb.mesh_apply(&fresh, 300_000_000_101).unwrap();
    assert_eq!(
        sa.mesh_status(300_000_000_102).unwrap()["links"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn mesh_delegates_broker_action_with_source_permissions_idempotency_and_unlink() {
    let (a, b, r, root) = linked_same_root();
    let mut sa = Store::open(a.path(), "a").unwrap();
    let mut sb = Store::open(b.path(), "b").unwrap();
    sa.mesh_apply(&r, 21).unwrap();
    sb.mesh_apply(&r, 21).unwrap();
    let source_cert = r.source.host_certificate.clone();
    let call = MeshAction {
        version: 1,
        link_id: r.link_id.clone(),
        source_machine_id: "a".into(),
        target_machine_id: "b".into(),
        source_device_public_key: source_cert.device_public_key,
        request: ActionRequest {
            version: 1,
            room: format!("mesh:{}", r.link_id),
            invocation_id: "mesh-edit-1".into(),
            operation: NativeOperation::WriterInsert {
                text: "delegated over link".into(),
            },
        },
    };
    let before = sb.peer().unwrap().rows();
    let receipt = sb.mesh_execute(&source_cert, &call, 22).unwrap();
    assert_eq!(receipt.status, "completed");
    assert_ne!(before, sb.peer().unwrap().rows());
    let heads = sb.peer().unwrap().rows();
    assert_eq!(receipt, sb.mesh_execute(&source_cert, &call, 23).unwrap());
    assert_eq!(heads, sb.peer().unwrap().rows());

    let mut unlink = r.clone();
    unlink.kind = "unlink".into();
    unlink.source = sa.mesh_inspect(30).unwrap();
    unlink.target = sb.mesh_inspect(30).unwrap();
    sign_request(&mut unlink, &root, &root).unwrap();
    sb.mesh_apply(&unlink, 31).unwrap();
    let error = sb
        .mesh_execute(&source_cert, &call, 32)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("revoked") || error.contains("missing"),
        "{error}"
    );
}

use super::*;
use tilo_user_identity::machine_mesh::{MeshRequest, sign_request};
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
        allowed_actions: vec!["model_complete".into()],
        source_signature: vec![],
        target_signature: vec![],
    };
    sign_request(&mut request, &ra, &rb).unwrap();
    (a, b, request)
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

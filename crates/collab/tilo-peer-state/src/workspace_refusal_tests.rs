//! A workspace keeps a device's verdict on a write in that device's own slice
//! of the workspace document, hands it on with the workspace, and lets it go
//! when a write of the same author lands.
use super::*;

/// The write `key`'s device publishes into the workspace's record, authored
/// by `author` as another device's peer sees it arrive.
fn write(author: &mut StatePeer, key: &SigningKey, now: SystemTime, name: &str) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&author.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, name, name).unwrap();
    doc.commit_edit("fixture", "fixture");
    author
        .publication_mut()
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), key, now)
        .unwrap()
}

/// The certificate of the device `key` in `peer`'s workspace.
fn certificate(peer: &StatePeer, key: &SigningKey) -> DeviceCert {
    let device = key.verifying_key().to_bytes();
    peer.owner_certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == device)
        .expect("the workspace holds the device's certificate")
}

#[test]
fn a_workspace_keeps_a_verdict_on_a_write_and_lets_it_go_when_the_write_lands() {
    let (mut peer, a_key, b_key, _, now) = seeded();
    let a_cert = certificate(&peer, &a_key);
    // Two writes of b's device, in the order its peer published them.
    let mut theirs = peer.clone();
    let first = write(&mut theirs, &b_key, now, "first");
    let second = write(&mut theirs, &b_key, now, "second");
    assert!(
        peer.workspace_mut()
            .refuse_write(&a_cert, &second, "no permission", &a_key, now)
            .unwrap(),
        "the workspace takes the device's verdict"
    );
    assert!(
        !peer
            .workspace_mut()
            .refuse_write(&a_cert, &second, "no permission", &a_key, now)
            .unwrap(),
        "one verdict per write"
    );
    let held = peer.workspace().write_refusals();
    assert_eq!(held.len(), 1, "{held:?}");
    let refusal = &held[0];
    assert_eq!(refusal.by, a_cert.device_public_key, "the refuser's own");
    assert_eq!(
        refusal.author,
        b_key.verifying_key().to_bytes(),
        "the write it refused is its author's"
    );
    assert_eq!(refusal.reason, crate::RefusalReason::Forbidden);
    assert_eq!(refusal.workspace, peer.workspace_id());
    assert!(
        !refusal.changes.is_empty(),
        "it names what the write carried"
    );
    // The verdict survives a restart: it is part of the workspace's document.
    let mut restored = StatePeer::restore(
        &peer.durable().unwrap(),
        peer.workspace_id(),
        certificate(&peer, &a_key),
        vec![certificate(&peer, &b_key)],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert_eq!(
        restored.workspace().write_refusals(),
        held,
        "the workspace keeps the verdict"
    );
    // A write before the refused one is not what landed.
    assert!(
        !restored
            .workspace_mut()
            .clear_landed_write(&a_cert, &first, &a_key, now)
            .unwrap(),
        "an earlier write leaves the verdict"
    );
    assert_eq!(restored.workspace().write_refusals().len(), 1);
    assert!(
        restored
            .workspace_mut()
            .clear_landed_write(&a_cert, &second, &a_key, now)
            .unwrap(),
        "the refused write landing lets the verdict go"
    );
    assert!(restored.workspace().write_refusals().is_empty());
}

#[test]
fn a_verdict_is_kept_only_in_the_slice_of_the_device_that_made_it() {
    let (mut peer, a_key, b_key, _, now) = seeded();
    let a_cert = certificate(&peer, &a_key);
    let b_cert = certificate(&peer, &b_key);
    let mut theirs = peer.clone();
    let message = write(&mut theirs, &b_key, now, "value");
    let before = peer.admitted_workspace().cloned();
    peer.workspace_mut()
        .refuse_write(&a_cert, &message, "no permission", &a_key, now)
        .unwrap();
    let after = peer.admitted_workspace().unwrap().clone();
    assert!(
        crate::workspace::refusal_slices_kept(
            before.as_ref(),
            &after,
            Some(&a_cert.user_public_key),
            Some(&a_cert.device_public_key),
        ),
        "the verdict is the refuser's own slice"
    );
    assert!(
        !crate::workspace::refusal_slices_kept(
            before.as_ref(),
            &after,
            Some(&b_cert.user_public_key),
            Some(&b_cert.device_public_key),
        ),
        "a change from another device leaves the verdict alone"
    );
    assert!(
        !crate::workspace::refusal_slices_kept(
            before.as_ref(),
            &after,
            Some(&a_cert.user_public_key),
            None,
        ),
        "a sender that names no device files no verdict"
    );
}

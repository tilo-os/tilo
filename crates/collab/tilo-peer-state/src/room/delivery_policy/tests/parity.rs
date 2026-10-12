use super::*;
use tilo_automerge::ApplyIncoming;
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
fn delivery_projection_preserves_winner_shapes_scopes_and_ignored_fields() {
    let (peer, _, _, device, _) = crate::admission_tests::seeded();
    let base = peer.admitted_room().unwrap();
    for mode in 0..17 {
        let mut doc = base.clone();
        let caps = doc.get(ROOT, "grants").unwrap().unwrap().1;
        match mode {
            0 => {
                doc.delete(ROOT, "grants").unwrap();
            }
            1 => {
                doc.put(ROOT, "grants", false).unwrap();
            }
            2..=4 => {
                doc.put_object(
                    ROOT,
                    "grants",
                    [ObjType::Table, ObjType::List, ObjType::Text][mode - 2],
                )
                .unwrap();
            }
            5 => {
                doc.put(&caps, "extra", "not bytes").unwrap();
            }
            6 => {
                doc.put_object(&caps, "extra", ObjType::Map).unwrap();
            }
            7 => {
                doc.put(ROOT, "owner", false).unwrap();
            }
            8 => {
                doc.delete(ROOT, "owner").unwrap();
            }
            9 => {
                doc.put(ROOT, "owner", "different owner accepted by this projection")
                    .unwrap();
            }
            10..=14 => {
                let node = match mode {
                    10 => TreePath::room(peer.room()),
                    11 => TreePath::root(),
                    12 => TreePath::room("elsewhere"),
                    13 => TreePath::room(peer.room()).app("app"),
                    _ => TreePath::room(peer.room()).rest().field("comments"),
                };
                let grant = Grant {
                    node,
                    ..room_grant(peer.room(), Subject::Role(Role::Editor))
                };
                doc.put(&caps, "extra", serde_json::to_vec(&grant).unwrap())
                    .unwrap();
            }
            15 => {
                doc.put(ROOT, "owner", false).unwrap();
                doc.put(&caps, "extra", b"bad-json".to_vec()).unwrap();
            }
            _ => {
                doc.put(ROOT, "name", "ignored name").unwrap();
                doc.put(ROOT, "room", "ignored room field").unwrap();
                doc.put(ROOT, crate::room::media::FIELD, "ignored media")
                    .unwrap();
            }
        }
        doc.commit();
        let got = compare(&doc, &peer, peer.authority(), &device, false);
        let expected = match mode {
            0 | 9..=14 | 16 => Failure::CurrentDenied,
            _ => Failure::Unavailable,
        };
        assert_eq!(got, Err(expected));
        if mode == 16 {
            assert_eq!(
                compare(&doc, &peer, peer.authority(), &device, true),
                compare(base, &peer, peer.authority(), &device, true)
            );
        }
    }
    let mut left = base.clone();
    left.set_actor(automerge::ActorId::from(vec![1]));
    let mut right = base.clone();
    right.set_actor(automerge::ActorId::from(vec![2]));
    left.put(ROOT, "owner", "left owner").unwrap();
    right.put(ROOT, "owner", "right owner").unwrap();
    left.commit();
    right.commit();
    left.merge_incoming(&mut right).unwrap();
    assert!(left.get_all(ROOT, "owner").unwrap().len() > 1);
    assert!(compare(&left, &peer, peer.authority(), &device, true).is_ok());
    let mut left = base.clone();
    left.set_actor(automerge::ActorId::from(vec![3]));
    let mut right = base.clone();
    right.set_actor(automerge::ActorId::from(vec![4]));
    let lc = left.put_object(ROOT, "grants", ObjType::Map).unwrap();
    let rc = right.put_object(ROOT, "grants", ObjType::Map).unwrap();
    let grant = room_grant(peer.room(), Subject::Role(Role::Editor));
    left.put(&lc, "left", serde_json::to_vec(&grant).unwrap())
        .unwrap();
    right
        .put(&rc, "right", serde_json::to_vec(&grant).unwrap())
        .unwrap();
    left.commit();
    right.commit();
    left.merge_incoming(&mut right).unwrap();
    assert!(left.get_all(ROOT, "grants").unwrap().len() > 1);
    assert!(compare(&left, &peer, peer.authority(), &device, true).is_ok());
}
#[test]
fn delivery_projection_preserves_exact_final_quota_and_quota_denial_order() {
    let (peer, _, _, device, _) = crate::admission_tests::seeded();
    let mut doc = peer.admitted_room().unwrap().clone();
    let old = doc
        .get(ROOT, "owner")
        .unwrap()
        .unwrap()
        .0
        .as_str()
        .unwrap()
        .len();
    let size = original_with_length(&doc, &peer, 37, &device, peer.authority(), true)
        .unwrap()
        .1;
    // Old64-byte string length is one postcard varint byte; the admitted near
    // one-MiB string uses three. All other tuple bytes remain literal oracle.
    assert!(old < 128);
    let length = POLICY_BYTES - (size - old) - 2;
    doc.put(ROOT, "owner", "x".repeat(length)).unwrap();
    doc.commit();
    assert_eq!(
        original_with_length(&doc, &peer, 37, &device, peer.authority(), true)
            .unwrap()
            .1,
        POLICY_BYTES
    );
    assert!(compare(&doc, &peer, peer.authority(), &device, true).is_ok());
    doc.put(ROOT, "owner", "x".repeat(length + 1)).unwrap();
    doc.commit();
    assert_eq!(
        compare(&doc, &peer, peer.authority(), &device, true),
        Err(Failure::Capacity)
    );
    assert_eq!(
        compare(&doc, &peer, peer.authority(), &device, false),
        Err(Failure::CurrentDenied)
    );
    doc.put(ROOT, "owner", "x".repeat(POLICY_BYTES + 1))
        .unwrap();
    doc.commit();
    assert_eq!(
        compare(&doc, &peer, peer.authority(), &device, false),
        Err(Failure::Capacity)
    );
    let mut doc = peer.admitted_room().unwrap().clone();
    let caps = doc.get(ROOT, "grants").unwrap().unwrap().1;
    doc.put(&caps, "oversized", vec![b' '; POLICY_BYTES + 1])
        .unwrap();
    doc.commit();
    assert_eq!(
        compare(&doc, &peer, peer.authority(), &device, false),
        Err(Failure::Capacity)
    );
}
#[test]
fn delivery_projection_reads_nested_valid_json_and_refuses_small_limits() {
    let (peer, _, _, device, _) = crate::admission_tests::seeded();
    let mut doc = peer.admitted_room().unwrap().clone();
    let caps = doc.get(ROOT, "grants").unwrap().unwrap().1;
    for i in 0..16 {
        let grant = room_grant(
            peer.room(),
            Subject::person(format!("{i}:{}", "name".repeat(512))),
        );
        doc.put(
            &caps,
            format!("key-{i:02}:{}", "key".repeat(256)),
            serde_json::to_vec(&grant).unwrap(),
        )
        .unwrap();
    }
    doc.commit();
    assert!(compare(&doc, &peer, peer.authority(), &device, true).is_ok());
    let certs = peer.certificates();
    let revoked = peer.revocations();
    let meta = || Metadata {
        room: peer.room(),
        workspace: 37,
        authority: peer.authority(),
        host: peer.authority(),
        device: &device,
        certificates: certs.iter(),
        revocations: revoked.iter(),
    };
    let run = |collections, allocation_bytes| {
        native(
            &LegacyRead(&doc),
            meta(),
            Limits {
                collections,
                json: JsonLimits {
                    allocation_bytes,
                    collection_entries: 65536,
                },
            },
            || Ok(()),
        )
    };
    assert!(matches!(run(1, 128 * 1024), Err(Error::Capacity)));
    assert!(matches!(
        run(4 * 1024 * 1024, 64),
        Err(Error::Decode(PeerResourceError::Capacity))
    ));
    assert!(run(4 * 1024 * 1024, 128 * 1024).is_ok());
}

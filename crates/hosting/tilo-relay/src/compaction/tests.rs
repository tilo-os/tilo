use super::*;
use crate::store::WorkspaceStore;
use automerge::{ROOT, ReadDoc as _, transaction::Transactable as _};
use ed25519_dalek::Signer as _;

const WORKSPACE: u64 = 7;

fn route() -> Route {
    Route {
        app_id: "so.tilo.notes".into(),
        cluster_id: "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".into(),
        record_id: "note-1".into(),
    }
}

fn channel() -> [u8; 32] {
    *blake3::hash(b"app\0so.tilo.notes\0note-1").as_bytes()
}

fn key(seed: u8) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
}

/// A device's retained frames for `mutation`, as a page signs them;
/// `sealed` stands in for channel-crypto ciphertext.
fn device_frames(seed: u8, mutation: &DocumentMutation, sealed: bool, seq: u64) -> Vec<Vec<u8>> {
    let key = key(seed);
    let author = key.verifying_key().to_bytes();
    let route = route();
    let artifact =
        collab_types::document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
    let attestation =
        collab_types::document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id);
    mutation
        .encode_frames()
        .unwrap()
        .into_iter()
        .enumerate()
        .map(|(index, payload)| {
            let snapshot_head = matches!(mutation, DocumentMutation::Snapshot(_)) && index == 0;
            let mut op = if sealed {
                collab_types::SignedOp::new_unsigned_with_encryption(
                    payload.iter().map(|byte| byte ^ 0x5a).collect(),
                    artifact,
                    collab_types::Ed25519PublicKey::from_bytes(author),
                    attestation,
                    collab_types::OpKind::Change,
                    std::time::UNIX_EPOCH,
                    seq.saturating_add(index as u64),
                    collab_types::EncryptionMetadata::mls_application(
                        1,
                        collab_types::ChannelId::new(),
                    ),
                )
            } else {
                collab_types::SignedOp::new_unsigned(
                    payload,
                    artifact,
                    collab_types::Ed25519PublicKey::from_bytes(author),
                    attestation,
                    collab_types::OpKind::Change,
                    std::time::UNIX_EPOCH,
                    seq.saturating_add(index as u64),
                )
            };
            op.signature = collab_types::Signature::from_bytes(
                key.sign(&op.signing_bytes().unwrap()).to_bytes(),
            );
            let body = postcard::to_stdvec(&NeighbourMsg::AppDocument {
                peer: tilo_peer_state::peer_handle(&author),
                app_id: route.app_id.clone(),
                cluster_id: route.cluster_id.clone(),
                record_id: route.record_id.clone(),
                envelope: Box::new(op),
            })
            .unwrap();
            let payload = if snapshot_head && !sealed {
                CoeditPayload::Checkpoint {
                    channel: channel(),
                    body,
                }
            } else {
                CoeditPayload::RetainedDelta {
                    channel: channel(),
                    body,
                }
            };
            crate::transport::encode(&Envelope::Delta {
                workspace_id: WORKSPACE,
                author_device_public_key: author,
                authored_at_ns: seq,
                delta: payload.encode().unwrap(),
                signature: [0; 64],
            })
            .unwrap()
        })
        .collect()
}

fn retain(store: &WorkspaceStore, frames: Vec<Vec<u8>>, seq: &mut u64) {
    for frame in frames {
        *seq = seq.saturating_add(1);
        let checkpoint = matches!(
            crate::store::retained::payload(&frame, WORKSPACE),
            Some((_, CoeditPayload::Checkpoint { .. }))
        );
        if checkpoint {
            store.retain_checkpoint(&channel(), &frame).unwrap();
        } else {
            store.retain_delta(&channel(), *seq, &frame).unwrap();
        }
    }
}

/// Merge everything the channel retains the way a late joiner does.
fn late_join(store: &WorkspaceStore) -> AutoCommit {
    let entries = store.channel_entries(&channel()).unwrap();
    let (units, _) = units(&entries);
    let mut doc = AutoCommit::new();
    for unit in units {
        match unit.mutation {
            Some(DocumentMutation::Snapshot(bytes)) => {
                doc.merge_incoming(&mut AutoCommit::load(&bytes).unwrap())
                    .unwrap();
            }
            Some(DocumentMutation::Incremental(bytes)) => {
                doc.load_incoming(&bytes).unwrap();
            }
            None => {}
        }
    }
    doc
}

fn read(doc: &AutoCommit, key: &str) -> Option<String> {
    doc.get(ROOT, key)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_scalar().ok())
        .and_then(|scalar| scalar.into_string().ok())
}

fn compact(store: &WorkspaceStore, entries: &[ChannelEntry]) -> Replaced {
    let folded = fold(entries).expect("something to fold");
    let frames = document_frames(&key(99), WORKSPACE, &folded.route, &folded.snapshot).unwrap();
    let remove: HashSet<[u8; 32]> = folded.removed.iter().copied().collect();
    store
        .replace_channel(
            &channel(),
            Some(SnapshotWrite {
                author: key(99).verifying_key().to_bytes(),
                frames,
            }),
            &|bytes| remove.contains(&file_digest(bytes)),
        )
        .unwrap()
}

use crate::store::compaction::Replaced;

#[test]
fn the_host_folds_a_long_tail_and_a_late_joiner_converges_from_its_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), WORKSPACE, false).unwrap();
    let mut seq = 0;
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "base").unwrap();
    doc.commit();
    retain(
        &store,
        device_frames(1, &DocumentMutation::Snapshot(doc.save()), false, 1),
        &mut seq,
    );
    for edit in 0..300 {
        let before = doc.get_heads();
        doc.put(ROOT, format!("k{}", edit % 20), i64::from(edit))
            .unwrap();
        doc.commit();
        let author = if edit % 3 == 0 { 2 } else { 1 };
        retain(
            &store,
            device_frames(
                author,
                &DocumentMutation::Incremental(doc.save_after(&before)),
                false,
                u64::try_from(edit).unwrap().saturating_add(10),
            ),
            &mut seq,
        );
    }
    let entries = store.channel_entries(&channel()).unwrap();
    assert!(compaction_due(&entries, COMPACT_TAIL_ENTRIES));
    let outcome = compact(&store, &entries);
    assert_eq!(outcome.removed, 301);
    assert_eq!(outcome.kept, 1, "one host snapshot, no tail");
    assert_eq!(late_join(&store).get_heads(), doc.get_heads());
    assert!(!compaction_due(
        &store.channel_entries(&channel()).unwrap(),
        COMPACT_TAIL_ENTRIES
    ));
}

/// Ported from the old relay's `checkpoint_race`: a change retained while
/// the host folds (after it read the channel) is never removed, and one it
/// read but could not apply (its dependency is sealed) is kept too.
#[test]
fn a_change_retained_during_the_fold_or_depending_on_unreadable_content_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), WORKSPACE, false).unwrap();
    let mut seq = 0;
    let mut base = AutoCommit::new();
    base.put(ROOT, "title", "base").unwrap();
    base.commit();
    retain(
        &store,
        device_frames(1, &DocumentMutation::Snapshot(base.save()), false, 1),
        &mut seq,
    );
    let mut on_a = base.fork();
    for edit in 0..10 {
        let before = on_a.get_heads();
        on_a.put(ROOT, "a", i64::from(edit)).unwrap();
        on_a.commit();
        retain(
            &store,
            device_frames(
                1,
                &DocumentMutation::Incremental(on_a.save_after(&before)),
                false,
                u64::try_from(edit).unwrap().saturating_add(10),
            ),
            &mut seq,
        );
    }
    // A sealed change and a cleartext one on top of it.
    let mut hidden = base.fork();
    hidden.put(ROOT, "hidden", "sealed").unwrap();
    hidden.commit();
    retain(
        &store,
        device_frames(
            3,
            &DocumentMutation::Incremental(hidden.save_after(&base.get_heads())),
            true,
            50,
        ),
        &mut seq,
    );
    let before = hidden.get_heads();
    hidden.put(ROOT, "after_hidden", "clear").unwrap();
    hidden.commit();
    let dependent = device_frames(
        3,
        &DocumentMutation::Incremental(hidden.save_after(&before)),
        false,
        51,
    );
    retain(&store, dependent.clone(), &mut seq);

    let entries = store.channel_entries(&channel()).unwrap();
    // B commits concurrently, after the host read the channel.
    let mut on_b = base.fork();
    on_b.put(ROOT, "from_b", "concurrent").unwrap();
    on_b.commit();
    let concurrent = device_frames(
        2,
        &DocumentMutation::Incremental(on_b.save_after(&base.get_heads())),
        false,
        60,
    );
    retain(&store, concurrent.clone(), &mut seq);
    let outcome = compact(&store, &entries);
    assert_eq!(outcome.removed, 11, "the snapshot and A's ten changes");

    let left: Vec<[u8; 32]> = store
        .channel_entries(&channel())
        .unwrap()
        .iter()
        .map(|entry| entry.digest)
        .collect();
    assert!(
        left.contains(&file_digest(&concurrent[0])),
        "the concurrent change survives"
    );
    assert!(
        left.contains(&file_digest(&dependent[0])),
        "a change waiting on sealed content survives"
    );
    assert_eq!(
        left.len(),
        4,
        "host snapshot, sealed, dependent, concurrent"
    );
    let mut joined = late_join(&store);
    assert_eq!(read(&joined, "from_b").as_deref(), Some("concurrent"));
    assert_eq!(read(&joined, "title").as_deref(), Some("base"));
    assert_eq!(
        joined.get_heads().len(),
        2,
        "A's line and B's; the sealed line waits for its key"
    );
}

#[test]
fn a_sealed_tail_is_never_folded_and_asks_for_a_member() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), WORKSPACE, false).unwrap();
    let mut seq = 0;
    let mut doc = AutoCommit::new();
    for edit in 0..COMPACT_TAIL_ENTRIES {
        let before = doc.get_heads();
        doc.put(ROOT, "k", i64::try_from(edit).unwrap()).unwrap();
        doc.commit();
        retain(
            &store,
            device_frames(
                1,
                &DocumentMutation::Incremental(doc.save_after(&before)),
                true,
                edit as u64 + 1,
            ),
            &mut seq,
        );
    }
    let entries = store.channel_entries(&channel()).unwrap();
    assert!(
        !compaction_due(&entries, COMPACT_TAIL_ENTRIES),
        "nothing readable to fold"
    );
    assert!(fold(&entries).is_none());
    assert_eq!(sealed_tail(&entries, &HashSet::new()), Some(route()));
    let known: HashSet<[u8; 32]> = entries
        .iter()
        .take(10)
        .map(|entry| entry_id(&entry.body))
        .collect();
    assert_eq!(
        sealed_tail(&entries, &known),
        None,
        "a member snapshot's own entries are not a tail"
    );
}

/// The #1432 piece: a native shell drops a document whose publisher is not
/// the handle of the device it came from, so the host's snapshot carries its
/// signing key's handle, and its signature verifies as that device's.
#[test]
fn host_frames_name_their_signer_as_publisher() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "x").unwrap();
    let frames = document_frames(&key(7), WORKSPACE, &route(), &doc.save()).unwrap();
    let host = key(7).verifying_key().to_bytes();
    let (
        author,
        CoeditPayload::Checkpoint {
            body,
            channel: named,
        },
    ) = crate::store::retained::payload(&frames[0], WORKSPACE).unwrap()
    else {
        panic!("the first frame is the checkpoint");
    };
    assert_eq!(author, host);
    assert_eq!(named, channel());
    let NeighbourMsg::AppDocument { peer, envelope, .. } = postcard::from_bytes(&body).unwrap()
    else {
        panic!("not a document");
    };
    assert_eq!(peer, tilo_peer_state::peer_handle(&host));
    assert_eq!(*envelope.author.as_bytes(), host);
    collab_types::verify_op(&envelope).unwrap();
}

/// The other #1432 piece: the host's certificate is retained in its own
/// certificate lineage, which a replay carries before every other lineage.
#[test]
fn the_host_certificate_is_retained_where_replay_leads_with_it() {
    let cert = tilo_user_identity::DeviceCert {
        user_public_key: key(1).verifying_key().to_bytes(),
        device_public_key: key(2).verifying_key().to_bytes(),
        device_label: "host".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::host(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    let (channel, frame) = identity_frame(&cert, WORKSPACE).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), WORKSPACE, false).unwrap();
    store.retain_checkpoint(&channel, &frame).unwrap();
    store.retain_checkpoint(&channel, &frame).unwrap();
    assert_eq!(
        store.load_retained().unwrap(),
        vec![frame.clone()],
        "retained once"
    );
    let lineage = format!(
        "{}-{}",
        hex::encode(cert.device_public_key),
        hex::encode(channel)
    );
    assert_eq!(crate::store::replay::lineage_rank(&lineage), "0");
    let (_, CoeditPayload::Checkpoint { body, .. }) =
        crate::store::retained::payload(&frame, WORKSPACE).unwrap()
    else {
        panic!("a checkpoint");
    };
    assert!(matches!(
        postcard::from_bytes(&body).unwrap(),
        NeighbourMsg::IdentityHello { cert: named, .. } if *named == cert
    ));
}

#[test]
fn a_large_snapshot_is_framed_in_segments_the_fold_reads_back() {
    let mut doc = AutoCommit::new();
    let text = doc
        .put_object(ROOT, "text", automerge::ObjType::Text)
        .unwrap();
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let noise: String = (0..3 * 1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(b'a' + (state % 26) as u8)
        })
        .collect();
    doc.splice_text(&text, 0, 0, &noise).unwrap();
    let frames = document_frames(&key(7), WORKSPACE, &route(), &doc.save()).unwrap();
    assert!(frames.len() > 1, "a snapshot above one frame is segmented");
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), WORKSPACE, false).unwrap();
    store
        .replace_channel(
            &channel(),
            Some(SnapshotWrite {
                author: key(7).verifying_key().to_bytes(),
                frames,
            }),
            &|_| false,
        )
        .unwrap();
    let entries = store.channel_entries(&channel()).unwrap();
    let (units, _) = units(&entries);
    assert_eq!(units.len(), 1, "every segment joins one snapshot");
    assert!(
        !compaction_due(&entries, COMPACT_TAIL_ENTRIES),
        "a host snapshot alone is no tail"
    );
    assert_eq!(late_join(&store).get_heads(), doc.get_heads());
}

/// `observe` before it borrowed the body and peeked the variant tag: an owned
/// decode of the payload and a full decode of its `NeighbourMsg`.
fn observe_by_full_decode(delta: &[u8]) -> Option<([u8; 32], usize)> {
    let (CoeditPayload::Checkpoint { channel, body }
    | CoeditPayload::RetainedDelta { channel, body }) = CoeditPayload::decode(delta)?;
    matches!(
        postcard::from_bytes::<NeighbourMsg>(&body),
        Ok(NeighbourMsg::AppDocument { .. })
    )
    .then_some((channel, delta.len()))
}

#[test]
fn observe_classifies_like_full_decode() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "base").unwrap();
    doc.commit();
    let mut deltas: Vec<Vec<u8>> = [
        device_frames(1, &DocumentMutation::Snapshot(doc.save()), false, 1),
        device_frames(2, &DocumentMutation::Snapshot(doc.save()), true, 2),
    ]
    .concat()
    .into_iter()
    .map(|raw| {
        let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
            panic!("device frames are deltas");
        };
        delta
    })
    .collect();
    let other_bodies = [
        postcard::to_stdvec(&NeighbourMsg::Heartbeat { peer: 3, ts: 4 }).unwrap(),
        postcard::to_stdvec(&NeighbourMsg::PresenceCleared {
            peer: 3,
            key: "cursor".into(),
        })
        .unwrap(),
        postcard::to_stdvec(&NeighbourMsg::SyncResp {
            changes: vec![vec![1, 2]],
            room: "room".into(),
        })
        .unwrap(),
        Vec::new(),
        vec![0xff; 16],
    ];
    for body in other_bodies {
        for payload in [
            CoeditPayload::Checkpoint {
                channel: channel(),
                body: body.clone(),
            },
            CoeditPayload::RetainedDelta {
                channel: channel(),
                body,
            },
        ] {
            deltas.push(payload.encode().unwrap());
        }
    }
    // Trailing bytes after a payload are tolerated by both decoders alike.
    let trailing: Vec<Vec<u8>> = deltas
        .iter()
        .map(|delta| [delta.as_slice(), &[0, 1]].concat())
        .collect();
    deltas.extend(trailing);
    // Plain workspace deltas that are no co-edit payload at all.
    deltas.extend([vec![], vec![5, 0, 0], vec![0xff; 64]]);

    let mut observed = 0;
    for delta in &deltas {
        let now = Compactor::observe(delta).map(|o| (o.channel, o.len));
        assert_eq!(now, observe_by_full_decode(delta), "delta {delta:02x?}");
        observed += usize::from(now.is_some());
    }
    assert!(observed >= 4, "app documents were observed: {observed}");
    assert!(observed < deltas.len(), "other deltas were not");
}

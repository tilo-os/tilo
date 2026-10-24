use super::*;
use crate::store::WorkspaceStore;
use automerge::{ROOT, ReadDoc as _, transaction::Transactable as _};
use ed25519_dalek::Signer as _;
use neighbour_wire::NeighbourMsg;

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
    signed_frames(
        &key(seed),
        key(seed).verifying_key().to_bytes(),
        mutation,
        sealed,
        seq,
    )
}

/// Frames claiming `author`, signed with `key`: a device's own frames when
/// `key` is the author's, a forgery otherwise.
fn signed_frames(
    key: &ed25519_dalek::SigningKey,
    author: [u8; 32],
    mutation: &DocumentMutation,
    sealed: bool,
    seq: u64,
) -> Vec<Vec<u8>> {
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

/// [`retain`] for the long histories compaction's thresholds need: each
/// delta whose author's lineage has a generation is written into it at the
/// next ordinal, as a commit leaves it, without the commit's two barriers.
/// Compaction reads those files whether or not they were synced; hundreds
/// of barriers made these tests wait minutes on a busy CI disk. An author's
/// first frame is retained through the store, which provisions its lineage.
fn append(store: &WorkspaceStore, frames: Vec<Vec<u8>>, seq: &mut u64) {
    for frame in frames {
        let Some((author, CoeditPayload::RetainedDelta { .. })) =
            crate::store::retained::payload(&frame, WORKSPACE)
        else {
            panic!("only a delta appends");
        };
        let lineage = store
            .root()
            .join(crate::store::RETAINED_DIR)
            .join("v2")
            .join(format!(
                "{}-{}",
                hex::encode(author),
                hex::encode(channel())
            ));
        let Some(active) = crate::store::retained::current(&lineage).unwrap() else {
            retain(store, vec![frame], seq);
            continue;
        };
        *seq = seq.saturating_add(1);
        let next = std::fs::read_dir(&active)
            .unwrap()
            .filter_map(|entry| {
                let name = entry.unwrap().file_name();
                u64::from_str_radix(name.to_str()?.strip_suffix(".delta")?, 16).ok()
            })
            .max()
            .map_or(0, |last| last.saturating_add(1));
        std::fs::write(active.join(format!("{next:016x}.delta")), frame).unwrap();
    }
}

/// Each entry checked as a round with nothing remembered checks it.
fn checked(entries: &[ChannelEntry]) -> Vec<Checked<'_>> {
    let mut signatures = Signatures::default();
    entries
        .iter()
        .map(|entry| signatures.check(entry))
        .collect()
}

fn due(entries: &[ChannelEntry], min_tail: usize) -> bool {
    compaction_due(entries, &units(checked(entries)), min_tail, false)
}

fn fold_all(entries: &[ChannelEntry]) -> Option<Folded> {
    fold(entries, &units(checked(entries)), 2)
}

/// Merge everything the channel retains the way a late joiner does.
fn late_join(store: &WorkspaceStore) -> AutoCommit {
    let entries = store.channel_entries(&channel()).unwrap();
    let mut doc = AutoCommit::new();
    for unit in units(checked(&entries)).units {
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
    let folded = fold_all(entries).expect("something to fold");
    let frames = document_frames(
        &crate::payloads::NeighbourPayloads,
        &key(99),
        WORKSPACE,
        &folded.route,
        &folded.snapshot,
    )
    .unwrap();
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
        append(
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
    assert!(due(&entries, COMPACT_TAIL_ENTRIES));
    let outcome = compact(&store, &entries);
    assert_eq!(outcome.removed, 301);
    assert_eq!(outcome.kept, 1, "one host snapshot, no tail");
    assert_eq!(late_join(&store).get_heads(), doc.get_heads());
    assert!(!due(
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
        append(
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
        !due(&entries, COMPACT_TAIL_ENTRIES),
        "nothing readable to fold"
    );
    assert!(fold_all(&entries).is_none());
    assert_eq!(
        sealed_tail(&checked(&entries), &HashSet::new()),
        Some(route())
    );
    let known: HashSet<[u8; 32]> = entries
        .iter()
        .take(10)
        .map(|entry| entry_id(&entry.body))
        .collect();
    assert_eq!(
        sealed_tail(&checked(&entries), &known),
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
    let frames = document_frames(
        &crate::payloads::NeighbourPayloads,
        &key(7),
        WORKSPACE,
        &route(),
        &doc.save(),
    )
    .unwrap();
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
    let (channel, frame) =
        identity_frame(&crate::payloads::NeighbourPayloads, &cert, WORKSPACE).unwrap();
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
    let frames = document_frames(
        &crate::payloads::NeighbourPayloads,
        &key(7),
        WORKSPACE,
        &route(),
        &doc.save(),
    )
    .unwrap();
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
    assert_eq!(
        units(checked(&entries)).units.len(),
        1,
        "every segment joins one snapshot"
    );
    assert!(
        !due(&entries, COMPACT_TAIL_ENTRIES),
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
        let now = Compactor::observe_with(&crate::payloads::NeighbourPayloads, delta)
            .map(|o| (o.channel, o.len));
        assert_eq!(now, observe_by_full_decode(delta), "delta {delta:02x?}");
        observed += usize::from(now.is_some());
    }
    assert!(observed >= 4, "app documents were observed: {observed}");
    assert!(observed < deltas.len(), "other deltas were not");
}

/// A base snapshot and `edits` incremental changes by device 1: the base
/// and the document.
fn history(store: &WorkspaceStore, edits: u64, seq: &mut u64) -> (AutoCommit, AutoCommit) {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "base").unwrap();
    doc.commit();
    retain(
        store,
        device_frames(1, &DocumentMutation::Snapshot(doc.save()), false, 1),
        seq,
    );
    let base = doc.fork();
    extend(store, &mut doc, edits, seq);
    (base, doc)
}

fn extend(store: &WorkspaceStore, doc: &mut AutoCommit, edits: u64, seq: &mut u64) {
    extend_with(store, doc, edits, seq, retain);
}

/// [`extend`], writing each edit's frames with `write`.
fn extend_with(
    store: &WorkspaceStore,
    doc: &mut AutoCommit,
    edits: u64,
    seq: &mut u64,
    write: fn(&WorkspaceStore, Vec<Vec<u8>>, &mut u64),
) {
    for _ in 0..edits {
        let before = doc.get_heads();
        doc.put(ROOT, format!("k{}", *seq), i64::try_from(*seq).unwrap())
            .unwrap();
        doc.commit();
        let at = seq.saturating_add(10);
        write(
            store,
            device_frames(
                1,
                &DocumentMutation::Incremental(doc.save_after(&before)),
                false,
                at,
            ),
            seq,
        );
    }
}

/// A change on top of `base` that claims device 1 but is signed by another
/// key: what a peer, or a writer with access to the store's disk, could
/// put where device 1's entries are. Its dependencies are retained, so a
/// fold that skipped the signature would merge it.
fn forged_change(base: &mut AutoCommit) -> Vec<u8> {
    let mut forged = base.fork();
    let before = forged.get_heads();
    forged.put(ROOT, "forged", "evil").unwrap();
    forged.commit();
    let mut frames = signed_frames(
        &key(66),
        key(1).verifying_key().to_bytes(),
        &DocumentMutation::Incremental(forged.save_after(&before)),
        false,
        999,
    );
    assert_eq!(frames.len(), 1);
    frames.remove(0)
}

/// One production round ([`plan`]) over the channel with `signatures`: the
/// entries it read, what it folded, and the signatures it verified.
fn round(store: &WorkspaceStore, signatures: &mut Signatures) -> (Vec<ChannelEntry>, Folded, u64) {
    let entries = store.channel_entries(&channel()).unwrap();
    let before = thread_signature_checks();
    let (_, folded) = plan(&entries, signatures, &HashSet::new(), 2, |_, _| {
        Some(Vec::new())
    });
    let checks = thread_signature_checks().saturating_sub(before);
    (entries, folded.expect("a tail to fold").0, checks)
}

/// The retained file whose bytes have `digest`.
fn retained_file(dir: &std::path::Path, digest: &[u8; 32]) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if let Some(found) = retained_file(&path, digest) {
                return Some(found);
            }
        } else if file_digest(&std::fs::read(&path).unwrap()) == *digest {
            return Some(path);
        }
    }
    None
}

fn is_opaque(entries: &[ChannelEntry], digest: &[u8; 32]) -> bool {
    let at = entries
        .iter()
        .position(|entry| entry.digest == *digest)
        .expect("the entry is retained");
    let checked = checked(entries).swap_remove(at);
    matches!(checked.into_parts().1, Piece::Opaque)
}

fn folded_doc(folded: &Folded) -> AutoCommit {
    AutoCommit::load(&folded.snapshot).unwrap()
}

/// A round only skips verifying bytes it verified before. A retained file
/// rewritten on disk after its entry verified hashes differently: it is
/// verified again, refused, neither folded nor removed.
#[test]
fn a_tampered_entry_is_refused_even_after_its_bytes_verified_once() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), WORKSPACE, false).unwrap();
    let mut seq = 0;
    let (mut base, _) = history(&store, 10, &mut seq);
    let mut signatures = Signatures::default();
    let (entries, folded, checks) = round(&store, &mut signatures);
    assert_eq!(checks, 11, "the first round verifies every entry");
    assert_eq!(folded.removed.len(), 11);

    let last = entries.last().unwrap().digest;
    let path = retained_file(tmp.path(), &last).expect("the last edit's file");
    let forged = forged_change(&mut base);
    std::fs::write(&path, &forged).unwrap();

    let (entries, folded, checks) = round(&store, &mut signatures);
    assert_eq!(checks, 1, "only the rewritten entry is verified again");
    assert!(is_opaque(&entries, &file_digest(&forged)));
    assert!(!folded.removed.contains(&file_digest(&forged)));
    assert_eq!(folded.removed.len(), 10, "the snapshot and nine edits");
    assert_eq!(read(&folded_doc(&folded), "forged"), None);
}

/// The signer check is not remembered: the bytes of device 1's verified
/// entry, retained in device 2's lineage, are refused.
#[test]
fn a_verified_body_in_another_authors_lineage_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), WORKSPACE, false).unwrap();
    let mut seq = 0;
    history(&store, 10, &mut seq);
    let mut signatures = Signatures::default();
    let (entries, _, _) = round(&store, &mut signatures);

    let body = entries.last().unwrap().body.clone();
    let rehomed = crate::transport::encode(&Envelope::Delta {
        workspace_id: WORKSPACE,
        author_device_public_key: key(2).verifying_key().to_bytes(),
        authored_at_ns: 1,
        delta: CoeditPayload::RetainedDelta {
            channel: channel(),
            body,
        }
        .encode()
        .unwrap(),
        signature: [0; 64],
    })
    .unwrap();
    store.retain_delta(&channel(), 500, &rehomed).unwrap();

    let (entries, folded, checks) = round(&store, &mut signatures);
    assert_eq!(checks, 0, "refused before any signature is checked");
    assert!(
        entries
            .iter()
            .any(|entry| entry.author == key(2).verifying_key().to_bytes())
    );
    assert!(is_opaque(&entries, &file_digest(&rehomed)));
    assert!(!folded.removed.contains(&file_digest(&rehomed)));
    assert_eq!(folded.removed.len(), 11, "device 1's own entries only");
}

/// A forged entry as a peer could have it admitted (admission does not
/// verify document signatures) is verified, and refused, in every round:
/// a failure is never remembered.
#[test]
fn a_forged_entry_never_verifies() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), WORKSPACE, false).unwrap();
    let mut seq = 0;
    let (mut base, _) = history(&store, 4, &mut seq);
    let forged = forged_change(&mut base);
    store.retain_delta(&channel(), 500, &forged).unwrap();
    let mut signatures = Signatures::default();
    for (round_number, expected) in [(1, 6), (2, 1), (3, 1)] {
        let (entries, folded, checks) = round(&store, &mut signatures);
        assert_eq!(checks, expected, "round {round_number}");
        assert!(is_opaque(&entries, &file_digest(&forged)));
        assert!(!folded.removed.contains(&file_digest(&forged)));
        assert_eq!(folded.removed.len(), 5);
        assert_eq!(read(&folded_doc(&folded), "forged"), None);
    }
    assert_eq!(signatures.remembered(), 5, "the forgery is not remembered");
}

/// Unchanged entries are verified once per process; new ones, entries of a
/// fresh process (a restart) and a new snapshot are verified when first
/// read, and what is gone is forgotten.
#[test]
fn a_repeat_round_verifies_only_new_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), WORKSPACE, false).unwrap();
    let mut seq = 0;
    let (_, mut doc) = history(&store, 10, &mut seq);
    let mut signatures = Signatures::default();
    assert_eq!(round(&store, &mut signatures).2, 11);
    assert_eq!(round(&store, &mut signatures).2, 0);
    extend(&store, &mut doc, 3, &mut seq);
    assert_eq!(round(&store, &mut signatures).2, 3);
    assert_eq!(signatures.remembered(), 14);

    let mut restarted = Signatures::default();
    assert_eq!(round(&store, &mut restarted).2, 14);

    let entries = store.channel_entries(&channel()).unwrap();
    compact(&store, &entries);
    extend(&store, &mut doc, 2, &mut seq);
    let (_, _, checks) = round(&store, &mut signatures);
    assert_eq!(checks, 3, "the host snapshot and the two new edits");
    assert_eq!(signatures.remembered(), 3, "removed entries are forgotten");
}

/// A payload adapter that recognises nothing: what the core sees for a body
/// vocabulary it was not built for.
struct Opaque;

impl crate::payloads::ShellPayloads for Opaque {
    fn is_document(&self, _: &[u8]) -> bool {
        false
    }
    fn open_document(&self, _: &[u8]) -> Option<(Route, Box<collab_types::SignedOp>)> {
        None
    }
    fn document_body(
        &self,
        _: &[u8; 32],
        _: &Route,
        _: collab_types::SignedOp,
    ) -> Result<Vec<u8>, String> {
        Err("opaque".into())
    }
    fn host_identity(
        &self,
        _: &tilo_user_identity::DeviceCert,
    ) -> Result<([u8; 32], Vec<u8>), String> {
        Err("opaque".into())
    }
    fn checkpoint(&self, _: &[u8; 32], _: &[u8]) -> Option<crate::payloads::Checkpoint> {
        None
    }
    fn audience(&self, _: &[u8]) -> Option<std::collections::BTreeSet<[u8; 32]>> {
        None
    }
    fn decoded_overhead(&self) -> usize {
        0
    }
}

/// The core makes no assumption about a body its payload adapter does not
/// classify: a channel the desktop adapter would fold and hand to a member
/// is, through an adapter that recognises nothing, kept whole. Nothing is
/// folded, removed or asked for, and nothing is signed.
#[test]
fn opaque_payloads_are_never_folded_or_gc_selected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), WORKSPACE, false).unwrap();
    let mut seq = 0;
    let (_, mut doc) = history(&store, 0, &mut seq);
    extend_with(&store, &mut doc, 300, &mut seq, append);
    let mut sealed = AutoCommit::new();
    for edit in 0..COMPACT_TAIL_ENTRIES {
        let before = sealed.get_heads();
        sealed.put(ROOT, "k", i64::try_from(edit).unwrap()).unwrap();
        sealed.commit();
        let at = seq.saturating_add(10);
        append(
            &store,
            device_frames(
                2,
                &DocumentMutation::Incremental(sealed.save_after(&before)),
                true,
                at,
            ),
            &mut seq,
        );
    }
    let entries = store.channel_entries(&channel()).unwrap();
    assert!(
        checked(&entries)
            .iter()
            .all(|entry| entry.route().is_some()),
        "every entry is a document frame to the desktop adapter"
    );

    let (ask, folded) = plan(
        &entries,
        &mut Signatures::default(),
        &HashSet::new(),
        COMPACT_TAIL_ENTRIES,
        |_, _| Some(Vec::new()),
    );
    assert_eq!(
        ask,
        Some(route()),
        "the desktop adapter asks a member for the sealed tail"
    );
    assert!(
        folded.is_some_and(|(folded, _)| folded.removed.len() > 2),
        "the desktop adapter folds the readable tail"
    );

    let mut signed = false;
    let mut opaque = Signatures::with_payloads(std::sync::Arc::new(Opaque));
    let (ask, folded) = plan(
        &entries,
        &mut opaque,
        &HashSet::new(),
        COMPACT_TAIL_ENTRIES,
        |_, _| {
            signed = true;
            Some(Vec::new())
        },
    );
    assert_eq!(ask, None, "no member is asked to compact opaque entries");
    assert!(folded.is_none(), "no opaque entry is folded or removed");
    assert!(!signed, "the host signs nothing for opaque entries");
    assert_eq!(opaque.remembered(), 0, "no opaque body counts as verified");
}

/// The device the host asks to compact a sealed channel.
const MEMBER: u8 = 2;
/// A device subscribed to the workspace that the host did not ask.
const STRANGER: u8 = 3;

fn device(seed: u8) -> [u8; 32] {
    key(seed).verifying_key().to_bytes()
}

/// Another record's retention channel in the same workspace.
fn other_channel() -> [u8; 32] {
    *blake3::hash(b"app\0so.tilo.notes\0note-2").as_bytes()
}

/// `frame` re-framed to retain on `to`.
fn on_channel(frame: &[u8], to: [u8; 32]) -> Vec<u8> {
    let (author, payload) = crate::store::retained::payload(frame, WORKSPACE).unwrap();
    let payload = match payload {
        CoeditPayload::RetainedDelta { body, .. } => {
            CoeditPayload::RetainedDelta { channel: to, body }
        }
        CoeditPayload::Checkpoint { body, .. } => CoeditPayload::Checkpoint { channel: to, body },
    };
    crate::transport::encode(&Envelope::Delta {
        workspace_id: WORKSPACE,
        author_device_public_key: author,
        authored_at_ns: 1,
        delta: payload.encode().unwrap(),
        signature: [0; 64],
    })
    .unwrap()
}

/// Every retained entry of a channel by author, file digest and body, in a
/// stable order: two equal values are byte-identical retention.
type Retained = Vec<([u8; 32], [u8; 32], Vec<u8>)>;

fn retained_entries(store: &WorkspaceStore, channel: &[u8; 32]) -> Retained {
    let mut entries: Retained = store
        .channel_entries(channel)
        .unwrap()
        .into_iter()
        .map(|entry| (entry.author, entry.digest, entry.body))
        .collect();
    entries.sort();
    entries
}

fn entry_ids(store: &WorkspaceStore, channel: &[u8; 32]) -> Vec<[u8; 32]> {
    store
        .channel_entries(channel)
        .unwrap()
        .iter()
        .map(|entry| entry_id(&entry.body))
        .collect()
}

/// An encoded coverage statement answering `request` for `channel`.
fn statement(
    request: [u8; 16],
    channel: [u8; 32],
    snapshot: &[[u8; 32]],
    covered: &[[u8; 32]],
) -> Vec<u8> {
    CoverageStatement {
        workspace_id: WORKSPACE,
        channel,
        request,
        snapshot: snapshot.to_vec(),
        covered: covered.to_vec(),
        heads: Vec::new(),
    }
    .encode()
    .unwrap()
}

/// `seed`'s signature over `statement`, as a member device signs one.
fn signed(seed: u8, statement: &[u8]) -> [u8; 64] {
    key(seed)
        .sign(&tilo_relay_protocol::compaction::signing_bytes(statement))
        .to_bytes()
}

/// A subscription's queues, open as a live connection keeps them.
type Queues = (
    tokio::sync::mpsc::Receiver<Envelope>,
    tokio::sync::mpsc::Receiver<()>,
);

/// A sealed tail one writer retained, the sealed snapshot of it the member
/// committed, and another record's sealed channel beside it; the host has
/// asked the member, over its live subscription, to compact the first.
struct SealedAsk {
    _tmp: tempfile::TempDir,
    workspace: SharedWorkspace,
    compactor: Compactor,
    request: [u8; 16],
    /// Entry ids of the writer's sealed tail.
    tail: Vec<[u8; 32]>,
    /// Entry ids of the member's sealed snapshot.
    snapshot: Vec<[u8; 32]>,
    _queues: Vec<Queues>,
}

/// Retain the writer's sealed tail on [`channel`] and three sealed entries
/// on [`other_channel`], then the member's sealed snapshot on [`channel`].
fn retain_sealed_channels(store: &WorkspaceStore) {
    let mut seq: u64 = 0;
    let mut doc = AutoCommit::new();
    for edit in 0..6_i64 {
        let before = doc.get_heads();
        doc.put(ROOT, "k", edit).unwrap();
        doc.commit();
        let mutation = DocumentMutation::Incremental(doc.save_after(&before));
        retain(
            store,
            device_frames(1, &mutation, true, seq.saturating_add(1)),
            &mut seq,
        );
    }
    for edit in 0..3_i64 {
        let before = doc.get_heads();
        doc.put(ROOT, "beside", edit).unwrap();
        doc.commit();
        let mutation = DocumentMutation::Incremental(doc.save_after(&before));
        for frame in device_frames(1, &mutation, true, seq.saturating_add(1)) {
            seq = seq.saturating_add(1);
            store
                .retain_delta(&other_channel(), seq, &on_channel(&frame, other_channel()))
                .unwrap();
        }
    }
    let snapshot = DocumentMutation::Snapshot(doc.save());
    retain(
        store,
        device_frames(MEMBER, &snapshot, true, seq.saturating_add(1)),
        &mut seq,
    );
}

/// Subscribe `seed` to `runtime`, as a live session does.
fn subscribe(runtime: &mut crate::workspace::WorkspaceRuntime, seed: u8) -> Queues {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let (disconnect, disconnected) = tokio::sync::mpsc::channel(1);
    runtime.add_subscriber(crate::workspace::Subscriber {
        device_public_key: device(seed),
        role: crate::auth::PeerRole::Device,
        tx,
        disconnect,
    });
    (rx, disconnected)
}

async fn sealed_ask() -> SealedAsk {
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        WORKSPACE,
        false,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    retain_sealed_channels(&runtime.store);
    let (mut tail, mut snapshot) = (Vec::new(), Vec::new());
    for entry in runtime.store.channel_entries(&channel()).unwrap() {
        let id = entry_id(&entry.body);
        if entry.author == device(MEMBER) {
            snapshot.push(id);
        } else {
            tail.push(id);
        }
    }
    assert!(!tail.is_empty() && !snapshot.is_empty());
    assert_eq!(entry_ids(&runtime.store, &other_channel()).len(), 3);
    let (mut member, stranger) = (
        subscribe(&mut runtime, MEMBER),
        subscribe(&mut runtime, STRANGER),
    );
    let workspace: SharedWorkspace = Arc::new(tokio::sync::Mutex::new(runtime));
    let cert = tilo_user_identity::DeviceCert {
        user_public_key: device(98),
        device_public_key: device(99),
        device_label: "host".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::host(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    let compactor = Compactor::new(key(99), cert, crate::payloads::neighbour());
    compactor.volunteer(WORKSPACE, device(MEMBER));
    let request = compactor
        .request(&workspace, WORKSPACE, channel(), &route())
        .await
        .expect("the volunteering member is asked");
    let Ok(Envelope::CompactRequest {
        request: sent,
        workspace_id,
        record,
        ..
    }) = member.0.try_recv()
    else {
        panic!("the member is sent the request");
    };
    assert_eq!(
        (sent, workspace_id, record),
        (request, WORKSPACE, route().record_id)
    );
    SealedAsk {
        _tmp: tmp,
        workspace,
        compactor,
        request,
        tail,
        snapshot,
        _queues: vec![member, stranger],
    }
}

impl SealedAsk {
    /// What the asked channel and the channel beside it retain.
    async fn retained(&self) -> (Retained, Retained) {
        let runtime = self.workspace.lock().await;
        (
            retained_entries(&runtime.store, &channel()),
            retained_entries(&runtime.store, &other_channel()),
        )
    }

    /// Hand the host `statement` from device `from`, signed by `signer`.
    async fn answer(&self, from: u8, signer: u8, statement: &[u8]) -> Result<usize, String> {
        self.compactor
            .compacted(
                &self.workspace,
                WORKSPACE,
                device(from),
                statement,
                &signed(signer, statement),
            )
            .await
    }

    /// The member's own answer: its snapshot covers the whole tail.
    fn whole_answer(&self) -> Vec<u8> {
        statement(self.request, channel(), &self.snapshot, &self.tail)
    }

    /// The member answers for real: the tail goes and its snapshot stays,
    /// so a refusal before it was the statement's, not the fixture's.
    async fn member_compacts(&self) {
        let (_, beside) = self.retained().await;
        assert_eq!(
            self.answer(MEMBER, MEMBER, &self.whole_answer()).await,
            Ok(self.tail.len())
        );
        let (asked, beside_after) = self.retained().await;
        let mut kept: Vec<[u8; 32]> = asked.iter().map(|(_, _, body)| entry_id(body)).collect();
        kept.sort_unstable();
        let mut snapshot = self.snapshot.clone();
        snapshot.sort_unstable();
        assert_eq!(kept, snapshot);
        assert_eq!(beside_after, beside);
    }
}

/// security.md, "What a workspace host reads and originates": sealed
/// channels are compacted by a member device, and the host removes only the
/// entries the member's signed statement names. A subscribed device the host
/// did not ask, or a statement claiming the asked device under another key,
/// removes nothing and leaves the request open for the member.
#[tokio::test]
async fn a_sealed_compaction_statement_from_a_device_not_asked_removes_nothing() {
    let ask = sealed_ask().await;
    let before = ask.retained().await;
    let whole = ask.whole_answer();
    assert_eq!(
        ask.answer(STRANGER, STRANGER, &whole).await,
        Err("statement does not answer this device's request".into()),
        "a subscribed device the host did not ask cannot answer"
    );
    assert_eq!(
        ask.answer(MEMBER, STRANGER, &whole).await,
        Err("statement signature does not verify".into()),
        "a statement for the asked device must carry its signature"
    );
    assert_eq!(ask.retained().await, before);
    ask.member_compacts().await;
    drop(ask);
}

/// A device removed from the room loses its subscription at once, while its
/// session lives until the next access audit: an answer it sends in between
/// removes nothing.
#[tokio::test]
async fn a_device_removed_from_the_room_cannot_answer_for_a_sealed_channel() {
    let ask = sealed_ask().await;
    let before = ask.retained().await;
    ask.workspace
        .lock()
        .await
        .refuse_removed_live_members(&[device(MEMBER)]);
    assert_eq!(
        ask.answer(MEMBER, MEMBER, &ask.whole_answer()).await,
        Err("the statement's device no longer subscribes to the workspace".into())
    );
    assert_eq!(ask.retained().await, before);
    drop(ask);
}

/// A statement the host never requested, or one naming another channel or
/// workspace than the request it cites, removes nothing.
#[tokio::test]
async fn a_sealed_compaction_statement_the_host_never_requested_removes_nothing() {
    let ask = sealed_ask().await;
    let before = ask.retained().await;
    let unrequested = statement([7; 16], channel(), &ask.snapshot, &ask.tail);
    assert_eq!(
        ask.answer(MEMBER, MEMBER, &unrequested).await,
        Err("no outstanding compaction request".into())
    );
    let elsewhere = statement(ask.request, other_channel(), &ask.snapshot, &ask.tail);
    let mut other_workspace = CoverageStatement::decode(&ask.whole_answer()).unwrap();
    other_workspace.workspace_id = WORKSPACE + 1;
    for mismatched in [elsewhere, other_workspace.encode().unwrap()] {
        assert_eq!(
            ask.answer(MEMBER, MEMBER, &mismatched).await,
            Err("statement does not answer this device's request".into())
        );
    }
    assert_eq!(ask.retained().await, before);
    ask.member_compacts().await;
    drop(ask);
}

/// A statement covering more than the channel the host asked about removes
/// only what it covers there: entries of another channel, unknown ids and
/// the snapshot's own entries stay byte-identical.
#[tokio::test]
async fn a_sealed_compaction_statement_removes_only_its_requested_channels_entries() {
    let ask = sealed_ask().await;
    let (asked, beside) = ask.retained().await;
    let (removed, kept) = ask.tail.split_at(2);
    let beside_ids = entry_ids(&ask.workspace.lock().await.store, &other_channel());
    let covered: Vec<[u8; 32]> = removed
        .iter()
        .chain(&beside_ids)
        .chain(&ask.snapshot)
        .copied()
        .chain([[9; 32]])
        .collect();
    let answer = statement(ask.request, channel(), &ask.snapshot, &covered);
    assert_eq!(ask.answer(MEMBER, MEMBER, &answer).await, Ok(removed.len()));
    let (asked_after, beside_after) = ask.retained().await;
    assert_eq!(beside_after, beside, "another channel is never touched");
    let expected: Retained = asked
        .into_iter()
        .filter(|(_, _, body)| !removed.contains(&entry_id(body)))
        .collect();
    assert_eq!(asked_after, expected);
    assert_eq!(
        asked_after.len(),
        kept.len().saturating_add(ask.snapshot.len())
    );
    drop(ask);
}

/// A statement whose snapshot the answering member did not retain itself
/// (here, the writer's own entries) removes nothing: the host keeps whatever
/// a member's snapshot does not provably replace.
#[tokio::test]
async fn a_sealed_compaction_statement_without_the_members_own_snapshot_removes_nothing() {
    let ask = sealed_ask().await;
    let before = ask.retained().await;
    let (claimed, covered) = ask.tail.split_at(1);
    let answer = statement(ask.request, channel(), claimed, covered);
    assert!(ask.answer(MEMBER, MEMBER, &answer).await.is_err());
    assert_eq!(ask.retained().await, before);
    drop(ask);
}

/// Two devices' writes of the record retained in the clear, as `(actor,
/// seq)` of every change they hold.
fn written_in_the_clear(store: &WorkspaceStore) -> HashSet<(automerge::ActorId, u64)> {
    let mut seq = 0;
    let (mut first, mut second) = (AutoCommit::new(), AutoCommit::new());
    first.put(ROOT, "title", "before the channel").unwrap();
    first.commit();
    retain(
        store,
        device_frames(1, &DocumentMutation::Snapshot(first.save()), false, 1),
        &mut seq,
    );
    second.merge_incoming(&mut first).unwrap();
    for edit in 0..3_u64 {
        for (seed, doc) in [(1, &mut first), (2, &mut second)] {
            let before = doc.get_heads();
            doc.put(ROOT, format!("by {seed}"), i64::try_from(edit).unwrap())
                .unwrap();
            doc.commit();
            retain(
                store,
                device_frames(
                    seed,
                    &DocumentMutation::Incremental(doc.save_after(&before)),
                    false,
                    edit.saturating_add(10),
                ),
                &mut seq,
            );
        }
    }
    first.merge_incoming(&mut second).unwrap();
    first
        .get_changes(&[])
        .iter()
        .map(|change| (change.actor_id().clone(), change.seq()))
        .collect()
}

/// A key holder's opener for `keys`, as [`Compactor::fold_as_steward`] reads.
fn steward_opener(
    keys: &channel_crypto::ChannelContentKeys,
) -> impl Fn(&collab_types::SignedOp) -> Option<Vec<u8>> + '_ {
    |op| {
        let metadata = op.encryption?;
        let ciphertext = collab_types::sealed_ciphertext(op).ok()?;
        keys.open_content(op.artifact, &metadata, &ciphertext)
            .ok()
            .map(|plaintext| plaintext.to_vec())
    }
}

fn steward_keys() -> channel_crypto::ChannelContentKeys {
    let route = route();
    channel_crypto::ChannelGroup::create(
        collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record_id),
        channel_crypto::MemberKey::generate("steward").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap()
}

/// One steward round over the channel, as [`Compactor::fold_as_steward`]
/// runs it; `removes` false stops it where a crash between writing the
/// sealed snapshot and removing what it holds would.
fn steward_round(
    store: &WorkspaceStore,
    keys: &channel_crypto::ChannelContentKeys,
    removes: bool,
) -> Option<Replaced> {
    let open = steward_opener(keys);
    let entries = store.channel_entries(&channel()).unwrap();
    let (_, folded) = plan_opening(
        &entries,
        &mut Signatures::default(),
        &HashSet::new(),
        COMPACT_TAIL_ENTRIES,
        (Some(&open), true),
        |route, snapshot| {
            sealed_document_frames(
                &crate::payloads::NeighbourPayloads,
                (&key(99), keys),
                WORKSPACE,
                route,
                snapshot,
            )
            .ok()
        },
    );
    let (folded, frames) = folded?;
    let remove: HashSet<[u8; 32]> = folded.removed.iter().copied().collect();
    Some(
        store
            .replace_channel(
                &channel(),
                Some(SnapshotWrite {
                    author: key(99).verifying_key().to_bytes(),
                    frames,
                }),
                &|bytes| removes && remove.contains(&file_digest(bytes)),
            )
            .unwrap(),
    )
}

/// Every change the channel's sealed snapshots hold, as `(actor, seq)`,
/// when it retains nothing in the clear.
fn sealed_changes(
    store: &WorkspaceStore,
    keys: &channel_crypto::ChannelContentKeys,
) -> Option<HashSet<(automerge::ActorId, u64)>> {
    let open = steward_opener(keys);
    let retained = store.channel_entries(&channel()).unwrap();
    let mut signatures = Signatures::default();
    let checked: Vec<Checked<'_>> = retained
        .iter()
        .map(|entry| signatures.check_opening(entry, Some(&open)))
        .collect();
    if checked.iter().any(Checked::is_cleartext) {
        return None;
    }
    let mut read = HashSet::new();
    for unit in units(checked).units {
        let Some(DocumentMutation::Snapshot(snapshot)) = unit.mutation else {
            panic!("a sealed snapshot opens with the channel's keys")
        };
        for change in AutoCommit::load(&snapshot).unwrap().get_changes(&[]) {
            read.insert((change.actor_id().clone(), change.seq()));
        }
    }
    Some(read)
}

/// What a record retained in the clear before its channel was opened,
/// sealed by its steward: the channel keeps one sealed snapshot, and a
/// member with the channel's keys reads every change of that history from
/// it with its author and sequence.
#[test]
fn the_steward_seals_the_history_written_in_the_clear_with_every_change_and_author() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), WORKSPACE, false).unwrap();
    let written = written_in_the_clear(&store);
    assert_eq!(written.len(), 7);
    let entries = store.channel_entries(&channel()).unwrap().len();
    let keys = steward_keys();
    let outcome = steward_round(&store, &keys, true).expect("a short history is sealed");
    assert_eq!((outcome.removed, outcome.kept), (entries, 1));
    assert_eq!(sealed_changes(&store, &keys), Some(written));
    assert!(
        steward_round(&store, &keys, true).is_none(),
        "a sealed channel is not sealed again"
    );
}

/// The steward stops after its sealed snapshot is durable and before it
/// removed anything: the history in the clear is all still there, and the
/// next round seals it without losing a change.
#[test]
fn a_seal_stopped_before_its_removal_loses_nothing_and_completes_on_the_next_round() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), WORKSPACE, false).unwrap();
    let written = written_in_the_clear(&store);
    let entries = store.channel_entries(&channel()).unwrap().len();
    let keys = steward_keys();
    let stopped = steward_round(&store, &keys, false).unwrap();
    assert_eq!((stopped.removed, stopped.kept), (0, entries + 1));
    assert_eq!(sealed_changes(&store, &keys), None);
    let outcome = steward_round(&store, &keys, true).unwrap();
    assert_eq!(outcome.kept, 1);
    assert_eq!(sealed_changes(&store, &keys), Some(written));
}

/// One write in the clear is not due for the host's own fold, and its
/// steward seals it all the same, once the host's operator decided that it
/// does.
#[test]
fn a_lone_write_in_the_clear_is_folded_only_by_its_steward() {
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
    let entries = store.channel_entries(&channel()).unwrap();
    let (_, folded) = plan(
        &entries,
        &mut Signatures::default(),
        &HashSet::new(),
        COMPACT_TAIL_ENTRIES,
        |_, _| Some(Vec::new()),
    );
    assert!(folded.is_none());
    let steward = |reseal| {
        plan_opening(
            &entries,
            &mut Signatures::default(),
            &HashSet::new(),
            COMPACT_TAIL_ENTRIES,
            (Some(&|_: &collab_types::SignedOp| None), reseal),
            |_, _| Some(Vec::new()),
        )
        .1
    };
    assert!(steward(false).is_none(), "kept until its operator decides");
    assert_eq!(
        steward(true)
            .expect("sealed by its steward")
            .0
            .removed
            .len(),
        1
    );
}

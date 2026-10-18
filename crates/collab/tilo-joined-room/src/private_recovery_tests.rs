//! Exercise opening a file written before confirmation privacy was enforced.
use super::tests::enrolled;
use automerge::{
    AutoCommit, ObjId, ObjType, Prop, ROOT, ReadDoc, Value, transaction::Transactable,
};
use ed25519_dalek::{Signer as _, SigningKey};
use tilo_desktop_model::{
    WorkspaceTaskConfirmation as Confirmation, WorkspaceTaskConfirmationState as State,
};

const SECRET: &str = "private historical confirmation body\n  exact whitespace";

fn confirmation() -> Confirmation {
    Confirmation {
        id: "review".into(),
        asking: uuid::Uuid::from_u128(19),
        run_epoch: 1,
        agent_id: "scribe".into(),
        app: "mail".into(),
        resource_kind: "mailbox".into(),
        resource: "inbox".into(),
        action: "send".into(),
        kind: tilo_action_policy::ActionKind::Undoable,
        risk: Default::default(),
        draft: tilo_actions::Draft {
            action_label: SECRET.into(),
            description: Some(SECRET.into()),
            ..Default::default()
        },
        arguments_digest: [7; 32],
        requested_at: 17,
        state: State::Waiting,
    }
}

// Recreate the old single-change checkpoint without calling today's sanitizer.
fn copy_object(source: &AutoCommit, from: &ObjId, target: &mut AutoCommit, to: &ObjId) {
    let kind = source.object_type(from).unwrap();
    if kind == ObjType::Text {
        target
            .splice_text(to, 0, 0, &source.text(from).unwrap())
            .unwrap();
        return;
    }
    let props: Vec<Prop> = if kind == ObjType::List {
        (0..source.length(from)).map(Prop::Seq).collect()
    } else {
        source.keys(from).map(Prop::Map).collect()
    };
    for prop in props {
        let (value, child) = source.get(from, prop.clone()).unwrap().unwrap();
        match value {
            Value::Scalar(value) => {
                if let Prop::Seq(index) = prop {
                    target.insert(to, index, value.as_ref().clone()).unwrap();
                } else {
                    target.put(to, prop, value.as_ref().clone()).unwrap();
                }
            }
            Value::Object(kind) => {
                let next = if let Prop::Seq(index) = prop {
                    target.insert_object(to, index, kind).unwrap()
                } else {
                    target.put_object(to, prop, kind).unwrap()
                };
                copy_object(source, &child, target, &next);
            }
        }
    }
}

fn sign(baseline: &mut tilo_peer_state::SanitizedRoomBaseline, key: &SigningKey) {
    let mut bytes = b"tilo/sanitized-room-baseline/v1\0".to_vec();
    bytes.extend(
        postcard::to_allocvec(&(
            &baseline.room,
            baseline.generation,
            baseline.prior_chain_hash,
            baseline.governor,
            blake3::hash(&baseline.document).as_bytes(),
        ))
        .unwrap(),
    );
    baseline.signature = key.sign(&bytes).to_bytes().to_vec();
}

// The durable v5 head includes the room checkpoint; all document parts, exact
// admission entries, receipts and the tail remain the actual owner's bytes.
fn replace_room(peer: &tilo_peer_state::StatePeer, doc: &mut AutoCommit) -> Vec<u8> {
    let parts = peer.durable_parts().unwrap();
    let (version, format, inner, room, authority, old, documents): (
        u32,
        u32,
        u32,
        String,
        tilo_user_identity::DeviceCert,
        Option<Vec<u8>>,
        usize,
    ) = postcard::from_bytes(&parts[0]).unwrap();
    assert_eq!((version, format, inner), (5, 4, 3));
    assert!(old.is_some());
    let mut bytes = postcard::to_allocvec(&(
        version,
        format,
        inner,
        room,
        authority,
        Some(doc.save()),
        documents,
    ))
    .unwrap();
    for part in &parts[1..] {
        bytes.extend_from_slice(part);
    }
    bytes
}

fn declined() -> Confirmation {
    let mut full = confirmation();
    full.state = State::Declined {
        person: "person".into(),
        at: 23,
        reason: Some(SECRET.into()),
    };
    full
}

fn legacy_candidate(
    owner: &crate::NativeWorkspaceOwner,
    room: crate::RoomId,
    now: std::time::SystemTime,
    key: &SigningKey,
) -> (
    crate::NativeCommittedView,
    tilo_peer_state::SanitizedRoomBaseline,
) {
    let mut candidate = owner.with_current(room, |view| Ok(view.clone())).unwrap();
    let mut baseline = candidate.record.sanitized_baseline.clone().unwrap();
    let source = tilo_automerge_isolate::load(&baseline.document).unwrap();
    let mut legacy = AutoCommit::new();
    copy_object(&source, &ROOT, &mut legacy, &ROOT);
    let controls = legacy
        .put_object(ROOT, "task_control_v1", ObjType::Map)
        .unwrap();
    let waiting = confirmation();
    let control = tilo_desktop_model::WorkspaceTaskControl {
        run_epoch: 1,
        confirmation: Some(waiting.clone()),
        ..Default::default()
    };
    legacy
        .put(
            &controls,
            "review-task",
            automerge::ScalarValue::Bytes(serde_json::to_vec(&control).unwrap()),
        )
        .unwrap();
    legacy
        .put(ROOT, "public-before-recovery", "keep this value")
        .unwrap();
    legacy.commit();
    assert_eq!(legacy.get_changes(&[]).len(), 1);
    baseline.document = legacy.save();
    sign(&mut baseline, key);
    assert_eq!(
        baseline
            .verify_anchored_document(&baseline.room, baseline.governor, &baseline.anchor())
            .unwrap_err(),
        "baseline contains private task confirmation bodies"
    );

    // A newer accepted answer and an unrelated public edit must not be rolled
    // back to the contaminated checkpoint while opening it for recovery.
    let answered = declined();
    let mut control = control;
    control.confirmation = Some(answered.clone());
    legacy
        .put(
            &controls,
            "review-task",
            automerge::ScalarValue::Bytes(serde_json::to_vec(&control).unwrap()),
        )
        .unwrap();
    legacy
        .put(ROOT, "public-after-checkpoint", "keep this newer value")
        .unwrap();
    legacy.commit();
    let bytes = replace_room(&candidate.peer, &mut legacy);
    candidate.peer = tilo_peer_state::StatePeer::restore(
        &bytes,
        &room.to_string(),
        candidate.governor().clone(),
        vec![candidate.device().clone()],
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    candidate.record.peer = bytes;
    candidate.record.sanitized_baseline = Some(baseline.clone());
    candidate.record.generation += 1;
    crate::publication::refresh_governance_checkpoint(&mut candidate).unwrap();
    crate::publication::validate_private_recovery(&candidate, key).unwrap();
    (candidate, baseline)
}

fn refuse_forged_or_unrelated(
    candidate: &crate::NativeCommittedView,
    baseline: &tilo_peer_state::SanitizedRoomBaseline,
    key: &SigningKey,
    now: std::time::SystemTime,
) {
    let room = candidate.record.room;
    let mut forged = candidate.clone();
    forged.record.sanitized_baseline.as_mut().unwrap().signature[0] ^= 1;
    crate::publication::refresh_governance_checkpoint(&mut forged).unwrap();
    assert!(crate::enrollment::restore_private_recovery(room, &forged.record, now, key).is_err());
    let mut unrelated = candidate.clone();
    let source = tilo_automerge_isolate::load(&baseline.document).unwrap();
    let mut other = AutoCommit::new();
    copy_object(&source, &ROOT, &mut other, &ROOT);
    other.commit();
    let envelope = unrelated.record.sanitized_baseline.as_mut().unwrap();
    envelope.document = other.save();
    sign(envelope, key);
    crate::publication::refresh_governance_checkpoint(&mut unrelated).unwrap();
    assert!(
        crate::enrollment::restore_private_recovery(room, &unrelated.record, now, key).is_err()
    );
}

#[test]
fn keyed_open_recovers_contaminated_signed_checkpoint_before_strict_restore() {
    let (directory, owner, room, now) = enrolled();
    let key = SigningKey::from_bytes(&[61; 32]);
    owner.ensure_private_task_baseline(room, &key, now).unwrap();
    let (candidate, baseline) = legacy_candidate(&owner, room, now, &key);
    refuse_forged_or_unrelated(&candidate, &baseline, &key, now);
    let mut storage = owner.0.authority.read().clone();
    storage.rooms.insert(room, candidate.record.clone());
    let path = directory.path().join("rooms.json");
    std::fs::write(&path, serde_json::to_vec(&storage).unwrap()).unwrap();
    drop(owner);
    let before = std::fs::read(&path).unwrap();
    assert!(crate::open_file(&path).is_err());
    assert!(
        crate::open_file_with_private_recovery(&path, &SigningKey::from_bytes(&[62; 32])).is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);

    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    assert!(
        with_failure(Checkpoint::BeforePublish, || {
            crate::open_file_with_private_recovery(&path, &key)
        })
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let recovered = crate::open_file_with_private_recovery(&path, &key).unwrap();
    drop(recovered);
    // Normal reopening, not another recovery call, must validate the replacement.
    let reopened = crate::open_file(&path).unwrap();
    reopened
        .with_current(room, |view| assert_recovered(view, &baseline))
        .unwrap();
    let committed = std::fs::read(&path).unwrap();
    drop(reopened);
    let again = crate::open_file_with_private_recovery(&path, &key).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        committed,
        "already scrubbed checkpoints are not migrated again"
    );
    again
        .with_current(room, |view| {
            let mut missing = view.record.clone();
            missing.private_task_text.confirmations.clear();
            assert!(
                crate::enrollment::restore_private_recovery(room, &missing, now, &key).is_err()
            );
            Ok(())
        })
        .unwrap();
}

fn assert_recovered(
    view: &crate::NativeCommittedView,
    baseline: &tilo_peer_state::SanitizedRoomBaseline,
) -> Result<(), String> {
    let waiting = confirmation();
    let answered = declined();
    let private = &view.record.private_task_text;
    assert_eq!(private.confirmations["review-task"], answered);
    assert!(private.historical_confirmations["review-task"].contains(&waiting));
    assert!(private.historical_confirmations["review-task"].contains(&answered));
    let current = view.peer.admitted_room().unwrap();
    assert_eq!(
        current
            .get(ROOT, "public-after-checkpoint")
            .unwrap()
            .unwrap()
            .0
            .as_str(),
        Some("keep this newer value")
    );
    assert_eq!(
        current
            .get(ROOT, "public-before-recovery")
            .unwrap()
            .unwrap()
            .0
            .as_str(),
        Some("keep this value")
    );
    let replacement = view.record.sanitized_baseline.as_ref().unwrap();
    assert_eq!(replacement.generation, baseline.generation + 1);
    let mut doc = replacement.verify_anchored_document(
        &replacement.room,
        replacement.governor,
        &replacement.anchor(),
    )?;
    assert_eq!(doc.get_changes(&[]).len(), 1);
    let mut public = crate::testing::entries(view)?;
    public.push(replacement.document.clone());
    public.extend(
        doc.get_changes(&[])
            .iter()
            .map(|change| change.raw_bytes().to_vec()),
    );
    for bytes in public {
        assert!(
            !bytes
                .windows(SECRET.len())
                .any(|part| part == SECRET.as_bytes())
        );
    }
    assert!(
        view.task_policy_snapshot()?.controls["review-task"]
            .confirmation
            .as_ref()
            == Some(&answered)
    );
    crate::publication::validate(view)
}

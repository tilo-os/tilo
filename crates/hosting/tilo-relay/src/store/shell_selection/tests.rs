use super::*;
use crate::store::shell_selection_tests::{producer_frame, producer_metadata};
fn retained(store: &WorkspaceStore, bytes: &[u8]) {
    let (_, payload) = retained::payload(bytes, store.workspace_id).unwrap();
    match payload {
        CoeditPayload::Checkpoint { channel, .. } => {
            store.retain_checkpoint(&channel, bytes).unwrap()
        }
        CoeditPayload::RetainedDelta { channel, .. } => {
            store.retain_delta(&channel, 1, bytes).unwrap()
        }
    }
}
fn setup() -> (tempfile::TempDir, WorkspaceStore) {
    let tmp = tempfile::tempdir().unwrap();
    let meta = producer_metadata();
    let authority = RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let mut store = WorkspaceStore::open(tmp.path(), id, true).unwrap();
    store.bind_shell_authority(authority).unwrap();
    for row in meta["baseline"].as_array().unwrap() {
        retained(&store, &producer_frame(&meta, row, 1));
    }
    // Trusted fixture import models admitted bytes durable before selection.
    let raw = WorkspaceStore::open(tmp.path(), id, true).unwrap();
    for row in meta["checkpoint2"].as_array().unwrap() {
        retained(&raw, &producer_frame(&meta, row, 2));
    }
    (tmp, store)
}

#[test]
fn unchanged_basis_does_not_repeat_snapshot_verification() {
    let (_tmp, store) = setup();
    let scratch = |name| store.root.join("store-work-v1").join(name);
    let mut initial = Selection::new(&store, scratch("verification-initial"), None).unwrap();
    while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}
    let selected_before = crate::store::shell_selection_tests::replay_frames(&store);

    let mut selector = Selection::new(&store, scratch("verification-repeat"), None).unwrap();
    while matches!(selector.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert!(selector.basis.complete());
    assert_eq!(selector.basis.routes.len(), 3);
    assert_eq!(
        selector.snapshot_verifications,
        (3, 0),
        "Pair verifies each possible route once; Check reuses only the identical proved input"
    );
    assert_eq!(
        crate::store::shell_selection_tests::replay_frames(&store),
        selected_before
    );
}

fn changed_snapshot_frame(raw: &[u8], change: &str) -> Vec<u8> {
    let mut envelope = crate::transport::decode(&raw[4..]).unwrap();
    let crate::transport::Envelope::Delta {
        author_device_public_key,
        delta,
        ..
    } = &mut envelope
    else {
        panic!()
    };
    let CoeditPayload::Checkpoint {
        mut channel,
        mut body,
    } = CoeditPayload::decode(delta).unwrap()
    else {
        panic!()
    };
    match change {
        "source" => author_device_public_key[0] ^= 1,
        "channel" => channel[0] ^= 1,
        "body" => {
            let mut message: NeighbourMsg = postcard::from_bytes(&body).unwrap();
            let NeighbourMsg::AppDocument { envelope, .. } = &mut message else {
                panic!()
            };
            envelope.seq += 1;
            body = postcard::to_allocvec(&message).unwrap();
        }
        "kind" => {}
        _ => unreachable!(),
    }
    *delta = if change == "kind" {
        CoeditPayload::RetainedDelta { channel, body }
    } else {
        CoeditPayload::Checkpoint { channel, body }
    }
    .encode()
    .unwrap();
    crate::transport::encode(&envelope).unwrap()
}

#[test]
fn fallback_original_proof_reuse_is_limited_to_exact_proved_incoming_slots() {
    for change in ["unchanged", "source", "channel", "kind", "body", "unproved"] {
        let tmp = tempfile::tempdir().unwrap();
        let meta = producer_metadata();
        let authority = RetentionAuthority::new(
            meta["room"].as_str().unwrap(),
            serde_json::from_value(meta["authority"].clone()).unwrap(),
        )
        .unwrap();
        let id = crate::room_workspace_id(authority.room()).unwrap();
        let mut store = WorkspaceStore::open(tmp.path(), id, true).unwrap();
        store.bind_shell_authority(authority).unwrap();
        for row in meta["baseline"].as_array().unwrap() {
            retained(&store, &producer_frame(&meta, row, 1));
        }
        let raw_store = WorkspaceStore::open(tmp.path(), id, true).unwrap();
        let mut apps = Vec::new();
        for row in meta["checkpoint2"].as_array().unwrap() {
            let mut raw = producer_frame(&meta, row, 2);
            let (_, CoeditPayload::Checkpoint { body, .. }) = retained::payload(&raw, id).unwrap()
            else {
                panic!()
            };
            if matches!(
                postcard::from_bytes::<NeighbourMsg>(&body).unwrap(),
                NeighbourMsg::AppDocument { .. }
            ) {
                if apps.len() == 2 {
                    continue;
                }
                if apps.is_empty() && change == "unproved" {
                    raw = changed_snapshot_frame(&raw, "source");
                }
                apps.push(raw.clone());
            }
            retained(&raw_store, &raw);
        }
        assert_eq!(apps.len(), 2);
        let incoming = incoming_identity(&apps[0], id).unwrap();
        let mut selector = Selection::new_batch(
            &store,
            store.root.join("store-work-v1").join("proved-original"),
            vec![incoming],
        )
        .unwrap();
        while !matches!(selector.phase, Phase::Check) {
            selector.step_projection(WorkLimit::default()).unwrap();
        }
        assert!(selector.fallback && selector.basis.room.is_some());
        assert_eq!(selector.incoming_verified, vec![change != "unproved"]);
        let mut inventory = File::open(selector.inventory.as_ref().unwrap()).unwrap();
        let path = loop {
            let meta = read_meta(&mut inventory)
                .unwrap()
                .expect("incoming retained frame");
            let path = store.root.join(meta.source);
            if frame(&path, id)
                .unwrap()
                .is_some_and(|value| value.identity() == incoming)
            {
                break path;
            }
        };
        let expected = if matches!(change, "unchanged" | "unproved") {
            apps[0].clone()
        } else {
            changed_snapshot_frame(&apps[0], change)
        };
        fs::write(&path, &expected).unwrap();
        let outcome = loop {
            if let Step::Done(outcome) = selector.step_projection(WorkLimit::default()).unwrap() {
                break outcome;
            }
        };
        assert_eq!(
            outcome.dispositions,
            vec![IncomingDisposition::Original],
            "{change}"
        );
        assert_eq!(outcome.publication, Publication::Original, "{change}");
        assert_eq!(
            selector.snapshot_verifications.1,
            if change == "unchanged" { 1 } else { 2 },
            "nonincoming and unproved/changed inputs must keep full verification: {change}"
        );
        assert_eq!(
            fs::read(path).unwrap(),
            expected,
            "Original was retired: {change}"
        );
    }
}

#[test]
fn changed_snapshot_input_cannot_reuse_a_paired_proof() {
    for change in ["source", "channel", "kind", "body"] {
        let (_tmp, store) = setup();
        let scratch = |name| store.root.join("store-work-v1").join(name);
        let mut initial = Selection::new(&store, scratch("proof-initial"), None).unwrap();
        while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}
        let mut selector = Selection::new(&store, scratch("proof-change"), None).unwrap();
        while !matches!(selector.phase, Phase::Check) {
            selector.step(WorkLimit::default()).unwrap();
        }
        let route = selector.basis.routes.values().next().unwrap();
        assert!(route.verified_input.is_some());
        let path = store.root.join(&route.file.as_ref().unwrap().source);
        let raw = fs::read(&path).unwrap();
        let mut envelope = crate::transport::decode(&raw[4..]).unwrap();
        let crate::transport::Envelope::Delta {
            author_device_public_key,
            delta,
            ..
        } = &mut envelope
        else {
            panic!()
        };
        let CoeditPayload::Checkpoint {
            mut channel,
            mut body,
        } = CoeditPayload::decode(delta).unwrap()
        else {
            panic!()
        };
        match change {
            "source" => author_device_public_key[0] ^= 1,
            "channel" => channel[0] ^= 1,
            "body" => {
                let mut message: NeighbourMsg = postcard::from_bytes(&body).unwrap();
                let NeighbourMsg::AppDocument { envelope, .. } = &mut message else {
                    panic!()
                };
                envelope.seq += 1; // Preserve the route, invalidate the signature.
                body = postcard::to_allocvec(&message).unwrap();
            }
            "kind" => {}
            _ => unreachable!(),
        }
        *delta = if change == "kind" {
            CoeditPayload::RetainedDelta { channel, body }
        } else {
            CoeditPayload::Checkpoint { channel, body }
        }
        .encode()
        .unwrap();
        fs::write(&path, crate::transport::encode(&envelope).unwrap()).unwrap();

        // Drive the actual Check path. Unchanged records may precede the edited
        // one, but none needs a second proof. The changed input always does.
        while selector.snapshot_verifications.1 == 0 {
            assert!(matches!(selector.phase, Phase::Check), "{change}");
            selector.step(WorkLimit::default()).unwrap();
        }
        assert_eq!(selector.snapshot_verifications.1, 1, "{change}");
        assert_eq!(selector.blocked, change != "kind", "{change}");
        assert!(
            path.exists(),
            "an uncommitted proof check retired its input"
        );
    }
}

#[test]
fn replacement_room_does_not_inherit_old_snapshot_proofs_before_pairing() {
    let (_tmp, store) = setup();
    let mut selector = Selection::new(
        &store,
        store.root.join("store-work-v1").join("proof-room-change"),
        None,
    )
    .unwrap();
    while !matches!(selector.phase, Phase::Pair) {
        selector.step(WorkLimit::default()).unwrap();
    }
    assert!(selector.old.complete());
    assert!(
        selector
            .old
            .routes
            .values()
            .all(|route| route.verified_input.is_some())
    );
    assert_ne!(
        selector.old.room.as_ref().unwrap().heads(),
        selector.basis.room.as_ref().unwrap().heads()
    );
    assert!(!selector.basis.routes.is_empty());
    assert!(
        selector
            .basis
            .routes
            .values()
            .all(|route| { route.file.is_none() && route.verified_input.is_none() })
    );
}

#[test]
fn serial_incomplete_checkpoint_pairs_wait_then_publish_each_route_once() {
    fn contains_original(path: &Path, expected: &[u8]) -> bool {
        fs::read_dir(path).unwrap().any(|entry| {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                contains_original(&entry.path(), expected)
            } else if entry.path().extension().is_some_and(|ext| ext == "delta") {
                replay::frame_bytes(&entry.path()).unwrap().as_deref() == Some(expected)
            } else {
                false
            }
        })
    }
    let tmp = tempfile::tempdir().unwrap();
    let meta = producer_metadata();
    let authority = RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let mut store = WorkspaceStore::open(
        tmp.path(),
        crate::room_workspace_id(authority.room()).unwrap(),
        true,
    )
    .unwrap();
    store.bind_shell_authority(authority.clone()).unwrap();
    for (generation, name) in [(1, "baseline"), (2, "checkpoint2")] {
        let rows = meta[name].as_array().unwrap();
        assert_eq!(
            rows.len(),
            4,
            "real owner fixture is room plus three document routes"
        );
        let mut originals = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let bytes = producer_frame(&meta, row, generation);
            let (_, CoeditPayload::Checkpoint { channel, .. }) =
                retained::payload(&bytes, store.workspace_id()).unwrap()
            else {
                panic!("checkpoint fixture")
            };
            originals.push(bytes.clone());
            let mut build = store.start_retain(&channel, None, bytes).unwrap();
            let outcome = loop {
                if let Step::Done(outcome) = build
                    .step(WorkLimit {
                        entries: 1,
                        ..Default::default()
                    })
                    .unwrap()
                {
                    break outcome;
                }
            };
            if index + 1 < rows.len() {
                // Reopen and complete selected GC: suppression must not discard the
                // only durable copy while the rest of the checkpoint is absent.
                store = WorkspaceStore::open(tmp.path(), store.workspace_id(), true).unwrap();
                store.bind_shell_authority(authority.clone()).unwrap();
                let mut selected = SelectedRead::new(&store).unwrap().unwrap();
                while matches!(selected.step().unwrap(), Step::More) {}
                for original in &originals {
                    assert!(
                        contains_original(&store.root.join("retained"), original),
                        "suppressed input lost durable retained custody"
                    );
                }
                assert_eq!(
                    outcome.publication,
                    Publication::Suppress,
                    "{name} input {index} is a verified incomplete pair, not an opaque original"
                );
            } else {
                let Publication::Activate(path) = outcome.publication else {
                    panic!("complete {name} must activate one coherent basis")
                };
                let mut cursor = replay::PublicationCursor::open(&path).unwrap();
                let mut delivered = Vec::new();
                loop {
                    match cursor.step(crate::persistence::output_budget()).unwrap() {
                        Step::More => {}
                        Step::Done(None) => break,
                        Step::Done(Some(mut batch)) => {
                            while let Some(frame) = batch.front() {
                                delivered.push(crate::transport::encode(frame).unwrap());
                                batch.pop_written();
                            }
                        }
                    }
                }
                originals.sort();
                delivered.sort();
                assert_eq!(
                    delivered, originals,
                    "each exact canonical frame must publish once"
                );
            }
        }
    }
}

#[test]
fn serial_checkpoint_refresh_reference_repeats_global_selection_per_frame() {
    let tmp = tempfile::tempdir().unwrap();
    let meta = producer_metadata();
    let authority = RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let mut store = WorkspaceStore::open(tmp.path(), id, true).unwrap();
    store.bind_shell_authority(authority).unwrap();
    for row in meta["baseline"].as_array().unwrap() {
        retained(&store, &producer_frame(&meta, row, 1));
    }
    let scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut initial = Selection::new(&store, scratch, None).unwrap();
    while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}
    let mut global_selections = 0;
    let mut global_reads = 0;
    for row in meta["checkpoint2"].as_array().unwrap() {
        let bytes = producer_frame(&meta, row, 2);
        let (_, payload) = retained::payload(&bytes, store.workspace_id()).unwrap();
        let CoeditPayload::Checkpoint { channel, .. } = payload else {
            panic!("coherent refresh fixture contains a retained delta")
        };
        let mut build = store.start_retain(&channel, None, bytes).unwrap();
        while matches!(build.step(WorkLimit::default()).unwrap(), Step::More) {}
        if let Some(stats) = build.selection_stats() {
            global_selections += 1;
            global_reads += stats.preparation_reads;
        }
    }

    assert_eq!(global_selections, 4);
    assert_eq!(global_reads, 100);
}

#[test]
fn projected_checkpoint_prefix_classifies_every_input_in_one_selection() {
    let (_tmp, store) = setup();
    let meta = producer_metadata();
    let incoming = meta["checkpoint2"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| incoming_identity(&producer_frame(&meta, row, 2), store.workspace_id()).unwrap())
        .collect::<Vec<_>>();
    let scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut selector = Selection::new_batch(&store, scratch, incoming).unwrap();
    let outcome = loop {
        if let Step::Done(outcome) = selector.step(WorkLimit::default()).unwrap() {
            break outcome;
        }
    };
    let SelectionOutcome::BatchPublished {
        supplemental_recovery,
        publication,
        dispositions,
    } = outcome
    else {
        panic!("coherent checkpoint projection did not finish")
    };
    assert!(supplemental_recovery.is_none());
    assert!(matches!(publication, Publication::Activate(_)));
    assert_eq!(dispositions.len(), 4);
    assert!(
        dispositions
            .iter()
            .all(|value| *value != IncomingDisposition::Original),
        "a coherent checkpoint projection left an input unrepresented: {dispositions:?}"
    );
    assert!(selector.stats().preparation_reads < 40);
}

#[test]
fn unchanged_all_covered_projected_prefix_suppresses_and_switches_current() {
    let (_tmp, store) = setup();
    let meta = producer_metadata();
    let selected_scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut selected = Selection::new(&store, selected_scratch, None).unwrap();
    while matches!(selected.step(WorkLimit::default()).unwrap(), Step::More) {}
    let before = retained::current(&store.root.join(DIRECTORY))
        .unwrap()
        .unwrap();

    // These older signed shell checkpoints are opaque to raw-Automerge
    // retention. Production CheckpointStage therefore preserves the active
    // checkpoint2 generation and appends each exact baseline input. The
    // unchanged selected checkpoint2 basis proves every appended input Covered.
    let root = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    fs::create_dir(&root).unwrap();
    let mut stages = Vec::new();
    let mut incoming = Vec::new();
    for (index, row) in meta["baseline"].as_array().unwrap().iter().enumerate() {
        let bytes = producer_frame(&meta, row, 3);
        let (_, CoeditPayload::Checkpoint { channel, .. }) =
            retained::payload(&bytes, store.workspace_id()).unwrap()
        else {
            panic!()
        };
        incoming.push(incoming_identity(&bytes, store.workspace_id()).unwrap());
        let mut stage = store
            .start_staged_checkpoint(&channel, bytes, root.join(format!("input-{index:02}")))
            .unwrap();
        loop {
            match stage.step(WorkLimit::default()).unwrap() {
                Step::More => {}
                Step::Done(eligible) => {
                    assert!(eligible, "opaque exact history must remain projectable");
                    break;
                }
            }
        }
        stages.push(stage);
    }
    let overrides = stages
        .iter()
        .map(|stage| {
            (
                stage.lineage_name().to_os_string(),
                stage.projected_generation().to_path_buf(),
            )
        })
        .collect();
    let mut selector =
        Selection::new_projected(&store, root.join("selection"), incoming, overrides).unwrap();
    let projected = loop {
        if let Step::Done(outcome) = selector.step_projection(WorkLimit::default()).unwrap() {
            break outcome;
        }
    };
    assert_eq!(projected.publication, Publication::Suppress);
    assert_eq!(projected.reason, BatchProjectionReason::Represented);
    assert!(
        projected
            .dispositions
            .iter()
            .all(|value| *value == IncomingDisposition::Covered),
        "unchanged projected inputs were not all proven Covered: {:?}",
        projected.dispositions
    );
    assert_eq!(
        retained::current(&store.root.join(DIRECTORY))
            .unwrap()
            .unwrap(),
        before,
        "projection switched selected CURRENT before lineage promotion"
    );

    for stage in &mut stages {
        stage.promote().unwrap();
    }
    let committed = loop {
        if let Step::Done(outcome) = selector.step(WorkLimit::default()).unwrap() {
            break outcome;
        }
    };
    let SelectionOutcome::BatchPublished {
        supplemental_recovery,
        publication,
        dispositions,
    } = committed
    else {
        panic!("projected batch did not publish a batch outcome")
    };
    assert_eq!(supplemental_recovery, projected.supplemental_recovery);
    assert_eq!(publication, Publication::Suppress);
    assert_eq!(dispositions, projected.dispositions);
    assert_ne!(
        retained::current(&store.root.join(DIRECTORY))
            .unwrap()
            .unwrap(),
        before,
        "unchanged projected cut did not durably switch selected CURRENT"
    );
    for stage in &mut stages {
        while matches!(stage.finish_gc(WorkLimit::default()).unwrap(), Step::More) {}
    }
    let mut reopened = SelectedRead::new(&store).unwrap().unwrap();
    while matches!(reopened.step().unwrap(), Step::More) {}
    assert!(reopened.basis.complete());
}

#[test]
fn successful_old_basis_fallback_keeps_newer_checkpoint_original() {
    rejected_checkpoint_fixture(false);
}

#[test]
fn serial_suppressed_prefix_rejected_cut_requires_coherent_release_or_replay() {
    rejected_checkpoint_fixture(true);
}

fn rejected_checkpoint_fixture(serial: bool) {
    use automerge::transaction::Transactable;

    fn signed(
        key: &ed25519_dalek::SigningKey,
        room: &str,
        mutation: collab_types::DocumentMutation,
        seq: u64,
        now: web_time::SystemTime,
    ) -> qos_peer_state::NeighbourMsg {
        let source = key.verifying_key().to_bytes();
        let mut envelope = collab_types::SignedOp::new_unsigned(
            mutation.encode().unwrap(),
            collab_types::document_artifact_id("selector.app", room, "one"),
            collab_types::Ed25519PublicKey::from_bytes(source),
            collab_types::document_attestation_id("selector.app", room, "one"),
            collab_types::OpClass::Splice,
            now,
            seq,
        );
        collab_types::sign_op(&mut envelope, key).unwrap();
        qos_peer_state::NeighbourMsg::AppDocument {
            peer: qos_peer_state::peer_handle(&source),
            app_id: "selector.app".into(),
            cluster_id: room.into(),
            record_id: "one".into(),
            envelope: Box::new(envelope),
        }
    }
    fn frame(
        authority: &RetentionAuthority,
        message: &qos_peer_state::NeighbourMsg,
        checkpoint: bool,
    ) -> Vec<u8> {
        let channel = qos_peer_state::transport::retention(message, checkpoint)
            .unwrap()
            .0;
        let body = postcard::to_allocvec(message).unwrap();
        let payload = if checkpoint {
            CoeditPayload::Checkpoint { channel, body }
        } else {
            CoeditPayload::RetainedDelta { channel, body }
        };
        let source = match message {
            qos_peer_state::NeighbourMsg::AppDocument { envelope, .. } => {
                *envelope.author.as_bytes()
            }
            _ => authority.governor().device_public_key,
        };
        crate::transport::encode(&crate::transport::Envelope::Delta {
            workspace_id: crate::room_workspace_id(authority.room()).unwrap(),
            author_device_public_key: source,
            authored_at_ns: 1,
            signature: [0; 64],
            delta: payload.encode().unwrap(),
        })
        .unwrap()
    }

    let root_key = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root_key.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "selector fallback fixture".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root_key).unwrap();
    let now = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_secs(2000);
    let room = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let mut owner = qos_peer_state::StatePeer::new(
        room,
        cert.clone(),
        vec![],
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    let mut document = automerge::AutoCommit::new();
    document.put(automerge::ROOT, "value", "baseline").unwrap();
    owner
        .install_seed(
            qos_peer_state::SeedState {
                room_changes: qos_peer_state::seed_room(room, &cert, &[cert.clone()]).unwrap(),
                documents: vec![qos_peer_state::SeedDocument {
                    app_id: "selector.app".into(),
                    record_id: "one".into(),
                    kind: "test".into(),
                    bytes: document.save(),
                }],
            },
            &key,
            now,
        )
        .unwrap();
    let authority = RetentionAuthority::new(room, cert).unwrap();
    let cp1 = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
    let qos_peer_state::NeighbourMsg::AppDocument { envelope, .. } = &cp1[1] else {
        panic!()
    };
    let next_sequence = envelope.seq + 1;

    // The prospective checkpoint admits branch A at next_sequence. A signed
    // concurrent branch-B tail at that same sequence is below its floor and
    // therefore blocks the prospective cut, while the old CP1 basis can still
    // append it in sequence. This makes fallback complete rather than return an
    // unresolved outcome.
    let baseline = owner.document("selector.app", "one").unwrap();
    let mut branch_a = automerge::AutoCommit::load(&baseline).unwrap();
    branch_a.set_actor(automerge::ActorId::from(vec![71]));
    branch_a.put(automerge::ROOT, "candidate", "A").unwrap();
    branch_a.commit();
    let candidate = signed(
        &key,
        authority.room(),
        collab_types::DocumentMutation::Snapshot(branch_a.save()),
        next_sequence,
        now,
    );
    let qos_peer_state::NeighbourMsg::AppDocument { envelope, .. } = candidate else {
        panic!()
    };
    owner
        .admit_local_app_document("selector.app", "one", *envelope, now)
        .unwrap();
    let cp2 = owner.durable_bootstrap_messages(&key, now, 2).unwrap();

    let mut branch_b = automerge::AutoCommit::load(&baseline).unwrap();
    branch_b.set_actor(automerge::ActorId::from(vec![72]));
    let baseline_heads = branch_b.get_heads();
    branch_b.put(automerge::ROOT, "fallback", "B").unwrap();
    branch_b.commit();
    let tail = signed(
        &key,
        authority.room(),
        collab_types::DocumentMutation::Incremental(
            branch_b
                .get_changes(&baseline_heads)
                .iter()
                .flat_map(|change| change.raw_bytes().to_vec())
                .collect(),
        ),
        next_sequence,
        now,
    );

    let tmp = tempfile::tempdir().unwrap();
    let mut store = WorkspaceStore::open(
        tmp.path(),
        crate::room_workspace_id(authority.room()).unwrap(),
        true,
    )
    .unwrap();
    store.bind_shell_authority(authority.clone()).unwrap();
    for message in &cp1 {
        retained(&store, &frame(&authority, message, true));
    }
    let initial_scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut initial = Selection::new(&store, initial_scratch, None).unwrap();
    while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}
    retained(&store, &frame(&authority, &tail, false));
    let selected_before = retained::current(&store.root.join(DIRECTORY))
        .unwrap()
        .unwrap();

    if serial {
        for (index, message) in cp2.iter().enumerate() {
            let bytes = frame(&authority, message, true);
            let channel = qos_peer_state::transport::retention(message, true)
                .unwrap()
                .0;
            let mut retain = store.start_retain(&channel, None, bytes).unwrap();
            let outcome = loop {
                if let Step::Done(outcome) = retain
                    .step(WorkLimit {
                        entries: 1,
                        ..Default::default()
                    })
                    .unwrap()
                {
                    break outcome;
                }
            };
            if index == 0 {
                assert_eq!(outcome.publication, Publication::Suppress);
            } else {
                assert!(
                    matches!(outcome.publication, Publication::ReplayRequired(_)),
                    "previously suppressed serial prerequisite requires the complete fallback, got {:?}",
                    outcome.publication
                );
            }
        }
        return;
    }

    let root = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    fs::create_dir(&root).unwrap();
    let mut stages = Vec::new();
    let mut incoming = Vec::new();
    for (index, message) in cp2.iter().enumerate() {
        let bytes = frame(&authority, message, true);
        let (_, CoeditPayload::Checkpoint { channel, .. }) =
            retained::payload(&bytes, store.workspace_id()).unwrap()
        else {
            panic!()
        };
        incoming.push(incoming_identity(&bytes, store.workspace_id()).unwrap());
        let mut stage = store
            .start_staged_checkpoint(&channel, bytes, root.join(format!("input-{index:02}")))
            .unwrap();
        loop {
            match stage.step(WorkLimit::default()).unwrap() {
                Step::More => {}
                Step::Done(eligible) => {
                    assert!(eligible, "signed checkpoint fixture must be projectable");
                    break;
                }
            }
        }
        stages.push(stage);
    }
    let overrides = stages
        .iter()
        .map(|stage| {
            (
                stage.lineage_name().to_os_string(),
                stage.projected_generation().to_path_buf(),
            )
        })
        .collect();
    let mut selector =
        Selection::new_projected(&store, root.join("selection"), incoming, overrides).unwrap();
    let mut saw_provisional = false;
    let mut saw_cleared_fallback = false;
    let outcome = loop {
        let was_fallback = selector.fallback;
        let result = selector
            .step_projection(WorkLimit {
                entries: 1,
                ..Default::default()
            })
            .unwrap();
        if !selector.fallback
            && selector
                .incoming_dispositions
                .iter()
                .all(|value| *value == Some(IncomingDisposition::Covered))
        {
            saw_provisional = true;
        }
        if !was_fallback && selector.fallback {
            saw_cleared_fallback = selector.incoming_dispositions.iter().all(Option::is_none);
        }
        if let Step::Done(outcome) = result {
            break outcome;
        }
    };
    assert!(
        saw_provisional,
        "prospective basis never classified the prefix Covered"
    );
    assert!(
        saw_cleared_fallback,
        "prospective dispositions survived the switch back to the old basis"
    );
    assert!(
        matches!(selector.phase, Phase::Commit),
        "fixture returned unresolved instead of completing the old-basis fallback"
    );
    assert_eq!(outcome.publication, Publication::Original);
    assert!(
        outcome.supplemental_recovery.is_none(),
        "full rejected batch has no external prefix"
    );
    assert_eq!(outcome.reason, BatchProjectionReason::FinalBasisOmission);
    assert!(
        outcome
            .dispositions
            .iter()
            .all(|value| *value == IncomingDisposition::Original),
        "newer checkpoint was suppressed even though the old-basis cut omits it: {:?}",
        outcome.dispositions
    );
    assert_eq!(
        retained::current(&store.root.join(DIRECTORY))
            .unwrap()
            .unwrap(),
        selected_before,
        "projection switched selected CURRENT before a serial retry"
    );
}

#[test]
fn projected_checkpoint_prefix_exposes_the_route_count_cutoff() {
    use automerge::transaction::Transactable;

    fn outcome(document_count: usize) -> BatchSelectionOutcome {
        fn frame(
            authority: &RetentionAuthority,
            message: &qos_peer_state::NeighbourMsg,
        ) -> Vec<u8> {
            let channel = qos_peer_state::transport::retention(message, true)
                .unwrap()
                .0;
            let body = postcard::to_allocvec(message).unwrap();
            let source = match message {
                qos_peer_state::NeighbourMsg::AppDocument { envelope, .. } => {
                    *envelope.author.as_bytes()
                }
                _ => authority.governor().device_public_key,
            };
            crate::transport::encode(&crate::transport::Envelope::Delta {
                workspace_id: crate::room_workspace_id(authority.room()).unwrap(),
                author_device_public_key: source,
                authored_at_ns: 1,
                signature: [0; 64],
                delta: CoeditPayload::Checkpoint { channel, body }
                    .encode()
                    .unwrap(),
            })
            .unwrap()
        }

        let root_key = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[document_count as u8; 32]);
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root_key.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("{document_count} route prefix fixture"),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root_key).unwrap();
        let now = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_secs(3000);
        let room = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        let mut owner = qos_peer_state::StatePeer::new(
            room,
            cert.clone(),
            vec![],
            tilo_user_identity::RevocationList::new(),
            now,
        )
        .unwrap();
        let documents = (0..document_count)
            .map(|index| {
                let mut document = automerge::AutoCommit::new();
                document
                    .put(automerge::ROOT, "index", index as u64)
                    .unwrap();
                qos_peer_state::SeedDocument {
                    app_id: "threshold.app".into(),
                    record_id: format!("record-{index:02}"),
                    kind: "test".into(),
                    bytes: document.save(),
                }
            })
            .collect();
        owner
            .install_seed(
                qos_peer_state::SeedState {
                    room_changes: qos_peer_state::seed_room(room, &cert, &[cert.clone()]).unwrap(),
                    documents,
                },
                &key,
                now,
            )
            .unwrap();
        let authority = RetentionAuthority::new(room, cert).unwrap();
        let baseline = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
        let next = owner.durable_bootstrap_messages(&key, now, 2).unwrap();
        assert_eq!(baseline.len(), document_count + 1);
        assert_eq!(next.len(), document_count + 1);

        let tmp = tempfile::tempdir().unwrap();
        let mut store = WorkspaceStore::open(
            tmp.path(),
            crate::room_workspace_id(authority.room()).unwrap(),
            true,
        )
        .unwrap();
        store.bind_shell_authority(authority.clone()).unwrap();
        for message in &baseline {
            retained(&store, &frame(&authority, message));
        }
        let initial_root = store.root.join("store-work-v1").join(format!(
            "retain-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let mut initial = Selection::new(&store, initial_root, None).unwrap();
        while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}

        // Native publication commits the newly signed room recovery checkpoint
        // before the capped run of per-record checkpoints reaches the relay.
        let room_frame = frame(&authority, &next[0]);
        retained(&store, &room_frame);
        let room_root = store.root.join("store-work-v1").join(format!(
            "retain-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let mut room_selection = Selection::new(
            &store,
            room_root,
            incoming_identity(&room_frame, store.workspace_id()),
        )
        .unwrap();
        let room_publication = loop {
            match room_selection.step(WorkLimit::default()).unwrap() {
                Step::More => {}
                Step::Done(SelectionOutcome::Published(publication)) => break publication,
                Step::Done(other) => panic!("unexpected room checkpoint result: {other:?}"),
            }
        };
        assert_eq!(room_publication, Publication::Suppress);

        let root = store.root.join("store-work-v1").join(format!(
            "retain-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        fs::create_dir(&root).unwrap();
        let mut stages = Vec::new();
        let mut incoming = Vec::new();
        for (index, message) in next.iter().skip(1).take(32).enumerate() {
            let bytes = frame(&authority, message);
            let (_, CoeditPayload::Checkpoint { channel, .. }) =
                retained::payload(&bytes, store.workspace_id()).unwrap()
            else {
                panic!()
            };
            incoming.push(incoming_identity(&bytes, store.workspace_id()).unwrap());
            let mut stage = store
                .start_staged_checkpoint(&channel, bytes, root.join(format!("input-{index:02}")))
                .unwrap();
            while matches!(stage.step(WorkLimit::default()).unwrap(), Step::More) {}
            stages.push(stage);
        }
        let overrides = stages
            .iter()
            .map(|stage| {
                (
                    stage.lineage_name().to_os_string(),
                    stage.projected_generation().to_path_buf(),
                )
            })
            .collect();
        let mut selector =
            Selection::new_projected(&store, root.join("selection"), incoming, overrides).unwrap();
        loop {
            if let Step::Done(outcome) = selector.step_projection(WorkLimit::default()).unwrap() {
                if document_count > 32 {
                    assert_eq!(
                        selector.snapshot_verifications.1, 0,
                        "a proved newer Original needs no repeated signature proof against the older fallback room"
                    );
                }
                return outcome;
            }
        }
    }

    let complete = outcome(32);
    assert_eq!(complete.reason, BatchProjectionReason::Represented);
    assert!(
        complete
            .dispositions
            .iter()
            .all(|value| *value != IncomingDisposition::Original)
    );
    for document_count in [33, 34] {
        let truncated = outcome(document_count);
        assert_eq!(
            truncated.reason,
            BatchProjectionReason::FinalBasisOmission,
            "{document_count} records unexpectedly fit in the 32-frame projection"
        );
        assert!(
            truncated
                .dispositions
                .iter()
                .all(|value| *value == IncomingDisposition::Original),
            "{document_count} record fallback was not the observed all-Original cut: {:?}",
            truncated.dispositions
        );
    }
}

#[test]
fn exact_history_checkpoint_prefix_stays_inactive_before_projected_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let meta = producer_metadata();
    let authority = RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let mut store = WorkspaceStore::open(tmp.path(), id, true).unwrap();
    store.bind_shell_authority(authority).unwrap();
    for row in meta["baseline"].as_array().unwrap() {
        retained(&store, &producer_frame(&meta, row, 1));
    }
    let initial_scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut initial = Selection::new(&store, initial_scratch, None).unwrap();
    while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}
    let selected_before = fs::read(store.root.join(DIRECTORY).join("CURRENT")).unwrap();
    let before = fs::read_dir(store.root.join("retained/v2"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name(),
                fs::read(entry.path().join("CURRENT")).unwrap(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let batch_root = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    fs::create_dir(&batch_root).unwrap();
    let mut stages = Vec::new();
    let mut incoming = Vec::new();
    for (index, row) in meta["checkpoint2"].as_array().unwrap().iter().enumerate() {
        let bytes = producer_frame(&meta, row, 2);
        let (_, payload) = retained::payload(&bytes, store.workspace_id()).unwrap();
        let CoeditPayload::Checkpoint { channel, .. } = payload else {
            panic!()
        };
        incoming.push(incoming_identity(&bytes, store.workspace_id()).unwrap());
        let mut stage = store
            .start_staged_checkpoint(
                &channel,
                bytes,
                batch_root.join(format!("input-{index:02}")),
            )
            .unwrap();
        loop {
            match stage.step(WorkLimit::default()).unwrap() {
                Step::More => {}
                Step::Done(eligible) => {
                    assert!(eligible, "exact history checkpoint must be projectable");
                    break;
                }
            }
        }
        stages.push(stage);
    }
    assert_eq!(incoming.len(), stages.len());
    let after = fs::read_dir(store.root.join("retained/v2"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name(),
                fs::read(entry.path().join("CURRENT")).unwrap(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(after, before, "projection switched a lineage CURRENT");
    assert_eq!(
        fs::read(store.root.join(DIRECTORY).join("CURRENT")).unwrap(),
        selected_before,
        "projection switched selected CURRENT"
    );
}

#[test]
fn atomic_checkpoint_prefix_commit_matches_serial_replay() {
    fn baseline(meta: &serde_json::Value) -> (tempfile::TempDir, WorkspaceStore) {
        let tmp = tempfile::tempdir().unwrap();
        let authority = RetentionAuthority::new(
            meta["room"].as_str().unwrap(),
            serde_json::from_value(meta["authority"].clone()).unwrap(),
        )
        .unwrap();
        let id = crate::room_workspace_id(authority.room()).unwrap();
        let mut store = WorkspaceStore::open(tmp.path(), id, true).unwrap();
        store.bind_shell_authority(authority).unwrap();
        for row in meta["baseline"].as_array().unwrap() {
            retained(&store, &producer_frame(meta, row, 1));
        }
        let scratch = store.root.join("store-work-v1").join(format!(
            "retain-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let mut initial = Selection::new(&store, scratch, None).unwrap();
        while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}
        (tmp, store)
    }

    let meta = producer_metadata();
    let (_serial_tmp, serial) = baseline(&meta);
    for row in meta["checkpoint2"].as_array().unwrap() {
        let bytes = producer_frame(&meta, row, 2);
        let (_, payload) = retained::payload(&bytes, serial.workspace_id()).unwrap();
        let CoeditPayload::Checkpoint { channel, .. } = payload else {
            panic!()
        };
        let mut build = serial.start_retain(&channel, None, bytes).unwrap();
        while matches!(build.step(WorkLimit::default()).unwrap(), Step::More) {}
    }
    let serial_replay = crate::store::shell_selection_tests::replay_frames(&serial);

    let (_batch_tmp, batch) = baseline(&meta);
    let root = batch.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    fs::create_dir(&root).unwrap();
    let mut stages = Vec::new();
    let mut incoming = Vec::new();
    for (index, row) in meta["checkpoint2"].as_array().unwrap().iter().enumerate() {
        let bytes = producer_frame(&meta, row, 2);
        let (_, payload) = retained::payload(&bytes, batch.workspace_id()).unwrap();
        let CoeditPayload::Checkpoint { channel, .. } = payload else {
            panic!()
        };
        incoming.push(incoming_identity(&bytes, batch.workspace_id()).unwrap());
        let mut stage = batch
            .start_staged_checkpoint(&channel, bytes, root.join(format!("input-{index:02}")))
            .unwrap();
        loop {
            match stage.step(WorkLimit::default()).unwrap() {
                Step::More => {}
                Step::Done(eligible) => {
                    assert!(eligible, "exact history checkpoint must be projectable");
                    break;
                }
            }
        }
        stages.push(stage);
    }
    for (index, stage) in stages.iter_mut().enumerate() {
        stage.promote().unwrap();
        let mut selector = Selection::new(
            &batch,
            root.join(format!("serial-{index:02}")),
            Some(incoming[index]),
        )
        .unwrap();
        loop {
            if matches!(selector.step(WorkLimit::default()).unwrap(), Step::Done(_)) {
                break;
            }
        }
        while matches!(stage.finish_gc(WorkLimit::default()).unwrap(), Step::More) {}
    }
    assert_eq!(
        crate::store::shell_selection_tests::replay_frames(&batch),
        serial_replay
    );
}

#[test]
fn promoted_prefix_repairs_on_reopen_and_suffix_republish_completes_the_cut() {
    fn opened(meta: &serde_json::Value) -> (tempfile::TempDir, WorkspaceStore) {
        let tmp = tempfile::tempdir().unwrap();
        let authority = RetentionAuthority::new(
            meta["room"].as_str().unwrap(),
            serde_json::from_value(meta["authority"].clone()).unwrap(),
        )
        .unwrap();
        let id = crate::room_workspace_id(authority.room()).unwrap();
        let mut store = WorkspaceStore::open(tmp.path(), id, true).unwrap();
        store.bind_shell_authority(authority).unwrap();
        for row in meta["baseline"].as_array().unwrap() {
            retained(&store, &producer_frame(meta, row, 1));
        }
        let scratch = store.root.join("store-work-v1").join(format!(
            "retain-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let mut selection = Selection::new(&store, scratch, None).unwrap();
        while matches!(selection.step(WorkLimit::default()).unwrap(), Step::More) {}
        (tmp, store)
    }

    let meta = producer_metadata();
    let rows = meta["checkpoint2"].as_array().unwrap();
    let (_expected_tmp, expected) = opened(&meta);
    for row in &rows[..2] {
        let bytes = producer_frame(&meta, row, 2);
        let (_, CoeditPayload::Checkpoint { channel, .. }) =
            retained::payload(&bytes, expected.workspace_id()).unwrap()
        else {
            panic!()
        };
        let mut build = expected.start_retain(&channel, None, bytes).unwrap();
        while matches!(build.step(WorkLimit::default()).unwrap(), Step::More) {}
    }
    let complete = crate::store::shell_selection_tests::replay_frames(&expected);

    let (recovery_tmp, recovery) = opened(&meta);
    let channels = rows[..2]
        .iter()
        .map(|row| {
            let bytes = producer_frame(&meta, row, 2);
            let (_, CoeditPayload::Checkpoint { channel, .. }) =
                retained::payload(&bytes, recovery.workspace_id()).unwrap()
            else {
                panic!()
            };
            channel
        })
        .collect::<Vec<_>>();
    let recovery_root = recovery.root.clone();
    let lineage = |channel: [u8; 32]| {
        recovery_root.join("retained/v2").join(format!(
            "{}-{}",
            hex::encode(
                serde_json::from_value::<tilo_user_identity::DeviceCert>(meta["authority"].clone())
                    .unwrap()
                    .device_public_key
            ),
            hex::encode(channel)
        ))
    };
    let before = channels
        .iter()
        .map(|channel| retained::current(&lineage(*channel)).unwrap().unwrap())
        .collect::<Vec<_>>();
    let root = recovery.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    fs::create_dir(&root).unwrap();
    let mut staged = Vec::new();
    for (index, row) in rows[..2].iter().enumerate() {
        let bytes = producer_frame(&meta, row, 2);
        let (_, CoeditPayload::Checkpoint { channel, .. }) =
            retained::payload(&bytes, recovery.workspace_id()).unwrap()
        else {
            panic!()
        };
        let mut stage = recovery
            .start_staged_checkpoint(&channel, bytes, root.join(format!("input-{index}")))
            .unwrap();
        while matches!(stage.step(WorkLimit::default()).unwrap(), Step::More) {}
        staged.push(stage);
    }
    staged[0].promote().unwrap();
    drop(staged);
    drop(recovery);

    let authority = RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let mut reopened = WorkspaceStore::open(recovery_tmp.path(), id, true).unwrap();
    reopened.bind_shell_authority(authority).unwrap();
    let repaired = crate::store::shell_selection_tests::replay_frames(&reopened);
    assert!(
        !repaired.is_empty(),
        "promoted prefix repairs into a replayable cut"
    );
    assert_ne!(
        retained::current(&lineage(channels[0])).unwrap().unwrap(),
        before[0]
    );
    assert_eq!(
        retained::current(&lineage(channels[1])).unwrap().unwrap(),
        before[1],
        "unpromoted suffix must not switch its lineage CURRENT"
    );

    let suffix = producer_frame(&meta, &rows[1], 2);
    let (_, CoeditPayload::Checkpoint { channel, .. }) =
        retained::payload(&suffix, reopened.workspace_id()).unwrap()
    else {
        panic!()
    };
    let mut build = reopened.start_retain(&channel, None, suffix).unwrap();
    while matches!(build.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert_eq!(
        crate::store::shell_selection_tests::replay_frames(&reopened),
        complete,
        "current checkpoint republish is accepted or semantically suppressed after repair"
    );
}
#[test]
fn selected_switch_failure_keeps_previous_durable_basis_and_retires_no_input() {
    let (_tmp, store) = setup();
    let before = retained::current(&store.root.join(DIRECTORY)).unwrap();
    let scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut selector = Selection::new(&store, scratch, None).unwrap();
    while !matches!(selector.phase, Phase::Commit) {
        assert!(matches!(
            selector
                .step(WorkLimit {
                    entries: 1,
                    ..Default::default()
                })
                .unwrap(),
            Step::More
        ));
    }
    let moved = selector.staged.with_extension("unavailable");
    fs::rename(&selector.staged, &moved).unwrap();
    assert!(
        selector.step(WorkLimit::default()).is_err(),
        "required directory sync fails before selector switch"
    );
    assert_eq!(
        retained::current(&store.root.join(DIRECTORY)).unwrap(),
        before
    );
    let mut plan = File::open(moved.join("gc.manifest")).unwrap();
    let mut count = 0;
    while let Some(meta) = read_meta(&mut plan).unwrap() {
        assert!(store.root.join(meta.source).exists());
        count += 1
    }
    assert!(count > 0);
    fs::rename(moved, &selector.staged).unwrap();
    while matches!(selector.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert_ne!(
        retained::current(&store.root.join(DIRECTORY)).unwrap(),
        before
    );
}
#[test]
fn durable_retirement_plan_preserves_reused_path_and_selected_references_survive_gc() {
    let (_tmp, store) = setup();
    let scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut selector = Selection::new(&store, scratch, None).unwrap();
    while !matches!(selector.phase, Phase::Gc) {
        selector.step(WorkLimit::default()).unwrap();
    }
    let mut plan = File::open(selector.staged.join("gc.manifest")).unwrap();
    let target = read_meta(&mut plan).unwrap().unwrap();
    let path = store.root.join(target.source);
    let bytes = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::write(&path, &bytes).unwrap();
    while matches!(
        selector
            .step(WorkLimit {
                entries: 1,
                ..Default::default()
            })
            .unwrap(),
        Step::More
    ) {}
    assert_eq!(
        fs::read(path).unwrap(),
        bytes,
        "new inode at a retired name is not the captured target"
    );
    let mut selected = SelectedRead::new(&store).unwrap().unwrap();
    while matches!(selected.step().unwrap(), Step::More) {}
    assert!(selected.basis.complete());
}
#[test]
fn shell_selection_spills_history_metadata_and_preserves_unknown_files() {
    let (_tmp, store) = setup();
    let raw = WorkspaceStore::open(
        store.root.parent().unwrap().parent().unwrap(),
        store.workspace_id,
        true,
    )
    .unwrap();
    let bytes = crate::transport::encode(&crate::transport::Envelope::Delta {
        workspace_id: store.workspace_id,
        author_device_public_key: [77; 32],
        authored_at_ns: 1,
        signature: [0; 64],
        delta: CoeditPayload::RetainedDelta {
            channel: [88; 32],
            body: vec![9; 64],
        }
        .encode()
        .unwrap(),
    })
    .unwrap();
    raw.retain_delta(&[88; 32], 1, &bytes).unwrap();
    let lineage = store.root.join("retained/v2").join(format!(
        "{}-{}",
        hex::encode([77; 32]),
        hex::encode([88; 32])
    ));
    let active = retained::current(&lineage).unwrap().unwrap();
    for i in 2..602 {
        fs::write(active.join(format!("{i:016x}.delta")), &bytes).unwrap();
    }
    let scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut selector = Selection::new(&store, scratch, None).unwrap();
    let mut steps = 0;
    loop {
        let result = selector
            .step(WorkLimit {
                entries: 1,
                ..Default::default()
            })
            .unwrap();
        assert!(selector.stats().last_step_work <= 1);
        steps += 1;
        if matches!(result, Step::Done(_)) {
            break;
        }
    }
    assert!(steps > 600);
    assert!(selector.stats().peak_metadata_entries <= 128);
    assert!(selector.stats().peak_metadata_bytes <= 512 * 1024);
    assert_eq!(
        fs::read_dir(active).unwrap().count(),
        601,
        "opaque foreign history has no retirement proof"
    );
}

#[test]
fn retirement_pin_survives_failed_source_barrier_on_retry_and_reopen() {
    let (_tmp, store) = setup();
    let scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut selector = Selection::new(&store, scratch, None).unwrap();
    while !matches!(selector.phase, Phase::Gc) {
        selector.step(WorkLimit::default()).unwrap();
    }
    let generation = selector.staged.clone();
    let mut plan = File::open(generation.join("gc.manifest")).unwrap();
    let target = read_meta(&mut plan).unwrap().unwrap();
    let source = store.root.join(target.source);
    let pin = generation.join(format!("gc-{:016x}.delta", target.pin));
    let fault = crate::store::test_barrier::fail(source.parent().unwrap(), "sync-dir");
    assert!(selector.step(WorkLimit::default()).is_err());
    assert!(
        !source.exists(),
        "injected barrier failure follows the real source unlink"
    );
    assert!(
        pin.exists(),
        "failed source barrier retains its durable witness"
    );
    assert!(
        selector.step(WorkLimit::default()).is_err(),
        "NotFound retry still requires source barrier"
    );
    assert!(
        pin.exists(),
        "retry must not retire witness without source barrier"
    );
    drop(selector);
    let mut reopened_store = WorkspaceStore::open(
        store.root.parent().unwrap().parent().unwrap(),
        store.workspace_id,
        true,
    )
    .unwrap();
    reopened_store
        .bind_shell_authority(store.shell_authority().unwrap().clone())
        .unwrap();
    let mut reopened = SelectedRead::new(&reopened_store).unwrap().unwrap();
    loop {
        match reopened.step() {
            Ok(Step::More) => (),
            Ok(Step::Done(())) => panic!("reopen bypassed failed source barrier"),
            Err(_) => break,
        }
    }
    assert!(
        pin.exists(),
        "reopen must retain witness while source barrier fails"
    );
    drop(fault);
    while matches!(reopened.step().unwrap(), Step::More) {}
    assert!(!pin.exists());
    assert!(!source.exists());
    assert!(reopened.basis.complete());
}

#[test]
fn retirement_confirms_immutable_selection_once_for_the_whole_plan() {
    let (_tmp, store) = setup();
    let scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut selector = Selection::new(&store, scratch, None).unwrap();
    while !matches!(selector.phase, Phase::Gc) {
        selector.step(WorkLimit::default()).unwrap();
    }
    let generation = selector.staged.clone();
    let mut gc = SelectedGc::new(store.root.clone(), generation.clone()).unwrap();
    let selected_dir = store.root.join(DIRECTORY);
    let fault = crate::store::test_barrier::fail(&selected_dir, "sync-dir");
    assert!(
        gc.step().is_err(),
        "the first retirement requires a durable selection"
    );
    drop(fault);
    assert!(matches!(gc.step().unwrap(), Step::More));

    // The owner serializes this immutable plan. Further records must not force
    // the already-confirmed CURRENT selection to disk again for every unlink.
    // Source/pin directory barriers still run for each actual retirement.
    let fault = crate::store::test_barrier::fail(&selected_dir, "sync-dir");
    while matches!(
        gc.step().expect("selection barrier repeated per record"),
        Step::More
    ) {}
    drop(fault);
    let mut plan = File::open(generation.join("gc.manifest")).unwrap();
    let mut retired = 0;
    while let Some(meta) = read_meta(&mut plan).unwrap() {
        assert!(
            !generation
                .join(format!("gc-{:016x}.delta", meta.pin))
                .exists()
        );
        retired += 1;
    }
    assert!(retired > 1, "exercise a multi-record plan");
    let mut reopened = SelectedRead::new(&store).unwrap().unwrap();
    while matches!(reopened.step().unwrap(), Step::More) {}
    assert!(reopened.basis.complete());
}

#[test]
fn selected_generation_batches_full_drive_flushes_before_publication() {
    let (_tmp, store) = setup();
    let scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut selector = Selection::new(&store, scratch, None).unwrap();
    while !matches!(selector.phase, Phase::Commit) {
        selector.step(WorkLimit::default()).unwrap();
    }
    assert!(
        selector.ordinal > 3,
        "exercise multiple staged records and retirement pins"
    );
    let before = retained::current(&store.root.join(DIRECTORY)).unwrap();
    let fault = crate::store::test_barrier::fail(&selector.staged, "selection-full-flush");
    assert!(selector.step(WorkLimit::default()).is_err());
    assert_eq!(
        retained::current(&store.root.join(DIRECTORY)).unwrap(),
        before
    );
    let mut pins = File::open(selector.staged.join("gc.manifest")).unwrap();
    while let Some(meta) = read_meta(&mut pins).unwrap() {
        assert!(
            store.root.join(meta.source).exists(),
            "failed full flush must not retire source"
        );
        assert!(
            selector
                .staged
                .join(format!("gc-{:016x}.delta", meta.pin))
                .exists()
        );
    }
    drop(fault);
    while matches!(selector.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert_eq!(
        selector.stats().staging_full_flushes,
        3,
        "drive-wide flush count must depend on commit boundaries, not retained history size"
    );
    let mut reopened = SelectedRead::new(&store).unwrap().unwrap();
    while matches!(reopened.step().unwrap(), Step::More) {}
    assert!(reopened.basis.complete());
}

#[test]
fn selected_generation_defers_hard_link_durability_to_the_commit_barrier() {
    let (_tmp, store) = setup();
    let scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut selector = Selection::new(&store, scratch, None).unwrap();
    let before = retained::current(&store.root.join(DIRECTORY)).unwrap();
    while !matches!(selector.phase, Phase::Commit) {
        selector.step(WorkLimit::default()).unwrap();
    }
    assert!(selector.ordinal > 3, "exercise multiple staged hard links");
    let barrier = crate::store::test_barrier::fail(&selector.staged, "sync-dir");
    assert!(selector.step(WorkLimit::default()).is_err());
    assert_eq!(
        retained::current(&store.root.join(DIRECTORY)).unwrap(),
        before
    );
    let mut pins = File::open(selector.staged.join("gc.manifest")).unwrap();
    let mut retired = 0;
    while let Some(meta) = read_meta(&mut pins).unwrap() {
        assert!(store.root.join(meta.source).exists());
        assert!(
            selector
                .staged
                .join(format!("gc-{:016x}.delta", meta.pin))
                .exists()
        );
        retired += 1;
    }
    assert!(
        retired > 1,
        "exercise multiple retirements after the staged barrier"
    );
    drop(barrier);
    while matches!(selector.step(WorkLimit::default()).unwrap(), Step::More) {}
    let mut reopened = SelectedRead::new(&store).unwrap().unwrap();
    while matches!(reopened.step().unwrap(), Step::More) {}
    assert!(reopened.basis.complete());
}

#[test]
fn recovery_cursor_rejects_other_workspace_before_publication() {
    use crate::store::{
        bounded::{Meta, write_meta},
        replay::{PublicationCursor, ReplayBudget},
    };
    use crate::transport::Envelope;
    let tmp = tempfile::tempdir().unwrap();
    let store = crate::store::WorkspaceStore::open(tmp.path(), 7, true).unwrap();
    let generation = store.root().join("shell-selected-v1/test");
    std::fs::create_dir_all(&generation).unwrap();
    let frame = Envelope::Delta {
        workspace_id: 8,
        author_device_public_key: [1; 32],
        authored_at_ns: 1,
        delta: vec![255],
        signature: [0; 64],
    };
    let bytes = crate::transport::encode(&frame).unwrap();
    let source = generation.join("0.delta");
    std::fs::write(&source, &bytes).unwrap();
    let mut manifest = std::fs::File::create(generation.join("basis.manifest")).unwrap();
    write_meta(
        &mut manifest,
        &Meta {
            key: vec!["extra".into()],
            source: source.strip_prefix(store.root()).unwrap().into(),
            pin: 0,
            len: bytes.len() as u64,
            pending: false,
        },
    )
    .unwrap();
    drop(manifest);
    let mut cursor = PublicationCursor::open_recovery(&generation).unwrap();
    assert!(
        cursor.step(&ReplayBudget::new(1024 * 1024)).is_err(),
        "recovery emitted an envelope from another workspace"
    );
}

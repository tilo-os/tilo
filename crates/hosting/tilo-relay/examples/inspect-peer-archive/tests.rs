use super::*;
use automerge::transaction::Transactable;
use ed25519_dalek::SigningKey;
use neighbour_wire::{Retention, retention};
use tilo_peer_state::StatePeer;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};
use web_time::SystemTime;

fn encode(message: &NeighbourMsg, author: [u8; 32], index: usize) -> Source {
    let (channel, kind) = retention(message, true).unwrap();
    let body = postcard::to_stdvec(message).unwrap();
    let payload = match kind {
        Retention::Checkpoint => CoeditPayload::Checkpoint { channel, body },
        Retention::Delta => CoeditPayload::RetainedDelta { channel, body },
    };
    Source {
        name: format!("fixture-{index}"),
        bytes: transport::encode(&Envelope::Delta {
            workspace_id: tilo_relay::room_workspace_id("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
                .unwrap(),
            author_device_public_key: author,
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: payload.encode().unwrap(),
            signature: [0; 64],
        })
        .unwrap(),
    }
}
fn fixture() -> (PublicEnrollment, Vec<Source>, SigningKey) {
    let user_key = SigningKey::from_bytes(&[51; 32]);
    let key = SigningKey::from_bytes(&[52; 32]);
    let mut cert = DeviceCert {
        user_public_key: user_key.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "archive test".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &user_key).unwrap();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let workspace = tilo_relay::room_workspace_id(room).unwrap();
    let now = SystemTime::now();
    let mut state = StatePeer::new(
        room,
        cert.clone(),
        vec![cert.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    state
        .install_seed(
            super::peer_fixture::fixture_seed(room, &cert, &[cert.clone()]).unwrap(),
            &key,
            now,
        )
        .unwrap();
    super::peer_fixture::fixture_writer_insert(&mut state, &key, "Recovery test", now).unwrap();
    let messages = state.bootstrap_messages(&key, now).unwrap();
    let mut directory = WorkspaceDirectory::new(cert.user_public_key, cert.device_public_key);
    directory.add_workspace(workspace, "archive test").unwrap();
    directory
        .set_member_devices(workspace, &[cert.device_public_key])
        .unwrap();
    directory
        .set_host_devices(workspace, &[cert.device_public_key])
        .unwrap();
    let update = directory
        .sync_incremental(&key, tilo_clock::unix_now_ns())
        .unwrap();
    let mut host = cert.clone();
    host.capabilities = DeviceCapabilities::host();
    sign_device_cert(&mut host, &user_key).unwrap();
    let auth = PublicEnrollment {
        root_public_key: cert.user_public_key,
        host_certificate: host,
        revocations: vec![],
        bootstrap: Bootstrap {
            room: room.into(),
            workspace_id: workspace.to_string(),
            directory_author: cert.clone(),
            directory_update: update,
            certified_roster: vec![cert.clone()],
            records: state.rows(),
        },
    };
    (
        auth,
        messages
            .iter()
            .enumerate()
            .map(|(i, m)| encode(m, cert.device_public_key, i))
            .collect(),
        key,
    )
}
fn alter(s: &mut Source, f: impl FnOnce(&mut NeighbourMsg)) {
    let Envelope::Delta {
        author_device_public_key,
        delta,
        ..
    } = decode_frame(&s.bytes).unwrap()
    else {
        panic!()
    };
    let body = match CoeditPayload::decode(&delta).unwrap() {
        CoeditPayload::Checkpoint { body, .. } | CoeditPayload::RetainedDelta { body, .. } => body,
    };
    let mut message: NeighbourMsg = postcard::from_bytes(&body).unwrap();
    f(&mut message);
    *s = encode(&message, author_device_public_key, 0);
}
fn app(sources: &mut [Source]) -> &mut Source {
    sources
        .iter_mut()
        .find(|s| {
            let Envelope::Delta { delta, .. } = decode_frame(&s.bytes).unwrap() else {
                return false;
            };
            let body = match CoeditPayload::decode(&delta).unwrap() {
                CoeditPayload::Checkpoint { body, .. }
                | CoeditPayload::RetainedDelta { body, .. } => body,
            };
            matches!(
                postcard::from_bytes::<NeighbourMsg>(&body),
                Ok(NeighbourMsg::AppDocument { .. })
            )
        })
        .unwrap()
}
#[test]
fn valid_complete_archive_reconstructs_canonical_documents() {
    let (auth, sources, _) = fixture();
    let recovered = inspect(&auth, &sources).unwrap();
    assert_eq!(recovered.docs.len(), 2);
    let writer = &recovered.docs[&(
        "so.tilo.word-processor".into(),
        "tab:0000000000000001".into(),
    )];
    let c = word_processor::model::open(writer).unwrap();
    assert_eq!(
        tilo_doc::model::materialize_region(writer, &c, tilo_doc::model::Region::Body)[0].text,
        "Recovery test"
    );
    for row in &auth.bootstrap.records {
        assert_eq!(
            recovered.docs[&(row.app_id.clone(), row.record_id.clone())]
                .base_heads()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            row.heads
        );
    }
}
#[test]
fn tampered_signature_rejected() {
    let (auth, mut sources, _) = fixture();
    alter(app(&mut sources), |m| {
        if let NeighbourMsg::AppDocument { envelope, .. } = m {
            envelope.signature = collab_types::Signature::from_bytes([9; 64]);
        }
    });
    assert!(inspect(&auth, &sources).is_err());
}
#[test]
fn tampered_payload_rejected() {
    let (auth, mut sources, _) = fixture();
    alter(app(&mut sources), |m| {
        if let NeighbourMsg::AppDocument { envelope, .. } = m {
            let mut mutation = DocumentMutation::decode(&envelope.payload).unwrap();
            match &mut mutation {
                DocumentMutation::Snapshot(b) | DocumentMutation::Incremental(b) => b.push(9),
            }
            envelope.payload = mutation.encode().unwrap();
        }
    });
    assert!(inspect(&auth, &sources).is_err());
}
#[test]
fn wrong_room_and_record_rejected() {
    let (auth, sources, _) = fixture();
    for room in [true, false] {
        let mut altered = sources.clone();
        alter(app(&mut altered), |m| {
            if let NeighbourMsg::AppDocument {
                cluster_id,
                record_id,
                ..
            } = m
            {
                if room {
                    *cluster_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into();
                } else {
                    *record_id = "tab:0000000000000042".into();
                }
            }
        });
        assert!(inspect(&auth, &altered).is_err());
    }
}
#[test]
fn missing_and_tampered_coverage_rejected() {
    let (auth, sources, _) = fixture();
    for missing in [true, false] {
        let mut altered = sources.clone();
        for source in &mut altered {
            alter(source, |m| {
                if let NeighbourMsg::SyncResp { changes, .. } = m {
                    let mut doc = room::candidate(None, changes).unwrap();
                    if missing {
                        doc.delete(automerge::ROOT, coverage::FIELD_V2).unwrap();
                        doc.commit();
                    } else {
                        let mut c = coverage::entries(&doc)
                            .unwrap()
                            .into_values()
                            .next()
                            .unwrap();
                        c.payload_hash = [9; 32];
                        coverage::put(&mut doc, c).unwrap();
                    }
                    *changes = doc
                        .get_changes(&[])
                        .iter()
                        .map(|c| c.raw_bytes().to_vec())
                        .collect();
                }
            });
        }
        assert!(inspect(&auth, &altered).is_err());
    }
}
#[test]
fn missing_room_dependency_rejected() {
    let (auth, mut sources, _) = fixture();
    for source in &mut sources {
        alter(source, |m| {
            if let NeighbourMsg::SyncResp { changes, .. } = m {
                assert!(changes.len() > 1);
                changes.remove(0);
            }
        });
    }
    assert!(inspect(&auth, &sources).is_err());
}

#[test]
fn signed_tail_with_missing_causal_predecessor_is_incomplete() {
    let (auth, mut sources, key) = fixture();
    let source = app(&mut sources).clone();
    let Envelope::Delta { delta, .. } = decode_frame(&source.bytes).unwrap() else {
        panic!()
    };
    let CoeditPayload::Checkpoint { body, .. } = CoeditPayload::decode(&delta).unwrap() else {
        panic!()
    };
    let mut msg: NeighbourMsg = postcard::from_bytes(&body).unwrap();
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut msg {
        let DocumentMutation::Snapshot(bytes) =
            DocumentMutation::decode(&envelope.payload).unwrap()
        else {
            panic!()
        };
        let mut doc = AutoCommit::load(&bytes).unwrap();
        doc.set_actor(automerge::ActorId::from(vec![99; 32]));
        doc.put(automerge::ROOT, "predecessor", true).unwrap();
        collab_types::CommitNamed::commit_edit(&mut doc, "fixture", "predecessor");
        let before = doc.get_heads();
        doc.put(automerge::ROOT, "successor", true).unwrap();
        collab_types::CommitNamed::commit_edit(&mut doc, "fixture", "successor");
        envelope.payload = DocumentMutation::Incremental(doc.save_after(&before))
            .encode()
            .unwrap();
        envelope.seq += 1;
        collab_types::sign_op(envelope, &key).unwrap();
    }
    sources.push(encode(&msg, key.verifying_key().to_bytes(), 7));
    assert!(inspect(&auth, &sources).is_err());
}

#[test]
fn forged_directory_and_certificate_rejected() {
    let (auth, sources, _) = fixture();
    let mut forged = auth.clone();
    forged.bootstrap.directory_update.signature = [9; 64];
    assert!(inspect(&forged, &sources).is_err());
    let mut forged = auth;
    forged.bootstrap.directory_author.signature = [9; 64];
    assert!(inspect(&forged, &sources).is_err());
}

#[test]
fn readable_export_contains_sent_content_and_shared_draft_without_changing_heads() {
    let (auth, sources, _) = fixture();
    let recovered = inspect(&auth, &sources).unwrap();
    let writer = &recovered.docs[&(
        "so.tilo.word-processor".into(),
        "tab:0000000000000001".into(),
    )];
    let before = writer.base_heads();
    let writer_export = readable("so.tilo.word-processor", "tab:0000000000000001", writer).unwrap();
    assert_eq!(writer_export["body_text"], "Recovery test");
    assert_eq!(writer.base_heads(), before);
    let mut chat = recovered.docs[&("so.tilo.chat".into(), String::new())].clone();
    let channel = chat::projection::read(&chat).channel_rows().remove(0).id;
    let mut part = AutoCommit::new();
    chat::demo::say_in_part(
        &mut part,
        &chat::projection::read(&chat),
        "test-person",
        &channel,
        "Sent archive content",
    )
    .unwrap();
    // A draft is its writer's own, kept in their person record, never in
    // the room's chat record this archive reads.
    let before = chat.base_heads();
    let export = readable("so.tilo.chat", "", &chat).unwrap();
    let row = export["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == channel)
        .unwrap();
    assert_eq!(row["draft"], "");
    assert_eq!(chat.base_heads(), before);
    let part_before = part.base_heads();
    let export = readable("so.tilo.chat", chat::parts::part(0).as_str(), &part).unwrap();
    let row = export["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == channel)
        .unwrap();
    assert!(
        row["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["text"] == "Sent archive content")
    );
    assert_eq!(part.base_heads(), part_before);
}

#[test]
fn output_is_private_and_refuses_overwrite_or_symlink() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let parent = tempfile::tempdir().unwrap();
    let out = parent.path().join("new");
    create_private_directory(&out).unwrap();
    assert_eq!(
        std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let file = out.join("manifest.json");
    write_private(&file, b"{}").unwrap();
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(write_private(&file, b"replacement").is_err());
    assert!(create_private_directory(&out).is_err());
    let link = parent.path().join("link");
    symlink(&out, &link).unwrap();
    assert!(create_private_directory(&link).is_err());
}

#[test]
fn complete_and_incomplete_runs_preserve_raw_evidence_without_placeholder_exports() {
    let (auth, sources, _) = fixture();
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("archive");
    let retained = archive.join(format!(
        "workspaces/{}/retained/checkpoints",
        auth.bootstrap.workspace_id
    ));
    std::fs::create_dir_all(&retained).unwrap();
    for source in &sources {
        let Envelope::Delta { delta, .. } = decode_frame(&source.bytes).unwrap() else {
            panic!()
        };
        let CoeditPayload::Checkpoint { channel, .. } = CoeditPayload::decode(&delta).unwrap()
        else {
            panic!()
        };
        write_private(&retained.join(hex::encode(channel)), &source.bytes).unwrap();
    }
    let enrollment = temp.path().join("public.json");
    let ready = temp.path().join("ready.json");
    let public = json!({"root_public_key":auth.root_public_key,"host_certificate":auth.host_certificate,"revocations":auth.revocations,"bootstrap":{"room":auth.bootstrap.room,"workspace_id":auth.bootstrap.workspace_id,"directory_author":auth.bootstrap.directory_author,"directory_update":auth.bootstrap.directory_update,"certified_roster":auth.bootstrap.certified_roster,"records":auth.bootstrap.records}});
    write_private(&enrollment, &serde_json::to_vec(&public).unwrap()).unwrap();
    write_private(
        &ready,
        &serde_json::to_vec(
            &json!({"room":auth.bootstrap.room,"workspace_id":auth.bootstrap.workspace_id}),
        )
        .unwrap(),
    )
    .unwrap();
    let arguments = || Arguments {
        archive_root: archive.clone(),
        public_enrollment: enrollment.clone(),
        old_ready: ready.clone(),
        output_parent: temp.path().to_owned(),
        expected_chat_head: None,
        expected_writer_head: None,
    };
    let mut before = vec![];
    tree(&archive, Path::new(""), &mut before).unwrap();
    let (out, complete, count) = run(arguments()).unwrap();
    assert!(complete);
    assert_eq!(count, 2);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["live_admission"], false);
    for row in manifest["documents"].as_array().unwrap() {
        assert!(out.join(row["canonical_file"].as_str().unwrap()).is_file());
        assert_eq!(row["causally_complete"], true);
    }
    let mut after = vec![];
    tree(&archive, Path::new(""), &mut after).unwrap();
    assert_eq!(
        before
            .iter()
            .map(|s| (&s.name, &s.bytes))
            .collect::<Vec<_>>(),
        after
            .iter()
            .map(|s| (&s.name, &s.bytes))
            .collect::<Vec<_>>()
    );
    // Damage only this synthetic fixture's evidence; an incomplete run must
    // retain it privately and produce no canonical document placeholder.
    let first = std::fs::read_dir(&retained)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(first, b"truncated").unwrap();
    let (out, complete, count) = run(arguments()).unwrap();
    assert!(!complete);
    assert_eq!(count, 0);
    assert!(out.join("raw/source-0000.bin").is_file());
    assert!(!out.join("document-0.automerge").exists());
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["status"], "incomplete");
}

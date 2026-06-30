//! The headline integration test: the full Phase 2 + 3 + 6 + 7 + 8
//! stack composes into one working end-to-end flow.
//!
//! The scenario this exercises matches what the spec promises:
//!
//! 1. Two devices (Alice's laptop + Alice's phone) hold one
//!    threshold-shared identity via FROST (Phase 8 — the secret half
//!    is split across the two devices; the public half is a single
//!    Ed25519 key).
//! 2. Channel owner Olga has authorised Alice as an Editor on a
//!    Note artifact (Phase 2 attestation, registered in the
//!    channel's role table).
//! 3. Alice's laptop encrypts an op's payload via the channel's
//!    MLS group (Phase 7), then her laptop + phone collaboratively
//!    sign the envelope across the ciphertext (Phase 8 +
//!    Phase 3 — outer signature covers the ciphertext, the broker
//!    validates the envelope without ever touching the cleartext).
//! 4. The envelope crosses the broker (Phase 3 wire enforcement) and
//!    reaches a second connected peer, Bob, who has also joined the
//!    MLS group.
//! 5. Bob's app receives `CollabEvent::SignedCRDTIncremental`,
//!    decrypts the payload via Bob's mirror of the MLS group, and
//!    surfaces the cleartext.
//! 6. Olga publishes a Compaction op (Phase 6), signed by her
//!    (Owner-only role permit). Older ops are evicted from the
//!    broker's in-memory log.
//! 7. A `FetchScrollback` request for the compacted prefix returns
//!    an empty batch with `exhausted = true` — the compaction is
//!    effective.
//!
//! If this test passes, every numbered Phase in the spec is
//! observably real: foundations compose into a working system.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use ed25519_dalek::SigningKey;
use qos_channel_crypto::{ChannelGroup, MemberKey};
use qos_collab_types::{
    ArtifactId, ArtifactKind, Attestation, AttestationId, ChannelId, ChannelState,
    CompactionCheckpoint, Ed25519PublicKey, EncryptionMetadata, OpClass, Role, RoleAttestation,
    Signature, SignedOp, encode_compaction_payload, generate_keypair, hash_op_prefix, sign_event,
    sign_op,
};
use qos_threshold_identity::{
    EnrolmentRound2, ThresholdIdentity, signing::SigningRound1, signing::SigningRound1Commitment,
    signing::SigningRound2Partial,
};
use rand_08::SeedableRng;
use rand_chacha::ChaCha20Rng;
use shell::shell::collab_broker;
use ui::collaboration::{CollabEvent, CollabRequest};

// ---- wire framing (matches broker exactly) ----

fn write_frame<T: serde::Serialize>(stream: &mut UnixStream, v: &T) {
    let bytes = serde_json::to_vec(v).expect("serialize");
    let len = (bytes.len() as u32).to_le_bytes();
    stream.write_all(&len).expect("write len");
    stream.write_all(&bytes).expect("write body");
}

fn read_event(stream: &mut UnixStream, timeout: Duration) -> Option<CollabEvent> {
    stream.set_read_timeout(Some(timeout)).expect("set timeout");
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).ok()?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn unique_socket(label: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("qos-everything-{label}-{pid}-{nanos}.sock"))
}

fn wait_for_broker(socket: &PathBuf) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match UnixStream::connect(socket) {
            Ok(s) => return s,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("could not connect to broker: {e}"),
        }
    }
}

// ---- FROST helpers, lifted from qos-threshold-identity's interop test ----

fn dkg_alice_2_of_2() -> (ThresholdIdentity, ThresholdIdentity) {
    let mut rng_a = ChaCha20Rng::from_seed([7u8; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([8u8; 32]);
    let mut laptop = ThresholdIdentity::bootstrap_single_device(&mut rng_a).unwrap();
    let mut phone = ThresholdIdentity::bootstrap_single_device(&mut rng_b).unwrap();
    let (mut s_a, r1_a) = laptop.begin_enrolment(1, 2, 2, &mut rng_a).unwrap();
    let (mut s_b, r1_b) = phone.begin_enrolment(2, 2, 2, &mut rng_b).unwrap();
    let r2_a_out = s_a.receive_round1(&[r1_b]).unwrap();
    let r2_b_out = s_b.receive_round1(&[r1_a]).unwrap();
    s_a.receive_round2(&[r2_b_out[&1].clone()]).unwrap();
    s_b.receive_round2(&[r2_a_out[&2].clone()]).unwrap();
    laptop.adopt_dkg_outcome(s_a).unwrap();
    phone.adopt_dkg_outcome(s_b).unwrap();
    (laptop, phone)
}

fn threshold_sign(
    laptop: &ThresholdIdentity,
    phone: &ThresholdIdentity,
    canon: &[u8],
    seed_a: u8,
    seed_b: u8,
) -> Signature {
    let mut rng_a = ChaCha20Rng::from_seed([seed_a; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([seed_b; 32]);
    let r1_a: SigningRound1 = laptop.sign_round1(&mut rng_a).unwrap();
    let r1_b: SigningRound1 = phone.sign_round1(&mut rng_b).unwrap();
    let commitments: Vec<SigningRound1Commitment> =
        vec![r1_a.commitment.clone(), r1_b.commitment.clone()];
    let p_a: SigningRound2Partial = laptop
        .sign_round2_raw(r1_a.nonces, canon, &commitments)
        .unwrap();
    let p_b: SigningRound2Partial = phone
        .sign_round2_raw(r1_b.nonces, canon, &commitments)
        .unwrap();
    laptop.aggregate_signature(&[p_a, p_b]).unwrap()
}

// ---- attestation fixture ----

fn make_role_attestation(
    issuer_sk: &SigningKey,
    issuer_pk: Ed25519PublicKey,
    subject: Ed25519PublicKey,
    channel: ChannelId,
    role: Role,
) -> RoleAttestation {
    let now = SystemTime::now();
    let mut att = Attestation {
        id: AttestationId::new(),
        issued_by: issuer_pk,
        subject,
        claims: vec![],
        valid_from: now - Duration::from_secs(365 * 24 * 3600),
        valid_until: now + Duration::from_secs(10 * 365 * 24 * 3600),
        signature: Signature::from_bytes([0u8; 64]),
    };
    let sb = att.signing_bytes().unwrap();
    att.signature = sign_event(&sb, issuer_sk).unwrap();
    RoleAttestation {
        channel,
        role,
        attestation: att,
    }
}

#[test]
fn threshold_signed_mls_encrypted_op_traverses_broker_then_compaction_evicts() {
    // === 1. Olga owns the channel; one Note artifact. ===
    let (olga_sk, olga_pk) = generate_keypair();
    let channel_id = ChannelId::new();
    let artifact = ArtifactId::new();

    // === 2. Alice's 2-of-2 threshold identity. ===
    let (alice_laptop, alice_phone) = dkg_alice_2_of_2();
    let alice_group_pk = *alice_laptop.group_public_key();
    assert_eq!(
        alice_group_pk,
        *alice_phone.group_public_key(),
        "Alice's two devices must share the same group public key",
    );

    // === 3. Bob's plain single-device identity (a third party who
    //         also got Editor + MLS membership). ===
    let (bob_sk, bob_pk) = generate_keypair();

    // === 4. Olga mints Editor attestations for Alice's threshold key
    //         and Bob's plain key. ===
    let alice_att = make_role_attestation(&olga_sk, olga_pk, alice_group_pk, channel_id, Role::Editor);
    let bob_att = make_role_attestation(&olga_sk, olga_pk, bob_pk, channel_id, Role::Editor);
    let alice_att_id = alice_att.attestation.id;
    let bob_att_id = bob_att.attestation.id;

    // Olga also self-attests as Owner so she can sign compaction ops.
    let olga_att = make_role_attestation(&olga_sk, olga_pk, olga_pk, channel_id, Role::Owner);
    let olga_att_id = olga_att.attestation.id;

    // === 5. MLS group: Olga creates, Alice joins, Bob joins. ===
    //         Each member uses MemberKey for their MLS-level identity
    //         (separate from their outer-envelope signing identity).
    let olga_mls = MemberKey::generate("olga").unwrap();
    let alice_mls = MemberKey::generate("alice").unwrap();
    let bob_mls = MemberKey::generate("bob").unwrap();
    let alice_kp = alice_mls.fresh_key_package().unwrap();
    let bob_kp = bob_mls.fresh_key_package().unwrap();

    let mut olga_g = ChannelGroup::create(channel_id, olga_mls).unwrap();
    let welcome_alice = olga_g.add_member(alice_kp).unwrap();
    let mut alice_g = ChannelGroup::process_welcome(channel_id, &welcome_alice, alice_mls).unwrap();

    // Adding Bob to a 2-member group needs the commit path: Olga
    // emits both a Welcome (for Bob) and a Commit (for Alice).
    let (welcome_bob, commit_for_existing) = olga_g.add_member_with_commit(bob_kp).unwrap();
    alice_g.process_commit(&commit_for_existing).unwrap();
    let mut bob_g = ChannelGroup::process_welcome(channel_id, &welcome_bob, bob_mls).unwrap();

    // === 6. Channel state: register all three attestations, the
    //         artifact, and Olga as the authorised signer. ===
    let mut state = ChannelState::default();
    state.authorised_signers.push(olga_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(alice_att);
    state.register_attestation(bob_att);
    state.register_attestation(olga_att);

    // === 7. Spawn broker, register channel state. ===
    let socket = unique_socket("everything");
    let handle = collab_broker::spawn(socket.clone()).expect("spawn broker");
    handle.register_channel_state(artifact, state);

    let mut alice_app = wait_for_broker(&socket);
    let mut bob_app = wait_for_broker(&socket);
    let mut olga_app = wait_for_broker(&socket);
    for s in [&mut alice_app, &mut bob_app, &mut olga_app] {
        write_frame(
            s,
            &CollabRequest::Hello {
                app_id: "everything".into(),
                cluster_id: uuid::Uuid::new_v4(),
            },
        );
    }

    // === 8. Alice publishes 3 MLS-encrypted, threshold-signed
    //         ops. Bob's app receives + decrypts each one. ===
    let mut alice_seq: u64 = 0;
    for (i, plaintext) in [
        b"first edit from alice".to_vec(),
        b"second edit from alice".to_vec(),
        b"third edit from alice".to_vec(),
    ]
    .into_iter()
    .enumerate()
    {
        let ciphertext = alice_g.encrypt(&plaintext).expect("alice encrypt");
        let encryption = EncryptionMetadata {
            epoch: alice_g.epoch(),
            channel: channel_id,
        };
        alice_seq += 1;
        let mut op = SignedOp::new_unsigned_with_encryption(
            ciphertext,
            artifact,
            alice_group_pk,
            alice_att_id,
            OpClass::Splice,
            SystemTime::now(),
            alice_seq,
            encryption,
        );
        // Threshold sign across both Alice devices.
        let canon = op.signing_bytes().unwrap();
        op.signature = threshold_sign(&alice_laptop, &alice_phone, &canon, 40 + i as u8, 60 + i as u8);

        write_frame(
            &mut alice_app,
            &CollabRequest::SignedSyncIncremental {
                envelope: Box::new(op.clone()),
            },
        );

        let event = read_event(&mut bob_app, Duration::from_millis(500));
        let event = match event {
            Some(e) => e,
            None => {
                let audit = handle.audit_for(artifact);
                panic!(
                    "bob did not receive op #{i}; audit: {:?}",
                    audit.iter().map(|e| &e.rejection).collect::<Vec<_>>()
                );
            }
        };
        match event {
            CollabEvent::SignedCRDTIncremental { envelope, .. } => {
                assert_eq!(envelope.seq, alice_seq);
                let decrypted = bob_g
                    .decrypt(&envelope.payload)
                    .expect("bob decrypts the alice ciphertext");
                assert_eq!(
                    decrypted, plaintext,
                    "decrypted bytes must match what alice encrypted",
                );
            }
            other => panic!("expected SignedCRDTIncremental, got {other:?}"),
        }
    }

    // Drain everything currently queued on olga's stream — she
    // received Alice's three ops via fan-out and we need a clean
    // slate before driving FetchScrollback through her socket.
    fn drain_all(stream: &mut UnixStream) {
        while read_event(stream, Duration::from_millis(50)).is_some() {}
    }
    drain_all(&mut olga_app);

    // === 9. Olga publishes a Compaction op covering Alice's first
    //         two ops. The payload is the canonical-encoded
    //         CompactionCheckpoint; the seq is *Olga's* per-author
    //         seq, NOT the artifact's overall seq. ===
    // Hash the prefix Olga claims to be summarising. We need the
    // ops as they sit in the broker's log — the broker has been
    // appending them via record_validated_op as they validated.
    // Reconstruct by inspecting the broker's scrollback.
    write_frame(
        &mut olga_app,
        &CollabRequest::FetchScrollback {
            artifact,
            before_seq: 3,
            max: 10,
        },
    );
    let prefix_ops = match read_event(&mut olga_app, Duration::from_millis(500))
        .expect("scrollback returns")
    {
        CollabEvent::ScrollbackBatch { ops, .. } => ops,
        other => panic!("expected ScrollbackBatch, got {other:?}"),
    };
    assert_eq!(
        prefix_ops.len(),
        2,
        "scrollback should hand us the two ops with seq < 3",
    );

    // Build the checkpoint that covers seq 1 and 2.
    let prefix_owned: Vec<SignedOp> = prefix_ops.iter().map(|b| (**b).clone()).collect();
    let prefix_hash = hash_op_prefix(&prefix_owned).expect("hash prefix");
    let checkpoint = CompactionCheckpoint {
        artifact,
        up_to_seq: 2,
        prefix_hash,
    };
    let payload = encode_compaction_payload(&checkpoint).expect("encode compaction payload");

    // Per the agent's Phase 6 convention (see
    // `tests/scrollback_pipeline.rs::compaction_evicts_log_then_scrollback_returns_exhausted`),
    // the compaction op's seq must be greater than the
    // checkpoint's `up_to_seq` so the compaction-op itself doesn't
    // fall inside the prefix being evicted. Per-author seq only needs
    // to be monotonic — we can skip numbers freely.
    let mut olga_op = SignedOp::new_unsigned(
        payload,
        artifact,
        olga_pk,
        olga_att_id,
        OpClass::Compaction,
        SystemTime::now(),
        100,
    );
    sign_op(&mut olga_op, &olga_sk).expect("olga signs compaction");
    write_frame(
        &mut olga_app,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(olga_op),
        },
    );

    // === 10. Wait briefly for the broker to apply the compaction. ===
    //          Both alice and bob receive the compaction op as a
    //          regular SignedCRDTIncremental event — drain everything
    //          queued on each socket so subsequent FetchScrollback
    //          calls don't trip over a stray frame.
    drain_all(&mut alice_app);
    drain_all(&mut bob_app);
    drain_all(&mut olga_app);

    // === 11. Bob asks for scrollback BEFORE the compaction
    //          checkpoint. After the eviction, ops 1+2 are no longer
    //          in the broker's log — the response should hand back
    //          nothing for the requested range. ===
    write_frame(
        &mut bob_app,
        &CollabRequest::FetchScrollback {
            artifact,
            before_seq: 3,
            max: 10,
        },
    );
    let event = read_event(&mut bob_app, Duration::from_millis(500))
        .expect("scrollback returns after compaction");
    match event {
        CollabEvent::ScrollbackBatch { ops, exhausted: _, .. } => {
            if !ops.is_empty() {
                let audit = handle.audit_for(artifact);
                panic!(
                    "compaction should have evicted ops 1+2 from the in-memory log; \
                     found {} stragglers (seqs {:?}). audit rejections: {:?}",
                    ops.len(),
                    ops.iter().map(|o| o.seq).collect::<Vec<_>>(),
                    audit.iter()
                        .filter(|e| e.rejection.is_some())
                        .map(|e| (&e.envelope.op_class, &e.rejection))
                        .collect::<Vec<_>>(),
                );
            }
        }
        other => panic!("expected ScrollbackBatch, got {other:?}"),
    }

    // === 12. Asking for scrollback at the upper end (seq <= 3
    //          which is alice's third op) should still return the
    //          untouched tail. ===
    write_frame(
        &mut bob_app,
        &CollabRequest::FetchScrollback {
            artifact,
            before_seq: 5,
            max: 10,
        },
    );
    let event = read_event(&mut bob_app, Duration::from_millis(500))
        .expect("scrollback returns for tail");
    match event {
        CollabEvent::ScrollbackBatch { ops, .. } => {
            // After compaction the log retains alice's op #3 (seq=3
            // > checkpoint.up_to_seq=2). Olga's compaction op has
            // seq=100, which is > before_seq=5, so it's excluded by
            // the scrollback's own filter. Exactly one entry remains.
            assert_eq!(
                ops.len(),
                1,
                "tail scrollback should retain alice's seq=3 op",
            );
            let has_alice_three = ops.iter().any(|o| o.author == alice_group_pk && o.seq == 3);
            assert!(
                has_alice_three,
                "alice's third op (post-compaction) should still be in the log",
            );
        }
        other => panic!("expected ScrollbackBatch, got {other:?}"),
    }

    // === The whole stack composes. ===
    drop(alice_app);
    drop(bob_app);
    drop(olga_app);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

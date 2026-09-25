//! Lane T: the Cloud Store stays bounded over many agent steps.
//!
//! A benchmark lab grew its saved Store from 4.5 MB to 17.7 MB over ~45 agent
//! messages: every member refresh and join retained a whole bootstrap (the
//! room governance document), and delivery history was retained until half of
//! the 64 MiB pool limit. Each step re-saves the whole Store, so step time and
//! the host's per-turn memory reservations grew with it until onboarding and
//! state requests were refused. This drives real Store commits in the shape of
//! agent work (document commits), periodic member refreshes with lost-reply
//! retries, and the owner's retention maintenance, and asserts that the saved
//! aggregate stays bounded.
use super::*;
use crate::ActionRequest;
use ed25519_dalek::SigningKey;
use qos_peer_state::join::*;
use qos_relay::delivery_v3::commit::MAX_REFERENCES;
use qos_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

struct Guest {
    key: SigningKey,
    cert: DeviceCert,
}

fn saved_bytes(dir: &std::path::Path) -> u64 {
    std::fs::metadata(dir.join("state.json")).unwrap().len()
        + std::fs::metadata(dir.join("state.journal")).map_or(0, |m| m.len())
}

fn join_guest(store: &mut Store, owner: [u8; 32], now: u64) -> Guest {
    let root = SigningKey::from_bytes(&[71; 32]);
    let key = SigningKey::from_bytes(&[72; 32]);
    let output = store
        .action(
            owner,
            &ActionRequest {
                version: 1,
                room: store.state.room.clone(),
                invocation_id: "growth-guest".into(),
                operation: crate::NativeOperation::CollaborationInvite {
                    role: MemberRole::Editor,
                    recipient_user_public_key: Some(root.verifying_key().to_bytes()),
                    invitee_name: Some("Growth guest".into()),
                    expires_at_ns: now + 600_000_000_000,
                },
            },
            now,
        )
        .unwrap()
        .output
        .unwrap();
    let invitation = decode_and_verify_invitation_text(
        output["signed_invitation"].as_str().unwrap(),
        Some(root.verifying_key().to_bytes()),
        &RevocationList::new(),
        now,
    )
    .unwrap()
    .signed_invitation()
    .to_vec();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Growth guest".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let challenge = store
        .join_challenge(
            cert.device_public_key,
            JoinChallengeRequest {
                signed_invitation: invitation,
                device_cert: cert.clone(),
                request_id: [71; 32],
            },
            now,
        )
        .unwrap();
    let proof = JoinCompleteRequest {
        proof: sign_join_proof(challenge.challenge, &key).unwrap(),
    };
    let first = store
        .join_complete(cert.device_public_key, proof.clone(), now)
        .unwrap();
    // A lost reply is answered again, identically while nothing changed.
    assert_eq!(
        store
            .join_complete(cert.device_public_key, proof, now)
            .unwrap(),
        first
    );
    Guest { key, cert }
}

/// One member refresh (a browser reconnect) plus a lost-reply retry of it.
fn refresh(store: &mut Store, guest: &Guest, request_id: [u8; 32], now: u64) -> JoinBootstrap {
    let challenge = store
        .membership_challenge(
            guest.cert.device_public_key,
            MembershipChallengeRequest {
                room: store.state.room.clone(),
                workspace_id: qos_relay::room_workspace_id(&store.state.room).unwrap(),
                device_cert: guest.cert.clone(),
                request_id,
            },
            now,
        )
        .unwrap();
    let complete = MembershipCompleteRequest {
        proof: sign_membership_proof(challenge.challenge, &guest.key).unwrap(),
    };
    let bootstrap = store
        .membership_complete(guest.cert.device_public_key, complete.clone(), now)
        .unwrap();
    let replay = store
        .membership_complete(guest.cert.device_public_key, complete.clone(), now)
        .unwrap();
    assert_eq!(
        replay, bootstrap,
        "unchanged state replays byte-identically"
    );
    // Bound to the authenticated endpoint.
    assert!(store.membership_complete([99; 32], complete, now).is_err());
    bootstrap
}

/// Room governance history by the top-level keys each change touches:
/// (changes, raw change bytes, total saved document bytes).
fn room_history(store: &Store, now: u64) -> (usize, usize, usize) {
    let peer = store.peer_at(now).unwrap();
    let mut room = peer.admitted_room().unwrap().clone();
    let changes = room.get_changes(&[]);
    let mut by_key = std::collections::BTreeMap::<String, (usize, usize)>::new();
    let mut total = 0;
    for change in &changes {
        let bytes = change.raw_bytes().len();
        total += bytes;
        let mut keys = change
            .decode()
            .operations
            .iter()
            .map(|op| match (&op.obj, &op.key) {
                (automerge::legacy::ObjectId::Root, automerge::legacy::Key::Map(key)) => {
                    key.to_string()
                }
                _ => "<nested>".to_string(),
            })
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        let entry = by_key.entry(keys.join(",")).or_default();
        entry.0 += 1;
        entry.1 += bytes;
    }
    if std::env::var("QOS_ROOM_HISTORY").is_ok() {
        for (key, row) in qos_peer_state::coverage::entries(&room).unwrap() {
            eprintln!(
                "coverage {key:?}: {} bytes heads={} watermarks={}",
                postcard::to_allocvec(&row).unwrap().len(),
                row.heads.len(),
                row.watermarks.len()
            );
        }
        for (key, row) in qos_peer_state::durability::entries(&room).unwrap() {
            eprintln!(
                "receipt {key:?}: {} bytes",
                postcard::to_allocvec(&row).unwrap().len()
            );
        }
        for change in changes.iter().rev().take(4) {
            eprintln!(
                "recent change {} bytes, {} ops, deps {}",
                change.raw_bytes().len(),
                change.len(),
                change.deps().len()
            );
        }
        for (key, bytes) in [
            ("coverage", qos_peer_state::coverage::FIELD_V2),
            ("receipts", qos_peer_state::durability::FIELD_V2),
        ] {
            if let Ok(Some((automerge::Value::Object(_), object))) =
                automerge::ReadDoc::get(&room, automerge::ROOT, bytes)
            {
                for route in automerge::ReadDoc::keys(&room, &object) {
                    if let Ok(Some((automerge::Value::Scalar(value), _))) =
                        automerge::ReadDoc::get(&room, &object, route.as_str())
                    {
                        if let automerge::ScalarValue::Bytes(raw) = value.as_ref() {
                            eprintln!("v2 {key} {route:?}: {} stored bytes", raw.len());
                        }
                    }
                }
            }
        }
        if let Some(change) = changes.last() {
            eprintln!(
                "last change {} bytes: {:?}",
                change.raw_bytes().len(),
                change
                    .decode()
                    .operations
                    .iter()
                    .map(|op| format!("{:?}", op.key))
                    .collect::<Vec<_>>()
            );
        }
        for (keys, (count, bytes)) in &by_key {
            eprintln!("room history {count:>6} changes {bytes:>9} bytes: {keys}");
        }
    }
    (changes.len(), total, room.save().len())
}

fn no_whole_replies(store: &Store) {
    let state = serde_json::to_value(&store.state).unwrap();
    for field in ["join_responses", "membership_responses"] {
        for (key, entry) in state[field].as_object().unwrap() {
            assert!(
                entry.get("bootstrap").is_none(),
                "{field}.{key} retains a whole bootstrap"
            );
            assert!(entry["receipt"].is_object(), "{field}.{key} has no receipt");
        }
    }
}

#[test]
fn store_stays_bounded_over_many_agent_steps_refreshes_and_retries() {
    let (dir, mut store, owner) = crate::agent_owner_tests::enrolled();
    let now = qos_relay::clock::now_ns();
    let guest = join_guest(&mut store, owner, now);
    store.activate_delivery_storage(now).unwrap();
    let room = store.state.room.clone();
    // Agent-typed text of the size a streamed type-text step carries.
    let text = "streamed agent text ".repeat(64);
    // At least 200 steps, then until the first rotation (at most 400).
    let mut steps = 0u32;
    let mut sizes = Vec::new();
    let mut drops = Vec::new();
    let mut largest = 0u64;
    for step in 0..400u32 {
        if step >= 200 && !drops.is_empty() {
            break;
        }
        steps = step + 1;
        store
            .action(
                owner,
                &ActionRequest {
                    version: 1,
                    room: room.clone(),
                    invocation_id: format!("agent-step-{step}"),
                    operation: crate::NativeOperation::WriterInsert { text: text.clone() },
                },
                now,
            )
            .unwrap();
        if step % 8 == 0 {
            let mut id = [0; 32];
            id[..4].copy_from_slice(&step.to_le_bytes());
            id[31] = 1;
            refresh(&mut store, &guest, id, now);
        }
        if step % 10 == 9 {
            // The owner's retention maintenance (every 30 s in the service).
            let before = saved_bytes(dir.path());
            if let Some(report) = store.retain_delivery(now).unwrap() {
                if report.rotated.is_some() {
                    drops.push((before, saved_bytes(dir.path())));
                }
            }
        }
        let bytes = saved_bytes(dir.path());
        largest = largest.max(bytes);
        if step % 40 == 39 {
            sizes.push(bytes);
        }
    }
    if let Ok(path) = std::env::var("QOS_GROWTH_DUMP") {
        std::fs::copy(dir.path().join("state.json"), path).unwrap();
    }
    eprintln!(
        "saved Store bytes every 40 steps: {sizes:?}; largest {largest}; rotations (before, after) {drops:?}"
    );
    let (changes, change_bytes, saved) = room_history(&store, now);
    eprintln!(
        "room governance after {steps} steps (generation {}): {changes} changes, {change_bytes} change bytes, {saved} saved bytes",
        store.state.generation
    );
    no_whole_replies(&store);
    assert_eq!(store.state.join_responses.len(), 1);
    assert_eq!(store.state.membership_responses.len(), 1);
    let pressure = store.delivery_pressure(now).unwrap();
    // Retention actually bounds history: a rotation happened within a few
    // hundred commits (not at half of the 64 MiB pool) and freed most of it.
    assert!(
        !drops.is_empty(),
        "history retention never rotated: {pressure:?}"
    );
    for (before, after) in &drops {
        assert!(
            after * 10 < before * 7,
            "rotation freed too little: {drops:?}"
        );
    }
    assert!(
        pressure.pool_bytes.saturating_sub(pressure.baseline_bytes)
            < 2 * HISTORY_BYTES.max(pressure.baseline_bytes),
        "{pressure:?}"
    );
    assert!(pressure.replay < 2 * HISTORY_COMMITS && pressure.replay < MAX_REFERENCES);
    assert!(largest < 6 * 1024 * 1024, "largest saved Store {largest}");
    // A full reopen validates the retained aggregate.
    drop(store);
    let store = Store::open(dir.path(), "agent-admission").unwrap();
    no_whole_replies(&store);
}

#[test]
fn legacy_whole_reply_entries_are_compacted_on_the_next_commit() {
    let (dir, mut store, owner) = crate::agent_owner_tests::enrolled();
    let now = qos_relay::clock::now_ns();
    let guest = join_guest(&mut store, owner, now);
    // Without process reuse (a restarted host) the refresh commits a receipt.
    crate::collaboration::forget_reused_governance(dir.path());
    let bootstrap = refresh(&mut store, &guest, [5; 32], now);
    // An older Store retained the whole reply for its refresh.
    let mut legacy = serde_json::to_value(&store.state).unwrap();
    let key = hex::encode([5u8; 32]);
    let entry = &mut legacy["membership_responses"][&key];
    let digest = entry["proof_digest"].clone();
    *entry = serde_json::json!({
        "proof_digest": digest,
        "bootstrap": serde_json::to_value(&bootstrap).unwrap(),
    });
    store.state.membership_responses =
        serde_json::from_value(legacy["membership_responses"].take()).unwrap();
    // The legacy reply still answers its own lost-reply retry.
    let invite_before = store.state.membership_responses[&key]
        .receipt()
        .unwrap()
        .invite_id;
    let mut next = store.state.clone();
    next.canonical_intents.clear();
    store.commit(next).unwrap();
    no_whole_replies(&store);
    let receipt = store.state.membership_responses[&key].receipt().unwrap();
    assert_eq!(receipt.invite_id, invite_before);
    assert_eq!(receipt.device, guest.cert.device_public_key);
    // A compacted legacy entry has no replay window left.
    drop(store);
    let reopened = Store::open(dir.path(), "agent-admission").unwrap();
    no_whole_replies(&reopened);
}

/// Diagnostic: room governance history of a copied lab Store
/// (`QOS_LAB_STATE=<state.json> QOS_LAB_MACHINE=<machine id>`).
#[test]
#[ignore = "reads a lab Store named by QOS_LAB_STATE and QOS_LAB_MACHINE"]
fn lab_room_history() {
    let (Ok(path), Ok(machine)) = (
        std::env::var("QOS_LAB_STATE"),
        std::env::var("QOS_LAB_MACHINE"),
    ) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(path, dir.path().join("state.json")).unwrap();
    let store = Store::open(dir.path(), &machine).unwrap();
    let now = qos_relay::clock::now_ns();
    let (changes, bytes, saved) = room_history(&store, now);
    let peer = store.peer_at(now).unwrap();
    let room = peer.admitted_room().unwrap();
    eprintln!(
        "lab room: generation {} changes {changes} change bytes {bytes} saved {saved} coverage routes {} durable routes {}",
        store.state.generation,
        qos_peer_state::coverage::entries(room).unwrap().len(),
        qos_peer_state::durability::entries(room).unwrap().len(),
    );
}

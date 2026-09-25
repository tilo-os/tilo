//! Public, synthetic real-host failure capture; not an atomic store snapshot.
use super::*;
use crate::{coedit::CoeditPayload, transport::Envelope};
use qos_peer_state::{NeighbourMsg, StatePeer, retained_proof::*};

pub(super) fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shell-proof")
}
pub(super) fn metadata() -> serde_json::Value {
    serde_json::from_slice(&fs::read(fixture().join("failure.json")).unwrap()).unwrap()
}
pub(crate) fn binding() -> RetentionAuthority {
    let meta = metadata();
    RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap()
}
pub(super) fn parts(payload: CoeditPayload) -> ([u8; 32], Vec<u8>, RoomInput) {
    match payload {
        CoeditPayload::Checkpoint { channel, body } => (channel, body, RoomInput::Checkpoint),
        CoeditPayload::RetainedDelta { channel, body } => (channel, body, RoomInput::Delta),
    }
}

#[test]
fn shell_authority_binding_is_local_persistent_conflict_checked_and_workspace_bound() {
    let temp = tempfile::tempdir().unwrap();
    let authority = binding();
    let workspace = crate::room_workspace_id(authority.room()).unwrap();
    let mut store = WorkspaceStore::open(temp.path(), workspace, true).unwrap();
    assert!(store.shell_authority().is_none());
    store.bind_shell_authority(authority.clone()).unwrap();
    assert_eq!(store.shell_authority(), Some(&authority));
    let stored = fs::read(store.root().join("shell-authority-v1.json")).unwrap();
    drop(store);
    let mut store = WorkspaceStore::open(temp.path(), workspace, true).unwrap();
    assert!(
        store.shell_authority().is_none(),
        "disk content cannot independently grant local authority"
    );
    store.bind_shell_authority(authority.clone()).unwrap();
    assert_eq!(
        fs::read(store.root().join("shell-authority-v1.json")).unwrap(),
        stored
    );
    let meta = metadata();
    let other = RetentionAuthority::new(
        authority.room(),
        serde_json::from_value(meta["certificates"][1].clone()).unwrap(),
    )
    .unwrap();
    assert!(store.bind_shell_authority(other).is_err());
    assert_eq!(store.shell_authority(), Some(&authority));
    assert_eq!(
        fs::read(store.root().join("shell-authority-v1.json")).unwrap(),
        stored
    );
    let mut wrong = WorkspaceStore::open(temp.path(), workspace ^ 1, true).unwrap();
    assert!(wrong.bind_shell_authority(authority).is_err());
    assert!(wrong.shell_authority().is_none());
    assert!(!wrong.root().join("shell-authority-v1.json").exists());
}

#[test]
fn shell_authority_failed_or_malformed_persistence_never_installs_trust() {
    let temp = tempfile::tempdir().unwrap();
    let authority = binding();
    let workspace = crate::room_workspace_id(authority.room()).unwrap();
    let mut store = WorkspaceStore::open(temp.path(), workspace, true).unwrap();
    let path = store.root().join("shell-authority-v1.json");
    fs::create_dir(&path).unwrap();
    assert!(store.bind_shell_authority(authority.clone()).is_err());
    assert!(store.shell_authority().is_none());
    fs::remove_dir(&path).unwrap();
    fs::write(&path, b"malformed").unwrap();
    assert!(store.bind_shell_authority(authority).is_err());
    assert!(store.shell_authority().is_none());
    assert_eq!(fs::read(path).unwrap(), b"malformed");
}

#[test]
fn captured_shell_corrupt_room_checksum_never_proves_or_covers() {
    let authority = binding();
    let source = authority.governor().device_public_key;
    let bytes = fs::read(fixture().join("frames/0006.postcard")).unwrap();
    let (channel, body, kind) = parts(CoeditPayload::decode(&bytes).unwrap());
    let original = verify_room(&authority, source, channel, &body, kind, None).unwrap();
    let mut message: NeighbourMsg = postcard::from_bytes(&body).unwrap();
    let NeighbourMsg::SyncResp { changes, .. } = &mut message else {
        panic!()
    };
    changes[0][4] ^= 1;
    let corrupted = postcard::to_allocvec(&message).unwrap();
    let checkpoint = verify_room(
        &authority,
        source,
        channel,
        &corrupted,
        RoomInput::Checkpoint,
        None,
    );
    let delta = verify_room(
        &authority,
        source,
        channel,
        &corrupted,
        RoomInput::Delta,
        Some(&original),
    );
    let coverage = original.covers_frame(source, channel, &corrupted);
    assert_eq!(
        (
            matches!(checkpoint, Err(ProofError::Unknown)),
            matches!(delta, Err(ProofError::Unknown)),
            coverage
        ),
        (true, true, CoverageDecision::Unknown),
        "the actual captured room checksum must be checked even when its change hash is known",
    );
}

#[test]
fn captured_shell_sequence_four_is_unpaired_with_current_seven() {
    let meta = metadata();
    let binding = binding();
    let source = binding.governor().device_public_key;
    let now = web_time::SystemTime::UNIX_EPOCH
        + web_time::Duration::from_nanos(meta["now_ns"].as_u64().unwrap());
    let mut peer = StatePeer::restore(
        &fs::read(fixture().join("initial-peer.postcard")).unwrap(),
        binding.room(),
        binding.governor().clone(),
        serde_json::from_value(meta["certificates"].clone()).unwrap(),
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    let mut proof = None;
    let mut first_room = None;
    let mut rejected = None;
    for i in 0..12 {
        let bytes = fs::read(fixture().join(format!("frames/{i:04}.postcard"))).unwrap();
        let (channel, body, kind) = parts(CoeditPayload::decode(&bytes).unwrap());
        let msg: NeighbourMsg = postcard::from_bytes(&body).unwrap();
        if matches!(msg, NeighbourMsg::SyncResp { .. }) {
            let next = verify_room(&binding, source, channel, &body, kind, proof.as_ref()).unwrap();
            if first_room.is_none() {
                first_room = Some(next.clone());
            }
            proof = Some(next);
        }
        match peer.receive_from(
            qos_peer_state::TransportOrigin::RelayDelta(source),
            msg,
            now,
        ) {
            Ok(outcome) => assert_eq!(outcome.rejected, 0, "prefix {i}"),
            Err(error) => {
                assert_eq!(i, 11);
                assert_eq!(error, "checkpoint does not match governed coverage");
                rejected = Some((channel, body));
            }
        }
    }
    let room = proof.unwrap();
    let (channel, body) = rejected.unwrap();
    let old = verify_snapshot(first_room.as_ref().unwrap(), source, channel, &body).unwrap();
    assert_eq!(old.coverage().sequence, 4);
    assert!(matches!(
        verify_snapshot(&room, source, channel, &body),
        Err(ProofError::Unpaired)
    ));
    let coverage = room.coverages().unwrap();
    let expected = coverage
        .get(&qos_peer_state::coverage::route_key(
            "dev.qos.notes",
            "tab:8b11000000000004",
        ))
        .unwrap();
    assert_eq!(expected.sequence, 7);
    assert_eq!(expected.payload_hash, old.coverage().payload_hash);

    // The copied store already contained the valid exact successor. Decode the
    // length-prefixed Envelope, then CoeditPayload, then NeighbourMsg.
    let retained = fixture().join("retained/v2").join(format!(
        "{}-{}",
        hex::encode(source),
        hex::encode(channel)
    ));
    let current = fs::read_to_string(retained.join("CURRENT")).unwrap();
    let mut found = false;
    for entry in fs::read_dir(retained.join(current.trim())).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        let Envelope::Delta {
            delta,
            author_device_public_key,
            ..
        } = crate::transport::decode(&bytes[4..]).unwrap()
        else {
            panic!()
        };
        let (channel, body, _) = parts(CoeditPayload::decode(&delta).unwrap());
        if let Ok(successor) = verify_snapshot(&room, author_device_public_key, channel, &body) {
            assert_eq!(successor.coverage().sequence, 7);
            assert!(successor.covers_snapshot(&old));
            found = true;
        }
    }
    assert!(
        found,
        "exact sequence-seven snapshot must be available in captured archive"
    );
}

//! The real-owner shell-selection scenario, generated once per test process,
//! and the helpers the selection, store, peer and persistence tests read it
//! with. Built only for tests and the `test-support` feature.
use crate::retained_proof::RetentionAuthority;
use std::path::{Path, PathBuf};
use tilo_peer_state::{StatePeer, TransportOrigin};
use tilo_relay_client::coedit::CoeditPayload;
use tilo_relay_protocol::transport::Envelope;

mod shell_selection_fixture;

/// The directory holding the generated shell-selection scenario (see
/// `shell_selection_fixture` for its layout).
pub fn producer_fixture() -> PathBuf {
    static FIXTURE: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            shell_selection_fixture::write(
                &shell_selection_fixture::generate(),
                &dir.path().join("shell-selection"),
            );
            dir
        })
        .path()
        .join("shell-selection")
}
/// The scenario's `manifest.json`.
pub fn producer_metadata() -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(producer_fixture().join("manifest.json")).unwrap())
        .unwrap()
}
/// The governor's retention authority over the scenario's room.
pub fn producer_authority(meta: &serde_json::Value) -> RetentionAuthority {
    RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap()
}
/// The retained relay frame of the manifest `row`, authored by the
/// governor's device at `time`.
pub fn producer_frame(meta: &serde_json::Value, row: &serde_json::Value, time: u64) -> Vec<u8> {
    let channel = hex::decode(row["channel"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let body = std::fs::read(producer_fixture().join(row["file"].as_str().unwrap())).unwrap();
    let payload = if row["kind"] == "Checkpoint" {
        CoeditPayload::Checkpoint { channel, body }
    } else {
        CoeditPayload::RetainedDelta { channel, body }
    };
    tilo_relay_client::frame::encode(&Envelope::Delta {
        workspace_id: tilo_relay_protocol::room_workspace_id(meta["room"].as_str().unwrap())
            .unwrap(),
        author_device_public_key: serde_json::from_value::<tilo_user_identity::DeviceCert>(
            meta["authority"].clone(),
        )
        .unwrap()
        .device_public_key,
        authored_at_ns: time,
        signature: [0; 64],
        delta: payload.encode().unwrap(),
    })
    .unwrap()
}
/// Receive the relay delta `frame` into `peer` at `now`, as a retained
/// relay delta when `retained`.
///
/// # Errors
///
/// Returns the peer's refusal.
pub fn receive(
    peer: &mut StatePeer,
    frame: Envelope,
    retained: bool,
    now: web_time::SystemTime,
) -> Result<tilo_peer_state::ReceiveOutcome, String> {
    let Envelope::Delta {
        author_device_public_key,
        delta,
        ..
    } = frame
    else {
        return Err("expected a delta frame".to_string());
    };
    let (CoeditPayload::Checkpoint { body, .. } | CoeditPayload::RetainedDelta { body, .. }) =
        CoeditPayload::decode(&delta).unwrap();
    let origin = if retained {
        TransportOrigin::RetainedRelayDelta(author_device_public_key)
    } else {
        TransportOrigin::RelayDelta(author_device_public_key)
    };
    peer.receive_from(origin, postcard::from_bytes(&body).unwrap(), now)
}
/// Restore the peer export `initial` at the scenario's admission time and
/// admit `frames` as relay deltas, none of them rejected.
pub fn admit_frames(meta: &serde_json::Value, initial: &Path, frames: Vec<Envelope>) -> StatePeer {
    admit_frames_with_origin(meta, initial, frames, false)
}
/// [`admit_frames`], as retained relay deltas when `retained`.
pub fn admit_frames_with_origin(
    meta: &serde_json::Value,
    initial: &Path,
    frames: Vec<Envelope>,
    retained: bool,
) -> StatePeer {
    let now = web_time::SystemTime::UNIX_EPOCH
        .checked_add(web_time::Duration::from_nanos(
            meta["now_ns"].as_u64().unwrap(),
        ))
        .unwrap();
    let mut peer = StatePeer::restore(
        &std::fs::read(initial).unwrap(),
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
        serde_json::from_value(meta["certificates"].clone()).unwrap(),
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    for (index, frame) in frames.into_iter().enumerate() {
        let outcome = receive(&mut peer, frame, retained, now)
            .unwrap_or_else(|e| panic!("replay frame {index}: {e}"));
        assert_eq!(outcome.rejected, 0, "deferred rejection at {index}");
    }
    peer
}

//! Existing native shell retained-channel policy shared with browser peers.
use crate::NeighbourMsg;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Retention {
    Checkpoint,
    Delta,
}

pub fn retention(msg: &NeighbourMsg, shell_checkpoint: bool) -> Option<([u8; 32], Retention)> {
    let (route, kind) = match msg {
        NeighbourMsg::SyncResp { .. } => (
            b"shell-tree".as_slice(),
            if shell_checkpoint {
                Retention::Checkpoint
            } else {
                Retention::Delta
            },
        ),
        NeighbourMsg::AppDocument {
            app_id,
            record_id,
            envelope,
            ..
        } => {
            let route = format!("app\0{app_id}\0{record_id}");
            let mutation = collab_types::DocumentMutation::decode(&envelope.payload).ok()?;
            let retention = match mutation {
                collab_types::DocumentMutation::Snapshot(_) => Retention::Checkpoint,
                collab_types::DocumentMutation::Incremental(_) => Retention::Delta,
            };
            return Some((*blake3::hash(route.as_bytes()).as_bytes(), retention));
        }
        NeighbourMsg::PersonalContinuityCheckpoint {
            user_public_key, ..
        } => {
            let mut route = Vec::new();
            route.extend_from_slice(b"personal-continuity\0");
            route.extend_from_slice(user_public_key);
            return Some((*blake3::hash(&route).as_bytes(), Retention::Checkpoint));
        }
        _ => return None,
    };
    Some((*blake3::hash(route).as_bytes(), kind))
}

//! The records a replica holds because the workspace's governance covers them
//! as empty.
//!
//! An empty record has no bytes to send anyone. Nothing of it travels: a device
//! that joins after it was made is replayed no change of it, and no peer hands
//! it over whole. The coverage row the governor signs says what it is — no
//! heads, the hash of the empty snapshot — and that is enough for a replica to
//! hold it.

use super::{
    AcceptedCheckpoint, AutoCommit, Ed25519PublicKey, InboundState, Route, StatePeer, backend, err,
};

/// Hold every record the workspace's governance covers as empty and `peer`
/// holds nothing of.
///
/// The workspace holds such a record and every member may write it, its desk's
/// for one. The replica holds it as if the checkpoint had reached it, so a
/// write of its own builds on a record it has, and the checkpoint it would
/// retransmit is the one the coverage names.
pub(super) fn hold_covered_empty_records(peer: &mut StatePeer) -> Result<usize, String> {
    let Some(workspace) = peer.core.workspace_doc.as_deref() else {
        return Ok(0);
    };
    let coverages = crate::coverage::entries(workspace)?;
    let mut held = 0_usize;
    for coverage in coverages.values() {
        let route = Route {
            app: coverage.app.clone(),
            record: coverage.record.clone(),
        };
        if coverage.workspace != peer.core.workspace
            || !coverage.heads.is_empty()
            || peer.core.documents.contains_key(&route)
            || !peer.signed_by_owner(&coverage.author)
        {
            continue;
        }
        // The one snapshot the coverage's empty heads and hash name: the
        // record holding no change at all. A coverage that names any other
        // checkpoint is not this replica's to make up.
        let checkpoint = AcceptedCheckpoint::new(AutoCommit::new().save());
        let decoded = checkpoint.decoded()?;
        if decoded.heads != coverage.heads || decoded.payload_hash != coverage.payload_hash {
            continue;
        }
        let mut channel = InboundState::default();
        channel.workspace.clone_from(&peer.core.workspace);
        for (author, sequence) in &coverage.watermarks {
            let key: [u8; 32] = hex::decode(author)
                .map_err(err)?
                .try_into()
                .map_err(|_| "coverage key length")?;
            let floor = channel
                .last_seq_per_author
                .entry(Ed25519PublicKey::from_bytes(key))
                .or_default();
            *floor = (*floor).max(*sequence);
        }
        peer.core.documents.insert(
            route,
            backend::PeerDocument {
                doc: AutoCommit::new().into(),
                channel,
                encryption_required: false,
                covered: coverage.watermarks.clone(),
                covered_snapshot: Some(checkpoint),
                sealed: backend::SealedLog::default(),
            }
            .into(),
        );
        held = held.saturating_add(1);
    }
    Ok(held)
}

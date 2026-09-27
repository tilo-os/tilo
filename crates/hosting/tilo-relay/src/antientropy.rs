//! Client-side anti-entropy: reconcile a local Automerge document
//! against a host's materialized copy, independent of the
//! store-and-forward queue.
//!
//! The host tier fans out deltas live and buffers them per-offline-peer
//! on disk. Both mechanisms can still leave a peer behind: a delta
//! fanned out to a *live* subscriber whose wire dies before the write
//! lands was counted delivered — so it is never re-queued — and a
//! delta already drained from the pending queue is deleted from disk
//! before the peer has durably applied it. Either way the peer can end
//! up missing a change with nothing left in its queue to recover it.
//!
//! Anti-entropy closes that gap. The peer advertises the Automerge
//! heads it currently holds ([`Envelope::SyncRequest`]); the host
//! replies with exactly the changes the peer is missing
//! ([`Envelope::SyncResponse`]), computed against its materialized
//! pinned doc (see [`crate::workspace::WorkspaceRuntime::changes_since`]).
//! Applying the response converges the peer regardless of what the
//! queue delivered.
//!
//! Two ways to use it:
//!
//! - Fold [`heads_wire`] + [`apply_sync_response`] into an existing
//!   single recv loop — send a `SyncRequest` whenever you want to
//!   reconcile (on subscribe, on a timer, on suspicion of loss) and
//!   handle the `SyncResponse` alongside your other inbound envelopes.
//! - Or call [`reconcile`] for a self-contained round trip on a client
//!   whose inbound stream you are not otherwise draining.

use n0_future::time::{Duration, Instant};
use tilo_automerge::ApplyIncoming;

use automerge::AutoCommit;

use crate::client::RelayClient;
use crate::transport::Envelope;

/// Failure modes for an anti-entropy round trip.
#[derive(Debug)]
pub enum SyncError {
    /// The relay connection is gone (outbound closed or inbound EOF).
    Disconnected,
    /// No matching [`Envelope::SyncResponse`] arrived within the budget.
    Timeout,
    /// The host's response did not apply as an Automerge delta.
    Apply(automerge::AutomergeError),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Disconnected => f.write_str("anti-entropy: relay disconnected"),
            SyncError::Timeout => f.write_str("anti-entropy: timed out waiting for SyncResponse"),
            SyncError::Apply(e) => write!(f, "anti-entropy: apply failed: {e}"),
        }
    }
}

impl std::error::Error for SyncError {}

/// `doc`'s current heads encoded for the wire — the `have_heads` a
/// [`Envelope::SyncRequest`] advertises. An empty result means the doc
/// is empty and the host will return its whole materialized document.
pub fn heads_wire(doc: &mut AutoCommit) -> Vec<[u8; 32]> {
    doc.get_heads().into_iter().map(|h| h.0).collect()
}

/// Apply the `delta_since` bytes from an [`Envelope::SyncResponse`] into
/// `doc`. Returns the number of new operations applied (0 = already
/// converged / empty response).
///
/// # Errors
///
/// Returns an error if `delta_since` is non-empty but does not decode
/// as a valid Automerge incremental change series.
pub fn apply_sync_response(
    doc: &mut AutoCommit,
    delta_since: &[u8],
) -> Result<usize, automerge::AutomergeError> {
    if delta_since.is_empty() {
        return Ok(0);
    }
    doc.load_incoming(delta_since)
}

/// Send a [`Envelope::SyncRequest`] advertising `doc`'s current heads
/// for `workspace_id`. Fire-and-forget; handle the matching
/// [`Envelope::SyncResponse`] in your recv loop with
/// [`apply_sync_response`].
///
/// # Errors
///
/// Returns [`SyncError::Disconnected`] if the relay connection is gone.
pub fn send_sync_request(
    client: &RelayClient,
    workspace_id: u64,
    doc: &mut AutoCommit,
) -> Result<(), SyncError> {
    client
        .send(Envelope::SyncRequest {
            workspace_id,
            have_heads: heads_wire(doc),
        })
        .map_err(|_| SyncError::Disconnected)
}

/// Run one anti-entropy round trip against the host: advertise `doc`'s
/// heads, wait up to `timeout` for the matching
/// [`Envelope::SyncResponse`], and apply the missing changes into `doc`.
/// Returns the number of new operations applied (0 = already converged).
///
/// Envelopes other than the matching `SyncResponse` are skipped while
/// waiting, so this is intended for a dedicated reconcile step (right
/// after `Subscribe`, or on a periodic timer) — not to be run
/// concurrently with an app's main recv loop on the same client, which
/// would race for the same inbound stream. For loop-integrated use,
/// prefer [`send_sync_request`] + [`apply_sync_response`].
///
/// # Errors
///
/// Returns [`SyncError::Disconnected`] if the relay connection is gone,
/// [`SyncError::Timeout`] if no matching `SyncResponse` arrives within
/// `timeout` (or `timeout` itself can't be added to the current time),
/// and [`SyncError::Apply`] if the response's changes fail to apply.
pub async fn reconcile(
    client: &RelayClient,
    workspace_id: u64,
    doc: &mut AutoCommit,
    timeout: Duration,
) -> Result<usize, SyncError> {
    send_sync_request(client, workspace_id, doc)?;
    // A `timeout` large enough to overflow `Instant`'s representable
    // range cannot be waited out anyway; treat it the same as running
    // out of budget rather than panicking on the addition.
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        return Err(SyncError::Timeout);
    };
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(SyncError::Timeout);
        }
        match n0_future::time::timeout(remaining, client.recv()).await {
            Ok(Some(Envelope::SyncResponse {
                workspace_id: ws,
                delta_since,
            })) if ws == workspace_id => {
                return apply_sync_response(doc, &delta_since).map_err(SyncError::Apply);
            }
            // Unrelated envelope (Ready, a live Delta, presence, a
            // SyncResponse for another workspace); keep waiting.
            Ok(Some(_)) => {}
            Ok(None) => return Err(SyncError::Disconnected),
            Err(_) => return Err(SyncError::Timeout),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;
    use automerge::transaction::Transactable;

    #[test]
    fn sync_heads_and_responses_converge_a_lagging_doc() {
        // Phase `heads_wire_round_trips_through_changehash`.
        {
            let mut doc = AutoCommit::new();
            doc.put(ROOT, "k", "v").unwrap();
            let wire = heads_wire(&mut doc);
            assert_eq!(wire.len(), 1);
            // The wire hash equals the doc's ChangeHash inner bytes.
            assert_eq!(wire[0], doc.get_heads()[0].0);
        }
        // Phase `apply_sync_response_empty_is_noop`.
        {
            let mut doc = AutoCommit::new();
            doc.put(ROOT, "k", "v").unwrap();
            let before = doc.get_heads();
            assert_eq!(apply_sync_response(&mut doc, &[]).unwrap(), 0);
            assert_eq!(doc.get_heads(), before);
        }
        // Phase `apply_sync_response_converges_a_lagging_doc`.
        {
            // A "host" doc with two edits; a lagging peer with none. The
            // bytes save_after([]) produces must load the peer to identical
            // heads — the exact wire contract SyncResponse carries.
            let mut host = AutoCommit::new();
            host.put(ROOT, "a", 1).unwrap();
            host.put(ROOT, "b", 2).unwrap();
            let delta_since = host.save_after(&[]);

            let mut peer = AutoCommit::new();
            assert!(apply_sync_response(&mut peer, &delta_since).unwrap() > 0);
            assert_eq!(peer.get_heads(), host.get_heads());
        }
    }
}

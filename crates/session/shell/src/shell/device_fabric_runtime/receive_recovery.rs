//! Receiver decisions for durable Nearby transfers whose live offer was lost.

use super::{NearbyDropState, NearbyTransferHistory};
use crate::shell::device_fabric::PersonalFabricAction;
use crate::shell::nearby_transfer::NearbyTransferStore;
use crate::shell::personal_continuity::DeviceId;

/// Discard only the selected recovered receive. Files has already authenticated
/// the workspace/connection-bound handle and checked continuity policy. Cleanup
/// is local: an offline sender must not force the receiver to keep unwanted bytes.
pub(super) fn decline_recovered_receive(
    store: &mut NearbyTransferStore,
    state: &mut NearbyDropState,
    transfer_id: [u8; 16],
) -> Result<RecoveredReceiveDecline, String> {
    // A media handoff's receive belongs to its handoff, not to Files.
    let entry = store
        .inbox()
        .iter()
        .find(|entry| {
            entry.id == transfer_id
                && entry.kind != crate::shell::nearby_transfer::TransferKind::MediaHandoff
        })
        .cloned()
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-consent-expired"))?;
    let peer = DeviceId::new(entry.source).map_err(|error| error.to_string())?;
    store.cancel_incoming(transfer_id)?;
    state.destinations.remove(&transfer_id);
    state.progress.remove(&transfer_id);
    state.record_history(NearbyTransferHistory {
        transfer_id,
        peer: peer.clone(),
        name: entry.name,
        phase: runtime_collab::NearbyTransferPhase::Cancelled,
        total_bytes: entry.total_bytes,
    });
    Ok(RecoveredReceiveDecline {
        peer,
        action: PersonalFabricAction::NearbyTransferCancel {
            request_id: entry.request_id,
            transfer_id,
        },
    })
}

/// The notification owns no store or runtime guard, so transport work happens
/// only after local cleanup releases both locks.
pub(super) struct RecoveredReceiveDecline {
    peer: DeviceId,
    action: PersonalFabricAction,
}

impl RecoveredReceiveDecline {
    pub(super) fn notify(
        self,
        send: impl FnOnce(&DeviceId, PersonalFabricAction) -> Result<[u8; 16], String>,
    ) {
        // Existing peers validate request and transfer against the exact
        // certified sender. Offline notification cannot undo local cleanup.
        let _ = send(&self.peer, self.action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::nearby_transfer::{CHECKPOINT_BYTES, IncomingManifest, TransferKind};

    #[test]
    fn recovered_receive_decline_removes_checkpoint_even_when_sender_is_offline() {
        let root = tempfile::tempdir().expect("receive fixture");
        let spool = root.path().join("spool");
        let transfer_id = [41; 16];
        let request_id = [42; 16];
        let bytes = vec![7; usize::try_from(CHECKPOINT_BYTES).expect("bounded checkpoint")];
        let manifest = IncomingManifest {
            id: transfer_id,
            source: "certified-source".into(),
            request_id,
            name: "report.bin".into(),
            kind: TransferKind::File,
            total_bytes: CHECKPOINT_BYTES + 1,
            content_hash: [3; 32],
        };
        let mut store = NearbyTransferStore::open(spool.clone(), 10).expect("open store");
        store
            .begin_incoming(manifest, 10)
            .expect("consented receive");
        store
            .append(transfer_id, 0, &bytes)
            .expect("receive checkpoint");
        let partial = store.partial_path(transfer_id);
        assert!(partial.exists());
        drop(store);

        // A fresh runtime has no in-memory offer or destination approval. The
        // durable checkpoint is still discoverable through Files' broker.
        let mut store = NearbyTransferStore::open(spool.clone(), 11).expect("reopen store");
        let mut state = NearbyDropState::default();
        assert!(state.incoming.is_empty());
        assert_eq!(store.inbox()[0].committed_offset, CHECKPOINT_BYTES);
        let mut notification = None;
        decline_recovered_receive(&mut store, &mut state, transfer_id)
            .expect("decline completes locally")
            .notify(|peer, action| {
                notification = Some((peer.clone(), action));
                Err("sender is offline".into())
            });
        assert!(!partial.exists());
        assert!(store.inbox().is_empty());
        let history = state.history.front().expect("cancellation is visible");
        assert_eq!(history.transfer_id, transfer_id);
        assert_eq!(
            history.phase,
            runtime_collab::NearbyTransferPhase::Cancelled
        );
        assert_eq!(
            notification,
            Some((
                DeviceId::new("certified-source").expect("peer"),
                PersonalFabricAction::NearbyTransferCancel {
                    request_id,
                    transfer_id
                },
            ))
        );
        drop(store);
        let store = NearbyTransferStore::open(spool, 12).expect("reopen after decline");
        assert!(
            store.inbox().is_empty(),
            "cancelled receive must not reappear"
        );
        assert!(!partial.exists());
    }

    #[test]
    fn recovered_receive_decline_rejects_unknown_id_without_touching_another_receive() {
        let root = tempfile::tempdir().expect("receive fixture");
        let mut store = NearbyTransferStore::open(root.path().join("spool"), 10).expect("store");
        let id = [51; 16];
        store
            .begin_incoming(
                IncomingManifest {
                    id,
                    source: "certified-source".into(),
                    request_id: [52; 16],
                    name: "keep.bin".into(),
                    kind: TransferKind::File,
                    total_bytes: 1,
                    content_hash: *blake3::hash(&[7]).as_bytes(),
                },
                10,
            )
            .expect("other consented receive");
        let partial = store.partial_path(id);
        let mut state = NearbyDropState::default();
        let mut notified = false;
        let result = decline_recovered_receive(&mut store, &mut state, [99; 16]).map(|declined| {
            declined.notify(|_, _| {
                notified = true;
                Ok([1; 16])
            });
        });
        assert!(result.is_err());
        assert!(!notified);
        assert!(state.history.is_empty());
        assert_eq!(store.inbox()[0].id, id);
        assert!(partial.exists());
    }
}

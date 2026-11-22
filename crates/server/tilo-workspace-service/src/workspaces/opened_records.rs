//! The records sessions opened, and where the host sends a write one lands on
//! afterwards.
//!
//! A device that reads a workspace over its link is sent every write of it.
//! A client that still reads through its session is sent the record it opened,
//! and then each write of that record as the host takes it in. This goes with
//! those clients' move onto the device link.

use std::collections::{BTreeMap, BTreeSet};

use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tokio::sync::mpsc;

use super::Workspaces;
use crate::transport::Envelope;

/// Every connection that opened a record, by workspace.
#[derive(Default)]
pub(crate) struct OpenedRecords(std::sync::Mutex<BTreeMap<WorkspaceId, Vec<Opened>>>);

/// One connection and the record channels the host answered it with.
struct Opened {
    device: [u8; 32],
    outbound: mpsc::Sender<Envelope>,
    disconnect: mpsc::Sender<()>,
    channels: BTreeSet<[u8; 32]>,
}

impl OpenedRecords {
    /// Send `device`'s connection `outbound` the writes of `channel` of
    /// `workspace` from now on.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the map is locked for the one entry it writes, and the lock is released with it"
    )]
    pub(super) fn watch(
        &self,
        workspace: WorkspaceId,
        device: [u8; 32],
        outbound: &mpsc::Sender<Envelope>,
        disconnect: &mpsc::Sender<()>,
        channel: [u8; 32],
    ) {
        let mut sessions = self.0.lock_recover();
        let opened = sessions.entry(workspace).or_default();
        match opened
            .iter_mut()
            .find(|opened| opened.device == device && opened.outbound.same_channel(outbound))
        {
            Some(opened) => {
                opened.channels.insert(channel);
            }
            None => opened.push(Opened {
                device,
                outbound: outbound.clone(),
                disconnect: disconnect.clone(),
                channels: BTreeSet::from([channel]),
            }),
        }
    }

    /// Whether any connection of `workspace` opened a record: a workspace no
    /// session reads has nowhere to send a write, and none is encoded for it.
    pub(super) fn any(&self, workspace: WorkspaceId) -> bool {
        let sessions = self.0.lock_recover();
        sessions
            .get(&workspace)
            .is_some_and(|opened| !opened.is_empty())
    }

    /// Send `envelope` to each connection of `workspace` that opened one of
    /// `channels` and `may_read` admits, and forget the connections that took
    /// no more: a connection whose queue is full is disconnected, and reads
    /// what it opened again on the session it comes back with. How many
    /// connections it reached.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "each connection is sent to under the one lock, and the lock is released with it"
    )]
    pub(super) fn deliver(
        &self,
        workspace: WorkspaceId,
        channels: &BTreeSet<[u8; 32]>,
        may_read: impl Fn([u8; 32]) -> bool,
        envelope: &Envelope,
    ) -> usize {
        let mut sessions = self.0.lock_recover();
        let Some(opened) = sessions.get_mut(&workspace) else {
            return 0;
        };
        let mut reached = 0_usize;
        opened.retain(|opened| {
            if opened.channels.is_disjoint(channels) || !may_read(opened.device) {
                return true;
            }
            match opened.outbound.try_send(envelope.clone()) {
                Ok(()) => {
                    reached = reached.saturating_add(1);
                    true
                }
                // The write is in the store: the connection reads the record
                // again when it comes back, rather than the write waiting in
                // a queue it cannot take.
                Err(mpsc::error::TrySendError::Full(_)) => {
                    let _ = opened.disconnect.try_send(());
                    false
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        });
        reached
    }
}

impl Workspaces {
    /// Send `device`'s connection `outbound` the writes of `channel`, a record
    /// the connection opened, from now on.
    pub(crate) fn watch_record(
        &self,
        workspace: WorkspaceId,
        device: [u8; 32],
        outbound: &mpsc::Sender<Envelope>,
        disconnect: &mpsc::Sender<()>,
        channel: [u8; 32],
    ) {
        self.inner
            .opened_records
            .watch(workspace, device, outbound, disconnect, channel);
    }

    /// Send `message`, a write of a record `workspace` took in, to the sessions
    /// that opened it: what a client reading through a session hears after the
    /// frames it opened the record with. A part of a chained record also reaches
    /// the sessions that opened its chain's head, and a channel's record alone
    /// its own.
    pub(crate) fn deliver_record_write(
        &self,
        workspace: WorkspaceId,
        message: &neighbour_wire::NeighbourMsg,
    ) {
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            document_id,
            ..
        } = message
        else {
            return;
        };
        if !self.inner.opened_records.any(workspace) {
            return;
        }
        let Some(link) = self.inner.direct.link(workspace) else {
            return;
        };
        let Some(envelope) = crate::peer::records::delta_of(workspace, message) else {
            return;
        };
        let mut channels =
            BTreeSet::from([neighbour_wire::app_document_group(app_id, document_id)]);
        if let neighbour_wire::DocumentSync::Part { base, .. } =
            neighbour_wire::document_sync(document_id)
        {
            channels.insert(neighbour_wire::app_document_group(
                app_id,
                &neighbour_wire::chain_head(base),
            ));
        }
        self.inner.opened_records.deliver(
            workspace,
            &channels,
            |device| link.reads_of(device)(message),
            &envelope,
        );
    }
}

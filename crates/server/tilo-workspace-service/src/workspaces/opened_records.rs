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

/// A chain's newest part a subscription replays: its index, the channels the session holds it
/// by, and the frames kept of it.
type ChainHead = (u64, Vec<[u8; 32]>, Vec<Envelope>);

/// Each chain the workspace's store keeps, by the app and base its parts name.
type Chains = std::collections::BTreeMap<(String, String), ChainHead>;

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

    /// Watch `device`'s connection `outbound`, which opened no record yet: what
    /// reaches every session of `workspace` reaches it too.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the map is locked for the one entry it writes, and the lock is released with it"
    )]
    pub(super) fn watch_session(
        &self,
        workspace: WorkspaceId,
        device: [u8; 32],
        outbound: &mpsc::Sender<Envelope>,
        disconnect: &mpsc::Sender<()>,
    ) {
        let mut sessions = self.0.lock_recover();
        let opened = sessions.entry(workspace).or_default();
        if opened
            .iter()
            .any(|opened| opened.device == device && opened.outbound.same_channel(outbound))
        {
            return;
        }
        opened.push(Opened {
            device,
            outbound: outbound.clone(),
            disconnect: disconnect.clone(),
            channels: BTreeSet::new(),
        });
    }

    /// Send `envelope` to each connection of `workspace` that `may_read` admits,
    /// whether or not it opened a record, and forget the connections that took
    /// no more: a connection whose queue is full is disconnected, and reads what
    /// it needs again on the session it comes back with. How many it reached.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "each connection is sent to under the one lock, and the lock is released with it"
    )]
    pub(super) fn deliver_to_all(
        &self,
        workspace: WorkspaceId,
        may_read: impl Fn([u8; 32]) -> bool,
        hold: Option<[u8; 32]>,
        envelope: &Envelope,
    ) -> usize {
        let mut sessions = self.0.lock_recover();
        let Some(opened) = sessions.get_mut(&workspace) else {
            return 0;
        };
        let mut reached = 0_usize;
        opened.retain_mut(|opened| {
            if !may_read(opened.device) {
                return true;
            }
            match opened.outbound.try_send(envelope.clone()) {
                Ok(()) => {
                    reached = reached.saturating_add(1);
                    if let Some(hold) = hold {
                        opened.channels.insert(hold);
                    }
                    true
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    let _ = opened.disconnect.try_send(());
                    false
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        });
        reached
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
        hold: Option<[u8; 32]>,
        envelope: &Envelope,
    ) -> usize {
        let mut sessions = self.0.lock_recover();
        let Some(opened) = sessions.get_mut(&workspace) else {
            return 0;
        };
        let mut reached = 0_usize;
        opened.retain_mut(|opened| {
            if opened.channels.is_disjoint(channels) || !may_read(opened.device) {
                return true;
            }
            match opened.outbound.try_send(envelope.clone()) {
                Ok(()) => {
                    reached = reached.saturating_add(1);
                    if let Some(hold) = hold {
                        opened.channels.insert(hold);
                    }
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
    /// Watch `device`'s connection `outbound` from its subscription: what
    /// reaches every session of `workspace` reaches it too.
    pub(crate) fn watch_session(
        &self,
        workspace: WorkspaceId,
        device: [u8; 32],
        outbound: &mpsc::Sender<Envelope>,
        disconnect: &mpsc::Sender<()>,
    ) {
        self.inner
            .opened_records
            .watch_session(workspace, device, outbound, disconnect);
    }

    /// Each channel's governance the workspace keeps, as a link is sent them
    /// when it starts, for `device`: a page that reaches its workspace only over
    /// a session joins a record's group with them, and writes its first sealed
    /// record. Only what the link's other end would be sent.
    pub(crate) fn channel_governance(
        &self,
        workspace: WorkspaceId,
        device: [u8; 32],
    ) -> Vec<Envelope> {
        let Some(link) = self.inner.direct.link(workspace) else {
            return Vec::new();
        };
        let reads = link.reads_of(device);
        self.inner
            .store
            .channels(workspace)
            .into_iter()
            .filter(|(_, message)| reads(message))
            .filter_map(|(author, message)| {
                crate::peer::records::delta_of(workspace, author, &message)
            })
            .collect()
    }

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

    /// Send `message`, `author`'s frame `workspace` took in or carried, to the
    /// sessions a link to their device would get it: a write of a record reaches
    /// the sessions that opened it, a part also the sessions that opened its
    /// chain's head, and a channel's governance every session of the workspace,
    /// which is how a page that reaches its workspace only over a session joins
    /// a record's group.
    pub(crate) fn deliver_record_write(
        &self,
        workspace: WorkspaceId,
        author: [u8; 32],
        message: &neighbour_wire::NeighbourMsg,
    ) {
        let Some(link) = self.inner.direct.link(workspace) else {
            return;
        };
        if message.governance_workspace().is_some() {
            let Some(envelope) = crate::peer::records::delta_of(workspace, author, message) else {
                return;
            };
            self.inner.opened_records.deliver_to_all(
                workspace,
                |device| device != author && link.reads_of(device)(message),
                None,
                &envelope,
            );
            return;
        }
        // The workspace's own governance, as the host's link carries it: every session of
        // the workspace takes it into the replica its bootstrap installed, as a linked
        // device does.
        if let neighbour_wire::NeighbourMsg::SyncResp {
            workspace: named, ..
        } = message
        {
            if *named != workspace.to_string() {
                return;
            }
            let Some(envelope) = crate::peer::records::delta_of(workspace, author, message) else {
                return;
            };
            self.inner.opened_records.deliver_to_all(
                workspace,
                |device| device != author,
                None,
                &envelope,
            );
            return;
        }
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
        let Some(envelope) = crate::peer::records::delta_of(workspace, author, message) else {
            return;
        };
        // A record every device is replayed goes to every session of the workspace, as a link
        // to the session's device hands it: one it never opened is a record it holds all the
        // same, until it reads it.
        let own = neighbour_wire::app_document_group(app_id, document_id);
        let sync = neighbour_wire::document_sync(document_id);
        if matches!(sync, neighbour_wire::DocumentSync::Eager) {
            self.inner.opened_records.deliver_to_all(
                workspace,
                |device| device != author && link.reads_of(device)(message),
                Some(own),
                &envelope,
            );
            return;
        }
        // A part of a chained record goes to the sessions that hold it, and the part that is the
        // chain's head also to the sessions that follow the chain: one that held the part before
        // held the chain, and holds this part from now on.
        let mut channels = BTreeSet::from([own]);
        if let neighbour_wire::DocumentSync::Part { base, index } = sync
            && is_chain_head(&link, app_id, base, index)
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
            Some(own),
            &envelope,
        );
    }

    /// What a session that subscribes is sent from `workspace`: each record a device over its
    /// link is replayed — every record that is not sent when opened, and each chain's newest
    /// part — with the channels the session holds from then on. A session that opened none of
    /// them holds them all the same, as a device over its link does; a record sent when opened
    /// waits to be asked for. A chain's head is held with the chain, so the parts after it reach
    /// the session too.
    pub(crate) async fn eager_records(
        &self,
        workspace: WorkspaceId,
        device: [u8; 32],
    ) -> Vec<(Envelope, Vec<[u8; 32]>)> {
        let Some(link) = self.inner.direct.link(workspace) else {
            return Vec::new();
        };
        let records = self.inner.records.clone();
        let Ok(Ok(bodies)) = tokio::task::spawn_blocking(move || records.retained(workspace)).await
        else {
            return Vec::new();
        };
        let reads = link.reads_of(device);
        let mut replayed: Vec<(Envelope, Vec<[u8; 32]>)> = Vec::new();
        // Each chain's newest part the store keeps, by the app and base it names.
        let mut heads = Chains::new();
        for body in bodies {
            let Ok(message) = neighbour_wire::NeighbourMsg::decode(&body.body) else {
                continue;
            };
            let neighbour_wire::NeighbourMsg::AppDocument {
                app_id,
                document_id,
                ..
            } = &message
            else {
                continue;
            };
            if !reads(&message) {
                continue;
            }
            let Some(envelope) = crate::peer::records::delta_of(workspace, body.author, &message)
            else {
                continue;
            };
            let own = neighbour_wire::app_document_group(app_id, document_id);
            match neighbour_wire::document_sync(document_id) {
                neighbour_wire::DocumentSync::Eager => replayed.push((envelope, vec![own])),
                neighbour_wire::DocumentSync::Part { base, index } => {
                    let held = vec![
                        own,
                        neighbour_wire::app_document_group(
                            app_id,
                            &neighbour_wire::chain_head(base),
                        ),
                    ];
                    let newest = heads
                        .entry((app_id.clone(), base.to_owned()))
                        .or_insert_with(|| (index, held.clone(), Vec::new()));
                    if index > newest.0 {
                        newest.0 = index;
                        newest.1 = held;
                        newest.2.clear();
                    }
                    if index == newest.0 {
                        newest.2.push(envelope);
                    }
                }
                _ => {}
            }
        }
        for (_, (_, held, envelopes)) in heads {
            for envelope in envelopes {
                replayed.push((envelope, held.clone()));
            }
        }
        replayed
    }
}

/// Whether part `index` of `base` is the newest part of `app`'s chain this workspace knows: the
/// one a session that follows the chain is sent. A part the workspace has not taken in yet (a
/// host's own write) is the newest by what is known.
fn is_chain_head(
    link: &tilo_peer_state::direct::DirectSync,
    app: &str,
    base: &str,
    index: u64,
) -> bool {
    link.read(crate::documents::listing)
        .iter()
        .all(|(held, record)| {
            !matches!(
                neighbour_wire::document_sync(record),
                neighbour_wire::DocumentSync::Part { base: held_base, index: held_index }
                    if *held == app && held_base == base && held_index > index
            )
        })
}

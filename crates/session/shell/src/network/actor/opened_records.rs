//! On-demand records this device asked its rooms' hosts for
//! ([`super::NetCmd::OpenRecord`]): a room's host replays a joining device
//! only the records every device follows, and sends an on-demand record, or
//! a part of a chain behind its head (`neighbour_wire::record_sync`), once
//! it is asked.

use tilo_relay_session::RelayCoeditSession;

use super::{ActorState, WorkspaceId};

impl ActorState {
    /// Ask a host session of `ws` that just started for every on-demand
    /// record this device opened in the room ([`NetCmd::OpenRecord`]).
    pub(super) fn reopen_records(&self, ws: WorkspaceId, session: &RelayCoeditSession) {
        for channel in self.opened_records.get(&ws).into_iter().flatten() {
            if let Err(error) = session.open_record(*channel) {
                tracing::debug!(ws, %error, "the room's new host was not asked for a record");
            }
        }
    }
}

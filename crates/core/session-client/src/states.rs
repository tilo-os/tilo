//! What state each workspace this session joined is in, and whether this device's writes are
//! with its host, typed. The session publishes each change as an event
//! ([`crate::Event::Workspace`], [`crate::Event::Sync`]): whoever waits for a workspace to open or
//! for its writes to land waits for the state, and never on a timer.

use std::collections::BTreeMap;

use tilo_server_protocol::WorkspaceId;

use crate::link::{PeerStatus, TransportPhase};

pub use tilo_actions::remote::{SessionState, SyncState, WorkspaceState};

impl PeerStatus {
    /// Where `workspace` stands on this session; `None` for one it did not join.
    #[must_use]
    pub fn workspace_state(&self, workspace: WorkspaceId) -> Option<WorkspaceState> {
        let ready = *self.workspaces.get(&workspace)?;
        Some(
            if self.phase != TransportPhase::Connected || self.unlinked.contains(&workspace) {
                WorkspaceState::Connecting
            } else if !ready {
                WorkspaceState::Admitting
            } else if self.serving.contains(&workspace) {
                WorkspaceState::Serving
            } else {
                WorkspaceState::Open
            },
        )
    }

    /// Every joined workspace's state.
    #[must_use]
    pub fn workspace_states(&self) -> BTreeMap<WorkspaceId, WorkspaceState> {
        self.workspaces
            .keys()
            .filter_map(|workspace| Some((*workspace, self.workspace_state(*workspace)?)))
            .collect()
    }

    /// Where `workspace` and this device's writes stand; `None` for a workspace the session did
    /// not join.
    #[must_use]
    pub fn session_state(&self, workspace: WorkspaceId) -> Option<SessionState> {
        Some(SessionState {
            workspace: self.workspace_state(workspace)?,
            sync: self.sync_state(),
        })
    }

    /// Whether this device's writes are with its host.
    #[must_use]
    pub const fn sync_state(&self) -> SyncState {
        if self.outgoing_pending == 0 {
            SyncState::Done
        } else {
            SyncState::Behind {
                pending: self.outgoing_pending,
            }
        }
    }
}

/// The states a status is in, as [`changes`] compares them.
pub(crate) type States = (BTreeMap<WorkspaceId, WorkspaceState>, SyncState);

/// The states `status` is in.
pub(crate) fn of(status: &PeerStatus) -> States {
    (status.workspace_states(), status.sync_state())
}

/// The events that say how a status in `before` came to be `after`: one per workspace whose
/// state changed, and one when whether the writes are with the host changed.
pub(crate) fn changes(before: &States, after: &PeerStatus) -> Vec<crate::Event> {
    let (was, synced) = before;
    let mut events: Vec<crate::Event> = after
        .workspace_states()
        .into_iter()
        .filter(|(workspace, state)| was.get(workspace) != Some(state))
        .map(|(ws, state)| crate::Event::Workspace { ws, state })
        .collect();
    let syncs = after.sync_state();
    if std::mem::discriminant(synced) != std::mem::discriminant(&syncs) {
        events.push(crate::Event::Sync(syncs));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(ready: bool) -> PeerStatus {
        let mut status = PeerStatus::connecting([1; 32]);
        status.phase = TransportPhase::Connected;
        status.workspaces.insert(WorkspaceId::from_u128(7), ready);
        status
    }

    #[test]
    fn a_workspace_is_told_as_it_moves_from_admitting_to_serving() {
        let ws = WorkspaceId::from_u128(7);
        let mut connecting = joined(false);
        connecting.phase = TransportPhase::Reconnecting;
        assert_eq!(
            connecting.workspace_state(ws),
            Some(WorkspaceState::Connecting)
        );
        let admitting = joined(false);
        assert_eq!(
            admitting.workspace_state(ws),
            Some(WorkspaceState::Admitting)
        );
        let open = joined(true);
        let told = changes(&of(&admitting), &open);
        assert!(
            matches!(
                told.as_slice(),
                [crate::Event::Workspace {
                    state: WorkspaceState::Open,
                    ..
                }]
            ),
            "{told:?}"
        );
        // Its own link ended while the session's others stay up: it waits for a connection.
        let mut unlinked = joined(false);
        unlinked.unlinked.insert(ws);
        assert_eq!(
            unlinked.workspace_state(ws),
            Some(WorkspaceState::Connecting)
        );
        let mut serving = joined(true);
        serving.serving.insert(ws);
        assert_eq!(serving.workspace_state(ws), Some(WorkspaceState::Serving));
        // Nothing moved, nothing is told.
        assert!(changes(&of(&serving), &serving).is_empty());
        assert_eq!(
            joined(true).workspace_state(WorkspaceId::from_u128(8)),
            None,
            "a workspace the session did not join has no state"
        );
    }

    #[test]
    fn sync_is_told_when_it_falls_behind_and_when_it_is_done() {
        let done = joined(true);
        let mut behind = joined(true);
        behind.outgoing_pending = 2;
        assert!(matches!(
            changes(&of(&done), &behind).as_slice(),
            [crate::Event::Sync(SyncState::Behind { pending: 2 })]
        ));
        let mut more = behind.clone();
        more.outgoing_pending = 3;
        assert!(
            changes(&of(&behind), &more).is_empty(),
            "behind by more is still behind"
        );
        assert!(matches!(
            changes(&of(&more), &done).as_slice(),
            [crate::Event::Sync(SyncState::Done)]
        ));
    }
}

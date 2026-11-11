//! A hosted browser environment's device: one workspace, one record.
//!
//! A workspace hosted on a relay that sells machines opens Browser in a
//! browser environment: a short-lived VM whose headless session joins the
//! workspace, as a device the relay certified for it
//! ([`docs/hosted-browser.md`](../../../../docs/hosted-browser.md)). Web pages
//! are strangers' code and Chromium runs a JIT on it, so a page that escapes
//! the engine holds that device's key: everything the device may reach is
//! everything such a page may read. The relay therefore admits it as a device
//! of one workspace with one record — its Browser record — whose channel is
//! the only record channel it reaches, and whose window's page is the only
//! track it publishes.
//!
//! This register is the environment's half of the rule that confines an agent
//! to its delegation ([`crate::peer::AgentRegistry`]): the devices this relay
//! certified for its environments, by device key, with the one record each
//! reaches. It is the relay's, and nothing here is asked of the session in the
//! VM: [`crate::workspace_access::decide`] answers every plane with it, the
//! state plane's replay, its live fan-out, its record opening and its changes
//! ask it what a record is, and the live plane offers such a device no one
//! else's tracks. A device that asked to be one is confined whether or not its
//! session cooperates.
//!
//! What it is given besides its record is the workspace's own document — the
//! layout every device needs to join at all — and its own device's presence on
//! the live plane: never one record of the workspace's, its channels' keys, a
//! call, or another window's page.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use tilo_lock::RwLockRecover as _;

use crate::workspace_access::WorkspaceOperation;

/// The record's channel on the state plane: [`neighbour_wire::app_document_channel`]
/// for the record's app and id. The state plane names a record by this alone;
/// the workspace a record belongs to is the scope's
/// [`workspace`](Self::workspace).
#[must_use]
pub fn record_channel(app: &str, record: &str) -> [u8; 32] {
    neighbour_wire::app_document_channel(app, record)
}

/// What one environment's device reaches: its one workspace, and the channel
/// of its one record there. A scope that reaches nothing has neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnvironmentScope {
    /// The workspace it was placed in, by this relay's workspace id.
    pub workspace: Option<u64>,
    /// The channel of its Browser record ([`record_channel`]).
    pub channel: Option<[u8; 32]>,
}

impl EnvironmentScope {
    /// The scope of an environment placed in `workspace` for `record` of `app`.
    #[must_use]
    pub fn for_record(workspace: u64, app: &str, record: &str) -> Self {
        Self {
            workspace: Some(workspace),
            channel: Some(record_channel(app, record)),
        }
    }

    /// A scope that reaches nothing, for a register that cannot be read: a
    /// device whose confinement cannot be looked up is admitted nowhere rather
    /// than as the member it is certified as.
    const fn nothing() -> Self {
        Self {
            workspace: None,
            channel: None,
        }
    }

    /// Whether the device this scope confines is admitted to `workspace` for
    /// `op`: the planes a window's session works on, in its one workspace, and
    /// no other plane anywhere.
    ///
    /// Subscription, state, replay and publishing are what joining, syncing
    /// and writing its record need; the live plane carries its window's page;
    /// a remote action is how the window answers the page action the relay
    /// handed it; and a channel is how it asks its record's keys for access.
    /// Every other plane — compute, an environment, content discovery,
    /// membership — is not its to ask.
    #[must_use]
    pub const fn admits(self, workspace: u64, op: WorkspaceOperation) -> bool {
        matches!(self.workspace, Some(own) if own == workspace)
            && matches!(
                op,
                WorkspaceOperation::Subscribe
                    | WorkspaceOperation::State
                    | WorkspaceOperation::Publish
                    | WorkspaceOperation::Replay
                    | WorkspaceOperation::Live
                    | WorkspaceOperation::RemoteAction
                    | WorkspaceOperation::ChannelAccess
            )
    }

    /// Whether `channel` — a record's channel, or `None` for a frame that
    /// belongs to no record — is inside this scope. A frame of no record is
    /// the workspace's own document, which a session needs to join at all.
    #[must_use]
    pub fn reaches(self, channel: Option<[u8; 32]>) -> bool {
        match (self.channel, channel) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(own), Some(channel)) => own == channel,
        }
    }
}

/// The environment devices this relay certified, by device key
/// ([`EnvironmentScope`]).
pub type EnvironmentRegistry = Arc<RwLock<HashMap<[u8; 32], EnvironmentScope>>>;

/// A register that confines nobody: what a plane no browser environment
/// reaches asks its [`crate::workspace_access::RelayTrust`] with, so that a
/// decision it takes about a device of another kind asks the same rule as
/// every other.
#[must_use]
pub fn none() -> &'static EnvironmentRegistry {
    static NONE: std::sync::OnceLock<EnvironmentRegistry> = std::sync::OnceLock::new();
    NONE.get_or_init(EnvironmentRegistry::default)
}

/// What `device` reaches, when it is one of this relay's environment devices.
/// A register that cannot be read confines every device to nothing.
#[must_use]
pub fn scope_of(environments: &EnvironmentRegistry, device: &[u8; 32]) -> Option<EnvironmentScope> {
    match environments.read() {
        Ok(scopes) => scopes.get(device).copied(),
        Err(poisoned) => {
            let _ = poisoned;
            Some(EnvironmentScope::nothing())
        }
    }
}

/// Confine `device` to `scope`, or forget it when `scope` is `None`: what the
/// relay does when it places an environment and when it stops.
pub fn confine(
    environments: &EnvironmentRegistry,
    device: [u8; 32],
    scope: Option<EnvironmentScope>,
) {
    let mut scopes = environments.write_recover();
    match scope {
        Some(scope) => {
            scopes.insert(device, scope);
        }
        None => {
            scopes.remove(&device);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: u64 = 7;

    fn scope() -> EnvironmentScope {
        EnvironmentScope::for_record(WORKSPACE, "so.tilo.browser", "record-1")
    }

    #[test]
    fn a_record_has_one_channel_on_the_state_plane() {
        assert_eq!(
            scope().channel,
            Some(neighbour_wire::app_document_channel(
                "so.tilo.browser",
                "record-1"
            ))
        );
        assert_ne!(
            scope().channel,
            EnvironmentScope::for_record(WORKSPACE, "so.tilo.browser", "record-2").channel,
            "another record is another channel"
        );
    }

    #[test]
    fn an_environment_reaches_its_workspace_and_no_other() {
        assert!(scope().admits(WORKSPACE, WorkspaceOperation::State));
        assert!(!scope().admits(WORKSPACE + 1, WorkspaceOperation::State));
    }

    #[test]
    fn an_environment_works_on_a_windows_planes_and_no_other() {
        for op in [
            WorkspaceOperation::Subscribe,
            WorkspaceOperation::State,
            WorkspaceOperation::Publish,
            WorkspaceOperation::Replay,
            WorkspaceOperation::Live,
            WorkspaceOperation::RemoteAction,
            WorkspaceOperation::ChannelAccess,
        ] {
            assert!(scope().admits(WORKSPACE, op), "{op:?}");
        }
        for op in [
            WorkspaceOperation::Compute,
            WorkspaceOperation::Environment,
            WorkspaceOperation::ContentDiscovery,
        ] {
            assert!(!scope().admits(WORKSPACE, op), "{op:?}");
        }
    }

    #[test]
    fn an_environment_reaches_its_record_and_no_other() {
        let scope = scope();
        let own = scope.channel.expect("its own record");
        assert!(scope.reaches(Some(own)), "its own record");
        assert!(scope.reaches(None), "the workspace's own document");
        assert!(
            !scope.reaches(Some(record_channel("so.tilo.notes", "notes-1"))),
            "a customer's document"
        );
        assert!(
            !scope.reaches(Some(record_channel("so.tilo.chat", "chat-1"))),
            "a customer's chat"
        );
    }

    #[test]
    fn a_register_that_cannot_be_read_confines_a_device_to_nothing() {
        let environments = EnvironmentRegistry::default();
        confine(&environments, [1; 32], Some(scope()));
        assert_eq!(scope_of(&environments, &[1; 32]), Some(scope()));
        assert_eq!(scope_of(&environments, &[2; 32]), None, "no environment");
        let poisoned = EnvironmentRegistry::default();
        let _ = std::thread::scope(|thread| {
            thread
                .spawn(|| {
                    let _guard = poisoned.write().unwrap();
                    panic!("a writer panicked");
                })
                .join()
        });
        let found = scope_of(&poisoned, &[1; 32]).expect("a poisoned register is no member");
        assert!(!found.admits(WORKSPACE, WorkspaceOperation::State));
        assert!(!found.reaches(None));
    }

    #[test]
    fn forgetting_an_environment_forgets_its_device() {
        let environments = EnvironmentRegistry::default();
        confine(&environments, [1; 32], Some(scope()));
        confine(&environments, [1; 32], None);
        assert_eq!(scope_of(&environments, &[1; 32]), None);
    }
}

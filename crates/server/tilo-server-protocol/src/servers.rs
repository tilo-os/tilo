//! The person's workspace, and how to find it from their root alone.
//!
//! It is an ordinary workspace whose only member is the person, and each of
//! their devices takes part in it. It holds what is the person's alone: their
//! servers (the `so.tilo.servers` resource), and with them their devices, the
//! list of their workspaces and the settings that follow them. Its id follows
//! from their root, so every one of their devices finds the same one without
//! being told ([`person_workspace_id`]).

/// One of the person's servers, as the document of their workspace names
/// it: what their other devices list, and where they reach it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PersonServer {
    /// Its device key, lowercase hex.
    pub device: String,
    /// The name people see for it.
    pub label: String,
    /// Where its service is dialled, as it last advertised it; `None` until it
    /// did.
    pub endpoint: Option<iroh_base::EndpointAddr>,
}

/// The disk recovery key of one of the person's machines, as the vault of their workspace keeps
/// it: what opens the machine's disk with nothing else. The machine keeps no copy.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecoveryKey {
    /// The machine's device key, lowercase hex.
    pub machine: String,
    /// The name the machine was certified under.
    pub label: String,
    /// The recovery key.
    pub key: String,
}

impl std::fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryKey")
            .field("machine", &self.machine)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// The id of the workspace of the person whose root is `person`, which the workspace policy
/// derives so that every process that decides a record's changes finds the workspace holding it.
pub use tilo_action_policy::roles::person_workspace_id;

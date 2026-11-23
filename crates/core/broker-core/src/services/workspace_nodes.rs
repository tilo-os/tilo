//! The workspace's answer about one node of its tree, as the host reaches it.

/// What the workspace answered: its output, or why it refused.
pub type WorkspaceNodeOutcome = Result<serde_json::Value, String>;

/// One ask of a node of a workspace's published tree, made by the person at
/// this device through one of their apps (Chat sharing a window): share it
/// with the workspace's members, end a share of it, or ask what they may do
/// there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceNodeAsk {
    /// Grant the workspace's members `access` (`view` or `edit`) on the node, as
    /// share `share`.
    Share { share: String, access: String },
    /// Take back what share `share` granted.
    Unshare { share: String },
    /// Whether the person may read and change the node.
    Permits,
}

/// Asking the workspace a node is in, as the person at this device. The workspace,
/// not this device, decides each ask, as it decides every read and write
/// a share's grant then covers: a host governing it answers through its
/// member actions. The broker answers it under the workspace's provider,
/// `node:<node>` resources ([`tilo_actions::remote::node_target`]), only to
/// the person at this device in that workspace.
pub trait WorkspaceNodeActions: collab_types::signer::MaybeSendSync {
    /// Ask workspace `workspace` `ask` of `node` (its text form), for the person at
    /// this device. The workspace's answer arrives on the returned answer.
    fn ask(
        &self,
        workspace: &str,
        node: &str,
        ask: WorkspaceNodeAsk,
    ) -> Result<tilo_actions::remote::Pending<WorkspaceNodeOutcome>, String>;
}

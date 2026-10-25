//! The room's answer about one node of its tree, as the host reaches it.

/// What the room answered: its output, or why it refused.
pub type RoomNodeOutcome = Result<serde_json::Value, String>;

/// One ask of a node of a room's published tree, made by the person at
/// this device through one of their apps (Chat sharing a window): share it
/// with the room's members, end a share of it, or ask what they may do
/// there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomNodeAsk {
    /// Grant the room's members `access` (`view` or `edit`) on the node, as
    /// share `share`.
    Share { share: String, access: String },
    /// Take back what share `share` granted.
    Unshare { share: String },
    /// Whether the person may read and change the node.
    Permits,
}

/// Asking the room a node is in, as the person at this device. The room,
/// not this device, decides each ask, as it decides every read and write
/// a share's grant then covers: a host governing it answers through its
/// member actions. The broker answers it under the workspace's provider,
/// `node:<node>` resources ([`tilo_actions::remote::node_target`]), only to
/// the person at this device in that room.
pub trait RoomNodeActions: Send + Sync {
    /// Ask room `room` `ask` of `node` (its text form), for the person at
    /// this device. The room's answer arrives on the returned answer.
    ///
    /// # Errors
    /// Why the ask cannot reach a room that decides it.
    fn ask(
        &self,
        room: &str,
        node: &str,
        ask: RoomNodeAsk,
    ) -> Result<tilo_actions::remote::Pending<RoomNodeOutcome>, String>;
}

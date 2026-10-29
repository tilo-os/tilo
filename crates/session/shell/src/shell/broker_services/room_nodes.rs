//! An app's ask of a node of its workspace's tree (Chat sharing a window
//! live), answered by this device: a share is grants its person gives,
//! written into its replica of the room, and what they may do at a node is
//! the room's policy ([`crate::shell::network::ShellNetwork::ask_room_node`]).
//! The browser desktop asks the room's host through the page's `/rooms`
//! requests instead. Either way the room's grants then decide every read
//! and write the share covers.

use std::sync::Arc;

use tilo_action_broker::services::{RoomNodeActions, RoomNodeAsk, RoomNodeOutcome};

/// A room's nodes over this device's network.
pub(super) struct NativeRoomNodes(pub(super) Arc<super::super::network::ShellNetwork>);

impl NativeRoomNodes {
    /// The service over `network`; none without a network.
    pub(super) fn service(
        network: Option<&Arc<super::super::network::ShellNetwork>>,
    ) -> Option<Arc<dyn RoomNodeActions>> {
        network.map(|network| Arc::new(Self(Arc::clone(network))) as Arc<dyn RoomNodeActions>)
    }
}

impl RoomNodeActions for NativeRoomNodes {
    fn ask(
        &self,
        room: &str,
        node: &str,
        ask: RoomNodeAsk,
    ) -> Result<tilo_actions::remote::Pending<RoomNodeOutcome>, String> {
        let room = crate::shell::state::RoomId::parse(room).ok_or("that workspace isn't a room")?;
        self.0.ask_room_node(room, node, ask)
    }
}

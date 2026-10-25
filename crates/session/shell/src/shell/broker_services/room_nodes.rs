//! An app's ask of a node of its workspace's tree (Chat sharing a window
//! live), carried to the room as the room's own member actions.
//!
//! The browser desktop asks a room's host through the page's `/rooms`
//! requests; this desktop asks the same host through its relay session, as
//! `resource.share`, `resource.unshare` and `resource.permits`. Either way
//! the room decides, and its grants then decide every read and write the
//! share covers.

use std::sync::Arc;

use tilo_action_broker::services::{RoomNodeActions, RoomNodeAsk, RoomNodeOutcome};
use tilo_relay_session::rooms::{RESOURCE_PERMITS, SHARE_RESOURCE, UNSHARE_RESOURCE};

/// The room's member actions over this device's network.
pub(super) struct NativeRoomNodes(pub(super) Arc<super::super::network::ShellNetwork>);

impl NativeRoomNodes {
    /// The service over `network`; none without a network.
    pub(super) fn service(
        network: Option<&Arc<super::super::network::ShellNetwork>>,
    ) -> Option<Arc<dyn RoomNodeActions>> {
        network.map(|network| Arc::new(Self(Arc::clone(network))) as Arc<dyn RoomNodeActions>)
    }
}

/// The member action and arguments that ask `ask` of `node`.
fn member_action(
    node: &str,
    ask: RoomNodeAsk,
) -> (&'static str, serde_json::Map<String, serde_json::Value>) {
    let mut arguments = serde_json::Map::new();
    let action = match ask {
        RoomNodeAsk::Share { share, access } => {
            arguments.insert("share".into(), share.into());
            arguments.insert("node".into(), node.into());
            arguments.insert("access".into(), access.into());
            SHARE_RESOURCE
        }
        RoomNodeAsk::Unshare { share } => {
            arguments.insert("share".into(), share.into());
            UNSHARE_RESOURCE
        }
        RoomNodeAsk::Permits => {
            arguments.insert("node".into(), node.into());
            RESOURCE_PERMITS
        }
    };
    (action, arguments)
}

impl RoomNodeActions for NativeRoomNodes {
    fn ask(
        &self,
        room: &str,
        node: &str,
        ask: RoomNodeAsk,
    ) -> Result<tilo_actions::remote::Pending<RoomNodeOutcome>, String> {
        let room = crate::shell::state::RoomId::parse(room).ok_or("that workspace isn't a room")?;
        let (action, arguments) = member_action(node, ask);
        self.0.ask_member_at_host(room, action, arguments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_ask_travels_as_the_rooms_member_action() {
        let node = "room:r/app:so.tilo.notes/kind:notes.notebook/resource:*/item:p1";
        let share = RoomNodeAsk::Share {
            share: "m1".into(),
            access: "view".into(),
        };
        let (action, arguments) = member_action(node, share);
        assert_eq!(action, SHARE_RESOURCE);
        assert_eq!(
            serde_json::Value::Object(arguments),
            serde_json::json!({ "share": "m1", "node": node, "access": "view" })
        );
        let (action, arguments) = member_action(node, RoomNodeAsk::Unshare { share: "m1".into() });
        assert_eq!(action, UNSHARE_RESOURCE);
        assert_eq!(
            serde_json::Value::Object(arguments),
            serde_json::json!({ "share": "m1" })
        );
        let (action, arguments) = member_action(node, RoomNodeAsk::Permits);
        assert_eq!(action, RESOURCE_PERMITS);
        assert_eq!(
            serde_json::Value::Object(arguments),
            serde_json::json!({ "node": node })
        );
    }
}

//! The gate every message from another member passes: the workspace it
//! arrived in decides, by its own policy, what the member may say there.
//!
//! The table of what each message is lives in [`crate::workspace_gate`]; here
//! is where the question is asked — with the sender's person, the desk the
//! session shows, and the workspace's policy — and what a refusal says.

use std::time::SystemTime;

use iroh::EndpointId;
use tilo_desktop_model::RoomId as DeskRoom;
use tilo_desktop_surface::DesktopHandle;
use tilo_network_actor::NetEvent;

use crate::workspace_gate::{self, WorkspaceAction};

use super::Client;

impl Client {
    /// Read the policies of the workspaces this session takes part in from
    /// `desk`: the desk it shows, whose workspaces' policies every message
    /// from another member is held to ([`Client::hear`]).
    pub fn attach_desk(&self, desk: DesktopHandle) {
        *self.desk.borrow_mut() = Some(desk);
    }

    /// Why `event`'s message is refused by the workspace it arrived in:
    /// `None` when it is admitted, and when nothing here decides — a plane
    /// that takes part as nobody, or a message that is no workspace's
    /// traffic ([`workspace_gate::required_workspace_action`]).
    pub(super) fn refusal(&self, event: &NetEvent) -> Option<String> {
        let NetEvent::Msg { ws, peer, msg, .. } = event else {
            return None;
        };
        // A session whose device no person certified is a member of no
        // workspace's governance: a stand-in plane has no policy to be held
        // to.
        self.user?;
        let required: WorkspaceAction = workspace_gate::required_workspace_action(msg)?;
        let (room, known) = {
            let plane = self.plane.borrow();
            if *peer == plane.publisher {
                return None;
            }
            let room = plane.rooms.iter().find(|room| room.ws == *ws)?;
            let known = room
                .peers
                .iter()
                .find(|known| known.publisher == *peer)
                .and_then(|known| known.user.clone());
            (room.id.clone(), known)
        };
        // Who the device is: the certificate it said here, or, for a member
        // that announces none — a browser of the workspace — the identity the
        // workspace's governance admitted its device as.
        let Some(person) = known.or_else(|| self.governed_person(&room, *peer)) else {
            return Some("the device is not known here".to_owned());
        };
        let desk = self.desk.borrow().clone()?;
        let now = tilo_clock::unix_now();
        let allowed =
            desk.read(|tree| workspace_gate::allows(tree, &room, &person, &required, now));
        if allowed {
            return None;
        }
        Some(format!(
            "the workspace's policy does not let {person} do {required:?}"
        ))
    }

    /// The person the workspace `room`'s governance admitted `device` as,
    /// when the device announced no certificate of its own here.
    fn governed_person(&self, room: &str, device: EndpointId) -> Option<String> {
        let engine = self.engine.as_ref()?;
        let room = DeskRoom::parse(room)?;
        engine
            .owner()
            .with_current(room, |view| {
                view.peer()
                    .permissions(*device.as_bytes(), SystemTime::now())
                    .map(|member| member.person)
                    .map_err(|error| error.to_string())
            })
            .ok()
    }
}

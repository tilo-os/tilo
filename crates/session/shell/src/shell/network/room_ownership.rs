//! Taking up a room another person offers this device's person: its
//! acceptance is signed by this device's own host, which governs the room
//! once the room's governor hands it on, and is asked of that governor.

use tilo_desktop_model::OWNERSHIP_ACCEPT_ACTION_ID;

use super::{RoomId, ShellNetwork};

impl ShellNetwork {
    /// Accept the open offer of `room` whose id is `offer` (hex), and wait
    /// for its governor's answer.
    ///
    /// # Errors
    /// The offer is no longer open, this device's host cannot sign for it,
    /// or the governor refused it.
    pub(super) fn accept_ownership(&self, room: RoomId, offer: &str) -> Result<(), String> {
        let open = self
            .native_owner()?
            .with_current(room, |view| Ok(view.peer().governance().ownership_offer()))?
            .filter(|open| hex::encode(open.offer_id) == offer)
            .ok_or("the offer was taken back or replaced")?;
        let acceptance = self
            .local_host()?
            .rooms()
            .acceptance(&open)
            .map_err(|error| error.to_string())?;
        let held = self
            .held_room(room)
            .ok_or("this device holds no session with the workspace host")?;
        let mut arguments = serde_json::Map::new();
        arguments.insert(
            "acceptance".into(),
            hex::encode(postcard::to_stdvec(&acceptance).map_err(|error| error.to_string())?)
                .into(),
        );
        self.govern(&held, OWNERSHIP_ACCEPT_ACTION_ID, arguments)
            .map(drop)
    }
}

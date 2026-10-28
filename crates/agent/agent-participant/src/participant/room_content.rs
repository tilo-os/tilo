//! The room's content, answered to a member that asks for it.
//!
//! What an agent's records import or make (a source a video project
//! places, a still it drew) is kept in the room's content store on the
//! agent's host, and a record names it by address alone. A member whose
//! record references such content and who lacks it asks the room on its
//! presence ([`PeerEvent::ContentWanted`](tilo_browser_peer::PeerEvent)),
//! as every device asks; the agent answers from the room's store, to that
//! member alone, in the pieces any member answers with. So a person's
//! editor, or an agent on another host, reads what this agent put in the
//! room, as the room lets it read the record.

use tilo_content::ContentStore;

use super::AgentParticipant;

impl AgentParticipant {
    /// Answer `request`, the room's member `from` asking for content, from
    /// the room's store.
    pub(super) fn content_wanted(&self, from: [u8; 32], request: &[u8]) {
        let pieces = answer(&self.device(), &from, request, &self.room, &self.content);
        for piece in pieces {
            if self
                .peer
                .send_content(self.workspace_id, from, piece)
                .is_err()
            {
                // Disconnected: the member asks again.
                return;
            }
        }
    }
}

/// The frames that answer `request` from device `asker`, sent by `device`:
/// the pieces of the content `room`'s store keeps, or word that it keeps
/// none, which content of another room always is.
pub(super) fn answer(
    device: &[u8; 32],
    asker: &[u8; 32],
    request: &[u8],
    room: &str,
    content: &ContentStore,
) -> Vec<Vec<u8>> {
    tilo_browser_peer::content::answer(device, asker, request, |_app, cluster, hash| {
        cluster
            .eq_ignore_ascii_case(room)
            .then(|| content.read(hash))
            .flatten()
    })
}

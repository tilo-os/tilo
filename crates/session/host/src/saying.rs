//! This session's presence tracks, as a participant's client says on them: the workspace's live
//! plane, which every device of the workspace says its word on.

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId;
use tilo_session_client::{SayPresence, topic_of};

use crate::session::PeerHandle;

/// Where a client's presence frames go on `peer`'s session: the live plane of the workspace each
/// frame names, or nothing where this session holds none — presence is ephemeral, and the next
/// change says where its sender is.
#[must_use]
pub fn saying(peer: &PeerHandle) -> Arc<dyn SayPresence> {
    Arc::new(Saying { peer: peer.clone() })
}

struct Saying {
    peer: PeerHandle,
}

impl std::fmt::Debug for Saying {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("live plane")
    }
}

impl SayPresence for Saying {
    fn say(
        &self,
        workspace: WorkspaceId,
        sequence: u32,
        frame: &collab_types::presence::Presence,
    ) -> Result<(), String> {
        // The plane numbers the objects it carries itself; `sequence` is the board's count.
        let _ = sequence;
        self.peer
            .publish_presence(topic_of(workspace), vec![frame.clone()])
    }
}

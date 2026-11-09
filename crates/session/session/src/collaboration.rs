//! The collaboration a workspace brings: what its members put on the desk, the calls they are in,
//! the invitations they redeemed, and whose view this device follows.

use crate::Session;

impl Session {
    /// Keep the workspaces this session takes part in current with the desktop's, and hand the
    /// desktop who is here.
    pub(crate) fn follow_collaboration(&self) {
        let Some(sync) = &self.shared.sync else {
            return;
        };
        self.shared.handle.read(|tree| sync.follow_desktop(tree));
        sync.follow();
        // A workspace's strips and windows are the workspace's: what its
        // members put there — before this device joined, and since — is this
        // device's desk too, and what this device arranges there is theirs to
        // see.
        self.shared.follow_shared_desktops();
        // This device's call, as everyone in the workspace reads it, and
        // theirs as this session's apps show them: a join or leave reaches a
        // channel's roster on the pass it happened on.
        if let Some(plane) = &self.action_plane {
            plane.follow_calls(sync);
        }
        // Whose view this device follows: their window here comes forward each time the place they
        // work in moves, and their app where no window here shows it.
        self.shared.follow_people();
        // What became of the invitations redeemed is the directory's to say, and the desk's to
        // show.
        self.shared.follow_enrollments();
    }
}

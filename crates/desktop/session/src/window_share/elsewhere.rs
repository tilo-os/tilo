//! A window that runs on another device, while nothing of it is shown here.
//!
//! The workspace's layout lists such a window on every shell as its device's
//! ([`tilo_desktop_model::TileContent::HostedWindow`]), shared or not, and a shell that holds none
//! of its pictures has nothing of the window to draw. Why it is not here is the host's session's
//! to read ([`Elsewhere`]): the shell shows what is read here and decides none of it.

/// Why a window that runs on another device is not shown in its tab, as the host's session reads
/// it. The shell decides none of it: a session that reads its workspaces says which holds, and a
/// shell nobody told says only where the window runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Elsewhere {
    /// Nobody said how the window stands here: the tab says where it runs, and no more.
    #[default]
    Unread,
    /// Its device is in the workspace and shares no such window with it.
    NotShared,
    /// The workspace says its device shares it, and nothing of it has arrived here yet.
    Awaited {
        /// What it waits for, as the session's own reading of the share says it; empty while it
        /// waits for the device alone.
        waiting_for: String,
    },
    /// This device hears nothing of its device in the workspace.
    Away,
    /// The workspace no longer admits its device.
    Left,
}

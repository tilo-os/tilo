//! An agent's presence in a workspace: who it shows itself as and where it
//! acts, said on the workspace's presence track
//! ([`collab_types::presence::Presence`]) the way every device says its own,
//! so every desktop draws it beside the people.

use collab_types::PeerPresencePayload;
use collab_types::presence::{DESKTOP_APP, Presence, ShellPresence, ShellSelection, ShellWindow};
use tilo_browser_peer::PeerHandle;
use tilo_relay_protocol::live::{LiveAnchor, LiveRecord};

/// The view an agent states: its face, what it is doing, and the record it
/// acts in with its caret where its last operation left it.
#[must_use]
pub fn shell(
    seq: u32,
    sent_at_ms: u64,
    label: &str,
    colour: [u8; 3],
    activity: Option<String>,
    focus: Option<&LiveRecord>,
    selection: Option<&(LiveAnchor, LiveAnchor)>,
) -> ShellPresence {
    ShellPresence {
        seq,
        sent_at_ms,
        label: label.to_owned(),
        agent: true,
        colour: format!("#{:02x}{:02x}{:02x}", colour[0], colour[1], colour[2]),
        activity,
        place: None,
        size: [0, 0],
        // A receiver draws the agent's face in the window it names; its rect
        // is not the agent's screen, which it does not have.
        windows: focus.map(window).into_iter().collect(),
        selection: selection.map(|(anchor, head)| ShellSelection {
            anchor: hex::encode(&anchor.cursor),
            head: hex::encode(&head.cursor),
        }),
        chat: None,
        reaction: None,
    }
}

/// The window showing `focus`, by the record's short digest: what a
/// receiver matches against its own windows.
fn window(focus: &LiveRecord) -> ShellWindow {
    ShellWindow {
        app: focus.app.clone(),
        record: focus.short_digest(),
        rect: [0, 0, 10_000, 10_000],
        focused: true,
    }
}

/// Say `view` in `workspace` on `peer`'s session, as the device that session
/// is: one presence frame on the workspace's plane, the way every device
/// says its own.
pub fn say(peer: &PeerHandle, workspace: &str, workspace_id: u64, view: ShellPresence) {
    let frame = Presence::desktop(workspace, PeerPresencePayload::Shell(Box::new(view)));
    if let Err(error) = peer.publish_presence(workspace_id, vec![frame]) {
        tracing::warn!(%error, "agent: its presence did not reach the workspace");
    }
}

/// Another member's view of itself, when `body` is the shell's presence of
/// `workspace`.
#[must_use]
pub fn heard(workspace: &str, body: &[u8]) -> Option<ShellPresence> {
    let frame = Presence::decode(body)?;
    if frame.app != DESKTOP_APP || frame.cluster != workspace {
        return None;
    }
    match frame.payload {
        PeerPresencePayload::Shell(shell) => Some(*shell),
        _ => None,
    }
}

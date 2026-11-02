//! An agent's presence in a workspace: who it shows itself as and where it
//! acts, said on the workspace's presence
//! ([`neighbour_wire::NeighbourMsg::AppPresenceV2`]) the way every device
//! says its own, so every desktop draws it beside the people.

use collab_types::PeerPresencePayload;
use collab_types::presence::{ShellPresence, ShellSelection, ShellWindow};
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

/// Say `view` in `workspace` on `peer`'s session, as the device `device`: one
/// presence frame, the way every device says its own.
pub fn say(
    peer: &PeerHandle,
    device: &[u8; 32],
    workspace: &str,
    workspace_id: u64,
    view: ShellPresence,
) {
    let message = neighbour_wire::desktop_presence(
        neighbour_wire::peer_handle(device),
        workspace,
        neighbour_wire::workspace_key(workspace),
        PeerPresencePayload::Shell(Box::new(view)),
    );
    let body = match postcard::to_stdvec(&message) {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(%error, "agent: presence out of bounds");
            return;
        }
    };
    if let Err(error) = peer.publish_presence(workspace_id, vec![body]) {
        tracing::warn!(%error, "agent: its presence did not reach the workspace");
    }
}

/// Another member's view of itself, when `body` (a postcard
/// `NeighbourMsg` the host stamped with its device) is the shell's presence
/// of `workspace`.
#[must_use]
pub fn heard(workspace: &str, body: &[u8]) -> Option<ShellPresence> {
    let message: neighbour_wire::NeighbourMsg = postcard::from_bytes(body).ok()?;
    let neighbour_wire::NeighbourMsg::AppPresenceV2 {
        app_id,
        cluster_id,
        presence,
        ..
    } = message
    else {
        return None;
    };
    if app_id != neighbour_wire::DESKTOP_PRESENCE_APP || cluster_id != workspace {
        return None;
    }
    match presence.payload {
        PeerPresencePayload::Shell(shell) => Some(*shell),
        _ => None,
    }
}

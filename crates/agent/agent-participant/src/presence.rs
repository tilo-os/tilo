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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    const WORKSPACE: &str = "6b3f9a10-0000-4000-8000-00000000000c";

    fn view(label: &str) -> ShellPresence {
        ShellPresence {
            seq: 1,
            sent_at_ms: 1,
            label: label.to_owned(),
            agent: true,
            colour: "#7c3aed".into(),
            activity: Some("New note".into()),
            place: None,
            size: [1440, 900],
            windows: Vec::new(),
            selection: None,
            chat: None,
            reaction: None,
        }
    }

    /// The view an agent says of itself is a desktop presence frame, and the
    /// frame a member hears is that view: the agent speaks the same presence
    /// a person's devices do.
    #[test]
    fn an_agents_view_reads_back_from_its_frame() {
        let said = view("Writer helper");
        let frame = Presence::desktop(
            WORKSPACE,
            PeerPresencePayload::Shell(Box::new(said.clone())),
        );
        let body = frame.encode().expect("a presence frame encodes");
        assert_eq!(heard(WORKSPACE, &body), Some(said));
        assert_eq!(heard("07848ffe-1898-4f72-8419-0b54c93e6dad", &body), None);
    }

    /// A person's pointer on a workspace is not a view of a desk: the agent
    /// reads no shell out of it.
    #[test]
    fn a_pointer_is_not_a_view_of_a_desk() {
        let body = Presence::pointing(WORKSPACE, 3, 1.0, 2.0)
            .encode()
            .expect("a presence frame encodes");
        assert_eq!(heard(WORKSPACE, &body), None);
    }
}

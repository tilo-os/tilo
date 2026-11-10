//! An agent's presence in a workspace: who it shows itself as and where it acts, said on the
//! workspace's presence track by the board every host keeps ([`tilo_live::presence::Board`]), so
//! every desktop draws it beside the people.

use collab_types::presence::{Presence, ShellPresence};
use tilo_live::presence::{Board, Own};
use tilo_session_host::PeerHandle;
use tilo_user_identity::DeviceCert;

/// The word a device certified to `label` says of itself, with `view` for the record it acts in.
#[must_use]
pub fn own(cert: &DeviceCert, label: &str, view: ShellPresence) -> Own {
    Own {
        label: label.to_owned(),
        device: cert.device_label.clone(),
        user: Some(hex::encode(cert.user_public_key)),
        shell: Some(Box::new(view)),
        ..Own::default()
    }
}

/// Say `own` on `board`, and what the board owes on `peer`'s session.
pub fn say(board: &mut Board, peer: &PeerHandle, workspace_id: u64, own: &Own) {
    board.say(own);
    due(board, peer, workspace_id);
}

/// Say the frames `board` owes now on `peer`'s session: a change since the last word, one lost,
/// or the restatement a member who came in later needs.
pub fn due(board: &mut Board, peer: &PeerHandle, workspace_id: u64) {
    #[expect(
        clippy::cast_precision_loss,
        reason = "milliseconds since the epoch are exact in an f64 for the life of a session"
    )]
    let now = tilo_clock::unix_now_ms() as f64;
    let Some(frames) = board.due(now) else {
        return;
    };
    let said: Vec<Presence> = frames.into_iter().map(|(_, frame)| frame).collect();
    if let Err(error) = peer.publish_presence(workspace_id, said) {
        tracing::warn!(%error, "agent: its presence did not reach the workspace");
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
    use tilo_live::presence::{Board, Own};
    use tilo_relay_protocol::live::LiveRecord;

    const WORKSPACE: &str = "6b3f9a10-0000-4000-8000-00000000000c";

    /// The agent's own word, heard by the person's board: the same presence a person's devices
    /// speak, from a device that says it is an agent.
    #[test]
    fn an_agents_word_is_heard_as_an_agents_view_of_itself() {
        let agent = Board::new(WORKSPACE, [7_u8; 32]);
        let mut person = Board::new(WORKSPACE, [9_u8; 32]);
        let mut agent = agent;
        let focus = LiveRecord {
            app: "so.tilo.notes".into(),
            record: "file:/workspace/Notes/brief.md".into(),
        };
        let view = tilo_session_host::presence::view(
            0,
            tilo_clock::unix_now_ms(),
            "Writer helper",
            [0x7c, 0x3a, 0xed],
            Some("New note".into()),
            Some(&focus),
            None,
        );
        let own = Own {
            label: "Writer helper".into(),
            device: "writer".into(),
            user: Some(hex::encode([3_u8; 32])),
            shell: Some(Box::new(view)),
            ..Own::default()
        };
        agent.say(&own);
        let frames = agent.flush(1_000.0);
        assert!(!frames.is_empty(), "the board said nothing");
        for (_, frame) in &frames {
            let body = frame.encode().expect("a presence frame encodes");
            person.heard([7_u8; 32], &body, 1_000.0);
        }
        let heard = person
            .member([7_u8; 32])
            .and_then(|member| member.shell.as_deref())
            .expect("the agent's view reached the person's board");
        assert_eq!(heard.label, "Writer helper");
        assert!(heard.agent, "an agent says it is one");
        assert_eq!(heard.activity.as_deref(), Some("New note"));
        assert_eq!(heard.windows.len(), 1, "the record it works in");
    }

    /// A person's pointer on a workspace is not a view of a desk.
    #[test]
    fn a_pointer_is_not_a_view_of_a_desk() {
        let mut person = Board::new(WORKSPACE, [9_u8; 32]);
        let body = Presence::pointing(WORKSPACE, 3, 1.0, 2.0)
            .encode()
            .expect("a presence frame encodes");
        assert!(person.heard([7_u8; 32], &body, 1_000.0));
        assert!(
            person
                .member([7_u8; 32])
                .is_some_and(|member| member.shell.is_none())
        );
    }
}

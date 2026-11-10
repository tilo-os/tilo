//! An agent's word in a workspace: the presence the client keeps, said on this agent's session so
//! every desktop's roster draws it beside the people's.

use std::sync::Arc;

use collab_types::presence::{Presence as Frame, ShellPresence};
use iroh::{EndpointAddr, EndpointId};
use tilo_desktop_model::WorkspaceId as DeskWorkspaceId;
use tilo_session_client::presence::OwnPresence;
use tilo_session_client::{Device, Presence, SayPresence, topic_of};
use tilo_session_host::PeerHandle;
use tilo_user_identity::DeviceCert;

/// This agent's session as a participant's transport: what the client says leaves on the workspace's
/// live plane, as every other device's word does.
struct Saying {
    peer: PeerHandle,
}

impl std::fmt::Debug for Saying {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("live plane")
    }
}

impl SayPresence for Saying {
    fn say(&self, workspace: DeskWorkspaceId, sequence: u32, frame: &Frame) -> Result<(), String> {
        // The plane numbers the objects it carries itself; `sequence` is the board's count.
        let _ = sequence;
        let topic = topic_of(workspace);
        self.peer.publish_presence(topic, vec![frame.clone()])
    }
}

/// The device an agent is on the plane: its certificate's own device key and label, belonging to the
/// person its root signed for.
#[must_use]
pub fn device(cert: &DeviceCert) -> Option<Device> {
    let publisher = EndpointId::from_bytes(&cert.device_public_key).ok()?;
    Some(Device {
        publisher,
        endpoint: EndpointAddr::new(publisher),
        person: Some(cert.user_public_key),
        name: cert.device_label.clone(),
    })
}

/// The agent's presence in the workspace `workspace` (its UUID), which `peer` already took part in:
/// it signs as `label` of the person `cert` belongs to, and says nothing of itself until its word is
/// set.
#[must_use]
pub fn enter(
    peer: &PeerHandle,
    cert: &DeviceCert,
    label: &str,
    workspace: &str,
) -> Option<Presence> {
    let device = device(cert)?;
    let workspace_id = DeskWorkspaceId::parse(workspace)?;
    let presence = Presence::over(
        Some(Arc::new(Saying { peer: peer.clone() })),
        &device,
        OwnPresence {
            label: label.to_owned(),
            user: Some(hex::encode(cert.user_public_key)),
            ..OwnPresence::default()
        },
    );
    let topic = topic_of(workspace_id);
    presence.taking_part(&[(topic, workspace.to_owned())], Some(topic));
    Some(presence)
}

/// Say `view` as this agent's own word, and what the workspace's board owes beside it.
pub fn say(presence: &Presence, view: &ShellPresence) {
    presence.set_where(None, Some(Box::new(view.clone())));
}

/// Say what the board owes now: a change since the last word, one lost, or the restatement a member
/// who came in later needs.
pub fn due(presence: &Presence, workspace: &str) {
    presence.due_in(workspace);
}

/// Take this agent down where it stood: the members' rosters stop drawing it.
pub fn leave(presence: &Presence) {
    presence.taking_part(&[], None);
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
    use tilo_relay_protocol::live::LiveRecord;

    const WORKSPACE: &str = "6b3f9a10-0000-4000-8000-00000000000c";

    /// The publisher `seed` names.
    fn publisher(seed: u8) -> [u8; 32] {
        iroh::SecretKey::from_bytes(&[seed; 32])
            .public()
            .as_bytes()
            .to_owned()
    }

    /// A presence saying nothing on any transport, so what it would have said is read off it.
    fn presence(seed: u8) -> Presence {
        let publisher = EndpointId::from_bytes(&publisher(seed)).expect("a publisher");
        Presence::over(
            None,
            &Device {
                publisher,
                endpoint: EndpointAddr::new(publisher),
                person: Some([3; 32]),
                name: "writer".into(),
            },
            OwnPresence {
                label: "Writer helper".into(),
                user: Some(hex::encode([3; 32])),
                ..OwnPresence::default()
            },
        )
    }

    /// The agent's own word, heard by the person's board: the same presence a person's devices
    /// speak, from a device that says it is an agent.
    #[test]
    fn an_agents_word_is_heard_as_an_agents_view_of_itself() {
        let agent = presence(7);
        let topic = topic_of(DeskWorkspaceId::parse(WORKSPACE).expect("a workspace"));
        agent.taking_part(&[(topic, WORKSPACE.to_owned())], Some(topic));
        let focus = LiveRecord {
            app: "so.tilo.notes".into(),
            record: "file:/workspace/Notes/brief.md".into(),
        };
        let view = tilo_session_client::presence::view(
            0,
            tilo_clock::unix_now_ms(),
            "Writer helper",
            [0x7c, 0x3a, 0xed],
            Some("New note".into()),
            Some(&focus),
            None,
        );
        say(&agent, &view);
        let said = agent.take_said();
        assert!(!said.is_empty(), "the board said nothing");

        let person = presence(9);
        person.taking_part(&[(topic, WORKSPACE.to_owned())], Some(topic));
        let from = EndpointId::from_bytes(&publisher(7)).expect("a publisher");
        for frame in &said {
            person.heard_presence(WORKSPACE, from, frame);
        }
        let heard = person
            .members(WORKSPACE)
            .into_iter()
            .find(|member| member.publisher == hex::encode(publisher(7)))
            .expect("the agent's view reached the person's board");
        assert_eq!(heard.label, "Writer helper");
        assert!(heard.agent, "an agent says it is one");
        assert_eq!(heard.activity.as_deref(), Some("New note"));
        assert_eq!(
            heard.focus.as_ref().map(|focus| focus.record.clone()),
            Some(hex::encode(focus.short_digest()))
        );
    }
}

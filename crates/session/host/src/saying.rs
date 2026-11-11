//! This session's presence tracks, as a participant's client says on them: the workspace's live
//! plane, which every device of the workspace says its word on.

use std::sync::Arc;

use iroh_base::{EndpointAddr, EndpointId};
use tilo_desktop_model::WorkspaceId;
use tilo_session_client::presence::OwnPresence;
use tilo_session_client::{Device, Presence, SayPresence, topic_of};
use tilo_user_identity::DeviceCert;

use crate::session::PeerHandle;

/// Where a client's presence frames go on `peer`'s session: the live plane of the workspace each
/// frame names, or nothing where this session holds none — presence is ephemeral, and the next
/// change says where its sender is.
#[must_use]
pub fn saying(peer: &PeerHandle) -> Arc<dyn SayPresence> {
    Arc::new(Saying { peer: peer.clone() })
}

/// The device `cert` makes on the plane: its own device key and label, belonging to the person its
/// root signed for.
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

/// A program's presence in the workspace `workspace` (its UUID), which `peer` already took part in:
/// it says as `label` of the person `cert` belongs to, and nothing of itself until its word is set.
/// What an agent and `tilo mcp` say on, so every member's board draws them beside the people.
#[must_use]
pub fn enter(
    peer: &PeerHandle,
    cert: &DeviceCert,
    label: &str,
    workspace: &str,
) -> Option<Presence> {
    let workspace_id = WorkspaceId::parse(workspace)?;
    let presence = Presence::over(
        Some(saying(peer)),
        &device(cert)?,
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
        iroh_base::SecretKey::from_bytes(&[seed; 32])
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
        let topic = topic_of(WorkspaceId::parse(WORKSPACE).expect("a workspace"));
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
        agent.set_where(None, Some(Box::new(view)));
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

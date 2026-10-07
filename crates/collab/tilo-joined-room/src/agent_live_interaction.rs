//! Current owner-backed authority for ephemeral managed-Agent interactions.
//!
//! The live frame remains inert. This capability retains the exact enrolled
//! owner and rechecks the authenticated publishing device and Agent membership
//! whenever the action broker consumes a frame.

use std::time::SystemTime;

use collab_types::Role;
use tilo_desktop_model::RoomId;

use crate::{NativeCommittedView, NativeWorkspaceOwner, WeakNativeWorkspaceOwner};

/// Revalidatable authority for one host-attested Agent on one room's live plane.
///
/// Fields are private so decoded interaction JSON, app input, or a raw device
/// id cannot manufacture this value.
#[derive(Clone)]
pub struct AgentLiveInteractionPublisher {
    owner: WeakNativeWorkspaceOwner,
    room: RoomId,
    publisher: [u8; 32],
    agent: String,
}

impl AgentLiveInteractionPublisher {
    /// The canonical broker actor id proven by this capability.
    #[must_use]
    pub fn actor_id(&self) -> String {
        format!("agent:{}", self.agent)
    }

    /// The exact room this attestation covers.
    #[must_use]
    pub const fn room(&self) -> RoomId {
        self.room
    }

    /// Recheck owner continuity, the pinned host certificate, and current
    /// signed Agent membership immediately before delivery.
    ///
    /// # Errors
    /// The owner retired or became fenced, the publishing device is no longer
    /// the pinned host, its certificate is not current, or the Agent lost its
    /// exact-room membership.
    pub fn revalidate(&self, now: SystemTime) -> Result<(), String> {
        self.owner
            .upgrade()
            .ok_or("native workspace owner retired")?
            .with_current(self.room, |view| {
                verify(view, self.publisher, &self.agent, now)
            })
    }
}

impl NativeWorkspaceOwner {
    /// Authenticate a host-stamped live frame as one current managed Agent.
    ///
    /// # Errors
    /// The room is unavailable, the device is not its exact pinned host, its
    /// certificate is stale, or `agent` is not a current room member.
    pub fn agent_live_interaction_publisher(
        &self,
        room: RoomId,
        publisher: [u8; 32],
        agent: &str,
        now: SystemTime,
    ) -> Result<AgentLiveInteractionPublisher, String> {
        self.with_current(room, |view| verify(view, publisher, agent, now))?;
        Ok(AgentLiveInteractionPublisher {
            owner: self.downgrade(),
            room,
            publisher,
            agent: agent.to_owned(),
        })
    }
}

fn verify(
    view: &NativeCommittedView,
    publisher: [u8; 32],
    agent: &str,
    now: SystemTime,
) -> Result<(), String> {
    tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RemoveAgent {
        agent: agent.to_owned(),
    }
    .validate()?;
    let pinned = view.host().unwrap_or_else(|| view.governor());
    if pinned.device_public_key != publisher
        || (view.host().is_some() && !pinned.capabilities.can_act_as_host)
        || !view.peer().certificates().iter().any(|cert| cert == pinned)
    {
        return Err("live Agent interaction lacks the exact current pinned host".into());
    }
    view.peer().permissions(publisher, now)?;
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| "live Agent interaction clock precedes the Unix epoch")?
        .as_secs();
    let room = view
        .peer()
        .admitted_room()
        .ok_or("live Agent interaction room membership is absent")?;
    let (_, owner, capabilities) = tilo_peer_state::room::extract_governed_fields(room);
    let mut workspace = tilo_desktop_model::Workspace::new(
        tilo_desktop_model::WorkspaceId(1),
        "live Agent interaction authority",
    );
    workspace.owner = owner;
    workspace.capabilities = capabilities;
    if workspace
        .role_of_agent_in_room_at(agent, view.peer().room(), seconds)
        .is_none_or(|role| role == Role::Denied)
    {
        return Err("live Agent interaction Agent is not a current room member".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_session::tests::set_agent_membership;
    use crate::application_record::tests::{fixture, fixture_with_expiry};

    #[test]
    fn publisher_is_exact_pinned_host_and_rechecks_agent_membership() {
        let fixture = fixture();
        let room = RoomId::parse(crate::testing::ROOM).unwrap();
        set_agent_membership(&fixture, true);
        let publisher = fixture
            .owner
            .agent_live_interaction_publisher(
                room,
                fixture.certificate.device_public_key,
                "scribe",
                fixture.now,
            )
            .expect("the current pinned host attests its current Agent");
        assert_eq!(publisher.actor_id(), "agent:scribe");
        publisher.revalidate(fixture.now).unwrap();

        assert!(
            fixture
                .owner
                .agent_live_interaction_publisher(room, [0x55; 32], "scribe", fixture.now)
                .is_err(),
            "a raw or foreign device cannot borrow the pinned host"
        );
        assert!(
            fixture
                .owner
                .agent_live_interaction_publisher(
                    RoomId::new(),
                    fixture.certificate.device_public_key,
                    "scribe",
                    fixture.now,
                )
                .is_err(),
            "a current host cannot attest an Agent into another room"
        );
        set_agent_membership(&fixture, false);
        assert!(
            publisher.revalidate(fixture.now).is_err(),
            "a retained capability cannot survive membership revocation"
        );
    }

    #[test]
    fn malformed_or_absent_agent_identity_never_creates_a_capability() {
        let fixture = fixture();
        let room = RoomId::parse(crate::testing::ROOM).unwrap();
        for agent in ["", "agent:scribe", "missing"] {
            assert!(
                fixture
                    .owner
                    .agent_live_interaction_publisher(
                        room,
                        fixture.certificate.device_public_key,
                        agent,
                        fixture.now,
                    )
                    .is_err(),
                "{agent:?} must not be inferred from an interaction claim"
            );
        }

        let expired = fixture_with_expiry(Some(std::time::Duration::from_secs(1)));
        set_agent_membership(&expired, true);
        assert!(
            expired
                .owner
                .agent_live_interaction_publisher(
                    room,
                    expired.certificate.device_public_key,
                    "scribe",
                    expired.now + std::time::Duration::from_secs(2),
                )
                .is_err(),
            "an expired host certificate cannot attest an Agent interaction"
        );
    }
}

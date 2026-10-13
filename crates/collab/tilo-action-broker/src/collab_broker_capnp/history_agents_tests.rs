//! An app's history names the agents its room's governance verified: the
//! broker tells each context receiver who they are when it subscribes, and
//! again on the policy turn after they change, never otherwise.
use super::*;

/// A host whose authority answers a room's verified agents from a list the
/// test changes, as the shell's answers from the room's governance.
struct Verified {
    tree: crate::test_support::WorkspaceTreeHandle,
    room: String,
    agents: Mutex<Vec<ui::history_source::RosterAgent>>,
}

impl crate::services::WorkspaceAuthority for Verified {
    fn with_tree(
        &self,
        read: &mut dyn FnMut(&tilo_desktop_model::DesktopTree),
    ) -> Result<(), String> {
        read(&self.tree.read().desktop);
        Ok(())
    }

    fn history_agents(&self, room: &str) -> Vec<ui::history_source::RosterAgent> {
        if room == self.room {
            self.agents.lock().unwrap().clone()
        } else {
            Vec::new()
        }
    }
}

fn scribe(label: &str) -> ui::history_source::RosterAgent {
    ui::history_source::RosterAgent {
        device: [3; 32],
        person: [1; 32],
        person_name: "Ada".into(),
        agent_id: "scribe".into(),
        label: label.into(),
        color: [10, 20, 30],
    }
}

fn history_frames(
    rx: &mut mpsc::UnboundedReceiver<ContextEnvelope>,
) -> Vec<Vec<ui::history_source::RosterAgent>> {
    let mut frames = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        if let ContextEnvelope::HistoryAgents(agents) = envelope {
            frames.push(agents);
        }
    }
    frames
}

#[test]
fn context_receivers_learn_their_rooms_verified_agents_and_each_change() {
    let room = uuid::Uuid::from_u128(11).to_string();
    let other = uuid::Uuid::from_u128(12).to_string();
    let authority = Arc::new(Verified {
        tree: crate::test_support::WorkspaceTree::default().shared(),
        room: room.clone(),
        agents: Mutex::new(vec![scribe("Scribe")]),
    });
    let (state, _bridge) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(authority.clone()),
        None,
        None,
        HostServices::default(),
    );
    let mut in_room = state.subscribe_context(
        &CrdtRoute::record("so.tilo.notes", &room, "a").channel_key(),
        1,
        None,
    );
    let mut elsewhere = state.subscribe_context(
        &CrdtRoute::record("so.tilo.notes", &other, "b").channel_key(),
        2,
        None,
    );
    assert_eq!(
        history_frames(&mut in_room),
        [vec![scribe("Scribe")]],
        "a receiver learns its room's agents as it subscribes"
    );
    assert!(
        history_frames(&mut elsewhere).is_empty(),
        "another room's agents are not this room's"
    );

    state.refresh_history_agents();
    assert!(
        history_frames(&mut in_room).is_empty(),
        "nothing changed, so nothing is said again"
    );

    *authority.agents.lock().unwrap() = vec![scribe("Scribe II")];
    state.refresh_history_agents();
    assert_eq!(history_frames(&mut in_room), [vec![scribe("Scribe II")]]);
    assert!(history_frames(&mut elsewhere).is_empty());

    // Governance withdrew trust in the delegation: the room's agents are
    // none, and the receiver is told so.
    authority.agents.lock().unwrap().clear();
    state.refresh_history_agents();
    assert_eq!(history_frames(&mut in_room), [Vec::new()]);
}

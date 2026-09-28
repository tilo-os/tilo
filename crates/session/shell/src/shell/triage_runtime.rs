//! Keeping the team's feedback triage agent running wherever it works.
//!
//! On the Tilo team's desktop the triage agent runs, as a desktop-managed
//! agent session (the same launcher path Task Center agents use), in every
//! room that admits it and lets this person operate it: the community room,
//! which the team administers, and each reporter's direct room, whose owner
//! named the team the agent's operator (`Scope::Agent`). Nobody launches it
//! per room. A room that appears with those grants gets the agent; a room
//! that loses them no longer does, and the agent in it stops by itself as
//! soon as its broker says it was removed.
//!
//! Only the team's desktop does this. A reporter owns their direct room and
//! so administers it, but the agent there is the team's, run on the team's
//! model and forge account; the reporter's desktop never starts it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::shell::launcher::{LaunchEvent, LaunchEventKind};
use crate::shell::state::{RoomId, WorkspaceTree};
use crate::shell::workspace_settings::{TRIAGE_AGENT, triage_agent_admitted};

/// How long after a session ended before it is started again in a room that
/// still wants it.
pub const RESTART_AFTER: Duration = Duration::from_secs(30);

/// Where the agent should run, and what it triages there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wanted {
    pub room: RoomId,
    /// A reporter's direct room rather than the public community room.
    pub direct: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Run {
    /// Asked for; the launcher has not said it ended.
    Live,
    /// Ended (or never started) at this time.
    Ended(Instant),
}

/// The rooms `me` should run the triage agent in: it is admitted there and
/// `me` may operate it.
#[must_use]
pub fn wanted(tree: &WorkspaceTree, me: &str, now_unix: u64) -> Vec<Wanted> {
    tree.desktop
        .workspaces
        .iter()
        .filter(|ws| {
            !ws.room.is_nil()
                && triage_agent_admitted(ws)
                && ws.may_operate_agent_at(&me.to_string(), TRIAGE_AGENT, now_unix)
        })
        .map(|ws| Wanted {
            room: ws.room,
            direct: ws.channel_class != collab_types::ChannelClass::Public,
        })
        .collect()
}

/// The team desktop's triage sessions, one per room.
#[derive(Debug, Default)]
pub struct TriageRuntime {
    runs: HashMap<RoomId, Run>,
}

impl TriageRuntime {
    /// Start the agent in every room of `wanted` it is not running in (or
    /// ended in more than [`RESTART_AFTER`] ago), through `launch`, and
    /// forget rooms no longer wanted. `true` when anything changed.
    pub fn reconcile(
        &mut self,
        wanted: &[Wanted],
        now: Instant,
        launch: &mut dyn FnMut(Wanted) -> Result<(), String>,
    ) -> bool {
        let before = self.runs.len();
        self.runs
            .retain(|room, _| wanted.iter().any(|want| want.room == *room));
        let mut changed = self.runs.len() != before;
        for want in wanted {
            let due = match self.runs.get(&want.room) {
                None => true,
                Some(Run::Live) => false,
                Some(Run::Ended(at)) => now.duration_since(*at) >= RESTART_AFTER,
            };
            if !due {
                continue;
            }
            let run = match launch(*want) {
                Ok(()) => Run::Live,
                Err(error) => {
                    tracing::warn!(room = %want.room, %error, "triage: could not start the agent");
                    Run::Ended(now)
                }
            };
            self.runs.insert(want.room, run);
            changed = true;
        }
        changed
    }

    /// Note that a triage session ended, from the launcher's events.
    pub fn observe(&mut self, event: &LaunchEvent, now: Instant) -> bool {
        if event.managed_agent_id.as_deref() != Some(TRIAGE_AGENT) {
            return false;
        }
        let Some(room) = event
            .managed_workspace_id
            .as_deref()
            .and_then(RoomId::parse)
        else {
            return false;
        };
        match &event.kind {
            LaunchEventKind::Started { .. } => false,
            LaunchEventKind::SpawnFailed { .. }
            | LaunchEventKind::Failed { .. }
            | LaunchEventKind::Exited => {
                if let Some(run) = self.runs.get_mut(&room) {
                    *run = Run::Ended(now);
                }
                true
            }
        }
    }

    /// Whether a session was asked for in `room` and has not ended.
    #[must_use]
    pub fn is_running(&self, room: RoomId) -> bool {
        matches!(self.runs.get(&room), Some(Run::Live))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::community::{DirectRoomSetup, prepare_direct_room};
    use crate::shell::state::WorkspaceAuthority as _;

    /// The team's desktop starts triage in a new direct room with nobody
    /// launching it, never in a room it may not operate the agent in, not
    /// twice while it runs, and not again once the reporter removed it.
    #[test]
    fn a_new_direct_room_gets_triage_without_a_manual_launch() {
        let team = "team-person".to_string();
        let now_unix = tilo_clock::unix_now();
        let mut tree = WorkspaceTree::default();
        let mut runtime = TriageRuntime::default();
        let launched = std::cell::RefCell::new(Vec::<Wanted>::new());
        let mut launch = |want: Wanted| {
            launched.borrow_mut().push(want);
            Ok(())
        };

        // A reporter's direct room arrives, admitting the agent with the
        // team as its operator (built here as the reporter's end builds it).
        let room = RoomId::parse("7a1a9e00-0000-4c0d-9a11-0000000000d3").expect("id");
        prepare_direct_room(
            &mut tree,
            &DirectRoomSetup {
                room,
                other: &team,
                label: "Reporter",
                governing: true,
                triage: true,
            },
        )
        .expect("the room");
        // The reporter owns it; the team is a member and the agent's operator.
        let reporter = crate::shell::state::local_user_id_for_room(&room);
        assert!(
            !tree
                .desktop
                .workspaces
                .iter()
                .find(|ws| ws.room == room)
                .expect("the room")
                .may_administer(&team),
            "the team does not own it"
        );
        // A room without the agent is none of triage's business.
        let other = tree.append_empty_workspace("Elsewhere");
        tree.set_room(other, RoomId::new());

        let want = wanted(&tree, &team, now_unix);
        assert_eq!(want, vec![Wanted { room, direct: true }]);
        let t0 = Instant::now();
        assert!(runtime.reconcile(&want, t0, &mut launch));
        assert!(!runtime.reconcile(&want, t0, &mut launch), "not twice");
        assert_eq!(*launched.borrow(), vec![Wanted { room, direct: true }]);
        assert!(runtime.is_running(room));

        // The reporter removes it: its session ends, and it is not started
        // again.
        {
            let ws = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|ws| ws.room == room)
                .expect("the room");
            crate::shell::workspace_settings::remove_agent(ws, &reporter, TRIAGE_AGENT);
        }
        runtime.observe(
            &LaunchEvent {
                package_id: crate::shell::launcher::WORKSPACE_AGENT_PACKAGE_ID.into(),
                display_name: "Agent".into(),
                managed_agent_id: Some(TRIAGE_AGENT.into()),
                managed_workspace_id: Some(room.to_string()),
                kind: LaunchEventKind::Exited,
            },
            t0,
        );
        let want = wanted(&tree, &team, now_unix);
        assert!(want.is_empty());
        runtime.reconcile(&want, t0 + RESTART_AFTER * 2, &mut launch);
        assert_eq!(
            launched.borrow().len(),
            1,
            "a removed agent is not started again"
        );
        assert!(!runtime.is_running(room));
    }
}

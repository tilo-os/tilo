//! The session's runs, as the broker's run channel reads them.
//!
//! The broker delivers a run to its agent and admits what the agent's steps
//! do, as the agent acting for the person who asked. Both are facts only this
//! session holds — who asked, in which room, with which agents admitted and
//! what grants — so the run channel asks them here and never takes them from
//! an action's arguments.

use tilo_action_broker::services::{NativeTaskActionRequirement, TrackerRun, TrackerRuns};
use tilo_clock::wall;

use crate::AgentRuns;

impl TrackerRuns for AgentRuns {
    /// Only the rooms this session holds a workspace of: it keeps no other
    /// room's runs.
    fn requires_room(&self, room: &str) -> bool {
        self.knows(room)
    }

    fn runs(&self, room: &str) -> Result<Vec<TrackerRun>, String> {
        self.tracked(room)
    }

    fn check_agent(&self, room: &str, agent: &str) -> Result<(), String> {
        self.admits(room, agent)
    }

    /// Whether the run's agent may do `requirement`: admitted to the room
    /// now, still working, and allowed it by the room's grants for the
    /// person whose ask started it — the same answer a room's task gets.
    fn authorize_action(
        &self,
        room: &str,
        run: &TrackerRun,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        self.admits(room, &run.agent)?;
        let policy = self
            .room_policy(room)
            .ok_or("this workspace's grants are not at hand")?;
        requirement.check_tracker_policy(&policy, room, run, wall())
    }

    fn presented(
        &self,
        room: &str,
        issue: &str,
        epoch: u64,
        presentation: Option<&runtime_collab::TaskPresentation>,
    ) {
        self.absorb_presentation(room, issue, epoch, presentation);
    }

    fn reported(
        &self,
        room: &str,
        issue: &str,
        epoch: u64,
        update: &runtime_collab::TaskRunUpdate,
    ) {
        self.absorb_report(room, issue, epoch, update);
    }
}

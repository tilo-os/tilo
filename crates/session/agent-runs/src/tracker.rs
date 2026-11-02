//! The session's runs, as the broker's run channel reads them.
//!
//! The broker delivers a run to its agent and admits what the agent's steps
//! do, as the agent acting for the person who asked. Both are facts only this
//! session holds — who asked, in which workspace, with which agents admitted and
//! what grants — so the run channel asks them here and never takes them from
//! an action's arguments.

use tilo_action_broker::services::{NativeTaskActionRequirement, TrackerRun, TrackerRuns};
use tilo_clock::wall;

use crate::AgentRuns;

impl TrackerRuns for AgentRuns {
    /// Only a workspace this session holds: it keeps no other workspace's
    /// runs.
    fn requires_workspace(&self, workspace: &str) -> bool {
        self.knows(workspace)
    }

    fn runs(&self, workspace: &str) -> Result<Vec<TrackerRun>, String> {
        self.tracked(workspace)
    }

    fn check_agent(&self, workspace: &str, run: &TrackerRun) -> Result<(), String> {
        self.admits(workspace, &run.agent)
    }

    /// Whether the run's agent may do `requirement`: admitted to the workspace
    /// now, still working, and allowed it by the workspace's grants for the
    /// person whose ask started it — the same answer a workspace's task gets.
    fn authorize_action(
        &self,
        workspace: &str,
        run: &TrackerRun,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        self.admits(workspace, &run.agent)?;
        let policy = self
            .workspace_policy(workspace)
            .ok_or("this workspace's grants are not at hand")?;
        requirement.check_tracker_policy(&policy, workspace, run, wall())
    }

    fn presented(
        &self,
        workspace: &str,
        issue: &str,
        epoch: u64,
        presentation: Option<&runtime_collab::TaskPresentation>,
    ) {
        self.absorb_presentation(workspace, issue, epoch, presentation);
    }

    fn reported(
        &self,
        workspace: &str,
        issue: &str,
        epoch: u64,
        update: &runtime_collab::TaskRunUpdate,
    ) {
        self.absorb_report(workspace, issue, epoch, update);
    }
}

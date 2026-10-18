//! Host-local proof of the exact native run that produced painted activity.
//! Tokens survive app disconnect and marker absence, but never a broker restart.
//! Eviction removes an affordance; it cannot rebind an old token to new work.
use super::{CapnpBrokerState, InvocationOwner, PendingInvocation, execution_activity::ShownActor};
use std::collections::VecDeque;

const CAPACITY: usize = 1024;

#[derive(Default)]
pub(super) struct PaintOrigins(VecDeque<PaintOrigin>);
struct PaintOrigin {
    room: String,
    app: String,
    invocation: tilo_actions::InvocationId,
    shown: ShownActor,
    agent: String,
    task: String,
    epoch: u64,
}
impl PaintOrigins {
    pub(super) fn clear(&mut self) {
        self.0.clear();
    }

    fn retain(&mut self, pending: &PendingInvocation) {
        let InvocationOwner::NativeTask { agent, task, epoch } = &pending.owner else {
            return;
        };
        let Some(target) = pending.activity.as_ref() else {
            return;
        };
        let Some(shown) = target.shown() else { return };
        if *epoch == 0
            || self
                .0
                .iter()
                .any(|held| held.shown.paint_origin == shown.paint_origin)
        {
            return;
        }
        while self.0.len() >= CAPACITY {
            self.0.pop_front();
        }
        self.0.push_back(PaintOrigin {
            room: target.route().cluster_id.clone(),
            app: target.route().app_id.clone(),
            invocation: pending.update.invocation(),
            shown: shown.clone(),
            agent: agent.clone(),
            task: task.clone(),
            epoch: *epoch,
        });
    }

    fn resolve(
        &self,
        room: &str,
        app: &str,
        agent: &str,
        interaction: &tilo_actions::SemanticInteraction,
    ) -> Option<(String, u64)> {
        let token = interaction.paint_origin?;
        if token.is_nil() || interaction.validate().is_err() {
            return None;
        }
        let proof = self
            .0
            .iter()
            .find(|proof| proof.shown.paint_origin == token)?;
        (proof.room == room
            && proof.app == app
            && proof.agent == agent
            && proof.invocation == interaction.invocation
            && proof.shown.actor == interaction.actor
            && interaction.actor.kind == tilo_actions::ActorKind::Agent
            && proof.shown.action == interaction.action
            && proof.shown.resource == interaction.resource
            && proof.shown.group == interaction.group
            && interaction
                .group
                .as_ref()
                .is_some_and(|group| group.kind == "task" && group.id == proof.task))
        .then(|| (proof.task.clone(), proof.epoch))
    }
}

impl CapnpBrokerState {
    pub(super) fn retain_painted_task_origin(&self, pending: &PendingInvocation) {
        if let Ok(mut origins) = self.painted_task_origins.lock() {
            origins.retain(pending);
        }
    }

    /// Resolve inert painted correlation against the original host admission.
    /// Current membership is necessary but never supplies the task or epoch.
    pub fn certified_painted_task(
        &self,
        room: &str,
        app: &str,
        agent: &str,
        interaction: &tilo_actions::SemanticInteraction,
    ) -> Option<(String, u64)> {
        if self
            .certified_agent_identity(room, &interaction.actor.id)?
            .agent_key
            != agent
        {
            return None;
        }
        self.painted_task_origins
            .lock()
            .ok()?
            .resolve(room, app, agent, interaction)
    }
}

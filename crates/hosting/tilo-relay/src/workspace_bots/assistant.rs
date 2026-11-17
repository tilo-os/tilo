//! What this host answers for the programs that run for a person
//! ([`tilo_agent_session::bot::host_steps`]).
//!
//! Calling a person's own model is one entry of the host's
//! ([`Workspaces::person_model`]), and starting a turn of a person's
//! assistant is another, which every event, reminder and sub-agent starts
//! work through. This module is the one place a program's steps reach both
//! from.
//!
//! A judgement is answered by the model its person chose for judging
//! ([`Interaction::Classifier`]), asked with their own key, and never by a
//! model somebody else pays for: a person who linked none is told so in the
//! words their settings would say it in, and the run is kept with that
//! reason. What a judgement costs is counted on the day of the assistant
//! its person keeps in the workspace, where they keep one, so the limit
//! they set their assistant bounds what its events spend as well.
//!
//! Until the entry that starts a turn is on this host, an instruction a
//! program hands over finds nothing to take it: the run is tried again, and
//! kept as a dead letter once its attempts are spent, so what was not
//! delivered can be read and run again.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde_json::Value;
use tilo_agent_session::bot::host_steps::{HostStepError, HostSteps, Instruction, Judgement};
use tilo_agent_session::bot::judging;
use tilo_relay_protocol::WorkspaceId;
use tilo_workspace_agent::{Agent, WorkspaceAgents};

use crate::workspaces::Workspaces;
use crate::workspaces::person_model::protocol::ErrorKind;
use crate::workspaces::person_model::{Interaction, ModelRefusal, Spender};

/// The app that keeps the workspace's record of its agents.
use ui::apps::CHAT;

/// What this host answers for a program that runs for a person.
#[must_use]
pub fn host_steps(workspaces: Workspaces) -> Arc<dyn HostSteps> {
    Arc::new(Hosted { workspaces })
}

/// The assistant `person` keeps in `workspace`: of the active agents they
/// keep there that no MCP client connects as, the one made first. None
/// where they keep no such agent.
///
/// # Errors
/// The workspace's record of its agents could not be read.
pub async fn assistant_of(
    workspaces: &Workspaces,
    workspace: &str,
    person: &[u8; 32],
) -> Result<Option<Agent>, String> {
    let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let Some(bytes) = workspaces
        .resource_document(workspace, CHAT, &record)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let mut doc = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
    let agents = super::opened::<WorkspaceAgents>(&mut doc)?;
    Ok(agents
        .agents
        .iter()
        .filter(|agent| {
            agent.is_active()
                && agent.mcp_client.is_none()
                && tilo_action_policy::PersonId::parse(&agent.keeper)
                    .ok()
                    .and_then(|keeper| keeper.root())
                    .as_ref()
                    == Some(person)
        })
        .min_by_key(|agent| agent.created_at)
        .cloned())
}

/// The host's own answers: a person's model for a judgement, and nothing
/// yet that takes an instruction.
struct Hosted {
    workspaces: Workspaces,
}

impl HostSteps for Hosted {
    fn instruct(&self, _instruction: Instruction) -> Result<String, HostStepError> {
        Err(HostStepError::Unavailable(
            "this host starts no turn of a person's assistant yet".into(),
        ))
    }

    fn judge(&self, judgement: Judgement) -> Result<Value, HostStepError> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            HostStepError::Unavailable("this host is not running its workspaces".into())
        })?;
        // The thread that asks is driving its run and cannot wait on the
        // runtime a second time: the model is waited for on a thread of the
        // judgement's own, for as long as the judgement takes.
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .name("judgement".into())
                .spawn_scoped(scope, || self.judged(&runtime, &judgement))
                .map_err(|error| HostStepError::Unavailable(error.to_string()))?
                .join()
                .unwrap_or_else(|_| {
                    Err(HostStepError::Unavailable(
                        "the judgement did not come to an end".into(),
                    ))
                })
        })
    }
}

impl Hosted {
    /// `judgement`, answered by its person's own model. Called on a thread
    /// that drives nothing else.
    fn judged(
        &self,
        runtime: &tokio::runtime::Handle,
        judgement: &Judgement,
    ) -> Result<Value, HostStepError> {
        let refused = |why: &str| HostStepError::Refused(why.to_owned());
        let person: [u8; 32] = hex::decode(&judgement.person)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| refused("the program runs for nobody this host knows"))?;
        let workspace = WorkspaceId::parse(&judgement.workspace)
            .ok_or_else(|| refused("the program runs in no workspace of this host"))?;
        let assistant = runtime
            .block_on(assistant_of(
                &self.workspaces,
                &judgement.workspace,
                &person,
            ))
            .map_err(HostStepError::Unavailable)?;
        let day = crate::workspace_agents::today();
        let mut model = self
            .workspaces
            .person_model(person, Interaction::Classifier)
            .working_in(workspace);
        if let Some(agent) = &assistant {
            model = model.spending_as(Spender {
                workspace: judgement.workspace.clone(),
                agent: agent.id.0.clone(),
                name: agent.name.clone(),
                day: day.clone(),
                limit_micros: agent.spend_limit_micros,
            });
        }
        let cost = AtomicU64::new(0);
        let answer = judging::answer(judgement, &|asking| {
            let now = Instant::now();
            let said = model
                .say_blocking(
                    (asking.instructions, asking.input),
                    asking.max_output_tokens,
                    now.checked_add(asking.within).unwrap_or(now),
                    &|| false,
                )
                .map_err(|refusal| not_answered(&refusal))?;
            cost.fetch_add(said.response.cost_micros, Ordering::Relaxed);
            Ok(said.text())
        });
        // What the provider charged shows on the assistant's record, where
        // its keeper reads what it spent today.
        let cost = cost.load(Ordering::Relaxed);
        if cost > 0
            && let Some(agent) = &assistant
            && let Err(error) = runtime.block_on(crate::workspace_agents::count_spend(
                &self.workspaces,
                &judgement.workspace,
                agent,
                &day,
                cost,
            ))
        {
            tracing::warn!(workspace = %judgement.workspace, agent = %agent.id.0, %error, "a judgement's cost is not counted on its person's assistant");
        }
        answer
    }
}

/// Why a person's model gave no answer, as a program's run takes it: in
/// the words the refusal has for its person, tried again where a later
/// call could go differently, and kept with the reason where only the
/// person can change it (a model to link, a key, a limit to raise).
fn not_answered(refusal: &ModelRefusal) -> HostStepError {
    let words = refusal.words(None);
    let later = match refusal {
        ModelRefusal::SettingsUnreadable => true,
        ModelRefusal::Provider(error) => matches!(
            error.kind,
            ErrorKind::Unavailable
                | ErrorKind::Conflict
                | ErrorKind::InvalidResponse
                | ErrorKind::RateLimited
                | ErrorKind::Provider
                | ErrorKind::TimedOut
                | ErrorKind::Cancelled
                | ErrorKind::Truncated
                | ErrorKind::Transport
        ),
        _ => false,
    };
    if later {
        HostStepError::Unavailable(words)
    } else {
        HostStepError::Refused(words)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::person_model::protocol::ModelError;

    #[test]
    fn a_refusal_only_its_person_can_lift_is_kept_and_a_passing_one_is_tried_again() {
        let kept = not_answered(&ModelRefusal::NoModelLinked {
            kind: Interaction::Classifier,
        });
        assert!(
            matches!(&kept, HostStepError::Refused(why) if why.contains("No model is linked")),
            "{kept:?}"
        );
        for passing in [
            ErrorKind::RateLimited,
            ErrorKind::TimedOut,
            ErrorKind::Transport,
        ] {
            let again = not_answered(&ModelRefusal::Provider(ModelError::new(passing)));
            assert!(matches!(again, HostStepError::Unavailable(_)), "{again:?}");
        }
        for lasting in [ErrorKind::Authentication, ErrorKind::CreditsExhausted] {
            let kept = not_answered(&ModelRefusal::Provider(ModelError::new(lasting)));
            assert!(matches!(kept, HostStepError::Refused(_)), "{kept:?}");
        }
    }
}

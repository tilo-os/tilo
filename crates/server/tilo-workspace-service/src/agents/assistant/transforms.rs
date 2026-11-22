//! The agent's transforms, answered by this device's broker.
//!
//! A shortcut's `Ai` step and an app that wants a summary invoke
//! `agent.transform:default` ([`tilo_agent_session::transform`]) as the person, through the
//! device's broker, which admits it as any other call of theirs. The engine on this device answers
//! it: one bounded call of the workspace's model for the kind of work the transform is, its
//! answer held to the shape the caller asked for, and its cost counted on the day of the
//! assistant the person keeps in the workspace, as a judgement of their programs is.

use std::sync::atomic::{AtomicU64, Ordering};

use tilo_actions::ActionDescriptor;
use tilo_actions::remote::{Pending, pending};
use tilo_agent_session::transform::{self, Interaction, TransformModel};
use tilo_broker_core::steps::{HostStepOutcome, HostStepRequest, HostStepServices};
use tilo_server_protocol::WorkspaceId;
use ui::time::Duration;

use super::{assistant_of, person_of};
use crate::service::ServiceSlot;
use crate::workspaces::Workspaces;
use crate::workspaces::person_model::Spender;

/// The provider the transforms are answered under.
pub const PROVIDER: &str = "tilo.agent.transforms";

/// The agent's transforms, for the workspaces of the service `service` installs.
pub struct Transforms {
    service: ServiceSlot,
    /// Where a transform's cost is written once its answer came.
    runtime: tokio::runtime::Handle,
}

impl Transforms {
    /// The transforms of the workspaces `service` holds once it opens, their costs written on
    /// `runtime`.
    #[must_use]
    pub fn new(service: ServiceSlot, runtime: tokio::runtime::Handle) -> Self {
        Self { service, runtime }
    }
}

impl HostStepServices for Transforms {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn app_id(&self) -> &str {
        tilo_automation::AGENT_APP_ID
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        transform::descriptors()
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        if request.address.resource != transform::RESOURCE_ID {
            return Err(format!(
                "the agent publishes no resource `{}`",
                request.address.resource
            ));
        }
        let workspaces = self
            .service
            .service()
            .ok_or("this device has not opened its workspaces yet")?
            .workspaces()
            .clone();
        if !workspaces.work().agents {
            return Err("this device runs no agents of its workspaces".into());
        }
        let workspace = WorkspaceId::parse(request.workspace)
            .ok_or("the transform was asked in no workspace of this device")?;
        let content = ui::content::session(request.workspace)
            .unwrap_or_else(ui::content::ContentStore::memory);
        let call = transform::bind_wire(&request.address.action, request.argument, &content)
            .map_err(|failure| failure.message)?;
        let person = request
            .actor
            .direct_person()
            .or_else(|| request.actor.acting_for())
            .and_then(|person| person_of(person).ok());
        let runtime = self.runtime.clone();
        let (done, answer) = pending();
        std::thread::Builder::new()
            .name("agent-transform".into())
            .spawn(move || {
                let spender = person.and_then(|person| {
                    runtime
                        .block_on(assistant_of(&workspaces, &workspace.to_string(), &person))
                        .ok()
                        .flatten()
                });
                let model = Asked {
                    workspaces: &workspaces,
                    workspace,
                    spender: spender.as_ref().map(|agent| Spender {
                        workspace: workspace.to_string(),
                        agent: agent.id.0.clone(),
                        name: agent.name.clone(),
                        day: crate::agents::today(),
                        limit_micros: agent.spend_limit_micros,
                    }),
                    cost: AtomicU64::new(0),
                };
                let outcome = transform::run(&call, &model);
                let cost = model.cost.load(Ordering::Relaxed);
                if cost > 0
                    && let (Some(agent), Some(spender)) = (&spender, &model.spender)
                    && let Err(error) = runtime.block_on(crate::agents::count_spend(
                        &workspaces,
                        &spender.workspace,
                        agent,
                        &spender.day,
                        cost,
                    ))
                {
                    tracing::warn!(%workspace, agent = %agent.id.0, %error, "a transform's cost is not counted on its person's assistant");
                }
                done.complete(outcome);
            })
            .map_err(|error| format!("the transform could not start: {error}"))?;
        Ok(answer)
    }
}

/// The workspace's model, asked for one transform's answer.
struct Asked<'a> {
    workspaces: &'a Workspaces,
    workspace: WorkspaceId,
    /// The assistant whose day the answer is counted on, where its person keeps one.
    spender: Option<Spender>,
    /// What the answers cost, in millionths of the currency.
    cost: AtomicU64,
}

impl TransformModel for Asked<'_> {
    fn answer(
        &self,
        kind: Interaction,
        instructions: &str,
        input: &str,
        max_output_tokens: u32,
        timeout: Duration,
    ) -> Result<String, String> {
        let mut model = self.workspaces.workspace_model(self.workspace, kind);
        if let Some(spender) = &self.spender {
            model = model.spending_as(spender.clone());
        }
        let now = tilo_clock::now();
        let said = model
            .say_blocking(
                (instructions, input),
                max_output_tokens,
                now.checked_add(timeout).unwrap_or(now),
                &|| false,
            )
            .map_err(|refusal| refusal.words(None))?;
        self.cost
            .fetch_add(said.response.cost_micros, Ordering::Relaxed);
        Ok(said.text())
    }
}

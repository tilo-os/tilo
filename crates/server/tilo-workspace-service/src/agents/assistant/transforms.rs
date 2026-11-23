//! The agent's transforms, answered by this device's broker.
//!
//! A shortcut's `Ai` step and an app that wants a summary invoke
//! `agent.transform:default` ([`tilo_agent_session::transform`]) as the person, through the
//! device's broker, which admits it as any other call of theirs. The engine on this device answers
//! it: one bounded call of the workspace's model for the kind of work the transform is, its
//! answer held to the shape the caller asked for, and its cost counted on the day of the
//! assistant the person keeps in the workspace, as a judgement of their programs is.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

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
    /// The transforms running now, so a burst is refused as busy rather than piling up on the
    /// model ([`transform::MAX_RUNNING`]).
    running: Arc<AtomicUsize>,
}

impl Transforms {
    /// The transforms of the workspaces `service` holds once it opens, their costs written on
    /// `runtime`.
    #[must_use]
    pub fn new(service: ServiceSlot, runtime: tokio::runtime::Handle) -> Self {
        Self {
            service,
            runtime,
            running: Arc::default(),
        }
    }

    /// One place of the transforms that may run at once, or `None` when as many as the agent
    /// allows already run. The place is given back when the value is dropped.
    fn begin(&self) -> Option<Place> {
        self.running
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |running| {
                (running < transform::MAX_RUNNING).then_some(running.saturating_add(1))
            })
            .ok()
            .map(|_| Place(Arc::clone(&self.running)))
    }
}

/// One of the transforms that may run at once: dropped, it gives its place back.
struct Place(Arc<AtomicUsize>);

impl Drop for Place {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
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
        let Some(place) = self.begin() else {
            return Ok(Pending::ready(Err(Box::new(tilo_actions::ActionFailure {
                code: transform::code::BUSY.to_owned(),
                message: "the agent is already running as many transforms as it allows".to_owned(),
                details: None,
                retryable: true,
            }))));
        };
        let workspaces = self
            .service
            .service()
            .ok_or("this device has not opened its workspaces yet")?
            .workspaces()
            .clone();
        if workspaces.agent_keys().is_none() {
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
        let stop = Arc::clone(&request.stop);
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
                    stop: stop.as_ref(),
                };
                let outcome = transform::run_until(&call, &model, &stop);
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
                // The place is given back now the model call has stopped, however it ended.
                drop(place);
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
    /// Set when the caller cancelled the transform: the call under way ends on it.
    stop: &'a AtomicBool,
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
        self.answer_until(
            kind,
            (instructions, input),
            max_output_tokens,
            timeout,
            self.stop,
        )
    }

    fn answer_until(
        &self,
        kind: Interaction,
        (instructions, input): (&str, &str),
        max_output_tokens: u32,
        timeout: Duration,
        stop: &AtomicBool,
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
                &|| stop.load(Ordering::Relaxed),
            )
            .map_err(|refusal| refusal.words(None))?;
        self.cost
            .fetch_add(said.response.cost_micros, Ordering::Relaxed);
        Ok(said.text())
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

    /// As many transforms as the agent allows run at once; the next is refused as busy until one
    /// ends.
    #[test]
    fn no_more_than_the_allowed_transforms_run_at_once() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let transforms = Transforms::new(ServiceSlot::default(), runtime.handle().clone());
        let mut places = Vec::new();
        for _ in 0..transform::MAX_RUNNING {
            places.push(transforms.begin().expect("a place is free"));
        }
        assert!(
            transforms.begin().is_none(),
            "the agent is running as many transforms as it allows"
        );
        drop(places);
        assert!(
            transforms.begin().is_some(),
            "a place is free once a transform ends"
        );
    }
}

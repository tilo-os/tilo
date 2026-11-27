//! The agent's transforms, answered by this device's broker.
//!
//! A shortcut's `Ai` step and an app that wants a summary invoke
//! `agent.transform:default` ([`tilo_agent_session::transform`]) as the person, through the
//! device's broker, which admits it as any other call of theirs. The engine on this device answers
//! it: one bounded call of the workspace's model for the kind of work the transform is, its
//! answer held to the shape the caller asked for, and its cost counted on the day of the
//! assistant the person keeps in the workspace, as a judgement of their programs is.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tilo_person_services::PersonServices;

use tilo_actions::remote::{Admitted, Caller, InvokeRequest, Outcome, Pending, pending};
use tilo_agent_session::transform::{self, Interaction, TransformModel};
use tilo_lock::MutexRecover as _;
use tilo_member_types::Actor;
use tilo_server_protocol::WorkspaceId;
use ui::time::Duration;

use super::{assistant_of, person_of};
use tilo_person_services::model_answers::Spender;
use tilo_workspace_service::service::ServiceSlot;
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

/// The provider the transforms are answered under.
pub const PROVIDER: &str = "tilo.agent.transforms";

/// The agent's transforms, for the workspaces of the service `service` installs.
pub struct Transforms {
    service: ServiceSlot,
    /// Where a transform's cost is written once its answer came.
    runtime: tokio::runtime::Handle,
    /// The transforms running now, by the agent whose work each is, so a burst is refused as
    /// busy rather than piling up on the model ([`transform::MAX_RUNNING`]).
    running: Running,
}

impl Transforms {
    /// The transforms of the workspaces `service` holds once it opens, their costs written on
    /// `runtime`.
    #[must_use]
    pub fn new(service: ServiceSlot, runtime: tokio::runtime::Handle) -> Self {
        Self {
            service,
            runtime,
            running: Running::default(),
        }
    }
}

/// The transforms running now, counted per agent: each agent the device answers for gets its own
/// [`transform::MAX_RUNNING`] places, so a burst one agent asks for never holds another agent's
/// work.
#[derive(Clone, Default)]
struct Running {
    by_agent: Arc<Mutex<HashMap<Actor, usize>>>,
}

impl Running {
    /// One of `agent`'s places to run a transform in, or `None` when it already runs as many as
    /// it may. The place is given back when the value is dropped.
    fn begin(&self, agent: &Actor) -> Option<Place> {
        let mut by_agent = self.by_agent.lock_recover();
        let running = by_agent.entry(agent.clone()).or_default();
        if *running >= transform::MAX_RUNNING {
            return None;
        }
        *running = running.saturating_add(1);
        drop(by_agent);
        Some(Place {
            by_agent: Arc::clone(&self.by_agent),
            agent: agent.clone(),
        })
    }
}

/// One agent's place among its transforms that may run at once: dropped, it gives its place
/// back.
struct Place {
    by_agent: Arc<Mutex<HashMap<Actor, usize>>>,
    agent: Actor,
}

impl Drop for Place {
    fn drop(&mut self) {
        let mut by_agent = self.by_agent.lock_recover();
        let Some(running) = by_agent.get_mut(&self.agent) else {
            return;
        };
        *running = running.saturating_sub(1);
        if *running == 0 {
            by_agent.remove(&self.agent);
        }
    }
}

impl Transforms {
    /// Register the transforms on this device, answered under the agent's application: a
    /// shortcut's `Ai` step and an app that wants a summary reach them as the person's own call.
    pub fn register(self: Arc<Self>, providers: &Providers) {
        providers.provide(
            PROVIDER,
            tilo_automation::AGENT_APP_ID,
            Reach::ThisDevice,
            self,
        );
    }
}

impl tilo_actions::remote::Admits for Transforms {
    const PROVIDER: &'static str = PROVIDER;
    const APP: &'static str = tilo_automation::AGENT_APP_ID;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        tilo_actions::remote::admitted::top_level(transform::descriptors())
    }

    /// Start one of `PROVIDER`'s actions on this device's workspaces for `caller`, once
    /// admitted: the transform runs on a thread of its own and its answer comes back when the
    /// model stopped.
    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let address = &request.target.address;
        if address.resource != transform::RESOURCE_ID {
            return Err(format!(
                "the agent publishes no resource `{}`",
                address.resource
            ));
        }
        let actor = tilo_broker_core::BrokerActor::for_caller(caller);
        let Some(place) = self.running.begin(actor.actor()) else {
            return Ok(Pending::ready(Err(Box::new(tilo_actions::ActionFailure {
                code: transform::code::BUSY.to_owned(),
                message: "this agent is already running as many transforms as it allows".to_owned(),
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
        let asked_in = caller
            .workspace()
            .ok_or("the transform was asked in no workspace of this device")?;
        let workspace = WorkspaceId::parse(asked_in)
            .ok_or("the transform was asked in no workspace of this device")?;
        let content =
            ui::content::session(asked_in).unwrap_or_else(ui::content::ContentStore::memory);
        let argument = request.argument.to_slot();
        let call = transform::bind_wire(&address.action, argument.as_deref(), &content)
            .map_err(|failure| failure.message)?;
        let person = actor
            .direct_person()
            .or_else(|| actor.acting_for())
            .and_then(|person| person_of(person).ok());
        let runtime = self.runtime.clone();
        let (done, answer) = pending();
        let stop = Arc::clone(&admitted.stop);
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
        let mut model = PersonServices::of(self.workspaces).workspace_model(self.workspace, kind);
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
#[path = "transforms_tests.rs"]
mod tests;

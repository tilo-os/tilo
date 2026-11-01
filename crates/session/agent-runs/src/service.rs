//! The runs as resources: every run the session keeps as the root's reading,
//! and each run as its own, with its actions.
//!
//! The session publishes them to the apps it runs — the agent composer, a
//! run's window — and to an agent or a script through the same door. An ask
//! queues a run on [`wire::RESOURCE`]; a run is followed by reading
//! [`wire::run`] of it, steered by sending it a message, and ended by
//! stopping it. Nothing here draws.

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::{DoorError, ResourcePresentation, ResourceSpec};
use tilo_resource::Effect as _;
use tilo_service::{Service, Started, decode, offer, reading, unavailable};

use crate::{AgentRuns, LABEL, RUN_KIND, RUNS_KIND, RunAction, RunsAction, wire};

/// The runs, served: the session hosts one of these, and the broker reads the
/// same runs through the [`AgentRuns`] it holds.
#[derive(Clone)]
pub struct Served(Arc<AgentRuns>);

impl Served {
    /// Serve `runs`.
    #[must_use]
    pub fn new(runs: Arc<AgentRuns>) -> Self {
        Self(runs)
    }

    /// The runs it serves.
    #[must_use]
    pub fn runs(&self) -> &Arc<AgentRuns> {
        &self.0
    }

    /// Everything it publishes now.
    fn published(&self) -> Vec<ResourceSpec> {
        let runs = self.0.readings();
        let children = runs.iter().map(resource);
        vec![
            offer::<RunsAction>(ResourceSpec::new(wire::RESOURCE, RUNS_KIND, LABEL))
                .with_children(children),
        ]
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        self.published()
    }

    /// The root reads as every run's facts; a run as its own.
    fn read(&self, resource: &str) -> Option<Value> {
        if resource == wire::RESOURCE {
            return Some(reading(&wire::Runs {
                runs: self.0.readings(),
            }));
        }
        let id = resource.strip_prefix(wire::RUN_PREFIX)?;
        self.0.reading(id).map(|run| reading(&run))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        let published = self.published();
        if resource == wire::RESOURCE {
            let RunsAction::Ask {
                id,
                title,
                body,
                agent,
                model,
                subject,
                quick,
            } = decode::<RunsAction>(&published, resource, action, arguments)?;
            let asked = self.0.ask(crate::Ask {
                id,
                title,
                body,
                agent,
                model,
                subject,
                quick,
            })?;
            return Ok(Started::Done(tilo_actions::ActionOutput {
                value: Some(reading(&asked)),
                resources: vec![asked.run],
            }));
        }
        let Some(id) = resource.strip_prefix(wire::RUN_PREFIX) else {
            return Err(DoorError::Unknown(tilo_actions::ActionAddress::new(
                resource, action,
            )));
        };
        match decode::<RunAction>(&published, resource, action, arguments)? {
            RunAction::SendMessage { text } => {
                self.0.send_message(id, &text)?;
                Ok(Started::Done(tilo_actions::ActionOutput::default()))
            }
            RunAction::StopRun => {
                self.0.stop(id)?;
                Ok(Started::Done(tilo_actions::ActionOutput::default()))
            }
            RunAction::RecordStep { step } => {
                let step: tilo_reference::ProgramStep =
                    serde_json::from_value(step).map_err(|error| {
                        DoorError::InvalidArguments(format!("the step does not read: {error}"))
                    })?;
                self.0.record_step(id, step)?;
                Ok(Started::Done(tilo_actions::ActionOutput::default()))
            }
            RunAction::RecordAnswer { view } => {
                let answer: tilo_reference::ResourceAt =
                    serde_json::from_value(view).map_err(|error| {
                        DoorError::InvalidArguments(format!("the answer does not read: {error}"))
                    })?;
                self.0.record_answer(id, answer)?;
                Ok(Started::Done(tilo_actions::ActionOutput::default()))
            }
        }
    }

    /// Whether a run changed since the loop last took it in: what makes the
    /// readings be read again, and every follower see it.
    fn poll(&mut self) -> bool {
        self.0.took_landed()
    }
}

/// What phase a run is in, in a word: what a reader with no words of its own
/// shows as its badge.
const fn phase_label(phase: wire::Phase) -> &'static str {
    match phase {
        wire::Phase::Approved => "asked",
        wire::Phase::Accepted => "accepted",
        wire::Phase::Planning => "planning",
        wire::Phase::Executing => "working",
        wire::Phase::Waiting => "waiting",
        wire::Phase::Done => "done",
        wire::Phase::Failed => "failed",
        wire::Phase::Stopped => "stopped",
    }
}

/// A run's facts as its resource's presentation: its agent, and where it
/// stands as its badge.
fn present(run: &wire::Run) -> ResourcePresentation {
    ResourcePresentation {
        detail: Some(run.agent.clone()),
        badge: Some(phase_label(run.phase).to_owned()),
        preview: run
            .said
            .clone()
            .or_else(|| Some(run.instruction.clone()))
            .filter(|preview| !preview.is_empty()),
    }
}

/// The resource of one run, with its actions.
fn resource(run: &wire::Run) -> ResourceSpec {
    let mut resource = ResourceSpec::new(wire::run(&run.id), RUN_KIND, run.title.clone())
        .with_presentation(present(run));
    resource.actions.extend(RunAction::specs());
    if !run.phase.active() {
        for action in ["send-message", "stop-run"] {
            unavailable(
                &mut resource,
                action,
                "run-ended",
                "This run ended; ask again to start another",
            );
        }
    }
    resource
}

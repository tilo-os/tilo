//! The loop's `tilo` calls and view answers: each program is one step of the task, run through
//! the room's tilolang hosts and recorded on the task's issue ([`crate::programs`]).

use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::{Value, json};
use ui::tilo_reference::{ProgramStep, ResourceAt};

use super::{Handled, LoopState, StreamedCall, TaskActionStop};
use crate::programs::{self, Mounted, Naming};
use crate::session::Session;
use crate::tools;

/// The app a task's view answers are kept in until the person keeps or discards them.
pub(crate) const VIEWS_APP: &str = "so.tilo.lang.views";
/// The Views action that keeps a view answer.
const ANSWER_ACTION: &str = "answer";

/// What the loop knows of the room's tilolang apps for this task: the hosts it can run programs
/// on, which apps each one mounts, and which app declares each type.
#[derive(Debug, Default)]
pub(in crate::session) struct ProgramWorld {
    pub(in crate::session) mounted: Vec<Mounted>,
    mounts: BTreeMap<String, Vec<String>>,
    types: BTreeMap<String, String>,
    /// Every host's declarations, merged: what the first message's app lines are read from.
    catalogue: Vec<Value>,
}

impl ProgramWorld {
    /// The session's first-message section on the apps, empty when no tilolang app is open.
    pub(in crate::session) fn apps_section(&self) -> String {
        if self.mounted.is_empty() {
            return String::new();
        }
        let lines = programs::app_lines(&self.mounted, &Value::Array(self.catalogue.clone()));
        format!(
            "Apps your `{}` programs can use (an app not listed is not open; open it with its \
             open operation first):\n\n{lines}",
            programs::TILO
        )
    }
}

impl Session {
    /// Read the room's tilolang hosts once per task: which apps each mounts and what they
    /// declare. A host whose catalogue cannot be read still runs programs on its own app.
    pub(in crate::session) fn program_world(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        state: &mut LoopState,
    ) -> Result<()> {
        let catalogue = self.task_catalogue();
        let mounted = programs::mounted(&catalogue);
        let known: Vec<String> = state
            .programs
            .mounted
            .iter()
            .map(|m| m.app.clone())
            .collect();
        for app in &mounted {
            if known.contains(&app.app) {
                continue;
            }
            let Some(operation) = app.catalogue.clone() else {
                continue;
            };
            match self.host_call(run, state, &operation, serde_json::Map::new())? {
                Ok(Value::Array(signatures)) => {
                    let declared = Value::Array(signatures.clone());
                    state
                        .programs
                        .mounts
                        .insert(app.app.clone(), programs::mounts_of(&declared));
                    for (name, of) in programs::types_of(&declared) {
                        state.programs.types.entry(name).or_insert(of);
                    }
                    for signature in signatures {
                        if !state.programs.catalogue.contains(&signature) {
                            state.programs.catalogue.push(signature);
                        }
                    }
                }
                Ok(_) | Err(_) => {}
            }
        }
        state.programs.mounted = mounted;
        Ok(())
    }

    /// One host operation, checked and performed like any step of the task, answering its value
    /// or the host's refusal in words.
    fn host_call(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        state: &mut LoopState,
        operation: &str,
        parameters: serde_json::Map<String, Value>,
    ) -> Result<Result<Value, String>> {
        state.revealed.reveal(operation);
        let input = json!({ "operation": operation, "parameters": parameters });
        let prepared = match self.prepare_invoke(run, state, &input) {
            Ok(prepared) => prepared,
            Err(report) => {
                return Ok(Err(report
                    .error
                    .unwrap_or_else(|| "the operation is not available".into())));
            }
        };
        let index = state.completed;
        let result = self.run_task_step(run, index, &prepared.action, &prepared.arguments)?;
        state.step_index += 1;
        Ok(match result {
            Ok(outcome) => {
                state.completed += 1;
                Ok(outcome.output.value.unwrap_or(Value::Null))
            }
            Err(TaskActionStop::Failed(message)) => Err(message),
            Err(TaskActionStop::Cancelled(message)) => {
                Err(message.unwrap_or_else(|| "cancelled".into()))
            }
        })
    }

    /// A `tilo` call: run the program where it can run, record the step on the task's issue,
    /// and answer the model with what it answered.
    pub(super) fn run_program(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        state: &mut LoopState,
        call: &StreamedCall,
    ) -> Result<Handled> {
        let program = match programs::read_program(&call.input) {
            Ok(program) => program,
            Err(error) => return Ok(Handled::Result(error)),
        };
        self.program_world(run, state)?;
        if state.programs.mounted.is_empty() {
            return Ok(Handled::Result(
                "No tilolang app is open. Open the app you need with its open operation (see \
                 list_applications), then run the program."
                    .into(),
            ));
        }
        let mounted = state.programs.mounted.clone();
        let host = match programs::route(&program.source, &mounted, &state.programs.mounts) {
            Ok(host) => host.clone(),
            Err(error) => {
                let step = ProgramStep {
                    program: program.source.clone(),
                    outcome: ui::tilo_reference::StepOutcome::Rejected,
                    answer: vec![format!("// {error}")],
                    ..ProgramStep::default()
                };
                self.record_step(run, &step)?;
                return Ok(Handled::Result(programs::observation(&step)));
            }
        };
        let mut parameters = serde_json::Map::new();
        parameters.insert("source".into(), Value::String(program.source.clone()));
        let answered = self.host_call(run, state, &host.run_block, parameters)?;
        let types = state.programs.types.clone();
        let naming = Naming {
            host: &host,
            mounted: &mounted,
            types: &types,
        };
        let step = programs::step_of(
            &program.source,
            &naming,
            answered.as_ref().map_err(String::as_str),
        );
        self.record_step(run, &step)?;
        Ok(Handled::Result(programs::observation(&step)))
    }

    /// Keep the view a finishing `plan` answers with: checked by the Views host first (it must
    /// compile as a view over the apps it mounts), then kept by the Views app's `answer` action
    /// as the task's, and recorded on the task's issue, which opens it beside the subject.
    ///
    /// `Err` is the refusal the model reads instead of finishing.
    pub(super) fn keep_view_answer(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        state: &mut LoopState,
        view: &tools::ViewAnswer,
    ) -> Result<Result<(), String>> {
        self.program_world(run, state)?;
        let Some(views) = state
            .programs
            .mounted
            .iter()
            .find(|m| m.app == VIEWS_APP)
            .cloned()
        else {
            let catalogue = self.task_catalogue();
            let open = catalogue
                .offers
                .iter()
                .find(|offer| {
                    offer.app == VIEWS_APP
                        && offer.descriptor.resource_kind == crate::surface::APPLICATION_KIND
                })
                .map(crate::surface::Offer::id);
            return Ok(Err(match open {
                Some(open) => {
                    state.revealed.reveal(&open);
                    format!(
                        "Not finished: a view answer is kept in Views, which is not open. \
                         Invoke `{open}` to open it, then finish again with the same view."
                    )
                }
                None => "Not finished: Views is not installed, so this answer cannot be a \
                         view. Finish with the answer in `reply` instead."
                    .into(),
            }));
        };
        let mut parameters = serde_json::Map::new();
        parameters.insert("source".into(), Value::String(view.source.clone()));
        match self.host_call(run, state, &views.run_block, parameters)? {
            Ok(answer) if answer.get("kind").and_then(Value::as_str) == Some("view") => {}
            Ok(_) => {
                return Ok(Err(
                    "Not finished: `view.source` is not a view (it is a value or an action). \
                     Write a view block, such as `column { ... }`."
                        .into(),
                ));
            }
            Err(error) => return Ok(Err(format!("Not finished: the view {error}"))),
        }
        let catalogue = self.task_catalogue();
        let Some(answer) = catalogue
            .offers
            .iter()
            .find(|offer| {
                offer.app == VIEWS_APP && offer.descriptor.address.action == ANSWER_ACTION
            })
            .map(crate::surface::Offer::id)
        else {
            return Ok(Err(
                "Not finished: Views does not offer to keep an answer here. Finish with the \
                 answer in `reply` instead."
                    .into(),
            ));
        };
        let mut parameters = serde_json::Map::new();
        parameters.insert("title".into(), Value::String(view.title.clone()));
        parameters.insert(
            "question".into(),
            Value::String(run.instruction().trim().to_owned()),
        );
        parameters.insert("source".into(), Value::String(view.source.clone()));
        let kept = match self.host_call(run, state, &answer, parameters)? {
            Ok(kept) => kept,
            Err(error) => {
                return Ok(Err(format!(
                    "Not finished: keeping the view failed: {error}"
                )));
            }
        };
        let types = state.programs.types.clone();
        let mounted = state.programs.mounted.clone();
        let naming = Naming {
            host: &views,
            mounted: &mounted,
            types: &types,
        };
        let Some(resource) = kept_resource(&kept, &naming, &view.title) else {
            return Ok(Err(
                "Not finished: Views kept the view but did not say where; finish again.".into(),
            ));
        };
        self.record_answer(run, &resource)?;
        Ok(Ok(()))
    }
}

/// The resource the Views `answer` action answered with, titled as the view is.
fn kept_resource(kept: &Value, naming: &Naming<'_>, title: &str) -> Option<ResourceAt> {
    let step = programs::step_of("", naming, Ok(&json!({ "kind": "data", "value": kept })));
    let mut resource = step.refs.into_iter().next()?;
    title.clone_into(&mut resource.reference.label);
    Some(resource)
}

/// Where the task is, said once in its first message (agents.md, "What the agent knows from the
/// start"): who it acts for, its limits, what `this` is when the person pointed at a tilolang
/// resource, and the apps its programs can use.
pub(super) fn where_section(
    run: &runtime_collab::TaskRunEnvelope,
    world: &ProgramWorld,
    most_steps: usize,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let context =
        serde_json::from_value::<tilo_actions::ActionContext>(run.action_context.clone()).ok();
    if let Some(person) = context
        .as_ref()
        .and_then(|context| context.invoker.as_ref())
        .and_then(|invoker| invoker.on_behalf_of.as_deref())
    {
        let _ = writeln!(out, "You act for {person}.");
    }
    let seconds = run
        .execution_budget
        .map_or(super::MAX_LOOP_TIME.as_secs(), |budget| {
            u64::from(budget.total_ms) / 1000
        });
    let _ = writeln!(
        out,
        "Limits: at most {most_steps} steps and {seconds} seconds for the whole task."
    );
    if let Some(this) = run
        .subject
        .as_ref()
        .and_then(|subject| this_literal(subject, world))
    {
        let _ = writeln!(out, "`this` is {this}: what the person pointed at.");
    }
    let apps = world.apps_section();
    if !apps.is_empty() {
        out.push('\n');
        out.push_str(&apps);
    }
    out
}

/// The subject as a program names it, when it is a tilolang app's resource: `@board.Board:root`
/// for the resource `tl:so.tilo.lang.board:root` of kind `tl.so.tilo.lang.board.Board`.
fn this_literal(subject: &tilo_actions::SelectionSubject, world: &ProgramWorld) -> Option<String> {
    let app = world.mounted.iter().find(|m| m.app == subject.app_id)?;
    let id = subject
        .resource
        .id
        .strip_prefix(&format!("tl:{}:", app.app))?;
    let type_name = subject
        .resource
        .kind
        .strip_prefix(&format!("tl.{}.", app.app))?;
    Some(format!("@{}.{type_name}:{id}", app.alias))
}

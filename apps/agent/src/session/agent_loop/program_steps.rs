//! The loop's `tilo` calls and view answers: each program is one step of the task, run by the
//! session's api as a step of the task's run and recorded on the task's issue
//! ([`crate::programs`]).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::Result;
use serde_json::{Value, json};
use ui::tilo_reference::ResourceAt;

use super::{Handled, LoopState, StreamedCall, TaskActionStop};
use crate::programs::{self, Mounted, Naming};
use crate::session::{Asked, Session};
use crate::tools;
use tilo_actions::remote::task_programs;

/// What the model reads after a program that called an app's actions and changed nothing, in
/// place of the reply it wrote for a change: nothing it looked for was there.
const CHANGED_NOTHING: &str = "\nIt changed nothing: no record matched what the program looked \
     for, so your reply was not shown. Find what the person means another way, or tell them it \
     is not there.";

/// The app that keeps each person's own views, in their own record: a task's view answer is
/// one of them, until the person keeps or discards it.
pub(crate) const VIEWS_APP: &str = "so.tilo.lang.views";
/// The Views action that keeps a view answer.
const ANSWER_ACTION: &str = "answer";

/// What the loop knows of the workspace's tilolang apps for this task: the apps its programs can use,
/// and which app declares each type.
#[derive(Debug, Default)]
pub(in crate::session) struct ProgramWorld {
    pub(in crate::session) mounted: Vec<Mounted>,
    types: BTreeMap<String, String>,
    /// Every host's declarations, merged: what the first message's app lines are read from.
    catalogue: Vec<Value>,
    /// The apps whose records the session serves, by alias, with their root's type, when the
    /// session's api runs programs: what a program names without a window of the app open.
    served: Vec<(String, String)>,
    /// What each served app declares, compacted ([`programs::compact_declarations`]), by alias:
    /// what the model writes its programs from without asking `describe` first.
    declared: BTreeMap<String, String>,
}

impl ProgramWorld {
    /// Whether `source` calls one of the actions the apps declare (`tasks.check(`): a program
    /// meant to change something, rather than only to read.
    pub(in crate::session) fn calls_an_action(&self, source: &str) -> bool {
        self.declared
            .values()
            .flat_map(|declared| declared.lines())
            .filter_map(|line| line.split_once("action ").map(|(_, rest)| rest))
            .filter_map(|rest| rest.split_once('(').map(|(name, _)| name.trim()))
            .any(|name| !name.is_empty() && source.contains(&format!("{name}(")))
    }

    /// The session's first-message section on the apps, empty when no program can run.
    pub(in crate::session) fn apps_section(&self) -> String {
        if self.mounted.is_empty() {
            if self.served.is_empty() {
                return String::new();
            }
            let mut lines = String::new();
            for (alias, root) in &self.served {
                let _ = writeln!(lines, "- `{alias}`: its record is a `{alias}.{root}`");
                if let Some(declared) = self.declared.get(alias).filter(|d| !d.is_empty()) {
                    for line in declared.lines() {
                        let _ = writeln!(lines, "    {line}");
                    }
                }
            }
            return format!(
                "Apps your `{}` programs can use, open or not (`find({{alias}}.{{Type}})` finds \
                 their records, `describe({{alias}})` lists an app's types with their fields, \
                 filters and actions; a program answers with references you can place):\n\n{lines}",
                programs::TILO
            );
        }
        let lines = programs::app_lines(&self.mounted, &Value::Array(self.catalogue.clone()));
        format!(
            "Apps your `{}` programs can use (an app not listed is not open; open it with its \
             open operation first):\n\n{lines}",
            programs::TILO
        )
    }
}

/// The apps whose records the session serves (`tl:<app>:<root>`, of kind `tl.<app>.<Type>`), by
/// alias with their root's type, when the session's api runs programs: none without the API.
fn served_apps(catalogue: &crate::surface::Catalogue) -> Vec<(String, String)> {
    let api = catalogue.offers.iter().any(|offer| {
        offer.descriptor.address.resource == task_programs::RESOURCE
            && offer.descriptor.address.action == task_programs::RUN
    });
    if !api {
        return Vec::new();
    }
    let mut served: Vec<(String, String)> = Vec::new();
    for offer in &catalogue.offers {
        let resource = offer.descriptor.address.resource.as_str();
        let Some((app, root)) = resource
            .strip_prefix("tl:")
            .and_then(|rest| rest.split_once(':'))
        else {
            continue;
        };
        if root.contains('/') {
            continue;
        }
        let Some(ty) = offer
            .descriptor
            .resource_kind
            .strip_prefix(&format!("tl.{app}."))
        else {
            continue;
        };
        let alias = programs::alias_of(app).to_owned();
        if !served.iter().any(|(seen, _)| *seen == alias) {
            served.push((alias, ty.to_owned()));
        }
    }
    served
}

impl Session {
    /// Read the workspace's tilolang hosts once per task: what each declares, for the first message's
    /// app lines.
    pub(in crate::session) fn program_world(
        &mut self,
        run: &Asked,
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
            if let Ok(Value::Array(signatures)) =
                self.host_call_uncounted(run, state, &operation, &serde_json::Map::new())?
            {
                let declared = Value::Array(signatures.clone());
                for (name, of) in programs::types_of(&declared) {
                    state.programs.types.entry(name).or_insert(of);
                }
                for signature in signatures {
                    if !state.programs.catalogue.contains(&signature) {
                        state.programs.catalogue.push(signature);
                    }
                }
            }
        }
        state.programs.mounted = mounted;
        state.programs.served = served_apps(&catalogue);
        self.describe_served(run, state, &catalogue)?;
        Ok(())
    }

    /// Read what each served app declares, once per app for the agent's life, through the API's
    /// `describe`, and give the task its compacted declarations: the first turn then carries every
    /// app's types and actions with their arguments, and a request is answered by a program
    /// written straight away rather than after a `describe` turn.
    fn describe_served(
        &mut self,
        run: &Asked,
        state: &mut LoopState,
        catalogue: &crate::surface::Catalogue,
    ) -> Result<()> {
        let Some(api) = catalogue
            .offers
            .iter()
            .find(|offer| {
                offer.descriptor.address.resource == task_programs::RESOURCE
                    && offer.descriptor.address.action == task_programs::RUN
            })
            .map(crate::surface::Offer::id)
        else {
            return Ok(());
        };
        let aliases: Vec<String> = state
            .programs
            .served
            .iter()
            .map(|(alias, _)| alias.clone())
            .collect();
        for alias in aliases {
            if !self.declarations.contains_key(&alias) {
                let mut parameters = serde_json::Map::new();
                parameters.insert(
                    "program".into(),
                    Value::String(format!("describe({alias})")),
                );
                let described = self
                    .host_call_uncounted(run, state, &api, &parameters)?
                    .ok()
                    .and_then(|value| {
                        serde_json::from_value::<task_programs::ProgramAnswer>(value).ok()
                    })
                    .filter(|answer| !answer.failed)
                    .map(|answer| programs::compact_declarations(&answer.text))
                    .unwrap_or_default();
                self.declarations.insert(alias.clone(), described);
            }
            if let Some(declared) = self.declarations.get(&alias) {
                state.programs.declared.insert(alias, declared.clone());
            }
        }
        Ok(())
    }

    /// One host operation, checked and performed like any step of the task, answering its value
    /// or the host's refusal in words.
    fn host_call(
        &mut self,
        run: &Asked,
        state: &mut LoopState,
        operation: &str,
        parameters: &serde_json::Map<String, Value>,
    ) -> Result<Result<Value, String>> {
        let answer = self.host_call_uncounted(run, state, operation, parameters)?;
        state.step_index += 1;
        if answer.is_ok() {
            state.completed += 1;
        }
        Ok(answer)
    }

    /// [`Self::host_call`] for what the task reads to know its world (an app's declarations),
    /// which is no step the run took: it counts as neither started nor done.
    fn host_call_uncounted(
        &mut self,
        run: &Asked,
        state: &mut LoopState,
        operation: &str,
        parameters: &serde_json::Map<String, Value>,
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
        Ok(match result {
            Ok(outcome) => Ok(outcome.output.value.unwrap_or(Value::Null)),
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
        run: &Asked,
        state: &mut LoopState,
        call: &StreamedCall,
    ) -> Result<Handled> {
        let program = match programs::read_program(&call.input) {
            Ok(program) => program,
            Err(error) => return Ok(Handled::Result(error)),
        };
        // The session's api runs the program in the task's own tilolang host, over every app
        // the session runs: no window of an app need be open for a program to read or change its
        // records. What the open apps' hosts declare only describes them better.
        self.program_world(run, state)?;
        let catalogue = self.task_catalogue();
        let Some(api) = catalogue.offers.iter().find(|offer| {
            offer.descriptor.address.resource == task_programs::RESOURCE
                && offer.descriptor.address.action == task_programs::RUN
        }) else {
            return Ok(Handled::Result(
                "This session runs no programs: its api does not offer them. Use the other \
                 tools instead."
                    .into(),
            ));
        };
        let mut parameters = serde_json::Map::new();
        parameters.insert("program".into(), Value::String(program.source.clone()));
        parameters.insert("confirm".into(), Value::Bool(program.confirm));
        if let Some(this) = run
            .subject
            .as_ref()
            .and_then(|subject| this_literal(subject, &state.programs))
        {
            parameters.insert("this".into(), Value::String(this));
        }
        let answered = match self.host_call(run, state, &api.id(), &parameters)? {
            Ok(value) => serde_json::from_value::<task_programs::ProgramAnswer>(value)
                .unwrap_or_else(|error| task_programs::ProgramAnswer {
                    text: format!("error: the API's answer was unreadable: {error}"),
                    failed: true,
                    ..task_programs::ProgramAnswer::default()
                }),
            Err(error) => task_programs::ProgramAnswer {
                text: format!("error: {error}"),
                failed: true,
                ..task_programs::ProgramAnswer::default()
            },
        };
        let step = programs::answer_step(&program.source, &answered, &state.programs.mounted);
        state.program_did(&step);
        self.record_program(run, &step)?;
        // The reply the model wrote with the program is the task's answer once the program has
        // run to its end: one response, both the steps and the words. A program that failed,
        // stopped for a preview, or waits on the person is read by the model again instead.
        // A program that calls an app's actions and changed nothing did not do what its reply
        // says it did (its `find` matched nothing): the reply is held back and the model reads
        // that, rather than the person reading a change that never happened.
        let changed = answered
            .answer
            .as_ref()
            .and_then(|answer| answer.get("changed"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let changed_nothing = changed == 0 && state.programs.calls_an_action(&program.source);
        let mut text = answered.text;
        let ran = !answered.failed && step.outcome == ui::tilo_reference::StepOutcome::Done;
        if ran && changed_nothing && program.reply.is_some() {
            text.push_str(CHANGED_NOTHING);
        }
        let reply = program
            .reply
            .as_deref()
            .filter(|_| ran && !changed_nothing)
            .map(|reply| programs::reply_with_answer(reply, &text))
            .and_then(|reply| tools::spoken_reply(&reply));
        if let Some(reply) = &reply {
            state.reply = Some(reply.clone());
        }
        Ok(match reply {
            Some(reply) => Handled::ResultFinish(
                text,
                tools::Finish {
                    reply: Some(reply),
                    done: true,
                    view: None,
                },
            ),
            None => Handled::Result(text),
        })
    }

    /// Keep the view a finishing `plan` answers with: checked by the Views host first (it must
    /// be one `view` declaration that compiles against the apps the workspace has installed),
    /// then kept among the person's own views by Views' `answer` action, written by the task,
    /// and recorded on the task's issue, which opens it beside the subject.
    ///
    /// `Err` is the refusal the model reads instead of finishing.
    pub(super) fn keep_view_answer(
        &mut self,
        run: &Asked,
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
        let shows = match self.host_call(run, state, &views.run_block, &parameters)? {
            Ok(answer) => match answer.get("definition") {
                Some(definition) if answer.get("kind").and_then(Value::as_str) == Some("view") => {
                    definition
                        .get("shows")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                }
                _ => {
                    return Ok(Err(
                        "Not finished: `view.source` is not a `view` declaration. Write one, \
                         as an app declares a view: `view left() { stack { ... } }`."
                            .into(),
                    ));
                }
            },
            Err(error) => return Ok(Err(format!("Not finished: the view {error}"))),
        };
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
            Value::String(run.instruction.trim().to_owned()),
        );
        parameters.insert("definition".into(), Value::String(view.source.clone()));
        parameters.insert("shows".into(), Value::String(shows));
        let kept = match self.host_call(run, state, &answer, &parameters)? {
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
        if kept_resource(&kept, &naming, &view.title).is_none() {
            return Ok(Err(
                "Not finished: Views kept the view but did not say where; finish again.".into(),
            ));
        }
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
/// start"): who it acts for, what `this` is when the person pointed at a tilolang resource, and
/// the apps its programs can use.
pub(super) fn where_section(run: &Asked, world: &ProgramWorld) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if let Some(person) = run
        .context
        .as_ref()
        .and_then(|context| context.invoker.as_ref())
        .and_then(|invoker| invoker.acting.acted_for_person())
    {
        let _ = writeln!(out, "You act for {person}.");
    }
    if let Some(subject) = run
        .subject
        .as_ref()
        .filter(|subject| this_literal(subject, world).is_some())
    {
        let _ = writeln!(
            out,
            "`this` is what the person pointed at: {}.",
            subject.resource.label
        );
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

#[cfg(test)]
mod tests {
    /// A program that calls an app's declared action means to change something; one that only
    /// finds and prints does not. "Move the reminder to Friday" whose `find` matched nothing
    /// changed nothing, and its reply ("Moved …") is held back rather than told.
    #[test]
    fn a_program_that_calls_an_action_is_known_from_its_source() {
        let mut world = super::ProgramWorld::default();
        world.declared.insert(
            "tasks".into(),
            "type Task { title: Text }\naction tasks.check(t: Task, done: Bool)\nprimary action \
             tasks.new_task(tasks: Tasks, title: Text) -> Task\n"
                .into(),
        );
        assert!(world.calls_an_action(
            "for t in find(tasks.Task).filter(t -> t.title == \"x\") { tasks.check(t, done: true) }"
        ));
        assert!(world.calls_an_action("tasks.new_task(tasks.tasks, title: \"x\")"));
        assert!(!world.calls_an_action("for t in find(tasks.Task) { print(t.title) }"));
    }
}

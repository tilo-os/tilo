//! The conversation on the canvas: the ways into a run's conversation (shell.md §8.1), answered
//! by the session rather than by whoever draws the canvas.
//!
//! The conversation's one input takes a line, and a line is one of two things. One that parses
//! as a tilolang program is a program: it runs as the person, as one `tilo` call on the run's
//! door session, taking the `let`s the agent's own calls bound, and shows in the conversation as
//! a step of theirs. Anything else is a sentence to the agent, taken in by the run.
//!
//! A question typed on the canvas starts a run and stands its conversation where the person
//! typed: one `ask` and one arranging `place`, both the person's, so the place stands on their
//! record beside the words that started it.
//!
//! Everything here is reached through the arrangement's own action (`shell:arrangement`'s
//! [`ACTION`]), as one attributed step, exactly as a press on the canvas entry is. The view
//! calls it; it does not reach the runs or the arrangement itself.

use std::sync::Arc;

use serde_json::Value as Json;
use tilo_actions::{ActionArgs, ActionParam, ActionSpec, OperationKind, ParamType};
use tilo_agent_runs::{AgentRuns, wire};
use tilo_reference::{ProgramStep, StepOutcome};

use crate::Plane;
use crate::agent_door;
use crate::arrangement;
use crate::run_programs;

impl crate::Shared {
    /// The door that decides what a person's line in a run's conversation reads and changes
    /// (shell.md §8.1): each line runs as the person there, in the run's own program host.
    pub(crate) fn lend_conversations(&self, plane: Option<&Plane>, front: &str) {
        // The starter a run's host begins with, as the agent's own `tilo` call uses it: the
        // person's line starts the host itself when it is the run's first program.
        self.conversation
            .lend_starter(std::sync::Arc::new(agent_door::TaskHosts::new(
                &self.person,
                std::sync::Arc::clone(&self.run_programs),
                std::sync::Arc::clone(&self.session_doors),
            )));
        // The door's host, over the broker when this session runs one and the session's own
        // doors alone otherwise: a line that reads nothing still runs, and one that reads an app
        // says it cannot here, as the agent's own call would.
        let broker = plane.and_then(Plane::agent_broker);
        self.conversation.lend(
            front,
            std::sync::Arc::new(agent_door::Host::new(
                broker,
                std::sync::Arc::clone(&self.session_doors),
            )),
        );
    }
}

/// The action the conversation's input runs: `shell:arrangement`'s, beside its arranging ones.
pub const ACTION: &str = "conversation";

/// The action's parameter: the line as the person typed it.
pub const LINE: &str = "line";
/// The action's parameter: the run it is in, by its id; a new run without.
pub const RUN: &str = "run";
/// The action's parameter: where a new run's conversation stands, in canvas pixels.
pub const X: &str = "x";
/// See [`X`].
pub const Y: &str = "y";

/// The agent a sentence typed at this session is asked of: the workspace's own agent, as the
/// composer's default is when none is here to name.
pub const AGENT: &str = "default";

/// Whether `line` parses as a tilolang program: the statements an agent's program is made of.
/// Only the parse is asked, never the types: a line that names an app or a field the session
/// does not have is still the person asking for a program, and its diagnostics are its own step
/// in the conversation, as the agent's would be.
///
/// A sentence must not read as one: words strung together ("tidy the launch notes") are no
/// single expression the parser accepts, and a lone name is no program either (it would run as
/// reading a name and print nothing). So a program is a line whose every statement does
/// something: a `let`, an assignment, a `for`, a `return`, or a call.
#[must_use]
pub fn parses(line: &str) -> bool {
    use tilolang_tilo::tilolang::ast::{ExprKind, StmtKind};
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    let (stmts, diagnostics) = tilolang_tilo::tilolang::parser::parse_program_source(line);
    if !diagnostics.is_empty() || stmts.is_empty() {
        return false;
    }
    stmts.iter().all(|stmt| match &stmt.kind {
        // A bare name reads a name and prints nothing: a sentence, not a program.
        StmtKind::Expr(expr) => !matches!(expr.kind, ExprKind::Ident(_)),
        _ => true,
    })
}

/// What the person typed, `line`, means in the run `run`: a program when `run` names a going
/// run and the line parses, else a sentence.
#[must_use]
pub fn is_program(line: &str, run: &str) -> bool {
    !run.is_empty() && parses(line)
}

/// The `conversation` action, as the arrangement publishes it: the one input of shell.md §8.1.
#[expect(
    clippy::expect_used,
    reason = "a typed operation always serializes to JSON"
)]
#[must_use]
pub fn action() -> ActionSpec {
    let mut spec = ActionSpec::typed_operation(
        ACTION,
        "Type to the agent".to_owned(),
        String::new(),
        OperationKind::Effect,
        arrangement::OPERATION,
        vec![
            ActionParam::required_owned(
                LINE,
                ParamType::Text,
                "A tilolang program, or a sentence to the agent".to_owned(),
            ),
            ActionParam::optional_owned(
                RUN,
                ParamType::Text,
                "The run it is in, by its id; a new run without".to_owned(),
            ),
            ActionParam::optional_owned(X, ParamType::Integer, "Its canvas x".to_owned()),
            ActionParam::optional_owned(Y, ParamType::Integer, "Its canvas y".to_owned()),
        ],
        |_| {
            tilo_actions::EncodedAction::encode_operation(arrangement::OPERATION, &Json::Null)
                .expect("a typed operation always serializes")
        },
    );
    spec.type_mut().contract.execution = tilo_actions::ExecutionMode::Immediate;
    spec
}

/// What the action asked: the line, the run, and where a new run's conversation stands.
#[must_use]
pub fn asked(args: &ActionArgs) -> (String, String, (i64, i64)) {
    (
        args.text(LINE).unwrap_or_default().trim().to_owned(),
        args.text(RUN).unwrap_or_default().to_owned(),
        (
            args.integer(X).unwrap_or_default(),
            args.integer(Y).unwrap_or_default(),
        ),
    )
}

/// The run a resource or a name `run` names: `agent-run:<id>`, or the bare id, as its own.
#[must_use]
pub fn run_id(run: &str) -> &str {
    run.strip_prefix(wire::RUN_PREFIX).unwrap_or(run)
}

/// The resource `run` names, `agent-run:<id>`, as the arrangement places a run.
#[must_use]
pub fn run_resource(run: &str) -> String {
    wire::run(run_id(run))
}

/// The session's conversations, as the arrangement reaches them: the runs people ask its agents
/// for, and the program host each run's lines run in.
pub struct Conversation {
    runs: Arc<AgentRuns>,
    programs: run_programs::Shared,
    /// Who sits here.
    person: String,
}

impl Conversation {
    /// The conversations over `runs`, whose programs are `programs`, for `person`.
    #[must_use]
    pub fn new(runs: Arc<AgentRuns>, programs: run_programs::Shared, person: &str) -> Self {
        Self {
            runs,
            programs,
            person: person.to_owned(),
        }
    }

    /// A sentence to the agent with no run yet: it starts one, whose resource this returns
    /// (`agent-run:<id>`). The run's conversation stands where the person typed, by the
    /// arrangement's own `place`.
    ///
    /// # Errors
    /// The runs refuse the ask (no workspace in front, no person).
    pub fn ask(&self, line: &str) -> Result<String, String> {
        let asked = self
            .runs
            .ask(tilo_agent_runs::Ask {
                id: None,
                title: line.to_owned(),
                body: None,
                agent: AGENT.to_owned(),
                model: None,
                subject: None,
                quick: None,
            })
            .map_err(|error| format!("{error:?}"))?;
        Ok(asked.run)
    }

    /// A sentence to run `run`: its agent takes it in as it goes.
    ///
    /// # Errors
    /// No such run, or it ended.
    pub fn tell(&self, run: &str, line: &str) -> Result<(), String> {
        self.runs
            .send_message(run_id(run), line)
            .map_err(|error| format!("{error:?}"))
    }

    /// A program typed in run `run`'s conversation, run as `person` in the run's own program
    /// host: started now, its step recorded on the run when it ends
    /// ([`Conversation::take_finished`]).
    ///
    /// # Errors
    /// The run's program host does not run yet: the agent's own first `tilo` call starts it.
    pub fn say(&self, person: &str, run: &str, line: &str) -> Result<(), String> {
        let id = run_id(run);
        if !self.programs.start(id, person, line) {
            return Err(format!(
                "the run's program host does not run yet: ask the agent first, and its first \
                 program starts it"
            ));
        }
        Ok(())
    }

    /// What the lines that ended since the last pass made, each recorded on its run as the
    /// person's step. What could not run at all is answered as a step that failed, so the
    /// conversation says why where the person typed.
    pub fn take_finished(&self) {
        for finished in self.programs.take_finished() {
            let step = match finished.step {
                Ok(ran) => step_of(&finished.line, &ran),
                Err(why) => ProgramStep {
                    program: finished.line.clone(),
                    outcome: StepOutcome::Failed,
                    answer: vec![format!("error: {why}")],
                    ..ProgramStep::default()
                },
            };
            if let Err(error) = self
                .runs
                .record_step_typed(&finished.run, step, &finished.person)
            {
                tracing::debug!(%error, run = finished.run, "a conversation's step was not kept");
            }
        }
    }

    /// Who sits here.
    #[must_use]
    pub fn person(&self) -> &str {
        &self.person
    }

    /// The runs' program connections, for the door's own side of them.
    #[must_use]
    pub fn programs(&self) -> &run_programs::Shared {
        &self.programs
    }

    /// Where a run's host is started, so a person's line can start it when it is the run's
    /// first program: the door's own starter, as the agent's `tilo` call uses it.
    pub fn lend_starter(&self, hosts: std::sync::Arc<dyn tilo_runtime::mcp::TaskHosts>) {
        self.programs.lend_starter(hosts);
    }

    /// The door a line acts through: the workspace it runs in, and the host that decides what
    /// it reads and changes. Lent every pass, so a workspace that moves is the one in front.
    pub fn lend(
        &self,
        workspace: &str,
        host: std::sync::Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>,
    ) {
        self.programs.lend(workspace, host);
    }
}

/// A step's outcome, from what a task host answered, as `apps/agent`'s `door_step` reads it:
/// `state` one of `done`, `preview`, `suspended`, `rejected`, `failed`.
#[must_use]
pub fn outcome_of(answer: &Json) -> StepOutcome {
    match answer.get("state").and_then(Json::as_str) {
        Some("done") | None => StepOutcome::Done,
        Some("preview") => StepOutcome::Preview,
        Some("suspended") => StepOutcome::Asked,
        Some("rejected") => StepOutcome::Rejected,
        _ => StepOutcome::Failed,
    }
}

/// The step a person's program made, as the run records it: the program as it ran, how it
/// ended, what it answered line by line, and the resources it printed as references.
#[must_use]
pub fn step_of(line: &str, ran: &tilo_runtime::mcp::PersonStep) -> ProgramStep {
    let mut step = ProgramStep {
        program: line.to_owned(),
        outcome: outcome_of(&ran.answer),
        answer: ran.text.lines().map(str::to_owned).collect(),
        ..ProgramStep::default()
    };
    for literal in tilo_runtime::mcp::literals_in(&ran.text) {
        if let Some(resource) = resource_of(&literal, &ran.text) {
            step.refs.push(resource);
        }
    }
    step.bounded()
}

/// The resource a literal (`@notes.Note:root/notes:n2`) names, labelled as the answer shows it
/// (its `"Packing list"` after it), or by its type when it shows none.
#[must_use]
pub fn resource_of(literal: &str, text: &str) -> Option<tilo_reference::ResourceAt> {
    let (path, id) = literal.strip_prefix('@')?.split_once(':')?;
    let (app, type_name) = path.rsplit_once('.')?;
    let shown = text
        .lines()
        .find_map(|line| {
            let at = line.find(literal)?;
            let after = line.get(at.saturating_add(literal.len())..)?.trim_start();
            after
                .strip_prefix('"')
                .and_then(|rest| rest.split_once('"'))
        })
        .map_or_else(|| type_name.to_owned(), |(label, _)| label.to_owned());
    Some(tilo_reference::ResourceAt {
        reference: tilo_reference::ResourceRef::whole(
            format!("tl.{app}.{type_name}"),
            format!("tl:{app}:{id}"),
            shown,
        ),
        app: app.to_owned(),
        record: None,
    })
}

#[cfg(test)]
mod tests {
    use super::parses;

    #[test]
    fn a_sentence_is_no_program_and_a_program_is() {
        for line in [
            "1 + 1",
            "find(notes.Note)",
            "session.place(resource: host.Resource, x: 1)",
            "let n = find(notes.Note)",
        ] {
            assert!(parses(line), "{line:?} parses as a program");
        }
        for line in [
            "tidy the launch notes",
            "make a table of these by due date",
            "hello",
            "what is on my mind",
        ] {
            assert!(!parses(line), "{line:?} is a sentence");
        }
    }
}

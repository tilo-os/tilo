//! A task's programs: the tilolang an agent writes as one step of its session, or a person runs
//! from their command line, run on the device the call reached, in the same API an MCP client
//! reaches for its `tilo` tool (`docs/designs/app-language/agents.md`). A caller reaches it through
//! the device's dispatch under
//! [`task_programs::PROVIDER`](tilo_actions::remote::task_programs::PROVIDER) and gets back what
//! the program answered: an agent here, one whose session Tilo's API forwards to this device, the
//! person's own command line, and a desktop session's agents alike.
//!
//! One agent session has one session of the API: its own tilolang host, the `let`s and references
//! its programs made, and the steps `undo` takes back. Every call the program makes is done as the
//! agent, for the person it acts for, attributed to the session, and decided by the workspace's
//! policy like the agent's own calls. A person's program runs as them, in the program session
//! their client names, so one client's `let`s go on from one program to the next and another
//! client's never mix in.
//!
//! An agent's program that would change much or destroy something waits for its person's yes:
//! the step runs it provisionally first, and a preview that asks refuses the call with what its
//! person is shown ([`asked::approved_or_asks`]), which the dispatch keeps as its asked record. An
//! approve brings the same call back carrying the yes, and the program runs for real in the same
//! session, its earlier `let`s still there.
//!
//! Where a program runs is the host's ([`Programs`]): a desktop session's own task hosts, or the
//! device daemon's processes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

use tilo_actions::WorkGroup;
use tilo_actions::remote::Admits;
use tilo_actions::remote::admitted::top_level;
use tilo_actions::remote::asked::{self, AskedBy, AskedFacts, Fact, Named};
use tilo_actions::remote::task_programs::{self as task_wire, Go};
use tilo_actions::remote::{ActionCallError, Admitted, Caller, InvokeRequest, Outcome, Pending};
use tilo_actions::{ActionArgs, ActionParam, ParamType};

use super::providers::{Providers, Reach};

/// Who a program runs as.
#[derive(Clone, Copy)]
pub enum ProgramCaller<'a> {
    /// An agent's program: a step of its session, its work the session's.
    Agent {
        /// The agent whose program it is, by its id without the `agent:` prefix.
        agent: &'a str,
        /// The person the agent acts for, when one delegated it: `me` in the program.
        person: Option<&'a str>,
        /// The agent session the program is a step of.
        session: &'a str,
    },
    /// The person's own program: every call it makes is theirs, and its work is their own.
    Person {
        /// The person at this device.
        person: &'a str,
        /// The program session their client continues, when it names one: its `let`s and the
        /// references its answers showed.
        session: Option<&'a str>,
    },
}

/// One program the person or an agent asked for, as the step that carries it.
pub struct ProgramRequest<'a> {
    /// The workspace the session works in.
    pub workspace: &'a str,
    /// The API the program's calls go through, when its caller brings one: an agent session's
    /// turn does. The person's program runs through the host's own API for them instead.
    pub provider: Option<Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>>,
    pub caller: ProgramCaller<'a>,
    /// The session's work, as history names it.
    pub group: WorkGroup,
    pub program: String,
    /// Whether it runs for real or provisionally.
    pub go: Go,
    /// What the person pointed at, as a resource literal.
    pub this: Option<String>,
    /// Set when the person stopped the run: the program makes no further call, and the run
    /// answers as stopped.
    pub stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// The run of the connection that asked, when it has one: a host may end the program session
    /// once it has ended.
    pub asked_in: Option<tilo_actions::remote::ExecutionLease>,
}

/// The programs this host runs, and how it runs them.
pub trait Programs: Send + Sync {
    /// Run `request`'s program and answer what it said, off the caller's pass: the program
    /// waits on its host and on every call it makes.
    fn run(&self, request: ProgramRequest<'_>) -> Result<Pending<Outcome>, String>;
}

/// What an agent's program's preview found, kept for the call it was made for until that call
/// is taken.
enum Previewed {
    /// It changed nothing and has answered: the call's answer.
    Answered(Outcome),
    /// It changes what its person need not be asked about: it runs.
    Runs,
}

/// How many previews are kept for calls not taken yet; a call the dispatch dropped after its
/// preview leaves its own, and these go once there are more.
const PREVIEWS_KEPT: usize = 256;

/// A task's programs, as this host runs them.
pub struct TaskPrograms {
    /// Where a program runs: the host's.
    programs: Arc<dyn Programs>,
    /// The language the step is described in: the person's.
    language: fn() -> Option<String>,
    /// What each agent call's preview found, by its invocation.
    previewed: Arc<Mutex<HashMap<tilo_actions::InvocationId, Previewed>>>,
}

impl TaskPrograms {
    /// The programs `programs` runs, described in the language `language` reads.
    #[must_use]
    pub fn new(programs: Arc<dyn Programs>, language: fn() -> Option<String>) -> Self {
        Self {
            programs,
            language,
            previewed: Arc::default(),
        }
    }

    /// Run the programs of the agent sessions and the person this device admits from now on,
    /// listed to the agent that works in a session.
    pub fn register(self: Arc<Self>, providers: &Providers) {
        providers.provide(
            task_wire::PROVIDER,
            task_wire::APP,
            Reach::AgentSessions { listed: true },
            self,
        );
    }
}

const PROGRAM: ActionParam = ActionParam::required(
    "program",
    ParamType::Text,
    "The tilolang program, or `undo`, `undo task` or `history task`.",
);
const THIS: ActionParam = ActionParam::optional(
    "this",
    ParamType::Text,
    "What the person pointed at, as a resource literal, which `this` names.",
);
const SESSION: ActionParam = ActionParam::optional(
    "session",
    ParamType::Text,
    "The program session a person's program continues: its `let`s and references.",
);
const PARAMS: &[ActionParam] = &[PROGRAM, THIS, SESSION];

/// The program step, described for the catalogue in `language`.
fn descriptor(language: Option<&str>) -> tilo_actions::ActionDescriptor {
    let t = |key: &str| crate::i18n::message(language, key, &tilo_history::FluentArgs::new());
    tilo_actions::ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(task_wire::RESOURCE, task_wire::RUN),
        resource_kind: task_wire::RESOURCE.into(),
        resource_label: t("task-programs-resource"),
        action_label: t("task-programs-run"),
        keywords: Vec::new(),
        icon: None,
        description: Some(t("task-programs-run-detail")),
        locus: tilo_actions::ExecutionLocus::default(),
        // The step itself only reads: every change a program makes is its own invocation,
        // decided where it lands.
        kind: tilo_actions::OperationKind::View,
        contract: tilo_actions::ActionContract {
            result: tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({ "type": "object", "required": ["text", "failed"] }),
            },
            ..tilo_actions::ActionContract::view()
        },
        input: tilo_actions::InputSpec::None,
        params: PARAMS.to_vec(),
        view: None,
    }
}

/// `argument` , in its slot form, bound to `params` .
fn bind(params: &'static [ActionParam], argument: Option<&str>) -> Result<ActionArgs, String> {
    let supplied = match argument {
        None | Some("") => serde_json::Map::new(),
        Some(slot) => {
            let json = slot
                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .ok_or("a program takes JSON arguments")?;
            serde_json::from_str(json).map_err(|error| error.to_string())?
        }
    };
    ActionArgs::bind_json(params, &supplied).map_err(|error| error.to_string())
}

impl Admits for TaskPrograms {
    const PROVIDER: &'static str = task_wire::PROVIDER;
    const APP: &'static str = task_wire::APP;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        top_level(vec![descriptor((self.language)().as_deref())])
    }

    /// Whether an agent's program, which nobody approved yet, waits for its person's yes: its
    /// preview answers. A program that changes nothing has answered there, and one that changes
    /// only what its person need not be asked about runs once the call is taken.
    fn asks_first(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        let agent = caller.acting().agent_id()?;
        if request.approval().is_some()
            || self
                .previewed
                .lock_recover()
                .contains_key(&request.invocation)
        {
            return None;
        }
        let previewing = program_request(caller, request, admitted, Go::Preview, |run| {
            let program = run.program.clone();
            self.programs.run(run).map(|preview| (program, preview))
        });
        let (program, preview) = match previewing {
            Ok(previewing) => previewing,
            Err(reason) => return Some(Pending::ready(Err(ActionCallError::Refused { reason }))),
        };
        // The agent asks: its program is the agent's own call, shown on the question as a fact.
        let by = AskedBy::Agent(Named {
            id: agent.to_owned(),
            name: caller.agent_called().unwrap_or(agent).to_owned(),
        });
        let (previewed, request) = (Arc::clone(&self.previewed), request.clone());
        Some(preview.map(move |outcome| {
            let shown = outcome.as_ref().ok().and_then(previewed_answer);
            if let Some(facts) = shown.as_ref().and_then(|shown| asks(by, &program, shown)) {
                return asked::approved_or_asks(&request, || facts);
            }
            let found = if shown.is_some() {
                Previewed::Runs
            } else {
                Previewed::Answered(outcome)
            };
            let mut previewed = previewed.lock_recover();
            if previewed.len() >= PREVIEWS_KEPT {
                previewed.clear();
            }
            previewed.insert(request.invocation, found);
            Ok(())
        }))
    }

    /// Carry out an invocation the device's dispatch admitted for `caller`: a person's program,
    /// and an agent's that its person approved or its preview found nothing to ask them about.
    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        program_request(caller, request, admitted, Go::Run, |run| {
            if matches!(run.caller, ProgramCaller::Agent { .. }) && request.approval().is_none() {
                match self.previewed.lock_recover().remove(&request.invocation) {
                    Some(Previewed::Answered(outcome)) => return Ok(Pending::ready(outcome)),
                    Some(Previewed::Runs) => {}
                    None => return Err("an agent's program is previewed before it runs".into()),
                }
            }
            self.programs.run(run)
        })
    }
}

/// `request`, which the dispatch admitted for `caller`, as the program it runs, gone `go`, handed
/// to `then`.
fn program_request<T>(
    caller: &Caller,
    request: &InvokeRequest,
    admitted: &Admitted,
    go: Go,
    then: impl FnOnce(ProgramRequest<'_>) -> Result<T, String>,
) -> Result<T, String> {
    let address = &request.target.address;
    if address.resource != task_wire::RESOURCE || address.action != task_wire::RUN {
        return Err("unknown step of a session's programs".into());
    }
    let acting = caller.acting();
    let (who, provider) = if let Some(agent) = acting.agent_id() {
        let session = acting
            .agent_session
            .as_ref()
            .ok_or("an agent runs programs as a step of its session")?
            .as_str();
        let provider = admitted
            .through
            .clone()
            .ok_or("an agent runs programs only through its session's API")?;
        (
            ProgramCaller::Agent {
                agent,
                person: acting.acted_for_person(),
                session,
            },
            Some(provider),
        )
    } else if acting.app().is_some() || acting.bot_id().is_some() {
        return Err("a program is a person's or an agent session's".into());
    } else {
        let person = acting
            .person_id()
            .ok_or("a program is a person's or an agent session's")?;
        (
            ProgramCaller::Person {
                person,
                session: None,
            },
            None,
        )
    };
    let workspace = caller
        .workspace()
        .ok_or("a session's programs run in its workspace")?;
    let argument = request.argument.to_slot();
    let args = bind(PARAMS, argument.as_deref())?;
    let program = args.text("program").unwrap_or_default().to_owned();
    let this = args
        .text("this")
        .filter(|this| !this.trim().is_empty())
        .map(str::to_owned);
    // A person's client names its program session; one that names none continues the one its
    // connection has.
    let execution = caller.execution_id().map(|id| id.simple().to_string());
    let caller_lease = caller.execution_lease();
    let session = args
        .text("session")
        .filter(|session| !session.trim().is_empty())
        .or(execution.as_deref());
    let caller = match who {
        ProgramCaller::Person { person, .. } => ProgramCaller::Person { person, session },
        agent => agent,
    };
    let group = request
        .context
        .as_ref()
        .and_then(|context| context.group.clone())
        .unwrap_or_else(|| default_group(caller));
    then(ProgramRequest {
        workspace,
        provider,
        caller,
        group,
        program,
        go,
        this,
        stop: Some(Arc::clone(&admitted.stop)),
        asked_in: caller_lease,
    })
}

/// The task host's answer in a program's preview, when it showed one instead of answering.
fn previewed_answer(output: &tilo_actions::ActionOutput) -> Option<serde_json::Value> {
    let answer: task_wire::ProgramAnswer = serde_json::from_value(output.value.clone()?).ok()?;
    answer
        .answer
        .filter(|answer| answer.get("state").and_then(serde_json::Value::as_str) == Some("preview"))
}

/// What `shown`, the preview of `program`, asks its person, when it asks them at all: the program,
/// and how many times it calls each action.
fn asks(by: AskedBy, program: &str, shown: &serde_json::Value) -> Option<AskedFacts> {
    if shown.get("asks").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let steps = shown
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut actions: Vec<(&str, usize)> = Vec::new();
    for step in steps {
        let action = step
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("an action");
        match actions.iter_mut().find(|(known, _)| *known == action) {
            Some((_, times)) => *times = times.saturating_add(1),
            None => actions.push((action, 1)),
        }
    }
    let mut facts = vec![Fact::new("Program", program.trim())];
    facts.extend(
        actions
            .into_iter()
            .map(|(action, times)| Fact::new(action, format!("×{times}"))),
    );
    Some(AskedFacts {
        by,
        facts,
        destructive: steps.iter().any(|step| step["destructive"] == true),
        verb: "run".to_owned(),
        ..AskedFacts::default()
    })
}

/// The work a program's changes are part of when its call names none: the agent's session, or
/// the person's program session, which History groups.
fn default_group(caller: ProgramCaller<'_>) -> WorkGroup {
    let (id, kind) = match caller {
        ProgramCaller::Agent { session, .. } => {
            (session, tilo_actions::context::AGENT_SESSION_GROUP)
        }
        ProgramCaller::Person { session, .. } => (
            session.unwrap_or(tilo_actions::remote::session_actions::PROGRAMS_RESOURCE),
            "api",
        ),
    };
    WorkGroup {
        id: id.to_owned(),
        label: None,
        kind: kind.to_owned(),
        declared_id: None,
        session: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ActionAddress;
    use tilo_actions::remote::{CatalogueQuery, InvokeArgument, PendingPoll, RemoteActionTarget};

    /// A host no step reaches: every case below is refused before a program runs.
    struct NoPrograms;

    impl Programs for NoPrograms {
        fn run(&self, _: ProgramRequest<'_>) -> Result<Pending<Outcome>, String> {
            Err("no program runs in these cases".into())
        }
    }

    /// A host whose programs that call `delete` change much, previewed as two deletes, and whose
    /// other programs change nothing: what each was run as, in order.
    #[derive(Default)]
    struct Previews {
        went: Mutex<Vec<Go>>,
    }

    impl Previews {
        fn answer(program: &str, go: Go) -> Outcome {
            let shown = if program.contains("delete") && go == Go::Preview {
                serde_json::json!({
                    "state": "preview",
                    "asks": true,
                    "steps": [
                        { "action": "notes.delete", "destructive": true },
                        { "action": "notes.delete", "destructive": true },
                    ],
                })
            } else {
                serde_json::json!({ "state": "done", "output": [program] })
            };
            Ok(tilo_actions::ActionOutput {
                value: Some(
                    serde_json::to_value(task_wire::ProgramAnswer {
                        text: program.to_owned(),
                        answer: Some(shown),
                        ..task_wire::ProgramAnswer::default()
                    })
                    .unwrap(),
                ),
                resources: Vec::new(),
            })
        }
    }

    impl Programs for Previews {
        fn run(&self, request: ProgramRequest<'_>) -> Result<Pending<Outcome>, String> {
            self.went.lock_recover().push(request.go);
            Ok(Pending::ready(Self::answer(&request.program, request.go)))
        }
    }

    /// The step as the session serves it.
    fn programs() -> Arc<TaskPrograms> {
        Arc::new(TaskPrograms::new(Arc::new(NoPrograms), || None))
    }

    /// The person at this desktop, in the workspace in front.
    fn person() -> Caller {
        Caller::local_person("Pat").in_workspace("the-workspace")
    }

    fn request(address: &ActionAddress, argument: &str) -> InvokeRequest {
        InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: task_wire::PROVIDER.to_owned(),
                address: address.clone(),
            },
            argument: InvokeArgument::from_slot(Some(argument)),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    tilo_test_env::scenario! {
        fn a_program_step_is_offered_in_agent_sessions_and_taken_from_people_and_agents;

        phase a_program_step_is_the_agents_and_is_offered_in_its_sessions;
        // The step is the agent's, offered to an agent in a session and nowhere else.
        {
            let programs = programs();
            let providers = Providers::default();
            Arc::clone(&programs).register(&providers);
            let registered = providers
                .get(task_wire::PROVIDER)
                .expect("the programs are registered under their provider");
            assert_eq!(registered.provider.as_ref(), task_wire::PROVIDER);
            assert_eq!(registered.app.as_ref(), task_wire::APP);
            assert_eq!(
                registered.reach,
                Reach::AgentSessions { listed: true },
                "an agent session's catalogue offers it"
            );
            let query = CatalogueQuery {
                provider: Some(task_wire::PROVIDER.to_owned()),
                max_results: 1024,
                ..CatalogueQuery::default()
            };
            let mut page = registered.answers.query(&person(), query);
            let PendingPoll::Ready(Ok(page)) = page.poll_ready() else {
                panic!("the provider lists what it offers at once");
            };
            let described: Vec<_> = page.rows.into_iter().filter_map(|row| row.action).collect();
            assert_eq!(described.len(), 1);
            let step = &described[0];
            assert_eq!(step.address, ActionAddress::new(task_wire::RESOURCE, task_wire::RUN));
            assert_eq!(step.kind, tilo_actions::OperationKind::View);
            assert!(
                matches!(
                    &step.contract.result,
                    tilo_actions::ResultSpec::Json { schema }
                        if schema["required"] == serde_json::json!(["text", "failed"])
                ),
                "the step answers what a program made: {:?}",
                step.contract.result
            );
        }

        phase a_persons_and_an_agents_program_in_its_session_is_taken;
        // A program is a person's own, or a step of an agent's session through the session's API:
        // an app, and an agent outside a session, are refused before anything starts.
        {
            let programs = programs();
            let address = ActionAddress::new(task_wire::RESOURCE, task_wire::RUN);
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::json!({ "program": "ls" })
            );
            let took = |caller: Caller| {
                programs
                    .start(&caller, &request(&address, &argument), &Admitted::default())
                    .err()
            };
            assert_eq!(
                took(Caller::local_person("Pat").in_workspace("the-workspace")),
                Some("no program runs in these cases".to_owned()),
                "the person's program reaches the host"
            );
            assert_eq!(
                took(Caller::local_app("so.tilo.notes").in_workspace("the-workspace")),
                Some("a program is a person's or an agent session's".to_owned())
            );
            assert_eq!(
                took(Caller::local_agent("claude").in_workspace("the-workspace")),
                Some("an agent runs programs as a step of its session".to_owned())
            );
            assert_eq!(
                took(
                    Caller::local_agent("claude")
                        .in_workspace("the-workspace")
                        .in_agent_session("agent-session:1")
                ),
                Some("an agent runs programs only through its session's API".to_owned())
            );
        }

        phase an_agents_program_asks_its_person_and_runs_on_their_yes;
        // An agent's program that changes much is previewed, and the call is refused with what
        // its person is shown, nothing of it run; their approve brings the same call back, and
        // it runs for real. One that changes nothing answers from its preview alone.
        {
            let host = Arc::new(Previews::default());
            let programs = Arc::new(TaskPrograms::new(
                Arc::clone(&host) as Arc<dyn Programs>,
                || None,
            ));
            let agent = Caller::local_agent("claude")
                .in_workspace("the-workspace")
                .in_agent_session("agent-session:1");
            let admitted = Admitted {
                through: Some(
                    Arc::clone(&programs)
                        as Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
                ),
                ..Admitted::default()
            };
            let address = ActionAddress::new(task_wire::RESOURCE, task_wire::RUN);
            let call = |program: &str| {
                request(
                    &address,
                    &format!(
                        "{}{}",
                        tilo_actions::ARGUMENTS_JSON_PREFIX,
                        serde_json::json!({ "program": program })
                    ),
                )
            };
            fn wait<T>(pending: Pending<T>) -> T {
                pending
                    .wait_timeout(tilo_test_env::hang_bound())
                    .ok()
                    .flatten()
                    .expect("the step answers")
            }

            let many = call("for n in notes.ls() { n.delete() }");
            let refused = wait(
                programs
                    .asks_first(&agent, &many, &admitted)
                    .expect("an agent's program is previewed first"),
            );
            let Err(ActionCallError::ConfirmationRequired(facts)) = refused else {
                panic!("a program that deletes much waits for its person: {refused:?}");
            };
            assert_eq!(
                facts.by,
                AskedBy::Agent(Named {
                    id: "claude".to_owned(),
                    name: "claude".to_owned(),
                })
            );
            assert!(facts.destructive);
            assert_eq!(
                facts.facts,
                vec![
                    Fact::new("Program", "for n in notes.ls() { n.delete() }"),
                    Fact::new("notes.delete", "×2"),
                ]
            );
            assert!(
                !asked::Offer::of(&facts).contains(&asked::Offer::ApproveAndGrant),
                "a program's question grants its agent nothing"
            );
            assert_eq!(*host.went.lock_recover(), vec![Go::Preview]);
            assert_eq!(
                programs.start(&agent, &many, &admitted).err().as_deref(),
                Some("an agent's program is previewed before it runs"),
                "a call its person did not answer never runs"
            );
            assert_eq!(*host.went.lock_recover(), vec![Go::Preview]);

            let mut approved = many.clone();
            approved.confirmation = Some(tilo_actions::remote::Confirmation::Asked(
                asked::Approval::new(
                    asked::RecordRef {
                        workspace: "the-workspace".to_owned(),
                        record: "asked-1".to_owned(),
                        device: None,
                    },
                    None,
                ),
            ));
            assert!(
                programs.asks_first(&agent, &approved, &admitted).is_none(),
                "an approved call is not asked again"
            );
            let ran = wait(programs.start(&agent, &approved, &admitted).expect("it runs"));
            assert!(ran.is_ok(), "{ran:?}");
            assert_eq!(*host.went.lock_recover(), vec![Go::Preview, Go::Run]);

            host.went.lock_recover().clear();
            let read = call("notes.ls()");
            assert!(
                wait(programs.asks_first(&agent, &read, &admitted).expect("previewed")).is_ok(),
                "a program that changes nothing asks nobody"
            );
            let answered = wait(programs.start(&agent, &read, &admitted).expect("answered"));
            assert_eq!(answered, Previews::answer("notes.ls()", Go::Preview));
            assert_eq!(
                *host.went.lock_recover(),
                vec![Go::Preview],
                "its preview's answer is the call's: it is not run twice"
            );
        }
    }
}

//! The task programs this daemon runs ([`TaskPrograms`]): an agent session's here, one whose
//! session Tilo's API forwards to this device from elsewhere, and the person's own from their
//! command line. Each session has its own API and its own task host, which the daemon's
//! processes start ([`super::tasks`]); the session goes, and its host ends, once the connection
//! that asked for it ended.

use std::collections::HashMap;
use std::sync::Arc;

use tilo_actions::remote::{ActionProvider, Caller, ExecutionLease, Outcome, Pending};
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::workspaces::task_programs::{
    ProgramCaller, ProgramRequest, Programs, TaskPrograms,
};

type Provider = Arc<dyn ActionProvider + Send + Sync>;

/// The API a session's latest step is served on.
type Latest = tilo_mcp::agent_tools::Latest;

/// One session's API, calling through the API its latest step is served on.
type Api = tilo_mcp::McpServer<Arc<Latest>>;

/// Whose session it is: an agent's, by the workspace it works in, its agent and its id; or the
/// person's, by the workspace, who they are and the program session their client names.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Agent {
        workspace: String,
        agent: String,
        session: String,
    },
    Person {
        workspace: String,
        person: String,
        session: Option<String>,
    },
}

/// One session: its API, what its calls go through now, and the run of the connection that
/// asked for it.
struct Session {
    latest: Arc<Latest>,
    api: Arc<Api>,
    asked_in: Option<ExecutionLease>,
}

impl Session {
    /// Whether the connection that asked for it still runs.
    fn goes(&self) -> bool {
        self.asked_in.as_ref().is_none_or(ExecutionLease::is_live)
    }
}

/// What a person's programs call through in `workspace`: the daemon's API for them, and the
/// caller it admitted them as.
type PersonApi = Box<dyn Fn(&str) -> Result<(Provider, Caller), String> + Send + Sync>;

/// What starts a session's task hosts: for the person its programs run as, its programs naming
/// the apps the session's API lists to the caller it acts as.
type StartHosts = Box<
    dyn Fn(Option<&str>, Provider, Caller) -> Option<Arc<dyn tilo_mcp::TaskHosts>> + Send + Sync,
>;

/// The programs of the sessions this daemon's dispatch admits.
pub(crate) struct DaemonPrograms(Arc<Sessions>);

/// What runs those programs: the person's API, the task hosts, and the sessions open.
struct Sessions {
    person_api: PersonApi,
    hosts: StartHosts,
    open: std::sync::Mutex<HashMap<Key, Session>>,
}

impl DaemonPrograms {
    fn new(person_api: PersonApi, hosts: StartHosts) -> Self {
        Self(Arc::new(Sessions {
            person_api,
            hosts,
            open: std::sync::Mutex::default(),
        }))
    }
}

impl Sessions {
    /// The session `request` names, started when it has none, its calls going through what
    /// `request` brings now. Sessions whose connection ended go first, and their hosts with them.
    fn session_of(&self, request: &ProgramRequest<'_>) -> Result<Arc<Api>, String> {
        let key = match request.caller {
            ProgramCaller::Agent { agent, session, .. } => Key::Agent {
                workspace: request.workspace.to_owned(),
                agent: agent.to_owned(),
                session: session.to_owned(),
            },
            ProgramCaller::Person { person, session } => Key::Person {
                workspace: request.workspace.to_owned(),
                person: person.to_owned(),
                session: session.map(str::to_owned),
            },
        };
        let ended: Vec<Session> = self
            .open
            .lock_recover()
            .extract_if(|_, session| !session.goes())
            .map(|(_, session)| session)
            .collect();
        drop(ended);
        let mut sessions = self.open.lock_recover();
        if let Some(session) = sessions.get(&key) {
            if let Some(provider) = &request.provider {
                session.latest.set(Arc::clone(provider));
            }
            let api = Arc::clone(&session.api);
            drop(sessions);
            return Ok(api);
        }
        let session = self.start(request)?;
        let api = Arc::clone(&session.api);
        sessions.insert(key, session);
        drop(sessions);
        Ok(api)
    }

    /// A new session for `request`: an agent's through its session's API, the person's through
    /// the daemon's API for them.
    fn start(&self, request: &ProgramRequest<'_>) -> Result<Session, String> {
        let (latest, api, person) = match request.caller {
            ProgramCaller::Agent {
                agent, of, person, ..
            } => {
                let provider = request
                    .provider
                    .clone()
                    .ok_or("an agent runs programs only through its session's API")?;
                let latest = Arc::new(Latest::new(provider));
                let api = Api::for_task(
                    Arc::clone(&latest),
                    request.workspace.to_owned(),
                    (agent, of),
                    person,
                );
                (latest, api, person)
            }
            ProgramCaller::Person { person, .. } => {
                let (provider, caller) = (self.person_api)(request.workspace)?;
                if caller.acting().person_id() != Some(person) {
                    return Err("a person's programs run here only as this device's person".into());
                }
                let latest = Arc::new(Latest::new(provider));
                let api = Api::admitted(Arc::clone(&latest), caller)
                    .map_err(|error| error.to_string())?;
                (latest, api, Some(person))
            }
        };
        let api = api.with_work_group(request.group.clone());
        let hosts = (self.hosts)(
            person,
            Arc::clone(&latest) as Provider,
            api.execution_caller(),
        );
        Ok(Session {
            latest,
            api: Arc::new(api.with_task_hosts(hosts)),
            asked_in: request.asked_in.clone(),
        })
    }
}

impl Sessions {
    /// Run `request`'s program in the session it names.
    fn run(&self, request: ProgramRequest<'_>) -> Result<Pending<Outcome>, String> {
        let api = self.session_of(&request)?;
        tilo_mcp::task_runs::run(api, request.program, request.go, request.this, request.stop)
    }
}

impl Programs for DaemonPrograms {
    fn run(&self, request: ProgramRequest<'_>) -> Result<Pending<Outcome>, String> {
        let ProgramCaller::Person { person, session } = request.caller else {
            return self.0.run(request);
        };
        if !tilo_rt::app().is_runtime_thread() {
            return self.0.run(request);
        }
        // A person's session is started through the daemon's API for them, which admits them in
        // the workspace and waits on the workspaces: never on the app runtime's own thread,
        // whose tasks would all stop behind that wait. Asked there, the program runs from a
        // thread of its own, and is answered when it ends.
        let (person, session) = (person.to_owned(), session.map(str::to_owned));
        let workspace = request.workspace.to_owned();
        let ProgramRequest {
            provider,
            group,
            program,
            go,
            this,
            stop,
            asked_in,
            ..
        } = request;
        let sessions = Arc::clone(&self.0);
        let (ended, answer) = tilo_actions::remote::pending();
        tilo_rt::app().handle().spawn_blocking(move || {
            let ran = sessions.run(ProgramRequest {
                workspace: &workspace,
                provider,
                caller: ProgramCaller::Person {
                    person: &person,
                    session: session.as_deref(),
                },
                group,
                program,
                go,
                this,
                stop,
                asked_in,
            });
            match ran {
                Ok(running) => running.then(move |outcome| {
                    if let Some(outcome) = outcome {
                        ended.complete(outcome);
                    }
                }),
                Err(reason) => ended.complete(Err(Box::new(tilo_actions::ActionFailure {
                    code: "refused".to_owned(),
                    message: reason,
                    details: None,
                    retryable: false,
                }))),
            }
        });
        Ok(answer)
    }
}

impl super::services::Daemon {
    /// Run the task programs of the sessions this daemon's dispatch admits from now on: the
    /// person's own and every agent session's, each in a task host this daemon's processes
    /// start.
    pub(crate) fn serve_task_programs(self: &Arc<Self>) {
        let asked = Arc::downgrade(self);
        let person_api: PersonApi = Box::new(move |workspace| {
            let daemon = asked
                .upgrade()
                .ok_or_else(|| "this device's daemon is stopping".to_owned())?;
            daemon.person_programs_api(workspace)
        });
        let starting = Arc::downgrade(self);
        let hosts: StartHosts = Box::new(move |person, api, caller| {
            let daemon = starting.upgrade()?;
            let person = person.map_or_else(|| daemon.persons_id().to_owned(), str::to_owned);
            daemon.task_hosts(api, caller, &person)
        });
        let programs = TaskPrograms::new(
            Arc::new(DaemonPrograms::new(person_api, hosts)),
            tilo_ui_base::i18n::preferred_language,
        );
        Arc::new(programs).register(&self.serving().copies().providers());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ActionAddress;
    use tilo_actions::remote::Admits as _;
    use tilo_actions::remote::task_programs as wire;
    use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};

    const WORKSPACE: &str = "the-workspace";

    /// The API the programs below call through: it lists no app and takes no call, since they
    /// ask for neither.
    struct Workspace;

    impl ActionProvider for Workspace {
        fn query(
            &self,
            _: &Caller,
            query: tilo_actions::remote::CatalogueQuery,
        ) -> Pending<
            Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
        > {
            Pending::ready(Ok(tilo_actions::remote::CataloguePage::from_rows(
                String::new(),
                Vec::new(),
                &query,
            )))
        }

        fn invoke(
            &self,
            _: &Caller,
            _: tilo_actions::remote::InvokeRequest,
        ) -> Pending<
            Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
        > {
            Pending::ready(Err(tilo_actions::remote::ActionCallError::UnknownAction))
        }

        fn observe(
            &self,
            _: &Caller,
            _: &tilo_actions::InvocationId,
        ) -> Option<tilo_actions::remote::InvocationState> {
            None
        }

        fn cancel(
            &self,
            _: &Caller,
            _: &tilo_actions::InvocationId,
        ) -> Pending<Result<(), tilo_actions::remote::ActionCallError>> {
            Pending::ready(Ok(()))
        }
    }

    /// Task hosts as the device's processes start them: each keeps the `let`s its programs made
    /// (here, every program it ran) and answers with them, and ends when the API lets go of its
    /// program connection, saying so on `ended`. A program that deletes previews as two deletes
    /// for its person to be asked about, keeping nothing; any other changes nothing, and its
    /// preview has answered, kept as a run is.
    struct Hosts {
        started: std::sync::Mutex<Vec<String>>,
        ended: std::sync::mpsc::Sender<String>,
        /// Each program a host runs for real, as it runs it.
        runs: std::sync::Mutex<Option<std::sync::mpsc::Sender<String>>>,
    }

    /// What a host answers for `kept`, every program it ran.
    fn ran(kept: &[String]) -> serde_json::Value {
        serde_json::json!({"answer": {
            "state": "done",
            "output": [format!("ran {}", kept.join(", "))],
        }})
    }

    impl tilo_mcp::TaskHosts for Hosts {
        fn start(&self, _workspace: &str, task: &str) -> std::io::Result<tilo_socket::Stream> {
            use std::io::{BufRead as _, Write as _};
            self.started.lock_recover().push(task.to_owned());
            let (api, host) = std::os::unix::net::UnixStream::pair()?;
            let (task, ended) = (task.to_owned(), self.ended.clone());
            let runs = self.runs.lock_recover().clone();
            std::thread::spawn(move || {
                let mut writer = host.try_clone().expect("the host's own end");
                let mut kept: Vec<String> = Vec::new();
                for line in std::io::BufReader::new(host).lines() {
                    let Ok(asked) =
                        serde_json::from_str::<serde_json::Value>(&line.unwrap_or_default())
                    else {
                        break;
                    };
                    let answer = if let Some(run) = asked.get("run") {
                        let source = run["source"].as_str().unwrap_or_default().to_owned();
                        if let Some(runs) = &runs {
                            let _ = runs.send(source.clone());
                        }
                        kept.push(source);
                        ran(&kept)
                    } else if let Some(preview) = asked.get("preview") {
                        let source = preview["source"].as_str().unwrap_or_default();
                        if source.contains("delete") {
                            let delete = serde_json::json!({
                                "action": "notes.delete",
                                "destructive": true,
                                "resources": ["n1"],
                            });
                            serde_json::json!({"answer": {
                                "state": "preview",
                                "asks": true,
                                "complete": true,
                                "output": [],
                                "steps": [delete, delete],
                            }})
                        } else {
                            kept.push(source.to_owned());
                            ran(&kept)
                        }
                    } else {
                        serde_json::json!({"catalogue": {}})
                    };
                    if writeln!(writer, "{answer}").is_err() {
                        break;
                    }
                }
                let _ = ended.send(task);
            });
            Ok(api)
        }
    }

    /// The person this device is, admitted on one connection of their command line.
    fn person() -> Caller {
        Caller::authenticated([7; 32], [9; 32])
            .in_workspace(WORKSPACE)
            .in_new_execution()
    }

    /// The step as the daemon serves it, over `hosts`.
    fn programs(hosts: &Arc<Hosts>) -> TaskPrograms {
        let person_api: PersonApi = Box::new(|_| Ok((Arc::new(Workspace) as Provider, person())));
        let started = Arc::clone(hosts);
        let start: StartHosts =
            Box::new(move |_, _, _| Some(Arc::clone(&started) as Arc<dyn tilo_mcp::TaskHosts>));
        TaskPrograms::new(Arc::new(DaemonPrograms::new(person_api, start)), || None)
    }

    /// The step as the device's dispatch takes a call of a provider registered on it: a call
    /// nobody approved is asked about first ([`ActionProvider::asks_first`]), and its own calls
    /// go through `through`, its session's API.
    struct Taken {
        programs: TaskPrograms,
        through: Provider,
    }

    impl ActionProvider for Taken {
        fn query(
            &self,
            caller: &Caller,
            query: tilo_actions::remote::CatalogueQuery,
        ) -> Pending<
            Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
        > {
            ActionProvider::query(&self.programs, caller, query)
        }

        fn invoke(
            &self,
            caller: &Caller,
            request: tilo_actions::remote::InvokeRequest,
        ) -> Pending<
            Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
        > {
            let admitted = || tilo_actions::remote::Admitted {
                through: Some(Arc::clone(&self.through)),
                ..tilo_actions::remote::Admitted::default()
            };
            if request.approval().is_none()
                && let Some(asking) =
                    ActionProvider::asks_first(&self.programs, caller, &request, &admitted())
            {
                match asking.wait_timeout(std::time::Duration::from_secs(20)) {
                    Ok(Some(Ok(()))) => {}
                    Ok(Some(Err(waits))) => return Pending::ready(Err(waits)),
                    _ => {
                        return Pending::ready(Err(
                            tilo_actions::remote::ActionCallError::Unanswered,
                        ));
                    }
                }
            }
            ActionProvider::run(&self.programs, caller, request, admitted())
        }

        fn observe(
            &self,
            _: &Caller,
            _: &tilo_actions::InvocationId,
        ) -> Option<tilo_actions::remote::InvocationState> {
            None
        }

        fn cancel(
            &self,
            _: &Caller,
            _: &tilo_actions::InvocationId,
        ) -> Pending<Result<(), tilo_actions::remote::ActionCallError>> {
            Pending::ready(Ok(()))
        }
    }

    /// One program of an agent session, as its call names the step.
    fn step_call(program: &str) -> tilo_actions::remote::InvokeRequest {
        tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: wire::target(),
            argument: InvokeArgument::from_slot(Some(&format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::json!({ "program": program })
            ))),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    /// What `answered`, a receipt of the step, says the program answered.
    fn answered(
        answered: Option<
            Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
        >,
    ) -> wire::ProgramAnswer {
        let receipt = answered
            .expect("the program answers")
            .expect("the step takes the program");
        let tilo_actions::remote::ReceiptStatus::Completed { output } = receipt.status else {
            panic!("the program has answered: {:?}", receipt.status);
        };
        serde_json::from_value(output.value.expect("an answer"))
            .expect("the step answers what the program made")
    }

    /// Run `program` as `caller`, in the program session `session` when it names one: what the
    /// program answered.
    fn run(
        programs: &TaskPrograms,
        caller: &Caller,
        program: &str,
        session: Option<&str>,
    ) -> wire::ProgramAnswer {
        let mut arguments = serde_json::json!({ "program": program });
        if let Some(session) = session {
            arguments["session"] = session.into();
        }
        let request = tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: wire::PROVIDER.to_owned(),
                address: ActionAddress::new(wire::RESOURCE, wire::RUN),
            },
            argument: InvokeArgument::from_slot(Some(&format!(
                "{}{arguments}",
                tilo_actions::ARGUMENTS_JSON_PREFIX
            ))),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let answered = programs
            .start(caller, &request, &tilo_actions::remote::Admitted::default())
            .expect("the program starts")
            .wait_timeout(std::time::Duration::from_secs(20))
            .ok()
            .flatten()
            .expect("the program answers")
            .expect("the program ran");
        serde_json::from_value(answered.value.expect("an answer"))
            .expect("the step answers what the program made")
    }

    /// A program the person's command line sends runs on the daemon, in a task host of the
    /// client's program session: the next program of that session goes on with what the first
    /// left, another client's session has a host and `let`s of its own, and a session's host
    /// ends once the connection that asked for it did.
    #[test]
    fn a_command_lines_programs_run_here_and_keep_their_sessions_lets() {
        let (ended, host_ended) = std::sync::mpsc::channel();
        let hosts = Arc::new(Hosts {
            started: std::sync::Mutex::default(),
            ended,
            runs: std::sync::Mutex::default(),
        });
        let programs = programs(&hosts);
        let line = person();

        let first = run(&programs, &line, "let n = notes", Some("client-1"));
        assert!(!first.failed, "{}", first.text);
        assert!(first.text.contains("ran let n = notes"), "{}", first.text);
        let second = run(&programs, &line, "n", Some("client-1"));
        assert!(
            second.text.contains("ran let n = notes, n"),
            "the session's second program goes on with what its first left: {}",
            second.text
        );
        let apart = run(&programs, &line, "n", Some("client-2"));
        assert!(
            apart.text.contains("ran n") && !apart.text.contains("let n"),
            "another client's session has its own: {}",
            apart.text
        );
        assert_eq!(
            *hosts.started.lock_recover(),
            ["client-1", "client-2"],
            "one host per program session, started on its first program"
        );
        assert!(host_ended.try_recv().is_err(), "both hosts go on");

        // The command line's connection ends: its sessions go with the next program here, and
        // their hosts end.
        line.retire_execution();
        let other = person();
        run(&programs, &other, "notes", Some("client-3"));
        let mut gone: Vec<String> = (0..2)
            .map(|_| {
                host_ended
                    .recv_timeout(std::time::Duration::from_secs(20))
                    .expect("a host ended with its connection")
            })
            .collect();
        gone.sort();
        assert_eq!(gone, ["client-1", "client-2"]);
    }

    /// An agent session Tilo's API forwards here runs its programs through its session's API,
    /// in one host for the session, which ends once the forwarded run here does.
    #[test]
    fn a_forwarded_sessions_task_runs_here_and_ends_with_its_run() {
        let (ended, host_ended) = std::sync::mpsc::channel();
        let hosts = Arc::new(Hosts {
            started: std::sync::Mutex::default(),
            ended,
            runs: std::sync::Mutex::default(),
        });
        let taken = Taken {
            programs: programs(&hosts),
            through: Arc::new(Workspace),
        };
        let agent = Caller::local_agent("agent-laptop-1a2b", "ada")
            .in_workspace(WORKSPACE)
            .in_agent_session("agent-session:1")
            .in_new_execution();
        let step = |program: &str| {
            answered(
                taken
                    .invoke(&agent, step_call(program))
                    .wait_timeout(std::time::Duration::from_secs(20))
                    .ok()
                    .flatten(),
            )
        };
        assert!(step("ls notes").text.contains("ran ls notes"));
        assert!(step("ls chat").text.contains("ran ls notes, ls chat"));
        assert_eq!(*hosts.started.lock_recover(), ["agent-session:1"]);

        agent.retire_execution();
        run(&taken.programs, &person(), "notes", Some("client-1"));
        assert_eq!(
            host_ended
                .recv_timeout(std::time::Duration::from_secs(20))
                .expect("the host ended with the forwarded run"),
            "agent-session:1"
        );
    }

    /// An agent's program on this device that deletes waits for its person's yes as its asked
    /// record, made through the device's dispatch: the record shows the program and what it
    /// would do, and nothing of it ran. The agent cannot answer it. The person's approve runs it
    /// in the same session, which still has the `let`s made before it; a decline runs nothing.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one deleting program, from its ask to the person's yes, read top to bottom"
    )]
    fn an_agents_program_that_deletes_waits_for_its_persons_yes_here() {
        use super::super::asked::{Admits, DeviceAsked, Run};
        use tilo_actions::remote::asked::{self, Fact, Offer, Resolve};
        use tilo_remote_actions::asked::AskedStore;

        const ADA: [u8; 32] = [1; 32];
        let wait = std::time::Duration::from_secs(20);
        let (ended, _host_ended) = std::sync::mpsc::channel();
        let (runs, ran_for_real) = std::sync::mpsc::channel();
        let hosts = Arc::new(Hosts {
            started: std::sync::Mutex::default(),
            ended,
            runs: std::sync::Mutex::new(Some(runs)),
        });
        let dispatch: Provider = Arc::new(Taken {
            programs: programs(&hosts),
            through: Arc::new(Workspace),
        });
        let scout = Caller::delegated_agent("scout", ADA, [9; 32])
            .in_workspace(WORKSPACE)
            .in_agent_session("agent-session:1")
            .in_new_execution();
        let ada = Caller::authenticated(ADA, [3; 32]).in_workspace(WORKSPACE);
        let store = AskedStore::opened(None, None, Vec::new()).expect("the store opens");
        let (again, runs_as) = (Arc::clone(&dispatch), scout.clone());
        let admits: Admits = Box::new(move |_| Ok((Arc::clone(&again), runs_as.clone())));
        let device = DeviceAsked::new(Arc::clone(&store), Arc::default(), admits);
        let run = Run::Agent {
            workspace: WORKSPACE.into(),
            agent: "scout".into(),
            name: "Scout".into(),
        };
        let send = |call: &tilo_actions::remote::InvokeRequest| {
            device
                .send(
                    run.clone(),
                    &scout,
                    (Arc::clone(&dispatch), scout.clone()),
                    call.clone(),
                )
                .wait_timeout(wait)
                .ok()
                .flatten()
        };
        let resolve = |call: &tilo_actions::remote::InvokeRequest, by: &Caller, resolution| {
            let at = store
                .record(&call.invocation.0.to_string())
                .expect("the call is kept as its record");
            store
                .resolve(
                    &at,
                    by,
                    &Resolve {
                        resolution,
                        ..Resolve::default()
                    },
                )
                .wait_timeout(wait)
                .ok()
                .flatten()
                .expect("the answer is admitted or refused at once")
        };
        // What the session's host has run so far, as a program that only reads shows it.
        let so_far = || answered(send(&step_call("n"))).text;

        let first = answered(send(&step_call("let n = notes")));
        assert!(first.text.contains("ran let n = notes"), "{}", first.text);

        let delete = "for x in n { x.delete() }";
        let deleting = step_call(delete);
        let receipt = send(&deleting)
            .expect("the dispatch answers")
            .expect("kept, not refused");
        assert!(
            matches!(
                receipt.status,
                tilo_actions::remote::ReceiptStatus::Accepted
            ),
            "it waits under way: {receipt:?}"
        );
        // What Ada is asked: the calls waiting in her workspace for her answer.
        let asked_of_ada = || -> Vec<asked::Asked> {
            store
                .waiting_in(WORKSPACE)
                .into_iter()
                .filter(|waiting| waiting.person == hex::encode(ADA))
                .map(|waiting| waiting.asked)
                .collect()
        };
        let questions = asked_of_ada();
        assert_eq!(questions.len(), 1, "Ada is asked");
        let facts = &questions[0].facts;
        assert_eq!(
            facts.by,
            asked::AskedBy::Agent(asked::Named {
                id: "scout".into(),
                name: "Scout".into(),
            }),
            "her agent asks, its program shown as what it would run"
        );
        assert!(facts.destructive);
        assert_eq!(
            facts.facts,
            [
                Fact::new("Program", delete),
                Fact::new("notes.delete", "×2")
            ],
            "she sees the program and what it would do"
        );
        assert!(
            !Offer::of(facts).contains(&Offer::ApproveAndGrant),
            "a program's question grants nothing"
        );
        assert!(!so_far().contains(delete), "nothing of it ran");

        let Err(tilo_actions::remote::ActionCallError::Refused { reason }) =
            resolve(&deleting, &scout, Offer::Approve)
        else {
            panic!("the agent that asked answers nothing");
        };
        assert_eq!(reason, tilo_remote_actions::asked::NOT_YOURS);
        assert!(!so_far().contains(delete), "nothing of it ran");

        resolve(&deleting, &ada, Offer::Approve).expect("her approve is taken");
        assert_eq!(
            ran_for_real
                .recv_timeout(wait)
                .expect("her approve runs it"),
            delete
        );
        let after = so_far();
        assert!(
            after.contains(&format!("ran let n = notes, n, n, {delete}, n")),
            "it ran once, in the session that kept its earlier lets: {after}"
        );

        let again = step_call("for x in notes { x.delete() }");
        send(&again)
            .expect("the dispatch answers")
            .expect("kept, not refused");
        resolve(&again, &ada, Offer::Decline).expect("her decline is taken");
        let declined = so_far();
        assert!(
            !declined.contains("for x in notes"),
            "a declined program runs nothing: {declined}"
        );
        assert!(asked_of_ada().is_empty(), "Ada is asked no more");
        assert!(
            ran_for_real.try_recv().is_err(),
            "only her approved program ran for real"
        );
    }
}

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
pub(crate) struct DaemonPrograms {
    person_api: PersonApi,
    hosts: StartHosts,
    sessions: std::sync::Mutex<HashMap<Key, Session>>,
}

impl DaemonPrograms {
    fn new(person_api: PersonApi, hosts: StartHosts) -> Self {
        Self {
            person_api,
            hosts,
            sessions: std::sync::Mutex::default(),
        }
    }

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
            .sessions
            .lock_recover()
            .extract_if(|_, session| !session.goes())
            .map(|(_, session)| session)
            .collect();
        drop(ended);
        let mut sessions = self.sessions.lock_recover();
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
            ProgramCaller::Agent { agent, person, .. } => {
                let provider = request
                    .provider
                    .clone()
                    .ok_or("an agent runs programs only through its session's API")?;
                let latest = Arc::new(Latest::new(provider));
                let api = Api::for_task(
                    Arc::clone(&latest),
                    request.workspace.to_owned(),
                    agent,
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

impl Programs for DaemonPrograms {
    fn run(&self, request: ProgramRequest<'_>) -> Result<Pending<Outcome>, String> {
        let api = self.session_of(&request)?;
        tilo_mcp::task_runs::run(
            api,
            request.program,
            request.confirm,
            request.this,
            request.stop,
        )
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
            ui::i18n::preferred_language,
        );
        Arc::new(programs).register(&self.serving().copies().providers());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::Admits as _;
    use tilo_actions::remote::task_programs as wire;
    use tilo_actions::{ActionAddress, InvokeArgument, RemoteActionTarget};

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
    /// program connection, saying so on `ended`.
    struct Hosts {
        started: std::sync::Mutex<Vec<String>>,
        ended: std::sync::mpsc::Sender<String>,
    }

    impl tilo_mcp::TaskHosts for Hosts {
        fn start(&self, _workspace: &str, task: &str) -> std::io::Result<std::os::fd::OwnedFd> {
            use std::io::{BufRead as _, Write as _};
            self.started.lock_recover().push(task.to_owned());
            let (api, host) = std::os::unix::net::UnixStream::pair()?;
            let (task, ended) = (task.to_owned(), self.ended.clone());
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
                        kept.push(run["source"].as_str().unwrap_or_default().to_owned());
                        serde_json::json!({"answer": {
                            "state": "done",
                            "output": [format!("ran {}", kept.join(", "))],
                        }})
                    } else {
                        serde_json::json!({"catalogue": {}})
                    };
                    if writeln!(writer, "{answer}").is_err() {
                        break;
                    }
                }
                let _ = ended.send(task);
            });
            Ok(api.into())
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
        });
        let programs = programs(&hosts);
        let agent = Caller::local_agent("agent-laptop-1a2b")
            .in_workspace(WORKSPACE)
            .in_agent_session("agent-session:1")
            .in_new_execution();
        let through = tilo_actions::remote::Admitted {
            through: Some(Arc::new(Workspace)),
            ..tilo_actions::remote::Admitted::default()
        };
        let step = |program: &str| {
            let request = tilo_actions::remote::InvokeRequest {
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
            };
            let answered = programs
                .start(&agent, &request, &through)
                .expect("the step starts")
                .wait_timeout(std::time::Duration::from_secs(20))
                .ok()
                .flatten()
                .expect("the program answers")
                .expect("the program ran");
            serde_json::from_value::<wire::ProgramAnswer>(answered.value.expect("an answer"))
                .expect("the step answers what the program made")
        };
        assert!(step("ls notes").text.contains("ran ls notes"));
        assert!(step("ls chat").text.contains("ran ls notes, ls chat"));
        assert_eq!(*hosts.started.lock_recover(), ["agent-session:1"]);

        agent.retire_execution();
        run(&programs, &person(), "notes", Some("client-1"));
        assert_eq!(
            host_ended
                .recv_timeout(std::time::Duration::from_secs(20))
                .expect("the host ended with the forwarded run"),
            "agent-session:1"
        );
    }
}

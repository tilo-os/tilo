//! The person's shell and the device's daemon call each other through the daemon's one dispatch
//! over the link the daemon hands the shell (`runtime_collab::wire::shell`): the shell's own
//! calls are answered as the person, and a provider the shell serves is called by the dispatch
//! as any provider registered on the device is, for as long as the link lasts.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime_collab::ShellClient;
use serde_json::{Map, json};
use tilo_actions::remote::TestProvider;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, CatalogueRow,
    Completer, Confirmation, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest,
    Pending, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};

use crate::language::host;
use tilo_test_env::SpawnUnderEnvLock as _;

const PROBE: &str = "so.tilo.probe";
const ASKS: &str = "session.asks";
const WAITS: &str = "session.waits";
const BOARD: &str = "session.board";
const SESSION: &str = tilo_actions::remote::session_actions::PROVIDER;
const MODELS: &str = tilo_actions::remote::MODELS_PROVIDER;
const SERVER_ACCOUNT: &str = tilo_actions::remote::SERVER_ACCOUNT_PROVIDER;

/// What `provider` offers: `resource#action`, taking an `echo`.
fn offered(provider: &str, app: &str, resource: &str, action: &str) -> CataloguePage {
    let row = CatalogueRow::for_action(
        provider,
        app,
        tilo_actions::ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(resource, action),
            resource_kind: resource.into(),
            resource_label: resource.into(),
            action_label: action.into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: tilo_actions::ExecutionLocus::ClientLocal,
            kind: tilo_actions::OperationKind::Effect,
            contract: tilo_actions::ActionContract::default(),
            input: tilo_actions::InputSpec::None,
            params: vec![tilo_actions::ActionParam::optional(
                "echo",
                tilo_actions::ParamType::Text,
                "What the provider answers",
            )],
            view: None,
        },
    );
    CataloguePage::from_rows(
        "1".into(),
        vec![row],
        &tilo_actions::remote::CatalogueQuery::default(),
    )
}

/// A provider answering each call with what it was asked to echo.
fn echoing(page: CataloguePage) -> Arc<TestProvider> {
    Arc::new(
        TestProvider::new()
            .with_catalogue(move |_, _| Ok(page.clone()))
            .completing(|request| ActionOutput {
                value: match &request.argument {
                    InvokeArgument::Json(arguments) => arguments.get("echo").cloned(),
                    _ => None,
                },
                ..ActionOutput::default()
            }),
    )
}

/// A provider that takes each call and holds it: a receipt saying the call goes on, a state that
/// says so, and a next change that never comes, as a provider still at work on it.
struct Holding {
    asked: Mutex<Vec<InvokeRequest>>,
    /// The completers that hold each call's next change: kept alive, they are what keeps the call
    /// from moving until the shell that served it goes.
    held: Mutex<Vec<Completer<()>>>,
}

impl Holding {
    fn new() -> Self {
        Self {
            asked: Mutex::new(Vec::new()),
            held: Mutex::new(Vec::new()),
        }
    }

    /// The requests asked so far, oldest first.
    fn asked(&self) -> Vec<InvokeRequest> {
        self.asked.lock().unwrap().clone()
    }
}

impl ActionProvider for Holding {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::default()))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.asked.lock().unwrap().push(request.clone());
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Accepted,
        }))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        Some(InvocationState::new(
            *id,
            InvocationUpdate::Accepted { invocation: *id },
        ))
    }

    fn next_change(&self, _caller: &Caller, _id: &InvocationId) -> Option<Pending<()>> {
        let (held, wait) = tilo_actions::remote::pending::<()>();
        self.held.lock().unwrap().push(held);
        Some(wait)
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }
}

/// `provider`'s `resource#action`, asked to echo `echo`.
fn request(provider: &str, resource: &str, action: &str, echo: &str) -> InvokeRequest {
    let mut arguments = Map::new();
    arguments.insert("echo".into(), json!(echo));
    InvokeRequest::plain(
        RemoteActionTarget {
            provider: provider.into(),
            address: ActionAddress::new(resource, action),
        },
        arguments,
    )
}

/// The shell's end of a link the daemon made, as the shell it starts holds it.
fn shell_of(host: &host::Host) -> ShellClient {
    let link = host.daemon().shell_link().unwrap();
    link.set_nonblocking(true).unwrap();
    ShellClient::open(tokio::net::UnixStream::from_std(link).unwrap()).0
}

/// How the call `id` ends at the daemon's dispatch, as the person follows it.
async fn ended(host: &host::Host, id: tilo_actions::InvocationId) -> InvocationUpdate {
    let (dispatch, caller) = (host.dispatch(), host.caller());
    tilo_test_env::within("the call ends", async move {
        loop {
            if let Some(state) = dispatch.observe(&caller, &id)
                && state.is_terminal()
            {
                return state.update;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

/// The session takes its dispatch on its own threads (its frame loop, its compositor's start),
/// outside any runtime: the dispatch carries its calls on the runtime the link runs on, and the
/// call is answered as one taken inside it.
#[test]
fn a_dispatch_taken_outside_any_runtime_reaches_the_daemon() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let probe = echoing(offered(PROBE, PROBE, "probe", "ping"));
        host.daemon().provide(PROBE, Arc::clone(&probe) as _);
        let shell = shell_of(&host);

        let dispatch = std::thread::spawn(move || shell.dispatch())
            .join()
            .expect("a dispatch is taken on a thread outside the runtime");
        let answered = dispatch
            .settled(
                Some(workspace),
                request(PROBE, "probe", "ping", "from the frame loop"),
            )
            .await;
        assert_eq!(answered, Ok(json!("from the frame loop")));
    });
}

/// The shell's own call is answered by the daemon's dispatch, as the person; a call the dispatch
/// makes of a provider the shell serves reaches it over the link and is answered, its approval
/// on the same frame.
#[test]
fn the_shell_and_the_daemon_call_each_other_through_the_one_dispatch() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let probe = echoing(offered(PROBE, PROBE, "probe", "ping"));
        host.daemon().provide(PROBE, Arc::clone(&probe) as _);
        let shell = shell_of(&host);

        // The shell's call, answered by the daemon's dispatch as the person.
        let answered = shell
            .dispatch()
            .settled(
                Some(workspace.clone()),
                request(PROBE, "probe", "ping", "from the shell"),
            )
            .await;
        assert_eq!(answered, Ok(json!("from the shell")));
        let callers = probe.callers();
        assert!(
            callers
                .iter()
                .any(|caller| caller.acting().person_id() == Some(host.person())),
            "the dispatch ran the shell's call as the person: {callers:?}"
        );

        // A provider the shell serves, called by the dispatch: the shell's own call of it goes
        // to the daemon and comes back over the same link.
        let asks = echoing(offered(ASKS, "so.tilo.shell", "asks/one", "answer"));
        let _served = shell
            .provide(
                ASKS,
                &offered(ASKS, "so.tilo.shell", "asks/one", "answer"),
                Arc::clone(&asks) as _,
                tilo_action_calls::serve,
            )
            .await
            .unwrap();
        let answered = shell
            .dispatch()
            .settled(
                Some(workspace.clone()),
                request(ASKS, "asks/one", "answer", "round trip"),
            )
            .await;
        assert_eq!(answered, Ok(json!("round trip")));

        // The phrase the person typed for a call travels with the call, on the frame the provider is
        // handed.
        let mut approved = request(ASKS, "asks/one", "answer", "approved");
        approved.confirmation = Some(Confirmation::Phrase("approved".into()));
        let receipt = host
            .dispatch()
            .invoke(&host.caller(), approved.clone())
            .await
            .unwrap()
            .unwrap();
        let update = ended(&host, receipt.invocation).await;
        assert!(
            matches!(&update, InvocationUpdate::Succeeded { output, .. } if output.value == Some(json!("approved"))),
            "the dispatch's call of the shell's provider is answered: {update:?}"
        );
        let asked = asks.asked();
        let reached = asked
            .iter()
            .find(|asked| asked.invocation == approved.invocation)
            .expect("the provider was handed the approved call");
        assert_eq!(
            reached.confirmation,
            Some(Confirmation::Phrase("approved".into()))
        );
        // The shell is told who the dispatch admitted the call for.
        assert!(
            asks.callers().iter().all(|caller| {
                caller.acting().person_id() == Some(host.person())
                    && caller.workspace() == Some(workspace.as_str())
            }),
            "the shell's provider hears the call as the person's, in its workspace: {:?}",
            asks.callers()
        );

        // Another provider by the same name is refused; so is one that is not the shell's own.
        let taken = shell
            .provide(
                ASKS,
                &CataloguePage::default(),
                Arc::clone(&asks) as _,
                tilo_action_calls::serve,
            )
            .await;
        assert!(
            taken.is_err(),
            "a provider served already is not taken over"
        );
        let foreign = shell
            .provide(
                PROBE,
                &CataloguePage::default(),
                Arc::clone(&asks) as _,
                tilo_action_calls::serve,
            )
            .await;
        assert!(foreign.is_err(), "the shell serves its own providers alone");
    });
}

/// Once the shell's link closes, what it served is gone, and a call it had not answered ends
/// failed, saying so.
#[test]
fn a_closed_link_takes_the_shells_providers_and_fails_their_calls() {
    host::run(|host| async move {
        let shell = shell_of(&host);
        // A provider that takes a call and holds it while the link lasts.
        let waits = Arc::new(Holding::new());
        let served = shell
            .provide(
                WAITS,
                &offered(WAITS, "so.tilo.shell", "waits/one", "answer"),
                Arc::clone(&waits) as _,
                tilo_action_calls::serve,
            )
            .await
            .unwrap();
        let waiting = request(WAITS, "waits/one", "answer", "never");
        let receipt = host
            .dispatch()
            .invoke(&host.caller(), waiting.clone())
            .await
            .unwrap()
            .unwrap();
        tilo_test_env::within("the shell is handed the call", async {
            while waits.asked().is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;

        drop((served, shell));
        let update = ended(&host, receipt.invocation).await;
        let InvocationUpdate::Failed { error, .. } = &update else {
            panic!("the waiting call fails once the link closed: {update:?}");
        };
        assert!(
            error.message.contains("has closed"),
            "the failure says the shell closed: {}",
            error.message
        );
        // The provider went with the link.
        let gone: Result<InvocationReceipt, _> =
            tilo_test_env::within("the provider goes", async {
                loop {
                    let again = host
                        .dispatch()
                        .invoke(
                            &host.caller(),
                            request(WAITS, "waits/one", "answer", "again"),
                        )
                        .await
                        .unwrap();
                    match again {
                        Err(error) => return Err(error),
                        Ok(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                    }
                }
            })
            .await;
        assert!(gone.is_err(), "nothing serves {WAITS} once the link closed");
    });
}

/// The session's own actions are served over the link too, as a turn's steps: what the daemon's
/// endpoint asks of them reaches the session, and what the session offers next is what the
/// dispatch admits from then on.
#[test]
fn the_sessions_own_actions_are_answered_through_the_daemons_dispatch() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let shell = shell_of(&host);
        let layout = echoing(offered(SESSION, SESSION, "shell:layout", "place"));
        let served = shell
            .provide(
                SESSION,
                &offered(SESSION, SESSION, "shell:layout", "place"),
                Arc::clone(&layout) as _,
                tilo_action_calls::serve,
            )
            .await
            .unwrap();

        // The daemon's endpoint answers each call through its dispatch, as the person.
        let placed = request(SESSION, "shell:layout", "place", "placed");
        let receipt = host
            .dispatch()
            .invoke(&host.caller(), placed.clone())
            .await
            .unwrap()
            .unwrap();
        let update = ended(&host, receipt.invocation).await;
        assert!(
            matches!(&update, InvocationUpdate::Succeeded { output, .. } if output.value == Some(json!("placed"))),
            "the session answers its layout's step: {update:?}"
        );
        assert!(
            layout
                .asked()
                .iter()
                .any(|asked| asked.invocation == placed.invocation),
            "the step reached the session"
        );
        assert_eq!(
            shell
                .dispatch()
                .settled(
                    Some(workspace.clone()),
                    request(SESSION, "shell:layout", "place", "from the shell"),
                )
                .await,
            Ok(json!("from the shell"))
        );

        // A step the session offers only from its next pass on is admitted once it is offered.
        let later = request(SESSION, "shell:notices", "dismiss", "dismissed");
        let unknown = host
            .dispatch()
            .invoke(&host.caller(), later.clone())
            .await
            .unwrap();
        assert!(
            unknown.is_err(),
            "a step the session does not offer is refused"
        );
        let mut both = offered(SESSION, SESSION, "shell:layout", "place");
        both.rows
            .extend(offered(SESSION, SESSION, "shell:notices", "dismiss").rows);
        served.publish(&both).await.unwrap();
        let later = request(SESSION, "shell:notices", "dismiss", "dismissed");
        let receipt = host
            .dispatch()
            .invoke(&host.caller(), later.clone())
            .await
            .unwrap()
            .unwrap();
        let update = ended(&host, receipt.invocation).await;
        assert!(
            matches!(&update, InvocationUpdate::Succeeded { output, .. } if output.value == Some(json!("dismissed"))),
            "a step offered from the session's next pass on is answered: {update:?}"
        );
    });
}

/// A test's session is the daemon's shell as a sign-in starts it: the daemon runs, gives the
/// shell the one-use secret it redeems for its link, and both stop together.
///
/// The person's shell is the desktop session's, which only a Linux device runs.
#[cfg(target_os = "linux")]
#[test]
fn a_shell_session_starts_the_persons_shell_on_its_link_and_stops_with_it() {
    host::in_own_process(|profile| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (session, socket, shell) = started_shell(profile, &runtime);
        let answered =
            runtime.block_on(shell.call(runtime_collab::wire::shell::ShellCall::Workspaces));
        assert!(
            matches!(
                answered,
                Ok(runtime_collab::wire::shell::ShellReply::Workspaces(ref held)) if !held.is_empty()
            ),
            "the link the secret admits reaches the daemon's workspaces: {answered:?}"
        );
        assert!(
            std::os::unix::net::UnixStream::connect(&socket).is_ok(),
            "the daemon's endpoint answers while the session runs"
        );
        session.stop().unwrap();
        assert!(
            !socket.exists(),
            "the endpoint is gone once the session stopped"
        );
    });
}

/// A started shell session's link, as the shell it starts redeems it: the session, the daemon's
/// endpoint, and the shell's end of its link on `runtime`.
#[cfg(target_os = "linux")]
fn started_shell(
    profile: &std::path::Path,
    runtime: &tokio::runtime::Runtime,
) -> (
    tilo_runtime::daemon::ShellSession,
    std::path::PathBuf,
    ShellClient,
) {
    use std::io::BufRead as _;
    let mut shell = std::process::Command::new("/bin/sh");
    // The shell says the secret it was given, then waits: the link is the one that secret
    // redeems.
    shell
        .args(["-c", "echo \"$TILO_SHELL_TOKEN\"; sleep 120"])
        .stdout(std::process::Stdio::piped());
    let mut session =
        tilo_runtime::daemon::ShellSession::start(profile, shell, Duration::from_secs(120))
            .unwrap();
    let socket = session.native_socket().to_owned();
    let mut said = String::new();
    std::io::BufReader::new(
        session
            .shell()
            .expect("the session's shell")
            .stdout
            .take()
            .unwrap(),
    )
    .read_line(&mut said)
    .unwrap();
    let token = said.trim();
    assert_eq!(token.len(), 64, "the shell is given one one-use secret");
    let link = tilo_session_client::daemon_documents::link_from_token(&socket, token)
        .expect("the secret admits the shell's link");
    let link = std::os::unix::net::UnixStream::from(link);
    link.set_nonblocking(true).unwrap();
    let _inside = runtime.enter();
    let (shell, _) = ShellClient::open(tokio::net::UnixStream::from_std(link).unwrap());
    (session, socket, shell)
}

/// The answers to `requests`, sent as an MCP client sends them over one connection to the
/// daemon's endpoint at `socket`, read until the last of them is answered.
#[cfg(target_os = "linux")]
fn mcp_over(socket: &std::path::Path, requests: &[serde_json::Value]) -> Vec<serde_json::Value> {
    use std::io::{BufRead as _, Write as _};
    let stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    for request in requests {
        writeln!(writer, "{request}").unwrap();
    }
    let last = requests
        .iter()
        .filter_map(|request| request.get("id").cloned())
        .next_back()
        .expect("a request to answer");
    let mut answers = Vec::new();
    for line in std::io::BufReader::new(stream).lines() {
        let answer: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        let done = answer.get("id") == Some(&last);
        answers.push(answer);
        if done {
            return answers;
        }
    }
    panic!("the daemon closed the connection before answering: {answers:?}");
}

/// An agent's `tilo-runtime --connect` in a Tilo login, with no profile named, reaches this
/// computer's daemon: the socket a desktop session once served its agents at hears nothing.
#[cfg(target_os = "linux")]
#[test]
fn connect_in_a_tilo_login_reaches_the_computers_daemon() {
    use std::io::{BufRead as _, Write as _};
    use std::os::unix::fs::DirBuilderExt as _;
    host::in_own_process(|_| {
        // This computer's own daemon, over the profile its account keeps.
        let profile = tilo_machine::account_daemon_profile().expect("the test's home");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&profile)
            .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (session, _, _shell) = started_shell(&profile, &runtime);
        let run = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
        let old = run.join("tilo-session");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&old)
            .unwrap();
        let unheard = std::os::unix::net::UnixListener::bind(old.join("agent.sock")).unwrap();
        unheard.set_nonblocking(true).unwrap();
        let mut client = std::process::Command::new(tilo_test_env::package_binary!("tilo-runtime"))
            .arg("--connect")
            .env("XDG_CURRENT_DESKTOP", "tilo:GNOME")
            .env_remove("TILO_RUNTIME_PROFILE")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn_locked()
            .unwrap();
        let mut stdin = client.stdin.take().unwrap();
        for request in [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "an agent", "version": "1"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        ] {
            writeln!(stdin, "{request}").unwrap();
        }
        let listed = std::io::BufReader::new(client.stdout.take().unwrap())
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(&line.unwrap()).unwrap())
            .find(|answer| answer["id"] == json!(2))
            .expect("the tools are listed");
        let tools = listed["result"]["tools"].to_string();
        assert!(tools.contains("tilo_do"), "the daemon's tools: {listed}");
        assert!(
            matches!(unheard.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "nothing dials the session's old socket"
        );
        drop(stdin);
        let _ = client.kill();
        let _ = client.wait();
        session.stop().unwrap();
    });
}

/// What the person's session serves on the device's dispatch, `tilo do` and an agent's MCP
/// client reach at the daemon's one socket: the session opens no socket of its own for them.
/// What the person's seat keeps for them (the account this computer is signed in to, their model
/// keys) is their desktop's alone: a program of theirs at the same socket neither lists nor
/// calls it.
#[cfg(target_os = "linux")]
#[test]
fn tilo_do_and_an_agent_reach_what_the_session_serves_at_the_daemons_socket() {
    host::in_own_process(|profile| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (session, socket, shell) = started_shell(profile, &runtime);
        // A fresh device holds no workspace until its shell founds the person's first, as the
        // desktop does when it starts.
        let mut first = Map::new();
        first.insert("name".into(), json!("Home"));
        first.insert("id".into(), json!(uuid::Uuid::new_v4().to_string()));
        runtime
            .block_on(shell.dispatch().settled(
                None,
                tilo_session_client::workspace_view::request(
                    RemoteActionTarget {
                        provider: tilo_action_policy::agent_answers::ACCOUNT_APP.into(),
                        address: ActionAddress::new(
                            tilo_peer_state::workspace_view::LIST,
                            tilo_peer_state::workspace_view::CREATE,
                        ),
                    },
                    first,
                ),
            ))
            .expect("the shell founds the person's first workspace");
        let board = echoing(offered(BOARD, "so.tilo.shell", "board/notes", "pin-note"));
        let _served = runtime
            .block_on(shell.provide(
                BOARD,
                &offered(BOARD, "so.tilo.shell", "board/notes", "pin-note"),
                Arc::clone(&board) as _,
                tilo_action_calls::serve,
            ))
            .unwrap();
        // The account this computer is signed in to, as their desktop's seat serves it.
        let account = echoing(offered(
            SERVER_ACCOUNT,
            "so.tilo.settings",
            "account/server",
            "reveal-token",
        ));
        let _account = runtime
            .block_on(shell.provide(
                SERVER_ACCOUNT,
                &offered(
                    SERVER_ACCOUNT,
                    "so.tilo.settings",
                    "account/server",
                    "reveal-token",
                ),
                Arc::clone(&account) as _,
                tilo_action_calls::serve,
            ))
            .unwrap();

        // `tilo do`, as the person types it, with no desktop's socket anywhere: a call of the
        // agent surface's walk over the daemon's socket.
        let (done, listed) = runtime.block_on(async {
            let calls = tilo_api::calls::Calls::connect(Some(&socket), None)
                .await
                .expect("tilo reaches the daemon");
            let workspace = calls.served_workspace().map(str::to_owned);
            let done = calls
                .start_waiting(
                    workspace.as_deref(),
                    "shell/board/notes#pin-note",
                    &json!({"echo": "pinned by hand"}),
                    "Pin a note",
                    30_000,
                )
                .await
                .map_err(|error| format!("{error:#}"));
            let mut listed = Vec::new();
            for path in [None, Some("shell/board/notes")] {
                listed.push(format!(
                    "{:?}",
                    calls.list(workspace.as_deref(), path, None).await
                ));
            }
            let listed = listed.join("\n");
            (done, listed)
        });
        // An agent's MCP client, at the same socket.
        let answers = mcp_over(
            &socket,
            &[
                json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                    "protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "an agent", "version": "1"}}}),
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                    "name": "tilo_find", "arguments": {"query": "pin-note"}}}),
                json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
                    "name": "tilo_do",
                    "arguments": {"path": "shell/board/notes#pin-note",
                                  "args": {"echo": "pinned by an agent"}}}}),
            ],
        );
        let called = answers.last().unwrap();
        let (text, failed) = host::said(&called["result"]);
        assert!(
            !failed && text.contains("pinned by an agent"),
            "the agent's call reaches the session's action: {called}\nwhat it finds: {}",
            answers[answers.len() - 2]
        );
        let out = match &done {
            Ok(answer) => answer.to_string(),
            Err(error) => {
                panic!("tilo do reaches the session's action: {error}\nwhat it lists:\n{listed}")
            }
        };
        assert!(
            listed.contains("pin-note"),
            "the session's action is listed: {listed}"
        );
        let typed_echo = board.asked().first().map(|asked| asked.argument.clone());
        assert!(
            matches!(&typed_echo, Some(InvokeArgument::Json(arguments)) if arguments.get("echo") == Some(&json!("pinned by hand"))),
            "the session's provider is asked what was typed: {typed_echo:?}"
        );
        assert!(
            out.contains("pinned by hand"),
            "tilo do says what it answered: {out}"
        );
        assert_eq!(
            board.asked().len(),
            2,
            "both calls reached the session's provider"
        );

        // The seat's: not listed to the command, and refused when called by name.
        assert!(
            !listed.contains("reveal-token"),
            "the seat's actions are not listed: {listed}"
        );
        let program = tilo_action_line::LineClient::of_stream(
            std::os::unix::net::UnixStream::connect(&socket).unwrap(),
        )
        .unwrap();
        for (provider, resource, action) in [
            (SERVER_ACCOUNT, "account/server", "reveal-token"),
            (MODELS, "models", "list"),
        ] {
            let answer = program
                .invoke(
                    &tilo_actions::remote::Caller::local_person(""),
                    request(provider, resource, action, "the secret"),
                )
                .wait_timeout(Duration::from_secs(30))
                .ok()
                .flatten()
                .expect("the daemon answers the program");
            let refused = match answer {
                Err(error) => error.to_string(),
                Ok(receipt) => format!("{:?}", receipt.status),
            };
            assert!(
                refused.contains("reached through your desktop"),
                "a program of the person's is refused {provider}: {refused}"
            );
        }
        assert!(
            account.asked().is_empty(),
            "the seat's provider was never asked"
        );
        session.stop().unwrap();
    });
}

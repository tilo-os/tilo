//! The person's shell and the device's daemon call each other through the daemon's one dispatch
//! over the link the daemon hands the shell (`runtime_collab::wire::shell`): the shell's own
//! calls are answered as the person, and a provider the shell serves is called by the dispatch
//! as any provider registered on the device is, for as long as the link lasts.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime_collab::ShellClient;
use serde_json::{Map, json};
use tilo_action_services::TestProvider;
use tilo_actions::remote::{
    ActionProvider as _, CataloguePage, CatalogueRow, Completer, Confirmation, InvocationReceipt,
    InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationUpdate};

use crate::language::host;

const PROBE: &str = "so.tilo.probe";
const ASKS: &str = "session.asks";
const WAITS: &str = "session.waits";
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
            .provide(ASKS, &CataloguePage::default(), Arc::clone(&asks) as _)
            .await;
        assert!(
            taken.is_err(),
            "a provider served already is not taken over"
        );
        let foreign = shell
            .provide(PROBE, &CataloguePage::default(), Arc::clone(&asks) as _)
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
        // A provider that takes a call and never answers it while the link lasts.
        let held: Arc<Mutex<Vec<Completer<_>>>> = Arc::default();
        let waits = {
            let held = Arc::clone(&held);
            let page = offered(WAITS, "so.tilo.shell", "waits/one", "answer");
            Arc::new(
                TestProvider::new()
                    .with_catalogue(move |_, _| Ok(page.clone()))
                    .answering(move |_, _| {
                        let (answer, answered) = tilo_actions::remote::pending::<
                            Result<InvocationReceipt, tilo_actions::remote::ActionCallError>,
                        >();
                        held.lock().unwrap().push(answer);
                        answered
                    }),
            )
        };
        let served = shell
            .provide(
                WAITS,
                &offered(WAITS, "so.tilo.shell", "waits/one", "answer"),
                Arc::clone(&waits) as _,
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

/// `tilo do` where no desktop's socket answers reaches the device's daemon, which answers the
/// person's actions there: it no longer says a desktop session is needed. What the person's seat
/// keeps for them (the account this computer is signed in to, their model keys) is their
/// desktop's alone: a program of theirs at the same socket neither lists nor calls it.
#[cfg(target_os = "linux")]
#[test]
fn tilo_do_without_a_desktop_is_answered_by_the_daemon() {
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

        // What the shell serves on the device's dispatch, as a desktop's seat serves it.
        let board = echoing(offered(ASKS, "so.tilo.shell", "board/notes", "pin-note"));
        let _served = runtime
            .block_on(shell.provide(
                ASKS,
                &offered(ASKS, "so.tilo.shell", "board/notes", "pin-note"),
                Arc::clone(&board) as _,
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
            ))
            .unwrap();

        // `tilo do`, as the person types it, with no desktop's socket anywhere.
        let daemon = [tilo_control::client::ControlSocket::Daemon(socket.clone())];
        let typed = |name: &str, words: &[&str]| {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let words: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
            let code = tilo_control::front::run(
                &daemon,
                name,
                &words,
                true,
                &tilo_control::front::Spelling::tilo(),
                &mut tilo_control::front::Io {
                    out: &mut out,
                    err: &mut err,
                },
            );
            (
                code,
                String::from_utf8_lossy(&out).into_owned(),
                String::from_utf8_lossy(&err).into_owned(),
            )
        };
        // The device's own action.
        let (code, out, err) = typed("layout#read", &[]);
        assert_eq!(code, 0, "the daemon answers the person's call: {err}");
        let answered: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(answered["status"], "ok", "{out}");
        // The shell's, listed and called at the same socket.
        let (code, out, err) = typed("pin-note", &["echo=pinned by hand"]);
        assert_eq!(code, 0, "tilo do reaches what the shell serves: {err}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out).unwrap()["result"],
            json!("pinned by hand"),
            "{out}"
        );
        assert_eq!(
            board.asked().len(),
            1,
            "the shell's provider answered the call"
        );

        // The seat's: not listed to the command, and refused when called by name.
        let (code, _, _) = typed("reveal-token", &["echo=the token"]);
        assert_ne!(code, 0, "tilo do finds no account token to reveal");
        let program = tilo_action_services::LineClient::of_stream(
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

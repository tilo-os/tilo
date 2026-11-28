use std::io::{BufRead, BufReader, Write as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tilo_actions::remote::TestProvider;
use tilo_actions::remote::{self, CataloguePage, CatalogueResource, InvokeRequest};
use tilo_actions::{
    ActionAddress, ActionContract, ActionOutput, ActionState, ExecutionLocus, InputSpec,
    OperationKind,
};

use super::*;

fn row(provider: &str, (resource, label): (&str, &str), (id, title): (&str, &str)) -> CatalogueRow {
    CatalogueRow {
        offered: None,
        provider: provider.into(),
        app_id: "so.tilo.test".into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: resource.into(),
            kind: "test".into(),
            label: label.into(),
            presentation: None,
        },
        action: Some(ActionDescriptor {
            did: None,
            state: ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(resource, id),
            resource_kind: "test".into(),
            resource_label: label.into(),
            action_label: title.into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: ExecutionLocus::ClientLocal,
            kind: OperationKind::Document,
            contract: ActionContract::default(),
            input: InputSpec::None,
            params: Vec::new(),
            view: None,
        }),
        has_children: false,
        visibility: None,
        surface: None,
    }
}

/// What the fake desktop below offers: an app to open.
fn rows() -> Vec<CatalogueRow> {
    vec![row(
        "workspace:w1",
        ("app.so.tilo.notes", "Notes"),
        (remote::APPLICATION_OPEN_ACTION, "Open"),
    )]
}

/// A desktop's control socket answering frames as a session does, through
/// [`tilo_action_calls::serve`] over a provider that offers [`rows`] and completes
/// every invocation with `done`.
struct FakeDesktop {
    path: PathBuf,
    provider: Arc<TestProvider>,
}

impl FakeDesktop {
    fn serve(dir: &Path, name: &str) -> Self {
        let path = dir.join(name);
        let listener = UnixListener::bind(&path).expect("bind the fake socket");
        let provider = Arc::new(
            TestProvider::new()
                .with_catalogue(|_, query| Ok(CataloguePage::from_rows("1".into(), rows(), &query)))
                .completing(|_| ActionOutput {
                    value: Some("done".into()),
                    resources: Vec::new(),
                }),
        );
        let serving = Arc::clone(&provider);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let provider = Arc::clone(&serving);
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().expect("clone");
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { return };
                        let frame = RemoteActionFrame::decode(line.as_bytes()).expect("a frame");
                        let caller = remote::Caller::local_person("person");
                        let answer = tilo_action_calls::serve(&provider, &caller, frame.message)
                            .and_then(|pending| pending.wait_timeout(Duration::from_secs(5)).ok())
                            .flatten()
                            .expect("an answer");
                        let bytes = RemoteActionFrame::new(answer).encode().expect("encode");
                        writer.write_all(&bytes).expect("write");
                        writer.write_all(b"\n").expect("write");
                    }
                });
            }
        });
        Self { path, provider }
    }

    fn invoked(&self) -> Vec<InvokeRequest> {
        self.provider.asked()
    }
}

use std::time::Duration;

/// The admin socket as an app's process meets it: one `not_permitted`
/// answer, then the connection closes.
fn refusing_admin(dir: &Path) -> PathBuf {
    let path = dir.join("admin-control.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut line = String::new();
            let _ = BufReader::new(stream.try_clone().expect("clone")).read_line(&mut line);
            let _ = writeln!(
                stream,
                "{}",
                serde_json::json!({"err": {"code": "not_permitted", "message": "an app's process"}})
            );
        }
    });
    path
}

struct Ran {
    code: u8,
    out: String,
    err: String,
}

fn ran(run: impl FnOnce(&mut Io<'_>) -> u8) -> Ran {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(&mut Io {
        out: &mut out,
        err: &mut err,
    });
    Ran {
        code,
        out: String::from_utf8(out).expect("utf-8"),
        err: String::from_utf8(err).expect("utf-8"),
    }
}

fn args(request: &InvokeRequest) -> Value {
    match &request.argument {
        InvokeArgument::Json(args) => Value::Object(args.clone()),
        InvokeArgument::None => Value::Null,
        InvokeArgument::Text(text) => Value::String(text.clone()),
    }
}

/// A verb that names its own action (`tilo machine show`) says its answer as
/// the lines a person reads, or with `--json` as Tilo's typed answer alone.
#[test]
fn a_named_verb_says_its_answer_as_lines_or_typed_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let desktop = FakeDesktop::serve(dir.path(), "control.sock");
    let sockets = [ControlSocket::Named(desktop.path.clone())];
    let spelling = Spelling::tilo();
    let target = RemoteActionTarget {
        provider: "tilo.machines".into(),
        address: ActionAddress::new("machines", "machine.show"),
    };
    let asking = serde_json::json!({"plan": "0123456789abcdef0123456789abcdef"});

    let typed = ran(|io| {
        let answer = call_target(&sockets, target.clone(), "Show", &asking, &spelling);
        say(answer, true, io)
    });
    assert_eq!(typed.code, 0, "{}", typed.err);
    let answer: Value = serde_json::from_str(&typed.out).expect("the answer is JSON");
    assert_eq!(answer["status"], "ok");
    assert_eq!(answer["result"], "done", "the action's own answer, typed");
    assert!(answer["invocation"].is_string(), "{}", typed.out);

    let spoken = ran(|io| {
        say(
            call_target(&sockets, target, "Show", &asking, &spelling),
            false,
            io,
        )
    });
    assert_eq!(spoken.code, 0, "{}", spoken.err);
    assert_eq!(spoken.out, "ok\ndone\n");
    assert_eq!(desktop.invoked().len(), 2);
    let asked = args(&desktop.invoked()[0]);
    assert_eq!(asked["plan"], "0123456789abcdef0123456789abcdef");
}

#[test]
fn open_finds_an_app_by_name_and_opens_files_in_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let desktop = FakeDesktop::serve(dir.path(), "control.sock");
    let sockets = [ControlSocket::Named(desktop.path.clone())];
    let cwd = Path::new("/home/me");
    let spelling = Spelling::tilo();

    let opened = ran(|io| open(&sockets, "notes", &[], cwd, &spelling, io));
    assert_eq!(opened.code, 0, "{}", opened.err);
    let with_file = ran(|io| open(&sockets, "Notes", &["plan.md".into()], cwd, &spelling, io));
    assert_eq!(with_file.code, 0, "{}", with_file.err);
    let missing = ran(|io| open(&sockets, "chess", &[], cwd, &spelling, io));
    assert_eq!(missing.err, "There is no app called “chess”.\n");

    let invoked = desktop.invoked();
    assert_eq!(invoked[0].target.address.resource, "app.so.tilo.notes");
    assert_eq!(invoked[1].target.provider, "workspace:w1");
    assert_eq!(
        invoked[1].target.address.resource,
        "app.open-with:so.tilo.notes"
    );
    assert_eq!(
        args(&invoked[1]),
        serde_json::json!({"open": ["/home/me/plan.md"]})
    );
    assert_eq!(complete_apps(&sockets, "no"), ["notes"]);
}

/// From inside an app the admin socket turns the process away, and the
/// same frames go to the session's MCP server, which acts with that app's
/// scope.
#[test]
fn inside_an_app_the_frames_go_to_the_mcp_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let admin = refusing_admin(dir.path());
    let agent = FakeDesktop::serve(dir.path(), "agent.sock");
    let sockets = [
        ControlSocket::Admin(admin),
        ControlSocket::Agent(agent.path.clone()),
    ];
    let target = RemoteActionTarget {
        provider: "so.tilo.session".into(),
        address: ActionAddress::new("desktop:chrome", "new-terminal-window"),
    };
    let done = ran(|io| {
        let spelling = Spelling::tilo();
        say(
            call_target(&sockets, target, "New", &Value::Null, &spelling),
            false,
            io,
        )
    });
    assert_eq!(done.code, 0, "{}", done.err);
    assert_eq!(agent.invoked().len(), 1);
}

#[test]
fn a_command_with_no_daemon_answering_says_how_to_look() {
    let opened = ran(|io| open(&[], "notes", &[], Path::new("/"), &Spelling::tilo(), io));
    assert_eq!(opened.code, 1);
    assert_eq!(
        opened.err,
        "Tilo isn't answering on this computer: the session that holds your workspaces is not \
         running. Run `tilo status` to see how it stands.\n"
    );
}

#[test]
fn files_become_absolute_and_links_stay_as_they_are() {
    let cwd = Path::new("/home/me/work");
    assert_eq!(open_item("plan.md", cwd), "/home/me/work/plan.md");
    assert_eq!(open_item("/etc/hosts", cwd), "/etc/hosts");
    assert_eq!(open_item("https://tilo.so/a?b", cwd), "https://tilo.so/a?b");
    assert_eq!(open_item("mailto:me@tilo.so", cwd), "mailto:me@tilo.so");
    assert_eq!(
        open_item("C:", cwd),
        "/home/me/work/C:",
        "no scheme without a body"
    );
}

#[test]
fn an_answer_says_what_ran_and_what_it_made() {
    let answer = serde_json::json!({"status": "ok", "result": "done", "resources": ["notes/Todo"]});
    assert_eq!(answer_text(&answer), "ok\ndone\n-> notes/Todo");
    // A list of records reads as a table, each other field as a line, in the order the action
    // answered them, and a link comes first.
    let invited = serde_json::json!({"status": "ok", "path": "workspace/invitations",
        "result": {"link": "https://tilo.sh/i/7", "role": "editor",
            "members": [{"name": "Ada", "role": "owner"}, {"name": "Grace Hopper", "role": "editor"}]}});
    assert_eq!(
        answer_text(&invited),
        "https://tilo.sh/i/7\nok workspace/invitations\nrole: editor\nmembers:\n  \
         name          role\n  Ada           owner\n  Grace Hopper  editor"
    );
}

/// A desktop whose every call waits in its asked record for the person's yes, as a call the
/// workspace asks them first does: followed, it stays under way, its progress naming the record,
/// until a resolve of that record answers it; approved, it ends done, declined, cancelled. What
/// it was answered is kept.
struct AskingDesktop {
    path: PathBuf,
    resolved: Arc<std::sync::Mutex<Vec<InvokeRequest>>>,
}

impl AskingDesktop {
    const RECORD: &'static str = "w1/7d1c";

    fn serve(dir: &Path) -> Self {
        let path = dir.join("asking.sock");
        let listener = UnixListener::bind(&path).expect("bind the fake socket");
        let resolved = Arc::new(std::sync::Mutex::new(Vec::<InvokeRequest>::new()));
        let answers = Arc::clone(&resolved);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let answers = Arc::clone(&answers);
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().expect("clone");
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { return };
                        let frame = RemoteActionFrame::decode(line.as_bytes()).expect("a frame");
                        let answer = match frame.message {
                            RemoteActionMessage::Invoke(request)
                                if tilo_actions::remote::asked::is_resolve(&request.target) =>
                            {
                                let invocation = request.invocation;
                                answers.lock().unwrap().push(request);
                                RemoteActionMessage::Receipt(remote::InvocationReceipt {
                                    invocation,
                                    status: ReceiptStatus::Completed {
                                        output: ActionOutput::default(),
                                    },
                                })
                            }
                            RemoteActionMessage::Invoke(request) => {
                                RemoteActionMessage::Receipt(remote::InvocationReceipt {
                                    invocation: request.invocation,
                                    status: ReceiptStatus::Accepted,
                                })
                            }
                            RemoteActionMessage::Observe {
                                correlation,
                                invocation,
                                ..
                            } => {
                                let answered = answers
                                    .lock()
                                    .unwrap()
                                    .last()
                                    .map(|answer| args(answer)["resolution"] == "approve");
                                let update = match answered {
                                    None => InvocationUpdate::Progress {
                                        invocation,
                                        sequence: 1,
                                        phase: tilo_actions::remote::asked::ASKED_PHASE.into(),
                                        completed: None,
                                        total: None,
                                        message: Some(Self::RECORD.into()),
                                    },
                                    Some(true) => InvocationUpdate::Succeeded {
                                        invocation,
                                        output: ActionOutput {
                                            value: Some("sent".into()),
                                            resources: Vec::new(),
                                        },
                                    },
                                    Some(false) => InvocationUpdate::Cancelled {
                                        invocation,
                                        message: Some("declined".into()),
                                    },
                                };
                                RemoteActionMessage::Update {
                                    invocation,
                                    correlation: Some(correlation),
                                    state: Some(remote::InvocationState {
                                        update,
                                        untrusted: false,
                                        left_at: None,
                                        link: None,
                                        read_answered: false,
                                    }),
                                }
                            }
                            other => panic!("the asking desktop answers no {other:?}"),
                        };
                        let bytes = RemoteActionFrame::new(answer).encode().expect("encode");
                        writer.write_all(&bytes).expect("write");
                        writer.write_all(b"\n").expect("write");
                    }
                });
            }
        });
        Self { path, resolved }
    }

    /// The person's `resolution` (`approve`, `decline`) of the record, made where they answer
    /// it, on their session or another device of theirs, never on the command's connection.
    fn answer(&self, resolution: &str) {
        let mut answer = Map::new();
        answer.insert("resolution".into(), resolution.into());
        self.resolved.lock().unwrap().push(InvokeRequest {
            asks: false,
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: tilo_actions::remote::WORKSPACE_INVOCATIONS_PROVIDER.into(),
                address: ActionAddress::new("7d1c", tilo_actions::remote::asked::RESOLVE),
            },
            argument: InvokeArgument::Json(answer),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        });
    }

    fn call(&self) -> Result<Value, String> {
        let sockets = [ControlSocket::Named(self.path.clone())];
        let mut desktop = Desktop::connect(&sockets).expect("the asking desktop");
        let mut args = Map::new();
        args.insert("to".into(), "bo@example.test".into());
        invoke(
            &mut desktop,
            RemoteActionTarget {
                provider: "workspace:w1".into(),
                address: ActionAddress::new("mail.draft:1", "send"),
            },
            args,
            ("Send", &Spelling::tilo()),
        )
    }
}

/// A call the workspace asks the person first waits in its asked record until the person answers
/// it where they answer questions: the command, a program of theirs, never answers its own
/// question. Their yes runs it; their no ends it with nothing done.
#[test]
fn a_call_that_asks_first_waits_for_the_persons_own_answer() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (resolution, ends) in [
        ("approve", Ok("sent")),
        ("decline", Err("“Send” was cancelled.")),
    ] {
        let desktop = Arc::new(AskingDesktop::serve(dir.path()));
        let calling = Arc::clone(&desktop);
        let call = std::thread::spawn(move || calling.call());
        std::thread::sleep(Duration::from_millis(200));
        assert!(!call.is_finished(), "it waits for the person");
        desktop.answer(resolution);
        let answer = call.join().expect("the call ends");
        assert_eq!(
            answer
                .as_ref()
                .map(|answer| answer["result"].as_str().unwrap_or_default()),
            ends.map_err(str::to_owned).as_ref().map(|sent| *sent),
            "{resolution}"
        );
        assert_eq!(
            desktop.resolved.lock().unwrap().len(),
            1,
            "the command answered nothing itself"
        );
        std::fs::remove_file(&desktop.path).expect("the socket goes");
    }
}

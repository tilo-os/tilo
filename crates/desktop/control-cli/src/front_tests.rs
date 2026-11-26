use std::io::{BufRead, BufReader, Write as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tilo_actions::remote::TestProvider;
use tilo_actions::remote::{self, CataloguePage, CatalogueResource, InvokeRequest};
use tilo_actions::{
    ActionAddress, ActionContract, ActionOutput, ActionParam, ActionState, ExecutionLocus,
    InputSpec, OperationKind, ParamType,
};

use super::*;

fn row(
    provider: &str,
    (resource, label): (&str, &str),
    (id, title): (&str, &str),
    params: Vec<ActionParam>,
) -> CatalogueRow {
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
            params,
            view: None,
        }),
        has_children: false,
        visibility: None,
        surface: None,
    }
}

/// What the fake desktop below offers: the shell's own action, an action two
/// places offer under one id, one with a typed parameter, and an app.
fn rows() -> Vec<CatalogueRow> {
    let named = |name: &'static str, ty| vec![ActionParam::required(name, ty, "")];
    vec![
        row(
            "so.tilo.session",
            ("desktop:chrome", "Desktop"),
            ("new-terminal-window", "New terminal window"),
            Vec::new(),
        ),
        row(
            "7",
            ("notes.page:1", "Todo"),
            ("rename", "Rename note"),
            named("title", ParamType::Text),
        ),
        row(
            "8",
            ("sheets.sheet:1", "Budget"),
            ("rename", "Rename"),
            named("name", ParamType::Text),
        ),
        row(
            "so.tilo.session",
            ("session:audio", "Sound"),
            ("set-volume", "Set volume"),
            named("level", ParamType::Integer),
        ),
        row(
            "workspace:w1",
            ("app.so.tilo.notes", "Notes"),
            (remote::APPLICATION_OPEN_ACTION, "Open"),
            Vec::new(),
        ),
    ]
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

#[test]
fn do_picks_by_id_title_or_place_and_invokes_the_one_frame() {
    let dir = tempfile::tempdir().expect("tempdir");
    let desktop = FakeDesktop::serve(dir.path(), "control.sock");
    let sockets = [ControlSocket::Named(desktop.path.clone())];
    let spelling = Spelling::tilo();

    let done = ran(|io| run(&sockets, "new terminal window", &[], false, &spelling, io));
    assert_eq!(
        (done.code, done.out.as_str()),
        (0, "ok\ndone\n"),
        "{}",
        done.err
    );
    let renamed = ran(|io| {
        run(
            &sockets,
            "todo#rename",
            &["Groceries".into()],
            false,
            &spelling,
            io,
        )
    });
    assert_eq!(renamed.code, 0, "{}", renamed.err);
    let louder = ran(|io| run(&sockets, "set-volume", &["40".into()], false, &spelling, io));
    assert_eq!(louder.code, 0, "{}", louder.err);
    let invoked = desktop.invoked();
    assert_eq!(invoked.len(), 3);
    assert_eq!(invoked[0].target.provider, "so.tilo.session");
    assert_eq!(invoked[0].target.address.action, "new-terminal-window");
    assert_eq!(invoked[1].target.provider, "7");
    assert_eq!(args(&invoked[1]), serde_json::json!({"title": "Groceries"}));
    assert_eq!(args(&invoked[2]), serde_json::json!({"level": 40}));

    let ambiguous = ran(|io| run(&sockets, "rename", &["x".into()], false, &spelling, io));
    assert_eq!(ambiguous.code, 1);
    assert!(
        ambiguous.err.contains("tilo do 7:notes.page:1#rename")
            && ambiguous.err.contains("tilo do 8:sheets.sheet:1#rename"),
        "each suggestion is the full address that runs as typed: {}",
        ambiguous.err
    );
    let unknown = ran(|io| run(&sockets, "fly", &[], false, &spelling, io));
    assert_eq!(unknown.code, 1);
    assert_eq!(desktop.invoked().len(), 3, "nothing unclear is invoked");
    // A suggestion typed back finds the one row it names.
    let picked = ran(|io| {
        run(
            &sockets,
            "7:notes.page:1#rename",
            &["x".into()],
            false,
            &spelling,
            io,
        )
    });
    assert_eq!(picked.code, 0, "{}", picked.err);
    assert_eq!(desktop.invoked()[3].target.provider, "7");

    let json = ran(|io| {
        run(
            &sockets,
            "set-volume",
            &["level=5".into()],
            true,
            &spelling,
            io,
        )
    });
    let typed: Value = serde_json::from_str(&json.out).expect("typed JSON");
    assert_eq!(typed["status"], "ok");
    assert_eq!(typed["result"], "done");

    let listed = ran(|io| list(&sockets, "", &spelling, io));
    assert_eq!(listed.code, 0, "{}", listed.err);
    assert!(
        listed
            .out
            .contains("new-terminal-window  New terminal window"),
        "{}",
        listed.out
    );
    assert!(listed.out.contains("todo#rename"), "{}", listed.out);

    let shown = ran(|io| show(&sockets, "todo#rename", &spelling, io));
    assert_eq!(shown.code, 0, "{}", shown.err);
    assert!(
        shown.out.starts_with("Rename note\nTodo\n"),
        "{}",
        shown.out
    );
    assert!(
        shown.out.contains("tilo do todo#rename title=…"),
        "{}",
        shown.out
    );
}

/// One action has one entry: a provider that offers the same address twice,
/// with another row between the copies, is still one action to pick.
#[test]
fn one_address_is_one_action_however_often_it_is_offered() {
    let duplicate = row("p", ("res", "Res"), ("do-thing", "Do thing"), Vec::new());
    let elsewhere = row(
        "p",
        ("other", "Other"),
        ("other-thing", "Other thing"),
        Vec::new(),
    );
    let offers: Vec<Offer> = [duplicate.clone(), elsewhere, duplicate]
        .into_iter()
        .filter_map(Offer::of)
        .collect();
    let found = find(&offers, "do-thing", &Spelling::tilo()).expect("one action, not two");
    assert_eq!(found.provider, "p");
    assert_eq!(found.action.address.resource, "res");
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
    let done = ran(|io| {
        run(
            &sockets,
            "new-terminal-window",
            &[],
            false,
            &Spelling::tilo(),
            io,
        )
    });
    assert_eq!(done.code, 0, "{}", done.err);
    assert_eq!(agent.invoked().len(), 1);
}

#[test]
fn the_listing_with_no_daemon_answering_says_how_to_look() {
    let listed = ran(|io| list(&[], "", &Spelling::tilo(), io));
    assert_eq!(listed.code, 1);
    assert_eq!(
        listed.err,
        "Tilo isn't answering on this computer. Run `tilo status` to see how it stands.\n"
    );
}

#[test]
fn loose_options_are_json_where_they_parse() {
    let args = arguments(&["count=3".into(), "--title=Groceries".into()]).expect("args");
    assert_eq!(args["count"], 3);
    assert_eq!(args["title"], "Groceries");
    assert_eq!(
        arguments(&["oops".into()]).unwrap_err(),
        "Write “oops” as name=value."
    );
}

#[test]
fn a_value_written_at_dash_is_read_from_standard_input_as_text() {
    let mut input = &b"sk-secret-123\n"[..];
    let args =
        arguments_reading(&["key=@-".into(), "provider=p1".into()], &mut input).expect("args");
    assert_eq!(args["key"], "sk-secret-123");
    assert_eq!(args["provider"], "p1");
    let mut input = &b"1"[..];
    assert_eq!(
        arguments_reading(&["a=@-".into(), "b=@-".into()], &mut input).unwrap_err(),
        "Only one value is read from standard input; “b” is a second."
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

    fn call(&self, prompt: &mut dyn FnMut(&str) -> Option<bool>) -> Result<Value, String> {
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
            prompt,
        )
    }
}

/// A call the workspace asks the person first waits in its asked record: `tilo do` asks them at
/// its own prompt, once, showing what it does and with what, and their answer is that record's
/// own resolve on the same connection. Their yes runs it; their no ends it with nothing done.
#[test]
fn a_call_that_asks_first_is_confirmed_at_tilos_own_prompt() {
    let dir = tempfile::tempdir().expect("tempdir");

    let yes = AskingDesktop::serve(dir.path());
    let mut asked = Vec::new();
    let answer = yes.call(&mut |question| {
        asked.push(question.to_owned());
        Some(true)
    });
    assert_eq!(answer.expect("it runs on the yes")["result"], "sent");
    assert_eq!(asked.len(), 1, "asked once: {asked:?}");
    assert!(
        asked[0].contains("“Send”") && asked[0].contains("to=bo@example.test"),
        "{asked:?}"
    );
    let resolved = yes.resolved.lock().unwrap().clone();
    assert_eq!(resolved.len(), 1, "{resolved:?}");
    assert_eq!(
        resolved[0].target.provider,
        tilo_actions::remote::WORKSPACE_INVOCATIONS_PROVIDER
    );
    assert_eq!(
        resolved[0].target.address,
        ActionAddress::new("7d1c", tilo_actions::remote::asked::RESOLVE)
    );
    assert_eq!(args(&resolved[0])["resolution"], "approve");
    assert!(
        resolved[0].confirmation.is_none(),
        "the CLI writes no approve itself"
    );

    std::fs::remove_file(&yes.path).expect("the socket goes");
    let no = AskingDesktop::serve(dir.path());
    let declined = no.call(&mut |_| Some(false));
    assert!(declined.is_err(), "{declined:?}");
    assert_eq!(
        args(&no.resolved.lock().unwrap()[0])["resolution"],
        "decline"
    );
}

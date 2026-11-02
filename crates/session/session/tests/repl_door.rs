//! The REPL at the session's agent door, on the headless session with the tilolang shell as the
//! whole screen: `tilolang repl --door` asks to act as the person, the person admits it, and the
//! lines it sends are the agent's own calls (`tilo_ls`, `tilo`), run as the person.
//!
//! The person admits it either on the sheet the door's question opens, pressed as they press it,
//! or ahead of time, by naming the program when they start the session (`--admit-repl`). A note
//! placed on the arrangement by a `tilo` program stands on the shell's screen, read from what the
//! shell lays out for assistive technology, and is the person's step, which `undo` takes back.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_service::{Questions, Service as _, Started};
use tilo_session::{ActionPlane, Options, Session};
use tilo_sheets::portal::PortalEffect;
use tilo_surface_host::shell_part::ShellLayer;
use tilolang_tilo::host::Host;
use tilolang_tilo::records_service::RecordService;
use tilolang_tilo::runtime::Principal;

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const PERSON: &str = "Pat";
const NOTES: &str = "so.tilo.lang.notes";
const NOTE_TITLE: &str = "Groceries";
const BTN_LEFT: u32 = 0x110;

/// A notebook kept by the notes app, published as the session publishes a workspace's records,
/// and one note in it titled [`NOTE_TITLE`], made through the app's own actions.
fn notebook() -> (RecordService, String) {
    let mut record = tilo_resource::Doc::new();
    let host = Host::new(&record, Principal::new(PERSON), || None).unwrap();
    let notes = host
        .load_builtin(
            include_str!("../../../../apps/tl/notes/app.tl"),
            include_str!("../../../../apps/tl/notes/app.ids"),
        )
        .unwrap();
    host.root_of(NOTES).unwrap();
    for committed in host.take_committed() {
        let action = &committed.action;
        let context =
            ui::tilo_actions::ActionContext::for_resource("workspace", &action.kind, &action.kind)
                .with_invoker(ui::tilo_actions::ActionInvoker::actor(PERSON));
        host.with_schemas(|| {
            ui::tilo_actions::apply_local_with_context(
                &mut record,
                action,
                action.encode().unwrap(),
                Some(PERSON.as_bytes()),
                context,
            )
        })
        .unwrap();
        record.commit();
        host.sync_from(&record);
    }
    let action = |name: &str| {
        notes
            .meta
            .actions
            .iter()
            .find(|action| action.name == name)
            .unwrap()
            .id
            .clone()
    };
    let mut service = RecordService::new(record, host, PERSON);
    let book = service
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{NOTES}.Notes"))
        .unwrap();
    let Started::Done(made) = service
        .invoke(book.id.as_str(), &action("new_note"), &Map::new())
        .unwrap()
    else {
        panic!("a tilolang action is done once committed");
    };
    let note = made.resources.first().cloned().unwrap();
    service
        .invoke(
            &note,
            &action("retitle"),
            json!({ "title": NOTE_TITLE }).as_object().unwrap(),
        )
        .unwrap();
    service.settle();
    (service, note)
}

/// A directory only this user reaches, for the door and the plane.
fn private_home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the session in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the session's directory is this user's");
    root
}

/// The headless session with the shell as the screen, the notebook published, its agent door
/// at `root`, admitting the programs `admit_repl` to act as the person.
fn session(
    root: &std::path::Path,
    questions: &Questions,
    admit_repl: Vec<std::path::PathBuf>,
) -> (Session, String) {
    let (notebook, note) = notebook();
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: Some(ShellLayer::Screen),
            collab: false,
            services: vec![Box::new(notebook)],
            questions: Some(questions.clone()),
            action_plane: ActionPlane::At(root.join("plane.sock")),
            agent_door: Some(root.join("agent.sock")),
            admit_repl,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    (session, note)
}

/// The REPL's end of the door, on a thread of its own so the session runs while it waits: it
/// sends each request it is handed and hands back the answer to it.
struct Repl {
    asks: Sender<Value>,
    answers: Receiver<Value>,
    next: u64,
}

impl Repl {
    /// Connect to the door at `socket` as `tilolang repl` does: asking to act as the person.
    fn connect(socket: &std::path::Path) -> Self {
        let socket = socket.to_path_buf();
        let (asks, asked) = std::sync::mpsc::channel::<Value>();
        let (answer, answers) = std::sync::mpsc::channel::<Value>();
        std::thread::spawn(move || {
            let stream = UnixStream::connect(&socket).expect("the agent door's socket");
            let mut writer = stream.try_clone().expect("a second handle on the door");
            let mut reader = BufReader::new(stream);
            for request in asked {
                writer
                    .write_all(format!("{request}\n").as_bytes())
                    .expect("the door reads");
                let Some(id) = request.get("id").cloned() else {
                    continue;
                };
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let reply: Value = serde_json::from_str(&line).expect("a JSON reply");
                    if reply.get("id") == Some(&id) {
                        answer.send(reply).expect("the test waits");
                        break;
                    }
                }
            }
        });
        let repl = Self {
            asks,
            answers,
            next: 1,
        };
        repl.asks
            .send(json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"experimental": {"so.tilo": {"actsAs": "person"}}},
                    "clientInfo": {"name": "tilolang repl", "version": "test"}
                }
            }))
            .unwrap();
        repl
    }

    /// The answer to the last request, running the session until it is in.
    fn answer(&self, session: &mut Session) -> Value {
        for _ in 0..30_000 {
            if let Ok(answer) = self.answers.try_recv() {
                return answer;
            }
            session.step(Duration::from_millis(1)).expect("step");
        }
        panic!("the door never answered");
    }

    /// Send `method` with `params` and wait for its answer.
    fn ask(&mut self, session: &mut Session, method: &str, params: &Value) -> Value {
        self.next = self.next.saturating_add(1);
        self.asks
            .send(json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}))
            .unwrap();
        self.answer(session)
    }

    /// Call the door's tool `name`: its text, and whether it is an error.
    fn call(&mut self, session: &mut Session, name: &str, arguments: &Value) -> (String, bool) {
        let reply = self.ask(
            session,
            "tools/call",
            &json!({"name": name, "arguments": arguments}),
        );
        let result = reply
            .get("result")
            .unwrap_or_else(|| panic!("{name} answered: {reply}"));
        let text = result["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|part| part["text"].as_str())
            .collect::<String>();
        (text, result["isError"] == true)
    }

    /// Admitted, it says it is initialized and lists its tools, as the REPL does.
    fn opened(&mut self, session: &mut Session) -> Vec<String> {
        let initialized = self.answer(session);
        assert!(
            initialized.pointer("/result/serverInfo").is_some(),
            "the REPL is admitted: {initialized}"
        );
        self.asks
            .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .unwrap();
        let listed = self.ask(session, "tools/list", &json!({}));
        listed["result"]["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect()
    }
}

/// Run frames until `done` holds; `false` when it never did.
fn reached(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) -> bool {
    for _ in 0..600 {
        session.step(Duration::from_millis(1)).expect("step");
        if done(session) {
            return true;
        }
    }
    false
}

#[track_caller]
fn until(session: &mut Session, what: &str, done: impl FnMut(&mut Session) -> bool) {
    assert!(
        reached(session, done),
        "{what}: not reached; the shell's refusals: {:?}; the session says: {:?}",
        session.shell_refusals(),
        session.accessible_labels()
    );
}

/// Where the shell's control named `name` stands on screen, when it does.
fn on_screen(session: &Session, name: &str) -> Option<(f64, f64, f64, f64)> {
    session.shell_control(name).filter(|&(x, y, w, h)| {
        w > 0.0
            && h > 0.0
            && x + w > 0.0
            && y + h > 0.0
            && x < f64::from(WIDTH)
            && y < f64::from(HEIGHT)
    })
}

/// Press the shell's control named `name` with the pointer, as the person does.
fn press(session: &mut Session, name: &str) {
    let (x, y, w, h) = on_screen(session, name)
        .unwrap_or_else(|| panic!("the shell shows {name}: {:?}", session.accessible_labels()));
    let (x, y) = (x + w / 2.0, y + h / 2.0);
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .unwrap();
    session.step(Duration::from_millis(1)).unwrap();
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed,
            })
            .unwrap();
        session.step(Duration::from_millis(1)).unwrap();
    }
}

/// Press the portal sheet's `effect`, as its control does.
fn press_sheet(session: &mut Session, effect: &str, args: &Value) {
    fn publisher(resources: &[tilo_actions::ResourceSpec], action: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            if resource.actions.iter().any(|offered| offered.id == action) {
                Some(resource.id.as_str().to_owned())
            } else {
                publisher(&resource.children, action)
            }
        })
    }
    let mut args = args.as_object().cloned().unwrap_or_default();
    if let Some(handle) = session
        .portal()
        .and_then(tilo_sheets::portal::PortalSeat::dialog)
        .map(|dialog| dialog.handle)
    {
        args.insert("handle".to_owned(), handle.into());
    }
    let sheet = publisher(&session.resources(), effect).expect("the sheet publishes that");
    session.invoke(&sheet, effect, &args).expect("press");
    session.step(Duration::from_millis(1)).expect("step");
}

/// The places of the arrangement that show `resource`.
fn placed(session: &Session, resource: &str) -> usize {
    session
        .arrangement()
        .borrow()
        .strips()
        .places()
        .filter(|place| place.record == resource)
        .count()
}

/// Lists the apps, places the note through a `tilo` program, sees it on the shell's screen as
/// the person's step, and takes it back with `undo`.
fn the_repl_works_as_the_person(session: &mut Session, repl: &mut Repl, note: &str) {
    let tools = repl.opened(session);
    assert!(
        tools.iter().any(|tool| tool == "tilo"),
        "the door runs programs: {tools:?}"
    );

    // `ls`: the apps, the session's own resources among them.
    let (apps, failed) = repl.call(session, "tilo_ls", &json!({}));
    assert!(!failed, "{apps}");
    assert!(
        apps.lines().any(|line| line.starts_with("session")),
        "the session's own resources are listed: {apps}"
    );
    let (desk, failed) = repl.call(session, "tilo_ls", &json!({"path": "session"}));
    assert!(!failed, "{desk}");
    assert!(
        desk.contains("shell:arrangement") || desk.contains("arrangement"),
        "the arrangement is among the session's resources: {desk}"
    );

    // `ls` lists each action as a program calls it, what it may go without included.
    let (chrome, failed) = repl.call(session, "tilo_ls", &json!({"path": "session/Desktop"}));
    assert!(!failed, "{chrome}");
    assert!(
        chrome
            .lines()
            .any(|line| line.starts_with("session.overview(open: Option<Bool> = .none)  ")),
        "the overview's action, as it is typed: {chrome}"
    );
    let (arranging, failed) =
        repl.call(session, "tilo_ls", &json!({"path": "session/arrangement"}));
    assert!(!failed, "{arranging}");
    let place = arranging
        .lines()
        .find(|line| line.starts_with("session.place("))
        .unwrap_or_else(|| panic!("`place`, as it is typed: {arranging}"));
    assert!(
        place.contains("resource: host.Resource") && place.contains("x: Option<Int> = .none"),
        "it takes the resource as a value, and may go without the rest: {place}"
    );
    // A resource that is a value goes by a reference, as an answer's do.
    // The notebook by its key: the launchpad's Notes card is another resource of that name.
    let book = note
        .split('/')
        .next()
        .and_then(|book| book.rsplit(':').next())
        .unwrap();
    let (notebook, failed) = repl.call(
        session,
        "tilo_ls",
        &json!({"path": format!("session/{book}")}),
    );
    assert!(!failed, "{notebook}");
    let reference = notebook
        .lines()
        .find(|line| line.contains(NOTE_TITLE))
        .and_then(|line| line.split_whitespace().next())
        .filter(|word| word.starts_with('n') && word[1..].chars().all(|c| c.is_ascii_digit()))
        .unwrap_or_else(|| panic!("the note goes by a reference: {notebook}"))
        .to_owned();
    // An action read as a value says how it is called.
    let (named, failed) = repl.call(session, "tilo", &json!({"program": "session.overview"}));
    assert!(failed, "{named}");
    assert!(
        named.contains("call it: `session.overview(open: Option<Bool> = .none)`"),
        "{named}"
    );

    // The framed canvas, as the person shows it.
    until(session, "the preset switch", |session| {
        on_screen(session, "Frames").is_some()
    });
    press(session, "Frames");
    until(session, "the canvas", |session| {
        on_screen(session, "Typed").is_some()
    });

    // A `tilo` program places the note on the arrangement, as the person.
    let (placed_it, failed) = repl.call(
        session,
        "tilo",
        &json!({"program": format!("session.place(resource: {reference}, x: 80, y: 60)")}),
    );
    assert!(!failed, "the program ran: {placed_it}");
    assert!(placed_it.contains("step 1"), "{placed_it}");
    until(session, "the note's title on the canvas", |session| {
        on_screen(session, NOTE_TITLE).is_some()
    });
    assert_eq!(placed(session, note), 1, "one place shows the note");
    let (who, verb) = session
        .arrangement()
        .borrow_mut()
        .last_step()
        .expect("it is a step of the arrangement's history");
    assert_eq!(
        (who.as_str(), verb.as_str()),
        (PERSON, "place"),
        "the person's step, not an agent's"
    );

    // `undo` takes the step back.
    let (undone, failed) = repl.call(session, "tilo", &json!({"program": "undo"}));
    assert!(!failed, "{undone}");
    until(session, "the note off the canvas", |session| {
        placed(session, note) == 0 && on_screen(session, NOTE_TITLE).is_none()
    });
}

/// The person admits the REPL on the sheet the door's question opens.
#[test]
fn the_person_admits_the_repl_on_the_sheet_and_it_works_as_them() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let questions = Questions::new();
    let (mut session, note) = session(root.path(), &questions, Vec::new());
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });

    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    until(&mut session, "the question", |_| {
        questions.pending().len() == 1
    });
    let shown = session
        .portal()
        .and_then(tilo_sheets::portal::PortalSeat::dialog)
        .expect("the question stands on the portal's sheet");
    assert_eq!(shown.title, "Act as you in “Desk”", "what it asks");
    assert!(
        shown.heading.starts_with("tilolang repl ("),
        "who asks: {}",
        shown.heading
    );
    press_sheet(&mut session, PortalEffect::SELECT.id, &json!({"index": 0}));
    press_sheet(&mut session, PortalEffect::CONFIRM.id, &json!({}));

    the_repl_works_as_the_person(&mut session, &mut repl, &note);
}

/// The person named the REPL when they started the session: it is admitted without a question.
#[test]
fn a_repl_named_when_the_session_starts_is_admitted_without_asking() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let questions = Questions::new();
    // The REPL here is this test's own program: what the kernel names the client by.
    let program = std::env::current_exe().expect("this test's program");
    let (mut session, note) = session(root.path(), &questions, vec![program]);
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });

    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    the_repl_works_as_the_person(&mut session, &mut repl, &note);
    assert!(questions.pending().is_empty(), "nobody was asked");
}

/// Placing what a text finds, twice, through the door: each place is the person's step, and the
/// session keeps answering, the connection that asked and a new one alike.
#[test]
fn placing_what_text_finds_twice_keeps_the_session_answering() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let questions = Questions::new();
    let program = std::env::current_exe().expect("this test's program");
    let (mut session, note) = session(root.path(), &questions, vec![program]);
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

    for placed_before in 0..2 {
        let (answer, failed) = repl.call(
            &mut session,
            "tilo",
            &json!({"program": format!(
                "session.place_found(text: \"{NOTE_TITLE}\", view: .none, x: .some({}), y: .some(60))",
                80 + placed_before * 400
            )}),
        );
        assert!(!failed, "the program ran: {answer}");
        until(&mut session, "the note placed once more", |session| {
            placed(session, &note) == placed_before as usize + 1
        });
    }

    // The connection that asked is still answered.
    let (apps, failed) = repl.call(&mut session, "tilo_ls", &json!({}));
    assert!(!failed, "{apps}");
    // And so is a new one.
    let mut second = Repl::connect(&root.path().join("agent.sock"));
    let tools = second.opened(&mut session);
    assert!(tools.iter().any(|tool| tool == "tilo"), "{tools:?}");
}

/// What the session's services publish (the seeded apps' records) is listed through the door
/// past the first page of the session's rows, page by page as its cursor reads on, and a note
/// is found by its title.
#[test]
fn a_record_the_sessions_services_publish_is_found_through_the_door() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let questions = Questions::new();
    let program = std::env::current_exe().expect("this test's program");
    let apps = tilo_session::seeded_apps::seeded(PERSON).expect("the seeded apps");
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: Some(ShellLayer::Screen),
            collab: false,
            services: vec![Box::new(apps)],
            questions: Some(questions.clone()),
            action_plane: ActionPlane::At(root.path().join("plane.sock")),
            agent_door: Some(root.path().join("agent.sock")),
            admit_repl: vec![program],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

    // Every page of the session's listing, as its cursors read on.
    let mut listed = String::new();
    let mut cursor = String::new();
    for _ in 0..20 {
        let (page, failed) = repl.call(
            &mut session,
            "tilo_ls",
            &json!({"path": "session", "cursor": cursor}),
        );
        assert!(!failed, "{page}");
        listed.push_str(&page);
        listed.push('\n');
        let Some(next) = page
            .rsplit_once("cursor \"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(next, _)| next.to_owned())
        else {
            break;
        };
        cursor = next;
    }
    for kind in [
        "tl.so.tilo.lang.tasks.",
        "tl.so.tilo.lang.chat.",
        "tl.so.tilo.lang.mail.",
    ] {
        assert!(
            listed.contains(kind),
            "the session lists the seeded {kind} records: {listed}"
        );
    }

    let (found, failed) = repl.call(&mut session, "tilo_find", &json!({"query": NOTE_TITLE}));
    assert!(!failed, "{found}");
    assert!(
        found.lines().any(|line| line.contains(NOTE_TITLE)),
        "the note is found by its title: {found}"
    );
}

/// What the seeded apps keep, placed through the door, is drawn on the canvas by its own app's
/// view, its content and not its title alone: a note found by its title shows its text. And a
/// text finds the resource it names even where many actions name it too: "Inbox" places the
/// inbox.
#[test]
fn a_placed_record_is_drawn_through_its_own_apps_view() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let questions = Questions::new();
    let program = std::env::current_exe().expect("this test's program");
    let apps = tilo_session::seeded_apps::seeded(PERSON).expect("the seeded apps");
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: Some(ShellLayer::Screen),
            collab: false,
            services: vec![Box::new(apps)],
            questions: Some(questions.clone()),
            action_plane: ActionPlane::At(root.path().join("plane.sock")),
            agent_door: Some(root.path().join("agent.sock")),
            admit_repl: vec![program],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    until(&mut session, "the preset switch", |session| {
        on_screen(session, "Frames").is_some()
    });
    press(&mut session, "Frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, "Typed").is_some()
    });
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

    let (answer, failed) = repl.call(
        &mut session,
        "tilo",
        &json!({"program": format!(
            "session.place_found(text: \"{NOTE_TITLE}\", view: .none, x: .some(80), y: .some(60))"
        )}),
    );
    assert!(!failed, "the program ran: {answer}");
    until(&mut session, "the note's text on the canvas", |session| {
        session.shell_says("Oat milk, eggs, sourdough")
    });

    // A text naming a resource that many actions name too finds the resource: "Inbox" is the
    // mailbox, under every conversation's "Move to inbox".
    let (answer, failed) = repl.call(
        &mut session,
        "tilo",
        &json!({"program": "session.place_found(text: \"Inbox\", view: .none, x: .some(640), y: .some(60))"}),
    );
    assert!(!failed, "the program ran: {answer}");
    let mailbox = |session: &Session| {
        let resources = session.resources();
        session
            .arrangement()
            .borrow()
            .strips()
            .places()
            .filter_map(|place| {
                tilo_actions::door::locate(
                    &resources,
                    &tilo_actions::ActionAddress::new(&place.record, "open"),
                )
                .map(|(resource, _)| (resource.kind.clone(), resource.label.clone()))
            })
            .find(|(kind, _)| kind == "tl.so.tilo.lang.mail.Mailbox")
    };
    until(&mut session, "the inbox placed", |session| {
        mailbox(session).is_some()
    });
    assert_eq!(
        mailbox(&session).map(|(_, label)| label).as_deref(),
        Some("Inbox"),
        "the mailbox placed is the inbox"
    );
}

/// A text naming what something is, not what it is called, places the one that kind means:
/// "my mails" is mail's Inbox, its mailboxes' declared primary, though no word of it says
/// "Inbox" and "my" names nothing.
#[test]
fn my_mails_on_the_canvas_places_mails_root() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let questions = Questions::new();
    let program = std::env::current_exe().expect("this test's program");
    let apps = tilo_session::seeded_apps::seeded(PERSON).expect("the seeded apps");
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: Some(ShellLayer::Screen),
            collab: false,
            services: vec![Box::new(apps)],
            questions: Some(questions.clone()),
            action_plane: ActionPlane::At(root.path().join("plane.sock")),
            agent_door: Some(root.path().join("agent.sock")),
            admit_repl: vec![program],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    until(&mut session, "the preset switch", |session| {
        on_screen(session, "Frames").is_some()
    });
    press(&mut session, "Frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, "Typed").is_some()
    });
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

    let (answer, failed) = repl.call(
        &mut session,
        "tilo",
        &json!({"program": "session.place_found(text: \"my mails\", view: .none, x: .some(80), y: .some(60))"}),
    );
    assert!(!failed, "the program ran: {answer}");
    let placed = |session: &Session| {
        let resources = session.resources();
        session
            .arrangement()
            .borrow()
            .strips()
            .places()
            .filter_map(|place| {
                tilo_actions::door::locate(
                    &resources,
                    &tilo_actions::ActionAddress::new(&place.record, "open"),
                )
                .map(|(resource, _)| (resource.kind.clone(), resource.label.clone()))
            })
            .collect::<Vec<_>>()
    };
    until(&mut session, "what \"my mails\" names placed", |session| {
        !placed(session).is_empty()
    });
    let found = placed(&session);
    assert_eq!(
        found
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        ["tl.so.tilo.lang.mail.Mail"],
        "\"my mails\" places Mail's root, which Mail draws as its inbox: {found:?}"
    );
}

use std::io::{BufRead, BufReader, Write as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::*;

/// What the fake desktop below lists: shell chrome, app actions with typed
/// parameters (two of them titled alike), and apps here and to get.
fn rows() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({"provider": "actions", "id": "window.new.terminal", "kind": "Actions",
            "title": "New terminal window", "params": []}),
        serde_json::json!({"provider": "app-actions", "id": "notes\u{1f}rename", "kind": "Actions",
            "title": "Rename note", "subtitle": "Todo",
            "params": [{"name": "title", "type": "text", "required": true, "description": "The new name"}]}),
        serde_json::json!({"provider": "app-actions", "id": "files\u{1f}rename", "kind": "Actions",
            "title": "Rename", "subtitle": "report.md",
            "params": [{"name": "name", "type": "text", "required": true}]}),
        serde_json::json!({"provider": "app-actions", "id": "sheets\u{1f}rename", "kind": "Actions",
            "title": "Rename", "subtitle": "Budget",
            "params": [{"name": "name", "type": "text", "required": true}]}),
        serde_json::json!({"provider": "app-actions", "id": "mixer\u{1f}volume", "kind": "Actions",
        "title": "Set volume",
        "params": [
            {"name": "level", "type": "integer", "required": true},
            {"name": "mode", "type": "text", "choices": ["quiet", "loud"]}
        ]}),
        serde_json::json!({"provider": "applications", "id": "so.tilo.calculator",
            "kind": "Apps", "title": "Calculator", "params": []}),
        serde_json::json!({"provider": "applications", "id": "so.tilo.notes",
            "kind": "Apps", "title": "Notes", "params": []}),
        serde_json::json!({"provider": "store-catalogue", "id": "app:so.tilo.chess",
            "kind": "Apps", "title": "Chess", "params": []}),
    ]
}

fn actions() -> Vec<Action> {
    rows().iter().filter_map(Action::from_row).collect()
}

/// The desktop's own matching, roughly: every word typed starts a word of
/// the title, or the query is in the id; `kinds` keeps apps.
fn enumerate(request: &serde_json::Value) -> serde_json::Value {
    let query = request["query"].as_str().unwrap_or_default().to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    let apps_only = request.get("kinds").is_some();
    let actions: Vec<serde_json::Value> = rows()
        .into_iter()
        .filter(|row| !apps_only || row["kind"] == "Apps")
        .filter(|row| {
            let title = row["title"].as_str().unwrap_or_default().to_lowercase();
            let id = row["id"].as_str().unwrap_or_default();
            words.is_empty()
                || id.contains(query.as_str())
                || words.iter().all(|word| {
                    title
                        .split(|c: char| !c.is_alphanumeric())
                        .any(|part| part.starts_with(word))
                })
        })
        .collect();
    serde_json::json!({"answer": {"query": query, "count": actions.len(), "actions": actions,
        "truncated": false}})
}

/// A desktop's answers: listings from [`enumerate`], invocations settled.
fn desktop_answer(request: &serde_json::Value) -> serde_json::Value {
    match request["op"].as_str() {
        Some("actions_enumerate") => enumerate(request),
        Some("actions_invoke") => serde_json::json!({"answer": {
            "provider": request["provider"], "id": request["id"],
            "receipt": {"update": {"state": "succeeded", "output": {"value": "done"}}}
        }}),
        _ => serde_json::json!({"err": {"code": "bad_request", "message": "?"}}),
    }
}

/// A socket answering each request line with `answer`, recording requests.
struct FakeDoor {
    path: PathBuf,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl FakeDoor {
    fn serve(
        dir: &Path,
        name: &str,
        answer: fn(&serde_json::Value) -> serde_json::Value,
        close_after_first: bool,
    ) -> Self {
        let path = dir.join(name);
        let listener = UnixListener::bind(&path).expect("bind the fake door");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let seen = Arc::clone(&seen);
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().expect("clone");
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { return };
                        let request: serde_json::Value =
                            serde_json::from_str(&line).expect("one JSON request per line");
                        let reply = answer(&request);
                        seen.lock().expect("requests").push(request);
                        if writeln!(writer, "{reply}").is_err() || close_after_first {
                            return;
                        }
                    }
                });
            }
        });
        Self { path, requests }
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().expect("requests").clone()
    }

    fn invocations(&self) -> Vec<serde_json::Value> {
        self.requests()
            .into_iter()
            .filter(|request| request["op"] == "actions_invoke")
            .collect()
    }
}

/// The admin socket as an app's process meets it: one `not_permitted`
/// answer, then the connection closes.
fn admin_refusal(_: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({"err": {"code": "not_permitted",
        "message": "a sandboxed process cannot use the admin control socket"}})
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

#[test]
fn names_are_title_slugs_made_unique_by_place_and_found_by_title_words() {
    let actions = actions();
    let names = display_names(&actions);
    assert_eq!(names[0], "new-terminal-window");
    assert_eq!(names[1], "rename-note");
    assert_eq!(names[2], "rename-report-md");
    assert_eq!(names[3], "rename-budget");
    assert_eq!(names[5], "calculator");
    let unique: std::collections::HashSet<&String> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "{names:?}");

    assert_eq!(pick("new-terminal-window", &actions), Pick::One(0));
    assert_eq!(pick("New terminal window", &actions), Pick::One(0));
    assert_eq!(pick("new term", &actions), Pick::One(0), "words of a title");
    assert_eq!(pick("rename-budget", &actions), Pick::One(3));
    assert_eq!(pick("rename", &actions), Pick::Many(vec![2, 3]));
    assert_eq!(pick("nothing-like-it", &actions), Pick::None);
    assert_eq!(
        pick("app-actions:notes\\x1frename", &actions),
        Pick::One(1),
        "the address `show` prints"
    );
}

#[test]
fn arguments_follow_the_schema_with_sentences_that_say_what_it_takes() {
    let actions = actions();
    let words = |words: &[&str]| {
        words
            .iter()
            .map(|word| (*word).to_string())
            .collect::<Vec<_>>()
    };
    let rename = &actions[1];
    assert_eq!(
        bind_args(rename, &words(&["Groceries"])).expect("a lone value"),
        serde_json::json!({"title": "Groceries"})
            .as_object()
            .cloned()
            .unwrap_or_default()
    );
    assert_eq!(
        bind_args(rename, &words(&["title=Big", "shop"])).expect_err("nowhere for shop"),
        "Say which option “shop” is for: title (text)."
    );
    let missing = bind_args(rename, &[]).expect_err("title is required");
    assert_eq!(
        missing,
        "“Rename note” needs title (text). It takes title (text)."
    );
    let unknown = bind_args(rename, &words(&["--colour", "red"])).expect_err("no such option");
    assert_eq!(
        unknown,
        "“Rename note” has no option “colour”. It takes title (text)."
    );

    let volume = &actions[4];
    assert_eq!(
        bind_args(volume, &words(&["40", "mode=quiet"])).expect("typed"),
        serde_json::json!({"level": 40, "mode": "quiet"})
            .as_object()
            .cloned()
            .unwrap_or_default()
    );
    assert_eq!(
        bind_args(volume, &words(&["level=loud"])).expect_err("not a number"),
        "level must be a whole number, not “loud”."
    );
    assert_eq!(
        bind_args(volume, &words(&["--level=3", "--mode", "medium"])).expect_err("not a choice"),
        "mode must be quiet or loud, not “medium”."
    );
    assert_eq!(
        bind_args(volume, &words(&["--json", r#"{"level": 7}"#])).expect("raw JSON"),
        serde_json::json!({"level": 7})
            .as_object()
            .cloned()
            .unwrap_or_default()
    );
    let terminal = &actions[0];
    assert_eq!(
        bind_args(terminal, &words(&["now"])).expect_err("takes nothing"),
        "“New terminal window” takes nothing."
    );
}

#[test]
fn open_resolves_an_app_by_word_name_or_id_like_search() {
    let actions = actions();
    assert_eq!(
        pick_app("calc", &actions),
        Some(5),
        "the desktop's best match"
    );
    assert_eq!(pick_app("Calculator", &actions), Some(5));
    assert_eq!(
        pick_app("notes", &actions),
        Some(6),
        "its name, not the note action"
    );
    assert_eq!(pick_app("so.tilo.calculator", &actions), Some(5));
    assert_eq!(
        pick_app("so.tilo.chess", &actions),
        Some(7),
        "an app to get"
    );
    assert_eq!(pick_app("x", &actions[..5]), None, "no app rows at all");

    let dir = tempfile::tempdir().expect("tempdir");
    let door = FakeDoor::serve(dir.path(), "control.sock", desktop_answer, false);
    let doors = [Door::Named(door.path.clone())];
    let spelling = Spelling::tilo();

    let opened = ran(|io| open(&doors, "calc", &[], dir.path(), &spelling, io));
    assert_eq!(opened.code, 0, "{}", opened.err);
    let invoked = door.invocations();
    assert_eq!(invoked.len(), 1);
    assert_eq!(invoked[0]["provider"], "applications");
    assert_eq!(invoked[0]["id"], "so.tilo.calculator");
    assert_eq!(invoked[0]["args"], serde_json::json!({}));
    let listed = door
        .requests()
        .into_iter()
        .find(|request| request["op"] == "actions_enumerate")
        .expect("a listing first");
    assert_eq!(listed["query"], "calc");
    assert_eq!(listed["kinds"], serde_json::json!(["app"]));

    let with_files = ran(|io| {
        open(
            &doors,
            "Notes",
            &["plan.md".to_string(), "https://tilo.so".to_string()],
            Path::new("/home/me"),
            &spelling,
            io,
        )
    });
    assert_eq!(with_files.code, 0, "{}", with_files.err);
    let invoked = door.invocations();
    let last = invoked.last().expect("an invocation");
    assert_eq!(last["provider"], "applications");
    assert_eq!(last["id"], "open-with:so.tilo.notes");
    assert_eq!(
        last["args"],
        serde_json::json!({"open": ["/home/me/plan.md", "https://tilo.so"]})
    );

    let absent = ran(|io| {
        open(
            &doors,
            "chess",
            &["game.pgn".to_string()],
            dir.path(),
            &spelling,
            io,
        )
    });
    assert_eq!(absent.code, 1);
    assert_eq!(
        absent.err,
        "Chess isn't on this device yet. Open it once to get it: tilo open chess\n"
    );
    let unknown = ran(|io| open(&doors[..0], "calc", &[], dir.path(), &spelling, io));
    assert_eq!(unknown.code, 1);
    assert_eq!(unknown.err, "This needs a Tilo desktop session.\n");
}

#[test]
fn do_validates_then_invokes_through_a_control_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let door = FakeDoor::serve(dir.path(), "control.sock", desktop_answer, false);
    let doors = [Door::Named(door.path.clone())];
    let spelling = Spelling::tilo();

    let done = ran(|io| run(&doors, "new terminal", &[], &spelling, io));
    assert_eq!(
        (done.code, done.out.as_str()),
        (0, "done\n"),
        "{}",
        done.err
    );
    let invoked = door.invocations();
    assert_eq!(invoked.len(), 1);
    assert_eq!(invoked[0]["provider"], "actions");
    assert_eq!(invoked[0]["id"], "window.new.terminal");
    assert_eq!(invoked[0]["wait_ms"], 5000);

    let renamed = ran(|io| run(&doors, "rename-note", &["Groceries".into()], &spelling, io));
    assert_eq!(renamed.code, 0, "{}", renamed.err);
    let invoked = door.invocations();
    assert_eq!(invoked[1]["id"], "notes\u{1f}rename");
    assert_eq!(
        invoked[1]["args"],
        serde_json::json!({"title": "Groceries"})
    );

    let refused = ran(|io| run(&doors, "set-volume", &["level=loud".into()], &spelling, io));
    assert_eq!(refused.code, 1);
    assert_eq!(refused.err, "level must be a whole number, not “loud”.\n");
    let ambiguous = ran(|io| run(&doors, "rename", &["x".into()], &spelling, io));
    assert_eq!(ambiguous.code, 1);
    assert!(
        ambiguous.err.contains("tilo do rename-report-md")
            && ambiguous.err.contains("tilo do rename-budget"),
        "{}",
        ambiguous.err
    );
    assert_eq!(
        door.invocations().len(),
        2,
        "nothing is invoked when its arguments are wrong or its name is unclear"
    );

    let shown = ran(|io| show(&doors, "rename-note", &spelling, io));
    assert_eq!(shown.code, 0);
    assert!(
        shown.out.starts_with("Rename note\nTodo\n"),
        "{}",
        shown.out
    );
    assert!(
        shown.out.contains("  title  text, required  The new name"),
        "{}",
        shown.out
    );
    assert!(
        shown.out.contains("tilo do rename-note title=…"),
        "{}",
        shown.out
    );
}

/// From inside an app — the Terminal app and what runs in it — the admin
/// socket turns the process away. The same commands then list and run
/// through the session's agent door, which acts with that terminal's
/// workspace scope; nothing is widened.
#[test]
fn inside_a_sandboxed_app_actions_list_and_run_through_the_agent_door() {
    let dir = tempfile::tempdir().expect("tempdir");
    let admin = FakeDoor::serve(dir.path(), "admin-control.sock", admin_refusal, true);
    let agent = FakeDoor::serve(dir.path(), "agent.sock", desktop_answer, false);
    let doors = [
        Door::Admin(admin.path.clone()),
        Door::Agent(agent.path.clone()),
    ];
    let spelling = Spelling::tilo();

    let listed = ran(|io| list(&doors, "", &spelling, io));
    assert_eq!(listed.code, 0, "{}", listed.err);
    assert!(listed.out.starts_with("Actions\n"), "{}", listed.out);
    assert!(
        listed
            .out
            .contains("  new-terminal-window  New terminal window\n"),
        "{}",
        listed.out
    );
    assert!(listed.out.contains("\nApps\n"), "{}", listed.out);

    let done = ran(|io| run(&doors, "new-terminal-window", &[], &spelling, io));
    assert_eq!(done.code, 0, "{}", done.err);
    assert_eq!(agent.invocations().len(), 1, "run through the agent door");
    assert!(
        admin.invocations().is_empty(),
        "nothing ran through the admin socket"
    );
    assert!(
        admin.requests().len() <= 2,
        "each command asked the admin socket once and moved on: {:?}",
        admin.requests()
    );

    // An agent door that turns the process away too ends in one sentence.
    let refusing = FakeDoor::serve(
        dir.path(),
        "refusing-agent.sock",
        |_| {
            serde_json::json!({"jsonrpc": "2.0", "id": null,
            "error": {"code": -32001, "message": "a sandboxed process cannot use the agent door"}})
        },
        true,
    );
    let doors = [Door::Admin(admin.path), Door::Agent(refusing.path)];
    let refused = ran(|io| list(&doors, "", &spelling, io));
    assert_eq!(refused.code, 1);
    assert_eq!(
        refused.err,
        "The desktop didn't let this terminal use its actions.\n"
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

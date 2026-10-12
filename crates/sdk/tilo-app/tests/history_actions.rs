#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! The history every framework app answers through the action door: undo
//! and redo of the caller's own steps (an agent's run), history with what
//! each step was given, checkpoints, restore whole or by item, diff, and
//! the view at a past version.

use serde_json::{Value, json};
use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Board {
    /// Board name
    pub title: String,
    pub cards: Collection<Card>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Card {
    pub text: String,
}

#[resource(kind = "test.board", label = "Board")]
impl Board {
    /// Rename the board.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }

    /// Add a card.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, text: String) {
        self.cards.push(cx.id(), Card { text });
    }

    /// Reword a card.
    #[action]
    pub fn reword(&mut self, card: &Id, text: String) -> Result<(), &'static str> {
        self.cards.get_mut(card).ok_or("no such card")?.text = text;
        Ok(())
    }
}

struct BoardApp;

impl ResourceApp for BoardApp {
    type Resource = Board;
    const ID: &'static str = "so.tilo.test-board";
    const TITLE: &'static str = "Board";

    fn view(cx: &mut ViewCx<Board>) -> Element {
        Box::new(
            Column::new()
                .push(Box::new(cx.text(|board| board.title.clone())))
                .push(Box::new(cx.text(|board| {
                    board
                        .cards
                        .values()
                        .map(|card| card.text.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                }))),
        )
    }
}

fn cards(app: &TestApp<BoardApp>) -> Vec<String> {
    app.state()
        .cards
        .values()
        .map(|card| card.text.clone())
        .collect()
}

fn refusal(result: Result<Option<Value>, tilo_actions::DoorError>) -> String {
    result.expect_err("refused").to_string()
}

/// Versions with what each carries over from the one before filled in.
fn carried(versions: &[Value]) -> Vec<Value> {
    let mut before = serde_json::Map::new();
    versions
        .iter()
        .map(|version| {
            let mut version = version.as_object().unwrap().clone();
            if !["run_id", "by"]
                .iter()
                .any(|key| version.contains_key(*key))
            {
                for key in ["run", "run_id", "by"] {
                    if let Some(value) = before.get(key) {
                        version.insert(key.into(), value.clone());
                    }
                }
            }
            if !version.contains_key("time_ms")
                && let Some(time) = before.get("time_ms")
            {
                version.insert("time_ms".into(), time.clone());
            }
            before.clone_from(&version);
            Value::Object(version)
        })
        .collect()
}

fn history(app: &mut TestApp<BoardApp>) -> Value {
    app.call("history", json!({}))
        .unwrap()
        .expect("history answers")
}

#[test]
fn undo_to_takes_back_every_step_after_a_version() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("add", json!({"text": "milk"})).unwrap();
    app.call("checkpoint", json!({"name": "shopping"})).unwrap();
    app.call("add", json!({"text": "eggs"})).unwrap();
    app.call("add", json!({"text": "bread"})).unwrap();
    app.call("rename", json!({"title": "Plan"})).unwrap();

    let undone = app
        .call("undo", json!({"to": "shopping"}))
        .unwrap()
        .unwrap();
    assert_eq!(cards(&app), ["milk"]);
    assert_eq!(undone["undone"].as_array().unwrap().len(), 3, "{undone}");

    // A change id from history works the same, and naming both is refused.
    app.call("add", json!({"text": "tea"})).unwrap();
    let listed = history(&mut app);
    let versions = listed["versions"].as_array().unwrap();
    let tea = versions.last().unwrap()["change"]
        .as_str()
        .unwrap()
        .to_owned();
    app.call("add", json!({"text": "jam"})).unwrap();
    app.call("undo", json!({"to": tea})).unwrap();
    assert_eq!(cards(&app), ["milk", "tea"]);
    let both = refusal(app.call("undo", json!({"to": "shopping", "steps": 1})));
    assert!(both.contains("steps or to"), "{both}");
}

#[test]
fn undo_and_redo_take_back_the_callers_own_steps() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "Plan"})).unwrap();
    app.call("add", json!({"text": "milk"})).unwrap();
    app.call("add", json!({"text": "eggs"})).unwrap();

    let undone = app.call("undo", json!({"steps": 2})).unwrap().unwrap();
    assert_eq!(cards(&app), Vec::<String>::new());
    assert_eq!(app.state().title, "Plan");
    let undone = undone["undone"].as_array().unwrap();
    assert_eq!(undone.len(), 2, "{undone:?}");
    assert_eq!(undone[0]["what"]["args"]["text"], "eggs", "{undone:?}");

    // A redo brings back one undo, however many steps it took.
    let redone = app.call("redo", json!({})).unwrap().unwrap();
    assert_eq!(cards(&app), ["milk", "eggs"]);
    assert_eq!(redone["redone"].as_array().unwrap().len(), 2, "{redone}");
    let nothing = refusal(app.call("redo", json!({})));
    assert!(nothing.contains("nothing to redo"), "{nothing}");

    // The undo is itself a step: it is listed and attributed.
    let listed = history(&mut app);
    let undos: Vec<&Value> = listed["versions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|version| version["what"]["did"] == "history" && version["what"]["op"] == "undo")
        .collect();
    assert_eq!(undos.len(), 1, "{listed}");
    assert_eq!(undos[0]["what"]["steps"], 2, "{listed}");
    assert_eq!(undos[0]["sentence"], "Undid 2 steps", "{listed}");
}

/// Exchange what each device has, as sync does.
fn sync(a: &mut TestApp<BoardApp>, b: &mut TestApp<BoardApp>) {
    let mut doc = b.peer_document();
    a.merge_peer(&mut doc).unwrap();
    b.merge_peer(&mut doc).unwrap();
    a.settle();
    b.settle();
}

#[test]
fn ctrl_z_takes_back_only_what_was_done_on_this_device() {
    let mut laptop = TestApp::<BoardApp>::on_device("laptop").unwrap();
    let mut phone = TestApp::<BoardApp>::on_device("phone").unwrap();
    sync(&mut laptop, &mut phone);
    laptop.call("add", json!({"text": "milk"})).unwrap();
    sync(&mut laptop, &mut phone);
    phone.call("add", json!({"text": "eggs"})).unwrap();
    sync(&mut laptop, &mut phone);
    assert_eq!(cards(&laptop), ["milk", "eggs"]);

    // The same person's newer step from the phone is not the laptop's to
    // take back with a key press.
    assert!(laptop.undo());
    assert_eq!(cards(&laptop), ["eggs"]);
    assert!(
        !laptop.undo(),
        "Ctrl+Z on the laptop never undoes phone work"
    );
    assert_eq!(cards(&laptop), ["eggs"]);

    // It stays reachable through the published undo.
    laptop.call("undo", json!({})).unwrap();
    assert_eq!(cards(&laptop), Vec::<String>::new());
}

/// The newest listed version, with what it carries over filled in.
fn newest(app: &mut TestApp<BoardApp>) -> Value {
    let listed = history(app);
    carried(listed.get("versions").and_then(Value::as_array).unwrap())
        .pop()
        .expect("a version")
}

#[test]
fn a_keyboard_undo_and_redo_are_named_after_the_step_and_stay_its_authors() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.invoke(Board::RENAME.with("Plan")).unwrap();
    let renamed = newest(&mut app);
    let label = renamed["sentence"].as_str().unwrap().to_owned();
    assert!(!label.is_empty(), "{renamed}");
    let by = renamed["by"].clone();
    assert!(by.is_string(), "{renamed}");

    // Ctrl+Z: the undo is the person's own step, named after what it took
    // back, never an unnamed change by a bare actor.
    assert!(app.undo());
    assert_eq!(app.state().title, "");
    let undo = newest(&mut app);
    assert_eq!(
        undo["sentence"],
        json!(format!("Undid “{label}”")),
        "{undo}"
    );
    assert_eq!(undo["by"], by, "{undo}");
    let context = app
        .versions()
        .pop()
        .unwrap()
        .context
        .expect("an undo says what it did");
    let Some(tilo_actions::Did::History(step)) = &context.what else {
        panic!("an undo records a history step: {context:?}");
    };
    assert_eq!(step.op, tilo_actions::HistoryOp::Undo);
    assert_eq!(
        step.of.as_ref().map(|of| json!(of.what)),
        Some(renamed["what"].clone()),
        "the undo records the step it took back"
    );
    assert!(context.invoker.is_some(), "{context:?}");

    // Ctrl+Shift+Z names the same step, not the undo it reverts.
    assert!(app.redo());
    assert_eq!(app.state().title, "Plan");
    let redo = newest(&mut app);
    assert_eq!(
        redo["sentence"],
        json!(format!("Redid “{label}”")),
        "{redo}"
    );
    assert_eq!(redo["by"], by, "{redo}");

    // And undoing that redo is again an undo of the step.
    assert!(app.undo());
    let again = newest(&mut app);
    assert_eq!(
        again["sentence"],
        json!(format!("Undid “{label}”")),
        "{again}"
    );
    assert_eq!(again["by"], by, "{again}");
}

#[test]
fn an_agent_run_undoes_its_own_steps_and_leaves_the_persons() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("add", json!({"text": "person's card"})).unwrap();
    app.call_in_run("mcp:run-1", "add", json!({"text": "agent one"}))
        .unwrap();
    app.call_in_run("mcp:run-1", "rename", json!({"title": "Agent plan"}))
        .unwrap();

    app.call_in_run("mcp:run-1", "undo", json!({})).unwrap();
    assert_eq!(app.state().title, "", "the run's last step went");
    assert_eq!(cards(&app), ["person's card", "agent one"]);

    // The person's undo is the person's, not the run's.
    app.call("undo", json!({})).unwrap();
    assert_eq!(cards(&app), ["agent one"]);
    app.call("redo", json!({})).unwrap();

    // Naming the run undoes everything of it still in effect.
    app.call_in_run("mcp:run-1", "add", json!({"text": "agent two"}))
        .unwrap();
    let listed = history(&mut app);
    let run = listed["versions"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|version| version["run_id"].as_str())
        .unwrap()
        .to_owned();
    assert_eq!(run, "mcp:run-1");
    app.call("undo", json!({"run": run})).unwrap();
    assert_eq!(cards(&app), ["person's card"]);
    let nothing = refusal(app.call_in_run("mcp:run-1", "undo", json!({})));
    assert!(
        nothing.contains("This agent run (\"mcp:run-1\") has nothing in effect to undo"),
        "{nothing}"
    );
}

#[test]
fn a_refused_undo_names_the_earlier_runs_and_the_call_that_takes_them_back() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    for text in ["one", "two", "three", "four"] {
        app.call_in_run("mcp:earlier", "add", json!({"text": text}))
            .unwrap();
    }
    // The same agent, connected again: a run of its own with nothing done.
    let refused = refusal(app.call_in_run("mcp:later", "undo", json!({"steps": 4})));
    assert!(
        refused.contains(
            r#""mcp:earlier" by mcp:earlier (4 steps): undo {"run": "mcp:earlier", "steps": 4}"#
        ),
        "{refused}"
    );
    assert!(refused.contains("earlier connection"), "{refused}");
    assert_eq!(cards(&app).len(), 4, "a refusal changes nothing");

    // Doing as it says takes them back.
    app.call_in_run(
        "mcp:later",
        "undo",
        json!({"run": "mcp:earlier", "steps": 4}),
    )
    .unwrap();
    assert!(cards(&app).is_empty());
    let none_left = refusal(app.call_in_run("mcp:later", "undo", json!({})));
    assert!(!none_left.contains("mcp:earlier"), "{none_left}");

    // A person asking names the runs too.
    app.call_in_run("mcp:earlier", "redo", json!({}))
        .unwrap_err();
    app.call_in_run("mcp:later", "redo", json!({})).unwrap();
    assert_eq!(cards(&app).len(), 4);
    let person = refusal(app.call("undo", json!({})));
    assert!(
        person.starts_with("You have nothing in effect to undo"),
        "{person}"
    );
    assert!(
        person.contains(r#"undo {"run": "mcp:later", "steps": 1}"#),
        "{person}"
    );
}

#[test]
fn history_lists_each_step_with_its_arguments_author_and_checkpoints() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("add", json!({"text": "sky"})).unwrap();
    let named = app
        .call("checkpoint", json!({"name": "before sea"}))
        .unwrap()
        .unwrap();
    assert_eq!(named["checkpoint"], "before sea");
    app.call_in_run("mcp:run-7", "add", json!({"text": "sea"}))
        .unwrap();
    app.call_in_run("mcp:run-7", "add", json!({"text": "gull"}))
        .unwrap();

    let listed = history(&mut app);
    let versions = carried(listed["versions"].as_array().unwrap());
    let sky = versions
        .iter()
        .find(|version| version["what"]["args"]["text"] == "sky")
        .unwrap();
    assert_eq!(sky["what"]["action"]["action"], "add", "{listed}");
    assert!(!sky["sentence"].as_str().unwrap().is_empty(), "{listed}");
    assert!(sky["by"].is_string(), "{listed}");
    assert_eq!(sky["checkpoint"], json!(["before sea"]), "{listed}");
    let [.., sea, gull] = versions.as_slice() else {
        panic!("sea and gull are listed: {listed}");
    };
    assert_eq!(sea["run_id"], "mcp:run-7", "{listed}");
    assert_eq!(gull["run_id"], "mcp:run-7", "{listed}");
    // The run is named once: its second step carries it over.
    let gull = listed["versions"].as_array().unwrap().last().unwrap();
    assert!(gull.get("run_id").is_none(), "{listed}");
    assert!(gull.get("by").is_none(), "{listed}");
    assert_eq!(listed["checkpoints"][0]["name"], "before sea");
    assert_eq!(listed["checkpoints"][0]["version"], named["version"]);
}

#[test]
fn restore_brings_back_a_version_whole_or_by_item_as_an_undoable_step() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "Calm"})).unwrap();
    app.call("add", json!({"text": "sky"})).unwrap();
    app.call("add", json!({"text": "sea"})).unwrap();
    app.call("checkpoint", json!({"name": "good"})).unwrap();
    let ids: Vec<Id> = app.state().cards.ids().cloned().collect();

    // A bad pass: rewording both cards and the title.
    app.call("rename", json!({"title": "Storm"})).unwrap();
    for id in &ids {
        app.call("reword", json!({"card": id.as_str(), "text": "mud"}))
            .unwrap();
    }

    let diff = app.call("diff", json!({"from": "good"})).unwrap().unwrap();
    let items: Vec<&str> = diff["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["item"].as_str().unwrap())
        .collect();
    assert!(items.contains(&"title"), "{diff}");
    assert!(
        items.contains(&format!("cards/{}", ids[0].as_str()).as_str()),
        "{diff}"
    );

    // Restore only the first card: the rest of the bad pass stays.
    let only = app
        .call(
            "restore",
            json!({"version": "good", "ids": [ids[0].as_str()]}),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        only["restored"],
        json!([format!("cards/{}", ids[0].as_str())]),
        "{only}"
    );
    assert_eq!(cards(&app), ["sky", "mud"]);
    assert_eq!(app.state().title, "Storm");

    // Then everything; the restore is one step the caller can undo.
    app.call("restore", json!({"version": "good"})).unwrap();
    assert_eq!(cards(&app), ["sky", "sea"]);
    assert_eq!(app.state().title, "Calm");
    let same = refusal(app.call("restore", json!({"version": "good"})));
    assert!(same.contains("already matches"), "{same}");
    app.call("undo", json!({})).unwrap();
    assert_eq!(app.state().title, "Storm");

    let unknown = refusal(app.call("restore", json!({"version": "nope"})));
    assert!(unknown.contains("good"), "names the checkpoints: {unknown}");
}

#[test]
fn render_shows_a_past_version() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "Early"})).unwrap();
    app.call("checkpoint", json!({"name": "early"})).unwrap();
    app.call("rename", json!({"title": "Late"})).unwrap();

    let now = app.call("render", json!({})).unwrap().unwrap();
    let past = app
        .call("render", json!({"version": "early"}))
        .unwrap()
        .unwrap();
    assert_eq!(past["width"], now["width"]);
    assert_ne!(past["revision"], now["revision"]);
    assert_ne!(past["content"], now["content"], "a different picture");
    let unknown = refusal(app.call("render", json!({"version": "abcdef12"})));
    assert!(unknown.contains("version"), "{unknown}");
}

#[test]
fn undo_author_selector_rejects_invalid_or_ambiguous_requests_without_mutation() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("add", json!({"text": "keep"})).unwrap();
    for args in [
        json!({"by": ""}),
        json!({"by": " \n "}),
        json!({"by": "author\u{0000}"}),
        json!({"by": "a".repeat(513)}),
        json!({"by": "agent:one", "run": "run:one"}),
    ] {
        assert!(app.call("undo", args).is_err());
        assert_eq!(cards(&app), ["keep"]);
    }
}

#[test]
fn undo_named_author_preserves_other_authors_and_attributes_the_undo_to_its_caller() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    for (actor, text) in [
        ("agent:clipboard-helper", "agent card"),
        ("person:owner", "person card"),
    ] {
        app.call_by(
            tilo_actions::ActionInvoker::actor(actor),
            "test.board",
            "add",
            json!({"text": text}),
        )
        .unwrap();
    }
    let listed = history(&mut app);
    let author = listed["versions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["what"]["args"]["text"] == "agent card")
        .and_then(|entry| entry["by"].as_str())
        .expect("history publishes the exact invoker");
    assert_eq!(author, "agent:clipboard-helper");
    app.call_by(
        tilo_actions::ActionInvoker::actor("person:owner"),
        "test.board",
        "undo",
        json!({"by": author}),
    )
    .unwrap();
    assert_eq!(cards(&app), ["person card"]);
    let undo = newest(&mut app);
    assert_eq!(undo["by"], "person:owner", "{undo}");
    assert_eq!(undo["what"]["op"], "undo", "{undo}");
}

#[test]
fn ctrl_z_takes_back_the_persons_steps_after_a_restart_through_the_one_history() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.invoke(Board::RENAME.with("Plan")).unwrap();
    app.invoke(Board::RENAME.with("Launch")).unwrap();
    let mut app = app.reopen().unwrap();

    // After a restart Ctrl+Z still takes back the person's last step, read
    // from the document, and records it as their undo of that step.
    assert!(app.undo());
    assert_eq!(app.state().title, "Plan");
    let undo = newest(&mut app);
    assert_eq!(undo["what"]["op"], "undo", "{undo}");
    assert_eq!(
        undo["what"]["of"]["what"]["args"]["title"], "Launch",
        "{undo}"
    );

    // The published redo brings back what Ctrl+Z took back: one history.
    app.call("redo", json!({})).unwrap();
    assert_eq!(app.state().title, "Launch");

    // An agent's or a script's undo is not a step Ctrl+Z takes back again:
    // the next Ctrl+Z goes on to the step before.
    app.call("undo", json!({})).unwrap();
    assert_eq!(app.state().title, "Plan");
    assert!(app.undo());
    assert_eq!(app.state().title, "");
}

#[test]
fn ctrl_z_in_a_session_is_the_same_undo_the_published_history_reports() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.invoke(Board::RENAME.with("Plan")).unwrap();
    assert!(app.undo());
    assert_eq!(app.state().title, "");
    let listed = history(&mut app);
    let versions = carried(listed["versions"].as_array().unwrap());
    let renamed = versions
        .iter()
        .find(|version| version["what"]["args"]["title"] == "Plan")
        .unwrap();
    assert_eq!(renamed["undone"], true, "{listed}");
    // So the published redo brings it back after Ctrl+Z.
    app.call("redo", json!({})).unwrap();
    assert_eq!(app.state().title, "Plan");
}

#[test]
fn an_agents_edit_is_recorded_as_the_agent_acting_for_its_person() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    let agent = tilo_actions::InteractionActor::new("agent:claude", tilo_actions::ActorKind::Agent)
        .acting_for("person:quinten");
    app.call_by(
        tilo_actions::ActionInvoker::admitted(&agent),
        "test.board",
        "rename",
        json!({"title": "Drafted"}),
    )
    .unwrap();
    let entry = app
        .versions()
        .pop()
        .and_then(|version| version.context)
        .expect("the agent's edit is recorded");
    let invoker = entry.invoker.expect("attributed");
    assert_eq!(invoker.actor, "agent:claude");
    assert_eq!(invoker.kind, tilo_actions::ActorKind::Agent);
    assert_eq!(invoker.on_behalf_of.as_deref(), Some("person:quinten"));
    let listed = newest(&mut app);
    assert_eq!(listed["actor"]["kind"], "agent", "{listed}");
    assert_eq!(listed["actor"]["for"], "person:quinten", "{listed}");
}

#[test]
fn a_field_edit_without_a_message_is_named_by_the_fields_title() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.invoke(Invoke::edit(&Board::paths().title.replace("Plan")).unwrap())
        .unwrap();
    let edited = newest(&mut app);
    assert_eq!(edited["what"]["did"], "edit", "{edited}");
    let sentence = edited["sentence"].as_str().unwrap();
    assert!(sentence.contains("Board name"), "{sentence}");
    assert!(!sentence.contains("title"), "{sentence}");
}

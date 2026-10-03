#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! The history every framework app answers through the action door: undo
//! and redo of the caller's own steps (an agent's run), history with what
//! each step was given, checkpoints, restore whole or by item, diff, and
//! the view at a past version.

use serde_json::{Value, json};
use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Board {
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
    assert_eq!(undone[0]["args"], json!("{text:\"eggs\"}"), "{undone:?}");

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
        .filter(|version| version["action"] == "Undo")
        .collect();
    assert_eq!(undos.len(), 1, "{listed}");
    assert_eq!(undos[0]["args"], json!("{steps:2}"), "{listed}");
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
        nothing.contains("This agent run has nothing in effect to undo"),
        "{nothing}"
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
        .find(|version| version["args"] == "{text:\"sky\"}")
        .unwrap();
    assert_eq!(sky["action"], "Add a card", "{listed}");
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

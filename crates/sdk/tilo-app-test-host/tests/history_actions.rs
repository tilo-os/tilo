//! The history every framework app answers through the action API: undo
//! and redo of the caller's own steps (an agent's run), history with what
//! each step was given, checkpoints, restore whole or by item, diff, and
//! the view at a past version.

use serde_json::{Value, json};
use tilo_app::prelude::*;
use tilo_app_test_host::TestApp;

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

fn read_board(board: &Board, item: Option<&Id>) -> Result<Value, String> {
    match item {
        Some(id) => board
            .cards
            .get(id)
            .map(|card| json!({"id": id.as_str(), "text": card.text}))
            .ok_or_else(|| "no such card".into()),
        None => Ok(json!({
            "title": board.title,
            "cards": board.cards.ids().map(|id| json!({
                "id": id.as_str(), "text": board.cards.get(id).unwrap().text,
            })).collect::<Vec<_>>(),
        })),
    }
}

#[resource(
    kind = "test.board",
    read_content = read_board,
    app = "so.tilo.test-board", catalogue = "tests/locales")]
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

pub(super) struct BoardApp;

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

fn refusal(result: Result<Option<Value>, tilo_actions::ApiError>) -> String {
    result.expect_err("refused").to_string()
}

/// Versions with what each carries over from the one before filled in.
fn carried(versions: &[Value]) -> Vec<Value> {
    let mut before = serde_json::Map::new();
    versions
        .iter()
        .map(|version| {
            let mut version = version.as_object().unwrap().clone();
            if !["group", "by"].iter().any(|key| version.contains_key(*key)) {
                for key in ["title", "group", "by"] {
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

/// The changes the last move through history wrote, oldest first: what each did (`undo`,
/// `redo`, `restore`), and the verb and invocation it moved.
fn last_move(app: &TestApp<BoardApp>) -> Vec<(String, String, String)> {
    use tilo_app::tilo_actions::{Did, HistoryOp, entry_of};
    app.read_document(|doc| {
        let document = doc.committed_document();
        let entries: Vec<_> = document
            .get_changes(&[])
            .iter()
            .filter_map(|change| entry_of(document, change.hash()))
            .collect();
        let group = entries
            .iter()
            .rev()
            .find_map(|entry| {
                entry
                    .group
                    .as_ref()
                    .filter(|group| group.kind.starts_with("history:"))
            })
            .map(|group| group.id.clone());
        entries
            .iter()
            .filter(|entry| entry.group.as_ref().map(|group| &group.id) == group.as_ref())
            .map(|entry| match &entry.what {
                Some(Did::History(step)) => (
                    match step.op {
                        HistoryOp::Undo => "undo",
                        HistoryOp::Redo => "redo",
                        _ => "restore",
                    }
                    .to_owned(),
                    step.verb.clone(),
                    step.undoes.clone().unwrap_or_default(),
                ),
                other => panic!("a move through history writes history steps: {other:?}"),
            })
            .collect()
    })
}

/// The change id History lists for the card `text` was added by.
fn change_of(app: &mut TestApp<BoardApp>, text: &str) -> String {
    history(app)["versions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|version| version["what"]["args"]["text"] == text)
        .and_then(|version| version["change"].as_str())
        .unwrap()
        .to_owned()
}

/// The change id of the newest row History lists: what a window keeps of a move it made.
fn newest_change(app: &mut TestApp<BoardApp>) -> String {
    history(app)["versions"]
        .as_array()
        .and_then(|versions| versions.last())
        .and_then(|version| version["change"].as_str())
        .unwrap()
        .to_owned()
}

fn history(app: &mut TestApp<BoardApp>) -> Value {
    app.call("history", json!({}))
        .unwrap()
        .expect("history answers")
}

tilo_test_env::scenario! {
    fn undo_takes_back_only_the_steps_of_who_asks;

    phase undo_to_takes_back_every_step_after_a_version;
    {
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

    phase undo_and_redo_take_back_the_callers_own_steps;
    {
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
        // Each step is taken back by a change of its own, newest first, naming the invocation
        // it took back and that invocation's verb.
        let moved = last_move(&app);
        assert_eq!(moved.len(), 2, "{moved:?}");
        assert!(moved.iter().all(|(op, verb, _)| op == "undo" && verb == "add"), "{moved:?}");
        assert_ne!(moved[0].2, moved[1].2, "one invocation each: {moved:?}");

        // A redo brings back one undo, however many steps it took.
        let redone = app.call("redo", json!({})).unwrap().unwrap();
        assert_eq!(cards(&app), ["milk", "eggs"]);
        assert_eq!(redone["redone"].as_array().unwrap().len(), 2, "{redone}");
        let nothing = refusal(app.call("redo", json!({})));
        assert!(nothing.contains("nothing to redo"), "{nothing}");
        let moved = last_move(&app);
        assert_eq!(moved.len(), 2, "{moved:?}");
        assert!(moved.iter().all(|(op, _, _)| op == "redo"), "{moved:?}");

        // The undo is itself a step: it is listed and attributed, as the one move it was.
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
        assert_eq!(undos[0]["changes"].as_array().map(Vec::len), Some(2), "{listed}");

        // Undoing the redo takes the whole move back again, as one step.
        app.call("undo", json!({})).unwrap();
        assert_eq!(cards(&app), Vec::<String>::new());
        assert_eq!(app.state().title, "Plan");
    }

    phase a_take_back_is_brought_back_by_naming_it_after_newer_undos;
    {
        let mut app = TestApp::<BoardApp>::new().unwrap();
        for text in ["milk", "tea", "eggs"] {
            app.call("add", json!({"text": text})).unwrap();
        }
        let chosen = [change_of(&mut app, "milk"), change_of(&mut app, "eggs")];
        app.call("undo", json!({"chosen": chosen})).unwrap();
        let take_back = newest_change(&mut app);
        // A newer step and its undo, made elsewhere: a plain redo would bring that back.
        app.call("add", json!({"text": "bread"})).unwrap();
        app.call("undo", json!({})).unwrap();
        assert_eq!(cards(&app), ["tea"]);

        let redone = app
            .call("redo", json!({"undos": [take_back]}))
            .unwrap()
            .unwrap();
        assert_eq!(redone["redone"].as_array().unwrap().len(), 2, "{redone}");
        assert_eq!(cards(&app), ["milk", "tea", "eggs"]);
        // One change for each step it brought back, listed as one move.
        let moved = last_move(&app);
        assert_eq!(moved.len(), 2, "{moved:?}");
        assert!(moved.iter().all(|(op, verb, _)| op == "redo" && verb == "add"), "{moved:?}");
        let listed = history(&mut app);
        let last = listed["versions"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["what"]["op"], "redo", "{listed}");
        assert_eq!(last["changes"].as_array().map(Vec::len), Some(2), "{listed}");

        let again = refusal(app.call("redo", json!({"undos": [take_back]})));
        assert!(again.contains("no longer in effect"), "{again}");
        let mixed = refusal(app.call("redo", json!({"undos": [take_back], "steps": 1})));
        assert!(mixed.contains("alone"), "{mixed}");
        // The undo made elsewhere is still there to redo.
        app.call("redo", json!({})).unwrap();
        let mut all = cards(&app);
        all.sort();
        assert_eq!(all, ["bread", "eggs", "milk", "tea"]);
    }

    phase a_restore_takes_back_each_step_since_and_brings_back_what_stood_then;
    {
        let mut app = TestApp::<BoardApp>::new().unwrap();
        app.call("add", json!({"text": "milk"})).unwrap();
        app.call("add", json!({"text": "tea"})).unwrap();
        app.call("checkpoint", json!({"name": "shopping"})).unwrap();
        app.call("add", json!({"text": "eggs"})).unwrap();
        app.call("rename", json!({"title": "Plan"})).unwrap();
        // A step that stood at the checkpoint, undone after it.
        let tea = history(&mut app)["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|version| version["what"]["args"]["text"] == "tea")
            .and_then(|version| version["change"].as_str())
            .unwrap()
            .to_owned();
        app.call("undo", json!({"step": tea})).unwrap();
        app.call("add", json!({"text": "bread"})).unwrap();
        assert_eq!(cards(&app), ["milk", "eggs", "bread"]);

        app.call("restore", json!({"version": "shopping"})).unwrap();
        // The card undone since comes back where a restore to the version has always put it.
        assert_eq!(cards(&app), ["tea", "milk"]);
        assert_eq!(app.state().title, "");

        // One change for each invocation since the checkpoint, newest first, each naming the
        // invocation and its verb; then what stood at the checkpoint and was undone since.
        let moved = last_move(&app);
        assert!(moved.iter().all(|(op, _, _)| op == "restore"), "{moved:?}");
        let (each, rest) = moved.split_at(moved.len().saturating_sub(1));
        let verbs: Vec<&str> = each.iter().map(|(_, verb, _)| verb.as_str()).collect();
        assert_eq!(verbs, ["add", "rename", "add"], "{moved:?}");
        let mut invocations: Vec<&str> = each
            .iter()
            .map(|(_, _, invocation)| invocation.as_str())
            .collect();
        invocations.dedup();
        assert_eq!(invocations.len(), 3, "{moved:?}");
        assert!(rest.iter().all(|(_, verb, _)| verb.is_empty()), "{moved:?}");

        // The restore is one step the caller can undo, and redo does not bring back.
        let nothing = refusal(app.call("redo", json!({})));
        assert!(nothing.contains("nothing to redo"), "{nothing}");

        // History lists it as the one restore it was.
        let listed = history(&mut app);
        let last = listed["versions"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["what"]["op"], "restore", "{listed}");
        assert_eq!(last["changes"].as_array().map(Vec::len), Some(moved.len()), "{listed}");
        app.call("undo", json!({})).unwrap();
        assert_eq!(cards(&app), ["milk", "eggs", "bread"]);
        assert_eq!(app.state().title, "Plan");
    }

    phase an_agent_run_undoes_its_own_steps_and_leaves_the_persons;
    {
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

        // Naming the group undoes everything of it still in effect.
        app.call_in_run("mcp:run-1", "add", json!({"text": "agent two"}))
            .unwrap();
        let listed = history(&mut app);
        let run = listed["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|version| version["group"].as_str())
            .unwrap()
            .to_owned();
        assert_eq!(run, "mcp:run-1");
        app.call("undo", json!({"invocation": run})).unwrap();
        assert_eq!(cards(&app), ["person's card"]);
        let nothing = refusal(app.call_in_run("mcp:run-1", "undo", json!({})));
        assert!(
            nothing.contains("This agent session (\"mcp:run-1\") has nothing in effect to undo"),
            "{nothing}"
        );
    }

    phase a_refused_undo_names_the_earlier_runs_and_the_call_that_takes_them_back;
    {
        let mut app = TestApp::<BoardApp>::new().unwrap();
        for text in ["one", "two", "three", "four"] {
            app.call_in_run("mcp:earlier", "add", json!({"text": text}))
                .unwrap();
        }
        // The same agent, connected again: a run of its own with nothing done.
        let refused = refusal(app.call_in_run("mcp:later", "undo", json!({"steps": 4})));
        assert!(
            refused.contains(
                r#""mcp:earlier" (4 steps): undo {"invocation": "mcp:earlier", "steps": 4}"#
            ),
            "{refused}"
        );
        assert!(refused.contains("earlier connection"), "{refused}");
        assert_eq!(cards(&app).len(), 4, "a refusal changes nothing");

        // Doing as it says takes them back.
        app.call_in_run(
            "mcp:later",
            "undo",
            json!({"invocation": "mcp:earlier", "steps": 4}),
        )
        .unwrap();
        assert!(cards(&app).is_empty());
        let none_left = refusal(app.call_in_run("mcp:later", "undo", json!({})));
        assert!(!none_left.contains("mcp:earlier"), "{none_left}");

        // A person asking names the groups too.
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
            person.contains(r#"undo {"invocation": "mcp:later", "steps": 1}"#),
            "{person}"
        );
    }

    phase ctrl_z_takes_back_the_persons_steps_after_a_restart_through_the_one_history;
    {
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

}

/// The newest listed version, with what it carries over filled in.
fn newest(app: &mut TestApp<BoardApp>) -> Value {
    let listed = history(app);
    carried(listed.get("versions").and_then(Value::as_array).unwrap())
        .pop()
        .expect("a version")
}

tilo_test_env::scenario! {
    fn history_names_each_step_and_restores_a_version;

    phase history_lists_each_step_with_its_arguments_author_and_checkpoints;
    {
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
        assert_eq!(sea["group"], "mcp:run-7", "{listed}");
        assert_eq!(gull["group"], "mcp:run-7", "{listed}");
        // The run is named once: its second step carries it over.
        let gull = listed["versions"].as_array().unwrap().last().unwrap();
        assert!(gull.get("group").is_none(), "{listed}");
        assert!(gull.get("by").is_none(), "{listed}");
        assert_eq!(listed["checkpoints"][0]["name"], "before sea");
        assert_eq!(listed["checkpoints"][0]["version"], named["version"]);
    }

    phase restore_brings_back_a_version_whole_or_by_item_as_an_undoable_step;
    {
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

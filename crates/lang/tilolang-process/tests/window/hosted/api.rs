//! What an agent session does is its own, for the person it acts for, and undo takes it back
//! like any app's agent-driven change.

use super::{Session, agent_session};
use crate::change_marks::{Driven as _, action_id, flattened, settle};
use serde_json::{Map, Value as Json, json};
use tilolang_tilo::runtime::store::Store as _;

const NOTES: &str = "so.tilo.notes";
const HOST: &str = "tl:host";

fn arguments(value: &Json) -> Map<String, Json> {
    value.as_object().cloned().unwrap_or_default()
}

/// The window of the installed Notes package as the person opened it, and the notebook's id.
fn notes_open() -> (Session, String) {
    let mut app = Session::notes();
    settle(&mut app);
    let root = super::root_id(&app, NOTES);
    (app, root)
}

/// How many notes the notebook `root` of `app` holds.
fn notes_in(app: &mut Session) -> usize {
    use tilolang_tilo::runtime::Value;
    settle(app);
    let host = app.instance_host();
    let notebook = host.root_of(NOTES).expect("the notebook");
    match host.runtime().read(&notebook, "notes") {
        Ok(Value::List(items)) => items.len(),
        other => panic!("the notebook lists its notes: {other:?}"),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one story: person, session, history, undo"
)]
fn what_an_agent_session_does_is_its_own_for_the_person() {
    let (mut app, root) = notes_open();
    let notebook = format!("tl:{NOTES}:{root}");
    // The person adds a note themselves.
    let new_note = action_id(&mut app, &notebook, "New note");
    app.invoke(None, None, &notebook, &new_note, &Map::new())
        .unwrap();
    settle(&mut app);
    // The agent session adds one by a block, and one by the published verb.
    let (agent, ambient) = agent_session();
    let block = format!("notes.new_note(@notes.Notebook:{root})");
    let run_block = action_id(&mut app, HOST, "Run a block");
    let answer = app
        .invoke(
            Some(agent.clone()),
            Some(&ambient),
            HOST,
            &run_block,
            &arguments(&json!({ "source": block })),
        )
        .unwrap();
    assert_eq!(answer.value.unwrap()["state"], "done");
    app.invoke(
        Some(agent.clone()),
        Some(&ambient),
        &notebook,
        &new_note,
        &Map::new(),
    )
    .unwrap();
    settle(&mut app);
    assert_eq!(
        notes_in(&mut app),
        3,
        "one by the person, two by the agent session"
    );

    // The change is the agent's: what history reads of it names the agent.
    let versions = app
        .instance_host()
        .store()
        .versions(&app.instance_host().root_of(NOTES).expect("the notebook"))
        .expect("the notebook's history");
    let agent_actor = agent_session().0.acting.by;
    let by_agent: Vec<_> = versions
        .iter()
        .filter(|version| version.who.as_ref() == Some(&agent_actor))
        .collect();
    assert!(
        by_agent.len() >= 2,
        "the agent session's changes are attributed to it: {versions:#?}"
    );
    // Each is a step of the session that did it: history groups them under it.
    for version in &by_agent {
        assert_eq!(
            version.run.as_ref().map(|run| run.id.as_str()),
            Some("agent-session:7"),
            "grouped under its session: {version:?}"
        );
    }
    // The person's own change is theirs, and undo takes back the agent's last one alone.
    let person = versions
        .iter()
        .filter(|version| version.who.as_ref() != Some(&agent_actor))
        .count();
    assert!(person >= 1, "the person's own changes are in history too");
    // The app's undo, as its record publishes it: it takes back the agent's last step, read from
    // the store's own log.
    let record = tilolang_tilo::naming::package_resource_id(NOTES);
    app.invoke(Some(agent), Some(&ambient), &record, "undo", &Map::new())
        .expect("the store takes it back");
    settle(&mut app);
    assert_eq!(notes_in(&mut app), 2, "the agent's last note is taken back");
}

#[test]
fn the_installed_package_is_the_app_and_opens_on_its_root_the_action_api_committed() {
    let definition = crate::support::installed("notes");
    assert_eq!(definition.app_id(), NOTES, "the app's id is the package's");
    assert_eq!(definition.title(), "Notes");
    let app = Session::notes();
    assert_eq!(
        app.instance_host()
            .store()
            .resources_of(&app.instance_host().root_of(NOTES).expect("the notebook").ty)
            .expect("the store answers")
            .len(),
        1,
        "the notebook exists"
    );
}

#[test]
fn history_names_what_was_made_and_changed_by_title_never_by_id() {
    let (mut app, root) = notes_open();
    let notebook = format!("tl:{NOTES}:{root}");
    let new_note = action_id(&mut app, &notebook, "New note");
    app.invoke(None, None, &notebook, &new_note, &Map::new())
        .unwrap();
    settle(&mut app);
    // The app's history, as an agent or a person asks the Action API for it.
    let package = format!("tl:{NOTES}");
    let history = action_id(&mut app, &package, "History");
    let listed = app
        .invoke(None, None, &package, &history, &Map::new())
        .unwrap()
        .value
        .unwrap();
    let sentences: Vec<&str> = listed["versions"]
        .as_array()
        .unwrap_or_else(|| panic!("the history lists its versions: {listed:#}"))
        .iter()
        .filter_map(|version| version["sentence"].as_str())
        .collect();
    assert!(
        sentences.contains(&"Created Notebook"),
        "the notebook the window made reads as native history's creation: {sentences:?}"
    );
    assert!(
        sentences.contains(&"Ran “New note” on Untitled"),
        "the person's action reads by its label, on the note it made by what its type calls an \
         untitled one: {sentences:?}"
    );
    for sentence in &sentences {
        assert!(
            !sentence.contains(&root) && !sentence.contains('$'),
            "no id in history: {sentence:?}"
        );
    }
}

#[test]
fn an_agent_reads_what_the_app_holds_and_each_item_it_names() {
    let (mut app, root) = notes_open();
    let notebook = format!("tl:{NOTES}:{root}");
    let new_note = action_id(&mut app, &notebook, "New note");
    app.invoke(None, None, &notebook, &new_note, &Map::new())
        .unwrap();
    settle(&mut app);
    let read = |app: &mut Session, resource: &str| {
        let read = action_id(app, resource, "Read the content");
        let (invoker, context) = agent_session();
        app.invoke(Some(invoker), Some(&context), resource, &read, &Map::new())
            .unwrap()
            .value
            .unwrap()
    };
    let content = read(&mut app, &notebook);
    assert_eq!(content["item"], json!(notebook));
    assert_eq!(content["kind"], json!(format!("tl.{NOTES}.Notebook")));
    let resources = app.harness().resources();
    let published = flattened(&resources)
        .into_iter()
        .find(|published| published.id.as_str() == notebook)
        .map(|published| published.label.clone());
    assert_eq!(
        content["label"].as_str(),
        published.as_deref(),
        "named as the window publishes it: {content:#}"
    );
    let notes = &content["fields"]["notes"];
    assert_eq!(notes["count"], json!(1), "{content:#}");
    let note = notes["items"][0]["item"]
        .as_str()
        .unwrap_or_else(|| panic!("the note is named by the id a read takes: {content:#}"))
        .to_owned();
    // A note nobody named yet is called what its type says an untitled one is called, as the
    // list shows it.
    assert_eq!(notes["items"][0]["label"], json!("Untitled"), "{content:#}");

    let content = read(&mut app, &note);
    assert_eq!(content["item"], json!(note));
    assert_eq!(content["kind"], json!(format!("tl.{NOTES}.Note")));
    assert_eq!(content["fields"]["title"], json!(""), "{content:#}");
    assert_eq!(content["fields"]["body"], json!(""), "{content:#}");
    assert_eq!(content["fields"]["pinned"], json!(false), "{content:#}");
}

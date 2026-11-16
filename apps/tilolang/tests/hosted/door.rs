//! What an agent session does is its own, for the person it acts for, and history and undo group
//! it by session like any app's agent-driven change.

use super::{Session, agent_session};
use crate::change_marks::{Driven as _, action_id, flattened, settle};
use serde_json::{Map, Value as Json, json};
use tilolang_tilo::action::resource_ids;
use ui::history_source::steps::Scope;

const NOTES: &str = "so.tilo.lang.notes";
const HOST: &str = "tl:host";

fn arguments(value: &Json) -> Map<String, Json> {
    value.as_object().cloned().unwrap_or_default()
}

/// The window of the installed Notes package as the person opened it, and the notebook's id.
fn notes_open() -> (Session, String) {
    let mut app = Session::notes();
    let root = app.read(|doc| resource_ids(doc, NOTES)).remove(0);
    (app, root)
}

/// How many items the collections of the notebooks of `doc` hold: the notes.
fn items_in(doc: &mut ui::tilo_resource::Doc) -> usize {
    let chunks = tilolang_tilo::chunk::Chunks::of(doc.clone());
    resource_ids(doc, NOTES)
        .iter()
        .map(|id| {
            tilolang_tilo::action::read_resource(&chunks, NOTES, id).map_or(0, |record| {
                record
                    .schema()
                    .fields
                    .iter()
                    .filter_map(|field| match record.get(&field.id) {
                        Some(tilolang_tilo::DynValue::Collection(items)) => Some(items.len()),
                        _ => None,
                    })
                    .sum::<usize>()
            })
        })
        .sum()
}

fn notes_in(app: &mut Session) -> usize {
    app.read(items_in)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one story: person, session, history, undo"
)]
fn what_an_agent_session_does_is_its_own_for_the_person_and_grouped_under_its_session() {
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
        Some(agent),
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

    let versions = app.read(ui::tilo_resource::history::versions);
    let by_session: Vec<_> = versions
        .iter()
        .filter(|version| {
            version
                .context
                .as_ref()
                .and_then(|context| context.invoker.as_ref())
                .is_some_and(|invoker| invoker.key() == agent_session().0.key())
        })
        .collect();
    assert!(
        by_session.len() >= 2,
        "the agent session's changes are attributed to it: {versions:#?}"
    );
    for version in &by_session {
        let context = version.context.as_ref().unwrap();
        let invoker = context.invoker.as_ref().unwrap();
        assert!(invoker.acting.is_agent());
        assert_eq!(
            invoker.acting.acted_for_person(),
            Some("ada"),
            "for the person"
        );
        assert_eq!(
            context.group.as_ref().map(|group| group.id.as_str()),
            Some("agent-session:7"),
            "grouped under the session"
        );
    }
    let person = versions
        .iter()
        .filter(|version| {
            version
                .context
                .as_ref()
                .and_then(|context| context.invoker.as_ref())
                .is_none_or(|invoker| invoker.key() != agent_session().0.key())
        })
        .filter(|version| {
            version
                .context
                .as_ref()
                .is_some_and(|context| context.what.is_some())
        })
        .collect::<Vec<_>>();
    assert!(
        !person.is_empty(),
        "the person's own changes are in history too"
    );
    for version in &person {
        let context = version.context.as_ref().unwrap();
        assert!(
            context.group.is_none(),
            "a person's own change is ungrouped"
        );
        assert!(
            context
                .invoker
                .as_ref()
                .is_none_or(|invoker| invoker.acting.is_person()),
            "and theirs"
        );
    }

    // Undoing the session takes back what it did, and nothing of the person's.
    let session_scope = Scope::Session("agent-session:7".to_owned());
    assert!(by_session.iter().all(|version| session_scope.owns(version)));
    assert!(person.iter().all(|version| !session_scope.owns(version)));
    let left = app.read(|doc| {
        let entries: Vec<_> = ui::tilo_resource::history::versions(doc)
            .iter()
            .filter(|version| session_scope.owns(version))
            .map(|version| ui::history_source::entry_id_from_hash(&version.hash.to_string()))
            .collect();
        let changes = ui::history_source::revert::changes_of_entries(doc, &entries);
        let authors = ui::history_source::authors::AuthorDirectory::new();
        ui::history_source::revert::revert_changes(doc, &changes, &authors).unwrap();
        doc.commit();
        items_in(doc)
    });
    assert_eq!(left, 1, "only the person's note is left");
}

/// The undo steps of the agent session's that are in effect in `app`'s record, newest first,
/// each the number of changes it takes back.
fn session_steps(app: &mut Session) -> Vec<usize> {
    let scope = Scope::Session("agent-session:7".to_owned());
    app.read(|doc| {
        ui::history_source::steps::steps_in_effect(doc, &|version| scope.owns(version), 16)
            .iter()
            .map(Vec::len)
            .collect()
    })
}

#[test]
fn the_invocations_of_one_agent_session_step_undo_as_one_step_and_separate_calls_as_their_own() {
    let (mut app, root) = notes_open();
    let notebook = format!("tl:{NOTES}:{root}");
    let new_note = action_id(&mut app, &notebook, "New note");
    let (agent, ambient) = agent_session();
    // One program: both invocations carry the one step it named.
    let step = ambient
        .clone()
        .in_gesture(Some(ui::tilo_actions::ActionId::new()));
    for _ in 0..2 {
        app.invoke(
            Some(agent.clone()),
            Some(&step),
            &notebook,
            &new_note,
            &Map::new(),
        )
        .unwrap();
        settle(&mut app);
    }
    assert_eq!(notes_in(&mut app), 2);
    let steps = session_steps(&mut app);
    assert_eq!(steps.len(), 1, "one program is one undo step: {steps:?}");
    assert!(steps[0] >= 2, "it holds both invocations: {steps:?}");
    // Two separate calls: each its own step.
    for _ in 0..2 {
        let call = ambient
            .clone()
            .in_gesture(Some(ui::tilo_actions::ActionId::new()));
        app.invoke(
            Some(agent.clone()),
            Some(&call),
            &notebook,
            &new_note,
            &Map::new(),
        )
        .unwrap();
        settle(&mut app);
    }
    assert_eq!(notes_in(&mut app), 4);
    let steps = session_steps(&mut app);
    assert_eq!(steps.len(), 3, "each call is a step of its own: {steps:?}");
}

#[test]
fn the_installed_package_is_the_app_and_opens_on_its_root_the_action_api_committed() {
    let definition = crate::support::installed("notes");
    assert_eq!(definition.app_id(), NOTES, "the app's id is the package's");
    assert_eq!(definition.title(), "Notes");
    let mut app = Session::notes();
    assert_eq!(
        app.read(|doc| resource_ids(doc, NOTES)).len(),
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
    // The record's history, as an agent or a person asks the Action API for it.
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
        sentences.contains(&"New note: Untitled"),
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
fn an_agent_reads_what_a_record_holds_and_each_item_it_names() {
    let (mut app, root) = notes_open();
    let notebook = format!("tl:{NOTES}:{root}");
    let before: Vec<String> = app.read(|doc| {
        doc.committed_document()
            .get_heads()
            .iter()
            .map(ToString::to_string)
            .collect()
    });
    let new_note = action_id(&mut app, &notebook, "New note");
    app.invoke(None, None, &notebook, &new_note, &Map::new())
        .unwrap();
    settle(&mut app);
    let read_at = |app: &mut Session, resource: &str, version: Option<&str>| {
        let read = action_id(app, resource, "Read the content");
        let (invoker, context) = agent_session();
        let mut arguments = Map::new();
        if let Some(version) = version {
            arguments.insert("version".into(), json!(version));
        }
        app.invoke(Some(invoker), Some(&context), resource, &read, &arguments)
            .unwrap()
            .value
            .unwrap()
    };
    let read = |app: &mut Session, resource: &str| read_at(app, resource, None);
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

    // A version of the past is read as it was, and the window's runtime stays where it is.
    let past = read_at(&mut app, &notebook, Some(&before.join("+")));
    assert_eq!(
        past["content"]["fields"]["notes"]["count"],
        json!(0),
        "{past:#}"
    );
    let now = read(&mut app, &notebook);
    assert_eq!(now["fields"]["notes"]["count"], json!(1), "{now:#}");
}

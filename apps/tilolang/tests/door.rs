//! What an agent's task does through the door is the task's, for the person it acts for, and
//! history and undo group it by task like any app's agent-driven change.

use serde_json::{Map, Value as Json, json};
use tilolang_app::definition;
use tilolang_tilo::action::resource_ids;
use ui::framework::testing::HostedApp;
use ui::history_source::steps::Scope;
use ui::tilo_actions::{ActionContext, ActionInvoker, ActorKind};

const NOTES: &str = "so.tilo.notes";
const HOST: &str = "tl:host";

/// An agent's task `7` on behalf of `ada`: the delegate the door admitted, and the task its work
/// is grouped under.
fn task() -> (ActionInvoker, ActionContext) {
    (
        ActionInvoker::actor("agent-task:7")
            .with_kind(ActorKind::Agent)
            .acting_for("ada"),
        ActionContext::default().with_group("task:7", "Tidy the notes", "task"),
    )
}

fn arguments(value: &Json) -> Map<String, Json> {
    value.as_object().cloned().unwrap_or_default()
}

fn settle(app: &mut HostedApp) {
    for _ in 0..12 {
        app.harness()
            .settle_for_capture(ui::time::Duration::from_millis(300));
        app.harness().relayout();
    }
}

/// The window with Notes opened by the person, and the notebook's id.
fn notes_open() -> (HostedApp, String) {
    let mut app = HostedApp::open(definition(), "ada").unwrap();
    settle(&mut app);
    // Open Notes the way a person's click does: its card is a control published on the door.
    app.invoke(
        None,
        None,
        "tilo.control:open-so.tilo.notes",
        "invoke",
        &Map::new(),
        false,
    )
    .unwrap();
    settle(&mut app);
    let root = app.read(|doc| resource_ids(doc, NOTES)).remove(0);
    (app, root)
}

/// The id of the action called `name` of the published resource `resource`.
fn action_id(app: &mut HostedApp, resource: &str, name: &str) -> String {
    app.harness()
        .resources()
        .iter()
        .find(|published| published.id.as_str() == resource)
        .and_then(|published| published.actions.iter().find(|action| action.label == name))
        .map_or_else(
            || panic!("{resource} publishes no `{name}`"),
            |action| action.id.clone(),
        )
}

/// How many items the collections of the notebooks of `doc` hold: the notes.
fn items_in(doc: &mut ui::tilo_resource::Doc) -> usize {
    resource_ids(doc, NOTES)
        .iter()
        .map(|id| {
            tilolang_tilo::action::read_resource(doc, NOTES, id).map_or(0, |record| {
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

fn notes_in(app: &mut HostedApp) -> usize {
    app.read(items_in)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one story: person, task, history, undo"
)]
fn what_a_task_does_through_the_door_is_its_own_for_the_person_and_grouped_under_the_task() {
    let (mut app, root) = notes_open();
    let notebook = format!("tl:{NOTES}:{root}");
    // The person adds a note themselves.
    let new_note = action_id(&mut app, &notebook, "new_note");
    app.invoke(None, None, &notebook, &new_note, &Map::new(), false)
        .unwrap();
    settle(&mut app);
    // The task adds one by a block, and one by the published verb.
    let (agent, ambient) = task();
    let block = format!("notes.new_note(@notes.Notebook:{root})");
    let run_block = action_id(&mut app, HOST, "Run a block");
    let answer = app
        .invoke(
            Some(agent.clone()),
            Some(&ambient),
            HOST,
            &run_block,
            &arguments(&json!({ "source": block })),
            false,
        )
        .unwrap();
    assert_eq!(answer.value.unwrap()["state"], "done");
    app.invoke(
        Some(agent),
        Some(&ambient),
        &notebook,
        &new_note,
        &Map::new(),
        false,
    )
    .unwrap();
    settle(&mut app);
    assert_eq!(notes_in(&mut app), 3, "one by the person, two by the task");

    let versions = app.read(ui::tilo_resource::history::versions);
    let by_task: Vec<_> = versions
        .iter()
        .filter(|version| {
            version
                .context
                .as_ref()
                .and_then(|context| context.invoker.as_ref())
                .is_some_and(|invoker| invoker.actor == "agent-task:7")
        })
        .collect();
    assert!(
        by_task.len() >= 2,
        "the task's changes are attributed to it: {versions:#?}"
    );
    for version in &by_task {
        let context = version.context.as_ref().unwrap();
        let invoker = context.invoker.as_ref().unwrap();
        assert_eq!(invoker.kind, ActorKind::Agent);
        assert_eq!(
            invoker.on_behalf_of.as_deref(),
            Some("ada"),
            "for the person"
        );
        assert_eq!(
            context.group.as_ref().map(|group| group.id.as_str()),
            Some("task:7"),
            "grouped under the task"
        );
    }
    let person = versions
        .iter()
        .filter(|version| {
            version
                .context
                .as_ref()
                .and_then(|context| context.invoker.as_ref())
                .is_none_or(|invoker| invoker.actor != "agent-task:7")
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
                .is_none_or(|invoker| invoker.kind == ActorKind::Person),
            "and theirs"
        );
    }

    // Undoing the task takes back what the task did, and nothing of the person's.
    let task_scope = Scope::Run("task:7".to_owned());
    assert!(by_task.iter().all(|version| task_scope.owns(version)));
    assert!(person.iter().all(|version| !task_scope.owns(version)));
    let left = app.read(|doc| {
        let entries: Vec<_> = ui::tilo_resource::history::versions(doc)
            .iter()
            .filter(|version| task_scope.owns(version))
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

#[test]
fn a_click_on_an_apps_card_opens_it_and_its_root_is_committed_through_the_door() {
    let mut app = HostedApp::open(definition(), "ada").unwrap();
    settle(&mut app);
    assert!(
        app.read(|doc| resource_ids(doc, NOTES)).is_empty(),
        "the home creates nothing"
    );
    app.harness().click_named("open-so.tilo.notes").unwrap();
    settle(&mut app);
    assert_eq!(
        app.read(|doc| resource_ids(doc, NOTES)).len(),
        1,
        "the notebook exists"
    );
}

/// The id of the control whose accessible name is `name`.
fn control_named(app: &mut HostedApp, name: &str) -> String {
    fn find(node: &ui::headless::InspectionNode, name: &str) -> Option<String> {
        let named = node
            .semantics
            .as_ref()
            .is_some_and(|access| access.name.as_deref() == Some(name));
        if named && let Some(id) = &node.test_id {
            return Some(id.clone());
        }
        node.children.iter().find_map(|child| find(child, name))
    }
    find(&app.harness().inspection().root, name)
        .unwrap_or_else(|| panic!("no control named `{name}`"))
}

#[test]
fn a_persons_click_on_a_button_runs_the_action_and_history_names_them() {
    let mut app = HostedApp::open(definition(), "ada").unwrap();
    settle(&mut app);
    app.harness().click_named("open-so.tilo.counter").unwrap();
    settle(&mut app);
    for _ in 0..3 {
        let plus = control_named(&mut app, "+1");
        app.harness().click_named(&plus).unwrap();
        settle(&mut app);
    }
    let counter = app.read(|doc| {
        let id = resource_ids(doc, "so.tilo.counter").remove(0);
        let record = tilolang_tilo::action::read_resource(doc, "so.tilo.counter", &id).unwrap();
        record
            .schema()
            .fields
            .iter()
            .find_map(|field| match record.get(&field.id) {
                Some(tilolang_tilo::DynValue::Counter(counter)) => Some(counter.get()),
                _ => None,
            })
            .unwrap()
    });
    assert_eq!(counter, 3, "each click counted once");
    let versions = app.read(ui::tilo_resource::history::versions);
    let presses = versions
        .iter()
        .filter(|version| {
            version.context.as_ref().is_some_and(|context| {
                context
                    .what
                    .as_ref()
                    .is_some_and(|did| format!("{did:?}").contains("Verb"))
            })
        })
        .count();
    assert!(presses >= 3, "the presses are in history: {versions:#?}");
    for version in &versions {
        let context = version.context.as_ref();
        assert!(
            context.is_none_or(|context| context.group.is_none()),
            "a person's own change is ungrouped"
        );
    }
}

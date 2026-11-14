//! One undo, as the record's platform history verb (`collaboration::history_source::verbs`):
//! `undo(v)` on a step and `undo(run)` on an agent's run, which the tilolang host running the
//! instance applies to the record.

use tilolang_tilo::DynValue;
use ui::tilo_actions::{ActionContext, ActionInvoker};

use super::Session;

const TASKS: &str = "so.tilo.lang.tasks";
const RUN: &str = "issue-7@8000000000000001";

/// An agent's run of `issue-7`, acting for ada, as the broker stamps its changes.
fn marked_run() -> (ActionInvoker, ActionContext) {
    (
        ActionInvoker::agent("helper").acting_for("ada"),
        ActionContext::default().with_group(
            RUN,
            "Tidy the tasks",
            ui::tilo_actions::context::TASK_RUN_GROUP,
        ),
    )
}

fn titled(title: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("title".to_owned(), title.into());
    arguments
}

/// The titles of the tasks in the root, in order.
fn titles(doc: &mut ui::tilo_resource::Doc) -> Vec<String> {
    let root = tilolang_tilo::action::resource_ids(doc, TASKS).remove(0);
    let chunks = tilolang_tilo::chunk::Chunks::of(doc.clone());
    let record =
        tilolang_tilo::action::read_resource(&chunks, TASKS, &root).expect("the root reads");
    record
        .schema()
        .fields
        .iter()
        .filter_map(|field| match record.get(&field.id) {
            Some(DynValue::Collection(items)) => Some(items),
            _ => None,
        })
        .flat_map(|items| items.iter())
        .filter_map(|(_, item)| {
            let title = item
                .schema()
                .fields
                .iter()
                .find(|field| field.kind == tilolang_tilo::MergeKind::Text)?;
            match item.get(&title.id) {
                Some(DynValue::Text(title)) => Some(title.clone()),
                _ => None,
            }
        })
        .collect()
}

/// The change id of the person's own step whose arguments name `title`, outside any run.
fn persons_step(doc: &mut ui::tilo_resource::Doc, title: &str) -> String {
    ui::tilo_resource::history::versions(doc)
        .into_iter()
        .find(|version| {
            version.context.as_ref().is_some_and(|context| {
                context.group.is_none()
                    && context.what.as_ref().is_some_and(|did| {
                        format!("{did:?}").contains(&format!("String({title:?})"))
                    })
            })
        })
        .unwrap_or_else(|| {
            panic!(
                "the person's own step naming {title}: {:?}",
                ui::tilo_resource::history::versions(&mut doc.clone())
                    .iter()
                    .filter_map(|version| version.context.as_ref())
                    .map(|context| format!("{:?}", context.what))
                    .collect::<Vec<_>>()
            )
        })
        .hash
        .to_string()
}

/// The root the window shows (`tl:<app>:<id>`), once published.
fn root(session: &mut Session) -> String {
    session.settle();
    session
        .instance()
        .published()
        .iter()
        .find(|d| {
            d.address.resource.starts_with(&format!("tl:{TASKS}:")) && d.action_label == "New task"
        })
        .map(|d| d.address.resource.clone())
        .expect("the root is published")
}

/// The id of the action named `name` published on `resource`.
fn action(session: &Session, resource: &str, name: &str) -> String {
    session
        .instance()
        .published()
        .iter()
        .find(|d| d.address.resource == resource && d.address.action == name)
        .map_or_else(
            || panic!("{resource} publishes no `{name}`"),
            |d| d.address.action.clone(),
        )
}

/// The person adds a task and takes back exactly that step with `undo(v)`; an agent's run adds
/// three and `undo(run)` takes back all three, leaving the person's own.
#[test]
fn undo_named_from_a_view_takes_back_a_step_and_a_run() {
    let mut window = Session::tasks();
    let root = root(&mut window);
    let add = action(&window, &root, "new_task");
    // The tilolang host publishes the history verbs every app has on the app's own resource.
    let record = tilolang_tilo::publish::package_resource_id(TASKS);
    window
        .invoke(None, None, &root, &add, &titled("Call the landlord"))
        .unwrap();
    assert_eq!(window.read(titles), ["Call the landlord"]);

    // `undo(v)` on the person's own step: exactly that step comes back, and nothing else.
    let person = window.read(|doc| persons_step(doc, "Call the landlord"));
    let mut undo_step = serde_json::Map::new();
    undo_step.insert("step".to_owned(), person.into());
    window
        .invoke(None, None, &record, "undo", &undo_step)
        .unwrap();
    assert_eq!(
        window.read(titles),
        Vec::<String>::new(),
        "`undo(v)` takes back exactly the step the history named"
    );

    // They add one again, and an agent's run adds three: one step of theirs, one run.
    window
        .invoke(None, None, &root, &add, &titled("Call the landlord"))
        .unwrap();
    let (agent, ambient) = marked_run();
    for title in ["Book the dentist", "Pay the rent", "Water the plants"] {
        window
            .invoke(
                Some(agent.clone()),
                Some(&ambient),
                &root,
                &add,
                &titled(title),
            )
            .unwrap();
    }
    assert_eq!(window.read(titles).len(), 4);

    // `undo(run)`: every step of the run comes back together, and the person's own step stays.
    let mut undo_run = serde_json::Map::new();
    undo_run.insert("run".to_owned(), RUN.into());
    window
        .invoke(None, None, &record, "undo", &undo_run)
        .unwrap();
    assert_eq!(
        window.read(titles),
        ["Call the landlord"],
        "the run's three adds are one step: all of them back, the person's own task kept"
    );
}

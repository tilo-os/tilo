//! One undo, as the platform's history verb (`tilo_resource_store::history::verbs`): `undo(v)`
//! on a step, which the tilolang host running the instance applies to the workspace's store.

use super::Session;

const TASKS: &str = "so.tilo.tasks";

fn titled(title: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("title".to_owned(), title.into());
    arguments
}

/// The titles of the tasks in the root, in order.
fn titles(session: &mut Session) -> Vec<String> {
    use tilolang_tilo::runtime::Value;
    session.settle();
    let host = session.instance_host();
    let root = host.root_of(TASKS).expect("the tasks root");
    let Ok(Value::List(items)) = host.runtime().read(&root, "items") else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| match item {
            Value::Ref(item) => match host.runtime().read(&item.target, "title") {
                Ok(Value::Text(title)) => Some(title),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The task titled `title`, as the workspace's store holds it.
fn task(session: &Session, title: &str) -> tilolang_tilo::runtime::value::ResourceRef {
    use tilolang_tilo::runtime::Value;
    let host = session.instance_host();
    let root = host.root_of(TASKS).expect("the tasks root");
    let Ok(Value::List(items)) = host.runtime().read(&root, "items") else {
        panic!("the workspace lists its tasks");
    };
    items
        .into_iter()
        .find_map(|item| match item {
            Value::Ref(item)
                if matches!(
                    host.runtime().read(&item.target, "title"),
                    Ok(Value::Text(said)) if said == title
                ) =>
            {
                Some((*item.target).clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("the task naming {title}"))
}

/// The versions of the task titled `title`, oldest first.
fn versions_of(session: &Session, title: &str) -> Vec<tilolang_tilo::runtime::store::Version> {
    use tilolang_tilo::runtime::store::Store as _;
    session
        .instance_host()
        .store()
        .versions(&task(session, title))
        .expect("the task's history")
}

/// The invocation of the person's own step whose arguments name `title`.
fn persons_step(session: &Session, title: &str) -> String {
    versions_of(session, title)
        .into_iter()
        .rev()
        .find(|version| {
            version.gesture.is_some()
                && version.who.as_ref().is_some_and(|who| {
                    let who = format!("{who:?}");
                    who.contains("ada") && !who.contains("helper")
                })
        })
        .and_then(|version| version.gesture)
        .unwrap_or_else(|| panic!("the person's own step naming {title}"))
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

/// The person adds a task and takes back exactly that step with `undo(v)`, as a view names it.
#[test]
fn undo_named_from_a_view_takes_back_that_step() {
    let mut window = Session::tasks();
    let root = root(&mut window);
    let add = action(&window, &root, "new_task");
    // The tilolang host publishes the history verbs every app has on the app's own resource.
    let record = tilolang_tilo::publish::package_resource_id(TASKS);
    window
        .invoke(None, None, &root, &add, &titled("Call the landlord"))
        .unwrap();
    assert_eq!(titles(&mut window), ["Call the landlord"]);

    // `undo(v)` on the person's own step: exactly that step comes back, and nothing else.
    let person = persons_step(&window, "Call the landlord");
    let mut undo_step = serde_json::Map::new();
    undo_step.insert("step".to_owned(), person.into());
    window
        .invoke(None, None, &record, "undo", &undo_step)
        .unwrap();
    assert_eq!(
        titles(&mut window),
        Vec::<String>::new(),
        "`undo(v)` takes back exactly the step the history named"
    );
}

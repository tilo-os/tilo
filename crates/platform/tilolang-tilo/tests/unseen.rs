//! What changed since a person last looked, in the Tasks app: Bo's change marks Ada's row and
//! counts beside her place, her own change never does, opening what changed clears both, and
//! the task's page draws the line where what she saw ends over the steps since.

use automerge::AutoCommit;
use tilolang_runtime::{InvokeOutcome, Principal, ResourceRef, UiNode, Value, ViewInstance};

use crate::fixtures::Replica;

const TASKS: &str = include_str!("../../../../apps/tl/tasks/app.tl");
const TASKS_IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
const APP: &str = "so.tilo.lang.tasks";

fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
        .id
        .clone();
    match r
        .host
        .runtime()
        .invoke_action(&Principal::new(&r.name), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

/// Every node of the tree, in the order it stands.
fn nodes<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
    out.push(node);
    for child in &node.children {
        nodes(child, out);
    }
}

fn all<'a>(node: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
    let mut out = Vec::new();
    nodes(node, &mut out);
    out.into_iter().filter(|node| node.kind == kind).collect()
}

fn target(v: &Value) -> ResourceRef {
    match v {
        Value::Ref(r) => (*r.target).clone(),
        other => panic!("a resource: {other:?}"),
    }
}

fn id(v: &Value) -> String {
    target(v).id_string()
}

/// Ada's replica of a workspace of tasks, and Bo's of the same one, at 10:00 on Thursday
/// 2026-10-29.
fn workspace() -> (Replica, Replica, Value) {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(TASKS, TASKS_IDS)]);
    ada.host
        .runtime()
        .set_clock(|| "2026-10-29T10:00:00Z".to_owned());
    let root = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let bo = ada.fork("bo", &[(TASKS, TASKS_IDS)]);
    (ada, bo, root)
}

/// The tasks window's places: each label, and how many of what it holds changed since the
/// person last looked (the count the sidebar draws in their own colour).
fn unseen_counts(view: &ViewInstance) -> Vec<(String, Option<i64>)> {
    all(view.tree(), "ui.NavItem")
        .into_iter()
        .filter_map(|node| {
            let Some(Value::Text(label)) = node.props.get("label") else {
                return None;
            };
            let unseen = match node.props.get("unseen") {
                Some(Value::Int(count)) => Some(*count),
                _ => None,
            };
            Some((label.clone(), unseen))
        })
        .collect()
}

fn unseen_count(view: &ViewInstance, place: &str) -> Option<i64> {
    unseen_counts(view)
        .into_iter()
        .find(|(label, _)| label == place)
        .unwrap_or_else(|| panic!("no place {place} in {:?}", unseen_counts(view)))
        .1
}

/// The rows the window's list draws, each as the resource it presents, and the change mark the
/// host placed on it when it carries one.
fn rows(view: &mut ViewInstance) -> Vec<(String, Option<UiNode>)> {
    view.poll_patches().unwrap();
    let found = all(view.tree(), "ui.VirtualList");
    let Some(list) = found.first() else {
        return Vec::new();
    };
    let list = list.key.clone();
    let len = view.list_len(&list).unwrap_or(0);
    let mut out = Vec::new();
    for i in 0..len {
        let Ok(Some(row)) = view.row(&list, i) else {
            continue;
        };
        for item in all(row, "ui.ListItem") {
            let Some(Value::Ref(r)) = item.props.get("item") else {
                continue;
            };
            let mark = all(item, "ui.ChangeMark")
                .first()
                .map(|mark| (*mark).clone());
            out.push((r.target.id_string(), mark));
        }
    }
    out
}

fn marked(view: &mut ViewInstance) -> Vec<String> {
    rows(view)
        .into_iter()
        .filter(|(_, mark)| mark.is_some())
        .map(|(at, _)| at)
        .collect()
}

fn mark_of(view: &mut ViewInstance, resource: &str) -> Option<UiNode> {
    rows(view)
        .into_iter()
        .find(|(at, _)| at == resource)
        .unwrap_or_else(|| panic!("no row for {resource}"))
        .1
}

#[test]
fn bo_changes_mark_ad_as_row_and_place_and_her_own_changes_never_do() {
    let (mut ada, mut bo, root) = workspace();
    let rent = call(
        &ada,
        "new_task",
        vec![root.clone(), Value::text("Pay rent")],
    );
    let milk = call(
        &ada,
        "new_task",
        vec![root.clone(), Value::text("Buy oat milk")],
    );
    ada.commit();
    ada.sync_with(&mut bo);

    // Ada's own changes mark nothing for her, neither on a row nor beside a place.
    let mut view = ada.view(APP);
    assert!(marked(&mut view).is_empty(), "{:?}", marked(&mut view));
    assert!(
        unseen_counts(&view)
            .iter()
            .all(|(_, unseen)| unseen.is_none()),
        "{:?}",
        unseen_counts(&view)
    );

    // Bo raises the rent's priority and makes a task of his own.
    call(&bo, "set_priority_high", vec![rent.clone()]);
    let present = call(
        &bo,
        "new_task",
        vec![root.clone(), Value::text("Order a present")],
    );
    bo.commit();
    ada.sync_with(&mut bo);

    // Her row for what Bo changed, and his new task's row as one he made: it is new to her,
    // not changed under her hands.
    let mut view = ada.view(APP);
    let changed = marked(&mut view);
    assert!(changed.contains(&id(&rent)), "{:?}", rows(&mut view));
    let mark = mark_of(&mut view, &id(&rent)).expect("the row is marked");
    assert_eq!(
        mark.props.get("by"),
        Some(&Value::Principal(Principal::new("bo")))
    );
    assert_eq!(mark.props.get("kind"), Some(&Value::text("changed")));
    let made = mark_of(&mut view, &id(&present)).expect("his task's row is marked");
    assert_eq!(made.props.get("kind"), Some(&Value::text("created")));
    // The place counts what changed under her hands, not what appeared: Inbox's open count
    // says the task is there, its changed count says what moved under her.
    // The place that holds it counts it; the places that hold nothing changed count nothing.
    assert_eq!(
        unseen_count(&view, "Inbox"),
        Some(1),
        "{:?}",
        unseen_counts(&view)
    );
    assert_eq!(
        unseen_count(&view, "Today"),
        None,
        "{:?}",
        unseen_counts(&view)
    );

    // Ada changes the other task herself: still nothing marked for her, and the count stands.
    call(
        &ada,
        "schedule",
        vec![milk.clone(), Value::some(Value::text("2026-10-29"))],
    );
    ada.commit();
    ada.sync_with(&mut bo);
    let mut view = ada.view(APP);
    assert!(
        !marked(&mut view).contains(&id(&milk)),
        "{:?}",
        rows(&mut view)
    );
    assert_eq!(
        unseen_count(&view, "Inbox"),
        Some(1),
        "{:?}",
        unseen_counts(&view)
    );

    // Seen (as opening it in a window of its own marks it), neither the row nor the place
    // counts it any more.
    assert!(ada.host.runtime().mark_seen("ada", &target(&rent)).unwrap());
    ada.commit();
    let mut view = ada.view(APP);
    assert!(
        !marked(&mut view).contains(&id(&rent)),
        "{:?}",
        marked(&mut view)
    );
    assert_eq!(
        unseen_count(&view, "Inbox"),
        None,
        "{:?}",
        unseen_counts(&view)
    );
}

/// What a page shows of its history: the steps by who made them, and where the line since the
/// person last looked stands among them (how many stand over it), in the order they draw.
fn page(view: &mut ViewInstance) -> (Vec<String>, Option<usize>, Vec<&'static str>) {
    view.poll_patches().unwrap();
    let section = all(view.tree(), "ui.Section")
        .into_iter()
        .find(|node| node.props.get("title") == Some(&Value::text("tilolang-history")))
        .unwrap_or_else(|| panic!("the page shows its history: {:#?}", view.tree()));
    let mut order = Vec::new();
    let mut steps = Vec::new();
    let mut line = None;
    let mut flat = Vec::new();
    nodes(section, &mut flat);
    for node in flat {
        if node.kind == "ui.Avatar" {
            order.push("step");
            if let Some(Value::Principal(who)) = node.props.get("who") {
                steps.push(who.0.clone());
            }
        } else if node.kind == "ui.UnreadDivider" {
            order.push("line");
            line = Some(steps.len());
        }
    }
    (steps, line, order)
}

#[test]
fn a_tasks_page_stands_the_line_since_ada_last_looked_over_the_steps_since() {
    let (mut ada, mut bo, root) = workspace();
    let rent = call(
        &ada,
        "new_task",
        vec![root.clone(), Value::text("Pay rent")],
    );
    ada.commit();
    ada.sync_with(&mut bo);
    call(&bo, "set_priority_high", vec![rent.clone()]);
    bo.commit();
    ada.sync_with(&mut bo);

    // The page draws Bo's step first, the line under it, and what Ada herself did below.
    let mut view = ada.host.open_resource(&target(&rent), None).unwrap();
    let (steps, line, order) = page(&mut view);
    assert_eq!(steps, ["bo", "ada"], "{order:?}");
    assert_eq!(
        line,
        Some(1),
        "the line stands over what she has not looked at: {order:?}"
    );
    assert_eq!(order, ["step", "line", "step"], "{order:?}");

    // Seen, and the line goes: what she looked at stands with the rest.
    assert!(ada.host.runtime().mark_seen("ada", &target(&rent)).unwrap());
    let mut view = ada.host.open_resource(&target(&rent), None).unwrap();
    let (steps, line, _) = page(&mut view);
    assert_eq!(steps, ["bo", "ada"]);
    assert_eq!(line, None, "nothing stands since she last looked");
}

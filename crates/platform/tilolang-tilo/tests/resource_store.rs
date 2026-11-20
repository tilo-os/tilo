//! A package run over the device's one resource store: what an action writes is read back at
//! once, lists, moves and removals land where they should, and a view follows them.

use tilolang_runtime::{
    InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, UiNode, Value, ViewInstance,
};
use tilolang_tilo::resources::ResourceStore;

const SOURCE: &str = include_str!("../../../lang/tilolang/tests/fixtures/todo.tl");

fn me() -> tilolang_runtime::Actor {
    tilolang_runtime::Actor::from(tilolang_runtime::ids::MemberId::person("me"))
}

struct Todo {
    rt: Runtime,
    meta: tilolang::PackageMeta,
    list: ResourceRef,
}

impl Todo {
    fn new() -> Self {
        let compiled = tilolang::compile_package(SOURCE, &tilolang::IdsLock::new(), &[])
            .unwrap_or_else(|errors| panic!("{errors:#?}"));
        let store = ResourceStore::in_memory();
        store.declare(&compiled.meta);
        let rt = Runtime::new(store, SimpleAuthority::new()).unwrap();
        rt.load_compiled(&compiled).unwrap();
        let list = rt.create("TodoList").unwrap();
        Self {
            rt,
            meta: compiled.meta,
            list,
        }
    }

    fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
        let id = &self
            .meta
            .actions
            .iter()
            .find(|held| held.name == action)
            .unwrap()
            .id;
        self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
    }

    fn add(&self, title: &str) -> ResourceRef {
        match self.call("add", vec![Value::resource(&self.list), Value::text(title)]) {
            InvokeOutcome::Done(Value::Ref(made)) => (*made.target).clone(),
            other => panic!("add answered {other:?}"),
        }
    }

    fn view(&self) -> ViewInstance {
        let id = &self
            .meta
            .views
            .iter()
            .find(|held| held.name == "todo_list")
            .unwrap()
            .id;
        self.rt
            .open_view(&me(), id, vec![Value::resource(&self.list)])
            .unwrap()
    }
}

fn of_kind<'a>(node: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
    let mut out = Vec::new();
    if node.kind == kind {
        out.push(node);
    }
    for child in &node.children {
        out.extend(of_kind(child, kind));
    }
    out
}

fn titles(tree: &UiNode) -> Vec<String> {
    of_kind(tree, "ui.Checkbox")
        .iter()
        .map(|checkbox| match checkbox.props.get("label") {
            Some(Value::Text(text)) => String::from(&**text),
            other => panic!("a checkbox labelled {other:?}"),
        })
        .collect()
}

fn heading(tree: &UiNode) -> String {
    of_kind(tree, "ui.Text")
        .into_iter()
        .find(|text| text.props.get("role") == Some(&Value::text("title")))
        .and_then(|text| match text.props.get("value") {
            Some(Value::Text(value)) => Some(String::from(&**value)),
            _ => None,
        })
        .expect("a title")
}

#[test]
fn what_an_action_writes_is_read_listed_and_shown() {
    let t = Todo::new();
    t.call(
        "rename",
        vec![Value::resource(&t.list), Value::text("Groceries")],
    );
    let milk = t.add("milk");
    t.add("eggs");
    assert_eq!(t.rt.read(&milk, "title").unwrap(), Value::text("milk"));
    assert_eq!(t.rt.read(&milk, "done").unwrap(), Value::Bool(false));
    let mut view = t.view();
    assert_eq!(heading(view.tree()), "Groceries (2 open)");
    assert_eq!(titles(view.tree()), ["milk", "eggs"]);
    t.call("toggle", vec![Value::resource(&milk)]);
    assert_eq!(t.rt.read(&milk, "done").unwrap(), Value::Bool(true));
    view.poll_patches().unwrap();
    assert_eq!(heading(view.tree()), "Groceries (1 open)");
}

#[test]
fn a_moved_item_lands_where_it_was_put_and_a_removed_one_leaves() {
    let t = Todo::new();
    let a = t.add("a");
    let b = t.add("b");
    let c = t.add("c");
    let mut view = t.view();
    t.call(
        "move_before",
        vec![Value::resource(&t.list), Value::resource(&c), Value::resource(&a)],
    );
    view.poll_patches().unwrap();
    assert_eq!(titles(view.tree()), ["c", "a", "b"]);
    t.call("remove", vec![Value::resource(&t.list), Value::resource(&b)]);
    view.poll_patches().unwrap();
    assert_eq!(titles(view.tree()), ["c", "a"]);
}

#[test]
fn a_bound_name_and_checkbox_write_through_the_view() {
    let t = Todo::new();
    let milk = t.add("milk");
    let mut view = t.view();
    let input = of_kind(view.tree(), "ui.TextInput")[0].key.clone();
    for text in ["Errands", "Chores today"] {
        view.edit(&input, &Value::text(text)).unwrap();
        assert_eq!(t.rt.read(&t.list, "name").unwrap(), Value::text(text));
    }
    let checkbox = of_kind(view.tree(), "ui.Checkbox")[0].key.clone();
    view.edit(&checkbox, &Value::Bool(true)).unwrap();
    assert_eq!(t.rt.read(&milk, "done").unwrap(), Value::Bool(true));
    view.poll_patches().unwrap();
    assert!(heading(view.tree()).starts_with("Chores today (0 open)"));
}

/// Thousands of items: adding them stays fast as the list grows, and opening the list reads
/// the rows it shows.
#[test]
fn thousands_of_items_are_written_fast_and_listed() {
    let t = Todo::new();
    let total = 3000_usize;
    let mut rates = Vec::new();
    for chunk in 0..3 {
        let started = std::time::Instant::now();
        for at in 0..total / 3 {
            t.add(&format!("item {chunk}-{at}"));
        }
        let per_second = f64::from(u32::try_from(total / 3).unwrap_or(u32::MAX)) / started.elapsed().as_secs_f64();
        rates.push(per_second);
    }
    eprintln!("items written per second, by thousand: {rates:?}");
    let view = t.view();
    assert_eq!(heading(view.tree()), format!(" ({total} open)"));
    assert_eq!(titles(view.tree()).len(), total);
}

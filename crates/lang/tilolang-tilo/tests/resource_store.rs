//! A package run over the device's one resource store: what an action writes is read back at
//! once, lists, moves and removals land where they should, and a view follows them.

use tilolang_runtime::{
    InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, UiNode, Value, ViewInstance,
};
use tilolang_tilo::resources::ResourceStore;

const SOURCE: &str = include_str!("../../tilolang/tests/fixtures/todo.tl");

fn compile() -> tilolang::Compiled {
    tilolang::compile_package(SOURCE, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|errors| panic!("{errors:#?}"))
}

/// The workspace's store as a session's workspace service keeps it: every app surface the device
/// knows declares its types there, and each write is drafted, checked against them, and only then
/// made, as the service checks a write before it keeps it.
struct Declared(std::cell::RefCell<tilo_resource_store::Store>);

impl Declared {
    fn of(surfaces: &[collab_types::surface::SurfaceFile]) -> Self {
        let mut store = tilo_resource_store::Store::new();
        for file in surfaces {
            for surface in &file.resources {
                store.declare(tilo_resource_store::Schema::from_surface(surface));
            }
        }
        Self(std::cell::RefCell::new(store))
    }
}

impl tilo_resource_store::Port for Declared {
    fn command(
        &self,
        command: tilo_resource_store::wire::Command,
    ) -> Result<tilo_resource_store::wire::Reply, String> {
        use tilo_resource_store::wire;
        let mut store = self.0.borrow_mut();
        if !command.writes() {
            return wire::read(&store, &command, &|_| true)
                .ok_or_else(|| "the store answered no read".to_owned());
        }
        let drafted =
            wire::draft(&mut store, command.clone()).map_err(|error| error.to_string())?;
        for write in &drafted {
            store
                .check_write(write, [7; 32])
                .map_err(|refusal| format!("refused: {refusal:?}"))?;
        }
        wire::write(&mut store, command).map_err(|error| error.to_string())?;
        Ok(wire::Reply::Written)
    }
}

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
        Self::over(ResourceStore::in_memory(), compile())
    }

    /// The package `compiled` run over `store`.
    fn over(store: ResourceStore, compiled: tilolang::Compiled) -> Self {
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

fn titles(tree: &UiNode) -> Vec<String> {
    tree.of_kind("ui.Checkbox")
        .iter()
        .map(|checkbox| match checkbox.props.get("label") {
            Some(Value::Text(text)) => String::from(&**text),
            other => panic!("a checkbox labelled {other:?}"),
        })
        .collect()
}

fn heading(tree: &UiNode) -> String {
    tree.of_kind("ui.Text")
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
        vec![
            Value::resource(&t.list),
            Value::resource(&c),
            Value::resource(&a),
        ],
    );
    view.poll_patches().unwrap();
    assert_eq!(titles(view.tree()), ["c", "a", "b"]);
    t.call(
        "remove",
        vec![Value::resource(&t.list), Value::resource(&b)],
    );
    view.poll_patches().unwrap();
    assert_eq!(titles(view.tree()), ["c", "a"]);
}

#[test]
fn a_bound_name_and_checkbox_write_through_the_view() {
    let t = Todo::new();
    let milk = t.add("milk");
    let mut view = t.view();
    let input = view.tree().of_kind("ui.TextInput")[0].key.clone();
    for text in ["Errands", "Chores today"] {
        view.edit(&input, &Value::text(text)).unwrap();
        assert_eq!(t.rt.read(&t.list, "name").unwrap(), Value::text(text));
    }
    let checkbox = view.tree().of_kind("ui.Checkbox")[0].key.clone();
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
        let per_second = f64::from(u32::try_from(total / 3).unwrap_or(u32::MAX))
            / started.elapsed().as_secs_f64();
        rates.push(per_second);
    }
    eprintln!("items written per second, by thousand: {rates:?}");
    let view = t.view();
    assert_eq!(heading(view.tree()), format!(" ({total} open)"));
    assert_eq!(titles(view.tree()).len(), total);
}

/// A tilolang app's writes land in a workspace whose other apps declare their own types: what
/// each app's surface declares judges that app's kinds, and none refuses another app's.
#[test]
fn an_apps_writes_land_beside_the_types_other_apps_declare() {
    let compiled = compile();
    let own =
        tilolang_tilo::surface::surface_toml(&[&compiled.meta], &tilolang_runtime::SourceWords)
            .unwrap();
    // An app whose surface declares the types it keeps, as a native app's does.
    let other = collab_types::surface::SurfaceFile::from_json(
        &serde_json::json!({"resources": [{
            "kind": "tasks.tracker", "label": "Tracker", "version": 1, "fields": [],
            "types": [
                {"name": "Tracker", "fields": [{"name": "name", "type": "text"}]},
                {"name": "Thread", "fields": [{"name": "subject", "type": "text"}]},
            ],
        }]})
        .to_string(),
    )
    .unwrap();
    let surfaces = [
        collab_types::surface::SurfaceFile::parse(&own).unwrap(),
        other,
    ];
    let t = Todo::over(
        ResourceStore::new(std::rc::Rc::new(Declared::of(&surfaces))),
        compiled,
    );
    let milk = t.add("milk");
    t.call("toggle", vec![Value::resource(&milk)]);
    assert_eq!(t.rt.read(&milk, "done").unwrap(), Value::Bool(true));
}

/// A board whose items stand in two collections of the one type, and whose items hold two of one
/// type themselves.
const SWEEP: &str = include_str!("../../tilolang/tests/fixtures/sweep.tl");

/// The sweep board over the workspace's store, and the store, to take its last change back.
struct Sweep {
    rt: Runtime,
    meta: tilolang::PackageMeta,
    store: ResourceStore,
    board: ResourceRef,
}

impl Sweep {
    fn new() -> Self {
        let compiled = tilolang::compile_package(SWEEP, &tilolang::IdsLock::new(), &[])
            .unwrap_or_else(|errors| panic!("{errors:#?}"));
        let store = ResourceStore::in_memory();
        store.declare(&compiled.meta);
        let rt = Runtime::new(store.clone(), SimpleAuthority::new()).unwrap();
        rt.load_compiled(&compiled).unwrap();
        let board = rt.create("Board").unwrap();
        Self {
            rt,
            meta: compiled.meta,
            store,
            board,
        }
    }

    fn call(&self, action: &str, args: Vec<Value>) -> Value {
        let id = &self
            .meta
            .actions
            .iter()
            .find(|held| held.name == action)
            .unwrap()
            .id;
        match self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap() {
            InvokeOutcome::Done(value) => value,
            other => panic!("{action} answered {other:?}"),
        }
    }

    /// Items titled `titles` in the collection `field`, those titled in `done` finished.
    fn fill(&self, field: &str, titles: &[&str], done: &[&str]) {
        let add = if field == "items" { "add" } else { "keep" };
        for title in titles {
            let made = self.call(add, vec![Value::resource(&self.board), Value::text(*title)]);
            if done.contains(title) {
                self.call("finish", vec![made]);
            }
        }
    }

    /// The titles of the items of the board's `field`, in order.
    fn titles(&self, field: &str) -> Vec<String> {
        self.names(&self.board, field, "title")
    }

    /// The text field `name` of each item of `holder`'s collection `field`, in order.
    fn names(&self, holder: &ResourceRef, field: &str, name: &str) -> Vec<String> {
        let Value::List(items) = self.rt.read(holder, field).unwrap() else {
            panic!("{field} reads as a list");
        };
        items
            .iter()
            .map(|item| {
                let Value::Ref(item) = item else {
                    panic!("an item reads as its reference, not {item:?}");
                };
                match self.rt.read(&item.target, name).unwrap() {
                    Value::Text(text) => String::from(&*text),
                    other => panic!("{name} reads {other:?}"),
                }
            })
            .collect()
    }

    /// Take the last change back, as the window's undo does.
    fn undo(&self) {
        assert!(
            self.store
                .undo_last(Some(&me()))
                .expect("the store takes it back"),
            "there was a change to take back"
        );
    }
}

/// Items a list operation answered are the collection's own items: removing the finished ones a
/// filter, a `remove_where`, a sort or a `find` picked out removes exactly those, as one change
/// that one undo takes back, and the rest stay where they were.
#[test]
fn items_a_list_operation_picked_out_are_removed_from_their_collection() {
    for (action, answer) in [
        ("clear_filtered", Value::Unit),
        ("clear_where", Value::Int(2)),
        ("clear_sorted", Value::Unit),
    ] {
        let s = Sweep::new();
        s.fill("items", &["milk", "eggs", "bread", "tea"], &["eggs", "tea"]);
        assert_eq!(
            s.call(action, vec![Value::resource(&s.board)]),
            answer,
            "{action}"
        );
        assert_eq!(s.titles("items"), ["milk", "bread"], "{action}");
        s.undo();
        assert_eq!(
            s.titles("items"),
            ["milk", "eggs", "bread", "tea"],
            "{action} is one change"
        );
    }
    let s = Sweep::new();
    s.fill("items", &["milk", "eggs", "tea"], &["eggs", "tea"]);
    s.call("clear_first", vec![Value::resource(&s.board)]);
    assert_eq!(s.titles("items"), ["milk", "tea"]);
}

/// A holder with two collections of one type: the finished items set aside leave the collection
/// they stand in, and the items on the board, finished or not, stay.
#[test]
fn items_filtered_from_one_of_two_collections_of_a_type_leave_that_one() {
    let s = Sweep::new();
    s.fill("items", &["milk", "eggs"], &["eggs"]);
    s.fill("kept", &["old", "older", "keep"], &["old", "older"]);
    s.call("clear_kept", vec![Value::resource(&s.board)]);
    assert_eq!(s.titles("kept"), ["keep"]);
    assert_eq!(s.titles("items"), ["milk", "eggs"]);
    s.call("clear_filtered", vec![Value::resource(&s.board)]);
    assert_eq!(s.titles("items"), ["milk"]);
    assert_eq!(s.titles("kept"), ["keep"]);
}

/// An item that holds two collections of one type itself: its worn spares leave its spares, and
/// its parts, worn or not, stay.
#[test]
fn an_items_filtered_spares_leave_its_spares_and_its_parts_stay() {
    let s = Sweep::new();
    let bike = made(&s.call("add", vec![Value::resource(&s.board), Value::text("bike")]));
    for (action, name, worn) in [
        ("fit", "chain", true),
        ("fit", "bell", false),
        ("stock", "tube", true),
        ("stock", "pad", false),
        ("stock", "cable", true),
    ] {
        s.call(
            action,
            vec![Value::resource(&bike), Value::text(name), Value::Bool(worn)],
        );
    }
    s.call("clear_worn_spares", vec![Value::resource(&bike)]);
    assert_eq!(s.names(&bike, "spares", "name"), ["pad"]);
    assert_eq!(s.names(&bike, "parts", "name"), ["chain", "bell"]);
}

/// The resource an action answered it made.
fn made(value: &Value) -> ResourceRef {
    match value {
        Value::Ref(made) => (*made.target).clone(),
        other => panic!("an action answered {other:?}, not what it made"),
    }
}

/// A list whose action finishes an item and puts it first among the finished ones: what it asks
/// for is filtered by the field it just wrote.
const TICKS: &str = r#"app so.tilo.ticks "Ticks"

type Tick {
    title: Text
    done: Bool = false
}

type Ticks {
    items: Collection<Tick>
}

action add(list: Ticks, title: Text) -> Tick {
    return list.items.insert(Tick(title: title))
}

action finish(list: Ticks, item: Tick) {
    item.done = true
    list.items.move(item, to: 0, among: list.items.filter(x -> x.done))
}

view ticks(list: Ticks) {
    stack {
        for item in list.items {
            checkbox(item.title, bind: item.done)
        }
    }
}
"#;

/// A store held in this process that counts the resources its pages answered with.
struct Counted {
    port: tilo_resource_store::LocalPort,
    rows: std::cell::Cell<usize>,
}

impl tilo_resource_store::Port for Counted {
    fn command(
        &self,
        command: tilo_resource_store::wire::Command,
    ) -> Result<tilo_resource_store::wire::Reply, String> {
        let reply = tilo_resource_store::Port::command(&self.port, command)?;
        if let tilo_resource_store::wire::Reply::Page(page) = &reply {
            self.rows.set(self.rows.get() + page.resources.len());
        }
        Ok(reply)
    }

    fn declare(&self, schema: tilo_resource_store::Schema) {
        tilo_resource_store::Port::declare(&self.port, schema);
    }

    fn set_record_reader(&self, reader: std::sync::Arc<dyn tilo_resource_store::RecordReader>) {
        tilo_resource_store::Port::set_record_reader(&self.port, reader);
    }
}

/// An action that writes a field, asks for its collection's items filtered by it and moves the
/// item among them reads the items the store's index holds to the filter, the ones it wrote and
/// the page where the item goes, never every item the collection holds, and still finds the one
/// it just wrote.
#[test]
fn a_filter_after_an_actions_own_write_reads_what_the_index_narrows_it_to() {
    let compiled = tilolang::compile_package(TICKS, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|errors| panic!("{errors:#?}"));
    let port = std::rc::Rc::new(Counted {
        port: tilo_resource_store::LocalPort::in_memory(),
        rows: std::cell::Cell::new(0),
    });
    let store = ResourceStore::new(port.clone());
    store.declare(&compiled.meta);
    let rt = Runtime::new(store, SimpleAuthority::new()).unwrap();
    rt.load_compiled(&compiled).unwrap();
    let list = rt.create("Ticks").unwrap();
    let call = |action: &str, args: Vec<Value>| {
        let id = &compiled
            .meta
            .actions
            .iter()
            .find(|held| held.name == action)
            .unwrap()
            .id;
        match rt.invoke_action(&me(), (id.as_str(), args)).unwrap() {
            InvokeOutcome::Done(value) => value,
            other => panic!("{action} answered {other:?}"),
        }
    };
    let items: Vec<ResourceRef> = (0..2000)
        .map(|at| {
            made(&call(
                "add",
                vec![Value::resource(&list), Value::text(format!("item {at}"))],
            ))
        })
        .collect();
    call(
        "finish",
        vec![Value::resource(&list), Value::resource(&items[10])],
    );
    let before = port.rows.get();
    call(
        "finish",
        vec![Value::resource(&list), Value::resource(&items[20])],
    );
    let read = port.rows.get() - before;
    assert!(
        read <= 2000 / 4,
        "finishing an item among 2000 read {read} of them from the store: the finished ones, the \
         one it wrote and a page where it goes"
    );
    let id = &compiled
        .meta
        .views
        .iter()
        .find(|held| held.name == "ticks")
        .unwrap()
        .id;
    let view = rt
        .open_view(&me(), id, vec![Value::resource(&list)])
        .unwrap();
    assert_eq!(
        titles(view.tree())[9..12],
        ["item 9", "item 20", "item 10"],
        "the item finished last stands first among the finished ones"
    );
}

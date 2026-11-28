//! A package run over the device's one resource store: what an action writes is read back at
//! once, lists, moves and removals land where they should, and a view follows them.

use crate::fixtures::solo;
use tilolang_runtime::{InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance};
use tilolang_tilo::resources::ResourceStore;

const SOURCE: &str = include_str!("../../tilolang/tests/fixtures/todo.tl");

/// [`SOURCE`], with an action that reads the list, adds to it, and then walks what it read: the
/// action holds the collection it read while its own write moves the store on past it.
const SOURCE_WALKING: &str = concat!(
    include_str!("../../tilolang/tests/fixtures/todo.tl"),
    r#"
/// Adds an item and walks the list it read before adding it.
action add_then_walk(list: TodoList) -> Int {
    let items = list.items
    list.items.insert(Todo(title: "walked"))
    let seen = 0
    for item in items {
        seen = seen + 1
    }
    return seen
}

/// Walks the list.
action walk(list: TodoList) -> Int {
    let seen = 0
    for item in list.items {
        seen = seen + 1
    }
    return seen
}
"#,
);

fn compile() -> tilolang::Compiled {
    tilolang::compile_package(SOURCE, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|errors| panic!("{errors:#?}"))
}

fn compile_walking() -> tilolang::Compiled {
    tilolang::compile_package(SOURCE_WALKING, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|errors| panic!("{errors:#?}"))
}

/// The workspace's store as a session's workspace service keeps it: every app surface the device
/// knows declares its types there, and each write is drafted, checked against them, and only then
/// made, as the service checks a write before it keeps it.
struct Declared(std::cell::RefCell<tilo_resource_store::Store>);

impl Declared {
    fn of(surfaces: &[(&str, &collab_types::surface::SurfaceFile)]) -> Self {
        let mut store = tilo_resource_store::Store::new("workspace");
        for (app, file) in surfaces {
            for surface in &file.resources {
                store
                    .declare(tilo_resource_store::Schema::of_app(app, surface))
                    .unwrap();
            }
        }
        Self(std::cell::RefCell::new(store))
    }
}

impl tilo_resource_store::Port for Declared {
    fn command(
        &self,
        command: collab_types::store::wire::Command,
    ) -> Result<collab_types::store::wire::Reply, String> {
        use tilo_resource_store::wire;
        let mut store = self.0.borrow_mut();
        // The caller's last steps, which an undo reads: a read that needs the store itself, as
        // the workspace service answers it.
        if let collab_types::store::wire::Command::Steps {
            steps,
            invoker,
            kinds,
            redo,
        } = command
        {
            return Ok(wire::steps(&mut store, steps, &invoker, &kinds, redo));
        }
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
        Ok(collab_types::store::wire::Reply::Written)
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

    /// The todo package that also walks a collection it read and then wrote.
    fn walking() -> Self {
        Self::over(ResourceStore::in_memory(), compile_walking())
    }

    /// The package `compiled` run over `store`.
    fn over(store: ResourceStore, compiled: tilolang::Compiled) -> Self {
        store.declare(&compiled.meta);
        let rt = Runtime::new(store, solo()).unwrap();
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
fn a_run_that_writes_what_it_reads_walks_the_collection_as_it_stands() {
    let t = Todo::walking();
    for title in ["a", "b", "c", "d", "e", "f", "g", "h", "i"] {
        t.add(title);
    }
    match t.call("add_then_walk", vec![Value::resource(&t.list)]) {
        InvokeOutcome::Done(Value::Int(seen)) => {
            assert_eq!(seen, 10, "the nine it read and the one it added");
        }
        other => panic!("add_then_walk answered {other:?}"),
    }
    assert_eq!(
        titles(t.view().tree()).len(),
        10,
        "the item it added is in the list"
    );
}

/// What a workspace says when it cannot answer a page of a collection's items: a read that
/// failed, in its own words.
const WHY_UNREAD: &str = "the person's workspace could not be read here";

/// A workspace whose daemon stops answering one page of a collection's items, its index still
/// counting them.
struct FailingQuery {
    port: std::rc::Rc<dyn tilo_resource_store::Port>,
    /// Whether the next page read fails, and then answers again.
    fail: std::cell::Cell<bool>,
}

impl tilo_resource_store::Port for FailingQuery {
    fn command(
        &self,
        command: collab_types::store::wire::Command,
    ) -> Result<collab_types::store::wire::Reply, String> {
        if self.fail.get() && matches!(command, collab_types::store::wire::Command::Query(_)) {
            self.fail.set(false);
            return Err(WHY_UNREAD.to_owned());
        }
        self.port.command(command)
    }

    fn declare(&self, schema: tilo_resource_store::Schema) {
        self.port.declare(schema);
    }

    fn set_record_reader(&self, reader: std::sync::Arc<dyn tilo_resource_store::RecordReader>) {
        self.port.set_record_reader(reader);
    }
}

/// A run that walks a collection the store cannot answer a page of says what failed, in the
/// store's words: the places the listing counts are not reported as out of range.
#[test]
fn a_collection_the_store_cannot_answer_is_reported_rather_than_indexed() {
    let port = std::rc::Rc::new(FailingQuery {
        port: std::rc::Rc::new(tilo_resource_store::LocalPort::in_memory()),
        fail: std::cell::Cell::new(false),
    });
    let t = Todo::over(ResourceStore::new(port.clone()), compile_walking());
    t.add("milk");
    t.add("eggs");
    port.fail.set(true);
    let walk = t
        .meta
        .actions
        .iter()
        .find(|held| held.name == "walk")
        .expect("the package walks the list")
        .id
        .clone();
    let said =
        t.rt.invoke_action(&me(), (walk.as_str(), vec![Value::resource(&t.list)]))
            .expect_err("a page the store cannot answer fails the run")
            .to_string();
    assert!(said.contains(WHY_UNREAD), "the store's own words: {said}");
    assert!(
        !said.contains("out of range"),
        "never a place out of range: {said}"
    );
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

/// A store that keeps every command it is sent: what a write moved is named from its commands,
/// as the workspace names it.
struct Asked(
    std::rc::Rc<dyn tilo_resource_store::Port>,
    std::cell::RefCell<Vec<collab_types::store::wire::Command>>,
);

impl tilo_resource_store::Port for Asked {
    fn command(
        &self,
        command: collab_types::store::wire::Command,
    ) -> Result<collab_types::store::wire::Reply, String> {
        self.1.borrow_mut().push(command.clone());
        self.0.command(command)
    }

    fn declare(&self, schema: tilo_resource_store::Schema) {
        self.0.declare(schema);
    }

    fn set_record_reader(&self, reader: std::sync::Arc<dyn tilo_resource_store::RecordReader>) {
        self.0.set_record_reader(reader);
    }
}

/// A batch an action makes asks the store as often as one item does: where its new items stand
/// in the listings it changed is one read for all of them, never one for each. An app runs apart
/// from the store, so each read is a trip there: a list of 200 tasks that asked for each one's
/// place two times took the trip 401 times.
#[test]
fn a_batch_asks_the_store_as_often_however_many_items_it_makes() {
    let source = r#"app so.tilo.batch "Batch"

type Item {
    number: Int = 0
    title: Text
    done: Bool = false
}

type Items {
    numbers: Int merge counter
    items: Collection<Item>
}

action add_all(list: Items, titles: List<Text>) -> List<Item> {
    let made: List<Item> = []
    for title in titles {
        list.numbers += 1
        let one = list.items.insert(Item(number: list.numbers, title: title))
        made = [made, [one]].flat_map(x -> x)
    }
    made
}

view items(list: Items) {
    stack {
        for item in list.items {
            checkbox(item.title, bind: item.done)
        }
    }
}
"#;
    let compiled = tilolang::compile_package(source, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|errors| panic!("{errors:#?}"));
    let port = std::rc::Rc::new(Asked(
        std::rc::Rc::new(tilo_resource_store::LocalPort::in_memory()),
        std::cell::RefCell::default(),
    ));
    let store = ResourceStore::new(port.clone());
    store.declare(&compiled.meta);
    let rt = Runtime::new(store, solo()).unwrap();
    rt.load_compiled(&compiled).unwrap();
    let list = rt.create("Items").unwrap();
    let find = |name: &str, of: &[(String, String)]| {
        of.iter()
            .find(|(held, _)| held == name)
            .map(|(_, id)| id.clone())
            .unwrap()
    };
    let actions: Vec<(String, String)> = compiled
        .meta
        .actions
        .iter()
        .map(|action| (action.name.clone(), action.id.clone()))
        .collect();
    let views: Vec<(String, String)> = compiled
        .meta
        .views
        .iter()
        .map(|view| (view.name.clone(), view.id.clone()))
        .collect();
    let add_all = find("add_all", &actions);
    // A person looks at the list while it fills, as the import's person does.
    let mut view = rt
        .open_view(&me(), &find("items", &views), vec![Value::resource(&list)])
        .unwrap();
    let mut asked = |count: usize, round: usize| {
        let titles = (0..count)
            .map(|at| Value::text(format!("item {round}-{at}")))
            .collect();
        port.1.take();
        let made = rt
            .invoke_action(
                &me(),
                (
                    add_all.as_str(),
                    vec![Value::resource(&list), Value::List(titles)],
                ),
            )
            .unwrap();
        let InvokeOutcome::Done(Value::List(made)) = made else {
            panic!("add_all answered {made:?}");
        };
        assert_eq!(made.len(), count, "every item of the batch is made");
        // What the view reads to show them is its own: a page of rows at a time.
        let action = port.1.take().len();
        view.poll_patches().unwrap();
        action
    };
    let two = asked(2, 0);
    let many = asked(200, 1);
    let again = asked(200, 2);
    assert_eq!(
        (many, again),
        (two, two),
        "a batch of 200 asks the store as often as one of two, however full the list"
    );
    assert_eq!(titles(view.tree()).len(), 402, "the view shows every item");
}

/// A tilolang app's writes land in a workspace whose other apps declare their own types: what
/// each app's surface declares judges that app's kinds, and none refuses another app's.
#[test]
fn an_apps_writes_land_beside_the_types_other_apps_declare() {
    let compiled = compile();
    let own =
        tilolang_tilo::surface::surface_bytes(&[&compiled.meta], &tilolang_runtime::SourceWords)
            .unwrap();
    // An app whose surface declares the types it keeps, as a native app's does.
    let other = collab_types::surface::SurfaceFile::from_json(&tilo_surface_compile::canonical(
        &serde_json::json!({"resources": [{
            "kind": "tasks.tracker", "label": "Tracker", "version": 1, "fields": [],
            "types": [
                {"name": "Tracker", "fields": [{"name": "name", "type": "text"}]},
                {"name": "Thread", "fields": [{"name": "subject", "type": "text"}]},
            ],
        }]}),
    ))
    .unwrap();
    let own = collab_types::surface::SurfaceFile::compiled(&own).unwrap();
    let app = compiled.meta.app.clone();
    let surfaces = [(app.as_str(), &own), ("so.tilo.test-other", &other)];
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
        let rt = Runtime::new(store.clone(), solo()).unwrap();
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

/// A store held in this process that counts the resources its pages answered with, the
/// resources it was asked for one by one, and the ends of histories it was asked for.
struct Counted {
    port: tilo_resource_store::LocalPort,
    rows: std::cell::Cell<usize>,
    gets: std::cell::Cell<usize>,
    ends: std::cell::Cell<usize>,
}

impl tilo_resource_store::Port for Counted {
    fn command(
        &self,
        command: collab_types::store::wire::Command,
    ) -> Result<collab_types::store::wire::Reply, String> {
        match command {
            collab_types::store::wire::Command::Get { .. } => {
                self.gets.set(self.gets.get().saturating_add(1));
            }
            collab_types::store::wire::Command::VersionEnd { .. } => {
                self.ends.set(self.ends.get().saturating_add(1));
            }
            _ => {}
        }
        let reply = tilo_resource_store::Port::command(&self.port, command)?;
        if let collab_types::store::wire::Reply::Page(page) = &reply {
            self.rows
                .set(self.rows.get().saturating_add(page.resources.len()));
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
        gets: std::cell::Cell::new(0),
        ends: std::cell::Cell::new(0),
    });
    let store = ResourceStore::new(port.clone());
    store.declare(&compiled.meta);
    let rt = Runtime::new(store, solo()).unwrap();
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

/// A resource the store holds none of, read again and again (each row of a list naming another
/// app's root nobody made yet), is asked of the store once: its answer holds until its record
/// moves, and then the store is asked again.
#[test]
fn a_resource_the_store_holds_none_of_is_asked_for_once_until_its_record_moves() {
    let compiled = tilolang::compile_package(TICKS, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|errors| panic!("{errors:#?}"));
    let port = std::rc::Rc::new(Counted {
        port: tilo_resource_store::LocalPort::in_memory(),
        rows: std::cell::Cell::new(0),
        gets: std::cell::Cell::new(0),
        ends: std::cell::Cell::new(0),
    });
    let store = ResourceStore::new(port.clone());
    store.declare(&compiled.meta);
    let ty = &compiled
        .meta
        .types
        .iter()
        .find(|held| held.name == "Ticks")
        .unwrap()
        .id;
    let nobody = ResourceRef::root("root-nobody-made-this", ty.clone());
    for _ in 0..1000 {
        assert!(!store.contains(&nobody));
    }
    assert_eq!(
        port.gets.get(),
        1,
        "a thousand reads of a resource the store holds none of asked it {} times",
        port.gets.get()
    );
    store.moved_records(&[collab_types::store::MovedRecord::whole(nobody.doc.clone())]);
    assert!(!store.contains(&nobody));
    assert_eq!(
        port.gets.get(),
        2,
        "its record moved: the store is asked again, once"
    );
}

/// When a list was made, read for each of its rows (who may read a row its author made), is
/// asked of the store once: the answer holds until the list is written, and then it is asked
/// again.
#[test]
fn a_lists_first_version_is_asked_for_once_until_the_list_is_written() {
    use tilolang_runtime::store::Store as _;
    let compiled = tilolang::compile_package(TICKS, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|errors| panic!("{errors:#?}"));
    let port = std::rc::Rc::new(Counted {
        port: tilo_resource_store::LocalPort::in_memory(),
        rows: std::cell::Cell::new(0),
        gets: std::cell::Cell::new(0),
        ends: std::cell::Cell::new(0),
    });
    let store = ResourceStore::new(port.clone());
    store.declare(&compiled.meta);
    let rt = Runtime::new(store.clone(), solo()).unwrap();
    rt.load_compiled(&compiled).unwrap();
    let list = rt.create("Ticks").unwrap();
    let made = store.version_end(&list, false).unwrap();
    assert!(
        made.is_some(),
        "the list was made, so it has a first version"
    );
    for _ in 0..1000 {
        assert_eq!(store.version_end(&list, false).unwrap(), made);
    }
    assert_eq!(
        port.ends.get(),
        1,
        "a thousand reads of when the list was made asked the store {} times",
        port.ends.get()
    );
    let before = store.version_end(&list, true).unwrap();
    assert_eq!(store.version_end(&list, true).unwrap(), before);
    assert_eq!(port.ends.get(), 2, "its last version is asked for once too");
    let add = &compiled
        .meta
        .actions
        .iter()
        .find(|held| held.name == "add")
        .unwrap()
        .id;
    rt.invoke_action(
        &me(),
        (
            add.as_str(),
            vec![Value::resource(&list), Value::text("written")],
        ),
    )
    .unwrap();
    assert!(store.version_end(&list, true).unwrap().is_some());
    assert!(store.version_end(&list, true).unwrap().is_some());
    assert_eq!(
        port.ends.get(),
        3,
        "an item was written into the list: its last version is asked again, once"
    );
}

/// The ticks app with as many kinds as Messages has, 84, and an action that removes a tick.
fn ticks_with_many_kinds() -> tilolang::Compiled {
    let mut source = TICKS.to_owned();
    source.push_str("\naction drop(list: Ticks, item: Tick) {\n    list.items.remove(item)\n}\n");
    for at in 0..82 {
        use std::fmt::Write as _;
        write!(source, "\ntype Other{at} {{\n    note: Text\n}}\n").unwrap();
    }
    let compiled = tilolang::compile_package(&source, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|errors| panic!("{errors:#?}"));
    assert_eq!(
        compiled.meta.types.len(),
        84,
        "the app has as many kinds as Messages"
    );
    compiled
}

/// What a resource is, read from its id alone, is one question to the store however many kinds
/// the app has: a read of a record the store holds none of (a window's state nobody wrote yet)
/// costs one round trip, not one for each of the app's 84 kinds, and still fails, naming the
/// record. One held now, and one removed, are found by the same one question.
#[test]
fn a_record_the_store_holds_none_of_is_one_question_however_many_kinds_the_app_has() {
    use tilolang_runtime::store::Store as _;
    let compiled = ticks_with_many_kinds();
    let port = std::rc::Rc::new(Asked(
        std::rc::Rc::new(tilo_resource_store::LocalPort::in_memory()),
        std::cell::RefCell::default(),
    ));
    let store = ResourceStore::new(port.clone());
    store.declare(&compiled.meta);
    let rt = Runtime::new(store, solo()).unwrap();
    rt.load_compiled(&compiled).unwrap();
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
    let list = rt.create("Ticks").unwrap();
    let kept = made(&call(
        "add",
        vec![Value::resource(&list), Value::text("kept")],
    ));
    let dropped = made(&call(
        "add",
        vec![Value::resource(&list), Value::text("dropped")],
    ));
    call(
        "drop",
        vec![Value::resource(&list), Value::resource(&dropped)],
    );
    let type_named = |name: &str| {
        compiled
            .meta
            .types
            .iter()
            .find(|held| held.name == name)
            .unwrap()
            .id
            .clone()
    };
    // Read as a reader that wrote none of them reads them: a process of its own over the store.
    let reader = ResourceStore::new(port.clone());
    reader.declare(&compiled.meta);
    let asked = |read: &dyn Fn()| {
        port.1.take();
        read();
        port.1.take().len()
    };

    let missing = "root-nobody-wrote-this";
    let questions = asked(&|| assert_eq!(reader.type_of_id(missing), None));
    assert_eq!(
        questions, 1,
        "what a record the store holds none of is asked of the store once, not of each kind"
    );
    let untyped = ResourceRef::root(missing, tilolang_runtime::ids::TypeId::default());
    let title = tilolang_runtime::store::FieldSpec {
        id: "title".into(),
        kind: tilolang_runtime::meta::FieldKind::Text,
    };
    let failure = std::cell::RefCell::new(None);
    let questions = asked(&|| *failure.borrow_mut() = Some(reader.read(&untyped, &title)));
    assert!(
        questions <= 2,
        "reading a field of a record the store holds none of asked the store {questions} times"
    );
    let failure = failure
        .into_inner()
        .unwrap()
        .expect_err("a record nobody wrote has no title");
    assert!(
        failure.to_string().contains(missing),
        "the failure names the record it could not read: {failure}"
    );

    let questions = asked(&|| {
        assert_eq!(
            reader.type_of_id(&kept.id_string()),
            Some(type_named("Tick"))
        );
    });
    assert_eq!(questions, 1, "a held record's type is one question too");
    let questions = asked(&|| {
        assert_eq!(
            reader.type_of_id(&dropped.id_string()),
            Some(type_named("Tick")),
            "a removed record is still of the kind its history holds"
        );
    });
    assert_eq!(questions, 1, "a removed record's type is one question too");
}

/// A list whose rows each read who made them, as a conversation's do.
const LINES: &str = r#"app so.tilo.lines "Lines"
type Line {
    said: Text
}
type Lines {
    items: Collection<Line>
}
action say(list: Lines, said: Text) -> Line {
    return list.items.insert(Line(said: said))
}
view lines(list: Lines) {
    stack {
        for line in list.items {
            if let c = created(line) {
                text("{line.said} by {c.who}")
            }
        }
    }
}
"#;

/// Another window's write among a thousand rows that each read their history is read again
/// alone: the view asks for the resource written, its holder and its history, never another
/// row's. Another device's write, which moves its record as a whole, reaches the view too.
#[test]
fn a_write_elsewhere_reads_again_only_what_it_moved() {
    use collab_types::store::wire::Command;
    let compiled = tilolang::compile_package(LINES, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|errors| panic!("{errors:#?}"));
    let shared: std::rc::Rc<dyn tilo_resource_store::Port> =
        std::rc::Rc::new(tilo_resource_store::LocalPort::in_memory());
    let [ours, theirs] = [(); 2].map(|()| std::rc::Rc::new(Asked(shared.clone(), <_>::default())));
    let [(store, rt), (_, other)] = [&ours, &theirs].map(|port| {
        let store = ResourceStore::new(port.clone());
        store.declare(&compiled.meta);
        let rt = Runtime::new(store.clone(), solo()).unwrap();
        rt.load_compiled(&compiled).unwrap();
        (store, rt)
    });
    let list = other.create("Lines").unwrap();
    let write = |said: String| {
        let line = vec![Value::resource(&list), Value::text(said)];
        other
            .invoke_action(&me(), (compiled.meta.actions[0].id.as_str(), line))
            .unwrap();
        let moved: Option<Vec<_>> = theirs.1.take().iter().map(Command::moves).collect();
        moved.map(|items| items.concat())
    };
    (0..1000).for_each(|at| _ = write(format!("line {at}")));
    let view = &compiled.meta.views[0].id;
    let mut view = rt
        .open_view(&me(), view, vec![Value::resource(&list)])
        .unwrap();
    for (said, near) in [("nearby", true), ("from afar", false)] {
        let items = write(said.to_owned()).filter(|_| near);
        ours.1.take();
        let record = list.doc.clone();
        store.moved_records(&[collab_types::store::MovedRecord { record, items }]);
        view.poll_patches().unwrap();
        let asked = ours.1.take();
        let rows = asked
            .iter()
            .filter(|read| matches!(read, Command::Get { .. } | Command::VersionEnd { .. }));
        assert!(!near || rows.count() <= 4, "{asked:#?}");
        let bottom = view.tree().of_kind("ui.Text").last().unwrap().props["value"].clone();
        assert!(matches!(bottom, Value::Text(shown) if shown.starts_with(said)));
    }
}

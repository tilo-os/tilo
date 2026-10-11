#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A long list in a scroll view mounts only the rows near what it shows,
//! while agents and assistive technology still reach every item.

use tilo_app::accesskit::Role;
use tilo_app::i18n::EmbeddedCatalog;
use tilo_app::prelude::*;
use tilo_app::winit::event::WindowEvent;

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[(
        "en-US",
        "window-draft = Next task\nwindow-done = Completed\n",
    )],
    "window-draft",
);

#[derive(Model, Default, Debug, Clone, PartialEq)]
/// One task.
pub struct Task {
    pub title: String,
    pub done: bool,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
/// The draft and the tasks.
pub struct Checklist {
    pub draft: String,
    pub tasks: Collection<Task>,
}

#[resource(kind = "test.window-checklist", label = "Checklist")]
impl Checklist {
    /// Set the next task's title.
    #[action]
    pub fn draft(&mut self, title: String) {
        self.draft = title;
    }

    /// Add `count` tasks at the end.
    #[action]
    pub fn seed(&mut self, cx: &mut ActionCx, count: u32) {
        for _ in 0..count {
            let index = self.tasks.len();
            self.tasks.push(
                cx.id(),
                Task {
                    title: format!("Task {index}"),
                    done: false,
                },
            );
        }
    }

    /// Mark a task completed or open.
    ///
    /// # Errors
    /// Refuses a task that no longer exists.
    #[action]
    pub fn set_done(&mut self, task: &Id, done: bool) -> Result<(), &'static str> {
        self.tasks.get_mut(task).ok_or("gone")?.done = done;
        Ok(())
    }
}

struct Window;

thread_local! {
    /// How many times this thread's rows read whether their task is done.
    static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many rows this thread's lists built.
    static BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn builds() -> usize {
    BUILDS.with(std::cell::Cell::get)
}

fn count_build() {
    BUILDS.with(|builds| builds.set(builds.get().saturating_add(1)));
}

impl ResourceApp for Window {
    type Resource = Checklist;
    const ID: &'static str = "so.tilo.test-window-checklist";
    const TITLE: &'static str = "Checklist";

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<Checklist>) -> Element {
        let draft = cx.control_with(
            |shown| TextInput::from_state(shown).with_id("draft"),
            |list| list.draft.clone(),
            Checklist::DRAFT,
        );
        let rows = cx.each(
            |list| &list.tasks,
            |row| {
                count_build();
                task_row(row)
            },
        );
        let list = Scrollable::new_vertical().with_child(Box::new(rows));
        Box::new(tilo_app::column![draft, list])
    }
}

fn task_row(row: &ViewCx<Checklist, Task>) -> Element {
    let Some(id) = row.item().cloned() else {
        return Box::new(Column::new());
    };
    let control = format!("set-done-{id}");
    Box::new(tilo_app::row![
        row.text(|task| task.title.clone()),
        row.control_with(
            |shown| Checkbox::from_state(shown)
                .with_id(&control)
                .with_label("window-done"),
            |task| {
                READS.with(|reads| reads.set(reads.get().saturating_add(1)));
                task.done
            },
            Checklist::SET_DONE.map(move |done| (id.clone(), done))
        ),
    ])
}

/// The same list, whose rows each bind an edit into the published tree:
/// the last task's row offers `finish-last`.
struct BindingWindow;

impl ResourceApp for BindingWindow {
    type Resource = Checklist;
    const ID: &'static str = "so.tilo.test-window-binding-checklist";
    const TITLE: &'static str = "Checklist";

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<Checklist>) -> Element {
        let rows = cx.each(
            |list| &list.tasks,
            |row| {
                count_build();
                let item = row.item().cloned();
                row.bind_declarations(move |list| {
                    if list.tasks.ids().last() != item.as_ref() {
                        return Vec::new();
                    }
                    Checklist::declarations()
                        .iter()
                        .find(|decl| decl.id == "draft")
                        .cloned()
                        .map(|mut decl| {
                            decl.id = "finish-last";
                            decl
                        })
                        .into_iter()
                        .collect()
                });
                task_row(row)
            },
        );
        Box::new(Scrollable::new_vertical().with_child(Box::new(rows)))
    }
}

const TASKS: u32 = 1_500;

fn seeded() -> TestApp<Window> {
    let mut app = TestApp::<Window>::for_person("ada").unwrap();
    app.call("seed", serde_json::json!({ "count": TASKS }))
        .unwrap();
    app.harness().relayout();
    app
}

/// The list's items as assistive technology sees them: `(position, set size)`.
fn list_items<A: ResourceApp>(app: &mut TestApp<A>) -> Vec<(usize, usize)> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == Role::ListItem)
        .map(|(_, node)| {
            (
                node.position_in_set().unwrap_or_default(),
                node.size_of_set().unwrap_or_default(),
            )
        })
        .collect()
}

fn task_id(app: &TestApp<Window>, index: usize) -> Id {
    app.state().tasks.ids().nth(index).cloned().unwrap()
}

fn control(id: &Id) -> String {
    format!("set-done-{id}")
}

#[test]
fn the_published_tree_lists_every_row_while_only_the_window_is_mounted() {
    let mut app = seeded();
    let total = usize::try_from(TASKS).unwrap();

    // Agents and the palette: the published tree carries a page of the
    // tasks with their actions, mounted or not; every other task is
    // resolved by its id when addressed, as an agent addresses it.
    let resources = app.harness().resources();
    let root = resources
        .iter()
        .find(|resource| resource.kind == Checklist::KIND)
        .unwrap();
    let page = total.min(tilo_resource::items::ITEM_PAGE);
    assert_eq!(
        root.children.len(),
        page,
        "a page of the items is published"
    );
    let paged = root
        .children
        .iter()
        .find(|child| child.label == format!("Task {}", page.saturating_sub(1)))
        .expect("the page's last item is published while its row is not mounted");
    assert!(
        paged.actions.iter().any(|spec| spec.id == "set-done"),
        "its actions are bound to it"
    );
    let last = task_id(&app, total.saturating_sub(1));
    app.call_on(
        &tilo_resource::items::item_resource_id::<Checklist>("tasks", &last),
        "set-done",
        serde_json::json!({ "done": true }),
    )
    .expect("an item past the page is reached by its id");
    assert!(app.state().tasks.get(&last).unwrap().done);

    // Assistive technology: the mounted rows, each placed in the whole list.
    let items = list_items(&mut app);
    assert!(!items.is_empty(), "the shown rows are in the tree");
    assert!(
        items.len() < 120,
        "{} of {total} rows mounted; only the window should be",
        items.len()
    );
    assert!(
        items.iter().all(|&(_, size)| size == total),
        "each row is announced as one of {total}: {items:?}"
    );
    assert!(items.contains(&(1, total)), "the first row is shown");

    // An item whose row is not mounted is still invoked by agents.
    let far = task_id(&app, total.saturating_sub(2));
    assert!(
        app.harness().click_named(&control(&far)).is_err(),
        "its row is not mounted"
    );
    let item = tilo_resource::items::item_resource_id::<Checklist>("tasks", &far);
    app.call_on(&item, "set-done", serde_json::json!({ "done": true }))
        .unwrap();
    assert!(app.state().tasks.get(&far).unwrap().done);
}

fn mounted(app: &mut TestApp<Window>, id: &Id) -> bool {
    app.harness().targets().contains_key(&control(id))
}

#[test]
fn scrolling_to_the_end_mounts_the_last_rows_with_their_actions() {
    let mut app = seeded();
    let total = usize::try_from(TASKS).unwrap();
    let first = task_id(&app, 0);
    let focused = task_id(&app, 10);
    let middle = task_id(&app, total.div_euclid(2));
    let near_end = task_id(&app, total.saturating_sub(3));
    assert!(mounted(&mut app, &first));
    assert!(!mounted(&mut app, &middle));
    assert!(!mounted(&mut app, &near_end));
    app.harness()
        .perform_named(&control(&focused), &tilo_app::a11y::Action::Focus)
        .unwrap();

    app.harness().send(&WindowEvent::CursorMoved {
        device_id: tilo_app::winit::event::DeviceId::dummy(),
        position: tilo_app::winit::dpi::PhysicalPosition::new(200.0, 300.0),
    });
    for _ in 0..200 {
        app.harness().scroll(0.0, -400.0);
    }
    app.harness().relayout();

    let items = list_items(&mut app);
    assert!(
        items.contains(&(total, total)),
        "the last row is mounted at the end: {items:?}"
    );
    assert!(
        items.len() < 120,
        "{} rows mounted after scrolling",
        items.len()
    );
    assert!(!mounted(&mut app, &first), "the first row left the window");
    assert!(!mounted(&mut app, &middle));
    assert!(
        mounted(&mut app, &focused),
        "the focused row stays mounted wherever the view scrolls"
    );
    app.harness().click_named(&control(&near_end)).unwrap();
    app.harness().relayout();
    assert!(
        app.state().tasks.get(&near_end).unwrap().done,
        "the row's checkbox acts on its item"
    );
}

#[test]
fn adding_an_item_away_from_the_view_keeps_the_window_tree() {
    let mut app = seeded();
    let before = list_items(&mut app).len();
    let nodes = app.harness().layout_node_ids();
    let frames = app.harness().relayout_frames();
    let rebuilt = app.harness().rebuilt_frames();

    app.call("seed", serde_json::json!({ "count": 1 })).unwrap();
    app.harness().relayout();

    assert!(app.harness().relayout_frames() > frames, "a frame ran");
    assert_eq!(
        app.harness().rebuilt_frames(),
        rebuilt,
        "an item added below the view recomposes no layout tree"
    );
    assert_eq!(
        app.harness().layout_node_ids(),
        nodes,
        "the window's layout tree is the one it was"
    );
    let items = list_items(&mut app);
    let total = usize::try_from(TASKS).unwrap().saturating_add(1);
    assert_eq!(items.len(), before, "no row mounted or dropped");
    assert!(items.iter().all(|&(_, size)| size == total), "{items:?}");
}

#[test]
fn a_change_reads_the_rows_shown_not_every_row() {
    let mut app = seeded();
    let total = usize::try_from(TASKS).unwrap();
    let shown = list_items(&mut app).len();
    let before = READS.with(std::cell::Cell::get);

    app.call("draft", serde_json::json!({ "title": "Next" }))
        .unwrap();
    app.harness().relayout();

    let reads = READS.with(std::cell::Cell::get).saturating_sub(before);
    assert!(
        reads > 0,
        "the shown rows follow the change (it ran {reads} reads)"
    );
    assert!(
        reads <= shown.saturating_mul(2),
        "{reads} rows read for a change to the draft, with {shown} of {total} mounted"
    );
}

#[test]
fn a_row_out_of_view_shows_its_item_as_it_is_when_it_mounts() {
    let mut app = seeded();
    let total = usize::try_from(TASKS).unwrap();
    let far = task_id(&app, total.saturating_sub(3));
    assert!(!mounted(&mut app, &far), "the row starts out unmounted");
    // Another participant (here an agent) completes the task while its row
    // is out of view.
    // Past the published page, the item is addressed by its id.
    let item = tilo_resource::items::item_resource_id::<Checklist>("tasks", &far);
    app.call_on(
        item.as_str(),
        "set-done",
        serde_json::json!({ "done": true }),
    )
    .unwrap();
    app.harness().relayout();

    app.harness().send(&WindowEvent::CursorMoved {
        device_id: tilo_app::winit::event::DeviceId::dummy(),
        position: tilo_app::winit::dpi::PhysicalPosition::new(200.0, 300.0),
    });
    for _ in 0..200 {
        app.harness().scroll(0.0, -400.0);
    }
    app.harness().relayout();
    assert!(mounted(&mut app, &far), "the row mounted at the end");

    // The checkbox shows the task completed: clicking it reopens the task.
    // A checkbox still showing it open would complete it again.
    app.harness().click_named(&control(&far)).unwrap();
    app.harness().relayout();
    assert!(
        !app.state().tasks.get(&far).unwrap().done,
        "the mounted row showed the task as completed"
    );
}

#[test]
fn tab_walks_rows_past_the_mounted_window() {
    let mut app = seeded();
    let steps = 90;
    let target = task_id(&app, steps);
    assert!(!mounted(&mut app, &target), "the row starts out unmounted");
    let first = control(&task_id(&app, 0));
    app.harness()
        .perform_named(&first, &tilo_app::a11y::Action::Focus)
        .unwrap();
    // Tab moves focus without scrolling the view: the rows beside the
    // focused one stay mounted, so each press finds the next row.
    for _ in 0..steps {
        app.harness().key("Tab");
    }
    app.harness().key("Space");
    app.harness().relayout();
    let done: Vec<usize> = app
        .state()
        .tasks
        .values()
        .enumerate()
        .filter(|(_, task)| task.done)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(done, [steps], "Tab reached row {steps} one row at a time");
}

/// `A`'s window opened again over a record that already holds every task,
/// as a person opening the app finds it, with the rows built since.
fn reopened<A: ResourceApp<Resource = Checklist>>() -> (TestApp<A>, usize) {
    let mut app = TestApp::<A>::for_person("ada").unwrap();
    app.call("seed", serde_json::json!({ "count": TASKS }))
        .unwrap();
    app.flush().unwrap();
    let before = builds();
    let mut app = app.reopen().unwrap();
    app.harness().relayout();
    let built = builds().saturating_sub(before);
    (app, built)
}

#[test]
fn opening_a_long_list_builds_only_the_rows_it_mounts() {
    let (mut app, built) = reopened::<Window>();
    let total = usize::try_from(TASKS).unwrap();
    let mounted = list_items(&mut app).len();
    assert!(mounted > 0, "the shown rows are mounted");
    assert!(
        built >= mounted && built < 120,
        "{built} of {total} rows built to open the list, {mounted} mounted"
    );

    // A change to the items away from the view builds no row.
    let before = builds();
    app.call("seed", serde_json::json!({ "count": 10 }))
        .unwrap();
    app.call("draft", serde_json::json!({ "title": "Next" }))
        .unwrap();
    app.harness().relayout();
    assert_eq!(builds(), before, "rows added below the view are not built");

    // Every item is still published with its actions, built or not: a page
    // of them in the tree, every other one reached by its id.
    let resources = app.harness().resources();
    let root = resources
        .iter()
        .find(|resource| resource.kind == Checklist::KIND)
        .unwrap();
    assert_eq!(
        root.children.len(),
        total
            .saturating_add(11)
            .min(tilo_resource::items::ITEM_PAGE)
    );
    let last = app.state().tasks.ids().last().cloned().unwrap();
    app.call_on(
        &tilo_resource::items::item_resource_id::<Checklist>("tasks", &last),
        "set-done",
        serde_json::json!({ "done": true }),
    )
    .expect("an item whose row was never built is reached by its id");
    assert!(app.state().tasks.get(&last).unwrap().done);
}

#[test]
fn rows_that_bind_into_the_published_tree_are_all_built() {
    let (mut app, built) = reopened::<BindingWindow>();
    let total = usize::try_from(TASKS).unwrap();
    assert_eq!(
        built, total,
        "each row's binding is published, so each is built"
    );
    let mounted = list_items(&mut app).len();
    assert!(mounted < 120, "{mounted} of {total} rows mounted");
    let resources = app.harness().resources();
    let root = resources
        .iter()
        .find(|resource| resource.kind == Checklist::KIND)
        .unwrap();
    assert!(
        root.actions.iter().any(|spec| spec.id == "finish-last"),
        "the last row's bound edit is published while the row is not mounted"
    );
}

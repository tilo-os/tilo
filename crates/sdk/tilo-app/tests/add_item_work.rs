#![allow(clippy::unwrap_used, reason = "test assertions")]

//! An agent adding one item to a long list, every row of which is mounted,
//! costs the window work for the item, not for the list: the frame that
//! shows it asks every widget a question or two, but enters the component's
//! callback scope a bounded number of times however long the list is.

use tilo_app::i18n::EmbeddedCatalog;
use tilo_app::prelude::*;
use tilo_app::tilo_resource::profiling::observe;

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", "work-draft = Next task\nwork-done = Completed\n")],
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

#[resource(kind = "test.add-item-work", label = "Checklist")]
impl Checklist {
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
}

/// The checklist with its rows outside a scroll view: every row mounted,
/// about seven widgets an item.
struct Mounted;

impl ResourceApp for Mounted {
    type Resource = Checklist;
    const ID: &'static str = "so.tilo.test-add-item-work";
    const TITLE: &'static str = "Checklist";

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<Checklist>) -> Element {
        let draft = cx.input(Checklist::paths().draft).with_id("draft");
        let rows = cx.each(
            |list| &list.tasks,
            |row| {
                if row.item().is_none() {
                    return Box::new(Column::new()) as Element;
                }
                let done = row.paths().done;
                Box::new(tilo_app::row![
                    row.text(|task| task.title.clone()),
                    row.checkbox("work-done", done),
                ]) as Element
            },
        );
        Box::new(Column::new().push(Box::new(draft)).push(Box::new(rows)))
    }
}

/// How many times one agent call adding a task to a list of `tasks` enters
/// a component's callback scope, end to end.
fn scope_entries_adding_one(tasks: u32) -> usize {
    let mut app = TestApp::<Mounted>::new().unwrap();
    app.call("seed", serde_json::json!({ "count": tasks }))
        .unwrap();
    // Settles the record and the layout, as the benchmark's warm-up does.
    app.call("seed", serde_json::json!({ "count": 1 })).unwrap();
    let (added, phases) = observe(|| app.call("seed", serde_json::json!({ "count": 1 })));
    added.unwrap();
    assert_eq!(
        app.state().tasks.len(),
        usize::try_from(tasks).unwrap().saturating_add(2)
    );
    phases
        .iter()
        .filter(|phase| phase.phase == "ui.callback_scope")
        .map(|phase| phase.calls)
        .sum()
}

#[test]
fn adding_an_item_enters_component_scopes_independently_of_the_list_length() {
    let short = scope_entries_adding_one(100);
    let long = scope_entries_adding_one(400);
    // The call reaches the component through its scope: none counted means
    // the count is no longer observed, not that the work is bounded.
    assert!(short > 0, "no component scope entry was observed");
    // 300 more rows are some 2,000 more widgets: a walk entering each
    // widget's scope adds thousands of entries per walk, and the frame walks
    // the widgets several times. Fewer than one more per added row is work
    // that does not follow the list.
    assert!(
        long.saturating_sub(short) < 300,
        "adding a task entered component scopes {short} times beside 100 tasks \
         and {long} times beside 400",
    );
}

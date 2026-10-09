//! What one keystroke publishes in a framework app, as its resource grows.
//!
//! Typing into a field unrelated to the list must not cost the list once per
//! row: bound controls refresh only the resource's own actions, and every
//! publication the host asks for binds row edits in one walk of the view.
#![cfg(feature = "test-support")]

use tilo_app::framework::profiling::observe;
use tilo_app::i18n::EmbeddedCatalog;
use tilo_app::prelude::*;

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[(
        "en-US",
        "keystroke-draft = Next task\nkeystroke-done = Completed\n",
    )],
    "keystroke-draft",
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

#[resource(kind = "test.keystroke-checklist", label = "Checklist")]
impl Checklist {
    /// Set the next task's title.
    #[action]
    pub fn draft(&mut self, title: String) {
        self.draft = title;
    }

    /// Add `count` tasks at once.
    #[action]
    pub fn seed(&mut self, cx: &mut ActionCx, count: u32) {
        for index in 0..count {
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

struct Keystrokes;

impl ResourceApp for Keystrokes {
    type Resource = Checklist;
    const ID: &'static str = "so.tilo.test-keystroke-checklist";
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
                let Some(id) = row.item().cloned() else {
                    return Box::new(Column::new()) as Element;
                };
                let control = format!("set-done-{id}");
                Box::new(tilo_app::row![
                    row.text(|task| task.title.clone()),
                    row.control_with(
                        |shown| Checkbox::from_state(shown)
                            .with_id(&control)
                            .with_label("keystroke-done"),
                        |task| task.done,
                        Checklist::SET_DONE.map(move |done| (id.clone(), done))
                    ),
                ]) as Element
            },
        );
        let list = Scrollable::new_vertical().with_child(Box::new(rows));
        Box::new(tilo_app::column![draft, list])
    }
}

/// How often each framework phase ran.
type Phases = std::collections::BTreeMap<&'static str, usize>;

fn failure(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

/// How often one keystroke into the draft ran each framework phase, with
/// `tasks` tasks in the list.
fn one_keystroke(tasks: u32) -> Result<Phases, String> {
    let mut app = TestApp::<Keystrokes>::for_person("ada").map_err(failure)?;
    app.call("seed", serde_json::json!({ "count": tasks }))
        .map_err(failure)?;
    app.harness()
        .perform_named("draft", &tilo_app::a11y::Action::Focus)
        .map_err(failure)?;
    app.harness().key("a");
    let ((), phases) = observe(|| app.harness().key("b"));
    if app.state().draft != "ab" {
        return Err(format!(
            "the keystrokes missed the draft: {:?}",
            app.state().draft
        ));
    }
    let mut counts = Phases::new();
    for phase in phases {
        let count = counts.entry(phase.phase).or_insert(0);
        *count = count.saturating_add(1);
    }
    Ok(counts)
}

#[test]
fn a_keystroke_publishes_the_same_few_times_whatever_the_list() {
    let small = one_keystroke(3).expect("a keystroke with 3 tasks");
    let large = one_keystroke(60).expect("a keystroke with 60 tasks");
    let publications = |counts: &Phases| counts.get("framework.publication").copied().unwrap_or(0);
    assert_eq!(
        publications(&small),
        publications(&large),
        "how often a keystroke publishes does not grow with the list"
    );
    // Each remaining publication is one the host asked for: resolving the
    // draft's invocation and the bound discovery after press and release.
    // Bound controls refresh from the resource's own actions instead (seven
    // whole publications per keystroke before).
    assert!(
        publications(&large) <= 3,
        "{} publications in one keystroke: {large:?}",
        publications(&large)
    );
    assert!(
        large.get("framework.action_bindings").copied().unwrap_or(0) >= 2,
        "bound controls refresh before and after the view syncs: {large:?}"
    );
}

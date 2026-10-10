//! What one keystroke publishes in a framework app, as its resource grows.
//!
//! Typing into a field unrelated to the list must not cost the list once per
//! row: bound controls refresh only the resource's own actions, and every
//! publication the host asks for binds row edits in one walk of the view.
//! A hosted app publishes its tree at most twice per keystroke: once when
//! the door resolves the field's edit, once when the next frame shows it.
#![cfg(feature = "test-support")]

use std::cell::Cell;

use tilo_app::framework::profiling::observe;
use tilo_app::framework::testing::drive_live_window;
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

thread_local! {
    /// How many tasks the next live window's record starts with.
    static GENESIS_TASKS: Cell<u32> = const { Cell::new(0) };
}

impl ResourceApp for Keystrokes {
    type Resource = Checklist;
    const ID: &'static str = "so.tilo.test-keystroke-checklist";
    const TITLE: &'static str = "Checklist";

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    /// A live window's record starts at the size under test. (A real app's
    /// genesis depends on its launch input alone.)
    fn genesis(_: &LaunchInput<'_>) -> Vec<Invoke<Checklist>> {
        match GENESIS_TASKS.get() {
            0 => Vec::new(),
            tasks => vec![Checklist::SEED.with(tasks)],
        }
    }

    fn view(cx: &mut ViewCx<Checklist>) -> Element {
        let draft = cx
            .input(|list| list.draft.clone(), Checklist::DRAFT)
            .with_id("draft");
        let rows = cx.each(
            |list| &list.tasks,
            |row| {
                let Some(id) = row.item().cloned() else {
                    return Box::new(Column::new()) as Element;
                };
                Box::new(tilo_app::row![
                    row.text(|task| task.title.clone()),
                    row.checkbox(
                        "keystroke-done",
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
    // whole publications per keystroke before). The harness's discovery is
    // uncached, so its release frame publishes again; a hosted app's keeps
    // the tree (`a_hosted_keystroke_publishes_at_most_twice`).
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

/// A window-sized region of a host's scene.
struct Region;

impl tilo_app::app::AppSurface for Region {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> tilo_app::winit::dpi::PhysicalSize<u32> {
        tilo_app::winit::dpi::PhysicalSize::new(1280, 800)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: tilo_app::winit::window::CursorIcon) {}
}

/// One host frame of an app shown in a region of the host's scene, as
/// `ui_core::app::EmbeddedApp` runs it: up to three app frames until it is
/// at rest, then the accessibility tree the host's walk reads.
fn host_frame(app: &mut tilo_app::App) {
    for _ in 0..3 {
        let mut scene = Scene::new();
        if !app.surface_frame(&Region, &mut scene) || !app.unsettled() {
            break;
        }
    }
    let _ = app.refresh_semantic_tree();
}

/// The draft field's value as assistive technology reads it.
fn draft_value(app: &tilo_app::App) -> Option<String> {
    app.semantic_tree()
        .iter()
        .find(|node| node.role == tilo_app::a11y::Role::TextInput)
        .and_then(|node| node.value.clone())
}

/// How many publications each of three keystrokes into the draft of a live
/// window with `tasks` tasks ran, driven the way a host drives an app it
/// shows: the key's press and release through `App::surface_input`, each
/// followed by a host frame, since a key is held longer than a frame lasts.
fn hosted_keystrokes(tasks: u32) -> Result<Vec<usize>, String> {
    use tilo_app::synthetic_input::key_input;
    use tilo_app::winit::event::ElementState;
    GENESIS_TASKS.set(tasks);
    let typed = drive_live_window::<Keystrokes, _>(|app| {
        app.init_layout();
        host_frame(app);
        if !app.focus_order_edge(false) {
            return Err("the window has no control to type into".to_owned());
        }
        host_frame(app);
        let mut counts = Vec::new();
        for key in ["a", "b", "c"] {
            let key = tilo_app::winit::keyboard::Key::Character(key.into());
            let ((), phases) = observe(|| {
                app.surface_input(&Region, key_input(&key, ElementState::Pressed), false);
                host_frame(app);
                app.surface_input(&Region, key_input(&key, ElementState::Released), false);
                host_frame(app);
            });
            counts.push(
                phases
                    .iter()
                    .filter(|phase| phase.phase == "framework.publication")
                    .count(),
            );
        }
        Ok((counts, draft_value(app)))
    });
    GENESIS_TASKS.set(0);
    let (counts, draft) = typed.map_err(failure)??;
    if draft.as_deref() != Some("abc") {
        return Err(format!("the keystrokes missed the draft: {draft:?}"));
    }
    Ok(counts)
}

#[test]
fn a_hosted_keystroke_publishes_at_most_twice() {
    for tasks in [3, 60] {
        let counts = hosted_keystrokes(tasks).expect("keystrokes into a live window");
        let (first, rest) = counts.split_first().expect("a keystroke was observed");
        // One publication resolves the draft's edit in the door, one shows
        // it in the next frame. Before TIL-751 the door resolved the edit a
        // second time to choose its route, and the key's release dropped the
        // tree so the frame after it published again: four.
        assert!(
            rest.iter().all(|&count| (1..=2).contains(&count)),
            "publications per keystroke with {tasks} tasks: {counts:?}"
        );
        // The first key moves the window from pointer to keyboard modality,
        // which may change what it presents, so its press republishes once.
        assert!(
            (1..=3).contains(first),
            "publications for the first keystroke with {tasks} tasks: {counts:?}"
        );
    }
}

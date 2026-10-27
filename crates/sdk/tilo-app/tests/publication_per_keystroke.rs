//! What one keystroke publishes in a framework app, as its resource grows.
//!
//! Typing into a field unrelated to the list must not cost the list once per
//! row: bound controls refresh only the resource's own actions, and every
//! publication the host asks for binds row edits in one walk of the view.
//! A hosted app publishes its tree at most twice per keystroke: once when
//! the door resolves the field's edit, once when the next frame shows it.
#![cfg(feature = "test-support")]

use std::cell::Cell;

use tilo_app::framework::testing::drive_live_window;
use tilo_app::i18n::EmbeddedCatalog;
use tilo_app::prelude::*;
use tilo_app::tilo_resource::profiling::observe;

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[(
        "en-US",
        "keystroke-draft = Next task\nkeystroke-done = Completed\n",
    )],
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
    Ok(phases
        .into_iter()
        .map(|phase| (phase.phase, phase.calls))
        .collect())
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
        let mut scene = tilo_app::advanced::Scene::new();
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
    // Keep the real hosted path, but hold monotonic time during this
    // input-only observation, so no deadline of the window's falls due
    // between press and release and publishes work the keystroke did not
    // cause. The separate save-status test below covers the frame after the
    // draft's write lands.
    let typed = tilo_app::time::testing::with_monotonic_time(tilo_app::time::now(), || {
        drive_live_window::<Keystrokes, _>(|app| {
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
                        .find(|phase| phase.phase == "framework.publication")
                        .map_or(0, |phase| phase.calls),
                );
            }
            Ok((counts, draft_value(app)))
        })
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
        // it in the next frame. Before #4148 the door resolved the edit a
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

#[test]
fn an_overdue_save_status_frame_still_publishes_and_preserves_typed_content() {
    use tilo_app::synthetic_input::key_input;
    use tilo_app::winit::event::ElementState;

    let seen = tilo_app::time::testing::with_monotonic_time(tilo_app::time::now(), || {
        drive_live_window::<Keystrokes, _>(|app| {
            app.init_layout();
            host_frame(app);
            assert!(app.focus_order_edge(false));
            host_frame(app);
            let key = tilo_app::winit::keyboard::Key::Character("a".into());
            app.surface_input(&Region, key_input(&key, ElementState::Pressed), false);
            host_frame(app);
            app.surface_input(&Region, key_input(&key, ElementState::Released), false);
            host_frame(app);
            assert_eq!(draft_value(app).as_deref(), Some("a"));
            // The window shows the draft as "Saving…" until the record's
            // store has written it, after its writer's debounce, on the
            // store's own thread; nothing resamples it on a timer. When the
            // write lands the store wakes the window, and the host frame
            // after that must publish the new save state without losing
            // what was typed. Run host frames, as a live host does while it
            // waits, until one publishes.
            tilo_test_env::wait_until(
                "the frame after the draft's write landed to publish",
                || {
                    let ((), phases) = observe(|| host_frame(app));
                    phases
                        .iter()
                        .any(|phase| phase.phase == "framework.publication")
                },
            );
            assert_eq!(draft_value(app).as_deref(), Some("a"));
        })
    });
    seen.expect("a live window whose record's write lands");
}

/// Which half of a keystroke [`hosted_frame_ms`] times.
#[derive(Clone, Copy, PartialEq)]
enum Half {
    /// From the key's press to the end of the host frame that shows it.
    Press,
    /// From the key's release to the end of the host frame after it, a
    /// frame that changes nothing.
    Release,
}

/// p50, in ms, of one half of `KEYS` keystrokes into the draft of a live
/// window with `tasks` tasks. Driven as [`hosted_keystrokes`] drives it,
/// with the app's monotonic clock held for the same reason: an idle
/// save-status frame is not this frame's cost. The stopwatch is
/// `Instant::elapsed`, which that hold does not touch.
fn hosted_frame_ms(tasks: u32, half: Half) -> Result<f64, String> {
    use tilo_app::synthetic_input::key_input;
    use tilo_app::winit::event::ElementState;
    const KEYS: usize = 12;
    GENESIS_TASKS.set(tasks);
    let timed = tilo_app::time::testing::with_monotonic_time(tilo_app::time::now(), || {
        drive_live_window::<Keystrokes, _>(|app| {
            app.init_layout();
            host_frame(app);
            if !app.focus_order_edge(false) {
                return Err("the window has no control to type into".to_owned());
            }
            host_frame(app);
            let key = tilo_app::winit::keyboard::Key::Character("a".into());
            let mut samples = Vec::with_capacity(KEYS);
            // The first key moves the window to keyboard modality; not timed.
            for typed in 0..=KEYS {
                let start = std::time::Instant::now();
                app.surface_input(&Region, key_input(&key, ElementState::Pressed), false);
                host_frame(app);
                let pressed = start.elapsed();
                let start = std::time::Instant::now();
                app.surface_input(&Region, key_input(&key, ElementState::Released), false);
                host_frame(app);
                let released = start.elapsed();
                if typed > 0 {
                    let elapsed = match half {
                        Half::Press => pressed,
                        Half::Release => released,
                    };
                    samples.push(elapsed.as_secs_f64() * 1e3);
                }
            }
            Ok((samples, draft_value(app)))
        })
    });
    GENESIS_TASKS.set(0);
    let (mut samples, draft) = timed.map_err(failure)??;
    if draft.map(|draft| draft.chars().count()) != Some(KEYS + 1) {
        return Err("the keystrokes missed the draft".to_owned());
    }
    samples.sort_by(f64::total_cmp);
    samples
        .get(samples.len() / 2)
        .copied()
        .ok_or_else(|| "no frame was timed".to_owned())
}

/// The best of three rounds of [`hosted_frame_ms`]: a loaded runner slows a
/// round, never speeds one.
fn best_hosted_frame_ms(tasks: u32, half: Half) -> Result<f64, String> {
    (0..3).try_fold(f64::INFINITY, |best, _| {
        Ok(best.min(hosted_frame_ms(tasks, half)?))
    })
}

/// Which framework and toolkit phases each of three keystrokes into the
/// draft of a live window with `tasks` tasks ran, and how often: the half of
/// each key that `half` names, after a first key that moves the window to
/// keyboard modality.
fn hosted_frame_phases(tasks: u32, half: Half) -> Result<Vec<Phases>, String> {
    use tilo_app::synthetic_input::key_input;
    use tilo_app::winit::event::ElementState;
    GENESIS_TASKS.set(tasks);
    let observed = tilo_app::time::testing::with_monotonic_time(tilo_app::time::now(), || {
        drive_live_window::<Keystrokes, _>(|app| {
            app.init_layout();
            host_frame(app);
            if !app.focus_order_edge(false) {
                return Err("the window has no control to type into".to_owned());
            }
            host_frame(app);
            let key = tilo_app::winit::keyboard::Key::Character("a".into());
            let mut frames = Vec::new();
            for typed in 0..=3 {
                let ((), pressed) = observe(|| {
                    app.surface_input(&Region, key_input(&key, ElementState::Pressed), false);
                    host_frame(app);
                });
                let ((), released) = observe(|| {
                    app.surface_input(&Region, key_input(&key, ElementState::Released), false);
                    host_frame(app);
                });
                if typed > 0 {
                    let phases = match half {
                        Half::Press => pressed,
                        Half::Release => released,
                    };
                    frames.push(
                        phases
                            .into_iter()
                            .map(|phase| (phase.phase, phase.calls))
                            .collect::<Phases>(),
                    );
                }
            }
            Ok((frames, draft_value(app)))
        })
    });
    GENESIS_TASKS.set(0);
    let (frames, draft) = observed.map_err(failure)??;
    if draft.as_deref() != Some("aaaa") {
        return Err(format!("the keystrokes missed the draft: {draft:?}"));
    }
    Ok(frames)
}

#[test]
fn a_hosted_frame_that_changes_nothing_does_the_same_work_whatever_the_list() {
    // Both windows are full: they mount the same rows, so only work that
    // grows with the list can tell them apart.
    let full = hosted_frame_phases(100, Half::Release).expect("keystrokes into a live window");
    let long = hosted_frame_phases(1_000, Half::Release).expect("keystrokes into a live window");
    assert_eq!(
        long, full,
        "phases of a frame that changes nothing, 1,000 tasks against 100"
    );
    for frame in &full {
        assert!(
            frame.keys().all(|phase| !phase.starts_with("framework.")),
            "a frame that changes nothing publishes, binds and surveys nothing: {frame:?}"
        );
    }
}

#[test]
fn a_hosted_keystroke_does_the_same_work_whatever_the_list() {
    let full = hosted_frame_phases(100, Half::Press).expect("keystrokes into a live window");
    let long = hosted_frame_phases(1_000, Half::Press).expect("keystrokes into a live window");
    assert_eq!(
        long, full,
        "phases of a keystroke's frame, 1,000 tasks against 100"
    );
}

#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn a_hosted_frame_that_changes_nothing_costs_the_same_whatever_the_list() {
    // Both windows are full: they mount the same rows, so only work that
    // grows with the list can tell them apart. Before #4176 each such
    // frame copied the whole published tree, re-filtered and re-surveyed
    // every action for the controls, and rebuilt a semantic node per
    // resource: at 1,000 tasks it cost several times what it does at 100.
    let full = best_hosted_frame_ms(100, Half::Release).expect("keystrokes into a live window");
    let long = best_hosted_frame_ms(1_000, Half::Release).expect("keystrokes into a live window");
    eprintln!("{full:.3} ms with 100 tasks, {long:.3} ms with 1,000");
    assert!(
        long <= 2.0 * full,
        "a frame that changes nothing took {long:.3} ms with 1,000 tasks and {full:.3} ms with \
         100: it grows with the list"
    );
}

#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn a_hosted_keystroke_costs_the_same_whatever_the_list() {
    // Both windows are full: they mount the same rows, so only work that
    // grows with the list can tell them apart. Before #4202 every edit
    // rebuilt each item's published resource, copied and filtered the whole
    // tree, re-surveyed every address for the controls and rebuilt a
    // semantic node per item, although the draft is no item's: at 1,000
    // tasks the key cost 4.7 times what it does at 100.
    let full = best_hosted_frame_ms(100, Half::Press).expect("keystrokes into a live window");
    let long = best_hosted_frame_ms(1_000, Half::Press).expect("keystrokes into a live window");
    eprintln!("{full:.3} ms with 100 tasks, {long:.3} ms with 1,000");
    assert!(
        long <= 2.0 * full,
        "a keystroke took {long:.3} ms to its frame with 1,000 tasks and {full:.3} ms with 100: \
         it grows with the list"
    );
}

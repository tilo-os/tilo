//! How the app framework's per-input cost grows with its resource: a
//! checklist of N tasks, each row a title and a checkbox.
//!
//! For each N it prints what one full publication of the resource tree costs
//! (the work every cache miss repeats), and what a pointer move over a real
//! [`tilo_app::App`] costs through its production input path
//! ([`tilo_app::App::surface_input`]), with how many publications those moves
//! ran. Bare pointer motion must not republish (TIL-628). It also prints the
//! sync pass per frame while typing into the draft and while scrolling the
//! list, which must stay flat as the list grows (TIL-621), and what opening
//! the window over a record that already holds the list costs: the time to
//! its first laid-out frame, the allocations that took, and the heap the
//! open window holds per task. It also types into the draft of the live
//! window the way a host drives an app it shows
//! ([`tilo_app::App::surface_input`] for the key's press and release, each
//! followed by the host's frame, [`tilo_app::App::surface_frame`], and its
//! accessibility walk), and prints the time from key press to the end of
//! the frame that shows the key, with how many publications one keystroke
//! runs (TIL-751).
//!
//! ```text
//! cargo run --release -p tilo-app --features test-support \
//!     --example framework-scaling -- 10,250,1000
//! ```
//!
//! With `TILO_PROFILE_RESOURCES=1` each move's resource work is printed to
//! stderr as `input_carry` stages (see `docs/app-performance.md`).
//!
//! A second argument `open` measures only the open, for profiling it. Each
//! size's open also prints a `FRAMEWORK-OPEN` line, which
//! `tools/perf/adapters.py framework-open` turns into the
//! `render.framework_open.*` KPIs of the scheduled performance lane:
//!
//! ```text
//! cargo run --release -p tilo-app --features test-support \
//!     --example framework-scaling -- 5000,50000 open
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use tilo_app::framework::profiling::observe;
use tilo_app::framework::testing::drive_live_window;
use tilo_app::i18n::EmbeddedCatalog;
use tilo_app::prelude::*;
use tilo_app::time::Instant;
use tilo_app::winit::dpi::{PhysicalPosition, PhysicalSize};
use tilo_app::winit::event::DeviceId;
use tilo_app::winit::window::CursorIcon;

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[(
        "en-US",
        "scaling-draft = Next task\nscaling-done = Completed\n",
    )],
    "scaling-draft",
);

/// The system allocator, counting allocations and the bytes held.
struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static HELD: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call forwards to the system allocator unchanged; the
// counters are plain atomics.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        HELD.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: the caller's contract is passed on unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        HELD.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: the caller's contract is passed on unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        HELD.fetch_add(new_size, Ordering::Relaxed);
        HELD.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: the caller's contract is passed on unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// How many tasks the next window's record starts with.
static TASKS: AtomicU32 = AtomicU32::new(0);

/// Pointer moves timed per size.
const MOVES: u32 = 100;

/// Keystrokes timed per size.
const KEYS: u32 = 20;

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

#[resource(kind = "example.framework-scaling", label = "Checklist")]
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
                    title: format!("Task {index}: the quick brown fox jumps over the lazy dog"),
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

/// The checklist's window.
pub struct Scaling;

impl ResourceApp for Scaling {
    type Resource = Checklist;
    const ID: &'static str = "so.tilo.example.framework-scaling";
    const TITLE: &'static str = "Checklist";

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    /// A benchmark's record starts at the size being measured. (A real app's
    /// genesis depends on its launch input alone.)
    fn genesis(_: &LaunchInput<'_>) -> Vec<Invoke<Checklist>> {
        vec![Checklist::SEED.with(TASKS.load(Ordering::Relaxed))]
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
                        "scaling-done",
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

/// A window-sized surface with nothing behind it.
struct Surface;

impl tilo_app::app::AppSurface for Surface {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> PhysicalSize<u32> {
        PhysicalSize::new(1280, 800)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: CursorIcon) {}
}

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples.get(samples.len() / 2).copied().unwrap_or(0.0)
}

fn millis(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1e3
}

/// The resources the tree publishes, children included.
fn count(resources: &[tilo_actions::Resource]) -> usize {
    resources
        .iter()
        .map(|resource| count(&resource.children).saturating_add(1))
        .fold(0, usize::saturating_add)
}

/// A checklist window holding `tasks` tasks. Its genesis seeds as many as
/// the last live window was asked for; the rest are added here.
fn checklist(tasks: u32) -> Result<TestApp<Scaling>, Box<dyn std::error::Error>> {
    let mut app = TestApp::<Scaling>::for_person("ada")?;
    let seeded = u32::try_from(app.state().tasks.len())?;
    if let Some(missing) = tasks.checked_sub(seeded).filter(|&missing| missing > 0) {
        app.call("seed", serde_json::json!({ "count": missing }))?;
    }
    Ok(app)
}

/// p50 of one full publication of `tasks` tasks, and how many resources it
/// publishes.
fn publication(tasks: u32) -> Result<(f64, usize), Box<dyn std::error::Error>> {
    let mut app = checklist(tasks)?;
    let doc = app.read_document(Clone::clone);
    let mut published = 0;
    let samples = (0..10)
        .map(|_| {
            let start = Instant::now();
            let resources =
                tilo_app::resources::resources_for(app.harness().root_widget(), None, &doc);
            published = count(&resources);
            millis(start)
        })
        .collect();
    Ok((median(samples), published))
}

/// The sync pass and the whole frame, p50 in ms, while typing into the
/// draft (each key a change to the resource) and while scrolling the list
/// (no change). They should stay flat however many tasks the list holds.
fn per_frame(tasks: u32) -> Result<[(f64, f64); 2], Box<dyn std::error::Error>> {
    use tilo_app::profiling::{PerfScenario, PerfStep, run_scenario};
    let mut app = checklist(tasks)?;
    app.harness()
        .perform_named("draft", &tilo_app::a11y::Action::Focus)?;
    let sync = |app: &mut TestApp<Scaling>, name: &str, steps| {
        let report = run_scenario(
            app.harness(),
            &PerfScenario {
                name: name.to_owned(),
                viewport: (1280, 800),
                warmup: 2,
                steps,
            },
        );
        let ms = |us| std::time::Duration::from_micros(us).as_secs_f64() * 1e3;
        (ms(report.sync.p50_us), ms(report.total.p50_us))
    };
    let typing = sync(
        &mut app,
        "typing",
        vec![PerfStep::Type("hello world again ".into())],
    );
    app.harness().send(&WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: PhysicalPosition::new(400.0, 500.0),
    });
    let scrolling = sync(
        &mut app,
        "scrolling",
        vec![PerfStep::Scroll {
            dy: -3.0,
            count: 60,
        }],
    );
    Ok([typing, scrolling])
}

/// What opening a window over `tasks` tasks cost.
struct Open {
    /// Each open to its first laid-out frame, in ms.
    samples: Vec<f64>,
    /// The allocations the last open made.
    allocations: usize,
    /// The heap held with only that window open.
    held: usize,
}

/// Opening the window over a record already holding `tasks` tasks, to its
/// first laid-out frame, five times.
fn open(tasks: u32) -> Result<Open, Box<dyn std::error::Error>> {
    TASKS.store(tasks, Ordering::Relaxed);
    let mut app = checklist(tasks)?;
    let mut samples = Vec::new();
    let mut allocations = 0;
    let mut held = 0;
    for _ in 0..5 {
        app.flush()?;
        let start = Instant::now();
        let before = ALLOCATIONS.load(Ordering::Relaxed);
        app = app.reopen()?;
        app.harness().relayout();
        allocations = ALLOCATIONS.load(Ordering::Relaxed).saturating_sub(before);
        samples.push(millis(start));
        held = HELD.load(Ordering::Relaxed);
    }
    Ok(Open {
        samples,
        allocations,
        held,
    })
}

/// p50 of one pointer move over the live window, and how many publications
/// the moves ran.
fn pointer_moves(app: &mut tilo_app::App) -> (f64, usize) {
    let move_to = |app: &mut tilo_app::App, x: f64| {
        app.surface_input(
            &Surface,
            WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(x, 400.0),
            },
            false,
        );
    };
    app.init_layout();
    // The first move after layout settles the pointer's position.
    move_to(app, 300.0);
    let mut publications: usize = 0;
    let samples = (0..MOVES)
        .map(|step| {
            let start = Instant::now();
            let ((), phases) = observe(|| move_to(app, 300.0 + f64::from(step)));
            let elapsed = millis(start);
            publications = publications.saturating_add(
                phases
                    .iter()
                    .filter(|phase| phase.phase == "framework.publication")
                    .count(),
            );
            elapsed
        })
        .collect();
    (median(samples), publications)
}

/// One host frame of an app shown in a region of the host's scene, as
/// `ui_core::app::EmbeddedApp` runs it: up to three app frames until it is
/// at rest, then the accessibility tree the host's walk reads.
fn host_frame(app: &mut tilo_app::App) {
    for _ in 0..3 {
        let mut scene = Scene::new();
        if !app.surface_frame(&Surface, &mut scene) || !app.unsettled() {
            break;
        }
    }
    let _ = app.refresh_semantic_tree();
}

/// The draft field's value in the accessibility tree.
fn draft_value(app: &tilo_app::App) -> Option<String> {
    app.semantic_tree()
        .iter()
        .find(|node| node.role == tilo_app::a11y::Role::TextInput)
        .and_then(|node| node.value.clone())
}

/// What typing into the draft of the live window costs.
struct Keystrokes {
    /// p50 from a key's press to the end of the host frame that shows it.
    to_frame_ms: f64,
    /// The most publications one keystroke ran.
    publications: usize,
    /// p50 of the time one keystroke spent publishing.
    publication_ms: f64,
}

/// Type into the draft of the live window. A frame runs between press and
/// release, as on a display: a key is held for longer than a frame lasts.
fn keystrokes(app: &mut tilo_app::App) -> Result<Keystrokes, String> {
    use tilo_app::synthetic_input::key_input;
    use tilo_app::winit::event::ElementState;
    let key = tilo_app::winit::keyboard::Key::Character("a".into());
    app.init_layout();
    host_frame(app);
    if !app.focus_order_edge(false) {
        return Err("the window has no control to type into".into());
    }
    host_frame(app);
    let mut counts = Vec::new();
    let mut work = Vec::new();
    let mut samples = Vec::new();
    // The first keystroke is a warm-up, neither timed nor counted: it moves
    // the window from pointer to keyboard modality, which republishes once.
    for _ in 0..=KEYS {
        let ((), phases) = observe(|| {
            let start = Instant::now();
            app.surface_input(&Surface, key_input(&key, ElementState::Pressed), false);
            host_frame(app);
            samples.push(millis(start));
            app.surface_input(&Surface, key_input(&key, ElementState::Released), false);
            host_frame(app);
        });
        let publications = phases
            .iter()
            .filter(|phase| phase.phase == "framework.publication");
        counts.push(publications.clone().count());
        work.push(
            publications
                .map(|phase| {
                    std::time::Duration::from_nanos(
                        u64::try_from(phase.elapsed_ns).unwrap_or(u64::MAX),
                    )
                })
                .sum::<std::time::Duration>()
                .as_secs_f64()
                * 1e3,
        );
    }
    samples.remove(0);
    counts.remove(0);
    work.remove(0);
    let publications = counts.into_iter().max().unwrap_or(0);
    let typed = draft_value(app).unwrap_or_default();
    let expected = KEYS.saturating_add(1);
    if typed.chars().count() != usize::try_from(expected).map_err(|error| error.to_string())? {
        return Err(format!("{expected} keystrokes left the draft at {typed:?}"));
    }
    Ok(Keystrokes {
        to_frame_ms: median(samples),
        publications,
        publication_ms: median(work),
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let open_only = std::env::args().nth(2).is_some_and(|mode| mode == "open");
    let sizes = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "10,250,1000".into())
        .split(',')
        .map(|size| size.trim().parse::<u32>())
        .collect::<Result<Vec<_>, _>>()?;
    if !open_only {
        println!(
            "tasks  resources  publication p50  pointer move p50  publications in {MOVES} moves  \
             typing p50: sync / frame  scrolling p50: sync / frame  keystroke→frame p50  \
             publications per keystroke  publishing/keystroke p50"
        );
    }
    // The heap a window over an empty list holds; the rest is per task.
    let empty = open(0)?.held;
    let mut opens = Vec::new();
    for &tasks in &sizes {
        if open_only {
            opens.push((tasks, open(tasks)?));
            continue;
        }
        let (publish_ms, published) = publication(tasks)?;
        let [(typing_sync, typing), (scrolling_sync, scrolling)] = per_frame(tasks)?;
        TASKS.store(tasks, Ordering::Relaxed);
        let (move_ms, publications) = drive_live_window::<Scaling, _>(pointer_moves)?;
        let keys = drive_live_window::<Scaling, _>(keystrokes)??;
        println!(
            "{tasks:>5}  {published:>9}  {publish_ms:>12.3} ms  {move_ms:>13.3} ms  \
             {publications:>27}  {typing_sync:>8.3} / {typing:>8.3} ms  \
             {scrolling_sync:>11.3} / {scrolling:>8.3} ms  \
             {:>16.3} ms  {:>26}  {:>25.3} ms",
            keys.to_frame_ms, keys.publications, keys.publication_ms
        );
        opens.push((tasks, open(tasks)?));
    }
    println!("\ntasks  open to first frame p50  allocations  heap held  heap per task");
    let mut lines = Vec::new();
    for (tasks, open) in opens {
        let per_task = open
            .held
            .saturating_sub(empty)
            .checked_div(usize::try_from(tasks)?)
            .unwrap_or_default();
        let Open {
            samples,
            allocations,
            held,
        } = open;
        let samples_ms = samples
            .iter()
            .map(|sample| format!("{sample:.3}"))
            .collect::<Vec<_>>()
            .join(",");
        let open_ms = median(samples);
        println!("{tasks:>5}  {open_ms:>20.3} ms  {allocations:>11}  {held:>9}  {per_task:>11} B");
        // For tools/perf/adapters.py framework-open.
        lines.push(format!(
            "FRAMEWORK-OPEN tasks={tasks} samples_ms={samples_ms} allocations={allocations} \
             heap_per_task={per_task}"
        ));
    }
    println!();
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

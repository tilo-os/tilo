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
//! open window holds per task.
//!
//! ```text
//! cargo run --release -p tilo-app --features test-support \
//!     --example framework-scaling -- 10,250,1000
//! ```
//!
//! With `TILO_PROFILE_RESOURCES=1` each move's resource work is printed to
//! stderr as `input_carry` stages (see `docs/app-performance.md`).

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

/// Opening the window over a record already holding `tasks` tasks, to its
/// first laid-out frame: p50 in ms, the allocations that took, and the heap
/// held with only that window open.
fn open(tasks: u32) -> Result<(f64, usize, usize), Box<dyn std::error::Error>> {
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
    Ok((median(samples), allocations, held))
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sizes = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "10,250,1000".into())
        .split(',')
        .map(|size| size.trim().parse::<u32>())
        .collect::<Result<Vec<_>, _>>()?;
    println!(
        "tasks  resources  publication p50  pointer move p50  publications in {MOVES} moves  \
         typing p50: sync / frame  scrolling p50: sync / frame"
    );
    // The heap a window over an empty list holds; the rest is per task.
    let (_, _, empty) = open(0)?;
    let mut opens = Vec::new();
    for &tasks in &sizes {
        let (publish_ms, published) = publication(tasks)?;
        let [(typing_sync, typing), (scrolling_sync, scrolling)] = per_frame(tasks)?;
        TASKS.store(tasks, Ordering::Relaxed);
        let (move_ms, publications) = drive_live_window::<Scaling, _>(pointer_moves)?;
        println!(
            "{tasks:>5}  {published:>9}  {publish_ms:>12.3} ms  {move_ms:>13.3} ms  \
             {publications:>27}  {typing_sync:>8.3} / {typing:>8.3} ms  \
             {scrolling_sync:>11.3} / {scrolling:>8.3} ms"
        );
        opens.push((tasks, open(tasks)?));
    }
    println!("\ntasks  open to first frame p50  allocations  heap held  heap per task");
    for (tasks, (open_ms, allocations, held)) in opens {
        let per_task = held.saturating_sub(empty) / usize::try_from(tasks.max(1))?;
        println!("{tasks:>5}  {open_ms:>20.3} ms  {allocations:>11}  {held:>9}  {per_task:>11} B");
    }
    Ok(())
}

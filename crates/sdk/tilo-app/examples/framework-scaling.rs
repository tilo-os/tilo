//! How the app framework's per-input cost grows with its resource: a
//! checklist of N tasks, each row a title and a checkbox.
//!
//! For each N it prints what one full publication of the resource tree costs
//! (the work every cache miss repeats), and what a pointer move over a real
//! [`tilo_app::App`] costs through its production input path
//! ([`tilo_app::App::surface_input`]), with how many publications those moves
//! ran. Bare pointer motion must not republish (TIL-628).
//!
//! ```text
//! cargo run --release -p tilo-app --features test-support \
//!     --example framework-scaling -- 10,250,1000
//! ```
//!
//! With `TILO_PROFILE_RESOURCES=1` each move's resource work is printed to
//! stderr as `input_carry` stages (see `docs/app-performance.md`).

use std::sync::atomic::{AtomicU32, Ordering};

use tilo_app::framework::profiling::observe;
use tilo_app::framework::testing::drive_live_window;
use tilo_app::i18n::EmbeddedCatalog;
use tilo_app::prelude::*;
use tilo_app::time::Instant;
use tilo_app::winit::dpi::{PhysicalPosition, PhysicalSize};
use tilo_app::winit::event::{DeviceId, WindowEvent};
use tilo_app::winit::window::CursorIcon;

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[(
        "en-US",
        "scaling-draft = Next task\nscaling-done = Completed\n",
    )],
    "scaling-draft",
);

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
            .input_with(|list| list.draft.clone(), Checklist::DRAFT)
            .with_id("draft");
        let rows = cx.each(
            |list| &list.tasks,
            |row| {
                let Some(id) = row.item().cloned() else {
                    return Box::new(Column::new()) as Element;
                };
                Box::new(tilo_app::row![
                    row.text(|task| task.title.clone()),
                    row.checkbox_with(
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

/// p50 of one full publication of `tasks` tasks, and how many resources it
/// publishes.
fn publication(tasks: u32) -> Result<(f64, usize), Box<dyn std::error::Error>> {
    let mut app = TestApp::<Scaling>::for_person("ada")?;
    let seeded = app.state().tasks.len();
    if seeded < usize::try_from(tasks)? {
        app.call("seed", serde_json::json!({ "count": tasks }))?;
    }
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
    println!("tasks  resources  publication p50  pointer move p50  publications in {MOVES} moves");
    for tasks in sizes {
        let (publish_ms, published) = publication(tasks)?;
        TASKS.store(tasks, Ordering::Relaxed);
        let (move_ms, publications) = drive_live_window::<Scaling, _>(pointer_moves)?;
        println!(
            "{tasks:>5}  {published:>9}  {publish_ms:>12.3} ms  {move_ms:>13.3} ms  {publications:>5}"
        );
    }
    Ok(())
}

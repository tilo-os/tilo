//! How the app framework's per-input cost grows with its resource: a
//! checklist of N tasks, each row a title and a checkbox.
//!
//! For each N it prints what one full publication of the resource tree costs
//! (the work every cache miss repeats), and what a pointer move over a real
//! [`tilo_app::App`] costs through its production input path
//! ([`tilo_app::App::surface_input`]), with how many publications those moves
//! ran. Bare pointer motion must not republish. It also prints the
//! sync pass per frame while typing into the draft and while scrolling the
//! list, which must stay flat as the list grows, and what opening
//! the window over a record that already holds the list costs: the time to
//! its first laid-out frame, the allocations that took, and the heap the
//! open window holds per task. It also types into the draft of the live
//! window the way a host drives an app it shows
//! ([`tilo_app::App::surface_input`] for the key's press and release, each
//! followed by the host's frame, [`tilo_app::App::surface_frame`], and its
//! accessibility walk), and prints the time from key press to the end of
//! the frame that shows the key, with how many publications one keystroke
//! runs, and the time from the key's release to the end of the
//! frame after it, a frame that changes nothing.
//!
//! ```text
//! cargo run --release -p tilo-app --features test-support \
//!     --example framework-scaling -- 10,250,1000,10000
//! ```
//!
//! `TILO_SCALING_KEYSTROKES=400` times that many keystrokes per size and
//! skips every other measurement, for a sampling profiler (`sample <pid>` on
//! macOS) to see typing alone.
//!
//! With `TILO_PROFILE_RESOURCES=1` each move's resource work is printed to
//! stderr as `input_carry` stages (see `docs/app-performance.md`), and in
//! the `focus` mode every scope of one more added task as an `add_item` line.
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
//!
//! A second argument `scroll` measures only scrolling: the live window's
//! list scrolled down 120 frames of 40 px (and of 40.5 px, part of a device
//! pixel), the pointer resting over it, each frame presented on an
//! offscreen GPU target as a window presents it, with and without copying
//! what a scroll keeps on screen (`ui::scroll_copy`): pixels repainted and
//! copied per frame, frames repainted whole, CPU and GPU time per frame. The
//! last frame must equal a full render, or the run fails. The window is
//! also scrolled with a collaborator's pointer resting over the list,
//! painted over the widgets as a window paints it, with and without copies.
//! Each run also prints a `FRAMEWORK-SCROLL` line (`-PEER` with the
//! collaborator, `-HOSTED` shown in a host's scene) for
//! `tools/perf/adapters.py framework-scroll` (`render.framework_scroll.*`
//! KPIs):
//!
//! ```text
//! cargo run --release -p tilo-app --features test-support \
//!     --example framework-scaling -- 5000 scroll
//! ```
//!
//! A second argument `focus` measures only the keyboard focus order: what an
//! agent call adding one task costs end to end through the test harness, what
//! one focus-order walk of the harness costs after it, and what one recomputation
//! of the live window's order costs ([`tilo_app::App::refresh_focus_order`],
//! timed as its `ui.focus_order` scope alone). Its rows sit outside a scroll
//! view, so every row is mounted, and a row is about four widgets: 500 tasks
//! is a window of about 2,000. Each size prints a
//! `FRAMEWORK-FOCUS` line for `tools/perf/adapters.py framework-focus`
//! (`render.framework_focus.*` KPIs):
//!
//! ```text
//! cargo run --release -p tilo-app --features test-support \
//!     --example framework-scaling -- 250,500,1000,2500 focus
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use tilo_app::framework::testing::{drive_live_embedded, drive_live_window};
use tilo_app::i18n::EmbeddedCatalog;
use tilo_app::prelude::*;
use tilo_app::tilo_resource::profiling::observe;
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
        let (draft, rows) = draft_and_rows(cx);
        let list: Element = Box::new(Scrollable::new_vertical().with_child(rows));
        Box::new(Column::new().push(draft).push(list))
    }
}

/// The draft field and one row per task.
fn draft_and_rows(cx: &ViewCx<Checklist>) -> (Element, Element) {
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
                row.checkbox("scaling-done", done),
            ]) as Element
        },
    );
    (Box::new(draft), Box::new(rows))
}

/// The same checklist with its rows outside a scroll view, so every row is
/// mounted: a window whose widget count grows with the list, as the `focus`
/// mode needs.
pub struct Mounted;

impl ResourceApp for Mounted {
    type Resource = Checklist;
    const ID: &'static str = "so.tilo.example.framework-scaling-mounted";
    const TITLE: &'static str = "Checklist";

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn genesis(input: &LaunchInput<'_>) -> Vec<Invoke<Checklist>> {
        Scaling::genesis(input)
    }

    fn view(cx: &mut ViewCx<Checklist>) -> Element {
        let (draft, rows) = draft_and_rows(cx);
        Box::new(Column::new().push(draft).push(rows))
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

/// The `percent`th percentile of `samples` (nearest rank).
fn percentile(mut samples: Vec<f64>, percent: usize) -> f64 {
    samples.sort_by(f64::total_cmp);
    let rank = samples
        .len()
        .saturating_mul(percent)
        .div_ceil(100)
        .saturating_sub(1);
    samples.get(rank).copied().unwrap_or(0.0)
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
    checklist_in::<Scaling>(tasks)
}

/// [`checklist`] in the window `A`.
fn checklist_in<A: ResourceApp<Resource = Checklist>>(
    tasks: u32,
) -> Result<TestApp<A>, Box<dyn std::error::Error>> {
    let mut app = TestApp::<A>::for_person("ada")?;
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
                    .map(|phase| phase.calls)
                    .sum(),
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
        let mut scene = tilo_app::advanced::Scene::new();
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
    /// p99 of the same.
    to_frame_p99_ms: f64,
    /// p50 from the key's release to the end of the host frame after it: a
    /// frame that changes nothing.
    release_ms: f64,
    /// The most publications one keystroke ran.
    publications: usize,
    /// p50 of the time one keystroke spent publishing.
    publication_ms: f64,
}

/// How many keystrokes [`keystrokes`] times: [`KEYS`], or
/// `TILO_SCALING_KEYSTROKES`, which also skips every other measurement so a
/// sampling profiler attached to the process sees typing alone.
fn keys_to_time() -> u32 {
    std::env::var("TILO_SCALING_KEYSTROKES")
        .ok()
        .and_then(|keys| keys.parse().ok())
        .unwrap_or(KEYS)
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
    let mut releases = Vec::new();
    // The first keystroke is a warm-up, neither timed nor counted: it moves
    // the window from pointer to keyboard modality, which republishes once.
    let keys = keys_to_time();
    for _ in 0..=keys {
        let ((), phases) = observe(|| {
            let start = Instant::now();
            app.surface_input(&Surface, key_input(&key, ElementState::Pressed), false);
            host_frame(app);
            samples.push(millis(start));
            let start = Instant::now();
            app.surface_input(&Surface, key_input(&key, ElementState::Released), false);
            host_frame(app);
            releases.push(millis(start));
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
    releases.remove(0);
    counts.remove(0);
    work.remove(0);
    let publications = counts.into_iter().max().unwrap_or(0);
    let typed = draft_value(app).unwrap_or_default();
    let expected = keys.saturating_add(1);
    if typed.chars().count() != usize::try_from(expected).map_err(|error| error.to_string())? {
        return Err(format!("{expected} keystrokes left the draft at {typed:?}"));
    }
    Ok(Keystrokes {
        to_frame_p99_ms: percentile(samples.clone(), 99),
        to_frame_ms: median(samples),
        release_ms: median(releases),
        publications,
        publication_ms: median(work),
    })
}

/// Logical pixels one continuous scroll frame moves the list, as a
/// touchpad's two-finger scroll does at 60 Hz.
const SCROLL_STEP: f64 = 40.0;

/// Scroll frames timed per size.
const SCROLL_FRAMES: u32 = 120;

/// What scrolling the live window costs per frame.
struct Scrolling {
    /// Frames the window drew.
    frames: usize,
    /// Of those, frames that repainted the window whole, and the first
    /// reason one gave.
    whole: usize,
    why: Option<&'static str>,
    /// Pixels each frame repaints, p50 and largest.
    pixels_p50: f64,
    pixels_max: f64,
    /// Pixels each frame copies from where the last frame had them, p50.
    copied_p50: f64,
    /// p50 of one scroll frame on the CPU: the wheel event and the frame.
    cpu_ms: f64,
    /// p50 of rasterizing each frame as it is presented (only where it
    /// changed over the frame before), and of rasterizing it whole, on the
    /// GPU, waiting for completion. `None` without a GPU.
    gpu_ms: Option<(f64, f64)>,
    /// Pixels of the last presented frame that differ from a full render
    /// beyond anti-aliasing noise.
    differing: usize,
}

/// The pixels `rects` cover in a window of `size`, each counted once.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "rects are clamped to the window before becoming pixel indices"
)]
fn covered(rects: &[Rect], size: PhysicalSize<u32>) -> f64 {
    let window = Rect::new(0.0, 0.0, f64::from(size.width), f64::from(size.height));
    let mut rows = vec![vec![false; size.width as usize]; size.height as usize];
    for rect in rects {
        let rect = rect.intersect(window).expand();
        let (x0, x1) = (rect.x0 as usize, rect.x1 as usize);
        for row in rows
            .iter_mut()
            .take(rect.y1 as usize)
            .skip(rect.y0 as usize)
        {
            if let Some(cells) = row.get_mut(x0..x1) {
                cells.fill(true);
            }
        }
    }
    rows.iter().flatten().filter(|cell| **cell).count() as f64
}

/// Where a collaborator's pointer rests over the list, in logical pixels.
const PEER_AT: [f32; 2] = [420.0, 360.0];

/// Scroll the live window down [`SCROLL_FRAMES`] frames of `step` logical
/// pixels each, the pointer over the list, through the host's input and
/// frame path ([`tilo_app::App::surface_input`],
/// [`tilo_app::App::surface_frame_with_damage`]), and present each frame on
/// an offscreen GPU target as a window does: only where it changed, after
/// copying what a scroll keeps on screen when `copying`
/// ([`tilo_app::scroll_copy`]). With `peer`, a collaborator's pointer rests
/// over the list, painted over the widgets as a window paints it
/// ([`tilo_app::app::SurfaceDamage::painting_peers`]).
#[allow(
    clippy::too_many_lines,
    reason = "one measured scroll loop: input, frame, damage, presentation and its parity check"
)]
fn scrolling(app: &mut tilo_app::App, step: f64, copying: bool, peer: bool) -> Scrolling {
    use tilo_app::app::{AppSurface, SurfaceDamage};
    use tilo_app::offscreen::OffscreenRenderer;
    use tilo_app::winit::event::{MouseScrollDelta, TouchPhase};
    let size = Surface.inner_size();
    let background = app.theme.tokens.colors.surfaces.content;
    let mut damage = if copying {
        SurfaceDamage::copying()
    } else {
        SurfaceDamage::default()
    }
    .painting_peers();
    if peer {
        app.presence_ctx_mut()
            .upsert_remote_cursor("ada".into(), PEER_AT, Some("Ada Lovelace"));
    }
    app.init_layout();
    let mut scene = tilo_app::advanced::Scene::new();
    app.surface_input(
        &Surface,
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(640.0, 500.0),
        },
        false,
    );
    for _ in 0..5 {
        scene.reset();
        app.surface_frame_with_damage(&Surface, &mut scene, &mut damage);
    }
    let _ = damage.take();
    let mut gpu = OffscreenRenderer::try_new(size.width, size.height)
        .ok()
        .zip(OffscreenRenderer::try_new(size.width, size.height).ok());
    if let Some((presented, _)) = gpu.as_mut() {
        let _ = presented.render(&scene, background);
    }
    let whole_window = f64::from(size.width) * f64::from(size.height);
    let mut result = Scrolling {
        frames: 0,
        whole: 0,
        why: None,
        pixels_p50: 0.0,
        pixels_max: 0.0,
        copied_p50: 0.0,
        cpu_ms: 0.0,
        gpu_ms: None,
        differing: 0,
    };
    let (mut pixels, mut copied, mut cpu, mut gpu_presented, mut gpu_full) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for _ in 0..SCROLL_FRAMES {
        let start = Instant::now();
        app.surface_input(
            &Surface,
            WindowEvent::MouseWheel {
                device_id: DeviceId::dummy(),
                delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -step)),
                phase: TouchPhase::Moved,
            },
            false,
        );
        scene.reset();
        app.surface_frame_with_damage(&Surface, &mut scene, &mut damage);
        cpu.push(millis(start));
        result.frames = result.frames.saturating_add(1);
        let presented_frame = damage.take_copying();
        pixels.push(presented_frame.as_ref().map_or_else(
            || {
                result.whole = result.whole.saturating_add(1);
                result.why = result.why.or_else(|| damage.repainted());
                whole_window
            },
            |(_, rects)| covered(rects, size),
        ));
        copied.push(presented_frame.as_ref().map_or(0.0, |(copies, _)| {
            copies
                .iter()
                .map(|copy| f64::from(copy.width) * f64::from(copy.height))
                .fold(0.0, |sum, pixels| sum + pixels)
        }));
        if let Some((presented, full)) = gpu.as_mut() {
            let start = Instant::now();
            match &presented_frame {
                Some((moves, rects)) => {
                    let _ = presented
                        .render_copies_and_damage_gpu_wait(&scene, background, moves, rects);
                }
                None => presented.render_gpu_wait(&scene, background),
            }
            gpu_presented.push(millis(start));
            let start = Instant::now();
            full.render_gpu_wait(&scene, background);
            gpu_full.push(millis(start));
        }
    }
    if let Some((presented, full)) = gpu.as_mut() {
        let expected = full.render(&scene, background);
        let actual = presented.readback_current();
        result.differing = OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected);
        result.gpu_ms = Some((median(gpu_presented), median(gpu_full)));
    }
    result.pixels_max = pixels.iter().copied().fold(0.0, f64::max);
    result.pixels_p50 = median(pixels);
    result.copied_p50 = median(copied);
    result.cpu_ms = median(cpu);
    result
}

/// The host's section around an in-process app, as the shell draws a tile:
/// its ground, its clip, the app in a damage part and the tile's edge in
/// bands of its own, tracked for damage parts as the shell's workspace is.
fn host_section(
    tile: &mut tilo_app::app::EmbeddedApp,
    text: &mut tilo_app::text_render::TextRenderer,
) -> (
    Box<tilo_app::advanced::Scene>,
    tilo_app::scene_parts::SectionParts,
) {
    use tilo_app::app::AppSurface as _;
    use tilo_app::kurbo::{Affine, RoundedRect};
    use tilo_app::peniko::{BlendMode, Fill};
    use tilo_app::scene_parts::{PartKey, part};
    use tilo_app::widget::Widget as _;
    let theme = tile.app().theme.clone();
    let size = Surface.inner_size();
    let region = Rect::new(0.0, 0.0, f64::from(size.width), f64::from(size.height));
    let recording = tilo_app::scene_parts::Recording::begin();
    let mut scene = Box::new(tilo_app::advanced::Scene::new());
    tilo_app::scene_parts::track(&scene);
    let shape = RoundedRect::from_rect(region, f64::from(theme.tokens.radii.tile));
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.surfaces.content,
        None,
        &shape,
    );
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &shape,
    );
    part(
        &mut scene,
        PartKey::new("scaling.tile-content", ()),
        vec![region],
        |scene| {
            tile.draw(
                scene,
                Affine::IDENTITY,
                text,
                &theme,
                &tilo_app::presence::PresenceCtx::empty(),
            );
        },
    );
    scene.pop_layer();
    let reach = 16.0;
    let bands = vec![
        Rect::new(region.x0, region.y0, region.x1, region.y0 + reach),
        Rect::new(region.x0, region.y1 - reach, region.x1, region.y1),
        Rect::new(region.x0, region.y0, region.x0 + reach, region.y1),
        Rect::new(region.x1 - reach, region.y0, region.x1, region.y1),
    ];
    part(
        &mut scene,
        PartKey::new("scaling.tile-edge", ()),
        bands,
        |scene| {
            scene.stroke(
                &tilo_app::kurbo::Stroke::new(1.0),
                Affine::IDENTITY,
                theme.window_edge(),
                None,
                &shape,
            );
        },
    );
    let parts = recording.finish().of(&scene).unwrap_or_default();
    (scene, parts)
}

/// [`scrolling`] for the app shown in a region of a host's scene, as the
/// shell shows an in-process app ([`tilo_app::app::EmbeddedApp`]): each
/// frame's section is compared with the last
/// ([`tilo_app::scene_parts::damage_with_copies`]) and presented over it as
/// a host that keeps its target does, copies first
/// ([`tilo_app::scene_parts::present_copies`]), when `copying`.
#[allow(
    clippy::too_many_lines,
    reason = "one measured scroll loop: input, frame, damage, presentation and its parity check"
)]
fn hosted_scrolling(tile: &mut tilo_app::app::EmbeddedApp, step: f64, copying: bool) -> Scrolling {
    use tilo_app::app::AppSurface as _;
    use tilo_app::offscreen::OffscreenRenderer;
    use tilo_app::scene_parts::{DrawBounds, damage, damage_with_copies, present_copies};
    use tilo_app::widget::Widget as _;
    use tilo_app::winit::event::{MouseScrollDelta, TouchPhase};
    let size = Surface.inner_size();
    tile.set_accepts_scroll_copies(copying);
    let mut text = tilo_app::text_render::TextRenderer::new();
    let mut taffy = tilo_app::taffy::TaffyTree::new();
    let theme = tile.app().theme.clone();
    let node = tile.build_layout(&mut taffy, &theme, &mut std::collections::HashMap::new());
    let _ = taffy.compute_layout(
        node,
        tilo_app::taffy::Size {
            width: tilo_app::taffy::AvailableSpace::Definite(tilo_app::num::f32_of_f64(f64::from(
                size.width,
            ))),
            height: tilo_app::taffy::AvailableSpace::Definite(tilo_app::num::f32_of_f64(
                f64::from(size.height),
            )),
        },
    );
    tile.apply_layout(&taffy, node, tilo_app::kurbo::Point::ZERO);
    let background = theme.tokens.colors.surfaces.content;
    tile.input_for_test(WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: PhysicalPosition::new(640.0, 500.0),
    });
    let mut shown = host_section(tile, &mut text);
    for _ in 0..6 {
        tile.input_for_test(WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(640.0, 500.0),
        });
        shown = host_section(tile, &mut text);
    }
    let mut gpu = OffscreenRenderer::try_new(size.width, size.height)
        .ok()
        .zip(OffscreenRenderer::try_new(size.width, size.height).ok());
    if let Some((presented, _)) = gpu.as_mut() {
        let _ = presented.render(&shown.0, background);
    }
    let whole_window = f64::from(size.width) * f64::from(size.height);
    let mut bounds = DrawBounds::default();
    let mut result = Scrolling {
        frames: 0,
        whole: 0,
        why: None,
        pixels_p50: 0.0,
        pixels_max: 0.0,
        copied_p50: 0.0,
        cpu_ms: 0.0,
        gpu_ms: None,
        differing: 0,
    };
    let (mut pixels, mut copied, mut cpu, mut gpu_presented, mut gpu_full) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for _ in 0..SCROLL_FRAMES {
        let start = Instant::now();
        tile.input_for_test(WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -step)),
            phase: TouchPhase::Moved,
        });
        let drawn = host_section(tile, &mut text);
        // The host's comparison and plan are part of the frame's cost.
        let presented_frame = if copying {
            damage_with_copies(&shown.0, &shown.1, &drawn.0, &drawn.1, &mut bounds)
                .ok()
                .map(|section| {
                    let rects = section.rects.iter().map(|rect| rect.expand()).collect();
                    let plan = present_copies(
                        &section.copies,
                        rects,
                        &[],
                        tilo_app::kurbo::Affine::IDENTITY,
                        (size.width, size.height),
                    );
                    (plan.copies, plan.raster)
                })
        } else {
            damage(&shown.0, &shown.1, &drawn.0, &drawn.1)
                .ok()
                .map(|rects| (Vec::new(), rects.iter().map(|rect| rect.expand()).collect()))
        };
        cpu.push(millis(start));
        result.frames = result.frames.saturating_add(1);
        pixels.push(presented_frame.as_ref().map_or_else(
            || {
                result.whole = result.whole.saturating_add(1);
                result.why = result
                    .why
                    .or(Some("the host could not compare the section"));
                whole_window
            },
            |(_, rects)| covered(rects, size),
        ));
        copied.push(presented_frame.as_ref().map_or(0.0, |(copies, _)| {
            copies
                .iter()
                .map(|copy| f64::from(copy.width) * f64::from(copy.height))
                .fold(0.0, |sum, pixels| sum + pixels)
        }));
        if let Some((presented, full)) = gpu.as_mut() {
            let start = Instant::now();
            match &presented_frame {
                Some((moves, rects)) => {
                    let _ = presented
                        .render_copies_and_damage_gpu_wait(&drawn.0, background, moves, rects);
                }
                None => presented.render_gpu_wait(&drawn.0, background),
            }
            gpu_presented.push(millis(start));
            let start = Instant::now();
            full.render_gpu_wait(&drawn.0, background);
            gpu_full.push(millis(start));
        }
        shown = drawn;
    }
    if let Some((presented, full)) = gpu.as_mut() {
        let expected = full.render(&shown.0, background);
        let actual = presented.readback_current();
        result.differing = OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected);
        result.gpu_ms = Some((median(gpu_presented), median(gpu_full)));
    }
    result.pixels_max = pixels.iter().copied().fold(0.0, f64::max);
    result.pixels_p50 = median(pixels);
    result.copied_p50 = median(copied);
    result.cpu_ms = median(cpu);
    result
}

/// How the scrolled list is shown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shown {
    /// In a window of its own.
    Window,
    /// In a window of its own, a collaborator's pointer resting over it.
    BesidePeer,
    /// In a region of a host's scene, as the shell shows an in-process app.
    Hosted,
}

/// The `scroll` mode: what one frame of continuous scrolling costs, at a
/// whole and at a fractional step, in a window of its own and shown in a
/// region of a host's scene as the shell shows an in-process app.
fn scroll_mode(sizes: &[u32]) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "tasks  step  copies  frames  whole  repainted px p50  max  copied px p50  \
         cpu frame p50  gpu presented p50  gpu full p50  differing px  \
         first whole-window reason"
    );
    let mut lines = Vec::new();
    // Whether any run had no GPU to present on, and the most pixels a run
    // presented that a full render does not have.
    let mut checked: Option<(bool, usize)> = None;
    for &tasks in sizes {
        TASKS.store(tasks, Ordering::Relaxed);
        for (shown, step, copying) in [
            (Shown::Window, SCROLL_STEP, false),
            (Shown::Window, SCROLL_STEP, true),
            (Shown::Window, SCROLL_STEP + 0.5, true),
            (Shown::BesidePeer, SCROLL_STEP, false),
            (Shown::BesidePeer, SCROLL_STEP, true),
            (Shown::Hosted, SCROLL_STEP, false),
            (Shown::Hosted, SCROLL_STEP, true),
        ] {
            let s = match shown {
                Shown::Hosted => {
                    drive_live_embedded::<Scaling, _>(|tile| hosted_scrolling(tile, step, copying))?
                }
                Shown::Window | Shown::BesidePeer => drive_live_window::<Scaling, _>(|app| {
                    scrolling(app, step, copying, shown == Shown::BesidePeer)
                })?,
            };
            let (missing_gpu, differing) = checked.unwrap_or_default();
            checked = Some((
                missing_gpu || s.gpu_ms.is_none(),
                differing.max(s.differing),
            ));
            let (gpu_presented, gpu_full) = s.gpu_ms.map_or_else(
                || ("n/a".to_owned(), "n/a".to_owned()),
                |(presented, full)| (format!("{presented:.3} ms"), format!("{full:.3} ms")),
            );
            let copies = match (shown, copying) {
                (Shown::Window, true) => "yes",
                (Shown::Window, false) => "no",
                (Shown::BesidePeer, true) => "peer",
                (Shown::BesidePeer, false) => "peer, none",
                (Shown::Hosted, true) => "hosted",
                (Shown::Hosted, false) => "hosted, none",
            };
            println!(
                "{tasks:>5}  {step:>4}  {copies:>6}  {:>6}  {:>5}  {:>16.0}  {:>7.0}  \
                 {:>13.0}  {:>10.3} ms  {gpu_presented:>17}  {gpu_full:>12}  {:>12}  {}",
                s.frames,
                s.whole,
                s.pixels_p50,
                s.pixels_max,
                s.copied_p50,
                s.cpu_ms,
                s.differing,
                s.why.unwrap_or("-"),
            );
            // For tools/perf/adapters.py.
            let tag = match shown {
                Shown::Window => "FRAMEWORK-SCROLL",
                Shown::BesidePeer => "FRAMEWORK-SCROLL-PEER",
                Shown::Hosted => "FRAMEWORK-SCROLL-HOSTED",
            };
            lines.push(format!(
                "{tag} tasks={tasks} step={step} copying={copying} \
                 pixels_p50={:.0} pixels_max={:.0} copied_p50={:.0} whole={} frames={} \
                 cpu_ms={:.3} gpu_ms={:.3} gpu_full_ms={:.3} differing={}",
                s.pixels_p50,
                s.pixels_max,
                s.copied_p50,
                s.whole,
                s.frames,
                s.cpu_ms,
                s.gpu_ms.map_or(0.0, |(presented, _)| presented),
                s.gpu_ms.map_or(0.0, |(_, full)| full),
                s.differing
            ));
        }
    }
    println!();
    for line in lines {
        println!("{line}");
    }
    // A scroll whose presented pixels cannot be checked, or differ from a
    // full render, is no measurement.
    if checked.is_some_and(|(missing_gpu, differing)| missing_gpu || differing > 0) {
        return Err("scroll frames were not presented as a full render shows them".into());
    }
    Ok(())
}

/// Times the focus-order scope ran in `phases`, and its time in ms.
fn focus_order_phase(phases: &[tilo_app::tilo_resource::profiling::Sample]) -> (usize, f64) {
    phases
        .iter()
        .filter(|phase| phase.phase == "ui.focus_order")
        .fold((0, 0.0), |(calls, ms), phase| {
            let elapsed = u64::try_from(phase.elapsed_ns).unwrap_or(u64::MAX);
            (
                calls.saturating_add(phase.calls),
                ms + std::time::Duration::from_nanos(elapsed).as_secs_f64() * 1e3,
            )
        })
}

/// Focus-order recomputations timed per size, and tasks added.
const FOCUS_SAMPLES: usize = 10;

/// What the keyboard focus order costs in a window over `tasks` tasks, in ms.
struct FocusCost {
    /// Each agent call adding one task, end to end.
    adding: Vec<f64>,
    /// Each focus-order recomputation those calls ran.
    recomputing: Vec<f64>,
    /// Each recomputation of the live window's order.
    native: Vec<f64>,
}

/// Add one task at a time through an agent call, and recompute the live
/// window's focus order, [`FOCUS_SAMPLES`] times each.
fn focus_cost(tasks: u32) -> Result<FocusCost, Box<dyn std::error::Error>> {
    TASKS.store(tasks, Ordering::Relaxed);
    let mut app = checklist_in::<Mounted>(tasks)?;
    let add = serde_json::json!({ "count": 1 });
    // The first call is a warm-up: it settles the record and the layout.
    app.call("seed", add.clone())?;
    let mut add_ms = Vec::new();
    let mut recompute_ms = Vec::new();
    for _ in 0..FOCUS_SAMPLES {
        let start = Instant::now();
        let called = app.call("seed", add.clone());
        add_ms.push(millis(start));
        called?;
        // The harness walks the order again within a call only while
        // something can trap focus, as the live window does between
        // rebuilds: the walk Tab takes over the grown window is timed here.
        let start = Instant::now();
        let order = app.harness().focus_order();
        recompute_ms.push(millis(start));
        if order.is_empty() {
            return Err("the window has no focus order to time".into());
        }
    }
    if std::env::var_os("TILO_PROFILE_RESOURCES").is_some() {
        // One more add, untimed, with every scope it ran: where the call's
        // time goes, slowest scope first. Scopes nest, so they do not add up.
        let (called, mut phases) = observe(|| app.call("seed", add.clone()));
        called?;
        phases.sort_by_key(|phase| std::cmp::Reverse(phase.elapsed_ns));
        for phase in phases {
            let elapsed = u64::try_from(phase.elapsed_ns).unwrap_or(u64::MAX);
            eprintln!(
                "add_item tasks={tasks} {} calls={} ms={:.3}",
                phase.phase,
                phase.calls,
                std::time::Duration::from_nanos(elapsed).as_secs_f64() * 1e3,
            );
        }
    }
    let native_ms = drive_live_window::<Mounted, _>(|app| {
        app.init_layout();
        host_frame(app);
        (0..FOCUS_SAMPLES)
            .map(|_| {
                let ((), phases) = observe(|| app.refresh_focus_order());
                focus_order_phase(&phases)
            })
            .collect::<Vec<_>>()
    })?;
    if native_ms.iter().any(|&(calls, _)| calls != 1) {
        return Err("the live window did not recompute its focus order once per refresh".into());
    }
    Ok(FocusCost {
        adding: add_ms,
        recomputing: recompute_ms,
        native: native_ms.into_iter().map(|(_, ms)| ms).collect(),
    })
}

/// The `focus` mode: what the keyboard focus order costs as the list grows.
fn focus_mode(sizes: &[u32]) -> Result<(), Box<dyn std::error::Error>> {
    println!("tasks  add one task p50  focus order in it p50  live window focus order p50");
    let joined = |samples: &[f64]| {
        samples
            .iter()
            .map(|sample| format!("{sample:.4}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut lines = Vec::new();
    for &tasks in sizes {
        let cost = focus_cost(tasks)?;
        println!(
            "{tasks:>5}  {:>13.3} ms  {:>18.4} ms  {:>24.4} ms",
            median(cost.adding.clone()),
            median(cost.recomputing.clone()),
            median(cost.native.clone()),
        );
        // For tools/perf/adapters.py framework-focus.
        lines.push(format!(
            "FRAMEWORK-FOCUS tasks={tasks} add_ms={} recompute_ms={} native_ms={}",
            joined(&cost.adding),
            joined(&cost.recomputing),
            joined(&cost.native),
        ));
    }
    println!();
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(2);
    let open_only = mode.as_deref() == Some("open");
    let sizes = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "10,250,1000,10000".into())
        .split(',')
        .map(|size| size.trim().parse::<u32>())
        .collect::<Result<Vec<_>, _>>()?;
    if mode.as_deref() == Some("scroll") {
        return scroll_mode(&sizes);
    }
    if mode.as_deref() == Some("focus") {
        return focus_mode(&sizes);
    }
    if std::env::var_os("TILO_SCALING_KEYSTROKES").is_some() {
        println!(
            "tasks  keystroke→frame p50  p99  release→frame p50  \
             publications per keystroke  publishing/keystroke p50"
        );
        for &tasks in &sizes {
            TASKS.store(tasks, Ordering::Relaxed);
            let keys = drive_live_window::<Scaling, _>(keystrokes)??;
            println!(
                "{tasks:>5}  {:>16.3} ms  {:>8.3} ms  {:>14.3} ms  {:>26}  {:>21.3} ms",
                keys.to_frame_ms,
                keys.to_frame_p99_ms,
                keys.release_ms,
                keys.publications,
                keys.publication_ms
            );
        }
        return Ok(());
    }
    if !open_only {
        println!(
            "tasks  resources  publication p50  pointer move p50  publications in {MOVES} moves  \
             typing p50: sync / frame  scrolling p50: sync / frame  keystroke→frame p50  \
             release→frame p50  publications per keystroke  publishing/keystroke p50"
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
             {:>16.3} ms  {:>14.3} ms  {:>26}  {:>25.3} ms",
            keys.to_frame_ms, keys.release_ms, keys.publications, keys.publication_ms
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

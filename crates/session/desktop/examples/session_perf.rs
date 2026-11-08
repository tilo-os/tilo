//! Session performance: the desktop the person lands on, measured with no GPU. The measured surface
//! is the one both hosts show ([`tilo_desktop_surface::DesktopSurface`]); the `arriving` , `apps`
//! and `activities` profiles cover what the hosts put in a window's tab, because they cost
//! different things. Every number comes from the production frame path
//! ([`ui::headless::FrameHarness`]): `build` , `first-frame` (startup), `steady` (forced frames
//! with nothing changed), `typing` , `tick` , `drag` and `idle` frames that find no work. Every
//! phase reports its allocations: a steady frame must not allocate. Output is a table, or
//! `--json PATH` for the budget check in `tools/perf/` .

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::too_many_lines,
    reason = "a benchmark reports to stdout and treats a failed setup as fatal"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use tilo_desktop::resolver::{
    ApplicationCatalogTileResolver, ComponentContextSource, TileRequest, TileResolver,
};
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_desktop_surface::{DesktopSurface, TileSource};
use ui::application::{
    ApplicationCatalog, ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget,
    RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::headless::FrameHarness;
use ui::profiling::{FRAME_BUDGET_US, FrameTiming, PerfReport, StatSummary};
use ui::record::{RecordOwners, RecordStore};
use ui::state::State;
use ui::widget::TileHost;

/// Counts allocations while a phase is being measured. Disabled by default,
/// so setup cost is not attributed to a frame.
struct Counting;

static MEASURING: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

#[global_allocator]
static ALLOCATOR: Counting = Counting;

// SAFETY: every operation delegates to `System` with the caller's own layout
// and pointers; the counters are relaxed atomics that never affect them.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() && MEASURING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(
                u64::try_from(layout.size()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() && MEASURING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(
                u64::try_from(new_size).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        new
    }
}

fn start_allocations() {
    ALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    MEASURING.store(true, Ordering::Relaxed);
}

/// `(calls, bytes)` since [`start_allocations`].
fn stop_allocations() -> (u64, u64) {
    MEASURING.store(false, Ordering::Relaxed);
    (
        ALLOCS.load(Ordering::Relaxed),
        BYTES.load(Ordering::Relaxed),
    )
}

/// Which content a window's tab holds ([`Source`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Profile {
    /// The native session: the client's own buffer covers the window.
    Arriving,
    /// The page: the tab mounts and draws its app.
    Apps,
    /// The floating layer over a desk: three activity cards, one ticking.
    Activities,
}

impl Profile {
    fn name(self) -> &'static str {
        match self {
            Self::Arriving => "arriving",
            Self::Apps => "apps",
            Self::Activities => "activities",
        }
    }
}

struct Host {
    owners: RecordOwners,
    views: Rc<InstanceViewStore>,
    wakes: Vec<ui::wake::WakeHandle>,
}

impl ComponentContextSource for Host {
    fn prepare(
        &mut self,
        definition: &'static ApplicationDefinition,
        request: &TileRequest<'_>,
        target: &LaunchTarget,
    ) -> Result<(HostContext, RestoreInput), ui::launch::LaunchError> {
        let records = self.owners.open(definition.app_id(), "workspace").unwrap();
        let context = HostContext::prepare(
            definition,
            HostContextInputs {
                records,
                views: self.views.clone(),
                device_id: "device".into(),
                instance_id: request.tab.id,
                permissions: ui::permissions::SharedPermissions::new(
                    ui::permissions::PermissionContext::member(
                        "author",
                        ui::permissions::Role::Owner,
                    ),
                ),
                author: ui::permissions::UserId("author".into()),
                action_context: tilo_actions::ActionContext::default(),
            },
            target,
        )?;
        self.wakes.push(context.wake_handle());
        Ok((context, RestoreInput::None))
    }
}

/// What a window's tab shows, built the first time its strip is shown.
struct Source {
    profile: Profile,
    resolver: ApplicationCatalogTileResolver<Host>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    failures: Vec<ui::launch::LaunchError>,
}

impl TileSource for Source {
    fn tile(&mut self, workspace: &Workspace, tab: &Tab) -> TileHost {
        if self.profile == Profile::Arriving {
            return TileHost::new(ui::widget::TileContent::Arriving);
        }
        self.resolver
            .resolve(TileRequest {
                room: workspace.id(),
                tab,
            })
            .mount(&mut self.failures, std::sync::Arc::clone(&self.wake))
    }
}

/// Three workspaces of two strips, two windows each, `tabs` tabs per window: a session someone has
/// used for a while, not an empty one.
fn fixture(
    profile: Profile,
    tabs_per_window: usize,
) -> (
    DesktopTree,
    ApplicationCatalogTileResolver<Host>,
    Vec<RecordStore>,
) {
    let cache = ui::portable_storage::PortableStorage::new("profile", "device").unwrap();
    let mut owners = RecordOwners::portable(cache.clone());
    let views = Rc::new(InstanceViewStore::portable(cache, "workspace").unwrap());
    let mut catalog = ApplicationCatalog::new();
    let definitions = [
        word_processor::definition(),
        chat::definition(),
        notes::definition(),
    ];
    // The host's catalogue holds each app once, as a session's does; a tab names it again without
    // adding it.
    for definition in &definitions {
        catalog.register(definition).unwrap();
    }
    let mut retained = vec![];
    let mut created = std::collections::HashSet::new();
    let mut workspaces = vec![];
    let mut next_tab: u64 = 10;
    for (w, name) in ["Work", "Design", "Reading"].into_iter().enumerate() {
        let mut workspace = Workspace::new(WorkspaceSlot(w as u64 + 1), name);
        let mut strips = vec![];
        for s in 0..2usize {
            let mut groups = vec![];
            for g in 0..2usize {
                let mut tabs = vec![];
                for (which, definition) in definitions.iter().take(tabs_per_window).enumerate() {
                    let id = next_tab;
                    next_tab += 1;
                    if profile == Profile::Apps {
                        let store = owners.open(definition.app_id(), "workspace").unwrap();
                        let checkout = tilo_desktop::records::component_record(
                            definition.app_id(),
                            id,
                            definition.record_kind.clone(),
                        );
                        // Chat's tabs of one workspace share the package's record, so a tab that is
                        // not the first opens what one before it made.
                        if created.insert((definition.app_id().to_string(), checkout.id.clone())) {
                            store.create_record(&checkout).unwrap();
                            store.with(&checkout.id, |doc| match which {
                                0 => word_processor::initialize_empty_document(doc).unwrap(),
                                1 => chat::demo::start(doc, "author", "Author", "AU").unwrap(),
                                _ => notes::read::initialize(doc),
                            });
                        }
                        retained.push(store);
                    }
                    tabs.push(Tab {
                        id,
                        title: format!("{name} {s}.{g}"),
                        tile: TileContent::LocalApp {
                            app_id: definition.app_id().to_string(),
                            label: definition.title.to_string(),
                            note: String::new(),
                        },
                        app_title: None,
                        view: String::new(),
                        pinned: false,
                    });
                }
                groups.push(WindowGroup {
                    id: (w * 4 + s * 2 + g) as u64 + 3,
                    active_tab: next_tab - tabs_per_window as u64,
                    tabs,
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                });
            }
            strips.push(Strip {
                id: StripId((w * 2 + s) as u64 + 2),
                name: format!("Strip {s}"),
                window_groups: groups,
            });
        }
        workspace.strips = strips;
        workspaces.push(workspace);
    }
    let tree = DesktopTree {
        active: workspaces[0].slot,
        workspaces,
        ..Default::default()
    };
    let resolver = ApplicationCatalogTileResolver {
        catalog,
        host: Host {
            owners,
            views,
            wakes: vec![],
        },
    };
    (tree, resolver, retained)
}

/// One measured phase.
#[derive(Default)]
struct Phase {
    wall_us: u64,
    allocs: u64,
    bytes: u64,
    frames: usize,
    report: Option<PerfReport>,
}

impl Phase {
    fn allocs_per_frame(&self) -> f64 {
        if self.frames == 0 {
            return 0.0;
        }
        self.allocs as f64 / self.frames as f64
    }
}

fn report_of(name: &str, samples: Vec<FrameTiming>) -> PerfReport {
    PerfReport::from_samples(name, samples, FRAME_BUDGET_US)
}

fn measure_frames(harness: &mut FrameHarness, name: &str, frames: usize) -> Phase {
    harness.begin_profiling();
    for _ in 0..3 {
        harness.profile_frame();
    }
    harness.clear_profile_samples();
    start_allocations();
    let start = Instant::now();
    for _ in 0..frames {
        harness.profile_frame();
    }
    let wall_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
    let (allocs, bytes) = stop_allocations();
    let samples = harness.take_profile_samples();
    Phase {
        wall_us,
        allocs,
        bytes,
        frames,
        report: Some(report_of(name, samples)),
    }
}

/// Frames while three fingers carry the strip: each frame takes the hand's next report and draws
/// the strip where it has carried it, back and forth across the windows.
fn measure_pan(harness: &mut FrameHarness, name: &str, frames: usize, extent: f64) -> Phase {
    /// Nine pixels of finger travel a frame, the pace of a brisk pan.
    const STEP: f64 = 9.0;
    /// The hand's report as this frame starts, and the frame that draws it.
    fn report_frame(harness: &mut FrameHarness, travel: f64, began: bool) {
        harness
            .root_mut::<DesktopSurface>()
            .carry_strip(travel, began);
        harness.profile_frame();
    }

    let mut travel = 0.0;
    let mut forward = true;
    harness.begin_profiling();
    for frame in 0..3 {
        report_frame(harness, travel, frame == 0);
        travel += 1.0;
    }
    harness.clear_profile_samples();
    start_allocations();
    let start = Instant::now();
    travel = 0.0;
    for frame in 0..frames {
        travel += if forward { STEP } else { -STEP };
        if !(0.0..=extent).contains(&travel) {
            forward = !forward;
            travel = travel.clamp(0.0, extent);
        }
        report_frame(harness, travel, frame == 0);
    }
    let wall_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
    let (allocs, bytes) = stop_allocations();
    let samples = harness.take_profile_samples();
    Phase {
        wall_us,
        allocs,
        bytes,
        frames,
        report: Some(report_of(name, samples)),
    }
}

fn stat(summary: &StatSummary) -> String {
    format!(
        "p50 {:>7.2} ms  p95 {:>7.2} ms  max {:>7.2} ms",
        summary.p50_us as f64 / 1000.0,
        summary.p95_us as f64 / 1000.0,
        summary.max_us as f64 / 1000.0
    )
}

/// Peak resident set in MiB, from `/proc/self/status` (native Linux only).
fn peak_rss_mib() -> Option<f64> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = text.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kib: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib / 1024.0)
}

/// One phase as a budget check reads it.
fn phase_json(phase: &Phase) -> serde_json::Value {
    let Some(report) = &phase.report else {
        return serde_json::Value::Null;
    };
    serde_json::json!({
        "frames": phase.frames,
        "wall_us": phase.wall_us,
        "allocs": phase.allocs,
        "allocs_per_frame": phase.allocs_per_frame(),
        "bytes": phase.bytes,
        "p50_us": report.total.p50_us,
        "p95_us": report.total.p95_us,
        "max_us": report.total.max_us,
        "draw_p95_us": report.draw.p95_us,
        "layout_p95_us": report.build_layout.p95_us + report.apply_layout.p95_us,
    })
}

/// The floating layer's own frames, hosted as both hosts host it: three activity cards docked in
/// the corner, one of them ticking, and a card dragged across the surface. What it measures is the
/// layer alone, so the numbers are the cards' cost rather than the desk's.
fn run_activities(json_path: Option<String>, device: &str) -> Result<(), String> {
    use ui::floating_layer::{Card, Corner, Placement};
    use ui::live_activity::{Control, ControlKind, Kind, Summary};
    use ui::vello::kurbo::{Point, Rect, Vec2};
    use ui::widget::floating_surface::painted_bounds;
    use ui::winit::dpi::PhysicalPosition;
    use ui::winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};

    let placement = Placement {
        work: Rect::new(0.0, 44.0, 1600.0, 1000.0),
        bar: Rect::new(0.0, 0.0, 1600.0, 44.0),
        banners: None,
        readout: None,
    };
    let timer = |value: String| {
        Summary::new(Kind::Timer, "Tea", value)
            .with_id("tea")
            .with_progress(0.5)
            .with_control(Control::new("toggle", "Pause", ControlKind::Primary))
            .with_control(Control::new("close", "End", ControlKind::Close))
    };
    let feed = |seconds: u64| tilo_activities::Feed {
        placement,
        corner: Corner::BottomRight,
        cards: vec![
            Card::new(
                "so.tilo.clocks",
                timer(format!("{}:{:02}", 4 + seconds / 60, seconds % 60)),
            ),
            Card::new(
                "so.tilo.music",
                Summary::new(Kind::Music, "Album", "Track 1").with_id("music"),
            ),
            Card::new(
                "so.tilo.chat",
                Summary::new(Kind::Call, "Ada", "12:04").with_id("call"),
            ),
        ],
        expanded: false,
    };

    // Startup: the surface from nothing, as the session makes it, counted on its own so the harness
    // below is not in it.
    let start = Instant::now();
    let surface = tilo_activities::ActivitiesSurface::new(None);
    let build_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
    let link = surface.link();
    let mut harness = FrameHarness::new(State::new(), Box::new(surface), (1600, 1000));

    // The harness's own first frame (its theme and its empty tree) is not the layer's: it stands
    // before the cards arrive, unmeasured. The first frame counted is the one that draws them.
    harness.begin_profiling();
    harness.profile_frame();
    harness.clear_profile_samples();
    link.publish(feed(0));
    harness.begin_profiling();
    let start = Instant::now();
    harness.profile_frame();
    let first_frame_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
    let first = harness.take_profile_samples();

    // Steady: the three cards stand still.
    let steady = measure_frames(&mut harness, "steady", 120);

    // Ticking: the timer's reading moves on every frame. What the session does between frames (a
    // new feed) is its own cost; the frame counted here is the layer's, and it may touch the card
    // that changed only.
    for seconds in 1..=3 {
        link.publish(feed(seconds));
        harness.profile_frame();
    }
    harness.clear_profile_samples();
    let mut tick = Phase::default();
    let mut tick_violations = 0usize;
    let mut tick_ratio = 0.0f64;
    let start = Instant::now();
    for seconds in 4..124 {
        link.publish(feed(seconds));
        start_allocations();
        harness.profile_frame();
        let (allocs, bytes) = stop_allocations();
        tick.allocs += allocs;
        tick.bytes += bytes;
        tick.frames += 1;
        let shown = link.shown();
        let Some(changed) = shown
            .cards
            .iter()
            .find(|(app, activity, _)| app == "so.tilo.clocks" && activity == "tea")
            .map(|(_, _, rect)| *rect)
        else {
            tick_violations += 1;
            continue;
        };
        let expected = painted_bounds(changed, harness.theme());
        let ground = expected.area();
        tick_ratio = tick_ratio.max(if ground > 0.0 {
            shown.damage.area() / ground
        } else {
            f64::INFINITY
        });
        if shown.damage.is_zero_area() || shown.damage.union(expected) != expected {
            tick_violations += 1;
        }
    }
    tick.wall_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
    tick.report = Some(report_of("tick", harness.take_profile_samples()));

    // Dragging: the top card follows the pointer, 120 frames of it.
    harness.settle_for_capture(ui::time::Duration::from_millis(500));
    let device_id = DeviceId::dummy();
    let grab = link
        .shown()
        .cards
        .first()
        .map(|(_, _, rect)| Point::new(rect.x0 + 24.0, rect.y0 + 24.0))
        .ok_or_else(|| "the layer showed no card to drag".to_string())?;
    harness.send_host_event(&WindowEvent::CursorMoved {
        device_id,
        position: PhysicalPosition::new(grab.x, grab.y),
    });
    harness.send_host_event(&WindowEvent::MouseInput {
        device_id,
        state: ElementState::Pressed,
        button: MouseButton::Left,
    });
    harness.profile_frame();
    let mut drag = Phase::default();
    let mut drag_violations = 0usize;
    let mut drag_ratio = 0.0f64;
    let mut drag_moved = 0usize;
    let width = |shown: &tilo_activities::Shown| shown.cards.first().map(|(_, _, rect)| *rect);
    let start = Instant::now();
    let mut pointer = grab;
    for _ in 0..120 {
        let before = width(&link.shown());
        pointer += Vec2::new(2.0, 1.0);
        // The frame a moved pointer draws is the event's own: a second frame offered behind it
        // finds nothing left to change.
        start_allocations();
        harness.send_host_event(&WindowEvent::CursorMoved {
            device_id,
            position: PhysicalPosition::new(pointer.x, pointer.y),
        });
        let (allocs, bytes) = stop_allocations();
        drag.allocs += allocs;
        drag.bytes += bytes;
        drag.frames += 1;
        let shown = link.shown();
        let Some(after) = width(&shown) else {
            drag_violations += 1;
            continue;
        };
        if before == Some(after) {
            // The press took hold and the pointer is still inside its slop: the card has not moved,
            // so there is nothing to repaint.
            if !shown.damage.is_zero_area() {
                drag_violations += 1;
            }
            continue;
        }
        drag_moved += 1;
        let ground = match before {
            Some(before) => painted_bounds(before, harness.theme())
                .union(painted_bounds(after, harness.theme())),
            None => painted_bounds(after, harness.theme()),
        };
        let area = ground.area();
        drag_ratio = drag_ratio.max(if area > 0.0 {
            shown.damage.area() / area
        } else {
            f64::INFINITY
        });
        if shown.damage.union(ground) != ground {
            drag_violations += 1;
        }
    }
    drag.wall_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
    drag.report = Some(report_of("drag", harness.take_profile_samples()));
    println!(
        "drag card        grabbed at ({:.0}, {:.0}), moved on {drag_moved} of {} frames",
        grab.x, grab.y, drag.frames
    );

    println!("device {device}, profile activities");
    println!("build            {:>8.2} ms", build_us as f64 / 1000.0);
    println!(
        "first frame      {:>8.2} ms",
        first_frame_us as f64 / 1000.0
    );
    for (name, phase, ratio, violations) in [
        ("steady", &steady, 0.0, 0),
        ("tick", &tick, tick_ratio, tick_violations),
        ("drag", &drag, drag_ratio, drag_violations),
    ] {
        println!("{name} ({} frames)", phase.frames);
        if let Some(report) = &phase.report {
            println!("  total         {}", stat(&report.total));
            println!("  draw          {}", stat(&report.draw));
        }
        println!(
            "  allocations  {:.1} per frame   damage ratio {ratio:.2}, \
             {violations} frames touching other ground",
            phase.allocs_per_frame()
        );
    }

    if let Some(path) = json_path {
        let value = serde_json::json!({
            "kind": "session",
            "device": device,
            "profile": "activities",
            "build_us": build_us,
            "first_frame_us": first_frame_us,
            "first_frame_layout_us": first.first().map_or(0, |f| f.build_layout_us + f.apply_layout_us),
            "first_frame_draw_us": first.first().map_or(0, |f| f.draw_us),
            "steady": phase_json(&steady),
            "tick": {
                "damage_ratio": tick_ratio,
                "damage_violations": tick_violations,
                "allocs_per_frame": tick.allocs_per_frame(),
                "body": phase_json(&tick),
            },
            "drag": {
                "damage_ratio": drag_ratio,
                "damage_violations": drag_violations,
                "allocs_per_frame": drag.allocs_per_frame(),
                "body": phase_json(&drag),
            },
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        println!("wrote {path}");
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("session_perf: {message}");
            std::process::ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let mut json_path = None;
    let mut tabs_per_window = 3usize;
    let mut profile = Profile::Arriving;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--json" => json_path = Some(args.next().expect("--json needs a path")),
            "--tabs" => {
                tabs_per_window = args
                    .next()
                    .expect("--tabs needs a count")
                    .parse()
                    .expect("--tabs takes a number");
            }
            "--profile" => {
                profile = match args.next().expect("--profile needs a name").as_str() {
                    "arriving" => Profile::Arriving,
                    "apps" => Profile::Apps,
                    "activities" => Profile::Activities,
                    other => {
                        return Err(format!(
                            "--profile takes arriving, apps or activities, not {other}"
                        ));
                    }
                }
            }
            "--help" | "-h" => {
                println!(
                    "session_perf [--json PATH] [--profile arriving|apps|activities] [--tabs N]\n\
                     Measures the shared desktop surface: build, first frame, steady frames, \
                     typing and idle frames. TILO_PERF_DEVICE labels the machine (opi3b, x86)."
                );
                return Ok(());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let tabs_per_window = tabs_per_window.clamp(1, 3);
    let device = std::env::var("TILO_PERF_DEVICE").unwrap_or_else(|_| "unlabelled".into());
    if profile == Profile::Activities {
        return run_activities(json_path, &device);
    }

    let (tree, resolver, _retained) = fixture(profile, tabs_per_window);
    let wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(|| {});

    // Startup: the surface from its tree, as both hosts make it.
    start_allocations();
    let start = Instant::now();
    let (surface, handle) = DesktopSurface::new(
        tree,
        7,
        Box::new(Source {
            profile,
            resolver,
            wake,
            failures: vec![],
        }),
    );
    let build_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
    let (build_allocs, build_bytes) = stop_allocations();

    // Startup: the first drawn frame. Mounted as its own type so a phase can hand it a frame's
    // worth of input between frames, as a host does.
    let mut harness =
        FrameHarness::for_root(State::new(), surface, (1600, 1000), ui::Theme::dark());
    start_allocations();
    let start = Instant::now();
    harness.begin_profiling();
    harness.profile_frame();
    let first_frame_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
    let (first_allocs, first_bytes) = stop_allocations();
    let first_samples = harness.take_profile_samples();

    // Steady: forced frames with nothing changed (the animation frame).
    let steady = measure_frames(&mut harness, "steady", 120);

    // A strip under three fingers: a report a frame, and the frame drawing where it carried the
    // strip.
    let extent = handle
        .snapshot("g", vec![])
        .desktop_pan
        .map_or(600.0, |pan| pan.max.min(1_000.0));
    let pan = measure_pan(&mut harness, "pan", 120, extent);

    // Interactive: typing into the focused document, where the profile mounts apps at all.
    let mut typing = Phase::default();
    if profile == Profile::Apps {
        harness
            .perform_named("word_processor", &ui::a11y::Action::Focus)
            .ok();
        typing = measure_frames(&mut harness, "typing", 60);
    }

    // Idle: frames offered with nothing to draw. The frames above ran microseconds apart, so the
    // clock barely moved and a window's entrance still counts as moving; settle the animations on
    // the toolkit's clock first, as a capture does, and then no offered frame may find work.
    harness.settle_for_capture(ui::time::Duration::from_millis(2000));
    // Settling moves what the desk shows: a placeholder says where its content comes from once it
    // has stood a while. A host presents that frame as soon as it is due; presenting it here keeps
    // the offers below to the idle loop itself, rather than counting the one frame the settled
    // state is still owed six hundred times.
    for _ in 0..3 {
        if !harness.frame_schedule().needs_redraw {
            break;
        }
        harness.profile_frame();
    }
    let mut idle_frames = 0usize;
    let mut idle_worked = 0usize;
    let start = Instant::now();
    for _ in 0..600 {
        idle_frames += 1;
        if harness.poll_frame().unwrap() {
            idle_worked += 1;
        }
    }
    let idle_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
    drop(handle);

    let total = steady.report.as_ref().unwrap().total;
    let steady_lines = steady
        .report
        .as_ref()
        .map(|report| {
            [
                ("sync", &report.sync),
                ("build_layout", &report.build_layout),
                ("apply_layout", &report.apply_layout),
                ("draw", &report.draw),
                ("total", &report.total),
            ]
            .into_iter()
            .map(|(name, summary)| format!("  {name:<13} {}", stat(summary)))
            .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    println!("device {device}, profile {}", profile.name());
    println!(
        "build            {:>8.2} ms   {} allocs",
        build_us as f64 / 1000.0,
        build_allocs
    );
    println!(
        "first frame      {:>8.2} ms   {} allocs   {}",
        first_frame_us as f64 / 1000.0,
        first_allocs,
        first_samples
            .first()
            .map(|frame| format!(
                "layout {:.2} ms, draw {:.2} ms",
                (frame.build_layout_us + frame.apply_layout_us) as f64 / 1000.0,
                frame.draw_us as f64 / 1000.0
            ))
            .unwrap_or_default()
    );
    println!("steady (120 frames, nothing changed)");
    for line in &steady_lines {
        println!("{line}");
    }
    println!(
        "  allocations  {:.1} per frame   ({:.1} KiB per frame)",
        steady.allocs_per_frame(),
        steady.bytes as f64 / 1024.0 / steady.frames as f64
    );
    if let Some(report) = &pan.report {
        println!("pan (120 frames, three fingers on the strip)");
        println!("  total         {}", stat(&report.total));
        println!("  draw          {}", stat(&report.draw));
        println!("  allocations  {:.1} per frame", pan.allocs_per_frame());
    }
    if let Some(report) = &typing.report {
        println!("typing (60 keys)");
        println!("  total         {}", stat(&report.total));
        println!("  draw          {}", stat(&report.draw));
        println!("  allocations  {:.1} per key", typing.allocs_per_frame());
    }
    println!(
        "idle             {idle_worked} of {idle_frames} frames did work   \
         {:.1} µs per offered frame",
        idle_us as f64 / idle_frames as f64
    );
    if let Some(rss) = peak_rss_mib() {
        println!("peak RSS         {rss:>8.1} MiB");
    }

    if let Some(path) = json_path {
        let first = first_samples.first();
        let value = serde_json::json!({
            "kind": "session",
            "device": device,
            "profile": profile.name(),
            "tabs_per_window": tabs_per_window,
            "build_us": build_us,
            "build_allocs": build_allocs,
            "build_bytes": build_bytes,
            "first_frame_us": first_frame_us,
            "first_frame_allocs": first_allocs,
            "first_frame_bytes": first_bytes,
            "first_frame_layout_us": first.map_or(0, |f| f.build_layout_us + f.apply_layout_us),
            "first_frame_draw_us": first.map_or(0, |f| f.draw_us),
            "steady": phase_json(&steady),
            "pan": phase_json(&pan),
            "typing": phase_json(&typing),
            "idle_offered_frames": idle_frames,
            "idle_working_frames": idle_worked,
            "idle_us_per_frame": idle_us as f64 / idle_frames as f64,
            "peak_rss_mib": peak_rss_mib(),
            "total_p50_us": total.p50_us,
            "total_p95_us": total.p95_us,
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        println!("wrote {path}");
    }
    Ok(())
}

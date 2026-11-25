//! The session's frame path as a person pays it, measured with no display: the tilolang shell the
//! session draws on every screen, over the windows its desktop places in the shell's wells.
//!
//! One profile a process. `arriving` is the shell on a desktop nobody has opened anything on:
//! its first frame, the frames it draws while what it shows moves, a strip carried under a hand,
//! and the frames offered once it rests. `apps` is the same shell with a note window in each
//! place, typed into. `activities` is the floating layer over it: three live activity cards, one
//! ticking, and the strip carried under them.
//!
//! Every number is the production loop's ([`tilo_session::Session::step`]): the session builds
//! its shell from the package the build carries, its services publish what the shell reads, and
//! each frame goes through the same layout, draw, raster and submit a person's screen gets. The
//! frame trace the compositor keeps (`Compositor::record_frames`) is where a frame's phases come
//! from — the session's pass before the frame, the surfaces' own layout and draw (the shell's
//! view among them), the scene and the submit — and a global allocator counts what the frames
//! allocate. `--json PATH` writes the measurement `tools/perf/check_kpi.py` reads against
//! `[devices.x86.session.*]` in `tools/perf/kpi-budgets.toml`.
//!
//! ```text
//! cargo run -p tilo-session --example session_perf -- --profile arriving
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "a benchmark reports to stdout and treats a failed setup as fatal"
)]

use std::alloc::{GlobalAlloc, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tilo_actions::MemberId;
use tilo_compositor::{GestureEvent, InputEvent, OutputConfig};
use tilo_service::{Service as _, Started};
use tilo_session::{Options, Session};
use tilolang_service::ResourceService;
use tilolang_tilo::host::Host;
use ui::headless::fixture_runner_inputs;
use ui::live_activity::{Control, ControlKind, Kind, Published, Summary};

/// The notes app, as a package names it: the app whose windows stand in the shell's places.
const NOTES: &str = "so.tilo.notes";
/// The app whose timer the bar's pill names and the floating layer's cards show.
const CLOCKS: &str = "so.tilo.clocks";
/// The music app's activity, behind the timer's.
const MUSIC: &str = "so.tilo.music";
/// A call's activity, behind the music's.
const CHAT: &str = "so.tilo.chat";
/// The display every profile draws on, as the old measurements did: a desktop screen.
const WIDTH: i32 = 1600;
const HEIGHT: i32 = 1000;
/// One display frame at 60 Hz: the cadence the headless display shows frames at.
const REFRESH: Duration = Duration::from_millis(16);
/// The clock reading a steady frame carries: each frame moves it, so each frame has something to
/// draw, as a clock does once a minute.
const CLOCK_TYPE: &str = "so.tilo.device#Time";
/// The person at the session.
const PERSON: &str = "Pat";
/// The titles of the notes the profiles open: three windows in the apps profile's strip, and six
/// in the activities one, where a strip of them scrolls under the cards.
const TITLES: [&str; 6] = [
    "Groceries",
    "Packing list",
    "Recipes",
    "Reading",
    "Garden",
    "Errands",
];

// -------------------------------------------------------------------------------------------------
// Allocation counting

/// Counts allocations while a phase is being measured. Disabled by default, so setup cost is not
/// attributed to a frame.
struct Counting;

static MEASURING: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

#[global_allocator]
static ALLOCATOR: Counting = Counting;

// SAFETY: every operation delegates to `System` with the caller's own layout and pointers; the
// counters are relaxed atomics that never affect them.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
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

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
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

// -------------------------------------------------------------------------------------------------
// Startup marks

/// The session's startup marks for this process, kept in a file of our own: the shell's build is
/// marked between `shell-build-start` and `shell-build-done`, which is what a person waits for
/// before the shell can draw, apart from the session built around it.
struct Marks {
    path: std::path::PathBuf,
}

impl Marks {
    /// Open this process's marks. The environment is process-global and the tests' rule is not
    /// to set it; a benchmark process names its own marks once, before it starts anything.
    #[allow(
        clippy::disallowed_methods,
        reason = "a benchmark process names its own startup marks before anything runs"
    )]
    fn open() -> Self {
        let path =
            std::env::temp_dir().join(format!("session-perf-startup-{}.json", std::process::id()));
        // SAFETY: this runs before the process starts any thread.
        unsafe {
            std::env::set_var("TILO_STARTUP_METRICS_PATH", &path);
        }
        tilo_startup_metrics::init();
        Self { path }
    }

    /// How long the marks `from` and `to` took apart, in microseconds.
    fn phase_us(&self, from: &str, to: &str) -> Option<u64> {
        let events = tilo_startup_metrics::events();
        let at = |name: &str| {
            events
                .iter()
                .find(|(mark, _)| mark == name)
                .map(|(_, ns)| *ns)
        };
        let (from, to) = (at(from)?, at(to)?);
        Some(u64::try_from(to.saturating_sub(from) / 1_000).unwrap_or(u64::MAX))
    }
}

impl Drop for Marks {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// -------------------------------------------------------------------------------------------------
// The profiles

/// What fills the shell's windows, and so what a frame of the profile costs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Profile {
    /// The shell on a desktop nobody has opened anything on.
    Arriving,
    /// The shell with a note window in each place, typed into.
    Apps,
    /// The floating layer over the shell: three live activity cards, one ticking.
    Activities,
}

impl Profile {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "arriving" => Some(Self::Arriving),
            "apps" => Some(Self::Apps),
            "activities" => Some(Self::Activities),
            _ => None,
        }
    }
}

// -------------------------------------------------------------------------------------------------
// One measured phase

/// One phase of a profile, as the budget check reads it.
#[derive(Default)]
struct Phase {
    wall_us: u64,
    allocs: u64,
    bytes: u64,
    frames: usize,
    painted: usize,
    p50_us: u32,
    p95_us: u32,
    max_us: u32,
    /// The surfaces' own frames (the shell's view's layout, scene and raster among them) plus the
    /// compositor's scene and submit: what a frame draws.
    draw_p95_us: u32,
    layout_p95_us: u32,
}

impl Phase {
    fn allocs_per_frame(&self) -> f64 {
        if self.frames == 0 {
            return 0.0;
        }
        self.allocs as f64 / self.frames as f64
    }

    /// The phase as a budget check reads it.
    fn json(&self) -> Value {
        json!({
            "frames": self.frames,
            "painted": self.painted,
            "wall_us": self.wall_us,
            "allocs": self.allocs,
            "allocs_per_frame": self.allocs_per_frame(),
            "bytes": self.bytes,
            "p50_us": self.p50_us,
            "p95_us": self.p95_us,
            "max_us": self.max_us,
            "draw_p95_us": self.draw_p95_us,
            "layout_p95_us": self.layout_p95_us,
        })
    }
}

/// One frame's whole cost, from the trace's phases: the session's pass before the frame, the
/// surfaces' own layout, draw and raster, the scene, the submit, and the session's share after.
fn frame_cost_us(sample: tilo_frame_metrics::FrameSample) -> u64 {
    u64::from(sample.input_us)
        + u64::from(sample.pump_us)
        + u64::from(sample.layout_us)
        + u64::from(sample.scene_us)
        + u64::from(sample.surfaces_us)
        + u64::from(sample.submit_us)
        + u64::from(sample.testctl_us)
        + u64::from(sample.clients_us)
        + u64::from(sample.after_us)
}

/// The phase the frames `run` drove cost: each iteration hands the frame its input and lets the
/// session's loop draw it.
fn measure(
    session: &mut Session,
    frames: usize,
    mut run: impl FnMut(&mut Session, usize),
) -> Phase {
    session.compositor().record_frames();
    start_allocations();
    let start = Instant::now();
    for frame in 0..frames {
        run(session, frame);
        session.step(REFRESH).expect("a turn of the session");
    }
    let wall_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
    let (allocs, bytes) = stop_allocations();
    let summary = tilo_frame_metrics::summarize(session.compositor().recorded_frames(), 16_667);
    Phase {
        wall_us,
        allocs,
        bytes,
        frames,
        painted: usize::try_from(summary.painted).unwrap_or(usize::MAX),
        p50_us: summary.work.p50,
        p95_us: summary.work.p95,
        max_us: summary.work.max,
        draw_p95_us: summary.surfaces.p95.saturating_add(summary.submit.p95),
        layout_p95_us: summary.layout.p95,
    }
}

/// Three fingers carrying the strip in front, nine pixels of travel a frame, outward and back:
/// the person's own pan, which the session hands to the shell's view, as a touchpad reports it.
fn three_finger_pan(session: &mut Session, frames: usize) -> Phase {
    session
        .compositor()
        .input(InputEvent::Gesture(GestureEvent::SwipeBegin { fingers: 3 }))
        .expect("the fingers land");
    let phase = measure(session, frames, |session, frame| {
        let dx = if frame < frames / 2 { 9.0 } else { -9.0 };
        session
            .compositor()
            .input(InputEvent::Gesture(GestureEvent::SwipeUpdate {
                dx,
                dy: 0.0,
            }))
            .expect("the fingers move");
    });
    session
        .compositor()
        .input(InputEvent::Gesture(GestureEvent::SwipeEnd {
            cancelled: false,
        }))
        .expect("the fingers lift");
    phase
}

/// Step the session's loop until `done` holds, or the wait runs out.
fn until(session: &mut Session, what: &str, mut done: impl FnMut(&mut Session) -> bool) -> bool {
    let deadline = Instant::now().checked_add(Duration::from_secs(30));
    loop {
        if done(session) {
            return true;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            eprintln!(
                "session_perf: gave up waiting for {what}; the shell's refusals: {:?}",
                session.shell_refusals()
            );
            return false;
        }
        session.step(REFRESH).expect("a turn of the session");
    }
}

// -------------------------------------------------------------------------------------------------
// The session, its shell and its apps

/// A headless session drawing the tilolang shell, with `services` beside it.
fn session(services: Vec<Box<dyn tilo_service::Service>>) -> (Session, u64) {
    let start = Instant::now();
    let session = Session::headless(
        Options {
            presentation: fixture_runner_inputs,
            person: PERSON.to_owned(),
            collab: false,
            services,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    (
        session,
        u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX),
    )
}

/// A notebook kept by the notes app, published as the session publishes a workspace's records,
/// holding the notes `titles`, made through the app's own actions: their ids, in order.
fn notebook(titles: &[&str]) -> (ResourceService, Vec<String>) {
    let host = Host::new(tilo_actions::Actor::from(MemberId::person(PERSON)), || None)
        .expect("a tilolang host");
    host.load_builtin(
        include_str!("../../../../apps/tl/discussion/app.tl"),
        include_str!("../../../../apps/tl/discussion/app.ids"),
    )
    .expect("the discussion package");
    host.load_builtin(
        include_str!("../../../../apps/tl/notes/app.tl"),
        include_str!("../../../../apps/tl/notes/app.ids"),
    )
    .expect("the notes package");
    host.root_of(NOTES).expect("the notes app");
    let mut service = ResourceService::new(host, PERSON);
    let book = service
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{NOTES}.Notebook"))
        .expect("the notebook resource");
    let mut made = Vec::new();
    for title in titles {
        let Started::Done(note) = service
            .invoke(book.id.as_str(), "new_note", &Map::new())
            .expect("a new note")
        else {
            panic!("a tilolang action is done once committed");
        };
        // What the action made is named by the id its app names it by: its published resource is
        // that id under the app.
        let id = tilolang_tilo::publish::resource_id(
            NOTES,
            note.resources.first().expect("the note's id"),
        );
        service
            .invoke(
                &id,
                "retitle",
                json!({ "title": title }).as_object().expect("an object"),
            )
            .expect("the note's title");
        made.push(id);
    }
    service.settle();
    (service, made)
}

/// Show `resource` as a window of its own on the shell, as a person placing what they found
/// does: the layout mints the window and its tab on the surface in front, so every note is a
/// window of the shown strip rather than a tab of the one before it.
fn place(session: &mut Session, resource: &str) {
    let arguments: Map<String, Value> = json!({
        "resource": resource,
        "x": 80,
        "y": 60,
        "width": 480,
        "height": 360,
    })
    .as_object()
    .map(Clone::clone)
    .expect("an object");
    session
        .invoke("shell:layout", "place", &arguments)
        .expect("the place is on the shell");
}

/// The window places the workspace in front holds.
fn places(session: &Session) -> usize {
    session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| {
                workspace
                    .strips
                    .iter()
                    .flat_map(|strip| strip.window_groups.iter())
                    .count()
            })
            .unwrap_or_default()
    })
}

/// Open each note as a window of its own, a frame apart, and wait until the shell shows them.
fn open_notes(session: &mut Session, notes: &[String]) {
    for note in notes {
        let before = places(session);
        place(session, note);
        until(session, "the note's window", |session| {
            places(session) > before
        });
        session.step(REFRESH).expect("a turn of the session");
    }
}

/// The editable field holding `value`, as assistive technology names it: the window place it
/// stands in and its node.
fn text_field(session: &Session, value: &str) -> Option<(usize, ui::accesskit::NodeId)> {
    for (place, _, tree) in session.a11y_windows() {
        for (id, node) in tree.nodes {
            if matches!(
                node.role(),
                ui::accesskit::Role::TextInput | ui::accesskit::Role::MultilineTextInput
            ) && node.value() == Some(value)
            {
                return Some((place, id));
            }
        }
    }
    None
}

/// Put the keyboard in the note titled `title`.
fn focus_note(session: &mut Session, title: &str) {
    assert!(
        until(session, "the note's field", |session| {
            text_field(session, title).is_some()
        }),
        "the note's field"
    );
    let (place, field) = text_field(session, title).expect("the note's field");
    assert!(
        session.a11y_perform(place, field, &ui::a11y::Action::Focus),
        "the note takes the keyboard"
    );
    session.step(REFRESH).expect("a turn of the session");
}

// -------------------------------------------------------------------------------------------------
// The shell's floating layer

/// What an activity says, as its card shows it.
fn timer(seconds: u64) -> Summary {
    Summary::new(
        Kind::Timer,
        "Tea",
        format!("{}:{:02}", 4 + seconds / 60, seconds % 60),
    )
    .with_id("tea")
    .with_progress(0.5)
    .with_control(Control::new("toggle", "Pause", ControlKind::Primary))
    .with_control(Control::new("close", "End", ControlKind::Close))
}

/// Show `summary` on the shell as `app` publishes it, floating over the windows.
fn show(session: &Session, app: &str, summary: Summary) {
    let mut published = Published::of(summary);
    published.overlay = true;
    session.live_activities().show(
        app,
        Some(serde_json::to_value(published).expect("an activity is JSON")),
    );
}

// -------------------------------------------------------------------------------------------------
// Damage, from the screen the person sees

/// One frame of the display read back.
struct Shot {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn shot(session: &mut Session) -> Shot {
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("the display is shown");
    Shot {
        width: shot.width,
        height: shot.height,
        rgba: shot.rgba,
    }
}

/// Where the pixels `before` and `after` differ, `(x0, y0, x1, y1)`: what a frame damaged.
fn changed_box(before: &Shot, after: &Shot) -> Option<(u32, u32, u32, u32)> {
    assert_eq!(
        (before.width, before.height),
        (after.width, after.height),
        "the two frames are of one screen"
    );
    let width = usize::try_from(after.width).expect("a display with columns");
    let stride = width * 4;
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for (y, (a, b)) in before
        .rgba
        .chunks_exact(stride)
        .zip(after.rgba.chunks_exact(stride))
        .enumerate()
    {
        if a == b {
            continue;
        }
        for x in 0..width {
            let p = x * 4;
            if a[p..p + 4] == b[p..p + 4] {
                continue;
            }
            let x = u32::try_from(x).expect("a column");
            let y = u32::try_from(y).expect("a row");
            found = Some(found.map_or((x, y, x, y), |(x0, y0, x1, y1)| {
                (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
            }));
        }
    }
    found
}

/// How many pixels differ outside `allowed` (a box grown by `margin`), and how many differ at all.
fn changed_outside(
    before: &Shot,
    after: &Shot,
    allowed: (u32, u32, u32, u32),
    margin: u32,
) -> (usize, usize) {
    let (x0, y0, x1, y1) = allowed;
    let (ax0, ay0) = (x0.saturating_sub(margin), y0.saturating_sub(margin));
    let (ax1, ay1) = (
        x1.saturating_add(margin).min(after.width.saturating_sub(1)),
        y1.saturating_add(margin)
            .min(after.height.saturating_sub(1)),
    );
    let width = usize::try_from(after.width).expect("a display with columns");
    let stride = width * 4;
    let (mut outside, mut changed) = (0usize, 0usize);
    for (y, (a, b)) in before
        .rgba
        .chunks_exact(stride)
        .zip(after.rgba.chunks_exact(stride))
        .enumerate()
    {
        if a == b {
            continue;
        }
        let y = u32::try_from(y).expect("a row");
        for x in 0..width {
            let p = x * 4;
            if a[p..p + 4] == b[p..p + 4] {
                continue;
            }
            changed += 1;
            let x = u32::try_from(x).expect("a column");
            if x < ax0 || x > ax1 || y < ay0 || y > ay1 {
                outside += 1;
            }
        }
    }
    (outside, changed)
}

fn area((x0, y0, x1, y1): (u32, u32, u32, u32)) -> f64 {
    f64::from(x1.saturating_sub(x0).saturating_add(1))
        * f64::from(y1.saturating_sub(y0).saturating_add(1))
}

/// One frame's damage against the ground the phase may touch.
struct Damage {
    violations: usize,
    ratio: f64,
}

/// The frames `run` drove, each compared with the screen before it: a frame may repaint the
/// `ground` (grown by `margin`) and nothing else, and a frame with nothing to draw is a violation.
fn measure_damage(
    session: &mut Session,
    frames: usize,
    ground: (u32, u32, u32, u32),
    margin: u32,
    mut run: impl FnMut(&mut Session, usize),
) -> (Phase, Damage) {
    let mut phase = measure_start(session);
    let mut previous = shot(session);
    let mut damage = Damage {
        violations: 0,
        ratio: 0.0,
    };
    let start = Instant::now();
    for frame in 0..frames {
        run(session, frame);
        session.step(REFRESH).expect("a turn of the session");
        let now = shot(session);
        let (outside, changed) = changed_outside(&previous, &now, ground, margin);
        if let Some(changed) = changed_box(&previous, &now) {
            damage.ratio = damage.ratio.max(area(changed) / area(ground).max(1.0));
        }
        if outside > 0 || changed == 0 {
            damage.violations += 1;
        }
        previous = now;
    }
    let wall_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
    phase.finish(session, frames, wall_us);
    (phase, damage)
}

// -------------------------------------------------------------------------------------------------
// A phase measured while the loop records its frames

/// A phase begun: the trace starts again at the next turn.
fn measure_start(session: &mut Session) -> Phase {
    session.compositor().record_frames();
    start_allocations();
    Phase::default()
}

impl Phase {
    /// Take what the loop recorded since [`measure_start`] and close the phase.
    fn finish(&mut self, session: &mut Session, frames: usize, wall_us: u64) {
        let (allocs, bytes) = stop_allocations();
        let summary = tilo_frame_metrics::summarize(session.compositor().recorded_frames(), 16_667);
        self.wall_us = wall_us;
        self.allocs = allocs;
        self.bytes = bytes;
        self.frames = frames;
        self.painted = usize::try_from(summary.painted).unwrap_or(usize::MAX);
        self.p50_us = summary.work.p50;
        self.p95_us = summary.work.p95;
        self.max_us = summary.work.max;
        self.draw_p95_us = summary.surfaces.p95.saturating_add(summary.submit.p95);
        self.layout_p95_us = summary.layout.p95;
    }
}

// -------------------------------------------------------------------------------------------------
// The profiles

/// The first frame the session presents, and what it cost.
fn first_frame(session: &mut Session) -> (u64, Value) {
    session.compositor().record_frames();
    let start = Instant::now();
    let mut presented = false;
    while !presented {
        session.step(REFRESH).expect("a turn of the session");
        presented = session.compositor().frames_presented() > 0;
        assert!(
            start.elapsed() <= Duration::from_secs(60),
            "the session presented no first frame in a minute"
        );
    }
    let first = *session
        .compositor()
        .recorded_frames()
        .iter()
        .find(|sample| sample.painted)
        .expect("the frame that presented");
    (
        u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX),
        json!({
            // The whole first frame: the session's pass before it (the workspace it settles and
            // the readings it publishes) and what the shell then lays out and draws. The phases
            // beside it say where the time went.
            "first_frame_us": frame_cost_us(first),
            "first_frame_pump_us": first.pump_us,
            "first_frame_layout_us": u64::from(first.layout_us) + u64::from(first.surfaces_us),
            "first_frame_surfaces_us": first.surfaces_us,
            "first_frame_draw_us": u64::from(first.scene_us) + u64::from(first.submit_us),
            "first_frame_scene_us": first.scene_us,
            "first_frame_submit_us": first.submit_us,
        }),
    )
}

/// Run the shell's loop until it has drawn its screen and settled: nothing owed, no motion.
fn settle(session: &mut Session) {
    let mut quiet = 0;
    let mut presented = session.compositor().frames_presented();
    let deadline = Instant::now().checked_add(Duration::from_secs(30));
    // A shell that never rests — a clock's minute, a caret blinking — is given its frames and
    // then measured: the phase that follows counts what it finds, and says so.
    while quiet < 6 && deadline.is_none_or(|deadline| Instant::now() < deadline) {
        session.step(REFRESH).expect("a turn of the session");
        let now = session.compositor().frames_presented();
        if now == presented {
            quiet += 1;
        } else {
            quiet = 0;
            presented = now;
        }
    }
}

/// Frames offered with nothing to do: how many found work, and what one cost.
fn idle(session: &mut Session) -> (usize, usize, f64) {
    session.compositor().record_frames();
    let offered = 600usize;
    let start = Instant::now();
    for _ in 0..offered {
        session.step(Duration::ZERO).expect("an offered turn");
    }
    let wall_us = start.elapsed().as_micros() as f64;
    let painted: Vec<_> = (session.compositor().recorded_frames().iter())
        .filter(|sample| sample.painted)
        .collect();
    // An idle frame that painted says why, so a shell that never rests names what woke it.
    let mut why = std::collections::BTreeMap::<String, usize>::new();
    for sample in &painted {
        let reasons = tilo_frame_metrics::paint_reason::names(sample.paint_reasons);
        let reason = format!(
            "{reasons} {} {}",
            sample.paint_clients, sample.paint_animations
        );
        *why.entry(reason).or_default() += 1;
    }
    for (reason, frames) in why {
        println!("  idle painted {frames} frames for: {reason}");
    }
    (offered, painted.len(), wall_us / offered as f64)
}

/// Peak resident set in MiB, from `/proc/self/status`.
fn peak_rss_mib() -> Option<f64> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = text.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kib: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib / 1024.0)
}

fn stat(summary: (u32, u32, u32)) -> String {
    format!(
        "p50 {:>6.2} ms  p95 {:>6.2} ms  max {:>6.2} ms",
        f64::from(summary.0) / 1000.0,
        f64::from(summary.1) / 1000.0,
        f64::from(summary.2) / 1000.0,
    )
}

/// The `arriving` profile: the shell on a desktop nobody has opened anything on.
fn run_arriving(marks: &Marks, json_path: Option<String>, device: &str) {
    let (mut session, session_build_us) = session(vec![]);
    let build_us = marks
        .phase_us("shell-build-start", "shell-build-done")
        .unwrap_or(session_build_us);
    let (_first_frame_wall, first) = first_frame(&mut session);
    // The shell's bar is up: what a person lands on. Measured from here, not from the empty loop.
    until(&mut session, "the shell's bar", |session| {
        session.shell_control("Notifications").is_some()
    });
    settle(&mut session);

    let link = session.shell_link().expect("the shell's link");
    let steady = measure(&mut session, 120, |_, frame| {
        link.publish_reading(
            "session:time",
            CLOCK_TYPE,
            json!({ "clock": format!("4:{:02} PM", frame % 60) }),
        );
    });

    let pan = three_finger_pan(&mut session, 120);

    settle(&mut session);
    let (idle_offered, idle_working, idle_us) = idle(&mut session);

    println!("device {device}, profile arriving");
    println!("build            {:>8.2} ms", build_us as f64 / 1000.0);
    println!(
        "first frame      {:>8.2} ms   pump {} µs, surfaces {} µs, draw {} µs",
        first["first_frame_us"].as_u64().unwrap_or_default() as f64 / 1000.0,
        first["first_frame_pump_us"],
        first["first_frame_surfaces_us"],
        first["first_frame_draw_us"],
    );
    println!("steady (120 frames, the shell redrawing its bar)");
    println!(
        "  total         {}",
        stat((steady.p50_us, steady.p95_us, steady.max_us))
    );
    println!(
        "  draw p95      {:>6.2} ms",
        f64::from(steady.draw_p95_us) / 1000.0
    );
    println!("  allocations  {:.1} per frame", steady.allocs_per_frame());
    println!("pan (120 frames, a strip carried)");
    println!(
        "  total         {}",
        stat((pan.p50_us, pan.p95_us, pan.max_us))
    );
    println!("  allocations  {:.1} per frame", pan.allocs_per_frame());
    println!(
        "idle             {idle_working} of {idle_offered} frames did work   {idle_us:.1} µs per offered frame"
    );

    if let Some(path) = json_path {
        let mut value = json!({
            "kind": "session",
            "device": device,
            "profile": "arriving",
            "build_us": build_us,
            "session_build_us": session_build_us,
            "steady": steady.json(),
            "pan": pan.json(),
            "idle_offered_frames": idle_offered,
            "idle_working_frames": idle_working,
            "idle_us_per_frame": idle_us,
            "peak_rss_mib": peak_rss_mib(),
            "total_p50_us": steady.p50_us,
            "total_p95_us": steady.p95_us,
        });
        merge(&mut value, &first);
        std::fs::write(&path, serde_json::to_vec_pretty(&value).expect("JSON"))
            .expect("the measurement written");
        println!("wrote {path}");
    }
}

/// The `apps` profile: the shell with a note window in each place, typed into.
fn run_apps(marks: &Marks, json_path: Option<String>, device: &str, tabs: usize) {
    let (notebook, notes) = notebook(&TITLES[..tabs.clamp(1, TITLES.len())]);
    let (mut session, session_build_us) = session(vec![Box::new(notebook)]);
    let build_us = marks
        .phase_us("shell-build-start", "shell-build-done")
        .unwrap_or(session_build_us);
    let (_first_frame_wall, first) = first_frame(&mut session);
    until(&mut session, "the shell's bar", |session| {
        session.shell_control("Notifications").is_some()
    });
    open_notes(&mut session, &notes);
    settle(&mut session);

    let link = session.shell_link().expect("the shell's link");
    let steady = measure(&mut session, 120, |_, frame| {
        link.publish_reading(
            "session:time",
            CLOCK_TYPE,
            json!({ "clock": format!("4:{:02} PM", frame % 60) }),
        );
    });

    let pan = three_finger_pan(&mut session, 120);

    focus_note(&mut session, TITLES[0]);
    let typed = "the wheels turn while the office sleeps and the lights stay on";
    let typing = measure(&mut session, typed.chars().count(), |session, frame| {
        if let Some(letter) = typed.chars().nth(frame) {
            session
                .compositor()
                .type_char(letter)
                .expect("a key of the person's layout");
        }
    });
    // The person leaves the field: no caret blinks in the frames they no longer type in, so an
    // idle frame finds no work at all.
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::Key {
                keycode: 1,
                pressed,
            })
            .expect("Escape");
        session.step(REFRESH).expect("a turn of the session");
    }

    settle(&mut session);
    let (idle_offered, idle_working, idle_us) = idle(&mut session);

    println!("device {device}, profile apps");
    println!("build            {:>8.2} ms", build_us as f64 / 1000.0);
    println!(
        "first frame      {:>8.2} ms   pump {} µs, surfaces {} µs, draw {} µs",
        first["first_frame_us"].as_u64().unwrap_or_default() as f64 / 1000.0,
        first["first_frame_pump_us"],
        first["first_frame_surfaces_us"],
        first["first_frame_draw_us"],
    );
    println!("steady (120 frames, the shell redrawing its bar)");
    println!(
        "  total         {}",
        stat((steady.p50_us, steady.p95_us, steady.max_us))
    );
    println!(
        "  draw p95      {:>6.2} ms",
        f64::from(steady.draw_p95_us) / 1000.0
    );
    println!("  allocations  {:.1} per frame", steady.allocs_per_frame());
    println!("pan (120 frames, a strip carried)");
    println!(
        "  total         {}",
        stat((pan.p50_us, pan.p95_us, pan.max_us))
    );
    println!("typing ({} keys)", typing.frames);
    println!(
        "  total         {}",
        stat((typing.p50_us, typing.p95_us, typing.max_us))
    );
    println!("  allocations  {:.1} per key", typing.allocs_per_frame());
    println!(
        "idle             {idle_working} of {idle_offered} frames did work   {idle_us:.1} µs per offered frame"
    );

    if let Some(path) = json_path {
        let mut value = json!({
            "kind": "session",
            "device": device,
            "profile": "apps",
            "tabs_per_window": notes.len(),
            "build_us": build_us,
            "session_build_us": session_build_us,
            "steady": steady.json(),
            "pan": pan.json(),
            "typing": typing.json(),
            "idle_offered_frames": idle_offered,
            "idle_working_frames": idle_working,
            "idle_us_per_frame": idle_us,
            "peak_rss_mib": peak_rss_mib(),
            "total_p50_us": steady.p50_us,
            "total_p95_us": steady.p95_us,
        });
        merge(&mut value, &first);
        std::fs::write(&path, serde_json::to_vec_pretty(&value).expect("JSON"))
            .expect("the measurement written");
        println!("wrote {path}");
    }
}

/// The `activities` profile: the floating layer over the shell. The cards a live activity shows
/// dock at the shell's corner (the person does not drag them); the thing a person carries over
/// the shell is the strip's windows, so that is what the drag measures.
fn run_activities(marks: &Marks, json_path: Option<String>, device: &str) {
    let (notebook, notes) = notebook(&TITLES[..6]);
    let (mut session, session_build_us) = session(vec![Box::new(notebook)]);
    let build_us = marks
        .phase_us("shell-build-start", "shell-build-done")
        .unwrap_or(session_build_us);
    let (_first_frame_wall, first) = first_frame(&mut session);
    until(&mut session, "the shell's bar", |session| {
        session.shell_control("Notifications").is_some()
    });
    settle(&mut session);

    // The three cards the floating layer shows, one ticking.
    let without = shot(&mut session);
    show(&session, CLOCKS, timer(0));
    show(
        &session,
        MUSIC,
        Summary::new(Kind::Music, "Album", "Track 1").with_id("music"),
    );
    show(
        &session,
        CHAT,
        Summary::new(Kind::Call, "Ada", "12:04").with_id("call"),
    );
    until(&mut session, "the activity cards", |session| {
        session.shell_says("Tea")
    });
    settle(&mut session);
    let with = shot(&mut session);
    let ground = changed_box(&without, &with).expect("the cards stand on the shell");

    // Steady: the cards stand still.
    let steady = measure(&mut session, 120, |_, _| {});

    // Ticking: the timer's reading moves every frame, and only where the cards stand may change.
    let (tick, tick_damage) = measure_damage(&mut session, 120, ground, 8, |session, frame| {
        show(session, CLOCKS, timer(1 + frame as u64));
    });

    // The drag: the strip carried under the cards. The live activity cards dock where the layer
    // puts them and do not move under the pointer; what a person carries over the shell is the
    // strip's windows, so three fingers pan them while the cards stand still.
    settle(&mut session);
    open_notes(&mut session, &notes);
    settle(&mut session);
    let (drag, drag_damage) = measure_strip_drag(&mut session, 120, &TITLES[..6]);

    println!("device {device}, profile activities");
    println!("build            {:>8.2} ms", build_us as f64 / 1000.0);
    println!(
        "first frame      {:>8.2} ms   pump {} µs, surfaces {} µs, draw {} µs",
        first["first_frame_us"].as_u64().unwrap_or_default() as f64 / 1000.0,
        first["first_frame_pump_us"],
        first["first_frame_surfaces_us"],
        first["first_frame_draw_us"],
    );
    println!("steady (120 frames, the cards standing still)");
    println!(
        "  total         {}",
        stat((steady.p50_us, steady.p95_us, steady.max_us))
    );
    println!("  allocations  {:.1} per frame", steady.allocs_per_frame());
    println!("tick (120 frames, the timer's reading moving)");
    println!(
        "  total         {}",
        stat((tick.p50_us, tick.p95_us, tick.max_us))
    );
    println!(
        "  damage        {:.2} of the cards' ground, {} frames touching other ground",
        tick_damage.ratio, tick_damage.violations
    );
    println!("drag (120 frames, a place carried across the shell)");
    println!(
        "  total         {}",
        stat((drag.p50_us, drag.p95_us, drag.max_us))
    );
    println!(
        "  damage        {:.2} of the place's ground, {} frames touching other ground",
        drag_damage.ratio, drag_damage.violations
    );

    if let Some(path) = json_path {
        let mut value = json!({
            "kind": "session",
            "device": device,
            "profile": "activities",
            "build_us": build_us,
            "session_build_us": session_build_us,
            "steady": steady.json(),
            "tick": {
                "damage_ratio": tick_damage.ratio,
                "damage_violations": tick_damage.violations,
                "allocs_per_frame": tick.allocs_per_frame(),
                "body": tick.json(),
            },
            "drag": {
                "damage_ratio": drag_damage.ratio,
                "damage_violations": drag_damage.violations,
                "allocs_per_frame": drag.allocs_per_frame(),
                "body": drag.json(),
            },
        });
        merge(&mut value, &first);
        std::fs::write(&path, serde_json::to_vec_pretty(&value).expect("JSON"))
            .expect("the measurement written");
        println!("wrote {path}");
    }
}

fn merge(value: &mut Value, extra: &Value) {
    if let (Some(map), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            map.insert(key.clone(), value.clone());
        }
    }
}

/// The box the places titled `titles` stand in, as the shell draws them, with a little room for
/// their edges: what a frame that carries the strip may repaint.
fn places_box(session: &Session, titles: &[&str]) -> Option<(u32, u32, u32, u32)> {
    let mut shown: Option<(f64, f64, f64, f64)> = None;
    for title in titles {
        let Some((x, y, width, height)) = session.shell_control(title) else {
            continue;
        };
        let (x0, y0, x1, y1) = (x - 16.0, y - 16.0, x + width + 16.0, y + height + 16.0);
        shown = Some(match shown {
            None => (x0, y0, x1, y1),
            Some((ax, ay, bx, by)) => (ax.min(x0), ay.min(y0), bx.max(x1), by.max(y1)),
        });
    }
    let (last_x, last_y) = (f64::from(WIDTH) - 1.0, f64::from(HEIGHT) - 1.0);
    shown.map(|(x0, y0, x1, y1)| {
        let clamp =
            |value: f64, last: f64| u32::try_from(value.clamp(0.0, last) as i64).unwrap_or(0);
        (
            clamp(x0, last_x),
            clamp(y0, last_y),
            clamp(x1, last_x),
            clamp(y1, last_y),
        )
    })
}

/// Three fingers carrying the strip under the cards: its windows move every frame, and a frame
/// may repaint only where they were and where they are, with the cards standing still.
fn measure_strip_drag(session: &mut Session, frames: usize, titles: &[&str]) -> (Phase, Damage) {
    session
        .compositor()
        .input(InputEvent::Gesture(GestureEvent::SwipeBegin { fingers: 3 }))
        .expect("the fingers land");
    let mut phase = measure_start(session);
    let mut previous = shot(session);
    let mut previous_ground = places_box(session, titles);
    assert!(
        previous_ground.is_some(),
        "a window of the strip stands on the shell to carry"
    );
    let mut damage = Damage {
        violations: 0,
        ratio: 0.0,
    };
    let start = Instant::now();
    for frame in 0..frames {
        let dx = if frame < frames / 2 { 9.0 } else { -9.0 };
        session
            .compositor()
            .input(InputEvent::Gesture(GestureEvent::SwipeUpdate {
                dx,
                dy: 0.0,
            }))
            .expect("the fingers move");
        session.step(REFRESH).expect("a turn of the session");
        let ground_now = places_box(session, titles);
        let now = shot(session);
        if let (Some(before), Some(after)) = (previous_ground, ground_now) {
            let ground = union(before, after, (0, 0), (WIDTH as u32, HEIGHT as u32));
            let (outside, changed) = changed_outside(&previous, &now, ground, 8);
            if let Some(changed) = changed_box(&previous, &now) {
                damage.ratio = damage.ratio.max(area(changed) / area(ground).max(1.0));
            }
            if outside > 0 || changed == 0 {
                damage.violations += 1;
            }
        }
        previous = now;
        previous_ground = ground_now;
    }
    session
        .compositor()
        .input(InputEvent::Gesture(GestureEvent::SwipeEnd {
            cancelled: false,
        }))
        .expect("the fingers lift");
    let wall_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
    phase.finish(session, frames, wall_us);
    (phase, damage)
}

/// `a` and `b` together, with `b` also travelled `travel` the way a drag carries it, kept on
/// `screen`: what the carried windows may repaint.
fn union(
    a: (u32, u32, u32, u32),
    b: (u32, u32, u32, u32),
    travel: (u32, u32),
    screen: (u32, u32),
) -> (u32, u32, u32, u32) {
    let x0 = a.0.min(b.0);
    let y0 = a.1.min(b.1);
    let x1 = a.2.max(b.2.saturating_add(travel.0)).min(screen.0 - 1);
    let y1 = a.3.max(b.3.saturating_add(travel.1)).min(screen.1 - 1);
    (x0, y0, x1, y1)
}

// -------------------------------------------------------------------------------------------------
// Entry

fn main() -> std::process::ExitCode {
    // The marks are opened before anything else runs, so setting the environment races no thread.
    let marks = Marks::open();
    // A session's workers are this same binary in their own modes, so it serves them first.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return std::process::ExitCode::from(code);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = tilo_render_service::dispatch(&args) {
        return code;
    }
    match run(&marks) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("session_perf: {message}");
            std::process::ExitCode::from(2)
        }
    }
}

fn run(marks: &Marks) -> Result<(), String> {
    let mut json_path = None;
    let mut tabs = 3;
    let mut profile = Profile::Arriving;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--json" => json_path = Some(args.next().expect("--json needs a path")),
            "--tabs" => {
                tabs = args
                    .next()
                    .expect("--tabs needs a count")
                    .parse()
                    .expect("--tabs takes a number");
            }
            "--profile" => {
                let name = args.next().expect("--profile needs a name");
                profile = Profile::parse(&name).ok_or_else(|| {
                    format!("--profile takes arriving, apps or activities, not {name}")
                })?;
            }
            "--help" | "-h" => {
                println!(
                    "session_perf [--json PATH] [--profile arriving|apps|activities] [--tabs N]\n\
                     Measures the session drawing the tilolang shell: build, first frame, steady \
                     frames, a strip carried, typing and idle frames. TILO_PERF_DEVICE labels the \
                     machine (opi3b, x86)."
                );
                return Ok(());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let device = std::env::var("TILO_PERF_DEVICE").unwrap_or_else(|_| "unlabelled".into());
    match profile {
        Profile::Arriving => run_arriving(marks, json_path, &device),
        Profile::Apps => run_apps(marks, json_path, &device, tabs),
        Profile::Activities => run_activities(marks, json_path, &device),
    }
    Ok(())
}

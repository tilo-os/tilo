//! Input→display latency of Notes on an owned notebook, through the path a
//! host drives an app it shows.
//!
//! The window is the real Notes window over a real record
//! ([`ui::framework::testing::drive_live_window`]). Its notebook is written
//! with Notes' own actions: 27 notes holding 838 blocks between them
//! (headings, paragraphs, list items), the size of the notebook the Notes
//! review measures. Every input goes through [`ui::app::App::surface_input`],
//! is followed by the host's frame ([`ui::app::App::surface_frame_with_damage`]
//! until the app is at rest, then the accessibility walk the host reads),
//! and that frame is rasterized on an offscreen GPU target where it changed,
//! waiting for the GPU to finish, as a window presents it.
//!
//! For each workload it prints p50, p99 and the largest sample of:
//!
//! * **to frame**: from the input to the end of the host frame that shows
//!   it (app input handling, sync, layout, the scene and the a11y walk);
//! * **to raster**: the same plus the GPU raster of that frame's damage, to
//!   GPU completion. Without a GPU this column is absent;
//!
//! and the p50 of the accessibility walk alone (`a11y`), the part of the
//! frame the host spends reading the app's semantic tree.
//!
//! Workloads: a character typed into the open note's first block, Enter
//! splitting a block and Backspace joining it back (each moves every block
//! below), the wheel scrolling the long note, the pointer moving over it,
//! a host frame with no input at all (`idle`: what a caret blink or another
//! window's animation costs this app), and opening one note after another
//! through the notebook's `open` action (what a search hit or an agent's
//! call does), timed to the frame that shows the note bound and adopted.
//!
//! Every sample is checked through the Notes editor observer: the canonical
//! body and the body the editor draws must agree after each input and the
//! input must have changed it as expected, or the run fails. A missed key
//! never makes the numbers better.
//!
//! It excludes the compositor, the display pipeline and the panel. The
//! installed-desktop measurement from the kernel input event to KMS
//! presentation is `scripts/perf/device_notes_latency.py`
//! (`docs/native-typing-measurement.md`).
//!
//! ```text
//! cargo run --release -p notes --example notes-latency
//! cargo run --release -p notes --example notes-latency -- keys=400 interval=100
//! ```
//!
//! `keys=N` sets the samples per typing workload (default 200), `opens=N` the
//! note opens (default 40), `scrolls=N` the wheel steps, pointer moves and
//! idle frames (default 120 each) and
//! `interval=MS` the idle time between keys, during which the host keeps
//! drawing frames as a display would (default 0: back to back). The last
//! line is `NOTES-LATENCY {json}` with every figure, in milliseconds.

use notes::NotesShell;
use ui::app::{App, AppSurface, SurfaceDamage};
use ui::framework::testing::drive_live_window;
use ui::offscreen::OffscreenRenderer;
use ui::synthetic_input::{cursor_moved, key_input};
use ui::time::Instant;
use ui::winit::event::{ElementState, MouseScrollDelta, TouchPhase, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};
use vello::Scene;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 900;

/// Notes in the notebook, and blocks across them.
const PAGES: usize = 27;
const BLOCKS: usize = 838;
/// Blocks of the long note every typing and scrolling sample runs in; the
/// other notes share the rest.
const LONG_NOTE_BLOCKS: usize = 136;
/// Blocks of each other note.
const SHARE: usize = (BLOCKS - LONG_NOTE_BLOCKS) / (PAGES - 1);
const _: () = assert!(
    LONG_NOTE_BLOCKS + SHARE * (PAGES - 1) == BLOCKS,
    "the other notes share the rest evenly"
);

/// The host frames an input may take to show, before the run fails.
const FRAMES_TO_SHOW: usize = 120;

type Failure = Box<dyn std::error::Error>;

/// The app's window region, at scale 1.
struct Region;

impl AppSurface for Region {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> ui::winit::dpi::PhysicalSize<u32> {
        ui::winit::dpi::PhysicalSize::new(WIDTH, HEIGHT)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: ui::winit::window::CursorIcon) {}
}

/// What the run was asked for.
#[derive(Clone, Copy)]
struct Options {
    keys: usize,
    opens: usize,
    scrolls: usize,
    interval_ms: u64,
}

impl Options {
    fn parse() -> Result<Self, Failure> {
        let mut options = Self {
            keys: 200,
            opens: 40,
            scrolls: 120,
            interval_ms: 0,
        };
        for argument in std::env::args().skip(1) {
            let (name, value) = argument
                .split_once('=')
                .ok_or_else(|| format!("expected name=value, got {argument:?}"))?;
            match name {
                "keys" => options.keys = value.parse()?,
                "opens" => options.opens = value.parse()?,
                "scrolls" => options.scrolls = value.parse()?,
                "interval" => options.interval_ms = value.parse()?,
                _ => return Err(format!("unknown option {name:?}").into()),
            }
        }
        if options.keys == 0 || options.opens == 0 || options.scrolls == 0 {
            return Err("every workload needs at least one sample".into());
        }
        Ok(options)
    }
}

/// The host around the app: its frame, its damage and the GPU target that
/// frame is presented on.
struct Host {
    damage: SurfaceDamage,
    scene: Scene,
    gpu: Option<OffscreenRenderer>,
    /// The accessibility walk of the last frame, in milliseconds.
    walk_ms: f64,
}

impl Host {
    fn new() -> Self {
        Self {
            damage: SurfaceDamage::copying(),
            scene: Scene::new(),
            gpu: OffscreenRenderer::try_new(WIDTH, HEIGHT).ok(),
            walk_ms: 0.0,
        }
    }

    /// One host frame, as `EmbeddedApp` runs it: up to three app frames
    /// until the app is at rest, then the accessibility walk.
    fn frame(&mut self, app: &mut App) {
        for _ in 0..3 {
            self.scene.reset();
            if !app.surface_frame_with_damage(&Region, &mut self.scene, &mut self.damage)
                || !app.unsettled()
            {
                break;
            }
        }
        let walk = Instant::now();
        let _ = app.refresh_semantic_tree();
        self.walk_ms = millis(walk);
    }

    /// Present the last frame on the GPU target where it changed, and wait
    /// for the GPU to finish it.
    fn present(&mut self, app: &App) {
        let background = app.theme.tokens.colors.surfaces.content;
        let presented = self.damage.take_copying();
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        match presented {
            Some((copies, rects)) => {
                if !copies.is_empty() || !rects.is_empty() {
                    let _ = gpu.render_copies_and_damage_gpu_wait(
                        &self.scene,
                        background,
                        &copies,
                        &rects,
                    );
                }
            }
            None => gpu.render_gpu_wait(&self.scene, background),
        }
    }

    /// A host frame and its presentation.
    fn draw(&mut self, app: &mut App) {
        self.frame(app);
        self.present(app);
    }

    /// Draw frames for `millis` of wall time, a frame every 16 ms, with no
    /// input: the idle time between a person's keys.
    fn idle(&mut self, app: &mut App, millis: u64) {
        let Some(until) = Instant::now().checked_add(std::time::Duration::from_millis(millis))
        else {
            return;
        };
        while Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(16));
            self.draw(app);
        }
    }
}

/// One timed input: to the end of its frame, and to its raster.
#[derive(Clone, Copy)]
struct Sample {
    frame_ms: f64,
    raster_ms: Option<f64>,
    /// The accessibility walk within `frame_ms`, in milliseconds.
    walk: f64,
}

/// Send `event` (none: a frame with no input) and draw the frame that shows
/// it, timed.
fn timed(app: &mut App, host: &mut Host, event: Option<WindowEvent>) -> Sample {
    let start = Instant::now();
    if let Some(event) = event {
        app.surface_input(&Region, event, false);
    }
    host.frame(app);
    let frame_ms = millis(start);
    host.present(app);
    Sample {
        frame_ms,
        raster_ms: host.gpu.is_some().then(|| millis(start)),
        walk: host.walk_ms,
    }
}

fn millis(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1e3
}

/// The Notes editor observer: canonical and drawn body, binding, caret.
fn observe(app: &App) -> Result<serde_json::Value, Failure> {
    app.controlled_native_input_snapshot_for_test()
        .ok_or_else(|| "Notes published no editor observation".into())
}

fn chars(observed: &serde_json::Value) -> Result<u64, Failure> {
    observed["body_chars"]
        .as_u64()
        .ok_or_else(|| format!("no canonical body: {observed}").into())
}

/// Whether the editor draws the canonical body of `page`, bound, focused
/// and adopted, with nothing pending.
fn shows(observed: &serde_json::Value, page: &str) -> bool {
    observed["editor_binding_page"] == page
        && observed["main_page"] == page
        && observed["adopted"] == true
        && observed["editor_focused"] == true
        && observed["pending_page_events"] == 0
        && observed["body_hash"].is_string()
        && observed["body_hash"] == observed["rendered_body_hash"]
}

/// The body must be `page`'s, drawn as stored, `expected` characters long.
fn require(app: &App, page: &str, expected: u64, what: &str) -> Result<(), Failure> {
    let observed = observe(app)?;
    if !shows(&observed, page) || chars(&observed)? != expected {
        return Err(format!(
            "{what}: expected {expected} characters drawn as stored in {page}, observed {observed}"
        )
        .into());
    }
    Ok(())
}

/// The Markdown of a note of `blocks` blocks: a heading every ninth block,
/// list items every fourth, paragraphs between.
fn body(note: usize, blocks: usize) -> String {
    use std::fmt::Write as _;
    let mut markdown = String::new();
    for block in 0..blocks {
        let _ = if block % 9 == 0 {
            writeln!(markdown, "## Section {note}.{block}\n")
        } else if block % 4 == 0 {
            writeln!(markdown, "- Item {block} of note {note} with a few words\n")
        } else {
            writeln!(
                markdown,
                "Paragraph {block} of note {note}: {}\n",
                "ordinary notebook prose that wraps across the editor's width. ".repeat(3)
            )
        };
    }
    markdown
}

/// Apply `action` through the window's local action door, as the palette or
/// an agent does.
fn apply(app: &mut App, action: &notes::actions::Action) -> Result<(), Failure> {
    let invoke = ui::tilo_resource::Invoke::<notes::Notebook>::apply(action)?;
    app.invoke_local_resource_action(
        &tilo_actions::ActionAddress::new("notes.notebook", invoke.id),
        &invoke.arguments,
    )
    .map_err(|error| format!("{}: {error:?}", invoke.id))?;
    Ok(())
}

/// Write the notebook: [`PAGES`] notes, [`BLOCKS`] blocks, the first note
/// the long one. Returns each note's id.
fn write_notebook(app: &mut App, host: &mut Host) -> Result<Vec<notes::PageId>, Failure> {
    let mut pages = Vec::new();
    let mut written: usize = 0;
    for note in 0..PAGES {
        let page = notes::PageId::new_v4();
        let blocks = if note == 0 { LONG_NOTE_BLOCKS } else { SHARE };
        apply(
            app,
            &notes::actions::create_page_action(page, None, &format!("Latency note {note}")),
        )?;
        let content = notes::MainPanel::seed_blocks(&body(note, blocks), |_| None);
        written = written.saturating_add(content.len());
        apply(app, &notes::actions::write_body_action(page, &content))?;
        pages.push(page);
        host.draw(app);
    }
    if written != BLOCKS {
        return Err(format!("the notebook holds {written} blocks, not {BLOCKS}").into());
    }
    Ok(pages)
}

/// Open `page` through the notebook's `open` action and draw until the note
/// shows, bound and adopted. Returns the time to that frame.
fn open(app: &mut App, host: &mut Host, page: notes::PageId) -> Result<Sample, Failure> {
    let target = app
        .semantic_tree()
        .iter()
        .find(|node| node.name.as_deref() == Some("Notebook") && node.action("open").is_some())
        .map(|node| node.id.clone())
        .ok_or("the notebook publishes no open action")?;
    let launch = notes::actions::page_resource_id(page);
    let wanted = page.to_string();
    let start = Instant::now();
    if !app.invoke_semantic_action(&target, "open", Some(&launch))? {
        return Err(format!("the notebook refused to open {launch}").into());
    }
    for _ in 0..FRAMES_TO_SHOW {
        host.frame(app);
        if shows(&observe(app)?, &wanted) {
            let frame_ms = millis(start);
            let walk = host.walk_ms;
            host.present(app);
            return Ok(Sample {
                frame_ms,
                raster_ms: host.gpu.is_some().then(|| millis(start)),
                walk,
            });
        }
        host.present(app);
    }
    Err(format!("{launch} never showed: {}", observe(app)?).into())
}

/// Type `keys` characters into the open note, one sample each.
fn typing(
    app: &mut App,
    host: &mut Host,
    page: &str,
    options: &Options,
) -> Result<Vec<Sample>, Failure> {
    let mut length = chars(&observe(app)?)?;
    let mut samples = Vec::new();
    for index in 0..options.keys {
        let key = Key::Character(if index % 6 == 5 { " " } else { "a" }.into());
        samples.push(timed(
            app,
            host,
            Some(key_input(&key, ElementState::Pressed)),
        ));
        app.surface_input(&Region, key_input(&key, ElementState::Released), false);
        host.draw(app);
        length = length.saturating_add(1);
        require(app, page, length, "typing")?;
        host.idle(app, options.interval_ms);
    }
    Ok(samples)
}

/// Split the caret's block with Enter and join it back with Backspace,
/// `keys` times each.
fn enter_and_backspace(
    app: &mut App,
    host: &mut Host,
    page: &str,
    options: &Options,
) -> Result<(Vec<Sample>, Vec<Sample>), Failure> {
    let length = chars(&observe(app)?)?;
    let (mut enters, mut backspaces) = (Vec::new(), Vec::new());
    for _ in 0..options.keys {
        for (key, samples, expected) in [
            (NamedKey::Enter, &mut enters, length.saturating_add(1)),
            (NamedKey::Backspace, &mut backspaces, length),
        ] {
            let key = Key::Named(key);
            samples.push(timed(
                app,
                host,
                Some(key_input(&key, ElementState::Pressed)),
            ));
            app.surface_input(&Region, key_input(&key, ElementState::Released), false);
            host.draw(app);
            require(app, page, expected, "Enter and Backspace")?;
            host.idle(app, options.interval_ms);
        }
    }
    Ok((enters, backspaces))
}

/// Where the editor's top left corner is.
fn editor_origin(app: &App) -> Result<(f64, f64), Failure> {
    observe(app)?
        .get("editor_rect")
        .and_then(serde_json::Value::as_array)
        .and_then(|rect| Some((rect.first()?.as_f64()?, rect.get(1)?.as_f64()?)))
        .ok_or_else(|| "the editor has no bounds".into())
}

/// Scroll the long note down and back up with the wheel, a step a sample.
fn scrolling(app: &mut App, host: &mut Host, options: &Options) -> Result<Vec<Sample>, Failure> {
    let editor = editor_origin(app)?;
    app.surface_input(
        &Region,
        cursor_moved(editor.0 + 80.0, editor.1 + 200.0),
        false,
    );
    host.draw(app);
    let mut samples = Vec::new();
    for step in 0..options.scrolls {
        // Down for the first half, back up for the second.
        let dy = if step < options.scrolls / 2 {
            -40.0
        } else {
            40.0
        };
        samples.push(timed(
            app,
            host,
            Some(WindowEvent::MouseWheel {
                device_id: ui::winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::PixelDelta(ui::winit::dpi::PhysicalPosition::new(0.0, dy)),
                phase: TouchPhase::Moved,
            }),
        ));
    }
    Ok(samples)
}

/// Move the pointer over the long note's text, a few pixels a sample, as a
/// hand resting on the mouse does.
fn pointing(app: &mut App, host: &mut Host, options: &Options) -> Result<Vec<Sample>, Failure> {
    let editor = editor_origin(app)?;
    let mut samples = Vec::new();
    for step in 0..options.scrolls {
        let x = editor.0 + 80.0 + f64::from(u8::try_from(step.checked_rem(7).unwrap_or_default())?);
        samples.push(timed(app, host, Some(cursor_moved(x, editor.1 + 200.0))));
    }
    Ok(samples)
}

/// Host frames with no input: the app is at rest and asked to draw, as when
/// another window animates or a caret blinks.
fn idling(app: &mut App, host: &mut Host, options: &Options) -> Vec<Sample> {
    (0..options.scrolls)
        .map(|_| timed(app, host, None))
        .collect()
}

/// p50, p99 and the largest of `values` (nearest rank), and the values.
fn summary(values: &[f64]) -> serde_json::Value {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = |percent: usize| {
        let at = sorted
            .len()
            .saturating_mul(percent)
            .div_ceil(100)
            .saturating_sub(1);
        sorted.get(at).copied().unwrap_or(0.0)
    };
    serde_json::json!({
        "samples": sorted.len(),
        "p50": rank(50),
        "p99": rank(99),
        "max": sorted.last().copied().unwrap_or(0.0),
        // Every sample, in the order taken, to the microsecond.
        "values": values.iter().map(|value| (value * 1e3).round() / 1e3).collect::<Vec<_>>(),
    })
}

fn report(name: &str, samples: &[Sample]) -> serde_json::Value {
    let frame: Vec<f64> = samples.iter().map(|sample| sample.frame_ms).collect();
    let raster: Vec<f64> = samples
        .iter()
        .filter_map(|sample| sample.raster_ms)
        .collect();
    let walk: Vec<f64> = samples.iter().map(|sample| sample.walk).collect();
    let frame = summary(&frame);
    let raster = (!raster.is_empty()).then(|| summary(&raster));
    let walk = summary(&walk);
    let cell = |figures: Option<&serde_json::Value>| {
        figures.map_or_else(
            || "        -        -        -".to_owned(),
            |figures| {
                format!(
                    "{:>9.2}{:>9.2}{:>9.2}",
                    figures["p50"].as_f64().unwrap_or(0.0),
                    figures["p99"].as_f64().unwrap_or(0.0),
                    figures["max"].as_f64().unwrap_or(0.0),
                )
            },
        )
    };
    println!(
        "{name:<12}{:>8}{}{}{:>9.3}",
        samples.len(),
        cell(Some(&frame)),
        cell(raster.as_ref()),
        walk.get("p50")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
    );
    serde_json::json!({ "to_frame_ms": frame, "to_raster_ms": raster, "a11y_walk_ms": walk })
}

fn run(app: &mut App, options: &Options) -> Result<serde_json::Value, Failure> {
    let mut host = Host::new();
    app.init_layout();
    for _ in 0..5 {
        host.draw(app);
    }
    let pages = write_notebook(app, &mut host)?;
    let (long, other) = match pages.as_slice() {
        [long, other, ..] => (*long, *other),
        _ => return Err("the notebook has fewer than two notes".into()),
    };
    // Warm up: open both notes once, untimed.
    open(app, &mut host, other)?;
    open(app, &mut host, long)?;
    let others = pages.get(1..).unwrap_or_default();
    let mut opens = Vec::new();
    for index in 0..options.opens {
        let page = index
            .checked_rem(others.len())
            .and_then(|at| others.get(at))
            .copied()
            .ok_or("no note to open")?;
        opens.push(open(app, &mut host, page)?);
    }
    open(app, &mut host, long)?;
    let page = long.to_string();
    // The first key moves the window to keyboard modality: warm-up.
    let warm = Options {
        keys: 2,
        ..*options
    };
    typing(app, &mut host, &page, &warm)?;
    let typed = typing(app, &mut host, &page, options)?;
    let (enters, backspaces) = enter_and_backspace(app, &mut host, &page, options)?;
    let scrolled = scrolling(app, &mut host, options)?;
    let pointed = pointing(app, &mut host, options)?;
    let idled = idling(app, &mut host, options);

    println!(
        "Notes input latency, {PAGES} notes / {BLOCKS} blocks, {WIDTH}x{HEIGHT} at scale 1, {} build, {}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        std::env::consts::ARCH,
    );
    println!(
        "{:<12}{:>8}{:>27}{:>27}{:>9}",
        "workload", "samples", "to frame p50/p99/max ms", "to raster p50/p99/max ms", "a11y p50"
    );
    let workloads = serde_json::json!({
        "type": report("type", &typed),
        "enter": report("enter", &enters),
        "backspace": report("backspace", &backspaces),
        "scroll": report("scroll", &scrolled),
        "pointer": report("pointer", &pointed),
        "idle": report("idle", &idled),
        "open": report("open", &opens),
    });
    if host.gpu.is_none() {
        println!("no GPU: raster timings were not taken");
    }
    Ok(serde_json::json!({
        "pages": PAGES,
        "blocks": BLOCKS,
        "long_note_blocks": LONG_NOTE_BLOCKS,
        "viewport": [WIDTH, HEIGHT],
        "scale": 1.0,
        "interval_ms": options.interval_ms,
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "architecture": std::env::consts::ARCH,
        "gpu": host.gpu.is_some(),
        "workloads": workloads,
    }))
}

fn main() -> Result<(), Failure> {
    let options = Options::parse()?;
    ui::i18n::I18nManager::init("en-US");
    let record = drive_live_window::<NotesShell, _>(|app| run(app, &options))??;
    println!("NOTES-LATENCY {record}");
    Ok(())
}

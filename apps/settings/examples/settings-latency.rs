//! Input→frame latency of Settings beside its navigation list, through the
//! path a host drives an app it shows.
//!
//! The window is the real Settings window on a fresh private profile, at
//! 1280×900, scale 1. Every input goes through
//! [`ui::app::App::surface_input`] and is followed by the host's frame
//! ([`ui::app::App::surface_frame_with_damage`] until the app is at rest,
//! then the accessibility walk the host reads), as `notes-latency` drives
//! Notes.
//!
//! The navigation list (every category, with its icon) stands beside the
//! pane through every workload; none of them changes it:
//!
//! * `type`: a character typed into the first text field a pane shows (the
//!   harness opens categories through the list's own `activate` action
//!   until one shows a field other than the window's filter);
//! * `pointer`: the pointer moving a few pixels at a time over the pane;
//! * `idle`: a host frame with no input at all.
//!
//! For each it prints the p50, p99 and largest **to frame** time (from the
//! input to the end of the host frame that shows it: input handling, sync,
//! layout, the scene and the accessibility walk) and the p50 of the
//! accessibility walk alone (`a11y`). The walk is what a keystroke beside a
//! navigation list pays for the list's rows.
//!
//! ```text
//! cargo run --release -p settings --example settings-latency
//! cargo run --release -p settings --example settings-latency -- keys=400 moves=200
//! ```
//!
//! The last line is `SETTINGS-LATENCY {json}` with every figure, in
//! milliseconds.

use settings::SettingsShell;
use ui::a11y::Role;
use ui::app::{App, AppSurface, SurfaceDamage};
use ui::framework::testing::drive_live_window;
use ui::synthetic_input::{cursor_moved, key_input, mouse_press, mouse_release};
use ui::time::Instant;
use ui::winit::event::{ElementState, WindowEvent};
use ui::winit::keyboard::Key;
use vello::Scene;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 900;

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
    moves: usize,
}

impl Options {
    fn parse() -> Result<Self, Failure> {
        let mut options = Self {
            keys: 300,
            moves: 300,
        };
        for argument in std::env::args().skip(1) {
            let (name, value) = argument
                .split_once('=')
                .ok_or_else(|| format!("expected name=value, got {argument:?}"))?;
            match name {
                "keys" => options.keys = value.parse()?,
                "moves" => options.moves = value.parse()?,
                _ => return Err(format!("unknown option {name:?}").into()),
            }
        }
        if options.keys == 0 || options.moves == 0 {
            return Err("every workload needs at least one sample".into());
        }
        Ok(options)
    }
}

/// The host around the app: its frame and its damage.
struct Host {
    damage: SurfaceDamage,
    scene: Scene,
    /// The accessibility walk of the last frame, in milliseconds.
    walk_ms: f64,
}

impl Host {
    fn new() -> Self {
        Self {
            damage: SurfaceDamage::copying(),
            scene: Scene::new(),
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
        let _ = self.damage.take_copying();
        let walk = Instant::now();
        let _ = app.refresh_semantic_tree();
        self.walk_ms = millis(walk);
    }
}

/// One timed input: to the end of its frame, and the walk within it.
#[derive(Clone, Copy)]
struct Sample {
    frame_ms: f64,
    walk: f64,
}

fn timed(app: &mut App, host: &mut Host, event: Option<WindowEvent>) -> Sample {
    let start = Instant::now();
    if let Some(event) = event {
        app.surface_input(&Region, event, false);
    }
    host.frame(app);
    Sample {
        frame_ms: millis(start),
        walk: host.walk_ms,
    }
}

fn millis(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1e3
}

/// The navigation list's rows, in reading order: their ids and names.
fn navigation_rows(app: &App) -> Vec<(ui::a11y_tree::SemanticId, String)> {
    app.semantic_tree()
        .iter()
        .filter(|node| node.role == Role::TreeItem && node.action("activate").is_some())
        .filter_map(|node| Some((node.id.clone(), node.name.clone()?)))
        .collect()
}

/// A text field the open pane shows, other than the window's filter.
fn pane_field(app: &App) -> Option<(ui::a11y_tree::SemanticId, String, vello::kurbo::Rect)> {
    app.semantic_tree()
        .iter()
        .filter(|node| node.role == Role::TextInput && !node.state.disabled)
        .filter(|node| !node.state.protected && !node.state.read_only)
        .filter(|node| node.action("focus").is_some())
        // On screen, in the pane: not the window's filter or search field
        // above the navigation list.
        .filter(|node| {
            node.bounds.is_some_and(|bounds| {
                bounds.x0 >= 300.0 && bounds.y0 >= 0.0 && bounds.y1 <= f64::from(HEIGHT)
            })
        })
        .filter(|node| {
            !node
                .name
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .contains("search")
        })
        .find_map(|node| {
            Some((
                node.id.clone(),
                node.name.clone().unwrap_or_default(),
                node.bounds?,
            ))
        })
}

/// Open categories through the navigation list's `activate` until a pane
/// shows a text field; focus it. Returns the category and the field.
fn open_a_field(app: &mut App, host: &mut Host) -> Result<(String, String), Failure> {
    let rows = navigation_rows(app);
    if rows.is_empty() {
        return Err("the navigation list publishes no rows".into());
    }
    let mut seen = Vec::new();
    for (_, category) in rows {
        // Ids are per frame: look the row up again after each switch.
        let Some((row, _)) = navigation_rows(app)
            .into_iter()
            .find(|(_, name)| *name == category)
        else {
            continue;
        };
        app.invoke_semantic_action(&row, "activate", None)?;
        // A pane may load its content off the frame: give it a moment.
        let opened = Instant::now();
        let mut found = None;
        while found.is_none() && opened.elapsed() < std::time::Duration::from_secs(2) {
            host.frame(app);
            found = pane_field(app);
            if found.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        if let Some((field, name, bounds)) = found {
            app.invoke_semantic_action(&field, "focus", None)?;
            host.frame(app);
            // And a click into it, as a person focuses a field.
            let centre = bounds.center();
            app.surface_input(&Region, cursor_moved(centre.x, centre.y), false);
            app.surface_input(&Region, mouse_press(), false);
            app.surface_input(&Region, mouse_release(), false);
            for _ in 0..4 {
                host.frame(app);
            }
            return Ok((category, name));
        }
        seen.push(format!("{category}: {:?}", text_fields(app)));
    }
    Err(format!("no category shows a text field: {seen:#?}").into())
}

/// The focused field's value, as the semantic tree publishes it.
fn focused_value(app: &App) -> Option<String> {
    app.semantic_tree()
        .iter()
        .find(|node| node.role == Role::TextInput && node.state.focused)
        .map(|node| node.value.clone().unwrap_or_default())
}

/// Every text field published, for a failure to say what it saw.
fn text_fields(app: &App) -> Vec<String> {
    app.semantic_tree()
        .iter()
        .filter(|node| node.role == Role::TextInput)
        .map(|node| {
            format!(
                "{:?}={:?} focused={} bounds={:?}",
                node.name, node.value, node.state.focused, node.bounds
            )
        })
        .collect()
}

/// Type `keys` characters into the focused field, one sample each.
fn typing(app: &mut App, host: &mut Host, keys: usize) -> Result<Vec<Sample>, Failure> {
    let before = focused_value(app)
        .ok_or_else(|| format!("no field has focus; fields: {:?}", text_fields(app)))?;
    let mut samples = Vec::with_capacity(keys);
    for index in 0..keys {
        let key = Key::Character(if index % 6 == 5 { " " } else { "a" }.into());
        samples.push(timed(
            app,
            host,
            Some(key_input(&key, ElementState::Pressed)),
        ));
        app.surface_input(&Region, key_input(&key, ElementState::Released), false);
        host.frame(app);
    }
    let after = focused_value(app).ok_or("the field lost focus while typing")?;
    let typed = after.chars().count().saturating_sub(before.chars().count());
    if typed != keys {
        let fields = text_fields(app);
        return Err(format!(
            "typed {keys} keys, the field took {typed}: {after:?}; fields: {fields:?}"
        )
        .into());
    }
    Ok(samples)
}

/// Move the pointer over the pane, a few pixels a sample.
fn pointing(app: &mut App, host: &mut Host, moves: usize) -> Result<Vec<Sample>, Failure> {
    let mut samples = Vec::with_capacity(moves);
    for step in 0..moves {
        let x = 900.0 + f64::from(u8::try_from(step % 7)?);
        samples.push(timed(app, host, Some(cursor_moved(x, 500.0))));
    }
    Ok(samples)
}

fn idling(app: &mut App, host: &mut Host, frames: usize) -> Vec<Sample> {
    (0..frames).map(|_| timed(app, host, None)).collect()
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
        "values": values.iter().map(|value| (value * 1e3).round() / 1e3).collect::<Vec<_>>(),
    })
}

fn report(name: &str, samples: &[Sample]) -> serde_json::Value {
    let frame: Vec<f64> = samples.iter().map(|sample| sample.frame_ms).collect();
    let walk: Vec<f64> = samples.iter().map(|sample| sample.walk).collect();
    let frame = summary(&frame);
    let walk = summary(&walk);
    let figure = |figures: &serde_json::Value, at: &str| figures[at].as_f64().unwrap_or(0.0);
    println!(
        "{name:<10}{:>8}{:>9.3}{:>9.3}{:>9.3}{:>10.3}",
        samples.len(),
        figure(&frame, "p50"),
        figure(&frame, "p99"),
        figure(&frame, "max"),
        figure(&walk, "p50"),
    );
    serde_json::json!({ "to_frame_ms": frame, "a11y_walk_ms": walk })
}

fn run(app: &mut App, options: Options) -> Result<serde_json::Value, Failure> {
    let mut host = Host::new();
    app.init_layout();
    for _ in 0..5 {
        host.frame(app);
    }
    let rows = navigation_rows(app).len();
    let (category, field) = open_a_field(app, &mut host)?;
    // The first keys move the window to keyboard modality: warm-up.
    typing(app, &mut host, 4)?;
    let typed = typing(app, &mut host, options.keys)?;
    let pointed = pointing(app, &mut host, options.moves)?;
    let idled = idling(app, &mut host, options.moves);

    println!(
        "Settings input latency beside a {rows}-row navigation list, typing into {field:?} in {category:?}, {WIDTH}x{HEIGHT} at scale 1, {} build, {}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        std::env::consts::ARCH,
    );
    println!(
        "{:<10}{:>8}{:>27}{:>10}",
        "workload", "samples", "to frame p50/p99/max ms", "a11y p50"
    );
    let workloads = serde_json::json!({
        "type": report("type", &typed),
        "pointer": report("pointer", &pointed),
        "idle": report("idle", &idled),
    });
    Ok(serde_json::json!({
        "navigation_rows": rows,
        "category": category,
        "field": field,
        "viewport": [WIDTH, HEIGHT],
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "architecture": std::env::consts::ARCH,
        "workloads": workloads,
    }))
}

fn main() -> Result<(), Failure> {
    let options = Options::parse()?;
    ui::i18n::I18nManager::init("en-US");
    let record = drive_live_window::<SettingsShell, _>(|app| run(app, options))??;
    println!("SETTINGS-LATENCY {record}");
    Ok(())
}

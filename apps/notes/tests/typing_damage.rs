//! What typing in a note repaints when Notes runs as the desktop runs an app
//! in a tile (`ui::app::EmbeddedApp`): the real window, its real record and
//! saver, driven through `App::surface_input` and painted by
//! `App::surface_frame_with_damage` into a scene the host compares part by
//! part (`ui::scene_parts`), exactly as the shell does.
//!
//! A keystroke repaints the line it edits, its caret and what else changed
//! (the save status bubble), not the app; naming a note repaints its title,
//! not the page; a pointer resting on a toolbar button repaints the button.
//! Every frame is presented sparsely over the frame before it and the result
//! compared with a full render of the same scene, in dark, light and high
//! contrast.

use notes::NotesShell;
use ui::app::{App, AppSurface, SurfaceDamage};
use ui::framework::testing::drive_live_window;
use ui::synthetic_input::{cursor_moved, key_input, mouse_press, mouse_release};
use ui::theme::Theme;
use ui::winit::event::ElementState;
use ui::winit::keyboard::{Key, NamedKey};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 900;
/// Every pixel of the app.
const PIXELS: u64 = WIDTH as u64 * HEIGHT as u64;
/// One row of the app's RGBA pixels.
const ROW_BYTES: usize = WIDTH as usize * 4;

/// A theme to check in, by name.
type ThemeCase = (&'static str, fn() -> Theme);

const THEMES: [ThemeCase; 3] = [
    ("dark", Theme::dark),
    ("light", Theme::light),
    ("high contrast", Theme::high_contrast),
];

/// The app's region of the host's scene, in logical pixels.
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
    fn hosts_tab_chords(&self) -> bool {
        true
    }
}

/// One frame the host presented: what it painted, and where it differs from
/// the frame before it, or why it could not say (so everywhere).
type Presented = (Scene, Result<Vec<Rect>, String>);

/// The host's side of the app, as `EmbeddedApp` and the shell run it: the
/// app paints into a scene of its own with its damage parts, the host
/// appends that as a part hinted with the app's damage, and compares each
/// host frame with the one before it.
struct Host {
    damage: SurfaceDamage,
    painted: Scene,
    painted_parts: Option<ui::scene_parts::SectionParts>,
    generation: u64,
    scene: Scene,
    parts: Option<ui::scene_parts::SectionParts>,
    /// The host frames since the last look.
    presented: Vec<Presented>,
}

impl Host {
    fn new() -> Self {
        Self {
            damage: SurfaceDamage::default(),
            painted: Scene::new(),
            painted_parts: None,
            generation: 0,
            scene: Scene::new(),
            parts: None,
            presented: Vec::new(),
        }
    }

    /// One host frame, as `EmbeddedApp::draw` runs it: up to three app
    /// frames until the app is at rest, the app's scene appended in a part
    /// hinted with its damage, then the accessibility tree the host's walk
    /// reads.
    fn frame(&mut self, app: &mut App) {
        for _ in 0..3 {
            let (again, parts) = ui::scene_parts::record(&mut self.painted, |painted| {
                painted.reset();
                app.surface_frame_with_damage(&Region, painted, &mut self.damage)
            });
            self.painted_parts = parts;
            if !again || !app.unsettled() {
                break;
            }
        }
        let from = self.generation;
        self.generation = from.wrapping_add(1);
        let hint = self.damage.take().map(|rects| ui::scene_parts::DamageHint {
            source: 1,
            from,
            to: self.generation,
            placement: Affine::IDENTITY,
            rects: rects.iter().map(|rect| rect.inflate(1.0, 1.0)).collect(),
        });
        let mut scene = Scene::new();
        let ((), parts) = ui::scene_parts::record(&mut scene, |scene| {
            let key = ui::scene_parts::PartKey::new("ui.embedded-app", ());
            ui::scene_parts::hinted(scene, key, hint, |scene| {
                ui::scene_parts::append(
                    scene,
                    &self.painted,
                    self.painted_parts.as_ref(),
                    Some(Affine::IDENTITY),
                );
            });
        });
        let damage = match (&self.parts, &parts) {
            (Some(old), Some(new)) => ui::scene_parts::damage(&self.scene, old, &scene, new)
                .map_err(|unproven| {
                    format!("{unproven:?} (the app: {:?})", self.damage.repainted())
                }),
            _ => Err("no frame before".to_owned()),
        };
        self.presented.push((scene.clone(), damage));
        self.scene = scene;
        self.parts = parts;
        let _ = app.refresh_semantic_tree();
    }

    fn frames(&mut self, app: &mut App, count: usize) {
        for _ in 0..count {
            self.frame(app);
        }
    }

    fn input(&mut self, app: &mut App, event: ui::winit::event::WindowEvent) {
        app.surface_input(&Region, event, false);
        self.frame(app);
    }

    fn key(&mut self, app: &mut App, key: &Key) {
        self.input(app, key_input(key, ElementState::Pressed));
        self.input(app, key_input(key, ElementState::Released));
    }

    fn click(&mut self, app: &mut App, at: Point) {
        self.input(app, cursor_moved(at.x, at.y));
        self.input(app, mouse_press());
        self.input(app, mouse_release());
    }
}

/// The bounds of the first published node named `name`.
fn bounds_of(app: &App, name: &str) -> Rect {
    app.semantic_tree()
        .iter()
        .find(|node| node.name.as_deref() == Some(name))
        .and_then(|node| node.bounds)
        .unwrap_or_else(|| panic!("no {name:?} on screen"))
}

/// The values of the published nodes named `name`.
fn values_of(app: &App, name: &str) -> Vec<String> {
    app.semantic_tree()
        .iter()
        .filter(|node| node.name.as_deref() == Some(name))
        .filter_map(|node| node.value.clone())
        .collect()
}

/// Whether the save status surface says the note is saved.
fn shows_saved(app: &App) -> bool {
    app.semantic_tree().iter().any(|node| {
        node.name
            .as_deref()
            .is_some_and(|name| name.starts_with("Saved"))
    })
}

/// The pixels of the app `rects` cover, each counted once: what a host
/// repaints for them.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "rects are clamped to the 1280x900 app before becoming pixel indices"
)]
fn area(rects: &[Rect]) -> f64 {
    let app = Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT));
    let mut covered = vec![vec![false; WIDTH as usize]; HEIGHT as usize];
    for rect in rects {
        let rect = rect.intersect(app).expand();
        for row in &mut covered[rect.y0 as usize..rect.y1 as usize] {
            row[rect.x0 as usize..rect.x1 as usize].fill(true);
        }
    }
    covered.iter().flatten().filter(|cell| **cell).count() as f64
}

fn app_area() -> f64 {
    f64::from(WIDTH) * f64::from(HEIGHT)
}

/// What a change cost the host: the pixels its frames repainted, summed,
/// and those of its largest frame.
#[derive(Clone, Copy, Default)]
struct Repainted {
    total: f64,
    largest: f64,
    /// A frame could not say where it changed and repainted the app whole.
    whole: bool,
    /// The largest frame's pixels within the region a test watches
    /// ([`measure_within`]).
    largest_within: f64,
}

/// Present `frames` over `before` as the host does, each only where its
/// damage says, and require the pixels of a full render of the last one.
fn present_and_compare(
    before: &Scene,
    frames: &[Presented],
    theme: &Theme,
    label: &str,
    within: Option<Rect>,
) -> Repainted {
    let background = theme.tokens.colors.surfaces.content;
    let mut retained = ui::offscreen::OffscreenRenderer::new(WIDTH, HEIGHT);
    let mut full = ui::offscreen::OffscreenRenderer::new(WIDTH, HEIGHT);
    let _ = retained.render(before, background);
    let mut repainted = Repainted::default();
    for (scene, damage) in frames {
        let damage = match damage {
            Ok(damage) => damage,
            // Counted as the whole app, which every budget refuses; drawn
            // whole, so the frames after it are still compared.
            Err(why) => {
                eprintln!("{label}: a frame repainted the app whole: {why}");
                let _ = retained.render(scene, background);
                repainted.total += app_area();
                repainted.largest = app_area();
                repainted.largest_within = within.map_or_else(app_area, |region| region.area());
                repainted.whole = true;
                continue;
            }
        };
        if damage.is_empty() {
            continue;
        }
        let rasterized = retained.render_damage_regions_gpu_wait(scene, background, damage);
        let pixels = area(damage);
        // Damage over half the app renders whole, as a host does (a header
        // that grew moves the page under it); less must render sparsely.
        assert!(
            rasterized < PIXELS || pixels >= app_area() / 2.0,
            "{label}: the damage must exercise a sparse render: {damage:?}"
        );
        repainted.total += pixels;
        repainted.largest = repainted.largest.max(pixels);
        if let Some(region) = within {
            let inside: Vec<_> = damage.iter().map(|rect| rect.intersect(region)).collect();
            repainted.largest_within = repainted.largest_within.max(area(&inside));
        }
    }
    let Some((last, _)) = frames.last() else {
        return repainted;
    };
    let expected = full.render(last, background);
    let actual = retained.readback_current();
    let mut differing = 0_usize;
    let mut spread: Option<Rect> = None;
    let stride = ROW_BYTES;
    for (y, (actual, expected)) in actual
        .chunks_exact(stride)
        .zip(expected.chunks_exact(stride))
        .enumerate()
    {
        for (x, (a, e)) in actual
            .chunks_exact(4)
            .zip(expected.chunks_exact(4))
            .enumerate()
        {
            if a.iter().zip(e).any(|(a, e)| a.abs_diff(*e) > 1) {
                differing = differing.saturating_add(1);
                let (x, y) = (
                    f64::from(u32::try_from(x).unwrap_or(u32::MAX)),
                    f64::from(u32::try_from(y).unwrap_or(u32::MAX)),
                );
                let pixel = Rect::new(x, y, x + 1.0, y + 1.0);
                spread = Some(spread.map_or(pixel, |rect| rect.union(pixel)));
            }
        }
    }
    assert_eq!(
        differing, 0,
        "{label}: the sparse repaint must equal a full render ({differing} pixels differ within {spread:?})"
    );
    repainted
}

/// What `change` repainted, in pixels, checked against a full render, and
/// how long it took from its first input to the end of the host frame that
/// shows it.
fn measure(
    app: &mut App,
    host: &mut Host,
    theme: &Theme,
    label: &str,
    change: impl FnOnce(&mut App, &mut Host),
) -> (Repainted, std::time::Duration) {
    let before = host.scene.clone();
    host.presented.clear();
    let started = std::time::Instant::now();
    change(app, host);
    let elapsed = started.elapsed();
    let frames = std::mem::take(&mut host.presented);
    (
        present_and_compare(&before, &frames, theme, label, None),
        elapsed,
    )
}

/// [`measure`], also counting what the change repainted within `region`.
fn measure_within(
    app: &mut App,
    host: &mut Host,
    (theme, label): (&Theme, &str),
    region: Rect,
    change: impl FnOnce(&mut App, &mut Host),
) -> (Repainted, std::time::Duration) {
    let before = host.scene.clone();
    host.presented.clear();
    let started = std::time::Instant::now();
    change(app, host);
    let elapsed = started.elapsed();
    let frames = std::mem::take(&mut host.presented);
    let repainted = present_and_compare(&before, &frames, theme, label, Some(region));
    (repainted, elapsed)
}

/// Begin a test: these share the toolkit's frozen wall clock (one test's
/// minute turning over would change another's "Edited" line), so they run
/// one at a time.
fn start() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ui::i18n::I18nManager::init("en-US");
    serial
}

/// Pin the wall clock `seconds` after the epoch, so when a note says it was
/// created and edited is the test's to choose.
fn pin_wall(seconds: u64) {
    ui::time::freeze_wall(
        ui::time::UNIX_EPOCH
            .checked_add(ui::time::Duration::from_secs(seconds))
            .unwrap(),
    );
}

/// The note's "Created … · Edited …" line.
fn dates_line(app: &App) -> Vec<String> {
    app.semantic_tree()
        .iter()
        .filter_map(|node| node.name.clone())
        .filter(|name| name.starts_with("Created"))
        .collect()
}

/// Open a new note in a fresh window, with the caret in its body, at a
/// pinned wall clock.
fn open_a_new_note(app: &mut App, host: &mut Host, theme: Theme) {
    pin_wall(1_790_000_000);
    app.set_theme(theme);
    app.init_layout();
    host.frames(app, 5);
    let first = bounds_of(app, "New note");
    host.click(app, first.center());
    // The pointer rests in the empty page below the note, so no button's
    // tooltip comes and goes while a test measures the note.
    host.input(app, cursor_moved(640.0, 700.0));
    host.frames(app, 5);
}

/// Let the note's record finish saving and show it, with no input, as it
/// does between a person's keystrokes.
fn let_the_save_land(app: &mut App, host: &mut Host) {
    for _ in 0..80 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        host.frame(app);
        if shows_saved(app) {
            return;
        }
    }
    panic!("the note's record never finished saving");
}

/// Let the window come to rest with no input: the save bubble shown for the
/// last save fades away, so what a test measures next is its own change.
fn let_it_rest(app: &mut App, host: &mut Host) {
    let mut quiet = 0_u32;
    for _ in 0..400 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        host.presented.clear();
        host.frame(app);
        let still = host
            .presented
            .iter()
            .all(|(_, damage)| damage.as_ref().is_ok_and(Vec::is_empty));
        quiet = if still { quiet.saturating_add(1) } else { 0 };
        if quiet >= 8 {
            host.presented.clear();
            return;
        }
    }
    panic!("the window never came to rest");
}

fn character(text: &str) -> Key {
    Key::Character(text.into())
}

/// Type `text` key by key, measuring nothing.
fn type_text(app: &mut App, host: &mut Host, text: &str) {
    for letter in text.chars() {
        host.key(app, &character(&letter.to_string()));
    }
}

/// Type each of `letters` after the previous key's save landed, as a person
/// types: each key and each landing measured, checked against a full render
/// and logged as `case`. Returns the largest frame and the median
/// keystroke→frame time.
fn measure_typing(
    app: &mut App,
    host: &mut Host,
    theme: &Theme,
    case: &str,
    letters: &[&str],
) -> (f64, std::time::Duration) {
    let mut worst: f64 = 0.0;
    let mut times = Vec::new();
    for letter in letters {
        let label = format!("{case}: the save landing before {letter:?}");
        let (landed, _) = measure(app, host, theme, &label, let_the_save_land);
        let label = format!("{case}: typing {letter:?}");
        let (typed, elapsed) = measure(app, host, theme, &label, |app, host| {
            host.key(app, &character(letter));
        });
        eprintln!(
            "notes-damage {case} key={letter:?} frame_px={:.0} save_landing_px={:.0} of {:.0} keystroke_to_frame={elapsed:?}",
            typed.largest,
            landed.largest,
            app_area(),
        );
        assert!(typed.total > 0.0, "{label}: the key changed the note");
        worst = worst.max(typed.largest).max(landed.largest);
        times.push(elapsed);
    }
    times.sort();
    let median = times[times.len() / 2];
    eprintln!(
        "notes-damage {case} worst_frame_px={worst:.0} ({:.2} % of the app) p50_keystroke_to_frame={median:?}",
        worst / app_area() * 100.0
    );
    (worst, median)
}

/// The bounds of the published node whose value is `value`.
fn bounds_of_value(app: &App, value: &str) -> Rect {
    app.semantic_tree()
        .iter()
        .find(|node| node.value.as_deref() == Some(value))
        .and_then(|node| node.bounds)
        .unwrap_or_else(|| panic!("no node showing {value:?} on screen"))
}

/// Whether a published node's value is `value`.
fn shows_value(app: &App, value: &str) -> bool {
    app.semantic_tree()
        .iter()
        .any(|node| node.value.as_deref() == Some(value))
}

/// Whether a published node is named `name`.
fn shows_name(app: &App, name: &str) -> bool {
    app.semantic_tree()
        .iter()
        .any(|node| node.name.as_deref() == Some(name))
}

/// Whether a published node's name or value mentions `text`.
fn mentions(app: &App, text: &str) -> bool {
    app.semantic_tree().iter().any(|node| {
        [&node.name, &node.value]
            .into_iter()
            .flatten()
            .any(|said| said.contains(text))
    })
}

/// The bounds of the first published node whose action target ends with
/// `suffix` and starts with `prefix`.
fn bounds_of_target(app: &App, prefix: &str, suffix: &str) -> Rect {
    app.semantic_tree()
        .iter()
        .find(|node| {
            node.semantic_target
                .as_deref()
                .is_some_and(|target| target.starts_with(prefix) && target.ends_with(suffix))
        })
        .and_then(|node| node.bounds)
        .unwrap_or_else(|| panic!("no {prefix}…{suffix} on screen"))
}

/// The ids (from `kind`, as `notes.page:…`) of the published resources of
/// `kind` named `name`.
fn resource_ids(app: &App, kind: &str, name: &str) -> Vec<String> {
    let marker = format!(":{kind}:");
    app.semantic_tree()
        .iter()
        .filter(|node| node.name.as_deref() == Some(name))
        .filter_map(|node| {
            let start = node.id.0.find(&marker)?;
            Some(node.id.0[start..].strip_prefix(':')?.to_owned())
        })
        .collect()
}

/// The id of the one published resource of `kind` named `name`.
fn resource_id(app: &App, kind: &str, name: &str) -> String {
    let ids = resource_ids(app, kind, name);
    assert_eq!(ids.len(), 1, "one {kind} named {name:?}: {ids:?}");
    ids[0].clone()
}

/// Invoke `action` on `resource` as the app's person does from the palette.
fn invoke(app: &mut App, resource: &str, action: &str, value: Option<&str>) {
    let mut arguments = serde_json::Map::new();
    if let Some(value) = value {
        arguments.insert(tilo_actions::PROMPTED_PARAM.into(), value.into());
    }
    app.invoke_local_resource_action(
        &tilo_actions::ActionAddress::new(resource, action),
        &arguments,
    )
    .unwrap_or_else(|error| panic!("{resource} {action}: {error:?}"));
}

#[test]
fn a_keystroke_in_a_note_damages_its_line_not_the_app() {
    let _serial = start();
    for (name, theme) in THEMES {
        let theme = theme();
        drive_live_window::<NotesShell, _>(|app| {
            let mut host = Host::new();
            open_a_new_note(app, &mut host, theme.clone());
            let mut worst: f64 = 0.0;
            let mut times = Vec::new();
            for letter in ["T", "h", "e", " ", "f", "o", "x"] {
                // The save of the previous key lands between keys.
                let label = format!("{name}: the save landing before {letter:?}");
                let (landed, _) = measure(app, &mut host, &theme, &label, let_the_save_land);
                let label = format!("{name}: typing {letter:?}");
                let (typed, elapsed) = measure(app, &mut host, &theme, &label, |app, host| {
                    host.key(app, &character(letter));
                });
                eprintln!(
                    "notes-typing-damage {name} key={letter:?} frame_px={:.0} key_px={:.0} save_landing_px={:.0} of {:.0} keystroke_to_frame={elapsed:?}",
                    typed.largest,
                    typed.total,
                    landed.largest,
                    app_area(),
                );
                assert!(typed.total > 0.0, "{label}: the key changed the note");
                worst = worst.max(typed.largest).max(landed.largest);
                times.push(elapsed);
            }
            assert!(
                values_of(app, "Block text").contains(&"The fox".to_owned()),
                "{name}: the keys reached the note: {:?}",
                values_of(app, "Block text")
            );
            // A key a minute later changes the note's "Edited" time: the
            // header's line, not its widgets (rebuilding them repaints the
            // app).
            let label = format!("{name}: the edit time turning over a minute");
            let (_, _) = measure(app, &mut host, &theme, &label, let_the_save_land);
            let dates = dates_line(app);
            pin_wall(1_790_000_120);
            let (typed, _) = measure(app, &mut host, &theme, &label, |app, host| {
                host.key(app, &character("!"));
            });
            assert_ne!(dates, dates_line(app), "{label}: the line changed");
            eprintln!(
                "notes-typing-damage {name} minute_turnover frame_px={:.0}",
                typed.largest
            );
            // The line, the bubble and the header's line.
            assert!(
                typed.largest < app_area() * 0.1,
                "{label}: repainted {:.0} px, over 10 % of the app",
                typed.largest
            );
            times.sort();
            eprintln!(
                "notes-typing-damage {name} worst_frame_px={worst:.0} ({:.2} % of the app) p50_keystroke_to_frame={:?}",
                worst / app_area() * 100.0,
                times[times.len() / 2],
            );
            // A line, its caret and the save bubble: not the app.
            assert!(
                worst < app_area() * 0.06,
                "{name}: a frame of typing repainted {worst:.0} px, over 6 % of the app"
            );
        })
        .unwrap();
    }
}

#[test]
fn naming_a_note_damages_its_title_not_the_page() {
    let _serial = start();
    for (name, theme) in THEMES {
        let theme = theme();
        drive_live_window::<NotesShell, _>(|app| {
            let mut host = Host::new();
            open_a_new_note(app, &mut host, theme.clone());
            let title = bounds_of(app, "Page title");
            host.click(app, title.center());
            host.frames(app, 3);
            let mut worst: f64 = 0.0;
            for letter in ["P", "l", "a", "n"] {
                let label = format!("{name}: naming the note, {letter:?}");
                let (typed, _) = measure(app, &mut host, &theme, &label, |app, host| {
                    host.key(app, &character(letter));
                });
                eprintln!(
                    "notes-title-damage {name} key={letter:?} frame_px={:.0} of {:.0}",
                    typed.largest,
                    app_area(),
                );
                worst = worst.max(typed.largest);
            }
            host.key(app, &Key::Named(NamedKey::Enter));
            host.frames(app, 3);
            assert!(
                values_of(app, "Page title")
                    .iter()
                    .any(|title| title.ends_with("Plan")),
                "{name}: the keys named the note: {:?}",
                values_of(app, "Page title")
            );
            // The title, not the page.
            assert!(
                worst < app_area() * 0.08,
                "{name}: naming the note repainted {worst:.0} px, over 8 % of the app"
            );
        })
        .unwrap();
    }
}

#[test]
fn resting_on_a_toolbar_button_damages_the_button_not_the_app() {
    let _serial = start();
    let theme = Theme::dark();
    drive_live_window::<NotesShell, _>(|app| {
        let mut host = Host::new();
        open_a_new_note(app, &mut host, theme.clone());
        let search = bounds_of(app, "Search");
        host.input(app, cursor_moved(640.0, 600.0));
        host.frames(app, 2);
        let (hovered, _) = measure(app, &mut host, &theme, "hovering Search", |app, host| {
            host.input(app, cursor_moved(search.center().x, search.center().y));
        });
        eprintln!(
            "notes-hover-damage frame_px={:.0} of {:.0}",
            hovered.largest,
            app_area()
        );
        assert!(
            hovered.total < app_area() * 0.005,
            "hovering a toolbar button repainted {:.0} px",
            hovered.total
        );
    })
    .unwrap();
}

#[test]
fn a_keystroke_in_a_code_block_damages_its_run_not_the_editor() {
    let _serial = start();
    for (name, theme) in THEMES {
        let theme = theme();
        drive_live_window::<NotesShell, _>(|app| {
            let mut host = Host::new();
            open_a_new_note(app, &mut host, theme.clone());
            // A few lines of prose, so the editor is more than its code.
            for line in ["Intro", "Some prose", "More prose", "Then code"] {
                type_text(app, &mut host, line);
                host.key(app, &Key::Named(NamedKey::Enter));
            }
            // Three backticks open a code block; Enter ends its language.
            type_text(app, &mut host, "```rust");
            host.key(app, &Key::Named(NamedKey::Enter));
            host.frames(app, 3);
            // The first line of a run also paints the run's ground.
            let case = format!("{name} code-block-first-line");
            let (first, _) = measure_typing(app, &mut host, &theme, &case, &["l", "e", "t"]);
            host.key(app, &Key::Named(NamedKey::Enter));
            host.frames(app, 3);
            let case = format!("{name} code-block-later-line");
            let (later, _) = measure_typing(app, &mut host, &theme, &case, &["x", "+", "1"]);
            assert!(
                mentions(app, "let") && mentions(app, "x+1"),
                "{name}: the keys reached the code block"
            );
            // A note that holds a code block types its prose as any note.
            host.click(app, bounds_of_value(app, "Intro").center());
            host.frames(app, 3);
            let case = format!("{name} paragraph-beside-code");
            let (prose, _) = measure_typing(app, &mut host, &theme, &case, &["!"]);
            for (case, worst) in [
                ("the run's first line", first),
                ("a later line", later),
                ("a paragraph beside the code", prose),
            ] {
                // The line or the run's rows, the caret and the save bubble:
                // not the editor.
                assert!(
                    worst < app_area() * 0.06,
                    "{name}: typing in {case} repainted {worst:.0} px, over 6 % of the app"
                );
            }
        })
        .unwrap();
    }
}

#[test]
fn a_keystroke_in_a_table_cell_damages_its_cell_not_the_editor() {
    let _serial = start();
    for (name, theme) in THEMES {
        let theme = theme();
        drive_live_window::<NotesShell, _>(|app| {
            let mut host = Host::new();
            open_a_new_note(app, &mut host, theme.clone());
            type_text(app, &mut host, "/table");
            host.key(app, &Key::Named(NamedKey::Enter));
            host.frames(app, 5);
            // Into the table's first body cell.
            host.click(
                app,
                bounds_of_target(app, "notes.table-cell:", ":1:0").center(),
            );
            host.frames(app, 3);
            let case = format!("{name} table-cell");
            let (cell, _) =
                measure_typing(app, &mut host, &theme, &case, &["4", "2", " ", "k", "g"]);
            assert!(
                shows_value(app, "42 kg"),
                "{name}: the keys reached the cell"
            );
            // The cell, its editor and the save bubble: not the editor.
            assert!(
                cell < app_area() * 0.06,
                "{name}: typing in a table cell repainted {cell:.0} px, over 6 % of the app"
            );
        })
        .unwrap();
    }
}

#[test]
fn tagging_a_note_damages_its_header_not_the_app() {
    let _serial = start();
    for (name, theme) in THEMES {
        let theme = theme();
        drive_live_window::<NotesShell, _>(|app| {
            let mut host = Host::new();
            open_a_new_note(app, &mut host, theme.clone());
            type_text(app, &mut host, "Plan");
            let_the_save_land(app, &mut host);
            host.frames(app, 3);
            let page = resource_id(app, "notes.page", "Untitled");
            let tag = |name: &str| {
                let page = page.clone();
                let name = name.to_owned();
                move |app: &mut App, host: &mut Host| {
                    invoke(app, &page, "new-and-assign-tag", Some(&name));
                    host.frames(app, 6);
                }
            };
            // The first tag adds the header's row of tags: the note's body
            // moves down under it.
            let label = format!("{name}: the first tag");
            let (first, _) = measure(app, &mut host, &theme, &label, tag("work"));
            assert!(shows_name(app, "Tag: work"), "{label}: the chip shows");
            let_the_save_land(app, &mut host);
            // A second tag joins the row: the header keeps its height.
            let label = format!("{name}: a second tag");
            let (second, _) = measure(app, &mut host, &theme, &label, tag("home"));
            assert!(shows_name(app, "Tag: home"), "{label}: the chip shows");
            let_the_save_land(app, &mut host);
            let work = resource_id(app, "notes.tag", "work");
            let work = work.trim_start_matches("notes.tag:");
            let label = format!("{name}: removing a tag");
            let (removed, _) = measure(app, &mut host, &theme, &label, |app, host| {
                invoke(app, &page, &format!("unassign-tag:{work}"), None);
                host.frames(app, 6);
            });
            assert!(!shows_name(app, "Tag: work"), "{label}: the chip went");
            for (case, repainted) in [
                ("the first tag", first),
                ("a second tag", second),
                ("removing a tag", removed),
            ] {
                eprintln!(
                    "notes-damage {name} tag case={case:?} frame_px={:.0} total_px={:.0} of {:.0}",
                    repainted.largest,
                    repainted.total,
                    app_area()
                );
                // The header rebuilt its widgets; the app's tree with them,
                // but the frame still says what changed.
                assert!(!repainted.whole, "{name}: {case} repainted the app whole");
            }
            // The row of tags and the bubble, not the page.
            assert!(
                second.largest < app_area() * 0.1,
                "{name}: a second tag repainted {:.0} px, over 10 % of the app",
                second.largest
            );
        })
        .unwrap();
    }
}

/// Write `lines` as the note's paragraphs, the caret ending in the last.
fn write_paragraphs(app: &mut App, host: &mut Host, lines: &[&str]) {
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            host.key(app, &Key::Named(NamedKey::Enter));
        }
        type_text(app, host, line);
    }
    host.frames(app, 3);
}

/// Put the caret in the paragraph showing `text`, `back` characters before
/// its end.
fn caret_in(app: &mut App, host: &mut Host, text: &str, back: usize) {
    host.click(app, bounds_of_value(app, text).center());
    // The pointer leaves the text, so no hover comes and goes meanwhile.
    host.input(app, cursor_moved(640.0, 860.0));
    host.key(app, &Key::Named(NamedKey::End));
    for _ in 0..back {
        host.key(app, &Key::Named(NamedKey::ArrowLeft));
    }
    host.frames(app, 3);
}

/// Press `key` after the previous edit's save landed, as a person does
/// between edits: what the key repainted, checked against a full render and
/// logged as `case`, and its keystroke→frame time.
fn measure_key(
    app: &mut App,
    host: &mut Host,
    (theme, case): (&Theme, &str),
    key: &Key,
) -> (Repainted, std::time::Duration) {
    let_the_save_land(app, host);
    let_it_rest(app, host);
    let (pressed, elapsed) = measure(app, host, theme, case, |app, host| host.key(app, key));
    eprintln!(
        "notes-damage {case} frame_px={:.0} total_px={:.0} whole={} of {:.0} keystroke_to_frame={elapsed:?}",
        pressed.largest,
        pressed.total,
        pressed.whole,
        app_area(),
    );
    (pressed, elapsed)
}

/// A key that adds or removes a block: the block, the rows of the blocks
/// below it that moved (where they were and are), the caret's drag handle and
/// the save bubble; never the window, nor the page.
fn assert_block_change(name: &str, case: &str, repainted: Repainted) {
    assert!(!repainted.whole, "{name}: {case} repainted the app whole");
    assert!(
        repainted.largest < app_area() * 0.25,
        "{name}: {case} repainted {:.0} px, over 25 % of the app",
        repainted.largest
    );
}

#[test]
fn enter_that_adds_a_block_damages_the_blocks_below_not_the_window() {
    let _serial = start();
    for (name, theme) in THEMES {
        let theme = theme();
        drive_live_window::<NotesShell, _>(|app| {
            let mut host = Host::new();
            open_a_new_note(app, &mut host, theme.clone());
            write_paragraphs(
                app,
                &mut host,
                &["Alpha line", "Bravo line", "Charlie line", "Delta line"],
            );
            // Splitting a paragraph: "Bravo " stays, "line" moves down.
            caret_in(app, &mut host, "Bravo line", "line".len());
            let label = format!("{name} enter-split-paragraph");
            let (split, _) = measure_key(
                app,
                &mut host,
                (&theme, &label),
                &Key::Named(NamedKey::Enter),
            );
            // The caret went with "line" to the start of its new paragraph.
            type_text(app, &mut host, "a ");
            host.frames(app, 3);
            assert!(
                values_of(app, "Block text").contains(&"a line".to_owned()),
                "{label}: the paragraph split, the caret at the new one's start: {:?}",
                values_of(app, "Block text")
            );
            // A new empty paragraph after one, as a person ends a line.
            caret_in(app, &mut host, "Charlie line", 0);
            let label = format!("{name} enter-new-paragraph");
            let (new, _) = measure_key(
                app,
                &mut host,
                (&theme, &label),
                &Key::Named(NamedKey::Enter),
            );
            // A list item after one: "- " starts a list.
            caret_in(app, &mut host, "Alpha line", 0);
            host.key(app, &Key::Named(NamedKey::Enter));
            type_text(app, &mut host, "- milk");
            host.frames(app, 3);
            let label = format!("{name} enter-list-item");
            let (item, _) = measure_key(
                app,
                &mut host,
                (&theme, &label),
                &Key::Named(NamedKey::Enter),
            );
            type_text(app, &mut host, "eggs");
            host.frames(app, 3);
            assert!(
                mentions(app, "milk") && mentions(app, "eggs"),
                "{label}: the list has both items"
            );
            for (case, repainted) in [
                ("splitting a paragraph", split),
                ("ending a paragraph", new),
                ("adding a list item", item),
            ] {
                assert_block_change(name, case, repainted);
            }
        })
        .unwrap();
    }
}

#[test]
fn backspace_that_removes_a_block_damages_the_blocks_below_not_the_window() {
    let _serial = start();
    for (name, theme) in THEMES {
        let theme = theme();
        drive_live_window::<NotesShell, _>(|app| {
            let mut host = Host::new();
            open_a_new_note(app, &mut host, theme.clone());
            write_paragraphs(
                app,
                &mut host,
                &["Alpha line", "Bravo line", "Charlie line", "Delta line"],
            );
            // Joining two paragraphs: the caret at the start of "Charlie".
            caret_in(app, &mut host, "Charlie line", "Charlie line".len());
            let label = format!("{name} backspace-join-paragraphs");
            let (joined, _) = measure_key(
                app,
                &mut host,
                (&theme, &label),
                &Key::Named(NamedKey::Backspace),
            );
            assert!(
                values_of(app, "Block text").contains(&"Bravo lineCharlie line".to_owned()),
                "{label}: the paragraphs joined: {:?}",
                values_of(app, "Block text")
            );
            // Removing an empty paragraph between two.
            caret_in(app, &mut host, "Alpha line", 0);
            host.key(app, &Key::Named(NamedKey::Enter));
            host.frames(app, 3);
            let label = format!("{name} backspace-remove-empty");
            let (removed, _) = measure_key(
                app,
                &mut host,
                (&theme, &label),
                &Key::Named(NamedKey::Backspace),
            );
            assert_eq!(
                values_of(app, "Block text"),
                ["Alpha line", "Bravo lineCharlie line", "Delta line"],
                "{label}: the empty paragraph went"
            );
            for (case, repainted) in [
                ("joining two paragraphs", joined),
                ("removing an empty paragraph", removed),
            ] {
                assert_block_change(name, case, repainted);
            }
        })
        .unwrap();
    }
}

#[test]
fn renaming_a_note_damages_its_row_not_the_page_tree() {
    let _serial = start();
    for (name, theme) in THEMES {
        let theme = theme();
        drive_live_window::<NotesShell, _>(|app| {
            let mut host = Host::new();
            open_a_new_note(app, &mut host, theme.clone());
            let title = bounds_of(app, "Page title");
            host.click(app, title.center());
            host.frames(app, 3);
            type_text(app, &mut host, "Plan");
            // Naming the note in its title renames its row in the page tree.
            let label = format!("{name}: naming the note in its title");
            let (named, _) = measure(app, &mut host, &theme, &label, |app, host| {
                host.key(app, &Key::Named(NamedKey::Enter));
                host.frames(app, 4);
            });
            assert!(shows_name(app, "Plan"), "{label}: the row shows it");
            let_the_save_land(app, &mut host);
            // Renaming it from the page tree: F2 opens the row's field.
            host.click(app, bounds_of(app, "Plan").center());
            // The pointer leaves the row, so its buttons' tooltips stay away.
            host.input(app, cursor_moved(640.0, 700.0));
            host.frames(app, 3);
            host.key(app, &Key::Named(NamedKey::F2));
            host.frames(app, 3);
            // A rename keystroke changes no record: nothing but the field
            // should repaint, so the bubble the title's save showed goes
            // first.
            let_it_rest(app, &mut host);
            let mut field: f64 = 0.0;
            // What the page tree repaints, apart from the save bubble that
            // may come and go meanwhile.
            let tree = app
                .semantic_tree()
                .iter()
                .find(|node| node.role == ui::a11y::Role::Tree)
                .and_then(|node| node.bounds)
                .expect("the page tree is on screen");
            // The field opens with the caret after the name.
            for letter in [" ", "v", "2"] {
                let label = format!("{name}: renaming in the page tree, {letter:?}");
                let (typed, elapsed) =
                    measure_within(app, &mut host, (&theme, &label), tree, |app, host| {
                        host.key(app, &character(letter));
                    });
                eprintln!(
                    "notes-damage {name} rename-field key={letter:?} frame_px={:.0} tree_px={:.0} of {:.0} keystroke_to_frame={elapsed:?}",
                    typed.largest,
                    typed.largest_within,
                    app_area()
                );
                field = field.max(typed.largest_within);
            }
            let label = format!("{name}: committing the rename");
            let (renamed, _) = measure(app, &mut host, &theme, &label, |app, host| {
                host.key(app, &Key::Named(NamedKey::Enter));
                host.frames(app, 4);
            });
            assert!(shows_name(app, "Plan v2"), "{label}: the row shows it");
            eprintln!(
                "notes-damage {name} rename named_px={:.0} field_px={field:.0} committed_px={:.0} of {:.0}",
                named.largest,
                renamed.largest,
                app_area()
            );
            // The field's row, not the tree's column.
            assert!(
                field < app_area() * 0.015,
                "{name}: a key in the rename field repainted {field:.0} px of the page tree, over 1.5 % of the app"
            );
            // The title, the row, the line the caret goes back to and the
            // save bubble: not the page tree.
            for (case, repainted) in [("naming", named), ("renaming", renamed)] {
                assert!(
                    repainted.largest < app_area() * 0.1,
                    "{name}: {case} the note repainted {:.0} px, over 10 % of the app",
                    repainted.largest
                );
            }
        })
        .unwrap();
    }
}

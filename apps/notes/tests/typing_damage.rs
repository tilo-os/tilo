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
}

/// Present `frames` over `before` as the host does, each only where its
/// damage says, and require the pixels of a full render of the last one.
fn present_and_compare(
    before: &Scene,
    frames: &[Presented],
    theme: &Theme,
    label: &str,
) -> Repainted {
    let background = theme.tokens.colors.surfaces.content;
    let mut retained = ui::offscreen::OffscreenRenderer::new(WIDTH, HEIGHT);
    let mut full = ui::offscreen::OffscreenRenderer::new(WIDTH, HEIGHT);
    let _ = retained.render(before, background);
    let mut repainted = Repainted::default();
    for (scene, damage) in frames {
        let damage = damage
            .as_ref()
            .unwrap_or_else(|why| panic!("{label}: a frame repainted the app whole: {why}"));
        if damage.is_empty() {
            continue;
        }
        let rasterized = retained.render_damage_regions_gpu_wait(scene, background, damage);
        assert!(
            rasterized < PIXELS,
            "{label}: the damage must exercise a sparse render: {damage:?}"
        );
        let pixels = area(damage);
        repainted.total += pixels;
        repainted.largest = repainted.largest.max(pixels);
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
    (present_and_compare(&before, &frames, theme, label), elapsed)
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

fn character(text: &str) -> Key {
    Key::Character(text.into())
}

#[test]
fn a_keystroke_in_a_note_damages_its_line_not_the_app() {
    ui::i18n::I18nManager::init("en-US");
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
    ui::i18n::I18nManager::init("en-US");
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
    ui::i18n::I18nManager::init("en-US");
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

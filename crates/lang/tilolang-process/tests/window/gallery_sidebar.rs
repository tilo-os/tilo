//! Choosing a place in the gallery's sidebar in its real window: input through
//! `App::surface_input`, frames through `App::surface_frame`, as the native window drives them.
//! One press on a place shows its page, whether frames ran between the move, the press and the
//! release or not, and when the press lands before the page of the place chosen just before. A
//! phone-wide window pushes the page it shows over the places, with Back to them.

use tilolang_process::window::app::TlApp;
use ui::app::{App, AppSurface};
use ui::framework::testing::drive_live_window;
use ui::vello::Scene;
use ui::winit::event::{ElementState, MouseButton};

const PLACES: [&str; 15] = [
    "Basics",
    "Controls",
    "Feedback",
    "States",
    "Backlog",
    "Release board",
    "Data",
    "Media",
    "Places",
    "Shell parts",
    "Charts and views",
    "Trends and series",
    "Shares and shapes",
    "Flows and trees",
    "Type and scripts",
];

struct Window;

thread_local! {
    /// The window's size: a laptop screen's by default, a phone's where a test says so.
    static SIZE: std::cell::Cell<ui::winit::dpi::PhysicalSize<u32>> =
        const { std::cell::Cell::new(ui::winit::dpi::PhysicalSize::new(1280, 800)) };
}

impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> ui::winit::dpi::PhysicalSize<u32> {
        SIZE.with(std::cell::Cell::get)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: ui::winit::window::CursorIcon) {}
}

fn frame(app: &mut App) {
    let mut scene = Scene::new();
    for _ in 0..3 {
        scene.reset();
        if !app.surface_frame(&Window, &mut scene) || !app.unsettled() {
            break;
        }
    }
    app.refresh_semantic_tree();
}

/// The frames after a press, until what it showed is drawn.
fn rest(app: &mut App) {
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        frame(app);
    }
}

/// The window's frames until `shows` reads what `waited` names. The instance reads its indexes
/// on workers that wake the window when a read lands, as a real window waits for them: a loaded
/// machine lands them after any fixed wait, so the test waits for what it reads, and fails
/// saying so if it never comes.
fn until(app: &mut App, waited: &str, shows: impl Fn(&App) -> bool) {
    let began = std::time::Instant::now();
    while !shows(app) {
        assert!(began.elapsed().as_secs() < 60, "{waited}");
        std::thread::sleep(std::time::Duration::from_millis(10));
        frame(app);
    }
}

/// Whether the sidebar lists every place.
fn lists_places(app: &App) -> bool {
    PLACES.iter().all(|place| {
        app.semantic_tree()
            .iter()
            .any(|node| node.role == tilo_ui_base::a11y::Role::TreeItem && node.answers_to(place))
    })
}

/// The middle of the sidebar's row for `place`, wheeling the sidebar until the row is in the
/// window as a person would: the places run longer than a laptop screen is tall.
fn row(app: &mut App, place: &str) -> (f64, f64) {
    let height = f64::from(SIZE.with(std::cell::Cell::get).height);
    for _ in 0..40 {
        let bounds = app
            .semantic_tree()
            .iter()
            .find(|node| node.role == tilo_ui_base::a11y::Role::TreeItem && node.answers_to(place))
            .and_then(|node| node.bounds)
            .unwrap_or_else(|| panic!("the sidebar has a row for {place}"));
        let middle = (
            f64::midpoint(bounds.x0, bounds.x1),
            f64::midpoint(bounds.y0, bounds.y1),
        );
        if bounds.y0 >= 0.0 && bounds.y1 <= height {
            return middle;
        }
        let lines = if bounds.y1 > height { -1.0 } else { 1.0 };
        app.surface_input(
            &Window,
            tilo_ui_base::synthetic_input::cursor_moved(middle.0, height / 2.0),
            false,
        );
        app.surface_input(&Window, tilo_ui_base::synthetic_input::mouse_wheel(0.0, lines), false);
        rest(app);
    }
    panic!("the sidebar never brings {place} into the window");
}

/// The place the page beside the sidebar shows: the window's title, read off everything but the
/// sidebar's rows.
fn shown(app: &App) -> Option<String> {
    app.semantic_tree()
        .iter()
        .filter(|node| node.role != tilo_ui_base::a11y::Role::TreeItem)
        .filter_map(|node| node.name.clone())
        .find(|name| PLACES.contains(&name.as_str()))
}

fn mouse(app: &mut App, state: ElementState) {
    let event = tilo_ui_base::synthetic_input::mouse_input(state, MouseButton::Left);
    app.surface_input(&Window, event, false);
}

/// Where `name` stands as `role` in the window, when it shows one.
fn named(app: &App, role: tilo_ui_base::a11y::Role, name: &str) -> Option<(f64, f64)> {
    let bounds = app
        .semantic_tree()
        .iter()
        .find(|node| node.role == role && node.answers_to(name))
        .and_then(|node| node.bounds)?;
    Some((
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    ))
}

/// One press at `(x, y)`, and the frames after it.
fn press(app: &mut App, (x, y): (f64, f64)) {
    app.surface_input(&Window, tilo_ui_base::synthetic_input::cursor_moved(x, y), false);
    mouse(app, ElementState::Pressed);
    mouse(app, ElementState::Released);
    rest(app);
}

#[test]
fn one_press_on_a_place_shows_its_page() {
    crate::support::installed("gallery");
    drive_live_window::<TlApp, _>(|app| {
        until(app, "the sidebar lists its places", lists_places);
        for (turn, place) in PLACES.iter().rev().enumerate() {
            let framed = turn % 2 == 1;
            let (x, y) = row(app, place);
            app.surface_input(&Window, tilo_ui_base::synthetic_input::cursor_moved(x, y), false);
            if framed {
                frame(app);
            }
            mouse(app, ElementState::Pressed);
            if framed {
                frame(app);
            }
            mouse(app, ElementState::Released);
            rest(app);
            until(app, &format!("one press shows {place}"), |app| {
                shown(app).as_deref() == Some(*place)
            });
        }
        // A press on the next place before the page of the last one arrived.
        for (first, then) in [("Controls", "Data"), ("Media", "Backlog")] {
            let (x, y) = row(app, first);
            app.surface_input(&Window, tilo_ui_base::synthetic_input::cursor_moved(x, y), false);
            mouse(app, ElementState::Pressed);
            mouse(app, ElementState::Released);
            let (x, y) = row(app, then);
            app.surface_input(&Window, tilo_ui_base::synthetic_input::cursor_moved(x, y), false);
            mouse(app, ElementState::Pressed);
            frame(app);
            mouse(app, ElementState::Released);
            rest(app);
            until(app, &format!("{then} pressed while {first} came"), |app| {
                shown(app).as_deref() == Some(then)
            });
        }
    })
    .expect("the gallery's window opens");
}

#[test]
fn a_phone_pushes_the_chosen_page_over_the_places() {
    crate::support::installed("gallery");
    SIZE.with(|size| size.set(ui::winit::dpi::PhysicalSize::new(390, 844)));
    drive_live_window::<TlApp, _>(|app| {
        until(app, "the sidebar lists its places", lists_places);
        for place in PLACES {
            let at = row(app, place);
            press(app, at);
            until(
                app,
                &format!("one press shows {place}, over the places, with Back"),
                |app| {
                    shown(app).as_deref() == Some(place)
                        && named(app, tilo_ui_base::a11y::Role::Button, "Back").is_some()
                },
            );
            let back = named(app, tilo_ui_base::a11y::Role::Button, "Back").expect("Back");
            press(app, back);
            until(
                app,
                &format!("Back from {place} shows the places again"),
                |app| named(app, tilo_ui_base::a11y::Role::Button, "Hide sidebar").is_some(),
            );
        }
    })
    .expect("the gallery's window opens");
}

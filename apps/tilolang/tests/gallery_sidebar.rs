//! Choosing a place in the gallery's sidebar in its real window: input through
//! `App::surface_input`, frames through `App::surface_frame`, as the native window drives them.
//! One press on a place shows its page, whether frames ran between the move, the press and the
//! release or not, and when the press lands before the page of the place chosen just before.

use tilolang_app::app::TlApp;
use ui::app::{App, AppSurface};
use ui::framework::testing::drive_live_window;
use ui::vello::Scene;
use ui::winit::event::{ElementState, MouseButton};

const PLACES: [&str; 10] = [
    "Basics",
    "Controls",
    "Feedback",
    "Backlog",
    "Release board",
    "Data",
    "Media",
    "Places",
    "Shell parts",
    "Charts and views",
];

struct Window;

impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> ui::winit::dpi::PhysicalSize<u32> {
        ui::winit::dpi::PhysicalSize::new(1280, 800)
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

/// The middle of the sidebar's row for `place`.
fn row(app: &App, place: &str) -> (f64, f64) {
    let bounds = app
        .semantic_tree()
        .iter()
        .find(|node| node.role == ui::a11y::Role::TreeItem && node.name.as_deref() == Some(place))
        .and_then(|node| node.bounds)
        .unwrap_or_else(|| panic!("the sidebar has a row for {place}"));
    (
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    )
}

/// The place the page beside the sidebar shows: the window's title, read off everything but the
/// sidebar's rows.
fn shown(app: &App) -> Option<String> {
    app.semantic_tree()
        .iter()
        .filter(|node| node.role != ui::a11y::Role::TreeItem)
        .filter_map(|node| node.name.clone())
        .find(|name| PLACES.contains(&name.as_str()))
}

fn mouse(app: &mut App, state: ElementState) {
    let event = ui::synthetic_input::mouse_input(state, MouseButton::Left);
    app.surface_input(&Window, event, false);
}

#[test]
fn one_press_on_a_place_shows_its_page() {
    crate::support::installed("gallery");
    drive_live_window::<TlApp, _>(|app| {
        rest(app);
        for (turn, place) in PLACES.iter().rev().enumerate() {
            let framed = turn % 2 == 1;
            let (x, y) = row(app, place);
            app.surface_input(&Window, ui::synthetic_input::cursor_moved(x, y), false);
            if framed {
                frame(app);
            }
            mouse(app, ElementState::Pressed);
            if framed {
                frame(app);
            }
            mouse(app, ElementState::Released);
            rest(app);
            assert_eq!(
                shown(app).as_deref(),
                Some(*place),
                "one press shows {place}"
            );
        }
        // A press on the next place before the page of the last one arrived.
        for (first, then) in [("Controls", "Data"), ("Media", "Backlog")] {
            let (x, y) = row(app, first);
            app.surface_input(&Window, ui::synthetic_input::cursor_moved(x, y), false);
            mouse(app, ElementState::Pressed);
            mouse(app, ElementState::Released);
            let (x, y) = row(app, then);
            app.surface_input(&Window, ui::synthetic_input::cursor_moved(x, y), false);
            mouse(app, ElementState::Pressed);
            frame(app);
            mouse(app, ElementState::Released);
            rest(app);
            assert_eq!(
                shown(app).as_deref(),
                Some(then),
                "{then} pressed while {first} came"
            );
        }
    })
    .expect("the gallery's window opens");
}

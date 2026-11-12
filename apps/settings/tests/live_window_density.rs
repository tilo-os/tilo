//! A press made after the interface density changed reaches the control it
//! lands on in the app's own live window — the `ui_core::App` a host runs,
//! not the frame harness — including a press that arrives before any frame has
//! shown what the new density changed.
//!
//! What a press after a density change was reported to miss happens around a
//! window, not inside it: this pins the app's half, so a fix in the host that
//! draws the window keeps a test of what the window itself owes.

use settings::SettingsShell;
use ui::app::{App, AppSurface, SurfaceDamage};
use ui::framework::testing::drive_live_window;
use ui::synthetic_input::{cursor_moved, mouse_press, mouse_release};
use vello::Scene;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 900;

/// The window's surface, at scale 1.
struct Window;

impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> ui::winit::dpi::PhysicalSize<u32> {
        ui::winit::dpi::PhysicalSize::new(WIDTH, HEIGHT)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: ui::winit::window::CursorIcon) {}
}

/// Frames until the window is at rest, then the host's accessibility walk: the
/// tree a person's assistive technology (and this test) reads.
fn frame(app: &mut App, damage: &mut SurfaceDamage) {
    for _ in 0..4 {
        let mut scene = Scene::new();
        if !app.surface_frame_with_damage(&Window, &mut scene, damage) || !app.unsettled() {
            break;
        }
    }
    app.refresh_semantic_tree();
}

/// Where the control named `name` stands, as the window publishes it.
fn centre(app: &App, name: &str) -> vello::kurbo::Point {
    app.semantic_tree()
        .iter()
        .find(|node| node.name.as_deref() == Some(name))
        .and_then(|node| node.bounds)
        .unwrap_or_else(|| {
            let names: Vec<&str> = app
                .semantic_tree()
                .iter()
                .filter_map(|node| node.name.as_deref())
                .collect();
            panic!("nothing named {name}; the window names: {names:?}")
        })
        .center()
}

/// Press the control named `name` with the pointer, as a person does.
fn press(app: &mut App, damage: &mut SurfaceDamage, name: &str) {
    let at = centre(app, name);
    app.surface_input(&Window, cursor_moved(at.x, at.y), false);
    frame(app, damage);
    tap(app, damage);
}

/// Press and let go with no frame between the press and its release, and none
/// before the press: what a person makes the instant they have clicked.
fn tap(app: &mut App, damage: &mut SurfaceDamage) {
    app.surface_input(&Window, mouse_press(), false);
    app.surface_input(&Window, mouse_release(), false);
    frame(app, damage);
}

fn names(app: &App) -> Vec<String> {
    app.semantic_tree()
        .iter()
        .filter_map(|node| node.name.clone())
        .collect()
}

#[test]
fn a_press_after_a_density_change_opens_the_pane_in_the_real_window() {
    ui::i18n::I18nManager::init("en-US");
    drive_live_window::<SettingsShell, _>(|app| {
        let mut damage = SurfaceDamage::default();
        frame(app, &mut damage);
        press(app, &mut damage, "Appearance");
        assert!(
            names(app).iter().any(|name| name == "Interface size"),
            "the Appearance pane shows: {:?}",
            names(app)
        );

        press(app, &mut damage, "Compact");
        // The press comes before any frame shows what the density changed: the
        // window has been laid out again but nothing has drawn it yet.
        let at = centre(app, "Accessibility");
        app.surface_input(&Window, cursor_moved(at.x, at.y), false);
        tap(app, &mut damage);
        let shown = names(app);
        assert!(
            shown.iter().any(|name| name == "Sticky keys"),
            "the press after the density change opened the Accessibility pane: {shown:?}"
        );
    })
    .unwrap();
}

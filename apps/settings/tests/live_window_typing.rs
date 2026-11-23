//! What a person types after pressing a field reaches the field, and a control the shell
//! presses while that typing is still on its way runs after it — in the app's own live window,
//! the `ui_core::App` a host runs, not the frame harness.

#![cfg(unix)]

use settings::SettingsShell;
use settings::categories::Category;
use ui::app::{App, AppSurface, SurfaceDamage};
use ui::floating_surface::FloatingSurfaceHostMessage;
use ui::framework::testing::drive_live_window;
use ui::synthetic_input::{char_key, cursor_moved, mouse_press, mouse_release};
use ui::window_targets::TargetAction;
use vello::Scene;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 900;

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

/// Frames until the window is at rest, then the host's accessibility walk: the tree a person's
/// assistive technology (and this test) reads.
fn frame(app: &mut App, damage: &mut SurfaceDamage) {
    for _ in 0..4 {
        let mut scene = Scene::new();
        if !app.surface_frame_with_damage(&Window, &mut scene, damage) || !app.unsettled() {
            break;
        }
    }
    app.refresh_semantic_tree();
}

fn names(app: &App) -> Vec<String> {
    app.semantic_tree()
        .iter()
        .filter_map(|node| node.name.clone())
        .collect()
}

/// Where the window draws the control called `name`, among the ones the person sees: a window
/// names a row once in the sidebar it shows and once in the copy it keeps for a narrow width.
fn centre(app: &App, name: &str) -> Option<vello::kurbo::Point> {
    app.semantic_tree()
        .iter()
        .filter(|node| node.name.as_deref() == Some(name))
        .filter_map(|node| node.bounds)
        .filter(|bounds| {
            bounds.width() > 0.0
                && bounds.height() > 0.0
                && bounds.x0 >= 0.0
                && bounds.y0 >= 0.0
                && bounds.x1 <= f64::from(WIDTH)
                && bounds.y1 <= f64::from(HEIGHT)
        })
        .max_by(|a, b| a.area().total_cmp(&b.area()))
        .map(|bounds| bounds.center())
}

/// What every field in the window holds, as an assistive technology reads it.
fn values(app: &App) -> Vec<String> {
    app.semantic_tree()
        .iter()
        .filter_map(|node| node.value.clone())
        .collect()
}

/// The point the control `name` stands at, once the window draws it: the sidebar scrolls for a
/// row below the fold, as the person scrolls to it.
fn until_centre(app: &mut App, damage: &mut SurfaceDamage, name: &str) -> vello::kurbo::Point {
    for step in 0..600 {
        if let Some(at) = centre(app, name) {
            return at;
        }
        if step % 40 == 39 {
            app.surface_input(&Window, cursor_moved(120.0, 400.0), false);
            app.surface_input(&Window, ui::synthetic_input::mouse_wheel(0.0, -60.0), false);
        }
        frame(app, damage);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("nothing named {name}; the window names: {:?}", names(app));
}

/// Point at `name` and press it, as a person does.
fn press(app: &mut App, damage: &mut SurfaceDamage, name: &str) {
    let at = until_centre(app, damage, name);
    app.surface_input(&Window, cursor_moved(at.x, at.y), false);
    frame(app, damage);
    app.surface_input(&Window, mouse_press(), false);
    app.surface_input(&Window, mouse_release(), false);
    frame(app, damage);
}

/// The role the window gives the control called `name`.
fn role(app: &App, name: &str) -> String {
    app.semantic_tree()
        .iter()
        .find(|node| node.name.as_deref() == Some(name))
        .map_or_else(
            || panic!("nothing named {name}"),
            |node| format!("{:?}", node.role),
        )
}

/// Tell the window the shell pressed the control `name` of its content, after the input with
/// the serial `after_input` (the shell's).
fn press_from_shell(app: &mut App, name: &str, role: &str, after_input: Option<u32>) {
    app.test_deliver_host_message(FloatingSurfaceHostMessage::PressTarget {
        name: name.to_owned(),
        role: role.to_owned(),
        identity: None,
        action: TargetAction::Activate,
        after_input,
    });
}

/// Settings' Accounts pane with the Add Email form open.
fn accounts_form(app: &mut App, damage: &mut SurfaceDamage) {
    frame(app, damage);
    press(app, damage, &Category::ExternalAccounts.label());
    press(app, damage, "Add Email");
    assert!(
        names(app).iter().any(|name| name == "Save account"),
        "the account form shows: {:?}",
        names(app)
    );
}

/// A press on a field of the Add Email form, then the keys typed after it, reach that field.
#[test]
fn a_press_on_the_account_name_field_takes_the_typing_after_it() {
    ui::i18n::I18nManager::init("en-US");
    drive_live_window::<SettingsShell, _>(|app| {
        let mut damage = SurfaceDamage::default();
        accounts_form(app, &mut damage);
        let at = until_centre(app, &mut damage, "Display name");
        app.surface_input(&Window, cursor_moved(at.x, at.y), false);
        app.surface_input(&Window, mouse_press(), false);
        app.surface_input(&Window, mouse_release(), false);
        for ch in "Ada".chars() {
            app.surface_input(&Window, char_key(&ch.to_string()), false);
        }
        frame(app, &mut damage);
        assert!(
            values(app).iter().any(|value| value == "Ada"),
            "the field holds every character typed after the press; it holds {:?}",
            values(app)
        );
    })
    .unwrap();
}

/// The shell presses the form's Cancel while the typing before it is still queued for the
/// window: the form stays until the window has taken the typing, and the field holds all of it.
#[test]
fn a_press_the_shell_makes_while_typing_is_queued_runs_after_it() {
    ui::i18n::I18nManager::init("en-US");
    drive_live_window::<SettingsShell, _>(|app| {
        let mut damage = SurfaceDamage::default();
        accounts_form(app, &mut damage);
        let at = until_centre(app, &mut damage, "Display name");
        app.surface_input(&Window, cursor_moved(at.x, at.y), false);
        app.surface_input(&Window, mouse_press(), false);
        app.surface_input(&Window, mouse_release(), false);
        // The keys are on their way to the window but not handed it yet: the shell wrote the
        // press after that input, so it waits for it.
        press_from_shell(app, "Cancel", &role(app, "Cancel"), Some(7));
        for ch in "Ada".chars() {
            app.surface_input(&Window, char_key(&ch.to_string()), false);
        }
        frame(app, &mut damage);
        assert!(
            values(app).iter().any(|value| value == "Ada"),
            "the field holds the typing before the press: {:?}",
            values(app)
        );
        assert!(
            names(app).iter().any(|name| name == "Display name"),
            "the Cancel the shell pressed waits for the typing before it: {:?}",
            names(app)
        );
        app.input_handled(7);
        frame(app, &mut damage);
        assert!(
            !names(app).iter().any(|name| name == "Display name"),
            "Cancel runs once the window has taken the typing: {:?}",
            names(app)
        );
    })
    .unwrap();
}

//! A theme package the appearance applied draws an app in its own process: the window the app's
//! process paints uses the bundle's painter sets, and a change of theme while it runs reaches it
//! the way the person's other appearance changes do.
//!
//! The app is a real tilolang package started by the session's launcher, its own binary drawing
//! the window it was sent; the frames read below are the compositor's, which holds what that
//! process painted.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tilo_app_launch::Launch;
use tilo_session::Session;

use crate::common::linked::{Builder, Fixture};

/// The person's appearance section, as the settings service publishes it.
const APPEARANCE: &str = "settings/appearance";
/// A real tilolang package of this tree, started in its own process by the session's launcher.
const APP: &str = "so.tilo.notes";
/// The theme package the test installs first, and the painter set its bundle names.
const FIRST_PACKAGE: &str = "so.tilo.theme.process-first";
const FIRST_SET: &str = "test.theme-process-first";
/// The second theme package: another installed bundle, applied while the app runs.
const SECOND_PACKAGE: &str = "so.tilo.theme.process-second";
const SECOND_SET: &str = "test.theme-process-second";

/// The colours the two bundles' painters fill a surface part with: colours the shipped themes do
/// not use, so a pixel of one in an app's window is that bundle's set painting it.
const FIRST_COLOUR: [u8; 4] = [0xd0, 0x30, 0x20, 0xff];
const SECOND_COLOUR: [u8; 4] = [0x20, 0x60, 0xd0, 0xff];

/// How many of a window's pixels carrying a bundle's colour say the app's process painted with it.
const ENOUGH: usize = 500;

/// The person's appearance changed, on a session whose settings source attaches once its daemon
/// holds the person's own workspace: the action is asked until it is accepted.
fn apply(fixture: &mut Fixture, package: &str) {
    let arguments: Map<String, Value> = json!({ "package": package })
        .as_object()
        .cloned()
        .expect("an object");
    crate::common::wait_for(
        &mut fixture.session,
        format!("the appearance applies {package}"),
        |session| {
            session
                .invoke(APPEARANCE, "apply-theme", &arguments)
                .is_ok()
                .then_some(())
        },
    );
}

/// Start `app` as the person opening it does, in the workspace in front, once the device is
/// admitted to it: the session is taken into the person's workspace a moment after it starts, and
/// an app starts there only once it is.
fn open(fixture: &mut Fixture, app: &str) {
    let launch = Launch::Package {
        app: app.to_owned(),
        open: None,
    };
    let deadline = Instant::now() + tilo_test_env::hang_bound();
    loop {
        let workspace = fixture
            .session
            .handle()
            .read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id().to_string())
            })
            .unwrap_or_else(|| fixture.workspace.clone());
        let workspace = fixture.apps.workspace_named(&workspace);
        match fixture.apps.start(&launch, &workspace) {
            Ok(()) => break,
            Err(why)
                if format!("{why}").contains("hasn't been admitted") && Instant::now() < deadline =>
            {
                fixture.pass(Duration::from_millis(100));
            }
            Err(why) => panic!("{app} starts: {why}"),
        }
    }
    fixture.until_published(app);
}

/// The tab of the app's window, while the desk holds one.
fn tab_of(session: &Session, app: &str) -> Option<u64> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|tab| {
                tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                    .is_some_and(|shown| shown == app)
            })
            .map(|tab| tab.id)
    })
}

/// Whether `pixel` shows `colour`, allowing for the last bit a renderer may leave off a channel.
fn close(pixel: [u8; 4], colour: [u8; 4]) -> bool {
    pixel[3] == 0xff && (0..3).all(|channel| pixel[channel].abs_diff(colour[channel]) <= 6)
}

/// How many pixels of the app's window are `colour`: the app's own process painted them.
fn app_pixels(session: &mut Session, app: &str, colour: [u8; 4]) -> Option<usize> {
    let tab = tab_of(session, app)?;
    let rect = session.handle().window_rect(tab)?;
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("the session draws its display");
    // The window's own content, inside the shell's edge around it.
    let inset = 6.0;
    let (x0, y0) = (
        i32::try_from((rect.x0 + inset).round() as i64).ok()?,
        i32::try_from((rect.y0 + inset).round() as i64).ok()?,
    );
    let (x1, y1) = (
        i32::try_from((rect.x1 - inset).round() as i64).ok()?,
        i32::try_from((rect.y1 - inset).round() as i64).ok()?,
    );
    let mut count = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
                continue;
            };
            if shot.pixel(x, y).is_some_and(|pixel| close(pixel, colour)) {
                count += 1;
            }
        }
    }
    Some(count)
}

/// How many pixels of the whole display are `colour`: what the shell draws with the set.
fn frame_pixels(session: &mut Session, colour: [u8; 4]) -> usize {
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("the session draws its display");
    shot.rgba
        .chunks_exact(4)
        .filter(|pixel| close([pixel[0], pixel[1], pixel[2], pixel[3]], colour))
        .count()
}

/// The painter module a bundle carries: it declares no parameters and fills every part it is
/// asked for with `colour`, so a window drawn by it is visibly its own.
fn filling_module(colour: [u8; 3]) -> Vec<u8> {
    use tilo_ui_base::{part_paint::Brush, part_paint::PaintedPart, part_paint::PartScene};

    let mut part = PartScene::new();
    let mut field = ui::kurbo::BezPath::new();
    field.move_to((0.0, 0.0));
    field.line_to((16384.0, 0.0));
    field.line_to((16384.0, 16384.0));
    field.line_to((0.0, 16384.0));
    field.close_path();
    part.fill(
        field,
        Brush::Solid(ui::peniko::Color::from_rgb8(colour[0], colour[1], colour[2])),
    );
    let answer = tilo_theme_sdk::wire::paint_answer(Some(&PaintedPart::new(part)));
    let params = tilo_theme_sdk::wire::params_answer(&[]);
    let data = |bytes: &[u8]| {
        let mut text = String::new();
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(text, "\\{byte:02x}");
        }
        text
    };
    let params = data(&params);
    let answer = data(&answer);
    wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 2)
            (data (i32.const 1024) "{params}")
            (data (i32.const 8192) "{answer}")
            (func (export "tilo_theme_buffer") (param i32) (result i32) (i32.const 65536))
            (func (export "tilo_theme_call") (param i32) (result i64)
                (if (result i64) (i32.eq (i32.load8_u (i32.const 65536)) (i32.const 3))
                    (then (i64.or (i64.shl (i64.const 1024) (i64.const 32)) (i64.const {params_len})))
                    (else (i64.or (i64.shl (i64.const 8192) (i64.const 32)) (i64.const {answer_len}))))))"#,
        params_len = params.len() / 3,
        answer_len = answer.len() / 3,
    ))
    .expect("the module reads")
}

/// The bundle's document: the shipped Bauhaus theme with the task's own set drawing every
/// surface role, its palette unchanged.
fn document(id: &str, set: &str) -> String {
    let mut value: Value = serde_json::from_str(include_str!("../../../ui/themes/bauhaus/theme.json"))
        .expect("the shipped theme reads");
    value["id"] = json!(id);
    value["name"] = json!(id);
    for role in ["window", "control", "floating", "bar"] {
        value["material"][role] = json!({ "painter": set });
    }
    serde_json::to_string(&value).expect("the document writes")
}

/// Lay a theme package out as an install leaves it: its bundle in the device's own store, a row in
/// the store's registry, and the set's machine code in the store the launcher grants read-only.
fn install_theme_package(store: &Path, id: &str, set: &str, colour: [u8; 3]) {
    let payload = store.join("pkgs").join(set);
    std::fs::create_dir_all(&payload).expect("the payload tree");
    std::fs::write(payload.join(tilo_ui_base::theme_bundle::DOCUMENT), document(id, set))
        .expect("the document");
    let bytes = filling_module(colour);
    std::fs::write(payload.join(format!("{set}.wasm")), &bytes).expect("the module");
    tilo_wasm_aot::Cache::at(
        tilo_paths::data_dir()
            .expect("the test home")
            .join("tilolang-compiled"),
    )
    .expect("the device compiles")
    .ensure(&bytes)
    .expect("the set compiles");
    let mut registry =
        package_store::Registry::load(&store.join("registry.toml")).expect("the registry reads");
    let mut row = package_store::InstalledPackage::detached(
        id,
        String::new(),
        payload.clone(),
        payload.join("manifest.toml"),
    );
    "1.0.0".clone_into(&mut row.version);
    registry.upsert(row);
    registry
        .save(&store.join("registry.toml"))
        .expect("the registry is written");
}

/// A step of the session's loop, so a change asked for above lands.
fn step(fixture: &mut Fixture) {
    fixture
        .session
        .step(Duration::from_millis(2))
        .expect("a turn of the session");
}

/// The applied theme package reaches the app's own process, and a change of it reaches the app
/// while it runs: the window's pixels are the first bundle's painters', then the second's.
#[test]
fn an_applied_theme_package_draws_the_app_in_its_own_process_and_follows_a_change() {
    let _settled = tilo_ui_base::motion::testing::settled();
    // The person's settings service beside the desktop's own: the appearance section it
    // publishes is where the applied package is written and read.
    let mut fixture = Builder::installing(APP)
        .service(Box::new(settings_sections::service::SettingsService::new()))
        .start();
    for _ in 0..20 {
        step(&mut fixture);
    }
    // Both packages are installed on the device before the app starts, as a person installs a
    // theme and then applies it; the app opens after the first was applied.
    install_theme_package(&fixture.store, FIRST_PACKAGE, FIRST_SET, [0xd0, 0x30, 0x20]);
    install_theme_package(&fixture.store, SECOND_PACKAGE, SECOND_SET, [0x20, 0x60, 0xd0]);
    apply(&mut fixture, FIRST_PACKAGE);
    for _ in 0..20 {
        step(&mut fixture);
    }
    // The session draws the bundle the appearance applied: the theme is loaded and its set paints.
    let shell = crate::common::wait_for(
        &mut fixture.session,
        "the session draws the applied bundle's set",
        |session| {
            session.step(Duration::from_millis(2)).expect("step");
            let count = frame_pixels(session, FIRST_COLOUR);
            (count >= ENOUGH).then_some(count)
        },
    );
    assert!(
        shell >= ENOUGH,
        "the session draws the applied bundle: {shell} pixels are its set's"
    );

    // The app is a package of this tree started in a process of its own.
    open(&mut fixture, APP);
    let painted = crate::common::wait_for(
        &mut fixture.session,
        "the app's window is painted by the bundle's set",
        |session| {
            session.step(Duration::from_millis(2)).expect("step");
            let count = app_pixels(session, APP, FIRST_COLOUR)?;
            (count >= ENOUGH).then_some(count)
        },
    );
    assert!(
        painted >= ENOUGH,
        "the app's own process paints the applied bundle's window: {painted} pixels are its set's"
    );

    // The person applies the other installed theme: the running app follows its settings change,
    // as it follows every appearance change, and paints the second bundle's set.
    apply(&mut fixture, SECOND_PACKAGE);
    let restyled = crate::common::wait_for(
        &mut fixture.session,
        "the app follows the theme the person applied",
        |session| {
            session.step(Duration::from_millis(2)).expect("step");
            let first = app_pixels(session, APP, FIRST_COLOUR)?;
            let second = app_pixels(session, APP, SECOND_COLOUR)?;
            (second >= ENOUGH && first < ENOUGH).then_some((second, first))
        },
    );
    assert!(
        restyled.0 >= ENOUGH && restyled.1 < ENOUGH,
        "the running app moved to the second bundle: {} pixels are its set's, {} the first's",
        restyled.0,
        restyled.1
    );
}

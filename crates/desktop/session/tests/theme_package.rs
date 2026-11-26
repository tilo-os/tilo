//! A theme package applied through the appearance's own action, on a running session: the frame
//! the session draws is the bundle's painters', drawn on the machine code the device compiled
//! when the package arrived; undo returns the theme the mode shows; a device without the package
//! falls back to that theme; and a caller without the grant is refused.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_session::{Options, Session};

use crate::common;

const PERSON: &str = "Pat";
/// The person's appearance section, as the settings service publishes it.
const APPEARANCE: &str = "settings/appearance";
/// The theme package the test installs, and the painter set its bundle names.
const PACKAGE: &str = "so.tilo.theme.triangle";
const SET: &str = "test.theme-triangle";

/// The session: the tilolang shell with the person's settings service beside it.
fn session() -> Session {
    let services: Vec<Box<dyn tilo_service::Service>> =
        vec![Box::new(settings_sections::service::SettingsService::new())];
    common::headless(Options {
        person: PERSON.to_owned(),
        collab: false,
        services,
        ..Options::default()
    })
}

fn step(session: &mut Session, frames: usize) {
    for _ in 0..frames {
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// The frame the compositor draws for the whole output.
fn frame(session: &mut Session) -> tilo_compositor::Screenshot {
    session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("the session draws its display")
}

fn differing_pixels(a: &tilo_compositor::Screenshot, b: &tilo_compositor::Screenshot) -> usize {
    a.rgba
        .chunks_exact(4)
        .zip(b.rgba.chunks_exact(4))
        .filter(|(left, right)| left != right)
        .count()
}

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn published(session: &Session) -> Value {
    session
        .read(APPEARANCE)
        .expect("the session publishes the appearance section")
        .get("theme_package")
        .cloned()
        .unwrap_or(Value::Null)
}

/// The painter module a bundle carries: it declares the SDK's parameters and draws a triangle, so
/// a frame drawn by it is visibly other than the stock theme's. The device's daemon compiles it
/// when the package arrives.
fn triangle_module() -> Vec<u8> {
    use tilo_ui_base::{
        part_paint::Brush, part_paint::PaintedPart, part_paint::ParamSpec, part_paint::PartScene,
    };

    let mut part = PartScene::new();
    let mut triangle = ui::kurbo::BezPath::new();
    triangle.move_to((60.0, 0.0));
    triangle.line_to((108.0, 40.0));
    triangle.line_to((12.0, 40.0));
    triangle.close_path();
    part.fill(
        triangle,
        Brush::Solid(ui::peniko::Color::from_rgb8(0xd0, 0x30, 0x20)),
    );
    let answer = tilo_theme_sdk::wire::paint_answer(Some(&PaintedPart::new(part)));
    let params = tilo_theme_sdk::wire::params_answer(&[
        ParamSpec::new("levels", Some(4.0), 2.0..=16.0),
        ParamSpec::new("depth", Some(0.14), 0.0..=1.0),
        ParamSpec::new("shadow", Some(3.0), 0.0..=10.0),
    ]);
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

/// Lay the package out as an install leaves it: its bundle in the device's user store, a row in
/// the store's registry, and the set's machine code in the store the launcher grants read-only.
fn install_theme_package(store: &std::path::Path) -> Vec<u8> {
    const DOCUMENT: &str = include_str!("../../../ui/themes/bauhaus/theme.json");
    let payload = store.join("pkgs").join("theme-triangle");
    std::fs::create_dir_all(&payload).expect("the payload tree");
    std::fs::write(
        payload.join(tilo_ui_base::theme_bundle::DOCUMENT),
        DOCUMENT.replace("\"bauhaus\"", &format!("{SET:?}")),
    )
    .expect("the document");
    let bytes = triangle_module();
    std::fs::write(payload.join(format!("{SET}.wasm")), &bytes).expect("the module");
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
        PACKAGE,
        String::new(),
        payload.clone(),
        payload.join("manifest.toml"),
    );
    "1.0.0".clone_into(&mut row.version);
    registry.upsert(row);
    registry
        .save(&store.join("registry.toml"))
        .expect("the registry is written");
    bytes
}

/// Take the package away, as a device the person never installed it on has none.
fn uninstall_theme_package(store: &std::path::Path) {
    let mut registry =
        package_store::Registry::load(&store.join("registry.toml")).expect("the registry reads");
    registry.remove(PACKAGE);
    registry
        .save(&store.join("registry.toml"))
        .expect("the registry is written");
}

#[test]
fn an_applied_theme_package_draws_the_session_on_the_compiled_store() {
    let _home = tilo_test_env::TestHome::new().expect("a home");
    // This device's own store, which its session reads, holding only the theme package: the
    // session's shell stands in where the store holds none.
    let store = package_store::layout::default_user_root().expect("the device's store");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APPS_DATA_DIR", &store);
    let bytes = install_theme_package(&store);
    let mut session = session();
    step(&mut session, 200);
    let stock = frame(&mut session);
    assert_eq!(published(&session), Value::Null, "nothing is applied yet");

    // Settings' Appearance page applies it: the section's own action, named by the package id.
    let lookups = tilo_wasm_aot::lookups();
    session
        .invoke_with_actor(
            &common::as_person(PERSON),
            APPEARANCE,
            "apply-theme",
            &json!({ "package": PACKAGE }),
        )
        .expect("the action runs");
    step(&mut session, 300);
    assert_eq!(published(&session), json!(PACKAGE));
    assert!(
        tilo_wasm_aot::lookups() > lookups,
        "the apply path read the device's compiled store"
    );

    let applied = frame(&mut session);
    assert!(
        differing_pixels(&stock, &applied) > 1000,
        "the next frames are the bundle's painters': {} of {} pixels differ",
        differing_pixels(&stock, &applied),
        stock.rgba.len() / 4
    );

    // Nothing else applies a theme: the section publishes no generic setter for the package, so
    // a write of the field is refused, and only the action names it.
    let refused = session.invoke(
        APPEARANCE,
        "set-theme-package",
        &arguments(json!({ "value": PACKAGE })),
    );
    assert!(
        refused.is_err(),
        "the applied package is written by its action, not a setter"
    );

    // The set the appearance resolves is the bundle's, on the machine code the daemon compiled
    // when the package arrived: no compile happens on this path, the store is read.
    let installed = package_store::find_installed(PACKAGE)
        .expect("the store reads")
        .expect("the package is installed");
    assert!(
        tilo_wasm_aot::kept_in(
            &tilo_paths::data_dir()
                .expect("the test home")
                .join("tilolang-compiled"),
            &bytes
        )
        .is_some(),
        "the install compiled the set"
    );
    let document =
        tilo_ui_base::theme_bundle::document(&installed.payload_root).expect("the bundle reads");
    let theme = tilo_ui_base::Theme::from_document(&document).expect("the theme draws");
    assert_eq!(
        theme
            .finish(tilo_ui_base::tokens::SurfaceRole::Control)
            .painter(),
        SET
    );

    // Undo takes the person's step back: nothing is applied, and the theme the mode shows is the
    // one drawn again.
    session
        .invoke_with_actor(&common::as_person(PERSON), APPEARANCE, "undo", &json!({}))
        .expect("the action runs");
    step(&mut session, 300);
    assert_eq!(published(&session), Value::Null, "no package is applied");
    let undone = frame(&mut session);
    assert!(
        differing_pixels(&stock, &undone) < differing_pixels(&stock, &applied) / 4,
        "the previous theme draws again: {} pixels differ from it, where the bundle's {} pixels did",
        differing_pixels(&stock, &undone),
        differing_pixels(&stock, &applied)
    );

    // Applied once more, then the package is taken away: this device falls back to the theme the
    // mode shows, and the appearance still names the package for the devices that have it.
    session
        .invoke_with_actor(
            &common::as_person(PERSON),
            APPEARANCE,
            "apply-theme",
            &json!({ "package": PACKAGE }),
        )
        .expect("the action runs");
    step(&mut session, 300);
    uninstall_theme_package(&store);
    session
        .invoke(
            APPEARANCE,
            "set-mode",
            &arguments(json!({ "value": "light" })),
        )
        .expect("the action runs");
    step(&mut session, 100);
    session
        .invoke(
            APPEARANCE,
            "set-mode",
            &arguments(json!({ "value": "dark" })),
        )
        .expect("the action runs");
    step(&mut session, 300);
    assert_eq!(
        published(&session),
        json!(PACKAGE),
        "a device without the package still reads the person's choice"
    );
    // What Settings' Appearance page reads to say the theme is missing: the package is named and
    // is not among the device's installed theme packages.
    let installed_packages = settings_sections::service::theme_packages::ThemePackages::installed();
    assert!(
        !installed_packages.has(PACKAGE),
        "the device has no such package: {installed_packages:?}"
    );
    // The appearance resolves the theme the mode names, exactly as the session draws it.
    let prefs: settings_sections::prefs::AppearancePrefs = serde_json::from_value(
        session
            .read(APPEARANCE)
            .expect("the session publishes the appearance section"),
    )
    .expect("the section reads");
    assert_eq!(
        prefs.user_document().map(|document| document.id),
        Some("tilo-dark".to_owned()),
        "without the package the theme the mode shows is what the device draws"
    );
    let missing = frame(&mut session);
    assert!(
        differing_pixels(&stock, &missing) < differing_pixels(&stock, &applied) / 4,
        "and the frame is the stock theme's again: {} pixels differ, where the bundle's {} did",
        differing_pixels(&stock, &missing),
        differing_pixels(&stock, &applied)
    );
}

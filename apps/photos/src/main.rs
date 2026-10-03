//! Photos — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default              — the live winit event loop via `ui::App`.
//!   * `--open PATH`        — start with that photograph open.
//!   * `--shots PATH`       — render one offscreen frame to a PNG and exit.
//!     `--scenario NAME` picks the pose (see [`photos::SHOT_SCENARIOS`]),
//!     `--size WxH` the viewport, and `TILO_SHOTS_THEME=light|dark|
//!     high-contrast` the palette.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::collections::HashMap;

use taffy::prelude::*;
use tilo_content::ContentStore;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Default live path: framework-hosted ResourceApp. Shots, imports and
    // explicit file opens keep the standalone widget launcher.
    if !photos::standalone_launch() {
        ui::framework::run::<photos::PhotosShell>();
        return Ok(());
    }
    ui::launch::run(
        &LaunchSpec::new_with_args("so.tilo.photos", live_root)
            .history(&photos::framework_history::HISTORY)
            .open_app_global_state_with_args(open_state)
            .shot_handler(render_shot)
            .allowed_options(&["--size"])
            .allowed_live_flags(&["--analyze-imports", "--library"])
            .allowed_live_options(&["--open", "--import-directory"])
            .max_positionals(1)
            .scenarios(photos::SHOT_SCENARIOS)
            .configure_app(configure_app),
    )?;
    Ok(())
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by the `LaunchSpec::new_with_args` root constructor callback"
)]
fn live_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    // Live and headless launches restore the same persistent image content.
    photos::strings::install();
    let mut app = photos::PhotosApp::new_persistent(ContentStore::for_app());
    if args.value("--import-directory").is_some() || args.has("--library") {
        app.show_library();
    }
    Ok(Box::new(app))
}

fn requested_path(args: &LaunchArgs) -> Option<&str> {
    args.value("--open")
        .or_else(|| args.positionals().first().map(String::as_str))
}

fn open_state(app_id: &'static str, args: &LaunchArgs) -> (ui::State, ui::persist::DocWriter) {
    let content = ContentStore::for_app();
    let (mut state, autosave) = ui::persist::open(app_id);
    if let Some(path) = requested_path(args) {
        let mut opener = photos::PhotosApp::new_persistent(content.clone());
        photos::strings::install();
        opener.open_path(&mut state.doc, path);
    }
    if let Some(directory) = args.value("--import-directory") {
        import_directory_at_startup(
            &mut state.doc,
            &content,
            directory,
            args.has("--analyze-imports"),
        );
    }
    (state, autosave)
}

/// An explicit launch import is useful for first-run migration and acceptance
/// testing. It uses the production bounded indexer and genuine local analysis;
/// no fixture labels, dates, locations, descriptions, or people are injected.
fn import_directory_at_startup(
    doc: &mut ui::automerge::AutoCommit,
    content: &ContentStore,
    directory: &str,
    analyze: bool,
) {
    use std::sync::atomic::AtomicBool;

    let cancelled = AtomicBool::new(false);
    let Ok(report) = photos::import::index_directory(
        std::path::Path::new(directory),
        content,
        photos::import::ImportOptions::default(),
        &cancelled,
        |_| {},
    ) else {
        return;
    };
    let capability = if analyze {
        tilo_vision::discover_local_capability(&agent_provider_config::ProviderConfig::load())
    } else {
        None
    };
    let mut library = photos::doc::PhotosDoc::library(doc);
    for mut indexed in report.photos {
        if analyze {
            let result = capability
                .as_ref()
                .and_then(|model| {
                    photos::analysis::analyze_image(&indexed.photo.image, model, content).ok()
                })
                .or_else(|| {
                    photos::analysis::recognize_text(&indexed.photo.image, "eng", content).ok()
                });
            if let Some(mut analysis) = result {
                "shared".clone_into(&mut analysis.provenance.scope);
                if let Some(provenance) = &mut analysis.ocr_provenance {
                    "shared".clone_into(&mut provenance.scope);
                }
                if let Some(provenance) = &mut analysis.vision_provenance {
                    "shared".clone_into(&mut provenance.scope);
                }
                if !analysis.searchable_text().is_empty() {
                    indexed.photo.analysis = Some(analysis);
                }
            }
        }
        let _ = library.add_photo(indexed.photo);
    }
    photos::doc::PhotosDoc::set_library(doc, &library);
}

fn configure_app(app: &mut ui::App) {
    photos::actions::register(app.action_registry_mut());
    #[cfg(not(debug_assertions))]
    {
        app.debug_mode = false;
    }
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or_else(|| {
        LaunchError::Operation("Photos screenshot handler called without --shots".into())
    })?;
    let scenario = args.scenario.as_deref().unwrap_or("opened");
    let viewport = args
        .value("--size")
        .and_then(parse_size)
        .or(args.viewport)
        .unwrap_or(DEFAULT_SHOTS_VIEWPORT);
    // The shared shot policy: `TILO_SHOTS_THEME`, `--text-scale` /
    // `TILO_SHOTS_TEXT_SCALE` and the reduced-motion preference resolve the
    // same way here as in every app on the standard launcher.
    render_scenario(scenario, viewport, &ui::launch::shot_theme(args), out)
        .map_err(|error| LaunchError::Operation(error.to_string()))
}

/// Parse `WxH`.
fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X', '*'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Render one scenario offscreen. Mirrors `ui::App`'s frame exactly:
/// sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
fn render_scenario(
    scenario: &str,
    viewport: (u32, u32),
    theme: &Theme,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    photos::strings::install();

    // The generated pattern is written beside the output, so a shot run
    // leaves no file anywhere the live app would ever look.
    let staging = out
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("photos-shots");
    let mut state = ui::State::new();
    let mut app = photos::PhotosApp::new();
    photos::seed_scenario(&mut app, &mut state.doc, scenario, &staging);

    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    text_renderer.set_text_scale(theme.text_scale);
    let mut widget = app;

    // Two ordinary frames settle responsive toolbar labels: the first gets the
    // root bounds and the second rebuilds wide labels as icon-only where
    // necessary. A library shot gets a few more *offscreen-only* frames so
    // its real background thumbnail worker can settle; the live UI never
    // waits for a decode.
    let settle_frames = if matches!(scenario, "people" | "people-detail") {
        // People cards intentionally use real library thumbnails rather than
        // avatar placeholders, so let the bounded decoder queue settle.
        20
    } else if matches!(
        scenario,
        "library"
            | "places"
            | "selection"
            | "selection-many-albums"
            | "album-manage"
            | "metadata-edit"
    ) {
        // The live view fills a four-item bounded queue per sync. Acceptance
        // images must let that real queue drain far enough to review the
        // photograph layout itself rather than mostly reviewing placeholders.
        20
    } else {
        2
    };
    let mut scene = Scene::new();
    for _ in 0..settle_frames {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

        widget.sync(&mut state.doc, &state.root);
        let child = widget.build_layout(&mut taffy, theme, &mut node_map);
        let root = taffy.new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        )?;
        {
            let measure_map = node_map.clone();
            let tr = &mut text_renderer;
            taffy.compute_layout_with_measure(
                root,
                taffy::Size {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )?;
        }
        widget.apply_layout(&taffy, child, Point::ORIGIN);

        scene = Scene::new();
        widget.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut text_renderer,
            theme,
            &presence,
            &permission,
        );
        if settle_frames > 2 {
            let _ = ui::rt::sleep_thread(std::time::Duration::from_millis(6));
        }
    }

    widget.draw_overlay(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        theme,
        &presence,
    );

    if let Some(parent) = out.parent()
        && let Ok((dir, name)) = tilo_files::file_at(parent)
    {
        let _ = tilo_files::blocking::create_dir_all(&dir, &name);
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("photos: wrote {scenario} shot to {}", out.display());
    Ok(())
}

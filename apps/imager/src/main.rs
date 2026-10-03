#![allow(
    clippy::cast_lossless,
    clippy::cloned_ref_to_slice_refs,
    clippy::collapsible_if,
    clippy::doc_markdown,
    clippy::double_ended_iterator_last,
    clippy::drop_non_drop,
    clippy::expect_used,
    clippy::format_push_string,
    clippy::ignored_unit_patterns,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    clippy::large_enum_variant,
    clippy::large_stack_arrays,
    clippy::manual_clamp,
    clippy::manual_let_else,
    clippy::many_single_char_names,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_const_for_thread_local,
    clippy::missing_errors_doc,
    clippy::needless_borrow,
    clippy::needless_continue,
    clippy::needless_lifetimes,
    clippy::needless_option_as_deref,
    clippy::needless_pass_by_ref_mut,
    clippy::needless_pass_by_value,
    clippy::nonminimal_bool,
    clippy::or_fun_call,
    clippy::panic_in_result_fn,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::significant_drop_tightening,
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::struct_field_names,
    clippy::suspicious_operation_groupings,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unnecessary_mut_passed,
    clippy::unnecessary_semicolon,
    clippy::unnecessary_wraps,
    clippy::unreachable,
    clippy::unreadable_literal,
    clippy::unused_io_amount,
    clippy::unused_self,
    clippy::unwrap_used,
    clippy::used_underscore_binding,
    clippy::wildcard_imports,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Disk Imager — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default — run the live winit event loop via `ui::App`, starting blank
//!     with a real `lsblk` drive probe.
//!   * `--demo` — run live but seed sample content (drives, a Tilo image,
//!     recent history) for a walkthrough / review.
//!   * `--setup` — run the capability-gated guided setup screens.
//!   * `--import-folders` — run the durable later-folder import surface.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit.
//!     `--scenario NAME` picks which wizard station (defaults to `source`);
//!     `--viewport WxH` sizes it.
//!   * `--shots-dir DIR` — render *every* wizard scenario into `DIR` as
//!     `NN-name.png` (the screenshot gallery).

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
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
        clippy::string_slice,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::collections::HashMap;

use taffy::prelude::*;
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

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1180, 820);

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// # Errors
///
/// Returns an error if a `--shots`/`--shots-dir` render fails: computing the
/// offscreen layout for a scenario, or writing its PNG to disk.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(target_os = "linux")]
    if let Some(code) = imager::setup::migration_native::maybe_run_command(&args)? {
        std::process::exit(code);
    }
    if let Some(code) = imager::setup::desktop_worker::maybe_run_desktop_helper(&args)? {
        std::process::exit(code);
    }
    if let Some(code) = imager::write::maybe_run_write_helper(&args)? {
        std::process::exit(code);
    }
    if let Some(code) = imager::setup::staging::helper::maybe_run_staging_helper(&args)? {
        std::process::exit(code);
    }
    if let Some(code) = imager::setup::staging::final_bootnext::maybe_run(&args)? {
        std::process::exit(code);
    }
    if let Some(code) = imager::setup::staging::cli::maybe_run_staging_request(&args)? {
        std::process::exit(code);
    }
    // Packaging smoke probe: reaching this point proves the executable and
    // all runtime-linked libraries load without starting graphics, portals,
    // device discovery, or a privileged path.
    if args.iter().any(|arg| arg == "--tilo-package-smoke") {
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--tilo-flatpak-probe-smoke") {
        if !cfg!(feature = "flatpak-media-writer")
            || std::env::var("TILO_IMAGER_FLATPAK").as_deref() != Ok("1")
        {
            return Err("the Flatpak probe smoke test is outside its sandbox".into());
        }
        let drives = imager::probe::probe()
            .ok_or("the Flatpak could not reconcile lsblk with host UDisks2 inventory")?;
        if drives.is_empty() {
            return Err("the Flatpak returned an empty drive inventory".into());
        }
        return Ok(());
    }
    let viewport = parse_value_arg(&args, "--viewport")
        .and_then(|s| parse_viewport(&s))
        .unwrap_or(DEFAULT_SHOTS_VIEWPORT);

    // Render every scenario into a directory.
    if let Some(dir) = parse_value_arg(&args, "--shots-dir") {
        let dir = std::path::PathBuf::from(dir);
        let text_scale = parse_value_arg(&args, "--text-scale")
            .as_deref()
            .and_then(parse_text_scale);
        let environment_text_scale = std::env::var("TILO_SHOTS_TEXT_SCALE").ok();
        let theme = standalone_shot_theme(text_scale, environment_text_scale.as_deref());
        std::fs::create_dir_all(&dir).ok();
        for (i, scenario) in imager::SHOT_SCENARIOS.iter().enumerate() {
            let out = dir.join(format!("{:02}-{}.png", i + 1, scenario));
            render_scenario(scenario, viewport, &theme, &out)?;
        }
        return Ok(());
    }

    // Default live path: ResourceApp host. Captures, helpers and smoke probes
    // above keep the standalone launch path.
    if !args.iter().any(|arg| {
        [
            "--shots",
            "--shots-dir",
            "--responsive-matrix",
            "--native-audit",
            "--headless",
            "--demo",
            "--setup",
            "--import-folders",
        ]
        .iter()
        .any(|flag| arg == *flag || arg.starts_with(&format!("{flag}=")))
    }) {
        // The drive list comes from lsblk only when this is set. Captures
        // and `--demo` leave it unset and stay on the seeded document.
        enable_live_drive_probe();
        ui::framework::run::<imager::ImagerShell>();
        return Ok(());
    }
    ui::launch::run(
        &LaunchSpec::new_with_args("so.tilo.imager", live_root)
            .runtime_attachment(|_| !cfg!(target_os = "windows"))
            .shot_handler(render_shot)
            .shot_root(shot_root)
            .allowed_live_flags(&["--demo", "--setup", "--import-folders"])
            .scenarios(imager::SHOT_SCENARIOS)
            .open_state_with_args(open_state)
            .configure_app(|app| imager::actions::register(app.action_registry_mut())),
    )?;
    Ok(())
}

#[allow(
    clippy::disallowed_methods,
    reason = "set once before the framework host constructs the drive probe"
)]
fn enable_live_drive_probe() {
    unsafe { std::env::set_var("IMAGER_LIVE", "1") };
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by LaunchSpec::new_with_args"
)]
#[allow(
    clippy::disallowed_methods,
    reason = "the live-mode switch is set once, before the root widget and the drive \
              probe that reads IMAGER_LIVE are constructed"
)]
fn live_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    if args.has("--import-folders") {
        return Ok(imager::setup::import_view::app_root());
    }
    if args.has("--setup") {
        return Ok(imager::setup::app_root());
    }
    if !args.has("--demo") {
        // Set once at startup, before the drive probe can start worker threads.
        unsafe { std::env::set_var("IMAGER_LIVE", "1") };
    }
    if args.has("--demo") {
        let mut app = imager::ImagerApp::new();
        app.seed_local_demo_state();
        Ok(Box::new(app))
    } else {
        Ok(imager::app_root())
    }
}

/// The widget a `--headless-drive --scenario NAME` drive runs against: the
/// same posed fixture `--shots --scenario NAME` renders, so a geometry dump
/// describes the screenshot rather than a blank live app.
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by LaunchSpec::shot_root"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(imager::app_root_for_shot(
        args.scenario.as_deref().unwrap_or("source"),
    ))
}

/// The document every non-screenshot mode opens. `--demo` seeds the sample
/// content into the persisted store, as before. A `--headless-drive
/// --scenario NAME` fixture instead gets an in-memory document posed exactly
/// as `render_scenario` poses it for `--shots --scenario NAME`, with
/// persistence disabled: a geometry dump describes the screenshot and can
/// never leave sample content behind in the user's store for the live app to
/// load. The launcher hands the headless drive through this opener (with
/// `args.scenario` set) since `ux1x/integration`; an older launcher drives the
/// fixture over an unposed `State::new()` document.
fn open_state(app_id: &'static str, args: &LaunchArgs) -> (ui::State, ui::persist::DocWriter) {
    if let Some(scenario) = args.scenario.as_deref() {
        let mut state = ui::State::new();
        imager::seed_scenario(&mut state.doc, scenario);
        return (
            state,
            ui::persist::DocWriter::disabled("screenshot fixture; nothing to persist"),
        );
    }
    let (mut state, autosave) = ui::persist::open(app_id);
    if args.has("--demo") {
        imager::doc::ImagerDoc::seed_demo(&mut state.doc);
    }
    (state, autosave)
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or(LaunchError::ShotsUnsupported)?;
    render_scenario(
        args.scenario.as_deref().unwrap_or("source"),
        args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT),
        &ui::launch::shot_theme(args),
        out,
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
}

/// Parse a `--key VALUE` or `--key=VALUE` flag.
fn parse_value_arg(args: &[String], key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == key {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix(&prefix) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Parse a `WIDTHxHEIGHT` viewport spec (e.g. `1180x820`).
fn parse_viewport(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Seed `scenario`, run one offscreen frame, and write the PNG.
fn render_scenario(
    scenario: &str,
    viewport: (u32, u32),
    theme: &Theme,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    text_renderer.set_text_scale(theme.text_scale);
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut widget = imager::app_root_for_shot(scenario);
    let mut state = ui::State::new();
    imager::seed_scenario(&mut state.doc, scenario);
    widget.sync(&mut state.doc, &state.root);

    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
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

    let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
    // SAFETY: the bound nodes come from this exact exclusively owned build.
    unsafe {
        bindings.rebuild(widget.as_ref(), &taffy, child, &node_map);
        ui::widget::compute_widget_layout(
            &mut taffy,
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            &node_map,
            &bindings,
            &mut text_renderer,
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("imager: wrote {} shot to {}", scenario, out.display());
    Ok(())
}

fn standalone_shot_theme(text_scale: Option<f32>, environment_text_scale: Option<&str>) -> Theme {
    let theme = std::env::var("TILO_SHOTS_THEME")
        .ok()
        .and_then(|name| Theme::named(&name))
        .unwrap_or_else(Theme::dark)
        .with_text_scale(ui::launch::shot_text_scale(
            text_scale,
            environment_text_scale,
        ));
    let reduced = [
        "TILO_SHOTS_REDUCE_MOTION",
        "TILO_A11Y_REDUCE_MOTION",
        "TILO_REDUCE_MOTION",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok())
    .any(|value| {
        let value = value.trim();
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    });
    theme.with_reduce_motion(reduced)
}

fn parse_text_scale(value: &str) -> Option<f32> {
    let scale: f32 = value.parse().ok()?;
    (scale.is_finite() && (0.75..=2.0).contains(&scale)).then_some(scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shot text scale parsing and precedence.
    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the scales are parsed from exact decimal literals and passed through unchanged"
    )]
    fn shot_text_scale_is_validated_and_cli_overrides_environment() {
        // Phase `shot_text_scale_rejects_unsafe_values`.
        {
            assert_eq!(parse_text_scale("1.3"), Some(1.3));
            for value in ["NaN", "0.5", "2.1", "not-a-number"] {
                assert_eq!(parse_text_scale(value), None, "accepted {value}");
            }
        }

        // Phase `standalone_shot_scale_uses_environment_unless_cli_overrides_it`.
        {
            assert_eq!(standalone_shot_theme(None, Some("1.25")).text_scale, 1.25);
            assert_eq!(
                standalone_shot_theme(Some(2.0), Some("1.25")).text_scale,
                2.0
            );
        }
    }
}

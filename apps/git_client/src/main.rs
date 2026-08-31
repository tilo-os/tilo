//! Git — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default               — run the live winit event loop via `ui::App`.
//!   * `--shots PATH`        — render one offscreen frame at the wide viewport
//!     to a PNG and exit (the docked side-by-side
//!     layout). Identical passes to the live app.
//!   * `--shots-narrow PATH` — render one offscreen frame at a narrow viewport
//!     with the primary list column revealed as an
//!     overlay flap over its scrim, exercising the
//!     responsive collapse.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
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
/// Below the breakpoint the primary list column collapses into an overlay
/// flap. Sized so the narrow capture is unambiguously below `NARROW_BREAKPOINT`
/// and tall enough to show the diff detail behind the flap.
const NARROW_SHOTS_VIEWPORT: (u32, u32) = (680, 860);

/// Runs the git client binary: the live app, a headless screenshot capture,
/// a headless click/keystroke drive, or `--stage-commit`.
///
/// # Errors
///
/// Returns an error if `--stage-commit` fails to open the repository, stage
/// its changes or create the commit, or if a `--shots`/`--shots-narrow`
/// capture fails to render its frame or write the PNG.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();

    // `--stage-commit "<message>"` is a headless developer/CI helper: it
    // opens the repo at `GIT_CLIENT_REPO` (or the CWD), stages every change
    // and commits it through the EXACT production `git2` actions the UI uses,
    // then prints the new commit id. Used to prove (and screenshot) a real
    // commit made by this binary. It performs no rendering.
    if let Some(msg) = parse_flag_value(&args, "--stage-commit") {
        return stage_and_commit(&msg.to_string_lossy());
    }

    let wide = parse_flag_value(&args, "--shots");
    let narrow = parse_flag_value(&args, "--shots-narrow");
    if wide.is_some() || narrow.is_some() {
        // The headless capture renders a deterministic, fully populated
        // state. By default that is the bundled sample fixture (so a shot
        // never depends on whatever repo surrounds the CWD). But when
        // `GIT_CLIENT_REPO` is set explicitly, capture that REAL repository
        // instead — used to screenshot the live `git2`-backed app (e.g. a
        // commit just made in a prepared temp repo) for verification.
        let real_repo = std::env::var_os("GIT_CLIENT_REPO").is_some();
        if !real_repo {
            unsafe {
                std::env::set_var("GIT_CLIENT_SAMPLE", "1");
            }
        }
        if let Some(out) = wide {
            render_shot_to_png(git_client::app_root(), DEFAULT_SHOTS_VIEWPORT, &out)?;
        }
        if let Some(out) = narrow {
            // Reveal the overlay flap so the single frame shows the collapsed
            // sidebar floating over its scrim rather than the closed default.
            unsafe {
                std::env::set_var("GIT_CLIENT_FLAP_OPEN", "1");
            }
            render_shot_to_png(git_client::app_root(), NARROW_SHOTS_VIEWPORT, &out)?;
        }
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(
            &spec,
            git_client::app_root(),
            "dev.qos.git-client",
        ));
    }

    let (state, autosave) = ui::persist::open("dev.qos.git-client");
    let mut app = ui::App::new(state, git_client::app_root()).with_autosave(autosave);
    // Register the client's preference actions so the shell's Ctrl+K palette can
    // decode + replay the same view-preference actions the on-screen controls
    // dispatch.
    git_client::actions::register(app.action_registry_mut());
    app.run();
    Ok(())
}

/// Stage every change and commit it through the production `git2` actions,
/// against the repo at `GIT_CLIENT_REPO` (or the current directory). Prints
/// the new short commit id. This drives `git_client::repo::stage_all` +
/// `git_client::repo::commit` — the same functions the UI's Stage-all and
/// Commit buttons invoke.
fn stage_and_commit(message: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var("GIT_CLIENT_REPO").map_or_else(
        |_| std::env::current_dir().unwrap_or_else(|_| ".".into()),
        std::path::PathBuf::from,
    );
    git_client::repo::stage_all(&path)?;
    let id = git_client::repo::commit(&path, message)?;
    println!("git_client: committed {id} to {}", path.display());
    Ok(())
}

/// Parse `--flag VALUE` or `--flag=VALUE` for the given flag name.
fn parse_flag_value(args: &[String], flag: &str) -> Option<std::path::PathBuf> {
    let eq_prefix = format!("{flag}=");
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix(&eq_prefix) {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    // `QOS_SHOTS_THEME=light|high-contrast` renders the shot under that theme
    // so the same frame can be graded in both light and dark (default dark).
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();
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
    println!("git_client: wrote shot to {}", out.display());
    Ok(())
}

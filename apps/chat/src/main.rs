//! Chat — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!     (headless screenshot; identical passes to the live app).
//!
//! Optional flags for `--shots` (used to capture interaction states the
//! single-frame renderer can't reach through live input):
//!   * `--scenario NAME` — stage a frame (`narrow`, `context`); see
//!     [`chat::app_root_for_shot`].
//!   * `--viewport WxH`  — render at a custom size (e.g. `--viewport 760x900`
//!     to exercise the narrow-window overlay flaps).

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
use std::sync::Arc;

use taffy::prelude::*;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{FloatingSurfaceWidgetExt as _, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

fn open_state(app_id: &'static str) -> (ui::State, ui::persist::DocWriter) {
    // An older release kept the workspace in its own file. Hand that path to
    // `open_migrating` so existing history is merged into the shared store on
    // first launch rather than orphaned; the merge is idempotent.
    match chat::storage::data_dir() {
        Some(dir) => ui::persist::open_migrating(app_id, &chat::storage::workspace_path(&dir)),
        None => ui::persist::open(app_id),
    }
}

fn shot_handler(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args
        .shots
        .as_deref()
        .ok_or_else(|| LaunchError::Operation("missing screenshot output".into()))?;
    let mut app = chat::ChatApp::new_ephemeral();
    let scenario = args.scenario.as_deref().unwrap_or_default();
    app.seed_shot_scenario(scenario);
    render_shot_to_png(
        app,
        args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT),
        ui::launch::shot_theme(args),
        out,
        scenario,
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    // qdoc blobs are content-addressed attachments. Their durable cache keeps
    // a restarted chat able to serve the same portable hash, while qdoc's
    // transport layer handles sharing it with collaborators.
    qdoc::blobstore::set_store(Arc::new(qdoc::blobstore::FsBlobStore::new(
        qdoc::blobstore::FsBlobStore::default_root(),
    )));
    ui::launch::run_or_exit(
        &LaunchSpec::new("so.tilo.chat", chat::app_root)
            .shot_handler(shot_handler)
            .scenarios(&[
                "narrow",
                "history",
                "compact",
                "starter",
                "people",
                "thread-narrow",
                "collaboration",
                "context",
                "msgmenu",
                "attach",
                "attach-loading",
                "attach-error",
                "edit",
                "composer",
                "call",
                "share",
                "floating-call-attached",
                "floating-call",
                "floating-call-hover",
                "floating-call-returning",
                "floating-call-returned",
            ])
            .open_state(open_state)
            .configure_app(|app| chat::actions::register(app.action_registry_mut())),
    );
}

/// Run one offscreen frame against `app` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle (sync → `build_layout` → `compute_layout` →
/// `apply_layout` → draw) and additionally pre-warms the attachment image cache
/// so the single frame paints real decoded thumbnails rather than a loading
/// placeholder.
fn render_shot_to_png(
    mut app: chat::ChatApp,
    viewport: (u32, u32),
    theme: Theme,
    out: &std::path::Path,
    scenario: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    text_renderer.set_text_scale(theme.text_scale);
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);
    app.prewarm_images();
    let widget: &mut dyn Widget = &mut app;

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
    // SAFETY: `widget`, `taffy`, `child`, and `node_map` were built together
    // and remain exclusively owned for the duration of layout.
    unsafe {
        bindings.rebuild(widget, &taffy, child, &node_map);
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
    app.debug_stage_floating_call_for_shot(scenario);
    let widget: &mut dyn Widget = &mut app;

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );
    widget.draw_floating_surfaces(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
    );

    let parent = out.parent().unwrap_or_else(|| std::path::Path::new("."));
    if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent).ok();
    }
    // Render into a sibling temp path and publish only after PNG encoding
    // returns. A killed headless renderer must leave no valid-looking but
    // truncated screenshot at the requested path for visual review tools to
    // mistake for an inspected frame.
    let name = out
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("chat.png");
    let tmp = parent.join(format!(".{name}.partial-{}", std::process::id()));
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, &tmp);
    std::fs::rename(&tmp, out)?;
    println!("chat: wrote shot to {}", out.display());
    Ok(())
}

//! Mail — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!                       (headless screenshot; identical passes to the live app).

use std::collections::HashMap;

use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        // Optional `--scenario <name>` selects a pre-seeded state + viewport so
        // a single offscreen frame can showcase a specific feature
        // (e.g. `compose`, `narrow`). Default renders the plain mailbox.
        let scenario = parse_scenario_arg(&args);
        // `--shots` always renders the hermetic sample corpus (the live root
        // has no fabricated mail and would render empty offscreen).
        let (root, viewport) = match scenario.as_deref() {
            Some(name) => (mail::app_root_scenario(name), mail::shot_viewport(name)),
            None => (mail::app_root_scenario("wide"), DEFAULT_SHOTS_VIEWPORT),
        };
        render_shot_to_png(root, viewport, &out)?;
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(spec, mail::app_root(), "dev.qos.mail"));
    }

    let (state, autosave) = ui::persist::open("dev.qos.mail");
    let mut app = ui::App::new(state, mail::app_root()).with_autosave(autosave);
    // Register Mail's actions so the shell's Ctrl+K palette can decode + replay
    // the same flag / move / label / folder / draft actions the on-screen
    // affordances dispatch.
    mail::actions::register(app.action_registry_mut());
    app.run();
    Ok(())
}

fn parse_shots_arg(args: &[String]) -> Option<std::path::PathBuf> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--shots" {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix("--shots=") {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
}

fn parse_scenario_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--scenario" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--scenario=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → build_layout → compute_layout → apply_layout → draw.
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

    // One full pass (sync → build → compute → apply → draw), then re-run it while
    // the widget still wants animation frames — bounded — so asynchronously
    // loaded content (an HTML mail's inline images decode off-thread) settles
    // into the single captured frame instead of showing a loading placeholder.
    // This mirrors what the live event loop does; a hermetic shot has no pump.
    let mut scene = Scene::new();
    // Bound the settle by a re-draw count, not wall-clock: the very first pass
    // is slow (font-system init + shaping the whole mailbox), which would blow a
    // time budget before an async image ever gets a second frame. Each extra
    // pass reuses the shaping cache, so it is cheap.
    let mut settle_passes = 0u32;
    loop {
        taffy = TaffyTree::new();
        node_map = HashMap::new();
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
        scene = Scene::new();
        widget.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut text_renderer,
            &theme,
            &presence,
            &permission,
        );
        widget.mark_drawn();
        // Settle once nothing wants another frame (or after a bounded number of
        // extra passes — ~1.5s of settling for async images to decode).
        if !widget.needs_animation_frame() || settle_passes >= 40 {
            break;
        }
        settle_passes += 1;
        std::thread::sleep(std::time::Duration::from_millis(40));
    }

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("mail: wrote shot to {}", out.display());
    Ok(())
}

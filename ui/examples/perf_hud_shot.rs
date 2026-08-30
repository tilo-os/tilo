//! Offscreen render of the **profiling HUD** overlaid on a real app scene, so
//! the HUD can be inspected without opening a window.
//!
//! `QOS_PROFILE=1` draws the HUD live, but the live path needs a display. This
//! example reproduces exactly what `ui::App` does — run the real five passes,
//! record each into a [`ui::profiling::FrameProfiler`], commit a per-frame
//! sample — for a rolling window of frames, then composites the production
//! [`FrameProfiler::draw_hud`] over the final frame and rasterises a PNG. The
//! HUD you see here is pixel-for-pixel the one the live app paints; only the
//! surface is offscreen.
//!
//! Run:  cargo run -p ui --example ui-shots -- perf_hud_shot
//! Out:  $PERF_HUD_OUT (default /tmp/perf_hud/hud.png)
//!
//! `PERF_HUD_FRAMES=N` sets how many real frames feed the rolling graph.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::profiling::{FrameProfiler, Pass};
use ui::state::State;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use super::gallery_scene;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;

pub fn run(_args: &[String]) {
    ui::i18n::I18nManager::init("en-US");

    let out = std::env::var("PERF_HUD_OUT").unwrap_or_else(|_| "/tmp/perf_hud/hud.png".to_string());
    let out = PathBuf::from(out);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let frames: usize = std::env::var("PERF_HUD_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(90);

    let theme = Theme::dark();
    let mut state = State::new();
    let mut root = gallery_scene::build_showcase(&mut state, &theme);

    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::empty();
    let mut prof = FrameProfiler::for_capture();
    let mut scene = Scene::new();

    // Roll a window of real frames so the HUD's graph and per-pass readout are
    // built from genuine measurements, not synthetic numbers. Each iteration is
    // one full frame: the same sync → build_layout → apply_layout → draw the
    // live app runs, timed into the profiler exactly as `App` times it.
    for _ in 0..frames {
        let t = Instant::now();
        root.sync(&mut state.doc, &state.root);
        prof.record(Pass::Sync, t.elapsed());

        // build_layout = widget-tree build + the taffy solve (one pass, as in
        // `App`'s RedrawRequested handler).
        let t = Instant::now();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = root.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout_with_measure(
                node,
                Size {
                    width: AvailableSpace::Definite(WIDTH as f32),
                    height: AvailableSpace::Definite(HEIGHT as f32),
                },
                |known, available, node_id, _, _| {
                    node_map
                        .get(&node_id)
                        .map(|&p| unsafe { (*p).measure(known, available, &mut tr) })
                        .unwrap_or(Size::ZERO)
                },
            )
            .unwrap();
        prof.record(Pass::BuildLayout, t.elapsed());

        let t = Instant::now();
        root.apply_layout(&taffy, node, Point::ZERO);
        prof.record(Pass::ApplyLayout, t.elapsed());

        let t = Instant::now();
        scene.reset();
        root.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        prof.record(Pass::Draw, t.elapsed());

        prof.commit_frame_summed();
    }

    // Overlay the production HUD on the final frame's scene, then rasterise it.
    prof.draw_hud(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        (WIDTH as f64, HEIGHT as f64),
    );
    let mut r = OffscreenRenderer::new(WIDTH, HEIGHT);
    r.render_png(&scene, theme.bg_app, &out);
    println!(
        "wrote {} ({WIDTH}x{HEIGHT} px, {frames} frames)",
        out.display()
    );
}

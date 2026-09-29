//! Open-to-first-paint profiler for the PDF viewer.
//!
//! Times the real open path stage by stage — `source::open` (engine parse,
//! per-page size probe, metadata, outline), then the first frame's
//! sync/layout/draw (the draw is where visible pages + rail thumbnails are
//! rasterised). Reports the wall-clock cost a user waits between "open file"
//! and "first painted page".
//!
//! Run: `cargo run -p pdf-viewer --example open_profile -- <FILE.pdf> [WxH]`
//! Set `TILO_PDF_PROFILE=1` for the intra-`source::open` breakdown.

use std::collections::HashMap;
use ui::time::Instant;

use pdf_viewer::PdfApp;
use taffy::prelude::*;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| {
        eprintln!("usage: open_profile <FILE.pdf> [WxH]");
        std::process::exit(2);
    });
    // u16 (not u32): a profiler viewport is a pixel size, always well under
    // 65536, and it keeps the width/height -> f32 conversion below lossless.
    let (w, h): (u16, u16) = args
        .get(2)
        .and_then(|s| s.split_once('x'))
        .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
        .unwrap_or((1280, 800));

    // ---- open (source::open runs synchronously in PdfApp::open) ----
    let t_open = ui::time::now();
    let mut widget: Box<dyn Widget> = Box::new(PdfApp::open(&path));
    let open_ms = ms(t_open);

    // ---- first frame: sync + layout + draw (draw rasterises visible pages) ----
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut state = ui::State::new();

    let t = ui::time::now();
    widget.sync(&mut state.doc, &state.root);
    let sync_ms = ms(t);

    let t = ui::time::now();
    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(f32::from(w)),
                height: Dimension::length(f32::from(h)),
            },
            ..Default::default()
        },
        &[child],
    )?;
    let build_ms = ms(t);

    let t = ui::time::now();
    {
        let measure_map = node_map.clone();
        let tr = &mut text_renderer;
        taffy.compute_layout_with_measure(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(f32::from(w)),
                height: AvailableSpace::Definite(f32::from(h)),
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
    let layout_ms = ms(t);

    // First real paint: this is where rasterize_visible runs (the engine renders
    // the first visible page + any visible rail thumbnails).
    let t = ui::time::now();
    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );
    let draw1_ms = ms(t);

    // A second draw shows the warm-cache steady-state (bitmaps cached).
    let t = ui::time::now();
    let mut scene2 = Scene::new();
    widget.draw_with_presence(
        &mut scene2,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );
    let draw2_ms = ms(t);

    let first_paint = open_ms + sync_ms + build_ms + layout_ms + draw1_ms;
    println!("\n==== open-to-first-paint for {path} ({w}x{h}) ====");
    println!("  PdfApp::open (source::open) : {open_ms:8.1} ms");
    println!("  sync                        : {sync_ms:8.1} ms");
    println!("  build_layout                : {build_ms:8.1} ms");
    println!("  compute+apply layout        : {layout_ms:8.1} ms");
    println!("  first draw (raster visible) : {draw1_ms:8.1} ms");
    println!("  ------------------------------------------");
    println!("  OPEN -> FIRST PAINT         : {first_paint:8.1} ms");
    println!("  second draw (warm cache)    : {draw2_ms:8.1} ms");
    Ok(())
}

//! Offscreen render guard for [`Scrollable`]'s visual scrollbar: drives
//! the full `build_layout` → measure → `apply_layout` → draw pipeline with a
//! tall child and asserts the scrollbar actually paints along the right
//! edge. A differential render with the scrollbar disabled proves the
//! painted pixels come from the bar, not from the content.
//!
//! Skips gracefully when no wgpu adapter is available, matching the other
//! offscreen tests. Set `TILO_UI_SCREENSHOT_DIR=<path>` to dump a PNG.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::support::offscreen::OffscreenRenderer;
use std::collections::HashMap;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{Column, Scrollable, Text, Widget};

const W: u32 = 400;
const H: u32 = 300;
const BG: [u8; 4] = [0x0f, 0x17, 0x2a, 0xff];

fn try_new_renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        OffscreenRenderer::new(w, h)
    }))
    .ok()
}

/// Tall column of left-aligned text, far taller than the viewport so the
/// vertical scrollbar must appear.
fn tall_child() -> Box<dyn Widget> {
    let mut col = Column::new().with_intrinsic_height();
    for i in 0..60 {
        col = col.push(Box::new(Text::new(&format!(
            "Row number {i} of the scrolling list"
        ))));
    }
    Box::new(col)
}

/// Drive the 5-pass pipeline and return the rendered RGBA buffer.
#[allow(
    clippy::cast_precision_loss,
    reason = "W and H are the fixed small test-canvas dimensions, exactly representable in f32"
)]
fn render(scrollable: &mut Scrollable, renderer: &mut OffscreenRenderer) -> Vec<u8> {
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();

    let root = scrollable.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(W as f32),
                height: AvailableSpace::Definite(H as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = node_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    Size::ZERO
                }
            },
        )
        .expect("taffy compute_layout");
    scrollable.apply_layout(&taffy, root, Point::ORIGIN);

    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    scrollable.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    renderer.render(&scene, Color::from_rgb8(BG[0], BG[1], BG[2]))
}

/// Count pixels differing from the background in the rightmost `strip`
/// columns — where the vertical scrollbar lives.
fn right_edge_non_bg(rgba: &[u8], strip: u32) -> usize {
    let mut count: usize = 0;
    for y in 0..H {
        for x in W.saturating_sub(strip)..W {
            let idx = (y.saturating_mul(W).saturating_add(x).saturating_mul(4)) as usize;
            if rgba[idx..idx.saturating_add(4)] != BG {
                count = count.saturating_add(1);
            }
        }
    }
    count
}

#[test]
fn scrollbar_paints_along_right_edge() {
    let Some(mut renderer) = try_new_renderer(W, H) else {
        eprintln!("no wgpu adapter — skipping scrollbar_paints_along_right_edge");
        return;
    };

    // With the scrollbar (default on).
    let mut with_bar = Scrollable::new_vertical().with_child(tall_child());
    let rgba_with = render(&mut with_bar, &mut renderer);

    // Same content, scrollbar suppressed.
    let mut without_bar = Scrollable::new_vertical()
        .with_child(tall_child())
        .with_scrollbar(false);
    let rgba_without = render(&mut without_bar, &mut renderer);

    // Content is left-aligned, so the no-bar render leaves the right-edge
    // strip on the background — it must light up only with the bar on.
    let with_px = right_edge_non_bg(&rgba_with, 12);
    let without_px = right_edge_non_bg(&rgba_without, 12);

    assert!(
        with_px > 100,
        "scrollbar should paint substantial pixels on the right edge, got {with_px}",
    );
    assert!(
        with_px > without_px * 4 + 50,
        "scrollbar render must add far more right-edge pixels than the bar-less render \
         (with={with_px}, without={without_px})",
    );

    if let Ok(dir) = tilo_ui::env::var("TILO_UI_SCREENSHOT_DIR") {
        let _ = std::fs::create_dir_all(&dir);
        for (name, buf) in [
            ("scrollbar_on", &rgba_with),
            ("scrollbar_off", &rgba_without),
        ] {
            let path = format!("{dir}/{name}.png");
            tilo_ui::png::write_rgba8(&path, W, H, buf).unwrap();
            eprintln!("wrote {path}");
        }
    }
}

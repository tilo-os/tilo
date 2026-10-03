#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "in a test these are the assertion, not a hazard; pixel probes round f64 rects to u32"
)]

use std::time::Duration;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::toast::{ToastCard, ToastKind, ToastStack, paint_card};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;

#[test]
fn toast_stack_expires_caps_and_registers_its_edge() {
    // Case `toast_stack_expires_entries`.
    {
        let mut s = ToastStack::new();
        s.push("Workspace created", ToastKind::Info);
        s.push("Save failed", ToastKind::Error);
        assert_eq!(s.len(), 2);
        s.tick(Duration::from_millis(3100)); // default 3s TTL elapsed
        assert_eq!(s.len(), 0);
    }

    // Case `toast_stack_caps_entries`.
    {
        let mut s = ToastStack::new().with_max(3);
        for i in 0..5 {
            s.push(format!("t{i}"), ToastKind::Info);
        }
        assert_eq!(s.len(), 3);
        assert!(s.iter().any(|t| t.text == "t4"));
        assert!(!s.iter().any(|t| t.text == "t0"));
    }

    // Case `toast_card_edge_registers_in_both_themes`.
    // HIG §9.5 "Rest edges": a settled toast card is a Panel whose edge is
    // derived against the ground and visibly moves against the card's own
    // fill, at 1× on the dark and the light preset alike. The declared
    // `border_standard` it replaced sat within rounding distance of `raised`
    // on the dark preset and read as no edge at all. Set
    // `TILO_UI_SCREENSHOT_DIR` to dump `toast_card_{dark,light}.png`.
    {
        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            let mut renderer = OffscreenRenderer::new(CANVAS_W, CANVAS_H);
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            let rect = Rect::new(
                CARD_INSET,
                CARD_INSET,
                f64::from(CANVAS_W) - CARD_INSET,
                f64::from(CANVAS_H) - CARD_INSET,
            );
            let mut card = ToastCard::new("Build finished", ToastKind::Success);
            card.body = "12 tests passed in 3.4 s";
            card.closable = true;
            paint_card(&mut scene, Affine::IDENTITY, &mut tr, &theme, rect, &card);
            let rgba = renderer.render(&scene, theme.bg_app);
            dump(&format!("toast_card_{name}"), &rgba);

            // Right edge, mid-height: clear of the accent stripe, the dismiss
            // button and the corner radius.
            let y = CANVAS_H / 2;
            let x_edge = ((f64::from(CANVAS_W) - CARD_INSET) as u32).saturating_sub(1);
            let edge = OffscreenRenderer::sample(&rgba, CANVAS_W, x_edge, y);
            let inside = OffscreenRenderer::sample(&rgba, CANVAS_W, x_edge.saturating_sub(6), y);
            assert_eq!(edge.3, 0xff, "{name}: edge pixel is opaque");
            assert_eq!(inside.3, 0xff, "{name}: fill pixel is opaque");
            let delta = (luminance_of(edge) - luminance_of(inside)).abs();
            eprintln!("toast/{name}: edge {edge:?} vs fill {inside:?} → Δlum={delta:.4}");
            assert!(
                delta >= tilo_ui::tokens::colors::HAIRLINE_MIN_DELTA,
                "{name}: toast edge {edge:?} does not register against its fill {inside:?} \
             (Δlum={delta:.4} < {})",
                tilo_ui::tokens::colors::HAIRLINE_MIN_DELTA
            );
        }
    }
}

const CANVAS_W: u32 = 420;
const CANVAS_H: u32 = 120;
/// Room around the card so its outermost edge pixel is sampleable.
const CARD_INSET: f64 = 20.0;

fn luminance_of(px: (u8, u8, u8, u8)) -> f64 {
    tilo_ui::a11y::relative_luminance(Color::from_rgba8(px.0, px.1, px.2, px.3))
}

fn dump(name: &str, rgba: &[u8]) {
    let Ok(dir) = tilo_ui::env::var("TILO_UI_SCREENSHOT_DIR") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let path = format!("{dir}/{name}.png");
    tilo_ui::png::write_rgba8(&path, CANVAS_W, CANVAS_H, rgba).unwrap();
    eprintln!("wrote {path}");
}

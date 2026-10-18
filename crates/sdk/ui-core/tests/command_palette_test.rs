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

use std::sync::{Arc, Mutex};
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::Widget;
use tilo_ui::widget::command_palette::{CommandEntry, CommandPalette};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;

#[test]
fn command_palette_filters_and_registers_its_panel_edge() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = tilo_ui::motion::testing::settled();
    // Case `command_palette_filters_by_query`.
    {
        let mut p = CommandPalette::new();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let h = hits.clone();
        p.push(
            CommandEntry::new("ws.create", "Create Workspace")
                .on_select(move |_, _| h.lock().unwrap().push("create")),
        );
        let h = hits.clone();
        p.push(
            CommandEntry::new("ws.rename", "Rename Workspace")
                .on_select(move |_, _| h.lock().unwrap().push("rename")),
        );
        p.set_query("ren");
        assert_eq!(p.visible_entries().len(), 1);
        assert_eq!(p.visible_entries()[0].label, "Rename Workspace");
    }

    // Case `command_palette_custom_filter_is_composed`.
    {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("file.open", "Open File"));
        p.push(CommandEntry::new("ws.create", "Create Workspace"));
        p.set_filter(Box::new(|e| e.id.starts_with("ws.")));
        p.set_query("create");
        assert_eq!(p.visible_entries().len(), 1);
        assert_eq!(p.visible_entries()[0].id, "ws.create");
    }

    // Case `command_palette_panel_edge_registers_in_both_themes`.
    // HIG §9.5 "Rest edges": the command palette is a Panel whose edge derives
    // against the ground and visibly moves against the palette's own fill, at
    // 1× on the dark and the light preset alike. The declared `border_standard`
    // it replaced sat within rounding distance of `raised` on the dark preset
    // and read as no edge at all. `TILO_UI_SCREENSHOT_DIR` dumps
    // `command_palette_{dark,light}.png`.
    {
        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            let mut p = CommandPalette::new();
            p.push(CommandEntry::new("ws.create", "Create Workspace"));
            p.push(CommandEntry::new("ws.rename", "Rename Workspace"));
            p.push(CommandEntry::new("file.open", "Open File"));
            p.open();
            p.set_rect(Rect::new(
                PALETTE_INSET,
                PALETTE_INSET,
                f64::from(CANVAS_W) - PALETTE_INSET,
                f64::from(CANVAS_H) - PALETTE_INSET,
            ));

            let mut renderer = OffscreenRenderer::new(CANVAS_W, CANVAS_H);
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            let presence = PresenceCtx::default();
            p.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            let rgba = renderer.render(&scene, theme.tokens.colors.surfaces.content);
            dump(&format!("command_palette_{name}"), &rgba);

            // Left edge, mid-height: clear of the corner radius and of any
            // row text, which starts well inside the panel padding.
            let y = CANVAS_H / 2;
            let x_edge = PALETTE_INSET as u32;
            let edge = OffscreenRenderer::sample(&rgba, CANVAS_W, x_edge, y);
            let inside = OffscreenRenderer::sample(&rgba, CANVAS_W, x_edge.saturating_add(6), y);
            assert_eq!(edge.3, 0xff, "{name}: edge pixel is opaque");
            assert_eq!(inside.3, 0xff, "{name}: fill pixel is opaque");
            let delta = (luminance_of(edge) - luminance_of(inside)).abs();
            eprintln!("command-palette/{name}: edge {edge:?} vs fill {inside:?} → Δlum={delta:.4}");
            assert!(
                delta >= tilo_ui::tokens::colors::HAIRLINE_MIN_DELTA,
                "{name}: palette edge {edge:?} does not register against its fill {inside:?} \
             (Δlum={delta:.4} < {})",
                tilo_ui::tokens::colors::HAIRLINE_MIN_DELTA
            );
        }
    }
}

const CANVAS_W: u32 = 560;
const CANVAS_H: u32 = 420;
/// Room around the palette so its outermost edge pixel is sampleable.
const PALETTE_INSET: f64 = 20.0;

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

//! Offscreen render of the **entire** [`ui::icons::Icon`] set in a labelled
//! grid, in both light and dark, so every glyph in the unified icon set can be
//! eyeballed for correct placement without opening a window.
//!
//! The grid is driven straight off [`Icon::ALL`], so a newly-added variant
//! shows up here automatically. Each cell paints the glyph through the same
//! `Icon::draw` helper every app uses (themed stroke colour, 24×24 lucide
//! grid) and prints its lucide name below.
//!
//! Run:  cargo run -p ui --example icon_gallery_shots
//! Out:  $ICON_GALLERY_OUT (default /tmp/icon_gallery) —
//!       icons-dark.png, icons-light.png

use std::path::PathBuf;

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::Fill;

use ui::icons::Icon;
use ui::offscreen::OffscreenRenderer;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

const COLS: usize = 8;
const CELL_W: f64 = 152.0;
const CELL_H: f64 = 104.0;
const MARGIN: f64 = 32.0;
const TITLE_H: f64 = 64.0;
const ICON_PX: f64 = 34.0;

fn dims() -> (u32, u32) {
    let rows = Icon::ALL.len().div_ceil(COLS);
    let w = MARGIN * 2.0 + COLS as f64 * CELL_W;
    let h = MARGIN * 2.0 + TITLE_H + rows as f64 * CELL_H;
    (w.round() as u32, h.round() as u32)
}

fn shoot(theme: &Theme, out: PathBuf) {
    let (w, h) = dims();
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    let t = Affine::IDENTITY;

    // Title.
    tr.draw_text(
        &mut scene,
        "ui::icons::Icon — unified Lucide set",
        22.0,
        theme.text_primary,
        Affine::translate((MARGIN, MARGIN + 6.0)),
        None,
    );
    tr.draw_text(
        &mut scene,
        &format!("{} glyphs · 24×24 grid · 1.5px stroke · themable", Icon::ALL.len()),
        13.0,
        theme.text_dim,
        Affine::translate((MARGIN, MARGIN + 34.0)),
        None,
    );

    let grid_top = MARGIN + TITLE_H;
    for (idx, &icon) in Icon::ALL.iter().enumerate() {
        let col = idx % COLS;
        let row = idx / COLS;
        let x0 = MARGIN + col as f64 * CELL_W;
        let y0 = grid_top + row as f64 * CELL_H;
        let cell = Rect::new(x0 + 6.0, y0 + 6.0, x0 + CELL_W - 6.0, y0 + CELL_H - 6.0);

        // Cell surface + hairline so misplacement reads against a known frame.
        let card = RoundedRect::from_rect(cell, 10.0);
        scene.fill(Fill::NonZero, t, theme.bg_primary, None, &card);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            t,
            theme.border_standard,
            None,
            &card,
        );

        // Glyph centred in the upper part of the cell.
        let gx = cell.center().x - ICON_PX / 2.0;
        let gy = cell.y0 + 16.0;
        let glyph_rect = Rect::new(gx, gy, gx + ICON_PX, gy + ICON_PX);
        icon.draw(&mut scene, t, glyph_rect, theme.text_primary);

        // Label centred below the glyph.
        let label = icon.name();
        let fs = 11.0;
        let (lw, _) = tr.measure(label, fs, None);
        let lx = cell.center().x - lw as f64 / 2.0;
        let ly = cell.y1 - 22.0;
        tr.draw_text(&mut scene, label, fs, theme.text_dim, Affine::translate((lx, ly)), None);
    }

    let mut r = OffscreenRenderer::new(w, h);
    r.render_png(&scene, theme.bg_app, &out);
    println!("wrote {} ({w}x{h} px, {} icons)", out.display(), Icon::ALL.len());
}

fn main() {
    ui::i18n::I18nManager::init("en-US");
    let dir = std::env::var("ICON_GALLERY_OUT").unwrap_or_else(|_| "/tmp/icon_gallery".to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let dir = PathBuf::from(dir);
    shoot(&Theme::dark(), dir.join("icons-dark.png"));
    shoot(&Theme::light(), dir.join("icons-light.png"));
}

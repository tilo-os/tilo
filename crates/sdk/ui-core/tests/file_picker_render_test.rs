//! Offscreen render guard for the overhauled [`FilePicker`]: drives the
//! draw pass in both Open and Save mode and asserts non-empty pixels, so
//! the icon / breadcrumb / save-field / filter-chip paths can't silently
//! regress to blank output.
//!
//! Renders on the dark and the light preset and asserts the picker's
//! derived panel edge registers against its own fill in both (HIG §9.5).
//! Skips when no wgpu adapter is available. The PNGs
//! (`file_picker_{open,save}_{dark,light}.png`) are written to
//! `target/ui-review/file-picker`, or `TILO_UI_SCREENSHOT_DIR`.

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

use std::path::PathBuf;
use tilo_ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;

use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::Widget;
use tilo_ui::widget::file_picker::{FileFilter, FilePicker, FilePickerMode};

fn temp_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir(root.join("Pictures")).unwrap();
    std::fs::create_dir(root.join("Documents")).unwrap();
    for f in [
        "photo.png",
        "song.flac",
        "movie.mkv",
        "main.rs",
        "notes.md",
        "data.zip",
    ] {
        std::fs::write(root.join(f), b"x").unwrap();
    }
    (dir, root)
}

/// The ground the picker floats over in each preset: an app's window
/// ground, so the frame reads the way the shell composites it.
fn backdrop(theme: &Theme) -> Color {
    theme.tokens.colors.surfaces.content
}

fn render(
    picker: &mut FilePicker,
    theme: &Theme,
    w: u32,
    h: u32,
    renderer: &mut OffscreenRenderer,
) -> Vec<u8> {
    let mut tr = TextRenderer::new();
    // Inset from the canvas so the panel's outer edge pixel is sampleable
    // against the backdrop on every side.
    picker.set_rect(Rect::new(
        PICKER_INSET,
        PICKER_INSET,
        f64::from(w) - PICKER_INSET,
        f64::from(h) - PICKER_INSET,
    ));
    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    picker.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    renderer.render(&scene, backdrop(theme))
}

/// Room around the picker for the edge probes.
const PICKER_INSET: f64 = 20.0;

fn luminance_of(px: (u8, u8, u8, u8)) -> f64 {
    tilo_ui::a11y::relative_luminance(Color::from_rgba8(px.0, px.1, px.2, px.3))
}

/// HIG §9.5 "Rest edges": the picker's outermost pixel row is a derived
/// panel edge that visibly moves against the fill just inside it — the
/// declared `border_standard` it replaced sat within rounding distance of
/// `raised` on the dark preset and read as no edge at all.
fn assert_panel_edge_registers(rgba: &[u8], w: u32, theme_name: &str) {
    // Sample away from the corner radius and away from the header text:
    // the left edge, a third of the way down.
    let y = 140u32;
    let x_edge = PICKER_INSET as u32;
    let x_inside = x_edge.saturating_add(6);
    let edge = OffscreenRenderer::sample(rgba, w, x_edge, y);
    let inside = OffscreenRenderer::sample(rgba, w, x_inside, y);
    assert_eq!(edge.3, 0xff, "{theme_name}: edge pixel is opaque");
    assert_eq!(inside.3, 0xff, "{theme_name}: fill pixel is opaque");
    let delta = (luminance_of(edge) - luminance_of(inside)).abs();
    eprintln!("{theme_name}: panel edge {edge:?} vs fill {inside:?} → Δlum={delta:.4}");
    assert!(
        delta >= tilo_ui::tokens::colors::HAIRLINE_MIN_DELTA,
        "{theme_name}: the picker's panel edge {edge:?} does not register against its fill          {inside:?} (Δlum={delta:.4} < {})",
        tilo_ui::tokens::colors::HAIRLINE_MIN_DELTA
    );
}

/// The review PNGs always land in `target/ui-review/file-picker`, so a
/// visual change to the dialog can be looked at without any setup; set
/// `TILO_UI_SCREENSHOT_DIR` to put them somewhere else.
fn dump(name: &str, rgba: &[u8], w: u32, h: u32) {
    let dir = tilo_ui::env::var("TILO_UI_SCREENSHOT_DIR").unwrap_or_else(|_| {
        format!(
            "{}/target/ui-review/file-picker",
            env!("CARGO_MANIFEST_DIR")
        )
    });
    std::fs::create_dir_all(&dir).unwrap();
    let path = format!("{dir}/{name}.png");
    tilo_ui::png::write_rgba8(&path, w, h, rgba).unwrap();
    eprintln!("wrote {path}");
}

#[test]
fn file_picker_open_and_save_render() {
    let (w, h) = (900u32, 620u32);
    let Some(mut renderer) = OffscreenRenderer::try_new(w, h).ok() else {
        eprintln!("no wgpu adapter — skipping file_picker_open_and_save_render");
        return;
    };
    let (_g, root) = temp_dir();

    for (theme_name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let bg8 = backdrop(&theme).to_rgba8();
        let bg = [bg8.r, bg8.g, bg8.b, 0xff];

        // Open mode with file-type filters.
        let mut open =
            FilePicker::new("Open a file", &root, vec![root.clone()]).with_filters(vec![
                FileFilter::new("Images", &["png", "jpg"]),
                FileFilter::all(),
            ]);
        open.set_selection(0);
        let rgba = render(&mut open, &theme, w, h, &mut renderer);
        let non_bg = rgba.chunks_exact(4).filter(|p| p[..4] != bg).count();
        assert!(
            non_bg > 2000,
            "{theme_name}: open-mode picker painted too little: {non_bg}"
        );
        assert_panel_edge_registers(&rgba, w, theme_name);
        dump(&format!("file_picker_open_{theme_name}"), &rgba, w, h);

        // Save mode with a typed filename.
        let mut save = FilePicker::new("Save as", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Save)
            .with_filename("report.md");
        let rgba = render(&mut save, &theme, w, h, &mut renderer);
        let non_bg = rgba.chunks_exact(4).filter(|p| p[..4] != bg).count();
        assert!(
            non_bg > 2000,
            "{theme_name}: save-mode picker painted too little: {non_bg}"
        );
        assert_panel_edge_registers(&rgba, w, theme_name);
        dump(&format!("file_picker_save_{theme_name}"), &rgba, w, h);
    }
}

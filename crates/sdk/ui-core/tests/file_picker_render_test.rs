//! Offscreen render guard for the overhauled [`FilePicker`]: drives the
//! draw pass in both Open and Save mode and asserts non-empty pixels, so
//! the icon / breadcrumb / save-field / filter-chip paths can't silently
//! regress to blank output.
//!
//! Skips when no wgpu adapter is available. Set `QUICK_UI_SCREENSHOT_DIR`
//! to dump PNGs (`file_picker_open.png`, `file_picker_save.png`).

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
use std::path::PathBuf;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;

use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::Widget;
use tilo_ui::widget::file_picker::{FileFilter, FilePicker, FilePickerMode};

fn try_new_renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        OffscreenRenderer::new(w, h)
    }))
    .ok()
}

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

fn render(picker: &mut FilePicker, w: u32, h: u32, renderer: &mut OffscreenRenderer) -> Vec<u8> {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    picker.set_rect(Rect::new(0.0, 0.0, f64::from(w), f64::from(h)));
    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    picker.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    renderer.render(&scene, Color::from_rgb8(0x0f, 0x17, 0x2a))
}

fn dump(name: &str, rgba: &[u8], w: u32, h: u32) {
    if let Ok(dir) = std::env::var("QUICK_UI_SCREENSHOT_DIR") {
        let _ = std::fs::create_dir_all(&dir);
        let mut png = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(rgba).unwrap();
        }
        let path = format!("{dir}/{name}.png");
        std::fs::write(&path, png).unwrap();
        eprintln!("wrote {path}");
    }
}

#[test]
fn file_picker_open_and_save_render() {
    let (w, h) = (560u32, 420u32);
    let Some(mut renderer) = try_new_renderer(w, h) else {
        eprintln!("no wgpu adapter — skipping file_picker_open_and_save_render");
        return;
    };
    let (_g, root) = temp_dir();

    // Open mode with file-type filters.
    let mut open = FilePicker::new("Open a file", &root, vec![root.clone()]).with_filters(vec![
        FileFilter::new("Images", &["png", "jpg"]),
        FileFilter::all(),
    ]);
    open.set_selection(0);
    let rgba = render(&mut open, w, h, &mut renderer);
    let bg = [0x0f, 0x17, 0x2a, 0xff];
    let non_bg = rgba.chunks_exact(4).filter(|p| p[..4] != bg).count();
    assert!(
        non_bg > 2000,
        "open-mode picker painted too little: {non_bg}"
    );
    dump("file_picker_open", &rgba, w, h);

    // Save mode with a typed filename.
    let mut save = FilePicker::new("Save as", &root, vec![root.clone()])
        .with_mode(FilePickerMode::Save)
        .with_filename("report.md");
    let rgba = render(&mut save, w, h, &mut renderer);
    let non_bg = rgba.chunks_exact(4).filter(|p| p[..4] != bg).count();
    assert!(
        non_bg > 2000,
        "save-mode picker painted too little: {non_bg}"
    );
    dump("file_picker_save", &rgba, w, h);
}

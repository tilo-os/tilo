//! Opt-in visual evidence for render tests.
//!
//! A render test asserts on the pixels it holds in memory. Setting
//! `TILO_UI_SCREENSHOT_DIR` additionally writes each frame there as a PNG
//! for a reviewer to open; without it nothing touches the filesystem, so
//! parallel runs never share output paths.

use std::path::Path;

/// Write `rgba` (tightly packed, `width` pixels per row) to
/// `$TILO_UI_SCREENSHOT_DIR/<name>.png` when that variable is set.
/// `name` may contain `/` to group a suite's frames in a subdirectory.
pub fn save_png(name: &str, width: u32, height: u32, rgba: &[u8]) {
    let Some(dir) = tilo_ui::env::var_os("TILO_UI_SCREENSHOT_DIR") else {
        return;
    };
    let path = Path::new(&dir).join(format!("{name}.png"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create screenshot directory");
    }
    tilo_ui::png::write_rgba8(&path, width, height, rgba).expect("write screenshot png");
    eprintln!("wrote {}", path.display());
}

/// Assert a frame shows something: at least two distinct pixel values.
pub fn assert_painted(name: &str, rgba: &[u8]) {
    let mut pixels = rgba.chunks_exact(4);
    let first = pixels.next().expect("a non-empty frame");
    assert!(
        pixels.any(|pixel| pixel != first),
        "{name} rendered a uniform frame"
    );
}

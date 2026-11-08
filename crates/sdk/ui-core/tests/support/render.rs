//! What the render tests do with a painted frame: fingerprint it, and write it out when a run
//! asks for the pictures.

/// A cheap 64-bit FNV-1a over the whole RGBA buffer: not a golden-image comparison, just a
/// fingerprint good enough to tell two renders apart.
pub fn pixel_hash(buf: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in buf {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Write the frame as `$TILO_UI_SCREENSHOT_DIR/<name>.png`, when the run asked for the pictures.
pub fn write_png(name: &str, w: u32, h: u32, rgba: &[u8]) {
    let Ok(dir) = tilo_ui::env::var("TILO_UI_SCREENSHOT_DIR") else {
        return;
    };
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    std::fs::create_dir_all(&dir).ok();
    tilo_ui::png::write_rgba8(&path, w, h, rgba).expect("write screenshot png");
    eprintln!("wrote {} to {}", name, path.display());
}

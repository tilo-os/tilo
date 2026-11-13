//! How large one frame's scene may be: a frame the renderer cannot encode in
//! a browser's memory is refused, with what it was, rather than killed by the
//! GPU process.

/// Glyphs one frame may encode. A full 4K screen of small text is far below
/// this; only unculled off-screen content reaches it.
pub(super) const MAX_SCENE_GLYPHS: usize = 400_000;
/// Encoded scene bytes one frame may hand to the renderer.
pub(super) const MAX_SCENE_BYTES: usize = 128 << 20;

pub(super) fn scene_within_limits(scene: &vello::Scene) -> Result<(), String> {
    let encoding = scene.encoding();
    let glyphs = encoding.resources.glyphs.len();
    let bytes = encoding
        .path_tags
        .len()
        .saturating_add(encoding.path_data.len().saturating_mul(4))
        .saturating_add(encoding.draw_tags.len().saturating_mul(4))
        .saturating_add(encoding.draw_data.len().saturating_mul(4))
        .saturating_add(encoding.transforms.len().saturating_mul(24))
        .saturating_add(encoding.styles.len().saturating_mul(8));
    if glyphs > MAX_SCENE_GLYPHS || bytes > MAX_SCENE_BYTES {
        Err(format!(
            "Browser runtime: frame not rendered, scene too large ({glyphs} glyphs, {bytes} encoded bytes)"
        ))
    } else {
        Ok(())
    }
}

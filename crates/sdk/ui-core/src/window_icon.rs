//! Resolves the raster icon named by the launcher's `TILO_APP_ICON`, falling
//! back to a generated Tilo icon when it is missing or invalid.
//!
//! On Wayland, most compositors ignore a client-set toplevel icon (sourced
//! from the `app_id` and a `.desktop` file instead).

use winit::window::Icon;

pub const ICON_ENV: &str = "TILO_APP_ICON";

/// The app's own icon when [`ICON_ENV`] names a decodable raster image,
/// otherwise the generated Tilo default. `None` only if even the default
/// fails to build (it never should).
pub fn resolve() -> Option<Icon> {
    from_env().or_else(default_icon)
}

fn from_env() -> Option<Icon> {
    let raw = std::env::var_os(ICON_ENV)?;
    if raw.is_empty() {
        return None;
    }
    load_path(std::path::Path::new(&raw))
}

pub(crate) fn load_path(path: &std::path::Path) -> Option<Icon> {
    let bytes = std::fs::read(path).ok()?;
    load_bytes(&bytes)
}

pub(crate) fn load_bytes(bytes: &[u8]) -> Option<Icon> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Icon::from_rgba(rgba.into_raw(), w, h).ok()
}

pub(crate) fn default_icon() -> Option<Icon> {
    const SIZE: u32 = 128;
    let rgba = render_default(SIZE);
    Icon::from_rgba(rgba, SIZE, SIZE).ok()
}

/// Anti-aliased rounded square filled with a vertical accent gradient.
/// Pure CPU raster — no GPU, no asset — so it works headless too.
#[allow(
    clippy::many_single_char_names,
    reason = "s is the pixel scale, x/y are pixel coordinates, and r/g/b are colour \
              channels — the conventional names for a per-pixel raster loop"
)]
fn render_default(size: u32) -> Vec<u8> {
    let top = [0x4Cu8, 0x6Eu8, 0xF5u8];
    let bottom = [0x8Bu8, 0x5Cu8, 0xF6u8];
    // `size` is a pixel dimension (the only caller passes a constant 128);
    // f32 represents every integer up to 2^24 exactly, far past any
    // realistic icon size, so this loses nothing.
    #[allow(
        clippy::cast_precision_loss,
        reason = "size is a pixel dimension, always far below f32's 2^24 exact-integer range"
    )]
    let s = size as f32;
    let radius = s * 0.24;
    let half = s * 0.5;
    let mut out = Vec::with_capacity(size.saturating_mul(size).saturating_mul(4) as usize);
    for y in 0..size {
        for x in 0..size {
            #[allow(
                clippy::cast_precision_loss,
                reason = "x/y are pixel coordinates, always far below f32's 2^24 exact-integer range"
            )]
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // Signed distance to a rounded square centred in the tile.
            // Positive outside, negative inside; the |d| < 1px band is
            // the antialiased edge.
            let dx = (px - half).abs() - (half - radius);
            let dy = (py - half).abs() - (half - radius);
            let outside = dx.max(0.0).hypot(dy.max(0.0));
            let inside = dx.max(dy).min(0.0);
            let dist = outside + inside - radius;
            let cover = (0.5 - dist).clamp(0.0, 1.0);
            let t = py / s;
            let r = lerp(top[0], bottom[0], t);
            let g = lerp(top[1], bottom[1], t);
            let b = lerp(top[2], bottom[2], t);
            // `cover` is clamped to 0.0..=1.0 just above, so this product,
            // rounded, always lands in 0.0..=255.0.
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "cover is clamped to 0.0..=1.0 above, so the scaled+rounded value always fits u8"
            )]
            let a = (cover * 255.0).round() as u8;
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    out
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    let v = (f32::from(a) + (f32::from(b) - f32::from(a)) * t)
        .round()
        .clamp(0.0, 255.0);
    // Clamped to `0.0..=255.0` just above, so this always fits `u8` exactly.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "v is clamped to 0.0..=255.0 just above, so it always fits u8 exactly"
    )]
    let result = v as u8;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_icons_build_decode_or_refuse_their_source() {
        // Case `default_icon_builds`.
        {
            assert!(
                default_icon().is_some(),
                "generated Tilo default icon must build"
            );
        }

        // Case `default_icon_has_opaque_centre_and_clear_corner`.
        {
            const SIZE: u32 = 128;
            let rgba = render_default(SIZE);
            let at = |x: u32, y: u32| {
                let i = ((y * SIZE + x) * 4) as usize;
                [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
            };
            assert_eq!(
                at(SIZE / 2, SIZE / 2)[3],
                255,
                "centre pixel must be opaque"
            );
            assert_eq!(at(0, 0)[3], 0, "extreme corner must be transparent");
        }

        // Case `load_bytes_decodes_a_png`.
        {
            let mut buf = std::io::Cursor::new(Vec::new());
            let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 255]));
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut buf, image::ImageFormat::Png)
                .expect("encode png");
            assert!(
                load_bytes(&buf.into_inner()).is_some(),
                "a valid PNG must decode into an icon"
            );
        }

        // Case `load_bytes_rejects_garbage`.
        {
            assert!(
                load_bytes(b"not an image").is_none(),
                "undecodable bytes must not yield an icon"
            );
        }

        // Case `load_path_of_missing_file_is_none`.
        {
            assert!(
                load_path(std::path::Path::new("/nonexistent/tilo-icon.png")).is_none(),
                "a missing icon file must not panic and must yield None"
            );
        }
    }
}

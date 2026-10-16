//! Raster application icons: an app's own artwork, where a lucide glyph
//! stands for a Tilo app.
//!
//! The toolkit does not know where artwork comes from — an icon theme, a
//! Wayland client, a tray item. The host hands chrome an [`AppImage`], and
//! chrome asks it for pixels at the device size it is about to paint, so a
//! vector source is rasterised for the output's scale rather than blurred up
//! from a fixed bitmap.

use std::sync::Arc;

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Fill, ImageBrush, ImageData};

/// The largest edge chrome asks a source to rasterise.
pub const MAX_DEVICE_PX: u32 = 512;

/// Where an [`AppImage`]'s pixels come from.
pub trait AppImageSource: Send + Sync {
    /// Identity of the artwork: two images with the same key paint the same
    /// pixels, so chrome can skip replacing one with the other.
    fn key(&self) -> &str;

    /// Pixels for an icon painted `device_px` square, or `None` when the
    /// artwork cannot be decoded. Called every frame the icon is painted, so
    /// a source caches its rasterisations.
    fn pixels(&self, device_px: u32) -> Option<ImageData>;
}

/// A shareable handle to an application's artwork.
#[derive(Clone)]
pub struct AppImage(Arc<dyn AppImageSource>);

impl AppImage {
    pub fn new(source: impl AppImageSource + 'static) -> Self {
        Self(Arc::new(source))
    }

    pub fn key(&self) -> &str {
        self.0.key()
    }

    pub fn pixels(&self, device_px: u32) -> Option<ImageData> {
        self.0.pixels(device_px.clamp(1, MAX_DEVICE_PX))
    }

    /// The image at `url` in `store`: a media row's thumbnail, a cover. The
    /// store loads it off the UI thread the first time it is painted; until
    /// then (or when it cannot be loaded) it has no pixels and the row paints
    /// its glyph.
    #[must_use]
    pub fn from_image_store(
        store: &crate::image_store::ImageStore,
        url: impl Into<String>,
    ) -> Self {
        Self::new(StoreImage {
            url: url.into(),
            store: store.clone(),
        })
    }
}

/// An [`AppImage`] read from an [`ImageStore`](crate::image_store::ImageStore).
struct StoreImage {
    url: String,
    store: crate::image_store::ImageStore,
}

impl AppImageSource for StoreImage {
    fn key(&self) -> &str {
        &self.url
    }

    fn pixels(&self, _device_px: u32) -> Option<ImageData> {
        self.store.get(&self.url)
    }
}

impl std::fmt::Debug for AppImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AppImage").field(&self.key()).finish()
    }
}

impl PartialEq for AppImage {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for AppImage {}

/// Device pixels one logical pixel covers under `transform`.
fn device_scale(transform: Affine) -> f64 {
    let [a, b, _, _, _, _] = transform.as_coeffs();
    let scale = a.hypot(b);
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

/// The device-pixel edge `rect` covers under `transform`.
pub fn device_px(transform: Affine, rect: Rect) -> u32 {
    let edge = (rect.width().max(rect.height()) * device_scale(transform)).ceil();
    if edge.is_finite() && edge >= 1.0 {
        // Clamped to 1..=MAX_DEVICE_PX: a small positive integer.
        edge.min(f64::from(MAX_DEVICE_PX)) as u32
    } else {
        1
    }
}

/// Paint `image` filling `rect`, rasterised for the device size `rect`
/// covers. Returns `false`, painting nothing, when the source has no pixels,
/// so the caller can fall back to a glyph.
pub fn draw_app_image(scene: &mut Scene, transform: Affine, rect: Rect, image: &AppImage) -> bool {
    let Some(pixels) = image.pixels(device_px(transform, rect)) else {
        return false;
    };
    let (width, height) = (
        f64::from(pixels.width.max(1)),
        f64::from(pixels.height.max(1)),
    );
    // Fit inside the square, centred, keeping the artwork's aspect.
    let fit = (rect.width() / width).min(rect.height() / height);
    let x0 = rect.x0 + (rect.width() - width * fit) / 2.0;
    let y0 = rect.y0 + (rect.height() - height * fit) / 2.0;
    scene.fill(
        Fill::NonZero,
        transform.pre_translate((x0, y0).into()).pre_scale(fit),
        &ImageBrush::new(pixels),
        None,
        &Rect::new(0.0, 0.0, width, height),
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Solid(&'static str, std::sync::Mutex<Vec<u32>>);

    impl AppImageSource for Solid {
        fn key(&self) -> &str {
            self.0
        }
        fn pixels(&self, device_px: u32) -> Option<ImageData> {
            self.1.lock().unwrap().push(device_px);
            let len = usize::try_from(device_px * device_px * 4).unwrap();
            Some(crate::raster::rgba8(vec![255u8; len], device_px, device_px))
        }
    }

    #[test]
    fn pixels_are_requested_at_the_scale_the_icon_is_painted() {
        let source = Arc::new(Solid("firefox", std::sync::Mutex::new(Vec::new())));
        let image = AppImage(source.clone());
        let mut scene = Scene::new();
        let rect = Rect::new(0.0, 0.0, 14.0, 14.0);
        assert!(draw_app_image(&mut scene, Affine::IDENTITY, rect, &image));
        assert!(draw_app_image(&mut scene, Affine::scale(2.0), rect, &image));
        assert!(draw_app_image(&mut scene, Affine::scale(1.5), rect, &image));
        assert_eq!(*source.1.lock().unwrap(), vec![14, 28, 21]);
    }

    #[test]
    fn images_compare_by_artwork_identity() {
        let a = AppImage::new(Solid("a", std::sync::Mutex::new(Vec::new())));
        let a2 = AppImage::new(Solid("a", std::sync::Mutex::new(Vec::new())));
        let b = AppImage::new(Solid("b", std::sync::Mutex::new(Vec::new())));
        assert_eq!(a, a2);
        assert_ne!(a, b);
    }
}

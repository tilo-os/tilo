//! Raster application icons: an app's own artwork, where a lucide glyph stands for a Tilo app. The
//! toolkit does not know where artwork comes from — an icon theme, a Wayland client, a tray item.
//!
//! Every surface that shows an app — a search row, a menu row, the switcher, a tab — asks one
//! place for its artwork, [`app_artwork`], and paints the app's glyph while that has no pixels. What
//! answers is the host the process runs in ([`set_app_artwork`]): the session reads the person's
//! icon themes and the icons installed packages signed, off its loop and cached per app and device
//! size. A sandboxed app reads no theme: the session hands it the pixels with the rows it asks for
//! ([`AppImage::from_png`]), and a process with no host to answer (the web build) shows glyphs.

use std::sync::{Arc, RwLock};

use tilo_lock::RwLockRecover as _;

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Fill, ImageBrush, ImageData};

/// The largest edge chrome asks a source to rasterise.
pub const MAX_DEVICE_PX: u32 = 512;

/// Where an [`AppImage`]'s pixels come from.
pub trait AppImageSource: Send + Sync {
    /// Identity of the artwork: two images with the same key paint the same pixels, so chrome can skip
    /// replacing one with the other.
    fn key(&self) -> &str;

    /// Pixels for an icon painted `device_px` square, or `None` when the artwork cannot be decoded.
    fn pixels(&self, device_px: u32) -> Option<ImageData>;
}

/// The most bytes of a handed-over icon [`AppImage::from_png`] decodes: an icon is kilobytes.
pub const MAX_PNG_BYTES: usize = 64 * 1024;
/// The most pixels a handed-over icon decodes to.
pub const MAX_PNG_PIXELS: u32 = 128 * 128;

/// What answers [`app_artwork`] in this process.
pub type ArtworkResolver = Arc<dyn Fn(&str) -> Option<AppImage> + Send + Sync>;

static RESOLVER: RwLock<Option<ArtworkResolver>> = RwLock::new(None);

/// Answer [`app_artwork`] in this process with `resolver`: set once by the host that can read
/// artwork (the session), which must answer without touching the disk on the caller's thread.
pub fn set_app_artwork(resolver: ArtworkResolver) {
    *RESOLVER.write_recover() = Some(resolver);
}

/// The artwork of the app `app_id`, as every surface that lists it paints it: `None` where this
/// process has no host that reads artwork, or the app has none, and the app wears its glyph. The
/// image may have no pixels yet the first frames it is painted; the glyph shows until it has.
#[must_use]
pub fn app_artwork(app_id: &str) -> Option<AppImage> {
    let app_id = app_id.trim();
    if app_id.is_empty() {
        return None;
    }
    let resolver = RESOLVER.read_recover().clone()?;
    resolver(app_id)
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

    /// Artwork handed over as PNG bytes, decoded once: what a sandboxed app is given for the apps
    /// it lists, since it reads no icon theme. `key` names the app it is the artwork of. `None` for
    /// bytes that are empty, larger than [`MAX_PNG_BYTES`], or not a PNG of at most
    /// [`MAX_PNG_PIXELS`].
    #[must_use]
    pub fn from_png(key: impl Into<String>, png: &[u8]) -> Option<Self> {
        if png.is_empty() || png.len() > MAX_PNG_BYTES {
            return None;
        }
        let decoded = crate::png::decode_rgba8_within(png, u64::from(MAX_PNG_PIXELS)).ok()?;
        let pixels = crate::raster::rgba8(decoded.rgba, decoded.width, decoded.height);
        Some(Self::new(Decoded {
            key: key.into(),
            pixels,
        }))
    }

    /// The image at `url` in `store`: a media row's thumbnail, a cover. The store loads it off the UI
    /// thread the first time it is painted; until then (or when it cannot be loaded) it has no pixels
    /// and the row paints its glyph.
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

/// Pixels decoded once, the same at every size: the source scales them to the slot.
struct Decoded {
    key: String,
    pixels: ImageData,
}

impl AppImageSource for Decoded {
    fn key(&self) -> &str {
        &self.key
    }

    fn pixels(&self, _device_px: u32) -> Option<ImageData> {
        Some(self.pixels.clone())
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

/// Paint `image` filling `rect`, rasterised for the device size `rect` covers. Returns `false`,
/// painting nothing, when the source has no pixels, so the caller can fall back to a glyph.
pub fn draw_app_image(scene: &mut Scene, transform: Affine, rect: Rect, image: &AppImage) -> bool {
    let Some(pixels) = image.pixels(device_px(transform, rect)) else {
        return false;
    };
    let (width, height) = (
        f64::from(pixels.width.max(1)),
        f64::from(pixels.height.max(1)),
    );
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

    /// A PNG one red pixel wide and tall.
    fn png() -> Vec<u8> {
        crate::png::encode_rgba8(1, 1, &[255, 0, 0, 255]).unwrap()
    }

    #[test]
    fn handed_over_artwork_is_decoded_once_and_bounded() {
        let image = AppImage::from_png("app:files", &png()).expect("a png");
        assert_eq!(image.key(), "app:files");
        let pixels = image.pixels(32).expect("its pixels");
        assert_eq!((pixels.width, pixels.height), (1, 1));
        assert!(AppImage::from_png("app:x", b"not a png").is_none());
        assert!(AppImage::from_png("app:x", &[]).is_none());
        assert!(AppImage::from_png("app:x", &vec![0; MAX_PNG_BYTES + 1]).is_none());
    }

    #[test]
    fn the_host_answers_artwork_and_no_host_is_the_glyph() {
        assert!(app_artwork("so.tilo.notes").is_none(), "no host: the glyph");
        set_app_artwork(Arc::new(|app: &str| {
            (app == "so.tilo.notes").then(|| AppImage::from_png("app:notes", &png()))?
        }));
        assert_eq!(
            app_artwork(" so.tilo.notes ").map(|image| image.key().to_owned()),
            Some("app:notes".to_owned())
        );
        assert!(app_artwork("org.example.None").is_none());
        assert!(app_artwork("").is_none());
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

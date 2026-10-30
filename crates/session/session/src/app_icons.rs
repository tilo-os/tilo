//! Artwork for the applications notifications come from.
//!
//! A notification names its app three ways — the id the session attributed
//! it to, the desktop entry its sender hinted, and an icon name the sender
//! claimed — in that order of trust. Each only ever yields pixels: the
//! order decides whose artwork a banner shows, never what the banner says
//! the app is.
//!
//! An icon is looked up in the person's themes and rasterised at the size
//! the banner paints it, which reads the disk. That happens on a worker,
//! and the loop is woken with the answer, so a banner paints its app's
//! letter for the frame before its artwork is there.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};

use desktop_entry::icon_theme::{self, IconFormat};
use tilo_lock::MutexRecover as _;
use ui::icons::{AppImage, AppImageSource};

/// The most bytes of an icon file the worker reads: an icon is kilobytes,
/// and a hostile or broken theme must not make it parse megabytes.
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
/// Largest decoded edge accepted from a raster icon.
const MAX_DECODED_EDGE: u32 = 1024;
/// Rasterisations kept before the cache is cleared and rebuilt.
const CACHE_LIMIT: usize = 512;

/// Where artwork comes from, in trust order.
enum Layer {
    /// An icon name in the person's themes, or an absolute icon path.
    Named(String),
    /// Pixels the sender handed over, already decoded and bounded.
    Pixels(ui::peniko::ImageData),
}

/// The artwork of one notification: the first of its layers that has
/// pixels.
pub(crate) struct Artwork {
    key: String,
    layers: Vec<Layer>,
}

impl AppImageSource for Artwork {
    fn key(&self) -> &str {
        &self.key
    }

    /// Never touches the disk on the frame's thread: an icon that is not
    /// rasterised yet is asked for, and this frame paints without it.
    fn pixels(&self, device_px: u32) -> Option<ui::peniko::ImageData> {
        let device_px = device_px.clamp(1, ui::icons::MAX_DEVICE_PX);
        for layer in &self.layers {
            match layer {
                Layer::Pixels(image) => return Some(image.clone()),
                Layer::Named(name) => match registry().answer(name, device_px) {
                    Some(Some(image)) => return Some(image),
                    // This theme has no such icon: the next layer shows.
                    Some(None) => {}
                    None => {
                        registry().ask(name, device_px);
                        return None;
                    }
                },
            }
        }
        None
    }
}

/// The artwork for a notification from `app_id`, hinting `desktop_entry`
/// and the sender's claimed `app_icon`, with `pixels` the sender handed
/// over; keyed by the notification's id and arrival generation so a
/// replaced image is never mistaken for the old one. `None` when nothing
/// names artwork.
pub(crate) fn notification(
    key: String,
    app_id: Option<&str>,
    desktop_entry: Option<&str>,
    app_icon: Option<&str>,
    pixels: Option<ui::peniko::ImageData>,
) -> Option<AppImage> {
    let mut layers = Vec::with_capacity(4);
    if let Some(app_id) = app_id.map(str::trim).filter(|id| !id.is_empty()) {
        // The icon the installed package of that id signed, else the theme
        // icon named like the id itself — what most Wayland applications
        // with no desktop entry of their own install.
        let name = installed_icon(app_id).unwrap_or_else(|| app_id.to_owned());
        layers.push(Layer::Named(name));
    }
    if let Some(name) = desktop_entry
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .and_then(entry_icon)
    {
        layers.push(Layer::Named(name));
    }
    layers.extend(pixels.map(Layer::Pixels));
    if let Some(name) = app_icon.map(str::trim).filter(|name| !name.is_empty()) {
        layers.push(Layer::Named(name.to_owned()));
    }
    if layers.is_empty() {
        return None;
    }
    Some(AppImage::new(Artwork { key, layers }))
}

/// The icon a desktop entry names, for the entry id `desktop_id`.
fn entry_icon(desktop_id: &str) -> Option<String> {
    desktop_entry::cache::cached_scan()
        .find(desktop_id)
        .and_then(|entry| entry.icon.clone())
}

/// The absolute path of the icon the installed package `app_id` signed.
fn installed_icon(app_id: &str) -> Option<String> {
    tilo_installed_apps::read_one_from_env(app_id)
        .ok()
        .flatten()
        .and_then(|app| app.icon_path())
        .and_then(|path| path.to_str().map(str::to_owned))
}

/// Wake the loop once artwork is ready, so the banner repaints with it.
pub(crate) fn set_wake(wake: Arc<dyn Fn() + Send + Sync>) {
    *registry().wake.lock_recover() = Some(wake);
}

struct Registry {
    /// Each icon's pixels at each size; `None` once a theme was found not
    /// to have it.
    answers: Mutex<BTreeMap<(String, u32), Option<ui::peniko::ImageData>>>,
    /// What was already asked for, so one frame does not queue it twice.
    asked: Mutex<BTreeSet<(String, u32)>>,
    /// Where work goes; `None` until the worker is started.
    jobs: Mutex<Option<mpsc::Sender<(String, u32)>>>,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Registry {
        answers: Mutex::new(BTreeMap::new()),
        asked: Mutex::new(BTreeSet::new()),
        jobs: Mutex::new(None),
        wake: Mutex::new(None),
    })
}

impl Registry {
    /// What `name` at `px` rasterises to; `None` while that is still being
    /// found out.
    #[expect(
        clippy::option_option,
        reason = "three answers differ: not found out yet, this theme has no such icon, pixels"
    )]
    fn answer(&self, name: &str, px: u32) -> Option<Option<ui::peniko::ImageData>> {
        self.answers
            .lock_recover()
            .get(&(name.to_owned(), px))
            .cloned()
    }

    /// Have the worker find out what `name` at `px` is.
    fn ask(&self, name: &str, px: u32) {
        let key = (name.to_owned(), px);
        if !self.asked.lock_recover().insert(key.clone()) {
            return;
        }
        if self.sender().send(key.clone()).is_err() {
            // The worker went away before taking it: let a later frame ask
            // again.
            self.asked.lock_recover().remove(&key);
        }
    }

    /// The queue to the worker, started on first use.
    fn sender(&self) -> mpsc::Sender<(String, u32)> {
        let mut jobs = self.jobs.lock_recover();
        if let Some(jobs) = jobs.as_ref() {
            return jobs.clone();
        }
        let (send, receive) = mpsc::channel::<(String, u32)>();
        let registry = registry();
        if let Err(error) = std::thread::Builder::new()
            .name("tilo-app-icons".to_owned())
            .spawn(move || {
                while let Ok((name, px)) = receive.recv() {
                    let answer = resolve(&name, px);
                    {
                        let mut answers = registry.answers.lock_recover();
                        if answers.len() >= CACHE_LIMIT {
                            answers.clear();
                        }
                        answers.insert((name, px), answer);
                    }
                    if let Some(wake) = registry.wake.lock_recover().as_ref() {
                        wake();
                    }
                }
            })
        {
            tracing::warn!(%error, "no worker for application artwork");
        }
        *jobs = Some(send.clone());
        send
    }
}

/// The pixels of icon `name` at `px` square, or `None` when no theme has
/// it or it cannot be decoded.
fn resolve(name: &str, px: u32) -> Option<ui::peniko::ImageData> {
    let file = icon_theme::lookup(name, px, 1)?;
    let bytes = read_bounded(&file.path)?;
    let pixels = match file.format {
        IconFormat::Png => rasterize_png(&bytes, px)?,
        IconFormat::Svg => rasterize_svg(&bytes, px)?,
    };
    Some(ui::raster::rgba8(pixels.rgba, pixels.width, pixels.height))
}

/// The file's bytes, if it is one small enough to be an icon.
fn read_bounded(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).ok()?);
    file.take(MAX_SOURCE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

/// Straight-alpha RGBA pixels, row-major.
struct Pixels {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// `bytes` as a PNG, scaled down to fit `px` square.
fn rasterize_png(bytes: &[u8], px: u32) -> Option<Pixels> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODED_EDGE);
    limits.max_image_height = Some(MAX_DECODED_EDGE);
    limits.max_alloc = Some(64 * 1024 * 1024);
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    if decoded.width() == 0 || decoded.height() == 0 {
        return None;
    }
    let decoded = if decoded.width().max(decoded.height()) > px {
        decoded.resize(px, px, image::imageops::FilterType::Triangle)
    } else {
        decoded
    };
    let rgba = decoded.into_rgba8();
    let (width, height) = rgba.dimensions();
    Some(Pixels {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

/// `bytes` as an SVG, rendered `px` square with its shape kept.
fn rasterize_svg(bytes: &[u8], px: u32) -> Option<Pixels> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let longest = size.width().max(size.height());
    if !longest.is_finite() || longest <= 0.0 {
        return None;
    }
    let edge = f32::from(u16::try_from(px).ok()?);
    let scale = edge / longest;
    let width = dimension(size.width() * scale)?;
    let height = dimension(size.height() * scale)?;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();
    Some(Pixels {
        width,
        height,
        rgba,
    })
}

/// A rendered edge in whole pixels, at least one.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a finite edge checked to lie in 1..=MAX_DEVICE_PX"
)]
fn dimension(edge: f32) -> Option<u32> {
    let edge = edge.ceil();
    if !edge.is_finite() || edge < 1.0 {
        return Some(1);
    }
    (edge <= f32::from(u16::try_from(ui::icons::MAX_DEVICE_PX).ok()?)).then_some(edge as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2×1 PNG: one red pixel beside one blue one.
    fn png() -> Vec<u8> {
        let mut pixels = image::RgbaImage::new(2, 1);
        pixels.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        pixels.put_pixel(1, 0, image::Rgba([0, 0, 255, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("a png encoder");
        bytes.into_inner()
    }
    const SQUARE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="#ff0000"/></svg>"##;

    #[test]
    fn an_icons_pixels_are_rasterised_at_the_size_asked_for() {
        let png = rasterize_png(&png(), 64).expect("the png decodes");
        assert_eq!((png.width, png.height), (2, 1));
        assert_eq!(png.rgba.get(..4), Some([255, 0, 0, 255].as_slice()));

        let svg = rasterize_svg(SQUARE, 32).expect("the svg rasterises");
        assert_eq!((svg.width, svg.height), (32, 32));
        assert_eq!(svg.rgba.get(..4), Some([255, 0, 0, 255].as_slice()));

        let wide =
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="#00ff00"/></svg>"##;
        let kept = rasterize_svg(wide, 20).expect("its shape is kept");
        assert_eq!((kept.width, kept.height), (20, 10));
    }

    #[test]
    fn artwork_asks_for_an_icon_once_and_paints_it_when_it_is_there() {
        let dir = tempfile::tempdir().expect("a theme");
        let icon = dir.path().join("so.tilo.mail.png");
        std::fs::write(&icon, png()).expect("the icon");
        // An absolute path names an icon file itself.
        let key = icon.to_str().expect("a path");
        let artwork = notification("notification:1:0".to_owned(), None, None, Some(key), None)
            .expect("named artwork");
        assert_eq!(artwork.key(), "notification:1:0");
        assert!(artwork.pixels(64).is_none(), "not read yet");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while artwork.pixels(64).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "the worker rasterised it"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let pixels = artwork.pixels(64).expect("its pixels");
        assert_eq!((pixels.width, pixels.height), (2, 1));
    }

    #[test]
    fn nothing_that_names_artwork_is_no_artwork() {
        assert!(notification("notification:2:0".to_owned(), None, None, None, None).is_none());
    }
}

//! Pictures placed on the artwork: where an image mark's pixels land, and
//! the decoded, scaled and graded copies drawing needs, kept between
//! frames.
//!
//! A picture is decoded once. Halved copies of it (a pyramid) let a view
//! that shows it small grade only the pixels it needs: the canvas at fit
//! grades a 12-megapixel photo at about screen size, an export at full
//! size. Grading is [`crate::adjust`]'s, the same for every view.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "pixel sizes and kurbo float geometry, bounded by the document's limits"
)]

use std::collections::HashMap;

use ui::vello::kurbo::{Affine, Rect};
use ui::vello::peniko::ImageData;

use crate::adjust::Grade;
use crate::model::{Adjust, Shape};

/// The largest picture file, in bytes.
pub const MAX_PICTURE_BYTES: usize = 64 << 20;
/// The longest side of a picture, in pixels.
pub const MAX_PICTURE_SIDE: u32 = 8192;

/// A PNG, JPEG or WebP decoded to straight RGBA, at most
/// [`MAX_PICTURE_SIDE`] pixels a side.
///
/// # Errors
/// Returns why the bytes are not a picture.
pub fn decode(bytes: &[u8]) -> Result<ImageData, String> {
    let format = image::guess_format(bytes).map_err(|error| error.to_string())?;
    if !matches!(
        format,
        image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP
    ) {
        return Err(format!("{format:?} is not PNG, JPEG or WebP"));
    }
    let (width, height) = dimensions(bytes)?;
    if width.max(height) > MAX_PICTURE_SIDE {
        return Err(format!(
            "the picture is over {MAX_PICTURE_SIDE} pixels a side"
        ));
    }
    let decoded =
        image::load_from_memory_with_format(bytes, format).map_err(|error| error.to_string())?;
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(ui::raster::rgba8(rgba.into_raw(), width, height))
}

/// A picture's size in pixels, read from its header.
///
/// # Errors
/// Returns why the bytes are not a picture.
pub fn dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?
        .into_dimensions()
        .map_err(|error| error.to_string())
}

/// The picture behind a content address, from `content`.
pub fn bytes_of(content: &tilo_content::ContentStore, address: &str) -> Option<Vec<u8>> {
    let hash = ui::tilo_actions::content_hash(address)?;
    content.read(hash)
}

/// The frame, crop and turn of an image mark.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Where the picture shows, in the mark's own units.
    pub frame: Rect,
    /// The part of the picture the frame shows, as fractions.
    pub crop: [f64; 4],
    /// Degrees the picture turns inside the frame.
    pub straighten: f64,
}

impl Placement {
    pub fn of(shape: &Shape) -> Option<Self> {
        let Shape::Image {
            x,
            y,
            width,
            height,
            crop,
            straighten,
            ..
        } = shape
        else {
            return None;
        };
        Some(Self {
            frame: Rect::new(*x, *y, x + width, y + height),
            crop: crop.unwrap_or([0.0, 0.0, 1.0, 1.0]),
            straighten: straighten.unwrap_or(0.0),
        })
    }

    /// How much the picture is enlarged so its turn leaves no corner of the
    /// frame uncovered.
    pub fn cover(&self) -> f64 {
        let turn = self.straighten.to_radians().abs();
        let (w, h) = (self.frame.width().max(1e-9), self.frame.height().max(1e-9));
        turn.cos() + (w / h).max(h / w) * turn.sin()
    }

    /// From the picture's pixels (`size` of them) to the mark's units.
    pub fn picture_to_local(&self, size: (u32, u32)) -> Affine {
        let (pw, ph) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
        let [cx, cy, cw, ch] = self.crop;
        let frame = self.frame;
        let fit = Affine::translate((frame.x0, frame.y0))
            * Affine::scale_non_uniform(
                frame.width() / (cw * pw).max(1e-9),
                frame.height() / (ch * ph).max(1e-9),
            )
            * Affine::translate((-cx * pw, -cy * ph));
        if self.straighten == 0.0 {
            return fit;
        }
        let centre = frame.center().to_vec2();
        Affine::translate(centre)
            * Affine::rotate(self.straighten.to_radians())
            * Affine::scale(self.cover())
            * Affine::translate(-centre)
            * fit
    }

    /// Picture pixels per unit as shown, the mean of both ways: fewer when
    /// a turn enlarges the picture.
    pub fn pixels_per_unit(&self, size: (u32, u32)) -> f64 {
        let [.., cw, ch] = self.crop;
        let across = cw * f64::from(size.0) / self.frame.width().max(1e-9);
        let down = ch * f64::from(size.1) / self.frame.height().max(1e-9);
        (across * down).sqrt() / self.cover()
    }

    /// The frame and crop after the frame becomes `frame`, the picture
    /// staying where it is: cropping by moving the frame's edges, kept
    /// within the picture.
    #[must_use]
    pub fn recropped(&self, frame: Rect) -> Self {
        let [cx, cy, cw, ch] = self.crop;
        let old = self.frame;
        let per_x = cw / old.width().max(1e-9);
        let per_y = ch / old.height().max(1e-9);
        let x0 = (cx + (frame.x0 - old.x0) * per_x).clamp(0.0, 1.0);
        let y0 = (cy + (frame.y0 - old.y0) * per_y).clamp(0.0, 1.0);
        let x1 = (cx + (frame.x1 - old.x0) * per_x).clamp(0.0, 1.0);
        let y1 = (cy + (frame.y1 - old.y0) * per_y).clamp(0.0, 1.0);
        let (x1, y1) = (x1.max(x0 + 1e-3), y1.max(y0 + 1e-3));
        let new_frame = Rect::new(
            old.x0 + (x0 - cx) / per_x,
            old.y0 + (y0 - cy) / per_y,
            old.x0 + (x1 - cx) / per_x,
            old.y0 + (y1 - cy) / per_y,
        );
        Self {
            frame: new_frame,
            crop: [x0, y0, x1 - x0, y1 - y0],
            straighten: self.straighten,
        }
    }

    /// The placement showing `crop` of the picture, the picture staying
    /// where it is.
    #[must_use]
    pub fn with_crop(&self, crop: [f64; 4]) -> Self {
        let [cx, cy, cw, ch] = self.crop;
        let old = self.frame;
        let per_x = cw / old.width().max(1e-9);
        let per_y = ch / old.height().max(1e-9);
        let [x, y, w, h] = crop;
        Self {
            frame: Rect::new(
                old.x0 + (x - cx) / per_x,
                old.y0 + (y - cy) / per_y,
                old.x0 + (x + w - cx) / per_x,
                old.y0 + (y + h - cy) / per_y,
            ),
            crop,
            straighten: self.straighten,
        }
    }

    /// The image shape with this placement.
    pub fn apply(&self, shape: &mut Shape) {
        if let Shape::Image {
            x,
            y,
            width,
            height,
            crop,
            straighten,
            ..
        } = shape
        {
            *x = round(self.frame.x0);
            *y = round(self.frame.y0);
            *width = round(self.frame.width());
            *height = round(self.frame.height());
            let whole = self
                .crop
                .iter()
                .zip([0.0, 0.0, 1.0, 1.0])
                .all(|(have, full)| (have - full).abs() < 1e-6);
            *crop = (!whole).then(|| self.crop.map(|value| (value * 1e5).round() / 1e5));
            *straighten = (self.straighten != 0.0).then_some(self.straighten);
        }
    }
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// A decoded picture and its halved copies, smallest last.
struct Pyramid {
    levels: Vec<ImageData>,
    used: u64,
}

impl Pyramid {
    /// The copy at `level` (each halves the one before), made when first
    /// asked for.
    fn level(&mut self, level: usize) -> ImageData {
        while self.levels.len() <= level {
            let Some(last) = self.levels.last() else {
                break;
            };
            if last.width.max(last.height) <= 64 {
                break;
            }
            let half = halve(last);
            self.levels.push(half);
        }
        let at = level.min(self.levels.len().saturating_sub(1));
        self.levels.get(at).cloned().unwrap_or_else(blank)
    }
}

fn blank() -> ImageData {
    ui::raster::rgba8(vec![0; 4], 1, 1)
}

/// `image` at half the size, each pixel the mean of the two by two it
/// covers.
fn halve(image: &ImageData) -> ImageData {
    let (width, height) = (image.width as usize, image.height as usize);
    let (half_w, half_h) = (width.div_ceil(2).max(1), height.div_ceil(2).max(1));
    let source = image.data.data();
    let mut out = vec![0_u8; half_w * half_h * 4];
    for (index, pixel) in out.chunks_exact_mut(4).enumerate() {
        let (x, y) = ((index % half_w) * 2, (index / half_w) * 2);
        let mut sums = [0_u32; 4];
        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let (sx, sy) = ((x + dx).min(width - 1), (y + dy).min(height - 1));
            let start = (sy * width + sx) * 4;
            for (sum, value) in sums
                .iter_mut()
                .zip(source.get(start..start + 4).unwrap_or(&[0; 4]))
            {
                *sum += u32::from(*value);
            }
        }
        for (channel, sum) in pixel.iter_mut().zip(sums) {
            *channel = ((sum + 2) / 4) as u8;
        }
    }
    image_of(out, half_w, half_h)
}

/// RGBA bytes as an image.
pub fn image_of(pixels: Vec<u8>, width: usize, height: usize) -> ImageData {
    ui::raster::rgba8(
        pixels,
        u32::try_from(width).unwrap_or(1),
        u32::try_from(height).unwrap_or(1),
    )
}

/// What a picture is graded with: the per-pixel steps, then blur and
/// sharpen in the picture's own pixels.
#[derive(Clone, Debug, Default)]
pub struct Treatment {
    pub grades: Vec<Grade>,
    /// Blur radius and sharpen amount, in pixels of the full picture.
    pub blur: f64,
    pub sharpen: f64,
    /// What the treatment is, for telling two apart.
    pub key: String,
}

impl Treatment {
    /// The adjustments `adjusts` (a mark's, then its layer's) of a picture
    /// with `pixels_per_unit` pixels to a unit.
    pub fn of(adjusts: &[&Adjust], pixels_per_unit: f64) -> Self {
        let grades: Vec<Grade> = adjusts.iter().filter_map(|adjust| adjust.grade()).collect();
        let blur: f64 = adjusts
            .iter()
            .filter_map(|adjust| adjust.blur)
            .map(|blur| blur * pixels_per_unit)
            .sum();
        let sharpen = adjusts
            .iter()
            .filter_map(|adjust| adjust.sharpen)
            .fold(0.0, f64::max);
        let key = if grades.is_empty() && blur <= 0.0 && sharpen <= 0.0 {
            String::new()
        } else {
            let named: Vec<String> = adjusts
                .iter()
                .map(|adjust| serde_json::to_string(adjust).unwrap_or_default())
                .collect();
            format!("{}|{blur:.3}|{sharpen:.3}", named.join("|"))
        };
        Self {
            grades,
            blur,
            sharpen,
            key,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.key.is_empty()
    }

    /// RGBA pixels at `scale` of the full picture (a halved copy is 0.5)
    /// treated.
    pub fn run(&self, pixels: &mut [u8], width: usize, height: usize, scale: f64) {
        crate::adjust::grade_pixels(pixels, &self.grades);
        if self.blur > 0.0 {
            crate::adjust::blur(pixels, width, height, (self.blur * scale) as f32);
        }
        if self.sharpen > 0.0 {
            // A fine detail's width: about a pixel and a half of the full
            // picture, at least one of the copy's.
            let radius = (1.5 * scale).max(1.0) as f32;
            crate::adjust::sharpen(pixels, width, height, self.sharpen as f32, radius);
        }
    }
}

/// The most bytes of treated copies kept.
const KEPT_BYTES: usize = 384 << 20;
/// The most pictures kept decoded.
const KEPT_PICTURES: usize = 12;

/// A picture decoded, and its halved copies made, off the UI thread.
type Decoding = std::sync::mpsc::Receiver<Option<Vec<ImageData>>>;

/// Decoded pictures and treated copies, kept between frames.
pub struct Pictures {
    decoded: HashMap<String, Option<Pyramid>>,
    treated: Vec<(String, ImageData, u64)>,
    clock: u64,
    /// Pictures being decoded in the background, by address.
    decoding: HashMap<String, Decoding>,
    /// Whether a picture not decoded yet is waited for (a look, an
    /// export: they must show it) or decoded in the background while a
    /// placeholder shows (the canvas: a frame must not wait).
    pub patient: bool,
    /// The store the pictures' bytes are read from: the window's, handed
    /// over when the window attaches. Without one, the store of the window
    /// whose code is running.
    content: Option<tilo_content::ContentStore>,
}

impl Default for Pictures {
    fn default() -> Self {
        Self {
            decoded: HashMap::new(),
            treated: Vec::new(),
            clock: 0,
            decoding: HashMap::new(),
            patient: true,
            content: None,
        }
    }
}

/// A picture's bytes decoded and halved down to a thumbnail's size, every
/// copy made at once: work for a background thread.
fn decode_all(content: &tilo_content::ContentStore, address: &str) -> Option<Vec<ImageData>> {
    let full = bytes_of(content, address).and_then(|bytes| decode(&bytes).ok())?;
    let mut levels = vec![full];
    while let Some(last) = levels.last() {
        if last.width.max(last.height) <= 64 {
            break;
        }
        let half = halve(last);
        levels.push(half);
    }
    Some(levels)
}

impl Pictures {
    /// Read the pictures' bytes from `content` from now on.
    pub fn read_from(&mut self, content: tilo_content::ContentStore) {
        self.content = Some(content);
    }

    fn content(&self) -> tilo_content::ContentStore {
        self.content.clone().unwrap_or_else(crate::content::store)
    }

    /// Whether pictures are still being decoded in the background: the
    /// canvas draws again until they are.
    pub fn decoding(&self) -> bool {
        !self.decoding.is_empty()
    }

    /// Keep the pictures whose background decoding has finished.
    fn collect(&mut self) {
        let finished: Vec<(String, Option<Vec<ImageData>>)> = self
            .decoding
            .iter()
            .filter_map(|(address, channel)| match channel.try_recv() {
                Ok(levels) => Some((address.clone(), levels)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some((address.clone(), None)),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
            })
            .collect();
        for (address, levels) in finished {
            self.decoding.remove(&address);
            self.keep(address, levels);
        }
    }

    /// Keep `levels` of the picture at `address`, making room first.
    fn keep(&mut self, address: String, levels: Option<Vec<ImageData>>) {
        if self.decoded.len() >= KEPT_PICTURES
            && let Some(oldest) = self
                .decoded
                .iter()
                .min_by_key(|(_, pyramid)| pyramid.as_ref().map_or(0, |p| p.used))
                .map(|(key, _)| key.clone())
        {
            self.decoded.remove(&oldest);
        }
        self.decoded
            .insert(address, levels.map(|levels| Pyramid { levels, used: 0 }));
    }

    /// Start decoding the picture at `address` in the background; answers
    /// whether it started.
    fn start(&mut self, address: &str) -> bool {
        if self.decoding.contains_key(address) {
            return true;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let owned = address.to_owned();
        // The thread reads the window's store, handed to it here.
        let content = self.content();
        let started = ui::rt::background::Background::current()
            .spawn("canvas-picture", move || {
                let _ = sender.send(decode_all(&content, &owned));
            })
            .is_ok();
        if started {
            self.decoding.insert(address.to_owned(), receiver);
        }
        started
    }

    /// The full picture at `address`, decoded once.
    pub fn full(&mut self, address: &str) -> Option<ImageData> {
        self.pyramid(address).map(|pyramid| pyramid.level(0))
    }

    /// The picture's size in pixels.
    pub fn size(&mut self, address: &str) -> Option<(u32, u32)> {
        self.pyramid(address)
            .and_then(|pyramid| pyramid.levels.first())
            .map(|full| (full.width, full.height))
    }

    fn pyramid(&mut self, address: &str) -> Option<&mut Pyramid> {
        self.clock += 1;
        self.collect();
        if !self.decoded.contains_key(address) {
            if !self.patient && self.start(address) {
                return None;
            }
            let levels = match self.decoding.remove(address) {
                // Already on its way: wait for it rather than decode twice.
                Some(channel) => channel.recv().ok().flatten(),
                None => bytes_of(&self.content(), address)
                    .and_then(|bytes| decode(&bytes).ok())
                    .map(|full| vec![full]),
            };
            self.keep(address.to_owned(), levels);
        }
        let clock = self.clock;
        let pyramid = self.decoded.get_mut(address)?.as_mut()?;
        pyramid.used = clock;
        Some(pyramid)
    }

    /// The picture at `address` as drawn `device` screen pixels to each of
    /// its own, treated: the smallest halved copy that still has a pixel
    /// for every one shown, and how many full pixels each of its pixels is.
    pub fn shown(
        &mut self,
        address: &str,
        device: f64,
        treatment: &Treatment,
    ) -> Option<(ImageData, f64)> {
        let level = if device >= 1.0 || !device.is_finite() || device <= 0.0 {
            0
        } else {
            (1.0 / device).log2().floor().clamp(0.0, 12.0) as usize
        };
        let base = self.pyramid(address)?.level(level);
        let full = self.size(address)?;
        let step = f64::from(full.0) / f64::from(base.width.max(1));
        if treatment.is_empty() {
            return Some((base, step));
        }
        let key = format!("{address}|{level}|{}", treatment.key);
        self.clock += 1;
        let clock = self.clock;
        if let Some(entry) = self.treated.iter_mut().find(|entry| entry.0 == key) {
            entry.2 = clock;
            return Some((entry.1.clone(), step));
        }
        let mut pixels = base.data.data().to_vec();
        let (width, height) = (base.width as usize, base.height as usize);
        treatment.run(&mut pixels, width, height, 1.0 / step);
        let treated = image_of(pixels, width, height);
        self.treated.push((key, treated.clone(), clock));
        let mut total: usize = self
            .treated
            .iter()
            .map(|entry| entry.1.data.data().len())
            .sum();
        while total > KEPT_BYTES && self.treated.len() > 1 {
            let Some(oldest) = self
                .treated
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.2)
                .map(|(index, _)| index)
            else {
                break;
            };
            let removed = self.treated.remove(oldest);
            total = total.saturating_sub(removed.1.data.data().len());
        }
        Some((treated, step))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(crop: [f64; 4]) -> Placement {
        Placement {
            frame: Rect::new(10.0, 20.0, 410.0, 320.0),
            crop,
            straighten: 0.0,
        }
    }

    #[test]
    fn the_whole_picture_fills_its_frame() {
        let map = placement([0.0, 0.0, 1.0, 1.0]).picture_to_local((800, 600));
        let corner = map * ui::vello::kurbo::Point::new(800.0, 600.0);
        assert!((corner.x - 410.0).abs() < 1e-9 && (corner.y - 320.0).abs() < 1e-9);
    }

    #[test]
    fn a_crop_keeps_the_picture_where_it_was() {
        let whole = placement([0.0, 0.0, 1.0, 1.0]);
        let cropped = whole.recropped(Rect::new(110.0, 20.0, 410.0, 220.0));
        assert!((cropped.crop[0] - 0.25).abs() < 1e-9);
        assert!((cropped.crop[3] - 2.0 / 3.0).abs() < 1e-9);
        let size = (800, 600);
        let pixel = ui::vello::kurbo::Point::new(400.0, 100.0);
        let before = whole.picture_to_local(size) * pixel;
        let after = cropped.picture_to_local(size) * pixel;
        assert!(before.distance(after) < 1e-6);
    }

    #[test]
    fn a_crop_stops_at_the_picture_edge() {
        let whole = placement([0.0, 0.0, 1.0, 1.0]);
        let grown = whole.recropped(Rect::new(-90.0, 20.0, 410.0, 320.0));
        assert!(grown.crop[0].abs() < 1e-12);
        assert!((grown.frame.x0 - 10.0).abs() < 1e-9);
    }

    #[test]
    fn a_straightened_picture_covers_its_frame() {
        let mut turned = placement([0.0, 0.0, 1.0, 1.0]);
        turned.straighten = 10.0;
        let map = turned.picture_to_local((800, 600)).inverse();
        for corner in [
            turned.frame.origin(),
            ui::vello::kurbo::Point::new(turned.frame.x1, turned.frame.y1),
            ui::vello::kurbo::Point::new(turned.frame.x0, turned.frame.y1),
            ui::vello::kurbo::Point::new(turned.frame.x1, turned.frame.y0),
        ] {
            let at = map * corner;
            assert!(at.x >= -1e-6 && at.y >= -1e-6 && at.x <= 800.0 + 1e-6 && at.y <= 600.0 + 1e-6);
        }
    }

    #[test]
    fn halving_averages() {
        let image = image_of(
            vec![
                0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
            ],
            2,
            2,
        );
        let half = halve(&image);
        assert_eq!((half.width, half.height), (1, 1));
        assert_eq!(half.data.data(), &[128, 128, 128, 255]);
    }
}

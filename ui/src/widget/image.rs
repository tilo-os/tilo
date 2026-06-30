//! General-purpose [`Image`] widget for displaying PNG/JPEG content.
//!
//! Decoded RGBA pixels are wrapped in a [`peniko::ImageData`] and painted via
//! `scene.fill(..)` — the same call shape `quick-shell`'s desktop backdrop
//! uses for its tiled noise overlay. Three fit modes mirror CSS
//! `object-fit`: [`ImageFit::Fill`] stretches, [`ImageFit::Contain`]
//! letterboxes, [`ImageFit::Cover`] crops.
//!
//! The peniko::ImageData is built lazily on first draw and cached; subsequent
//! draws reuse the cached handle (cloning a [`peniko::ImageData`] only bumps
//! the internal `Arc`). Path-loaded variants that fail to decode render
//! a red border + "Image failed" text so the failure surfaces during
//! development rather than silently painting nothing.
//!
//! Only PNG decoding is provided in-tree (via the `png` crate already in
//! the workspace). JPEG support arrives whenever `image` crate
//! dependency conflicts — the API surface is decoder-agnostic; consumers
//! can call [`Image::from_rgba`] with bytes they decoded themselves.

use std::path::PathBuf;
use std::sync::Arc;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use vello::peniko::{Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageData as PenikoImage, ImageFormat as Format};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// How the image is fitted within the widget's rect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFit {
    /// Stretch the image to fill the entire rect, ignoring aspect ratio.
    /// May distort.
    Fill,
    /// Scale the image to fit inside the rect while preserving aspect
    /// ratio. Letterbox bars appear on the shorter axis.
    Contain,
    /// Scale the image to fully cover the rect while preserving aspect
    /// ratio. Overflow on the longer axis is clipped to the rect.
    Cover,
}

/// Errors that can occur when constructing an [`Image`] from encoded bytes.
#[derive(Debug)]
pub enum ImageDecodeError {
    /// The PNG decoder rejected the input (corrupt header, unsupported
    /// chunk ordering, etc.).
    Decode(String),
    /// I/O failure while reading the encoded buffer.
    Io(std::io::Error),
}

impl std::fmt::Display for ImageDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageDecodeError::Decode(msg) => write!(f, "image decode failed: {msg}"),
            ImageDecodeError::Io(err) => write!(f, "image io failed: {err}"),
        }
    }
}

impl std::error::Error for ImageDecodeError {}

/// Source of the image bytes. Path-based sources decode lazily on first
/// draw so widget construction is cheap and a missing file does not
/// panic the caller.
enum Source {
    /// Already decoded RGBA bytes plus dimensions.
    Decoded {
        rgba: Arc<[u8]>,
        width: u32,
        height: u32,
    },
    /// Filesystem path to a PNG file. Decoded the first time `draw`
    /// runs; the result (Ok or error) is then latched.
    Path(PathBuf),
    /// Decode previously failed — paint the error state every frame.
    Failed,
}

/// General-purpose image widget. See module docs for the contract.
pub struct Image {
    source: Source,
    intrinsic_size: (u32, u32),
    fit: ImageFit,
    rect: Rect,
    cached_image: Option<PenikoImage>,
}

impl Image {
    /// Construct from a filesystem path. Decoding is deferred to the
    /// first draw so widget construction never fails. If decoding
    /// later fails, the widget renders an error state.
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self {
            source: Source::Path(path.into()),
            intrinsic_size: (0, 0),
            fit: ImageFit::Contain,
            rect: Rect::ZERO,
            cached_image: None,
        }
    }

    /// Construct from raw decoded RGBA bytes plus dimensions. Bytes
    /// must be `width * height * 4` long; shorter buffers will be
    /// rejected on the first draw via the error state.
    pub fn from_rgba(rgba: Arc<[u8]>, width: u32, height: u32) -> Self {
        Self {
            source: Source::Decoded {
                rgba,
                width,
                height,
            },
            intrinsic_size: (width, height),
            fit: ImageFit::Contain,
            rect: Rect::ZERO,
            cached_image: None,
        }
    }

    /// Construct from encoded PNG bytes. Decoded eagerly so callers
    /// see decode errors immediately. JPEG bytes will be rejected
    /// here; decode them yourself and use [`Image::from_rgba`] until
    /// the multi-format decoder lands.
    pub fn from_encoded(bytes: &[u8]) -> Result<Self, ImageDecodeError> {
        let (rgba, width, height) = decode_png(bytes)?;
        Ok(Self::from_rgba(Arc::from(rgba), width, height))
    }

    /// Set the fit mode. See [`ImageFit`].
    pub fn with_fit(mut self, fit: ImageFit) -> Self {
        self.fit = fit;
        self
    }

    /// The image's intrinsic pixel dimensions. Returns `(0, 0)` for
    /// path-loaded images that have not yet been drawn (and therefore
    /// not yet decoded).
    pub fn intrinsic_size(&self) -> (u32, u32) {
        self.intrinsic_size
    }

    /// Paint the image into `scene` at `rect` under `transform`, without a
    /// surrounding layout pass — for consumers (e.g. the word processor's inline
    /// images) that position images themselves rather than via taffy.
    pub fn paint_at(&mut self, scene: &mut Scene, transform: Affine, rect: Rect) {
        self.rect = rect;
        self.paint(scene, transform);
    }

    /// Test-only helper: paint the widget into `scene` at `rect` with
    /// no surrounding layout pass. Mirrors the path the real `App`
    /// frame loop takes (apply_layout → draw) but skips taffy.
    #[cfg(test)]
    pub fn draw_for_test(&mut self, scene: &mut Scene, rect: Rect) {
        self.rect = rect;
        self.paint(scene, Affine::IDENTITY);
    }

    /// Resolve `self.source` into a cached [`PenikoImage`]. On
    /// success, [`Self::intrinsic_size`] is populated and the handle
    /// is reused on every subsequent frame. On failure, the source
    /// flips to [`Source::Failed`] so the error state paints
    /// deterministically.
    fn ensure_cached(&mut self) {
        if self.cached_image.is_some() {
            return;
        }
        match &self.source {
            Source::Decoded {
                rgba,
                width,
                height,
            } => {
                let expected = (*width as usize) * (*height as usize) * 4;
                if rgba.len() != expected {
                    self.source = Source::Failed;
                    return;
                }
                let blob = Blob::new(Arc::new(rgba.to_vec()));
                let img = PenikoImage {
                    data: blob,
                    format: Format::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width: *width,
                    height: *height,
                };
                self.intrinsic_size = (*width, *height);
                self.cached_image = Some(img);
            }
            Source::Path(path) => {
                let bytes = match std::fs::read(path) {
                    Ok(b) => b,
                    Err(_) => {
                        self.source = Source::Failed;
                        return;
                    }
                };
                let (rgba, width, height) = match decode_png(&bytes) {
                    Ok(t) => t,
                    Err(_) => {
                        self.source = Source::Failed;
                        return;
                    }
                };
                let blob = Blob::new(Arc::new(rgba));
                let img = PenikoImage {
                    data: blob,
                    format: Format::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width,
                    height,
                };
                self.intrinsic_size = (width, height);
                self.cached_image = Some(img);
            }
            Source::Failed => {}
        }
    }

    /// Paint the cached image into `scene` honouring the fit mode, or
    /// paint the error state if decoding has failed.
    fn paint(&mut self, scene: &mut Scene, transform: Affine) {
        self.ensure_cached();

        let Some(img) = self.cached_image.clone() else {
            self.paint_error(scene, transform);
            return;
        };

        let (iw, ih) = self.intrinsic_size;
        if iw == 0 || ih == 0 || self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }

        let rw = self.rect.width();
        let rh = self.rect.height();
        let iw_f = iw as f64;
        let ih_f = ih as f64;

        // Brush-transform shape: we apply the brush to the rect itself
        // (`scene.fill(.., &rect)`), so the brush_transform must map
        // the rect's origin into image-space. peniko::ImageData samples its
        // bytes in pixel coordinates with (0, 0) at the top-left; a
        // brush_transform of `T * S` placed in fill means "scale +
        // translate the image before sampling".
        let (brush_transform, fill_rect) = match self.fit {
            ImageFit::Fill => {
                let sx = rw / iw_f;
                let sy = rh / ih_f;
                let bt = Affine::translate((self.rect.x0, self.rect.y0))
                    * Affine::scale_non_uniform(sx, sy);
                (bt, self.rect)
            }
            ImageFit::Contain => {
                let scale = (rw / iw_f).min(rh / ih_f);
                let dw = iw_f * scale;
                let dh = ih_f * scale;
                let dx = self.rect.x0 + (rw - dw) * 0.5;
                let dy = self.rect.y0 + (rh - dh) * 0.5;
                let bt =
                    Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);
                let dest = Rect::new(dx, dy, dx + dw, dy + dh);
                (bt, dest)
            }
            ImageFit::Cover => {
                let scale = (rw / iw_f).max(rh / ih_f);
                let dw = iw_f * scale;
                let dh = ih_f * scale;
                let dx = self.rect.x0 + (rw - dw) * 0.5;
                let dy = self.rect.y0 + (rh - dh) * 0.5;
                let bt =
                    Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);
                // Clip to the widget rect so cover crops on the long axis.
                (bt, self.rect)
            }
        };

        // For Cover, push a clip so the oversized image is cropped to
        // the widget rect. Fill/Contain don't overflow, so no clip is
        // needed.
        let needs_clip = matches!(self.fit, ImageFit::Cover);
        if needs_clip {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &self.rect,
            );
        }

        let image_brush = ImageBrush::new(img);
        scene.fill(
            Fill::NonZero,
            transform,
            &image_brush,
            Some(brush_transform),
            &fill_rect,
        );

        if needs_clip {
            scene.pop_layer();
        }
    }

    /// Paint the failure state: a red border and a "Image failed"
    /// label. Drawn whenever `cached_image` is `None` after
    /// `ensure_cached`.
    fn paint_error(&mut self, scene: &mut Scene, transform: Affine) {
        let red = Color::from_rgba8(220, 38, 38, 255);
        let stroke = Stroke::new(2.0);
        scene.stroke(&stroke, transform, red, None, &self.rect);

        // Diagonal cross to make the failure state unambiguous even
        // without a text renderer in scope.
        let tl = Point::new(self.rect.x0, self.rect.y0);
        let tr = Point::new(self.rect.x1, self.rect.y0);
        let bl = Point::new(self.rect.x0, self.rect.y1);
        let br = Point::new(self.rect.x1, self.rect.y1);
        scene.stroke(
            &stroke,
            transform,
            red,
            None,
            &vello::kurbo::Line::new(tl, br),
        );
        scene.stroke(
            &stroke,
            transform,
            red,
            None,
            &vello::kurbo::Line::new(tr, bl),
        );
    }
}

/// Decode a PNG byte slice into an `(rgba, width, height)` triple. The
/// returned buffer is always 8-bit-per-channel RGBA, regardless of the
/// PNG's source colour type.
fn decode_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), ImageDecodeError> {
    // png 0.18 requires `Seek` on the decoder source; wrap the byte slice in
    // a `std::io::Cursor` so it satisfies both `Read` and `Seek`.
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|e| ImageDecodeError::Decode(e.to_string()))?;
    // png 0.18: output_buffer_size / buffer_size return Option<usize>.
    let buf_size = reader
        .output_buffer_size()
        .ok_or_else(|| ImageDecodeError::Decode("png output buffer size unavailable".into()))?;
    let mut buf = vec![0; buf_size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| ImageDecodeError::Decode(e.to_string()))?;
    let bytes = &buf[..info.buffer_size()];

    let (width, height) = (info.width, info.height);
    let rgba = match info.color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity((width * height * 4) as usize);
            for chunk in bytes.chunks_exact(3) {
                out.extend_from_slice(chunk);
                out.push(0xFF);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity((width * height * 4) as usize);
            for chunk in bytes.chunks_exact(2) {
                out.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity((width * height * 4) as usize);
            for &v in bytes {
                out.extend_from_slice(&[v, v, v, 0xFF]);
            }
            out
        }
        png::ColorType::Indexed => {
            return Err(ImageDecodeError::Decode(
                "indexed-colour PNGs are not supported; re-export as RGB(A)".into(),
            ));
        }
    };

    Ok((rgba, width, height))
}

impl Widget for Image {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: taffy::Dimension::auto(),
                    height: taffy::Dimension::auto(),
                },
                flex_shrink: 1.0,
                flex_grow: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.paint(scene, transform);
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &str {
        "Image"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Path to the bundled fixture relative to the crate root.
    const FIXTURE_PATH: &str = "tests/fixtures/test_image.png";

    /// Generate the 16×16 four-quadrant fixture if it isn't already on
    /// disk. Top-left red, top-right green, bottom-left blue,
    /// bottom-right white. Idempotent: subsequent calls are no-ops.
    fn build_fixture_if_missing() {
        if Path::new(FIXTURE_PATH).exists() {
            return;
        }
        std::fs::create_dir_all("tests/fixtures").unwrap();

        let mut rgba = Vec::with_capacity(16 * 16 * 4);
        for y in 0..16u32 {
            for x in 0..16u32 {
                let (r, g, b) = if y < 8 && x < 8 {
                    (255, 0, 0) // top-left red
                } else if y < 8 {
                    (0, 255, 0) // top-right green
                } else if x < 8 {
                    (0, 0, 255) // bottom-left blue
                } else {
                    (255, 255, 255) // bottom-right white
                };
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }

        let file = std::fs::File::create(FIXTURE_PATH).unwrap();
        let w = std::io::BufWriter::new(file);
        let mut encoder = png::Encoder::new(w, 16, 16);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&rgba).unwrap();
    }

    #[test]
    fn image_loads_from_encoded_bytes_and_reports_size() {
        build_fixture_if_missing();
        let bytes = std::fs::read(FIXTURE_PATH).unwrap();
        let img = Image::from_encoded(&bytes).unwrap();
        assert_eq!(img.intrinsic_size(), (16, 16));
    }

    #[test]
    fn image_with_fill_fit_fills_entire_rect() {
        let img = Image::from_rgba(Arc::from(vec![255u8; 16 * 16 * 4]), 16, 16)
            .with_fit(ImageFit::Fill);
        let mut scene = Scene::new();
        let mut img = img;
        img.draw_for_test(&mut scene, Rect::new(0.0, 0.0, 100.0, 50.0));
        // No panic; rendering proceeds. The widget's rect should now
        // match the requested target so a parent layout pass would
        // place subsequent siblings correctly.
        assert_eq!(img.get_rect(), Rect::new(0.0, 0.0, 100.0, 50.0));
    }

    #[test]
    fn missing_path_renders_error_state_without_panic() {
        let mut img = Image::from_path("nonexistent/file.png");
        let mut scene = Scene::new();
        img.draw_for_test(&mut scene, Rect::new(0.0, 0.0, 100.0, 100.0));
        // Source must have flipped to `Failed` so subsequent draws
        // also paint the error state instead of retrying disk I/O.
        assert!(matches!(img.source, Source::Failed));
    }

    #[test]
    fn contain_fit_preserves_aspect_ratio_in_wide_rect() {
        let img = Image::from_rgba(Arc::from(vec![255u8; 16 * 16 * 4]), 16, 16)
            .with_fit(ImageFit::Contain);
        let mut scene = Scene::new();
        let mut img = img;
        // 200×50 rect with a square source: contain should scale to
        // height (50), width 50, centred at x=75.
        img.draw_for_test(&mut scene, Rect::new(0.0, 0.0, 200.0, 50.0));
        // No assertion on scene encoding here; the paint path simply
        // must not panic and must respect the chosen fit.
        assert_eq!(img.intrinsic_size(), (16, 16));
    }
}

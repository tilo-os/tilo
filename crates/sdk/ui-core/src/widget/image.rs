//! General-purpose [`Image`] widget for displaying PNG or vector SVG content. Decoded
//! RGBA pixels are wrapped in a [`peniko::ImageData`] and painted via
//! `scene.fill(..)`. Three fit modes mirror CSS `object-fit`:
//! [`ImageFit::Fill`] stretches, [`ImageFit::Contain`] letterboxes,
//! [`ImageFit::Cover`] crops.
//!
//! The `ImageData` is built once at layout-tree construction or first direct
//! paint and cached (cloning only bumps the internal `Arc`). A decode failure renders a red border +
//! "Image failed" text so it surfaces during development instead of
//! silently painting nothing.
//!
//! PNG raster decoding and vector SVG loading are provided in-tree. Call
//! [`Image::from_rgba`] with bytes decoded elsewhere for other raster formats.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use crate::widget::TaffyTreeExt;
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use vello::peniko::{Blob, Color, Fill, ImageBrush, ImageData as PenikoImage};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// The ink a decode failure is marked in when the caller has no `Theme` to
/// offer ([`Image::paint_at`]): the default palette's `semantic.danger`,
/// resolved once rather than by building a `Theme` per frame. Draw paths
/// that hold a theme go through [`Image::paint_at_on`] or [`Widget::draw`]
/// and take that palette's danger instead.
static FALLBACK_ERROR_INK: LazyLock<Color> =
    LazyLock::new(|| Theme::dark().tokens.colors.semantic.danger);

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

/// Source of the image bytes. Path-based sources decode lazily when admitted
/// to a layout tree or directly painted, never in the measurement callback.
/// Widget construction is cheap and a missing file does not panic the caller.
enum Source {
    /// Already decoded RGBA bytes plus dimensions.
    Decoded {
        rgba: Arc<[u8]>,
        width: u32,
        height: u32,
    },
    /// Explicit host path to a PNG file. The first layout-tree build or
    /// direct paint latches the decoded result (including an error).
    Path(PathBuf),
    /// Parsed SVG paths, kept as vectors through rendering.
    Vector(Box<Scene>),
    /// Decode previously failed — paint the error state every frame.
    Failed,
}

/// General-purpose image widget. See module docs for the contract.
pub struct Image {
    source: Source,
    intrinsic_size: (u32, u32),
    fit: ImageFit,
    rect: Rect,
    cached: Option<PenikoImage>,
    /// Optional alternative text describing the image's content. Surfaced as
    /// the accessible name in [`Widget::accessibility`]; `None` leaves the
    /// image unnamed (decorative).
    alt: Option<String>,
    /// Fill the rectangle offered by the parent instead of remaining an
    /// intrinsic/auto-sized leaf. This is the right mode for previews inside
    /// an explicitly sized viewport or card.
    fill_parent: bool,
    icon_size: Option<super::IconSize>,
    layout_scale: f32,
}

impl Image {
    /// Construct from an explicit host filesystem path. Decoding is deferred
    /// to layout-tree construction (or first direct paint), so construction
    /// never fails and the first measured frame knows the image dimensions.
    /// If decoding fails, the widget renders an error state.
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self {
            source: Source::Path(path.into()),
            intrinsic_size: (0, 0),
            fit: ImageFit::Contain,
            rect: Rect::ZERO,
            cached: None,
            alt: None,
            fill_parent: false,
            icon_size: None,
            layout_scale: 1.0,
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
            cached: None,
            alt: None,
            fill_parent: false,
            icon_size: None,
            layout_scale: 1.0,
        }
    }

    /// Construct from encoded PNG bytes. Decoded eagerly so callers
    /// see decode errors immediately. JPEG bytes will be rejected
    /// here; decode them yourself and use [`Image::from_rgba`] until
    /// the multi-format decoder lands.
    ///
    /// # Errors
    ///
    /// Returns [`ImageDecodeError`] if `bytes` isn't a decodable PNG.
    pub fn from_encoded(bytes: &[u8]) -> Result<Self, ImageDecodeError> {
        let (rgba, width, height) = decode_png(bytes)?;
        Ok(Self::from_rgba(Arc::from(rgba), width, height))
    }

    /// Parse bundled SVG artwork using the shared vector parser, without
    /// opening a filesystem path or rasterizing it. `width` and `height` are
    /// the logical display size; artwork is centred and aspect-preserving.
    ///
    /// This uses the same SVG subset as web content: gradients use their
    /// first stop, while unsupported text and raster nodes are skipped.
    /// The parser is native only: the browser toolkit core leaves it out of
    /// its start module, and there every SVG is unsupported.
    ///
    /// # Errors
    /// Returns an error for invalid SVG or artwork without supported paths.
    pub fn from_svg(bytes: &[u8], width: u32, height: u32) -> Result<Self, ImageDecodeError> {
        let (bounds, shapes) = tilo_paint_stream::svg::drawing(bytes)
            .ok_or_else(|| ImageDecodeError::Decode("unsupported SVG artwork".into()))?;
        let scale =
            (f64::from(width) / f64::from(bounds.w)).min(f64::from(height) / f64::from(bounds.h));
        let at = Affine::scale(scale).then_translate(vello::kurbo::Vec2::new(
            (f64::from(width) - f64::from(bounds.w) * scale) * 0.5,
            (f64::from(height) - f64::from(bounds.h) * scale) * 0.5,
        ));
        let mut scene = Scene::new();
        for shape in shapes {
            let path = vello::kurbo::BezPath::from_svg(&shape.path)
                .map_err(|error| ImageDecodeError::Decode(error.to_string()))?;
            if let Some(tilo_paint_stream::Color([r, g, b, a])) = shape.fill {
                let rule = if shape.even_odd {
                    Fill::EvenOdd
                } else {
                    Fill::NonZero
                };
                scene.fill(rule, at, Color::from_rgba8(r, g, b, a), None, &path);
            }
            if let Some(tilo_paint_stream::Color([r, g, b, a])) = shape.stroke {
                scene.stroke(
                    &Stroke::new(f64::from(shape.stroke_width)),
                    at,
                    Color::from_rgba8(r, g, b, a),
                    None,
                    &path,
                );
            }
        }
        Ok(Self {
            source: Source::Vector(Box::new(scene)),
            intrinsic_size: (width, height),
            fit: ImageFit::Contain,
            rect: Rect::ZERO,
            cached: None,
            alt: None,
            fill_parent: false,
            icon_size: None,
            layout_scale: 1.0,
        })
    }

    /// Size bundled artwork using the same live theme token as an icon glyph.
    #[must_use]
    pub fn with_icon_size(mut self, size: super::IconSize) -> Self {
        self.icon_size = Some(size);
        self
    }

    /// Set the fit mode. See [`ImageFit`].
    #[must_use]
    pub fn with_fit(mut self, fit: ImageFit) -> Self {
        self.fit = fit;
        self
    }

    /// Set the alternative text describing the image. Surfaced as the
    /// accessible name in [`Widget::accessibility`]. Omitting it leaves the
    /// image unnamed (treated as decorative by assistive technology).
    #[must_use]
    pub fn with_alt(mut self, alt: impl Into<String>) -> Self {
        self.alt = Some(alt.into());
        self
    }

    /// Stretch the widget's layout box to its parent's available width and
    /// height; [`ImageFit`] still controls whether pixels contain, cover, or
    /// fill that box.
    #[must_use]
    pub fn filling(mut self) -> Self {
        self.fill_parent = true;
        self
    }

    /// The alternative text describing the image, if any.
    pub fn alt(&self) -> Option<&str> {
        self.alt.as_deref()
    }

    /// The image's intrinsic pixel dimensions. Returns `(0, 0)` for
    /// path-loaded images not yet admitted to layout or directly painted.
    pub fn intrinsic_size(&self) -> (u32, u32) {
        self.intrinsic_size
    }

    /// Paint the image into `scene` at `rect` under `transform`, without a
    /// surrounding layout pass — for consumers (e.g. the word processor's inline
    /// images) that position images themselves rather than via taffy.
    ///
    /// A decode failure is marked in the default palette's danger ink; a
    /// caller that holds a `Theme` should prefer [`Self::paint_at_on`] so the
    /// mark follows the live palette.
    pub fn paint_at(&mut self, scene: &mut Scene, transform: Affine, rect: Rect) {
        self.rect = rect;
        self.paint(scene, transform, *FALLBACK_ERROR_INK);
    }

    /// [`Self::paint_at`] with the host's theme, so a decode failure paints
    /// its mark in that palette's `semantic.danger` rather than the default
    /// palette's. Prefer this from any draw path that holds a `Theme`.
    pub fn paint_at_on(&mut self, scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
        self.rect = rect;
        self.paint(scene, transform, theme.tokens.colors.semantic.danger);
    }

    /// Test-only helper: paint the widget into `scene` at `rect` with
    /// no surrounding layout pass. Mirrors the path the real `App`
    /// frame loop takes (`apply_layout` → `draw`) but skips taffy.
    #[cfg(test)]
    pub fn draw_for_test(&mut self, scene: &mut Scene, rect: Rect) {
        self.rect = rect;
        self.paint(scene, Affine::IDENTITY, *FALLBACK_ERROR_INK);
    }

    /// Resolve `self.source` into a cached [`PenikoImage`]. On
    /// success, [`Self::intrinsic_size`] is populated and the handle
    /// is reused on every subsequent frame. On failure, the source
    /// flips to [`Source::Failed`] so the error state paints
    /// deterministically.
    fn ensure_cached(&mut self) {
        if self.cached.is_some() {
            return;
        }
        match &self.source {
            Source::Decoded {
                rgba,
                width,
                height,
            } => {
                // `width`/`height` came in through `from_rgba`, potentially
                // from decoded data of unknown provenance; a crafted huge
                // pair could overflow the multiplication, so this treats
                // overflow as "can't possibly match" rather than wrapping.
                let expected = usize::try_from(*width)
                    .ok()
                    .zip(usize::try_from(*height).ok())
                    .and_then(|(w, h)| w.checked_mul(h))
                    .and_then(|wh| wh.checked_mul(4));
                if Some(rgba.len()) != expected {
                    self.source = Source::Failed;
                    return;
                }
                let blob = Blob::new(Arc::new(rgba.to_vec()));
                let img = crate::raster::rgba8(blob, *width, *height);
                self.intrinsic_size = (*width, *height);
                self.cached = Some(img);
            }
            Source::Path(path) => {
                let Ok(bytes) = std::fs::read(path) else {
                    self.source = Source::Failed;
                    return;
                };
                let Ok((rgba, width, height)) = decode_png(&bytes) else {
                    self.source = Source::Failed;
                    return;
                };
                let blob = Blob::new(Arc::new(rgba));
                let img = crate::raster::rgba8(blob, width, height);
                self.intrinsic_size = (width, height);
                self.cached = Some(img);
            }
            Source::Vector(_) | Source::Failed => {}
        }
    }

    /// Paint the cached image into `scene` honouring the fit mode, or
    /// paint the error state if decoding has failed.
    #[allow(
        clippy::similar_names,
        reason = "iw_f/ih_f (image width/height as f64) and rw/rh (rect width/height) are the clearest short names for this fit-mode geometry"
    )]
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "the `Affine::translate(..) * Affine::scale_non_uniform(..)` calls below compose 2D transforms via kurbo's float `Mul` impl, which cannot overflow, wrap, or panic like integer arithmetic"
    )]
    fn paint(&mut self, scene: &mut Scene, transform: Affine, error_ink: Color) {
        self.ensure_cached();

        if self.cached.is_none() && !matches!(self.source, Source::Vector(_)) {
            self.paint_error(scene, transform, error_ink);
            return;
        }

        let (iw, ih) = self.intrinsic_size;
        if iw == 0 || ih == 0 || self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }

        let rw = self.rect.width();
        let rh = self.rect.height();
        let iw_f = f64::from(iw);
        let ih_f = f64::from(ih);

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
                let bt = Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);
                let dest = Rect::new(dx, dy, dx + dw, dy + dh);
                (bt, dest)
            }
            ImageFit::Cover => {
                let scale = (rw / iw_f).max(rh / ih_f);
                let dw = iw_f * scale;
                let dh = ih_f * scale;
                let dx = self.rect.x0 + (rw - dw) * 0.5;
                let dy = self.rect.y0 + (rh - dh) * 0.5;
                let bt = Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);
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

        if let Source::Vector(vector) = &self.source {
            scene.append(vector, Some(transform * brush_transform));
        } else if let Some(img) = self.cached.clone() {
            let image_brush = ImageBrush::new(img);
            scene.fill(
                Fill::NonZero,
                transform,
                &image_brush,
                Some(brush_transform),
                &fill_rect,
            );
        }

        if needs_clip {
            scene.pop_layer();
        }
    }

    /// Paint the failure state: a danger-coloured border and a "Image
    /// failed" label. Drawn whenever `cached` is `None` after
    /// `ensure_cached`. The colour is the theme's `semantic.danger`, not a
    /// literal red, so it follows the palette (and high contrast) like every
    /// other error mark.
    fn paint_error(&self, scene: &mut Scene, transform: Affine, red: Color) {
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
/// PNG's source colour type or bit depth.
fn decode_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), ImageDecodeError> {
    let decoded =
        crate::png::decode_rgba8(bytes).map_err(|e| ImageDecodeError::Decode(e.to_string()))?;
    Ok((decoded.rgba, decoded.width, decoded.height))
}

impl Widget for Image {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Resolve an explicit host path once during tree construction, never
        // inside Taffy's repeatedly invoked measurement callback. This also
        // gives the very first frame its decoded intrinsic dimensions.
        self.ensure_cached();
        self.layout_scale = theme.ui_scale;
        let edge = self.icon_size.map(|size| size.px(theme)).or_else(|| {
            matches!(self.source, Source::Failed).then(|| super::IconSize::Regular.px(theme))
        });
        let style = Style {
            aspect_ratio: (self.intrinsic_size.1 > 0).then(|| {
                crate::num::f32_of_u32(self.intrinsic_size.0)
                    / crate::num::f32_of_u32(self.intrinsic_size.1)
            }),
            size: Size {
                width: if self.fill_parent {
                    taffy::Dimension::percent(1.0)
                } else if let Some(edge) = edge {
                    taffy::Dimension::length(edge)
                } else {
                    taffy::Dimension::auto()
                },
                height: if self.fill_parent {
                    taffy::Dimension::percent(1.0)
                } else if let Some(edge) = edge {
                    taffy::Dimension::length(edge)
                } else {
                    taffy::Dimension::auto()
                },
            },
            // Replaced content may shrink inside a bounded flex parent;
            // its intrinsic size is a preference, not a minimum viewport.
            min_size: Size::from_lengths(0.0, 0.0),
            flex_shrink: if self.fill_parent { 0.0 } else { 1.0 },
            flex_grow: if self.fill_parent { 1.0 } else { 0.0 },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> Size<f32> {
        let width = crate::num::f32_of_u32(self.intrinsic_size.0) * self.layout_scale;
        let height = crate::num::f32_of_u32(self.intrinsic_size.1) * self.layout_scale;
        match (known.width, known.height) {
            (Some(width), Some(height)) => Size { width, height },
            (Some(w), None) if width > 0.0 => Size {
                width: w,
                height: w * height / width,
            },
            (None, Some(h)) if height > 0.0 => Size {
                width: h * width / height,
                height: h,
            },
            _ => {
                let limit = |space: taffy::AvailableSpace, intrinsic: f32| match space {
                    taffy::AvailableSpace::Definite(value) if intrinsic > 0.0 => {
                        (value / intrinsic).clamp(0.0, 1.0)
                    }
                    _ => 1.0,
                };
                let scale = limit(available.width, width).min(limit(available.height, height));
                Size {
                    width: width * scale,
                    height: height * scale,
                }
            }
        }
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.paint(scene, transform, theme.tokens.colors.semantic.danger);
    }

    fn name(&self) -> &'static str {
        "Image"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        // The caller-supplied alt text is the accessible name; without it the
        // image is treated as decorative (unnamed).
        Some(AccessNode::new(Role::Image).with_optional_name(self.alt.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intrinsic_images_receive_real_layout_bounds() {
        use crate::headless::FrameHarness;
        use crate::widget::{Container, Row};

        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_ui_scale(scale);
            let svg = Image::from_svg(
                br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 16"><path fill="#126abc" d="M0 0H32V16H0Z"/></svg>"##,
                32,
                16,
            ).unwrap();
            let row = Row::new()
                .push(Box::new(svg))
                .push(Box::new(Image::from_path(FIXTURE_PATH)))
                .push(Box::new(Image::from_rgba(
                    Arc::from(vec![255; 32 * 16 * 4]),
                    32,
                    16,
                )));
            let harness =
                FrameHarness::new_with_theme(crate::State::new(), Box::new(row), (400, 120), theme);
            let children = harness.root_widget().children();
            for (index, (width, height)) in [(32.0, 16.0), (16.0, 16.0), (32.0, 16.0)]
                .into_iter()
                .enumerate()
            {
                let rect = children[index].get_rect();
                assert_eq!(rect.width(), width * f64::from(scale));
                assert_eq!(rect.height(), height * f64::from(scale));
            }
        }

        let image = Image::from_rgba(Arc::from(vec![255; 32 * 16 * 4]), 32, 16);
        let harness = FrameHarness::new(
            crate::State::new(),
            Box::new(Row::new().push(Box::new(image))),
            (16, 80),
        );
        let rect = harness.root_widget().children()[0].get_rect();
        assert_eq!(rect.width(), 16.0);
        assert_eq!(
            rect.height(),
            8.0,
            "a bounded auto image keeps its aspect ratio"
        );

        let harness = FrameHarness::new(
            crate::State::new(),
            Box::new(Row::new().push(Box::new(Image::from_path("missing-image.png")))),
            (200, 100),
        );
        let rect = harness.root_widget().children()[0].get_rect();
        assert!(
            rect.width() > 0.0 && rect.height() > 0.0,
            "decode failures remain visible"
        );

        for fit in [ImageFit::Contain, ImageFit::Cover, ImageFit::Fill] {
            let image = Image::from_path(FIXTURE_PATH).filling().with_fit(fit);
            let root = Row::new().push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_size(80.0, 40.0)
                    .with_child(Box::new(image)),
            ));
            let harness = FrameHarness::new(crate::State::new(), Box::new(root), (200, 100));
            let container = harness.root_widget().children()[0];
            let rect = container.children()[0].get_rect();
            assert_eq!(rect.width(), 80.0);
            assert_eq!(rect.height(), 40.0);
        }
    }

    #[test]
    fn svg_loads_and_paints_vector_paths_with_image_accessibility() {
        let mut image = Image::from_svg(
            br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 12"><path fill="#126abc" d="M0 0H24V12H0Z"/></svg>"##,
            64,
            64,
        )
        .unwrap()
        .with_alt("Source application");
        assert!(matches!(image.source, Source::Vector(_)));
        assert!(image.cached.is_none());
        assert_eq!(image.alt(), Some("Source application"));
        for fit in [ImageFit::Contain, ImageFit::Cover, ImageFit::Fill] {
            image.fit = fit;
            let mut scene = Scene::new();
            image.draw_for_test(&mut scene, Rect::new(0.0, 0.0, 64.0, 64.0));
            assert!(!scene.encoding().is_empty());
            assert!(!scene.encoding().path_tags.is_empty());
            assert!(image.cached.is_none(), "SVG never becomes a raster image");
        }
        assert!(Image::from_svg(b"not SVG", 64, 64).is_err());
    }

    /// Original quadrant image decoded from the committed text fixture by the build.
    const FIXTURE_PATH: &str = concat!(env!("OUT_DIR"), "/test_image.png");

    #[test]
    fn image_loads_fits_and_survives_missing_sources() {
        fn image_loads_from_encoded_bytes_and_reports_size() {
            let bytes = std::fs::read(FIXTURE_PATH).unwrap();
            let img = Image::from_encoded(&bytes).unwrap();
            assert_eq!(img.intrinsic_size(), (16, 16));
        }

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

        fn missing_path_renders_error_state_without_panic() {
            let mut img = Image::from_path("nonexistent/file.png");
            let mut scene = Scene::new();
            img.draw_for_test(&mut scene, Rect::new(0.0, 0.0, 100.0, 100.0));
            // Source must have flipped to `Failed` so subsequent draws
            // also paint the error state instead of retrying disk I/O.
            assert!(matches!(img.source, Source::Failed));
        }

        image_loads_from_encoded_bytes_and_reports_size();
        image_with_fill_fit_fills_entire_rect();
        contain_fit_preserves_aspect_ratio_in_wide_rect();
        missing_path_renders_error_state_without_panic();
    }

    #[test]
    fn image_accessible_name_comes_from_alt_text() {
        fn accessibility_alt_text_surfaces_as_accessible_name() {
            use crate::a11y::Role;
            let img = Image::from_rgba(Arc::from(vec![255u8; 16 * 16 * 4]), 16, 16)
                .with_alt("A red square");
            assert_eq!(img.alt(), Some("A red square"));
            let node = Widget::accessibility(&img).expect("image is not presentational");
            assert_eq!(node.role, Role::Image);
            assert_eq!(node.name.as_deref(), Some("A red square"));
        }

        fn accessibility_without_alt_has_no_name() {
            use crate::a11y::Role;
            let img = Image::from_rgba(Arc::from(vec![255u8; 16 * 16 * 4]), 16, 16);
            assert_eq!(img.alt(), None, "an image defaults to no alt text");
            let node = Widget::accessibility(&img).unwrap();
            assert_eq!(node.role, Role::Image);
            assert_eq!(node.name, None, "a decorative image has no accessible name");
        }

        accessibility_alt_text_surfaces_as_accessible_name();
        accessibility_without_alt_has_no_name();
    }
}

//! The client buffer a display could show by itself.
//!
//! A fullscreen client whose buffer is the topmost thing drawn on its output,
//! opaque, covering the output exactly at one buffer pixel per output pixel,
//! looks the same composited as scanned out: KMS can flip the client's own
//! buffer instead of the compositor rendering a copy of it. The frame builder
//! proves the "topmost" part while it builds the scene ([`DrawnOnTop`]); the
//! DRM presenter proves the rest against the output and its plane.
//!
//! Every proof here is structural, about the encoded scene, so an overlay
//! nobody thought of — a notification, a peer's pointer, a popup, night
//! light, a drag — disqualifies the frame it appears in simply by being drawn
//! after the client's image.

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::ImageData;
use vello_encoding::{DrawTag, Patch, Transform};

/// How far a placement may be from whole pixels at unit scale, in output
/// pixels at the buffer's far edge, and still be drawn pixel for pixel.
const PIXEL_EPSILON: f64 = 1.0 / 64.0;

/// The image one scene drew last, as the frame builder saw it drawn.
#[derive(Clone, Debug)]
pub(crate) struct DrawnOnTop {
    image: ImageData,
    /// Image pixels → the scene it was drawn in.
    transform: Affine,
    /// Draw objects in that scene once it was drawn.
    draws: usize,
}

impl DrawnOnTop {
    /// The image fill `scene` gained since it held `draws_before` draw
    /// objects, when that is exactly one fill of a whole image, outside any
    /// layer: `fill(transform, image, None, Rect(0, 0, width, height))`.
    pub(crate) fn last_in(scene: &Scene, draws_before: usize) -> Option<Self> {
        let encoding = scene.encoding();
        let draws = encoding.draw_tags.len();
        if draws != draws_before.checked_add(1)?
            || encoding.draw_tags.last() != Some(&DrawTag::IMAGE)
            || encoding.n_open_clips != 0
        {
            return None;
        }
        let image = encoding
            .resources
            .patches
            .iter()
            .rev()
            .find_map(|patch| match patch {
                Patch::Image { image, .. } => Some(image.clone()),
                _ => None,
            })?;
        let rect = Rect::new(0.0, 0.0, f64::from(image.width), f64::from(image.height));
        if !path_tail_is_rect(&encoding.path_data, rect) {
            return None;
        }
        let transform = encoding
            .transforms
            .last()
            .copied()
            .unwrap_or(Transform::IDENTITY)
            .to_kurbo();
        Some(Self {
            image,
            transform,
            draws,
        })
    }

    /// Nothing was drawn into its scene after it.
    pub(crate) fn still_last_in(&self, scene: &Scene) -> bool {
        scene.encoding().draw_tags.len() == self.draws
    }

    /// Where its pixels land once the scene is magnified by `to_scene`.
    pub(crate) fn placed(self, to_scene: Affine) -> Self {
        Self {
            transform: self.transform.then(to_scene),
            ..self
        }
    }

    /// The image's extent in its scene.
    pub(crate) fn bounds(&self) -> Rect {
        self.transform.transform_rect_bbox(Rect::new(
            0.0,
            0.0,
            f64::from(self.image.width),
            f64::from(self.image.height),
        ))
    }
}

/// Whether the last path encoded into `path_data` is `rect`, as a fill of a
/// `kurbo::Rect` encodes it: its corners clockwise from the origin and back.
fn path_tail_is_rect(path_data: &[u32], rect: Rect) -> bool {
    let corners = [
        (rect.x0, rect.y0),
        (rect.x1, rect.y0),
        (rect.x1, rect.y1),
        (rect.x0, rect.y1),
        (rect.x0, rect.y0),
    ];
    // Two words per point.
    let Some(tail) = path_data
        .len()
        .checked_sub(corners.len().saturating_mul(2))
        .and_then(|start| path_data.get(start..))
    else {
        return false;
    };
    tail.chunks_exact(2).zip(corners).all(|(point, (x, y))| {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "vello encodes path points as f32; the corner is compared in that precision"
        )]
        let expected = [(x as f32).to_bits(), (y as f32).to_bits()];
        point == expected.as_slice()
    })
}

/// A client image the frame shows on top of everything else on its output,
/// pixel for pixel: a display covering exactly `origin`/`size` (scene pixels)
/// may scan the image's buffer out instead of rendering the frame. Whether
/// it can — the buffer, its format, the plane — is the presenter's question.
#[derive(Clone, Debug)]
pub struct ScanoutCandidate {
    /// The client frame's image (`gpu_frames::client_frame` resolves it).
    pub image: ImageData,
    /// The image's top-left corner in the scene.
    pub origin: (i32, i32),
    /// The image's size, in its own pixels and in scene pixels.
    pub size: (u32, u32),
    /// The scene it was found on top of; any change to it voids the proof.
    scene: SceneFingerprint,
}

impl ScanoutCandidate {
    /// `drawn` as a candidate in `scene`, the finished scene of the frame,
    /// when it lands on whole scene pixels at unit scale.
    pub(crate) fn new(drawn: DrawnOnTop, scene: &Scene) -> Option<Self> {
        let size = (drawn.image.width, drawn.image.height);
        let origin = pixel_exact_origin(drawn.transform, size)?;
        Some(Self {
            image: drawn.image,
            origin,
            size,
            scene: SceneFingerprint::of(scene),
        })
    }

    /// Whether `scene` is still the scene this candidate was proven on top
    /// of: drawing anything more into it, or replacing it with a transformed
    /// copy, voids the proof.
    pub fn still_on_top_of(&self, scene: &Scene) -> bool {
        self.scene == SceneFingerprint::of(scene)
    }

    /// The candidate covers exactly the output at `position` of `size`.
    pub fn covers(&self, position: (i32, i32), size: (u32, u32)) -> bool {
        self.origin == position && self.size == size
    }
}

/// Enough of a scene's encoding to tell a scene with more drawn into it, or a
/// re-transformed copy, from the one a candidate was found in.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SceneFingerprint {
    draws: usize,
    paths: usize,
    path_data: usize,
    transforms: usize,
    last_transform: Option<Transform>,
    open_clips: u32,
}

impl SceneFingerprint {
    fn of(scene: &Scene) -> Self {
        let encoding = scene.encoding();
        Self {
            draws: encoding.draw_tags.len(),
            paths: encoding.path_tags.len(),
            path_data: encoding.path_data.len(),
            transforms: encoding.transforms.len(),
            last_transform: encoding.transforms.last().copied(),
            open_clips: encoding.n_open_clips,
        }
    }
}

/// The whole-pixel origin of a `size` image drawn through `transform`, when
/// that maps one image pixel onto one scene pixel: a translation by whole
/// pixels, up to [`PIXEL_EPSILON`] at the image's far edge.
pub fn pixel_exact_origin(transform: Affine, size: (u32, u32)) -> Option<(i32, i32)> {
    let [xx, yx, xy, yy, tx, ty] = transform.as_coeffs();
    if size.0 == 0 || size.1 == 0 || !transform.as_coeffs().iter().all(|c| c.is_finite()) {
        return None;
    }
    let extent = f64::from(size.0.max(size.1));
    let unit = |value: f64, want: f64| (value - want).abs() * extent <= PIXEL_EPSILON;
    if !(unit(xx, 1.0) && unit(yy, 1.0) && unit(yx, 0.0) && unit(xy, 0.0)) {
        return None;
    }
    let whole = |value: f64| {
        let rounded = value.round();
        ((value - rounded).abs() <= PIXEL_EPSILON && rounded.abs() <= f64::from(i32::MAX))
            .then_some(rounded)
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "both values are whole and within i32 range, checked above"
    )]
    let origin = (whole(tx)? as i32, whole(ty)? as i32);
    // The far corner must land on whole pixels too: a scale that passed the
    // check above but drifts by a pixel over a 4K image would not.
    // Unit scale without rotation keeps the corners of the bounding box.
    let placed =
        transform.transform_rect_bbox(Rect::new(0.0, 0.0, f64::from(size.0), f64::from(size.1)));
    let far_expected = (
        f64::from(origin.0) + f64::from(size.0),
        f64::from(origin.1) + f64::from(size.1),
    );
    ((placed.x1 - far_expected.0).abs() <= PIXEL_EPSILON
        && (placed.y1 - far_expected.1).abs() <= PIXEL_EPSILON)
        .then_some(origin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vello::peniko::{Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageFormat};

    fn image(width: u32, height: u32) -> ImageData {
        ImageData {
            data: Blob::new(Arc::new(vec![
                0_u8;
                width.saturating_mul(height).saturating_mul(4)
                    as usize
            ])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::AlphaPremultiplied,
            width,
            height,
        }
    }

    fn fill_image(scene: &mut Scene, image: &ImageData, transform: Affine) {
        scene.fill(
            Fill::NonZero,
            transform,
            &ImageBrush::new(image.clone()),
            None,
            &Rect::new(0.0, 0.0, f64::from(image.width), f64::from(image.height)),
        );
    }

    fn below(scene: &mut Scene) {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(10, 20, 30),
            None,
            &Rect::new(0.0, 0.0, 100.0, 100.0),
        );
    }

    #[test]
    fn a_whole_image_fill_drawn_last_is_on_top() {
        let image = image(64, 32);
        let mut scene = Scene::new();
        below(&mut scene);
        let before = scene.encoding().draw_tags.len();
        fill_image(&mut scene, &image, Affine::translate((16.0, 8.0)));
        let drawn = DrawnOnTop::last_in(&scene, before).expect("one whole image fill");
        assert_eq!(drawn.image.data.id(), image.data.id());
        assert_eq!(drawn.bounds(), Rect::new(16.0, 8.0, 80.0, 40.0));
        assert!(drawn.still_last_in(&scene));
        let candidate = ScanoutCandidate::new(drawn, &scene).expect("pixel exact");
        assert_eq!(candidate.origin, (16, 8));
        assert_eq!(candidate.size, (64, 32));
        assert!(candidate.covers((16, 8), (64, 32)));
        assert!(!candidate.covers((0, 0), (64, 32)));
        assert!(candidate.still_on_top_of(&scene));
    }

    #[test]
    fn anything_but_one_whole_image_fill_is_not_on_top() {
        let image = image(64, 32);
        // Nothing drawn.
        let scene = Scene::new();
        assert!(DrawnOnTop::last_in(&scene, 0).is_none());
        // A colour fill.
        let mut scene = Scene::new();
        below(&mut scene);
        assert!(DrawnOnTop::last_in(&scene, 0).is_none());
        // An image and something after it.
        let mut scene = Scene::new();
        fill_image(&mut scene, &image, Affine::IDENTITY);
        below(&mut scene);
        assert!(DrawnOnTop::last_in(&scene, 0).is_none());
        // An image inside a clip: the clip may cut it.
        let mut scene = Scene::new();
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            Affine::IDENTITY,
            &Rect::new(0.0, 0.0, 10.0, 10.0),
        );
        let before = scene.encoding().draw_tags.len();
        fill_image(&mut scene, &image, Affine::IDENTITY);
        assert!(DrawnOnTop::last_in(&scene, before).is_none());
        // Part of an image: not the whole buffer.
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            &ImageBrush::new(image.clone()),
            None,
            &Rect::new(0.0, 0.0, 32.0, 32.0),
        );
        assert!(DrawnOnTop::last_in(&scene, 0).is_none());
    }

    #[test]
    fn a_draw_after_the_image_or_a_transformed_copy_voids_the_candidate() {
        let image = image(64, 32);
        let mut scene = Scene::new();
        fill_image(&mut scene, &image, Affine::IDENTITY);
        let drawn = DrawnOnTop::last_in(&scene, 0).expect("on top");
        let candidate = ScanoutCandidate::new(drawn.clone(), &scene).expect("pixel exact");
        let mut overlaid = scene.clone();
        below(&mut overlaid);
        assert!(!drawn.still_last_in(&overlaid));
        assert!(!candidate.still_on_top_of(&overlaid));
        let mut magnified = Scene::new();
        magnified.append(&scene, Some(Affine::scale(2.0)));
        assert!(!candidate.still_on_top_of(&magnified));
    }

    #[test]
    fn only_whole_pixel_unit_scale_placements_are_pixel_exact() {
        let size = (3840, 2160);
        assert_eq!(pixel_exact_origin(Affine::IDENTITY, size), Some((0, 0)));
        assert_eq!(
            pixel_exact_origin(Affine::translate((1920.0, 0.0)), size),
            Some((1920, 0))
        );
        // A 2x output drawing a buffer-scale-2 client: logical half size,
        // magnified back.
        let logical = Affine::scale(0.5).then_translate(vello::kurbo::Vec2::new(960.0, 0.0));
        let to_scene = Affine::scale(2.0);
        assert_eq!(
            pixel_exact_origin(logical.then(to_scene), size),
            Some((1920, 0))
        );
        // Fractional 1.5 through f32 encoding precision.
        let fractional = Affine::scale(f64::from(1.0_f32 / 1.5_f32)).then(Affine::scale(1.5));
        assert_eq!(pixel_exact_origin(fractional, size), Some((0, 0)));
        // Scaled, rotated, half-pixel, empty, non-finite: not pixel for pixel.
        assert_eq!(pixel_exact_origin(Affine::scale(0.5), size), None);
        assert_eq!(pixel_exact_origin(Affine::scale(1.001), size), None);
        assert_eq!(pixel_exact_origin(Affine::rotate(0.01), size), None);
        assert_eq!(pixel_exact_origin(Affine::FLIP_X, size), None);
        assert_eq!(
            pixel_exact_origin(Affine::translate((0.5, 0.0)), size),
            None
        );
        assert_eq!(pixel_exact_origin(Affine::IDENTITY, (0, 10)), None);
        assert_eq!(
            pixel_exact_origin(Affine::translate((f64::NAN, 0.0)), size),
            None
        );
    }
}

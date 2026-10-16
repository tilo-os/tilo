//! Conservative damage for replacement foreign pixels in otherwise identical scenes.
use std::cell::RefCell;
use std::collections::HashSet;

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::ImageData;
use vello_encoding::Patch;

use super::foreign::SurfaceId;

#[derive(Clone)]
pub(crate) struct DrawnImage {
    surface: SurfaceId,
    image: ImageData,
    bounds: Rect,
    cached_seq: u64,
    brush_transform: Affine,
}

thread_local! {
    static RECORDING: RefCell<Option<Vec<DrawnImage>>> = const { RefCell::new(None) };
}

pub(crate) struct Recording {
    previous: Option<Vec<DrawnImage>>,
    finished: bool,
}

impl Recording {
    pub(crate) fn begin() -> Self {
        Self {
            previous: RECORDING.with(|slot| slot.replace(Some(Vec::new()))),
            finished: false,
        }
    }

    pub(crate) fn finish(mut self) -> Vec<DrawnImage> {
        self.finished = true;
        RECORDING
            .with(|slot| slot.replace(self.previous.take()))
            .unwrap_or_default()
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        if !self.finished {
            RECORDING.with(|slot| slot.replace(self.previous.take()));
        }
    }
}

pub(crate) fn record(
    surface: SurfaceId,
    image: &ImageData,
    cached_seq: u64,
    brush_transform: Affine,
    bounds: Rect,
) {
    RECORDING.with(|slot| {
        if let Some(images) = slot.borrow_mut().as_mut() {
            images.push(DrawnImage {
                surface,
                image: image.clone(),
                bounds,
                cached_seq,
                brush_transform,
            });
        }
    });
}

/// A recorded image must occur exactly once across the whole composition.
/// Otherwise a preview or another unrecorded use might also need repair.
pub(crate) fn unique_uses<const N: usize>(scenes: [&Scene; N], images: &[DrawnImage]) -> bool {
    let mut ids = HashSet::new();
    images.iter().all(|draw| {
        ids.insert(draw.image.data.id())
            && scenes
                .iter()
                .flat_map(|scene| &scene.encoding().resources.patches)
                .filter(|patch| {
                    matches!(patch, Patch::Image { image, .. }
                    if image.data.id() == draw.image.data.id())
                })
                .count()
                == 1
    })
}

/// Normalize only proven foreign image replacements in a comparison copy.
/// The scene submitted to Vello retains its new identities and new pixels.
#[cfg(test)]
pub(crate) fn image_only_damage(
    previous: &Scene,
    current: &Scene,
    old_images: &[DrawnImage],
    new_images: &[DrawnImage],
) -> Option<Vec<Rect>> {
    if previous.encoding_eq(current) {
        return Some(Vec::new());
    }
    let (normalized, damage) = normalize_images(current, old_images, new_images)?;
    previous.encoding_eq(&normalized).then_some(damage)
}

/// A comparison copy of `current` with each proven foreign image replacement
/// put back to the image it replaced, and the damage of those replacements.
/// The copy equals the previous frame's scene exactly when the replacements
/// are all that changed; `None` when the images cannot be paired up.
pub(crate) fn normalize_images(
    current: &Scene,
    old_images: &[DrawnImage],
    new_images: &[DrawnImage],
) -> Option<(Scene, Vec<Rect>)> {
    if old_images.len() != new_images.len() {
        return None;
    }
    let pairs: Vec<_> = old_images.iter().zip(new_images).collect();
    if pairs.iter().any(|(old, new)| {
        old.surface != new.surface
            || old.bounds != new.bounds
            || !old.bounds.is_finite()
            || old.bounds.width() <= 0.0
            || old.bounds.height() <= 0.0
            || old.brush_transform != new.brush_transform
            || !old
                .brush_transform
                .as_coeffs()
                .iter()
                .all(|value| value.is_finite())
            || old.brush_transform.determinant() == 0.0
            || !old
                .brush_transform
                .transform_rect_bbox(Rect::new(
                    0.0,
                    0.0,
                    f64::from(old.image.width),
                    f64::from(old.image.height),
                ))
                .is_finite()
            || old.image.width == 0
            || old.image.height == 0
            || old.image.width != new.image.width
            || old.image.height != new.image.height
            || old.image.format != new.image.format
            || old.image.alpha_type != new.image.alpha_type
    }) {
        return None;
    }
    let mut normalized = current.clone();
    let mut damage = Vec::new();
    for patch in &mut normalized.encoding_mut().resources.patches {
        let Patch::Image { image, .. } = patch else {
            continue;
        };
        let Some((old, new)) = pairs
            .iter()
            .find(|(_, new)| new.image.data.id() == image.data.id())
        else {
            continue;
        };
        if old.image.data.id() != new.image.data.id() {
            // Certify only damage between the exact two buffers that were
            // actually painted. Capture history unions coalesced commits and
            // every skipped paint; unknown/evicted metadata retains full bounds.
            // Foreign uses the default linear image sampler. One buffer texel
            // around each dirty rect covers its footprint under scale/rotation.
            if let Some(rects) = super::foreign::store().frame_damage_between(
                new.surface,
                old.cached_seq,
                new.cached_seq,
                (new.image.width, new.image.height),
            ) {
                damage.extend(
                    rects
                        .into_iter()
                        .map(|rect| {
                            new.brush_transform
                                .transform_rect_bbox(rect.inflate(1.0, 1.0).intersect(Rect::new(
                                    0.0,
                                    0.0,
                                    f64::from(new.image.width),
                                    f64::from(new.image.height),
                                )))
                                .intersect(new.bounds)
                        })
                        .filter(|rect| !rect.is_zero_area()),
                );
            } else {
                damage.push(new.bounds);
            }
            image.data = old.image.data.clone();
        }
    }
    Some((normalized, damage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Affine;
    use vello::peniko::{Fill, ImageAlphaType, ImageBrush, ImageFormat};

    fn scene(texel: [u8; 4]) -> (Scene, Vec<DrawnImage>) {
        let image = ui::raster::rgba8_premultiplied(texel.repeat(4), 2, 2);
        let bounds = Rect::new(0.0, 0.0, 2.0, 2.0);
        let recording = Recording::begin();
        record(37, &image, 0, Affine::IDENTITY, bounds);
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            &ImageBrush::new(image),
            None,
            &bounds,
        );
        (scene, recording.finish())
    }

    #[test]
    fn replacement_proof_rejects_changed_surface_geometry_and_image_metadata() {
        let (old, old_images) = scene([255, 0, 0, 255]);
        let (new, new_images) = scene([0, 255, 0, 255]);
        assert_eq!(
            image_only_damage(&old, &new, &old_images, &new_images),
            Some(vec![Rect::new(0.0, 0.0, 2.0, 2.0)])
        );
        let changes: [fn(&mut DrawnImage); 9] = [
            |draw| draw.surface = 38,
            |draw| draw.brush_transform = Affine::scale(2.0),
            |draw| draw.brush_transform = Affine::scale(0.0),
            |draw| draw.bounds = Rect::new(1.0, 0.0, 3.0, 2.0),
            |draw| draw.bounds = Rect::new(f64::NAN, 0.0, 2.0, 2.0),
            |draw| draw.image.width = 3,
            |draw| draw.image.height = 3,
            |draw| draw.image.format = ImageFormat::Bgra8,
            |draw| draw.image.alpha_type = ImageAlphaType::Alpha,
        ];
        for change in changes {
            let mut different = new_images.clone();
            change(&mut different[0]);
            assert!(image_only_damage(&old, &new, &old_images, &different).is_none());
        }
        assert!(image_only_damage(&old, &new, &old_images, &[]).is_none());
        let mut transformed = new;
        transformed.fill(
            Fill::NonZero,
            Affine::translate((3.0, 0.0)),
            &ImageBrush::new(new_images[0].image.clone()),
            None,
            &Rect::new(0.0, 0.0, 2.0, 2.0),
        );
        assert!(
            image_only_damage(&old, &transformed, &old_images, &new_images).is_none(),
            "extra commands cannot be normalized away"
        );
    }

    #[test]
    fn whole_composition_requires_exactly_one_use_of_each_recorded_image() {
        let (root, images) = scene([255, 0, 0, 255]);
        let empty = Scene::new();
        assert!(unique_uses(
            [&root, &empty, &empty, &empty, &empty],
            &images
        ));
        assert!(
            !unique_uses([&root, &root, &empty, &empty, &empty], &images),
            "a preview outside the root also needs repair"
        );
        assert!(!unique_uses(
            [&empty, &empty, &empty, &empty, &empty],
            &images
        ));
        assert!(!unique_uses(
            [&root, &empty, &empty, &empty, &empty],
            &[images[0].clone(), images[0].clone()]
        ));
    }
}

// Damaged frames are published through the Linux capture seam
// (`publish_damaged_rgba_for_test`).
#[cfg(test)]
#[cfg(target_os = "linux")]
mod capture_tests {
    use super::*;
    use vello::peniko::{Fill, ImageBrush};

    fn captured_scene(
        surface: SurfaceId,
        rgba: &[u8],
        seq: u64,
        brush: Affine,
    ) -> (Scene, Vec<DrawnImage>) {
        captured_scene_with_extent(surface, rgba, seq, brush, (8, 8))
    }

    fn captured_scene_with_extent(
        surface: SurfaceId,
        rgba: &[u8],
        seq: u64,
        brush: Affine,
        size: (u32, u32),
    ) -> (Scene, Vec<DrawnImage>) {
        let image = ui::raster::rgba8_premultiplied(rgba.to_vec(), size.0, size.1);
        let extent = Rect::new(0.0, 0.0, f64::from(size.0), f64::from(size.1));
        let recording = Recording::begin();
        record(
            surface,
            &image,
            seq,
            brush,
            brush.transform_rect_bbox(extent),
        );
        let mut scene = Scene::new();
        scene.fill(Fill::NonZero, brush, &ImageBrush::new(image), None, &extent);
        (scene, recording.finish())
    }

    #[test]
    fn cropped_image_without_buffer_coordinate_proof_keeps_full_bounds() {
        let surface = 8_493_302;
        let frames = super::super::foreign::store();
        let old_buffer = [255, 0, 0, 255].repeat(64);
        let mut new_buffer = old_buffer.clone();
        new_buffer
            .get_mut(72..76)
            .unwrap()
            .copy_from_slice(&[0, 255, 0, 255]);
        frames.publish_rgba(surface, &old_buffer, 8, 8);
        let old_crop = [255, 0, 0, 255].repeat(32);
        let mut new_crop = old_crop.clone();
        new_crop
            .get_mut(40..44)
            .unwrap()
            .copy_from_slice(&[0, 255, 0, 255]);
        let brush = Affine::translate((10.0, 20.0));
        let (old, old_images) = captured_scene_with_extent(
            surface,
            &old_crop,
            frames.frame_seq(surface),
            brush,
            (4, 8),
        );
        frames.publish_damaged_rgba_for_test(
            surface,
            &new_buffer,
            (8, 8),
            vec![Rect::new(2.0, 2.0, 3.0, 3.0)],
        );
        let (new, new_images) = captured_scene_with_extent(
            surface,
            &new_crop,
            frames.frame_seq(surface),
            brush,
            (4, 8),
        );
        assert_eq!(
            image_only_damage(&old, &new, &old_images, &new_images),
            Some(vec![new_images[0].bounds]),
            "full-buffer damage cannot certify a cropped image without its coordinate mapping"
        );
        frames.forget(surface);
    }

    #[test]
    fn exact_capture_sequences_map_sampling_footprint_and_unknown_history_keeps_full_bounds() {
        let surface = 8_493_301;
        let frames = super::super::foreign::store();
        let old_pixels = [255, 0, 0, 255].repeat(64);
        let mut new_pixels = old_pixels.clone();
        new_pixels
            .get_mut(72..76)
            .expect("the certified pixel at (2,2) is inside the 8x8 buffer")
            .copy_from_slice(&[0, 255, 0, 255]);
        frames.publish_rgba(surface, &old_pixels, 8, 8);
        let first = frames.frame_seq(surface);
        let brush = Affine::translate((10.0, 20.0)) * Affine::scale(1.25);
        let (old, old_images) = captured_scene(surface, &old_pixels, first, brush);
        frames.publish_damaged_rgba_for_test(
            surface,
            &new_pixels,
            (8, 8),
            vec![Rect::new(2.0, 2.0, 3.0, 3.0)],
        );
        let second = frames.frame_seq(surface);
        let (new, new_images) = captured_scene(surface, &new_pixels, second, brush);
        assert_eq!(
            image_only_damage(&old, &new, &old_images, &new_images),
            Some(vec![Rect::new(11.25, 21.25, 15.0, 25.0)])
        );
        let mut missing = old_images.clone();
        missing[0].cached_seq = 0;
        assert_eq!(
            image_only_damage(&old, &new, &missing, &new_images),
            Some(vec![new_images[0].bounds])
        );
        let mut reused = new_images.clone();
        reused[0].cached_seq = first;
        assert_eq!(
            image_only_damage(&old, &new, &old_images, &reused),
            Some(vec![new_images[0].bounds])
        );
        frames.publish_rgba(surface, &new_pixels, 8, 8);
        let (unknown, unknown_images) =
            captured_scene(surface, &new_pixels, frames.frame_seq(surface), brush);
        assert_eq!(
            image_only_damage(&old, &unknown, &old_images, &unknown_images),
            Some(vec![unknown_images[0].bounds])
        );
        frames.forget(surface);
    }
}

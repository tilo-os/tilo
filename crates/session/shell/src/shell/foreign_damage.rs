//! Conservative damage for replacement foreign pixels in otherwise identical scenes.
use std::cell::RefCell;
use std::collections::HashSet;

use vello::Scene;
use vello::kurbo::Rect;
use vello::peniko::ImageData;
use vello_encoding::Patch;

use super::foreign::SurfaceId;

#[derive(Clone)]
pub(crate) struct DrawnImage {
    surface: SurfaceId,
    image: ImageData,
    bounds: Rect,
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

pub(crate) fn record(surface: SurfaceId, image: &ImageData, bounds: Rect) {
    RECORDING.with(|slot| {
        if let Some(images) = slot.borrow_mut().as_mut() {
            images.push(DrawnImage {
                surface,
                image: image.clone(),
                bounds,
            });
        }
    });
}

/// A recorded image must occur exactly once across the whole composition.
/// Otherwise a preview or another unrecorded use might also need repair.
pub(crate) fn unique_uses(scenes: [&Scene; 5], images: &[DrawnImage]) -> bool {
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
pub(crate) fn image_only_damage(
    previous: &Scene,
    current: &Scene,
    old_images: &[DrawnImage],
    new_images: &[DrawnImage],
) -> Option<Vec<Rect>> {
    if previous.encoding_eq(current) {
        return Some(Vec::new());
    }
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
            damage.push(new.bounds);
            image.data = old.image.data.clone();
        }
    }
    previous.encoding_eq(&normalized).then_some(damage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vello::kurbo::Affine;
    use vello::peniko::{Blob, Fill, ImageAlphaType, ImageBrush, ImageFormat};

    fn scene(texel: [u8; 4]) -> (Scene, Vec<DrawnImage>) {
        let image = ImageData {
            data: Blob::new(Arc::new(texel.repeat(4))),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::AlphaPremultiplied,
            width: 2,
            height: 2,
        };
        let bounds = Rect::new(0.0, 0.0, 2.0, 2.0);
        let recording = Recording::begin();
        record(37, &image, bounds);
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
        let changes: [fn(&mut DrawnImage); 7] = [
            |draw| draw.surface = 38,
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

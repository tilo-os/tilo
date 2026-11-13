//! Parts of a frame the host composites itself. A part that only fades, grows or moves while it shows
//! the same thing (the overview's ground coming up, its panel rising into place) costs a whole raster
//! of everything it covers on every frame when its pose is drawn into the frame. Everywhere else
//! ([`lifted`] outside a recording) the part is drawn into the frame in its pose, as it always was: the
//! widget draws it one way.

use std::cell::RefCell;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::{BlendMode, Fill};

use crate::scene_parts::{PartKey, SectionParts};

/// How a lifted part is drawn this frame instead of at rest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    /// Its opacity, 0 (invisible) to 1.
    pub opacity: f32,
    /// Scaled by this much about [`Self::pivot`] (1: as it is).
    pub scale: f64,
    /// The point it grows from, in the coordinates it is drawn in.
    pub pivot: Point,
    /// How far it is drawn from where it rests.
    pub offset: Vec2,
}

impl Pose {
    /// At rest: drawn as it stands.
    pub const REST: Self = Self {
        opacity: 1.0,
        scale: 1.0,
        pivot: Point::ORIGIN,
        offset: Vec2::ZERO,
    };

    /// At rest, at `opacity`.
    #[must_use]
    pub const fn faded(opacity: f32) -> Self {
        Self {
            opacity,
            ..Self::REST
        }
    }

    /// What the pose does to what is drawn at rest.
    #[must_use]
    pub fn transform(&self) -> Affine {
        Affine::translate(self.pivot.to_vec2() + self.offset)
            * Affine::scale(self.scale)
            * Affine::translate(-self.pivot.to_vec2())
    }

    fn mapped(self, transform: Affine) -> Self {
        let pivot = transform * self.pivot;
        Self {
            pivot,
            offset: transform * (self.pivot + self.offset) - pivot,
            ..self
        }
    }
}

/// One part a host composites: its scene drawn at rest in the frame's coordinates, the rectangle it may
/// paint and the pose to draw it in.
pub struct Lifted {
    /// Names the part across frames.
    pub key: PartKey,
    /// What it may paint, in the frame's coordinates: nothing outside shows.
    pub bounds: Rect,
    /// Its pose, in the frame's coordinates.
    pub pose: Pose,
    /// What it draws, at rest.
    pub scene: Box<Scene>,
    /// Its damage parts, when the frame's were recorded ([`crate::scene_parts`]).
    pub parts: Option<SectionParts>,
}

struct Active {
    root: usize,
    lifted: Vec<Lifted>,
}

thread_local! {
    static RECORDING: RefCell<Option<Active>> = const { RefCell::new(None) };
}

/// Lifts parts out of the frame drawn into `root` until finished. The scene must stay at its address
/// until then.
pub struct Recording {
    previous: Option<Active>,
    finished: bool,
}

impl Recording {
    /// Lift parts out of the frame drawn into `root`.
    #[must_use]
    pub fn begin(root: &Scene) -> Self {
        let active = Active {
            root: std::ptr::from_ref(root) as usize,
            lifted: Vec::new(),
        };
        Self {
            previous: RECORDING.with(|slot| slot.replace(Some(active))),
            finished: false,
        }
    }

    /// The parts lifted out of the frame, bottom first.
    #[must_use]
    pub fn finish(mut self) -> Vec<Lifted> {
        self.finished = true;
        RECORDING
            .with(|slot| slot.replace(self.previous.take()))
            .map(|active| active.lifted)
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

/// Draw a part that may move as a whole: `draw` paints it at rest with the transform it is handed,
/// inside `bounds` (in the coordinates `transform` maps from), and `pose` says how it is drawn this
/// frame.
pub fn lifted(
    scene: &mut Scene,
    transform: Affine,
    (key, bounds): (PartKey, Rect),
    pose: Pose,
    draw: impl FnOnce(&mut Scene, Affine),
) {
    let address = std::ptr::from_ref(scene) as usize;
    let lifts = RECORDING.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|active| active.root == address)
    }) && scene.encoding().draw_tags.is_empty();
    if lifts {
        let mut layer = Box::new(Scene::new());
        if crate::scene_parts::is_tracked(scene) {
            crate::scene_parts::track(&layer);
        }
        draw(&mut layer, transform);
        let lifted = Lifted {
            key,
            bounds: transform.transform_rect_bbox(bounds),
            pose: pose.mapped(transform),
            scene: layer,
            parts: None,
        };
        RECORDING.with(|slot| {
            if let Some(active) = slot.borrow_mut().as_mut() {
                active.lifted.push(lifted);
            }
        });
        return;
    }
    if pose.opacity <= 0.0 {
        return;
    }
    let placed = transform * pose.transform();
    if pose.opacity >= 1.0 {
        draw(scene, placed);
        return;
    }
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        pose.opacity,
        transform,
        &bounds,
    );
    draw(scene, placed);
    scene.pop_layer();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test fixtures")]

    use super::*;
    use vello::peniko::Color;

    const KEY: &str = "lift-test";

    fn square(scene: &mut Scene, transform: Affine) {
        scene.fill(
            Fill::NonZero,
            transform,
            Color::WHITE,
            None,
            &Rect::new(0.0, 0.0, 10.0, 10.0),
        );
    }

    #[test]
    fn a_part_drawn_first_is_lifted_at_rest_with_its_pose() {
        let mut scene = Scene::new();
        let recording = Recording::begin(&scene);
        let pose = Pose {
            opacity: 0.5,
            scale: 0.9,
            pivot: Point::new(5.0, 5.0),
            offset: Vec2::new(0.0, 4.0),
        };
        lifted(
            &mut scene,
            Affine::translate((100.0, 0.0)),
            (PartKey::new(KEY, 0), Rect::new(0.0, 0.0, 10.0, 10.0)),
            pose,
            square,
        );
        let lifted = recording.finish();
        assert!(
            scene.encoding().draw_tags.is_empty(),
            "the frame keeps none of it"
        );
        assert_eq!(lifted.len(), 1);
        let part = &lifted[0];
        assert_eq!(part.bounds, Rect::new(100.0, 0.0, 110.0, 10.0));
        assert_eq!(
            part.pose.pivot,
            Point::new(105.0, 5.0),
            "in the frame's coordinates"
        );
        assert_eq!(part.pose.offset, Vec2::new(0.0, 4.0));
        assert!(!part.scene.encoding().draw_tags.is_empty(), "drawn at rest");
    }

    #[test]
    fn a_part_over_something_drawn_stays_in_the_frame() {
        let mut scene = Scene::new();
        let recording = Recording::begin(&scene);
        square(&mut scene, Affine::IDENTITY);
        let before = scene.encoding().draw_tags.len();
        lifted(
            &mut scene,
            Affine::IDENTITY,
            (PartKey::new(KEY, 0), Rect::new(0.0, 0.0, 10.0, 10.0)),
            Pose::faded(0.5),
            square,
        );
        assert!(recording.finish().is_empty());
        assert!(
            scene.encoding().draw_tags.len() > before,
            "drawn in place, faded"
        );
    }

    #[test]
    fn without_a_recording_a_part_is_drawn_in_its_pose() {
        let mut scene = Scene::new();
        lifted(
            &mut scene,
            Affine::IDENTITY,
            (PartKey::new(KEY, 0), Rect::new(0.0, 0.0, 10.0, 10.0)),
            Pose::faded(0.0),
            square,
        );
        assert!(
            scene.encoding().draw_tags.is_empty(),
            "an invisible part draws nothing"
        );
        lifted(
            &mut scene,
            Affine::IDENTITY,
            (PartKey::new(KEY, 0), Rect::new(0.0, 0.0, 10.0, 10.0)),
            Pose::REST,
            square,
        );
        assert!(!scene.encoding().draw_tags.is_empty());
    }
}

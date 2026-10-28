use super::*;

use crate::scene_parts::{PartKey, Recording, part, track};
use vello::kurbo::Affine;
use vello::peniko::Fill;

/// A 400x200 frame (drawn at twice its size on the target) with a part in
/// each corner.
fn corners() -> (Box<Scene>, crate::scene_parts::SectionParts) {
    let recording = Recording::begin();
    let mut scene = Box::new(Scene::new());
    track(&scene);
    let corners = [
        Rect::new(0.0, 0.0, 20.0, 20.0),
        Rect::new(380.0, 0.0, 400.0, 20.0),
        Rect::new(0.0, 180.0, 20.0, 200.0),
        Rect::new(380.0, 180.0, 400.0, 200.0),
    ];
    for (index, corner) in corners.into_iter().enumerate() {
        part(
            &mut scene,
            PartKey::new("corner", index),
            vec![corner],
            |scene| {
                scene.fill(Fill::NonZero, Affine::IDENTITY, Color::WHITE, None, &corner);
            },
        );
    }
    let parts = recording.finish().of(&scene).expect("the corners' parts");
    (scene, parts)
}

fn key() -> TargetKey {
    TargetKey {
        width: 800,
        height: 400,
        to_device: Affine::scale(2.0),
        base_color: Color::BLACK,
        generation: 0,
    }
}

#[test]
fn a_small_damage_takes_only_the_parts_that_reach_it() {
    let (scene, parts) = corners();
    let caret = [Rect::new(10.0, 10.0, 30.0, 30.0)];
    let culled =
        culled_for_damage(&scene, Some(&parts), key(), &caret).expect("three corners are left out");
    assert_eq!(culled.encoding().n_paths, 1);
}

#[test]
fn a_damage_repainted_whole_takes_every_part() {
    let (scene, parts) = corners();
    // Two corners far apart: the window around them is the whole target,
    // which is then repainted as one frame and must draw everything.
    let apart = [
        Rect::new(10.0, 10.0, 30.0, 30.0),
        Rect::new(770.0, 370.0, 790.0, 390.0),
    ];
    assert!(culled_for_damage(&scene, Some(&parts), key(), &apart).is_none());
    assert!(culled_for_damage(&scene, Some(&parts), key(), &[]).is_none());
    assert!(culled_for_damage(&scene, None, key(), &apart[..1]).is_none());
}

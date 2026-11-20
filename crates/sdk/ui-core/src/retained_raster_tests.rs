use super::*;

use crate::scene_parts::{PartKey, Recording, part, track};
use vello::kurbo::Affine;
use vello::peniko::Fill;

/// A 400x200 frame (drawn at twice its size on the target) with a part in each corner.
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

tilo_test_env::scenario! {
    fn a_damage_culls_the_parts_it_does_not_reach;

    phase a_small_damage_takes_only_the_parts_that_reach_it;
    {
        let (scene, parts) = corners();
        let caret = [Rect::new(10.0, 10.0, 30.0, 30.0)];
        let culled =
            culled_for_damage(&scene, Some(&parts), key(), &caret).expect("three corners are left out");
        assert_eq!(culled.encoding().n_paths, 1);
    }

    phase damage_far_apart_takes_only_the_parts_each_window_reaches;
    {
        let (scene, parts) = corners();
        // Two corners far apart are two windows of their own, not one around the whole target:
        // only the two corners they reach are drawn.
        let apart = [
            Rect::new(10.0, 10.0, 30.0, 30.0),
            Rect::new(770.0, 370.0, 790.0, 390.0),
        ];
        let culled = culled_for_damage(&scene, Some(&parts), key(), &apart)
            .expect("the corners between them are left out");
        assert_eq!(culled.encoding().n_paths, 2);
    }

    phase a_damage_repainted_whole_takes_every_part;
    {
        let (scene, parts) = corners();
        // Half the target changed: it is repainted as one frame and must draw everything.
        let half = [Rect::new(0.0, 0.0, 800.0, 200.0)];
        assert!(culled_for_damage(&scene, Some(&parts), key(), &half).is_none());
        assert!(culled_for_damage(&scene, Some(&parts), key(), &[]).is_none());
        assert!(culled_for_damage(&scene, None, key(), &half[..1]).is_none());
    }
}

fn region(x: u32, y: u32, width: u32, height: u32) -> FineDamageRect {
    FineDamageRect {
        x,
        y,
        width,
        height,
    }
}

tilo_test_env::scenario! {
    fn damage_is_repainted_through_windows_that_cost_about_its_own_pixels;

    phase a_composer_and_a_list_line_typed_into_at_once_are_two_windows;
    {
        // What a keystroke in Chat changes on a 1280x800 page: the composer at the bottom and the
        // conversation's line in the list at the top left. One window around both covers more than
        // half the page; two cover a seventh of it.
        let composer = region(295, 530, 698, 99);
        let line = region(31, 79, 298, 71);
        let windows = damage_windows(vec![composer, line]);
        assert_eq!(windows.len(), 2, "{windows:?}");
        assert!(window_pixels(&windows) < 1280 * 800 / 6, "{windows:?}");
        assert!(repaints_alone(
            1280,
            800,
            &[
                Rect::new(295.0, 530.0, 993.0, 629.0),
                Rect::new(31.0, 79.0, 329.0, 150.0),
            ]
        ));
    }

    phase regions_close_together_are_one_window;
    {
        let windows = damage_windows(vec![region(100, 100, 50, 20), region(100, 130, 50, 20)]);
        assert_eq!(windows.len(), 1, "{windows:?}");
        assert_eq!(windows[0].regions.len(), 2);
        assert_eq!(windows[0].window, region(96, 96, 54, 54));
    }

    phase every_window_starts_at_a_tile_corner_and_holds_its_regions;
    {
        let regions = vec![region(17, 33, 10, 10), region(700, 500, 40, 9)];
        for window in damage_windows(regions) {
            assert_eq!(window.window.x % RASTER_TILE, 0);
            assert_eq!(window.window.y % RASTER_TILE, 0);
            for held in &window.regions {
                assert!(held.x >= window.window.x && held.y >= window.window.y);
                assert!(held.x + held.width <= window.window.x + window.window.width);
                assert!(held.y + held.height <= window.window.y + window.window.height);
            }
        }
    }
}

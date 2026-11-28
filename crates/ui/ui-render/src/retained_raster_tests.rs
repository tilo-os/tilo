use super::*;

use tilo_ui_base::{
    scene_parts::PartKey, scene_parts::Recording, scene_parts::part, scene_parts::track,
};
use vello::kurbo::Affine;
use vello::peniko::Fill;

/// A 400x200 frame (drawn at twice its size on the target) with a part in each corner.
fn corners() -> (Box<Scene>, tilo_ui_base::scene_parts::SectionParts) {
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
        let windows = damage_windows(&[composer, line]);
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
        let windows = damage_windows(&[region(100, 100, 50, 20), region(100, 130, 50, 20)]);
        assert_eq!(windows, vec![region(96, 96, 54, 54)]);
    }

    phase every_window_starts_at_a_tile_corner_and_holds_its_regions;
    {
        let regions = [region(17, 33, 10, 10), region(700, 500, 40, 9)];
        let windows = damage_windows(&regions);
        for window in &windows {
            assert_eq!(window.x % RASTER_TILE, 0);
            assert_eq!(window.y % RASTER_TILE, 0);
        }
        for held in regions {
            assert!(windows.iter().any(|window| contains(*window, held)), "{windows:?}");
        }
    }
}

tilo_test_env::scenario! {
    fn a_frame_rasterized_on_the_cpu_keeps_its_pixels_for_a_screen_with_no_gpu;

    phase the_frame_holds_its_colours_and_says_which_pixels_it_drew;
    // Catches a page whose GPU keeps failing having nothing to put on its
    // canvas: the frame is on the CPU, ready for it.
    {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 0, 0, 255),
            None,
            &Rect::new(4.0, 4.0, 8.0, 8.0),
        );
        let mut raster = RetainedRaster::new(true);
        raster
            .rasterize_to_cpu(FrameContent {
                scene: &scene,
                width: 16,
                height: 16,
                base_color: Color::WHITE,
                damage_regions: &[],
                scroll_copies: &[],
                full_redraw: true,
                backdrop: &tilo_ui_base::part_paint::backdrop::BackdropFrame::default(),
            })
            .expect("the CPU draws the frame");
        let cpu = raster.cpu_frame().expect("the frame is on the CPU");
        assert_eq!((cpu.width, cpu.height), (16, 16));
        assert_eq!(cpu.dirty.len(), 1, "the whole frame is new");
        assert_eq!(cpu.dirty[0], tilo_cpu_raster::Region::new(0, 0, 16, 16));
        let pixel = |x: usize, y: usize| {
            let at = (y * 16 + x) * 4;
            [
                cpu.pixels[at],
                cpu.pixels[at + 1],
                cpu.pixels[at + 2],
                cpu.pixels[at + 3],
            ]
        };
        assert_eq!(pixel(1, 1), [255, 255, 255, 255], "the base colour");
        assert_eq!(pixel(6, 6), [255, 0, 0, 255], "the rectangle drawn on it");
    }

    phase a_rasterizer_for_the_gpu_has_no_pixels_of_its_own;
    {
        let mut raster = RetainedRaster::new(false);
        assert!(raster.cpu_frame().is_none());
        let scene = Scene::new();
        assert!(
            raster
                .rasterize_to_cpu(FrameContent {
                    scene: &scene,
                    width: 16,
                    height: 16,
                    base_color: Color::WHITE,
                    damage_regions: &[],
                    scroll_copies: &[],
                    full_redraw: true,
                    backdrop: &tilo_ui_base::part_paint::backdrop::BackdropFrame::default(),
                })
                .is_err()
        );
    }

    phase premultiplied_pixels_become_the_colours_a_canvas_shows;
    // Catches a translucent pixel shown the colour of its premultiplied
    // value, which reads darker than what was drawn.
    {
        let mut pixels = [
            255, 0, 0, 255, // opaque red
            128, 0, 0, 128, // half red over nothing
            0, 0, 0, 0, // nothing
            64, 64, 64, 128, // a quarter of a grey
        ];
        straighten(&mut pixels);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[255, 0, 0, 128]);
        assert_eq!(&pixels[8..12], &[0, 0, 0, 0]);
        assert_eq!(&pixels[12..16], &[128, 128, 128, 128]);
    }
}

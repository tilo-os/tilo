//! A wide foreign tile's buffer edits, CPU or GPU and at one or two buffer
//! pixels per logical pixel: bounded damage, and a dense render's pixels.
use super::*;

// One test per buffer kind and density: each makes its own bridges, and the
// four in one test ran one after another.
#[test]
fn wide_foreign_commit_damage_tracks_exact_buffers_and_matches_dense_gpu_pixels() {
    wide_foreign_commit_damage(false, 1);
}

#[test]
fn wide_foreign_commit_damage_tracks_exact_hidpi_buffers_and_matches_dense_gpu_pixels() {
    wide_foreign_commit_damage(false, 2);
}

#[test]
fn wide_foreign_commit_damage_tracks_exact_imported_buffers_and_matches_dense_gpu_pixels() {
    wide_foreign_commit_damage(true, 1);
}

#[test]
fn wide_foreign_commit_damage_tracks_exact_imported_hidpi_buffers_and_matches_dense_gpu_pixels() {
    wide_foreign_commit_damage(true, 2);
}

/// Two small edits to a wide foreign tile's buffer, CPU (`imported` false) or
/// GPU, at `density` buffer pixels per logical pixel, stay bounded damage and
/// paint what a dense render paints.
fn wide_foreign_commit_damage(imported: bool, density: u32) {
    {
        let _ambient = DenyAmbientStarts::new();
        let mut clock = tilo_test_env::ScopedEnv::new();
        clock.set("TILO_CLOCK", "09:41");
        let surface = 8_493_206 + u64::from(imported) * 2 + u64::from(density == 2);
        // A little short of native height, so the tile stays at most 2048 px
        // tall: the renderer's 8192 px image atlas keeps a window's previous
        // frame resident while its next one arrives, and two 2x frames of a
        // taller tile cannot be stacked in it, so the new one would be
        // omitted (an atlas budget, not the damage tracking under test).
        let (mut fixture, logical_size, opened) = foreign_damage_fixture_on(
            ui::widget::WindowWidth::Wide,
            surface,
            (NATIVE_WIDTH, NATIVE_HEIGHT - 32),
        );
        // Native HiDPI clients render two buffer pixels per logical pixel.
        // The oracle must prove real filtered GPU output at both densities.
        let size = (logical_size.0 * density, logical_size.1 * density);
        let mut retained = bridge();
        let mut oracle = bridge();
        publish_foreign_fixture_frame(
            surface,
            size,
            [255, 0, 0, 255],
            imported.then_some(&retained),
        );
        // Pinned: the client answers the instant it is asked, so set-up is no slow client.
        let first = ui::time::testing::with_monotonic_time(opened, || fixture.paint_with_damage());
        paint_retained_and_compare(&mut retained, &mut oracle, &first, "wide original buffer");
        let logical_rect = fixture.foreign_surface_rect(surface).unwrap();
        let placement = fixture.layout_rect_to_scene(logical_rect).unwrap();
        assert!(
            placement.area() * 2.0 >= f64::from(NATIVE_WIDTH) * f64::from(NATIVE_HEIGHT),
            "this fixture must exercise a genuinely wide tile, not the narrow performance case"
        );
        let mut pixels = [255, 0, 0, 255].repeat(size.0.saturating_mul(size.1) as usize);
        let first_dirty = Rect::new(128.0, 128.0, 160.0, 160.0);
        let second_dirty = Rect::new(256.0, 256.0, 288.0, 288.0);
        // Two captured buffers arrive before the compositor paints. Both
        // edits must survive the exact old->new cached sequence join.
        for dirty in [first_dirty, second_dirty] {
            for y in dirty.y0 as usize..dirty.y1 as usize {
                for x in dirty.x0 as usize..dirty.x1 as usize {
                    let index = (y * size.0 as usize + x) * 4;
                    pixels[index..index + 4].copy_from_slice(&[0, 255, 0, 255]);
                }
            }
            let store = crate::shell::foreign::store();
            if imported {
                let (device, queue) = retained.gpu();
                let frame = crate::shell::gpu_frames::frame_pixels_for_test(
                    device,
                    queue,
                    retained.present_generation_for_test().unwrap(),
                    size,
                    &pixels,
                    surface,
                );
                assert!(
                    store
                        .publish_damaged_gpu_for_test(surface, frame, Some(vec![dirty]))
                        .is_some()
                );
            } else {
                store.publish_damaged_rgba_for_test(surface, &pixels, size, vec![dirty]);
            }
        }
        let changed = fixture.paint_with_damage();
        let damage = changed
            .damage_regions
            .as_ref()
            .expect("exact buffer damage must stay bounded");
        let dirty_pixels: u64 = damage
            .iter()
            .map(|r| u64::from(r.width) * u64::from(r.height))
            .sum();
        assert!(
            crate::shell::render::paint::damage_is_profitable(
                dirty_pixels,
                u64::from(NATIVE_WIDTH) * u64::from(NATIVE_HEIGHT),
            ),
            "a wide tile's two small edits must exercise sparse admission"
        );
        // A fresh dense oracle: its image atlas keeps the previous frame
        // resident for two passes, and at density 2 this 3456x2056 tile's
        // 6912x4112 buffer cannot share the 8192 px atlas with its own
        // predecessor, so a reused oracle would omit the new frame and
        // compare against a blank tile rather than against the truth.
        oracle = bridge();
        paint_retained_and_compare(
            &mut retained,
            &mut oracle,
            &changed,
            "wide coalesced dirty buffers",
        );
        assert!(retained.last_sparse_render_attempts() >= 1);
        for dirty in [first_dirty, second_dirty] {
            let p = dirty.center();
            let output = Point::new(
                placement.x0 + p.x * placement.width() / f64::from(size.0),
                placement.y0 + p.y * placement.height() / f64::from(size.1),
            );
            assert_foreign_pixel(&mut oracle, &changed, output, [0, 255, 0]);
        }
        assert_foreign_pixel(&mut oracle, &changed, placement.center(), [0, 0, 255]);
        crate::shell::foreign::store().forget(surface);
    }
}

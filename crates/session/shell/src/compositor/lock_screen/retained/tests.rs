use super::*;
use crate::compositor::lock_screen::LockView;
use crate::shell::osd::Osd;
use crate::shell::render::paint::{SceneDamage, VelloDrmBridge};
use ui::theme::Theme;

struct Fixture {
    retained: RetainedLockScene,
    builder: FrameBuilder,
    outputs: Vec<LockOutput>,
    osd: Option<Osd>,
    wallpaper: Option<vello::peniko::ImageData>,
}

impl Fixture {
    fn new(size: (u32, u32)) -> Self {
        Self {
            retained: RetainedLockScene::default(),
            builder: FrameBuilder::new(size),
            outputs: vec![LockOutput {
                origin: (0.0, 0.0),
                size: (f64::from(size.0), f64::from(size.1)),
                scale: 1.0,
            }],
            osd: None,
            wallpaper: None,
        }
    }

    fn draw(&mut self, theme: &Theme, view: LockView<'_>) -> FramePaint {
        self.retained.draw(
            &mut self.builder,
            &LockComposition {
                outputs: &self.outputs,
                theme,
                view: &view,
                wallpaper: self.wallpaper.as_ref(),
                osd: self.osd,
                now: ui::time::now(),
            },
        )
    }
}

fn view(length: usize) -> LockView<'static> {
    LockView {
        password_len: length,
        clock: "10:42",
        date: "Saturday, 24 October",
        ..LockView::default()
    }
}

fn damage_pixels(frame: &FramePaint) -> u64 {
    frame
        .damage_regions
        .as_ref()
        .expect("password-only edit must be bounded")
        .iter()
        .map(|r| u64::from(r.width).saturating_mul(u64::from(r.height)))
        .sum()
}

#[test]
fn password_changes_are_bounded_and_other_changes_break_the_proof() {
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let mut f = Fixture::new((3840, 2160));
        f.retained.clear();
        let first = f.draw(&theme, view(0));
        assert!(first.damage_regions.is_none());
        let mut previous = first.revision;
        for length in [1, 2, 8, 1, 0, 18, 19, 0] {
            let frame = f.draw(&theme, view(length));
            assert_eq!(frame.previous_revision, Some(previous));
            assert!(
                damage_pixels(&frame) < 3840 * 2160 / 20,
                "a field must not repaint the output"
            );
            previous = frame.revision;
        }
        for changed in [
            LockView {
                clock: "10:43",
                ..view(0)
            },
            LockView {
                date: "Sunday, 25 October",
                ..view(0)
            },
            LockView {
                error: Some("Incorrect password"),
                ..view(0)
            },
            LockView {
                verifying: true,
                ..view(0)
            },
            LockView {
                notification_count: 2,
                ..view(0)
            },
            LockView {
                keyboard: Some(ui::passcode_keyboard::PasscodeKeyboard::default()),
                ..view(0)
            },
            LockView {
                alert: Some("Low battery"),
                ..view(0)
            },
        ] {
            f.draw(&theme, view(0));
            assert!(f.draw(&theme, changed).damage_regions.is_none());
        }
        f.draw(&theme, view(0));
        f.osd = Some(Osd::volume(0.5, false));
        assert!(f.draw(&theme, view(1)).damage_regions.is_none());
        f.osd = None;
        assert!(f.draw(&theme, view(2)).damage_regions.is_none());
        drop(f);
    }
}

#[test]
fn relock_display_changes_and_desktop_revisions_cannot_reuse_an_old_lock() {
    let mut f = Fixture::new((1280, 800));
    let theme = Theme::dark();
    let desktop = f.builder.paint_with_damage();
    let first = f.draw(&theme, view(0));
    assert!(first.revision > desktop.revision);
    assert!(first.damage_regions.is_none());
    let second = f.draw(&theme, view(1));
    assert!(second.revision > first.revision);
    assert!(second.damage_regions.is_some());
    // A lock edge invalidates history even when no desktop frame intervened.
    f.retained.clear();
    assert!(f.draw(&theme, view(2)).damage_regions.is_none());
    f.outputs[0].scale = 2.0;
    assert!(f.draw(&theme, view(3)).damage_regions.is_none());
    f.outputs[0].size = (1200.0, 700.0);
    assert!(f.draw(&theme, view(4)).damage_regions.is_none());
    f.outputs.push(LockOutput {
        origin: (1200.0, 0.0),
        size: (80.0, 800.0),
        scale: 1.0,
    });
    assert!(f.draw(&theme, view(5)).damage_regions.is_none());
    assert!(f.draw(&Theme::light(), view(6)).damage_regions.is_none());
    f.wallpaper = Some(ui::raster::rgba8(
        vello::peniko::Blob::new(std::sync::Arc::new(vec![10, 30, 70, 255])),
        1,
        1,
    ));
    assert!(f.draw(&theme, view(6)).damage_regions.is_none());
    f.wallpaper = None;
    assert!(f.draw(&theme, view(6)).damage_regions.is_none());
    f.retained.clear();
    let desktop = f.builder.paint_with_damage();
    let relock = f.draw(&theme, view(0));
    drop(f);
    assert!(relock.revision > desktop.revision);
    assert!(relock.damage_regions.is_none());
}

fn assert_pixels(retained: &mut VelloDrmBridge, dense: &mut VelloDrmBridge, frame: &FramePaint) {
    let pitch = 1280 * 4;
    let mut actual = vec![0; pitch * 800];
    let mut expected = actual.clone();
    retained
        .paint_damage_into_xrgb8888(
            &frame.scene,
            SceneDamage {
                revision: frame.revision,
                previous_revision: frame.previous_revision,
                regions: frame.damage_regions.as_deref(),
            },
            &mut actual,
            pitch,
        )
        .expect("retained lock render");
    if frame
        .damage_regions
        .as_ref()
        .is_some_and(|rects| !rects.is_empty())
    {
        let stats = retained
            .last_sparse_render_stats()
            .expect("bounded password edit must use sparse fine");
        assert!(stats.written_pixels <= damage_pixels(frame));
    }
    dense
        .paint_into_xrgb8888(&frame.scene, &mut expected, pitch)
        .expect("dense lock oracle");
    let differences: Vec<_> = actual
        .iter()
        .zip(&expected)
        .filter(|(a, b)| a != b)
        .collect();
    // The same native-pixel oracle allowance as frame_retained_tests: GPU
    // blend-edge rounding can differ by one unit in a handful of bytes.
    assert!(
        differences.len() <= 16 && differences.iter().all(|(a, b)| a.abs_diff(**b) <= 1),
        "retained lock pixels diverged: {} bytes",
        differences.len()
    );
}

#[test]
fn retained_password_pixels_match_full_renders_in_every_theme_and_after_dropped_frames() {
    let mut retained = VelloDrmBridge::new(1280, 800).expect("GPU required for lock pixel oracle");
    let mut dense = VelloDrmBridge::new(1280, 800).expect("GPU required for dense lock oracle");
    retained.wait_for_gpu_completion_for_test();
    dense.wait_for_gpu_completion_for_test();
    let mut f = Fixture::new((1280, 800));
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        f.retained.clear();
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(0)));
        for length in [1, 8, 2, 0, 18, 19, 0] {
            let frame = f.draw(&theme, view(length));
            assert!(frame.damage_regions.is_some());
            assert_pixels(&mut retained, &mut dense, &frame);
        }
        let skipped = f.draw(&theme, view(8));
        let latest = f.draw(&theme, view(1)).superseding(&skipped);
        assert!(latest.damage_regions.is_some());
        assert_pixels(&mut retained, &mut dense, &latest);
        let changed_clock = f.draw(
            &theme,
            LockView {
                clock: "11:00",
                ..view(1)
            },
        );
        assert!(changed_clock.damage_regions.is_none());
        assert_pixels(&mut retained, &mut dense, &changed_clock);
        // Return to the old clock and clear the placeholder/dots again.
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(0)));
        f.retained.clear();
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(1)));
        // Mixed-scale outputs, including moving the credential surface to
        // the other display: no old field or placeholder may survive there.
        f.outputs = vec![
            LockOutput {
                origin: (0.0, 0.0),
                size: (640.0, 800.0),
                scale: 1.0,
            },
            LockOutput {
                origin: (640.0, 0.0),
                size: (640.0, 800.0),
                scale: 2.0,
            },
        ];
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(0)));
        let typed = f.draw(&theme, view(1));
        assert!(typed.damage_regions.is_some());
        assert_pixels(&mut retained, &mut dense, &typed);
        f.outputs.swap(0, 1);
        let moved = f.draw(&theme, view(1));
        assert!(moved.damage_regions.is_none());
        assert_pixels(&mut retained, &mut dense, &moved);
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(2)));
        f.outputs = vec![LockOutput {
            origin: (0.0, 0.0),
            size: (1280.0, 800.0),
            scale: 1.0,
        }];
        f.wallpaper = Some(ui::raster::rgba8(
            vello::peniko::Blob::new(std::sync::Arc::new(vec![40, 70, 100, 255])),
            1,
            1,
        ));
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(0)));
        let over_wallpaper = f.draw(&theme, view(1));
        assert!(over_wallpaper.damage_regions.is_some());
        assert_pixels(&mut retained, &mut dense, &over_wallpaper);
        f.wallpaper = None;
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(2)));
    }
}

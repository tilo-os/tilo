//! Scrolling an in-process app on a presenter that keeps its target: the
//! producer hands it what moved ([`FramePaint::scroll_copies`]), the target
//! copies it and rasterises only the rest, and every presented frame is a
//! whole render's, in dark, light and high contrast, at 100 % and 200 %,
//! with a collaborator's pointer resting over the scrolled note.

use super::*;

/// The Notes desktop on one panel of 1600×1000 layout pixels shown at
/// `scale`, for a presenter that copies scrolls when `copies`.
fn scroll_desktop(
    theme: Theme,
    scale: f64,
    copies: bool,
) -> (PopulatedShellFixture, tilo_test_env::ScopedEnv, (u32, u32)) {
    const LOGICAL: (f64, f64) = (1600.0, 1000.0);
    FOCUS.with(|focus| focus.set(None));
    let mut clock = tilo_test_env::ScopedEnv::new();
    clock.set("TILO_CLOCK", "09:41");
    let panel = (
        (LOGICAL.0 * scale).round() as u32,
        (LOGICAL.1 * scale).round() as u32,
    );
    // Laid out at the desktop's layout size from the start; a scaled panel
    // only magnifies it.
    let mut frame = PopulatedShellFixture::new(
        (LOGICAL.0 as u32, LOGICAL.1 as u32),
        theme.with_reduce_motion(true),
    );
    if (scale - 1.0).abs() > f64::EPSILON {
        frame.set_output_geometry(vec![crate::shell::output_scene::OutputGeometry {
            id: 0,
            monitor: String::new(),
            connector: String::new(),
            x: 0.0,
            y: 0.0,
            width: LOGICAL.0,
            height: LOGICAL.1,
            scale,
            scene: Rect::new(0.0, 0.0, f64::from(panel.0), f64::from(panel.1)),
        }]);
    }
    frame.set_scroll_copy_presenter(copies);
    settle(&mut frame, 4);
    (frame, clock, panel)
}

fn press_key(frame: &mut FrameBuilder, key: &Key) {
    for state in [
        winit::event::ElementState::Pressed,
        winit::event::ElementState::Released,
    ] {
        dispatch(frame, &ui::synthetic_input::key_input(key, state));
    }
}

/// Hand the Notes tile `event` in its own logical pixels, as the shell
/// routes a pointer event to the tile under the pointer, and let it run a
/// frame for it.
fn notes_input(frame: &mut FrameBuilder, event: &WindowEvent) {
    let tile = frame.node_map.values_mut().find_map(|widget| {
        // SAFETY: node_map pointers are owned by the live widget tree and
        // remain valid while the frame is exclusively borrowed.
        let widget = unsafe { &mut **widget };
        let app = widget
            .as_any_mut()?
            .downcast_mut::<ui::app::EmbeddedApp>()?;
        let notes = app.component_root_mut().and_then(|root| {
            tilo_app::framework::testing::surface_in_root::<notes::NotesShell, notes::NotesApp>(
                root,
            )
        });
        notes.is_some().then(|| std::ptr::from_mut(app))
    });
    let tile = tile.expect("the desktop shows a Notes tile");
    // SAFETY: the tile lives in the widget tree `frame` borrows exclusively.
    unsafe { &mut *tile }.input_for_test(event.clone());
}

/// The pointer resting at `at` (layout pixels) over the Notes tile, in the
/// tile's own pixels.
fn rest_over_notes(frame: &mut FrameBuilder, at: Point) {
    point_at(frame, at);
    let origin = with_notes_on_screen(&*frame, |rect, _| rect.origin());
    notes_input(
        frame,
        &ui::synthetic_input::cursor_moved(at.x - origin.x, at.y - origin.y),
    );
}

/// A touchpad scroll of `dy` over the Notes tile.
fn wheel(frame: &mut FrameBuilder, dy: f64) {
    notes_input(
        frame,
        &WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
                0.0, dy,
            )),
            phase: winit::event::TouchPhase::Moved,
        },
    );
}

/// Open a note in the Notes window on screen and write `paragraphs`
/// paragraphs into it, as a person types them; then leave the body, rest
/// the pointer over it and scroll back to the top. Returns where the
/// pointer rests, in layout pixels.
fn write_a_long_note(frame: &mut FrameBuilder, label: &str, reach: f64) -> Point {
    let _ = open_a_note(frame, label);
    // A new note may open naming itself: leave the title first.
    if with_notes_on_screen(&*frame, |_, notes| notes.main_title_editing_for_test()) {
        press_key(frame, &Key::Named(NamedKey::Escape));
        settle(frame, 2);
    }
    let body = with_notes_on_screen(&*frame, |rect, notes| {
        let panel = notes.main_panel_mut();
        let title = panel.title_rect_for_test();
        Point::new(
            rect.x0 + title.x0 + 24.0,
            rect.y0 + panel.body_top_for_test() + 12.0,
        )
    });
    point_at(frame, body);
    press(frame, winit::event::MouseButton::Left);
    settle(frame, 3);
    let max_scroll = |frame: &FrameBuilder| {
        with_notes_on_screen(frame, |_, notes| {
            notes.main_panel_mut().max_scroll_for_test()
        })
    };
    // Paragraphs until the note scrolls `reach` past its window, laid out
    // as they come (shaping can land a frame or two after the text).
    let mut paragraph = 0_usize;
    while max_scroll(frame) < reach && paragraph < 400 {
        // Each a committed phrase and Enter, handed to the tile as the
        // shell hands it keyboard input: a key at a time through the whole
        // desktop is minutes of a debug build per note.
        for _ in 0..10 {
            notes_input(
                frame,
                &WindowEvent::Ime(winit::event::Ime::Commit(format!(
                    "Paragraph {paragraph}: the quick brown fox jumps over the lazy dog, \
                     and a second clause so the line runs on a while."
                ))),
            );
            for state in [
                winit::event::ElementState::Pressed,
                winit::event::ElementState::Released,
            ] {
                notes_input(
                    frame,
                    &ui::synthetic_input::key_input(&Key::Named(NamedKey::Enter), state),
                );
            }
            paragraph += 1;
        }
        for _ in 0..5 {
            frame.wake_mounted_components();
            settle(frame, 1);
            if max_scroll(frame) >= reach {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    press_key(frame, &Key::Named(NamedKey::Escape));
    settle(frame, 3);
    assert!(
        max_scroll(frame) >= reach,
        "{label}: the note scrolls {reach} px past its window after {paragraph} paragraphs: {}",
        with_notes_on_screen(&*frame, |_, notes| notes
            .main_panel_mut()
            .scroll_state_for_test())
    );
    let rest = with_notes_on_screen(&*frame, |rect, _| {
        Point::new(rect.center().x, rect.y0 + rect.height() * 0.6)
    });
    rest_over_notes(frame, rest);
    wheel(frame, 100_000.0);
    settle(frame, 6);
    settle_notes(frame);
    rest
}

/// What a run of scroll frames did on the presenter.
#[derive(Debug, Default)]
struct ScrollRun {
    frames: usize,
    /// Frames the target copied, with what it rasterised and copied.
    copied_frames: usize,
    rasterised: u64,
    copied: u64,
    /// Every frame's rasterised pixels, copies or not.
    raster_total: u64,
    /// Frames rendered whole.
    dense: usize,
    /// The Notes window, in scene pixels.
    notes_area: u64,
}

/// The pixels of a `panel` target `rects` cover, each once.
fn covered(rects: &[FineDamageRect], panel: (u32, u32)) -> u64 {
    let (width, height) = (panel.0 as usize, panel.1 as usize);
    let mut cells = vec![false; width * height];
    for rect in rects {
        let (x0, y0) = (rect.x as usize, rect.y as usize);
        let x1 = (x0 + rect.width as usize).min(width);
        let y1 = (y0 + rect.height as usize).min(height);
        for y in y0.min(height)..y1 {
            cells[y * width + x0.min(x1)..y * width + x1].fill(true);
        }
    }
    cells.iter().filter(|cell| **cell).count() as u64
}

/// Scroll the note `frames` frames of `step` layout pixels (down for a
/// positive step), presenting each frame on a retained target as the DRM
/// presenter does (copies staged, then the frame rendered with its damage)
/// and checking it against a whole render every few frames.
fn scroll_and_present(
    frame: &mut FrameBuilder,
    (presented, whole): (
        &mut crate::shell::render::paint::RetainedGpuTarget,
        &mut crate::shell::render::paint::RetainedGpuTarget,
    ),
    panel: (u32, u32),
    (label, step, frames): (&str, f64, usize),
) -> ScrollRun {
    let mut run = ScrollRun::default();
    let scale = f64::from(panel.0) / frame.viewport.0;
    let notes = with_notes_on_screen(&*frame, |rect, _| rect);
    run.notes_area = (notes.area() * scale * scale) as u64;
    for index in 0..frames {
        let scrolled_from = with_notes_on_screen(&*frame, |_, notes| {
            notes.main_panel_mut().scroll_y_for_test()
        });
        wheel(frame, -step);
        frame.relayout();
        let scrolled_to = with_notes_on_screen(&*frame, |_, notes| {
            notes.main_panel_mut().scroll_y_for_test()
        });
        assert!(
            index > 0 || (scrolled_to - scrolled_from).abs() > f64::EPSILON,
            "{label}: the wheel scrolls the note ({scrolled_from} -> {scrolled_to}): {}",
            with_notes_on_screen(&*frame, |_, notes| notes
                .main_panel_mut()
                .scroll_state_for_test())
        );
        let previous = frame.last_produced_sections.clone();
        let painted = frame.paint_with_damage();
        let changed = painted
            .damage_regions
            .as_ref()
            .is_none_or(|damage| !damage.is_empty());
        if changed && painted.scroll_copies.is_none() {
            eprintln!(
                "{label}: frame {index} copies={} damage={:?}: {}",
                painted.scroll_copies.is_some(),
                painted
                    .damage_regions
                    .as_ref()
                    .map(|damage| covered(damage, panel)),
                part_refusals(previous.as_ref(), frame)
            );
        }
        let damage = SceneDamage {
            revision: painted.revision,
            previous_revision: painted.previous_revision,
            regions: painted.damage_regions.as_deref(),
        };
        if presented.keep_unchanged(damage) {
            run.frames += 1;
            continue;
        }
        presented.stage_scroll_copies(Some(painted.revision), painted.scroll_copies.as_ref());
        presented
            .render_until(
                &painted.scene,
                damage,
                Instant::now() + tilo_test_env::hang_bound(),
            )
            .unwrap_or_else(|error| panic!("{label}: frame {index}: {error}"));
        run.frames += 1;
        match (presented.last_scroll_copy_work(), &painted.scroll_copies) {
            (Some((_, moved_pixels)), Some(copies)) => {
                let rasterised = covered(&copies.raster, panel);
                run.copied_frames += 1;
                run.rasterised += rasterised;
                run.copied += moved_pixels;
                run.raster_total += rasterised;
            }
            _ => match painted.damage_regions.as_deref() {
                Some(damage) if presented.last_sparse_render_attempts() > 0 => {
                    run.raster_total += covered(damage, panel);
                }
                _ => {
                    run.dense += 1;
                    run.raster_total += u64::from(panel.0) * u64::from(panel.1);
                }
            },
        }
        if index % 4 == 3 || index + 1 == frames {
            let shown = presented.read_rgba_for_test();
            let expected = render_whole(whole, &painted.scene, painted.revision, label);
            let apart = pixels_apart(&shown, &expected, panel.0);
            let worst = apart.iter().map(|&(_, _, step)| step).max().unwrap_or(0);
            assert!(
                apart.len() <= OFFSET_PIXELS && worst <= OFFSET_STEPS,
                "{label}: frame {index} shows {} pixels a whole render does not (worst {worst} \
                 steps) ({run:?}):\n{}",
                apart.len(),
                apart_map(&apart, panel)
            );
        }
    }
    run
}

/// Scroll a long note down and back up on a desktop at `scale`, with a
/// collaborator's pointer resting over the note when `peer`, on a presenter
/// that copies when `copies`.
fn scroll_a_long_note(
    theme: Theme,
    scale: f64,
    peer: bool,
    copies: bool,
) -> (ScrollRun, ScrollRun) {
    let (mut frame, _clock, panel) = scroll_desktop(theme, scale, copies);
    let label = format!("scrolling a note at {scale}x");
    let rest = write_a_long_note(&mut frame, &label, 16.0 * 40.0 + 120.0);
    if peer {
        point_peer_at(&mut frame, (rest.x - 120.0, rest.y - 80.0));
    }
    settle(&mut frame, 2);
    let mut presented = gpu_target(panel);
    let mut whole = gpu_target(panel);
    let first = frame.paint_with_damage();
    let _ = render_whole(&mut presented, &first.scene, first.revision, &label);
    let down = scroll_and_present(
        &mut frame,
        (&mut presented, &mut whole),
        panel,
        (&format!("{label}, down"), 40.0, 16),
    );
    let up = scroll_and_present(
        &mut frame,
        (&mut presented, &mut whole),
        panel,
        (&format!("{label}, up"), -24.0, 8),
    );
    (down, up)
}

fn per(total: u64, count: usize) -> u64 {
    total / u64::try_from(count.max(1)).unwrap_or(1)
}

fn assert_scroll_copies(run: &ScrollRun, label: &str) {
    let notes_area = run.notes_area;
    eprintln!(
        "shell-scroll-copy {label}: frames={} copied={} dense={} notes_px={notes_area} \
         rasterised_px_per_copied_frame={} copied_px_per_copied_frame={} raster_px_per_frame={}",
        run.frames,
        run.copied_frames,
        run.dense,
        per(run.rasterised, run.copied_frames),
        per(run.copied, run.copied_frames),
        per(run.raster_total, run.frames),
    );
    assert!(
        run.copied_frames * 4 >= run.frames * 3,
        "{label}: most frames copy ({run:?})"
    );
    let per_frame = per(run.rasterised, run.copied_frames);
    // The exposed strip, the window's and tile's edge bands, the thumb's
    // track and the peer's mark, where they are and where the copy took
    // them, in 16-pixel clip tiles.
    assert!(
        per_frame * 10 < notes_area * 3,
        "{label}: a frame that copies rasterises the strip the scroll exposed and what is \
         drawn over the note, not the note ({per_frame} of {notes_area} px, {run:?})"
    );
}

/// Scroll a long note in `theme` at 100 % and 200 %, with a peer's
/// pointer over it, on a presenter that copies.
fn scroll_copies_in((name, theme): ThemeCase) {
    for scale in [1.0, 2.0] {
        let (down, up) = scroll_a_long_note(theme(), scale, true, true);
        assert_scroll_copies(&down, &format!("{name} {scale}x down"));
        assert_scroll_copies(&up, &format!("{name} {scale}x up"));
    }
}

// One theme per test: each writes and scrolls a note twice, and the three
// together would outlast a test's time budget.
#[test]
fn scrolling_an_in_process_app_copies_on_the_presenter_and_matches_a_full_render() {
    scroll_copies_in(THEMES[0]);
}

#[test]
fn scrolling_an_in_process_app_copies_and_matches_in_the_light_theme() {
    scroll_copies_in(THEMES[1]);
}

#[test]
fn scrolling_an_in_process_app_copies_and_matches_in_high_contrast() {
    scroll_copies_in(THEMES[2]);
}

/// At 200 % a step of half a layout pixel is a whole device pixel: the tile
/// plans its copies in the display's pixels, so the presenter copies those
/// steps too, and every frame is still a whole render's.
#[test]
fn half_pixel_steps_at_200_percent_copy_and_match() {
    let (mut frame, _clock, panel) = scroll_desktop(Theme::dark(), 2.0, true);
    let label = "half-pixel steps at 2x";
    let rest = write_a_long_note(&mut frame, label, 16.0 * 20.5 + 120.0);
    point_peer_at(&mut frame, (rest.x - 120.0, rest.y - 80.0));
    settle(&mut frame, 2);
    let mut presented = gpu_target(panel);
    let mut whole = gpu_target(panel);
    let first = frame.paint_with_damage();
    let _ = render_whole(&mut presented, &first.scene, first.revision, label);
    let down = scroll_and_present(
        &mut frame,
        (&mut presented, &mut whole),
        panel,
        (&format!("{label}, down"), 20.5, 12),
    );
    assert_scroll_copies(&down, label);
}

/// The same scroll for a presenter that does not copy: every frame
/// rasterises the note's viewport, the number the copies are measured
/// against.
#[test]
fn a_presenter_that_does_not_copy_rasterises_the_scrolled_note_and_matches() {
    let (down, _) = scroll_a_long_note(Theme::dark(), 1.0, false, false);
    eprintln!(
        "shell-scroll-copy without copies: frames={} dense={} notes_px={} raster_px_per_frame={}",
        down.frames,
        down.dense,
        down.notes_area,
        per(down.raster_total, down.frames)
    );
    assert_eq!(down.copied_frames, 0, "{down:?}");
}

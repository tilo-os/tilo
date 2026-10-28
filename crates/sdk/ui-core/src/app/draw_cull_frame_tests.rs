//! A frame drawn only where its damage needs it ([`crate::draw_cull`])
//! presents what a frame drawn in full presents: the same copies and the
//! same damage, and inside that damage the same pixels. Outside it the host
//! shows the last frame moved by the copies, which the dense comparisons of
//! the copies themselves check against a full render. Each scroller is
//! scrolled down and back up, in dark, light and high contrast, at 100 %
//! and 200 %, beside a twin drawn in full.

use super::*;
use crate::offscreen::OffscreenRenderer;

/// A window of `.0` device pixels per logical pixel, 640 × 400 logical.
struct Scaled(f64);

impl AppSurface for Scaled {
    fn scale_factor(&self) -> f64 {
        self.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new((640.0 * self.0) as u32, (400.0 * self.0) as u32)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: winit::window::CursorIcon) {}
}

/// An app showing `root`, its damage as `damage` reports it.
fn app(root: Box<dyn Widget>, theme: &Theme, window: &Scaled) -> App {
    let mut app = App::new_with_controlled_inputs(crate::state::State::new(), root);
    app.set_theme(theme.clone());
    let _ = window;
    app
}

/// What a run of a culled app beside its twin drawn in full did.
#[derive(Debug, Default)]
struct Twins {
    frames: usize,
    copied: usize,
    /// Frames whose culled scene encoded fewer draw objects.
    smaller: usize,
    /// Frames that left something out, and those drawn again in full.
    culled: (usize, usize),
}

/// `pixels` (RGBA8, `width` wide) with every pixel outside `damage`
/// (device pixels, as a host repaints them: rounded outward) taken from
/// `other`.
fn within(pixels: &[u8], other: &[u8], width: u32, damage: &[Rect]) -> Vec<u8> {
    let mut out = other.to_vec();
    let height = pixels.len() / 4 / width as usize;
    for rect in damage {
        let x0 = rect.x0.floor().clamp(0.0, f64::from(width)) as usize;
        let x1 = rect.x1.ceil().clamp(0.0, f64::from(width)) as usize;
        let y0 = rect.y0.floor().clamp(0.0, height as f64) as usize;
        let y1 = rect.y1.ceil().clamp(0.0, height as f64) as usize;
        for y in y0..y1 {
            let row = y * width as usize * 4;
            out[row + x0 * 4..row + x1 * 4].copy_from_slice(&pixels[row + x0 * 4..row + x1 * 4]);
        }
    }
    out
}

/// Scroll a culled app showing `make()` and its twin drawn in full by
/// `steps` (logical pixels per frame, frames), checking every frame.
fn twins(
    make: fn() -> Box<dyn Widget>,
    theme: &Theme,
    scale: f64,
    steps: &[(f64, usize)],
) -> Twins {
    let window = Scaled(scale);
    let size = window.inner_size();
    let mut culled = app(make(), theme, &window);
    let mut full = app(make(), theme, &window);
    let mut culled_damage = SurfaceDamage::copying().drawing_only_damage();
    let mut full_damage = SurfaceDamage::copying();
    let (mut culled_scene, mut full_scene) = (Scene::new(), Scene::new());
    // The twins see one clock, a frame at 120 Hz apart: what moves with
    // time (a bar fading in, a glide) stands the same in both.
    let start = crate::time::now();
    let clock = |tick: u64| start + std::time::Duration::from_nanos(tick * FRAME_NS);
    let mut tick = 0;
    for at in 0..6 {
        tick += 1;
        with_monotonic_time(clock(tick), || {
            if at == 2 {
                for app in [&mut culled, &mut full] {
                    app.surface_input(
                        &window,
                        crate::synthetic_input::cursor_moved(320.0 * scale, 200.0 * scale),
                        false,
                    );
                }
            }
            culled_scene.reset();
            full_scene.reset();
            culled.surface_frame_with_damage(&window, &mut culled_scene, &mut culled_damage);
            full.surface_frame_with_damage(&window, &mut full_scene, &mut full_damage);
        });
        let _ = culled_damage.take_copying();
        let _ = full_damage.take_copying();
    }
    let background = theme.tokens.colors.surfaces.content;
    let mut renderer = OffscreenRenderer::try_new(size.width, size.height)
        .expect("a culled frame is checked on the GPU the toolkit renders with");
    let mut twins = Twins::default();
    for &(step, frames) in steps {
        for frame in 0..frames {
            tick += 1;
            with_monotonic_time(clock(tick), || {
                for app in [&mut culled, &mut full] {
                    app.surface_input(&window, wheel(-step * scale, TouchPhase::Moved), false);
                }
                culled_scene.reset();
                full_scene.reset();
                culled.surface_frame_with_damage(&window, &mut culled_scene, &mut culled_damage);
                full.surface_frame_with_damage(&window, &mut full_scene, &mut full_damage);
            });
            twins.frames += 1;
            let (culled_frame, full_frame) =
                (culled_damage.take_copying(), full_damage.take_copying());
            assert_eq!(
                culled_frame, full_frame,
                "frame {frame} of a {step} px scroll at {scale}x: the culled frame's copies \
                 and damage are the full frame's"
            );
            let Some((copies, damage)) = culled_frame else {
                // Repainted whole, the culled frame drew everything.
                assert_eq!(
                    culled_scene.encoding().draw_tags.len(),
                    full_scene.encoding().draw_tags.len()
                );
                continue;
            };
            twins.copied += usize::from(!copies.is_empty());
            twins.smaller += usize::from(
                culled_scene.encoding().draw_tags.len() < full_scene.encoding().draw_tags.len(),
            );
            let expected = renderer.render(&full_scene, background);
            let drawn = renderer.render(&culled_scene, background);
            let differing = OffscreenRenderer::pixels_differing_beyond_aa(
                &within(&drawn, &expected, size.width, &damage),
                &expected,
            );
            assert_eq!(
                differing, 0,
                "frame {frame} of a {step} px scroll at {scale}x: {differing} pixels of the \
                 damage {damage:?} differ from the frame drawn in full"
            );
        }
    }
    twins.culled = culled.layout.cull.tally;
    twins
}

/// A long column of text in a scroll view that does not fill the window,
/// at its top: the frame around it paints at its edges, where the planner
/// repaints whatever a copy takes there.
fn framed_column() -> Box<dyn Widget> {
    let column = (0..400).fold(Column::new().with_intrinsic_height(), |column, row| {
        column.push(Box::new(Text::new(&sentence(row))))
    });
    Box::new(
        Column::new().with_no_padding().push(Box::new(
            Scrollable::new_vertical()
                .with_height(300.0)
                .with_child(Box::new(column)),
        )),
    )
}

const RUNS: &[(f64, usize)] = &[(40.0, 10), (-24.0, 6)];

#[test]
fn culled_scroll_frames_present_what_full_frames_present() {
    let scrollers: [(&str, fn() -> Box<dyn Widget>); 4] = [
        ("scrollable column", || column(400)),
        ("virtual list", || virtual_list(2_000)),
        ("code editor", || code(600)),
        ("framed column", framed_column),
    ];
    for (name, make) in scrollers {
        for (theme_name, theme) in [
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high contrast", Theme::high_contrast()),
        ] {
            for scale in [1.0, 2.0] {
                let twins = twins(make, &theme, scale, RUNS);
                eprintln!("{name} {theme_name} {scale}x: {twins:?}");
                assert!(
                    twins.copied * 4 >= twins.frames * 3,
                    "{name} {theme_name} {scale}x: most frames copy ({twins:?})"
                );
                assert!(
                    twins.smaller * 2 >= twins.copied,
                    "{name} {theme_name} {scale}x: a frame that copies draws less than a full \
                     one ({twins:?})"
                );
            }
        }
    }
}

#[test]
fn the_other_scrollers_present_what_full_frames_present() {
    let scrollers: [(&str, fn() -> Box<dyn Widget>); 3] = [
        ("data table", || data_table(2_000)),
        ("tree view", || tree_view(100, 10)),
        ("scroll region", || transcript(2_000)),
    ];
    for (name, make) in scrollers {
        for scale in [1.0, 2.0] {
            let twins = twins(make, &Theme::dark(), scale, RUNS);
            eprintln!("{name} {scale}x: {twins:?}");
        }
    }
}

/// The first frame of a scroll inside a frame expects nothing the frame
/// paints at its edges: the rows it leaves out there are repainted, so it
/// draws its widgets again in full, and later frames leave out only what
/// the copies keep.
#[test]
fn a_frame_that_left_out_what_it_repaints_draws_its_widgets_again() {
    let twins = twins(framed_column, &Theme::dark(), 1.0, &[(40.0, 8)]);
    assert!(twins.culled.1 >= 1, "{twins:?}");
    assert!(
        twins.culled.1 * 2 < twins.culled.0.max(2),
        "most frames that leave something out hold ({twins:?})"
    );
}

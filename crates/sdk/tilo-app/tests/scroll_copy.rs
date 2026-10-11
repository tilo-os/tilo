#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    reason = "frame tallies and pixel counts of a bounded run over a window clamped before indexing"
)]

//! A scroll in a long list copies the pixels that stay on screen and
//! repaints only what it exposed, the way a window presents it: the live
//! window driven through the host's input and frame path
//! ([`tilo_app::App::surface_input`], [`tilo_app::App::surface_frame_with_damage`]
//! with [`tilo_app::app::SurfaceDamage::copying`]), every frame presented
//! over the one before on a GPU target (copies, then damage), and the
//! result compared with a full render of the same scene, in dark, light and
//! high contrast. A step that is not a whole number of device pixels
//! repaints the viewport instead, and still matches.

use tilo_app::app::{AppSurface, SurfaceDamage};
use tilo_app::framework::testing::drive_live_window;
use tilo_app::i18n::EmbeddedCatalog;
use tilo_app::offscreen::OffscreenRenderer;
use tilo_app::prelude::*;
use tilo_app::theme::Theme;
use tilo_app::winit::dpi::{PhysicalPosition, PhysicalSize};
use tilo_app::winit::event::{DeviceId, MouseScrollDelta, TouchPhase};
use tilo_app::winit::window::CursorIcon;

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", "copy-draft = Next task\ncopy-done = Completed\n")],
    "copy-draft",
);

#[derive(Model, Default, Debug, Clone, PartialEq)]
/// One task.
pub struct Task {
    pub title: String,
    pub done: bool,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
/// The draft and the tasks.
pub struct Checklist {
    pub draft: String,
    pub tasks: Collection<Task>,
}

#[resource(kind = "test.scroll-copy-checklist", label = "Checklist")]
impl Checklist {
    /// Set the next task's title.
    #[action]
    pub fn draft(&mut self, title: String) {
        self.draft = title;
    }

    /// Add `count` tasks at the end.
    #[action]
    pub fn seed(&mut self, cx: &mut ActionCx, count: u32) {
        for _ in 0..count {
            let index = self.tasks.len();
            self.tasks.push(
                cx.id(),
                Task {
                    title: format!("Task {index}: the quick brown fox jumps over the lazy dog"),
                    done: index.is_multiple_of(3),
                },
            );
        }
    }

    /// Mark a task completed or open.
    ///
    /// # Errors
    /// Refuses a task that no longer exists.
    #[action]
    pub fn set_done(&mut self, task: &Id, done: bool) -> Result<(), &'static str> {
        self.tasks.get_mut(task).ok_or("gone")?.done = done;
        Ok(())
    }
}

/// A checklist of 2,000 tasks.
struct Window;

impl ResourceApp for Window {
    type Resource = Checklist;
    const ID: &'static str = "so.tilo.test-scroll-copy-checklist";
    const TITLE: &'static str = "Checklist";

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn genesis(_: &LaunchInput<'_>) -> Vec<Invoke<Checklist>> {
        vec![Checklist::SEED.with(2_000)]
    }

    fn view(cx: &mut ViewCx<Checklist>) -> Element {
        checklist(cx)
    }
}

/// A checklist of 30 tasks: a little more than the window shows.
struct ShortWindow;

impl ResourceApp for ShortWindow {
    type Resource = Checklist;
    const ID: &'static str = "so.tilo.test-scroll-copy-short-checklist";
    const TITLE: &'static str = "Checklist";

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn genesis(_: &LaunchInput<'_>) -> Vec<Invoke<Checklist>> {
        vec![Checklist::SEED.with(30)]
    }

    fn view(cx: &mut ViewCx<Checklist>) -> Element {
        checklist(cx)
    }
}

/// The draft over the scrolled list of tasks.
fn checklist(cx: &ViewCx<Checklist>) -> Element {
    let draft = cx
        .input(|list| list.draft.clone(), Checklist::DRAFT)
        .with_id("draft");
    let rows = cx.each(
        |list| &list.tasks,
        |row| {
            let Some(id) = row.item().cloned() else {
                return Box::new(Column::new()) as Element;
            };
            Box::new(tilo_app::row![
                row.text(|task| task.title.clone()),
                row.checkbox(
                    "copy-done",
                    |task| task.done,
                    Checklist::SET_DONE.map(move |done| (id.clone(), done))
                ),
            ]) as Element
        },
    );
    let list = Scrollable::new_vertical().with_child(Box::new(rows));
    Box::new(tilo_app::column![draft, list])
}

/// The window's surface: 800×600 logical pixels at `scale`, presented by a
/// host that copies what a scroll keeps on screen when `copying`.
struct Surface {
    scale: f64,
    copying: bool,
}

impl Surface {
    fn size(&self) -> PhysicalSize<u32> {
        let side = |logical: f64| {
            let device = (logical * self.scale).round();
            u32::try_from(device as i64).unwrap()
        };
        PhysicalSize::new(side(800.0), side(600.0))
    }
}

impl AppSurface for Surface {
    fn scale_factor(&self) -> f64 {
        self.scale
    }
    fn inner_size(&self) -> PhysicalSize<u32> {
        self.size()
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: CursorIcon) {}
}

/// What a run of scroll frames did.
#[derive(Debug, Default)]
struct Run {
    frames: usize,
    /// Frames that copied, and what they repainted, summed.
    copied_frames: usize,
    repainted: f64,
    /// Frames the app repainted whole.
    whole: usize,
}

/// The pixels `rects` cover, each counted once, in a window of `size`.
fn covered(rects: &[Rect], size: PhysicalSize<u32>) -> f64 {
    let (width, height) = (size.width as usize, size.height as usize);
    let window = Rect::new(0.0, 0.0, width as f64, height as f64);
    let mut cells = vec![false; width * height];
    for rect in rects {
        let rect = rect.intersect(window).expand();
        for y in rect.y0 as usize..rect.y1 as usize {
            cells[y * width + rect.x0 as usize..y * width + rect.x1 as usize].fill(true);
        }
    }
    cells.iter().filter(|cell| **cell).count() as f64
}

/// Scroll the live window `frames` frames of `step` logical pixels each
/// (down for a positive step), the pointer resting over the list, and
/// present every frame as a window does. Every `check_every` frames, and
/// after the last, the presented pixels must equal a full render.
fn scroll(
    app: &mut tilo_app::App,
    surface: &Surface,
    theme: &Theme,
    (step, frames): (f64, usize),
) -> Run {
    let size = surface.size();
    let background = theme.tokens.colors.surfaces.content;
    let mut damage = if surface.copying {
        SurfaceDamage::copying()
    } else {
        SurfaceDamage::default()
    };
    let mut scene = Scene::new();
    app.surface_input(
        surface,
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(300.0 * surface.scale, 400.0 * surface.scale),
        },
        false,
    );
    for _ in 0..5 {
        scene.reset();
        app.surface_frame_with_damage(surface, &mut scene, &mut damage);
    }
    let _ = damage.take_copying();
    let mut presented = OffscreenRenderer::try_new(size.width, size.height)
        .expect("a scroll copy is checked on the GPU the toolkit renders with");
    let mut full = OffscreenRenderer::new(size.width, size.height);
    let _ = presented.render(&scene, background);
    let mut run = Run::default();
    for frame in 0..frames {
        app.surface_input(
            surface,
            WindowEvent::MouseWheel {
                device_id: DeviceId::dummy(),
                delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                    0.0,
                    -step * surface.scale,
                )),
                phase: TouchPhase::Moved,
            },
            false,
        );
        scene.reset();
        app.surface_frame_with_damage(surface, &mut scene, &mut damage);
        run.frames += 1;
        if let Some((copies, rects)) = damage.take_copying() {
            if !copies.is_empty() {
                run.copied_frames += 1;
                run.repainted += covered(&rects, size);
            }
            let _ =
                presented.render_copies_and_damage_gpu_wait(&scene, background, &copies, &rects);
        } else {
            run.whole += 1;
            let _ = presented.render(&scene, background);
        }
        if frame % 7 == 6 || frame + 1 == frames {
            let expected = full.render(&scene, background);
            let actual = presented.readback_current();
            let differing = OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected);
            assert_eq!(
                differing, 0,
                "frame {frame} of a {step} px scroll at {}x presented {differing} pixels a \
                 full render does not have ({run:?})",
                surface.scale
            );
        }
    }
    run
}

/// A theme to check in, by name.
type ThemeCase = (&'static str, fn() -> Theme);

const THEMES: [ThemeCase; 3] = [
    ("dark", Theme::dark),
    ("light", Theme::light),
    ("high contrast", Theme::high_contrast),
];

#[test]
fn scrolling_a_long_list_copies_what_stays_on_screen_and_matches_a_full_render() {
    for (name, theme) in THEMES {
        let run = drive_live_window::<Window, _>(|app| {
            app.set_theme(theme());
            app.init_layout();
            let surface = Surface {
                scale: 1.0,
                copying: true,
            };
            let down = scroll(app, &surface, &theme(), (40.0, 30));
            let up = scroll(app, &surface, &theme(), (-24.0, 15));
            (down, up)
        })
        .unwrap();
        let (down, up) = run;
        eprintln!("{name}: down {down:?}, up {up:?}");
        let window = 800.0 * 600.0;
        for (direction, run) in [("down", &down), ("up", &up)] {
            assert!(
                run.copied_frames * 4 >= run.frames * 3,
                "{name}: most frames scrolling {direction} copy ({run:?})"
            );
            let per_frame = run.repainted / run.copied_frames.max(1) as f64;
            assert!(
                per_frame < window * 0.3,
                "{name}: a frame scrolling {direction} that copies repaints the exposed strip \
                 and its surroundings, not the list ({per_frame} px, {run:?})"
            );
        }
    }
}

#[test]
fn a_step_of_part_of_a_device_pixel_repaints_and_matches() {
    let theme = Theme::dark;
    let fractional = drive_live_window::<Window, _>(|app| {
        app.set_theme(theme());
        app.init_layout();
        scroll(
            app,
            &Surface {
                scale: 1.0,
                copying: true,
            },
            &theme(),
            (40.5, 12),
        )
    })
    .unwrap();
    assert_eq!(
        fractional.copied_frames, 0,
        "half a device pixel cannot be copied ({fractional:?})"
    );
    // At 200 % the same half logical pixel is a whole device pixel.
    let doubled = drive_live_window::<Window, _>(|app| {
        app.set_theme(theme());
        app.init_layout();
        scroll(
            app,
            &Surface {
                scale: 2.0,
                copying: true,
            },
            &theme(),
            (40.5, 12),
        )
    })
    .unwrap();
    assert!(
        doubled.copied_frames * 4 >= doubled.frames * 3,
        "a whole device pixel step copies ({doubled:?})"
    );
}

#[test]
fn scrolling_past_the_end_stays_put_and_matches() {
    let theme = Theme::light;
    let run = drive_live_window::<ShortWindow, _>(|app| {
        app.set_theme(theme());
        app.init_layout();
        scroll(
            app,
            &Surface {
                scale: 1.0,
                copying: true,
            },
            &theme(),
            (90.0, 30),
        )
    })
    .unwrap();
    assert_eq!(run.whole, 0, "{run:?}");
}

#[test]
fn a_host_that_does_not_copy_repaints_the_view_and_matches() {
    let theme = Theme::high_contrast;
    let run = drive_live_window::<Window, _>(|app| {
        app.set_theme(theme());
        app.init_layout();
        let surface = Surface {
            scale: 1.0,
            copying: false,
        };
        scroll(app, &surface, &theme(), (40.0, 30))
    })
    .unwrap();
    eprintln!("without copies: {run:?}");
    assert_eq!(run.copied_frames, 0, "{run:?}");
}

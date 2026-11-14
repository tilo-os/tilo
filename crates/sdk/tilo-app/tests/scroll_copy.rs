#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
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
//! repaints the viewport instead, and still matches. A collaborator's
//! pointer resting over the list, painted over the widgets as a window
//! paints it ([`tilo_app::app::SurfaceDamage::painting_peers`]), does not
//! stop the copies: its mark repaints where it is and where a copy took it.
//!
//! The windows checked on a GPU target are the rows of one table ([`cases`]),
//! run by one test on the screens they share ([`Screen`]): a test process
//! pays for its first GPU frame in seconds, and each row adds only its own
//! window and frames.

use tilo_app::advanced::{Scene, WindowEvent};
use tilo_app::app::{AppSurface, SurfaceDamage};
use tilo_app::framework::testing::drive_live_window;
use tilo_app::i18n::EmbeddedCatalog;
use tilo_app::offscreen::OffscreenRenderer;
use tilo_app::prelude::*;

use crate::support_checklist;
use crate::support_checklist::{Checklist, Task};
use tilo_app::theme::Theme;
use tilo_app::winit::dpi::{PhysicalPosition, PhysicalSize};
use tilo_app::winit::event::{DeviceId, ElementState, MouseButton, MouseScrollDelta, TouchPhase};
use tilo_app::winit::window::CursorIcon;

/// A checklist of 2,000 tasks.
struct Window;

impl ResourceApp for Window {
    type Resource = Checklist;
    const ID: &'static str = "so.tilo.test-scroll-copy-checklist";
    const TITLE: &'static str = "Checklist";

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&support_checklist::CATALOG)
    }

    fn genesis(_: &LaunchInput<'_>) -> Vec<Invoke<Checklist>> {
        vec![Checklist::SEED_LONG.with(2_000)]
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
        Some(&support_checklist::CATALOG)
    }

    fn genesis(_: &LaunchInput<'_>) -> Vec<Invoke<Checklist>> {
        vec![Checklist::SEED_LONG.with(30)]
    }

    fn view(cx: &mut ViewCx<Checklist>) -> Element {
        checklist(cx)
    }
}

/// The draft over the scrolled list of tasks.
fn checklist(cx: &ViewCx<Checklist>) -> Element {
    let draft = cx.input(cx.paths().draft).with_id("draft");
    let rows = cx.each(|list| &list.tasks, task_row);
    let list = Scrollable::new_vertical().with_child(Box::new(rows));
    Box::new(tilo_app::column![draft, list])
}

/// One task: its title and whether it is done.
fn task_row(row: &mut ViewCx<Checklist, Task>) -> Element {
    let done = row.checkbox(support_checklist::DONE, row.paths().done);
    Box::new(tilo_app::row![row.text(|task| task.title.clone()), done])
}

/// The window's surface: 800×600 logical pixels at `scale`, presented by a
/// host that copies what a scroll keeps on screen when `copying`, with a
/// collaborator's pointer resting at `peer` (logical pixels), if any.
#[derive(Clone, Copy)]
struct Surface {
    scale: f64,
    copying: bool,
    peer: Option<(f64, f64)>,
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
///
/// A frame's rectangles are few and a window is up to two million pixels,
/// and this runs for every frame that copies: the cover is summed band by
/// band between the rectangles' own edges, where every row is crossed by
/// the same rectangles, rather than marked and counted pixel by pixel
/// ([`marked`], which the table below holds it to).
fn covered(rects: &[Rect], size: PhysicalSize<u32>) -> usize {
    let window = Rect::new(0.0, 0.0, f64::from(size.width), f64::from(size.height));
    // Each rectangle as the whole device pixels it touches inside the
    // window: its columns, then its rows, far edges exclusive.
    let spans: Vec<[usize; 4]> = rects
        .iter()
        .map(|rect| {
            let rect = rect.intersect(window).expand();
            [
                rect.x0 as usize,
                rect.x1 as usize,
                rect.y0 as usize,
                rect.y1 as usize,
            ]
        })
        .collect();
    let mut edges: Vec<usize> = spans.iter().flat_map(|span| [span[2], span[3]]).collect();
    edges.sort_unstable();
    edges.dedup();
    let mut pixels = 0;
    for band in edges.windows(2) {
        let mut columns: Vec<(usize, usize)> = spans
            .iter()
            .filter(|span| span[2] <= band[0] && band[1] <= span[3])
            .map(|span| (span[0], span[1]))
            .collect();
        columns.sort_unstable();
        let (mut width, mut end) = (0, 0);
        for (from, to) in columns {
            let from = from.max(end);
            if to > from {
                width += to - from;
                end = to;
            }
        }
        pixels += width * (band[1] - band[0]);
    }
    pixels
}

/// What [`covered`] counts, by marking every pixel a rectangle touches and
/// counting the marks.
fn marked(rects: &[Rect], size: PhysicalSize<u32>) -> usize {
    let (width, height) = (size.width as usize, size.height as usize);
    let window = Rect::new(0.0, 0.0, width as f64, height as f64);
    let mut cells = vec![false; width * height];
    for rect in rects {
        let rect = rect.intersect(window).expand();
        for y in rect.y0 as usize..rect.y1 as usize {
            cells[y * width + rect.x0 as usize..y * width + rect.x1 as usize].fill(true);
        }
    }
    cells.iter().filter(|cell| **cell).count()
}

#[test]
fn rectangles_cover_each_pixel_they_share_once() {
    let size = PhysicalSize::new(40, 30);
    let rect = Rect::new;
    let cases: [(&str, Vec<Rect>, usize); 13] = [
        ("no rectangle", vec![], 0),
        ("one", vec![rect(2.0, 3.0, 10.0, 8.0)], 40),
        ("one of no area", vec![rect(2.0, 3.0, 2.0, 8.0)], 0),
        (
            "the same one twice",
            vec![rect(2.0, 3.0, 10.0, 8.0), rect(2.0, 3.0, 10.0, 8.0)],
            40,
        ),
        (
            "two overlapping at a corner",
            vec![rect(0.0, 0.0, 10.0, 10.0), rect(5.0, 5.0, 15.0, 15.0)],
            175,
        ),
        (
            "two apart in the same rows",
            vec![rect(0.0, 0.0, 5.0, 10.0), rect(20.0, 0.0, 25.0, 10.0)],
            100,
        ),
        (
            "two sharing an edge",
            vec![rect(0.0, 0.0, 10.0, 10.0), rect(10.0, 0.0, 20.0, 10.0)],
            200,
        ),
        (
            "one inside another",
            vec![rect(0.0, 0.0, 20.0, 20.0), rect(5.0, 5.0, 10.0, 10.0)],
            400,
        ),
        (
            "a cross",
            vec![rect(10.0, 0.0, 20.0, 30.0), rect(0.0, 10.0, 40.0, 20.0)],
            600,
        ),
        (
            "edges inside a pixel take the pixel",
            vec![rect(1.5, 1.5, 3.2, 2.1)],
            6,
        ),
        (
            "two reaching past the window",
            vec![rect(-10.0, -10.0, 5.0, 5.0), rect(35.0, 25.0, 100.0, 100.0)],
            50,
        ),
        (
            "one outside the window",
            vec![rect(50.0, 40.0, 60.0, 50.0)],
            0,
        ),
        (
            "the window and a part of it",
            vec![rect(0.0, 0.0, 40.0, 30.0), rect(3.0, 3.0, 9.0, 9.0)],
            1200,
        ),
    ];
    for (name, rects, pixels) in &cases {
        assert_eq!(covered(rects, size), *pixels, "{name}");
        assert_eq!(
            marked(rects, size),
            *pixels,
            "{name}, marked pixel by pixel"
        );
    }
}

/// What a window's frames are presented and checked on: the target a host
/// presents into, copies first and then damage, and a renderer that draws
/// each checked frame whole.
///
/// Building a renderer costs as much as presenting several frames, so the
/// windows of one size are presented on one pair, as the windows a person
/// opens one after another are presented on one screen: every run begins
/// by rendering its window whole, which replaces every pixel the run before
/// it left in the target.
struct Screen {
    presented: OffscreenRenderer,
    full: OffscreenRenderer,
}

/// The screens a test presents its windows on, by size in device pixels.
type Screens = std::collections::HashMap<(u32, u32), Screen>;

/// Scroll the live window `frames` frames of `step` logical pixels each
/// (down for a positive step), the pointer resting over the list, and
/// present every frame on `screen` as a window does. Every seventh frame,
/// and after the last, the presented pixels must equal a full render;
/// `case` names the run where they do not.
fn scroll(
    app: &mut tilo_app::App,
    surface: &Surface,
    theme: &Theme,
    screen: &mut Screen,
    case: &str,
    (step, frames): (f64, usize),
) -> Run {
    let size = surface.size();
    let background = theme.tokens.colors.surfaces.content;
    let mut damage = if surface.copying {
        SurfaceDamage::copying()
    } else {
        SurfaceDamage::default()
    }
    .painting_peers();
    if let Some((x, y)) = surface.peer {
        app.presence_ctx_mut().upsert_remote_cursor(
            "ada".into(),
            [x as f32, y as f32],
            Some("Ada Lovelace"),
        );
    }
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
    let Screen { presented, full } = screen;
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
                run.repainted += covered(&rects, size) as f64;
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
                "{case}: frame {frame} of a {step} px scroll at {}x presented {differing} \
                 pixels a full render does not have ({run:?})",
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

/// Which checklist a case's window shows.
#[derive(Clone, Copy)]
enum List {
    /// [`Window`]'s 2,000 tasks.
    Long,
    /// [`ShortWindow`]'s 30.
    Short,
}

/// How many frames of a scroll must have copied.
#[derive(Clone, Copy)]
enum Copies {
    /// At least three in four.
    Most,
    /// None.
    None,
    /// As many as did.
    Any,
}

/// What each scroll of a case must have done, besides presenting what a
/// full render shows.
#[derive(Clone, Copy)]
struct Expect {
    copies: Copies,
    /// No frame repainted the window whole.
    never_whole: bool,
    /// A frame that copied repainted, on average, less than three tenths of
    /// the window: the exposed strip and its surroundings, not the list.
    a_strip: bool,
}

/// One window, opened and scrolled.
struct Case {
    name: String,
    list: List,
    theme: fn() -> Theme,
    /// How the window is presented. Where a collaborator's pointer rests
    /// over it, the same window is scrolled again without one, and the mark
    /// may cost a frame no more than a twentieth of the window.
    surface: Surface,
    /// The scrolls, in order: logical pixels a frame (down when positive),
    /// and frames.
    scrolls: &'static [(f64, usize)],
    expect: Expect,
}

/// Every window this file scrolls on a GPU target.
fn cases() -> Vec<Case> {
    let copying = |scale: f64, peer: Option<(f64, f64)>| Surface {
        scale,
        copying: true,
        peer,
    };
    let mut cases = Vec::new();
    // A long list scrolled down and back up copies what stays on screen.
    for (theme_name, theme) in THEMES {
        cases.push(Case {
            name: format!("{theme_name}: a long list"),
            list: List::Long,
            theme,
            surface: copying(1.0, None),
            scrolls: &[(40.0, 30), (-24.0, 15)],
            expect: Expect {
                copies: Copies::Most,
                never_whole: false,
                a_strip: true,
            },
        });
    }
    // A collaborator's pointer over the list does not stop the copies.
    for (theme_name, theme) in THEMES {
        for scale in [1.0, 2.0] {
            cases.push(Case {
                name: format!("{theme_name} {scale}x: a peer's pointer over the list"),
                list: List::Long,
                theme,
                surface: copying(scale, Some((260.0, 300.0))),
                scrolls: &[(40.0, 22), (-24.0, 15)],
                expect: Expect {
                    copies: Copies::Most,
                    never_whole: true,
                    a_strip: true,
                },
            });
        }
    }
    // A step of half a device pixel cannot be copied; at 200 % the same
    // half logical pixel is a whole device pixel, and copies.
    for (scale, copies) in [(1.0, Copies::None), (2.0, Copies::Most)] {
        cases.push(Case {
            name: format!("dark {scale}x: a step of 40.5 logical pixels"),
            list: List::Long,
            theme: Theme::dark,
            surface: copying(scale, None),
            scrolls: &[(40.5, 12)],
            expect: Expect {
                copies,
                never_whole: false,
                a_strip: false,
            },
        });
    }
    cases.push(Case {
        name: "light: scrolling past the end of a short list".into(),
        list: List::Short,
        theme: Theme::light,
        surface: copying(1.0, None),
        scrolls: &[(90.0, 30)],
        expect: Expect {
            copies: Copies::Any,
            never_whole: true,
            a_strip: false,
        },
    });
    cases.push(Case {
        name: "high contrast: a host that does not copy".into(),
        list: List::Long,
        theme: Theme::high_contrast,
        surface: Surface {
            scale: 1.0,
            copying: false,
            peer: None,
        },
        scrolls: &[(40.0, 30)],
        expect: Expect {
            copies: Copies::None,
            never_whole: false,
            a_strip: false,
        },
    });
    cases.extend(edge_cases());
    cases
}

/// The windows at the edges of what the rows of [`cases`] scroll: a display
/// whose scale is not a whole number, and a list short enough to be
/// scrolled from one of its ends to the other.
fn edge_cases() -> Vec<Case> {
    vec![
        // At 150 % the rows and the viewport's edges fall between device
        // pixels, and a step of 40 logical pixels is still 60 whole device
        // pixels (one of 24 is 36): it copies.
        Case {
            name: "dark 1.5x: a long list".into(),
            list: List::Long,
            theme: Theme::dark,
            surface: Surface {
                scale: 1.5,
                copying: true,
                peer: None,
            },
            scrolls: &[(40.0, 12), (-24.0, 8)],
            expect: Expect {
                copies: Copies::Most,
                never_whole: false,
                a_strip: true,
            },
        },
        // The list stops at its end part of a step in, turns round there,
        // and stops again at its top.
        Case {
            name: "light: a short list scrolled past its end and back past its top".into(),
            list: List::Short,
            theme: Theme::light,
            surface: Surface {
                scale: 1.0,
                copying: true,
                peer: None,
            },
            scrolls: &[(90.0, 22), (-90.0, 22)],
            expect: Expect {
                copies: Copies::Any,
                never_whole: true,
                a_strip: false,
            },
        },
    ]
}

/// The case's window, opened in its theme and scrolled through each of its
/// scrolls in turn with `peer`'s pointer resting over the list; what each
/// scroll did.
fn scrolled(case: &Case, peer: Option<(f64, f64)>, screens: &mut Screens) -> Vec<Run> {
    let surface = Surface {
        peer,
        ..case.surface
    };
    let size = surface.size();
    let screen = screens
        .entry((size.width, size.height))
        .or_insert_with(|| Screen {
            presented: OffscreenRenderer::try_new(size.width, size.height)
                .expect("a scroll copy is checked on the GPU the toolkit renders with"),
            full: OffscreenRenderer::new(size.width, size.height),
        });
    let drive = |app: &mut tilo_app::App| {
        app.set_theme((case.theme)());
        app.init_layout();
        let theme = (case.theme)();
        let mut runs = Vec::new();
        for by in case.scrolls {
            runs.push(scroll(app, &surface, &theme, screen, &case.name, *by));
        }
        runs
    };
    match case.list {
        List::Long => drive_live_window::<Window, _>(drive),
        List::Short => drive_live_window::<ShortWindow, _>(drive),
    }
    .unwrap()
}

/// Every case presents what a full render shows ([`scroll`] holds each
/// checked frame to it), and scrolls as its row says.
#[test]
fn a_scrolled_window_presents_what_a_full_render_shows() {
    let mut screens = Screens::new();
    for case in cases() {
        let name = &case.name;
        let runs = scrolled(&case, case.surface.peer, &mut screens);
        let alone = case
            .surface
            .peer
            .map(|_| scrolled(&case, None, &mut screens));
        eprintln!("{name}: {runs:?}; without the peer {alone:?}");
        let scale = case.surface.scale;
        let window = 800.0 * 600.0 * scale * scale;
        for (run, (step, _)) in runs.iter().zip(case.scrolls) {
            let direction = if *step > 0.0 { "down" } else { "up" };
            if case.expect.never_whole {
                assert_eq!(run.whole, 0, "{name}, scrolling {direction}: {run:?}");
            }
            match case.expect.copies {
                Copies::Most => assert!(
                    run.copied_frames * 4 >= run.frames * 3,
                    "{name}: most frames scrolling {direction} copy ({run:?})"
                ),
                Copies::None => assert_eq!(
                    run.copied_frames, 0,
                    "{name}: no frame scrolling {direction} copies ({run:?})"
                ),
                Copies::Any => {}
            }
            if case.expect.a_strip {
                let per_frame = run.repainted / run.copied_frames.max(1) as f64;
                assert!(
                    per_frame < window * 0.3,
                    "{name}: a frame scrolling {direction} that copies repaints the exposed \
                     strip, its surroundings and a peer's mark, not the list ({per_frame} px, \
                     {run:?})"
                );
            }
        }
        // The mark costs its own pixels, where it is and where a copy took
        // it, not the list's.
        if let Some(alone) = alone {
            let (down, alone) = (&runs[0], &alone[0]);
            let with_peer = down.repainted / down.copied_frames.max(1) as f64;
            let without = alone.repainted / alone.copied_frames.max(1) as f64;
            assert!(
                with_peer < without + window * 0.05,
                "{name}: the peer's mark costs {} px a frame ({with_peer} with, {without} \
                 without)",
                with_peer - without
            );
        }
    }
}

/// What each step left the History bar saying.
type Statuses = Vec<(&'static str, Option<String>)>;
/// The frames repainted whole, by step, with why.
type Whole = Vec<(&'static str, Option<&'static str>)>;

/// Each of `events` given to the window, then three frames drawn as a
/// copying window draws them; what each step left the History bar saying,
/// and the frames repainted whole, with why.
fn without_edits(
    app: &mut tilo_app::App,
    surface: &Surface,
    events: Vec<(&'static str, WindowEvent)>,
) -> (Statuses, Whole) {
    let mut damage = SurfaceDamage::copying();
    let mut scene = Scene::new();
    for _ in 0..5 {
        scene.reset();
        app.surface_frame_with_damage(surface, &mut scene, &mut damage);
    }
    let _ = damage.take_copying();
    let mut statuses = Vec::new();
    let mut whole = Vec::new();
    for (what, event) in events {
        app.surface_input(surface, event, false);
        for _ in 0..3 {
            scene.reset();
            app.surface_frame_with_damage(surface, &mut scene, &mut damage);
            if damage.take_copying().is_none() {
                whole.push((what, damage.repainted()));
            }
        }
        statuses.push((
            what,
            app.document_sync_status().map(std::borrow::Cow::into_owned),
        ));
    }
    (statuses, whole)
}

#[test]
fn scrolling_selecting_and_hovering_change_no_history_chrome_or_repaint() {
    let at = |x: f64, y: f64| PhysicalPosition::new(x, y);
    let moved = |x: f64, y: f64| WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: at(x, y),
    };
    let button = |state: ElementState| WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    };
    let wheel = |dy: f64| WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(at(0.0, dy)),
        phase: TouchPhase::Moved,
    };
    let (before, (statuses, whole)) = drive_live_window::<Window, _>(|app| {
        app.set_theme(Theme::dark());
        app.init_layout();
        let surface = Surface {
            scale: 1.0,
            copying: true,
            peer: None,
        };
        let before = app.document_sync_status().map(std::borrow::Cow::into_owned);
        let seen = without_edits(
            app,
            &surface,
            vec![
                ("hover", moved(300.0, 400.0)),
                ("hover", moved(320.0, 440.0)),
                ("scroll", wheel(-40.0)),
                ("scroll", wheel(-40.0)),
                ("scroll", wheel(40.0)),
                ("select", moved(40.0, 400.0)),
                ("select", button(ElementState::Pressed)),
                ("select", moved(200.0, 400.0)),
                ("select", button(ElementState::Released)),
                ("hover", moved(300.0, 300.0)),
            ],
        );
        (before, seen)
    })
    .unwrap();
    assert_eq!(before, None, "an untouched document says nothing");
    for (what, status) in &statuses {
        assert_eq!(
            status, &before,
            "{what} edits nothing, so History must not change ({statuses:?})"
        );
    }
    assert!(
        whole.is_empty(),
        "input that edits nothing repaints only what it changed, never the window \
         whole ({whole:?})"
    );
}

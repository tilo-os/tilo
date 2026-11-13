//! The article list offers its scrolls as copies: presented over the last
//! frame (copies, then damage) as a window that copies presents it, every
//! frame equals a full render, in dark, light and high contrast, at 100 %
//! and 200 %, with the pointer resting over the rows (whose hover follows
//! the pointer, not the rows); any other change to the rows repaints the
//! list.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test scenes and pixel tallies: failures are the assertion"
)]

use std::rc::Rc;

use super::*;
use crate::app::FeedsApp;
use ui::app::{App, AppSurface, SurfaceDamage};
use ui::offscreen::OffscreenRenderer;
use ui::widget::Column;
use winit::event::{MouseScrollDelta, TouchPhase};

const EXTRA: i64 = 300;

/// The window, in logical pixels: as tall as the overview column of a
/// window on a laptop.
const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 800.0;

struct Window {
    scale: f64,
}

impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        self.scale
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(
            (WIDTH * self.scale).round() as u32,
            (HEIGHT * self.scale).round() as u32,
        )
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: winit::window::CursorIcon) {}
}

/// How many pixels of the window `rects` cover, a pixel under several of
/// them once: a cell counts when the first rect over it marks it, so only the
/// cells under the rects are read, not the window's.
fn covered(rects: &[Rect], size: winit::dpi::PhysicalSize<u32>) -> f64 {
    let window = Rect::new(0.0, 0.0, f64::from(size.width), f64::from(size.height));
    let width = size.width as usize;
    let mut cells = vec![false; width * size.height as usize];
    let mut covered = 0_usize;
    for rect in rects {
        let rect = rect.intersect(window).expand();
        for y in rect.y0 as usize..rect.y1 as usize {
            let row = &mut cells[y * width + rect.x0 as usize..y * width + rect.x1 as usize];
            covered += row.iter().filter(|cell| !**cell).count();
            row.fill(true);
        }
    }
    covered as f64
}

#[derive(Debug, Default)]
struct Run {
    frames: usize,
    copied_frames: usize,
    repainted: f64,
    whole: usize,
}

/// The sample library and [`EXTRA`] more articles, read and starred in
/// turns, one of them open.
fn library() -> (FeedsApp, Shared) {
    let app = FeedsApp::new_demo();
    let shared = app.core_shared();
    {
        let mut core = shared.borrow_mut();
        let mut store = (*core.store).clone();
        let feed = store
            .feeds
            .ids()
            .next()
            .cloned()
            .expect("the sample has feeds");
        let now = core.now;
        for index in 0..EXTRA {
            store.articles.push(
                Id::from_raw(format!("copy-test-article-{index}")),
                Article {
                    feed_id: feed.clone(),
                    title: format!("Article {index}: the quick brown fox jumps over the lazy dog"),
                    summary: format!("Summary {index}"),
                    published: now - 3_600 * (index + 1),
                    read: index % 3 == 0,
                    starred: index % 7 == 0,
                    ..Article::default()
                },
            );
        }
        core.store = Rc::new(store);
        core.filter = Filter::All;
        core.selected = Some(Id::from_raw("copy-test-article-4"));
    }
    (app, shared)
}

/// What a window at one scale presents its frames on, and what draws the
/// full render each of them is compared with. A window keeps its own while
/// its theme changes, so the windows of one size use one pair in turn: a
/// window's first frame is drawn whole over what the one before it left.
struct Targets {
    scale: f64,
    presented: OffscreenRenderer,
    full: OffscreenRenderer,
}

impl Targets {
    fn new(scale: f64) -> Self {
        let size = Window { scale }.inner_size();
        Self {
            scale,
            presented: OffscreenRenderer::try_new(size.width, size.height)
                .expect("a scroll's copies are checked on the GPU the toolkit renders with"),
            full: OffscreenRenderer::new(size.width, size.height),
        }
    }
}

/// The window, the library and how it presents each frame.
struct Host<'t> {
    window: Window,
    app: App,
    shared: Shared,
    // Keeps the window's other panes alive beside the list.
    _feeds: FeedsApp,
    damage: SurfaceDamage,
    scene: Scene,
    targets: &'t mut Targets,
    background: vello::peniko::Color,
}

impl<'t> Host<'t> {
    /// The list in `theme` at the scale of `targets`, the pointer resting
    /// over the rows when `hover`.
    fn new(theme: &Theme, hover: bool, targets: &'t mut Targets) -> Self {
        let scale = targets.scale;
        let window = Window { scale };
        let (feeds, shared) = library();
        let list = ListPane::new(&shared);
        let mut app = App::new(
            ui::State::new(),
            Box::new(Column::new().with_no_padding().push(Box::new(list))),
        );
        app.set_theme(theme.clone());
        let mut host = Self {
            window,
            app,
            shared,
            _feeds: feeds,
            damage: SurfaceDamage::copying(),
            scene: Scene::new(),
            targets,
            background: theme.tokens.colors.surfaces.content,
        };
        // Nothing an earlier window presented on these targets shows through
        // this one: its first frame replaces them whole.
        let mut first = Run::default();
        host.frame(&mut first);
        assert_eq!(
            (first.whole, first.copied_frames),
            (1, 0),
            "a window's first frame is drawn whole"
        );
        for _ in 0..2 {
            host.frame(&mut Run::default());
        }
        if !hover {
            return host;
        }
        // The pointer rests over the rows: the row under it lifts.
        host.app.surface_input(
            &host.window,
            ui::synthetic_input::cursor_moved(200.0 * scale, 210.0 * scale),
            false,
        );
        for _ in 0..3 {
            host.frame(&mut Run::default());
        }
        host
    }

    fn frame(&mut self, run: &mut Run) {
        self.scene.reset();
        self.app
            .surface_frame_with_damage(&self.window, &mut self.scene, &mut self.damage);
        run.frames += 1;
        if let Some((copies, rects)) = self.damage.take_copying() {
            if !copies.is_empty() {
                run.copied_frames += 1;
                run.repainted += covered(&rects, self.window.inner_size());
            }
            let _ = self.targets.presented.render_copies_and_damage_gpu_wait(
                &self.scene,
                self.background,
                &copies,
                &rects,
            );
        } else {
            run.whole += 1;
            let _ = self.targets.presented.render(&self.scene, self.background);
        }
    }

    fn wheel(&mut self, step: f64) {
        self.app.surface_input(
            &self.window,
            WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
                    0.0,
                    -step * self.window.scale,
                )),
                phase: TouchPhase::Moved,
            },
            false,
        );
    }

    fn assert_matches(&mut self, label: &str, run: &Run) {
        let expected = self.targets.full.render(&self.scene, self.background);
        let actual = self.targets.presented.readback_current();
        let differing = OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected);
        assert_eq!(
            differing, 0,
            "{label}: presented {differing} pixels a full render does not have ({run:?})"
        );
    }

    /// `frames` frames of `step` logical pixels each, every one checked
    /// against a full render.
    fn scroll(&mut self, label: &str, (step, frames): (f64, usize)) -> Run {
        let mut run = Run::default();
        for frame in 0..frames {
            self.wheel(step);
            self.frame(&mut run);
            self.assert_matches(&format!("{label}, frame {frame}"), &run);
        }
        run
    }
}

#[test]
fn scrolling_the_article_list_copies_and_matches_a_full_render() {
    let mut at_100 = Targets::new(1.0);
    let mut at_200 = Targets::new(2.0);
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high contrast", Theme::high_contrast()),
    ] {
        for (doubled, hover) in [(false, true), (true, true), (false, false)] {
            let targets = if doubled { &mut at_200 } else { &mut at_100 };
            let scale = targets.scale;
            let mut host = Host::new(&theme, hover, targets);
            let window = WIDTH * HEIGHT * scale * scale;
            // The row under a resting pointer lifts, and that lift follows
            // the pointer as the rows move: where it was and is repaints.
            let budget = if hover { 0.35 } else { 0.25 };
            for (direction, step) in [("down", (40.0, 12)), ("up", (-24.0, 8))] {
                let label = format!("feeds {name} {scale}x hover={hover} {direction}");
                let run = host.scroll(&label, step);
                let per_frame = run.repainted / run.copied_frames.max(1) as f64;
                eprintln!(
                    "feeds-scroll-copy {name} {scale}x hover={hover} {direction}: frames={} copied={} \
                     whole={} repainted_px_per_copied_frame={per_frame:.0} of {window:.0} \
                     (without copies: the whole list)",
                    run.frames, run.copied_frames, run.whole
                );
                assert!(
                    run.copied_frames * 4 >= run.frames * 3,
                    "{label}: most frames copy ({run:?})"
                );
                assert!(
                    per_frame < window * budget,
                    "{label}: a frame that copies repaints the exposed strip, the hovered row, \
                     the fades and the bar, not the list ({per_frame:.0} of {window:.0} px)"
                );
            }
        }
    }
}

#[test]
fn a_frame_that_changes_the_rows_repaints_the_list_and_matches() {
    let mut targets = Targets::new(1.0);
    let mut host = Host::new(&Theme::dark(), true, &mut targets);
    let _ = host.scroll("feeds, warm up", (40.0, 2));
    let shown = host.shared.borrow().list.rows[2].id.clone();
    let star = |host: &mut Host<'_>| {
        let mut core = host.shared.borrow_mut();
        let mut store = (*core.store).clone();
        let article = store.articles.get_mut(&shown).unwrap();
        article.starred = !article.starred;
        core.store = Rc::new(store);
    };
    // A star on a row on screen, with a scroll.
    star(&mut host);
    let mut run = Run::default();
    host.wheel(40.0);
    host.frame(&mut run);
    host.assert_matches("feeds, a scroll and a star", &run);
    assert_eq!(run.copied_frames, 0, "a changed row repaints ({run:?})");
    // A star alone.
    star(&mut host);
    let mut run = Run::default();
    host.frame(&mut run);
    host.assert_matches("feeds, a star", &run);
    // A frame that changes nothing repaints nothing.
    let mut run = Run::default();
    host.frame(&mut run);
    host.assert_matches("feeds, idle", &run);
    // And scrolling copies again afterwards.
    let run = host.scroll("feeds, after", (40.0, 4));
    assert!(run.copied_frames >= 3, "{run:?}");
}

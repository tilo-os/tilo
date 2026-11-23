//! A list builds and lays out only the rows near its view, however long it
//! is: through the host's real frame path ([`App::surface_frame_with_damage`])
//! a window holding a tree of thousands of rows, as an issue tracker's list
//! is, builds a screenful of them on its first frame, keeps a screenful as it
//! scrolls, and reaches its last row from the keyboard without building the
//! rows it passed.
//!
//! The counts are asserted; the times are printed for reading
//! (`cargo test -p tilo-ui --lib long_list -- --nocapture`), never asserted:
//! on a shared machine they vary with the load, the counts do not.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::arithmetic_side_effects,
    reason = "benchmark fixtures and tallies: failures are the assertion"
)]

use super::*;
use crate::app::{AppSurface, SurfaceDamage};
use crate::widget::{Column, ListRow, TreeMarks, VecListModel, VirtualList};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;
use winit::event::{MouseScrollDelta, TouchPhase};
use winit::keyboard::NamedKey;

const WIDTH: f64 = 1240.0;
const HEIGHT: f64 = 780.0;

struct Window;

impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(WIDTH as u32, HEIGHT as u32)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: winit::window::CursorIcon) {}
}

#[derive(Clone)]
struct Item {
    key: u64,
    parent: Option<u64>,
    label: String,
}

/// `count` rows as a tracker's do: every tenth a parent, the nine
/// after it its sub-issues.
fn items(count: u64) -> Vec<Item> {
    (1..=count)
        .map(|key| Item {
            key,
            parent: (key % 10 != 1).then(|| key - (key - 1) % 10),
            label: format!("ISS-{key}  Issue number {key} of the long list"),
        })
        .collect()
}

/// What the list built, row by row.
#[derive(Default)]
struct Built {
    rows: Cell<usize>,
    keys: RefCell<BTreeSet<u64>>,
}

/// How the list stands in its window.
#[derive(Clone, Copy, Debug)]
enum Stance {
    /// Filling what its column leaves it, scrolling its own rows, as the
    /// Tasks window's list does.
    Filling,
    /// As tall as its rows, in a page that scrolls, as a list in a pane of
    /// settings or under a heading does.
    InPage,
}

/// A window whose one column holds a button's worth of header and the list.
fn window(count: u64, built: &Rc<Built>, stance: Stance) -> Box<dyn Widget> {
    let model = VecListModel::new(items(count), |item| item.key)
        .unwrap()
        .with_parents(|item| item.parent);
    let marks = Rc::new(TreeMarks::default());
    let depths = Rc::clone(&marks);
    let counted = Rc::clone(built);
    let list = VirtualList::new(
        model,
        crate::tokens::Metrics::BASE.panel_row_height,
        move |model, index, key| {
            counted.rows.set(counted.rows.get() + 1);
            counted.keys.borrow_mut().insert(*key);
            let label = model.item(index).map_or("", |item| item.label.as_str());
            Box::new(
                ListRow::new(format!("key:{key}"), label)
                    .depth(depths.depth(key))
                    .value("Todo"),
            ) as Box<dyn Widget>
        },
    )
    .with_id("long-list")
    .with_accessible_name("Issues")
    .with_tree(marks)
    .with_semantic_key(ToString::to_string)
    .selection_follows_focus();
    let header = Box::new(crate::widget::Text::new("New issue"));
    match stance {
        Stance::Filling => Box::new(
            Column::new()
                .with_rhythm(crate::tokens::Rhythm::Group)
                .with_fill_remaining_height()
                .push(header)
                .push(Box::new(list.filling())),
        ),
        Stance::InPage => Box::new(
            crate::widget::Scrollable::new_vertical().with_child(Box::new(
                Column::new()
                    .with_rhythm(crate::tokens::Rhythm::Group)
                    .with_intrinsic_height()
                    .push(header)
                    .push(Box::new(list.fitting_rows())),
            )),
        ),
    }
}

/// The most rows a frame of this window may build: before its first layout
/// a list holds the rows of a tall screen
/// ([`crate::virtual_viewport::UNPLACED_VIEWPORT`]), then the rows its own
/// height shows, with a margin either side; a glide to the end passes a few
/// screens' worth. A list that builds rows by its length rather than by its
/// height passes this at a few hundred rows.
fn screenful() -> usize {
    let row = f64::from(crate::tokens::Metrics::BASE.panel_row_height);
    let rows = |height: f64| (height / row).ceil() as usize;
    rows(crate::virtual_viewport::UNPLACED_VIEWPORT) + rows(HEIGHT) * 3 + 16
}

fn resident_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
}

fn wheel(dy: f64) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, dy)),
        phase: TouchPhase::Moved,
    }
}

struct Run {
    first_frame_ms: f64,
    first_built: usize,
    scroll_ms: Vec<f64>,
    scroll_built: usize,
    end_built: usize,
    last_reached: bool,
    resident_kib: Option<u64>,
}

fn run(count: u64, stance: Stance) -> Run {
    let before = resident_kib();
    let built = Rc::new(Built::default());
    let mut app = App::new_with_controlled_inputs(State::new(), window(count, &built, stance));
    app.set_theme(Theme::dark());
    let mut damage = SurfaceDamage::copying().drawing_only_damage();
    let mut scene = Scene::new();

    let started = std::time::Instant::now();
    app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
    let first_frame_ms = started.elapsed().as_secs_f64() * 1000.0;
    let first_built = built.rows.get();
    for _ in 0..3 {
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
    }

    // Two seconds of a two-finger scroll down, one wheel event a frame.
    app.surface_input(
        &Window,
        crate::synthetic_input::cursor_moved(WIDTH / 2.0, HEIGHT / 2.0),
        false,
    );
    let mut scroll_ms = Vec::new();
    let mut scroll_built = 0;
    for _ in 0..120 {
        let built_before = built.rows.get();
        let started = std::time::Instant::now();
        app.surface_input(&Window, wheel(-60.0), false);
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
        scroll_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        scroll_built = scroll_built.max(built.rows.get() - built_before);
    }

    // A click on a row takes the keyboard; End goes to the last row.
    app.surface_input(&Window, crate::synthetic_input::mouse_press(), false);
    app.surface_input(&Window, crate::synthetic_input::mouse_release(), false);
    scene.reset();
    app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
    let built_before = built.rows.get();
    app.surface_input(
        &Window,
        crate::synthetic_input::named_key(NamedKey::End),
        false,
    );
    for _ in 0..4 {
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
    }
    let end_built = built.rows.get() - built_before;
    let last_reached = built.keys.borrow().contains(&count);
    let resident = resident_kib()
        .zip(before)
        .map(|(after, before)| after.saturating_sub(before));
    Run {
        first_frame_ms,
        first_built,
        scroll_ms,
        scroll_built,
        end_built,
        last_reached,
        resident_kib: resident,
    }
}

fn report(count: u64, stance: Stance, run: &Run) {
    let mut sorted = run.scroll_ms.clone();
    sorted.sort_by(f64::total_cmp);
    let at = |share: f64| sorted[((sorted.len() - 1) as f64 * share) as usize];
    println!(
        "{count:>6} rows {stance:?}: first frame {:.1} ms ({} rows built), scroll p50 {:.2} ms p95 {:.2} ms \
         max {:.2} ms (at most {} rows built a frame), End built {} rows, resident +{} KiB",
        run.first_frame_ms,
        run.first_built,
        at(0.5),
        at(0.95),
        at(1.0),
        run.scroll_built,
        run.end_built,
        run.resident_kib
            .map_or_else(|| "?".to_owned(), |kib| kib.to_string()),
    );
}

#[test]
fn a_long_list_builds_only_the_rows_near_its_view() {
    crate::i18n::I18nManager::init("en-US");
    let limit = screenful();
    for stance in [Stance::Filling, Stance::InPage] {
        let short = run(2_500, stance);
        report(2_500, stance, &short);
        let long = run(25_000, stance);
        report(25_000, stance, &long);
        // Ten times the rows is no more rows built.
        assert!(
            long.first_built <= short.first_built + 4,
            "the first frame of 25,000 rows ({stance:?}) built {}, of 2,500 rows {}",
            long.first_built,
            short.first_built
        );
        assert!(
            long.scroll_built <= short.scroll_built + 4,
            "a scroll frame of 25,000 rows ({stance:?}) built {}, of 2,500 rows {}",
            long.scroll_built,
            short.scroll_built
        );
        let (count, run) = (25_000, long);
        assert!(
            run.first_built <= limit,
            "the first frame of {count} rows ({stance:?}) built {} of them, more than the {limit} \
             a window this tall shows",
            run.first_built
        );
        assert!(
            run.scroll_built <= limit,
            "a scroll frame over {count} rows ({stance:?}) built {} of them",
            run.scroll_built
        );
        if matches!(stance, Stance::Filling) {
            assert!(run.last_reached, "End shows the 25,000th row");
            assert!(
                run.end_built <= limit,
                "going to the end built {} rows, more than the {limit} the window shows",
                run.end_built
            );
        }
    }
}

#[test]
fn a_page_scrolled_to_its_end_shows_the_last_of_its_lists_rows_building_only_those() {
    crate::i18n::I18nManager::init("en-US");
    let limit = screenful();
    let built = Rc::new(Built::default());
    let mut app =
        App::new_with_controlled_inputs(State::new(), window(2_500, &built, Stance::InPage));
    app.set_theme(Theme::dark());
    let mut damage = SurfaceDamage::copying().drawing_only_damage();
    let mut scene = Scene::new();
    for _ in 0..3 {
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
    }
    app.surface_input(
        &Window,
        crate::synthetic_input::cursor_moved(WIDTH / 2.0, HEIGHT / 2.0),
        false,
    );
    // Far more than the page's height in one long fling of the wheel.
    let row = f64::from(crate::tokens::Metrics::BASE.panel_row_height);
    for _ in 0..4 {
        let built_before = built.rows.get();
        app.surface_input(&Window, wheel(-row * 2_600.0), false);
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
        let frame = built.rows.get() - built_before;
        assert!(
            frame <= limit,
            "a frame of the page built {frame} rows, more than the {limit} the window shows"
        );
    }
    assert!(
        built.keys.borrow().contains(&2_500),
        "the page's end shows the list's last row"
    );
}

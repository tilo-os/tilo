//! What a frame costs that only an animation asked for, through the native
//! runner's frame passes (model update, sync, layout, draw): a frame whose
//! animation only repaints is drawn without a relayout or a republication of
//! the action surface; one whose animation moves what is laid out is laid
//! out again.
//!
//! `cargo test -p tilo-ui --lib animation_frame -- --nocapture` prints what
//! each frame cost, per stage. Times are for reading; the counts are
//! asserted.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::arithmetic_side_effects,
    reason = "benchmark fixture: failures are the assertion"
)]

use super::*;
use crate::collaboration::{CollabEvent, CollabRequest, CollaborationManager};
use crate::time::testing::with_monotonic_time;
use crate::widget::{Button, Column, Row, Scrollable, Spinner, Text};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
/// A frame every 1/60 s.
const FRAME_NS: u64 = 16_666_667;
/// Rows of ordinary controls around the animating widget.
const ROWS: usize = 120;

struct Window;
impl crate::app::AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(WIDTH, HEIGHT)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: winit::window::CursorIcon) {}
}

fn wheel(dy: f64, phase: winit::event::TouchPhase) -> winit::event::WindowEvent {
    winit::event::WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
            0.0, dy,
        )),
        phase,
    }
}

/// A connected session, so publication does its real work.
struct Connected;
impl crate::collaboration::PeerTransport for Connected {
    fn is_connected(&self) -> bool {
        true
    }
    fn send(&mut self, _: CollabRequest) -> Result<(), String> {
        Ok(())
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        Vec::new()
    }
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Ok(())
    }
}

/// What one frame did.
#[derive(Debug, Default)]
struct FrameCost {
    total_us: u64,
    update_us: u64,
    layout_us: u64,
    draw_us: u64,
    /// Layout passes that placed or solved the tree.
    laid_out: bool,
    /// Action surface publications attempted.
    publications: usize,
    /// Every probed span of the frame, in microseconds.
    spans: Vec<(&'static str, u64)>,
}

fn micros(since: std::time::Instant) -> u64 {
    since.elapsed().as_micros() as u64
}

/// One frame as the native runner draws it after its animation pump asked
/// for one: model update and sync, structure, layout, draw.
fn native_frame(app: &mut App) -> FrameCost {
    crate::profiling::frame_probe::enable();
    let publications = app.actions.action_surface_publication_attempts;
    let started = std::time::Instant::now();
    app.update_native_model();
    app.sync_model(false);
    let update_us = micros(started);
    app.layout.structure_dirty |= app.take_root_structure_dirty();
    let rebuilt = app.layout.structure_dirty;
    if rebuilt {
        app.rebuild_structure();
    }
    let root = app.layout.root_node.unwrap();
    let layout = std::time::Instant::now();
    assert!(run_layout_pass(app, root, WIDTH, HEIGHT, 1.0, rebuilt, None).is_some());
    let layout_us = micros(layout);
    let draw = std::time::Instant::now();
    let mut scene = vello::Scene::new();
    draw_frame_content(app, &mut scene, WIDTH, HEIGHT, 1.0, None);
    complete_native_frame_content(app, |_| {});
    let draw_us = micros(draw);
    let total_us = micros(started);
    let spans: Vec<_> = crate::profiling::frame_probe::take()
        .into_iter()
        .map(|(label, (nanos, _))| (label, (nanos / 1000) as u64))
        .collect();
    FrameCost {
        total_us,
        update_us,
        layout_us,
        draw_us,
        laid_out: spans
            .iter()
            .any(|(label, _)| *label == "layout.compute" || *label == "layout.place"),
        publications: app.actions.action_surface_publication_attempts - publications,
        spans,
    }
}

fn rows(column: Column) -> Column {
    (0..ROWS).fold(column, |column, i| {
        column.push(Box::new(
            Row::new()
                .push(Box::new(Text::new(&format!("Row {i}: an ordinary line"))))
                .push(Box::new(Button::new(&format!("row-{i}"), "Open"))),
        ))
    })
}

/// `root`, shown in a connected window and settled.
fn shown(root: Box<dyn Widget>) -> App {
    crate::i18n::I18nManager::init("en-US");
    let mut app = App::new_with_controlled_inputs(State::new(), root);
    app.collaboration.manager = CollaborationManager::with_peer_transport(Connected);
    crate::app::framework_history_tests::layout(&mut app);
    for _ in 0..3 {
        native_frame(&mut app);
    }
    app
}

fn report(name: &str, frames: &[FrameCost]) {
    let median = |of: &dyn Fn(&FrameCost) -> u64| {
        let mut values: Vec<u64> = frames.iter().map(of).collect();
        values.sort_unstable();
        values[values.len() / 2]
    };
    let mut spans: std::collections::BTreeMap<&str, Vec<u64>> = Default::default();
    for frame in frames {
        for (label, us) in &frame.spans {
            spans.entry(label).or_default().push(*us);
        }
    }
    let spans = spans
        .into_iter()
        .map(|(label, mut us)| {
            us.sort_unstable();
            format!("{label}={}", us[us.len() / 2])
        })
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!(
        "animation-frame {name}: frames={} total={}us update={}us layout={}us draw={}us \
         laid_out={} published={} spans[{spans}]",
        frames.len(),
        median(&|f| f.total_us),
        median(&|f| f.update_us),
        median(&|f| f.layout_us),
        median(&|f| f.draw_us),
        frames.iter().filter(|f| f.laid_out).count(),
        frames.iter().map(|f| f.publications).sum::<usize>(),
    );
}

/// A spinner among a window of controls: its frames repaint the spinner and
/// lay nothing out, so neither the layout nor the action surface runs again.
#[test]
fn animation_frame_that_only_repaints_skips_layout_and_publication() {
    let root = rows(Column::new().push(Box::new(Spinner::new())));
    let mut app = shown(Box::new(root));
    assert!(app.root_widget.needs_animation_frame());
    let start = crate::time::now();
    let mut frames = Vec::new();
    let mut repainted = 0;
    for tick in 1..=60 {
        let at = start + std::time::Duration::from_nanos(tick * FRAME_NS);
        frames.push(with_monotonic_time(at, || {
            repainted += usize::from(app.root_widget.needs_redraw());
            native_frame(&mut app)
        }));
    }
    report("spinner", &frames);
    assert!(repainted >= 25, "the spinner steps 30 times a second");
    assert_eq!(frames.iter().filter(|f| f.laid_out).count(), 0);
    assert_eq!(frames.iter().map(|f| f.publications).sum::<usize>(), 0);
}

/// A scroll glide moves what its view lays out: each of its frames places
/// the content again, and the glide still publishes nothing.
#[test]
fn animation_frame_that_moves_layout_lays_out_again() {
    let offset = std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0)));
    let root = Scrollable::new_vertical()
        .with_persisted_offset(std::rc::Rc::clone(&offset))
        .with_child(Box::new(rows(Column::new().with_intrinsic_height())));
    let mut app = shown(Box::new(root));
    let start = crate::time::now();
    let at = |tick: u64| start + std::time::Duration::from_nanos(tick * FRAME_NS);
    app.surface_input(
        &Window,
        crate::synthetic_input::cursor_moved(400.0, 400.0),
        false,
    );
    // Flick: fingers move, then lift, and the glide carries on.
    for tick in 1..=4 {
        with_monotonic_time(at(tick), || {
            app.surface_input(
                &Window,
                wheel(-60.0, winit::event::TouchPhase::Moved),
                false,
            );
            native_frame(&mut app);
        });
    }
    with_monotonic_time(at(5), || {
        app.surface_input(&Window, wheel(0.0, winit::event::TouchPhase::Ended), false);
        native_frame(&mut app);
    });
    assert!(app.root_widget.needs_animation_frame(), "the glide runs");
    let mut frames = Vec::new();
    let mut moved = 0;
    for tick in 6..=20 {
        let before = offset.get();
        frames.push(with_monotonic_time(at(tick), || native_frame(&mut app)));
        moved += usize::from(offset.get() != before);
    }
    report("glide", &frames);
    assert!(moved >= 10, "the glide moves the content");
    assert!(
        frames.iter().filter(|f| f.laid_out).count() >= moved,
        "every frame the glide moves is laid out"
    );
    assert_eq!(frames.iter().map(|f| f.publications).sum::<usize>(), 0);
}

/// What an animation frame leaves out is only what the animation did not
/// change: a model change landing while a spinner turns (a worker's wake
/// asking for a sync) is published in the frame that syncs it.
#[test]
fn model_change_during_an_animation_is_published() {
    let root = rows(Column::new().push(Box::new(Spinner::new())));
    let mut app = shown(Box::new(root));
    let start = crate::time::now();
    let at = |tick: u64| start + std::time::Duration::from_nanos(tick * FRAME_NS);
    let quiet = with_monotonic_time(at(1), || native_frame(&mut app));
    assert_eq!(quiet.publications, 0);
    app.needs_sync = true;
    let synced = with_monotonic_time(at(2), || native_frame(&mut app));
    assert_eq!(synced.publications, 1, "the sync's frame publishes");
    let after = with_monotonic_time(at(3), || native_frame(&mut app));
    assert_eq!(after.publications, 0, "and only that frame");
}

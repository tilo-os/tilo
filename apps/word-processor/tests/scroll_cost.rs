//! What a frame costs the live window as the document grows: scrolling,
//! typing in the middle and moving the caret through documents of 50, 500
//! and 5000 blocks, each step charged one host frame on the UI thread.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "statistics over a few hundred frames and blocks"
)]

use std::time::Duration;

use tilo_document::action::{RegionKind, RichTextAction};
use tilo_document::{ClipAttr, ClipBlock};
use ui::app::{App, AppSurface, SurfaceDamage};
use ui::tilo_resource::Invoke;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use word_processor::framework::{Document, WordProcessorApp};

const SIZE: (u32, u32) = (1280, 800);

struct Window;

impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(SIZE.0, SIZE.1)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: winit::window::CursorIcon) {}
}

fn clip(kind: &str, text: String) -> ClipBlock {
    ClipBlock {
        kind: kind.into(),
        text,
        marks: Vec::new(),
        level: None,
        ordered: None,
        align: None,
        attrs: std::collections::BTreeMap::new(),
    }
}

/// A document of `blocks` blocks: headings, paragraphs of a few lines,
/// bulleted and numbered list items, a 3×3 table and a missing image now
/// and then.
pub(crate) fn document(blocks: usize) -> Vec<ClipBlock> {
    let mut out = Vec::with_capacity(blocks);
    let mut table = 0;
    while out.len() < blocks {
        let n = out.len();
        if n % 97 == 40 && blocks - n >= 9 {
            table += 1;
            for cell in 0..9_i64 {
                let mut block = clip("cell", format!("Cell {cell} of table {table}"));
                block
                    .attrs
                    .insert("table".into(), ClipAttr::Str(format!("t{table}")));
                block.attrs.insert("row".into(), ClipAttr::Int(cell / 3));
                block.attrs.insert("col".into(), ClipAttr::Int(cell % 3));
                out.push(block);
            }
            continue;
        }
        let block = match n % 23 {
            0 => {
                let mut heading = clip("heading", format!("Section {n}"));
                heading.level = Some(if n % 69 == 0 { 1 } else { 2 });
                heading
            }
            5..=7 => {
                let mut item = clip("list_item", format!("Item {n}: a point worth listing"));
                item.ordered = Some(n % 2 == 0);
                item
            }
            15 if n % 3 == 0 => {
                let mut image = clip("image", String::new());
                image.attrs.insert("width".into(), ClipAttr::Int(320));
                image.attrs.insert("height".into(), ClipAttr::Int(180));
                image
            }
            _ => clip(
                "paragraph",
                format!(
                    "Paragraph {n}. {}",
                    "The rehearsal ran long and the doors opened late, so the first half \
                     went out to a room still finding its seats. "
                        .repeat(1 + n % 3)
                ),
            ),
        };
        out.push(block);
    }
    out
}

/// The editor's own diagnostic view of itself.
pub(crate) fn observe(app: &App) -> serde_json::Value {
    app.controlled_native_input_snapshot_for_test()
        .expect("the editor publishes its input snapshot")
}

/// Paste `blocks` into the open record's first paragraph through the
/// editor's own action, as a paste does.
fn fill(app: &mut App, blocks: Vec<ClipBlock>) {
    let caret = observe(app)["caret_block"]
        .as_str()
        .expect("the empty record has a paragraph")
        .to_owned();
    let paste = RichTextAction::PasteFragment {
        region: RegionKind::Body,
        start_block: caret.clone(),
        start: 0,
        end_block: caret,
        end: 0,
        blocks,
        adopt_first_style: false,
    };
    let invoke = Invoke::<Document>::edit(&word_processor::model::edit(&paste)).expect("encodes");
    app.invoke_local_resource_action(&invoke.address(), &invoke.arguments)
        .expect("the paste is allowed");
}

/// A host: the window's damage, as a copying host keeps it, and the scene.
pub(crate) struct Host {
    damage: SurfaceDamage,
    scene: vello::Scene,
}

/// One step: its UI-thread CPU time, and whether it repainted the window
/// whole or drew anything.
#[derive(Clone, Copy)]
pub(crate) struct Step {
    pub(crate) cpu_ms: f64,
    drew: bool,
    whole: bool,
}

impl Host {
    pub(crate) fn new() -> Self {
        Self {
            damage: SurfaceDamage::copying(),
            scene: vello::Scene::new(),
        }
    }

    /// One host frame.
    pub(crate) fn frame(&mut self, app: &mut App) -> (bool, bool) {
        self.scene.reset();
        app.surface_frame_with_damage(&Window, &mut self.scene, &mut self.damage);
        let window = f64::from(SIZE.0) * f64::from(SIZE.1);
        match self.damage.take_copying() {
            Some((moves, rects)) => {
                let painted: f64 = rects.iter().map(vello::kurbo::Rect::area).sum();
                (
                    !moves.is_empty() || !rects.is_empty(),
                    painted >= 0.6 * window,
                )
            }
            None => (true, true),
        }
    }

    /// `events`, then one frame, on the UI thread's clock.
    pub(crate) fn step(&mut self, app: &mut App, events: Vec<WindowEvent>) -> Step {
        let before = ui::rt::thread_cpu_time();
        for event in events {
            app.surface_input(&Window, event, false);
        }
        let (drew, whole) = self.frame(app);
        let cpu_ms = match (before, ui::rt::thread_cpu_time()) {
            (Some(a), Some(b)) => b.saturating_sub(a).as_secs_f64() * 1000.0,
            _ => 0.0,
        };
        Step {
            cpu_ms,
            drew,
            whole,
        }
    }

    /// Frames until the window is at rest.
    pub(crate) fn settle(&mut self, app: &mut App) {
        for _ in 0..240 {
            self.frame(app);
            if !app.unsettled() {
                break;
            }
        }
    }
}

pub(crate) fn wheel(dy: f64, phase: TouchPhase) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, dy)),
        phase,
    }
}

fn press(button: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: button,
        button: MouseButton::Left,
    }
}

pub(crate) const AT: (f64, f64) = (640.0, 450.0);

/// The live window on a document of `blocks` blocks, settled, the pointer
/// over the page.
pub(crate) fn opened(app: &mut App, host: &mut Host, blocks: usize) {
    app.init_layout();
    host.settle(app);
    fill(app, document(blocks));
    host.settle(app);
    app.surface_input(
        &Window,
        ui::synthetic_input::cursor_moved(AT.0, AT.1),
        false,
    );
    host.settle(app);
    let observed = observe(app);
    assert!(
        observed["body_block_count"].as_u64() >= Some(blocks as u64),
        "the document holds its blocks: {observed}"
    );
}

/// Scroll to the middle of the document and click into the text there.
pub(crate) fn caret_in_the_middle(app: &mut App, host: &mut Host) {
    let max = observe(app)["max_scroll"].as_f64().unwrap_or(0.0);
    host.step(app, vec![wheel(-max / 2.0, TouchPhase::Moved)]);
    host.settle(app);
    host.step(app, vec![press(ElementState::Pressed)]);
    host.step(app, vec![press(ElementState::Released)]);
    host.settle(app);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollInput {
    /// A mouse wheel notch every fourth frame.
    Wheel,
    /// A touchpad flick, released to glide.
    Fling,
    /// A finger dragging the page up, lifting, and again.
    TouchDrag,
}

fn finger(
    fingers: &mut ui::interaction::TouchPointerEmulator,
    phase: TouchPhase,
    y: f64,
) -> Vec<WindowEvent> {
    let contact = winit::event::Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: winit::dpi::PhysicalPosition::new(AT.0, y),
        force: None,
        id: 0,
    };
    fingers
        .events(&contact, |_| false)
        .into_iter()
        .map(|event| event.window_event(winit::event::DeviceId::dummy()))
        .collect()
}

fn scroll_events(
    input: ScrollInput,
    frame: usize,
    fingers: &mut ui::interaction::TouchPointerEmulator,
) -> Vec<WindowEvent> {
    let line = |phase| WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: MouseScrollDelta::LineDelta(0.0, -1.0),
        phase,
    };
    match input {
        ScrollInput::Wheel => {
            if frame.is_multiple_of(4) {
                vec![line(TouchPhase::Moved)]
            } else {
                Vec::new()
            }
        }
        ScrollInput::Fling => match frame % 60 {
            0 => vec![wheel(-36.0, TouchPhase::Started)],
            1..=9 => vec![wheel(-36.0, TouchPhase::Moved)],
            10 => vec![wheel(0.0, TouchPhase::Ended)],
            _ => Vec::new(),
        },
        ScrollInput::TouchDrag => {
            let step = frame % 60;
            let y = AT.1 + 200.0 - 20.0 * step.min(20) as f64;
            match step {
                0 => finger(fingers, TouchPhase::Started, y),
                1..=19 => finger(fingers, TouchPhase::Moved, y),
                20 => finger(fingers, TouchPhase::Ended, y),
                _ => Vec::new(),
            }
        }
    }
}

fn percentile(values: &mut [f64], p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return 0.0;
    }
    let index = ((values.len() - 1) as f64 * p).round() as usize;
    values[index]
}

fn quantiles(mut values: Vec<f64>) -> String {
    let p50 = percentile(&mut values, 0.5);
    let p95 = percentile(&mut values, 0.95);
    let max = values.last().copied().unwrap_or(0.0);
    format!("p50 {p50:6.2} p95 {p95:6.2} max {max:6.2}")
}

/// Pin this thread to two of the CPUs it may run on, as a weak machine has.
fn two_cores() {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return;
    };
    let Some(list) = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
    else {
        return;
    };
    let mut cpus = Vec::new();
    for part in list.trim().split(',') {
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
                cpus.extend(a..=b);
            }
            None => cpus.push(part.parse().unwrap()),
        }
    }
    if cpus.len() < 2 {
        return;
    }
    let status = std::process::Command::new("taskset")
        .args([
            "-a",
            "-p",
            "-c",
            &format!("{},{}", cpus[0], cpus[1]),
            &std::process::id().to_string(),
        ])
        .status();
    eprintln!("pinned to cpus {},{}: {status:?}", cpus[0], cpus[1]);
}

/// Measurement: ten seconds of scrolling by wheel, fling and touch drag,
/// typing in the middle and moving the caret, through documents of 50, 500
/// and 5000 blocks.
#[test]
#[ignore = "measurement"]
fn measure_frame_cost_by_document_length() {
    measure();
}

/// The same, on two cores.
#[test]
#[ignore = "measurement"]
fn measure_frame_cost_on_two_cores() {
    two_cores();
    measure();
}

/// The scopes counted while `steps` ran in the live window on a document
/// of `blocks` blocks, `prepare` having run first.
fn counted(
    blocks: usize,
    prepare: impl FnOnce(&mut App, &mut Host),
    steps: impl FnOnce(&mut App, &mut Host),
) -> Vec<ui::tilo_resource::profiling::Sample> {
    let mut counted = None;
    ui::framework::testing::drive_live_window::<WordProcessorApp, _>(|app| {
        let mut host = Host::new();
        opened(app, &mut host, blocks);
        prepare(app, &mut host);
        let ((), phases) = ui::tilo_resource::profiling::observe(|| steps(app, &mut host));
        counted = Some(phases);
    })
    .expect("the window opens");
    counted.expect("the window ran")
}

fn calls(phases: &[ui::tilo_resource::profiling::Sample], phase: &str) -> usize {
    phases
        .iter()
        .filter(|sample| sample.phase == phase)
        .map(|sample| sample.calls)
        .sum()
}

fn typing_in_the_middle(blocks: usize) -> Vec<ui::tilo_resource::profiling::Sample> {
    counted(
        blocks,
        |app, host| {
            caret_in_the_middle(app, host);
            host.step(app, vec![ui::synthetic_input::char_key("x")]);
        },
        |app, host| {
            for _ in 0..8 {
                host.step(app, vec![ui::synthetic_input::char_key("x")]);
            }
        },
    )
}

#[test]
fn typing_publishes_nothing_again_however_long_the_document() {
    let short = typing_in_the_middle(600);
    let long = typing_in_the_middle(5000);
    for typed in [&short, &long] {
        assert_eq!(calls(typed, "framework.resource_head"), 0);
        assert_eq!(calls(typed, "framework.resource_tree"), 0);
    }
    for phase in ["ui.bound_discovery_miss", "ui.filtered_resource"] {
        assert_eq!(calls(&long, phase), calls(&short, phase), "{phase}");
    }
}

#[test]
fn reading_more_while_scrolling_publishes_nothing_again() {
    let scrolled = counted(5000, caret_in_the_middle, |app, host| {
        let mut fingers = ui::interaction::TouchPointerEmulator::default();
        for frame in 0..160 {
            host.step(app, scroll_events(ScrollInput::Wheel, frame, &mut fingers));
        }
    });
    assert!(calls(&scrolled, "rich_text.load.materialize") > 0);
    assert_eq!(calls(&scrolled, "framework.resource_head"), 0);
    assert_eq!(calls(&scrolled, "framework.resource_tree"), 0);
}

#[test]
fn moving_the_caret_lists_no_paragraphs() {
    let moved = counted(5000, caret_in_the_middle, |app, host| {
        for key in [
            NamedKey::ArrowDown,
            NamedKey::ArrowRight,
            NamedKey::ArrowUp,
            NamedKey::ArrowLeft,
        ]
        .into_iter()
        .cycle()
        .take(40)
        {
            key_steps(app, host, key);
        }
    });
    assert_eq!(calls(&moved, "word_processor.region_listed"), 0);
}

fn tables(app: &App) -> usize {
    app.collect_resources()
        .iter()
        .flat_map(|resource| &resource.children)
        .filter(|child| child.kind == "word-processor.document.table")
        .count()
}

#[test]
fn a_new_table_is_published() {
    let (before, after) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
    let inserted = counted(
        600,
        |app, _| before.set(tables(app)),
        |app, host| {
            let caret = observe(app)["caret_block"]
                .as_str()
                .expect("the caret is in a paragraph")
                .to_owned();
            let invoke = Document::INSERT_TABLE.with((caret, 2_usize, 2_usize));
            app.invoke_local_resource_action(&invoke.address(), &invoke.arguments)
                .expect("the table is inserted");
            host.settle(app);
            after.set(tables(app));
        },
    );
    assert!(calls(&inserted, "framework.resource_tree") > 0);
    assert_eq!(after.get(), before.get() + 1);
}

fn print_phases(label: &str, steps: usize, phases: Vec<ui::tilo_resource::profiling::Sample>) {
    use std::fmt::Write as _;
    let mut line = format!("  phases {label}:");
    for sample in phases {
        let _ = write!(
            line,
            " {}={:.2}ms/{:.2}x",
            sample.phase,
            sample.elapsed_ns as f64 / 1e6 / steps as f64,
            sample.calls as f64 / steps as f64
        );
    }
    eprintln!("{line}");
}

fn key_steps(app: &mut App, host: &mut Host, key: NamedKey) -> f64 {
    let events = vec![
        ui::synthetic_input::key_input(&Key::Named(key), ElementState::Pressed),
        ui::synthetic_input::key_input(&Key::Named(key), ElementState::Released),
    ];
    host.step(app, events).cpu_ms
}

fn measure() {
    const FRAMES: usize = 600;
    const EARLY: usize = 18;
    ui::i18n::I18nManager::init("en-US");
    for blocks in [50, 500, 5000] {
        ui::framework::testing::drive_live_window::<WordProcessorApp, _>(|app| {
            let mut host = Host::new();
            opened(app, &mut host, blocks);
            for input in [
                ScrollInput::Wheel,
                ScrollInput::Fling,
                ScrollInput::TouchDrag,
            ] {
                host.step(app, vec![wheel(1.0e9, TouchPhase::Moved)]);
                host.settle(app);
                let mut fingers = ui::interaction::TouchPointerEmulator::default();
                let period = Duration::from_micros(16_667);
                let (steps, phases) = ui::tilo_resource::profiling::observe(|| {
                    let mut next = ui::time::now();
                    let mut steps = Vec::with_capacity(FRAMES);
                    for frame in 0..FRAMES {
                        next += period;
                        steps.push(host.step(app, scroll_events(input, frame, &mut fingers)));
                        let now = ui::time::now();
                        if next > now {
                            std::thread::sleep(next - now);
                        } else {
                            next = now;
                        }
                    }
                    steps
                });
                let drawn = |steps: &[Step]| -> Vec<f64> {
                    steps.iter().filter(|s| s.drew).map(|s| s.cpu_ms).collect()
                };
                let (early, late) = steps.split_at(EARLY);
                eprintln!(
                    "WP {blocks:5} blocks {input:?}: drew {}, whole {}, scrolled to {:.0}\n  first 0.3 s  {}\n  after 0.3 s  {}",
                    steps.iter().filter(|s| s.drew).count(),
                    steps.iter().filter(|s| s.whole).count(),
                    observe(app)["offset_y"].as_f64().unwrap_or(0.0),
                    quantiles(drawn(early)),
                    quantiles(drawn(late)),
                );
                print_phases("scroll", FRAMES, phases);
            }
            caret_in_the_middle(app, &mut host);
            let (typed, phases) = ui::tilo_resource::profiling::observe(|| {
                (0..120)
                    .map(|_| {
                        host.step(app, vec![ui::synthetic_input::char_key("x")])
                            .cpu_ms
                    })
                    .collect::<Vec<f64>>()
            });
            print_phases("typing", 120, phases);
            let (moved, phases) = ui::tilo_resource::profiling::observe(|| {
                (0..120)
                    .map(|i| {
                        let key = if i % 2 == 0 {
                            NamedKey::ArrowDown
                        } else {
                            NamedKey::ArrowRight
                        };
                        key_steps(app, &mut host, key)
                    })
                    .collect::<Vec<f64>>()
            });
            print_phases("caret", 120, phases);
            eprintln!(
                "WP {blocks:5} blocks typing  {}\nWP {blocks:5} blocks caret   {}\n  caret at {}",
                quantiles(typed),
                quantiles(moved),
                observe(app)["caret_block"]
            );
        })
        .expect("the window opens");
    }
}

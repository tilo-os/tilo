#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "statistics over a few hundred frames and blocks"
)]

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
                    "The rehearsal ran long and the venue opened late, so the first half \
                     went out to a workspace still finding its seats. "
                        .repeat(1 + n % 3)
                ),
            ),
        };
        out.push(block);
    }
    out
}

pub(crate) fn observe(app: &App) -> serde_json::Value {
    app.controlled_native_input_snapshot_for_test()
        .expect("the editor publishes its input snapshot")
}

fn fill(app: &mut App, blocks: Vec<ClipBlock>) {
    let caret = observe(app)["caret_block"]
        .as_str()
        .expect("the empty resource has a paragraph")
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

pub(crate) struct Host {
    damage: SurfaceDamage,
    scene: vello::Scene,
}

#[derive(Clone, Copy)]
pub(crate) struct Step {
    pub(crate) cpu_ms: f64,
}

impl Host {
    pub(crate) fn new() -> Self {
        Self {
            damage: SurfaceDamage::copying(),
            scene: vello::Scene::new(),
        }
    }

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

    pub(crate) fn step(&mut self, app: &mut App, events: Vec<WindowEvent>) -> Step {
        let before = ui::rt::thread_cpu_time();
        for event in events {
            app.surface_input(&Window, event, false);
        }
        self.frame(app);
        let cpu_ms = match (before, ui::rt::thread_cpu_time()) {
            (Some(a), Some(b)) => b.saturating_sub(a).as_secs_f64() * 1000.0,
            _ => 0.0,
        };
        Step { cpu_ms }
    }

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

pub(crate) fn caret_in_the_middle(app: &mut App, host: &mut Host) {
    let max = observe(app)["max_scroll"].as_f64().unwrap_or(0.0);
    host.step(app, vec![wheel(-max / 2.0, TouchPhase::Moved)]);
    host.settle(app);
    host.step(app, vec![press(ElementState::Pressed)]);
    host.step(app, vec![press(ElementState::Released)]);
    host.settle(app);
}

/// A wheel notch every fourth frame, as a person scrolling a long document sends.
fn scroll_events(frame: usize) -> Vec<WindowEvent> {
    if frame.is_multiple_of(4) {
        vec![WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, -1.0),
            phase: TouchPhase::Moved,
        }]
    } else {
        Vec::new()
    }
}

fn key_steps(app: &mut App, host: &mut Host, key: NamedKey) -> f64 {
    let events = vec![
        ui::synthetic_input::key_input(&Key::Named(key), ElementState::Pressed),
        ui::synthetic_input::key_input(&Key::Named(key), ElementState::Released),
    ];
    host.step(app, events).cpu_ms
}

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

/// One use of the open document: `warm_up` once the caret stands in the middle of it, then
/// `steps`, which are what is counted.
struct Case {
    name: &'static str,
    warm_up: fn(&mut App, &mut Host),
    steps: fn(&mut App, &mut Host),
}

/// What the steps of each of `cases` counted, one case after another in one window on one
/// document of `blocks` blocks: writing the document is most of what a window on it costs, and
/// none of these counts it. The first case meets the window as [`counted`] with
/// [`caret_in_the_middle`] hands it over. For each later one the page goes back to its top and
/// the same wheel and click place the caret again, and the case runs only if the editor then
/// reports what it reported to the first: the same scroll offset and range, the same loaded
/// paragraphs, the same caret and the same text under it. So a case that changes the document
/// goes last. The click lands where the last case's did, so it waits out what is left of the
/// multi-click interval: within it the press would be the second of a double click and select
/// a word, and whether it is would depend on how fast the case before it ran.
fn counted_in_turn<const N: usize>(
    blocks: usize,
    cases: [Case; N],
) -> [Vec<ui::tilo_resource::profiling::Sample>; N] {
    let mut counted = None;
    ui::framework::testing::drive_live_window::<WordProcessorApp, _>(|app| {
        let mut host = Host::new();
        opened(app, &mut host, blocks);
        let mut first_start = None;
        let mut clicked = None;
        counted = Some(cases.map(|case| {
            if let Some(clicked) = clicked {
                host.step(app, vec![wheel(1.0e9, TouchPhase::Moved)]);
                host.settle(app);
                let since = ui::time::now().duration_since(clicked);
                std::thread::sleep(ui::gesture::multi_click_interval().saturating_sub(since));
            }
            caret_in_the_middle(app, &mut host);
            clicked = Some(ui::time::now());
            let start = observe(app);
            let first = first_start.get_or_insert_with(|| start.clone());
            assert_eq!(
                start, *first,
                "{} starts where the first case started",
                case.name
            );
            (case.warm_up)(app, &mut host);
            let ((), phases) =
                ui::tilo_resource::profiling::observe(|| (case.steps)(app, &mut host));
            phases
        }));
    })
    .expect("the window opens");
    counted.expect("the window ran")
}

fn typed_letter(app: &mut App, host: &mut Host) {
    host.step(app, vec![ui::synthetic_input::char_key("x")]);
}

fn eight_typed_letters(app: &mut App, host: &mut Host) {
    for _ in 0..8 {
        typed_letter(app, host);
    }
}

fn typing_in_the_middle(blocks: usize) -> Vec<ui::tilo_resource::profiling::Sample> {
    counted(
        blocks,
        |app, host| {
            caret_in_the_middle(app, host);
            typed_letter(app, host);
        },
        eight_typed_letters,
    )
}

fn forty_caret_moves(app: &mut App, host: &mut Host) {
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
}

fn a_hundred_and_sixty_wheel_frames(app: &mut App, host: &mut Host) {
    for frame in 0..160 {
        host.step(app, scroll_events(frame));
    }
}

#[test]
fn a_long_document_is_neither_published_nor_listed_again_while_it_is_used() {
    let short = typing_in_the_middle(600);
    let [moved, scrolled, long] = counted_in_turn(
        5000,
        [
            Case {
                name: "moving the caret",
                warm_up: |_, _| {},
                steps: forty_caret_moves,
            },
            Case {
                name: "scrolling",
                warm_up: |_, _| {},
                steps: a_hundred_and_sixty_wheel_frames,
            },
            Case {
                name: "typing",
                warm_up: typed_letter,
                steps: eight_typed_letters,
            },
        ],
    );
    assert_eq!(
        calls(&moved, "word_processor.region_listed"),
        0,
        "moving the caret listed the paragraphs"
    );
    assert!(
        calls(&scrolled, "rich_text.load.materialize") > 0,
        "scrolling read no more of the document"
    );
    assert_eq!(calls(&scrolled, "framework.resource_head"), 0, "scrolling");
    assert_eq!(calls(&scrolled, "framework.resource_tree"), 0, "scrolling");
    for (typed, document) in [(&short, "short"), (&long, "long")] {
        assert_eq!(
            calls(typed, "framework.resource_head"),
            0,
            "typing in the {document} document"
        );
        assert_eq!(
            calls(typed, "framework.resource_tree"),
            0,
            "typing in the {document} document"
        );
    }
    for phase in ["ui.bound_discovery_miss", "ui.filtered_resource"] {
        assert_eq!(calls(&long, phase), calls(&short, phase), "{phase}");
    }
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

//! What one frame of scrolling costs, for every kind of scroller the toolkit
//! has, through the host's real input and frame path
//! ([`App::surface_input`], [`App::surface_frame_with_damage`]) as a window
//! that copies what a scroll keeps on screen presents it.
//!
//! Each frame is counted, not only timed: the text runs it shaped, the draw
//! objects it encoded, the pixels it repainted and copied, and whether it
//! repainted the window whole. Times are reported per stage (layout, the
//! walks that compare widget boxes, drawing, damage) for reading, never
//! asserted: on a shared machine they vary with the load, the counts do not.
//!
//! `cargo test -p tilo-ui --lib scroll_cost -- --nocapture` prints a table.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "benchmark fixtures and pixel tallies: failures are the assertion"
)]

use super::*;
use crate::app::{AppSurface, SurfaceDamage};
use crate::time::testing::with_monotonic_time;
use crate::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use crate::widget::{
    CodeEditor, Column, ScrollRegion, Scrollable, Text, TreeNode, TreeView, VecListModel,
    VirtualList,
};
use std::collections::HashMap;
use vello::kurbo::Rect;
use winit::event::{MouseScrollDelta, TouchPhase};

/// The window every scroller fills, in logical pixels.
const WIDTH: f64 = 1280.0;
const HEIGHT: f64 = 800.0;

/// One frame of a two-finger scroll at 60 Hz, in logical pixels.
const STEP: f64 = 40.0;
/// Scroll frames measured per scroller.
const FRAMES: usize = 60;
/// The display the glide is drawn for: a frame every 1/120 s.
const FRAME_NS: u64 = 8_333_333;

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

/// What one scroll frame did.
#[derive(Clone, Debug, Default)]
struct FrameCost {
    /// The wheel event and the frame, on this thread, in microseconds.
    cpu_us: u64,
    /// The frame's stages, in microseconds.
    layout_us: u64,
    boxes_us: u64,
    draw_us: u64,
    damage_us: u64,
    /// Text runs shaped anew (shape-cache misses).
    shaped: u64,
    /// Draw objects and glyph runs in the frame's scene.
    draws: usize,
    glyph_runs: usize,
    /// Pixels repainted, and pixels copied from where the last frame had
    /// them.
    repainted: f64,
    copied: f64,
    /// Why the frame repainted the window whole, if it did.
    whole: Option<&'static str>,
    /// Where the frame's last rebuild no widget describes was asked for.
    undescribed_by: Option<&'static std::panic::Location<'static>>,
    /// Every probed span of the input and the frame, in microseconds.
    spans: Vec<(&'static str, u64)>,
    /// Where the frame repainted and what it copied, for the report.
    damage: String,
}

/// A scroller's frames, summarised.
#[derive(Debug, Default)]
struct Costs {
    frames: Vec<FrameCost>,
}

fn percentile(mut values: Vec<f64>, percent: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let rank = ((percent / 100.0) * values.len() as f64).ceil() as usize;
    values[rank.clamp(1, values.len()) - 1]
}

impl Costs {
    fn p50(&self, of: impl Fn(&FrameCost) -> f64) -> f64 {
        percentile(self.frames.iter().map(&of).collect(), 50.0)
    }

    fn max(&self, of: impl Fn(&FrameCost) -> f64) -> f64 {
        self.frames.iter().map(of).fold(0.0, f64::max)
    }

    /// The probed spans whose median is worth reading, slowest first.
    fn spans(&self) -> String {
        let mut labels: Vec<&'static str> = self
            .frames
            .iter()
            .flat_map(|frame| frame.spans.iter().map(|(label, _)| *label))
            .collect();
        labels.sort_unstable();
        labels.dedup();
        let mut medians: Vec<(&'static str, f64)> = labels
            .into_iter()
            .map(|label| {
                let median = self.p50(|frame| {
                    frame
                        .spans
                        .iter()
                        .find(|(at, _)| *at == label)
                        .map_or(0.0, |(_, us)| *us as f64)
                });
                (label, median)
            })
            .filter(|(_, median)| *median >= 20.0)
            .collect();
        medians.sort_by(|a, b| b.1.total_cmp(&a.1));
        medians
            .iter()
            .map(|(label, median)| format!("{label} {median:.0}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn whole(&self) -> (usize, Option<String>) {
        let whole: Vec<_> = self
            .frames
            .iter()
            .filter_map(|f| {
                f.whole.map(|why| match f.undescribed_by {
                    Some(at) => format!("{why}, rebuild asked at {at}"),
                    None => why.to_owned(),
                })
            })
            .collect();
        (whole.len(), whole.first().cloned())
    }

    fn row(&self, name: &str) -> String {
        let (whole, why) = self.whole();
        format!(
            "{name:<22} {cpu:>7.0} {layout:>7.0} {boxes:>6.0} {draw:>6.0} {damage:>6.0} \
             {shaped:>5.0}/{shaped_max:<5.0} {draws:>6.0} {runs:>5.0} {repainted:>8.0}/{repainted_max:<8.0} \
             {copied:>8.0} {whole:>3}{why}",
            cpu = self.p50(|f| f.cpu_us as f64),
            layout = self.p50(|f| f.layout_us as f64),
            boxes = self.p50(|f| f.boxes_us as f64),
            draw = self.p50(|f| f.draw_us as f64),
            damage = self.p50(|f| f.damage_us as f64),
            shaped = self.p50(|f| f.shaped as f64),
            shaped_max = self.max(|f| f.shaped as f64),
            draws = self.p50(|f| f.draws as f64),
            runs = self.p50(|f| f.glyph_runs as f64),
            repainted = self.p50(|f| f.repainted),
            repainted_max = self.max(|f| f.repainted),
            copied = self.p50(|f| f.copied),
            why = why.map(|why| format!(" ({why})")).unwrap_or_default(),
        )
    }
}

const HEADER: &str = "scroller                cpu µs layout  boxes   draw damage shaped p50/max  draws  runs  repaint px p50/max   copied whole";

/// The pixels `rects` cover in the window, each counted once.
fn covered(rects: &[Rect]) -> f64 {
    let (width, height) = (WIDTH as usize, HEIGHT as usize);
    let window = Rect::new(0.0, 0.0, WIDTH, HEIGHT);
    let mut cells = vec![false; width * height];
    for rect in rects {
        let rect = rect.intersect(window).expand();
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            continue;
        }
        for y in rect.y0 as usize..rect.y1 as usize {
            cells[y * width + rect.x0 as usize..y * width + rect.x1 as usize].fill(true);
        }
    }
    cells.iter().filter(|cell| **cell).count() as f64
}

fn wheel(dy: f64, phase: TouchPhase) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, dy)),
        phase,
    }
}

/// A host frame of `app`, counted, with the spans probed since the input
/// that led to it ([`crate::profiling::frame_probe::enable`]).
fn frame(app: &mut App, scene: &mut Scene, damage: &mut SurfaceDamage, input: u64) -> FrameCost {
    let _ = app.text_renderer.take_shape_stats();
    if !crate::profiling::frame_probe::enabled() {
        crate::profiling::frame_probe::enable();
    }
    let started = std::time::Instant::now();
    scene.reset();
    app.surface_frame_with_damage(&Window, scene, damage);
    let cpu_us = input + started.elapsed().as_micros() as u64;
    let stages = crate::profiling::frame_probe::take();
    let stage = |name: &str| stages.get(name).map_or(0, |(ns, _)| (*ns / 1000) as u64);
    let (_, shaped, _) = app.text_renderer.take_shape_stats();
    let encoding = scene.encoding();
    let mut cost = FrameCost {
        cpu_us,
        layout_us: stage("frame.layout"),
        boxes_us: stage("frame.boxes"),
        draw_us: stage("frame.draw"),
        damage_us: stage("frame.damage"),
        shaped,
        draws: encoding.draw_tags.len(),
        glyph_runs: encoding.resources.glyph_runs.len(),
        spans: stages
            .iter()
            .map(|(label, (ns, _))| (*label, (*ns / 1000) as u64))
            .collect(),
        ..FrameCost::default()
    };
    if let Some((copies, rects)) = damage.take_copying() {
        let round = |rect: &Rect| {
            format!(
                "({:.0},{:.0} {:.0}x{:.0})",
                rect.x0,
                rect.y0,
                rect.width(),
                rect.height()
            )
        };
        cost.damage = format!(
            "repaints {} copies {:?}",
            rects
                .iter()
                .take(6)
                .map(round)
                .collect::<Vec<_>>()
                .join(" "),
            copies.iter().take(2).collect::<Vec<_>>()
        );
        cost.repainted = covered(&rects);
        cost.copied = copies
            .iter()
            .map(|copy| f64::from(copy.width) * f64::from(copy.height))
            .sum::<f64>()
            + 0.0;
    } else {
        cost.undescribed_by = app.layout.undescribed_by;
        cost.whole = Some(damage.repainted().unwrap_or("unknown"));
        cost.repainted = WIDTH * HEIGHT;
    }
    cost
}

/// An app showing `root` in the window, laid out and drawn until it rests,
/// the pointer over its middle.
fn shown(root: Box<dyn Widget>) -> (App, Scene, SurfaceDamage) {
    let mut app = App::new_with_controlled_inputs(crate::state::State::new(), root);
    app.set_theme(Theme::dark());
    let mut damage = SurfaceDamage::copying();
    let mut scene = Scene::new();
    for at in 0..6 {
        if at == 2 {
            app.surface_input(
                &Window,
                crate::synthetic_input::cursor_moved(WIDTH / 2.0, HEIGHT / 2.0),
                false,
            );
        }
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
    }
    let _ = damage.take_copying();
    (app, scene, damage)
}

/// Scroll `root` down [`FRAMES`] frames of [`STEP`] under the fingers, one
/// wheel event per frame.
fn scrolled(root: Box<dyn Widget>) -> Costs {
    let (mut app, mut scene, mut damage) = shown(root);
    let mut costs = Costs::default();
    for _ in 0..FRAMES {
        crate::profiling::frame_probe::enable();
        app.layout.undescribed_by = None;
        let started = std::time::Instant::now();
        app.surface_input(&Window, wheel(-STEP, TouchPhase::Moved), false);
        let input = started.elapsed().as_micros() as u64;
        costs
            .frames
            .push(frame(&mut app, &mut scene, &mut damage, input));
    }
    costs
}

/// Flick `root` and draw the glide that follows at 120 Hz until it rests:
/// the frames after the fingers lift.
fn glided(root: Box<dyn Widget>) -> Costs {
    let (mut app, mut scene, mut damage) = shown(root);
    let start = crate::time::now();
    let at = |frame: u64| start + std::time::Duration::from_nanos(frame * FRAME_NS);
    let mut tick = 0;
    for _ in 0..8 {
        with_monotonic_time(at(tick), || {
            app.surface_input(&Window, wheel(-24.0, TouchPhase::Moved), false);
            scene.reset();
            app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
        });
        tick += 1;
    }
    with_monotonic_time(at(tick), || {
        app.surface_input(&Window, wheel(0.0, TouchPhase::Ended), false);
    });
    let _ = damage.take_copying();
    let mut costs = Costs::default();
    for _ in 0..240 {
        tick += 1;
        app.layout.undescribed_by = None;
        let cost = with_monotonic_time(at(tick), || frame(&mut app, &mut scene, &mut damage, 0));
        if cost.repainted == 0.0 && cost.copied == 0.0 {
            break;
        }
        costs.frames.push(cost);
    }
    costs
}

fn sentence(index: usize) -> String {
    format!("Row {index}: the quick brown fox jumps over the lazy dog, {index} times")
}

/// A long column of text in a `Scrollable`.
fn column(rows: usize) -> Box<dyn Widget> {
    let column = (0..rows).fold(Column::new().with_intrinsic_height(), |column, row| {
        column.push(Box::new(Text::new(&sentence(row))))
    });
    Box::new(Scrollable::new_vertical().with_child(Box::new(column)))
}

/// A `VirtualList` of `rows` rows filling the window.
fn virtual_list(rows: u32) -> Box<dyn Widget> {
    let model = VecListModel::new((0..rows).collect(), |key| *key).unwrap();
    Box::new(
        VirtualList::new(model, 32.0, |_, index, _| {
            Box::new(Text::new(&sentence(index))) as Box<dyn Widget>
        })
        .filling(),
    )
}

#[derive(Clone)]
struct Track {
    title: String,
    artist: String,
    length: String,
}

/// A `DataTable` of `rows` rows filling the window.
fn data_table(rows: usize) -> Box<dyn Widget> {
    let columns = vec![
        ColumnDef::new("title", "Title", ColumnWidth::Flex(2.0))
            .with_text(|row: &Track| row.title.clone()),
        ColumnDef::new("artist", "Artist", ColumnWidth::Flex(1.0))
            .with_text(|row: &Track| row.artist.clone()),
        ColumnDef::new("length", "Length", ColumnWidth::Flex(0.5))
            .with_text(|row: &Track| row.length.clone()),
    ];
    // The rows scroll inside the window, under the header.
    let mut table = DataTable::new(columns)
        .selectable(true)
        .with_viewport(crate::num::f32_of_f64(HEIGHT - 40.0), 36.0);
    table.set_rows(
        (0..rows)
            .map(|row| Track {
                title: sentence(row),
                artist: format!("Artist {}", row % 97),
                length: format!("{}:{:02}", 2 + row % 5, row % 60),
            })
            .collect(),
    );
    Box::new(table)
}

/// A `TreeView` of `branches` folders of `leaves` files each, every folder
/// open, filling the window.
fn tree_view(branches: u64, leaves: u64) -> Box<dyn Widget> {
    let folders = (0..branches)
        .map(|folder| {
            let id = 1 + folder * (leaves + 1);
            TreeNode::branch(
                id,
                format!("Folder {folder}"),
                (),
                (1..=leaves)
                    .map(|leaf| TreeNode::leaf(id + leaf, sentence(leaf as usize), ()))
                    .collect(),
            )
        })
        .collect();
    let ids: Vec<u64> = (0..branches)
        .map(|folder| 1 + folder * (leaves + 1))
        .collect();
    Box::new(
        TreeView::new(TreeNode::branch(0, "Root", (), folders))
            .with_expanded(std::iter::once(0).chain(ids))
            .fill(),
    )
}

/// A transcript painted line by line inside a [`ScrollRegion`], as chat,
/// a terminal's scrollback or a calculator's history paint theirs.
struct Transcript {
    region: ScrollRegion,
    rect: Rect,
    lines: usize,
    scrolled: bool,
}

const LINE: f64 = 24.0;

impl Widget for Transcript {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        crate::widget::TaffyTreeExt::fill_leaf(taffy, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.region.set_viewport(self.rect, theme);
        self.region
            .set_content_size(self.rect.width(), self.lines as f64 * LINE);
        let visible = self.region.visible_content();
        let lines = self.lines;
        self.region
            .paint(scene, transform, theme, |scene, content| {
                let first = (visible.y0 / LINE).floor().max(0.0) as usize;
                let last = ((visible.y1 / LINE).ceil() as usize).min(lines);
                for line in first..last {
                    let top = line as f64 * LINE;
                    text.draw_text(
                        scene,
                        &sentence(line),
                        14.0,
                        theme.text_primary,
                        content.pre_translate(vello::kurbo::Vec2::new(12.0, top + 4.0)),
                        None,
                    );
                }
            });
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let before = self.region.offset();
        let theme = Theme::dark();
        if self.region.handle_event(event, ctx, &theme) && self.region.offset() != before {
            self.scrolled = true;
        }
    }

    fn name(&self) -> &'static str {
        "Transcript"
    }

    fn needs_redraw(&self) -> bool {
        self.scrolled
    }

    fn needs_animation_frame(&self) -> bool {
        self.region.needs_animation_frame(&Theme::dark())
    }

    fn update(
        &mut self,
        _state: &crate::widget::AppModel,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        // A glide is a scroll, drawn and copied like one; it changes no
        // model, so it asks for no sync.
        self.scrolled |= self.region.advance();
        false
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.scrolled && !self.region.offer_copy(self, regions) {
            regions.push(self.rect);
        }
    }

    fn mark_drawn(&mut self) {
        self.region.mark_presented();
        self.scrolled = false;
    }
}

fn transcript(lines: usize) -> Box<dyn Widget> {
    Box::new(Transcript {
        region: ScrollRegion::vertical(),
        rect: Rect::ZERO,
        lines,
        scrolled: false,
    })
}

/// A code file of `lines` lines in the shared code editor.
fn code(lines: usize) -> Box<dyn Widget> {
    let source: String = (0..lines)
        .map(|line| {
            format!(
                "    let value_{line} = compute({line}, \"{}\");\n",
                sentence(line)
            )
        })
        .collect();
    let mut editor = CodeEditor::local()
        .with_language(crate::widget::Language::Rust)
        .with_max_lines(30);
    editor.set_text(&source);
    Box::new(editor)
}

type Fixture = (&'static str, fn() -> Box<dyn Widget>);

fn fixtures() -> [Fixture; 6] {
    [
        ("scrollable column 2k", || column(2_000)),
        ("virtual list 100k", || virtual_list(100_000)),
        ("data table 100k", || data_table(100_000)),
        ("tree view 10k", || tree_view(500, 20)),
        ("scroll region 10k", || transcript(10_000)),
        ("code editor 3k lines", || code(3_000)),
    ]
}

/// Every scroller, scrolled under the fingers and then gliding: the report
/// that tells where a scroll frame's work goes. Each scroller has to move
/// on every frame (it repaints or copies something), so a fixture that
/// stopped scrolling cannot report a cheap frame.
#[test]
fn scroll_cost_of_every_scroller() {
    let mut report = vec![format!(
        "under the fingers, {FRAMES} frames of {STEP} px\n{HEADER}"
    )];
    let mut glides = vec![format!("gliding after a flick, 120 Hz\n{HEADER}")];
    let mut problems = Vec::new();
    for (name, fixture) in fixtures() {
        let costs = scrolled(fixture());
        for (index, cost) in costs.frames.iter().enumerate() {
            if cost.repainted == 0.0 && cost.copied == 0.0 {
                problems.push(format!(
                    "{name}: frame {index} of the scroll changed nothing on screen"
                ));
            }
        }
        let glide = glided(fixture());
        for (moving, frames) in [("scroll", &costs.frames), ("glide", &glide.frames)] {
            for (index, cost) in frames.iter().enumerate() {
                if let Some(why) = cost.whole {
                    problems.push(format!(
                        "{name}: frame {index} of the {moving} repainted the window whole: {why} ({:?})",
                        cost.undescribed_by
                    ));
                }
            }
        }
        report.push(costs.row(name));
        report.push(format!("    {}", costs.spans()));
        if let Some(frame) = costs.frames.get(2) {
            report.push(format!("    frame 2: {}", frame.damage));
        }
        if glide.frames.is_empty() {
            problems.push(format!("{name}: a flick does not glide"));
        }
        glides.push(format!(
            "{} ({} frames)",
            glide.row(name),
            glide.frames.len()
        ));
        glides.push(format!("    {}", glide.spans()));
        if let Some(frame) = glide.frames.get(2) {
            glides.push(format!("    frame 2: {}", frame.damage));
        }
    }
    eprintln!("{}\n\n{}", report.join("\n"), glides.join("\n"));
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

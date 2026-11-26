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
//! A key typed into a long note is counted the same way: it paints its own
//! line, whatever the note's length.
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
    CodeEditor, Column, ListRow, ScrollRegion, Scrollable, Text, TreeMarks, VecListModel,
    VirtualList,
};
use std::collections::HashMap;
use std::rc::Rc;
use vello::kurbo::Rect;
use winit::event::{MouseScrollDelta, TouchPhase};

/// What a long list builds as it opens and scrolls, through the same frame path.
#[path = "long_list_tests.rs"]
mod long_list;

/// The window every scroller fills, in logical pixels.
const WIDTH: f64 = 1280.0;
const HEIGHT: f64 = 800.0;

/// One frame of a two-finger scroll at 60 Hz, in logical pixels.
const STEP: f64 = 40.0;
/// Scroll frames measured per scroller: the first frames of a scroll and
/// enough after them that the reported medians are steady.
const FRAMES: usize = 24;
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

fn wheel(dy: f64, phase: TouchPhase) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, dy)),
        phase,
    }
}

/// An app showing `root` in the window, laid out and drawn until it rests,
/// the pointer over its middle.
fn shown(root: Box<dyn Widget>) -> (App, Scene, SurfaceDamage) {
    let mut app = App::new_with_controlled_inputs(crate::state::State::new(), root);
    app.set_theme(Theme::dark());
    let mut damage = SurfaceDamage::copying().drawing_only_damage();
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
    // The tree's own name, a row per folder and its files under it.
    let mut items: Vec<(u64, String, Option<u64>, bool)> = vec![(0, "Root".to_owned(), None, true)];
    for folder in 0..branches {
        let id = 1 + folder * (leaves + 1);
        items.push((id, format!("Folder {folder}"), Some(0), true));
        for leaf in 1..=leaves {
            items.push((id + leaf, sentence(leaf as usize), Some(id), false));
        }
    }
    let model = VecListModel::new(items, |item| item.0)
        .expect("every row has its own key")
        .with_parents(|item| item.2)
        .with_branches(|item| item.3);
    let marks = Rc::new(TreeMarks::<u64>::default());
    let rows = Rc::clone(&marks);
    Box::new(
        VirtualList::with_selection_delegate(model, 24.0, move |model, index, key, _| {
            let label = model.item(index).map_or("", |item| item.1.as_str());
            Box::new(ListRow::new(key.to_string(), label).depth(rows.depth(key)))
        })
        .with_tree(marks)
        .filling(),
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

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
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

/// A glide moves the content in frames no input asked for: each frame that
/// copies the rows on screen has placed them where the copy moves them.
#[test]
fn a_glide_places_the_rows_it_copies() {
    /// The top of `row` (by its place in the column) and of the first row
    /// in view.
    fn rows(widget: &dyn Widget, at: Option<usize>, out: &mut (Option<f64>, Option<(usize, f64)>)) {
        if widget.name() == "Scrollable" {
            widget.for_each_child(&mut |column| {
                let mut index = 0;
                column.for_each_child(&mut |row| {
                    let rect = row.get_rect();
                    if Some(index) == at {
                        out.0 = Some(rect.y0);
                    }
                    if out.1.is_none() && rect.y1 > 0.0 {
                        out.1 = Some((index, rect.y0));
                    }
                    index += 1;
                });
            });
            return;
        }
        widget.for_each_child(&mut |child| rows(child, at, out));
    }
    let row_top = |app: &App, at: usize| {
        let mut out = (None, None);
        rows(app.root_widget.as_ref(), Some(at), &mut out);
        out.0.expect("the column has the row")
    };
    let first_in_view = |app: &App| {
        let mut out = (None, None);
        rows(app.root_widget.as_ref(), None, &mut out);
        out.1.expect("the column shows a row").0
    };
    let (mut app, mut scene, mut damage) = shown(column(2000));
    let start = crate::time::now();
    let at = |frame: u64| start + std::time::Duration::from_nanos(frame * FRAME_NS);
    let mut tick = 0;
    for _ in 0..8 {
        with_monotonic_time(at(tick), || {
            app.surface_input(&Window, wheel(-24.0, TouchPhase::Moved), false);
            scene.reset();
            app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
        });
        damage.take_copying();
        tick += 1;
    }
    with_monotonic_time(at(tick), || {
        app.surface_input(&Window, wheel(0.0, TouchPhase::Ended), false);
    });
    let lifted = first_in_view(&app);
    let mut glided = 0.0;
    for _ in 0..30 {
        // A row mid-view before the frame, still in view after it: the rows
        // the copy moves are the ones on screen.
        let row = first_in_view(&app) + 20;
        let top = row_top(&app, row);
        tick += 1;
        with_monotonic_time(at(tick), || {
            scene.reset();
            app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
        });
        let copied: i32 = damage
            .take_copying()
            .map(|(copies, _)| copies.iter().map(|copy| copy.dy).sum())
            .unwrap_or_default();
        let now = row_top(&app, row);
        assert_eq!(
            f64::from(copied),
            now - top,
            "the rows moved as far as the copy moved their pixels"
        );
        glided += now - top;
    }
    assert!(
        glided < 0.0 && first_in_view(&app) > lifted,
        "the flick glided on: {glided} px, from row {lifted} to {}",
        first_in_view(&app)
    );
}

/// Rows a scroll brings into a `VirtualList` are laid out where they enter,
/// without rebuilding the window's layout: every frame the list's tree
/// stays the one it was, and each row it shows stands where its index puts
/// it, the rows a copy moved as far as the copy moved their pixels.
#[test]
fn rows_entering_a_virtual_list_are_laid_out_in_place() {
    /// The top of each row the list shows, by index: the rows it placed
    /// near its box ([`Widget::for_each_child_in_clip`]); those mounted
    /// further ahead stand where they are placed once they come near.
    fn rows(widget: &dyn Widget, out: &mut Vec<(usize, f64)>) {
        if widget.name() == "Text" {
            let name = widget.accessibility().and_then(|node| node.name);
            let index = name
                .as_deref()
                .and_then(|name| name.strip_prefix("Row "))
                .and_then(|rest| rest.split(':').next())
                .and_then(|index| index.parse().ok());
            if let Some(index) = index {
                out.push((index, widget.get_rect().y0));
            }
        }
        widget.for_each_child_in_clip(&mut |child| rows(child, out));
    }
    fn list_rect(widget: &dyn Widget) -> Option<Rect> {
        if widget.name() == "VirtualList" {
            return Some(widget.get_rect());
        }
        let mut found = None;
        widget.for_each_child(&mut |child| {
            found = found.or_else(|| list_rect(child));
        });
        found
    }
    let shown_rows = |app: &App| {
        let mut out = Vec::new();
        rows(app.root_widget.as_ref(), &mut out);
        out.sort_by_key(|(index, _)| *index);
        out
    };
    let (mut app, mut scene, mut damage) = shown(virtual_list(2000));
    let rebuilds = app.layout.rebuilds;
    let mut before = shown_rows(&app);
    for _ in 0..FRAMES {
        app.surface_input(&Window, wheel(-STEP, TouchPhase::Moved), false);
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
        let copied: i32 = damage
            .take_copying()
            .map(|(copies, _)| copies.iter().map(|copy| copy.dy).sum())
            .unwrap_or_default();
        let now = shown_rows(&app);
        assert!(!now.is_empty(), "the list shows rows");
        // Every row is one line: measured, they stand one pitch apart.
        let pitch = now[1].1 - now[0].1;
        assert!(pitch > 0.0, "the rows stand down the list: {now:?}");
        let origin = now[0].1 - now[0].0 as f64 * pitch;
        for (index, top) in &now {
            assert!(
                (top - (origin + *index as f64 * pitch)).abs() < 0.01,
                "row {index} stands where its index puts it: {now:?}"
            );
        }
        let list = list_rect(app.root_widget.as_ref()).expect("the list is shown");
        assert!(now[0].1 <= list.y0, "the rows reach the top: {now:?}");
        let (last, bottom) = now[now.len() - 1];
        assert!(
            bottom + pitch >= list.y1,
            "the rows reach the bottom: {now:?}"
        );
        assert_eq!(last + 1 - now[0].0, now.len(), "the rows are contiguous");
        if copied != 0 {
            let kept = before.iter().find_map(|(index, then)| {
                now.iter()
                    .find(|(at, _)| at == index)
                    .map(|(_, top)| top - then)
            });
            assert_eq!(
                kept,
                Some(f64::from(copied)),
                "the rows moved as far as the copy moved their pixels"
            );
        }
        before = now;
    }
    assert!(
        before[0].0 > 40,
        "the scroll brought rows in: first row {}",
        before[0].0
    );
    assert_eq!(
        app.layout.rebuilds, rebuilds,
        "the rows entered without rebuilding the layout"
    );
    assert!(app.layout.remounts > 0, "they were mounted in place");
}

#[path = "draw_cull_frame_tests.rs"]
mod culled_frames;

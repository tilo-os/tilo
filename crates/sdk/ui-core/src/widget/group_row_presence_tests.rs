//! Where a reader is told a peer's caret in a window's text stands, while the strip is
//! panned: it must be where the window shows the caret.

use super::tests::{relayout_strip, strip_at};
use super::*;
use crate::headless::FrameHarness;
use crate::retained_present::{PanFrame, PanReplay};
use std::cell::Cell;
use std::rc::Rc;

struct Field {
    input: crate::widget::TextInput,
    drawn: Rc<Cell<Rect>>,
}

impl Field {
    fn new(field: &str, drawn: Rc<Cell<Rect>>) -> Self {
        let mut input =
            crate::widget::TextInput::new(vec![crate::presence::PathSegment::Field(field.into())]);
        input.set_text("the words a note holds");
        Self { input, drawn }
    }

    fn key(field: &str) -> crate::presence::PresenceKey {
        crate::presence::PresenceKey::Widget {
            path: vec![crate::presence::PathSegment::Field(field.into())],
        }
    }
}

impl crate::widget::Decorator for Field {
    type Inner = crate::widget::TextInput;

    fn inner(&self) -> &crate::widget::TextInput {
        &self.input
    }

    fn inner_mut(&mut self) -> &mut crate::widget::TextInput {
        &mut self.input
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        crate::widget::Widget::draw(&mut self.input, scene, transform, text, theme, presence);
        self.drawn.set(self.input.caret_rect(theme, 0));
    }
}

impl crate::widget::AppWidget for Field {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Editor
    }

    fn app_title(&self) -> &str {
        "Field"
    }

    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("F")
    }
}

fn window(id: u64, field: &str, drawn: Rc<Cell<Rect>>) -> WindowGroup {
    let mut group = WindowGroup::new(
        id,
        vec![(
            TabDescriptor::new(id, format!("T{id}")),
            TileContent::Stub {
                kind: crate::widget::AppKind::Editor,
                label: field.into(),
            },
        )],
        id,
        WindowWidth::Normal,
    );
    let host = group.host_for_tab_mut(id).expect("the tab has a host");
    *host = TileHost::new(TileContent::App(Box::new(Field::new(field, drawn))));
    group
}

fn strip(count: usize, field: &str, drawn: Rc<Cell<Rect>>) -> GroupRow {
    let mut row = strip_at(count, 390.0, 0.0, 1000);
    let tab = row.groups[1].active_tab_id();
    *row.groups[1]
        .host_for_tab_mut(tab)
        .expect("the tab has a host") =
        TileHost::new(TileContent::App(Box::new(Field::new(field, drawn))));
    relayout_strip(&mut row, 390.0);
    relayout_strip(&mut row, 390.0);
    row
}

fn frame(
    row: &mut GroupRow,
    text: &mut crate::text_render::TextRenderer,
    presence: &crate::presence::PresenceCtx,
    kind: PanFrame,
) {
    thread_local! {
        static FRAMES: Cell<u64> = const { Cell::new(0) };
    }
    if kind != PanFrame::Moved {
        presence.clear_caret_tags();
        presence.clear_presence_marks();
    }
    let number = FRAMES.with(|frames| {
        frames.set(frames.get().wrapping_add(1));
        frames.get()
    });
    let _frame = PanReplay::begin(kind, number);
    let mut scene = Scene::new();
    row.draw(&mut scene, Affine::IDENTITY, text, &Theme::dark(), presence);
    row.mark_drawn();
}

fn mark(presence: &crate::presence::PresenceCtx) -> Rect {
    let marks = presence.presence_marks();
    assert_eq!(marks.len(), 1, "one caret for one peer: {marks:?}");
    marks[0].rect
}

fn close(a: Rect, b: Rect) -> bool {
    (a.x0 - b.x0).abs() <= 0.5
        && (a.y0 - b.y0).abs() <= 0.5
        && (a.x1 - b.x1).abs() <= 0.5
        && (a.y1 - b.y1).abs() <= 0.5
}

#[test]
fn a_window_replayed_through_a_pan_tells_a_reader_where_its_caret_moved_to() {
    let drawn = Rc::new(Cell::new(Rect::ZERO));
    let mut row = strip(2, "second", Rc::clone(&drawn));
    let peer = crate::permissions::UserId("claude".into());
    let mut presence = crate::presence::PresenceCtx::empty();
    presence.set_display_name(peer.clone(), "Claude Code");
    presence.set_caret(&Field::key("second"), peer, 0);
    let text = &mut crate::text_render::TextRenderer::new();

    frame(&mut row, text, &presence, PanFrame::Drawn);
    let home = drawn.get();
    assert!(home.width() > 0.0, "the window drew a caret: {home:?}");
    assert!(close(mark(&presence), home), "{:?}", mark(&presence));

    row.set_scroll_x_for_test(60.0);
    relayout_strip(&mut row, 390.0);
    frame(&mut row, text, &presence, PanFrame::Replayed);
    let drawn_once = drawn.get();
    assert!(close(mark(&presence), drawn_once), "{:?}", mark(&presence));

    let before = row.groups[1].get_rect().x0;
    row.set_scroll_x_for_test(120.0);
    relayout_strip(&mut row, 390.0);
    frame(&mut row, text, &presence, PanFrame::Replayed);
    let moved_by = row.groups[1].get_rect().x0 - before;
    assert!(
        (moved_by + 60.0).abs() <= 0.5,
        "the window moved by the pan: {moved_by}"
    );
    assert_eq!(
        drawn.get(),
        drawn_once,
        "the window at rest kept its drawing through the pan"
    );
    assert!(
        row.replayed_groups.contains(&row.groups[1].id),
        "the window was shown by its drawing"
    );
    let told = mark(&presence);
    assert!(
        close(told, drawn_once + vello::kurbo::Vec2::new(moved_by, 0.0)),
        "the caret is told where the drawing shows it: {told:?}, drawn {drawn_once:?} moved by {moved_by}"
    );
}

fn mark_bounds(node: &crate::headless::InspectionNode) -> Option<Rect> {
    if node.test_id.as_deref() == Some("presence-mark") {
        return node
            .semantics
            .as_ref()
            .and_then(|semantics| semantics.bounds)
            .map(crate::a11y::AccessBounds::to_rect);
    }
    node.children.iter().find_map(mark_bounds)
}

#[test]
fn a_peers_caret_in_a_panned_window_is_a_node_where_the_window_drew_it() {
    let first = Rc::new(Cell::new(Rect::ZERO));
    let second = Rc::new(Cell::new(Rect::ZERO));
    let row = GroupRow::new(vec![
        window(100, "first", Rc::clone(&first)),
        window(200, "second", Rc::clone(&second)),
    ]);
    let mut frame = FrameHarness::for_root(crate::State::new(), row, (390, 844), Theme::default());
    let peer = crate::permissions::UserId("claude".into());
    frame
        .presence_mut()
        .set_display_name(peer.clone(), "Claude Code");
    frame
        .presence_mut()
        .set_caret(&Field::key("second"), peer, 0);
    frame.relayout();

    let offset = {
        let row = frame.root::<GroupRow>();
        row.groups[1].get_rect().x0 - row.get_rect().x0
    };
    assert!(offset > 1.0, "the second window stands beside the first");
    assert!(
        frame.root_mut::<GroupRow>().pan_to(offset),
        "the strip pans to the second window"
    );
    frame.relayout();

    let drawn = second.get();
    assert!(
        drawn.x0 >= 0.0 && drawn.x1 <= 390.0,
        "the caret is drawn on the phone's screen: {drawn:?}"
    );
    let bounds = mark_bounds(&frame.inspection().root)
        .expect("the peer's caret is a node a reader finds, where it stands");
    assert!(
        close(bounds, drawn),
        "the node is exactly the caret the window drew: {bounds:?}, drawn {drawn:?}"
    );
}

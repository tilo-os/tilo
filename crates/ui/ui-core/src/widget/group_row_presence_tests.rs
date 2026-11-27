//! Where a reader is told a peer's caret in a window's text stands, while the strip is
//! panned: it must be where the window shows the caret.

use super::tests::{relayout_strip, strip_at};
use super::*;
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
            crate::widget::TextInput::new(vec![tilo_ui_base::presence::PathSegment::Field(
                field.into(),
            )]);
        input.set_text("the words a note holds");
        Self { input, drawn }
    }

    fn key(field: &str) -> tilo_ui_base::presence::PresenceKey {
        tilo_ui_base::presence::PresenceKey::Widget {
            path: vec![tilo_ui_base::presence::PathSegment::Field(field.into())],
        }
    }
}

impl tilo_ui_base::widget::Decorator for Field {
    type Inner = crate::widget::TextInput;

    fn inner(&self) -> &crate::widget::TextInput {
        &self.input
    }

    fn inner_mut(&mut self) -> &mut crate::widget::TextInput {
        &mut self.input
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        tilo_ui_base::widget::Widget::draw(
            &mut self.input,
            &mut tilo_ui_base::draw::DrawCtx::new(scene, transform, text, theme, env.clone()),
        );
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
    text: &mut tilo_ui_base::text_render::TextRenderer,
    presence: &tilo_ui_base::presence::PresenceCtx,
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
    row.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        text,
        &Theme::dark(),
        presence.draw_env(),
    ));
    row.mark_drawn();
}

fn mark(presence: &tilo_ui_base::presence::PresenceCtx) -> Rect {
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
    let peer = tilo_member_types::MemberId::person("claude");
    let mut presence = tilo_ui_base::presence::PresenceCtx::empty();
    presence.set_display_name(peer.clone(), "Claude Code");
    presence.set_caret(&Field::key("second"), peer, 0);
    let text = &mut tilo_ui_base::text_render::TextRenderer::new();

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

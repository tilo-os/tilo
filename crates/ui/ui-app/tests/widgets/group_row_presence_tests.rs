//! A peer's caret in a panned window is a node where the window drew it.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

struct Field {
    input: tilo_ui::widget::TextInput,
    drawn: Rc<Cell<Rect>>,
}

impl Field {
    fn new(field: &str, drawn: Rc<Cell<Rect>>) -> Self {
        let mut input =
            tilo_ui::widget::TextInput::new(vec![tilo_ui_base::presence::PathSegment::Field(
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
    type Inner = tilo_ui::widget::TextInput;

    fn inner(&self) -> &tilo_ui::widget::TextInput {
        &self.input
    }

    fn inner_mut(&mut self) -> &mut tilo_ui::widget::TextInput {
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

impl tilo_ui::widget::AppWidget for Field {
    fn app_kind(&self) -> tilo_ui::widget::AppKind {
        tilo_ui::widget::AppKind::Editor
    }

    fn app_title(&self) -> &str {
        "Field"
    }

    fn app_icon(&self) -> tilo_ui::widget::IconDescriptor {
        tilo_ui::widget::IconDescriptor::Glyph("F")
    }
}

fn window(id: u64, field: &str, drawn: Rc<Cell<Rect>>) -> WindowGroup {
    let mut group = WindowGroup::new(
        id,
        vec![(
            TabDescriptor::new(id, format!("T{id}")),
            TileContent::Stub {
                kind: tilo_ui::widget::AppKind::Editor,
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

fn mark_bounds(node: &InspectionNode) -> Option<Rect> {
    if node.test_id.as_deref() == Some("presence-mark") {
        return node
            .semantics
            .as_ref()
            .and_then(|semantics| semantics.bounds)
            .map(tilo_ui_base::a11y::AccessBounds::to_rect);
    }
    node.children.iter().find_map(mark_bounds)
}

fn close(a: Rect, b: Rect) -> bool {
    (a.x0 - b.x0).abs() <= 0.5
        && (a.y0 - b.y0).abs() <= 0.5
        && (a.x1 - b.x1).abs() <= 0.5
        && (a.y1 - b.y1).abs() <= 0.5
}

#[test]
fn a_peers_caret_in_a_panned_window_is_a_node_where_the_window_drew_it() {
    let first = Rc::new(Cell::new(Rect::ZERO));
    let second = Rc::new(Cell::new(Rect::ZERO));
    let row = GroupRow::new(vec![
        window(100, "first", Rc::clone(&first)),
        window(200, "second", Rc::clone(&second)),
    ]);
    let mut frame = Driver::for_root(tilo_ui::State::new(), row, (390, 844), Theme::default());
    let peer = tilo_action_policy::MemberId::person("claude");
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

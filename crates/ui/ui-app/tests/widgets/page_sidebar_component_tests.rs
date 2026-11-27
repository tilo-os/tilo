use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::internals::page_sidebar::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::Event;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use tilo_ui_base::{component::Fired, component::Intent};
use winit::event::*;
use winit::keyboard::*;

/// A rail a view declares answers the pointer: a page pressed is the page the view is told
/// was chosen.
#[test]
fn a_page_pressed_on_a_declared_rail_is_chosen() {
    let queue = tilo_ui_base::Intents::<Fired>::default();
    let rail = DeclaredRail::from_props(
        "r",
        PageSidebarProps {
            aspect: 1.414,
            count: 3,
            current: 0,
            select: Event::View(Intent::new(&queue, "r", "select")),
            ..PageSidebarProps::default()
        },
    );
    let mut harness = tilo_ui_app::app::driver::Driver::new_with_theme(
        tilo_ui_app::State::new(),
        Box::new(rail),
        (240, 900),
        tilo_ui_base::theme::Theme::light(),
    );
    fn find(widget: &dyn tilo_ui_base::widget::Widget) -> Option<&DeclaredRail> {
        widget
            .as_any()
            .and_then(|any| any.downcast_ref::<DeclaredRail>())
            .or_else(|| widget.children().into_iter().find_map(find))
    }
    let second = find(harness.root_widget())
        .and_then(|rail| rail.rail.thumb_rects().get(1).map(|(_, at)| *at))
        .expect("the second page's thumbnail");
    harness.click_at(second.center().x, second.center().y);
    let fired: Vec<_> = queue
        .take()
        .into_iter()
        .map(|fired| (fired.event, fired.value))
        .collect();
    assert!(
        fired.contains(&("select".to_owned(), Some(Value::Int(1)))),
        "the second page is chosen: {fired:?}"
    );
}

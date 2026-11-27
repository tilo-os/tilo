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
use tilo_ui::widget::{CanvasPlace, PlaceProps};
use tilo_ui_base::a11y::*;
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
use winit::event::*;
use winit::keyboard::*;

/// Whether a menu stands open under `widget`: an open menu is among the children its area
/// shows.
fn menu_open(widget: &dyn Widget) -> bool {
    widget.name() == "ContextMenu" || widget.children().into_iter().any(menu_open)
}

/// A right-click on a place opens its resource's actions as its menu (`canvas.md` §1, "Right-click
/// keeps its menu"): the same commands its handle's menu lists.
#[test]
fn a_right_click_on_a_place_opens_its_actions() {
    let place = CanvasPlace::from_props(
        "p",
        PlaceProps {
            name: Some("Inbox".into()),
            carries: Some("tl:mail:root".into()),
            controls: None,
            body: Some(Box::new(tilo_ui_base::widget::Spacer::fixed(300.0))),
            items: vec![MenuItemProps {
                label: "Archive".into(),
                ..MenuItemProps::default()
            }],
            ..PlaceProps::default()
        },
    );
    let mut place = place;
    let mut text = tilo_ui_base::font_catalog::fixture_renderer();
    tilo_ui_base::widget::layout::lay_out_detached(
        &mut place,
        Rect::new(0.0, 0.0, 400.0, 300.0),
        &tilo_ui_base::Theme::dark(),
        &mut text,
    )
    .expect("the place lays out");
    let mut host = Driver::for_root(
        tilo_ui::State::new(),
        place,
        (400, 300),
        tilo_ui_base::Theme::dark(),
    );
    host.relayout();
    host.send(&tilo_ui_base::synthetic_input::cursor_moved(200.0, 150.0));
    host.send(&tilo_ui_base::synthetic_input::mouse_input(
        winit::event::ElementState::Pressed,
        winit::event::MouseButton::Right,
    ));
    assert!(
        menu_open(host.root_widget()),
        "the right-click opened the place's menu of its actions"
    );
}

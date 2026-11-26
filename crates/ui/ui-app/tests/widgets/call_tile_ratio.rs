//! A call tile in a window is laid out at its frame's ratio.

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

#[test]
fn a_window_with_a_tile_lays_it_out_at_its_ratio() {
    let person = Person::new(tilo_actions::MemberId::person("ef".repeat(32)), "Lin");
    let tile = CallTile::of(person, CallSource::Camera);
    let harness = Driver::new(tilo_ui::State::new(), Box::new(tile), (640, 360));
    let root = harness.root_widget();
    let rect = root.get_rect();
    assert!(rect.width() > 1.0, "the tile takes the window's width");
    assert!(
        (rect.height() - rect.width() / (16.0 / 9.0)).abs() <= 1.0,
        "a camera tile is laid out at its frame's ratio, got {}x{}",
        rect.width(),
        rect.height()
    );
}

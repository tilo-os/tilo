//! A member's avatar keeps its photo when the name it follows is edited, and
//! the editor beside it keeps its selection.

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
fn bound_name_refresh_keeps_photo_and_adjacent_input_selection() {
    use tilo_ui::widget::{Row, TextInput};
    let name = tilo_ui_base::LocalState::new("Ada".to_owned());
    let photo = tilo_identity_client::ProfilePhoto::new(1, 1, vec![200, 100, 50, 255]).unwrap();
    let avatar = Avatar::person("Ada", "ada")
        .with_name_state(&name)
        .with_profile_photo(&photo)
        .size(AvatarSize::Portrait);
    let input = TextInput::local().with_initial_value("Ada");
    let root = Row::new().push(Box::new(avatar)).push(Box::new(input));
    let mut harness = Driver::new_with_theme(
        tilo_ui::State::new(),
        Box::new(root),
        (640, 320),
        Theme::dark(),
    );
    harness
        .with_widget_mut::<TextInput, _>(|input| {
            input.set_text("Ada Lovelace");
            input.set_focus(true);
            input.set_selection(4, 8);
        })
        .unwrap();
    let original = harness
        .with_widget_mut::<Avatar, _>(|avatar| (std::ptr::from_ref(avatar), avatar.get_rect()))
        .unwrap();
    name.set("Ada Lovelace".to_owned());
    // Run the root's production update/sync/layout passes before inspecting
    // the widgets: with_widget_mut invokes its visitor before pumping a frame.
    // A name edit must not rebuild the neighbouring editor or replace its
    // presentation state.
    harness.relayout();
    harness
        .with_widget_mut::<Avatar, _>(|avatar| {
            assert_eq!(std::ptr::from_ref(avatar), original.0);
            assert_eq!(avatar.get_rect(), original.1);
            assert_eq!(avatar.name_for_tests(), "Ada Lovelace");
            assert!(avatar.has_image_for_tests());
            assert_eq!(
                Widget::accessibility(avatar).unwrap().name.as_deref(),
                Some("Ada Lovelace")
            );
        })
        .unwrap();
    harness
        .with_widget_mut::<TextInput, _>(|input| {
            assert!(input.is_focused());
            assert_eq!(input.cursor_pos(), 8);
            assert_eq!(input.selection_range_public(), Some((4, 8)));
        })
        .unwrap();
}

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
use tilo_ui_app::widget::internals::shortcut_recorder::test_support::*;
use tilo_ui_app::widget::internals::shortcut_recorder::*;
use tilo_ui_app::widget::*;
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
#[test]
fn backspace_on_a_changed_shortcut_puts_back_the_default() {
    let state = tilo_ui_base::LocalState::new("Ctrl+J".to_owned());
    let mut host = tilo_ui_app::app::driver::Driver::for_root(
        tilo_ui_app::State::new(),
        ShortcutRecorder::new("x", &state, "Ctrl+K"),
        (400, 200),
        tilo_ui_base::theme::Theme::light(),
    );
    let at = host.root::<ShortcutRecorder>().chord_rect().center();
    host.click_at(at.x, at.y);
    host.key("Escape");
    assert_eq!(
        state.get(),
        "Ctrl+J",
        "Escape leaves the shortcut as it was"
    );
    host.key("Backspace");
    assert_eq!(state.get(), "Ctrl+K");
}

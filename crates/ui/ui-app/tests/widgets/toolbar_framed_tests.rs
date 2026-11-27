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
use tilo_ui::widget::toolbar::Orientation;
use tilo_ui::widget::*;
use tilo_ui_base::a11y::Action;
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
fn held_activation_keys_do_not_repeat_admitted_toolbar_commands() {
    use std::cell::Cell;
    use std::rc::Rc;
    use tilo_ui::app::driver::Framed;
    let _settled = tilo_ui_base::motion::testing::settled();
    let press = || WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event: tilo_ui_base::synthetic_input::key_event(
            Key::Named(NamedKey::Enter),
            None,
            ElementState::Pressed,
        ),
        is_synthetic: true,
    };
    for width in [800, 100] {
        let calls = Rc::new(Cell::new(0));
        let count = Rc::clone(&calls);
        let toolbar = Toolbar::new(Orientation::Horizontal)
            .push_button("Open workspace settings", move || {
                count.set(count.get() + 1)
            });
        let mut frame = Framed::new(tilo_ui::State::new(), toolbar, (width, 480), Theme::dark());
        assert!(Framed::harness(&frame).native_app_audit().is_certified());
        if width == 100 {
            Framed::harness_mut(&mut frame)
                .perform_named("toolbar.overflow", &Action::Activate)
                .unwrap();
            Framed::harness_mut(&mut frame).poll_frame().unwrap();
        }
        Framed::harness_mut(&mut frame)
            .perform_named("toolbar.item.0", &Action::Focus)
            .unwrap();
        let mut held = press();
        if let WindowEvent::KeyboardInput { event, .. } = &mut held {
            event.repeat = true;
        }
        Framed::harness_mut(&mut frame).send_host_event(&held);
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert_eq!(
            calls.get(),
            0,
            "{width}: a held key cannot start a new command"
        );
        Framed::harness_mut(&mut frame).send_host_event(&press());
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert_eq!(
            calls.get(),
            1,
            "{width}: one deliberate key press invokes once"
        );
        for _ in 0..3 {
            Framed::harness_mut(&mut frame).send_host_event(&held);
            Framed::harness_mut(&mut frame).poll_frame().unwrap();
        }
        assert_eq!(calls.get(), 1, "{width}: repetition must not invoke again");
    }
}

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]

use super::*;
use crate::testing::dispatch;
use crate::theme::Theme;
use crate::widget::{Button, Checkbox};

fn rest() -> Interaction {
    Interaction::default()
}

tilo_test_env::scenario! {
    fn a_control_in_a_state_shows_it_and_takes_no_input;

    // Outside an `InState` a control paints its own state; inside one, the state it names, but
    // never as usable when the control itself is not.
    phase a_control_paints_the_state_in_effect;
    {
        assert_eq!(interaction(rest()), rest());
        let held = State::Pressed.interaction();
        with(held, || {
            assert!(is_shown());
            assert_eq!(interaction(rest()), held);
            let locked = interaction(Interaction {
                disabled: true,
                ..rest()
            });
            assert!(locked.disabled && locked.pressed);
        });
        assert!(!is_shown(), "the state ends with the draw");
        assert_eq!(interaction(rest()), rest());
    }

    // The control shown is drawn by the control itself, in the state named: a hovered button and
    // a focused box draw otherwise than resting ones.
    phase the_control_draws_the_state_itself;
    {
        let theme = Theme::light();
        let draw = |widget: &mut dyn Widget| {
            let mut taffy = taffy::TaffyTree::<()>::new();
            let node =
                widget.build_layout(&mut taffy, &theme, &mut std::collections::HashMap::new());
            taffy
                .compute_layout(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(240.0),
                        height: taffy::AvailableSpace::MaxContent,
                    },
                )
                .unwrap();
            widget.apply_layout(&taffy, node, vello::kurbo::Point::ZERO);
            let mut scene = vello::Scene::new();
            crate::draw::draw_widget(
                widget,
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut crate::text_render::TextRenderer::new(),
                &theme,
                crate::draw::DrawEnv::default(),
            );
            scene.encoding().draw_data.clone()
        };
        let button = || Box::new(Button::new("save", "Save")) as Element;
        let check = || Box::new(Checkbox::given(true).with_label("Send")) as Element;
        let resting = draw(&mut InState::new(State::Rest, button()));
        assert!(!resting.is_empty(), "the button draws");
        assert_ne!(resting, draw(&mut InState::new(State::Hovered, button())));
        assert_ne!(resting, draw(&mut InState::new(State::Pressed, button())));
        assert_ne!(
            draw(&mut InState::new(State::Rest, check())),
            draw(&mut InState::new(State::Focused, check())),
            "a focused box wears its ring"
        );
    }

    // Shown, never used: no press reaches it, the keyboard's order passes it by and assistive
    // technology skips it.
    phase it_takes_no_input;
    {
        let mut shown = InState::new(State::Hovered, Box::new(Checkbox::given(false).with_label("Send")));
        assert!(!Widget::is_focusable(&shown));
        assert!(Widget::accessibility_hidden(&shown));
        assert!(!dispatch(
            &mut shown,
            &crate::synthetic_input::mouse_press()
        ));
        assert!(!dispatch(
            &mut shown,
            &crate::synthetic_input::named_key(winit::keyboard::NamedKey::Space)
        ));
    }
}

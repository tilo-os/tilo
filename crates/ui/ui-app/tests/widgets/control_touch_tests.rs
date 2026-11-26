//! The controls a finger reaches: a station of a stepper, a link's line, a shortcut's keycaps — each as
//! large as the input the host has.

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
use tilo_ui::widget::{Autocomplete, Column, Link, ShortcutRecorder, Stepper};
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::InteractionEnvironment;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::widget::*;
use tilo_ui_base::{component::Bound, component::Described};
use winit::event::*;
use winit::keyboard::*;

fn finger() -> f64 {
    f64::from(InteractionEnvironment::TOUCH.minimum_target())
}

tilo_test_env::scenario! {
    /// The touch targets of the smaller controls.
    /// Each phase was a test of its own and builds its own fixture.
    fn on_touch_controls_stand_a_fingers_target_tall;

    phase on_touch_a_step_done_is_a_fingers_target_and_a_pointers_by_mouse;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let stepper = || {
            Stepper::new(vec![
                "Draft".to_owned(),
                "Review".to_owned(),
                "Send".to_owned(),
            ])
            .current(2usize)
            .on_station(|_| {})
        };
        for (input, want) in [
            (InteractionEnvironment::default(), false),
            (InteractionEnvironment::TOUCH, true),
        ] {
            let mut host = tilo_ui::app::driver::Driver::for_root(
                tilo_ui::State::new(),
                stepper(),
                (600, 200),
                Theme::light(),
            );
            host.set_interaction_environment(input);
            host.relayout_at((600, 200)).expect("the stepper lays out");
            let theme = Theme::light();
            tilo_ui_base::text_render::with_layout_renderer(&theme, |text| {
                let mut scene = vello::Scene::new();
                Widget::draw(&mut *host.root_mut::<Stepper>(), &mut tilo_ui_base::draw::DrawCtx::new(&mut scene, vello::kurbo::Affine::IDENTITY, text, &theme, tilo_ui_base::presence::PresenceCtx::default().draw_env()));
            });
            let announced = host
                .root_mut::<Stepper>()
                .accessibility()
                .expect("a stepper names its stations")
                .children
                .iter()
                .filter_map(|station| station.bounds)
                .map(tilo_ui_base::AccessBounds::to_rect)
                .collect::<Vec<_>>();
            assert_eq!(announced.len(), 2, "two steps are done: {input:?}");
            for rect in announced {
                let big = rect.width() >= finger() - 0.5 && rect.height() >= finger() - 0.5;
                assert_eq!(big, want, "{input:?}: {rect:?}");
            }
        }
    }

    phase on_touch_a_link_is_a_fingers_target_about_its_line;
    {
        let mut link = Link::new("https://tilo.sh").with_label("The site");
        Widget::set_interaction_environment(&mut link, InteractionEnvironment::TOUCH);
        let rect = vello::kurbo::Rect::new(10.0, 20.0, 210.0, 41.0);
        link.apply_layout_external(rect);
        let target = link.target();
        assert!(
            target.height() >= finger() - 0.5,
            "the link's target is a finger's tall: {target:?}"
        );
        assert!(target.contains(rect.center()));
        let bounds = link
            .accessibility()
            .and_then(|node| node.bounds)
            .map(tilo_ui_base::AccessBounds::to_rect)
            .expect("a link says where it is");
        assert_eq!(bounds, target, "assistive technology meets the same target");
    }

    phase on_touch_a_shortcut_recorder_and_an_autocompletes_field_stand_a_fingers_target_tall;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let state = tilo_ui_base::LocalState::new("Ctrl+K".to_owned());
        let recorder: Box<dyn Widget> = Box::new(ShortcutRecorder::new("open", &state, "Ctrl+K"));
        let field = Auto::field("to", "ada@example.com");
        let mut host = tilo_ui::app::driver::Driver::for_root(
            tilo_ui::State::new(),
            Column::new().push(recorder).push(field),
            (400, 300),
            Theme::light(),
        );
        host.set_interaction_environment(InteractionEnvironment::TOUCH);
        let heights: Vec<f64> = host
            .root_widget()
            .children()
            .iter()
            .map(|child| child.get_rect().height())
            .collect();
        assert_eq!(heights.len(), 2);
        for height in heights {
            assert!(height >= finger() - 0.5, "a finger's tall: {height}");
        }
    }
}

struct Auto;

impl Auto {
    fn field(key: &str, text: &str) -> Box<dyn Widget> {
        let bound = Bound {
            state: tilo_ui_base::LocalState::new(text.to_owned()),
            edits: None,
            name: key.to_owned(),
            path: Vec::new(),
            identity: None,
        };
        let props = tilo_ui::widget::AutocompleteProps {
            value: bound,
            ..tilo_ui::widget::AutocompleteProps::default()
        };
        Box::new(<Autocomplete as Described>::from_props(key, props))
    }
}

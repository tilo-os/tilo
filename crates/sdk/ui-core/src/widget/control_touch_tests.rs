//! The controls a finger reaches: a station of a stepper, a link's line, a shortcut's keycaps —
//! each as large as the input the host has.

use super::*;
use crate::component::{Bound, Described};
use crate::interaction::InteractionEnvironment;

fn finger() -> f64 {
    f64::from(InteractionEnvironment::TOUCH.minimum_target())
}

#[test]
fn on_touch_a_step_done_is_a_fingers_target_and_a_pointers_by_mouse() {
    let _settled = crate::motion::testing::settled();
    let stepper = || {
        Stepper::new(vec![
            "Draft".to_owned(),
            "Review".to_owned(),
            "Send".to_owned(),
        ])
        .current(2usize)
        .on_station_view(|_| {})
    };
    for (input, want) in [
        (InteractionEnvironment::default(), false),
        (InteractionEnvironment::TOUCH, true),
    ] {
        let mut host = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            stepper(),
            (600, 200),
            Theme::light(),
        );
        host.set_interaction_environment(input);
        host.relayout_at((600, 200)).expect("the stepper lays out");
        // Its stations are where it drew them: a frame draws it.
        let theme = Theme::light();
        crate::text_render::with_layout_renderer(theme.text_scale, |text| {
            let mut scene = vello::Scene::new();
            Widget::draw(
                &mut *host.root_mut::<Stepper>(),
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                text,
                &theme,
                &crate::presence::PresenceCtx::default(),
            );
        });
        let announced = host
            .root_mut::<Stepper>()
            .accessibility()
            .expect("a stepper names its stations")
            .children
            .iter()
            .filter_map(|station| station.bounds)
            .map(|bounds| bounds.to_rect())
            .collect::<Vec<_>>();
        assert_eq!(announced.len(), 2, "two steps are done: {input:?}");
        for rect in announced {
            let big = rect.width() >= finger() - 0.5 && rect.height() >= finger() - 0.5;
            assert_eq!(big, want, "{input:?}: {rect:?}");
        }
    }
}

#[test]
fn on_touch_a_link_is_a_fingers_target_about_its_line() {
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
        .map(|bounds| bounds.to_rect())
        .expect("a link says where it is");
    assert_eq!(bounds, target, "assistive technology meets the same target");
}

#[test]
fn on_touch_a_shortcut_recorder_and_an_autocompletes_field_stand_a_fingers_target_tall() {
    let _settled = crate::motion::testing::settled();
    let state = crate::LocalState::new("Ctrl+K".to_owned());
    let recorder: Box<dyn Widget> = Box::new(ShortcutRecorder::new("open", &state, "Ctrl+K"));
    let field = Auto::field("to", "ada@example.com");
    let mut host = crate::headless::FrameHarness::for_root(
        crate::State::new(),
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

/// An autocomplete over a bound field, as the gallery places one.
struct Auto;

impl Auto {
    fn field(key: &str, text: &str) -> Box<dyn Widget> {
        let bound = Bound {
            state: crate::LocalState::new(text.to_owned()),
            edits: None,
            name: key.to_owned(),
            path: Vec::new(),
        };
        let props = crate::widget::AutocompleteProps {
            value: bound,
            ..crate::widget::AutocompleteProps::default()
        };
        Box::new(<Autocomplete as Described>::from_props(key, props))
    }
}

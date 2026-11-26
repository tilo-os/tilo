use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use taffy::Size;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::internals::scrollable::test_support::*;
use tilo_ui::widget::internals::scrollable::*;
use tilo_ui::widget::*;
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
use tilo_ui_base::widget::MeasureConstraint;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;
#[test]
fn field_validation_focus_reveal_fits_context_or_prioritises_protected_input() {
    use tilo_ui::State;
    use tilo_ui::widget::{Field, TextInput, TextInputKind};
    use tilo_ui_base::LocalState;

    fn named<'a>(root: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
        if root.test_id() == Some(id) {
            return Some(root);
        }
        root.children()
            .into_iter()
            .find_map(|child| named(child, id))
    }
    fn correction<'a>(root: &'a dyn Widget, message: &str) -> Option<&'a dyn Widget> {
        if root.accessibility().is_some_and(|node| {
            node.name.as_deref() == Some(message)
                && node.live == Some(tilo_ui_base::a11y::Politeness::Assertive)
        }) {
            return Some(root);
        }
        root.children()
            .into_iter()
            .find_map(|child| correction(child, message))
    }
    for scale in [1.0, 2.0] {
        for oversized in [false, true] {
            let message = if oversized {
                "Use a different password. ".repeat(80)
            } else {
                "Use at least 15 characters.".to_string()
            };
            let value = LocalState::new("private sample password".to_string());
            let mut input = TextInput::from_state(&value)
                .with_id("validation-password")
                .with_kind(TextInputKind::Password)
                .with_secret(true);
            input.set_focus(true);
            let field = Field::new("Password", input)
                .with_error_live(&message, tilo_ui_base::a11y::Politeness::Assertive);
            let mut content = Column::new().with_intrinsic_height();
            for _ in 0..20 {
                content = content.push(Box::new(Text::new("Earlier form content")));
            }
            content = content.push(Box::new(field));
            let scroll = Scrollable::new_vertical()
                .fill_parent_height()
                .with_focus_reveal()
                .with_child(Box::new(content));
            let mut harness = Driver::new_with_theme(
                State::new(),
                Box::new(scroll),
                (400, 300),
                Theme::dark().with_ui_scale(scale).with_reduce_motion(true),
            );
            harness.with_widget_mut::<Scrollable, _>(|scroll| {
                let viewport = scroll.content_viewport_rect();
                let input = named(scroll, "validation-password").unwrap();
                let access = input.accessibility().unwrap();
                assert!(input.is_focused());
                assert!(access.state.invalid && access.state.protected);
                assert!(access.value.is_none(), "focus must not expose the password");
                let input = input.get_rect();
                let caption = correction(scroll, &message).unwrap().get_rect();
                let context_height = caption.y1 - input.y0;
                assert!(scroll.offset_y() > 0.0, "focus must reveal a field after earlier content");
                assert!(input.y0 >= viewport.y0 && input.y1 <= viewport.y1,
                    "actual protected input remains fully visible: {input:?}, {viewport:?}");
                if oversized {
                    assert!(context_height > viewport.height(),
                        "fixture must exercise the too-tall context fallback: {context_height}, {viewport:?}");
                    assert!(caption.y1 > viewport.y1,
                        "oversized caption cannot displace the correction input");
                } else {
                    assert!(context_height <= viewport.height(),
                        "fixture must exercise fitting shared Field context");
                    assert!(caption.y0 >= viewport.y0 && caption.y1 <= viewport.y1,
                        "entire actual correction caption is revealed with input: {caption:?}, {viewport:?}");
                }
                scroll.set_offset_y(0.0);
            }).unwrap();
            // The driver ran the production layout after that deliberate
            // scroll. Unchanged focus must not pull the person back down.
            harness
                .with_widget_mut::<Scrollable, _>(|scroll| {
                    assert_eq!(scroll.offset_y(), 0.0);
                    let input = named(scroll, "validation-password").unwrap();
                    assert!(input.is_focused());
                    assert!(
                        input.get_rect().y0 > scroll.content_viewport_rect().y1,
                        "manual scrolling really moved the focused field out of view"
                    );
                })
                .unwrap();
            harness
                .with_widget_mut::<Scrollable, _>(|scroll| {
                    assert_eq!(
                        scroll.offset_y(),
                        0.0,
                        "later layout must continue respecting manual scroll with unchanged focus"
                    );
                })
                .unwrap();
        }
    }
}

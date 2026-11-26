//! A button that hears about a finger after its node was built.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::Rect;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::*;
use tilo_ui::widget::{Button, ButtonRole};
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

tilo_test_env::scenario! {
    /// Touch targets of buttons in a bar.
    /// Each phase was a test of its own and builds its own fixture.
    fn bar_controls_reach_a_finger_without_growing_the_bar;



    phase bar_controls_a_cluster_gap_apart_keep_their_press_targets_off_each_other;
    // A bar's controls stand a cluster gap apart, so no control's press target reaches into the
    // one beside it: a mouse's face is the bar's control height and its target reaches past the
    // face into the gap, which at a cluster gap is exactly the width the face leaves; a finger's
    // face is the target itself, and the gap is air between two whole targets. The bar is as wide
    // as a phone and insets what it holds as a bar's body does, so the row has nowhere to put the
    // controls but beside each other.
    {
        use tilo_ui_base::interaction::InteractionEnvironment;
        use tilo_ui_base::tokens::Rhythm;
        for pointer in [
            InteractionEnvironment::POINTER,
            InteractionEnvironment::TOUCH,
        ] {
            let mut row = tilo_ui::widget::Row::new()
                .with_rhythm(Rhythm::Related)
                .with_padding_rhythm(Rhythm::Tight, Rhythm::Tight);
            for key in ["menu", "workspace", "bell"] {
                row = row.push(Box::new(
                    Button::icon_only(key, tilo_ui_base::icons::Icon::Bell, key).role(ButtonRole::Bar),
                ));
            }
            let mut harness =
                tilo_ui::app::driver::Driver::new(tilo_ui::State::new(), Box::new(row), (320, 60));
            harness
                .root_widget_mut()
                .set_interaction_environment(pointer);
            harness.relayout();
            let faces = harness.inspection().root.children;
            let targets: Vec<Rect> = faces
                .iter()
                .map(|face| {
                    let rect = Rect::new(
                        face.rect[0],
                        face.rect[1],
                        face.rect[0] + face.rect[2],
                        face.rect[1] + face.rect[3],
                    );
                    pointer.minimum_target_rect(rect)
                })
                .collect();
            for (index, target) in targets.iter().enumerate() {
                assert!(
                    target.x0 >= 0.0 && target.x1 <= 320.0,
                    "control {index} reaches from {} to {} in a window 320 wide",
                    target.x0,
                    target.x1
                );
                if let Some(next) = targets.get(index + 1) {
                    assert!(
                        target.x1 <= next.x0 + 0.01,
                        "control {index} reaches to {} and control {} begins at {}: their press \
                     targets share the press between them ({pointer:?})",
                        target.x1,
                        index + 1,
                        next.x0
                    );
                }
            }
        }
    }
}

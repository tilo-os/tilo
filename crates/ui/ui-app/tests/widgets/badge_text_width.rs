//! A text badge is as wide as its label at every text scale.

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
use tilo_ui_base::widget::Tone;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

#[test]
fn a_text_badge_is_as_wide_as_its_label_at_every_text_scale() {
    for scale in [1.0, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        let harness = Driver::new_with_theme(
            tilo_ui::State::new(),
            Box::new(tilo_ui::widget::Row::new().push(Box::new(
                Badge::text("Maintainer").style(ChipStyle::Tinted(Tone::Success)),
            ))),
            (600, 200),
            theme.clone(),
        );
        let rect = harness.inspection().root.children[0].rect;
        let mut text = TextRenderer::new();
        text.set_text_scale(scale);
        let (label, line) = text.measure_style("Maintainer", None, &theme.text(TextRole::Caption));
        assert!(rect[2] > f64::from(label), "{scale}x: {rect:?}");
        assert!(rect[3] >= f64::from(line), "{scale}x: {rect:?}");
    }
}

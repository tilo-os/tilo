//! A page's edges land on a device pixel at every display scale.

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
use tilo_ui::widget::arrange::{Page, Row, Stack};
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

fn boxed(size: f32) -> tilo_ui_base::widget::Element {
    Box::new(Spacer::fixed(size))
}

fn boxes(widget: &dyn Widget, out: &mut Vec<(String, Rect)>) {
    out.push((widget.name().to_owned(), widget.get_rect()));
    for child in widget.children() {
        boxes(child, out);
    }
}

#[test]
fn every_edge_lands_on_a_device_pixel_at_the_display_scale() {
    use tilo_ui::widget::Text;
    use tilo_ui_base::widget::Manual;
    use vello::kurbo::Rect;
    let content = Page::new().push(Box::new(
        Stack::stack()
            .push(boxed(10.3))
            .push(Box::new(Text::new(
                "A paragraph long enough to wrap onto a few lines in a narrow window",
            )))
            .push(Box::new(Row::row().push(boxed(7.7)).push(boxed(3.35))))
            .push(boxed(5.45)),
    ));
    let region = Manual::new(|ctx| {
        let available = ctx.available();
        ctx.place(
            0,
            Rect::new(
                3.3,
                7.1,
                f64::from(available.width) - 2.2,
                f64::from(available.height) - 1.7,
            ),
        )?;
        Ok(available)
    })
    .child(Box::new(content));
    let mut harness = tilo_ui::app::driver::Driver::new_with_theme(
        tilo_ui::State::new(),
        Box::new(region),
        (333, 517),
        Theme::light().with_text_scale(1.3),
    );
    for scale in [1.0, 1.5, 2.0, 1.25] {
        harness.set_input_scale(scale).expect("a scale");
        harness.relayout();
        let mut laid = Vec::new();
        boxes(harness.root_widget(), &mut laid);
        assert!(laid.len() > 8, "{laid:?}");
        for (name, rect) in laid {
            for edge in [rect.x0, rect.y0, rect.x1, rect.y1] {
                assert!(
                    (edge * scale - (edge * scale).round()).abs() < 1e-3,
                    "{name} {rect:?}: {edge} is no device pixel at {scale}x"
                );
            }
        }
    }
}

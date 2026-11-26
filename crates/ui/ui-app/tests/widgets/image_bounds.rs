//! An image receives layout bounds of its own: its intrinsic size at the
//! display's scale, its aspect within a box, and a fit filling its place.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::Arc;
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
fn intrinsic_images_receive_real_layout_bounds() {
    use Driver;
    use tilo_ui::widget::{Container, Row};

    for scale in [1.0, 2.0] {
        let theme = Theme::dark().with_ui_scale(scale);
        let svg = Image::from_svg(
                br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 16"><path fill="#126abc" d="M0 0H32V16H0Z"/></svg>"##,
                32,
                16,
            ).unwrap();
        let row = Row::new()
            .push(Box::new(svg))
            .push(Box::new(Image::from_path(
                tilo_ui::widget::fixture_image_path_for_tests(),
            )))
            .push(Box::new(Image::from_rgba(
                Arc::from(vec![255; 32 * 16 * 4]),
                32,
                16,
            )));
        let harness =
            Driver::new_with_theme(tilo_ui::State::new(), Box::new(row), (400, 120), theme);
        let children = harness.root_widget().children();
        for (index, (width, height)) in [(32.0, 16.0), (16.0, 16.0), (32.0, 16.0)]
            .into_iter()
            .enumerate()
        {
            let rect = children[index].get_rect();
            assert_eq!(rect.width(), width * f64::from(scale));
            assert_eq!(rect.height(), height * f64::from(scale));
        }
    }

    let image = Image::from_rgba(Arc::from(vec![255; 32 * 16 * 4]), 32, 16);
    let harness = Driver::new(
        tilo_ui::State::new(),
        Box::new(Row::new().push(Box::new(image))),
        (16, 80),
    );
    let rect = harness.root_widget().children()[0].get_rect();
    assert_eq!(rect.width(), 16.0);
    assert_eq!(
        rect.height(),
        8.0,
        "a bounded auto image keeps its aspect ratio"
    );

    let harness = Driver::new(
        tilo_ui::State::new(),
        Box::new(Row::new().push(Box::new(Image::from_path("missing-image.png")))),
        (200, 100),
    );
    let rect = harness.root_widget().children()[0].get_rect();
    assert!(
        rect.width() > 0.0 && rect.height() > 0.0,
        "decode failures remain visible"
    );

    for fit in [
        ImageFit::Contain,
        ImageFit::Cover,
        ImageFit::Fill,
        ImageFit::Actual,
    ] {
        let image = Image::from_path(tilo_ui::widget::fixture_image_path_for_tests())
            .filling()
            .with_fit(fit);
        let root = Row::new().push(Box::new(
            Container::new()
                .with_no_padding()
                .with_size(80.0, 40.0)
                .with_child(Box::new(image)),
        ));
        let harness = Driver::new(tilo_ui::State::new(), Box::new(root), (200, 100));
        let container = harness.root_widget().children()[0];
        let rect = container.children()[0].get_rect();
        assert_eq!(rect.width(), 80.0);
        assert_eq!(rect.height(), 40.0);
    }
}

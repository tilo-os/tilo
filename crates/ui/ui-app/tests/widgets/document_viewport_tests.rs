//! The document viewport's zoom, pan, transforms and scrollbars.

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

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "in a test these are the assertion"
)]

tilo_test_env::scenario! {
    /// The document viewport's zoom, pan, transforms and scrollbars.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_document_viewport_zooms_pans_and_hit_tests;

    phase a_zoomed_canvas_control_is_pressed_where_it_is_drawn;
    {
        use std::cell::Cell;
        use std::rc::Rc;

        let presses = Rc::new(Cell::new(0_u32));
        let counter = Rc::clone(&presses);
        let button = tilo_ui::widget::Button::new("canvas-button", "Inspect")
            .on_click(move || counter.set(counter.get() + 1));
        let manual = tilo_ui_base::widget::Manual::new(|cx| {
            let size = cx
                .measure_child(
                    0,
                    Size {
                        width: tilo_ui_base::widget::MeasureConstraint::Intrinsic,
                        height: tilo_ui_base::widget::MeasureConstraint::Intrinsic,
                    },
                )?
                .size;
            cx.place(
                0,
                Rect::from_origin_size(
                    (300.0, 300.0),
                    (f64::from(size.width), f64::from(size.height)),
                ),
            )?;
            Ok(Size {
                width: 600.0,
                height: 600.0,
            })
        })
        .child(Box::new(button));
        let mut viewport = DocumentViewport::new(Box::new(manual));
        viewport.set_content_size(600.0, 600.0);
        viewport.state_mut().set_zoom(2.0);
        let mut harness =
            Driver::new(tilo_ui::State::new(), Box::new(viewport), (800, 900));
        let content = canvas_rect(&harness, "canvas-button");
        let pad = f64::from(tilo_ui_base::tokens::Spacing::default().s6);
        let drawn = Point::new(
            pad + content.center().x * 2.0,
            pad + content.center().y * 2.0,
        );
        assert!(
            !content.contains(drawn),
            "the drawn place and the content rect must differ for this test to say anything"
        );
        harness.click_at(drawn.x, drawn.y);
        assert_eq!(presses.get(), 1, "the press lands on the drawn control");
        harness.click_at(content.center().x, content.center().y);
        assert_eq!(
            presses.get(),
            1,
            "a press at the control's content-space rect hits nothing on screen"
        );
    }

    phase a_toolbar_sets_the_zoom_and_the_viewport_answers_with_the_factor_it_shows;
    {
        let (content, _, _) = canvas_with_button("Inspect", 0.0, 0.0);
        let zoom = tilo_ui_base::LocalState::new(ViewportZoom::Fit);
        let mut viewport = DocumentViewport::new(content).with_zoom(&zoom);
        viewport.set_content_size(600.0, 600.0);
        let mut harness =
            Driver::new(tilo_ui::State::new(), Box::new(viewport), (800, 600));
        let _ = harness.recompose();
        let fit = match zoom.with(|shown| *shown) {
            ViewportZoom::Factor(factor) => factor,
            ViewportZoom::Fit => panic!("the viewport answers Fit with the factor it used"),
        };
        assert!(
            fit < 1.0,
            "a 600×600 content in an 800×600 frame fits at {fit}"
        );
        zoom.set(ViewportZoom::Factor(1.0));
        let _ = harness.recompose();
        assert_eq!(
            zoom.with(|shown| *shown),
            ViewportZoom::Factor(1.0),
            "the factor a toolbar asks for is kept"
        );
        zoom.set(ViewportZoom::Fit);
        let _ = harness.recompose();
        assert_eq!(
            zoom.with(|shown| *shown),
            ViewportZoom::Factor(fit),
            "Fit is answered with the factor it resolved to"
        );
        assert_eq!(ViewportZoom::Factor(1.25).percent(), Some(125.0));
        assert_eq!(ViewportZoom::Fit.percent(), None);
    }

    phase the_windows_zoom_reaches_a_canvas_that_has_one_of_its_own;
    {
        use tilo_ui::content_zoom::{ContentZoom, ZoomStep};
        let (content, _, _) = canvas_with_button("Inspect", 0.0, 0.0);
        let zoom = tilo_ui_base::LocalState::new(ViewportZoom::Fit);
        let mut viewport = DocumentViewport::new(content).with_zoom(&zoom);
        viewport.set_content_size(600.0, 600.0);
        let mut text = TextRenderer::new();
        tilo_ui_base::widget::layout::lay_out_detached(
            &mut viewport,
            Rect::new(0.0, 0.0, 800.0, 900.0),
            &Theme::dark(),
            &mut text,
        )
        .expect("the canvas lays out");
        let fitted = viewport.state().zoom();
        let factor = viewport
            .zoom_content(ZoomStep::In)
            .expect("a canvas with a scale of its own takes the window's zoom");
        assert!(
            f64::from(factor) > fitted,
            "Zoom in draws the canvas larger: {factor} from {fitted}"
        );
        let shown = zoom.with(|shown| *shown);
        assert!(
            matches!(shown, ViewportZoom::Factor(level) if (level - f64::from(factor)).abs() < 1e-6),
            "and the toolbar is told what the canvas shows: {shown:?} for {factor}"
        );
        let reset = viewport.zoom_content_for_tests(ZoomStep::Reset).expect("actual size");
        assert!(
            (f64::from(reset) - fitted).abs() < 1e-6,
            "actual size is the whole canvas fitted: {reset} vs {fitted}"
        );
    }

    phase zoom_keeps_focus_respects_bounds_and_fits;
    {
        fn zoom_at_keeps_focal_point_stationary() {
            let mut v = kernel();
            // Zoom in so both axes overflow, then mid-scroll so neither clamps.
            v.set_zoom(2.0);
            v.set_scroll(v.max_scroll_x() / 2.0, v.max_scroll_y() / 2.0);
            let focal = Point::new(400.0, 300.0);
            let before = v.screen_to_content(focal);
            v.zoom_at(1.3, focal);
            let after = v.screen_to_content(focal);
            assert!(
                (before.x - after.x).abs() < 1.0,
                "x focal drift {} too large",
                (before.x - after.x).abs()
            );
            assert!(
                (before.y - after.y).abs() < 1.0,
                "y focal drift {} too large",
                (before.y - after.y).abs()
            );
        }

        // Exact equality is intentional: `reset_to_fit` either sets `zoom` to a
        // computed value or clamps it to `zoom_bounds().0` via `f64::clamp`,
        // which returns the bound verbatim — not an independent float
        // computation prone to precision drift.
        #[allow(
            clippy::float_cmp,
            reason = "checks whether zoom was clamped to the exact min-zoom bound, not a fuzzy tolerance"
        )]
        fn reset_to_fit_shows_whole_content() {
            let mut v = kernel();
            v.set_zoom(4.0);
            v.set_scroll(100.0, 100.0);
            v.reset_to_fit();
            assert!(!v.overflows_x() || v.zoom() == v.zoom_bounds().0);
            assert!(
                v.scaled_content_height() <= v.rect().height() + 1.0
                    || v.zoom() == v.zoom_bounds().0
            );
            assert_eq!(v.scroll_x(), 0.0);
            assert_eq!(v.scroll_y(), 0.0);
        }

        zoom_at_keeps_focal_point_stationary();
        reset_to_fit_shows_whole_content();
    }

    phase pan_clamps_and_centres_content;
    {
        fn horizontal_pan_reaches_full_width_when_zoomed() {
            let mut v = kernel();
            v.set_zoom(3.0);
            assert!(v.overflows_x(), "3x zoom must overflow horizontally");
            v.pan_by(f64::MAX, 0.0);
            let expected = (v.scaled_content_width() - v.rect().width()).max(0.0);
            assert!(
                (v.scroll_x() - expected).abs() < 1.0,
                "pan should reach the full scaled width: {} vs {}",
                v.scroll_x(),
                expected
            );
            let right_edge = v.content_point_to_screen(Point::new(1000.0, 0.0));
            assert!(
                right_edge.x <= v.rect().x1 + 1.0,
                "right content edge {} should be within the viewport right {}",
                right_edge.x,
                v.rect().x1
            );
        }

        fn vertical_pan_clamps_at_both_ends() {
            let mut v = kernel();
            v.set_zoom(2.0);
            v.pan_by(0.0, -10_000.0);
            assert_eq!(v.scroll_y(), 0.0, "cannot scroll above the top");
            v.pan_by(0.0, 10_000.0);
            assert!(
                (v.scroll_y() - v.max_scroll_y()).abs() < 1e-6,
                "cannot scroll past the bottom"
            );
        }

        fn no_overflow_means_no_scroll_and_centered_x() {
            let mut v = kernel();
            v.set_zoom(0.25);
            assert!(!v.overflows_x(), "small content must not overflow x");
            assert!(!v.overflows_y(), "small content must not overflow y");
            assert_eq!(v.max_scroll_x(), 0.0);
            assert_eq!(v.max_scroll_y(), 0.0);
            let scaled_w = 1000.0 * v.zoom();
            let expected_x0 = v.rect().x0 + (v.rect().width() - scaled_w) / 2.0;
            let origin = v.content_origin();
            assert!(
                (origin.x - expected_x0).abs() < 1.0,
                "small content is centred horizontally: {} vs {}",
                origin.x,
                expected_x0
            );
        }

        fn drag_pan_maps_one_to_one_and_reports_movement() {
            let mut v = kernel();
            v.set_zoom(2.0);
            v.set_scroll(100.0, 100.0);
            v.begin_pan(Point::new(400.0, 300.0));
            v.update_pan(Point::new(401.0, 300.0));
            v.update_pan(Point::new(450.0, 360.0));
            assert!(
                (v.scroll_x() - (100.0 - 50.0)).abs() < 1e-6,
                "x tracks drag"
            );
            assert!(
                (v.scroll_y() - (100.0 - 60.0)).abs() < 1e-6,
                "y tracks drag"
            );
            assert!(v.end_pan(), "a real drag reports movement");
        }

        fn no_move_press_is_a_click_not_a_pan() {
            let mut v = kernel();
            v.set_zoom(2.0);
            v.begin_pan(Point::new(400.0, 300.0));
            v.update_pan(Point::new(401.0, 301.0)); // under slop
            assert!(!v.end_pan(), "a no-move press is a click, not a pan");
        }

        fn arrow_keys_pan() {
            let mut v = kernel();
            v.set_zoom(2.0);
            v.set_scroll(100.0, 100.0);
            let mods = winit::keyboard::ModifiersState::empty();
            assert!(v.on_key(&Key::Named(NamedKey::ArrowRight), &mods));
            assert!((v.scroll_x() - (100.0 + DEFAULT_PAN_STEP)).abs() < 1e-6);
            assert!(v.on_key(&Key::Named(NamedKey::ArrowUp), &mods));
            assert!((v.scroll_y() - (100.0 - DEFAULT_PAN_STEP)).abs() < 1e-6);
        }

        horizontal_pan_reaches_full_width_when_zoomed();
        vertical_pan_clamps_at_both_ends();
        no_overflow_means_no_scroll_and_centered_x();
        drag_pan_maps_one_to_one_and_reports_movement();
        no_move_press_is_a_click_not_a_pan();
        arrow_keys_pan();
    }

    phase viewport_scrollbars_appear_track_and_drag;
    {
        fn scrollbars_present_only_when_overflowing() {
            let mut v = kernel();
            v.set_zoom(0.25);
            assert!(v.h_scrollbar_thumb().is_none());
            assert!(v.v_scrollbar_thumb().is_none());
            v.set_zoom(3.0);
            assert!(v.h_scrollbar_thumb().is_some(), "h-bar appears when wide");
            assert!(v.v_scrollbar_thumb().is_some(), "v-bar appears when tall");
        }

        fn press_thumb_begins_drag_then_move_scrolls() {
            let mut v = kernel();
            v.set_zoom(3.0);
            v.set_scroll(0.0, 0.0);
            let thumb = v.v_scrollbar_thumb().unwrap();
            let mid = Point::new(
                f64::midpoint(thumb.x0, thumb.x1),
                f64::midpoint(thumb.y0, thumb.y1),
            );
            assert!(v.press_scrollbar(mid), "press on the v-thumb is consumed");
            assert!(v.scrollbar_dragging(), "thumb press begins a drag");
            assert_eq!(v.scroll_y(), 0.0, "press alone does not scroll");
            // Drag the thumb down 100 px → content scrolls down proportionally.
            assert!(v.drag_scrollbar(Point::new(mid.x, mid.y + 100.0)));
            assert!(v.scroll_y() > 0.0, "thumb drag scrolls: {}", v.scroll_y());
            assert!(v.scroll_y() <= v.max_scroll_y(), "stays clamped");
            v.drag_scrollbar(Point::new(mid.x, mid.y + 100_000.0));
            assert!(
                (v.scroll_y() - v.max_scroll_y()).abs() < 1.0,
                "clamped to bottom"
            );
            assert!(v.release_scrollbar(), "release ends the drag");
            assert!(!v.scrollbar_dragging());
        }

        fn press_track_pages_toward_click() {
            let mut v = kernel();
            v.set_zoom(3.0);
            v.set_scroll(0.0, 0.0); // normalise to the top (see above)
            let thumb = v.v_scrollbar_thumb().unwrap();
            let below = Point::new(f64::midpoint(thumb.x0, thumb.x1), thumb.y1 + 5.0);
            assert!(v.press_scrollbar(below), "track click consumed");
            assert!(!v.scrollbar_dragging(), "track click is not a drag");
            assert!(
                v.scroll_y() > 0.0,
                "paged down toward the click: {}",
                v.scroll_y()
            );
        }

        fn horizontal_thumb_drag_scrolls_x() {
            let mut v = kernel();
            v.set_zoom(3.0);
            v.set_scroll(0.0, 0.0); // normalise to the left
            let thumb = v.h_scrollbar_thumb().unwrap();
            let mid = Point::new(
                f64::midpoint(thumb.x0, thumb.x1),
                f64::midpoint(thumb.y0, thumb.y1),
            );
            assert!(v.press_scrollbar(mid), "press on the h-thumb is consumed");
            v.drag_scrollbar(Point::new(mid.x + 80.0, mid.y));
            assert!(
                v.scroll_x() > 0.0,
                "h-thumb drag scrolls x: {}",
                v.scroll_x()
            );
            assert!(v.release_scrollbar());
        }

        scrollbars_present_only_when_overflowing();
        press_thumb_begins_drag_then_move_scrolls();
        press_track_pages_toward_click();
        horizontal_thumb_drag_scrolls_x();
    }

    phase zoom_to_rect_fills_and_centres_the_area;
    {
        let mut v = kernel();
        let area = Rect::new(400.0, 600.0, 600.0, 750.0);
        v.zoom_to_rect(area);
        let fit = ((800.0 - 2.0 * v.pad_for_tests()) / 200.0).min((600.0 - 2.0 * v.pad_for_tests()) / 150.0);
        assert!((v.zoom() - fit).abs() < 1e-9, "{}", v.zoom());
        let centre = v.content_point_to_screen(area.center());
        assert!(
            centre.distance(Point::new(400.0, 300.0)) < 1e-6,
            "{centre:?}"
        );
    }
}

fn kernel() -> ViewportState {
    let mut v = ViewportState::new();
    v.set_rect(Rect::new(0.0, 0.0, 800.0, 600.0));
    v.set_content_size(1000.0, 1400.0);
    v
}

fn canvas_with_button(label: &str, x: f64, y: f64) -> (Element, f64, f64) {
    let button = tilo_ui::widget::Button::new("canvas-button", label);
    let manual = tilo_ui_base::widget::Manual::new(move |cx| {
        let size = cx
            .measure_child(
                0,
                Size {
                    width: tilo_ui_base::widget::MeasureConstraint::Intrinsic,
                    height: tilo_ui_base::widget::MeasureConstraint::Intrinsic,
                },
            )?
            .size;
        cx.place(
            0,
            Rect::from_origin_size((x, y), (f64::from(size.width), f64::from(size.height))),
        )?;
        Ok(Size {
            width: 600.0,
            height: 600.0,
        })
    })
    .child(Box::new(button));
    (Box::new(manual), x, y)
}

fn canvas_rect(harness: &Driver, id: &str) -> Rect {
    fn find(node: &tilo_ui::headless::InspectionNode, id: &str) -> Option<Rect> {
        if node.test_id.as_deref() == Some(id) {
            let [x, y, w, h] = node.rect;
            return Some(Rect::new(x, y, x + w, y + h));
        }
        node.children.iter().find_map(|child| find(child, id))
    }
    find(&harness.inspection().root, id).expect("the canvas button is laid out")
}

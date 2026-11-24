//! A presented view paints the way out where a person sees it: a control over the view's
//! leading top corner, on the themes the shell runs under. Rendering is the only check of that:
//! the widget's own test says the control is there and reports a press, and this says it is
//! painted, and painted nothing while the view is not presented.

use tilo_ui::State;
use tilo_ui::component::{Described, Event};
use tilo_ui::headless::FrameHarness;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{Fullscreen, FullscreenProps, Spacer, Widget};

/// A `Fullscreen` over a plain body, laid out at the window's size.
fn presented(showing: bool) -> Box<dyn Widget> {
    Box::new(Fullscreen::from_props(
        "page",
        FullscreenProps {
            key: "page".to_owned(),
            showing,
            body: Some(Box::new(Spacer::new())),
            ..FullscreenProps::default()
        },
    ))
}

/// The colour `pixel` of a frame.
fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let at = usize::try_from((y * width + x) * 4).unwrap();
    [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
}

/// How many pixels of the top-leading corner differ from what the frame paints at rest.
fn changed_in_the_corner(rgba: &[u8], width: u32, ground: [u8; 4]) -> usize {
    let mut changed = 0;
    for y in 0..96 {
        for x in 0..320 {
            if pixel(rgba, width, x, y) != ground {
                changed += 1;
            }
        }
    }
    changed
}

/// The way out is painted over the presented view, not elsewhere, and a view at rest paints
/// nothing of it.
#[test]
fn the_way_out_is_painted_over_the_presented_view() {
    let (width, height) = (800_u32, 600_u32);
    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let at_rest = FrameHarness::new_with_theme(
            State::new(),
            presented(false),
            (width, height),
            theme.clone(),
        )
        .render_rgba();
        let presented_frame = FrameHarness::new_with_theme(
            State::new(),
            presented(true),
            (width, height),
            theme.clone(),
        )
        .render_rgba();
        assert_eq!(
            at_rest.len(),
            usize::try_from(width * height * 4).unwrap(),
            "the frame is the window's"
        );
        let ground = pixel(&at_rest, width, 0, 0);
        assert_eq!(
            changed_in_the_corner(&at_rest, width, ground),
            0,
            "{name}: a view at rest paints nothing of the way out"
        );
        let painted = changed_in_the_corner(&presented_frame, width, ground);
        assert!(
            painted > 400,
            "{name}: the way out is painted over the view's leading top corner, {painted} px"
        );
        assert_eq!(
            pixel(&presented_frame, width, width - 1, height - 1),
            ground,
            "{name}: nothing is painted away from it"
        );
        crate::support::render::write_png(
            &format!("fullscreen_way_out/{name}"),
            width,
            height,
            &presented_frame,
        );
    }
}

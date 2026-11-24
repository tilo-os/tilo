//! A presented view paints the way out where a person sees it: a control over the view's
//! leading top corner, on the themes the shell runs under. Rendering is the only check of that:
//! the widget's own test says the control is there and reports a press, and this says it is
//! painted, and painted nothing while the view is not presented.

use tilo_ui::State;
use tilo_ui::component::Described;
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

/// Where the four channels of the pixel at `x`, `y` start in a frame `width` across.
fn pixel_at(width: u32, x: u32, y: u32) -> usize {
    let coordinate = |value: u32| usize::try_from(value).expect("a coordinate fits a usize");
    coordinate(y)
        .checked_mul(coordinate(width))
        .and_then(|row| row.checked_add(coordinate(x)))
        .and_then(|pixel| pixel.checked_mul(4))
        .expect("a pixel inside the window's frame")
}

/// The colour `pixel` of a frame.
fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    rgba.get(pixel_at(width, x, y)..)
        .and_then(|rest| rest.get(..4))
        .and_then(|channels| <[u8; 4]>::try_from(channels).ok())
        .expect("four channels a pixel")
}

/// How many pixels of the top-leading corner differ from what the frame paints at rest.
fn changed_in_the_corner(rgba: &[u8], width: u32, ground: [u8; 4]) -> usize {
    (0..96)
        .flat_map(|y| (0..320).map(move |x| (x, y)))
        .filter(|&(x, y)| pixel(rgba, width, x, y) != ground)
        .count()
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
        let frame_bytes = usize::try_from(width)
            .unwrap()
            .checked_mul(usize::try_from(height).unwrap())
            .and_then(|pixels| pixels.checked_mul(4))
            .expect("the frame's bytes");
        assert_eq!(at_rest.len(), frame_bytes, "the frame is the window's");
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
            pixel(
                &presented_frame,
                width,
                width.saturating_sub(1),
                height.saturating_sub(1)
            ),
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

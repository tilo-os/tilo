//! Hosted frames: a surface the session draws itself, filled the way a
//! toolkit app fills one, shows on the output the same way up.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a failed step in a test is the assertion"
)]

use tilo_compositor::{
    Compositor, HostFrame, HostLayer, OutputConfig, Region, StackPolicy, SurfaceHost,
};

/// A hosted surface whose host fills its frame row by row, the way a
/// toolkit app draws into one: red above the middle, blue below.
struct Rows {
    frame: HostFrame,
    size: (i32, i32),
}

impl Rows {
    fn new(size: (i32, i32)) -> Self {
        let (width, height) = (
            u32::try_from(size.0).unwrap(),
            u32::try_from(size.1).unwrap(),
        );
        let mut frame = HostFrame::new((width, height));
        let mut pixels = Vec::new();
        for row in 0..size.1 {
            let pixel = if row < size.1 / 2 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
            for _ in 0..size.0 {
                pixels.extend_from_slice(&pixel);
            }
        }
        frame.set_pixels(&pixels, &[Region::new(0, 0, width, height)]);
        Self { frame, size }
    }
}

impl SurfaceHost for Rows {
    fn layers(&self) -> Vec<HostLayer> {
        vec![HostLayer {
            location: (0, 0),
            size: self.size,
            frame: self.frame.clone(),
        }]
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}
}

#[test]
fn a_hosted_frame_its_host_fills_row_by_row_draws_the_same_way_up() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    compositor.add_host(Box::new(Rows::new((640, 480))));
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let above = frame.pixel(320, 120).unwrap();
    let below = frame.pixel(320, 360).unwrap();
    assert!(
        above[0] > 128 && above[2] < 128,
        "red above the middle: {above:?}"
    );
    assert!(
        below[2] > 128 && below[0] < 128,
        "blue below the middle: {below:?}"
    );
}

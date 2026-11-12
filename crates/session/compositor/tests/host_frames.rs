//! Hosted frames: a surface the session draws itself, filled the way a toolkit app fills one, shows
//! on the output the same way up.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a failed step in a test is the assertion"
)]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    reason = "a fixture's pixels are small whole numbers it laid out itself"
)]

use std::cell::RefCell;
use std::rc::Rc;

use tilo_compositor::{Compositor, OutputConfig, StackPolicy};
use tilo_session_core::surface::{HostFrame, HostLayer, LayerPose, Region, SurfaceHost};

/// A hosted surface whose host fills its frame row by row, the way a toolkit app draws into one:
/// red above the middle, blue below.
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
        let frame = HostFrame::new((width, height));
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
            pose: LayerPose::SETTLED,
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
    compositor.add_host(Rc::new(RefCell::new(Rows::new((640, 480)))));
    compositor.draw_frame().expect("a frame");
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

/// A host that paints one opaque bar into a frame the size of the output,
/// the way the top bar does, leaving the rest transparent.
struct Bar {
    frame: HostFrame,
    location: (i32, i32),
    size: (i32, i32),
    pose: LayerPose,
}

impl Bar {
    /// A `frame` of the drawn pixels, covering `layer` on the output, with the bar painted `scale`
    /// times up: what a surface drawn at the output's scale hands over.
    fn new(frame: (u32, u32), layer: (i32, i32), scale: u32) -> Self {
        Self::at((0, 0), frame, layer, scale)
    }

    /// [`Bar::new`], for a layer whose top left corner is `location` on the output rather than the
    /// output's own.
    fn at(location: (i32, i32), frame: (u32, u32), layer: (i32, i32), scale: u32) -> Self {
        let (width, height) = frame;
        let bar = (40 * scale, 20 * scale, 200 * scale, 60 * scale);
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for y in bar.1..bar.1 + bar.3 {
            for x in bar.0..bar.0 + bar.2 {
                let start = ((y * width + x) * 4) as usize;
                pixels[start..start + 4].copy_from_slice(&[0, 255, 0, 255]);
            }
        }
        let frame = HostFrame::new(frame);
        frame.set_pixels(&pixels, &[Region::new(bar.0, bar.1, bar.2, bar.3)]);
        Self {
            frame,
            location,
            size: layer,
            pose: LayerPose::SETTLED,
        }
    }
}

impl SurfaceHost for Bar {
    fn layers(&self) -> Vec<HostLayer> {
        vec![HostLayer {
            pose: self.pose,
            location: self.location,
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

/// A host that has a layer but has drawn nothing into it: it never shows.
struct Blank {
    frame: HostFrame,
    size: (i32, i32),
}

impl SurfaceHost for Blank {
    fn layers(&self) -> Vec<HostLayer> {
        vec![HostLayer {
            pose: LayerPose::SETTLED,
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
fn a_layer_shows_where_it_drew_and_nowhere_else() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    compositor.add_host(Rc::new(RefCell::new(Bar::new((640, 480), (640, 480), 1))));
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let inside = frame.pixel(120, 40).unwrap();
    assert!(
        inside[1] > 128 && inside[0] < 128,
        "the bar it drew shows: {inside:?}"
    );
    let outside = frame.pixel(500, 400).unwrap();
    assert!(
        (i32::from(outside[0]) - 26).abs() <= 2
            && (i32::from(outside[1]) - 27).abs() <= 2
            && (i32::from(outside[2]) - 38).abs() <= 2,
        "the rest is the desktop behind it: {outside:?}"
    );
}

#[test]
fn a_layer_the_size_of_the_output_drawn_at_half_of_it_lands_where_it_was_drawn() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    // The frame is drawn at twice the pixels of the rectangle it covers: the bar it draws at (80,
    // 40) of the frame covers (40, 20) on the output.
    compositor.add_host(Rc::new(RefCell::new(Bar::new((640, 480), (320, 240), 2))));
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let inside = frame.pixel(50, 25).unwrap();
    assert!(
        inside[1] > 128 && inside[0] < 128,
        "the bar, at the rectangle the layer covers: {inside:?}"
    );
    let below = frame.pixel(50, 120).unwrap();
    assert!(below[1] < 128, "nothing below the bar it drew: {below:?}");
    let beside = frame.pixel(280, 25).unwrap();
    assert!(beside[1] < 128, "and nothing beside it: {beside:?}");
}

/// A host that paints a bar in its first frame and erases it in the next,
/// the way a banner leaves: what it stopped drawing is painted over, not
/// left on the screen.
struct Vanishes {
    frame: HostFrame,
    size: (i32, i32),
    /// How many frames it has been asked for: it erases its bar in the second.
    frames: u32,
}

impl Vanishes {
    const BAR: (u32, u32, u32, u32) = (40, 20, 200, 60);

    fn new(size: (i32, i32)) -> Self {
        let (width, height) = (
            u32::try_from(size.0).unwrap(),
            u32::try_from(size.1).unwrap(),
        );
        let (bx, by, bw, bh) = Self::BAR;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for y in by..by + bh {
            for x in bx..bx + bw {
                let start = ((y * width + x) * 4) as usize;
                pixels[start..start + 4].copy_from_slice(&[0, 255, 0, 255]);
            }
        }
        let frame = HostFrame::new((width, height));
        frame.set_pixels(&pixels, &[Region::new(bx, by, bw, bh)]);
        Self {
            frame,
            size,
            frames: 0,
        }
    }
}

impl SurfaceHost for Vanishes {
    fn layers(&self) -> Vec<HostLayer> {
        vec![HostLayer {
            pose: LayerPose::SETTLED,
            location: (0, 0),
            size: self.size,
            frame: self.frame.clone(),
        }]
    }

    fn prepare_frame(&mut self) -> bool {
        self.frames += 1;
        if self.frames != 2 {
            return false;
        }
        let (width, height) = self.frame.size();
        let (bx, by, bw, bh) = Self::BAR;
        self.frame.set_pixels(
            &vec![0u8; (width * height * 4) as usize],
            &[Region::new(bx, by, bw, bh)],
        );
        true
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}
}

#[test]
fn what_a_layer_stopped_drawing_is_painted_over() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    compositor.add_host(Rc::new(RefCell::new(Vanishes::new((640, 480)))));
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let painted = frame.pixel(120, 40).unwrap();
    assert!(
        painted[1] > 128 && painted[0] < 128,
        "the bar it drew shows: {painted:?}"
    );
    // A display shows one frame per refresh, and the frame that paints the erase over is built when
    // its turn comes: drawing on brings it.
    let mut erased = None;
    for _ in 0..8 {
        compositor.draw_frame().expect("a frame");
        let frame = compositor.screenshot("HEADLESS-1").unwrap();
        let pixel = frame.pixel(120, 40).unwrap();
        erased = Some(pixel);
        if (i32::from(pixel[1]) - 27).abs() <= 2 {
            break;
        }
    }
    let erased = erased.unwrap();
    assert!(
        (i32::from(erased[0]) - 26).abs() <= 2
            && (i32::from(erased[1]) - 27).abs() <= 2
            && (i32::from(erased[2]) - 38).abs() <= 2,
        "the desktop behind it shows where the bar was: {erased:?}"
    );
}

#[test]
fn a_layer_that_is_not_at_the_outputs_corner_draws_where_it_says() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    // A panel a third of the way down the output, drawn at twice the pixels of the rectangle it
    // covers: its bar at (80, 40) of the frame lands at (40, 320) of the output.
    compositor.add_host(Rc::new(RefCell::new(Bar::at(
        (0, 300),
        (640, 480),
        (320, 240),
        2,
    ))));
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let inside = frame.pixel(50, 325).unwrap();
    assert!(
        inside[1] > 128 && inside[0] < 128,
        "the bar it drew, where the layer put it: {inside:?}"
    );
    for (x, y) in [(50, 420), (50, 40), (280, 325)] {
        let outside = frame.pixel(x, y).unwrap();
        assert!(
            outside[1] < 128,
            "nothing at ({x}, {y}), where it drew none: {outside:?}"
        );
    }
}

#[test]
fn a_posed_layer_is_drawn_faded_scaled_and_moved_from_the_same_pixels() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    // Its bar rests at (40, 20)..(240, 80); half its size about its top left corner, moved 300
    // to the right and half faded, it is drawn at (340, 10)..(440, 40).
    let mut bar = Bar::new((640, 480), (640, 480), 1);
    bar.pose = LayerPose {
        scale: 0.5,
        opacity: 0.5,
        anchor: (0.0, 0.0),
        offset: (300.0, 0.0),
    };
    compositor.add_host(Rc::new(RefCell::new(bar)));
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let posed = frame.pixel(390, 25).unwrap();
    assert!(
        posed[1] > 100 && posed[1] < 180,
        "the bar, half faded over the desktop: {posed:?}"
    );
    for (x, y) in [(120, 40), (390, 60), (460, 25)] {
        let outside = frame.pixel(x, y).unwrap();
        assert!(outside[1] < 60, "nothing at ({x}, {y}): {outside:?}");
    }
}

#[test]
fn a_layer_that_drew_nothing_is_not_drawn() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    compositor.add_host(Rc::new(RefCell::new(Blank {
        frame: HostFrame::new((640, 480)),
        size: (640, 480),
    })));
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let middle = frame.pixel(320, 240).unwrap();
    assert!(
        (i32::from(middle[0]) - 26).abs() <= 2
            && (i32::from(middle[1]) - 27).abs() <= 2
            && (i32::from(middle[2]) - 38).abs() <= 2,
        "an empty layer shows nothing, the desktop shows through: {middle:?}"
    );
}

/// A surface that says it needs a frame now wakes the loop for it: what a host animates is drawn at
/// the pace it asks, not at the loop's idle wait.
#[test]
fn a_host_that_needs_a_frame_now_shortens_the_loops_wait() {
    use std::time::{Duration, Instant};

    struct Animated;

    impl SurfaceHost for Animated {
        fn layers(&self) -> Vec<HostLayer> {
            Vec::new()
        }

        fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
            false
        }

        fn pointer_motion(&mut self, _x: f64, _y: f64) {}

        fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

        fn next_deadline(&self) -> Option<Instant> {
            Some(Instant::now())
        }
    }

    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    compositor.draw_frame().expect("a frame");
    assert_eq!(
        compositor.next_wait(),
        None,
        "an idle loop waits for an event, not for a poll interval"
    );
    compositor.add_host(Rc::new(RefCell::new(Animated)));
    assert!(
        compositor.next_wait() <= Some(Duration::from_millis(1)),
        "a frame a host asks for now is drawn now"
    );
}

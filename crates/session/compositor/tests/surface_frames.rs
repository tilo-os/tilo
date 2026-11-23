//! Hosted frames: a surface the session draws itself, filled the way a toolkit app fills one, shows
//! on the output the same way up.

#![cfg(target_os = "linux")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    reason = "a fixture's pixels are small whole numbers it laid out itself"
)]

use std::cell::RefCell;
use std::rc::Rc;

use tilo_compositor::{Compositor, InputEvent, OutputConfig, StackPolicy};
use tilo_session_core::surface::{LayerPose, Region, ShellSurface, SurfaceFrame, SurfaceLayer};

/// A surface that fills its frame row by row, the way a toolkit app draws into one:
/// red above the middle, blue below.
struct Rows {
    frame: SurfaceFrame,
    size: (i32, i32),
}

impl Rows {
    fn new(size: (i32, i32)) -> Self {
        let (width, height) = (
            u32::try_from(size.0).unwrap(),
            u32::try_from(size.1).unwrap(),
        );
        let frame = SurfaceFrame::new((width, height));
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

impl ShellSurface for Rows {
    fn layers(&self) -> Vec<SurfaceLayer> {
        vec![SurfaceLayer {
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

tilo_test_env::scenario! {
    /// The frames a surface hands the compositor.
    fn a_surfaces_layers_are_drawn_where_and_as_it_drew_them;

    phase a_frame_a_surface_fills_row_by_row_draws_the_same_way_up;
    {
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
            .expect("output");
        compositor.add_surface(Rc::new(RefCell::new(Rows::new((640, 480)))));
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

    phase a_layer_shows_where_it_drew_and_nowhere_else;
    {
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
            .expect("output");
        compositor.add_surface(Rc::new(RefCell::new(Bar::new((640, 480), (640, 480), 1))));
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

    phase a_layer_the_size_of_the_output_drawn_at_half_of_it_lands_where_it_was_drawn;
    {
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
            .expect("output");
        // The frame is drawn at twice the pixels of the rectangle it covers: the bar it draws at (80,
        // 40) of the frame covers (40, 20) on the output.
        compositor.add_surface(Rc::new(RefCell::new(Bar::new((640, 480), (320, 240), 2))));
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

    phase what_a_layer_stopped_drawing_is_painted_over;
    {
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
            .expect("output");
        compositor.add_surface(Rc::new(RefCell::new(Vanishes::new((640, 480)))));
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

    phase a_layer_that_is_not_at_the_outputs_corner_draws_where_it_says;
    {
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
            .expect("output");
        // A panel a third of the way down the output, drawn at twice the pixels of the rectangle it
        // covers: its bar at (80, 40) of the frame lands at (40, 320) of the output.
        compositor.add_surface(Rc::new(RefCell::new(Bar::at(
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

    phase a_posed_layer_is_drawn_faded_scaled_and_moved_from_the_same_pixels;
    {
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
        compositor.add_surface(Rc::new(RefCell::new(bar)));
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

    phase a_layer_that_drew_nothing_is_not_drawn;
    {
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
            .expect("output");
        compositor.add_surface(Rc::new(RefCell::new(Blank {
            frame: SurfaceFrame::new((640, 480)),
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

    phase a_surface_that_needs_a_frame_now_shortens_the_loops_wait;
    // A surface that says it needs a frame now wakes the loop for it: what it animates is drawn at
    // the pace it asks, not at the loop's idle wait.
    {
        use std::time::{Duration, Instant};

        struct Animated;

        impl ShellSurface for Animated {
            fn layers(&self) -> Vec<SurfaceLayer> {
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
        compositor.add_surface(Rc::new(RefCell::new(Animated)));
        assert!(
            compositor.next_wait() <= Some(Duration::from_millis(1)),
            "a frame a surface asks for now is drawn now"
        );
    }

    phase a_program_opening_a_window_is_answered_while_a_surface_draws_elsewhere;
    // A surface drawing on the CPU hands its picture to a thread of its own, so the loop goes on
    // answering programs while it draws: a window opened during the draw is configured before the
    // draw finishes, and the finished draw wakes the loop and is shown.
    {
        use crate::harness::{TestClient, pump_one};

        let (release, released) = std::sync::mpsc::channel::<()>();
        let surface = DrawnElsewhere::new((640, 480), released);
        let frame = surface.frame.clone();
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
            .expect("output");
        compositor.add_surface(Rc::new(RefCell::new(surface)));
        let mut client = TestClient::connect(&mut compositor);
        compositor.render().expect("a frame");
        assert!(
            frame.in_flight(),
            "the frame was handed to another thread, and drawing it did not hold up the loop"
        );

        pump_one(&mut compositor, &mut client, |_, client| {
            client.seen.compositor.is_some() && client.seen.wm_base.is_some()
        });
        client.open_window("drawn.elsewhere");
        pump_one(&mut compositor, &mut client, |_, client| client.seen.configured);
        assert!(
            frame.in_flight(),
            "the program that connected and opened a window during the draw was answered before \
             the draw finished"
        );

        release.send(()).expect("the drawing thread waits");
        // The finished draw ends a wait that has no limit: the loop is woken for it, never polled.
        while frame.in_flight() {
            compositor.dispatch(None).expect("dispatch");
        }
        compositor.render().expect("a frame");
        let shot = compositor.screenshot("HEADLESS-1").unwrap();
        let middle = shot.pixel(320, 240).unwrap();
        assert!(
            middle[1] > 128 && middle[0] < 128 && middle[2] < 128,
            "the frame drawn elsewhere is shown once it is done: {middle:?}"
        );
    }

    phase input_is_timed_to_the_frame_that_shows_the_draw_it_started;
    // A key opens what a surface draws on the CPU: the turn that hands the draw to its thread has
    // nothing new to show, and the frame that shows the finished draw is the one that answers the
    // key, however many turns later it comes.
    {
        let (release, released) = std::sync::mpsc::channel::<()>();
        let surface = DrawsOnPress::new((640, 480), released);
        let frame = surface.frame.clone();
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
            .expect("output");
        compositor.add_surface(Rc::new(RefCell::new(surface)));
        compositor.draw_frame().expect("the first frame");
        compositor.record_frames();
        compositor.begin_tick();
        // The key comes from another device's link, routed as the display backend routes a
        // keyboard's own, not through `Compositor::input`.
        compositor
            .input_remote(InputEvent::Key {
                keycode: 30,
                pressed: true,
            })
            .expect("the key is routed");
        compositor.render().expect("the turn that hands the draw over");
        compositor.end_tick();
        assert!(frame.in_flight(), "the surface draws on its own thread");
        let handed = *compositor.recorded_frames().last().expect("the turn");
        assert!(!handed.painted, "nothing new to show yet: {handed:?}");

        release.send(()).expect("the drawing thread waits");
        while frame.in_flight() {
            compositor.dispatch(None).expect("dispatch");
        }
        compositor.begin_tick();
        compositor.render().expect("the frame that shows the draw");
        compositor.end_tick();
        let shown = *compositor.recorded_frames().last().expect("the turn");
        assert!(shown.painted, "the finished draw is shown: {shown:?}");
        assert!(
            shown.input_latency_us.is_some(),
            "the frame that shows the draw answers the key: {shown:?}"
        );
    }

    phase a_look_at_the_display_shows_a_draw_that_finished_between_its_frames;
    // A draw that finishes is taken by the next frame the loop builds; a display that is not due
    // for one yet leaves it owed. A screenshot taken then shows the finished draw all the same.
    {
        let (release, released) = std::sync::mpsc::channel::<()>();
        let surface = DrawnElsewhere::new((640, 480), released);
        let frame = surface.frame.clone();
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
        // Once a second: the frame after the first is not due while the test runs on.
        let mut output = OutputConfig::new("HEADLESS-1", 640, 480);
        output.refresh_mhz = 1_000;
        compositor.add_output(&output).expect("output");
        compositor.add_surface(Rc::new(RefCell::new(surface)));
        compositor.render().expect("a frame");
        release.send(()).expect("the drawing thread waits");
        while frame.in_flight() {
            compositor.dispatch(None).expect("dispatch");
        }
        // The loop takes the finished draw; the display is not due, so the frame stays owed.
        compositor.render().expect("a frame");
        let shot = compositor.screenshot("HEADLESS-1").unwrap();
        let middle = shot.pixel(320, 240).unwrap();
        assert!(
            middle[1] > 128 && middle[0] < 128 && middle[2] < 128,
            "the screenshot shows the draw that finished: {middle:?}"
        );
    }

    phase a_press_while_a_panel_draws_its_first_frame_reaches_the_panel;
    // The pointer goes where a surface's frame shows something. A panel that just opened shows
    // nothing until its first frame is drawn on another thread: a press made meanwhile waits for
    // that draw, so it reaches the panel the person pressed on rather than passing through to
    // what was there before.
    {
        let (release, released) = std::sync::mpsc::channel::<()>();
        let surface = Rc::new(RefCell::new(DrawnElsewhere::new((640, 480), released)));
        let frame = surface.borrow().frame.clone();
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
            .expect("output");
        compositor.add_surface(Rc::clone(&surface) as Rc<RefCell<dyn ShellSurface>>);
        compositor.render().expect("a frame");
        assert!(frame.in_flight(), "the panel's first frame is drawing");

        compositor
            .input(InputEvent::PointerMotion { x: 320.0, y: 240.0 })
            .expect("the pointer moves onto the panel");
        release.send(()).expect("the drawing thread waits");
        compositor
            .input(InputEvent::PointerButton { button: 0x110, pressed: true })
            .expect("a press");
        compositor
            .input(InputEvent::PointerButton { button: 0x110, pressed: false })
            .expect("its release");
        assert_eq!(
            surface.borrow().buttons,
            vec![true, false],
            "the press and then its release reach the panel"
        );
    }
}

/// A surface whose frames another thread draws, as a surface drawing on the CPU draws them: the
/// thread paints the frame green once the test lets it.
struct DrawnElsewhere {
    frame: SurfaceFrame,
    size: (i32, i32),
    released: Option<std::sync::mpsc::Receiver<()>>,
    /// The buttons that reached it, pressed or released.
    buttons: Vec<bool>,
}

impl DrawnElsewhere {
    fn new(size: (i32, i32), released: std::sync::mpsc::Receiver<()>) -> Self {
        let frame = SurfaceFrame::new((
            u32::try_from(size.0).unwrap(),
            u32::try_from(size.1).unwrap(),
        ));
        Self {
            frame,
            size,
            released: Some(released),
            buttons: Vec::new(),
        }
    }
}

impl ShellSurface for DrawnElsewhere {
    fn layers(&self) -> Vec<SurfaceLayer> {
        vec![SurfaceLayer {
            pose: LayerPose::SETTLED,
            location: (0, 0),
            size: self.size,
            frame: self.frame.clone(),
        }]
    }

    fn prepare_frame(&mut self) -> bool {
        let Some(released) = self.released.take() else {
            return false;
        };
        let ticket = self.frame.drawing();
        let frame = self.frame.clone();
        std::thread::spawn(move || {
            released.recv().expect("the test lets the draw finish");
            let (width, height) = frame.size();
            let green = [0, 255, 0, 255].repeat((width * height) as usize);
            frame.set_pixels(&green, &[Region::new(0, 0, width, height)]);
            frame.finished(ticket);
        });
        false
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a point on the output, inside the surface"
    )]
    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        self.frame
            .alpha(x as u32, y as u32)
            .is_some_and(|alpha| alpha > 0)
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, pressed: bool) {
        self.buttons.push(pressed);
    }
}

/// A surface that takes the keyboard and starts a draw on another thread for each key pressed, as a
/// launcher drawing on the CPU redraws for what was typed: the thread paints the frame green once
/// the test lets it.
struct DrawsOnPress {
    frame: SurfaceFrame,
    size: (i32, i32),
    released: std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
    pressed: bool,
}

impl DrawsOnPress {
    fn new(size: (i32, i32), released: std::sync::mpsc::Receiver<()>) -> Self {
        let frame = SurfaceFrame::new((
            u32::try_from(size.0).unwrap(),
            u32::try_from(size.1).unwrap(),
        ));
        Self {
            frame,
            size,
            released: std::sync::Arc::new(std::sync::Mutex::new(released)),
            pressed: false,
        }
    }
}

impl ShellSurface for DrawsOnPress {
    fn layers(&self) -> Vec<SurfaceLayer> {
        vec![SurfaceLayer {
            pose: LayerPose::SETTLED,
            location: (0, 0),
            size: self.size,
            frame: self.frame.clone(),
        }]
    }

    fn prepare_frame(&mut self) -> bool {
        if !std::mem::take(&mut self.pressed) {
            return false;
        }
        let ticket = self.frame.drawing();
        let frame = self.frame.clone();
        let released = std::sync::Arc::clone(&self.released);
        std::thread::spawn(move || {
            released
                .lock()
                .expect("one draw at a time")
                .recv()
                .expect("the test lets the draw finish");
            let (width, height) = frame.size();
            let green = [0, 255, 0, 255].repeat((width * height) as usize);
            frame.set_pixels(&green, &[Region::new(0, 0, width, height)]);
            frame.finished(ticket);
        });
        false
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

    fn wants_keyboard(&self) -> bool {
        true
    }

    fn key(&mut self, key: &tilo_session_core::surface::SurfaceKey) {
        self.pressed |= key.pressed;
    }
}

/// A surface that paints one opaque bar into a frame the size of the output,
/// the way the top bar does, leaving the rest transparent.
struct Bar {
    frame: SurfaceFrame,
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
        let frame = SurfaceFrame::new(frame);
        frame.set_pixels(&pixels, &[Region::new(bar.0, bar.1, bar.2, bar.3)]);
        Self {
            frame,
            location,
            size: layer,
            pose: LayerPose::SETTLED,
        }
    }
}

impl ShellSurface for Bar {
    fn layers(&self) -> Vec<SurfaceLayer> {
        vec![SurfaceLayer {
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

/// A surface that has a layer but has drawn nothing into it: it never shows.
struct Blank {
    frame: SurfaceFrame,
    size: (i32, i32),
}

impl ShellSurface for Blank {
    fn layers(&self) -> Vec<SurfaceLayer> {
        vec![SurfaceLayer {
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

/// A surface that paints a bar in its first frame and erases it in the next,
/// the way a banner leaves: what it stopped drawing is painted over, not
/// left on the screen.
struct Vanishes {
    frame: SurfaceFrame,
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
        let frame = SurfaceFrame::new((width, height));
        frame.set_pixels(&pixels, &[Region::new(bx, by, bw, bh)]);
        Self {
            frame,
            size,
            frames: 0,
        }
    }
}

impl ShellSurface for Vanishes {
    fn layers(&self) -> Vec<SurfaceLayer> {
        vec![SurfaceLayer {
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

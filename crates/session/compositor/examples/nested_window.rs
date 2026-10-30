//! The window shows a scene the same way up as an output with no display.
//!
//! The nested backend draws the scene into a window the desktop's compositor
//! shows; a headless output draws the same scene on the CPU. Both are
//! pictures of the same hosted layer, and both must have its red half above
//! its blue one. winit only builds its window on the process's main thread,
//! so this is one run rather than a harness of cases; a machine with no
//! display runs nothing.
//!
//!     cargo run -p tilo-compositor --example nested_window
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test fixture: a failed step is the assertion"
)]

fn main() {
    #[cfg(target_os = "linux")]
    nested::main();
}

#[cfg(target_os = "linux")]
mod nested {
    use std::time::Duration;

    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
    use smithay::utils::Transform;
    use tilo_compositor::{
        Compositor, HostLayer, NestedConfig, OutputConfig, Screenshot, StackPolicy, SurfaceHost,
    };

    const WIDTH: i32 = 320;
    const HEIGHT: i32 = 200;

    /// A layer as tall as the output: red on top, blue below.
    struct Marker {
        buffer: MemoryRenderBuffer,
    }

    impl Marker {
        fn new() -> Self {
            // Rows top first; ABGR8888 is R, G, B, A in memory.
            let mut bytes = Vec::with_capacity(usize::try_from(WIDTH * HEIGHT * 4).unwrap());
            for row in 0..HEIGHT {
                let pixel = if row < HEIGHT / 2 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                };
                for _ in 0..WIDTH {
                    bytes.extend_from_slice(&pixel);
                }
            }
            Self {
                buffer: MemoryRenderBuffer::from_slice(
                    &bytes,
                    Fourcc::Abgr8888,
                    (WIDTH, HEIGHT),
                    1,
                    Transform::Normal,
                    None,
                ),
            }
        }
    }

    impl SurfaceHost for Marker {
        fn layers(&self) -> Vec<HostLayer> {
            vec![HostLayer {
                location: (0, 0),
                size: (WIDTH, HEIGHT),
                buffer: self.buffer.clone(),
            }]
        }

        // The layer covers the output but takes nothing: this run only draws
        // it.
        fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
            false
        }

        fn pointer_motion(&mut self, _x: f64, _y: f64) {}

        fn pointer_button(&mut self, _button: u32, _pressed: bool) {}
    }

    pub(crate) fn main() {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
            eprintln!("no display: nothing to compare");
            return;
        }
        let headless = headless_picture();
        let shown = match window_picture() {
            Some(shown) => shown,
            None => {
                eprintln!("no window: nothing to compare");
                return;
            }
        };
        let top = (WIDTH / 2, HEIGHT / 4);
        let bottom = (WIDTH / 2, HEIGHT * 3 / 4);
        let (headless_top, headless_bottom) = (pixel(&headless, top), pixel(&headless, bottom));
        let (shown_top, shown_bottom) = (pixel(&shown, top), pixel(&shown, bottom));
        eprintln!("headless: top {headless_top:?}, bottom {headless_bottom:?}");
        eprintln!("window:   top {shown_top:?}, bottom {shown_bottom:?}");
        assert!(
            is_red(headless_top) && is_blue(headless_bottom),
            "the headless output is upside down"
        );
        assert!(
            is_red(shown_top) && is_blue(shown_bottom),
            "the window is upside down"
        );
    }

    /// The scene on an output with no display.
    fn headless_picture() -> Screenshot {
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT))
            .expect("output");
        compositor.add_host(Box::new(Marker::new()));
        compositor.render().expect("render");
        compositor.screenshot("HEADLESS-1").expect("screenshot")
    }

    /// The scene in a window on the desktop's compositor.
    fn window_picture() -> Option<Screenshot> {
        let mut compositor = match Compositor::nested(
            Box::new(StackPolicy),
            &NestedConfig {
                size: (
                    u16::try_from(WIDTH).expect("width"),
                    u16::try_from(HEIGHT).expect("height"),
                ),
                ..NestedConfig::default()
            },
        ) {
            Ok(compositor) => compositor,
            Err(error) => {
                eprintln!("the window did not open: {error}");
                return None;
            }
        };
        compositor.add_host(Box::new(Marker::new()));
        for _ in 0..10 {
            compositor
                .dispatch(Duration::from_millis(16))
                .expect("wait");
            compositor.render().expect("render");
        }
        Some(compositor.screenshot("NESTED-1").expect("screenshot"))
    }

    fn pixel(picture: &Screenshot, (x, y): (i32, i32)) -> [u8; 4] {
        picture
            .pixel(u32::try_from(x).unwrap(), u32::try_from(y).unwrap())
            .expect("the picture holds the pixel")
    }

    fn is_red(pixel: [u8; 4]) -> bool {
        pixel[0] > 128 && pixel[1] < 128 && pixel[2] < 128
    }

    fn is_blue(pixel: [u8; 4]) -> bool {
        pixel[2] > 128 && pixel[0] < 128 && pixel[1] < 128
    }
}

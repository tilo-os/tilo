//! The window shows a scene the same way up as an output with no display.
//!
//! The nested backend draws the scene into a window; the window is a client of
//! a compositor with no display in this process, so the run needs no desktop
//! and nothing it draws reaches one. Three pictures of the same hosted layer
//! must have its red half above its blue one: the window's output as the
//! backend reads it back, the same window as the compositor around it shows
//! it, and a headless output.
//!
//!     cargo run -p tilo-compositor --example nested_orientation
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
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
    use smithay::utils::Transform;
    use tilo_compositor::{
        Compositor, HostFrame, HostLayer, NestedConfig, OutputConfig, Screenshot, StackPolicy,
        SurfaceHost,
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
                frame: HostFrame::from_buffer(self.buffer.clone(), (WIDTH as u32, HEIGHT as u32)),
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
        let mut outer = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
        outer
            .add_output(&OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT))
            .expect("output");
        let socket = outer.listen().expect("socket");
        // The window opens on the compositor here, by its full path: the
        // process gets no other display to open it on.
        let socket = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
            || PathBuf::from(&socket),
            |directory| Path::new(&directory).join(&socket),
        );
        // SAFETY: this run is one thread and has opened no window yet; winit
        // reads the display when it does, below.
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", &socket);
            std::env::remove_var("DISPLAY");
        }

        let mut window = match Compositor::nested(
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
            Err(error) => panic!("the window did not open on the output here: {error}"),
        };
        window.add_host(Box::new(Marker::new()));
        // Both sides run: the window draws into its own output, and the
        // compositor around it takes the window's frames.
        for _ in 0..10 {
            window.dispatch(Duration::from_millis(16)).expect("wait");
            window.render().expect("render");
            outer.dispatch(Duration::from_millis(16)).expect("wait");
            outer.render().expect("render");
        }

        let headless = headless_picture();
        let read_back = window.screenshot("NESTED-1").expect("screenshot");
        let shown = outer.screenshot("HEADLESS-1").expect("screenshot");
        let top = (WIDTH / 2, HEIGHT / 4);
        let bottom = (WIDTH / 2, HEIGHT * 3 / 4);
        for (what, picture) in [
            ("headless", &headless),
            ("the window", &read_back),
            ("the window as shown", &shown),
        ] {
            let (above, below) = (pixel(picture, top), pixel(picture, bottom));
            eprintln!("{what}: top {above:?}, bottom {below:?}");
            assert!(is_red(above) && is_blue(below), "{what} is upside down");
        }
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

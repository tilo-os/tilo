//! The window shows a scene the same way up as an output with no display.
//!
//! The nested backend draws the scene into a window; the window is a client of
//! a compositor with no display, so the run needs no desktop and nothing it
//! draws reaches one. Three pictures of the same hosted layer must have its
//! red half above its blue one: the window's output as the backend reads it
//! back, the same window as the compositor around it shows it, and a headless
//! output.
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
    if std::env::args().any(|argument| argument == "--window") {
        nested::window_side();
    } else {
        nested::main();
    }
}

#[cfg(target_os = "linux")]
mod nested {
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
    use smithay::utils::Transform;
    use tilo_compositor::{
        Compositor, HostLayer, NestedConfig, OutputConfig, Screenshot, StackPolicy, SurfaceHost,
    };

    const WIDTH: i32 = 320;
    const HEIGHT: i32 = 200;

    /// How long the window may take to show itself and report.
    const OPENING: Duration = Duration::from_secs(60);

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

    /// The compositor with no display, and the window in a process of its
    /// own on it.
    ///
    /// The window cannot start in this process: its client makes round trips
    /// this compositor has to answer while the window is created.
    pub(crate) fn main() {
        let mut outer = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
        outer
            .add_output(&OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT))
            .expect("output");
        outer.add_host(Box::new(Marker::new()));
        let socket = outer.listen().expect("socket");
        // By its full path: the window's process gets a runtime directory of
        // its own, so it cannot find the socket by name.
        let socket = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
            || PathBuf::from(&socket),
            |directory| Path::new(&directory).join(&socket),
        );

        let directory = tempfile::tempdir().expect("runtime directory");
        let mut window = Command::new(std::env::current_exe().expect("own path"))
            .arg("--window")
            // Nothing of this desktop: no X display, and a runtime directory
            // of its own. The window opens on the compositor here and
            // nowhere else.
            .env("WAYLAND_DISPLAY", &socket)
            .env("XDG_RUNTIME_DIR", directory.path())
            .env_remove("DISPLAY")
            .stdout(Stdio::piped())
            .spawn()
            .expect("window process");

        // The window's frames arrive only while this compositor runs.
        let deadline = Instant::now() + OPENING;
        while window.try_wait().expect("wait").is_none() {
            outer.dispatch(Duration::from_millis(16)).expect("wait");
            outer.render().expect("render");
            assert!(
                Instant::now() < deadline,
                "the window showed nothing here (no GPU to draw one)"
            );
        }
        // The last frame the window committed, and the compositor's answer to
        // it, are still on their way.
        for _ in 0..5 {
            outer.dispatch(Duration::from_millis(16)).expect("wait");
            outer.render().expect("render");
        }

        let printed = window.wait_with_output().expect("report").stdout;
        let printed = String::from_utf8(printed).expect("report is text");
        eprintln!("{printed}");
        let (above, below) = reported(&printed);

        let headless = headless_picture();
        let shown = outer.screenshot("HEADLESS-1").expect("screenshot");
        let top = (WIDTH / 2, HEIGHT / 4);
        let bottom = (WIDTH / 2, HEIGHT * 3 / 4);
        // Where the compositor put the window: the same two points of the
        // layer inside the window's rectangle, in the picture of the output.
        let (x, y, width, height) = outer
            .windows()
            .into_iter()
            .next()
            .map(|(_, rect)| rect)
            .expect("the window is on the output");
        let shown_top = (x + width / 2, y + height / 4);
        let shown_bottom = (x + width / 2, y + height * 3 / 4);
        for (what, above, below) in [
            ("the window", above, below),
            (
                "a headless output",
                pixel(&headless, top),
                pixel(&headless, bottom),
            ),
            (
                "the window as shown",
                pixel(&shown, shown_top),
                pixel(&shown, shown_bottom),
            ),
        ] {
            eprintln!("{what}: top {above:?}, bottom {below:?}");
            assert!(is_red(above) && is_blue(below), "{what} is upside down");
        }
    }

    /// The window's own picture of the layer, as it reports it.
    pub(crate) fn window_side() {
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
        for _ in 0..10 {
            window.dispatch(Duration::from_millis(16)).expect("wait");
            window.render().expect("render");
        }
        let picture = window.screenshot("NESTED-1").expect("screenshot");
        let top = (WIDTH / 2, HEIGHT / 4);
        let bottom = (WIDTH / 2, HEIGHT * 3 / 4);
        let mut report = std::io::stdout();
        for (what, pixel) in [
            ("top", pixel(&picture, top)),
            ("bottom", pixel(&picture, bottom)),
        ] {
            writeln!(
                report,
                "{what} {} {} {} {}",
                pixel[0], pixel[1], pixel[2], pixel[3]
            )
            .expect("report");
        }
    }

    /// The first two pixels the window's report names: above the middle of
    /// the layer, then below it.
    fn reported(report: &str) -> ([u8; 4], [u8; 4]) {
        let mut found: Vec<[u8; 4]> = Vec::new();
        for line in report.lines() {
            let mut fields = line.split_whitespace();
            let Some(what) = fields.next() else {
                continue;
            };
            if what != "top" && what != "bottom" {
                continue;
            }
            let mut pixel = [0_u8; 4];
            for channel in &mut pixel {
                *channel = fields
                    .next()
                    .and_then(|field| field.parse().ok())
                    .expect("a channel of a reported pixel");
            }
            found.push(pixel);
        }
        assert_eq!(found.len(), 2, "the window reported two pixels: {report}");
        (found[0], found[1])
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

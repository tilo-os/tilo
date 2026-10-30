//! The session in a window of another compositor: its window takes a new
//! size, and closing it ends the session.
//!
//! The compositor here is an output with no display, and the session runs in
//! a window as its client, the way the desktop it runs inside would have it.
//! Resizing that window is measured until the session commits the new size,
//! closing it until the process is gone. A machine without a GPU to draw the
//! window on skips the run.
//!
//!     cargo run -p tilo-session --example nested_window
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "this run: a failed step is the assertion"
)]

fn main() {
    #[cfg(target_os = "linux")]
    nested::main();
}

#[cfg(target_os = "linux")]
mod nested {
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use smithay::utils::{Logical, Point, Rectangle};
    use tilo_compositor::{
        Arrangement, Compositor, InputEvent, ModeRequest, OutputChange, OutputConfig, WindowInfo,
        WindowPolicy,
    };

    /// How long the session may take to show its window here.
    const OPENING: Duration = Duration::from_secs(30);
    /// How long a resize may take to reach the session's client buffer.
    const RESIZING: Duration = Duration::from_secs(5);
    /// How long closing the window may take to end the session.
    const CLOSING: Duration = Duration::from_secs(5);
    /// How long either may take for a person not to wait on it.
    const PROMPT: Duration = Duration::from_secs(1);
    /// How long the session is watched doing nothing, and following the
    /// pointer.
    const IDLE: Duration = Duration::from_secs(3);
    /// One display's refresh: how often a desktop asks its clients for their
    /// next frame.
    const REFRESH: Duration = Duration::from_millis(16);

    /// The window fills the output, and follows it when it changes.
    struct Fill;

    impl WindowPolicy for Fill {
        fn initial_size(
            &mut self,
            _window: &WindowInfo,
            outputs: &[Rectangle<i32, Logical>],
        ) -> Option<(i32, i32)> {
            outputs.first().map(|output| (output.size.w, output.size.h))
        }

        fn place(
            &mut self,
            _window: &WindowInfo,
            _size: (i32, i32),
            outputs: &[Rectangle<i32, Logical>],
        ) -> Point<i32, Logical> {
            outputs
                .first()
                .map_or_else(Point::default, |output| output.loc)
        }

        fn arrange(
            &mut self,
            windows: &[(WindowInfo, Rectangle<i32, Logical>)],
            outputs: &[Rectangle<i32, Logical>],
        ) -> Option<Vec<Arrangement>> {
            let output = outputs.first()?;
            let changes: Vec<_> = windows
                .iter()
                .filter(|(_, rect)| *rect != Rectangle::new(output.loc, output.size))
                .map(|(info, _)| Arrangement {
                    id: info.id,
                    location: output.loc,
                    size: Some((output.size.w, output.size.h)),
                })
                .collect();
            (!changes.is_empty()).then_some(changes)
        }
    }

    /// The session's window: it is killed if it is still running when the test
    /// ends, so a failed run leaves nothing behind.
    struct Session(Child);

    impl Drop for Session {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    pub(crate) fn main() {
        let mut outer = Compositor::headless(Box::new(Fill)).expect("compositor");
        outer
            .add_output(&OutputConfig::new("HEADLESS-1", 800, 600))
            .expect("output");
        let socket = outer.listen().expect("socket");
        // By its full path: the session gets a runtime directory of its own,
        // so it cannot find the socket by name.
        let socket = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
            || PathBuf::from(&socket),
            |directory| Path::new(&directory).join(&socket),
        );

        let directory = tempfile::tempdir().expect("runtime directory");
        let log = directory.path().join("session.log");
        let mut session = Session(
            Command::new(binary())
                .args(["--backend", "nested", "--size", "800x600"])
                // Nothing of this desktop: no bus, and a runtime directory of
                // its own.
                .args(["--session-bus", "none", "--system-bus", "none"])
                .env("WAYLAND_DISPLAY", &socket)
                .env("XDG_RUNTIME_DIR", directory.path())
                .env_remove("DISPLAY")
                .stdout(Stdio::null())
                .stderr(std::fs::File::create(&log).expect("log"))
                .spawn()
                .expect("session"),
        );

        let started = Instant::now();
        let Some(window) = pump_until(&mut outer, &mut session, OPENING, |compositor, _| {
            compositor.windows().into_iter().next()
        }) else {
            let report = std::fs::read_to_string(&log).unwrap_or_default();
            eprintln!(
                "the session showed no window here (no GPU to draw one):\n{}",
                report
                    .chars()
                    .rev()
                    .take(2000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>()
            );
            return;
        };
        eprintln!(
            "the window came up in {:?} at {:?}",
            started.elapsed(),
            (window.1.2, window.1.3)
        );

        // Nothing happens, then the pointer moves across the window: what the
        // session costs the machine while it waits, and while it follows the
        // pointer.
        let (idle, passes) = during(&mut outer, &session, IDLE, |_| None);
        eprintln!(
            "{IDLE:?} idle: the session used {} of CPU over {passes} frames",
            cpu(idle)
        );
        let (moving, passes) = during(&mut outer, &session, IDLE, |step| {
            let across = f64::from(u32::try_from(step % 40).unwrap_or(0));
            Some(InputEvent::PointerMotion {
                x: 20.0 + across * 15.0,
                y: 100.0,
            })
        });
        eprintln!(
            "{IDLE:?} following the pointer: the session used {} of CPU over {passes} frames",
            cpu(moving)
        );

        // A resize of the output the window fills: the session is asked for a
        // bigger window and must draw it at that size.
        let resized = Instant::now();
        outer
            .configure_outputs(&[OutputChange {
                mode: Some(ModeRequest {
                    size: (1000, 700),
                    refresh_mhz: None,
                }),
                ..OutputChange::enable("HEADLESS-1")
            }])
            .expect("resize");
        let grown = pump_until(&mut outer, &mut session, RESIZING, |compositor, _| {
            compositor
                .windows()
                .into_iter()
                .find(|(_, rect)| (rect.2, rect.3) == (1000, 700))
        });
        let Some((_, (_, _, width, height))) = grown else {
            panic!("the session never drew its window at 1000x700");
        };
        let resized = resized.elapsed();
        eprintln!("the session drew {width}x{height} after {resized:?}");
        assert!(resized <= PROMPT, "the resize took {resized:?}");

        // Closing the window, as its own close button does.
        let closed = Instant::now();
        outer.close_window(window.0.id);
        let ended = pump_until(&mut outer, &mut session, CLOSING, |_, session| {
            session
                .0
                .try_wait()
                .expect("wait")
                .map(|status| (status, Instant::now()))
        });
        let Some((status, _)) = ended else {
            panic!("closing the window did not end the session");
        };
        let closed = closed.elapsed();
        eprintln!("the session ended {closed:?} after its window was closed ({status})");
        assert!(closed <= PROMPT, "closing took {closed:?}");
    }

    /// The session binary, beside this example's own directory (`cargo build
    /// -p tilo-session` puts it there).
    fn binary() -> PathBuf {
        std::env::current_exe().map_or_else(
            |_| PathBuf::from("tilo-session"),
            |example| {
                example.parent().and_then(Path::parent).map_or_else(
                    || PathBuf::from("tilo-session"),
                    |target| target.join("tilo-session"),
                )
            },
        )
    }

    /// Run `compositor` for `for_`, sending each pass what `event` makes of
    /// it, and answer how much CPU the session used meanwhile, and how many
    /// passes there were.
    fn during(
        compositor: &mut Compositor,
        session: &Session,
        for_: Duration,
        mut event: impl FnMut(usize) -> Option<InputEvent>,
    ) -> (Duration, usize) {
        let before = cpu_time(&session.0);
        let started = Instant::now();
        let mut passes = 0;
        while started.elapsed() < for_ {
            if let Some(event) = event(passes) {
                compositor.input(event).expect("input");
            }
            compositor.dispatch(REFRESH).expect("dispatch");
            let _ = compositor.render();
            passes = passes.saturating_add(1);
        }
        (cpu_time(&session.0).saturating_sub(before), passes)
    }

    /// The CPU time the process has used, from `/proc/<pid>/stat`.
    fn cpu_time(session: &Child) -> Duration {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", session.id())) else {
            return Duration::ZERO;
        };
        // The name in parentheses may hold spaces; the fields after it do not.
        let Some((_, fields)) = stat.rsplit_once(')') else {
            return Duration::ZERO;
        };
        // Each field after the name, from the state on: the user and system
        // times are the twelfth and thirteenth.
        let fields: Vec<&str> = fields.split_whitespace().collect();
        let ticks: u64 = [fields.get(11), fields.get(12)]
            .into_iter()
            .flatten()
            .filter_map(|field| field.parse::<u64>().ok())
            .sum();
        // The kernel counts CPU time in hundredths of a second.
        Duration::from_millis(ticks.saturating_mul(10))
    }

    /// A duration as milliseconds.
    fn cpu(used: Duration) -> String {
        format!("{} ms", used.as_millis())
    }

    /// Pump `compositor` until `done` has an answer, the session exits, or
    /// `within` passes.
    fn pump_until<T>(
        compositor: &mut Compositor,
        session: &mut Session,
        within: Duration,
        mut done: impl FnMut(&mut Compositor, &mut Session) -> Option<T>,
    ) -> Option<T> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(answer) = done(compositor, session) {
                return Some(answer);
            }
            let ended = session.0.try_wait().expect("wait").is_some();
            if ended || Instant::now() >= deadline {
                return done(compositor, session);
            }
            compositor
                .dispatch(Duration::from_millis(2))
                .expect("dispatch");
            let _ = compositor.render();
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

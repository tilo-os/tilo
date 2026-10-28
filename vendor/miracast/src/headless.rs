//! A headless screen for tests: a private PipeWire daemon and session
//! manager serving a live video node, the way a compositor serves a screen
//! cast. Its node and a connection to it are what the ScreenCast portal hands
//! out, so a test can feed the production capture path
//! ([`VideoInput::PipeWire`](crate::media::VideoInput::PipeWire)) without a
//! desktop.
//!
//! Needs `pipewire`, `wireplumber`, `pw-dump` and GStreamer's PipeWire plugin.
//! Everything runs in a private runtime directory: nothing touches the
//! user's own PipeWire or session bus.

use std::fmt;
use std::fs;
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;

const NODE_NAME: &str = "miracast-headless-screen";
const STARTUP: Duration = Duration::from_secs(20);

/// Why the headless screen did not start.
#[derive(Debug)]
pub struct HeadlessError(String);

impl fmt::Display for HeadlessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HeadlessError {}

fn failed(message: impl Into<String>) -> HeadlessError {
    HeadlessError(message.into())
}

/// A running headless screen. Dropping it stops everything it started.
pub struct HeadlessScreen {
    runtime: PathBuf,
    daemons: Vec<Child>,
    provider: Option<gst::Pipeline>,
    node: u32,
}

impl fmt::Debug for HeadlessScreen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeadlessScreen")
            .field("runtime", &self.runtime)
            .field("node", &self.node)
            .finish_non_exhaustive()
    }
}

impl HeadlessScreen {
    /// Starts PipeWire and WirePlumber in a private runtime directory and a
    /// `width`x`height` test pattern node at `fps` in it.
    ///
    /// # Errors
    ///
    /// A program is missing or did not come up in time.
    pub fn start(width: u32, height: u32, fps: u32) -> Result<Self, HeadlessError> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        gst::init().map_err(|e| failed(format!("GStreamer could not start: {e}")))?;
        let runtime = std::env::temp_dir().join(format!(
            "miracast-pw-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&runtime);
        fs::create_dir_all(runtime.join("home"))
            .and_then(|()| fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)))
            .map_err(|e| failed(format!("cannot create {}: {e}", runtime.display())))?;
        let mut screen = Self {
            runtime,
            daemons: Vec::new(),
            provider: None,
            node: 0,
        };
        screen.spawn("pipewire")?;
        let socket = screen.runtime.join("pipewire-0");
        wait_for(|| socket.exists(), "the PipeWire socket")?;
        screen.spawn("wireplumber")?;
        let provider = provide(&screen.connect()?, width, height, fps)?;
        screen.provider = Some(provider);
        let runtime = screen.runtime.clone();
        let mut node = None;
        wait_for(
            || {
                node = node_id(&runtime);
                node.is_some()
            },
            "the screen node",
        )?;
        screen.node = node.unwrap_or_default();
        Ok(screen)
    }

    /// The node to capture.
    #[must_use]
    pub fn node(&self) -> u32 {
        self.node
    }

    /// A new connection to the private PipeWire, as `OpenPipeWireRemote`
    /// returns.
    ///
    /// # Errors
    ///
    /// The daemon is gone.
    pub fn connect(&self) -> Result<OwnedFd, HeadlessError> {
        UnixStream::connect(self.runtime.join("pipewire-0"))
            .map(OwnedFd::from)
            .map_err(|e| failed(format!("cannot reach the private PipeWire: {e}")))
    }

    fn spawn(&mut self, program: &str) -> Result<(), HeadlessError> {
        let log = fs::File::create(self.runtime.join(format!("{program}.log")))
            .map_err(|e| failed(format!("cannot create the {program} log: {e}")))?;
        let child = environment(Command::new(program), &self.runtime)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .map_err(|e| failed(format!("cannot start {program}: {e}")))?;
        self.daemons.push(child);
        Ok(())
    }
}

impl Drop for HeadlessScreen {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take() {
            let _ = provider.set_state(gst::State::Null);
        }
        for daemon in self.daemons.iter_mut().rev() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
        let _ = fs::remove_dir_all(&self.runtime);
    }
}

/// The private environment every program of the screen runs in.
fn environment(mut command: Command, runtime: &Path) -> Command {
    let home = runtime.join("home");
    command
        .env("XDG_RUNTIME_DIR", runtime)
        .env("PIPEWIRE_RUNTIME_DIR", runtime)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_DATA_HOME", home.join("data"))
        // No bus: the daemons must not reach a desktop session's services.
        .env("DBUS_SESSION_BUS_ADDRESS", "disabled:")
        .env_remove("DBUS_SYSTEM_BUS_ADDRESS")
        .env_remove("PIPEWIRE_REMOTE");
    command
}

fn wait_for(mut ready: impl FnMut() -> bool, what: &str) -> Result<(), HeadlessError> {
    let deadline = Instant::now() + STARTUP;
    while !ready() {
        if Instant::now() >= deadline {
            return Err(failed(format!("{what} did not appear within {STARTUP:?}")));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn provide(
    remote: &OwnedFd,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<gst::Pipeline, HeadlessError> {
    use std::os::fd::AsRawFd;
    let make = |name: &str| {
        gst::ElementFactory::make(name)
            .build()
            .map_err(|_| failed(format!("the GStreamer element {name} is not installed")))
    };
    let source = make("videotestsrc")?;
    source.set_property("is-live", true);
    source.set_property_from_str("pattern", "smpte");
    let caps = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("format", "BGRx")
                .field("width", i32::try_from(width).unwrap_or(1280))
                .field("height", i32::try_from(height).unwrap_or(720))
                .field(
                    "framerate",
                    gst::Fraction::new(i32::try_from(fps).unwrap_or(30), 1),
                )
                .build(),
        )
        .build()
        .map_err(|_| failed("the GStreamer element capsfilter is not installed"))?;
    let sink = make("pipewiresink")?;
    // pipewiresink duplicates the descriptor; the caller keeps its own.
    sink.set_property("fd", remote.as_raw_fd());
    sink.set_property_from_str("mode", "provide");
    sink.set_property(
        "stream-properties",
        gst::Structure::builder("props")
            .field("media.class", "Video/Source")
            .field("node.name", NODE_NAME)
            .build(),
    );
    let pipeline = gst::Pipeline::new();
    pipeline
        .add_many([&source, &caps, &sink])
        .and_then(|()| gst::Element::link_many([&source, &caps, &sink]))
        .map_err(|e| failed(format!("cannot build the screen: {e}")))?;
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|_| failed("the screen pipeline did not start"))?;
    Ok(pipeline)
}

/// The id of the screen node, from `pw-dump`'s JSON: the nearest top-level
/// `"id"` before the node's name.
fn node_id(runtime: &Path) -> Option<u32> {
    let output = environment(Command::new("pw-dump"), runtime)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let dump = String::from_utf8(output.stdout).ok()?;
    let name = dump.find(&format!("\"node.name\": \"{NODE_NAME}\""))?;
    let id = dump[..name].rfind("\"id\": ")?;
    dump[id + 6..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

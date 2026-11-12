//! A workspace's render service: the one toolkit-app process that draws on
//! the GPU.
//!
//! Toolkit apps record each frame as a vello scene, as always, and hand it
//! to their workspace's render service instead of drawing it themselves
//! (`tilo-render-protocol`). The service validates the scene, rasterizes it
//! on a device it opened once — warm pipelines, warm glyph cache, shared by
//! every app of the workspace — into a dma-buf, and the app attaches that
//! buffer to its own surface, so the compositor composites or scans it out
//! like any client's.
//!
//! **One service per workspace.** The person's private workspaces share one;
//! every other workspace has its own, so an app in a workspace with
//! strangers cannot touch the rendering, caches or GPU memory of another
//! workspace. The session's render broker starts them, restarts one that
//! dies (apps keep showing their last frame and send their whole scene
//! again), and decides which service draws an app from the workspace the
//! launcher named, never from what the app says.
//!
//! The service runs as a mode of the session binary (`tilo-session
//! __render-service`, [`dispatch`]): it inherits nothing but its broker
//! socket, opens the compositor's render node, builds its pipelines, then
//! confines itself (`confine`): Landlock to the render node, fonts and
//! driver libraries, and seccomp without sockets or exec.

pub mod broker;
pub mod buffers;
#[cfg(target_os = "linux")]
mod confine;
mod gpu;
mod registry;
mod service;
mod surface;

use std::path::PathBuf;

pub use gpu::{Gpu, GpuError};
pub use registry::FontFiles;
pub use service::Service;
use tilo_render_protocol::Limits;
#[cfg(target_os = "linux")]
use tilo_render_protocol::channel::Channel;

/// Environment variable naming the render node the service opens
/// (`/dev/dri/renderD128`) and allocates its buffers on; unset or empty
/// draws on the CPU that wgpu offers into shared memory.
pub const RENDER_NODE_ENV: &str = "TILO_RENDER_NODE";

/// Environment variable listing the font directories apps may name fonts
/// in, separated by `:`.
pub const FONT_ROOTS_ENV: &str = "TILO_RENDER_FONT_ROOTS";

/// Font directories when none are named: the system's, which hold the
/// toolkit's catalog (`/usr/share/fonts/tilo`).
pub const DEFAULT_FONT_ROOTS: &[&str] = &["/usr/share/fonts", "/usr/local/share/fonts"];

/// How a service is started.
#[derive(Clone, Debug)]
pub struct Config {
    pub render_node: Option<PathBuf>,
    pub font_roots: Vec<PathBuf>,
    pub limits: Limits,
    /// Enter the sandbox once the device is open. Off only in tests that run
    /// the service on a thread of the test process.
    pub confine: bool,
}

impl Config {
    /// From the environment the broker started the service with.
    pub fn from_env() -> Self {
        let render_node = std::env::var_os(RENDER_NODE_ENV)
            .filter(|node| !node.is_empty())
            .map(PathBuf::from);
        let font_roots = std::env::var_os(FONT_ROOTS_ENV)
            .filter(|roots| !roots.is_empty())
            .map_or_else(
                || DEFAULT_FONT_ROOTS.iter().map(PathBuf::from).collect(),
                |roots| std::env::split_paths(&roots).collect(),
            );
        Self {
            render_node,
            font_roots,
            limits: Limits::default(),
            confine: true,
        }
    }
}

/// Open the device every frame is drawn on.
///
/// # Errors
///
/// When wgpu offers no adapter at all: the service has nothing to draw on,
/// and ends, so the broker's next request starts another rather than the app
/// waiting on a service that cannot draw.
pub fn open_gpu(config: &Config) -> Result<Gpu, GpuError> {
    let gpu = Gpu::open(config.render_node.as_deref())?;
    if gpu.software {
        tracing::info!(adapter = %gpu.adapter.name, "software adapter: drawing on the CPU");
    }
    Ok(gpu)
}

/// Serve the broker on `broker` with `config`, on this thread, until the
/// broker goes away.
#[cfg(target_os = "linux")]
pub fn serve(config: &Config, broker: &Channel) -> Result<(), String> {
    let gpu = open_gpu(config).map_err(|error| format!("no device to draw on: {error}"))?;
    let fonts = FontFiles::new(&config.font_roots);
    if config.confine {
        let node = config.render_node.as_ref().filter(|_| gpu.dmabufs());
        confine::confine(node, fonts.roots()).map_err(|error| error.to_string())?;
    }
    Service::new(gpu, fonts, config.limits)
        .run(broker)
        .map_err(|error| error.to_string())
}

/// When `args` — the session's arguments after the program's name — start
/// it in render-service mode, run it and return its exit code.
#[cfg(target_os = "linux")]
pub fn dispatch(args: &[String]) -> Option<std::process::ExitCode> {
    if args.first().is_none_or(|arg| arg != broker::MODE) {
        return None;
    }
    Some(run_from_env())
}

#[cfg(target_os = "linux")]
fn run_from_env() -> std::process::ExitCode {
    use std::os::fd::{FromRawFd, OwnedFd};
    let Some(fd) = std::env::var(broker::BROKER_FD_ENV)
        .ok()
        .and_then(|fd| fd.parse::<i32>().ok())
        .filter(|fd| *fd > 2)
    else {
        eprintln!("render service: {} names no socket", broker::BROKER_FD_ENV);
        return std::process::ExitCode::FAILURE;
    };
    confine::close_inherited(fd);
    // SAFETY: the broker started this process with its socket at `fd` and
    // nothing else in the process owns that descriptor.
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    let broker = match Channel::from_socket(socket) {
        Ok(broker) => broker,
        Err(error) => {
            eprintln!("render service: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match serve(&Config::from_env(), &broker) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("render service: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

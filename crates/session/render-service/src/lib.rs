//! A room's render service: the one toolkit-app process that draws on the
//! GPU.
//!
//! Toolkit apps record each frame as a vello scene, as always, and hand it
//! to their room's render service instead of drawing it themselves
//! (`tilo-render-protocol`). The service validates the scene, rasterizes it
//! on a device it opened once — warm pipelines, warm glyph cache, shared by
//! every app of the room — into a dma-buf, and the app attaches that buffer
//! to its own surface, so the compositor composites or scans it out like any
//! client's. `docs/architecture/app-rendering.md` has the design;
//! `docs/architecture/security.md` "App rendering" the boundary.
//!
//! **One service per room.** The person's private rooms share one; every
//! other room has its own, so an app in a room with strangers cannot touch
//! the rendering, caches or GPU memory of another room. The compositor's
//! broker starts them, restarts one that dies (apps keep showing their last
//! frame and send their whole scene again), and decides which service draws
//! an app from the room the launcher named, never from what the app says.
//!
//! The service runs as a mode of the shell binary (`shell __render-service`,
//! [`dispatch`]): it inherits nothing but its broker socket, opens the
//! compositor's render node, builds its pipelines, then confines itself
//! (`confine`): Landlock to the render node, fonts and driver libraries, and
//! seccomp without sockets or exec.

pub mod broker;
mod buffers;
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
/// (`/dev/dri/renderD128`); unset or empty draws on the CPU into shared
/// memory.
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

/// Open the device the config names, or none: a software adapter, or no GPU,
/// draws on the CPU into shared memory.
pub fn open_gpu(config: &Config) -> Option<Gpu> {
    let node = config.render_node.as_deref()?;
    match Gpu::open(Some(node)) {
        Ok(gpu) if gpu.dmabufs() => Some(gpu),
        Ok(gpu) => {
            tracing::info!(adapter = %gpu.adapter.name, "software adapter: drawing on the CPU");
            None
        }
        Err(error) => {
            tracing::warn!(%error, "no GPU for the render service: drawing on the CPU");
            None
        }
    }
}

/// Serve the broker on `broker` with `config`, on this thread, until the
/// broker goes away.
#[cfg(target_os = "linux")]
pub fn serve(config: &Config, broker: &Channel) -> Result<(), String> {
    let gpu = open_gpu(config);
    let fonts = FontFiles::new(&config.font_roots);
    if config.confine {
        let node = gpu.as_ref().and(config.render_node.as_ref());
        confine::confine(node, fonts.roots()).map_err(|error| error.to_string())?;
    }
    Service::new(gpu, fonts, config.limits)
        .run(broker)
        .map_err(|error| error.to_string())
}

/// When `args` start the shell binary in render-service mode, run it and
/// return its exit code.
#[cfg(target_os = "linux")]
pub fn dispatch(args: &[String]) -> Option<std::process::ExitCode> {
    if args.get(1).is_none_or(|arg| arg != broker::MODE) {
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

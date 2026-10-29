//! Tilo's Wayland compositor.
//!
//! It shows buffers and routes input; it draws no interface of its own.
//! Client surfaces and the layers of hosted surface apps ([`SurfaceHost`])
//! are composited per output ([`render`]); where windows go and who has focus
//! is a [`WindowPolicy`]'s decision; backends supply outputs and input
//! ([`headless`] here: CPU composition with no display, for tests and
//! screenshots).
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use tilo_compositor::{Compositor, OutputConfig, StackPolicy};
//! let mut compositor = Compositor::headless(Box::new(StackPolicy))?;
//! compositor.add_output(&OutputConfig::new("HEADLESS-1", 1280, 720))?;
//! let socket = compositor.connect_client()?; // hand to a client
//! compositor.dispatch(std::time::Duration::from_millis(16))?;
//! compositor.render()?;
//! let frame = compositor.screenshot("HEADLESS-1")?;
//! # drop((socket, frame)); Ok(()) }
//! ```

#![cfg(target_os = "linux")]

pub mod headless;
pub mod host;
pub mod input;
pub mod output;
pub mod policy;
pub mod render;
mod state;

use std::os::unix::net::UnixStream;
use std::time::Duration;

use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{EventLoop, Interest, Mode, PostAction};
use smithay::reexports::wayland_server::Display;

pub use headless::{RenderError, Screenshot};
pub use host::{HostLayer, SurfaceHost};
pub use input::InputEvent;
pub use output::OutputConfig;
pub use policy::{StackPolicy, WindowInfo, WindowPolicy};
pub use state::State;

/// Why the compositor could not start or keep running.
#[derive(Debug)]
pub enum Error {
    /// The event loop or a client socket failed.
    Io(std::io::Error),
    /// A frame could not be drawn or read back.
    Render(RenderError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "compositor I/O: {error}"),
            Self::Render(error) => write!(f, "compositor render: {error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<RenderError> for Error {
    fn from(error: RenderError) -> Self {
        Self::Render(error)
    }
}

/// A running compositor: the Wayland display, its state and a backend.
pub struct Compositor {
    event_loop: EventLoop<'static, State>,
    display: Display<State>,
    state: State,
    backend: headless::HeadlessBackend,
}

impl Compositor {
    /// A compositor whose outputs have no display (see [`headless`]).
    pub fn headless(policy: Box<dyn WindowPolicy>) -> Result<Self, Error> {
        let event_loop =
            EventLoop::try_new().map_err(|error| Error::Io(std::io::Error::other(error)))?;
        let mut display =
            Display::<State>::new().map_err(|error| Error::Io(std::io::Error::other(error)))?;
        let state = State::new(&display.handle(), policy);
        // Client requests are read after the loop wakes, in `dispatch`; the
        // loop only needs to wake when the display's socket is readable.
        let poll_fd = display.backend().poll_fd().try_clone_to_owned()?;
        event_loop
            .handle()
            .insert_source(
                Generic::new(poll_fd, Interest::READ, Mode::Level),
                |_, _, _| Ok(PostAction::Continue),
            )
            .map_err(|error| Error::Io(std::io::Error::other(error.to_string())))?;
        Ok(Self {
            event_loop,
            display,
            state,
            backend: headless::HeadlessBackend::new()?,
        })
    }

    /// Add an output.
    pub fn add_output(&mut self, config: &OutputConfig) -> Result<(), Error> {
        self.backend.add_output(&mut self.state, config)?;
        Ok(())
    }

    /// Host a surface app above the windows (and above earlier hosts).
    pub fn add_host(&mut self, host: Box<dyn SurfaceHost>) {
        self.state.hosts.push(host);
    }

    /// Connect a client over a socket pair; the returned end is the
    /// client's (`wl_display` connection).
    pub fn connect_client(&mut self) -> Result<UnixStream, Error> {
        let (ours, theirs) = UnixStream::pair()?;
        self.display
            .handle()
            .insert_client(ours, state::client_data())
            .map_err(|error| Error::Io(std::io::Error::other(error)))?;
        Ok(theirs)
    }

    /// Wait up to `timeout` for client requests, handle them and flush the
    /// answers.
    pub fn dispatch(&mut self, timeout: Duration) -> Result<(), Error> {
        self.event_loop
            .dispatch(Some(timeout), &mut self.state)
            .map_err(|error| Error::Io(std::io::Error::other(error)))?;
        self.display.dispatch_clients(&mut self.state)?;
        self.state.refresh();
        self.display.flush_clients()?;
        Ok(())
    }

    /// Route one input event (see [`InputEvent`]) and flush what it sent.
    pub fn input(&mut self, event: InputEvent) -> Result<(), Error> {
        self.state.handle_input(event);
        self.display.flush_clients()?;
        Ok(())
    }

    /// Draw a frame on every output and tell clients it was shown.
    pub fn render(&mut self) -> Result<(), Error> {
        self.backend.render(&self.state)?;
        self.display.flush_clients()?;
        Ok(())
    }

    /// The last frame drawn on the output named `name`.
    pub fn screenshot(&mut self, name: &str) -> Result<Screenshot, Error> {
        Ok(self.backend.screenshot(name)?)
    }

    /// The mapped windows and their logical rectangles `(x, y, w, h)`,
    /// bottom first.
    pub fn windows(&self) -> Vec<(WindowInfo, (i32, i32, i32, i32))> {
        self.state
            .windows()
            .into_iter()
            .map(|(info, rect)| (info, (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h)))
            .collect()
    }

    /// The window with keyboard focus.
    pub fn focused_window(&self) -> Option<WindowInfo> {
        self.state.focused_window()
    }
}

impl std::os::fd::AsFd for Compositor {
    /// The loop's descriptor, readable when [`Compositor::dispatch`] has
    /// work, for embedding in another loop.
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.event_loop.as_fd()
    }
}

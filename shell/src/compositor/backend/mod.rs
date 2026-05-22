//! Backend trait abstracting frame submission + event polling for the
//! native compositor mode.
//!
//! [`WinitBackend`] (this phase) hosts on top of an existing
//! Wayland/X11 session for development. [`DrmBackend`] (Phase 5) will
//! drive a TTY directly.

use vello::Scene;

pub mod drm;
pub mod headless;
pub mod winit;

pub use drm::DrmBackend;
pub use headless::HeadlessBackend;
pub use winit::WinitBackend;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct OutputId(pub u32);

#[derive(Clone, Debug)]
pub struct Output {
    pub id: OutputId,
    pub size: (u32, u32),
    pub scale: f32,
}

#[derive(Debug)]
pub enum BackendEvent {
    CloseRequested,
    OutputResized { id: OutputId, size: (u32, u32), scale: f32 },
    Input,
    Redraw,
    Focus(bool),
}

pub trait Backend {
    fn submit_frame(&mut self, scene: &Scene, output: OutputId);
    fn poll_events(&mut self, callback: &mut dyn FnMut(BackendEvent));
    fn outputs(&self) -> &[Output];
}

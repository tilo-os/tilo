//! Backend trait abstracting frame submission + event polling for the
//! native compositor mode.
//!
//! [`WinitBackend`] (this phase) hosts on top of an existing
//! Wayland/X11 session for development. [`DrmBackend`] (Phase 5) will
//! drive a TTY directly.

use vello::Scene;
// Absolute path: this module has a `pub mod winit;` child below that
// would otherwise shadow the external `winit` crate.
use ::winit::event::{Modifiers, WindowEvent};

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
    /// A pointer / keyboard / wheel event. The full `winit::WindowEvent`
    /// is carried so `compositor::native::run` can hand it to
    /// `FrameBuilder::dispatch_event` — the same dispatch path the
    /// headless `execute_planned_action` uses.
    ///
    /// Previously a marker variant: the compositor knew "some input
    /// happened" but the event was discarded. That meant the native
    /// backend silently swallowed every click — visible only because
    /// hardware-clicking the window produced no widget reaction
    /// (headless tests bypassed this path entirely, so the bug hid).
    Input(WindowEvent),
    /// Modifier-state changed. Tracked separately because
    /// `WindowEvent::ModifiersChanged` carries a `Modifiers` struct
    /// the loop needs to retain across subsequent events.
    ModifiersChanged(Modifiers),
    Redraw,
    Focus(bool),
}

pub trait Backend {
    fn submit_frame(&mut self, scene: &Scene, output: OutputId);
    fn poll_events(&mut self, callback: &mut dyn FnMut(BackendEvent));
    fn outputs(&self) -> &[Output];
}

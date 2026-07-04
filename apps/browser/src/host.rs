//! Servo↔chrome boundary abstraction.
//!
//! Phase A of the iced→quick-ui migration introduces this trait so the
//! Servo glue (`servo_engine.rs`) no longer depends on the iced runtime
//! directly. Servo is framework-agnostic — it produces RGBA frames via a
//! [`servo::SoftwareRenderingContext`] and signals via an
//! [`servo::EventLoopWaker`]. The host (whichever UI framework is on top)
//! is responsible for displaying frames and pumping the event loop.
//!
//! Two implementors:
//!   * the iced application in `main.rs` (lives during Phase A, dropped in
//!     Phase D)
//!   * the quick-ui application that lands in Phase B
//!
//! The trait is deliberately narrow. It carries only the two things the
//! engine actually needs:
//!   * `submit_frame` — Servo just rendered new bytes, please display them.
//!   * `wake`         — Servo wants the event loop to spin again soon.
//!
//! Note: the original RFC sketched a `servo_surface() -> &wgpu::Surface`
//! shape. The real Servo integration in this app uses
//! [`servo::SoftwareRenderingContext`], which is wgpu-free; Servo owns
//! its own GPU resources and hands the host RGBA pixels. The trait below
//! matches that reality.

use std::sync::Arc;

/// Bridge from the Servo engine to the surrounding UI framework.
///
/// Implementors must be cheap to clone (typical implementations wrap an
/// `Arc<Mutex<…>>` around their internal sinks).
pub trait BrowserHost: Send + Sync {
    /// Hand a freshly composited frame from Servo to the host. The host
    /// is expected to display these pixels — typically by storing them
    /// in a widget that paints them on the next draw pass.
    fn submit_frame(&self, rgba: Vec<u8>, width: u32, height: u32);

    /// Ask the host to wake its event loop. Servo calls this from its
    /// internal threads when it needs the embedder to spin the engine
    /// (load completion, idle frames, etc.). May be called from any
    /// thread.
    fn wake(&self);
}

/// Convenience alias — the engine stores its host as a trait object
/// behind an `Arc` so multiple Servo callbacks (frame ready / waker) can
/// share it without lifetime juggling.
pub type SharedHost = Arc<dyn BrowserHost>;

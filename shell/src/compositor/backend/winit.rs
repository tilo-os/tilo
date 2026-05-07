//! Smithay's winit backend hosts the compositor on top of an existing
//! Wayland/X11 session — the dev/test path. Headless TTY is `DrmBackend`
//! (Phase 5).
//!
//! Phase 3 only constructs the backend and converts smithay's
//! [`WinitEvent`]s into [`BackendEvent`]s. Frame submission is wired
//! in Phase 4 once the per-frame scene production pipeline lifts out
//! of `bin/shots`.

use smithay::backend::renderer::glow::GlowRenderer;
use smithay::backend::winit::{self, WinitEvent, WinitEventLoop, WinitGraphicsBackend};
use vello::Scene;

use super::{Backend, BackendEvent, Output, OutputId};

pub struct WinitBackend {
    graphics: WinitGraphicsBackend<GlowRenderer>,
    winit_loop: WinitEventLoop,
    outputs: Vec<Output>,
}

impl WinitBackend {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let (graphics, winit_loop) =
            winit::init::<GlowRenderer>().map_err(|e| format!("winit init: {e:?}"))?;
        let size = graphics.window_size();
        let outputs = vec![Output {
            id: OutputId(0),
            size: (size.w.max(0) as u32, size.h.max(0) as u32),
            scale: 1.0,
        }];
        Ok(Self { graphics, winit_loop, outputs })
    }
}

impl Backend for WinitBackend {
    fn submit_frame(&mut self, _scene: &Scene, _output: OutputId) {
        // Phase 4: bind the EGL surface, encode the scene via vello,
        // swap buffers. For now we keep a borrow of `graphics` to
        // ensure it isn't dropped between ticks.
        let _ = &self.graphics;
    }

    fn poll_events(&mut self, callback: &mut dyn FnMut(BackendEvent)) {
        let _ = self.winit_loop.dispatch_new_events(|event| match event {
            WinitEvent::CloseRequested => callback(BackendEvent::CloseRequested),
            WinitEvent::Resized { size, scale_factor } => {
                callback(BackendEvent::OutputResized {
                    id: OutputId(0),
                    size: (size.w.max(0) as u32, size.h.max(0) as u32),
                    scale: scale_factor as f32,
                });
            }
            WinitEvent::Input(_) => callback(BackendEvent::Input),
            WinitEvent::Focus(focused) => callback(BackendEvent::Focus(focused)),
            WinitEvent::Redraw => callback(BackendEvent::Redraw),
        });
    }

    fn outputs(&self) -> &[Output] { &self.outputs }
}

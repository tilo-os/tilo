//! A toolkit widget shown on a rectangle of the desktop: a surface that is
//! a widget tree of its own rather than an app (the desktop's strips and
//! window bars). The toolkit's portable host runs it (input in, frames
//! out) and the frame is rasterized on the CPU as an app's is.

use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use tilo_compositor::{HostLayer, SurfaceHost};
use ui::headless::FrameHarness;
use vello::kurbo::Affine;
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent};

use crate::canvas::Canvas;
use crate::{Placement, RASTER_THREADS, buffer_for, mouse_input};

/// One widget tree shown on a rectangle of the desktop.
pub struct WidgetHost {
    host: FrameHarness,
    placement: Placement,
    canvas: Canvas,
    buffer: MemoryRenderBuffer,
    scene: vello::Scene,
    below: bool,
    /// A frame is owed: input arrived or the placement changed.
    owed: bool,
}

impl WidgetHost {
    /// Show `root` at `placement`, above the windows or (`below`) under
    /// them. Nothing is drawn before the first
    /// [`SurfaceHost::prepare_frame`].
    ///
    /// # Panics
    /// Never: the placement's scale is a positive output scale.
    #[must_use]
    pub fn new(root: Box<dyn ui::Widget>, placement: Placement, below: bool) -> Self {
        let mut host = FrameHarness::new_with_controlled_inputs(
            ui::state::State::new(),
            root,
            placement.size,
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId(String::new()),
        );
        host.set_caller_palette_enabled(false);
        let _ = host.set_input_scale(placement.scale);
        let size = placement.physical_size();
        Self {
            host,
            placement,
            canvas: Canvas::new(size, RASTER_THREADS),
            buffer: buffer_for(size),
            scene: vello::Scene::new(),
            below,
            owed: true,
        }
    }

    /// The toolkit's host of the widget tree, to drive it directly.
    pub fn host_mut(&mut self) -> &mut FrameHarness {
        &mut self.host
    }

    /// Where the tree is shown.
    pub const fn placement(&self) -> Placement {
        self.placement
    }

    /// Show the tree on another rectangle or at another scale.
    pub fn set_placement(&mut self, placement: Placement) {
        if placement == self.placement {
            return;
        }
        if placement.size != self.placement.size {
            let _ = self.host.resize(placement.size);
        }
        let _ = self.host.set_input_scale(placement.scale);
        let size = placement.physical_size();
        if size != self.placement.physical_size() {
            self.canvas = Canvas::new(size, RASTER_THREADS);
            self.buffer = buffer_for(size);
        }
        self.placement = placement;
        self.owed = true;
    }

    /// Lay the tree out and draw it now, whether or not a frame is due:
    /// what a caller reading its layout needs first.
    pub fn relayout(&mut self) {
        self.host.relayout();
        self.owed = true;
    }

    /// A logical point of the desktop in the tree's physical pixels.
    fn local(&self, x: f64, y: f64) -> (f64, f64) {
        let (ox, oy) = self.placement.origin;
        (
            (x - f64::from(ox)) * self.placement.scale,
            (y - f64::from(oy)) * self.placement.scale,
        )
    }

    fn input(&mut self, event: &WindowEvent) {
        self.host.send(event);
        self.owed = true;
    }
}

impl SurfaceHost for WidgetHost {
    fn layers(&self) -> Vec<HostLayer> {
        vec![HostLayer {
            location: self.placement.origin,
            size: (
                i32::try_from(self.placement.size.0).unwrap_or(i32::MAX),
                i32::try_from(self.placement.size.1).unwrap_or(i32::MAX),
            ),
            buffer: self.buffer.clone(),
        }]
    }

    fn below_windows(&self) -> bool {
        self.below
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        let (lx, ly) = self.local(x, y);
        if lx < 0.0 || ly < 0.0 {
            return false;
        }
        if self.below {
            // The desktop under the windows takes the pointer wherever the
            // compositor offers it: there is nothing beneath it.
            let (width, height) = self.canvas.size();
            return lx < f64::from(width) && ly < f64::from(height);
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a non-negative point inside a surface on a screen"
        )]
        let (px, py) = (lx as u32, ly as u32);
        self.canvas.alpha(px, py).is_some_and(|alpha| alpha > 0)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        let (lx, ly) = self.local(x, y);
        self.input(&WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(lx, ly),
        });
    }

    fn pointer_leave(&mut self) {
        self.input(&WindowEvent::CursorLeft {
            device_id: DeviceId::dummy(),
        });
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.input(&mouse_input(button, pressed));
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        let scale = self.placement.scale;
        self.input(&WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                -horizontal * scale,
                -vertical * scale,
            )),
            phase: TouchPhase::Moved,
        });
    }

    fn prepare_frame(&mut self) {
        let drew = self.host.poll_frame().unwrap_or(false);
        if !(drew || std::mem::take(&mut self.owed)) {
            return;
        }
        self.scene.reset();
        self.scene
            .append(self.host.scene(), Some(Affine::scale(self.placement.scale)));
        self.canvas.present(&self.scene, None, &mut self.buffer);
    }
}

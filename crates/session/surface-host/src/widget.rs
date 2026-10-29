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

use std::cell::Cell;
use std::rc::Rc;

use tilo_compositor::HostKey;
use winit::event::Modifiers;
use winit::keyboard::ModifiersState;

use crate::canvas::Canvas;
use crate::{Placement, RASTER_THREADS, buffer_for, key_event, modifiers_state, mouse_input};

/// One widget tree shown on a rectangle of the desktop.
pub struct WidgetHost {
    host: FrameHarness,
    placement: Placement,
    canvas: Canvas,
    buffer: MemoryRenderBuffer,
    scene: vello::Scene,
    below: bool,
    /// Set by the tree while keys should come to it rather than to the
    /// focused window (an open menu or field).
    keyboard: Option<Rc<Cell<bool>>>,
    /// The modifiers the tree last heard of.
    modifiers: ModifiersState,
    /// The tree covers the outputs' bounding box, following them.
    covers: bool,
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
            keyboard: None,
            modifiers: ModifiersState::empty(),
            covers: false,
            owed: true,
        }
    }

    /// Cover every output, following them as they come and go: chrome
    /// over the whole desktop.
    #[must_use]
    pub const fn covering_outputs(mut self) -> Self {
        self.covers = true;
        self
    }

    /// Take keys from the focused window while `wants` holds: the tree
    /// sets it while a menu or field of its own is open.
    #[must_use]
    pub fn with_keyboard(mut self, wants: Rc<Cell<bool>>) -> Self {
        self.keyboard = Some(wants);
        self
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

impl crate::Published for WidgetHost {
    fn resources(&mut self) -> Vec<ui::tilo_actions::ResourceSpec> {
        self.host.resources()
    }

    fn invoke(
        &mut self,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::DoorError> {
        let started = self
            .host
            .invoke_published(&address.resource, &address.action, arguments);
        self.owed = true;
        started
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        self.host
            .a11y_nodes()
            .into_iter()
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect()
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

    fn outputs_changed(&mut self, outputs: &[(i32, i32, i32, i32)]) {
        if !self.covers {
            return;
        }
        let Some((x0, y0, x1, y1)) = outputs
            .iter()
            .map(|&(x, y, w, h)| (x, y, x.saturating_add(w), y.saturating_add(h)))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
        else {
            return;
        };
        let (Ok(width), Ok(height)) = (
            u32::try_from(x1.saturating_sub(x0)),
            u32::try_from(y1.saturating_sub(y0)),
        ) else {
            return;
        };
        self.set_placement(Placement {
            origin: (x0, y0),
            size: (width.max(1), height.max(1)),
            scale: self.placement.scale,
        });
    }

    fn wants_keyboard(&self) -> bool {
        self.keyboard.as_ref().is_some_and(|wants| wants.get())
    }

    fn key(&mut self, key: &HostKey) {
        let modifiers = modifiers_state(key.modifiers);
        if modifiers != self.modifiers {
            self.modifiers = modifiers;
            self.input(&WindowEvent::ModifiersChanged(Modifiers::from(modifiers)));
        }
        self.input(&key_event(key));
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

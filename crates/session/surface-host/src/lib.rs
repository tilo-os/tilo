//! The shell's surface apps in the compositor's process.
//!
//! The top bar, the launcher and the panels are ordinary toolkit apps. An
//! [`AppHost`] shows one of them on a rectangle of the desktop: it drives the
//! app as any host does (input in, frames out), rasterizes each frame on the
//! CPU where it changed, and hands the compositor the buffer as a
//! [`SurfaceHost`] layer. The compositor knows nothing of the app; the app
//! knows nothing of the compositor.

#![cfg(target_os = "linux")]

mod canvas;
mod desktop;
mod door;
mod widget;

pub use desktop::{DeskGesture, DesktopHost, StripPolicy, desktop, desktop_with};
pub use door::{Doors, Published, Shared, through_door};
pub use widget::WidgetHost;

use std::cell::Cell;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::Transform;
use tilo_compositor::{HostKey, HostLayer, KeyModifiers, SurfaceHost};
use ui::App;
use ui::app::{AppSurface, SurfaceDamage};
use ui::device_local_door::SharedDoor;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{
    DeviceId, ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, RawKeyEvent,
    TouchPhase, WindowEvent,
};
use winit::keyboard::{Key, KeyLocation, ModifiersState, PhysicalKey, SmolStr};
use winit::platform::scancode::PhysicalKeyExtScancode as _;

use canvas::Canvas;

/// Cores the rasterizer uses for a large damaged region.
const RASTER_THREADS: u16 = 2;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

/// The rectangle an app is shown on, in the compositor's logical layout,
/// and the output scale it is drawn at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Top-left corner, logical pixels.
    pub origin: (i32, i32),
    /// Size, logical pixels.
    pub size: (u32, u32),
    /// Physical pixels per logical pixel.
    pub scale: f64,
}

impl Placement {
    fn physical_size(self) -> (u32, u32) {
        let scale = |side: u32| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a surface side on a screen, scaled by a small positive factor"
            )]
            let physical = (f64::from(side) * self.scale).round().max(1.0) as u32;
            physical
        };
        (scale(self.size.0), scale(self.size.1))
    }
}

/// The surface the app is drawn on, as the toolkit asks it.
struct Surface {
    size: PhysicalSize<u32>,
    scale: f64,
    redraw: Cell<bool>,
}

impl AppSurface for Surface {
    fn scale_factor(&self) -> f64 {
        self.scale
    }

    fn inner_size(&self) -> PhysicalSize<u32> {
        self.size
    }

    fn request_redraw(&self) {
        self.redraw.set(true);
    }

    fn set_cursor(&self, _cursor: winit::window::CursorIcon) {}
}

/// One toolkit app shown on a rectangle of the desktop.
pub struct AppHost {
    app: App,
    placement: Placement,
    surface: Surface,
    canvas: Canvas,
    buffer: MemoryRenderBuffer,
    scene: vello::Scene,
    damage: SurfaceDamage,
    /// The app asked for another frame when it drew the last.
    wants: bool,
    /// Keys come here rather than to the focused window.
    keyboard: bool,
    /// The modifiers the app last heard of.
    modifiers: ModifiersState,
    /// The app's actions for this device: what it publishes, and what the
    /// session's catalogue asked of it, run on the next frame.
    door: SharedDoor,
}

impl AppHost {
    /// Show `app` at `placement`. Nothing is drawn before the first
    /// [`SurfaceHost::prepare_frame`].
    pub fn new(app: App, placement: Placement) -> Self {
        let (width, height) = placement.physical_size();
        Self {
            app,
            placement,
            surface: Surface {
                size: PhysicalSize::new(width, height),
                scale: placement.scale,
                redraw: Cell::new(true),
            },
            canvas: Canvas::new((width, height), RASTER_THREADS),
            buffer: buffer_for((width, height)),
            scene: vello::Scene::new(),
            damage: SurfaceDamage::default(),
            wants: true,
            keyboard: false,
            modifiers: ModifiersState::empty(),
            door: SharedDoor::default(),
        }
    }

    /// The app's door, shared with the catalogue the person, a script or
    /// an agent on this device drives the app through.
    pub fn door(&self) -> SharedDoor {
        self.door.clone()
    }

    /// Run what was asked through the door now, then publish what the app
    /// offers: the session serves an invocation without waiting for a
    /// frame. Whether anything ran.
    pub fn serve_door(&mut self) -> bool {
        let app = &mut self.app;
        let ran = self
            .door
            .serve_with(|address, arguments| app.invoke_local_resource_action(address, arguments));
        if ran {
            self.wants = true;
        }
        self.door
            .publish_resources(Some(self.app.collect_resources()));
        ran
    }

    /// The app, to hand it events of its own (external events, settings).
    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    /// Where the app is shown.
    pub const fn placement(&self) -> Placement {
        self.placement
    }

    /// Show the app on another rectangle or at another scale.
    pub fn set_placement(&mut self, placement: Placement) {
        if placement == self.placement {
            return;
        }
        let size = placement.physical_size();
        if size != self.placement.physical_size()
            || placement.scale.to_bits() != self.placement.scale.to_bits()
        {
            self.surface.size = PhysicalSize::new(size.0, size.1);
            self.surface.scale = placement.scale;
            self.canvas = Canvas::new(size, RASTER_THREADS);
            self.buffer = buffer_for(size);
            self.app.surface_input(
                &self.surface,
                WindowEvent::Resized(self.surface.size),
                false,
            );
            self.surface.redraw.set(true);
        }
        self.placement = placement;
    }

    /// Take keys from the focused window (`true`, an open launcher or
    /// menu) or leave them to it.
    pub fn set_wants_keyboard(&mut self, wants: bool) {
        if self.keyboard && !wants && !self.modifiers.is_empty() {
            // The app must not think a modifier is still held.
            self.modifiers = ModifiersState::empty();
            self.input(WindowEvent::ModifiersChanged(Modifiers::from(
                self.modifiers,
            )));
        }
        self.keyboard = wants;
    }

    /// A logical point of the desktop in the app's physical pixels.
    fn local(&self, x: f64, y: f64) -> (f64, f64) {
        let (ox, oy) = self.placement.origin;
        (
            (x - f64::from(ox)) * self.placement.scale,
            (y - f64::from(oy)) * self.placement.scale,
        )
    }

    fn input(&mut self, event: WindowEvent) {
        self.app.surface_input(&self.surface, event, false);
        self.wants = true;
    }
}

impl Published for AppHost {
    fn resources(&mut self) -> Vec<ui::tilo_actions::ResourceSpec> {
        self.app.collect_resources()
    }

    fn invoke(
        &mut self,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::DoorError> {
        // Through the app's own door, as every caller on this device.
        let door = self.door.clone();
        through_door(&door, address, arguments, || {
            self.serve_door();
        })
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        self.app.refresh_semantic_tree();
        self.app
            .semantic_tree()
            .iter()
            .filter_map(|node| node.name.clone())
            .collect()
    }
}

impl SurfaceHost for AppHost {
    fn layers(&self) -> Vec<HostLayer> {
        let size = (
            i32::try_from(self.placement.size.0).unwrap_or(i32::MAX),
            i32::try_from(self.placement.size.1).unwrap_or(i32::MAX),
        );
        vec![HostLayer {
            location: self.placement.origin,
            size,
            buffer: self.buffer.clone(),
        }]
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        let (lx, ly) = self.local(x, y);
        if lx < 0.0 || ly < 0.0 {
            return false;
        }
        // Where the app drew nothing, the pointer reaches what lies beneath.
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
        self.input(WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(lx, ly),
        });
    }

    fn pointer_leave(&mut self) {
        self.input(WindowEvent::CursorLeft {
            device_id: DeviceId::dummy(),
        });
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.input(mouse_input(button, pressed));
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        let scale = self.placement.scale;
        self.input(WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            // The compositor scrolls content by what it reports; the toolkit
            // moves content against the wheel's delta.
            delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                -horizontal * scale,
                -vertical * scale,
            )),
            phase: TouchPhase::Moved,
        });
    }

    fn wants_keyboard(&self) -> bool {
        self.keyboard
    }

    fn key(&mut self, key: &HostKey) {
        let modifiers = modifiers_state(key.modifiers);
        if modifiers != self.modifiers {
            self.modifiers = modifiers;
            self.input(WindowEvent::ModifiersChanged(Modifiers::from(modifiers)));
        }
        self.input(key_event(key));
    }

    fn prepare_frame(&mut self) {
        if self.door.has_queued() {
            self.serve_door();
        }
        let now = ui::time::now();
        let due = self.app.surface_next_frame().is_some_and(|at| at <= now);
        if !(self.wants || self.surface.redraw.replace(false) || self.app.wake_pending() || due) {
            return;
        }
        self.app.surface_accessibility();
        self.scene.reset();
        self.wants =
            self.app
                .surface_frame_with_damage(&self.surface, &mut self.scene, &mut self.damage);
        let changed = self.damage.take();
        self.canvas
            .present(&self.scene, changed.as_deref(), &mut self.buffer);
        self.door
            .publish_resources(Some(self.app.collect_resources()));
    }
}

/// An empty (transparent) buffer of `size` physical pixels; the layer's
/// logical size scales it onto the output.
pub(crate) fn buffer_for(size: (u32, u32)) -> MemoryRenderBuffer {
    let width = i32::try_from(size.0).unwrap_or(i32::MAX);
    let height = i32::try_from(size.1).unwrap_or(i32::MAX);
    // ABGR8888 is R, G, B, A in memory: the rasterizer's premultiplied RGBA.
    MemoryRenderBuffer::new(
        Fourcc::Abgr8888,
        (width, height),
        1,
        Transform::Normal,
        None,
    )
}

/// The toolkit's event for a pointer button (evdev code) changing state.
pub(crate) fn mouse_input(button: u32, pressed: bool) -> WindowEvent {
    let button = match button {
        BTN_LEFT => MouseButton::Left,
        BTN_RIGHT => MouseButton::Right,
        BTN_MIDDLE => MouseButton::Middle,
        BTN_SIDE => MouseButton::Back,
        BTN_EXTRA => MouseButton::Forward,
        other => MouseButton::Other(u16::try_from(other).unwrap_or(u16::MAX)),
    };
    let state = if pressed {
        ElementState::Pressed
    } else {
        ElementState::Released
    };
    WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button,
    }
}

pub(crate) fn modifiers_state(modifiers: KeyModifiers) -> ModifiersState {
    let mut state = ModifiersState::empty();
    state.set(ModifiersState::SHIFT, modifiers.shift);
    state.set(ModifiersState::CONTROL, modifiers.ctrl);
    state.set(ModifiersState::ALT, modifiers.alt);
    state.set(ModifiersState::SUPER, modifiers.logo);
    state
}

/// The toolkit's key event for a key the compositor's keymap resolved.
pub(crate) fn key_event(key: &HostKey) -> WindowEvent {
    let text = key
        .pressed
        .then_some(key.text)
        .flatten()
        .map(|c| SmolStr::new(c.to_string()));
    let (logical_key, location) = key_for(key.keysym, key.text);
    let (key_without_modifiers, _) = key_for(key.unmodified_keysym, None);
    let state = if key.pressed {
        ElementState::Pressed
    } else {
        ElementState::Released
    };
    let event = KeyEvent::synthetic(
        RawKeyEvent {
            physical_key: PhysicalKey::from_scancode(key.keycode),
            state,
        },
        logical_key,
        text.clone(),
        location,
        key.repeat,
        text,
        key_without_modifiers,
    );
    WindowEvent::KeyboardInput {
        device_id: DeviceId::dummy(),
        event,
        is_synthetic: false,
    }
}

/// The key a hosted surface got, as the toolkit names it, with the
/// modifiers held: what a shortcut table matches against.
#[must_use]
pub fn toolkit_key(key: &HostKey) -> (Key, Modifiers) {
    let (logical, _) = key_for(key.keysym, key.text);
    (logical, Modifiers::from(modifiers_state(key.modifiers)))
}

/// The key `keysym` is: a named key when winit names it, else the
/// character it types.
fn key_for(keysym: u32, text: Option<char>) -> (Key, KeyLocation) {
    let (key, location) = winit::platform::scancode::keysym_to_key(keysym);
    if !matches!(key, Key::Unidentified(_)) {
        return (key, location);
    }
    let typed = text.or_else(|| char::from_u32(keysym).filter(|c| !c.is_control()));
    match typed {
        Some(c) => (Key::Character(SmolStr::new(c.to_string())), location),
        None => (key, location),
    }
}

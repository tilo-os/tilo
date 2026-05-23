//! `ServoSurface` — the quick-ui widget that displays Servo's rendered
//! output and forwards pointer/keyboard events back into the engine.
//!
//! This is the quick-ui equivalent of iced's `image(handle)` plus
//! `mouse_area(...)`. Unlike the in-tree [`ui::widget::Image`] —
//! which caches a single PNG decoded at construction — `ServoSurface`
//! reads the latest RGBA frame from a shared slot on every draw, so the
//! widget can refresh in step with Servo's repaint cadence.
//!
//! The widget is a leaf: it has no children. Hit-testing happens against
//! `self.rect`; events are routed through a `Sender<BrowserCommand>` so
//! the engine thread can react without going through the CRDT (input
//! events are not durable state, so the CRDT path isn't a good fit).

use crossbeam_channel::Sender;
use std::sync::{Arc, Mutex};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Blob, Color as PenikoColor, Fill, ImageAlphaType, ImageData as PenikoImage, ImageFormat as Format};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key as WKey, NamedKey as WNamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use servo::{
    DevicePoint, InputEvent, Key as SKey, KeyState, KeyboardEvent, MouseButton as SMouseButton,
    MouseButtonAction, MouseButtonEvent, MouseMoveEvent, NamedKey as SNamedKey, WebViewPoint,
};

use crate::engine::BrowserCommand;

/// Shared slot holding the most recent RGBA frame from Servo. Written by
/// the engine thread (`IcedHost`-like sink), read by the widget on every
/// `draw` call.
pub type FrameSlot = Arc<Mutex<Option<FrameData>>>;

/// One frame's worth of pixels plus dimensions.
pub struct FrameData {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Live surface widget that paints the latest Servo frame and forwards
/// pointer/keyboard events back into the engine.
pub struct ServoSurface {
    rect: Rect,
    frames: FrameSlot,
    cmd_tx: Option<Sender<BrowserCommand>>,
    /// Logical-pixel scale factor used to translate widget-space pointer
    /// positions into Servo's device coordinates.
    scale_factor: f32,
    is_focused: bool,
    last_mouse_pos: Point,
    cached_dims: (u32, u32),
    cached_image: Option<PenikoImage>,
    /// On the last frame painted, the count of bytes in the most recent
    /// blob, so we can tell the cached `PenikoImage` is stale even when
    /// dimensions match (e.g. Servo re-renders the same size).
    cached_buffer_ptr: usize,
}

impl ServoSurface {
    pub fn new(frames: FrameSlot) -> Self {
        Self {
            rect: Rect::ZERO,
            frames,
            cmd_tx: None,
            scale_factor: 1.0,
            is_focused: false,
            last_mouse_pos: Point::ZERO,
            cached_dims: (0, 0),
            cached_image: None,
            cached_buffer_ptr: 0,
        }
    }

    pub fn with_sender(mut self, tx: Sender<BrowserCommand>) -> Self {
        self.cmd_tx = Some(tx);
        self
    }

    pub fn set_scale_factor(&mut self, sf: f32) {
        self.scale_factor = sf;
    }

    /// Returns the rectangle the widget currently occupies. The browser
    /// app reads this every frame to issue a `Resize` to Servo when the
    /// layout-derived size differs from what the engine last knew.
    pub fn rect(&self) -> Rect {
        self.rect
    }
}

impl Widget for ServoSurface {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Background — paints a placeholder until the first frame
        // arrives, and stays under the image as a clear colour for any
        // letterboxed pixels.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        // Pull the most recent frame (clones the Vec; cheap enough for
        // a typical viewport — ~3 MB at 1024×768). If we wanted to
        // amortise this further we could swap the slot for an
        // `ArcSwap<Arc<Vec<u8>>>` later; not necessary for v1.
        let maybe_frame = {
            let guard = self.frames.lock();
            guard.ok().and_then(|g| {
                g.as_ref().map(|f| {
                    let buf_ptr = f.rgba.as_ptr() as usize;
                    (f.rgba.clone(), f.width, f.height, buf_ptr)
                })
            })
        };

        if let Some((rgba, width, height, ptr)) = maybe_frame {
            let dims = (width, height);
            let stale = self.cached_image.is_none()
                || dims != self.cached_dims
                || ptr != self.cached_buffer_ptr;
            if stale {
                let expected = (width as usize) * (height as usize) * 4;
                if rgba.len() == expected {
                    let blob = Blob::new(Arc::new(rgba));
                    self.cached_image = Some(PenikoImage {
                        data: blob,
                        format: Format::Rgba8,
                        alpha_type: ImageAlphaType::Alpha,
                        width,
                        height,
                    });
                    self.cached_dims = dims;
                    self.cached_buffer_ptr = ptr;
                }
            }
        }

        if let Some(img) = self.cached_image.clone() {
            let (iw, ih) = self.cached_dims;
            if iw == 0 || ih == 0 {
                return;
            }
            let rw = self.rect.width();
            let rh = self.rect.height();
            // Fit the image scaled down to the widget's logical size.
            // Servo renders at *physical* size = logical * scale_factor,
            // so painting brush-scaled by 1/scale_factor gives crisp
            // pixels on hi-DPI displays. We treat this as a Fill so the
            // surface covers the whole widget rect (the engine resizes
            // Servo to match aspect anyway).
            let sx = rw / iw as f64;
            let sy = rh / ih as f64;
            let bt = Affine::translate((self.rect.x0, self.rect.y0))
                * Affine::scale_non_uniform(sx, sy);
            let image_brush = vello::peniko::ImageBrush::new(img);
            scene.fill(Fill::NonZero, transform, &image_brush, Some(bt), &self.rect);
        } else {
            // Loading state: a 1 px dashed border so it's visible on
            // dark backgrounds without needing fonts loaded.
            let _ = PenikoColor::WHITE; // keep peniko::Color import live
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                theme.tokens.colors.border_standard,
                None,
                &self.rect,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos =
                    Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse_pos = logical_pos;
                if self.rect.contains(logical_pos) {
                    if let Some(tx) = &self.cmd_tx {
                        let dev_x =
                            (logical_pos.x - self.rect.x0) as f32 * self.scale_factor;
                        let dev_y =
                            (logical_pos.y - self.rect.y0) as f32 * self.scale_factor;
                        let event = InputEvent::MouseMove(MouseMoveEvent::new(
                            WebViewPoint::Device(DevicePoint::new(dev_x, dev_y)),
                        ));
                        let _ = tx.send(BrowserCommand::Input(event));
                    }
                }
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                if self.rect.contains(self.last_mouse_pos) {
                    *focus_target = Some(node_id);
                    *handled = true;
                    if let Some(tx) = &self.cmd_tx {
                        let dev_x =
                            (self.last_mouse_pos.x - self.rect.x0) as f32 * self.scale_factor;
                        let dev_y =
                            (self.last_mouse_pos.y - self.rect.y0) as f32 * self.scale_factor;
                        let action = if *element_state == ElementState::Pressed {
                            // Mirror the iced version: a click also
                            // focuses the embedded webview so subsequent
                            // keyboard input is dispatched into Servo.
                            let _ = tx.send(BrowserCommand::Focus);
                            MouseButtonAction::Down
                        } else {
                            MouseButtonAction::Up
                        };
                        let event = InputEvent::MouseButton(MouseButtonEvent::new(
                            action,
                            SMouseButton::Left,
                            WebViewPoint::Device(DevicePoint::new(dev_x, dev_y)),
                        ));
                        let _ = tx.send(BrowserCommand::Input(event));
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key,
                    state: element_state,
                    ..
                },
                ..
            } => {
                if self.is_focused && *element_state == ElementState::Pressed {
                    if let Some(tx) = &self.cmd_tx {
                        let mapped = match logical_key {
                            WKey::Character(c) => Some(SKey::Character(c.to_string())),
                            WKey::Named(WNamedKey::Enter) => Some(SKey::Named(SNamedKey::Enter)),
                            WKey::Named(WNamedKey::Backspace) => {
                                Some(SKey::Named(SNamedKey::Backspace))
                            }
                            WKey::Named(WNamedKey::Tab) => Some(SKey::Named(SNamedKey::Tab)),
                            WKey::Named(WNamedKey::ArrowDown) => {
                                Some(SKey::Named(SNamedKey::ArrowDown))
                            }
                            WKey::Named(WNamedKey::ArrowUp) => {
                                Some(SKey::Named(SNamedKey::ArrowUp))
                            }
                            WKey::Named(WNamedKey::ArrowLeft) => {
                                Some(SKey::Named(SNamedKey::ArrowLeft))
                            }
                            WKey::Named(WNamedKey::ArrowRight) => {
                                Some(SKey::Named(SNamedKey::ArrowRight))
                            }
                            _ => None,
                        };
                        if let Some(k) = mapped {
                            let kb = KeyboardEvent::from_state_and_key(KeyState::Down, k);
                            let _ =
                                tx.send(BrowserCommand::Input(InputEvent::Keyboard(kb)));
                            *handled = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "ServoSurface"
    }

    fn test_id(&self) -> Option<&str> {
        Some("browser.surface")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }
}

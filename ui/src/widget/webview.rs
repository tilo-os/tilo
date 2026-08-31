//! `WebView` — a leaf widget that displays the live output of a
//! [`crate::webview::WebViewEngine`] and forwards pointer, scroll and
//! keyboard input into it.
//!
//! The widget is deliberately chrome-free: address bar, navigation
//! buttons, progress and permission prompts belong to the host app / OS
//! chrome. It paints whatever frame the engine last produced (scaled to
//! its layout rect), shows a neutral placeholder until the first frame
//! arrives, and translates toolkit-space input into engine commands
//! (physical pixels relative to the view's origin).
//!
//! Navigation state (URL, title, progress) intentionally does NOT flow
//! through this widget — the host app drains
//! [`crate::webview::WebViewEvent`]s from the engine during `sync` and
//! owns that state in its CRDT.

use std::sync::Arc;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Blob, Fill, ImageAlphaType, ImageData as PenikoImage, ImageFormat};
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key as WKey, NamedKey as WNamedKey};

use crate::gesture::NavDirection;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::webview::{KeyInput, KeyModifiers, MouseButton, NamedKey, SharedEngine, WebViewCommand};
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Pixels scrolled per mouse-wheel line tick, in logical units (scaled
/// by the display factor before being sent to the engine).
const WHEEL_LINE_PX: f64 = 40.0;

/// Narrows a screen-space `f64` (view-relative pointer/scroll coordinate) to
/// the `f32` the engine's command protocol expects.
#[allow(
    clippy::cast_possible_truncation,
    reason = "on-screen physical coordinates never approach f32::MAX; this narrowing is exact for every practical viewport size"
)]
fn to_f32(v: f64) -> f32 {
    v as f32
}

/// Rounds a physical pixel extent to `u32`, clamping to `[0, u32::MAX]`
/// first so a negative or absurdly large rect (a transient layout glitch)
/// saturates instead of wrapping.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to u32's range immediately above, so the cast itself can never truncate or flip sign"
)]
fn round_to_u32_px(v: f64) -> u32 {
    v.round().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// See module docs.
pub struct WebView {
    rect: Rect,
    engine: SharedEngine,
    scale_factor: f32,
    focused: bool,
    last_mouse: Point,
    /// Physical size last sent as a `Resize`, to avoid spamming the engine.
    last_sent_size: (u32, u32),
    /// Keyed by the frame buffer's allocation pointer + dimensions.
    cached_image: Option<PenikoImage>,
    cached_key: (usize, u32, u32),
    test_id: String,
}

impl WebView {
    pub fn new(engine: SharedEngine) -> Self {
        Self {
            rect: Rect::ZERO,
            engine,
            scale_factor: 1.0,
            focused: false,
            last_mouse: Point::ZERO,
            last_sent_size: (0, 0),
            cached_image: None,
            cached_key: (0, 0, 0),
            test_id: "webview".to_string(),
        }
    }

    /// Override the e2e test identifier (default `"webview"`), for a tree
    /// hosting several web views.
    #[must_use]
    pub fn with_test_id(mut self, id: &str) -> Self {
        self.test_id = id.to_string();
        self
    }

    /// Place the view at an explicit logical rect without going through a
    /// taffy layout pass — for hosts that embed the view inside their own
    /// immediate-mode drawing (e.g. an email reader rendering an HTML body
    /// inline) rather than as a first-class widget-tree node. Sends a
    /// `Resize` when the physical size changes, like [`Widget::apply_layout`].
    pub fn set_viewport(&mut self, rect: Rect, scale_factor: f32) {
        self.rect = rect;
        self.scale_factor = scale_factor;
        self.send_resize_if_needed();
    }

    /// The engine handle this view drives, so a host can send it commands
    /// (Navigate to an email's sandboxed `data:` document, install a
    /// content filter) directly.
    pub fn engine(&self) -> &SharedEngine {
        &self.engine
    }

    /// Engine-space (physical, view-relative) coordinates for a logical point.
    fn to_engine(&self, p: Point) -> (f32, f32) {
        (
            to_f32((p.x - self.rect.x0) * f64::from(self.scale_factor)),
            to_f32((p.y - self.rect.y0) * f64::from(self.scale_factor)),
        )
    }

    fn send_resize_if_needed(&mut self) {
        let w = round_to_u32_px(self.rect.width() * f64::from(self.scale_factor));
        let h = round_to_u32_px(self.rect.height() * f64::from(self.scale_factor));
        if w == 0 || h == 0 || (w, h) == self.last_sent_size {
            return;
        }
        self.last_sent_size = (w, h);
        self.engine.send(WebViewCommand::Resize {
            width: w,
            height: h,
            scale_factor: self.scale_factor,
        });
    }

    /// Map a winit logical key to the engine's key model. Pure so tests can
    /// pin the table without synthesising `winit::KeyEvent`s (whose
    /// platform fields aren't portably constructible).
    fn map_key(key: &WKey) -> Option<KeyInput> {
        match key {
            WKey::Character(c) => Some(KeyInput::Character(c.to_string())),
            WKey::Named(WNamedKey::Space) => Some(KeyInput::Character(" ".to_string())),
            WKey::Named(named) => {
                let mapped = match named {
                    WNamedKey::Enter => NamedKey::Enter,
                    WNamedKey::Backspace => NamedKey::Backspace,
                    WNamedKey::Delete => NamedKey::Delete,
                    WNamedKey::Tab => NamedKey::Tab,
                    WNamedKey::Escape => NamedKey::Escape,
                    WNamedKey::ArrowUp => NamedKey::ArrowUp,
                    WNamedKey::ArrowDown => NamedKey::ArrowDown,
                    WNamedKey::ArrowLeft => NamedKey::ArrowLeft,
                    WNamedKey::ArrowRight => NamedKey::ArrowRight,
                    WNamedKey::Home => NamedKey::Home,
                    WNamedKey::End => NamedKey::End,
                    WNamedKey::PageUp => NamedKey::PageUp,
                    WNamedKey::PageDown => NamedKey::PageDown,
                    _ => return None,
                };
                Some(KeyInput::Named(mapped))
            }
            _ => None,
        }
    }
}

impl Widget for WebView {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            flex_grow: 1.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        self.send_resize_if_needed();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Placeholder before the first frame, letterbox backing afterwards.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        let latest = {
            let slot = self.engine.frames();
            let guard = slot.lock();
            guard.ok().and_then(|g| g.clone())
        };

        if let Some(frame) = latest {
            let key = (Arc::as_ptr(&frame.rgba) as usize, frame.width, frame.height);
            if self.cached_image.is_none() || key != self.cached_key {
                let expected = (frame.width as usize)
                    .saturating_mul(frame.height as usize)
                    .saturating_mul(4);
                if frame.rgba.len() == expected && expected > 0 {
                    self.cached_image = Some(PenikoImage {
                        data: Blob::new(frame.rgba.clone()),
                        format: ImageFormat::Rgba8,
                        alpha_type: ImageAlphaType::Alpha,
                        width: frame.width,
                        height: frame.height,
                    });
                    self.cached_key = key;
                }
            }
        }

        if let Some(img) = self.cached_image.clone() {
            let (_, iw, ih) = self.cached_key;
            crate::webview::profile::record_draw(
                self.rect.width(),
                self.rect.height(),
                Some((iw, ih)),
            );
            if iw == 0 || ih == 0 {
                return;
            }
            // The engine renders at physical resolution for our logical rect
            // — scale down so hi-DPI frames map 1:1 to device pixels.
            let sx = self.rect.width() / f64::from(iw);
            let sy = self.rect.height() / f64::from(ih);
            let brush_transform =
                Affine::translate((self.rect.x0, self.rect.y0)).pre_scale_non_uniform(sx, sy);
            let brush = vello::peniko::ImageBrush::new(img);
            scene.fill(
                Fill::NonZero,
                transform,
                &brush,
                Some(brush_transform),
                &self.rect,
            );
        } else {
            crate::webview::profile::record_draw(self.rect.width(), self.rect.height(), None);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if (f64::from(self.scale_factor) - scale_factor).abs() > f64::EPSILON {
            self.scale_factor = to_f32(scale_factor);
            // Force a resize eagerly, to keep engine DPI in step without a re-layout.
            self.last_sent_size = (0, 0);
            self.send_resize_if_needed();
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = logical;
                if self.rect.contains(logical) {
                    let (x, y) = self.to_engine(logical);
                    self.engine.send(WebViewCommand::MouseMove { x, y });
                }
            }
            WindowEvent::MouseInput {
                state: element_state,
                button,
                ..
            } => {
                if !self.rect.contains(self.last_mouse) {
                    return;
                }
                let down = *element_state == ElementState::Pressed;
                let (x, y) = self.to_engine(self.last_mouse);
                match button {
                    winit::event::MouseButton::Left
                    | winit::event::MouseButton::Middle
                    | winit::event::MouseButton::Right => {
                        let mapped = match button {
                            winit::event::MouseButton::Left => MouseButton::Left,
                            winit::event::MouseButton::Middle => MouseButton::Middle,
                            _ => MouseButton::Right,
                        };
                        if down && mapped == MouseButton::Left {
                            *focus_target = Some(node_id);
                        }
                        crate::webview::profile::record_input("mouse");
                        self.engine.send(WebViewCommand::MouseButton {
                            button: mapped,
                            down,
                            x,
                            y,
                        });
                        *handled = true;
                    }
                    // Mouse side buttons drive history, as in every desktop browser.
                    winit::event::MouseButton::Back => {
                        if down {
                            self.engine.send(WebViewCommand::GoBack);
                            *handled = true;
                        }
                    }
                    winit::event::MouseButton::Forward => {
                        if down {
                            self.engine.send(WebViewCommand::GoForward);
                            *handled = true;
                        }
                    }
                    winit::event::MouseButton::Other(_) => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !self.rect.contains(self.last_mouse) {
                    return;
                }
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (
                        to_f32(f64::from(*x) * WHEEL_LINE_PX * scale_factor),
                        to_f32(f64::from(*y) * WHEEL_LINE_PX * scale_factor),
                    ),
                    // PixelDelta is already physical.
                    MouseScrollDelta::PixelDelta(p) => (to_f32(p.x), to_f32(p.y)),
                };
                let (x, y) = self.to_engine(self.last_mouse);
                crate::webview::profile::record_input("scroll");
                self.engine.send(WebViewCommand::Scroll { dx, dy, x, y });
                *handled = true;
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                if !self.focused {
                    return;
                }
                if let Some(key) = Self::map_key(&key_event.logical_key) {
                    crate::webview::profile::record_input("key");
                    let m = modifiers.state();
                    self.engine.send(WebViewCommand::Key {
                        key,
                        down: key_event.state == ElementState::Pressed,
                        modifiers: KeyModifiers {
                            ctrl: m.control_key(),
                            shift: m.shift_key(),
                            alt: m.alt_key(),
                            meta: m.super_key(),
                        },
                    });
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "WebView"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.test_id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// A freshly-exported engine frame that the scene hasn't painted yet
    /// keeps the app's ~60 Hz animation pump alive; once the frame is
    /// consumed (cached) the widget stops asking and the loop can idle.
    fn needs_animation_frame(&self) -> bool {
        let slot = self.engine.frames();
        let guard = slot.lock();
        match guard.ok().as_deref() {
            Some(Some(f)) => (Arc::as_ptr(&f.rgba) as usize, f.width, f.height) != self.cached_key,
            _ => false,
        }
    }

    /// Two-finger swipe / edge-pull → browser-style history navigation.
    /// Overriding this here makes every embedder of `WebView`
    /// swipe-navigable, not just the browser app.
    fn on_nav_gesture(
        &mut self,
        dir: NavDirection,
        _state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        self.engine.send(match dir {
            NavDirection::Back => WebViewCommand::GoBack,
            NavDirection::Forward => WebViewCommand::GoForward,
        });
        true
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.engine.send(WebViewCommand::SetFocused(focused));
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webview::WebViewEngine; // send()/commands() on the concrete TestEngine
    use crate::webview::test_engine::TestEngine;
    use std::sync::Arc;
    use winit::event::{DeviceId, ElementState, WindowEvent};

    fn laid_out(engine: Arc<TestEngine>) -> (WebView, TaffyTree<()>, NodeId) {
        let mut w = WebView::new(engine);
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = w.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(400.0),
                height: taffy::AvailableSpace::Definite(300.0),
            },
        )
        .unwrap();
        w.apply_layout(&tree, node, Point::ZERO);
        (w, tree, node)
    }

    fn dispatch(w: &mut WebView, tree: &TaffyTree<()>, node: NodeId, event: &WindowEvent) {
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        let mut handled = false;
        let mut focus = None;
        let mods = winit::event::Modifiers::default();
        w.handle_event(
            event,
            1.0,
            &mods,
            &mut state,
            &root,
            &mut handled,
            &mut focus,
            node,
            tree,
        );
    }

    #[test]
    fn layout_sends_physical_resize_once() {
        let engine = TestEngine::new();
        let (mut w, tree, node) = laid_out(engine.clone());
        // Re-applying the same layout must not resend.
        w.apply_layout(&tree, node, Point::ZERO);
        let resizes: Vec<_> = engine
            .commands()
            .into_iter()
            .filter(|c| matches!(c, WebViewCommand::Resize { .. }))
            .collect();
        assert_eq!(
            resizes,
            vec![WebViewCommand::Resize {
                width: 400,
                height: 300,
                scale_factor: 1.0
            }]
        );
    }

    #[test]
    fn pointer_inside_forwards_translated_coordinates() {
        let engine = TestEngine::new();
        let (mut w, tree, node) = laid_out(engine.clone());
        let device: DeviceId = unsafe { std::mem::zeroed() };
        dispatch(
            &mut w,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: device,
                position: (50.0, 60.0).into(),
            },
        );
        dispatch(
            &mut w,
            &tree,
            node,
            &WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
        );
        let cmds = engine.commands();
        assert!(cmds.contains(&WebViewCommand::MouseMove { x: 50.0, y: 60.0 }));
        assert!(cmds.contains(&WebViewCommand::MouseButton {
            button: MouseButton::Left,
            down: true,
            x: 50.0,
            y: 60.0
        }));
    }

    #[test]
    fn pointer_outside_sends_nothing() {
        let engine = TestEngine::new();
        let (mut w, tree, node) = laid_out(engine.clone());
        let device: DeviceId = unsafe { std::mem::zeroed() };
        dispatch(
            &mut w,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: device,
                position: (900.0, 900.0).into(),
            },
        );
        dispatch(
            &mut w,
            &tree,
            node,
            &WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
        );
        assert!(
            engine
                .commands()
                .iter()
                .all(|c| matches!(c, WebViewCommand::Resize { .. })),
            "only the layout resize may reach the engine",
        );
    }

    #[test]
    fn back_side_button_navigates_history() {
        let engine = TestEngine::new();
        let (mut w, tree, node) = laid_out(engine.clone());
        let device: DeviceId = unsafe { std::mem::zeroed() };
        dispatch(
            &mut w,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: device,
                position: (10.0, 10.0).into(),
            },
        );
        dispatch(
            &mut w,
            &tree,
            node,
            &WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Back,
            },
        );
        assert!(engine.commands().contains(&WebViewCommand::GoBack));
    }

    #[test]
    fn wheel_scrolls_in_physical_pixels() {
        let engine = TestEngine::new();
        let (mut w, tree, node) = laid_out(engine.clone());
        let device: DeviceId = unsafe { std::mem::zeroed() };
        dispatch(
            &mut w,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: device,
                position: (10.0, 10.0).into(),
            },
        );
        dispatch(
            &mut w,
            &tree,
            node,
            &WindowEvent::MouseWheel {
                device_id: device,
                delta: MouseScrollDelta::LineDelta(0.0, -2.0),
                phase: winit::event::TouchPhase::Moved,
            },
        );
        assert!(engine.commands().contains(&WebViewCommand::Scroll {
            dx: 0.0,
            dy: -80.0,
            x: 10.0,
            y: 10.0
        }));
    }

    #[test]
    fn focus_change_notifies_engine_once() {
        let engine = TestEngine::new();
        let (mut w, _tree, _node) = laid_out(engine.clone());
        w.set_focus(true);
        w.set_focus(true);
        w.set_focus(false);
        let focus_cmds: Vec<_> = engine
            .commands()
            .into_iter()
            .filter(|c| matches!(c, WebViewCommand::SetFocused(_)))
            .collect();
        assert_eq!(
            focus_cmds,
            vec![
                WebViewCommand::SetFocused(true),
                WebViewCommand::SetFocused(false)
            ]
        );
    }

    #[test]
    fn key_mapping_covers_text_and_named_keys() {
        assert_eq!(
            WebView::map_key(&WKey::Character("a".into())),
            Some(KeyInput::Character("a".into()))
        );
        assert_eq!(
            WebView::map_key(&WKey::Named(WNamedKey::Space)),
            Some(KeyInput::Character(" ".into()))
        );
        assert_eq!(
            WebView::map_key(&WKey::Named(WNamedKey::Enter)),
            Some(KeyInput::Named(NamedKey::Enter))
        );
        assert_eq!(
            WebView::map_key(&WKey::Named(WNamedKey::PageDown)),
            Some(KeyInput::Named(NamedKey::PageDown))
        );
        // Modifier keys are not forwarded as page keys.
        assert_eq!(WebView::map_key(&WKey::Named(WNamedKey::Shift)), None);
    }

    #[test]
    fn nav_gesture_sends_history_commands() {
        use crate::gesture::NavDirection;
        let engine = TestEngine::new();
        let mut w = WebView::new(engine.clone());
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        assert!(w.on_nav_gesture(NavDirection::Back, &mut state, &root));
        assert!(w.on_nav_gesture(NavDirection::Forward, &mut state, &root));
        let cmds = engine.commands();
        assert!(cmds.contains(&WebViewCommand::GoBack));
        assert!(cmds.contains(&WebViewCommand::GoForward));
    }

    #[test]
    fn draw_after_navigation_paints_frame_image() {
        let engine = TestEngine::new();
        let (mut w, _tree, _node) = laid_out(engine.clone());
        engine.send(WebViewCommand::Navigate("https://example.org".into()));

        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let presence = PresenceCtx::default();
        w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        assert!(
            w.cached_image.is_some(),
            "draw caches the engine frame as a vello image",
        );
    }
}

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

use crate::widget::{AppModel, ModelPath};
use std::sync::Arc;

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
    reason = "on-screen physical coordinates never approach f32::MAX; f32 precision is sufficient for engine screen coordinates and device scale"
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

/// Backends may render the exact physical request or quantize it through an
/// integer logical viewport. Allow rounding in either direction (an engine may
/// round logical dimensions to nearest; CEF rounds outward). Integer device scales
/// need at most scale - 1 pixels; fractional scales can add a final pixel when
/// converting the logical viewport back to an integer physical buffer.
fn frame_quantization_slack(scale_factor: f32) -> u32 {
    let scale = f64::from(scale_factor);
    if !scale.is_finite() || scale <= 0.0 {
        return 0;
    }
    round_to_u32_px(if scale.fract() == 0.0 {
        scale - 1.0
    } else {
        scale.ceil()
    })
}

/// See module docs.
pub struct WebView {
    text_input: crate::webview::text_input::Guard,
    history_operation: Option<(
        tilo_actions::InvocationId,
        crate::webview::text_input::Receipt,
    )>,
    rect: Rect,
    engine: SharedEngine,
    scale_factor: f32,
    focused: bool,
    last_mouse: Point,
    /// Physical size last sent as a `Resize`, to avoid spamming the engine.
    last_sent_size: (u32, u32),
    /// Last expected/observed mismatch for which we requested a repaint.
    /// A matching frame clears it, so a stalled backend cannot cause a
    /// redraw-command loop on every UI frame.
    stale_resize_retry: Option<((u32, u32), (u32, u32))>,
    native_mounted: bool,
    /// Keyed by the frame buffer's allocation pointer + dimensions.
    cached_image: Option<PenikoImage>,
    cached_trace_id: Option<u64>,
    cached_key: (usize, u32, u32),
    /// Draws the engine's display list when it has one (the paint stream).
    page: crate::webview::paint::PageRenderer,
    test_id: String,
    accessible_name: String,
}

impl WebView {
    pub fn new(engine: SharedEngine) -> Self {
        Self {
            text_input: Default::default(),
            history_operation: None,
            rect: Rect::ZERO,
            engine,
            scale_factor: 1.0,
            focused: false,
            last_mouse: Point::ZERO,
            last_sent_size: (0, 0),
            stale_resize_retry: None,
            native_mounted: false,
            cached_image: None,
            cached_trace_id: None,
            cached_key: (0, 0, 0),
            page: crate::webview::paint::PageRenderer::new(),
            test_id: "webview".to_string(),
            accessible_name: "Web content".to_string(),
        }
    }

    /// While the session shares this window's page with someone watching,
    /// hand it the page as a paint stream — the view on screen is the page
    /// the window shows. Payloads the app → shell channel cannot carry are
    /// dropped; a dropped whole frame is asked for again.
    fn share_page(&self) {
        use crate::webview::paint;
        if !paint::share_watched() {
            return;
        }
        let keyframe = paint::take_share_keyframe();
        let page = [
            self.rect.x0,
            self.rect.y0,
            self.rect.width(),
            self.rect.height(),
        ];
        for payload in self.engine.take_paint_stream(keyframe) {
            let whole = matches!(
                payload,
                crate::webview::paint::stream::wire::Payload::Key(_)
            );
            match payload.encode() {
                Ok(bytes) if bytes.len() <= paint::MAX_SHARE_PAYLOAD => {
                    paint::queue_share(paint::SharedFrame {
                        payload: bytes,
                        page,
                        scale: f64::from(self.scale_factor),
                    });
                }
                _ => {
                    tracing::debug!(whole, "paint payload too large to share; skipped");
                }
            }
        }
    }

    /// Seed the window DPI before the first layout of a rebuilt view.
    #[must_use]
    pub fn with_scale_factor(mut self, scale_factor: f32) -> Self {
        if scale_factor.is_finite() && scale_factor > 0.0 {
            self.scale_factor = scale_factor;
        }
        self
    }

    /// Override the e2e test identifier (default `"webview"`), for a tree
    /// hosting several web views.
    #[must_use]
    pub fn with_test_id(mut self, id: &str) -> Self {
        self.test_id = id.to_string();
        self
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = name.into();
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
        crate::webview::profile::record_resize(w, h, self.scale_factor);
        self.engine.send(WebViewCommand::Resize {
            width: w,
            height: h,
            scale_factor: self.scale_factor,
        });
    }

    fn retry_resize_for_stale_frame(&mut self, observed: (u32, u32)) -> bool {
        let expected = (
            round_to_u32_px(self.rect.width() * f64::from(self.scale_factor)),
            round_to_u32_px(self.rect.height() * f64::from(self.scale_factor)),
        );
        if expected.0 == 0 || expected.1 == 0 {
            self.stale_resize_retry = None;
            return true;
        }
        // This shared check rejects stale density/viewport frames without
        // requiring every engine to use CEF's exact allocation geometry.
        // Chromium additionally enforces its exact paint size in the host.
        let slack = frame_quantization_slack(self.scale_factor);
        let matches =
            observed.0.abs_diff(expected.0) <= slack && observed.1.abs_diff(expected.1) <= slack;
        if matches {
            self.stale_resize_retry = None;
            return true;
        }
        let retry = (expected, observed);
        if self.stale_resize_retry != Some(retry) {
            self.stale_resize_retry = Some(retry);
            crate::webview::profile::record_resize(expected.0, expected.1, self.scale_factor);
            self.engine.send(WebViewCommand::Resize {
                width: expected.0,
                height: expected.1,
                scale_factor: self.scale_factor,
            });
        }
        false
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

impl crate::widget::NativeViewLifecycle for WebView {
    fn mount_native_view(&mut self, host: &crate::widget::NativeViewHost<'_>) {
        let first_mount = !self.native_mounted;
        self.native_mounted = true;
        let host_scale = to_f32(host.scale_factor());
        // A freshly rebuilt Browser WebView is seeded from the device-scale
        // draw that requested the rebuild. Some macOS startup windows still
        // report 1 here until their first visible frame. Preserve the larger
        // preseed only on that first mount; later mounts (including an actual
        // ScaleFactorChanged event) take the host value in either direction.
        let scale_factor = if first_mount {
            self.scale_factor.max(host_scale)
        } else {
            host_scale
        };
        if scale_factor.is_finite()
            && scale_factor > 0.0
            && (first_mount || scale_factor.to_bits() != self.scale_factor.to_bits())
        {
            self.scale_factor = scale_factor;
            self.last_sent_size = (0, 0);
            self.send_resize_if_needed();
        }
    }

    fn unmount_native_view(&mut self) {
        self.text_input.invalidate(&self.engine);
        if self.focused {
            self.engine.send(WebViewCommand::SetFocused(false));
        }
        self.focused = false;
        self.native_mounted = false;
    }

    fn native_view_frame_start(&mut self) {}
}

impl crate::undo::LocalUndo for WebView {
    fn local_undo_is_deferred(&self) -> bool {
        true
    }
    fn local_undo_status(&self) -> Option<crate::undo::LocalUndoStatus> {
        (self.focused && self.engine.supports_guarded_edit_history()).then(|| {
            self.text_input.history_status().map_or(
                crate::undo::LocalUndoStatus::Pending,
                crate::undo::LocalUndoStatus::Ready,
            )
        })
    }

    fn handle_local_undo_request(&mut self, request: crate::undo::UndoRequest) -> bool {
        if self.focused
            && self.engine.supports_guarded_edit_history()
            && self.text_input.history(request).is_some()
        {
            return true;
        }
        if !self.focused || !self.engine.handle_undo_request(request) {
            return false;
        }
        // Editing the page retires any original-field speech reservation.
        self.text_input.invalidate(&self.engine);
        true
    }
}

impl Widget for WebView {
    crate::provides!(
        dyn crate::widget::NativeViewLifecycle,
        dyn crate::undo::LocalUndo
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::undo::EDIT_OPERATION]
    }

    fn start_operation(
        &mut self,
        operation: &crate::widget::ResourceOperation<'_>,
        _: &mut crate::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let Some(request) = operation.unscoped() else {
            return Ok(None);
        };
        if request.type_id != crate::undo::EDIT_OPERATION {
            return Ok(None);
        }
        if !self.focused || !self.engine.supports_guarded_edit_history() {
            return Ok(None);
        }
        if let Some((invocation, _)) = &self.history_operation {
            return if *invocation == request.invocation {
                Ok(Some(crate::widget::ResourceOperationStart::Accepted))
            } else {
                Err("An original-field history request is already pending".into())
            };
        }
        let direction: crate::undo::UndoRequest =
            serde_json::from_slice(request.payload).map_err(|_| "Invalid native Edit request")?;
        if !self
            .text_input
            .history_status()
            .is_some_and(|status| status.can(direction))
        {
            return Err("The original field's history is not available".into());
        }
        let receipt = self
            .text_input
            .history(direction)
            .ok_or("The original field's history reservation retired")?;
        self.history_operation = Some((request.invocation, receipt));
        Ok(Some(crate::widget::ResourceOperationStart::Accepted))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _: &automerge::AutoCommit,
        _: &automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let (original, receipt) = self.history_operation.as_ref()?;
        if *original != invocation {
            return None;
        }
        if cancellation_requested {
            receipt.cancel();
        }
        let result = receipt.poll()?;
        self.history_operation = None;
        Some(match result {
            Ok(None) => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: Default::default(),
            },
            _ => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "native_history_unconfirmed".into(),
                    message: crate::i18n::I18nManager::lookup_plain(
                        "host-native-history-unconfirmed",
                    ),
                    details: None,
                    retryable: false,
                },
            },
        })
    }

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        self.text_input.pump(
            &self.engine,
            self.focused && self.engine.supports_text_input(),
        );
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        (self.focused && self.engine.supports_text_input() && self.text_input.ready())
            .then_some(winit::window::ImePurpose::Normal)
    }

    fn ime_multiline(&self) -> bool {
        self.ime_purpose().is_some() && self.text_input.multiline()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.ime_purpose()?;
        Some(Rect::new(
            self.rect.x0,
            self.rect.y1 - 1.0,
            self.rect.x0 + 1.0,
            self.rect.y1,
        ))
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_purpose()?;
        self.text_input.stamp(key)
    }

    fn ime_requires_deferred_ack(&self) -> bool {
        true
    }

    fn start_deferred_text(&mut self) -> Option<crate::webview::text_input::Receipt> {
        let receipt = self.focused.then(|| self.text_input.start()).flatten();
        Some(receipt.unwrap_or_else(|| {
            crate::webview::text_input::Receipt::failed(
                crate::webview::text_input::Failure::Rejected,
            )
        }))
    }

    fn commit_deferred_text(&mut self, text: &str) -> Option<crate::webview::text_input::Receipt> {
        let receipt = self
            .focused
            .then(|| self.text_input.commit(&self.engine, text))
            .flatten();
        Some(receipt.unwrap_or_else(|| {
            let receipt = crate::webview::text_input::Receipt::pending();
            receipt.complete(Err(crate::webview::text_input::Failure::Rejected));
            receipt
        }))
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        ((self.focused || self.text_input.has_work()) && self.engine.supports_text_input())
            .then(|| self.text_input.deadline())
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            flex_grow: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.send_resize_if_needed();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // First layout can precede all window/input events. The host's draw
        // transform already carries the actual device scale at that point.
        let coefficients = transform.as_coeffs();
        let device_scale = to_f32(coefficients[0].hypot(coefficients[1]));
        if device_scale.is_finite()
            && device_scale > 0.0
            && device_scale.to_bits() != self.scale_factor.to_bits()
        {
            self.scale_factor = device_scale;
            self.last_sent_size = (0, 0);
            self.send_resize_if_needed();
        }
        // Placeholder before the first frame, letterbox backing afterwards.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        self.engine.presented();
        self.share_page();
        // A display list replaces pixels: draw it like any other widget
        // content, in this toolkit's own text and Vello.
        if let Some(page) = self.engine.paint_frame() {
            let engine = Arc::clone(&self.engine);
            let blobs = move |hash: &[u8; 32]| engine.paint_blob(hash);
            let mut canvas = crate::webview::paint::PageCanvas {
                scene,
                transform: transform * Affine::translate((self.rect.x0, self.rect.y0)),
                text: text_renderer,
                blobs: &blobs,
            };
            self.page.draw(&mut canvas, &page);
            crate::webview::profile::record_draw(self.rect.width(), self.rect.height(), None);
            return;
        }

        let latest = {
            let slot = self.engine.frames();
            let guard = slot.lock();
            guard.ok().and_then(|g| g.clone())
        };

        if let Some(frame) = latest {
            if !self.retry_resize_for_stale_frame((frame.width, frame.height)) {
                self.cached_image = None;
                self.cached_trace_id = None;
                self.cached_key = (0, 0, 0);
                crate::webview::profile::record_draw(
                    self.rect.width(),
                    self.rect.height(),
                    Some((frame.width, frame.height)),
                );
                return;
            }
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
                    self.cached_trace_id = frame.trace_id;
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
            // Keep the last frame at its physical pixel size while a resize
            // awaits paint. Stretching each axis independently distorts text.
            let scale = 1.0 / f64::from(self.scale_factor);
            let brush_transform = Affine::translate((self.rect.x0, self.rect.y0)).pre_scale(scale);
            let painted = self.rect.intersect(Rect::new(
                self.rect.x0,
                self.rect.y0,
                self.rect.x0 + f64::from(iw) * scale,
                self.rect.y0 + f64::from(ih) * scale,
            ));
            crate::webview::frame_trace::drawn(self.cached_trace_id);
            let brush = vello::peniko::ImageBrush::new(img);
            scene.fill(
                Fill::NonZero,
                transform,
                &brush,
                Some(brush_transform),
                &painted,
            );
        } else {
            crate::webview::profile::record_draw(self.rect.width(), self.rect.height(), None);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // Someone is using the page: it is on screen, whether or not a frame
        // has been drawn since it came back.
        if matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::KeyboardInput { .. }
                | WindowEvent::Focused(true)
                | WindowEvent::Occluded(false)
        ) {
            self.engine.presented();
        }
        if (f64::from(self.scale_factor) - ctx.scale_factor).abs() > f64::EPSILON {
            self.scale_factor = to_f32(ctx.scale_factor);
            // Force a resize eagerly, to keep engine DPI in step without a re-layout.
            self.last_sent_size = (0, 0);
            self.send_resize_if_needed();
        }

        // Modifier releases may never become mapped page keys (and system
        // shortcuts consume their presses). Pointer default actions must use
        // the current host state rather than the last key sent to Chromium.
        if matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        ) {
            let modifiers = ctx.modifiers.state();
            self.engine.set_modifiers(KeyModifiers {
                ctrl: modifiers.control_key(),
                shift: modifiers.shift_key(),
                alt: modifiers.alt_key(),
                meta: modifiers.super_key(),
            });
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                            *ctx.focus_target = Some(ctx.node_id);
                        }
                        self.text_input.invalidate(&self.engine);
                        crate::webview::profile::record_input("mouse");
                        self.engine.send(WebViewCommand::MouseButton {
                            button: mapped,
                            down,
                            x,
                            y,
                        });
                        *ctx.handled = true;
                    }
                    // Mouse side buttons drive history, as in every desktop browser.
                    winit::event::MouseButton::Back => {
                        if down {
                            self.engine.send(WebViewCommand::GoBack);
                            *ctx.handled = true;
                        }
                    }
                    winit::event::MouseButton::Forward => {
                        if down {
                            self.engine.send(WebViewCommand::GoForward);
                            *ctx.handled = true;
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
                        to_f32(f64::from(*x) * WHEEL_LINE_PX * ctx.scale_factor),
                        to_f32(f64::from(*y) * WHEEL_LINE_PX * ctx.scale_factor),
                    ),
                    // PixelDelta is already physical.
                    MouseScrollDelta::PixelDelta(p) => (to_f32(p.x), to_f32(p.y)),
                };
                let (x, y) = self.to_engine(self.last_mouse);
                crate::webview::profile::record_input("scroll");
                self.engine.send(WebViewCommand::Scroll { dx, dy, x, y });
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                if !self.focused {
                    return;
                }
                if let Some(key) = Self::map_key(&key_event.logical_key) {
                    // A host shortcut consumes its press but its release still
                    // reaches the page. Retiring here canceled queued Undo/Redo
                    // before the next sync could send it to the original editor.
                    // Release handlers remain subject to the renderer's field
                    // and selection checks before deferred history is applied.
                    if key_event.state == ElementState::Pressed {
                        self.text_input.invalidate(&self.engine);
                    }
                    crate::webview::profile::record_input("key");
                    let m = ctx.modifiers.state();
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
                    *ctx.handled = true;
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

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Document)
                .with_name(self.accessible_name.clone())
                .with_state(crate::a11y::AccessState {
                    focusable: true,
                    focused: self.focused,
                    ..Default::default()
                }),
        )
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
    fn on_nav_gesture(&mut self, dir: NavDirection, _state: &AppModel, _path: &ModelPath) -> bool {
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
            self.text_input.invalidate(&self.engine);
            self.focused = focused;
            self.engine.send(WebViewCommand::SetFocused(focused));
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn wants_tab_input(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Dispatch;
    use crate::webview::WebViewEngine; // send()/commands() on the concrete TestEngine
    use crate::webview::test_engine::TestEngine;
    use automerge::AutoCommit;
    use std::sync::Arc;
    use winit::event::{ElementState, WindowEvent};

    #[test]
    fn native_history_operation_waits_for_renderer_ack_and_rejects_focus_change() {
        use crate::webview::text_input::Transactions;
        use serde_json::{Value, json};
        struct Acknowledged {
            inner: SharedEngine,
            transactions: std::sync::Mutex<Transactions>,
            edits: std::sync::Mutex<Vec<Value>>,
        }
        impl WebViewEngine for Acknowledged {
            fn wrapped(&self) -> Option<&SharedEngine> {
                Some(&self.inner)
            }
            fn send(&self, command: WebViewCommand) {
                self.inner.send(command);
            }
            fn frames(&self) -> crate::webview::FrameSlot {
                self.inner.frames()
            }
            fn drain_events(&self) -> Vec<crate::webview::WebViewEvent> {
                self.inner.drain_events()
            }
            fn supports_text_input(&self) -> bool {
                true
            }
            fn supports_guarded_edit_history(&self) -> bool {
                true
            }
            fn text_input(
                &self,
                request: crate::webview::text_input::Request,
            ) -> Option<crate::webview::text_input::Receipt> {
                let mut transactions = self.transactions.lock().unwrap();
                let (receipt, command) = transactions.admit(request)?;
                if matches!(
                    command["action"].as_str(),
                    Some("undo_text_input" | "redo_text_input")
                ) {
                    assert_eq!(command["history_scope"], "page");
                    self.edits.lock().unwrap().push(command);
                } else {
                    transactions.reply(&json!({"request_id":command["request_id"],"ok":true,
                        "data":{"lease":"0123456789abcdef0123456789abcdef"},
                        "history":{"can_undo":true,"can_redo":false}}));
                }
                Some(receipt)
            }
        }
        for (changed, acknowledged, key_edge) in [
            (false, true, None),
            (false, false, None),
            (true, true, None),
            (false, true, Some(ElementState::Released)),
            (false, true, Some(ElementState::Pressed)),
        ] {
            let native = TestEngine::new();
            let engine = Arc::new(Acknowledged {
                inner: native.clone(),
                transactions: Default::default(),
                edits: Default::default(),
            });
            let (mut page, tree, node) = laid_out(engine.clone());
            page.focused = true;
            let edit = crate::host_chrome::edit::edit_resource(&crate::undo::UndoStatus {
                can_undo: true,
                ..Default::default()
            });
            let violations = tilo_actions::validate_surface(
                &[edit],
                &tilo_actions::ActionRegistry::new(),
                &page.resource_operation_handlers(),
            );
            assert!(
                violations.is_empty(),
                "native Edit publication: {violations:?}"
            );
            let mut doc = AutoCommit::new();
            page.sync(&doc, &automerge::ROOT);
            page.sync(&doc, &automerge::ROOT);
            let invocation = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&crate::undo::UndoRequest::Undo).unwrap();
            let operation = crate::widget::ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                crate::host_chrome::edit::EDIT_OPERATION,
                &payload,
            );
            assert_eq!(
                page.start_operation(
                    &operation,
                    &mut crate::widget::Edits::lend(&mut doc),
                    &automerge::ROOT
                )
                .unwrap(),
                Some(crate::widget::ResourceOperationStart::Accepted)
            );
            assert!(
                page.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                    .is_none(),
                "queue admission cannot report success"
            );
            if changed {
                page.focused = false;
            }
            if let Some(edge) = key_edge {
                dispatch(
                    &mut page,
                    &tree,
                    node,
                    &crate::synthetic_input::key_input(
                        &winit::keyboard::Key::Character("z".into()),
                        edge,
                    ),
                );
                assert!(native.commands().iter().any(|command| {
                    matches!(command, WebViewCommand::Key { down, .. }
                        if *down == (edge == ElementState::Pressed))
                }));
            }
            let retired = changed || key_edge == Some(ElementState::Pressed);
            page.sync(&doc, &automerge::ROOT);
            let edits = engine.edits.lock().unwrap();
            if retired {
                assert!(
                    edits.is_empty(),
                    "changed focus or a new press cannot receive the original edit"
                );
            } else {
                assert_eq!(edits.len(), 1);
                assert!(
                    page.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                        .is_none()
                );
                engine.transactions.lock().unwrap().reply(&json!({"request_id":edits[0]["request_id"],"ok":acknowledged,"code":"rejected"}));
            }
            drop(edits);
            page.sync(&doc, &automerge::ROOT);
            let terminal = page
                .poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                .unwrap();
            assert_eq!(
                matches!(terminal, tilo_actions::InvocationUpdate::Succeeded { .. }),
                !retired && acknowledged
            );
            assert!(
                page.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                    .is_none(),
                "terminal receipt is one shot"
            );
        }
    }

    struct NativeHistory {
        inner: crate::webview::SharedEngine,
        requests: std::sync::Mutex<Vec<crate::undo::UndoRequest>>,
        modifiers: std::sync::Mutex<Vec<KeyModifiers>>,
        pointer_modifiers: std::sync::Mutex<Vec<KeyModifiers>>,
    }
    impl WebViewEngine for NativeHistory {
        fn send(&self, command: WebViewCommand) {
            if matches!(command, WebViewCommand::MouseButton { .. }) {
                let modifiers = self
                    .modifiers
                    .lock()
                    .unwrap()
                    .last()
                    .copied()
                    .unwrap_or_default();
                self.pointer_modifiers.lock().unwrap().push(modifiers);
            }
            self.inner.send(command);
        }
        fn set_modifiers(&self, modifiers: KeyModifiers) {
            self.modifiers.lock().unwrap().push(modifiers);
        }
        fn frames(&self) -> crate::webview::FrameSlot {
            self.inner.frames()
        }
        fn drain_events(&self) -> Vec<crate::webview::WebViewEvent> {
            self.inner.drain_events()
        }
        fn wrapped(&self) -> Option<&crate::webview::SharedEngine> {
            Some(&self.inner)
        }
        fn handle_undo_request(&self, request: crate::undo::UndoRequest) -> bool {
            self.requests
                .lock()
                .expect("native history fixture")
                .push(request);
            true
        }
    }

    #[test]
    fn system_undo_delegates_only_to_focused_native_page() {
        use crate::undo::{UndoRequest, handle_local_undo_request};
        let engine = Arc::new(NativeHistory {
            inner: TestEngine::new(),
            requests: Default::default(),
            modifiers: Default::default(),
            pointer_modifiers: Default::default(),
        });
        let mut page = WebView::new(engine.clone());
        assert!(!handle_local_undo_request(&mut page, UndoRequest::Undo));
        assert!(engine.requests.lock().expect("fixture").is_empty());
        page.set_focus(true);
        assert!(handle_local_undo_request(&mut page, UndoRequest::Undo));
        assert!(handle_local_undo_request(&mut page, UndoRequest::Redo));
        page.set_focus(false);
        assert!(!handle_local_undo_request(&mut page, UndoRequest::Redo));
        assert_eq!(
            *engine.requests.lock().expect("fixture"),
            [UndoRequest::Undo, UndoRequest::Redo]
        );
        let mut unsupported = WebView::new(TestEngine::new());
        unsupported.set_focus(true);
        assert!(!handle_local_undo_request(
            &mut unsupported,
            UndoRequest::Undo
        ));
    }

    #[test]
    fn keyboard_focus_is_forwarded_to_the_page() {
        // Case `focused_page_owns_tab_for_dom_focus_navigation`.
        {
            let mut page = WebView::new(TestEngine::new());
            page.set_focus(true);
            assert!(page.wants_tab_input());
        }

        // Case `focus_change_notifies_engine_once`.
        {
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

        // Case `key_mapping_covers_text_and_named_keys`.
        {
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
    }

    fn laid_out(engine: SharedEngine) -> (WebView, TaffyTree<()>, NodeId) {
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
        Dispatch::new().layout(tree, node).send(w, event);
    }

    #[test]
    fn engine_learns_physical_size_and_retina_scale_before_input() {
        // Case `layout_sends_physical_resize_once`.
        {
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

        // Case `first_retina_draw_sets_dpi_without_an_input_event`.
        {
            let engine = TestEngine::new();
            let (mut view, _, _) = laid_out(engine.clone());
            view.draw(
                &mut Scene::new(),
                Affine::scale(2.0),
                &mut TextRenderer::new(),
                &Theme::dark(),
                &PresenceCtx::default(),
            );
            assert_eq!(
                engine.commands().last(),
                Some(&WebViewCommand::Resize {
                    width: 800,
                    height: 600,
                    scale_factor: 2.0,
                })
            );
            let count = engine.commands().len();
            view.draw(
                &mut Scene::new(),
                Affine::scale(2.0),
                &mut TextRenderer::new(),
                &Theme::dark(),
                &PresenceCtx::default(),
            );
            assert_eq!(engine.commands().len(), count);
        }

        // Case `mounting_in_a_retina_window_sets_dpi_before_input`.
        {
            let engine = TestEngine::new();
            let (mut view, _, _) = laid_out(engine.clone());
            crate::widget::mount_native_views(
                &mut view,
                &crate::widget::NativeViewHost::headless(
                    crate::platform::PlatformConventions::default(),
                    2.0,
                ),
            );
            assert_eq!(
                engine.commands().last(),
                Some(&WebViewCommand::Resize {
                    width: 800,
                    height: 600,
                    scale_factor: 2.0,
                })
            );
        }

        // Case `mounting_reissues_physical_size_when_retina_scale_was_preseeded`.
        {
            let engine = TestEngine::new();
            let mut view = WebView::new(engine.clone()).with_scale_factor(2.0);
            let mut taffy = TaffyTree::new();
            let mut node_map = std::collections::HashMap::new();
            let node = view.build_layout(&mut taffy, &Theme::default(), &mut node_map);
            taffy
                .compute_layout(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(400.0),
                        height: taffy::AvailableSpace::Definite(300.0),
                    },
                )
                .unwrap();
            view.apply_layout(&taffy, node, Point::ORIGIN);
            let before_mount = engine.commands().len();
            crate::widget::mount_native_views(
                &mut view,
                &crate::widget::NativeViewHost::headless(
                    crate::platform::PlatformConventions::default(),
                    2.0,
                ),
            );
            assert_eq!(
                engine.commands().len(),
                before_mount + 1,
                "native mount must reissue a resize that startup could have missed"
            );
            assert_eq!(
                engine.commands().last(),
                Some(&WebViewCommand::Resize {
                    width: 800,
                    height: 600,
                    scale_factor: 2.0,
                })
            );
            crate::widget::mount_native_views(
                &mut view,
                &crate::widget::NativeViewHost::headless(
                    crate::platform::PlatformConventions::default(),
                    2.0,
                ),
            );
            assert_eq!(
                engine.commands().len(),
                before_mount + 1,
                "repeated mount of the same retained view must be idempotent"
            );
        }

        // Case `first_mount_does_not_downgrade_a_preseeded_retina_scale`.
        {
            let engine = TestEngine::new();
            let mut view = WebView::new(engine.clone()).with_scale_factor(2.0);
            let mut taffy = TaffyTree::new();
            let mut node_map = std::collections::HashMap::new();
            let node = view.build_layout(&mut taffy, &Theme::default(), &mut node_map);
            taffy
                .compute_layout(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(400.0),
                        height: taffy::AvailableSpace::Definite(300.0),
                    },
                )
                .unwrap();
            view.apply_layout(&taffy, node, Point::ORIGIN);
            crate::widget::mount_native_views(
                &mut view,
                &crate::widget::NativeViewHost::headless(
                    crate::platform::PlatformConventions::default(),
                    1.0,
                ),
            );
            assert_eq!(
                engine.commands().last(),
                Some(&WebViewCommand::Resize {
                    width: 800,
                    height: 600,
                    scale_factor: 2.0,
                })
            );
        }
    }

    #[test]
    fn engine_frames_are_accepted_by_size_and_density_with_bounded_repaint_retries() {
        // Case `stale_frame_dimensions_request_one_bounded_repaint_retry`.
        {
            let engine = TestEngine::new();
            let (mut view, _, _) = laid_out(engine.clone());
            let initial = engine.commands().len();
            view.retry_resize_for_stale_frame((400, 292));
            assert_eq!(engine.commands().len(), initial + 1);
            assert_eq!(
                engine.commands().last(),
                Some(&WebViewCommand::Resize {
                    width: 400,
                    height: 300,
                    scale_factor: 1.0,
                })
            );
            view.retry_resize_for_stale_frame((400, 292));
            assert_eq!(
                engine.commands().len(),
                initial + 1,
                "the same stale paint must not create an invalidation loop"
            );
            view.retry_resize_for_stale_frame((399, 300));
            assert_eq!(
                engine.commands().len(),
                initial + 2,
                "a distinct stale frame gets one retry"
            );
            view.retry_resize_for_stale_frame((400, 300));
            view.retry_resize_for_stale_frame((400, 292));
            assert_eq!(
                engine.commands().len(),
                initial + 3,
                "a matching frame resets the bounded retry state"
            );
        }

        // Case `cef_outward_rounding_is_not_treated_as_a_stale_frame`.
        {
            let engine = TestEngine::new();
            let (mut view, _, _) = laid_out(engine.clone());
            view.rect = Rect::new(0.0, 0.0, 400.5, 225.5);
            view.scale_factor = 2.0;
            let initial = engine.commands().len();
            assert!(view.retry_resize_for_stale_frame((802, 452)));
            assert_eq!(engine.commands().len(), initial);
        }

        // Case `fractional_dpr_accepts_exact_and_cef_rounded_frames`.
        {
            let engine = TestEngine::new();
            let (mut view, _, _) = laid_out(engine.clone());
            view.rect = Rect::new(0.0, 0.0, 802.0 / 1.75, 451.0 / 1.75);
            view.scale_factor = 1.75;
            let initial = engine.commands().len();
            assert!(
                view.retry_resize_for_stale_frame((804, 452)),
                "CEF rounds requested physical pixels to integer DIP and back out"
            );
            assert_eq!(engine.commands().len(), initial);
            assert!(
                view.retry_resize_for_stale_frame((802, 451)),
                "engines that render the exact physical request are also valid"
            );
            assert_eq!(engine.commands().len(), initial);
        }

        // Case `old_density_frames_are_not_drawn_and_recovery_is_bounded`.
        {
            let engine = TestEngine::new();
            let mut view = WebView::new(engine.clone());
            view.set_viewport(Rect::new(0.0, 0.0, 400.0, 300.0), 2.0);
            engine.send(WebViewCommand::Navigate("about:frame-regression".into()));
            let current = engine.frames().lock().unwrap().clone().unwrap();
            let stale = crate::webview::Frame {
                trace_id: None,
                rgba: Arc::new(vec![255; 400 * 300 * 4]),
                width: 400,
                height: 300,
            };
            let initial = engine.commands().len();
            for (frame, should_draw, retries) in [
                (current.clone(), true, 0),
                (stale.clone(), false, 1),
                (stale.clone(), false, 1),
                (current, true, 1),
                (stale, false, 2),
            ] {
                *engine.frames().lock().unwrap() = Some(frame);
                let mut scene = Scene::new();
                view.draw(
                    &mut scene,
                    Affine::scale(2.0),
                    &mut TextRenderer::new(),
                    &Theme::dark(),
                    &PresenceCtx::default(),
                );
                assert_eq!(
                    scene
                        .encoding()
                        .draw_tags
                        .contains(&vello_encoding::DrawTag::IMAGE),
                    should_draw
                );
                assert_eq!(engine.commands().len(), initial + retries);
            }
        }
    }

    #[test]
    fn engine_frames_draw_at_their_placed_geometry_and_scene_trace() {
        // Case `exact_engine_frames_draw_at_fractional_and_odd_physical_sizes`.
        {
            for (width, height, scale) in [(802, 451, 1.75), (801, 451, 2.0), (800, 600, 2.0)] {
                let engine = TestEngine::new();
                let mut view = WebView::new(engine.clone());
                view.set_viewport(
                    Rect::new(
                        0.0,
                        0.0,
                        f64::from(width) / scale,
                        f64::from(height) / scale,
                    ),
                    scale as f32,
                );
                engine.send(WebViewCommand::Navigate("about:frame-regression".into()));
                let initial = engine.commands().len();
                let mut scene = Scene::new();
                view.draw(
                    &mut scene,
                    Affine::scale(scale),
                    &mut TextRenderer::new(),
                    &Theme::dark(),
                    &PresenceCtx::default(),
                );
                assert!(
                    scene
                        .encoding()
                        .draw_tags
                        .contains(&vello_encoding::DrawTag::IMAGE),
                    "exact {width}x{height} frame at {scale} must draw"
                );
                assert_eq!((view.cached_key.1, view.cached_key.2), (width, height));
                assert_eq!(
                    engine.commands().len(),
                    initial,
                    "valid paint must not retry resize"
                );
            }
        }

        // Case `quantized_engine_frames_draw_on_either_side_of_requested_size`.
        {
            // CEF rounds outward twice; an engine rounding the logical size to
            // nearest turns an 802x451 request at 3x into 267x150 logical /
            // 801x450 physical.
            for (requested, painted, scale) in [
                ((802, 451), (804, 452), 1.75),
                ((802, 451), (801, 450), 3.0),
                ((801, 451), (802, 452), 2.0),
            ] {
                let engine = TestEngine::new();
                let mut view = WebView::new(engine.clone());
                view.set_viewport(
                    Rect::new(
                        0.0,
                        0.0,
                        f64::from(requested.0) / scale,
                        f64::from(requested.1) / scale,
                    ),
                    scale as f32,
                );
                *engine.frames().lock().unwrap() = Some(crate::webview::Frame {
                    trace_id: None,
                    rgba: Arc::new(vec![255; painted.0 as usize * painted.1 as usize * 4]),
                    width: painted.0,
                    height: painted.1,
                });
                let initial = engine.commands().len();
                let mut scene = Scene::new();
                view.draw(
                    &mut scene,
                    Affine::scale(scale),
                    &mut TextRenderer::new(),
                    &Theme::dark(),
                    &PresenceCtx::default(),
                );
                assert!(
                    scene
                        .encoding()
                        .draw_tags
                        .contains(&vello_encoding::DrawTag::IMAGE),
                    "{painted:?} paint for {requested:?} at {scale} must draw"
                );
                assert_eq!((view.cached_key.1, view.cached_key.2), painted);
                assert_eq!(engine.commands().len(), initial);
            }
        }

        // Case `scene_trace_follows_drawn_cached_image_and_excludes_stale_geometry`.
        {
            let engine = TestEngine::new();
            let mut view = WebView::new(engine.clone());
            view.set_viewport(Rect::new(0.0, 0.0, 16.0, 16.0), 1.0);
            let frame = crate::webview::Frame {
                trace_id: Some(101),
                rgba: Arc::new(vec![255; 16 * 16 * 4]),
                width: 16,
                height: 16,
            };
            crate::webview::frame_trace::take_for_test();
            for available in [Some(frame), None] {
                *engine.frames().lock().unwrap() = available;
                let mut scene = Scene::new();
                view.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut TextRenderer::new(),
                    &Theme::dark(),
                    &PresenceCtx::default(),
                );
                assert!(
                    scene
                        .encoding()
                        .draw_tags
                        .contains(&vello_encoding::DrawTag::IMAGE)
                );
                assert_eq!(
                    crate::webview::frame_trace::take_for_test(),
                    (vec![101], false)
                );
            }
            *engine.frames().lock().unwrap() = Some(crate::webview::Frame {
                trace_id: Some(102),
                rgba: Arc::new(vec![255; 8 * 8 * 4]),
                width: 8,
                height: 8,
            });
            let mut scene = Scene::new();
            view.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &PresenceCtx::default(),
            );
            assert!(
                !scene
                    .encoding()
                    .draw_tags
                    .contains(&vello_encoding::DrawTag::IMAGE)
            );
            assert_eq!(
                crate::webview::frame_trace::take_for_test(),
                (vec![], false)
            );
        }

        // Case `scene_trace_follows_nested_layout_on_repeated_and_new_frames`.
        {
            let engine = TestEngine::new();
            let mut view = WebView::new(engine.clone());
            view.set_viewport(Rect::new(0.0, 0.0, 16.0, 16.0), 1.0);
            *engine.frames().lock().unwrap() = Some(crate::webview::Frame {
                trace_id: Some(201),
                rgba: Arc::new(vec![255; 16 * 16 * 4]),
                width: 16,
                height: 16,
            });
            let mut root = crate::widget::Column::new()
                .push(Box::new(crate::widget::Row::new().push(Box::new(view))));
            crate::webview::frame_trace::take_for_test();
            for pass in 0..3 {
                if pass == 1 {
                    *engine.frames().lock().unwrap() = None;
                } else if pass == 2 {
                    *engine.frames().lock().unwrap() = Some(crate::webview::Frame {
                        trace_id: Some(202),
                        rgba: Arc::new(vec![128; 16 * 16 * 4]),
                        width: 16,
                        height: 16,
                    });
                }
                let mut scene = Scene::new();
                root.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut TextRenderer::new(),
                    &Theme::dark(),
                    &PresenceCtx::default(),
                );
                assert!(
                    scene
                        .encoding()
                        .draw_tags
                        .contains(&vello_encoding::DrawTag::IMAGE)
                );
                let expected = if pass == 2 { vec![202] } else { vec![201] };
                assert_eq!(
                    crate::webview::frame_trace::take_for_test(),
                    (expected, false),
                    "pass {pass}"
                );
            }
        }

        // Case `draw_after_navigation_paints_frame_image`.
        {
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

    #[test]
    fn pointer_press_uses_released_modifiers_after_a_native_shortcut() {
        let engine = Arc::new(NativeHistory {
            inner: TestEngine::new(),
            requests: std::sync::Mutex::new(Vec::new()),
            modifiers: std::sync::Mutex::new(Vec::new()),
            pointer_modifiers: std::sync::Mutex::new(Vec::new()),
        });
        engine.set_modifiers(KeyModifiers {
            ctrl: true,
            shift: true,
            ..KeyModifiers::default()
        });
        let (mut page, tree, node) = laid_out(engine.clone());
        let device = unsafe { std::mem::zeroed() };
        dispatch(
            &mut page,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: device,
                position: (50.0, 60.0).into(),
            },
        );
        dispatch(
            &mut page,
            &tree,
            node,
            &WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
        );
        assert_eq!(
            &*engine.pointer_modifiers.lock().unwrap(),
            &[KeyModifiers::default()],
            "pointer default actions must see the current released modifiers before delivery"
        );
    }

    #[test]
    fn pointer_input_forwards_inside_only_and_navigates_history() {
        // Case `pointer_inside_forwards_translated_coordinates`.
        {
            let engine = TestEngine::new();
            let (mut w, tree, node) = laid_out(engine.clone());
            let device = winit::event::DeviceId::dummy();
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

        // Case `pointer_outside_sends_nothing`.
        {
            let engine = TestEngine::new();
            let (mut w, tree, node) = laid_out(engine.clone());
            let device = winit::event::DeviceId::dummy();
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

        // Case `back_side_button_navigates_history`.
        {
            let engine = TestEngine::new();
            let (mut w, tree, node) = laid_out(engine.clone());
            let device = winit::event::DeviceId::dummy();
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

        // Case `wheel_scrolls_in_physical_pixels`.
        {
            let engine = TestEngine::new();
            let (mut w, tree, node) = laid_out(engine.clone());
            let device = winit::event::DeviceId::dummy();
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

        // Case `nav_gesture_sends_history_commands`.
        {
            use crate::gesture::NavDirection;
            let engine = TestEngine::new();
            let mut w = WebView::new(engine.clone());
            let state = AutoCommit::new();
            let root = automerge::ROOT;
            assert!(w.on_nav_gesture(NavDirection::Back, &state, &root));
            assert!(w.on_nav_gesture(NavDirection::Forward, &state, &root));
            let cmds = engine.commands();
            assert!(cmds.contains(&WebViewCommand::GoBack));
            assert!(cmds.contains(&WebViewCommand::GoForward));
        }
    }
}

/// A web view is a window of its own where a page is all there is to show:
/// a browser tab a host streams to this desktop. It keeps no record here.
impl crate::widget::AppWidget for WebView {
    fn uses_host_record(&self) -> bool {
        false
    }
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Browser
    }
    fn app_title(&self) -> &str {
        &self.accessible_name
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("W")
    }
    fn permission_hint(&self) -> Option<crate::widget::AppKind> {
        Some(crate::widget::AppKind::Browser)
    }
}

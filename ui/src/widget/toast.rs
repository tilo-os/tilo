//! `Toast` + `ToastStack` primitive — transient notifications anchored
//! top-right of the shell.
//!
//! Spec: `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` — Phase A
//! step 15 (`ContextMenu`, `Tooltip`, `Toast` primitives landed with their
//! first shell consumer). Toasts are the sink for fallible OS operations
//! (trash/rename/move/copy), `ShellError` conversions and "Workspace
//! created" confirmations.
//!
//! This landing (plan task **T9**) provides the primitive's expiry + cap
//! contract exercised by the plan test plus a `Widget` impl that stacks
//! the active toasts top-right using Plan 1 tokens (no hard-coded colors
//! or spacing).
//!
//! Public surface:
//! - [`ToastKind`] — `Info`, `Success`, `Warning`, `Error`. Maps to
//!   `Theme::tokens.colors.semantic.*` for the accent bar.
//! - [`Toast`] — `text`, `kind`, `ttl_remaining`. Constructed via
//!   [`ToastStack::push`]; not meant to be created directly by shell code.
//! - [`ToastStack`] — ring-buffered list with `new`, `with_max(n)`,
//!   `push(text, kind)`, `tick(dt)`, `len`, `iter`. The default TTL is
//!   `DEFAULT_TTL` (3s); the default cap is `DEFAULT_MAX` (5 toasts).
//!
//! The `Widget` impl draws each active toast as a stacked raised-surface
//! card with a kind-colored accent bar on the left. Enter/exit motion
//! uses `theme.tokens.motion.springs.snappy` — wired by the shell
//! consumer that owns the animation driver (the primitive itself is a
//! pure render of the current state so tests stay deterministic).

use automerge::{AutoCommit, ObjId};
use std::time::Duration;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Default time-to-live for a freshly pushed toast.
pub const DEFAULT_TTL: Duration = Duration::from_millis(3000);

/// Default maximum number of simultaneous toasts. When the cap is
/// exceeded by [`ToastStack::push`] the oldest entry is dropped.
pub const DEFAULT_MAX: usize = 5;

/// Severity / category of a toast. Maps to the semantic color ramp in
/// [`crate::tokens::Colors`] for the left accent bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

/// A single toast entry held by a [`ToastStack`]. Entries are produced
/// internally by [`ToastStack::push`]; shell code does not construct
/// `Toast` values directly.
#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub ttl_remaining: Duration,
}

/// Top-right notification stack with a configurable maximum entry count
/// and TTL-based expiry.
///
/// The stack is render-only — input routing (click-to-dismiss, hover
/// pause) is wired by the first shell consumer. The primitive itself
/// exposes timing/capacity API so it stays unit-testable without a live
/// event loop or a real clock.
pub struct ToastStack {
    toasts: Vec<Toast>,
    max: usize,
    default_ttl: Duration,
    rect: Rect,
    /// Whether the pointer is currently over the stack — [`tick`] pauses
    /// expiry while hovered so the user can read/click a toast.
    hovered: bool,
    last_cursor: Point,
}

impl std::fmt::Debug for ToastStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToastStack")
            .field("toasts", &self.toasts)
            .field("max", &self.max)
            .field("default_ttl", &self.default_ttl)
            .finish()
    }
}

impl Default for ToastStack {
    fn default() -> Self { Self::new() }
}

impl ToastStack {
    /// Build an empty toast stack with the [`DEFAULT_MAX`] cap and
    /// [`DEFAULT_TTL`] per-toast lifetime.
    pub fn new() -> Self {
        Self {
            toasts: Vec::new(),
            max: DEFAULT_MAX,
            default_ttl: DEFAULT_TTL,
            rect: Rect::ZERO,
            hovered: false,
            last_cursor: Point::ZERO,
        }
    }

    /// Override the maximum number of simultaneous toasts.
    pub fn with_max(mut self, max: usize) -> Self {
        self.max = max.max(1);
        self
    }

    /// Override the TTL applied to subsequently pushed toasts.
    pub fn with_default_ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = ttl;
        self
    }

    /// Push a new toast. If the stack is at capacity the oldest entry
    /// is dropped to make room (FIFO drop policy).
    pub fn push(&mut self, text: impl Into<String>, kind: ToastKind) {
        let toast = Toast {
            text: text.into(),
            kind,
            ttl_remaining: self.default_ttl,
        };
        self.toasts.push(toast);
        // Drop oldest entries until we are back within the cap.
        while self.toasts.len() > self.max {
            self.toasts.remove(0);
        }
    }

    /// Advance time. Each toast's `ttl_remaining` is saturatingly
    /// decremented by `dt`; entries that reach zero are removed. Expiry is
    /// **paused while the pointer hovers the stack** so a toast can be read
    /// or clicked without vanishing.
    pub fn tick(&mut self, dt: Duration) {
        if self.hovered {
            return;
        }
        for t in &mut self.toasts {
            t.ttl_remaining = t.ttl_remaining.saturating_sub(dt);
        }
        self.toasts.retain(|t| t.ttl_remaining > Duration::ZERO);
    }

    /// Whether the pointer is currently over the stack.
    pub fn is_hovered(&self) -> bool {
        self.hovered
    }

    /// The card index under a logical point, if any (top-to-bottom layout).
    fn card_at(&self, p: Point) -> Option<usize> {
        if p.x < self.rect.x0 || p.x > self.rect.x0 + TOAST_WIDTH {
            return None;
        }
        let rel = p.y - self.rect.y0;
        if rel < 0.0 {
            return None;
        }
        let stride = TOAST_HEIGHT + TOAST_GAP;
        let idx = (rel / stride) as usize;
        // Reject the inter-card gap.
        if rel - idx as f64 * stride > TOAST_HEIGHT {
            return None;
        }
        if idx < self.toasts.len() { Some(idx) } else { None }
    }

    /// Dismiss the toast at `idx` (no-op if out of range).
    pub fn dismiss(&mut self, idx: usize) {
        if idx < self.toasts.len() {
            self.toasts.remove(idx);
        }
    }

    /// Number of active (non-expired) toasts.
    pub fn len(&self) -> usize { self.toasts.len() }

    /// `true` when no toasts are currently visible.
    pub fn is_empty(&self) -> bool { self.toasts.is_empty() }

    /// Iterate the active toasts in push order (oldest first).
    pub fn iter(&self) -> std::slice::Iter<'_, Toast> { self.toasts.iter() }

    /// Maximum simultaneous toasts.
    pub fn max(&self) -> usize { self.max }

    /// Current default TTL applied to subsequent [`Self::push`] calls.
    pub fn default_ttl(&self) -> Duration { self.default_ttl }
}

const TOAST_WIDTH: f64 = 320.0;
const TOAST_HEIGHT: f64 = 48.0;
const TOAST_GAP: f64 = 8.0;
const ACCENT_BAR_WIDTH: f64 = 4.0;
const PADDING_X: f64 = 14.0;

fn kind_color(kind: ToastKind, theme: &Theme) -> vello::peniko::Color {
    match kind {
        ToastKind::Info => theme.tokens.colors.semantic.info,
        ToastKind::Success => theme.tokens.colors.semantic.success,
        ToastKind::Warning => theme.tokens.colors.semantic.warning,
        ToastKind::Error => theme.tokens.colors.semantic.danger,
    }
}

impl Widget for ToastStack {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let n = self.toasts.len();
        let (w, h) = if n == 0 {
            (0.0f32, 0.0f32)
        } else {
            let height = (TOAST_HEIGHT * n as f64) + (TOAST_GAP * (n.saturating_sub(1)) as f64);
            (TOAST_WIDTH as f32, height as f32)
        };
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(w),
                    height: Dimension::length(h),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.toasts.is_empty() {
            return;
        }

        let radius = theme.tokens.radii.md as f64;
        let text_size = theme.tokens.typography.scale.sm;

        let mut y = self.rect.y0;
        for toast in &self.toasts {
            let card_rect = Rect::new(
                self.rect.x0,
                y,
                self.rect.x0 + TOAST_WIDTH,
                y + TOAST_HEIGHT,
            );
            let shape = RoundedRect::from_rect(card_rect, radius);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.tokens.colors.border_standard,
                None,
                &shape,
            );

            // Left accent bar keyed to the toast's kind.
            let accent = Rect::new(
                card_rect.x0 + 1.0,
                card_rect.y0 + 6.0,
                card_rect.x0 + 1.0 + ACCENT_BAR_WIDTH,
                card_rect.y1 - 6.0,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                kind_color(toast.kind, theme),
                None,
                &RoundedRect::from_rect(accent, theme.tokens.radii.sm as f64),
            );

            let text_y = card_rect.y0 + (TOAST_HEIGHT - text_size as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                &toast.text,
                text_size,
                theme.tokens.colors.text_primary,
                transform
                    * Affine::translate((card_rect.x0 + PADDING_X + ACCENT_BAR_WIDTH, text_y)),
                None,
            );

            y += TOAST_HEIGHT + TOAST_GAP;
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                // Hover-pause: expiry halts while the pointer is over the stack.
                let now_hover = !self.toasts.is_empty() && self.card_at(self.last_cursor).is_some();
                if now_hover != self.hovered {
                    self.hovered = now_hover;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if let Some(idx) = self.card_at(self.last_cursor) {
                    self.dismiss(idx);
                    self.hovered = false;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "ToastStack" }

    fn get_rect(&self) -> Rect { self.rect }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use winit::event::{DeviceId, ElementState, MouseButton};

    fn lay_out(stack: &mut ToastStack) {
        let mut tree = TaffyTree::<()>::new();
        let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let theme = Theme::dark();
        let node = stack.build_layout(&mut tree, &theme, &mut nm);
        tree.compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(TOAST_WIDTH as f32),
                height: taffy::AvailableSpace::Definite(1000.0),
            },
        )
        .unwrap();
        stack.apply_layout(&tree, node, Point::ZERO);
    }

    fn device() -> DeviceId { unsafe { std::mem::zeroed() } }

    fn feed(s: &mut ToastStack, ev: WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        s.handle_event(&ev, 1.0, &Default::default(), &mut doc, &root, &mut handled, &mut focus, node, &tree);
    }

    #[test]
    fn click_dismisses_the_toast_under_the_cursor() {
        let mut s = ToastStack::new();
        s.push("first", ToastKind::Info);
        s.push("second", ToastKind::Error);
        lay_out(&mut s);
        // Card 1 ("second") spans y ∈ [56, 104); centre ≈ (160, 80).
        feed(&mut s, WindowEvent::CursorMoved { device_id: device(), position: (160.0, 80.0).into() });
        feed(&mut s, WindowEvent::MouseInput { device_id: device(), state: ElementState::Pressed, button: MouseButton::Left });
        assert_eq!(s.len(), 1);
        assert_eq!(s.iter().next().unwrap().text, "first");
    }

    #[test]
    fn hover_pauses_expiry() {
        let mut s = ToastStack::new().with_default_ttl(Duration::from_millis(100));
        s.push("stay", ToastKind::Info);
        lay_out(&mut s);
        // Hover over the only card.
        feed(&mut s, WindowEvent::CursorMoved { device_id: device(), position: (160.0, 24.0).into() });
        assert!(s.is_hovered());
        // tick longer than the TTL — but hovered, so it must NOT expire.
        s.tick(Duration::from_millis(500));
        assert_eq!(s.len(), 1, "hovered toast does not expire");
        // Move away → expiry resumes.
        feed(&mut s, WindowEvent::CursorMoved { device_id: device(), position: (900.0, 900.0).into() });
        assert!(!s.is_hovered());
        s.tick(Duration::from_millis(500));
        assert_eq!(s.len(), 0, "expires once un-hovered");
    }
}

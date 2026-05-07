//! Inline permission prompt — the bar that pops in at the top of the
//! active strip when a sandboxed app requests a capability.
//!
//! Mirrors the concept's notification-bar pattern: app icon + capability
//! description + four-action button cluster. Buttons:
//!   - Allow         — grant for this single request (Once)
//!   - This session  — grant until shell restart (Session)
//!   - Always allow  — persist; only enabled for the workspace owner
//!   - Deny          — don't grant; subsequent requests prompt again
//!
//! The widget is render-only relative to the CRDT — its content is fed
//! by a parent (the portal request flow lands in Phase 4 Task 4.3).
//! The user's choice is exposed via [`PermissionPrompt::pending_choice`]
//! as a one-shot output; the parent drains it each frame and applies
//! the resulting [`Capability`] grant to the active workspace.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Fixed bar height matching the concept's notification-bar pattern.
pub const PROMPT_HEIGHT: f64 = 80.0;

/// Padding inside the bar, applied uniformly on the left/right of the
/// icon + label cluster.
const PADDING_X: f64 = 16.0;

/// Horizontal gap between successive button pills.
const BUTTON_GAP: f64 = 8.0;

/// Width of each action button. Four buttons of equal width keeps the
/// layout legible regardless of label length (labels are short and
/// driven by `i18n` keys).
const BUTTON_WIDTH: f64 = 110.0;

/// Height of each action button.
const BUTTON_HEIGHT: f64 = 36.0;

/// Width of the leading app icon square.
const ICON_SIZE: f64 = 36.0;

/// User's response to a [`PermissionPrompt`]. Mirrors the variants of
/// [`crate`]'s consumer-facing `GrantScope` (the shell's
/// `quick_shell::shell::state::GrantScope`) plus an explicit
/// [`PromptChoice::Deny`] for the negative path. Held one-shot on
/// [`PermissionPrompt::pending_choice`] until the host drains it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PromptChoice {
    /// Grant for this single request — don't persist on the workspace.
    Allow,
    /// Hold until shell restart.
    Session,
    /// Persist; owner-only.
    Always,
    /// Reject. Subsequent requests prompt again.
    Deny,
}

/// Inline permission prompt widget.
///
/// Render contract: a horizontal bar of [`PROMPT_HEIGHT`] pixels with
/// the requesting app's icon + label on the left, the requested
/// capability description in the middle, and the four action buttons
/// on the right. The `Always` button is dimmed and ignores clicks
/// when [`PermissionPrompt::is_owner`] is `false`.
#[derive(Clone, Debug)]
pub struct PermissionPrompt {
    rect: Rect,
    /// Wayland app-id of the requesting client (e.g. `org.gnome.gedit`).
    pub app_id: String,
    /// Human-readable description of the requested capability, shown
    /// in the middle slot of the bar (e.g. `"Camera access"`).
    pub capability_label: String,
    /// `true` when the active user is the workspace owner. Owners
    /// see the `Always` button enabled; invited collaborators see
    /// it dimmed and clicks through it are dropped.
    pub is_owner: bool,
    /// One-shot output: set when the user clicks an enabled action
    /// button. The host drains it each frame.
    pub pending_choice: Option<PromptChoice>,

    // Hit-test rects updated each draw pass (or by `apply_layout_helper`
    // in tests). All four are `Rect::ZERO` until the first layout call.
    allow_rect: Rect,
    session_rect: Rect,
    always_rect: Rect,
    deny_rect: Rect,
    /// Last-known cursor position in logical pixels. Tracked for
    /// hover-state contrast and to disambiguate `MouseInput` events
    /// (which carry no position payload).
    last_cursor: Point,
    /// 0..=3 — index of the button currently under the cursor, or
    /// `None` when the cursor is outside every button rect. Drives
    /// the per-button hover background.
    hover_idx: Option<u8>,
}

impl PermissionPrompt {
    /// Construct a prompt for the given app + capability description.
    /// `is_owner` controls whether the `Always` button is interactive.
    pub fn new(app_id: String, capability_label: String, is_owner: bool) -> Self {
        Self {
            rect: Rect::ZERO,
            app_id,
            capability_label,
            is_owner,
            pending_choice: None,
            allow_rect: Rect::ZERO,
            session_rect: Rect::ZERO,
            always_rect: Rect::ZERO,
            deny_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            hover_idx: None,
        }
    }

    /// Convenience helper — owner-prompt construction used by
    /// internal call-sites that always want the `Always` button
    /// enabled.
    pub fn for_owner(app_id: impl Into<String>, capability_label: impl Into<String>) -> Self {
        Self::new(app_id.into(), capability_label.into(), true)
    }

    /// Override the bar's screen-space rect outside a taffy pass.
    /// Used by hosts that paint the prompt as an absolutely-positioned
    /// overlay (the shell renders it at the top of the active strip
    /// without going through `build_layout` / `apply_layout`).
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.apply_layout_helper();
    }

    /// Recompute the four button hit-test rects from the current
    /// `self.rect`. Exposed so tests can drive
    /// `apply_layout_helper(); handle_event(...)` without spinning
    /// up a full taffy pass.
    pub fn apply_layout_helper(&mut self) {
        let by = self.rect.y0 + (PROMPT_HEIGHT - BUTTON_HEIGHT) / 2.0;
        let by1 = by + BUTTON_HEIGHT;
        // Buttons right-aligned: rightmost is Deny, then Always,
        // Session, Allow as we move left. Visual order on-screen is
        // left-to-right [Allow, Session, Always, Deny] — first the
        // positive variants, then the negative. Compute from the
        // right edge so a narrower bar still keeps the buttons inside.
        let right_edge = self.rect.x1 - PADDING_X;
        let deny_x1 = right_edge;
        let deny_x0 = deny_x1 - BUTTON_WIDTH;
        let always_x1 = deny_x0 - BUTTON_GAP;
        let always_x0 = always_x1 - BUTTON_WIDTH;
        let session_x1 = always_x0 - BUTTON_GAP;
        let session_x0 = session_x1 - BUTTON_WIDTH;
        let allow_x1 = session_x0 - BUTTON_GAP;
        let allow_x0 = allow_x1 - BUTTON_WIDTH;
        self.allow_rect = Rect::new(allow_x0, by, allow_x1, by1);
        self.session_rect = Rect::new(session_x0, by, session_x1, by1);
        self.always_rect = Rect::new(always_x0, by, always_x1, by1);
        self.deny_rect = Rect::new(deny_x0, by, deny_x1, by1);
    }

    /// Test/host accessor for the `Allow` button rect.
    pub fn allow_rect(&self) -> Rect { self.allow_rect }
    /// Test/host accessor for the `Session` button rect.
    pub fn session_rect(&self) -> Rect { self.session_rect }
    /// Test/host accessor for the `Always` button rect. Even when the
    /// button is disabled this rect is non-zero — hosts use it to
    /// hit-test for tooltip-style "owner-only" affordances.
    pub fn always_rect(&self) -> Rect { self.always_rect }
    /// Test/host accessor for the `Deny` button rect.
    pub fn deny_rect(&self) -> Rect { self.deny_rect }
}

fn button_label(choice: PromptChoice) -> &'static str {
    match choice {
        PromptChoice::Allow => "Allow",
        PromptChoice::Session => "This session",
        PromptChoice::Always => "Always allow",
        PromptChoice::Deny => "Deny",
    }
}

fn draw_button(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    is_hover: bool,
    is_disabled: bool,
    is_primary: bool,
) {
    let radius = theme.tokens.radii.md as f64;
    let shape = RoundedRect::from_rect(rect, radius);

    // Background colour: primary action (`Allow`) gets the accent
    // tint; the rest pull from the raised surface token. Disabled
    // buttons fade to 40% alpha (overridden by the caller via the
    // `is_disabled` flag rather than rendered by the parent — keeps
    // hit-test/draw symmetric).
    let mut bg = if is_primary {
        theme.tokens.colors.accent.stops[crate::tokens::colors::ACCENT_PRIMARY_IDX]
    } else if is_hover {
        theme.bg_hover
    } else {
        theme.tokens.colors.surfaces.raised
    };
    if is_disabled {
        // 40% alpha — the brief explicitly calls out that the disabled
        // `Always` button must render at 40% alpha for non-owners.
        bg = bg.with_alpha_factor(0.4);
    }
    scene.fill(Fill::NonZero, transform, bg, None, &shape);

    let border = if is_disabled {
        theme.tokens.colors.border_standard.with_alpha_factor(0.4)
    } else {
        theme.tokens.colors.border_standard
    };
    scene.stroke(&Stroke::new(1.0), transform, border, None, &shape);

    // Label colour mirrors the disabled/enabled split.
    let text_color = if is_disabled {
        theme.tokens.colors.text_primary.with_alpha_factor(0.4)
    } else if is_primary {
        // Accent buttons get the white-on-accent treatment from the
        // concept (concept ref: every `bg-indigo-500` button uses
        // `text-white`).
        Color::rgb8(255, 255, 255)
    } else {
        theme.tokens.colors.text_primary
    };

    let text_size = theme.tokens.typography.scale.sm;
    let (text_w, _) = text_renderer.measure(label, text_size, None);
    let text_x = rect.center().x - (text_w as f64 / 2.0);
    let text_y = rect.y0 + (rect.height() - text_size as f64) / 2.0;
    text_renderer.draw_text(
        scene,
        label,
        text_size,
        text_color,
        transform * Affine::translate((text_x, text_y)),
        None,
    );
}

impl Widget for PermissionPrompt {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        // No CRDT sync at v1 — the prompt's content is pushed by
        // the host (Phase 4 Task 4.3's portal request flow), not
        // pulled from the doc. Future iterations may sync the
        // pending-grants queue itself if collaboration semantics
        // demand it.
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(PROMPT_HEIGHT as f32),
                },
                flex_grow: 0.0,
                flex_shrink: 0.0,
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
        self.apply_layout_helper();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Bar background — translucent overlay surface. Concept ref:
        // notifications use the `surfaces.overlay` token (a darker
        // tint than `raised`) so they read as a layer above the
        // strip's content rather than another tile.
        let radius = theme.tokens.radii.md as f64;
        let shape = RoundedRect::from_rect(self.rect, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
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

        // App icon — flat rounded square pulled from the accent ramp.
        // The portal layer (Task 4.3) will surface real icons via the
        // app's desktop-file metadata; for v1 we paint a placeholder
        // so the bar is visually balanced.
        let icon_x = self.rect.x0 + PADDING_X;
        let icon_y = self.rect.y0 + (PROMPT_HEIGHT - ICON_SIZE) / 2.0;
        let icon_rect = Rect::new(icon_x, icon_y, icon_x + ICON_SIZE, icon_y + ICON_SIZE);
        let icon_shape = RoundedRect::from_rect(icon_rect, theme.tokens.radii.sm as f64);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.accent.stops[crate::tokens::colors::ACCENT_PRIMARY_IDX],
            None,
            &icon_shape,
        );

        // App-id label (left cluster, beneath the icon's right edge).
        let label_x = icon_x + ICON_SIZE + theme.tokens.spacing.s2 as f64;
        let app_id_size = theme.tokens.typography.scale.sm;
        let cap_size = theme.tokens.typography.scale.base;
        let label_y = self.rect.y0
            + (PROMPT_HEIGHT - (app_id_size as f64 + cap_size as f64 + 4.0)) / 2.0;
        text_renderer.draw_text(
            scene,
            &self.app_id,
            app_id_size,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((label_x, label_y)),
            None,
        );
        text_renderer.draw_text(
            scene,
            &self.capability_label,
            cap_size,
            theme.tokens.colors.text_primary,
            transform
                * Affine::translate((label_x, label_y + app_id_size as f64 + 4.0)),
            None,
        );

        // Recompute button rects so a draw without a preceding
        // `apply_layout` (e.g. a synthetic test path) still hit-tests
        // correctly. Cheap — just four `Rect::new` calls.
        self.apply_layout_helper();

        // Action buttons — left to right [Allow, Session, Always, Deny].
        let buttons = [
            (self.allow_rect, PromptChoice::Allow, true), // primary (accent)
            (self.session_rect, PromptChoice::Session, false),
            (self.always_rect, PromptChoice::Always, false),
            (self.deny_rect, PromptChoice::Deny, false),
        ];
        for (i, (rect, choice, is_primary)) in buttons.iter().enumerate() {
            let is_disabled = matches!(choice, PromptChoice::Always) && !self.is_owner;
            let is_hover = self.hover_idx == Some(i as u8) && !is_disabled;
            draw_button(
                scene,
                transform,
                text_renderer,
                theme,
                *rect,
                button_label(*choice),
                is_hover,
                is_disabled,
                *is_primary,
            );
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
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = p;
                let new_idx = if self.allow_rect.contains(p) {
                    Some(0)
                } else if self.session_rect.contains(p) {
                    Some(1)
                } else if self.always_rect.contains(p) {
                    // Owner-only — non-owners get no hover affordance
                    // on the disabled button.
                    if self.is_owner { Some(2) } else { None }
                } else if self.deny_rect.contains(p) {
                    Some(3)
                } else {
                    None
                };
                if new_idx != self.hover_idx {
                    self.hover_idx = new_idx;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_cursor;
                if self.allow_rect.contains(p) {
                    self.pending_choice = Some(PromptChoice::Allow);
                    *handled = true;
                } else if self.session_rect.contains(p) {
                    self.pending_choice = Some(PromptChoice::Session);
                    *handled = true;
                } else if self.always_rect.contains(p) {
                    if self.is_owner {
                        self.pending_choice = Some(PromptChoice::Always);
                        *handled = true;
                    }
                    // Non-owner click on the disabled `Always` button
                    // is intentionally dropped — leave `pending_choice`
                    // untouched so the host's drain doesn't see a
                    // ghost grant.
                } else if self.deny_rect.contains(p) {
                    self.pending_choice = Some(PromptChoice::Deny);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "PermissionPrompt" }

    fn get_rect(&self) -> Rect { self.rect }
}

#[cfg(test)]
mod tests {
    //! Coverage for the Phase 4 Task 4.2 inline-prompt widget. Tests
    //! exercise the four button-click branches end-to-end without a
    //! taffy pass — `apply_layout_helper` populates the hit-test
    //! rects from a synthetic `self.rect` so we don't depend on a
    //! GPU/text-renderer.
    use super::*;
    use winit::event::{DeviceId, ElementState, MouseButton};

    fn synth_device() -> DeviceId {
        // SAFETY: `DeviceId` is opaque; tests construct one via
        // `mem::zeroed`. Same pattern as `quick-ui::widget::slider`'s
        // own test helpers.
        unsafe { std::mem::zeroed() }
    }

    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }

    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    /// Drive an event through the prompt with default scaffolding.
    fn dispatch(p: &mut PermissionPrompt, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        p.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
    }

    /// Build a 600x80 prompt with computed button rects so each
    /// click-through test starts from a known geometry. `is_owner`
    /// is the only knob — every other field is fixed.
    fn prompt_at(is_owner: bool) -> PermissionPrompt {
        let mut p = PermissionPrompt::new(
            "org.example.app".into(),
            "Camera access".into(),
            is_owner,
        );
        p.rect = Rect::new(0.0, 0.0, 600.0, 80.0);
        p.apply_layout_helper();
        p
    }

    #[test]
    fn click_allow_sets_pending_choice_to_allow() {
        let mut p = prompt_at(false);
        let center = p.allow_rect().center();
        dispatch(&mut p, &cursor_at(center));
        dispatch(&mut p, &left_press());
        assert_eq!(p.pending_choice, Some(PromptChoice::Allow));
    }

    #[test]
    fn click_session_sets_pending_choice_to_session() {
        let mut p = prompt_at(false);
        let center = p.session_rect().center();
        dispatch(&mut p, &cursor_at(center));
        dispatch(&mut p, &left_press());
        assert_eq!(p.pending_choice, Some(PromptChoice::Session));
    }

    #[test]
    fn always_button_disabled_for_non_owner_drops_clicks() {
        let mut p = prompt_at(false);
        let center = p.always_rect().center();
        dispatch(&mut p, &cursor_at(center));
        dispatch(&mut p, &left_press());
        // Non-owner cannot escalate to `Always` — the click is
        // silently dropped, leaving the host's drain to see nothing.
        assert_eq!(p.pending_choice, None);
    }

    #[test]
    fn always_button_active_for_owner_sets_choice_to_always() {
        let mut p = prompt_at(true);
        let center = p.always_rect().center();
        dispatch(&mut p, &cursor_at(center));
        dispatch(&mut p, &left_press());
        assert_eq!(p.pending_choice, Some(PromptChoice::Always));
    }

    #[test]
    fn deny_click_sets_pending_choice_to_deny() {
        let mut p = prompt_at(false);
        let center = p.deny_rect().center();
        dispatch(&mut p, &cursor_at(center));
        dispatch(&mut p, &left_press());
        assert_eq!(p.pending_choice, Some(PromptChoice::Deny));
    }

    #[test]
    fn click_outside_button_rects_does_not_set_choice() {
        // Click on the icon (left edge of the bar) — far from any
        // button rect. The prompt must not fabricate a choice.
        let mut p = prompt_at(true);
        dispatch(&mut p, &cursor_at(Point::new(20.0, 40.0)));
        dispatch(&mut p, &left_press());
        assert_eq!(p.pending_choice, None);
    }

    #[test]
    fn build_layout_emits_fixed_height_node() {
        // Pin the bar's intrinsic height — overlay-host wires the
        // prompt at the top of a strip and depends on the height
        // being exactly `PROMPT_HEIGHT` so the strip's content
        // shifts down by the right amount.
        let mut p = prompt_at(false);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let theme = Theme::dark();
        let node = p.build_layout(&mut taffy, &theme, &mut node_map);
        let style = taffy.style(node).unwrap();
        // `Dimension` is opaque — compare equality against the
        // canonical `length(...)` constructor instead of pattern-
        // matching on a private representation.
        assert_eq!(style.size.height, Dimension::length(PROMPT_HEIGHT as f32));
    }
}

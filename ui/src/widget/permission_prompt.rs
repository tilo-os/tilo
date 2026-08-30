//! Inline permission prompt — the bar that pops in at the top of the
//! active strip when a sandboxed app requests a capability.
//!
//! App icon + capability description + four-action button cluster: Allow
//! (this request only), This session (until shell restart), Always allow
//! (persists; owner-only), Deny (subsequent requests prompt again).
//!
//! Render-only relative to the CRDT — its content is fed by a parent. The
//! user's choice is exposed via [`PermissionPrompt::pending_choice`] as a
//! one-shot output; the parent drains it each frame and applies the
//! resulting [`Capability`] grant.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

pub const PROMPT_HEIGHT: f64 = 80.0;
const PADDING_X: f64 = 16.0;
const BUTTON_GAP: f64 = 8.0;
/// Four buttons of equal width keeps the layout legible regardless of
/// label length.
const BUTTON_WIDTH: f64 = 110.0;
const BUTTON_HEIGHT: f64 = 36.0;
const ICON_SIZE: f64 = 36.0;

/// Mirrors the variants of the shell's consumer-facing
/// `quick_shell::shell::state::GrantScope` plus an explicit
/// [`PromptChoice::Deny`] for the negative path. Held one-shot on
/// [`PermissionPrompt::pending_choice`] until the host drains it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PromptChoice {
    /// Don't persist on the workspace.
    Allow,
    /// Hold until shell restart.
    Session,
    /// Persist; owner-only.
    Always,
    /// Subsequent requests prompt again.
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
    pub app_id: String,
    pub capability_label: String,
    /// Owners see the `Always` button enabled; invited collaborators see it
    /// dimmed and clicks through it are dropped.
    pub is_owner: bool,
    pub pending_choice: Option<PromptChoice>,

    // Updated each draw pass (or by `apply_layout_helper` in tests). All
    // four are `Rect::ZERO` until the first layout call.
    allow_rect: Rect,
    session_rect: Rect,
    always_rect: Rect,
    deny_rect: Rect,
    /// For hover-state contrast and to disambiguate `MouseInput` events (no position payload).
    last_cursor: Point,
    /// `None` outside every button rect. Drives the per-button hover background.
    hover_idx: Option<u8>,
}

impl PermissionPrompt {
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

    /// For call-sites that always want the `Always` button enabled.
    pub fn for_owner(app_id: impl Into<String>, capability_label: impl Into<String>) -> Self {
        Self::new(app_id.into(), capability_label.into(), true)
    }

    /// For hosts that paint the prompt as an absolutely-positioned overlay
    /// without going through `build_layout` / `apply_layout`.
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.apply_layout_helper();
    }

    /// Exposed so tests can drive layout without a full taffy pass.
    pub fn apply_layout_helper(&mut self) {
        let by = self.rect.y0 + (PROMPT_HEIGHT - BUTTON_HEIGHT) / 2.0;
        let by1 = by + BUTTON_HEIGHT;
        // Computed from the right edge inward (Deny, Always, Session, Allow)
        // so a narrower bar keeps the buttons inside; on-screen this reads
        // left-to-right as [Allow, Session, Always, Deny].
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

    pub fn allow_rect(&self) -> Rect {
        self.allow_rect
    }
    pub fn session_rect(&self) -> Rect {
        self.session_rect
    }
    /// Non-zero even when disabled — hosts hit-test it for "owner-only" affordances.
    pub fn always_rect(&self) -> Rect {
        self.always_rect
    }
    pub fn deny_rect(&self) -> Rect {
        self.deny_rect
    }
}

fn button_label(choice: PromptChoice) -> &'static str {
    match choice {
        PromptChoice::Allow => "Allow",
        PromptChoice::Session => "This session",
        PromptChoice::Always => "Always allow",
        PromptChoice::Deny => "Deny",
    }
}

// Standard immediate-mode paint-helper signature for this vello renderer
// (scene + transform + text_renderer + theme, plus this draw's own
// content); every parameter is independently load-bearing.
#[allow(clippy::too_many_arguments)]
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
    let radius = f64::from(theme.tokens.radii.md);
    let shape = RoundedRect::from_rect(rect, radius);

    let mut bg = if is_primary {
        theme.tokens.colors.accent.stops[crate::tokens::colors::ACCENT_PRIMARY_IDX]
    } else if is_hover {
        theme.bg_hover
    } else {
        theme.tokens.colors.surfaces.raised
    };
    if is_disabled {
        bg = bg.with_alpha_factor(0.4);
    }
    scene.fill(Fill::NonZero, transform, bg, None, &shape);

    let border = if is_disabled {
        theme.tokens.colors.border_standard.with_alpha_factor(0.4)
    } else {
        theme.tokens.colors.border_standard
    };
    scene.stroke(&Stroke::new(1.0), transform, border, None, &shape);

    let text_color = if is_disabled {
        theme.tokens.colors.text_primary.with_alpha_factor(0.4)
    } else if is_primary {
        // White is the intent; steps off white only when the accent is too
        // light to carry it, rather than shipping a ratio that makes a
        // consent decision hard to read.
        theme.tokens.colors.ink_on_any(
            &[bg],
            Color::WHITE,
            crate::tokens::colors::ON_FILL_MIN_CONTRAST,
        )
    } else {
        theme.tokens.colors.text_primary
    };

    let text_size = theme.tokens.typography.scale.sm;
    let (text_w, _) = text_renderer.measure(label, text_size, None);
    let text_x = rect.center().x - (f64::from(text_w) / 2.0);
    let text_y = rect.y0 + (rect.height() - f64::from(text_size)) / 2.0;
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
        // Content is pushed by the host's portal request flow, not pulled from the doc.
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(PROMPT_HEIGHT as f32),
            },
            flex_grow: 0.0,
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
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
        // `surfaces.overlay` is a darker tint than `raised` so the prompt
        // reads as a layer above the strip's content, not another tile.
        let radius = f64::from(theme.tokens.radii.md);
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

        // Placeholder pulled from the accent ramp until the portal layer
        // surfaces real icons via the app's desktop-file metadata.
        let icon_x = self.rect.x0 + PADDING_X;
        let icon_y = self.rect.y0 + (PROMPT_HEIGHT - ICON_SIZE) / 2.0;
        let icon_rect = Rect::new(icon_x, icon_y, icon_x + ICON_SIZE, icon_y + ICON_SIZE);
        let icon_shape = RoundedRect::from_rect(icon_rect, f64::from(theme.tokens.radii.sm));
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.accent.stops[crate::tokens::colors::ACCENT_PRIMARY_IDX],
            None,
            &icon_shape,
        );

        let label_x = icon_x + ICON_SIZE + f64::from(theme.tokens.spacing.s2);
        let app_id_size = theme.tokens.typography.scale.sm;
        let cap_size = theme.tokens.typography.scale.base;
        let label_y = self.rect.y0
            + (PROMPT_HEIGHT - (f64::from(app_id_size) + f64::from(cap_size) + 4.0)) / 2.0;
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
            transform * Affine::translate((label_x, label_y + f64::from(app_id_size) + 4.0)),
            None,
        );

        // So a draw without a preceding `apply_layout` still hit-tests correctly.
        self.apply_layout_helper();

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
                    // Non-owners get no hover affordance on the disabled button.
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
                    // Dropped for a non-owner: `pending_choice` stays untouched.
                } else if self.deny_rect.contains(p) {
                    self.pending_choice = Some(PromptChoice::Deny);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "PermissionPrompt"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(format!(
                    "{} requested by {}",
                    self.capability_label, self.app_id
                ))
                .with_state(AccessState {
                    modal: true,
                    ..Default::default()
                }),
        )
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    //! `apply_layout_helper` populates the hit-test rects from a synthetic
    //! `self.rect` so tests don't depend on a GPU/text-renderer.
    use super::*;
    use winit::event::{DeviceId, ElementState, MouseButton};

    fn synth_device() -> DeviceId {
        // SAFETY: `DeviceId` is opaque; tests construct one via
        // `mem::zeroed`. Same pattern as `quick-ui::widget::slider`'s
        // own test helpers.
        unsafe { std::mem::zeroed() }
    }

    #[test]
    fn accessibility_reports_modal_dialog_with_request_name() {
        use crate::a11y::Role;
        let p = PermissionPrompt::new("org.gnome.gedit".into(), "Camera access".into(), true);
        let node = Widget::accessibility(&p).expect("prompt is not presentational");
        assert_eq!(node.role, Role::Dialog);
        assert!(node.state.modal, "a permission prompt traps focus");
        assert_eq!(
            node.name.as_deref(),
            Some("Camera access requested by org.gnome.gedit")
        );
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

    fn dispatch(p: &mut PermissionPrompt, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
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

    /// 600x80 with computed button rects; `is_owner` is the only
    /// knob, every other field is fixed.
    fn prompt_at(is_owner: bool) -> PermissionPrompt {
        let mut p =
            PermissionPrompt::new("org.example.app".into(), "Camera access".into(), is_owner);
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
        // Click on the icon (left edge of the bar), far from any
        // button rect.
        let mut p = prompt_at(true);
        dispatch(&mut p, &cursor_at(Point::new(20.0, 40.0)));
        dispatch(&mut p, &left_press());
        assert_eq!(p.pending_choice, None);
    }

    #[test]
    fn build_layout_emits_fixed_height_node() {
        // The overlay-host depends on this being exactly `PROMPT_HEIGHT` so
        // the strip's content shifts down by the right amount.
        let mut p = prompt_at(false);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let theme = Theme::dark();
        let node = p.build_layout(&mut taffy, &theme, &mut node_map);
        let style = taffy.style(node).unwrap();
        // `Dimension` is opaque, so this compares against the canonical
        // constructor rather than pattern-matching a private representation.
        assert_eq!(style.size.height, Dimension::length(PROMPT_HEIGHT as f32));
    }
}

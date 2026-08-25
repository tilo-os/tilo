//! Inline permission prompt — the bar that pops in at the top of the
//! active strip when a sandboxed app requests a capability.
//!
//! Mirrors the concept's notification-bar pattern: app icon + capability
//! description + action button cluster. Buttons:
//!   - Allow         — grant for this single request (Once)
//!   - This session  — grant until shell restart (Session)
//!   - Always allow  — persist; **offered only to the workspace owner**
//!   - Deny          — don't grant; subsequent requests prompt again
//!
//! `Always allow` used to be drawn for everyone and dimmed to 40% for
//! anyone who was not the owner, with nothing on the bar saying why. A
//! guest in somebody else's room was therefore handed a consent decision
//! with a dead control in the middle of it — the one arrangement that
//! reads as the machine being broken rather than as the answer being
//! unavailable. It is now simply absent for a non-owner, and the three
//! buttons that *are* live fill the row. The hit-test rect goes empty
//! with it, so a click where it used to sit still cannot mint a grant.
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

use crate::color_compat::ColorCompat;
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

/// How far the app's glyph sits inside its backplate, so the mark reads
/// as sitting *on* the square rather than being cropped by it.
const GLYPH_INSET: f64 = 8.0;

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
    ///
    /// The **key**, not the label: the host matches this against the head
    /// of its pending-grant queue before applying an answer. What the
    /// person reads is [`Self::app_label`].
    pub app_id: String,
    /// What to call the requesting app on screen.
    ///
    /// A consent decision is about a program the person knows by name,
    /// and a reverse-DNS id is not that name. The bar used to draw
    /// `app_id` directly, so the one question in the system that most
    /// needs to be understood asked it about `dev.qos.file-explorer` —
    /// while the tab of the very window it was covering read `Files`,
    /// because tab labels have gone through the installed-app manifest
    /// for a long time. The host fills this in from the same lookup;
    /// it falls back to the id when there is genuinely no name to give.
    pub app_label: String,
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
            app_label: app_id.clone(),
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

    /// Name the requesting app the way the rest of the desktop names it.
    ///
    /// Separate from the constructor because the name and the id come
    /// from different places: the id is the queue's key and is always
    /// known, the name is a lookup the *shell* can do and this crate
    /// cannot. A blank name is ignored rather than drawn, so a failed
    /// lookup leaves the id showing instead of an empty line.
    #[must_use]
    pub fn with_app_label(mut self, label: impl Into<String>) -> Self {
        let label = label.into();
        if !label.trim().is_empty() {
            self.app_label = label;
        }
        self
    }

    /// Whether the person answering may escalate to a permanent grant.
    /// When false the `Always allow` button is not offered at all —
    /// see the module docs.
    pub fn offers_always(&self) -> bool {
        self.is_owner
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
        //
        // A non-owner is not offered `Always allow`, so its slot is not
        // reserved either: the row closes up to three buttons and the
        // rect goes empty. Empty rather than merely undrawn, because
        // `handle_event` hit-tests these rects — a rect left in place
        // for a button nobody can see is a live click target for a
        // control that is not there.
        let right_edge = self.rect.x1 - PADDING_X;
        let deny_x1 = right_edge;
        let deny_x0 = deny_x1 - BUTTON_WIDTH;
        self.deny_rect = Rect::new(deny_x0, by, deny_x1, by1);
        let session_x1 = if self.is_owner {
            let always_x1 = deny_x0 - BUTTON_GAP;
            let always_x0 = always_x1 - BUTTON_WIDTH;
            self.always_rect = Rect::new(always_x0, by, always_x1, by1);
            always_x0 - BUTTON_GAP
        } else {
            self.always_rect = Rect::ZERO;
            deny_x0 - BUTTON_GAP
        };
        let session_x0 = session_x1 - BUTTON_WIDTH;
        let allow_x1 = session_x0 - BUTTON_GAP;
        let allow_x0 = allow_x1 - BUTTON_WIDTH;
        self.allow_rect = Rect::new(allow_x0, by, allow_x1, by1);
        self.session_rect = Rect::new(session_x0, by, session_x1, by1);
    }

    /// Test/host accessor for the `Allow` button rect.
    pub fn allow_rect(&self) -> Rect { self.allow_rect }
    /// Test/host accessor for the `Session` button rect.
    pub fn session_rect(&self) -> Rect { self.session_rect }
    /// Test/host accessor for the `Always` button rect. [`Rect::ZERO`]
    /// when the button is not offered, which is the case for anyone but
    /// the workspace owner — nothing is drawn there and nothing can be
    /// clicked there.
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
    is_primary: bool,
) {
    let radius = theme.tokens.radii.md as f64;
    let shape = RoundedRect::from_rect(rect, radius);

    // Background colour: primary action (`Allow`) gets the accent
    // tint; the rest pull from the raised surface token. Every button
    // this bar draws is live — an unavailable one is not drawn at all —
    // so there is no dimmed variant to account for.
    let bg = if is_primary {
        theme.tokens.colors.accent.stops[crate::tokens::colors::ACCENT_PRIMARY_IDX]
    } else if is_hover {
        theme.bg_hover
    } else {
        theme.tokens.colors.surfaces.raised
    };
    scene.fill(Fill::NonZero, transform, bg, None, &shape);

    scene.stroke(
        &Stroke::new(1.0),
        transform,
        theme.tokens.colors.border_standard,
        None,
        &shape,
    );

    let text_color = if is_primary {
        // White on the accent is the intent, and where the accent is dark
        // enough to carry it that is what comes back. Where it is not, the
        // label steps out of white rather than shipping at a ratio that
        // makes a consent decision hard to read.
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

        // App icon — the app's own mark on an accent backplate.
        //
        // The backplate alone used to be the whole icon, described in a
        // comment as a placeholder until real icons arrived. They did not,
        // and a bar that asks "may this program read your files?" was
        // answering "which program?" with a blank blue square. The glyph
        // comes from the shared table so the same app carries the same
        // mark here as in the palette and the window chips.
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
        // Ink measured against the accent rather than assumed white: the
        // accent is a light lilac on some palettes and white-on-white is
        // how the backplate would go back to being featureless.
        crate::icons::draw_icon(
            scene,
            transform,
            icon_rect.inset(-GLYPH_INSET),
            crate::icons::icon_for_app_id(&self.app_id),
            theme
                .tokens
                .colors
                .on_accent(crate::tokens::colors::NON_TEXT_MIN_CONTRAST),
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );

        // App name (left cluster, beneath the icon's right edge), with
        // the capability under it. The name, not the id — see
        // `app_label`.
        let label_x = icon_x + ICON_SIZE + theme.tokens.spacing.s2 as f64;
        let app_id_size = theme.tokens.typography.scale.sm;
        let cap_size = theme.tokens.typography.scale.base;
        let label_y = self.rect.y0
            + (PROMPT_HEIGHT - (app_id_size as f64 + cap_size as f64 + 4.0)) / 2.0;
        text_renderer.draw_text(
            scene,
            &self.app_label,
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
            // `Always allow` is the owner's to make. For anybody else the
            // slot is empty and nothing is drawn there — a control the
            // person cannot use, with no room on an 80px bar to say why,
            // is worse than no control.
            if matches!(choice, PromptChoice::Always) && !self.is_owner {
                continue;
            }
            let is_hover = self.hover_idx == Some(i as u8);
            draw_button(
                scene,
                transform,
                text_renderer,
                theme,
                *rect,
                button_label(*choice),
                is_hover,
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
                    // Owner-only. For anybody else the rect is empty and
                    // this arm is unreachable, so the guard is belt and
                    // braces against a host that set the rect itself.
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

    /// Accessibility: a permission request is a modal, attention-demanding
    /// dialog. `Role::Dialog` with `modal = true`; the accessible name states
    /// what is being requested ("Camera access requested by org.gnome.gedit")
    /// so an assertive screen-reader announcement is meaningful.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(format!(
                    "{} requested by {}",
                    self.capability_label, self.app_label
                ))
                .with_state(AccessState {
                    modal: true,
                    ..Default::default()
                }),
        )
    }

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

    #[test]
    fn accessibility_reports_modal_dialog_with_request_name() {
        use crate::a11y::Role;
        let p = PermissionPrompt::new(
            "org.gnome.gedit".into(),
            "Camera access".into(),
            true,
        );
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
    fn a_non_owner_is_not_offered_the_always_button_at_all() {
        // Not "offered and dimmed" — absent. An empty rect is how the
        // draw pass and the hit test agree there is no control there.
        let p = prompt_at(false);
        assert!(!p.offers_always());
        assert_eq!(
            p.always_rect(),
            Rect::ZERO,
            "the owner-only button must not reserve a slot for a non-owner"
        );
        // And the three that remain still occupy the row, in order,
        // without the hole the fourth used to leave behind.
        assert!(
            p.allow_rect().x1 <= p.session_rect().x0,
            "Allow sits left of This session"
        );
        assert!(
            p.session_rect().x1 <= p.deny_rect().x0,
            "This session sits left of Deny"
        );
        assert!(
            p.deny_rect().x0 - p.session_rect().x1 <= BUTTON_GAP + 0.5,
            "the row closed up: {} px between This session and Deny",
            p.deny_rect().x0 - p.session_rect().x1
        );
    }

    #[test]
    fn the_owner_still_gets_four_buttons_in_order() {
        let p = prompt_at(true);
        assert!(p.offers_always());
        assert!(p.allow_rect().x1 <= p.session_rect().x0);
        assert!(p.session_rect().x1 <= p.always_rect().x0);
        assert!(p.always_rect().x1 <= p.deny_rect().x0);
    }

    #[test]
    fn the_bar_names_the_app_the_way_the_desktop_names_it() {
        // The id is the queue's key and stays the key; what a person
        // reads is the name the host looked up.
        let p = PermissionPrompt::new(
            "dev.qos.file-explorer".into(),
            "read/write access to /var/tmp/qos/home/xaver".into(),
            false,
        )
        .with_app_label("Files");
        assert_eq!(p.app_id, "dev.qos.file-explorer", "the key is unchanged");
        assert_eq!(p.app_label, "Files");
        let node = Widget::accessibility(&p).expect("prompt is not presentational");
        assert_eq!(
            node.name.as_deref(),
            Some("read/write access to /var/tmp/qos/home/xaver requested by Files"),
            "a screen reader hears the name too, not the reverse-DNS id"
        );
    }

    #[test]
    fn a_blank_name_leaves_the_id_showing() {
        // A lookup that came back empty must not blank the line — an
        // unrecognised app is still better named by its id than by
        // nothing at all.
        let p = PermissionPrompt::new("com.example.unknown".into(), "Camera access".into(), true)
            .with_app_label("   ");
        assert_eq!(p.app_label, "com.example.unknown");
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

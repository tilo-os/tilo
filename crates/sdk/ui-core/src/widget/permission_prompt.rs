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
//! resulting `Capability` grant.

use crate::widget::ModelPath;
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_metrics::centered_text_top;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
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

/// Stable synthetic-node keys for the four choices. They deliberately do not
/// depend on the translated label or visual order, so assistive technology can
/// keep pointing at the same decision when the prompt redraws.
const ALLOW_KEY: u64 = 1;
const SESSION_KEY: u64 = 2;
const ALWAYS_KEY: u64 = 3;
const DENY_KEY: u64 = 4;
const CHOICES: [PromptChoice; 4] = [
    PromptChoice::Allow,
    PromptChoice::Session,
    PromptChoice::Always,
    PromptChoice::Deny,
];

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
    /// The explicit decision receiving keyboard/assistive-technology focus.
    /// A permission request starts on Deny when it receives keyboard focus so
    /// an accidental Enter cannot grant a capability.
    focused_choice: Option<PromptChoice>,
    has_keyboard_focus: bool,
    local_input: Option<LocalInputPrompt>,
}

/// Local input consent and controls preserve the editable field's keyboard owner.
#[derive(Clone, Debug)]
struct LocalInputPrompt {
    labels: [&'static str; 2],
    modal: bool,
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
            focused_choice: None,
            has_keyboard_focus: false,
            local_input: None,
        }
    }

    /// Two one-shot choices for a trusted local input surface. The translated
    /// label keys name the affirmative/negative controls. Session and persistent
    /// grants are absent from paint, hit testing and the accessibility door.
    /// Pointer activation preserves the original editable field's keyboard owner.
    /// Identical labels collapse to a single negative control for loading/error status.
    pub fn for_local_input(
        app_label: String,
        capability_label: String,
        labels: [&'static str; 2],
        modal: bool,
    ) -> Self {
        let mut prompt = Self::new(app_label, capability_label, false);
        prompt.local_input = Some(LocalInputPrompt { labels, modal });
        prompt
    }

    fn visible_choices(&self) -> &[PromptChoice] {
        if let Some(local) = &self.local_input {
            if local.labels[0] == local.labels[1] {
                return &[PromptChoice::Deny];
            }
            &[PromptChoice::Allow, PromptChoice::Deny]
        } else {
            &CHOICES
        }
    }

    fn button_label(&self, choice: PromptChoice) -> String {
        if let Some(local) = &self.local_input {
            match choice {
                PromptChoice::Allow => return crate::i18n!(local.labels[0]),
                PromptChoice::Deny => return crate::i18n!(local.labels[1]),
                _ => {}
            }
        }
        button_label(choice)
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
        if self.local_input.is_some() {
            self.allow_rect = Rect::new(always_x0, by, always_x1, by1);
            self.session_rect = Rect::ZERO;
            self.always_rect = Rect::ZERO;
            if !self.visible_choices().contains(&PromptChoice::Allow) {
                self.allow_rect = Rect::ZERO;
            }
        }
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

    fn choice_enabled(&self, choice: PromptChoice) -> bool {
        self.visible_choices().contains(&choice)
            && (!matches!(choice, PromptChoice::Always) || self.is_owner)
    }

    fn choose(&mut self, choice: PromptChoice) -> bool {
        if self.choice_enabled(choice) {
            self.pending_choice = Some(choice);
            true
        } else {
            false
        }
    }

    fn choice_from_index(&self, index: usize) -> Option<PromptChoice> {
        self.visible_choices().get(index).copied()
    }

    fn choice_from_key(key: u64) -> Option<PromptChoice> {
        match key {
            ALLOW_KEY => Some(PromptChoice::Allow),
            SESSION_KEY => Some(PromptChoice::Session),
            ALWAYS_KEY => Some(PromptChoice::Always),
            DENY_KEY => Some(PromptChoice::Deny),
            _ => None,
        }
    }

    fn key_for_choice(choice: PromptChoice) -> u64 {
        match choice {
            PromptChoice::Allow => ALLOW_KEY,
            PromptChoice::Session => SESSION_KEY,
            PromptChoice::Always => ALWAYS_KEY,
            PromptChoice::Deny => DENY_KEY,
        }
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "the fixed four-choice array bounds current/offset to 0..=4, so these index-cycle operations cannot overflow or underflow"
    )]
    fn move_focus(&mut self, forward: bool) {
        let current = self
            .focused_choice
            .and_then(|c| CHOICES.iter().position(|candidate| *candidate == c))
            // Deny is the conservative default when no choice is focused.
            .unwrap_or(CHOICES.len().saturating_sub(1));
        for offset in 1..=CHOICES.len() {
            let idx = if forward {
                (current + offset) % CHOICES.len()
            } else {
                (current + CHOICES.len() - (offset % CHOICES.len())) % CHOICES.len()
            };
            let Some(&choice) = CHOICES.get(idx) else {
                continue;
            };
            if self.choice_enabled(choice) {
                self.focused_choice = Some(choice);
                return;
            }
        }
    }
}

/// Shared localized label for pointer and semantic permission decisions.
pub fn button_label(choice: PromptChoice) -> String {
    crate::i18n!(match choice {
        PromptChoice::Allow => "permission-prompt-allow",
        PromptChoice::Session => "permission-prompt-session",
        PromptChoice::Always => "permission-prompt-always",
        PromptChoice::Deny => "permission-prompt-deny",
    })
}

#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the four flags are independent paint inputs (interaction, availability, emphasis and focus), not one state machine"
)]
struct ButtonVisualState {
    hover: bool,
    disabled: bool,
    primary: bool,
    focused: bool,
}

// Standard immediate-mode paint-helper signature for this vello renderer
// (scene + transform + text_renderer + theme, plus this draw's own
// content); every parameter is independently load-bearing.
fn draw_button(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    state: ButtonVisualState,
) {
    let radius = f64::from(theme.tokens.radii.control);
    let shape = RoundedRect::from_rect(rect, radius);

    let mut bg = if state.primary {
        theme.tokens.colors.intent.inverted
    } else if state.hover {
        theme
            .tokens
            .colors
            .lift_on(theme.tokens.colors.surfaces.raised, 0.5)
    } else {
        theme.tokens.colors.surfaces.raised
    };
    if state.disabled {
        bg = bg.with_alpha_factor(0.4);
    }
    scene.fill(Fill::NonZero, transform, bg, None, &shape);

    // Rest edge derived from the face itself (HIG §9.5), under the same
    // alpha the disabled face carries; a declared `border_standard` sat
    // within rounding distance of `raised` on the dark preset.
    let border = if state.disabled {
        crate::surface::edge_on(bg).with_alpha_factor(0.4)
    } else {
        crate::surface::edge_on(bg)
    };
    if state.focused {
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            theme.border_focus,
            None,
            &shape,
        );
    } else {
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            border,
            None,
            &crate::surface::edge_outline(rect, radius),
        );
    }

    let text_color = if state.disabled {
        theme.tokens.colors.text_primary.with_alpha_factor(0.4)
    } else if state.primary {
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

    let text_style = theme.text(TextRole::Caption);
    // Centre on the measured line in both axes, not on the font size.
    let (text_w, text_h) = text_renderer.measure_style(label, None, &text_style);
    let text_x = rect.center().x - (f64::from(text_w) / 2.0);
    let text_y = rect.y0 + (rect.height() - f64::from(text_h)) / 2.0;
    text_renderer.draw_style(
        scene,
        label,
        text_color,
        transform.pre_translate(Vec2::new(text_x, text_y)),
        None,
        &text_style,
    );
}

impl Widget for PermissionPrompt {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "PROMPT_HEIGHT is the compile-time constant 80.0, exact in f32"
                )]
                height: Dimension::length(PROMPT_HEIGHT as f32),
            },
            flex_grow: 0.0,
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
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
        // A permission prompt is a security surface: the app id and the
        // capability are read letter by letter.
        text_renderer.security_surface(|text_renderer| {
            // `surfaces.overlay` is a darker tint than `raised` so the prompt
            // reads as a layer above the strip's content, not another tile.
            let radius = f64::from(theme.tokens.radii.control);
            let shape = RoundedRect::from_rect(self.rect, radius);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.overlay,
                None,
                &shape,
            );
            // Panel role (HIG §9.1): the derived panel edge as a crisp 1 px
            // inset stroke, never a declared `border_standard`.
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                crate::chrome::panel_border(theme),
                None,
                &crate::surface::edge_outline(self.rect, radius),
            );

            // Placeholder pulled from the accent ramp until the portal layer
            // surfaces real icons via the app's desktop-file metadata.
            let icon_x = self.rect.x0 + PADDING_X;
            let icon_y = self.rect.y0 + (PROMPT_HEIGHT - ICON_SIZE) / 2.0;
            let icon_rect = Rect::new(icon_x, icon_y, icon_x + ICON_SIZE, icon_y + ICON_SIZE);
            let icon_shape =
                RoundedRect::from_rect(icon_rect, f64::from(theme.tokens.radii.detail));
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.intent.inverted,
                None,
                &icon_shape,
            );

            // Button geometry first: the label block ends `spacing.s3` before the
            // leftmost button and is elided to that width, so the two never
            // overlap on a narrow strip (HIG §7 "Nothing touches"). Also keeps a
            // draw without a preceding `apply_layout` hit-testing correctly.
            self.apply_layout_helper();
            let label_x = icon_x + ICON_SIZE + f64::from(theme.tokens.spacing.s2);
            let label_max_w = (if self.allow_rect == Rect::ZERO {
                self.deny_rect.x0
            } else {
                self.allow_rect.x0
            } - f64::from(theme.tokens.spacing.s3)
                - label_x)
                .max(0.0);
            let app_id_style = theme.text(TextRole::Caption);
            let cap_style = theme.text(TextRole::Body);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "bounded screen-space width; f32 precision is sufficient for text layout"
            )]
            let label_max_w = label_max_w as f32;
            let app_id =
                text_renderer.elide_to_width_style(&self.app_id, label_max_w, &app_id_style);
            let capability =
                text_renderer.elide_to_width_style(&self.capability_label, label_max_w, &cap_style);
            // Centre the two-line block on its measured line boxes, not on the
            // font sizes, so both lines sit where the band says they are.
            let (_, app_id_h) = text_renderer.measure_style(&app_id, None, &app_id_style);
            let (_, cap_h) = text_renderer.measure_style(&capability, None, &cap_style);
            let label_y = centered_text_top(
                self.rect.y0,
                PROMPT_HEIGHT,
                f64::from(app_id_h) + 4.0 + f64::from(cap_h),
            );
            text_renderer.draw_style(
                scene,
                &app_id,
                theme.tokens.colors.text_dim,
                transform.pre_translate(Vec2::new(label_x, label_y)),
                None,
                &app_id_style,
            );
            text_renderer.draw_style(
                scene,
                &capability,
                theme.tokens.colors.text_primary,
                transform.pre_translate(Vec2::new(label_x, label_y + f64::from(app_id_h) + 4.0)),
                None,
                &cap_style,
            );

            let buttons = [
                (self.allow_rect, PromptChoice::Allow, true), // primary (accent)
                (self.session_rect, PromptChoice::Session, false),
                (self.always_rect, PromptChoice::Always, false),
                (self.deny_rect, PromptChoice::Deny, false),
            ];
            for (i, (rect, choice, is_primary)) in buttons.iter().enumerate() {
                let is_disabled = matches!(choice, PromptChoice::Always) && !self.is_owner;
                let is_hover = self.hover_idx.is_some_and(|h| usize::from(h) == i) && !is_disabled;
                let is_focused = self.has_keyboard_focus && self.focused_choice == Some(*choice);
                if !self.visible_choices().contains(choice) {
                    continue;
                }
                draw_button(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    *rect,
                    &self.button_label(*choice),
                    ButtonVisualState {
                        hover: is_hover,
                        disabled: is_disabled,
                        primary: *is_primary,
                        focused: is_focused,
                    },
                );
            }
        });
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_cursor;
                if self.allow_rect.contains(p) {
                    self.focused_choice = Some(PromptChoice::Allow);
                    self.choose(PromptChoice::Allow);
                    if self.local_input.is_none() {
                        *ctx.focus_target = Some(ctx.node_id);
                    }
                    *ctx.handled = true;
                } else if self.session_rect.contains(p) {
                    self.focused_choice = Some(PromptChoice::Session);
                    self.choose(PromptChoice::Session);
                    if self.local_input.is_none() {
                        *ctx.focus_target = Some(ctx.node_id);
                    }
                    *ctx.handled = true;
                } else if self.always_rect.contains(p) {
                    if self.is_owner {
                        self.focused_choice = Some(PromptChoice::Always);
                        self.choose(PromptChoice::Always);
                        if self.local_input.is_none() {
                            *ctx.focus_target = Some(ctx.node_id);
                        }
                        *ctx.handled = true;
                    }
                    // Dropped for a non-owner: `pending_choice` stays untouched.
                } else if self.deny_rect.contains(p) {
                    self.focused_choice = Some(PromptChoice::Deny);
                    self.choose(PromptChoice::Deny);
                    if self.local_input.is_none() {
                        *ctx.focus_target = Some(ctx.node_id);
                    }
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        ..
                    },
                ..
            } if self.has_keyboard_focus => match logical_key {
                Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => {
                    self.move_focus(false);
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => {
                    self.move_focus(true);
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::Enter | NamedKey::Space) => {
                    if let Some(choice) = self.focused_choice {
                        *ctx.handled = self.choose(choice);
                    }
                }
                Key::Named(NamedKey::Escape) => {
                    self.focused_choice = Some(PromptChoice::Deny);
                    *ctx.handled = self.choose(PromptChoice::Deny);
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "PermissionPrompt"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let children = self
            .visible_choices()
            .iter()
            .map(|choice| {
                AccessNode::new(Role::Button)
                    .with_name(self.button_label(*choice))
                    .with_key(Self::key_for_choice(*choice))
                    .with_state(AccessState {
                        focusable: self.choice_enabled(*choice),
                        focused: self.has_keyboard_focus && self.focused_choice == Some(*choice),
                        disabled: !self.choice_enabled(*choice),
                        ..Default::default()
                    })
            })
            .collect();
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(crate::i18n!(
                    "permission-prompt-requested",
                    "capability" => self.capability_label.as_str(),
                    "app" => self.app_id.as_str()
                ))
                .with_state(AccessState {
                    modal: self.local_input.as_ref().is_none_or(|local| local.modal),
                    focusable: true,
                    focused: self.has_keyboard_focus,
                    ..Default::default()
                })
                .with_children(children),
        )
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.has_keyboard_focus = focused;
        if focused && self.focused_choice.is_none() {
            self.focused_choice = Some(PromptChoice::Deny);
        }
    }

    fn is_focused(&self) -> bool {
        self.has_keyboard_focus
    }

    /// The host maps Enter/Space on a focused retained control to `Activate`.
    /// Keep that route identical to the raw-key handler and the synthetic AT
    /// child, rather than falling back to a coordinate click in the middle of
    /// the dialog.
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if matches!(action, crate::a11y::Action::Activate)
            && let Some(choice) = self.focused_choice
        {
            self.choose(choice)
        } else {
            false
        }
    }

    fn traps_focus(&self) -> bool {
        self.local_input.as_ref().is_none_or(|local| local.modal)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else {
            return None;
        };
        match self.choice_from_index(*index)? {
            PromptChoice::Allow => Some(self.allow_rect),
            PromptChoice::Session => Some(self.session_rect),
            PromptChoice::Always => Some(self.always_rect),
            PromptChoice::Deny => Some(self.deny_rect),
        }
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let Some(choice) = path
            .first()
            .and_then(|index| self.choice_from_index(*index))
        else {
            return false;
        };
        if !self.choice_enabled(choice) {
            return false;
        }
        self.has_keyboard_focus = true;
        self.focused_choice = Some(choice);
        true
    }

    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        let [key] = key_path else {
            return false;
        };
        let Some(choice) = Self::choice_from_key(*key) else {
            return false;
        };
        match action {
            crate::a11y::Action::Focus => {
                self.has_keyboard_focus = true;
                self.focused_choice = Some(choice);
                self.choice_enabled(choice)
            }
            crate::a11y::Action::Activate => {
                self.focused_choice = Some(choice);
                self.choose(choice)
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    //! `apply_layout_helper` populates the hit-test rects from a synthetic
    //! `self.rect` so tests don't depend on a GPU/text-renderer.
    use super::*;
    use automerge::AutoCommit;
    use winit::event::{DeviceId, ElementState, MouseButton};

    fn synth_device() -> DeviceId {
        // SAFETY: `DeviceId` is opaque; tests construct one via
        // `mem::zeroed`. Same pattern as `quick-tilo_ui::widget::slider`'s
        // own test helpers.
        unsafe { std::mem::zeroed() }
    }

    #[test]
    fn local_input_prompt_has_only_one_shot_choices_and_preserves_field_focus() {
        let mut prompt = PermissionPrompt::for_local_input(
            "Dictation".into(),
            "Microphone".into(),
            ["dictation-record-once", "dictation-cancel"],
            true,
        );
        prompt.set_rect(Rect::new(0.0, 0.0, 600.0, 80.0));
        let node = prompt.accessibility().unwrap();
        assert_eq!(node.children.len(), 2);
        assert_eq!(
            prompt.synthetic_accessibility_rect(&[1]),
            Some(prompt.deny_rect())
        );
        assert_eq!(prompt.session_rect(), Rect::ZERO);
        assert_eq!(prompt.always_rect(), Rect::ZERO);
        let mut doc = AutoCommit::new();
        for key in [SESSION_KEY, ALWAYS_KEY] {
            assert!(!prompt.perform_action_at(
                &crate::a11y::Action::Activate,
                &[key],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert!(prompt.pending_choice.is_none());
        }
        let mut tree = TaffyTree::<()>::new();
        let field = tree.leaf_node(Style::default());
        let overlay = tree.leaf_node(Style::default());
        let mut focus = Some(field);
        let mut handled = false;
        for event in [cursor_at(prompt.allow_rect().center()), left_press()] {
            prompt.handle_event(
                &event,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: crate::widget::Edits::lend(&mut doc),
                    path: &automerge::ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: overlay,
                    taffy: &tree,
                },
            );
        }
        assert_eq!(prompt.pending_choice, Some(PromptChoice::Allow));
        assert_eq!(
            focus,
            Some(field),
            "microphone consent must not redirect IME insertion"
        );
        let mut single = PermissionPrompt::for_local_input(
            "Dictation".into(),
            "Loading".into(),
            ["dictation-cancel", "dictation-cancel"],
            false,
        );
        single.set_rect(prompt.get_rect());
        assert_eq!(single.accessibility().unwrap().children.len(), 1);
        assert!(!single.traps_focus());
        assert_eq!(single.allow_rect(), Rect::ZERO);
    }

    #[test]
    fn permission_dialog_semantics_focus_and_owner_only_always() {
        // Case `accessibility_reports_modal_dialog_with_request_name`.
        {
            use crate::a11y::Role;
            let p = PermissionPrompt::new("org.gnome.gedit".into(), "Camera access".into(), true);
            let node = Widget::accessibility(&p).expect("prompt is not presentational");
            assert_eq!(node.role, Role::Dialog);
            assert!(node.state.modal, "a permission prompt traps focus");
            assert!(node.state.focusable);
            assert_eq!(
                node.name.as_deref(),
                Some("Camera access requested by org.gnome.gedit")
            );
            assert_eq!(node.children.len(), 4, "every decision is exposed");
            assert_eq!(node.children[0].role, Role::Button);
            assert_eq!(node.children[0].name.as_deref(), Some("Allow"));
            assert_eq!(node.children[3].name.as_deref(), Some("Deny"));
        }

        // Case `non_owner_prompt_exposes_always_as_disabled_and_traps_focus`.
        {
            let p = prompt_at(false);
            let node = Widget::accessibility(&p).expect("prompt is semantic");
            assert!(Widget::traps_focus(&p));
            assert!(node.children[2].state.disabled);
            assert!(!node.children[2].state.focusable);
            assert_eq!(p.synthetic_accessibility_rect(&[0]), Some(p.allow_rect()));
            assert_eq!(p.synthetic_accessibility_rect(&[3]), Some(p.deny_rect()));
        }

        // Case `keyboard_focus_defaults_to_deny_and_at_can_activate_an_exact_choice`.
        {
            let mut p = prompt_at(false);
            Widget::set_focus(&mut p, true);
            let node = Widget::accessibility(&p).expect("prompt is semantic");
            assert!(node.state.focused);
            assert!(node.children[3].state.focused, "Deny is the safe default");

            let mut doc = AutoCommit::new();
            assert!(Widget::perform_action(
                &mut p,
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert_eq!(p.pending_choice, Some(PromptChoice::Deny));

            p.pending_choice = None;
            assert!(Widget::perform_action_at(
                &mut p,
                &crate::a11y::Action::Activate,
                &[SESSION_KEY],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert_eq!(p.pending_choice, Some(PromptChoice::Session));

            p.pending_choice = None;
            assert!(
                !Widget::perform_action_at(
                    &mut p,
                    &crate::a11y::Action::Activate,
                    &[ALWAYS_KEY],
                    &mut crate::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                ),
                "a disabled persistent grant cannot be activated through AT"
            );
            assert_eq!(p.pending_choice, None);
        }

        // Case `build_layout_emits_fixed_height_node`.
        {
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
            #[allow(
                clippy::cast_possible_truncation,
                reason = "PROMPT_HEIGHT is the compile-time constant 80.0, exact in f32"
            )]
            let expected = Dimension::length(PROMPT_HEIGHT as f32);
            assert_eq!(style.size.height, expected);
        }
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
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: crate::widget::Edits::lend(&mut doc),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &tree,
            },
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
    fn permission_buttons_set_exactly_their_pending_choice() {
        // Case `click_allow_sets_pending_choice_to_allow`.
        {
            let mut p = prompt_at(false);
            let center = p.allow_rect().center();
            dispatch(&mut p, &cursor_at(center));
            dispatch(&mut p, &left_press());
            assert_eq!(p.pending_choice, Some(PromptChoice::Allow));
        }

        // Case `click_session_sets_pending_choice_to_session`.
        {
            let mut p = prompt_at(false);
            let center = p.session_rect().center();
            dispatch(&mut p, &cursor_at(center));
            dispatch(&mut p, &left_press());
            assert_eq!(p.pending_choice, Some(PromptChoice::Session));
        }

        // Case `always_button_disabled_for_non_owner_drops_clicks`.
        {
            let mut p = prompt_at(false);
            let center = p.always_rect().center();
            dispatch(&mut p, &cursor_at(center));
            dispatch(&mut p, &left_press());
            assert_eq!(p.pending_choice, None);
        }

        // Case `always_button_active_for_owner_sets_choice_to_always`.
        {
            let mut p = prompt_at(true);
            let center = p.always_rect().center();
            dispatch(&mut p, &cursor_at(center));
            dispatch(&mut p, &left_press());
            assert_eq!(p.pending_choice, Some(PromptChoice::Always));
        }

        // Case `deny_click_sets_pending_choice_to_deny`.
        {
            let mut p = prompt_at(false);
            let center = p.deny_rect().center();
            dispatch(&mut p, &cursor_at(center));
            dispatch(&mut p, &left_press());
            assert_eq!(p.pending_choice, Some(PromptChoice::Deny));
        }

        // Case `click_outside_button_rects_does_not_set_choice`.
        {
            // Click on the icon (left edge of the bar), far from any
            // button rect.
            let mut p = prompt_at(true);
            dispatch(&mut p, &cursor_at(Point::new(20.0, 40.0)));
            dispatch(&mut p, &left_press());
            assert_eq!(p.pending_choice, None);
        }
    }
}

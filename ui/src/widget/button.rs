use crate::i18n;
use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ON_FILL_MIN_CONTRAST;
use crate::widget::ActionEmitter;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use winit::event::{ElementState, MouseButton, WindowEvent};

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent interaction/lifecycle flag (hover, press, focus, enabled, cache-dirty), not a combinatorial state machine"
)]
pub struct Button {
    id: String,
    label: String,
    /// Accessible name for an icon-only button (empty visible label);
    /// left unset when the label itself serves as the accessible name.
    accessible_name: Option<String>,
    /// Fluent key or plain string, revealed by the host's central
    /// [`TooltipHost`](crate::widget::TooltipHost) — and, for an
    /// icon-only button, used as the accessible name when none was set
    /// explicitly.
    tooltip: Option<String>,
    rect: Rect,
    is_hovered: bool,
    is_pressed: bool,
    is_focused: bool,
    /// Drawn instead of (icon-only) or before (icon + label) the text,
    /// so toolbars render crisp lucide icons without depending on a
    /// Nerd Font being installed.
    icon: Option<crate::icons::Icon>,
    /// Defaults to 120×40.
    size: Option<(f32, f32)>,
    /// Stretch to the containing column while retaining an explicit height.
    /// This is distinct from `fit_content`, which derives a compact width
    /// from the label.
    fill_width: bool,
    /// Drives the fill / border / label colours via [`face_colors`], the
    /// same source of truth the immediate-mode [`Button::paint`] face uses.
    style: ButtonStyle,
    on_click_path: Option<String>, // Path in CRDT to mutate
    /// Optional string to write instead of the default counter/boolean pulse.
    /// This lets command-bus users share the standard button implementation.
    click_value: Option<String>,
    /// Size the width from the visible label rather than using the 120px default.
    fit_content: bool,
    /// Fires on release-while-hovered *in addition to* any
    /// `on_click_path` CRDT mutation, for app code that isn't modelling
    /// its control flow in the CRDT (e.g. the file explorer's command sink).
    on_click: Option<Box<dyn FnMut()>>,
    /// An advertised action intent. While present it is the exclusive
    /// activation path; raw document writes and callbacks are suppressed.
    action: Option<ActionEmitter<()>>,
    /// Resource and handler synthesized by the low-ceremony `on_effect` /
    /// `on_view` APIs. The closure is still ordinary Rust, but it is reachable
    /// only through the same published action door as agents and search.
    native_resource: Option<qos_actions::Resource>,
    native_handler: Option<Box<dyn FnMut()>>,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// When `false`: skipped by focus, rejects input, renders dimmed.
    enabled: bool,
    /// Holds the last emitted body/border/label primitives at *logical*
    /// coordinates; the App's transform is reapplied on `Scene::append`.
    cached: vello::Scene,
    dirty: bool,
    layout_dirty: bool,
    i18n_revision: u64,
    interaction: crate::interaction::InteractionEnvironment,
    /// Compared *by size* to the current rect — pure translation is
    /// absorbed by the per-frame append transform without invalidating
    /// the cache.
    cache_rect: Rect,
    /// Snapshot the cache was emitted from; any change invalidates it.
    last_state: (bool, bool, bool), // (hovered, pressed, focused)
}

impl Button {
    fn effectively_enabled(&self) -> bool {
        self.enabled && self.action.as_ref().is_none_or(ActionEmitter::is_available)
    }
    pub fn new(id: &str, label: &str, on_click_path: Option<&str>) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            accessible_name: None,
            tooltip: None,
            rect: Rect::ZERO,
            is_hovered: false,
            is_pressed: false,
            is_focused: false,
            icon: None,
            size: None,
            fill_width: false,
            style: ButtonStyle::Neutral,
            on_click_path: on_click_path.map(std::string::ToString::to_string),
            click_value: None,
            fit_content: false,
            on_click: None,
            action: None,
            native_resource: None,
            native_handler: None,
            presence_path: Vec::new(),
            permission_scope: None,
            enabled: true,
            cached: vello::Scene::new(),
            dirty: true,
            layout_dirty: false,
            i18n_revision: crate::i18n::I18nManager::revision(),
            interaction: crate::interaction::InteractionEnvironment::default(),
            cache_rect: Rect::ZERO,
            last_state: (false, false, false),
        }
    }

    /// An empty path leaves presence opt-out (the default).
    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// When the scope is denied the widget renders dimmed and rejects input.
    #[must_use]
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }

    /// Fires once per completed click (left-button release while the
    /// cursor is still over the button), in addition to any
    /// `on_click_path` CRDT mutation.
    #[must_use]
    pub fn on_click<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }

    /// Route pointer, keyboard and accessibility activation through an
    /// exported resource action. The host resolves policy against the fresh
    /// resource tree before dispatch.
    #[must_use]
    pub fn on_action(mut self, binding: qos_actions::ActionBinding<()>) -> Self {
        self.action = Some(ActionEmitter::new(binding));
        self.on_click_path = None;
        self.click_value = None;
        self.on_click = None;
        self
    }

    /// Publish a zero-argument effect and route every activation through it.
    ///
    /// This is the quick-app path: it keeps closure ergonomics without making
    /// an invisible callback-only control. For replayable document mutations,
    /// prefer a typed document [`Action`](qos_actions::Action).
    #[must_use]
    pub fn on_effect<F: FnMut() + 'static>(self, handler: F) -> Self {
        self.on_native(qos_actions::OperationKind::Effect, handler)
    }

    /// Publish a zero-argument transient presentation action. View actions are
    /// discoverable and agent-driveable but intentionally absent from document
    /// history and collaboration logs.
    #[must_use]
    pub fn on_view<F: FnMut() + 'static>(self, handler: F) -> Self {
        self.on_native(qos_actions::OperationKind::View, handler)
    }

    #[expect(
        clippy::expect_used,
        reason = "String serialization and lookup of the just-created action are infallible"
    )]
    fn on_native<F: FnMut() + 'static>(
        mut self,
        kind: qos_actions::OperationKind,
        handler: F,
    ) -> Self {
        const TYPE_ID: &str = "qos.ui.control.invoke";
        let resource_id = format!("qos.control:{}", self.id);
        let mut spec = qos_actions::ActionSpec::operation(
            "invoke",
            self.label.clone(),
            kind,
            TYPE_ID,
            &self.id,
        )
        .expect("a control id is always JSON serializable");
        spec.contract.discovery = qos_actions::DiscoveryScope::Launchable;
        let resource = qos_actions::Resource::new(resource_id, "qos.control", self.label.clone())
            .with_action(spec);
        let binding = qos_actions::ActionBinding::unit_for(&resource, "invoke")
            .expect("the synthesized invoke action is nullary");
        self = self.on_action(binding);
        self.native_resource = Some(resource);
        self.native_handler = Some(Box::new(handler));
        self
    }

    /// With an empty label the icon is centred (icon-only button); with
    /// a label it sits to its left.
    #[must_use]
    pub fn with_icon(mut self, icon: crate::icons::Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The text isn't painted — it's what a screen reader announces in
    /// place of the empty visible label. Use for every icon-only toolbar
    /// control so the glyph isn't silent. A button that shows a label
    /// doesn't need this; the label is its accessible name.
    #[must_use]
    pub fn with_accessible_name(mut self, name: &str) -> Self {
        self.accessible_name = Some(name.to_string());
        self
    }

    /// On an icon-only button, absent an explicit
    /// [`with_accessible_name`](Self::with_accessible_name), this also
    /// becomes the accessible name, so the same short label serves both
    /// mouse and screen-reader users. Rendered centrally by the host's
    /// [`TooltipHost`](crate::widget::TooltipHost) — no per-app wiring.
    #[must_use]
    pub fn tooltip(mut self, text: impl Into<String>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn set_tooltip(&mut self, text: Option<String>) {
        self.tooltip = text;
    }

    fn observe_i18n_revision(&mut self) {
        let revision = crate::i18n::I18nManager::revision();
        if revision != self.i18n_revision {
            self.i18n_revision = revision;
            self.dirty = true;
            self.layout_dirty |= self.fit_content;
        }
    }

    /// Overrides the default 120×40 intrinsic size.
    #[must_use]
    pub fn with_size(mut self, width: f32, height: f32) -> Self {
        self.size = Some((width, height));
        self.fill_width = false;
        self
    }

    /// Fill the available parent width at the supplied height.
    #[must_use]
    pub fn fill_width(mut self, height: f32) -> Self {
        self.size = Some((0.0, height));
        self.fill_width = true;
        self.fit_content = false;
        self
    }

    /// Use a compact label-derived width and the supplied height.
    ///
    /// Layout cannot access the text renderer, so this deliberately mirrors the
    /// conservative sizing used by app-local buttons: 8px per character plus
    /// 30px of horizontal padding, with a 64px minimum.
    #[must_use]
    pub fn fit_content(mut self, height: f32) -> Self {
        self.fit_content = true;
        self.fill_width = false;
        self.size = Some((0.0, height));
        self
    }

    /// Write `value` to the click path on activation instead of pulsing it as a
    /// boolean/counter. A button without an `on_click_path` ignores this value.
    #[must_use]
    pub fn with_click_value(mut self, value: impl Into<String>) -> Self {
        self.click_value = Some(value.into());
        self
    }

    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.dirty = true;
        }
    }

    /// Colours resolve through [`face_colors`], so the retained widget
    /// and the immediate-mode [`Button::paint`] face stay pixel-identical
    /// for the same style + interaction state.
    #[must_use]
    pub fn with_style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }

    #[must_use]
    pub fn primary(mut self) -> Self {
        self.style = ButtonStyle::Primary;
        self
    }

    #[must_use]
    pub fn danger(mut self) -> Self {
        self.style = ButtonStyle::Danger;
        self
    }

    #[must_use]
    pub fn ghost(mut self) -> Self {
        self.style = ButtonStyle::Ghost;
        self
    }

    pub fn set_style(&mut self, style: ButtonStyle) {
        if self.style != style {
            self.style = style;
            self.dirty = true;
        }
    }

    /// The counterpart to [`TextInput::apply_layout_external`] for an app
    /// that hand-positions a shared control over its own painted chrome;
    /// the button still owns its focus / a11y / activation contract, only
    /// its geometry is caller-driven.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        // Bitwise: this asks whether the stored size changed at all, not
        // whether it changed by more than some tolerance.
        let size_changed = rect.width().to_bits() != self.rect.width().to_bits()
            || rect.height().to_bits() != self.rect.height().to_bits();
        self.rect = rect;
        if size_changed {
            self.dirty = true;
        }
    }
}

/// `Ghost` is borderless and transparent at rest, tinting to `bg_hover` /
/// `bg_active` only on interaction with a dim label/icon — the look a
/// close-`X`, toolbar icon, or overlay affordance wants, replacing the
/// per-app "draw a bare icon and hit-test the rect" pattern.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ButtonStyle {
    #[default]
    Neutral,
    Primary,
    Danger,
    Ghost,
}

/// The retained [`Button`] tracks these itself; an immediate-mode caller
/// (a canvas app that owns its own hit-testing) passes what it knows —
/// usually just `hovered`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a POD bundle of independent interaction flags a button can be in any combination of; not a state machine"
)]
pub struct ButtonInteract {
    pub hovered: bool,
    pub pressed: bool,
    pub focused: bool,
    pub disabled: bool,
}

/// The single source of truth for both the retained widget's look and
/// the immediate-mode [`Button::paint`]. `border` is `None` for the
/// accent-filled styles, whose fill is self-defining; the painter adds a
/// focus ring on top when focused.
pub fn face_colors(
    theme: &Theme,
    style: ButtonStyle,
    interact: ButtonInteract,
) -> (
    vello::peniko::Color,
    Option<vello::peniko::Color>,
    vello::peniko::Color,
) {
    use vello::peniko::Color;
    match style {
        ButtonStyle::Neutral => {
            let rest = theme.tokens.colors.surfaces.raised;
            let bg = if interact.pressed {
                theme.bg_active
            } else if interact.hovered {
                theme.bg_hover
            } else {
                rest
            };
            let fg = theme.tokens.colors.ink_on_any(
                &[rest, theme.bg_hover, theme.bg_active],
                theme.tokens.colors.text_primary,
                ON_FILL_MIN_CONTRAST,
            );
            (bg, Some(theme.tokens.colors.border_standard), fg)
        }
        ButtonStyle::Primary => {
            let rest = theme.accent_primary;
            let active = theme.accent_hover;
            let bg = if interact.hovered || interact.pressed {
                active
            } else {
                rest
            };
            // White is the label this style wants and, on a dark enough
            // accent, what comes back; on a bright one it walks out far
            // enough to be read — resolved once over both fills so it
            // doesn't change under the pointer.
            let fg =
                theme
                    .tokens
                    .colors
                    .ink_on_any(&[rest, active], Color::WHITE, ON_FILL_MIN_CONTRAST);
            (bg, None, fg)
        }
        ButtonStyle::Danger => {
            use crate::color_compat::ColorCompat;
            let d = theme.tokens.colors.semantic.danger;
            // The label is resolved first: the feedback has to respect it.
            let fg = theme
                .tokens
                .colors
                .ink_on(d, Color::WHITE, ON_FILL_MIN_CONTRAST);
            // Hover/press shift the fill ~15%, and the direction is not a
            // constant: lifting a mid-tone danger toward white took a
            // white label from 4.97:1 at rest to 3.96:1 the moment the
            // pointer arrived. A control can't buy feedback with its own
            // label's legibility, so the shift always moves the fill
            // *away* from the ink that sits on it.
            let toward: u8 = if crate::a11y::contrast_ratio(fg, Color::WHITE)
                > crate::a11y::contrast_ratio(fg, Color::from_rgb8(0, 0, 0))
            {
                255
            } else {
                0
            };
            let shift = |a: u8| {
                let blended = (f32::from(a) * 0.85 + f32::from(toward) * 0.15)
                    .round()
                    .clamp(0.0, 255.0);
                // `blended` is a convex combination of two `u8`-derived
                // values (weights 0.85 + 0.15), clamped immediately above,
                // so it can't wrap or flip sign on the way into `u8`.
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "blended is clamped to [0.0, 255.0] on the line above"
                )]
                {
                    blended as u8
                }
            };
            let active = Color::from_rgba8(shift(d.r()), shift(d.g()), shift(d.b()), d.a());
            let bg = if interact.hovered || interact.pressed {
                active
            } else {
                d
            };
            (bg, None, fg)
        }
        ButtonStyle::Ghost => {
            let bg = if interact.pressed {
                theme.bg_active
            } else if interact.hovered {
                theme.bg_hover
            } else {
                Color::TRANSPARENT
            };
            // At rest the ghost paints nothing, so the base surface is
            // the closest the widget can know as "the ground". The label
            // is most at risk on `bg_hover`, close enough to the dim
            // tier that it can all but vanish.
            let fg = theme.tokens.colors.ink_on_any(
                &[
                    theme.tokens.colors.surfaces.base,
                    theme.bg_hover,
                    theme.bg_active,
                ],
                theme.tokens.colors.text_dim,
                ON_FILL_MIN_CONTRAST,
            );
            (bg, None, fg)
        }
    }
}

/// Built fluently: `ButtonFace::new(rect, "Save").primary().hovered(hit)`.
/// Defaults to a neutral, medium-weight, 14px label at an 8px corner radius.
pub struct ButtonFace<'a> {
    rect: Rect,
    label: &'a str,
    icon: Option<crate::icons::Icon>,
    style: ButtonStyle,
    interact: ButtonInteract,
    radius: f64,
    weight: crate::text_render::TextWeight,
    text_size: f32,
    accessible_name: Option<&'a str>,
}

impl<'a> ButtonFace<'a> {
    pub fn new(rect: Rect, label: &'a str) -> Self {
        Self {
            rect,
            label,
            icon: None,
            style: ButtonStyle::Neutral,
            interact: ButtonInteract::default(),
            radius: 8.0,
            weight: crate::text_render::TextWeight::Medium,
            text_size: 14.0,
            accessible_name: None,
        }
    }
    #[must_use]
    pub fn style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn primary(mut self) -> Self {
        self.style = ButtonStyle::Primary;
        self
    }
    #[must_use]
    pub fn danger(mut self) -> Self {
        self.style = ButtonStyle::Danger;
        self
    }
    #[must_use]
    pub fn ghost(mut self) -> Self {
        self.style = ButtonStyle::Ghost;
        self
    }
    #[must_use]
    pub fn icon(mut self, icon: crate::icons::Icon) -> Self {
        self.icon = Some(icon);
        self
    }
    #[must_use]
    pub fn radius(mut self, radius: f64) -> Self {
        self.radius = radius;
        self
    }
    #[must_use]
    pub fn weight(mut self, weight: crate::text_render::TextWeight) -> Self {
        self.weight = weight;
        self
    }
    #[must_use]
    pub fn text_size(mut self, size: f32) -> Self {
        self.text_size = size;
        self
    }
    #[must_use]
    pub fn hovered(mut self, v: bool) -> Self {
        self.interact.hovered = v;
        self
    }
    #[must_use]
    pub fn pressed(mut self, v: bool) -> Self {
        self.interact.pressed = v;
        self
    }
    #[must_use]
    pub fn focused(mut self, v: bool) -> Self {
        self.interact.focused = v;
        self
    }
    #[must_use]
    pub fn disabled(mut self, v: bool) -> Self {
        self.interact.disabled = v;
        self
    }
    /// For callers that expose their own accessibility node for the region.
    #[must_use]
    pub fn accessible_name(mut self, name: &'a str) -> Self {
        self.accessible_name = Some(name);
        self
    }
    /// The visible label, or the explicit `accessible_name` when it's empty.
    pub fn spoken_name(&self) -> &str {
        if self.label.is_empty() {
            self.accessible_name.unwrap_or("")
        } else {
            self.label
        }
    }
    /// Exposed for tests and callers that want the resolved palette
    /// without painting.
    pub fn colors(
        &self,
        theme: &Theme,
    ) -> (
        vello::peniko::Color,
        Option<vello::peniko::Color>,
        vello::peniko::Color,
    ) {
        face_colors(theme, self.style, self.interact)
    }
}

/// Widens a label's character count for the `fit_content` width estimate.
/// A button label is a handful of characters, so this is far below `f32`'s
/// exact-integer range.
#[allow(
    clippy::cast_precision_loss,
    reason = "a button label's character count is always far below f32's exact-integer range"
)]
fn label_px(chars: usize) -> f32 {
    chars as f32
}

/// Paints the icon and/or label centred in `rect`: icon-only centres the
/// glyph, otherwise the icon (if any) sits left of the label with a fixed
/// gap, the cluster as a whole centred. Shared by the immediate-mode
/// [`Button::paint`] and the retained widget's cached [`Widget::draw`] so
/// the two stay pixel-identical.
#[allow(
    clippy::too_many_arguments,
    reason = "a painting primitive shared by two call sites; splitting further would only re-bundle these into a throwaway struct"
)]
fn paint_icon_and_label(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    rect: Rect,
    icon: Option<crate::icons::Icon>,
    label: &str,
    text_size: f32,
    weight: crate::text_render::TextWeight,
    fg: vello::peniko::Color,
) {
    let icon_px = (rect.height() - 14.0).clamp(12.0, 22.0);
    match (icon, label.is_empty()) {
        (Some(icon), true) => {
            // Icon-only — centre the glyph.
            let ix = rect.center().x - icon_px / 2.0;
            let iy = rect.center().y - icon_px / 2.0;
            crate::icons::draw_icon(
                scene,
                transform,
                Rect::new(ix, iy, ix + icon_px, iy + icon_px),
                icon,
                fg,
                crate::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
        (icon, _) => {
            let (text_width, _) = text_renderer.measure_weighted(label, text_size, None, weight);
            let gap = if icon.is_some() { 6.0 } else { 0.0 };
            let icon_w = if icon.is_some() { icon_px } else { 0.0 };
            let cluster_w = icon_w + gap + f64::from(text_width);
            let start_x = rect.center().x - cluster_w / 2.0;
            if let Some(icon) = icon {
                let iy = rect.center().y - icon_px / 2.0;
                crate::icons::draw_icon(
                    scene,
                    transform,
                    Rect::new(start_x, iy, start_x + icon_px, iy + icon_px),
                    icon,
                    fg,
                    crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            let text_x = start_x + icon_w + gap;
            let text_y = rect.y0 + (rect.height() - f64::from(text_size)) / 2.0;
            text_renderer.draw_text_weighted(
                scene,
                label,
                text_size,
                fg,
                transform.pre_translate(Vec2::new(text_x, text_y)),
                None,
                weight,
            );
        }
    }
}

impl Button {
    /// Matches the retained [`Button`]'s look. For canvas/immediate-mode
    /// apps that own their own rect hit-testing: call this to paint, keep
    /// your existing hit-test to fire the action. Shares `face_colors`
    /// with the retained draw rather than forking a second painter.
    pub fn paint(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        face: &ButtonFace<'_>,
    ) {
        let (bg, border, fg) = face_colors(theme, face.style, face.interact);
        let shape = RoundedRect::from_rect(face.rect, face.radius);

        // One layer so fill + border + label dim together, matching the
        // retained widget's DISABLED_ALPHA look.
        let dim = face.interact.disabled;
        if dim {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &shape,
            );
        }

        scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &shape);
        if face.interact.focused {
            scene.stroke(
                &vello::kurbo::Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &shape,
            );
        } else if let Some(bc) = border {
            scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, bc, None, &shape);
        }

        let label = i18n!(face.label);
        paint_icon_and_label(
            scene,
            transform,
            text_renderer,
            face.rect,
            face.icon,
            &label,
            face.text_size,
            face.weight,
            fg,
        );

        if dim {
            scene.pop_layer();
        }
    }

    /// Appends the cached face scene, wrapped in the `DISABLED_ALPHA` dim
    /// layer when the button is disabled. `inner` lets the fast
    /// (no re-encode) path in [`Widget::draw`] pass a translated
    /// transform while the freshly-encoded path passes `out_transform`
    /// unchanged.
    fn append_cached(
        &self,
        out_scene: &mut Scene,
        out_transform: Affine,
        inner: Affine,
        theme: &Theme,
    ) {
        if self.effectively_enabled() {
            out_scene.append(&self.cached, Some(inner));
        } else {
            let clip = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.md));
            out_scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                out_transform,
                &clip,
            );
            out_scene.append(&self.cached, Some(inner));
            out_scene.pop_layer();
        }
    }
}

impl Widget for Button {
    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        if self.interaction != environment {
            self.interaction = environment;
            self.is_hovered &= environment.supports_hover();
            self.layout_dirty = true;
            self.dirty = true;
        }
    }

    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        self.observe_i18n_revision();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.observe_i18n_revision();
        let (mut w, mut h) = self.size.unwrap_or((120.0, 40.0));
        let minimum_target = self.interaction.minimum_target();
        w = w.max(minimum_target);
        h = h.max(minimum_target);
        if self.fit_content {
            let label = i18n!(&self.label);
            w = (label_px(label.chars().count()) * 8.0 + 30.0).max(64.0);
        }
        let node = taffy.leaf_node(Style {
            flex_shrink: 0.0,
            size: Size {
                width: if self.fill_width {
                    Dimension::percent(1.0)
                } else {
                    Dimension::length(w)
                },
                height: Dimension::length(h),
            },
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
        // Bitwise: this asks whether the cached size is stale at all, not
        // whether it drifted by more than some tolerance.
        let size_changed = self.rect.width().to_bits() != self.cache_rect.width().to_bits()
            || self.rect.height().to_bits() != self.cache_rect.height().to_bits();
        if size_changed {
            self.dirty = true;
        }
    }

    fn draw(
        &mut self,
        out_scene: &mut Scene,
        out_transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        self.observe_i18n_revision();
        let current_state = (self.is_hovered, self.is_pressed, self.is_focused);
        if current_state != self.last_state {
            self.dirty = true;
            self.last_state = current_state;
        }
        let needs_emit = self.dirty || self.cache_rect == Rect::ZERO;
        if !needs_emit {
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            let inner = out_transform.pre_translate(Vec2::new(dx, dy));
            self.append_cached(out_scene, out_transform, inner, theme);
            return;
        }
        // cache_rect tracks *where* the cache was emitted so subsequent
        // frames can translate without re-encoding.
        self.cached.reset();
        let scene = &mut self.cached;
        let transform = Affine::IDENTITY;
        // `disabled` is left false — the dimmed look is applied by the
        // `DISABLED_ALPHA` layer around the cache append, not the colours.
        let (bg_color, border_opt, fg_color) = face_colors(
            theme,
            self.style,
            ButtonInteract {
                hovered: self.is_hovered,
                pressed: self.is_pressed,
                focused: self.is_focused,
                disabled: false,
            },
        );

        let shape = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.md));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            bg_color,
            None,
            &shape,
        );

        // Focus ring wins over the style border; an accent-filled style
        // has no rest border, so it only strokes when focused.
        if self.is_focused {
            scene.stroke(
                &vello::kurbo::Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &shape,
            );
        } else if let Some(border_color) = border_opt {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                border_color,
                None,
                &shape,
            );
        }

        // Regular weight and a fixed 14px size, matching what `measure`/
        // `draw_text` use internally — spelled out here since this now
        // goes through the `_weighted` entry points shared with
        // `ButtonFace`'s (independently weighted/sized) immediate-mode path.
        let label = i18n!(&self.label);
        paint_icon_and_label(
            scene,
            transform,
            text_renderer,
            self.rect,
            self.icon,
            &label,
            14.0,
            crate::text_render::TextWeight::Regular,
            fg_color,
        );
        self.cache_rect = self.rect;
        self.append_cached(out_scene, out_transform, out_transform, theme);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.i18n_revision != crate::i18n::I18nManager::revision()
            || (self.is_hovered, self.is_pressed, self.is_focused) != self.last_state
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.layout_dirty = false;
    }

    /// The `Rect::ZERO` sentinel keeps the first frame's
    /// `compute_layout` running so `apply_layout` can populate the rect;
    /// after that it stays clean (pure translation handled via the
    /// cache's append transform, no taffy involvement).
    fn is_layout_dirty(&self) -> bool {
        self.cache_rect == Rect::ZERO
            || self.layout_dirty
            || (self.fit_content && self.i18n_revision != crate::i18n::I18nManager::revision())
    }

    fn needs_idle_sync(&self) -> bool {
        self.i18n_revision != crate::i18n::I18nManager::revision()
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Reject all input without setting `handled`.
        if !self.effectively_enabled() {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hover = self.rect.contains(logical_pos);
                // A hover change is a repaint (`needs_redraw` compares the
                // state), not consumed input: a parent that stops at the
                // first handled child must still deliver this move to the
                // widget the pointer actually went to. Marking the move
                // that *left* a button as handled starved the file list of
                // the press position that followed it.
                self.is_hovered = hover;
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                if self.is_hovered {
                    let pressed = *element_state == ElementState::Pressed;
                    if pressed != self.is_pressed {
                        self.is_pressed = pressed;
                        *handled = true;

                        if self.is_pressed {
                            *focus_target = Some(node_id);
                        }

                        if !self.is_pressed {
                            self.activate(state, root);
                        }
                    }
                } else if self.is_pressed {
                    self.is_pressed = false;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Button"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.id.as_str())
    }

    /// Returns the raw string (a fluent key or plain text); the host
    /// resolves i18n and renders.
    fn tooltip(&self) -> Option<&str> {
        self.tooltip.as_deref()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.rect.contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
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
    fn is_enabled(&self) -> bool {
        self.effectively_enabled()
    }

    fn resources(&self, _doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        self.native_resource.iter().cloned().collect()
    }

    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        self.native_resource
            .as_ref()
            .map(|_| std::iter::once("qos.ui.control.invoke".to_owned()).collect())
            .unwrap_or_default()
    }

    fn invoke_resource_operation_v2(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<qos_actions::ActionOutput>, String> {
        if kind == qos_actions::OperationKind::Document
            || type_id != "qos.ui.control.invoke"
            || self.native_handler.is_none()
        {
            return Ok(None);
        }
        let control: String = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        if control != self.id {
            return Ok(None);
        }
        if let Some(handler) = &mut self.native_handler {
            handler();
        }
        Ok(Some(qos_actions::ActionOutput::default()))
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<qos_actions::ActionAddress>,
    ) {
        if let Some(action) = &mut self.action {
            let before = action.is_available();
            action.refresh_availability(allowed);
            if before != action.is_available() {
                self.dirty = true;
            }
        }
    }

    fn take_control_invocation(&mut self) -> Option<qos_actions::ControlInvocation> {
        self.action.as_mut().and_then(ActionEmitter::take)
    }

    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        use crate::widget::{ControlBindingKind, ControlBindingMetadata};
        if let Some(action) = &self.action {
            return vec![action.metadata(self.id.clone())];
        }
        let mut bindings = Vec::new();
        if let Some(path) = &self.on_click_path {
            bindings.push(ControlBindingMetadata {
                control_id: self.id.clone(),
                kind: ControlBindingKind::CollaborativeDocument(path.clone()),
            });
        }
        if self.on_click.is_some() {
            bindings.push(ControlBindingMetadata {
                control_id: self.id.clone(),
                kind: ControlBindingKind::LegacyCallback,
            });
        }
        bindings
    }

    /// Enter, Space, or an AccessKit `Activate` performs the same CRDT
    /// mutation + imperative callback as a mouse click.
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> bool {
        use crate::a11y::Action;
        if !self.effectively_enabled() {
            return false;
        }
        match action {
            Action::Activate => {
                self.activate(state, root);
                true
            }
            _ => false,
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let label = i18n!(&self.label);
        let name = if label.is_empty() {
            // Icon-only: prefer the explicit accessible name, else fall
            // back to the tooltip text so the glyph is never silent when
            // a tooltip is set.
            self.accessible_name
                .as_ref()
                .map(|n| i18n!(n))
                .or_else(|| self.tooltip.as_ref().map(|t| i18n!(t)))
                .filter(|n| !n.is_empty())
        } else {
            Some(label)
        };
        let mut node = AccessNode::new(Role::Button)
            .with_semantic_key(&self.id)
            .with_optional_name(name)
            .with_state(AccessState {
                focusable: self.is_focusable(),
                focused: self.is_focused(),
                disabled: !self.is_enabled(),
                ..Default::default()
            });
        if self.action.is_none() && self.on_click_path.is_some() {
            node = node.with_action_target(&self.id);
        }
        Some(node)
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget {
                path: self.presence_path.clone(),
            })
        }
    }

    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) {
            return;
        }
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(scope) = self.permission_scope()
            && !permissions.check(&scope)
        {
            let overlay = crate::tokens::colors::PERMISSION_DENIED_SCRIM;
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                overlay,
                None,
                &self.rect,
            );
        }

        if let Some(key) = self.presence_key()
            && let Some(peers) = presence.here.get(&key)
            && let Some(user) = peers.first()
        {
            presence.draw_peer_focus(scene, theme, user, self.rect);
        }
    }
}

impl Button {
    fn activate(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(action) = &mut self.action {
            if let Err(error) = action.emit(()) {
                tracing::warn!(%error, button = %self.id, "action-bound control refused its arguments");
            }
            return;
        }
        if let Some(path) = &self.on_click_path {
            if let Some(value) = &self.click_value {
                let _ = state.put(root, path.as_str(), value.as_str());
            } else {
                match state.get(root, path.as_str()) {
                    Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                        automerge::ScalarValue::Counter(_) => {
                            let _ = state.increment(root, path.as_str(), 1);
                        }
                        // Boolean and every other scalar type both just
                        // get set true.
                        _ => {
                            let _ = state.put(root, path.as_str(), true);
                        }
                    },
                    _ => {
                        let _ = state.put(root, path.as_str(), true);
                    }
                }
            }
        }
        if let Some(cb) = &mut self.on_click {
            cb();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Widget;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn button_test_id_is_stable_constructor_id() {
        let b = Button::new("id", "Save", None);
        assert_eq!(Widget::test_id(&b), Some("id"));
        assert_eq!(
            Widget::accessibility(&b).unwrap().name.as_deref(),
            Some("Save")
        );
    }

    #[test]
    fn action_mode_suppresses_raw_mutation_and_callback() {
        let resource = qos_actions::Resource::new("doc:1", "doc", "Document").with_action(
            qos_actions::ActionSpec::operation(
                "save",
                "Save",
                qos_actions::OperationKind::Effect,
                "test.save",
                &(),
            )
            .unwrap(),
        );
        let binding = qos_actions::ActionBinding::unit_for(&resource, "save").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        let mut button = Button::new("save", "Save", Some("legacy"))
            .on_action(binding)
            // Builder order cannot accidentally restore an imperative path.
            .on_click(move || {
                callback_calls.fetch_add(1, Ordering::SeqCst);
            });
        let mut doc = AutoCommit::new();
        button.activate(&mut doc, &automerge::ROOT);
        assert!(doc.get(&automerge::ROOT, "legacy").unwrap().is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let intent = button.take_control_invocation().expect("one intent");
        assert_eq!(
            intent.address,
            qos_actions::ActionAddress::new("doc:1", "save")
        );
        assert!(intent.arguments.is_empty());
        assert_eq!(button.control_action_bindings().len(), 1);
    }

    #[test]
    fn action_availability_revokes_and_restores_without_overriding_authored_state() {
        let resource = qos_actions::Resource::new("doc:1", "doc", "Document").with_action(
            qos_actions::ActionSpec::operation(
                "save",
                "Save",
                qos_actions::OperationKind::Effect,
                "test.save",
                &(),
            )
            .unwrap(),
        );
        let binding = qos_actions::ActionBinding::unit_for(&resource, "save").unwrap();
        let mut button = Button::new("save", "Save", None).on_action(binding);
        button.activate(&mut AutoCommit::new(), &automerge::ROOT);
        Widget::refresh_control_action_availability(&mut button, &std::collections::HashSet::new());
        assert!(!Widget::is_enabled(&button));
        assert!(
            Widget::take_control_invocation(&mut button).is_none(),
            "revocation drops queued authority"
        );
        Widget::refresh_control_action_availability(
            &mut button,
            &std::collections::HashSet::from([qos_actions::ActionAddress::new("doc:1", "save")]),
        );
        assert!(Widget::is_enabled(&button));

        button.set_enabled(false);
        Widget::refresh_control_action_availability(&mut button, &std::collections::HashSet::new());
        Widget::refresh_control_action_availability(
            &mut button,
            &std::collections::HashSet::from([qos_actions::ActionAddress::new("doc:1", "save")]),
        );
        assert!(
            !Widget::is_enabled(&button),
            "permission restore cannot override app-authored disabled state"
        );
    }

    #[test]
    fn legacy_button_paths_are_explicit_in_inventory() {
        let button = Button::new("legacy", "Legacy", Some("flag")).on_click(|| {});
        let bindings = button.control_action_bindings();
        assert!(bindings.iter().any(|entry| matches!(
            entry.kind,
            crate::widget::ControlBindingKind::CollaborativeDocument(_)
        )));
        assert!(bindings.iter().any(|entry| matches!(
            entry.kind,
            crate::widget::ControlBindingKind::LegacyCallback
        )));
    }

    #[test]
    fn face_colors_neutral_tracks_hover_and_press() {
        let theme = Theme::dark();
        let rest = ButtonInteract::default();
        let hover = ButtonInteract {
            hovered: true,
            ..Default::default()
        };
        let press = ButtonInteract {
            pressed: true,
            ..Default::default()
        };
        let (bg, border, fg) = face_colors(&theme, ButtonStyle::Neutral, rest);
        assert_eq!(
            bg, theme.tokens.colors.surfaces.raised,
            "rest fill = raised surface"
        );
        assert_eq!(
            border,
            Some(theme.tokens.colors.border_standard),
            "neutral shows a border"
        );
        assert_eq!(fg, theme.tokens.colors.text_primary);
        assert_eq!(
            face_colors(&theme, ButtonStyle::Neutral, hover).0,
            theme.bg_hover
        );
        assert_eq!(
            face_colors(&theme, ButtonStyle::Neutral, press).0,
            theme.bg_active
        );
    }

    #[test]
    fn face_colors_ghost_is_transparent_at_rest_then_tints() {
        use crate::a11y::contrast_ratio;
        let theme = Theme::dark();
        let rest = ButtonInteract::default();
        let hover = ButtonInteract {
            hovered: true,
            ..Default::default()
        };
        let press = ButtonInteract {
            pressed: true,
            ..Default::default()
        };
        let (bg, border, fg) = face_colors(&theme, ButtonStyle::Ghost, rest);
        assert_eq!(
            bg,
            vello::peniko::Color::TRANSPARENT,
            "ghost rest fill is transparent"
        );
        assert_eq!(border, None, "ghost has no border");
        // Quiet is a rank, not a licence to be unreadable: the tone starts
        // at `text_dim` and steps out only as far as the hover surface demands.
        let base = theme.tokens.colors.surfaces.base;
        assert!(
            contrast_ratio(fg, base) < contrast_ratio(theme.tokens.colors.text_primary, base),
            "ghost label must stay quieter than the primary tier",
        );
        assert!(
            contrast_ratio(fg, theme.bg_hover) >= ON_FILL_MIN_CONTRAST,
            "ghost label must survive the surface hover paints under it",
        );
        assert_eq!(
            face_colors(&theme, ButtonStyle::Ghost, hover).0,
            theme.bg_hover
        );
        assert_eq!(
            face_colors(&theme, ButtonStyle::Ghost, press).0,
            theme.bg_active
        );
    }

    #[test]
    fn face_colors_primary_is_accent_borderless_and_readable() {
        use crate::a11y::contrast_ratio;
        let theme = Theme::dark();
        let rest = ButtonInteract::default();
        let hover = ButtonInteract {
            hovered: true,
            ..Default::default()
        };
        let (bg, border, fg) = face_colors(&theme, ButtonStyle::Primary, rest);
        assert_eq!(bg, theme.accent_primary, "primary rest = accent fill");
        assert_eq!(border, None, "accent fill is self-defining — no border");
        let r = contrast_ratio(fg, bg);
        assert!(
            r >= ON_FILL_MIN_CONTRAST,
            "primary label {fg:?} on {bg:?} is {r:.2}:1, under the {ON_FILL_MIN_CONTRAST}:1 floor",
        );
        let (hbg, _, hfg) = face_colors(&theme, ButtonStyle::Primary, hover);
        assert_eq!(
            hbg, theme.accent_hover,
            "primary hover darkens to the accent-hover token"
        );
        assert_eq!(
            hfg, fg,
            "the label must not change colour under the pointer"
        );
    }

    #[test]
    fn face_colors_danger_is_solid_at_rest_borderless_and_readable() {
        use crate::a11y::contrast_ratio;
        let theme = Theme::dark();
        let rest = ButtonInteract::default();
        let hover = ButtonInteract {
            hovered: true,
            ..Default::default()
        };
        let (bg, border, fg) = face_colors(&theme, ButtonStyle::Danger, rest);
        assert_eq!(
            bg, theme.tokens.colors.semantic.danger,
            "danger rest = solid danger"
        );
        assert_eq!(border, None);
        let r = contrast_ratio(fg, bg);
        assert!(
            r >= ON_FILL_MIN_CONTRAST,
            "danger label {fg:?} on {bg:?} is {r:.2}:1, under the {ON_FILL_MIN_CONTRAST}:1 floor",
        );
        let (hbg, _, hfg) = face_colors(&theme, ButtonStyle::Danger, hover);
        assert_ne!(
            hbg, theme.tokens.colors.semantic.danger,
            "danger hover lifts off the rest colour for feedback",
        );
        let hr = contrast_ratio(hfg, hbg);
        assert!(
            hr >= ON_FILL_MIN_CONTRAST,
            "danger hover label {hfg:?} on {hbg:?} is {hr:.2}:1"
        );
        assert_eq!(
            hfg, fg,
            "the label must not change colour under the pointer"
        );
    }

    #[test]
    fn button_face_builder_sets_style_and_state() {
        let f = ButtonFace::new(Rect::new(0.0, 0.0, 80.0, 32.0), "Send")
            .primary()
            .hovered(true)
            .weight(crate::text_render::TextWeight::Bold)
            .text_size(13.0);
        let theme = Theme::dark();
        let (bg, border, fg) = f.colors(&theme);
        assert_eq!(bg, theme.accent_hover);
        assert_eq!(border, None);
        // Resolves through `face_colors`, so it carries the same derived
        // label rather than a literal of its own.
        assert_eq!(
            fg,
            face_colors(
                &theme,
                ButtonStyle::Primary,
                ButtonInteract {
                    hovered: true,
                    ..Default::default()
                }
            )
            .2,
        );
        assert!(crate::a11y::contrast_ratio(fg, bg) >= ON_FILL_MIN_CONTRAST);
        assert_eq!(f.spoken_name(), "Send", "visible label is the spoken name");
    }

    #[test]
    fn icon_only_face_spoken_name_falls_back_to_accessible_name() {
        let f = ButtonFace::new(Rect::new(0.0, 0.0, 32.0, 32.0), "")
            .icon(crate::icons::Icon::Plus)
            .accessible_name("Add page");
        assert_eq!(f.spoken_name(), "Add page");
    }

    #[test]
    fn paint_emits_fill_and_label_geometry() {
        use vello_encoding::DrawTag;
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let mut scene = Scene::new();
        Button::paint(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &ButtonFace::new(Rect::new(10.0, 10.0, 110.0, 46.0), "Go").primary(),
        );
        let fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(
            fills > 1,
            "body fill + label glyphs ⇒ several COLOR tags, got {fills}"
        );
    }

    #[test]
    fn cursor_shape_is_pointer_over_button() {
        let mut b = Button::new("id", "Go", None);
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = b.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(200.0),
                height: taffy::AvailableSpace::Definite(60.0),
            },
        )
        .unwrap();
        b.apply_layout(&tree, node, Point::ZERO);

        assert_eq!(
            Widget::cursor_shape(&b, Point::new(10.0, 10.0)),
            Some(winit::window::CursorIcon::Pointer),
            "pointer over the button requests the pointing-hand cursor",
        );
        assert_eq!(
            Widget::cursor_shape(&b, Point::new(500.0, 500.0)),
            None,
            "pointer well outside the button defers (no cursor claim)",
        );
    }

    #[test]
    fn on_click_callback_fires_on_release_over_button() {
        use std::cell::Cell;
        use std::rc::Rc;
        use winit::event::{DeviceId, ElementState, MouseButton};

        let fired = Rc::new(Cell::new(0u32));
        let mut b = {
            let fired = fired.clone();
            Button::new("id", "Go", None).on_click(move || fired.set(fired.get() + 1))
        };

        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = b.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(200.0),
                height: taffy::AvailableSpace::Definite(60.0),
            },
        )
        .unwrap();
        b.apply_layout(&tree, node, Point::ZERO);

        let device: DeviceId = unsafe { std::mem::zeroed() };
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        let mut dispatch = |b: &mut Button, ev: WindowEvent| {
            let mut handled = false;
            let mut focus = None;
            b.handle_event(
                &ev,
                1.0,
                &mods,
                &mut doc,
                &root,
                &mut handled,
                &mut focus,
                node,
                &tree,
            );
        };

        dispatch(
            &mut b,
            WindowEvent::CursorMoved {
                device_id: device,
                position: (10.0, 10.0).into(),
            },
        );
        dispatch(
            &mut b,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        assert_eq!(fired.get(), 0, "not fired until release");
        dispatch(
            &mut b,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );
        assert_eq!(fired.get(), 1, "fired once on release over the button");
    }

    #[test]
    fn with_enabled_false_is_reported_by_is_enabled() {
        let b = Button::new("id", "Go", None).with_enabled(false);
        assert!(!Widget::is_enabled(&b));
        let b2 = Button::new("id", "Go", None);
        assert!(Widget::is_enabled(&b2), "default is enabled");
    }

    #[test]
    fn accessibility_reports_button_role_name_and_state() {
        use crate::a11y::Role;
        let b = Button::new("id", "Save", None);
        let node = Widget::accessibility(&b).expect("button is not presentational");
        assert_eq!(node.role, Role::Button);
        assert_eq!(node.name.as_deref(), Some("Save"));
        assert!(node.state.focusable);
        assert!(!node.state.focused);
        assert!(!node.state.disabled);
        assert_eq!(node.state.checked, None, "a button is not checkable");
    }

    #[test]
    fn accessibility_reflects_disabled_and_focus() {
        let mut b = Button::new("id", "Save", None).with_enabled(false);
        b.set_focus(true);
        let node = Widget::accessibility(&b).unwrap();
        assert!(node.state.disabled);
        assert!(node.state.focused);
    }

    #[test]
    fn accessibility_icon_only_button_without_name_is_silent() {
        let b = Button::new("id", "", None).with_icon(crate::icons::Icon::Check);
        let node = Widget::accessibility(&b).unwrap();
        assert_eq!(node.name, None);
    }

    #[test]
    fn accessibility_icon_only_button_uses_accessible_name() {
        let b = Button::new("id", "", None)
            .with_icon(crate::icons::Icon::Check)
            .with_accessible_name("Confirm");
        let node = Widget::accessibility(&b).unwrap();
        assert_eq!(node.name.as_deref(), Some("Confirm"));
    }

    #[test]
    fn tooltip_is_exposed_to_the_host() {
        let b = Button::new("id", "", None)
            .with_icon(crate::icons::Icon::Search)
            .tooltip("Search");
        assert_eq!(Widget::tooltip(&b), Some("Search"));
        let plain = Button::new("id", "Save", None);
        assert_eq!(Widget::tooltip(&plain), None, "no tooltip unless attached");
    }

    #[test]
    fn icon_only_tooltip_doubles_as_accessible_name() {
        let b = Button::new("id", "", None)
            .with_icon(crate::icons::Icon::Search)
            .tooltip("Search");
        let node = Widget::accessibility(&b).unwrap();
        assert_eq!(node.name.as_deref(), Some("Search"));
    }

    #[test]
    fn explicit_accessible_name_wins_over_tooltip() {
        let b = Button::new("id", "", None)
            .with_icon(crate::icons::Icon::MoreHorizontal)
            .with_accessible_name("View options")
            .tooltip("More");
        let node = Widget::accessibility(&b).unwrap();
        assert_eq!(node.name.as_deref(), Some("View options"));
    }

    #[test]
    fn accessibility_visible_label_wins_over_accessible_name() {
        let b = Button::new("id", "Save", None).with_accessible_name("Persist");
        let node = Widget::accessibility(&b).unwrap();
        assert_eq!(node.name.as_deref(), Some("Save"));
    }

    #[test]
    fn click_value_writes_a_string_command() {
        let mut button = Button::new("id", "Apply", Some("command"))
            .with_click_value("apply")
            .fit_content(34.0);
        let mut doc = AutoCommit::new();
        button.activate(&mut doc, &automerge::ROOT);

        let (value, _) = doc.get(&automerge::ROOT, "command").unwrap().unwrap();
        assert_eq!(value.to_str(), Some("apply"));
    }

    #[test]
    fn semantic_activation_uses_the_same_click_value_contract() {
        let mut button = Button::new("id", "Apply", Some("command")).with_click_value("apply");
        let mut doc = AutoCommit::new();

        assert!(button.perform_action(&crate::a11y::Action::Activate, &mut doc, &automerge::ROOT,));

        let (value, _) = doc.get(&automerge::ROOT, "command").unwrap().unwrap();
        assert_eq!(value.to_str(), Some("apply"));
    }

    #[test]
    fn disabled_button_rejects_input() {
        use std::cell::Cell;
        use std::rc::Rc;
        use winit::event::{DeviceId, ElementState, MouseButton};

        let fired = Rc::new(Cell::new(0u32));
        let mut b = {
            let fired = fired.clone();
            Button::new("id", "Go", Some("clicked"))
                .on_click(move || fired.set(fired.get() + 1))
                .with_enabled(false)
        };

        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = b.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(200.0),
                height: taffy::AvailableSpace::Definite(60.0),
            },
        )
        .unwrap();
        b.apply_layout(&tree, node, Point::ZERO);

        let device: DeviceId = unsafe { std::mem::zeroed() };
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        let mut focus = None;
        let mut handled = false;

        for ev in [
            WindowEvent::CursorMoved {
                device_id: device,
                position: (10.0, 10.0).into(),
            },
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        ] {
            b.handle_event(
                &ev,
                1.0,
                &mods,
                &mut doc,
                &root,
                &mut handled,
                &mut focus,
                node,
                &tree,
            );
        }

        assert_eq!(fired.get(), 0, "disabled button must not fire on_click");
        assert!(focus.is_none(), "disabled button must not request focus");
        assert!(!handled, "disabled button must not swallow input");
        assert!(
            doc.get(&root, "clicked").unwrap().is_none(),
            "disabled button must not write the CRDT path"
        );
    }
}

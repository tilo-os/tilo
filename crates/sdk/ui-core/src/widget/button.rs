use crate::i18n;
use crate::paint::PaintCtx;
use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::colors::ON_FILL_MIN_CONTRAST;
use crate::widget::ActionEmitter;
use crate::widget::ControlSize;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::{AppModel, ModelPath};
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
    /// Original presentation captured while a responsive toolbar displays
    /// this button icon-first. Kept separately so an app's explicit tooltip
    /// and accessible name are restored exactly when space returns.
    compact_presentation: Option<(String, Option<String>, Option<String>)>,
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
    /// Optional target-height floor independent of intrinsic content sizing.
    min_height: Option<f32>,
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
    measured_fit_width: Option<f32>,
    /// The settled footprint of a default-sized button — [`DEFAULT_SIZE`]
    /// grown to hold the measured label. Seeds the next `build_layout` so a
    /// stable label costs no extra layout pass; cleared whenever the label
    /// can have changed shape (theme type step, locale).
    default_footprint: Option<(f32, f32)>,
    content_sized: bool,
    content_padding: f32,
    /// Sets the label role (see [`ControlSize::text_role`]).
    control_size: ControlSize,
    /// The label role resolved against the theme seen in `build_layout`,
    /// for `measure`, which has no theme of its own; `draw` reads the theme
    /// it is handed.
    label_style: TextStyle,
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
    native_resource: Option<tilo_actions::Resource>,
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
    last_state: (bool, bool, bool, bool), // (hovered, pressed, focused, enabled)
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
            compact_presentation: None,
            rect: Rect::ZERO,
            is_hovered: false,
            is_pressed: false,
            is_focused: false,
            icon: None,
            size: None,
            min_height: None,
            fill_width: false,
            style: ButtonStyle::Neutral,
            on_click_path: on_click_path.map(std::string::ToString::to_string),
            click_value: None,
            fit_content: false,
            measured_fit_width: None,
            default_footprint: None,
            content_sized: false,
            content_padding: 16.0,
            control_size: ControlSize::Regular,
            label_style: crate::tokens::typography::stock().style(ControlSize::Regular.text_role()),
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
            last_state: (false, false, false, true),
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
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
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
    /// prefer a typed document [`Action`](tilo_actions::Action).
    #[must_use]
    pub fn on_effect<F: FnMut() + 'static>(self, handler: F) -> Self {
        self.on_native(tilo_actions::OperationKind::Effect, handler)
    }

    /// Publish a zero-argument transient presentation action. View actions are
    /// discoverable and agent-driveable but intentionally absent from document
    /// history and collaboration logs.
    #[must_use]
    pub fn on_view<F: FnMut() + 'static>(self, handler: F) -> Self {
        self.on_native(tilo_actions::OperationKind::View, handler)
    }

    #[expect(
        clippy::expect_used,
        reason = "String serialization and lookup of the just-created action are infallible"
    )]
    fn on_native<F: FnMut() + 'static>(
        mut self,
        kind: tilo_actions::OperationKind,
        handler: F,
    ) -> Self {
        const TYPE_ID: &str = "tilo.ui.control.invoke";
        let resource_id = format!("tilo.control:{}", self.id);
        let mut spec = tilo_actions::ActionSpec::operation(
            "invoke",
            self.label.clone(),
            kind,
            TYPE_ID,
            &self.id,
        )
        .expect("a control id is always JSON serializable");
        spec.contract.discovery = tilo_actions::DiscoveryScope::Launchable;
        let resource = tilo_actions::Resource::new(resource_id, "tilo.control", self.label.clone())
            .with_action(spec);
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "invoke")
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
            self.layout_dirty |= self.fit_content || self.content_sized;
            // A new locale re-shapes the label; the default footprint it
            // settled to may no longer hold it.
            self.default_footprint = None;
        }
    }

    /// Overrides the default 120×40 intrinsic size.
    #[must_use]
    pub fn with_size(mut self, width: f32, height: f32) -> Self {
        self.size = Some((width, height));
        self.fill_width = false;
        self.fit_content = false;
        self.content_sized = false;
        self
    }

    /// Keep at least this logical target height without capping intrinsic
    /// content. Negative values clamp to zero; non-finite values clear the floor.
    #[must_use]
    pub fn with_min_height(mut self, height: f32) -> Self {
        self.min_height = height.is_finite().then_some(height.max(0.0));
        self
    }

    /// Fill the available parent width at the supplied height.
    #[must_use]
    pub fn fill_width(mut self, height: f32) -> Self {
        self.size = Some((0.0, height));
        self.fill_width = true;
        self.fit_content = false;
        self.content_sized = false;
        self
    }

    /// Measure the actual label and icon width, retaining the supplied fixed height.
    /// Long text may intentionally overflow that explicit height.
    ///
    /// App and `FrameHarness` settle the measured width before applying layout.
    /// Manual hosts must use [`crate::widget::compute_widget_layout`]; direct
    /// Taffy computation skips this settlement and can stretch the auto width.
    /// Rebuild layout after changing renderer metrics or localization so the
    /// next measurement resolves current content rather than a previous width.
    #[must_use]
    pub fn fit_content(mut self, height: f32) -> Self {
        self.fit_content = true;
        self.content_sized = false;
        self.fill_width = false;
        self.size = Some((0.0, height));
        self
    }

    /// Size both dimensions from the measured label, wrapping to available width.
    #[must_use]
    pub fn content_sized(mut self) -> Self {
        self.content_sized = true;
        self.fit_content = false;
        self.fill_width = false;
        self.size = None;
        self
    }

    pub(crate) fn with_action_sizing(self) -> Self {
        if self.size.is_none() {
            self.content_sized()
        } else {
            self
        }
    }

    /// How big this button is: its label role. Small and Regular set the
    /// label in `Label`, Large in `Body`.
    #[must_use]
    pub fn control_size(mut self, size: ControlSize) -> Self {
        self.control_size = size;
        self.dirty = true;
        self.default_footprint = None;
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

    /// Outlined destructive action; see [`ButtonStyle::DangerOutline`].
    #[must_use]
    pub fn danger_outline(mut self) -> Self {
        self.style = ButtonStyle::DangerOutline;
        self
    }

    pub fn set_style(&mut self, style: ButtonStyle) {
        if self.style != style {
            self.style = style;
            self.dirty = true;
        }
    }

    /// Prefer the icon in a space-constrained toolbar while retaining the
    /// original visible label as the accessible name and hover tooltip.
    ///
    /// Apps should use this for responsive presentation only; activation,
    /// focus and resource-action semantics are intentionally unchanged.
    pub fn set_compact_toolbar_presentation(&mut self, compact: bool) {
        if compact && !self.label.is_empty() && self.compact_presentation.is_none() {
            self.compact_presentation = Some((
                std::mem::take(&mut self.label),
                self.accessible_name.take(),
                self.tooltip.take(),
            ));
            self.accessible_name = self
                .compact_presentation
                .as_ref()
                .and_then(|presentation| presentation.1.clone())
                .or_else(|| {
                    self.compact_presentation
                        .as_ref()
                        .map(|presentation| presentation.0.clone())
                });
            self.tooltip = self
                .compact_presentation
                .as_ref()
                .and_then(|presentation| presentation.2.clone())
                .or_else(|| self.accessible_name.clone());
            self.dirty = true;
        } else if !compact
            && let Some((label, accessible_name, tooltip)) = self.compact_presentation.take()
        {
            self.label = label;
            self.accessible_name = accessible_name;
            self.tooltip = tooltip;
            self.dirty = true;
        }
    }

    /// The counterpart to `TextInput::apply_layout_external` for an app
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
///
/// `DangerOutline` is the quiet destructive action: transparent at rest
/// like `Ghost`, but outlined and lettered in the danger ink, so a
/// secondary "Delete" beside a primary "Save" reads as destructive without
/// the weight of a filled `Danger` face. Hover and press tint the fill the
/// same way `Ghost` does; the ink is resolved once so it clears the
/// readability floor on every one of those grounds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ButtonStyle {
    #[default]
    Neutral,
    Primary,
    Danger,
    Ghost,
    DangerOutline,
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

fn danger_face_colors(
    theme: &Theme,
    interact: ButtonInteract,
) -> (
    vello::peniko::Color,
    Option<vello::peniko::Color>,
    vello::peniko::Color,
) {
    use crate::color_compat::ColorCompat;
    use vello::peniko::Color;
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

    // Disabled actions must not retain the semantic fill of an enabled
    // primary or destructive action — a translucent blue "Save" still reads
    // as actionable. Every style resolves to the *neutral rest face*
    // (raised fill, derived edge, primary ink) and the painters lay that
    // face down under `widget::DISABLED_ALPHA`, so rest and disabled never
    // look alike (HIG §9.5 "Rest edges") while the edge keeps the control's
    // bounds discoverable without implying that it can be activated.
    if interact.disabled {
        let bg = theme.tokens.colors.surfaces.raised;
        let fg = theme.tokens.colors.ink_on_any(
            &[bg],
            theme.tokens.colors.text_primary,
            ON_FILL_MIN_CONTRAST,
        );
        return (bg, Some(crate::surface::edge_on(bg)), fg);
    }

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
            // The edge is derived from the rest surface and held across
            // hover and press: a declared `border_standard` is three shades
            // from the dark raised tier and simply vanished there.
            (bg, Some(crate::surface::edge_on(rest)), fg)
        }
        ButtonStyle::Primary => {
            let rest = theme.tokens.colors.intent.inverted;
            let active = theme.tokens.colors.intent.inverted_hover;
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
        ButtonStyle::Danger => danger_face_colors(theme, interact),
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
        ButtonStyle::DangerOutline => {
            // Same grounds as `Ghost` — transparent at rest, the theme's
            // opaque hover/active fills under the pointer — so the danger
            // ink can be measured against every fill it will sit on. A
            // translucent danger tint (what apps hand-painted here) has no
            // measurable ground; the shared face uses the opaque tiers.
            let bg = if interact.pressed {
                theme.bg_active
            } else if interact.hovered {
                theme.bg_hover
            } else {
                Color::TRANSPARENT
            };
            let ink = theme.tokens.colors.ink_on_any(
                &[
                    theme.tokens.colors.surfaces.base,
                    theme.bg_hover,
                    theme.bg_active,
                ],
                theme.tokens.colors.semantic.danger,
                ON_FILL_MIN_CONTRAST,
            );
            // One colour for outline and label: the border is what says
            // "button" at rest, so it must be as readable as the word.
            (bg, Some(ink), ink)
        }
    }
}

/// Built fluently: `ButtonFace::new(rect, "Save").primary().hovered(hit)`.
/// Defaults to a neutral [`ControlSize::Regular`] face — a `Label` — and the
/// theme's `radii.control` corner (8 px at 1×, read from the [`Theme`] handed to
/// [`Button::paint`]).
///
/// The label is the size's role resolved in the theme handed to
/// [`Button::paint`]; a canvas host sizes its rect with
/// [`ControlSize::height`] and measures the label with
/// [`ControlSize::label_style`], so rect and glyphs agree at every scale.
pub struct ButtonFace<'a> {
    rect: Rect,
    label: &'a str,
    icon: Option<crate::icons::Icon>,
    style: ButtonStyle,
    interact: ButtonInteract,
    /// `None` resolves to `theme.tokens.radii.control` at paint time.
    radius: Option<f64>,
    control_size: ControlSize,
    accessible_name: Option<&'a str>,
    wrap_label: bool,
}

impl<'a> ButtonFace<'a> {
    pub fn new(rect: Rect, label: &'a str) -> Self {
        Self {
            rect,
            label,
            icon: None,
            style: ButtonStyle::Neutral,
            interact: ButtonInteract::default(),
            radius: None,
            control_size: ControlSize::Regular,
            accessible_name: None,
            wrap_label: false,
        }
    }
    /// Wrap the label inside this manually assigned rectangle using theme insets.
    #[must_use]
    pub fn wrap_label(mut self) -> Self {
        self.wrap_label = true;
        self
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
    /// Outlined destructive action; see [`ButtonStyle::DangerOutline`].
    #[must_use]
    pub fn danger_outline(mut self) -> Self {
        self.style = ButtonStyle::DangerOutline;
        self
    }
    #[must_use]
    pub fn icon(mut self, icon: crate::icons::Icon) -> Self {
        self.icon = Some(icon);
        self
    }
    /// Override the theme's `radii.control` corner.
    #[must_use]
    pub fn radius(mut self, radius: f64) -> Self {
        self.radius = Some(radius);
        self
    }
    /// How big this face is: its label role. The host sizes the rect.
    #[must_use]
    pub fn control_size(mut self, size: ControlSize) -> Self {
        self.control_size = size;
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

/// Footprint of a retained [`Button`] when no size, `fit_content` or
/// `content_sized` was requested: 40 px tall so it shares a centre line with
/// the 40 px dropdown and text field in a mixed row, 120 px wide. Not a
/// floor — `fit_content` measures down to [`FIT_MIN_WIDTH`]. Both axes are
/// floored at the interaction environment's minimum target.
const DEFAULT_SIZE: (f32, f32) = (120.0, 40.0);

/// Focus ring and rest border, stroked centred on the face outline like
/// every other control (checkbox, toggle, segmented strip).
const FOCUS_RING_WIDTH: f64 = 2.0;
const REST_BORDER_WIDTH: f64 = 1.0;

/// Icon edge inside a face: the face height less one base-size text line,
/// held between 12 and 22 px — 22 in the 40 px default, 18 at the 32 px
/// minimum target, 14 in a 28 px dense row. Deliberately off the spacing
/// scale: it tracks the label's line, not a layout gap.
const ICON_INSET: f64 = 14.0;
const ICON_MIN_EDGE: f64 = 12.0;
const ICON_MAX_EDGE: f64 = 22.0;

/// Gap between icon and label. 6 px sits between `s1` (4 — the glyph
/// crowds the first letter) and `s2` (8 — icon and label read as two
/// clusters); kept at its shipped value so no existing button moves.
const ICON_LABEL_GAP: f64 = 6.0;

/// Horizontal allowance `fit_content` adds around its measured label when
/// the button keeps an explicit height: 15 px a side, the pre-measurement
/// width every fit-content button has shipped with. `content_sized`
/// buttons use `theme.padding_standard` instead.
const FIT_SIDE_PADDING: f32 = 15.0;
/// Narrowest a `fit_content` button gets, so a one-letter label still
/// reads as a button rather than a square.
const FIT_MIN_WIDTH: f32 = 64.0;

/// One formula for the icon edge, shared by [`label_width`],
/// [`paint_icon_and_label`] and [`Widget::measure`] so the three cannot
/// drift apart.
fn icon_edge(face_height: f64) -> f64 {
    (face_height - ICON_INSET).clamp(ICON_MIN_EDGE, ICON_MAX_EDGE)
}

/// `measure` works in taffy's `f32`; the painters work in kurbo's `f64`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "logical pixel counts far below f32 precision"
)]
fn px_f32(value: f64) -> f32 {
    value as f32
}

/// Resolve a label key for painting, measuring or speaking. An empty key is
/// an icon-only face and stays empty: routed through `i18n!` it would come
/// back as the pseudo-locale's `⟦⟧` markers — a visible bracket pair beside
/// the glyph, and a non-empty spoken name that hides the `accessible_name` /
/// tooltip fallback.
fn localized_label(key: &str) -> String {
    if key.is_empty() {
        String::new()
    } else {
        i18n!(key)
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "logical layout width narrows to renderer precision"
)]
fn label_width(rect: Rect, icon: Option<crate::icons::Icon>, padding: f32) -> f32 {
    let icon_width = if icon.is_some() {
        icon_edge(rect.height()) + ICON_LABEL_GAP
    } else {
        0.0
    };
    (rect.width() as f32 - padding * 2.0 - icon_width as f32).max(0.0)
}

/// What sits inside a button's face: an optional icon, the label, the style
/// it is set in, and the width it wraps to (if any).
struct ButtonLabel<'a> {
    icon: Option<crate::icons::Icon>,
    text: &'a str,
    style: TextStyle,
    wrap_width: Option<f32>,
}

impl ButtonLabel<'_> {
    /// Paints the icon and/or label centred in `rect`: icon-only centres the
    /// glyph, otherwise the icon (if any) sits left of the label with a fixed
    /// gap, the cluster as a whole centred. Shared by the immediate-mode
    /// [`Button::paint`] and the retained widget's cached [`Widget::draw`] so
    /// the two stay pixel-identical.
    fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect, fg: vello::peniko::Color) {
        let icon_px = icon_edge(rect.height());
        match (self.icon, self.text.is_empty()) {
            (Some(icon), true) => {
                // Icon-only — centre the glyph.
                let ix = rect.center().x - icon_px / 2.0;
                let iy = rect.center().y - icon_px / 2.0;
                crate::icons::draw_icon(
                    ctx.scene,
                    ctx.transform,
                    Rect::new(ix, iy, ix + icon_px, iy + icon_px),
                    icon,
                    fg,
                    crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            (icon, _) => {
                let gap = if icon.is_some() { ICON_LABEL_GAP } else { 0.0 };
                let icon_w = if icon.is_some() { icon_px } else { 0.0 };
                // A single-line label wider than its face is elided, never
                // painted across the edge; the inset keeps it off the edge.
                // A face too small for any text keeps the ellipsis, so a
                // labelled control never reads as icon-only.
                let elided;
                let label = if self.wrap_width.is_none() {
                    let inset = f64::from(ctx.theme.tokens.spacing.s2);
                    let room = rect.width() - 2.0 * inset - icon_w - gap;
                    elided = ctx.text.elide_to_width_style(
                        self.text,
                        px_f32(room.max(1.0)),
                        &self.style,
                    );
                    elided.as_str()
                } else {
                    self.text
                };
                let (text_width, text_height) =
                    ctx.text.measure_style(label, self.wrap_width, &self.style);
                let cluster_w = icon_w + gap + f64::from(text_width);
                let start_x = rect.center().x - cluster_w / 2.0;
                if let Some(icon) = icon {
                    let iy = rect.center().y - icon_px / 2.0;
                    crate::icons::draw_icon(
                        ctx.scene,
                        ctx.transform,
                        Rect::new(start_x, iy, start_x + icon_px, iy + icon_px),
                        icon,
                        fg,
                        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
                let text_x = start_x + icon_w + gap;
                let text_y = rect.y0 + (rect.height() - f64::from(text_height)) / 2.0;
                ctx.text(
                    label,
                    &self.style,
                    fg,
                    Point::new(text_x, text_y),
                    self.wrap_width,
                );
            }
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
        let radius = face
            .radius
            .unwrap_or_else(|| f64::from(theme.tokens.radii.control));
        let shape = RoundedRect::from_rect(face.rect, radius);

        // Disabled is the rest face under one group alpha (HIG §9.5), the
        // same signal every other control gives, so rest and disabled are
        // never the same picture.
        if face.interact.disabled {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &shape,
            );
        }
        scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &shape);
        // Rest edge and focus ring share one half-pixel-inset outline, so
        // the hairline never bleeds onto the ground.
        let outline = crate::surface::edge_outline(face.rect, radius);
        if face.interact.focused && !face.interact.disabled {
            scene.stroke(
                &vello::kurbo::Stroke::new(FOCUS_RING_WIDTH),
                transform,
                theme.border_focus,
                None,
                &outline,
            );
        } else if let Some(bc) = border {
            scene.stroke(
                &vello::kurbo::Stroke::new(REST_BORDER_WIDTH),
                transform,
                bc,
                None,
                &outline,
            );
        }

        let label = localized_label(face.label);
        ButtonLabel {
            icon: face.icon,
            text: &label,
            style: face.control_size.label_style(theme),
            wrap_width: face
                .wrap_label
                .then(|| label_width(face.rect, face.icon, theme.padding_standard)),
        }
        .paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            face.rect,
            fg,
        );
        if face.interact.disabled {
            scene.pop_layer();
        }
    }

    /// Appends the cached face scene. The disabled group alpha is encoded
    /// *inside* the cache by `draw`, so the append itself applies none — a
    /// second layer here would dim a disabled face twice.
    fn append_cached(&self, out_scene: &mut Scene, inner: Affine) {
        out_scene.append(&self.cached, Some(inner));
    }

    /// No explicit size, no fit/fill height, not content-sized: the button
    /// stands at [`DEFAULT_SIZE`], grown only when the label needs more.
    fn uses_default_footprint(&self) -> bool {
        self.size.is_none() && !self.content_sized
    }

    /// [`DEFAULT_SIZE`] floored at the measured label: the height grows from
    /// the label's measured line plus the content padding (the same rule the
    /// `content_sized` face uses, so the two auto-height modes agree at
    /// every scale), the width from label + icon + the fit-content side
    /// allowance. At 1× a base-size line is 17.5 px and a label under 90 px
    /// wide, so the result is exactly 120×40; at 2× text the 35 px line
    /// lifts the face to 47 px and a wide label pushes it past 120 instead
    /// of painting outside it.
    fn measured_default_footprint(&self, text: &mut TextRenderer) -> (f32, f32) {
        let label = localized_label(&self.label);
        let (label_width, line_height) = text.measure_style(&label, None, &self.label_style);
        let minimum_target = self.interaction.minimum_target();
        let height = DEFAULT_SIZE
            .1
            .max((line_height + self.content_padding).ceil())
            .max(minimum_target);
        let icon_width = if self.icon.is_some() {
            let gap = if label.is_empty() {
                0.0
            } else {
                ICON_LABEL_GAP
            };
            px_f32(icon_edge(f64::from(height)) + gap)
        } else {
            0.0
        };
        let width = DEFAULT_SIZE
            .0
            .max((label_width + icon_width + FIT_SIDE_PADDING * 2.0).ceil())
            .max(minimum_target);
        (width, height)
    }
}

impl Button {
    /// Run the native handler for a `tilo.ui.control.invoke` addressed to
    /// this button. `Ok(false)` when the operation is not this button's.
    fn invoke_native_control(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document
            || type_id != "tilo.ui.control.invoke"
            || self.native_handler.is_none()
        {
            return Ok(false);
        }
        let control: String = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        if control != self.id {
            return Ok(false);
        }
        if let Some(handler) = &mut self.native_handler {
            handler();
        }
        Ok(true)
    }
}

impl Widget for Button {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::permissions::capability::PermissionGated,
        dyn crate::presence::capability::PublishesPresence
    );

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        if self.interaction != environment {
            self.interaction = environment;
            self.is_hovered &= environment.supports_hover();
            self.layout_dirty = true;
            self.dirty = true;
            // The footprint is floored at the environment's minimum target.
            self.default_footprint = None;
        }
    }

    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {
        self.observe_i18n_revision();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.observe_i18n_revision();
        self.measured_fit_width = None;
        let label_style = self.control_size.label_style(theme);
        if label_style != self.label_style
            || theme.padding_standard.to_bits() != self.content_padding.to_bits()
        {
            // A theme scale change re-shapes or re-wraps the label; the
            // cached face must not survive at the old metrics. (The live App
            // rebuilds widgets on a theme change; a relayout-only host such
            // as `FrameHarness::set_theme` relies on this.)
            self.dirty = true;
            self.default_footprint = None;
        }
        self.label_style = label_style;
        self.content_padding = theme.padding_standard;
        // A default-sized button starts from its last settled footprint so
        // the solver's first pass already agrees with the measurement
        // `adjust_layout_with_measure` will make; `DEFAULT_SIZE` until then.
        let (mut w, mut h) = self
            .size
            .or_else(|| {
                self.default_footprint
                    .filter(|_| self.uses_default_footprint())
            })
            .unwrap_or(DEFAULT_SIZE);
        let minimum_target = self.interaction.minimum_target();
        w = w.max(minimum_target);
        h = h.max(minimum_target);
        let node = taffy.leaf_node(Style {
            flex_shrink: if self.content_sized { 1.0 } else { 0.0 },
            min_size: Size {
                width: Dimension::length(0.0),
                height: self.min_height.map_or(Dimension::auto(), Dimension::length),
            },
            max_size: Size {
                width: if self.content_sized {
                    Dimension::percent(1.0)
                } else {
                    Dimension::auto()
                },
                height: Dimension::auto(),
            },
            size: Size {
                width: if self.fill_width {
                    Dimension::percent(1.0)
                } else if self.fit_content || self.content_sized {
                    Dimension::auto()
                } else {
                    Dimension::length(w)
                },
                height: if self.content_sized {
                    Dimension::auto()
                } else {
                    Dimension::length(h)
                },
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> Size<f32> {
        let label = localized_label(&self.label);
        let fixed_height = self
            .size
            .map_or(DEFAULT_SIZE.1, |(_, h)| h)
            .max(self.interaction.minimum_target());
        let icon_width = if self.icon.is_some() {
            px_f32(if self.content_sized {
                ICON_MAX_EDGE
            } else {
                icon_edge(f64::from(fixed_height))
            })
        } else {
            0.0
        };
        let icon_gap = if self.icon.is_some() && !label.is_empty() {
            px_f32(ICON_LABEL_GAP)
        } else {
            0.0
        };
        let horizontal = if self.content_sized {
            self.content_padding * 2.0
        } else {
            FIT_SIDE_PADDING * 2.0
        };
        let extra_width = icon_width + icon_gap + horizontal;
        let max_width = if self.content_sized {
            known
                .width
                .or(match available.width {
                    taffy::AvailableSpace::Definite(width) => Some(width),
                    taffy::AvailableSpace::MinContent => Some(0.0),
                    taffy::AvailableSpace::MaxContent => None,
                })
                .map(|width| (width.floor() - extra_width).max(0.0))
        } else {
            None
        };
        let (width, height) = text.measure_style(&label, max_width, &self.label_style);
        let intrinsic_width = (width + extra_width)
            .ceil()
            .max(if self.content_sized {
                0.0
            } else {
                FIT_MIN_WIDTH
            })
            .max(self.interaction.minimum_target());
        if self.fit_content {
            // Capture the real width even when a stretching parent supplies
            // a known width. The shared solver pins it before geometry lands.
            self.measured_fit_width = Some(intrinsic_width);
        }
        Size {
            width: known.width.unwrap_or(intrinsic_width),
            height: known.height.unwrap_or_else(|| {
                if self.content_sized {
                    (height.max(icon_width) + self.content_padding)
                        .max(DEFAULT_SIZE.1)
                        .max(self.interaction.minimum_target())
                } else {
                    fixed_height
                }
            }),
        }
    }

    fn adjust_layout_style(&mut self, taffy: &mut TaffyTree<()>, node: NodeId) -> bool {
        let Some(width) = self.measured_fit_width.filter(|_| self.fit_content) else {
            return false;
        };
        let Ok(style) = taffy.style(node) else {
            return false;
        };
        let width = Dimension::length(width);
        if style.size.width == width {
            return false;
        }
        let mut style = style.clone();
        style.size.width = width;
        taffy.set_style(node, style).is_ok()
    }

    /// A default-sized button pins both dimensions as `length`, so taffy
    /// never consults `measure` for it (both axes are known before the leaf
    /// is reached). The label is measured here instead, after each pass,
    /// and the node grown to `Self::measured_default_footprint` when it
    /// does not hold the label — at 1× that is `DEFAULT_SIZE` for every
    /// label narrower than the face, so nothing shipped moves.
    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        if !self.uses_default_footprint() {
            return self.adjust_layout_style(taffy, node);
        }
        let (width, height) = if let Some(footprint) = self.default_footprint {
            footprint
        } else {
            let footprint = self.measured_default_footprint(text);
            self.default_footprint = Some(footprint);
            footprint
        };
        let Ok(current) = taffy.style(node) else {
            return false;
        };
        let size = Size {
            width: Dimension::length(width),
            height: Dimension::length(height),
        };
        if current.size == size {
            return false;
        }
        let mut style = current.clone();
        style.size = size;
        taffy.set_style(node, style).is_ok()
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
        let current_state = (
            self.is_hovered,
            self.is_pressed,
            self.is_focused,
            self.effectively_enabled(),
        );
        if current_state != self.last_state {
            self.dirty = true;
            self.last_state = current_state;
        }
        let needs_emit = self.dirty || self.cache_rect == Rect::ZERO;
        if !needs_emit {
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            let inner = out_transform.pre_translate(Vec2::new(dx, dy));
            self.append_cached(out_scene, inner);
            return;
        }
        // cache_rect tracks *where* the cache was emitted so subsequent
        // frames can translate without re-encoding.
        self.cached.reset();
        let enabled = self.effectively_enabled();
        let scene = &mut self.cached;
        let transform = Affine::IDENTITY;
        let (bg_color, border_opt, fg_color) = face_colors(
            theme,
            self.style,
            ButtonInteract {
                hovered: self.is_hovered,
                pressed: self.is_pressed,
                focused: self.is_focused,
                disabled: !enabled,
            },
        );

        let shape = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.control));
        // Disabled is the rest face under one group alpha (HIG §9.5); the
        // layer lives inside the cache so a cached frame carries it too.
        if !enabled {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &shape,
            );
        }
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            bg_color,
            None,
            &shape,
        );

        // Focus ring wins over the style border; an accent-filled style
        // has no rest border, so it only strokes when focused.
        let outline =
            crate::surface::edge_outline(self.rect, f64::from(theme.tokens.radii.control));
        if self.is_focused && enabled {
            scene.stroke(
                &vello::kurbo::Stroke::new(FOCUS_RING_WIDTH),
                transform,
                theme.border_focus,
                None,
                &outline,
            );
        } else if let Some(border_color) = border_opt {
            scene.stroke(
                &vello::kurbo::Stroke::new(REST_BORDER_WIDTH),
                transform,
                border_color,
                None,
                &outline,
            );
        }

        // The size's label role, read from the theme handed in, so a host
        // that positions the button without a `build_layout` pass still gets
        // the theme's type; `measure` uses the copy captured in
        // `build_layout`, the same value on every taffy-driven path.
        let label = localized_label(&self.label);
        ButtonLabel {
            icon: self.icon,
            text: &label,
            style: self.control_size.label_style(theme),
            wrap_width: self
                .content_sized
                .then(|| label_width(self.rect, self.icon, self.content_padding)),
        }
        .paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
            fg_color,
        );
        if !enabled {
            scene.pop_layer();
        }
        self.cache_rect = self.rect;
        self.append_cached(out_scene, out_transform);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.i18n_revision != crate::i18n::I18nManager::revision()
            || (
                self.is_hovered,
                self.is_pressed,
                self.is_focused,
                self.effectively_enabled(),
            ) != self.last_state
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
            || ((self.fit_content || self.content_sized)
                && self.i18n_revision != crate::i18n::I18nManager::revision())
    }

    fn needs_idle_sync(&self) -> bool {
        self.i18n_revision != crate::i18n::I18nManager::revision()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // Reject all input without setting `handled`.
        if !self.effectively_enabled() {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                        *ctx.handled = true;

                        if self.is_pressed {
                            *ctx.focus_target = Some(ctx.node_id);
                        }

                        if !self.is_pressed {
                            self.activate(ctx.state, ctx.path);
                        }
                    }
                } else if self.is_pressed {
                    self.is_pressed = false;
                    *ctx.handled = true;
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

    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        self.native_resource
            .as_ref()
            .map(|_| std::iter::once("tilo.ui.control.invoke".to_owned()).collect())
            .unwrap_or_default()
    }

    fn start_operation(
        &mut self,
        op: &super::ResourceOperation<'_>,
        _doc: &mut AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<super::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .invoke_native_control(request.kind, request.type_id, request.payload)?
            .then(|| {
                super::ResourceOperationStart::Completed(tilo_actions::ActionOutput::default())
            }))
    }

    /// Enter, Space, or an AccessKit `Activate` performs the same CRDT
    /// mutation + imperative callback as a mouse click.
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AppModel,
        root: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let (state, root) = crate::doc::document(state, root);
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
        let label = localized_label(&self.label);
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

        if let Some(scope) = crate::permissions::capability::permission_scope(self)
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

        if let Some(key) = crate::presence::capability::presence_key(self)
            && let Some(peers) = presence.here.get(&key)
            && let Some(user) = peers.first()
        {
            presence.draw_peer_focus(scene, theme, user, self.rect);
        }
    }
}

impl crate::presence::capability::PublishesPresence for Button {
    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget {
                path: self.presence_path.clone(),
            })
        }
    }
}

impl crate::permissions::capability::PermissionGated for Button {
    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }
}

impl crate::resources::PublishesResources for Button {
    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let Some(mut resource) = self.native_resource.clone() else {
            return Vec::new();
        };
        // The action directory refuses unnamed resources and actions, and
        // an icon-only button learns its accessible name or tooltip after
        // `on_effect` built the resource; publish the name it is announced by.
        if resource.label.trim().is_empty()
            && let Some(name) = self
                .accessible_name
                .as_ref()
                .or(self.tooltip.as_ref())
                .filter(|name| !name.trim().is_empty())
        {
            resource.label.clone_from(name);
            for action in &mut resource.actions {
                if action.label.trim().is_empty() {
                    action.label.clone_from(name);
                }
            }
        }
        vec![resource]
    }
}

impl crate::widget::controls::ActionControls for Button {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        if let Some(action) = &mut self.action {
            let _ = action.install_custody(domain, attachment);
        }
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
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
    fn has_action_control_bindings(&self) -> bool {
        self.action.is_some()
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Some(action) = &mut self.action {
            let before = action.is_available();
            action.refresh_availability(allowed);
            if before != action.is_available() {
                self.dirty = true;
            }
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
    use crate::ColorCompat;
    use crate::tokens::TextRole;
    use crate::widget::Widget;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_shared_button_answers_through_an_ordinary_container() {
        let clicks = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&clicks);
        let mut column = crate::widget::Column::new().push(Box::new(
            Button::new("files.retry", "Retry", None).on_view(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        ));
        let mut doc = AutoCommit::new();
        let kind = tilo_actions::OperationKind::View;
        let payload = br#""files.retry""#;

        let started = crate::widget::ResourceOperation::local(
            tilo_actions::InvocationId::new(),
            kind,
            "tilo.ui.control.invoke",
            payload,
        )
        .start(&mut column, &mut doc, &automerge::ROOT)
        .unwrap();
        assert!(
            started
                .and_then(super::super::ResourceOperationStart::completed)
                .is_some()
        );

        assert_eq!(clicks.load(Ordering::SeqCst), 1);
    }

    fn layout_fit_host(
        host: &mut dyn Widget,
        alignment: taffy::AlignItems,
        theme: &Theme,
        text: &mut TextRenderer,
    ) -> Rect {
        let mut tree = TaffyTree::new();
        let mut map = std::collections::HashMap::new();
        let root = host.build_layout(&mut tree, theme, &mut map);
        let mut style = tree.style(root).unwrap().clone();
        style.size.width = Dimension::length(600.0);
        if style.display == taffy::Display::Grid {
            style.justify_items = Some(alignment);
        } else if style.flex_direction == taffy::FlexDirection::Row {
            style.justify_content = Some(match alignment {
                taffy::AlignItems::Center => taffy::JustifyContent::Center,
                taffy::AlignItems::End => taffy::JustifyContent::End,
                _ => taffy::JustifyContent::Start,
            });
        } else {
            style.align_items = Some(alignment);
        }
        tree.set_style(root, style).unwrap();
        let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
        unsafe {
            bindings.rebuild(host, &tree, root, &map);
            crate::widget::compute_widget_layout(
                &mut tree,
                root,
                Size {
                    width: taffy::AvailableSpace::Definite(600.0),
                    height: taffy::AvailableSpace::Definite(200.0),
                },
                &map,
                &bindings,
                text,
            )
            .unwrap();
        }
        host.apply_layout(&tree, root, Point::ZERO);
        host.children()[0].get_rect()
    }

    fn fit_host(kind: usize, button: Button) -> crate::widget::Element {
        match kind {
            0 => Box::new(
                crate::widget::Column::new()
                    .with_no_padding()
                    .push(Box::new(button)),
            ),
            1 => Box::new(
                crate::widget::Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(button)),
            ),
            _ => Box::new(
                crate::widget::Container::new()
                    .with_no_padding()
                    .with_child(Box::new(button)),
            ),
        }
    }

    #[test]
    fn fit_content_width_stays_compact_and_aligned_in_hosts() {
        fn fit_content_stays_compact_in_stretched_flex_column() {
            let mut host = fit_host(
                0,
                Button::new("back", "Back to apps", None).fit_content(40.0),
            );
            let rect = layout_fit_host(
                host.as_mut(),
                taffy::AlignItems::Stretch,
                &Theme::dark(),
                &mut TextRenderer::new(),
            );
            assert!(rect.width() < 300.0, "fit_content stretched: {rect:?}");
            assert_eq!(rect.height(), 40.0);
        }

        fn fit_content_stays_compact_in_intrinsic_grid_column() {
            let mut host = fit_host(
                1,
                Button::new("back", "Back to apps", None).fit_content(40.0),
            );
            let rect = layout_fit_host(
                host.as_mut(),
                taffy::AlignItems::Stretch,
                &Theme::dark(),
                &mut TextRenderer::new(),
            );
            assert!(rect.width() < 300.0, "fit_content stretched: {rect:?}");
            assert_eq!(rect.height(), 40.0);
        }

        fn compact_fit_width_preserves_host_center_and_end_alignment() {
            for kind in 0..3 {
                for alignment in [taffy::AlignItems::Center, taffy::AlignItems::End] {
                    let mut host = fit_host(
                        kind,
                        Button::new("back", "Back to apps", None).fit_content(40.0),
                    );
                    let rect = layout_fit_host(
                        host.as_mut(),
                        alignment,
                        &Theme::dark(),
                        &mut TextRenderer::new(),
                    );
                    assert!(rect.width() < 300.0, "host {kind}: {rect:?}");
                    let edge = if alignment == taffy::AlignItems::Center {
                        rect.center().x * 2.0
                    } else {
                        rect.x1
                    };
                    assert!(
                        (edge - 600.0).abs() <= 1.0,
                        "host {kind}, {alignment:?}: {rect:?}"
                    );
                }
            }
        }

        fn compact_fit_width_is_the_prepaint_click_and_focus_geometry() {
            let fired = Arc::new(AtomicUsize::new(0));
            let clicks = Arc::clone(&fired);
            let button = Button::new("back", "Back to apps", None)
                .fit_content(40.0)
                .on_click(move || {
                    clicks.fetch_add(1, Ordering::SeqCst);
                });
            let mut harness = crate::headless::FrameHarness::new_with_theme(
                crate::State::new(),
                fit_host(1, button),
                (600, 200),
                Theme::dark(),
            );
            let before = harness.inspection().root.children[0].rect;
            harness.click_at(550.0, before[1] + before[3] / 2.0);
            assert_eq!(
                fired.load(Ordering::SeqCst),
                0,
                "blank grid track must not activate compact button"
            );
            harness.click_at(before[0] + before[2] / 2.0, before[1] + before[3] / 2.0);
            assert_eq!(fired.load(Ordering::SeqCst), 1);
            assert_eq!(
                harness
                    .focused_access_node()
                    .unwrap()
                    .semantic_key
                    .as_deref(),
                Some("back")
            );
            harness.resize((800, 200)).unwrap();
            assert_eq!(harness.inspection().root.children[0].rect[2], before[2]);
        }

        fn explicit_fill_width_still_stretches_in_intrinsic_column() {
            let mut host = fit_host(
                1,
                Button::new("fill", "Back to apps", None).fill_width(40.0),
            );
            let rect = layout_fit_host(
                host.as_mut(),
                taffy::AlignItems::Stretch,
                &Theme::dark(),
                &mut TextRenderer::new(),
            );
            assert_eq!(rect.width(), 600.0);
        }

        fit_content_stays_compact_in_stretched_flex_column();
        fit_content_stays_compact_in_intrinsic_grid_column();
        compact_fit_width_preserves_host_center_and_end_alignment();
        compact_fit_width_is_the_prepaint_click_and_focus_geometry();
        explicit_fill_width_still_stretches_in_intrinsic_column();
    }

    #[test]
    fn content_sized_buttons_remeasure_and_keep_usable_targets() {
        fn retained_fit_width_remeasures_scale_and_locale_after_rebuild() {
            use crate::i18n::I18nManager;
            I18nManager::install(
                "en-US",
                "en-US",
                &[
                    ("en-US", "fit-refresh = Refresh"),
                    ("de", "fit-refresh = Verbindungen aktualisieren"),
                ],
            )
            .unwrap();
            let mut host = fit_host(
                1,
                Button::new("refresh", "fit-refresh", None).fit_content(40.0),
            );
            let mut text = TextRenderer::new();
            let normal = layout_fit_host(
                host.as_mut(),
                taffy::AlignItems::Stretch,
                &Theme::dark(),
                &mut text,
            );
            text.set_text_scale(2.0);
            let large = layout_fit_host(
                host.as_mut(),
                taffy::AlignItems::Stretch,
                &Theme::dark().with_text_scale(2.0),
                &mut text,
            );
            assert!(
                large.width() > normal.width(),
                "renderer scale must refresh retained width: {normal:?} -> {large:?}"
            );
            I18nManager::set_locale("de");
            assert!(host.is_layout_dirty());
            let localized = layout_fit_host(
                host.as_mut(),
                taffy::AlignItems::Stretch,
                &Theme::dark().with_text_scale(2.0),
                &mut text,
            );
            assert!(
                localized.width() > large.width(),
                "locale must refresh retained width: {large:?} -> {localized:?}"
            );
            assert_eq!(localized.height(), 40.0);
        }

        fn content_sized_minimum_height_keeps_targets_and_grows_for_large_text() {
            let ordinary = measured_button_rect(
                Button::new("ordinary", "Restart", None).content_sized(),
                1.0,
            );
            let minimum = measured_button_rect(
                Button::new("minimum", "Restart", None)
                    .content_sized()
                    .with_min_height(44.0),
                1.0,
            );
            assert_eq!(ordinary.height(), 40.0, "default sizing stays unchanged");
            assert_eq!(
                minimum.height(),
                44.0,
                "explicit floor preserves the target"
            );

            let label = "Restart the computer";
            let theme = Theme::dark().with_text_scale(4.0);
            let harness = crate::headless::FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(
                    crate::widget::Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .push(Box::new(
                            Button::new("restart", label, None)
                                .with_icon(crate::icons::Icon::Refresh)
                                .content_sized()
                                .with_min_height(44.0),
                        )),
                ),
                (220, 1000),
                theme.clone(),
            );
            let rect = harness.inspection().root.children[0].rect;
            let mut text = TextRenderer::new();
            text.set_text_scale(4.0);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "bounded logical test viewport"
            )]
            let available = rect[2] as f32 - theme.padding_standard * 2.0 - 28.0;
            let (_, label_height) =
                text.measure_style(label, Some(available), &theme.text(TextRole::Label));
            assert!(label_height > 60.0, "the real scaled label must wrap");
            assert!(rect[2] <= 220.0 && rect[3] > 44.0);
            assert!(
                rect[3] >= f64::from(label_height + theme.padding_standard),
                "minimum must not cap intrinsic wrapped height: {rect:?}, text={label_height}"
            );
        }

        fn invalid_minimum_heights_leave_finite_usable_targets() {
            for minimum in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -20.0] {
                let rect = measured_button_rect(
                    Button::new("minimum", "Restart", None)
                        .content_sized()
                        .with_min_height(minimum),
                    1.0,
                );
                assert!(
                    rect.height().is_finite() && rect.height() >= 40.0,
                    "invalid floor must not poison layout: {minimum:?}, {rect:?}"
                );
            }
        }

        fn content_sized_wraps_long_scaled_actions_inside_available_width() {
            let label = "Refresh all remote connections and discover updates";
            let harness = crate::headless::FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(
                    crate::widget::Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .push(Box::new(
                            Button::new("refresh", label, None)
                                .with_icon(crate::icons::Icon::Search)
                                .content_sized(),
                        )),
                ),
                (220, 400),
                Theme::dark().with_text_scale(2.0),
            );
            let rect = harness.inspection().root.children[0].rect;
            assert!(rect[2] <= 220.0);
            assert!(
                rect[3] > 70.0,
                "wrapped action must grow in height: {rect:?}"
            );
        }

        fn intrinsic_width_rounding_does_not_wrap_a_fitting_single_line() {
            let mut text = TextRenderer::new();
            for label in ["Install workspace", "Refresh connections", "View details"] {
                let rect =
                    measured_button_rect(Button::new("action", label, None).content_sized(), 1.0);
                let (width, _) =
                    text.measure_style(label, None, &Theme::dark().text(TextRole::Label));
                assert!(
                    rect.width() >= f64::from(width + Theme::dark().padding_standard * 2.0),
                    "rounded width clips or wraps {label}: {rect:?}, text {width}"
                );
            }
        }

        retained_fit_width_remeasures_scale_and_locale_after_rebuild();
        content_sized_minimum_height_keeps_targets_and_grows_for_large_text();
        invalid_minimum_heights_leave_finite_usable_targets();
        content_sized_wraps_long_scaled_actions_inside_available_width();
        intrinsic_width_rounding_does_not_wrap_a_fitting_single_line();
    }

    fn measured_button_rect(button: Button, scale: f32) -> Rect {
        let harness = crate::headless::FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(crate::widget::Row::new().push(Box::new(button))),
            (1000, 200),
            Theme::dark().with_text_scale(scale),
        );
        let rect = harness.inspection().root.children[0].rect;
        Rect::new(rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3])
    }

    #[test]
    fn label_face_geometry_follows_the_theme_scale() {
        fn label_line_box_is_centered_at_scaled_size() {
            let mut text = TextRenderer::new();
            text.set_text_scale(2.0);
            let mut scene = Scene::new();
            let rect = Rect::new(0.0, 0.0, 200.0, 40.0);
            let (_, height) = text.measure("Refresh", 14.0, None);
            ButtonLabel {
                icon: None,
                text: "Refresh",
                style: TextStyle::new(14.0),
                wrap_width: None,
            }
            .paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &Theme::dark()),
                rect,
                vello::peniko::Color::WHITE,
            );
            let actual = &scene.encoding().resources.glyph_runs[0];
            let mut expected = Scene::new();
            text.draw_text_weighted(
                &mut expected,
                "Refresh",
                14.0,
                vello::peniko::Color::WHITE,
                Affine::translate((0.0, (40.0 - f64::from(height)) / 2.0)),
                None,
                crate::text_render::TextWeight::Regular,
            );
            let reference = &expected.encoding().resources.glyph_runs[0];
            assert_eq!(
                actual.transform.translation[1],
                reference.transform.translation[1]
            );
        }

        fn wrapped_retained_and_immediate_labels_share_geometry() {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut text = TextRenderer::new();
            text.set_text_scale(2.0);
            let label = "Refresh all remote connections";
            let rect = Rect::new(10.0, 20.0, 230.0, 180.0);
            let mut button = Button::new("action", label, None)
                .content_sized()
                .with_icon(crate::icons::Icon::Refresh);
            let mut tree = TaffyTree::new();
            button.build_layout(&mut tree, &theme, &mut std::collections::HashMap::new());
            button.apply_layout_external(rect);
            let mut retained = Scene::new();
            button.draw(
                &mut retained,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &crate::presence::PresenceCtx::default(),
            );
            let mut immediate = Scene::new();
            Button::paint(
                &mut immediate,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &ButtonFace::new(rect, label)
                    .icon(crate::icons::Icon::Refresh)
                    .wrap_label(),
            );
            let a = &retained.encoding().resources.glyph_runs;
            let b = &immediate.encoding().resources.glyph_runs;
            assert!(!a.is_empty());
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(b) {
                assert_eq!(a.transform, b.transform);
                assert_eq!(a.font_size, b.font_size);
            }
        }

        /// A default `ButtonFace` reads its corner from the theme and sets its
        /// label in the Regular size's role; Large sets it in Body.
        fn default_face_reads_its_corner_and_label_role_from_the_theme() {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let rect = Rect::new(10.0, 10.0, 130.0, 50.0);
            let mut paint = |face: ButtonFace<'_>| {
                let mut scene = Scene::new();
                Button::paint(&mut scene, Affine::IDENTITY, &mut text, &theme, &face);
                scene
            };
            let themed = paint(ButtonFace::new(rect, "Save").icon(crate::icons::Icon::Refresh));
            let explicit = paint(
                ButtonFace::new(rect, "Save")
                    .icon(crate::icons::Icon::Refresh)
                    .radius(8.0)
                    .control_size(ControlSize::Regular),
            );
            assert_eq!(themed.encoding().path_data, explicit.encoding().path_data);
            let a = &themed.encoding().resources.glyph_runs;
            let b = &explicit.encoding().resources.glyph_runs;
            assert_eq!(a.len(), 1);
            assert_eq!(a[0].transform, b[0].transform);
            assert_eq!(a[0].font_size, b[0].font_size);
            assert!((a[0].font_size - theme.text(TextRole::Label).size).abs() < 1e-6);
            let large = paint(ButtonFace::new(rect, "Save").control_size(ControlSize::Large));
            assert!(
                (large.encoding().resources.glyph_runs[0].font_size
                    - theme.text(TextRole::Body).size)
                    .abs()
                    < 1e-6
            );
        }

        /// The retained and immediate labels follow the theme's type scale —
        /// 2× the Label role under a 2× theme — whether the shared renderer is
        /// matched (2×, the live App and `FrameHarness`) or left at 1× (the
        /// apps' raw `--shots` loop), and whether or not a `build_layout` pass
        /// preceded the draw.
        fn retained_label_follows_the_theme_scale_in_matched_and_unmatched_hosts() {
            let theme = Theme::dark().with_text_scale(2.0);
            let label = theme.text(TextRole::Label).size;
            let rect = Rect::new(0.0, 0.0, 160.0, 40.0);
            let presence = crate::presence::PresenceCtx::default();
            let font_size = |scene: &Scene| scene.encoding().resources.glyph_runs[0].font_size;
            for renderer_scale in [1.0_f32, 2.0] {
                let mut text = TextRenderer::new();
                text.set_text_scale(renderer_scale);

                let mut laid_out = Button::new("save", "Save", None);
                laid_out.build_layout(
                    &mut TaffyTree::new(),
                    &theme,
                    &mut std::collections::HashMap::new(),
                );
                laid_out.apply_layout_external(rect);
                let mut retained = Scene::new();
                laid_out.draw(
                    &mut retained,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &presence,
                );
                assert!(
                    (font_size(&retained) - label).abs() < 1e-3,
                    "retained label at renderer {renderer_scale}×: {}",
                    font_size(&retained)
                );

                // Positioned by a canvas host: no taffy pass at all.
                let mut external = Button::new("save", "Save", None);
                external.apply_layout_external(rect);
                let mut external_scene = Scene::new();
                external.draw(
                    &mut external_scene,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &presence,
                );
                assert!(
                    (font_size(&external_scene) - label).abs() < 1e-3,
                    "externally laid-out label at renderer {renderer_scale}×: {}",
                    font_size(&external_scene)
                );

                let mut immediate = Scene::new();
                Button::paint(
                    &mut immediate,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &ButtonFace::new(rect, "Save"),
                );
                assert!(
                    (font_size(&immediate) - label).abs() < 1e-3,
                    "ButtonFace default at renderer {renderer_scale}×: {} (expected {label})",
                    font_size(&immediate)
                );
            }
        }

        /// A theme text-scale change between frames re-emits the cached face at
        /// the new label size instead of replaying the old glyphs.
        fn theme_text_scale_change_invalidates_the_cached_face() {
            let rect = Rect::new(0.0, 0.0, 160.0, 40.0);
            let mut text = TextRenderer::new();
            let mut button = Button::new("save", "Save", None);
            let presence = crate::presence::PresenceCtx::default();
            let one = Theme::dark();
            button.build_layout(
                &mut TaffyTree::new(),
                &one,
                &mut std::collections::HashMap::new(),
            );
            button.apply_layout_external(rect);
            let mut first = Scene::new();
            button.draw(&mut first, Affine::IDENTITY, &mut text, &one, &presence);
            button.mark_drawn();
            assert!(!button.needs_redraw(), "settled at 1×");

            let larger = Theme::dark().with_text_scale(1.5);
            button.build_layout(
                &mut TaffyTree::new(),
                &larger,
                &mut std::collections::HashMap::new(),
            );
            assert!(
                button.needs_redraw(),
                "a new label size must dirty the cache"
            );
            let mut second = Scene::new();
            button.draw(&mut second, Affine::IDENTITY, &mut text, &larger, &presence);
            let before = first.encoding().resources.glyph_runs[0].font_size;
            let after = second.encoding().resources.glyph_runs[0].font_size;
            assert!((before - 13.0).abs() < 1e-3, "{before}");
            assert!((after - 19.5).abs() < 1e-3, "{after}");
        }

        label_line_box_is_centered_at_scaled_size();
        wrapped_retained_and_immediate_labels_share_geometry();
        default_face_reads_its_corner_and_label_role_from_the_theme();
        retained_label_follows_the_theme_scale_in_matched_and_unmatched_hosts();
        theme_text_scale_change_invalidates_the_cached_face();
    }

    #[test]
    fn manual_size_modifiers_own_dimensions_over_content_sizing() {
        fn manual_modifiers_replace_fully_content_sized_mode() {
            let rect = measured_button_rect(
                Button::new("action", "A long scaled label", None)
                    .content_sized()
                    .with_size(81.0, 35.0),
                2.0,
            );
            assert_eq!(rect.size(), vello::kurbo::Size::new(81.0, 35.0));
            let rect = measured_button_rect(
                Button::new("action", "Short", None)
                    .content_sized()
                    .fit_content(36.0),
                2.0,
            );
            assert_eq!(rect.height(), 36.0);
        }

        fn fit_content_contains_real_scaled_label_and_icon() {
            let label = "WWW Refresh remote connections";
            let mut text = TextRenderer::new();
            text.set_text_scale(2.0);
            let (width, _) = text.measure(label, 14.0, None);
            let rect = measured_button_rect(
                Button::new("refresh", label, None)
                    .with_icon(crate::icons::Icon::Search)
                    .fit_content(40.0),
                2.0,
            );
            assert!(
                rect.width() >= f64::from(width) + 22.0 + 6.0,
                "measured label and icon escape button: {rect:?}, text width {width}"
            );
            assert_eq!(rect.height(), 40.0, "fit_content keeps explicit height");
        }

        fn manual_size_after_fit_content_owns_both_dimensions() {
            let rect = measured_button_rect(
                Button::new("manual", "Long label intentionally clipped", None)
                    .fit_content(40.0)
                    .with_size(81.0, 35.0),
                2.0,
            );
            assert_eq!(rect.size(), vello::kurbo::Size::new(81.0, 35.0));
        }

        /// Explicit sizes are the caller's decision: neither a scaled theme nor a
        /// wide label grows a `with_size` button.
        fn an_explicit_size_is_never_grown_by_the_measured_footprint() {
            let rect = measured_button_rect(
                Button::new("remove", "Remove every remote connection", None)
                    .with_size(120.0, 40.0),
                2.0,
            );
            assert_eq!((rect.width(), rect.height()), (120.0, 40.0), "{rect:?}");
        }

        manual_modifiers_replace_fully_content_sized_mode();
        fit_content_contains_real_scaled_label_and_icon();
        manual_size_after_fit_content_owns_both_dimensions();
        an_explicit_size_is_never_grown_by_the_measured_footprint();
    }

    #[test]
    fn action_mode_and_availability_gate_mutation_and_callbacks() {
        fn button_test_id_is_stable_constructor_id() {
            let b = Button::new("id", "Save", None);
            assert_eq!(Widget::test_id(&b), Some("id"));
            assert_eq!(
                Widget::accessibility(&b).unwrap().name.as_deref(),
                Some("Save")
            );
        }

        fn action_mode_suppresses_raw_mutation_and_callback() {
            let resource = tilo_actions::Resource::new("doc:1", "doc", "Document").with_action(
                tilo_actions::ActionSpec::operation(
                    "save",
                    "Save",
                    tilo_actions::OperationKind::Effect,
                    "test.save",
                    &(),
                )
                .unwrap(),
            );
            let binding = tilo_actions::ActionBinding::unit_for(&resource, "save").unwrap();
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
            let intent =
                crate::widget::controls::take_control_invocation(&mut button).expect("one intent");
            assert_eq!(
                intent.address,
                tilo_actions::ActionAddress::new("doc:1", "save")
            );
            assert!(intent.arguments.is_empty());
            assert_eq!(
                crate::widget::controls::control_action_bindings(&button).len(),
                1
            );
        }

        fn action_availability_revokes_and_restores_without_overriding_authored_state() {
            let resource = tilo_actions::Resource::new("doc:1", "doc", "Document").with_action(
                tilo_actions::ActionSpec::operation(
                    "save",
                    "Save",
                    tilo_actions::OperationKind::Effect,
                    "test.save",
                    &(),
                )
                .unwrap(),
            );
            let binding = tilo_actions::ActionBinding::unit_for(&resource, "save").unwrap();
            let mut button = Button::new("save", "Save", None).on_action(binding);
            button.activate(&mut AutoCommit::new(), &automerge::ROOT);
            crate::widget::controls::refresh_control_action_availability(
                &mut button,
                &std::collections::HashSet::new(),
            );
            assert!(!Widget::is_enabled(&button));
            assert!(
                crate::widget::controls::take_control_invocation(&mut button).is_none(),
                "revocation drops queued authority"
            );
            crate::widget::controls::refresh_control_action_availability(
                &mut button,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "doc:1", "save",
                )]),
            );
            assert!(Widget::is_enabled(&button));

            button.set_enabled(false);
            crate::widget::controls::refresh_control_action_availability(
                &mut button,
                &std::collections::HashSet::new(),
            );
            crate::widget::controls::refresh_control_action_availability(
                &mut button,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "doc:1", "save",
                )]),
            );
            assert!(
                !Widget::is_enabled(&button),
                "permission restore cannot override app-authored disabled state"
            );
        }

        fn legacy_button_paths_are_explicit_in_inventory() {
            let button = Button::new("legacy", "Legacy", Some("flag")).on_click(|| {});
            let bindings = crate::widget::controls::control_action_bindings(&button);
            assert!(bindings.iter().any(|entry| matches!(
                entry.kind,
                crate::widget::ControlBindingKind::CollaborativeDocument(_)
            )));
            assert!(bindings.iter().any(|entry| matches!(
                entry.kind,
                crate::widget::ControlBindingKind::LegacyCallback
            )));
        }

        button_test_id_is_stable_constructor_id();
        action_mode_suppresses_raw_mutation_and_callback();
        action_availability_revokes_and_restores_without_overriding_authored_state();
        legacy_button_paths_are_explicit_in_inventory();
    }

    #[test]
    fn face_colors_per_style_track_state_and_stay_readable() {
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
                Some(crate::surface::edge_on(theme.tokens.colors.surfaces.raised)),
                "neutral shows an edge derived from its rest surface"
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

        fn face_colors_primary_is_accent_borderless_and_readable() {
            use crate::a11y::contrast_ratio;
            let theme = Theme::dark();
            let rest = ButtonInteract::default();
            let hover = ButtonInteract {
                hovered: true,
                ..Default::default()
            };
            let (bg, border, fg) = face_colors(&theme, ButtonStyle::Primary, rest);
            assert_eq!(
                bg, theme.tokens.colors.intent.inverted,
                "primary rest = accent fill"
            );
            assert_eq!(border, None, "accent fill is self-defining — no border");
            let r = contrast_ratio(fg, bg);
            assert!(
                r >= ON_FILL_MIN_CONTRAST,
                "primary label {fg:?} on {bg:?} is {r:.2}:1, under the {ON_FILL_MIN_CONTRAST}:1 floor",
            );
            let (hbg, _, hfg) = face_colors(&theme, ButtonStyle::Primary, hover);
            assert_eq!(
                hbg, theme.tokens.colors.intent.inverted_hover,
                "primary hover darkens to the accent-hover token"
            );
            assert_eq!(
                hfg, fg,
                "the label must not change colour under the pointer"
            );
        }

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

        /// Transparent at rest with the danger ink as both outline and label;
        /// under the pointer the theme's opaque hover/active fills, so the ink
        /// can be measured against every ground it sits on.
        fn face_colors_danger_outline_is_transparent_at_rest_and_outlined_in_its_ink() {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let rest = face_colors(
                    &theme,
                    ButtonStyle::DangerOutline,
                    ButtonInteract::default(),
                );
                assert!(
                    rest.0.components[3] <= 0.0,
                    "{}: rest fill is transparent",
                    theme.name
                );
                assert_eq!(
                    rest.1,
                    Some(rest.2),
                    "{}: outline is the label ink",
                    theme.name
                );
                let ratio = crate::a11y::contrast_ratio(rest.2, theme.tokens.colors.surfaces.base);
                assert!(
                    ratio >= ON_FILL_MIN_CONTRAST,
                    "{}: danger ink {ratio:.2}:1 on the base surface",
                    theme.name
                );
                let hovered = face_colors(
                    &theme,
                    ButtonStyle::DangerOutline,
                    ButtonInteract {
                        hovered: true,
                        ..ButtonInteract::default()
                    },
                );
                assert_eq!(
                    hovered.0, theme.bg_hover,
                    "{}: hover tints the fill",
                    theme.name
                );
                assert_eq!(
                    hovered.2, rest.2,
                    "{}: the ink does not change under the pointer",
                    theme.name
                );
            }
        }

        face_colors_neutral_tracks_hover_and_press();
        face_colors_ghost_is_transparent_at_rest_then_tints();
        face_colors_primary_is_accent_borderless_and_readable();
        face_colors_danger_is_solid_at_rest_borderless_and_readable();
        face_colors_danger_outline_is_transparent_at_rest_and_outlined_in_its_ink();
    }

    #[test]
    fn disabled_faces_are_neutral_and_reject_input() {
        fn disabled_faces_are_neutral_bordered_and_readable_in_every_theme() {
            use crate::a11y::contrast_ratio;

            let disabled = ButtonInteract {
                hovered: true,
                pressed: true,
                focused: true,
                disabled: true,
            };
            for name in Theme::ALL {
                let theme = Theme::named(name).expect("shipped theme resolves");
                for style in [
                    ButtonStyle::Primary,
                    ButtonStyle::Neutral,
                    ButtonStyle::Danger,
                    ButtonStyle::Ghost,
                ] {
                    let (bg, border, fg) = face_colors(&theme, style, disabled);
                    assert_eq!(
                        bg, theme.tokens.colors.surfaces.raised,
                        "{name} {style:?}: disabled face uses the neutral raised surface"
                    );
                    assert_eq!(
                        border,
                        Some(crate::surface::edge_on(theme.tokens.colors.surfaces.raised)),
                        "{name} {style:?}: disabled face keeps the derived rest edge, no semantic fill"
                    );
                    let ratio = contrast_ratio(fg, bg);
                    assert!(
                        ratio >= ON_FILL_MIN_CONTRAST,
                        "{name} {style:?}: disabled label is only {ratio:.2}:1 on {bg:?}"
                    );
                }
            }
        }

        /// Writes a small disabled-button matrix for human visual review. The
        /// controls intentionally carry hover/focus input too: disabled state
        /// must win over stale pointer/focus state and never recover a semantic
        /// action colour or focus ring.
        #[ignore = "developer visual audit writes /tmp/tilo-button-disabled-audit"]
        fn disabled_button_visual_audit() {
            use std::path::Path;
            use vello::peniko::Color;

            let output = Path::new("/tmp/tilo-button-disabled-audit");
            std::fs::create_dir_all(output).expect("visual-audit output directory");
            for (name, theme) in [
                ("light", Theme::light()),
                ("dark", Theme::dark()),
                ("high-contrast", Theme::high_contrast()),
            ] {
                let mut scene = Scene::new();
                let mut text = TextRenderer::new();
                let faces = [
                    (ButtonStyle::Primary, "Primary", None),
                    (ButtonStyle::Neutral, "Secondary", None),
                    (ButtonStyle::Danger, "Destructive", None),
                    (ButtonStyle::Primary, "", Some(crate::icons::Icon::Check)),
                ];
                for (index, (style, label, icon)) in faces.into_iter().enumerate() {
                    let y = 88.0 + f64::from(u32::try_from(index).expect("four audit rows")) * 86.0;
                    let mut face = ButtonFace::new(Rect::new(80.0, y, 360.0, y + 52.0), label)
                        .style(style)
                        .disabled(true)
                        .hovered(true)
                        .focused(true);
                    if let Some(icon) = icon {
                        face = face.icon(icon).accessible_name("Confirm");
                    }
                    Button::paint(&mut scene, Affine::IDENTITY, &mut text, &theme, &face);
                }
                let mut renderer = crate::offscreen::OffscreenRenderer::new(440, 480);
                renderer.render_png(
                    &scene,
                    Color::from_rgba8(theme.bg_app.r(), theme.bg_app.g(), theme.bg_app.b(), 255),
                    output.join(format!("{name}.png")),
                );
            }
        }

        fn with_enabled_false_is_reported_by_is_enabled() {
            let b = Button::new("id", "Go", None).with_enabled(false);
            assert!(!Widget::is_enabled(&b));
            let b2 = Button::new("id", "Go", None);
            assert!(Widget::is_enabled(&b2), "default is enabled");
        }

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
                    &mut crate::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &mods,
                        state: &mut doc,
                        path: &root,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &tree,
                    },
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

        disabled_faces_are_neutral_bordered_and_readable_in_every_theme();
        disabled_button_visual_audit();
        with_enabled_false_is_reported_by_is_enabled();
        disabled_button_rejects_input();
    }

    #[test]
    fn button_face_builder_paints_fill_label_and_icon_only_faces() {
        fn button_face_builder_sets_style_and_state() {
            let f = ButtonFace::new(Rect::new(0.0, 0.0, 80.0, 32.0), "Send")
                .primary()
                .hovered(true)
                .control_size(ControlSize::Small);
            let theme = Theme::dark();
            let (bg, border, fg) = f.colors(&theme);
            assert_eq!(bg, theme.tokens.colors.intent.inverted_hover);
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

        fn icon_only_face_spoken_name_falls_back_to_accessible_name() {
            let f = ButtonFace::new(Rect::new(0.0, 0.0, 32.0, 32.0), "")
                .icon(crate::icons::Icon::Plus)
                .accessible_name("Add page");
            assert_eq!(f.spoken_name(), "Add page");
        }

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

        /// An icon-only face has an empty label key. Under the expanded
        /// pseudo-locale `i18n!("")` came back as `⟦⟧`, painting a bracket pair
        /// beside the glyph; the resolved label must stay empty.
        fn icon_only_face_paints_no_label_under_the_pseudo_locale() {
            use crate::i18n::{I18nManager, PseudoLocale};
            I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let rect = Rect::new(0.0, 0.0, 40.0, 40.0);

            let mut icon_only = Scene::new();
            Button::paint(
                &mut icon_only,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &ButtonFace::new(rect, "").icon(crate::icons::Icon::X),
            );
            let icon_only_runs = glyph_origins(&icon_only).len();

            let mut labelled = Scene::new();
            Button::paint(
                &mut labelled,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &ButtonFace::new(rect, "Go").icon(crate::icons::Icon::X),
            );
            let labelled_runs = glyph_origins(&labelled).len();
            I18nManager::set_pseudo_locale(PseudoLocale::Off);

            assert_eq!(icon_only_runs, 0, "an icon-only face paints no glyph run");
            assert!(
                labelled_runs > 0,
                "the labelled control still paints its text"
            );
        }

        /// The same defect in the accessibility tree: `⟦⟧` is not empty, so the
        /// icon-only button lost its tooltip fallback and was spoken as brackets.
        fn icon_only_button_speaks_its_tooltip_under_the_pseudo_locale() {
            use crate::i18n::{I18nManager, PseudoLocale};
            I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
            let button = Button::new("close", "", None)
                .with_icon(crate::icons::Icon::X)
                .tooltip("Close");
            let name = Widget::accessibility(&button).and_then(|node| node.name);
            I18nManager::set_pseudo_locale(PseudoLocale::Off);
            let name = name.expect("icon-only button with a tooltip has a spoken name");
            assert_ne!(
                name, "⟦⟧",
                "the spoken name must not be the empty pseudo-locale markers"
            );
            assert!(
                name.contains('C') && name.contains('s'),
                "the tooltip is what gets spoken, got {name:?}"
            );
        }

        /// A fixed face narrower than its label (a long German label in a
        /// half-width footer button) elides inside the face instead of
        /// painting across both edges.
        fn a_label_wider_than_its_face_stays_inside_it() {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let rect = Rect::new(100.0, 0.0, 220.0, 40.0);
            let label = "Standard wiederherstellen";
            let (natural, _) = text.measure_style(label, None, &theme.text(TextRole::Body));
            assert!(
                f64::from(natural) > rect.width(),
                "the fixture must overflow"
            );
            let mut scene = Scene::new();
            Button::paint(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &ButtonFace::new(rect, label).control_size(ControlSize::Large),
            );
            let origins = glyph_origins(&scene);
            assert!(!origins.is_empty(), "the elided label still paints");
            let inset = f64::from(theme.tokens.spacing.s2);
            for [x, _] in origins {
                assert!(
                    f64::from(x) >= rect.x0 + inset,
                    "label starts inside the face: {x}"
                );
            }
        }

        button_face_builder_sets_style_and_state();
        icon_only_face_spoken_name_falls_back_to_accessible_name();
        paint_emits_fill_and_label_geometry();
        a_label_wider_than_its_face_stays_inside_it();
        icon_only_face_paints_no_label_under_the_pseudo_locale();
        icon_only_button_speaks_its_tooltip_under_the_pseudo_locale();
    }

    /// Glyph-run origins painted into `scene`, in paint order.
    fn glyph_origins(scene: &Scene) -> Vec<[f32; 2]> {
        scene
            .encoding()
            .resources
            .glyph_runs
            .iter()
            .map(|run| run.transform.translation)
            .collect()
    }

    #[test]
    fn default_footprint_is_120_by_40_and_grows_with_the_measured_label() {
        /// The shipped default footprint is untouched at 1×: a base-size label
        /// shapes a 17.5 px line and "Save" is far narrower than the face, so the
        /// measured floor resolves to exactly `DEFAULT_SIZE`.
        fn default_footprint_is_exactly_the_shipped_120_by_40_at_one_x() {
            let rect = measured_button_rect(Button::new("save", "Save", None), 1.0);
            assert_eq!(
                (rect.width(), rect.height()),
                (120.0, 40.0),
                "default button at 1×: {rect:?}"
            );
        }

        /// At 2× text the label line is 35 px; a 40 px face would leave it 2.5 px
        /// of slack. The default footprint grows from the measured line plus the
        /// content padding — the `content_sized` rule — so both auto-height
        /// modes agree, and the width holds at 120 for a label that fits.
        fn default_footprint_grows_from_the_measured_line_at_two_x() {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut text = TextRenderer::new();
            text.set_text_scale(2.0);
            let (label_w, line_h) = text.measure_style("Save", None, &theme.text(TextRole::Label));
            assert!((line_h - 33.8).abs() < 1e-3, "2× label line: {line_h}");
            let expected_height = (line_h + theme.padding_standard).ceil();
            assert!(expected_height > 40.0);
            assert!(
                label_w + FIT_SIDE_PADDING * 2.0 < 120.0,
                "fixture label must fit 120"
            );

            let rect = measured_button_rect(Button::new("save", "Save", None), 2.0);
            assert!(
                (rect.height() - f64::from(expected_height)).abs() < 1e-6,
                "2× default height {} should be the label line + padding = {expected_height}",
                rect.height()
            );
            assert!(
                (rect.width() - 120.0).abs() < 1e-6,
                "a label that fits keeps the 120 px default, got {}",
                rect.width()
            );
        }

        /// A label wider than the face used to paint outside it; the default
        /// footprint now widens to hold it (label + the fit-content side
        /// allowance) — the only case in which a default button leaves 120 at 1×.
        fn a_wide_label_widens_the_default_button_instead_of_overflowing_it() {
            let label = "Remove every remote connection";
            let mut text = TextRenderer::new();
            let (label_w, _) =
                text.measure_style(label, None, &Theme::dark().text(TextRole::Label));
            let expected = (label_w + FIT_SIDE_PADDING * 2.0).ceil();
            assert!(
                expected > 120.0,
                "fixture label must exceed the default face"
            );

            let rect = measured_button_rect(Button::new("remove", label, None), 1.0);
            assert!(
                (rect.width() - f64::from(expected)).abs() < 1e-6,
                "width {} should be the measured label + 2×{FIT_SIDE_PADDING} = {expected}",
                rect.width()
            );
            assert_eq!(rect.height(), 40.0, "the height stays at the shipped 40");
        }

        /// Hover, press, focus and disabled recolour the face; none of them may
        /// move it. The focus ring is stroked on the outline, not inset, so the
        /// label origin is identical in every state.
        fn interaction_states_do_not_move_the_face_or_its_label() {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let rect = Rect::new(10.0, 10.0, 130.0, 50.0);
            let states = [
                ("rest", ButtonInteract::default()),
                (
                    "hovered",
                    ButtonInteract {
                        hovered: true,
                        ..ButtonInteract::default()
                    },
                ),
                (
                    "pressed",
                    ButtonInteract {
                        pressed: true,
                        ..ButtonInteract::default()
                    },
                ),
                (
                    "focused",
                    ButtonInteract {
                        focused: true,
                        ..ButtonInteract::default()
                    },
                ),
                (
                    "disabled",
                    ButtonInteract {
                        disabled: true,
                        ..ButtonInteract::default()
                    },
                ),
            ];
            for style in [
                ButtonStyle::Neutral,
                ButtonStyle::Primary,
                ButtonStyle::Danger,
                ButtonStyle::Ghost,
                ButtonStyle::DangerOutline,
            ] {
                let mut reference: Option<[f32; 2]> = None;
                for (name, interact) in states {
                    let mut scene = Scene::new();
                    let face = ButtonFace::new(rect, "Delete")
                        .style(style)
                        .hovered(interact.hovered)
                        .pressed(interact.pressed)
                        .focused(interact.focused)
                        .disabled(interact.disabled);
                    Button::paint(&mut scene, Affine::IDENTITY, &mut text, &theme, &face);
                    let origins = glyph_origins(&scene);
                    let origin = *origins.first().expect("label painted");
                    match reference {
                        None => reference = Some(origin),
                        Some(expected) => {
                            assert_eq!(origin, expected, "{style:?}/{name}: the label moved")
                        }
                    }
                }
            }
        }

        default_footprint_is_exactly_the_shipped_120_by_40_at_one_x();
        default_footprint_grows_from_the_measured_line_at_two_x();
        a_wide_label_widens_the_default_button_instead_of_overflowing_it();
        interaction_states_do_not_move_the_face_or_its_label();
    }

    #[test]
    fn pointer_click_fires_callback_and_writes_click_value() {
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
                    &mut crate::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &mods,
                        state: &mut doc,
                        path: &root,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &tree,
                    },
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

        fn click_value_writes_a_string_command() {
            let mut button = Button::new("id", "Apply", Some("command"))
                .with_click_value("apply")
                .fit_content(34.0);
            let mut doc = AutoCommit::new();
            button.activate(&mut doc, &automerge::ROOT);

            let (value, _) = doc.get(&automerge::ROOT, "command").unwrap().unwrap();
            assert_eq!(value.as_str(), Some("apply"));
        }

        fn semantic_activation_uses_the_same_click_value_contract() {
            let mut button = Button::new("id", "Apply", Some("command")).with_click_value("apply");
            let mut doc = AutoCommit::new();

            assert!(button.perform_action(
                &crate::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));

            let (value, _) = doc.get(&automerge::ROOT, "command").unwrap().unwrap();
            assert_eq!(value.as_str(), Some("apply"));
        }

        cursor_shape_is_pointer_over_button();
        on_click_callback_fires_on_release_over_button();
        click_value_writes_a_string_command();
        semantic_activation_uses_the_same_click_value_contract();
    }

    #[test]
    fn accessibility_reports_role_name_state_and_icon_only_naming() {
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

        fn accessibility_reflects_disabled_and_focus() {
            let mut b = Button::new("id", "Save", None).with_enabled(false);
            b.set_focus(true);
            let node = Widget::accessibility(&b).unwrap();
            assert!(node.state.disabled);
            assert!(node.state.focused);
        }

        fn accessibility_icon_only_button_without_name_is_silent() {
            let b = Button::new("id", "", None).with_icon(crate::icons::Icon::Check);
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(node.name, None);
        }

        fn accessibility_icon_only_button_uses_accessible_name() {
            let b = Button::new("id", "", None)
                .with_icon(crate::icons::Icon::Check)
                .with_accessible_name("Confirm");
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(node.name.as_deref(), Some("Confirm"));
        }

        fn accessibility_visible_label_wins_over_accessible_name() {
            let b = Button::new("id", "Save", None).with_accessible_name("Persist");
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(node.name.as_deref(), Some("Save"));
        }

        accessibility_reports_button_role_name_and_state();
        accessibility_reflects_disabled_and_focus();
        accessibility_icon_only_button_without_name_is_silent();
        accessibility_icon_only_button_uses_accessible_name();
        accessibility_visible_label_wins_over_accessible_name();
    }

    #[test]
    fn tooltip_naming_precedence_and_compact_toolbar_semantics() {
        fn tooltip_is_exposed_to_the_host() {
            let b = Button::new("id", "", None)
                .with_icon(crate::icons::Icon::Search)
                .tooltip("Search");
            assert_eq!(Widget::tooltip(&b), Some("Search"));
            let plain = Button::new("id", "Save", None);
            assert_eq!(Widget::tooltip(&plain), None, "no tooltip unless attached");
        }

        fn compact_toolbar_presentation_round_trips_authored_semantics() {
            let mut button = Button::new("publish", "Publish", None)
                .with_icon(crate::icons::Icon::CloudDownload)
                .with_accessible_name("Publish document")
                .tooltip("Publish this document now");

            button.set_compact_toolbar_presentation(true);
            assert!(button.label.is_empty(), "only the icon should be painted");
            assert_eq!(
                Widget::accessibility(&button).unwrap().name.as_deref(),
                Some("Publish document"),
                "compaction never makes an icon-only control silent"
            );
            assert_eq!(
                Widget::tooltip(&button),
                Some("Publish this document now"),
                "the authored tooltip survives compaction"
            );

            button.set_compact_toolbar_presentation(false);
            assert_eq!(button.label, "Publish");
            assert_eq!(
                button.accessible_name.as_deref(),
                Some("Publish document"),
                "expansion restores the authored accessible name rather than retaining a fallback"
            );
            assert_eq!(
                Widget::accessibility(&button).unwrap().name.as_deref(),
                Some("Publish"),
                "the visible label again takes precedence after expansion"
            );
            assert_eq!(Widget::tooltip(&button), Some("Publish this document now"));
        }

        fn icon_only_tooltip_doubles_as_accessible_name() {
            let b = Button::new("id", "", None)
                .with_icon(crate::icons::Icon::Search)
                .tooltip("Search");
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(node.name.as_deref(), Some("Search"));
        }

        fn explicit_accessible_name_wins_over_tooltip() {
            let b = Button::new("id", "", None)
                .with_icon(crate::icons::Icon::MoreHorizontal)
                .with_accessible_name("View options")
                .tooltip("More");
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(node.name.as_deref(), Some("View options"));
        }

        fn icon_only_effect_publishes_its_spoken_name() {
            let doc = AutoCommit::new();
            let named = Button::new("private", "", None)
                .with_icon(crate::icons::Icon::Shield)
                .with_accessible_name("New Private Window")
                .on_effect(|| {});
            let tooltip_after = Button::new("search", "", None)
                .with_icon(crate::icons::Icon::Search)
                .on_effect(|| {})
                .tooltip("Search");
            for (button, name) in [(named, "New Private Window"), (tooltip_after, "Search")] {
                let resources = crate::resources::resources(&button, &doc);
                assert_eq!(resources.len(), 1);
                assert_eq!(resources[0].label, name);
                assert!(
                    resources[0]
                        .actions
                        .iter()
                        .all(|action| action.label == name)
                );
            }
        }

        tooltip_is_exposed_to_the_host();
        compact_toolbar_presentation_round_trips_authored_semantics();
        icon_only_tooltip_doubles_as_accessible_name();
        explicit_accessible_name_wins_over_tooltip();
        icon_only_effect_publishes_its_spoken_name();
    }
}

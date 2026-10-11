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
use automerge::{AutoCommit, ObjId};
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
    /// The press this button took, carried to a rebuilt button in its place.
    press: crate::pointer_replay::HeldPress,
    is_focused: bool,
    /// Focus came from pressing the button: the pointer already shows where
    /// the person is, so the focus ring is kept for keyboard focus.
    pressed_into_focus: bool,
    /// Drawn instead of (icon-only) or before (icon + label) the text,
    /// so toolbars render crisp lucide icons without depending on a
    /// Nerd Font being installed.
    icon: Option<crate::icons::Icon>,
    /// Stretch to the parent's width instead of sizing to the label.
    fill_width: bool,
    /// What the button does, which decides its whole look (see
    /// [`ButtonRole`]).
    role: ButtonRole,
    /// `Some` for a toggle button; `Some(true)` paints the lift and is
    /// announced as pressed.
    toggled: Option<bool>,
    /// Trailing keyboard shortcut, spelled with the platform's keycaps.
    shortcut: Option<crate::platform::Shortcut>,
    /// The width `measure` settled on, pinned onto the node so a stretching
    /// parent cannot widen an intrinsic button.
    measured_width: Option<f32>,
    /// Sets the label role and the height (see [`ControlSize`]).
    control_size: ControlSize,
    /// The geometry resolved against the theme seen in `build_layout`, for
    /// `measure`, which has no theme of its own; `draw` reads the theme it
    /// is handed.
    metrics: FaceMetrics,
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
    /// Where `enabled` comes from, when the app keeps it in a state.
    enabled_binding: Option<crate::widget::binding::BoxedBinding<bool>>,
    /// The page's encouraged action: a focus reset lands here, so Enter
    /// activates it.
    default_focus: bool,
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
    /// The theme roles the cached face was painted from, so a theme change
    /// that keeps the layout (dark to light, a new emphasis) repaints it.
    last_theme: Option<ThemeKey>,
}

/// The theme roles a button face reads.
type ThemeKey = (
    vello::peniko::Color,
    vello::peniko::Color,
    vello::peniko::Color,
    vello::peniko::Color,
    vello::peniko::Color,
    vello::peniko::Color,
    u32,
);

fn theme_key(theme: &Theme) -> ThemeKey {
    let colors = &theme.tokens.colors;
    (
        colors.surfaces.raised,
        colors.intent.inverted,
        colors.intent.on_inverted,
        colors.text_primary,
        colors.semantic.danger,
        colors.intent.you,
        theme.tokens.radii.control.to_bits(),
    )
}

impl Button {
    /// Focus a person needs to be shown: keyboard focus, not the focus a
    /// click on the button gave it.
    fn focus_visible(&self) -> bool {
        self.is_focused && !self.pressed_into_focus
    }

    fn effectively_enabled(&self) -> bool {
        self.enabled && self.action.as_ref().is_none_or(ActionEmitter::is_available)
    }
    pub fn new(id: &str, label: &str) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            accessible_name: None,
            tooltip: None,
            compact_presentation: None,
            rect: Rect::ZERO,
            is_hovered: false,
            press: crate::pointer_replay::HeldPress::default(),
            is_focused: false,
            pressed_into_focus: false,
            icon: None,
            fill_width: false,
            role: ButtonRole::Secondary,
            toggled: None,
            shortcut: None,
            measured_width: None,
            control_size: ControlSize::Regular,
            metrics: FaceMetrics::stock(ControlSize::Regular),
            default_focus: false,
            on_click: None,
            action: None,
            native_resource: None,
            native_handler: None,
            presence_path: Vec::new(),
            permission_scope: None,
            enabled: true,
            enabled_binding: None,
            cached: vello::Scene::new(),
            dirty: true,
            layout_dirty: false,
            i18n_revision: crate::i18n::I18nManager::revision(),
            interaction: crate::interaction::InteractionEnvironment::default(),
            cache_rect: Rect::ZERO,
            last_state: (false, false, false, true),
            last_theme: None,
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
    /// cursor is still over the button), Enter, Space or an assistive
    /// `Activate`.
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
        // An icon-only button is named by what a screen reader announces.
        let label = if self.label.is_empty() {
            self.accessible_name
                .clone()
                .or_else(|| self.tooltip.clone())
                .unwrap_or_default()
        } else {
            self.label.clone()
        };
        let mut spec =
            tilo_actions::ActionSpec::operation("invoke", label.clone(), kind, TYPE_ID, &self.id)
                .expect("a control id is always JSON serializable");
        spec.contract.discovery = tilo_actions::DiscoveryScope::Launchable;
        let resource =
            tilo_actions::Resource::new(resource_id, "tilo.control", label).with_action(spec);
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

    /// Change what an icon-only button is announced (and published) as, for
    /// a control whose meaning flips with state ("Show sidebar" / "Hide
    /// sidebar").
    pub fn set_accessible_name(&mut self, name: &str) {
        if self.accessible_name.as_deref() != Some(name) {
            self.accessible_name = Some(name.to_owned());
            self.dirty = true;
        }
    }

    fn observe_i18n_revision(&mut self) {
        let revision = crate::i18n::I18nManager::revision();
        if revision != self.i18n_revision {
            self.i18n_revision = revision;
            self.dirty = true;
            // A new locale re-shapes the label, and the width follows it.
            self.layout_dirty = true;
        }
    }

    /// An icon-only button: `icon` is what it shows, `label` is what a
    /// screen reader announces and what its tooltip says. It is a square of
    /// its [`ControlSize`]'s height (never under the minimum pointer target)
    /// and, like every toolbar glyph, a [`ButtonRole::Plain`] button unless
    /// a role is given.
    pub fn icon_only(id: &str, icon: crate::icons::Icon, label: &str) -> Self {
        Self::new(id, "")
            .with_icon(icon)
            .with_accessible_name(label)
            .tooltip(label)
            .role(ButtonRole::Plain)
    }

    /// Stretch to the parent's width. The height still comes from the
    /// [`ControlSize`]; without this a button is as wide as its label.
    #[must_use]
    pub fn fill_width(mut self) -> Self {
        self.fill_width = true;
        self.layout_dirty = true;
        self
    }

    /// How big this button is: its height and label role together (see
    /// [`ControlSize`]).
    #[must_use]
    pub fn control_size(mut self, size: ControlSize) -> Self {
        self.control_size = size;
        self.metrics = FaceMetrics::stock(size);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    /// What the button does; the role decides its whole look.
    #[must_use]
    pub fn role(mut self, role: ButtonRole) -> Self {
        self.role = role;
        self.dirty = true;
        self
    }

    pub fn set_role(&mut self, role: ButtonRole) {
        if self.role != role {
            self.role = role;
            self.dirty = true;
        }
    }

    /// Make this a toggle button: while `on` it stands on a lift of its
    /// ground (a selected toolbar glyph, HIG §9.3) and is announced as a
    /// pressed toggle. Use it on a [`ButtonRole::Plain`] glyph.
    #[must_use]
    pub fn toggled(mut self, on: bool) -> Self {
        self.set_toggled(on);
        self
    }

    /// Turn a toggle button on or off (the host syncing from its model).
    pub fn set_toggled(&mut self, on: bool) {
        if self.toggled != Some(on) {
            self.toggled = Some(on);
            self.dirty = true;
        }
    }

    /// Whether this is a toggle button that is on.
    pub fn is_toggled(&self) -> bool {
        self.toggled == Some(true)
    }

    /// Show the keyboard shortcut that does the same thing, after the label,
    /// in the platform's keycap legends (`Ctrl+Enter` on Linux and Windows,
    /// `⌘↩` on macOS). It is part of the button's width.
    #[must_use]
    pub fn shortcut_hint(mut self, shortcut: crate::platform::Shortcut) -> Self {
        self.shortcut = Some(shortcut);
        self.dirty = true;
        self.layout_dirty = true;
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

    /// Follow `state` for whether the button can act ("Save" enabled while
    /// there is something to save): a change through any clone of it shows
    /// on the next sync.
    #[must_use]
    pub fn enabled_from(mut self, state: &crate::LocalState<bool>) -> Self {
        let (enabled, binding) = super::binding::local(state);
        self.enabled = enabled;
        self.enabled_binding = Some(binding);
        self
    }

    /// Mark this button as its page's encouraged action: when the page is
    /// rebuilt with a focus reset, keyboard focus starts here, so Enter
    /// activates it.
    #[must_use]
    pub fn with_default_focus(mut self) -> Self {
        self.default_focus = true;
        self
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

    /// Whether [`Self::set_compact_toolbar_presentation`] leaves a glyph:
    /// a labelled button with an icon (or one already shown icon-only).
    pub(crate) fn compacts_to_icon(&self) -> bool {
        self.icon.is_some() && (!self.label.is_empty() || self.compact_presentation.is_some())
    }

    /// Whether the button is shown icon-only by a responsive toolbar.
    pub(crate) fn is_compact_presentation(&self) -> bool {
        self.compact_presentation.is_some()
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

/// What a button does. The role is a button's whole appearance: an app never
/// picks a fill, an ink, an edge or a radius for one.
///
/// | Role | Use | Face |
/// |---|---|---|
/// | `Primary` | the one main action of a view | inverted: primary ink as the fill, the content surface as the label |
/// | `Secondary` | every other action | a raised control with a derived edge and primary ink |
/// | `Destructive` | deletes, removes or revokes something | the secondary face lettered in danger ink; filled only inside the confirmation step |
/// | `Plain` | inline text actions, toolbar glyphs, close buttons | nothing at rest, a lift of its ground under the pointer; a labelled one's ink sits on its layout edge and its lift and hit area reach outward |
/// | `Key` | a keypad or keyboard key (a calculator digit, an on-screen key, a one-letter toggle) | a faint lift of its ground at rest, its label in `Title` |
///
/// Every role has the control radius (`radii.control`): buttons are not
/// pills.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum ButtonRole {
    Primary,
    #[default]
    Secondary,
    Destructive,
    Plain,
    /// A key of a keypad or an on-screen keyboard.
    Key,
}

impl ButtonRole {
    /// A labelled face of this role sets its ink on its layout edge and
    /// reaches outward for its lift and hit area.
    const fn flush(self) -> bool {
        matches!(self, Self::Plain)
    }
}

/// Glyphs authored as one closed outline, so a toggle that is on draws them
/// filled (a starred item, a pinned one).
fn fills_when_on(icon: crate::icons::Icon) -> bool {
    matches!(icon, crate::icons::Icon::Star | crate::icons::Icon::Pin)
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

/// `(fill, edge, ink)` of one button face.
type FaceColors = (
    vello::peniko::Color,
    Option<vello::peniko::Color>,
    vello::peniko::Color,
);

/// The filled destructive face: the confirmation step of a destructive
/// action, and nowhere else.
fn confirmation_face_colors(theme: &Theme, interact: ButtonInteract) -> FaceColors {
    let colors = &theme.tokens.colors;
    let rest = colors.semantic.danger;
    let active = colors.lift_on(rest, 0.5);
    // The label is resolved over both fills, so it does not change under
    // the pointer, and the feedback never costs it its contrast.
    let ink = colors.ink_on_any(
        &[rest, active],
        colors.intent.on_inverted,
        ON_FILL_MIN_CONTRAST,
    );
    let fill = if interact.hovered || interact.pressed {
        active
    } else {
        rest
    };
    (fill, None, ink)
}

/// The colours of `role` in `interact`: the single source of truth for the
/// retained [`Button`] and the immediate-mode [`Button::paint`].
///
/// `edge` is `None` where the fill defines the shape by itself (an inverted
/// primary face) or there is no face (a plain button); the painters stroke
/// the focus ring, in your colour, on top when focused.
pub fn face_colors(theme: &Theme, role: ButtonRole, interact: ButtonInteract) -> FaceColors {
    use vello::peniko::Color;
    let colors = &theme.tokens.colors;
    let intent = &colors.intent;

    // Disabled never keeps the fill of an enabled primary or destructive
    // action: a translucent "Save" still reads as actionable. A faced role
    // falls back to the secondary rest face and a plain one stays bare; the
    // painters lay either under `widget::DISABLED_ALPHA`, keeping the edge
    // at full strength so the control's bounds stay visible in light.
    if interact.disabled {
        let ink = colors.ink_on_any(
            &[colors.surfaces.raised],
            colors.text_primary,
            ON_FILL_MIN_CONTRAST,
        );
        return if role == ButtonRole::Plain {
            (Color::TRANSPARENT, None, ink)
        } else if role == ButtonRole::Key {
            (colors.lift_on(colors.surfaces.content, 0.5), None, ink)
        } else {
            let rest = colors.surfaces.raised;
            (rest, Some(crate::surface::edge_on(rest)), ink)
        };
    }

    match role {
        ButtonRole::Primary => {
            let (rest, hover, pressed) = (
                intent.inverted,
                intent.inverted_hover,
                colors.inverted_pressed(),
            );
            let fill = if interact.pressed {
                pressed
            } else if interact.hovered {
                hover
            } else {
                rest
            };
            // The label of the inverted fill is the theme's own
            // (`intent.on_inverted`, the window ground), resolved over every
            // fill so it does not change under the pointer.
            let ink = colors.ink_on_any(
                &[rest, hover, pressed],
                intent.on_inverted,
                ON_FILL_MIN_CONTRAST,
            );
            (fill, None, ink)
        }
        ButtonRole::Secondary | ButtonRole::Destructive => {
            let rest = colors.surfaces.raised;
            let (hover, active) = (colors.lift_on(rest, 0.5), colors.lift_on(rest, 1.0));
            let fill = if interact.pressed {
                active
            } else if interact.hovered {
                hover
            } else {
                rest
            };
            let wanted = if role == ButtonRole::Destructive {
                colors.semantic.danger
            } else {
                colors.text_primary
            };
            let ink = colors.ink_on_any(&[rest, hover, active], wanted, ON_FILL_MIN_CONTRAST);
            // The edge is derived from the rest face and held across hover
            // and press.
            (fill, Some(crate::surface::edge_on(rest)), ink)
        }
        ButtonRole::Key => {
            // A key rests on a faint lift of its ground and lifts further
            // under the pointer and the press.
            let ground = colors.surfaces.content;
            let (rest, hover, pressed) = (
                colors.lift_on(ground, 0.5),
                colors.lift_on(ground, 1.0),
                colors.lift_on(ground, 1.5),
            );
            let fill = if interact.pressed {
                pressed
            } else if interact.hovered {
                hover
            } else {
                rest
            };
            let ink = colors.ink_on_any(
                &[rest, hover, pressed],
                colors.text_primary,
                ON_FILL_MIN_CONTRAST,
            );
            (fill, None, ink)
        }
        ButtonRole::Plain => {
            let fill = if interact.pressed {
                intent.selection
            } else if interact.hovered {
                intent.selection_hover
            } else {
                Color::TRANSPARENT
            };
            // At rest a plain button paints nothing: it stands on the ground
            // its container handed it (`surfaces.content`, re-pointed by
            // `Theme::on_ground` inside a region), and hover and press are
            // that ground's lift.
            let ink = colors.ink_on_any(
                &[
                    colors.surfaces.content,
                    intent.selection_hover,
                    intent.selection,
                ],
                colors.text_primary,
                ON_FILL_MIN_CONTRAST,
            );
            (fill, None, ink)
        }
    }
}

fn resolve_face_colors(
    theme: &Theme,
    role: ButtonRole,
    confirms: bool,
    interact: ButtonInteract,
) -> FaceColors {
    if confirms && role == ButtonRole::Destructive && !interact.disabled {
        confirmation_face_colors(theme, interact)
    } else {
        face_colors(theme, role, interact)
    }
}

/// A button's geometry in one theme: everything its size decides.
#[derive(Clone, Debug, PartialEq)]
struct FaceMetrics {
    /// The [`ControlSize`] height, grown with the label's line at large
    /// text.
    height: f64,
    /// Inset of the content from each side.
    pad_x: f64,
    /// Edge of a leading or sole icon.
    icon: f64,
    /// Between the icon and the label.
    icon_gap: f64,
    /// Between the label and the shortcut hint.
    hint_gap: f64,
    label_style: TextStyle,
}

impl FaceMetrics {
    fn of(size: ControlSize, theme: &Theme) -> Self {
        let metrics = &theme.tokens.metrics;
        let spacing = &theme.tokens.spacing;
        let (pad_x, icon) = match size {
            ControlSize::Small => (spacing.s2, metrics.icon_sm),
            ControlSize::Regular => (spacing.s3, metrics.icon_md),
            ControlSize::Large => (spacing.s4, metrics.icon_lg),
        };
        let height = size.height(theme);
        // The icon grows with the label at a large text scale, never past
        // the face's inner band.
        let icon = (f64::from(icon) * f64::from(theme.text_scale.max(1.0)))
            .min(height - 2.0 * f64::from(spacing.s1))
            .round();
        Self {
            height,
            pad_x: f64::from(pad_x),
            icon,
            icon_gap: f64::from(metrics.icon_label_gap),
            hint_gap: f64::from(spacing.s3),
            label_style: size.label_style(theme),
        }
    }

    /// The geometry of `size` for a face of `role`: a key sets its label in
    /// `Title` and its icon to that line.
    fn of_role(size: ControlSize, role: ButtonRole, theme: &Theme) -> Self {
        let mut metrics = Self::of(size, theme);
        if role == ButtonRole::Key {
            metrics.label_style = theme.text(crate::tokens::TextRole::Title);
            metrics.icon = f64::from(metrics.label_style.size).round();
        }
        metrics
    }

    /// The geometry of `size` in the stock theme: what a button measures
    /// with before it has seen the active theme.
    fn stock(size: ControlSize) -> Self {
        static STOCK: std::sync::LazyLock<[FaceMetrics; 3]> = std::sync::LazyLock::new(|| {
            let theme = Theme::dark();
            [
                FaceMetrics::of(ControlSize::Small, &theme),
                FaceMetrics::of(ControlSize::Regular, &theme),
                FaceMetrics::of(ControlSize::Large, &theme),
            ]
        });
        let index = match size {
            ControlSize::Small => 0,
            ControlSize::Regular => 1,
            ControlSize::Large => 2,
        };
        STOCK
            .get(index)
            .cloned()
            .unwrap_or_else(|| Self::of(size, &Theme::dark()))
    }
}

/// A button face's geometry captured from the theme in `build_layout`, for a
/// toolkit control that lays itself out like a [`Button`] and measures
/// without a theme of its own.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FaceSizer {
    metrics: FaceMetrics,
}

impl FaceSizer {
    pub(crate) fn of(size: ControlSize, theme: &Theme) -> Self {
        Self {
            metrics: FaceMetrics::of(size, theme),
        }
    }

    pub(crate) fn stock(size: ControlSize) -> Self {
        Self {
            metrics: FaceMetrics::stock(size),
        }
    }

    pub(crate) fn height(&self) -> f64 {
        self.metrics.height
    }

    pub(crate) fn intrinsic_width(
        &self,
        text: &mut TextRenderer,
        icon: Option<crate::icons::Icon>,
        label: &str,
    ) -> f64 {
        FaceContent {
            icon,
            label,
            hint: None,
            flush: false,
            filled_icon: false,
        }
        .intrinsic_width(&self.metrics, text, self.metrics.height)
    }
}

/// What sits inside a button's face: an optional leading icon, the label and
/// an optional trailing shortcut hint.
struct FaceContent<'a> {
    icon: Option<crate::icons::Icon>,
    label: &'a str,
    hint: Option<String>,
    /// A labelled plain face: no inner padding, the ink on the layout edge.
    flush: bool,
    /// Draw the icon filled (a toggle that is on, with a closed glyph).
    filled_icon: bool,
}

impl FaceContent<'_> {
    /// The narrowest face of `height` that shows everything without eliding:
    /// an icon-only face is a square, and a labelled one is at least twice
    /// its height, so a one-word label still reads as a button.
    fn intrinsic_width(&self, metrics: &FaceMetrics, text: &mut TextRenderer, height: f64) -> f64 {
        if self.label.is_empty() && self.hint.is_none() {
            return height;
        }
        let pad = if self.flush { 0.0 } else { metrics.pad_x };
        let mut width = 2.0 * pad;
        if self.icon.is_some() {
            width += metrics.icon + metrics.icon_gap;
        }
        if !self.label.is_empty() {
            width += f64::from(text.measure_style(self.label, None, &metrics.label_style).0);
        }
        if let Some(hint) = &self.hint {
            width += metrics.hint_gap
                + f64::from(text.measure_style(hint, None, &metrics.label_style).0);
        }
        if self.flush {
            width.ceil()
        } else {
            width.ceil().max(2.0 * height)
        }
    }

    fn draw_icon(
        &self,
        ctx: &mut PaintCtx<'_>,
        icon: crate::icons::Icon,
        rect: Rect,
        ink: vello::peniko::Color,
    ) {
        if self.filled_icon {
            icon.draw_filled(ctx.scene, ctx.transform, rect, ink);
        } else {
            crate::icons::draw_icon(
                ctx.scene,
                ctx.transform,
                rect,
                icon,
                ink,
                crate::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
    }

    /// Paints the content centred in `rect`. A label that does not fit is
    /// elided, never painted across the edge; the hint is dropped before the
    /// label is shortened.
    fn paint(
        &self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        metrics: &FaceMetrics,
        ink: vello::peniko::Color,
        hint_ink: vello::peniko::Color,
    ) {
        let icon_px = metrics.icon.min(rect.height());
        if self.label.is_empty() {
            if let Some(icon) = self.icon {
                let ix = rect.center().x - icon_px / 2.0;
                let iy = rect.center().y - icon_px / 2.0;
                self.draw_icon(
                    ctx,
                    icon,
                    Rect::new(ix, iy, ix + icon_px, iy + icon_px),
                    ink,
                );
            }
            return;
        }
        let style = &metrics.label_style;
        let lead = if self.icon.is_some() {
            icon_px + metrics.icon_gap
        } else {
            0.0
        };
        let pad = if self.flush { 0.0 } else { metrics.pad_x };
        let room = (rect.width() - 2.0 * pad - lead).max(1.0);
        let hint = self.hint.as_ref().and_then(|hint| {
            let label_w = f64::from(ctx.text.measure_style(self.label, None, style).0);
            let hint_w = f64::from(ctx.text.measure_style(hint, None, style).0);
            (label_w + metrics.hint_gap + hint_w <= room).then_some((hint.as_str(), hint_w))
        });
        let label_room = hint.map_or(room, |(_, hint_w)| room - metrics.hint_gap - hint_w);
        // Half a pixel of slack: a face sized to its measured label never
        // elides it over shaping round-off.
        let label =
            ctx.text
                .elide_to_width_style(self.label, px_f32(label_room.max(1.0) + 0.5), style);
        let (label_w, label_h) = ctx.text.measure_style(&label, None, style);
        let hint_w = hint.map_or(0.0, |(_, w)| metrics.hint_gap + w);
        let cluster = lead + f64::from(label_w) + hint_w;
        let start_x = rect.center().x - cluster / 2.0;
        if let Some(icon) = self.icon {
            let iy = rect.center().y - icon_px / 2.0;
            self.draw_icon(
                ctx,
                icon,
                Rect::new(start_x, iy, start_x + icon_px, iy + icon_px),
                ink,
            );
        }
        let text_y = rect.y0 + (rect.height() - f64::from(label_h)) / 2.0;
        let label_x = start_x + lead;
        ctx.text(&label, style, ink, Point::new(label_x, text_y), None);
        if let Some((hint, _)) = hint {
            let hint_x = label_x + f64::from(label_w) + metrics.hint_gap;
            ctx.text(hint, style, hint_ink, Point::new(hint_x, text_y), None);
        }
    }
}

/// The ink of a shortcut hint: the label's own ink on an inverted or filled
/// face, the secondary tier on a raised or bare one.
fn hint_ink(
    theme: &Theme,
    role: ButtonRole,
    ink: vello::peniko::Color,
    fill: vello::peniko::Color,
) -> vello::peniko::Color {
    match role {
        ButtonRole::Primary => ink,
        ButtonRole::Secondary | ButtonRole::Destructive | ButtonRole::Plain | ButtonRole::Key => {
            let ground = if fill.components[3] > 0.0 {
                fill
            } else {
                theme.tokens.colors.surfaces.content
            };
            theme.tokens.colors.ink_on_any(
                &[ground],
                theme.tokens.colors.text_dim,
                ON_FILL_MIN_CONTRAST,
            )
        }
    }
}

/// The shortcut as the platform spells it on its keycaps.
fn shortcut_text(shortcut: &crate::platform::Shortcut) -> String {
    shortcut.label(crate::platform::PlatformConventions::new(
        crate::platform::Platform::current(),
    ))
}

/// One button face, painted: the shared body of the retained [`Button`] and
/// the immediate-mode [`Button::paint`], so the two are pixel-identical.
struct FacePaint<'a> {
    rect: Rect,
    role: ButtonRole,
    confirms: bool,
    /// A toggle that is on: the face stands on the selection lift.
    on: bool,
    interact: ButtonInteract,
    content: FaceContent<'a>,
    metrics: &'a FaceMetrics,
}

impl FacePaint<'_> {
    fn paint(&self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let disabled = self.interact.disabled;
        let on = self.on && !disabled;
        // An "on" toggle is a lift and stays one under the pointer; a press
        // still answers.
        let interact = ButtonInteract {
            hovered: self.interact.hovered && !on,
            ..self.interact
        };
        let (fill, edge, ink) = resolve_face_colors(theme, self.role, self.confirms, interact);
        let fill = if on && !interact.pressed {
            theme.tokens.colors.intent.selection
        } else {
            fill
        };
        let radius = f64::from(theme.tokens.radii.control);
        // A labelled plain face keeps its ink on its layout edge; its lift
        // (and focus ring) reach outward by the padding it does not take.
        let face_rect = if self.content.flush {
            self.rect.inflate(self.metrics.pad_x, 0.0)
        } else {
            self.rect
        };
        let shape = RoundedRect::from_rect(face_rect, radius);
        // Disabled is the rest face under one group alpha (HIG §9.5), the
        // same signal every other control gives.
        if disabled {
            ctx.scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                ctx.transform,
                &shape,
            );
        }
        if fill.components[3] > 0.0 {
            ctx.scene.fill(
                vello::peniko::Fill::NonZero,
                ctx.transform,
                fill,
                None,
                &shape,
            );
        }
        let hint = hint_ink(theme, self.role, ink, fill);
        self.content.paint(ctx, self.rect, self.metrics, ink, hint);
        if disabled {
            ctx.scene.pop_layer();
        }
        // Rest edge and focus ring share one half-pixel-inset outline. The
        // edge stays at full strength while disabled so the bounds of a
        // disabled control stay visible on a light ground.
        let outline = crate::surface::edge_outline(face_rect, radius);
        if self.interact.focused && !disabled {
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(FOCUS_RING_WIDTH),
                ctx.transform,
                theme.tokens.colors.intent.you,
                None,
                &outline,
            );
        } else if let Some(edge) = edge {
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(REST_BORDER_WIDTH),
                ctx.transform,
                edge,
                None,
                &outline,
            );
        }
    }
}

/// An immediate-mode button: the [`Button`] look for a canvas host that owns
/// its own hit-testing. Built fluently:
/// `ButtonFace::new(rect, "Save").role(ButtonRole::Primary).hovered(hit)`.
///
/// Size the rect from the face itself, so rect and glyphs agree at every
/// scale: [`ButtonFace::height`] for the height,
/// [`ButtonFace::intrinsic_width`] for the width, and hit-test with
/// [`ButtonFace::hit_rect`] (never smaller than the minimum pointer target).
pub struct ButtonFace<'a> {
    rect: Rect,
    label: &'a str,
    icon: Option<crate::icons::Icon>,
    role: ButtonRole,
    confirms: bool,
    on: bool,
    interact: ButtonInteract,
    control_size: ControlSize,
    shortcut: Option<crate::platform::Shortcut>,
    accessible_name: Option<&'a str>,
}

impl<'a> ButtonFace<'a> {
    pub fn new(rect: Rect, label: &'a str) -> Self {
        Self {
            rect,
            label,
            icon: None,
            role: ButtonRole::Secondary,
            confirms: false,
            on: false,
            interact: ButtonInteract::default(),
            control_size: ControlSize::Regular,
            shortcut: None,
            accessible_name: None,
        }
    }
    /// An icon-only face; `name` is its accessible name.
    pub fn icon_only(rect: Rect, icon: crate::icons::Icon, name: &'a str) -> Self {
        Self::new(rect, "")
            .icon(icon)
            .accessible_name(name)
            .role(ButtonRole::Plain)
    }
    /// What the button does; the role decides its whole look.
    #[must_use]
    pub fn role(mut self, role: ButtonRole) -> Self {
        self.role = role;
        self
    }
    /// The confirmation step of a destructive action, the one place a
    /// destructive face is filled.
    #[must_use]
    pub(crate) fn confirming(mut self) -> Self {
        self.confirms = true;
        self
    }
    #[must_use]
    pub fn icon(mut self, icon: crate::icons::Icon) -> Self {
        self.icon = Some(icon);
        self
    }
    /// The face's height and label role.
    #[must_use]
    pub fn control_size(mut self, size: ControlSize) -> Self {
        self.control_size = size;
        self
    }
    /// A trailing keyboard shortcut in the platform's keycap legends.
    #[must_use]
    pub fn shortcut_hint(mut self, shortcut: crate::platform::Shortcut) -> Self {
        self.shortcut = Some(shortcut);
        self
    }
    /// A toggle that is on: the face stands on the selection lift.
    #[must_use]
    pub fn toggled(mut self, on: bool) -> Self {
        self.on = on;
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
    /// The resolved palette without painting: `(fill, edge, ink)`.
    pub fn colors(&self, theme: &Theme) -> FaceColors {
        resolve_face_colors(theme, self.role, self.confirms, self.interact)
    }
    /// The height of a face of `size` in `theme`.
    pub fn height(size: ControlSize, theme: &Theme) -> f64 {
        FaceMetrics::of(size, theme).height
    }
    /// The narrowest rect width that shows this face's icon, label and
    /// shortcut without eliding.
    pub fn intrinsic_width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let label = localized_label(self.label);
        let metrics = FaceMetrics::of_role(self.control_size, self.role, theme);
        FaceContent {
            icon: self.icon,
            label: &label,
            hint: self.shortcut.as_ref().map(shortcut_text),
            flush: false,
            filled_icon: false,
        }
        .intrinsic_width(&metrics, text, metrics.height)
    }
    /// Where a pointer activates this face: its rect, grown about its centre
    /// to the minimum pointer target.
    pub fn hit_rect(&self, theme: &Theme) -> Rect {
        let target = f64::from(theme.tokens.metrics.min_pointer_target);
        let grow_x = ((target - self.rect.width()) / 2.0).max(0.0);
        let grow_y = ((target - self.rect.height()) / 2.0).max(0.0);
        self.rect.inflate(grow_x, grow_y)
    }
    /// Whether `point` activates this face.
    pub fn contains(&self, theme: &Theme, point: Point) -> bool {
        self.hit_rect(theme).contains(point)
    }
}

/// Focus ring and rest border, stroked centred on the face outline like
/// every other control (checkbox, toggle, segmented strip).
const FOCUS_RING_WIDTH: f64 = 2.0;
const REST_BORDER_WIDTH: f64 = 1.0;

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

impl Button {
    /// Paint a [`ButtonFace`]: the retained [`Button`]'s look for a canvas
    /// host that owns its own hit-testing ([`ButtonFace::contains`]).
    pub fn paint(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        face: &ButtonFace<'_>,
    ) {
        let label = localized_label(face.label);
        let metrics = FaceMetrics::of_role(face.control_size, face.role, theme);
        FacePaint {
            rect: face.rect,
            role: face.role,
            confirms: face.confirms,
            on: face.on,
            interact: face.interact,
            content: FaceContent {
                icon: face.icon,
                label: &label,
                hint: face.shortcut.as_ref().map(shortcut_text),
                // A painted face fills the rect its host gave it.
                flush: false,
                filled_icon: face.on && face.icon.is_some_and(fills_when_on),
            },
            metrics: &metrics,
        }
        .paint(&mut PaintCtx::new(scene, transform, text_renderer, theme));
    }

    /// Appends the cached face scene. The disabled group alpha is encoded
    /// *inside* the cache by `draw`, so the append itself applies none — a
    /// second layer here would dim a disabled face twice.
    fn append_cached(&self, out_scene: &mut Scene, inner: Affine) {
        out_scene.append(&self.cached, Some(inner));
    }

    fn content(&self) -> (String, Option<String>) {
        (
            localized_label(&self.label),
            self.shortcut.as_ref().map(shortcut_text),
        )
    }

    /// The face height in the current theme. A fine pointer keeps the
    /// size's height and reaches the minimum target through the hit rect
    /// ([`Self::hit_rect`]); a coarse pointer grows the face to it.
    fn face_height(&self) -> f64 {
        if self.interaction.supports_hover() {
            self.metrics.height
        } else {
            self.metrics
                .height
                .max(f64::from(self.interaction.minimum_target()))
        }
    }

    /// The face's width with everything shown, measured with the geometry
    /// captured in `build_layout`.
    fn intrinsic_width(&self, text: &mut TextRenderer) -> f32 {
        let (label, hint) = self.content();
        px_f32(
            FaceContent {
                flush: self.role.flush() && !label.is_empty(),
                icon: self.icon,
                label: &label,
                hint,
                filled_icon: false,
            }
            .intrinsic_width(&self.metrics, text, self.face_height()),
        )
    }

    /// Where a pointer activates the button: the face, grown about its
    /// centre to the minimum pointer target; a labelled plain button also
    /// reaches outward by the padding its ink does not take.
    fn hit_rect(&self) -> Rect {
        let rect = if self.role.flush() && !self.label.is_empty() {
            self.rect.inflate(self.metrics.pad_x, 0.0)
        } else {
            self.rect
        };
        self.interaction.minimum_target_rect(rect)
    }
}

impl Button {
    fn pin_measured_width(&self, taffy: &mut TaffyTree<()>, node: NodeId) -> bool {
        let Some(width) = self.measured_width.filter(|_| !self.fill_width) else {
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
        }
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.observe_i18n_revision();
        if let Some(binding) = &mut self.enabled_binding
            && let crate::widget::binding::Pulled::Value(enabled) = binding.pull(state, path)
        {
            self.set_enabled(enabled);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.observe_i18n_revision();
        self.measured_width = None;
        let metrics = FaceMetrics::of_role(self.control_size, self.role, theme);
        if metrics != self.metrics {
            // A theme scale change re-shapes the label; the cached face must
            // not survive at the old metrics. (The live App rebuilds widgets
            // on a theme change; a relayout-only host such as
            // `FrameHarness::set_theme` relies on this.)
            self.dirty = true;
            self.metrics = metrics;
        }
        let height = px_f32(self.face_height());
        let style = Style {
            // An intrinsic button never shrinks below its label: a parent
            // with room shows it whole, and only a parent without room
            // (capped by `max_size`) makes the painter elide it.
            flex_shrink: 0.0,
            min_size: Size {
                width: Dimension::length(height),
                height: Dimension::length(height),
            },
            max_size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            size: Size {
                width: if self.fill_width {
                    Dimension::percent(1.0)
                } else {
                    Dimension::auto()
                },
                height: Dimension::length(height),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        _available: Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> Size<f32> {
        let intrinsic = self.intrinsic_width(text);
        if !self.fill_width {
            // Pinned onto the node after the pass, so a stretching parent
            // cannot widen an intrinsic button.
            self.measured_width = Some(intrinsic);
        }
        Size {
            width: known.width.unwrap_or(intrinsic),
            height: known.height.unwrap_or_else(|| px_f32(self.face_height())),
        }
    }

    /// An intrinsic button's width is its measured content. Taffy only asks
    /// `measure` for an axis it cannot resolve, so the label is measured
    /// here as well and the width pinned when the solver has not done so.
    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        if self.fill_width {
            return false;
        }
        if self.measured_width.is_none() {
            self.measured_width = Some(self.intrinsic_width(text));
        }
        self.pin_measured_width(taffy, node)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
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
        presence: &crate::presence::PresenceCtx,
    ) {
        self.observe_i18n_revision();
        let current_state = (
            self.is_hovered,
            self.press.is_held(),
            self.focus_visible(),
            self.effectively_enabled(),
        );
        if current_state != self.last_state {
            self.dirty = true;
            self.last_state = current_state;
        }
        let key = theme_key(theme);
        if self.last_theme != Some(key) {
            self.dirty = true;
            self.last_theme = Some(key);
        }
        let needs_emit = self.dirty || self.cache_rect == Rect::ZERO;
        if !needs_emit {
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            let inner = out_transform.pre_translate(Vec2::new(dx, dy));
            self.append_cached(out_scene, inner);
            crate::widget::draw_gate_and_peer_focus(
                self,
                out_scene,
                out_transform,
                theme,
                presence,
            );
            return;
        }
        // cache_rect tracks *where* the cache was emitted so subsequent
        // frames can translate without re-encoding.
        self.cached.reset();
        let enabled = self.effectively_enabled();
        // The geometry is read from the theme handed in, so a host that
        // positions the button without a `build_layout` pass still gets the
        // theme's type; `measure` uses the copy captured in `build_layout`,
        // the same value on every taffy-driven path.
        let metrics = FaceMetrics::of_role(self.control_size, self.role, theme);
        let (label, hint) = self.content();
        let on = self.is_toggled();
        FacePaint {
            rect: self.rect,
            role: self.role,
            confirms: false,
            on,
            interact: ButtonInteract {
                hovered: self.is_hovered,
                pressed: self.press.is_held(),
                focused: self.focus_visible(),
                disabled: !enabled,
            },
            content: FaceContent {
                flush: self.role.flush() && !label.is_empty(),
                icon: self.icon,
                label: &label,
                hint,
                filled_icon: on && self.icon.is_some_and(fills_when_on),
            },
            metrics: &metrics,
        }
        .paint(&mut PaintCtx::new(
            &mut self.cached,
            Affine::IDENTITY,
            text_renderer,
            theme,
        ));
        self.cache_rect = self.rect;
        self.append_cached(out_scene, out_transform);
        crate::widget::draw_gate_and_peer_focus(self, out_scene, out_transform, theme, presence);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.i18n_revision != crate::i18n::I18nManager::revision()
            || (
                self.is_hovered,
                self.press.is_held(),
                self.focus_visible(),
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
            || self.i18n_revision != crate::i18n::I18nManager::revision()
    }

    fn layout_needs_measure(&self) -> bool {
        // A missing paint cache asks for drawing, not another intrinsic
        // measurement. Geometry is available as soon as layout is applied.
        self.rect == Rect::ZERO
            || self.layout_dirty
            || self.i18n_revision != crate::i18n::I18nManager::revision()
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
                let hover = self.hit_rect().contains(logical_pos);
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
                use crate::pointer_replay::{PressAt, Release};
                let rect = self.hit_rect();
                let at = PressAt::one(&self.id, rect);
                match element_state {
                    ElementState::Pressed if self.is_hovered && !self.press.is_held() => {
                        self.press.press(at);
                        *ctx.handled = true;
                        self.pressed_into_focus |= !self.is_focused;
                        *ctx.focus_target = Some(ctx.node_id);
                    }
                    ElementState::Pressed => {
                        if !self.is_hovered && self.press.is_held() {
                            self.press.cancel();
                            *ctx.handled = true;
                        }
                    }
                    // The press this button took, or the one a button it
                    // replaced took at this place (a rebuild while the
                    // button was held), completes here.
                    ElementState::Released => {
                        match self.press.release(self.is_hovered.then_some(at)) {
                            Release::Click(()) => {
                                *ctx.handled = true;
                                self.activate();
                            }
                            Release::Cancelled => *ctx.handled = true,
                            Release::NotHeld => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Button"
    }

    /// A host that lays buttons out (a header row) reaches their
    /// presentation through this.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
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
        if self.hit_rect().contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn is_default_keyboard_focus(&self) -> bool {
        self.default_focus
    }
    fn set_focus(&mut self, focused: bool) {
        // A press that focused it keeps its mark; losing focus clears it.
        self.pressed_into_focus &= focused;
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
        _doc: &mut crate::widget::Edits<'_>,
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
        _key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _root: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        if !self.effectively_enabled() {
            return false;
        }
        match action {
            Action::Activate => {
                self.activate();
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
        let role = if self.toggled.is_some() {
            Role::ToggleButton
        } else {
            Role::Button
        };
        let node = AccessNode::new(role)
            .with_semantic_key(&self.id)
            .with_bounds(self.rect)
            .with_optional_name(name)
            .with_state(AccessState {
                focusable: self.is_focusable(),
                focused: self.is_focused(),
                disabled: !self.is_enabled(),
                checked: self.toggled,
                ..Default::default()
            });
        Some(node)
    }
}

impl crate::presence::capability::PublishesPresence for Button {
    fn presence_key(&self) -> Option<PresenceKey> {
        PresenceKey::widget(&self.presence_path)
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
        // The action directory refuses unnamed resources and actions, and an
        // icon-only button is named by what it is announced as now: its
        // accessible name or tooltip may be set after `on_effect` built the
        // resource, or change with its state ("Hide sidebar" / "Show
        // sidebar"). Publish the current name.
        if self.label.is_empty()
            && let Some(name) = self
                .accessible_name
                .as_ref()
                .or(self.tooltip.as_ref())
                .filter(|name| !name.trim().is_empty())
        {
            let stale = std::mem::replace(&mut resource.label, name.clone());
            for action in &mut resource.actions {
                if action.label.trim().is_empty() || action.label == stale {
                    action.label.clone_from(name);
                }
            }
        }
        vec![resource]
    }
}

impl crate::widget::controls::ActionControls for Button {
    crate::widget::controls::optional_emitter_controls!(custody);
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        use crate::widget::{ControlBindingKind, ControlBindingMetadata};
        if let Some(action) = &self.action {
            return vec![action.metadata(self.id.clone())];
        }
        let mut bindings = Vec::new();
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
    fn activate(&mut self) {
        if let Some(action) = &mut self.action {
            if let Err(error) = action.emit(()) {
                tracing::warn!(%error, button = %self.id, "action-bound control refused its arguments");
            }
            return;
        }
        if let Some(cb) = &mut self.on_click {
            cb();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::TextRole;
    use crate::widget::Widget;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_shared_button_answers_through_an_ordinary_container() {
        let clicks = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&clicks);
        let mut column = crate::widget::Column::new().push(Box::new(
            Button::new("files.retry", "Retry").on_view(move || {
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
        .start(
            &mut column,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        )
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
    fn intrinsic_width_stays_compact_and_aligned_in_hosts() {
        fn intrinsic_width_stays_compact_in_stretched_flex_column() {
            let mut host = fit_host(0, Button::new("back", "Back to apps"));
            let rect = layout_fit_host(
                host.as_mut(),
                taffy::AlignItems::Stretch,
                &Theme::dark(),
                &mut TextRenderer::new(),
            );
            assert!(rect.width() < 300.0, "intrinsic width stretched: {rect:?}");
            assert_eq!(rect.height(), 32.0);
        }

        fn intrinsic_width_stays_compact_in_intrinsic_grid_column() {
            let mut host = fit_host(1, Button::new("back", "Back to apps"));
            let rect = layout_fit_host(
                host.as_mut(),
                taffy::AlignItems::Stretch,
                &Theme::dark(),
                &mut TextRenderer::new(),
            );
            assert!(rect.width() < 300.0, "intrinsic width stretched: {rect:?}");
            assert_eq!(rect.height(), 32.0);
        }

        fn compact_fit_width_preserves_host_center_and_end_alignment() {
            for kind in 0..3 {
                for alignment in [taffy::AlignItems::Center, taffy::AlignItems::End] {
                    let mut host = fit_host(kind, Button::new("back", "Back to apps"));
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
            let button = Button::new("back", "Back to apps").on_click(move || {
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
            let mut host = fit_host(1, Button::new("fill", "Back to apps").fill_width());
            let rect = layout_fit_host(
                host.as_mut(),
                taffy::AlignItems::Stretch,
                &Theme::dark(),
                &mut TextRenderer::new(),
            );
            assert_eq!(rect.width(), 600.0);
        }

        intrinsic_width_stays_compact_in_stretched_flex_column();
        intrinsic_width_stays_compact_in_intrinsic_grid_column();
        compact_fit_width_preserves_host_center_and_end_alignment();
        compact_fit_width_is_the_prepaint_click_and_focus_geometry();
        explicit_fill_width_still_stretches_in_intrinsic_column();
    }

    #[test]
    fn intrinsic_buttons_remeasure_and_size_from_their_control_size() {
        fn retained_width_remeasures_scale_and_locale_after_rebuild() {
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
            let mut host = fit_host(1, Button::new("refresh", "fit-refresh"));
            let mut text = TextRenderer::new();
            let normal = layout_fit_host(
                host.as_mut(),
                taffy::AlignItems::Stretch,
                &Theme::dark(),
                &mut text,
            );
            text.set_text_scale(2.0);
            let large_theme = Theme::dark().with_text_scale(2.0);
            let large = layout_fit_host(
                host.as_mut(),
                taffy::AlignItems::Stretch,
                &large_theme,
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
                &large_theme,
                &mut text,
            );
            I18nManager::set_locale("en-US");
            assert!(
                localized.width() > large.width(),
                "locale must refresh retained width: {large:?} -> {localized:?}"
            );
            assert_eq!(
                localized.height(),
                ControlSize::Regular.height(&large_theme)
            );
        }

        fn intrinsic_width_holds_the_whole_label() {
            let mut text = TextRenderer::new();
            let theme = Theme::dark();
            for label in ["Install workspace", "Refresh connections", "View details"] {
                let rect = measured_button_rect(Button::new("action", label), 1.0);
                let (width, _) = text.measure_style(label, None, &theme.text(TextRole::Label));
                assert!(
                    rect.width() >= f64::from(width + theme.tokens.spacing.s3 * 2.0),
                    "the label must fit without eliding: {label}: {rect:?}, text {width}"
                );
            }
        }

        fn each_control_size_has_its_height_and_grows_with_large_text() {
            for (size, height) in [
                (ControlSize::Small, 28.0),
                (ControlSize::Regular, 32.0),
                (ControlSize::Large, 40.0),
            ] {
                let rect =
                    measured_button_rect(Button::new("save", "Save").control_size(size), 1.0);
                assert_eq!(rect.height(), height, "{size:?}");
                assert!(
                    rect.width() >= 2.0 * height,
                    "{size:?} labelled minimum: {rect:?}"
                );
            }
            let theme = Theme::dark().with_text_scale(2.0);
            let rect = measured_button_rect(Button::new("save", "Save"), 2.0);
            let line = f64::from(theme.text(TextRole::Label).line_height_px());
            assert!(rect.height() > 32.0 && rect.height() >= line, "{rect:?}");
        }

        fn icon_only_buttons_are_square_named_and_plain() {
            let button = Button::icon_only("close", crate::icons::Icon::X, "Close");
            assert_eq!(button.role, ButtonRole::Plain);
            assert_eq!(Widget::tooltip(&button), Some("Close"));
            assert_eq!(
                Widget::accessibility(&button).unwrap().name.as_deref(),
                Some("Close")
            );
            let rect = measured_button_rect(button, 1.0);
            assert_eq!((rect.width(), rect.height()), (32.0, 32.0));
        }

        fn a_shortcut_hint_widens_the_button_and_paints_after_the_label() {
            let shortcut = crate::platform::Shortcut::command("Enter");
            let plain = measured_button_rect(Button::new("run", "Run query"), 1.0);
            let hinted = measured_button_rect(
                Button::new("run", "Run query").shortcut_hint(shortcut.clone()),
                1.0,
            );
            assert!(hinted.width() > plain.width(), "{plain:?} -> {hinted:?}");
            let label = shortcut.label(crate::platform::PlatformConventions::new(
                crate::platform::Platform::current(),
            ));
            if crate::platform::Platform::current() == crate::platform::Platform::Linux {
                assert!(!label.contains('⌘'), "no Apple legend on Linux: {label}");
            }
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let face = ButtonFace::new(Rect::ZERO, "Run query").shortcut_hint(shortcut);
            let width = face.intrinsic_width(&mut text, &theme);
            let face = ButtonFace::new(Rect::new(0.0, 0.0, width, 32.0), "Run query")
                .shortcut_hint(crate::platform::Shortcut::command("Enter"));
            let mut scene = Scene::new();
            Button::paint(&mut scene, Affine::IDENTITY, &mut text, &theme, &face);
            let origins = glyph_origins(&scene);
            assert_eq!(origins.len(), 2, "label and hint are two runs");
            assert!(origins[1][0] > origins[0][0], "the hint trails the label");
        }

        fn a_face_hit_rect_is_never_under_the_pointer_target() {
            let theme = Theme::dark();
            let face = ButtonFace::icon_only(
                Rect::new(10.0, 10.0, 30.0, 30.0),
                crate::icons::Icon::X,
                "Close",
            );
            let hit = face.hit_rect(&theme);
            assert_eq!((hit.width(), hit.height()), (32.0, 32.0));
            assert_eq!(hit.center(), face.rect.center());
            assert!(face.contains(&theme, Point::new(8.0, 20.0)));
            assert!(!face.contains(&theme, Point::new(40.0, 20.0)));
        }

        retained_width_remeasures_scale_and_locale_after_rebuild();
        intrinsic_width_holds_the_whole_label();
        each_control_size_has_its_height_and_grows_with_large_text();
        icon_only_buttons_are_square_named_and_plain();
        a_shortcut_hint_widens_the_button_and_paints_after_the_label();
        a_face_hit_rect_is_never_under_the_pointer_target();
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
        fn retained_and_immediate_labels_share_geometry() {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut text = TextRenderer::new();
            text.set_text_scale(2.0);
            let label = "Refresh";
            let rect = Rect::new(10.0, 20.0, 330.0, 84.0);
            let mut button = Button::new("action", label).with_icon(crate::icons::Icon::Refresh);
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
                &ButtonFace::new(rect, label).icon(crate::icons::Icon::Refresh),
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

                let mut laid_out = Button::new("save", "Save");
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
                let mut external = Button::new("save", "Save");
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
            let mut button = Button::new("save", "Save");
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

        retained_and_immediate_labels_share_geometry();
        default_face_reads_its_corner_and_label_role_from_the_theme();
        retained_label_follows_the_theme_scale_in_matched_and_unmatched_hosts();
        theme_text_scale_change_invalidates_the_cached_face();
    }

    #[test]
    fn action_mode_and_availability_gate_mutation_and_callbacks() {
        fn button_test_id_is_stable_constructor_id() {
            let b = Button::new("id", "Save");
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
            let mut button = Button::new("save", "Save")
                .on_action(binding)
                // Builder order cannot accidentally restore an imperative path.
                .on_click(move || {
                    callback_calls.fetch_add(1, Ordering::SeqCst);
                });
            button.activate();
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
            let mut button = Button::new("save", "Save").on_action(binding);
            button.activate();
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
            let button = Button::new("legacy", "Legacy").on_click(|| {});
            let bindings = crate::widget::controls::control_action_bindings(&button);
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

    /// A theme switched under a live tree repaints a plain button's face
    /// in the new theme's roles, rather than keeping the cached dark-theme
    /// ink that all but vanishes on a light ground.
    #[test]
    fn a_cached_face_follows_a_theme_switch() {
        let mut button = Button::new("plain", "Undo").role(ButtonRole::Plain);
        button.rect = Rect::new(0.0, 0.0, 80.0, 32.0);
        let presence = crate::presence::PresenceCtx::default();
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        for theme in [Theme::dark(), Theme::light()] {
            button.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
            assert_eq!(
                button.last_theme,
                Some(theme_key(&theme)),
                "the face is painted in {}",
                theme.name
            );
        }
    }

    #[test]
    fn face_colors_per_role_track_state_and_stay_readable() {
        use crate::a11y::contrast_ratio;
        let hover = ButtonInteract {
            hovered: true,
            ..Default::default()
        };
        let press = ButtonInteract {
            pressed: true,
            ..Default::default()
        };
        let rest = ButtonInteract::default();
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let colors = &theme.tokens.colors;
            let raised = colors.surfaces.raised;

            // Primary is inverted: primary ink as the fill, never a hue.
            let (fill, edge, ink) = face_colors(&theme, ButtonRole::Primary, rest);
            assert_eq!(fill, colors.intent.inverted, "{}", theme.name);
            assert_eq!(edge, None);
            assert!(contrast_ratio(ink, fill) >= ON_FILL_MIN_CONTRAST);
            let (hover_fill, _, hover_ink) = face_colors(&theme, ButtonRole::Primary, hover);
            assert_eq!(hover_fill, colors.intent.inverted_hover);
            assert_eq!(
                hover_ink, ink,
                "the label does not change under the pointer"
            );
            // Pressed reads past hover: rest, hover and press are three
            // distinct steps, and the label holds across all of them.
            let (press_fill, _, press_ink) = face_colors(&theme, ButtonRole::Primary, press);
            assert_eq!(press_fill, colors.inverted_pressed(), "{}", theme.name);
            assert_ne!(press_fill, hover_fill, "{}", theme.name);
            let step = |a, b| crate::chrome::shell::lightness_step(a, b);
            assert!(
                step(press_fill, fill) > step(hover_fill, fill),
                "{}: press must move further from rest than hover",
                theme.name
            );
            assert_eq!(press_ink, ink);
            assert!(contrast_ratio(press_ink, press_fill) >= ON_FILL_MIN_CONTRAST);

            // Secondary is the raised control with its derived edge.
            let (fill, edge, ink) = face_colors(&theme, ButtonRole::Secondary, rest);
            assert_eq!(fill, raised);
            assert_eq!(edge, Some(crate::surface::edge_on(raised)));
            assert!(contrast_ratio(ink, fill) >= ON_FILL_MIN_CONTRAST);
            assert_eq!(
                face_colors(&theme, ButtonRole::Secondary, hover).0,
                colors.lift_on(raised, 0.5)
            );
            assert_eq!(
                face_colors(&theme, ButtonRole::Secondary, press).0,
                colors.lift_on(raised, 1.0)
            );

            // Destructive is the same neutral control lettered in danger ink.
            for interact in [rest, hover, press] {
                let (fill, edge, ink) = face_colors(&theme, ButtonRole::Destructive, interact);
                assert_eq!(fill, face_colors(&theme, ButtonRole::Secondary, interact).0);
                assert_eq!(edge, Some(crate::surface::edge_on(raised)));
                assert!(
                    contrast_ratio(ink, fill) >= ON_FILL_MIN_CONTRAST,
                    "{}: danger ink on the neutral face",
                    theme.name
                );
                assert_ne!(
                    ink, colors.text_primary,
                    "{}: reads as destructive",
                    theme.name
                );
            }

            // Plain paints nothing at rest and lifts its ground under the pointer.
            let (fill, edge, ink) = face_colors(&theme, ButtonRole::Plain, rest);
            assert!(fill.components[3] <= 0.0);
            assert_eq!(edge, None);
            assert!(contrast_ratio(ink, colors.surfaces.content) >= ON_FILL_MIN_CONTRAST);
            assert_eq!(
                face_colors(&theme, ButtonRole::Plain, hover).0,
                colors.intent.selection_hover
            );
            assert_eq!(
                face_colors(&theme, ButtonRole::Plain, press).0,
                colors.intent.selection
            );
        }
    }

    /// The filled danger face exists only in a confirmation step.
    #[test]
    fn only_the_confirmation_step_fills_a_destructive_button() {
        use crate::a11y::contrast_ratio;
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let rect = Rect::new(0.0, 0.0, 120.0, 32.0);
            let plain = ButtonFace::new(rect, "Delete").role(ButtonRole::Destructive);
            assert_ne!(plain.colors(&theme).0, theme.tokens.colors.semantic.danger);
            let confirm = ButtonFace::new(rect, "Delete")
                .role(ButtonRole::Destructive)
                .confirming();
            for face in [
                confirm.hovered(false),
                ButtonFace::new(rect, "Delete")
                    .role(ButtonRole::Destructive)
                    .confirming()
                    .hovered(true),
            ] {
                let (fill, edge, ink) = face.colors(&theme);
                assert_eq!(edge, None);
                assert!(
                    contrast_ratio(ink, fill) >= ON_FILL_MIN_CONTRAST,
                    "{}: confirmation label {:.2}:1",
                    theme.name,
                    contrast_ratio(ink, fill)
                );
            }
            assert_eq!(
                ButtonFace::new(rect, "Delete")
                    .role(ButtonRole::Destructive)
                    .confirming()
                    .colors(&theme)
                    .0,
                theme.tokens.colors.semantic.danger
            );
        }
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
                    ButtonRole::Primary,
                    ButtonRole::Secondary,
                    ButtonRole::Destructive,
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
                let (bg, border, _) = face_colors(&theme, ButtonRole::Plain, disabled);
                assert!(
                    bg.components[3] <= 0.0,
                    "{name}: a disabled plain button stays bare"
                );
                assert_eq!(border, None);
            }
        }

        /// Writes a small disabled-button matrix for human visual review. The
        /// controls intentionally carry hover/focus input too: disabled state
        /// must win over stale pointer/focus state and never recover a semantic
        /// action colour or focus ring.
        #[ignore = "developer visual audit writes /tmp/tilo-button-disabled-audit"]
        fn disabled_button_visual_audit() {
            use std::path::Path;

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
                    (ButtonRole::Primary, "Primary", None),
                    (ButtonRole::Secondary, "Secondary", None),
                    (ButtonRole::Destructive, "Destructive", None),
                    (ButtonRole::Primary, "", Some(crate::icons::Icon::Check)),
                ];
                for (index, (style, label, icon)) in faces.into_iter().enumerate() {
                    let y = 88.0 + f64::from(u32::try_from(index).expect("four audit rows")) * 86.0;
                    let mut face = ButtonFace::new(Rect::new(80.0, y, 360.0, y + 52.0), label)
                        .role(style)
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
                    theme.tokens.colors.surfaces.content,
                    output.join(format!("{name}.png")),
                );
            }
        }

        fn with_enabled_false_is_reported_by_is_enabled() {
            let b = Button::new("id", "Go").with_enabled(false);
            assert!(!Widget::is_enabled(&b));
            let b2 = Button::new("id", "Go");
            assert!(Widget::is_enabled(&b2), "default is enabled");
        }

        fn disabled_button_rejects_input() {
            use std::cell::Cell;
            use std::rc::Rc;
            use winit::event::{ElementState, MouseButton};

            let fired = Rc::new(Cell::new(0u32));
            let mut b = {
                let fired = fired.clone();
                Button::new("id", "Go")
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

            let device = winit::event::DeviceId::dummy();
            let mut at_node = crate::testing::Dispatch::new().layout(&tree, node);
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
                let sent = at_node.send(&mut b, &ev);
                handled |= sent.handled;
                focus = sent.focus_target.or(focus);
            }

            assert_eq!(fired.get(), 0, "disabled button must not fire on_click");
            assert!(focus.is_none(), "disabled button must not request focus");
            assert!(!handled, "disabled button must not swallow input");
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
                .role(ButtonRole::Primary)
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
                    ButtonRole::Primary,
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
                &ButtonFace::new(Rect::new(10.0, 10.0, 110.0, 46.0), "Go")
                    .role(ButtonRole::Primary),
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
            let button = Button::new("close", "")
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
    fn the_intrinsic_footprint_follows_the_label_and_never_moves_it() {
        /// A short label stands at the labelled minimum, twice the height.
        fn a_short_label_is_at_the_labelled_minimum() {
            let rect = measured_button_rect(Button::new("save", "Save"), 1.0);
            assert_eq!((rect.width(), rect.height()), (64.0, 32.0), "{rect:?}");
        }

        /// A long label widens the button to hold it, never elided while the
        /// parent has room.
        fn a_wide_label_widens_the_button_instead_of_overflowing_it() {
            let label = "Remove every remote connection";
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let (label_w, _) = text.measure_style(label, None, &theme.text(TextRole::Label));
            let expected = (f64::from(label_w) + 2.0 * f64::from(theme.tokens.spacing.s3)).ceil();
            let rect = measured_button_rect(Button::new("remove", label), 1.0);
            assert!(
                (rect.width() - expected).abs() < 1e-6,
                "{rect:?} vs {expected}"
            );
            assert_eq!(rect.height(), 32.0);
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
                ButtonRole::Secondary,
                ButtonRole::Primary,
                ButtonRole::Destructive,
                ButtonRole::Plain,
            ] {
                let mut reference: Option<[f32; 2]> = None;
                for (name, interact) in states {
                    let mut scene = Scene::new();
                    let face = ButtonFace::new(rect, "Delete")
                        .role(style)
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

        a_short_label_is_at_the_labelled_minimum();
        a_wide_label_widens_the_button_instead_of_overflowing_it();
        interaction_states_do_not_move_the_face_or_its_label();
    }

    #[test]
    fn pointer_click_fires_the_callback() {
        fn cursor_shape_is_pointer_over_button() {
            let mut b = Button::new("id", "Go");
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
            use winit::event::{ElementState, MouseButton};

            let fired = Rc::new(Cell::new(0u32));
            let mut b = {
                let fired = fired.clone();
                Button::new("id", "Go").on_click(move || fired.set(fired.get() + 1))
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

            let device = winit::event::DeviceId::dummy();
            let mut at_node = crate::testing::Dispatch::new().layout(&tree, node);
            let mut dispatch = |b: &mut Button, ev: WindowEvent| {
                at_node.send(b, &ev);
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

        cursor_shape_is_pointer_over_button();
        on_click_callback_fires_on_release_over_button();
    }

    #[test]
    fn accessibility_reports_role_name_state_and_icon_only_naming() {
        fn accessibility_reports_button_role_name_and_state() {
            use crate::a11y::Role;
            let b = Button::new("id", "Save");
            let node = Widget::accessibility(&b).expect("button is not presentational");
            assert_eq!(node.role, Role::Button);
            assert_eq!(node.name.as_deref(), Some("Save"));
            assert!(node.state.focusable);
            assert!(!node.state.focused);
            assert!(!node.state.disabled);
            assert_eq!(node.state.checked, None, "a button is not checkable");
        }

        fn accessibility_reflects_disabled_and_focus() {
            let mut b = Button::new("id", "Save").with_enabled(false);
            b.set_focus(true);
            let node = Widget::accessibility(&b).unwrap();
            assert!(node.state.disabled);
            assert!(node.state.focused);
        }

        fn accessibility_icon_only_button_without_name_is_silent() {
            let b = Button::new("id", "").with_icon(crate::icons::Icon::Check);
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(node.name, None);
        }

        fn accessibility_icon_only_button_uses_accessible_name() {
            let b = Button::new("id", "")
                .with_icon(crate::icons::Icon::Check)
                .with_accessible_name("Confirm");
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(node.name.as_deref(), Some("Confirm"));
        }

        fn accessibility_visible_label_wins_over_accessible_name() {
            let b = Button::new("id", "Save").with_accessible_name("Persist");
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
            let b = Button::new("id", "")
                .with_icon(crate::icons::Icon::Search)
                .tooltip("Search");
            assert_eq!(Widget::tooltip(&b), Some("Search"));
            let plain = Button::new("id", "Save");
            assert_eq!(Widget::tooltip(&plain), None, "no tooltip unless attached");
        }

        fn compact_toolbar_presentation_round_trips_authored_semantics() {
            let mut button = Button::new("publish", "Publish")
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
            let b = Button::new("id", "")
                .with_icon(crate::icons::Icon::Search)
                .tooltip("Search");
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(node.name.as_deref(), Some("Search"));
        }

        fn explicit_accessible_name_wins_over_tooltip() {
            let b = Button::new("id", "")
                .with_icon(crate::icons::Icon::MoreHorizontal)
                .with_accessible_name("View options")
                .tooltip("More");
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(node.name.as_deref(), Some("View options"));
        }

        fn icon_only_effect_publishes_its_spoken_name() {
            let doc = AutoCommit::new();
            let named = Button::new("private", "")
                .with_icon(crate::icons::Icon::Shield)
                .with_accessible_name("New Private Window")
                .on_effect(|| {});
            let tooltip_after = Button::new("search", "")
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

    #[test]
    fn keys_plain_ink_and_bound_state() {
        fn a_key_rests_on_a_faint_lift_with_a_title_label() {
            let theme = Theme::dark();
            let ground = theme.tokens.colors.surfaces.content;
            let (rest, edge, _) = face_colors(&theme, ButtonRole::Key, ButtonInteract::default());
            assert_ne!(rest, ground, "a key is lifted off its ground");
            assert!(edge.is_none());
            let (hover, _, _) = face_colors(
                &theme,
                ButtonRole::Key,
                ButtonInteract {
                    hovered: true,
                    ..ButtonInteract::default()
                },
            );
            assert_ne!(hover, rest);
            let metrics = FaceMetrics::of_role(ControlSize::Regular, ButtonRole::Key, &theme);
            assert_eq!(metrics.label_style, theme.text(TextRole::Title));
        }

        fn a_plain_label_sits_on_its_layout_edge() {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let label = f64::from(
                text.measure_style("Show details", None, &theme.text(TextRole::Label))
                    .0,
            );
            let plain = measured_button_rect(
                Button::new("details", "Show details").role(ButtonRole::Plain),
                1.0,
            );
            let secondary = measured_button_rect(Button::new("details", "Show details"), 1.0);
            assert!(
                (plain.width() - label.ceil()).abs() <= 1.0,
                "a plain button is as wide as its ink: {plain:?} vs {label}"
            );
            assert!(secondary.width() > plain.width());
            let mut button = Button::new("details", "Show details").role(ButtonRole::Plain);
            button.metrics = FaceMetrics::of(ControlSize::Regular, &theme);
            button.apply_layout_external(plain);
            let hit = button.hit_rect();
            assert!(
                hit.x0 < plain.x0 && hit.x1 > plain.x1,
                "the hit area reaches out"
            );
        }

        fn closed_glyphs_fill_when_on() {
            assert!(fills_when_on(crate::icons::Icon::Star));
            assert!(fills_when_on(crate::icons::Icon::Pin));
            assert!(!fills_when_on(crate::icons::Icon::Refresh));
        }

        fn icons_grow_with_the_text_scale() {
            let one = FaceMetrics::of(ControlSize::Regular, &Theme::dark());
            let two = FaceMetrics::of(ControlSize::Regular, &Theme::dark().with_text_scale(2.0));
            assert!(two.icon > one.icon);
            assert!(two.icon <= two.height);
        }

        fn enabled_follows_its_state_and_bounds_are_spoken() {
            let can_save = crate::LocalState::new(false);
            let mut button = Button::new("save", "Save").enabled_from(&can_save);
            assert!(!Widget::is_enabled(&button));
            can_save.set(true);
            Widget::sync(&mut button, &AutoCommit::new(), &automerge::ROOT);
            assert!(Widget::is_enabled(&button));
            button.apply_layout_external(Rect::new(4.0, 8.0, 84.0, 40.0));
            let node = Widget::accessibility(&button).expect("semantics");
            assert!(node.bounds.is_some());
        }

        a_key_rests_on_a_faint_lift_with_a_title_label();
        a_plain_label_sits_on_its_layout_edge();
        closed_glyphs_fill_when_on();
        icons_grow_with_the_text_scale();
        enabled_follows_its_state_and_bounds_are_spoken();
    }
}

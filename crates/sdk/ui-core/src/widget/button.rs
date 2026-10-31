use crate::i18n;
use crate::num::f32_of_f64;
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
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent interaction/lifecycle flag (hover, press, focus, enabled, cache-dirty), not a combinatorial state machine"
)]
pub struct Button {
    id: String,
    label: String,
    /// A labelled button that opens a menu shows a trailing chevron, so it
    /// reads as a choice rather than an action.
    pub(crate) opens_menu: bool,
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
    /// A picture drawn in the icon's place (an icon-only button), with an
    /// optional badge pill on its corner: what an extension's toolbar action
    /// shows. The glyph in `icon` remains the fallback and the layout key.
    image: Option<vello::peniko::ImageData>,
    /// A media transport's control (previous, play/pause, next), drawn as
    /// its [`crate::widget::TransportFace`] at that face's side.
    transport: Option<(crate::widget::TransportControl, f64)>,
    badge: Option<String>,
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
    native_resource: Option<tilo_actions::ResourceSpec>,
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
    /// The last painted body, border and label; a move only translates it.
    face: crate::widget::CachedPaint,
    dirty: bool,
    layout_dirty: bool,
    /// Whether the layout solver has ever placed it. A button only its host
    /// places ([`Self::apply_layout_external`]) has no node to measure.
    solver_placed: bool,
    i18n_revision: u64,
    interaction: crate::interaction::InteractionEnvironment,
    /// Snapshot the cache was emitted from; any change invalidates it.
    last_state: (bool, bool, bool, bool), // (hovered, pressed, focused, enabled)
    /// The theme roles the cached face was painted from, so a theme change
    /// that keeps the layout (dark to light, a new emphasis) repaints it.
    last_theme: Option<ThemeKey>,
    /// The container's motion between states, on the toolkit's clock, as
    /// the theme's painter set moves it ([`crate::part_paint`]).
    motion: crate::part_paint::PartMotion,
    /// The shape the theme's painter set drew the container as, relative
    /// to `rect`'s origin: where the button takes the pointer. `None`: its
    /// hit rect.
    painted_hit: Option<vello::kurbo::BezPath>,
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
    u64,
);

fn theme_key(theme: &Theme) -> ThemeKey {
    let colors = &theme.tokens.colors;
    (
        colors.surfaces.raised,
        colors.intent.accent,
        colors.intent.on_accent,
        colors.text_primary,
        colors.semantic.danger,
        colors.intent.accent_ink,
        theme.tokens.radii.control.to_bits(),
        theme
            .painter(crate::tokens::SurfaceRole::Control)
            .map_or(0, crate::part_paint::PainterSet::id),
    )
}

impl Button {
    /// Box this widget in the SDK so thin apps reuse its shared implementation.
    /// This keeps the widget's vtable instantiation out of the app.
    #[must_use]
    pub fn into_widget(self) -> Box<dyn crate::widget::Widget> {
        Box::new(self)
    }

    /// Focus a person needs to be shown: keyboard focus, not the focus a
    /// click on the button gave it.
    fn focus_visible(&self) -> bool {
        self.is_focused && !self.pressed_into_focus && crate::focus_visible::shown()
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
            image: None,
            transport: None,
            badge: None,
            fill_width: false,
            role: ButtonRole::Secondary,
            toggled: None,
            opens_menu: false,
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
            face: crate::widget::CachedPaint::default(),
            dirty: true,
            layout_dirty: false,
            solver_placed: false,
            i18n_revision: crate::i18n::I18nManager::revision(),
            interaction: crate::interaction::InteractionEnvironment::default(),
            last_state: (false, false, false, true),
            last_theme: None,
            motion: crate::part_paint::PartMotion::new(crate::part_paint::States::REST),
            painted_hit: None,
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

    /// [`Self::on_effect`] for an effect whose scope is the person's own
    /// device and nothing shared: copying to their clipboard, applying their
    /// own appearance, starting a draft of their own. A room's grants have
    /// nothing to consent to, so a guest presses it as the room's owner
    /// does ([`tilo_actions::OWN_DEVICE_SCOPE`]); a shared change it goes on
    /// to make is decided as that change's own action.
    #[must_use]
    pub fn on_own_device_effect<F: FnMut() + 'static>(self, handler: F) -> Self {
        self.on_native_scoped(tilo_actions::OperationKind::Effect, true, handler)
    }

    /// Publish a zero-argument transient presentation action. View actions are
    /// discoverable and agent-driveable but intentionally absent from document
    /// history and collaboration logs.
    #[must_use]
    pub fn on_view<F: FnMut() + 'static>(self, handler: F) -> Self {
        self.on_native(tilo_actions::OperationKind::View, handler)
    }

    fn on_native<F: FnMut() + 'static>(
        self,
        kind: tilo_actions::OperationKind,
        handler: F,
    ) -> Self {
        self.on_native_scoped(kind, false, handler)
    }

    fn on_native_scoped<F: FnMut() + 'static>(
        self,
        kind: tilo_actions::OperationKind,
        own_device: bool,
        handler: F,
    ) -> Self {
        self.on_native_boxed(kind, own_device, Box::new(handler))
    }

    /// [`Self::on_native_scoped`] once the handler is boxed: the toolkit
    /// compiles publishing the press once, rather than every app once for
    /// each closure it hands a button.
    #[expect(
        clippy::expect_used,
        reason = "String serialization and lookup of the just-created action are infallible"
    )]
    fn on_native_boxed(
        mut self,
        kind: tilo_actions::OperationKind,
        own_device: bool,
        handler: Box<dyn FnMut()>,
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
        // Pressing the button is the control's own verb: how agents and
        // scripts press what a person presses directly, never a row of its
        // own in people's lists, where it would repeat the app's actions.
        let mut spec =
            tilo_actions::ActionSpec::operation("invoke", label.clone(), kind, TYPE_ID, &self.id)
                .expect("a control id is always JSON serializable")
                .for_audience(tilo_actions::Audience::Agents);
        if own_device {
            spec.type_mut()
                .contract
                .tags
                .push(tilo_actions::OWN_DEVICE_SCOPE.to_owned());
        }
        let resource =
            tilo_actions::ResourceSpec::new(resource_id, "tilo.control", label).with_action(spec);
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "invoke")
            .expect("the synthesized invoke action is nullary");
        self = self.on_action(binding);
        self.native_resource = Some(resource);
        self.native_handler = Some(handler);
        self
    }

    /// With an empty label the icon is centred (icon-only button); with
    /// a label it sits to its left.
    #[must_use]
    pub fn with_icon(mut self, icon: crate::icons::Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Draw `image` where the icon would be, and a small pill with `badge`
    /// on its bottom-right corner. Only an icon-only button shows either.
    #[must_use]
    pub fn with_image(mut self, image: vello::peniko::ImageData, badge: Option<&str>) -> Self {
        self.image = Some(image);
        self.badge = badge.filter(|text| !text.is_empty()).map(str::to_owned);
        self.dirty = true;
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

    /// A media transport's control: previous, play/pause or next, drawn as
    /// the transport draws it (play/pause the one round, prominent control)
    /// and named for what it does now.
    pub fn transport(id: &str, control: crate::widget::TransportControl) -> Self {
        let mut button = Self::icon_only(id, control.icon(), control.name());
        button.transport = Some((control, 0.0));
        button
    }

    /// Show `control` now (play while paused, pause while playing).
    pub fn set_transport(&mut self, control: crate::widget::TransportControl) {
        if self.transport.map(|(shown, _)| shown) == Some(control) {
            return;
        }
        let side = self.transport.map_or(0.0, |(_, side)| side);
        self.transport = Some((control, side));
        self.icon = Some(control.icon());
        self.set_accessible_name(control.name());
        self.tooltip = Some(control.name().to_owned());
        self.dirty = true;
    }

    /// Show `icon` now, for a control whose glyph follows its state (a
    /// speaker that is muted, a repeat that repeats one).
    pub fn set_icon(&mut self, icon: crate::icons::Icon) {
        if self.icon != Some(icon) {
            self.icon = Some(icon);
            self.dirty = true;
        }
    }

    /// An icon-only button: `icon` is what it shows, `label` is what a
    /// screen reader announces and what its tooltip says. It is a square of
    /// its [`ControlSize`]'s height (never under the minimum pointer target)
    /// and, like every toolbar glyph, a [`ButtonRole::Plain`] button unless
    /// a role is given.
    #[track_caller]
    pub fn icon_only(id: &str, icon: crate::icons::Icon, label: &str) -> Self {
        debug_assert!(
            !label.trim().is_empty(),
            "an icon-only button needs its accessible name: it is also its tooltip"
        );
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

    /// What the button does, which decides its whole look.
    #[must_use]
    pub const fn role_of(&self) -> ButtonRole {
        self.role
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

    /// Follow `state` for whether the button can act, as `ready` reads it:
    /// "Create" enabled while the draft beside it parses.
    #[must_use]
    pub fn enabled_while<S: Clone + 'static>(
        mut self,
        state: &crate::LocalState<S>,
        ready: impl Fn(&S) -> bool + 'static,
    ) -> Self {
        let (enabled, binding) = super::binding::derived(state, ready);
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
/// | `Bar` | a control of the desktop's top bar or a window's bar | the bar control ([`crate::widget::BarControlFace`]): nothing at rest, half a lift under the pointer, a whole lift while pressed or on, its icon in secondary ink until then |
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
    /// A control of the desktop's top bar or a window's bar: the bar
    /// control's face and lifts ([`crate::widget::BarControlFace`]).
    Bar,
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
        colors.intent.on_accent,
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
        return if matches!(role, ButtonRole::Plain | ButtonRole::Bar) {
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
                intent.accent,
                intent.accent_hover,
                colors.intent.accent_pressed,
            );
            let fill = if interact.pressed {
                pressed
            } else if interact.hovered {
                hover
            } else {
                rest
            };
            // The label of the inverted fill is the theme's own
            // (`intent.on_accent`, the window ground), resolved over every
            // fill so it does not change under the pointer.
            let ink = colors.ink_on_any(
                &[rest, hover, pressed],
                intent.on_accent,
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
            // A raised face has no edge: the level step separates it
            // (`docs/design-language.md`, "Surfaces").
            (fill, None, ink)
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
        ButtonRole::Bar => {
            // The bar has no surface of its own: its controls stand on the
            // ground and lift off it, half under the pointer, whole while
            // pressed (`BarControlFace::fill`).
            let ground = colors.surfaces.content;
            let (hover, pressed) = (colors.lift_on(ground, 0.5), colors.lift_on(ground, 1.0));
            let fill = if interact.pressed {
                pressed
            } else if interact.hovered {
                hover
            } else {
                Color::TRANSPARENT
            };
            let ink = colors.ink_on_any(
                &[ground, hover, pressed],
                colors.text_primary,
                ON_FILL_MIN_CONTRAST,
            );
            (fill, None, ink)
        }
        ButtonRole::Plain => {
            // Hover and press are neutral lifts of the ground: never the
            // accent, which is for what is selected.
            let pressed = colors.lift_on(colors.surfaces.content, 1.0);
            let fill = if interact.pressed {
                pressed
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
                &[colors.surfaces.content, intent.selection_hover, pressed],
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
        // Lucide at 16 in every small and regular control, 20 in a large one.
        let (pad_x, icon) = match size {
            ControlSize::Small => (spacing.s2, metrics.icon_control),
            ControlSize::Regular => (spacing.s3, metrics.icon_control),
            ControlSize::Large => (spacing.s4, metrics.icon_bar),
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
        if role == ButtonRole::Bar {
            // The top bar's control: a label line with air above and below
            // (the bar control less one `s1`), and the bar control's inset,
            // whatever size is asked.
            let tokens = &theme.tokens;
            metrics.height = f64::from(tokens.metrics.bar_control_height - tokens.spacing.s1);
            metrics.pad_x = f64::from(tokens.spacing.rhythm.related);
            metrics.icon = f64::from(tokens.metrics.icon_control);
            metrics.label_style = theme.text(crate::tokens::TextRole::Label);
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
            image: None,
            badge: None,
            label,
            hint: None,
            flush: false,
            filled_icon: false,
            menu: false,
        }
        .intrinsic_width(&self.metrics, text, self.metrics.height)
    }
}

/// A badge pill on the bottom-right corner of `rect`.
fn paint_corner_badge(ctx: &mut PaintCtx<'_>, rect: Rect, label: &str) {
    use crate::widget::badge::BadgeFace;
    let height = BadgeFace::height(ctx.theme);
    let width = BadgeFace::intrinsic_width(label, ctx.text, ctx.theme).min(rect.width());
    let pill = Rect::new(rect.x1 - width, rect.y1 - height, rect.x1, rect.y1);
    BadgeFace::new(pill, label).paint(ctx.scene, ctx.transform, ctx.text, ctx.theme);
}

/// What sits inside a button's face: an optional leading icon, the label and
/// an optional trailing shortcut hint.
struct FaceContent<'a> {
    icon: Option<crate::icons::Icon>,
    /// Painted instead of `icon` on an icon-only face.
    image: Option<&'a vello::peniko::ImageData>,
    badge: Option<&'a str>,
    label: &'a str,
    hint: Option<String>,
    /// A labelled plain face: no inner padding, the ink on the layout edge.
    flush: bool,
    /// Draw the icon filled (a toggle that is on, with a closed glyph).
    filled_icon: bool,
    /// A trailing chevron after a label: the face opens a menu.
    menu: bool,
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
        if self.menu && !self.label.is_empty() {
            width += metrics.icon_gap + metrics.icon;
        }
        if self.flush {
            width.ceil()
        } else {
            width.ceil().max(2.0 * height)
        }
    }

    /// `image` stretched over `rect`.
    fn draw_image(&self, ctx: &mut PaintCtx<'_>, image: &vello::peniko::ImageData, rect: Rect) {
        let (width, height) = (f64::from(image.width), f64::from(image.height));
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let place = Affine::translate((rect.x0, rect.y0))
            * Affine::scale_non_uniform(rect.width() / width, rect.height() / height);
        ctx.scene.fill(
            vello::peniko::Fill::NonZero,
            ctx.transform,
            &vello::peniko::ImageBrush::new(image.clone()),
            Some(place),
            &rect,
        );
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
            if let Some(image) = self.image {
                let ix = rect.center().x - icon_px / 2.0;
                let iy = rect.center().y - icon_px / 2.0;
                self.draw_image(ctx, image, Rect::new(ix, iy, ix + icon_px, iy + icon_px));
                if let Some(badge) = self.badge {
                    paint_corner_badge(ctx, rect, badge);
                }
                return;
            }
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
        let trail = if self.menu {
            metrics.icon_gap + icon_px
        } else {
            0.0
        };
        let room = (rect.width() - 2.0 * pad - lead - trail).max(1.0);
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
                .elide_to_width_style(self.label, f32_of_f64(label_room.max(1.0) + 0.5), style);
        let (label_w, label_h) = ctx.text.measure_style(&label, None, style);
        let hint_w = hint.map_or(0.0, |(_, w)| metrics.hint_gap + w);
        let cluster = lead + f64::from(label_w) + hint_w + trail;
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
        if self.menu {
            let ix = label_x + f64::from(label_w) + hint_w + metrics.icon_gap;
            let iy = rect.center().y - icon_px / 2.0;
            crate::icons::draw_icon(
                ctx.scene,
                ctx.transform,
                Rect::new(ix, iy, ix + icon_px, iy + icon_px),
                crate::icons::Icon::ChevronDown,
                ink,
                crate::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
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
        ButtonRole::Secondary
        | ButtonRole::Destructive
        | ButtonRole::Plain
        | ButtonRole::Key
        | ButtonRole::Bar => {
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
    /// Where the container is in its motion between states.
    moment: crate::part_paint::Moment,
}

/// The container states of a face in `interact`, `on` for a toggle that is
/// on.
fn container_states(interact: ButtonInteract, on: bool) -> crate::part_paint::States {
    use crate::part_paint::States;
    States::REST
        .with(States::HOVERED, interact.hovered)
        .with(States::PRESSED, interact.pressed)
        .with(States::FOCUSED, interact.focused)
        .with(States::DISABLED, interact.disabled)
        .with(States::ON, on)
}

impl FacePaint<'_> {
    /// Paint the face. Returns the shape the theme's painter set drew the
    /// container as, where it drew one: the button takes the pointer there.
    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Option<vello::kurbo::BezPath> {
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
        let set = theme.painter(crate::tokens::SurfaceRole::Control);
        // Disabled is the rest face under one group alpha (HIG §9.5), the
        // same signal every other control gives. A painter set's container
        // may take any shape in the face's rect.
        if disabled {
            let clip = if set.is_some() {
                RoundedRect::from_rect(face_rect, 0.0)
            } else {
                shape
            };
            ctx.scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                ctx.transform,
                &clip,
            );
        }
        let painted = set.and_then(|set| {
            crate::part_paint::paint_part(
                ctx.scene,
                ctx.transform,
                face_rect,
                (
                    set,
                    crate::part_paint::Part::ButtonContainer,
                    crate::tokens::SurfaceRole::Control,
                ),
                self.moment,
                &crate::part_paint::part_tokens(
                    theme,
                    crate::tokens::SurfaceRole::Control,
                    (fill, ink, edge.unwrap_or(vello::peniko::Color::TRANSPARENT)),
                    theme.tokens.colors.surfaces.content,
                ),
                theme,
            )
        });
        // A painted container draws its own edge; a border stands in for
        // the rest edge.
        let bordered = painted.is_some()
            || crate::surface::control_face(ctx.scene, ctx.transform, &shape, fill, theme);
        let edge = if bordered { None } else { edge };
        let hit = painted.and_then(|painted| painted.hit);
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
            let ring = theme.tokens.colors.intent.accent_ink;
            // The ring follows a painted shape.
            if let Some(hit) = &hit {
                ctx.scene.stroke(
                    &vello::kurbo::Stroke::new(FOCUS_RING_WIDTH),
                    ctx.transform,
                    ring,
                    None,
                    hit,
                );
                return Some(hit.clone());
            }
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(FOCUS_RING_WIDTH),
                ctx.transform,
                ring,
                None,
                &outline,
            );
            // Your colour on a fill of your colour (a primary face, an "on"
            // toggle) would only make the face look larger: a line of the
            // ground both are made to stand out from parts the two.
            if fill.components[3] > 0.0
                && crate::a11y::contrast_ratio(ring, fill)
                    < crate::tokens::colors::NON_TEXT_MIN_CONTRAST
            {
                let gap = f64::from(theme.tokens.metrics.chrome_rule_px);
                let inset = FOCUS_RING_WIDTH - 0.5 + gap / 2.0;
                ctx.scene.stroke(
                    &vello::kurbo::Stroke::new(gap),
                    ctx.transform,
                    theme.tokens.colors.surfaces.content,
                    None,
                    &RoundedRect::from_rect(face_rect.inset(-inset), (radius - inset).max(0.0)),
                );
            }
        } else if let Some(edge) = edge {
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(REST_BORDER_WIDTH),
                ctx.transform,
                edge,
                None,
                &outline,
            );
        }
        hit
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
            image: None,
            badge: None,
            label: &label,
            hint: self.shortcut.as_ref().map(shortcut_text),
            flush: false,
            filled_icon: false,
            menu: false,
        }
        .intrinsic_width(&metrics, text, metrics.height)
    }
    /// Where this face draws its icon: on a labelled face it leads the
    /// label, the two centred together. A host that paints its own mark in
    /// the icon's place (a colour swatch over the choice it names) puts it
    /// exactly here, so the two never stand side by side. Ignores a
    /// shortcut hint.
    pub fn icon_rect(&self, text: &mut TextRenderer, theme: &Theme) -> Option<Rect> {
        self.icon?;
        let metrics = FaceMetrics::of_role(self.control_size, self.role, theme);
        let rect = self.rect;
        let icon_px = metrics.icon.min(rect.height());
        let iy = rect.center().y - icon_px / 2.0;
        let label = localized_label(self.label);
        if label.is_empty() {
            let ix = rect.center().x - icon_px / 2.0;
            return Some(Rect::new(ix, iy, ix + icon_px, iy + icon_px));
        }
        let lead = icon_px + metrics.icon_gap;
        let room = (rect.width() - 2.0 * metrics.pad_x - lead).max(1.0);
        let style = &metrics.label_style;
        let shown = text.elide_to_width_style(&label, f32_of_f64(room + 0.5), style);
        let label_w = f64::from(text.measure_style(&shown, None, style).0);
        let start_x = rect.center().x - (lead + label_w) / 2.0;
        Some(Rect::new(start_x, iy, start_x + icon_px, iy + icon_px))
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

/// How far past a control's face its ink may reach under `theme`: the focus
/// ring, the ground-coloured line that parts it from a fill of your colour,
/// and their antialiasing. A 2 px mark stroked centred on a rect reaches no
/// further, so a region drawn as a part of its own pads its bounds by this.
#[must_use]
pub fn focus_ring_reach(theme: &Theme) -> f64 {
    FOCUS_RING_WIDTH + f64::from(theme.tokens.metrics.chrome_rule_px) + 0.5
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
                image: None,
                badge: None,
                label: &label,
                hint: face.shortcut.as_ref().map(shortcut_text),
                // A painted face fills the rect its host gave it.
                flush: false,
                filled_icon: face
                    .icon
                    .is_some_and(|icon| icon.is_brand() || (face.on && fills_when_on(icon))),
                menu: false,
            },
            metrics: &metrics,
            moment: crate::part_paint::Moment::settled(container_states(face.interact, face.on)),
        }
        .paint(&mut PaintCtx::new(scene, transform, text_renderer, theme));
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
        let height = self.transport.map_or(self.metrics.height, |(_, side)| side);
        if self.interaction.supports_hover() {
            height
        } else {
            height.max(f64::from(self.interaction.minimum_target()))
        }
    }

    /// The face's width with everything shown, measured with the geometry
    /// captured in `build_layout`.
    fn intrinsic_width(&self, text: &mut TextRenderer) -> f32 {
        if self.transport.is_some() {
            return f32_of_f64(self.face_height());
        }
        let (label, hint) = self.content();
        f32_of_f64(
            FaceContent {
                flush: self.role.flush() && !label.is_empty(),
                icon: self.icon,
                image: None,
                badge: None,
                label: &label,
                hint,
                filled_icon: false,
                menu: self.opens_menu,
            }
            .intrinsic_width(&self.metrics, text, self.face_height()),
        )
    }

    /// Whether `point` activates the button: inside the shape the theme's
    /// painter set drew it as, or else its hit rect ([`Self::hit_rect`]).
    fn takes(&self, point: Point) -> bool {
        match &self.painted_hit {
            Some(hit) => crate::part_paint::hit_contains(hit, point - self.rect.origin().to_vec2()),
            None => self.hit_rect().contains(point),
        }
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

    /// The face's height on the node, as the input asks for it now: a button made before the
    /// window told it a finger reaches it grows to the finger's target when it is laid out
    /// again, not only when its node is built anew.
    fn pin_face_height(&self, taffy: &mut TaffyTree<()>, node: NodeId) -> bool {
        let Ok(style) = taffy.style(node) else {
            return false;
        };
        let height = f32_of_f64(self.face_height());
        if style.size.height == Dimension::length(height) {
            return false;
        }
        let mut style = style.clone();
        style.size.height = Dimension::length(height);
        style.min_size = Size::from_lengths(height, height);
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
        dyn crate::presence::capability::PublishesPresence,
        dyn crate::widget::ResourceOperations
    );

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        if self.interaction != environment {
            self.interaction = environment;
            self.is_hovered &= environment.supports_hover();
            // An icon's square and a label's padding follow the face's height.
            self.measured_width = None;
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
        if let Some((control, side)) = &mut self.transport {
            *side = crate::widget::TransportFace::side(*control, theme);
        }
        if metrics != self.metrics {
            // A theme scale change re-shapes the label; the cached face must
            // not survive at the old metrics. (The live App rebuilds widgets
            // on a theme change; a relayout-only host such as
            // `FrameHarness::set_theme` relies on this.)
            self.dirty = true;
            self.metrics = metrics;
        }
        let height = f32_of_f64(self.face_height());
        let style = Style {
            // An intrinsic button never shrinks below its label: a parent
            // with room shows it whole, and only a parent without room
            // (capped by `max_size`) makes the painter elide it.
            flex_shrink: 0.0,
            min_size: Size::from_lengths(height, height),
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
            height: known
                .height
                .unwrap_or_else(|| f32_of_f64(self.face_height())),
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
        let tall = self.pin_face_height(taffy, node);
        if self.fill_width {
            return tall;
        }
        if self.measured_width.is_none() {
            self.measured_width = Some(self.intrinsic_width(text));
        }
        self.pin_measured_width(taffy, node) | tall
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.solver_placed = true;
        if self.face.resized(self.rect) {
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
        let interact = ButtonInteract {
            hovered: self.is_hovered,
            pressed: self.press.is_held(),
            focused: self.focus_visible(),
            disabled: !self.effectively_enabled(),
        };
        let on = self.is_toggled();
        let now = crate::time::now();
        let set = theme.painter(crate::tokens::SurfaceRole::Control);
        // The toolkit's own button changes state at once.
        self.motion.go(
            container_states(interact, on),
            (set, crate::part_paint::Part::ButtonContainer),
            crate::part_paint::Transition::None,
            theme.reduce_motion,
            now,
        );
        let moment = self.motion.at(now);
        // The disabled group alpha is painted into the face, so appending
        // it applies none: a second layer would dim a disabled face twice.
        if !self.dirty && moment.settled && !self.face.is_empty() {
            self.face.append(out_scene, out_transform, self.rect);
            crate::widget::draw_gate_and_peer_focus(
                self,
                out_scene,
                out_transform,
                theme,
                presence,
            );
            return;
        }
        let enabled = self.effectively_enabled();
        // The geometry is read from the theme handed in, so a host that
        // positions the button without a `build_layout` pass still gets the
        // theme's type; `measure` uses the copy captured in `build_layout`,
        // the same value on every taffy-driven path.
        let metrics = FaceMetrics::of_role(self.control_size, self.role, theme);
        if let Some((control, _)) = self.transport {
            let face = crate::widget::TransportFace::new(self.rect, control)
                .hovered(self.is_hovered)
                .pressed(self.press.is_held())
                .focused(self.focus_visible())
                .disabled(!enabled);
            crate::widget::TransportFace::paint(
                self.face.repaint(self.rect),
                Affine::IDENTITY,
                theme,
                &face,
            );
            self.painted_hit = None;
            self.face.append(out_scene, out_transform, self.rect);
            crate::widget::draw_gate_and_peer_focus(
                self,
                out_scene,
                out_transform,
                theme,
                presence,
            );
            return;
        }
        let (label, hint) = self.content();
        let hit = FacePaint {
            rect: self.rect,
            role: self.role,
            confirms: false,
            on,
            interact: ButtonInteract {
                disabled: !enabled,
                ..interact
            },
            content: FaceContent {
                flush: self.role.flush() && !label.is_empty(),
                icon: self.icon,
                image: self.image.as_ref(),
                badge: self.badge.as_deref(),
                label: &label,
                hint,
                filled_icon: self
                    .icon
                    .is_some_and(|icon| icon.is_brand() || (on && fills_when_on(icon))),
                menu: self.opens_menu,
            },
            metrics: &metrics,
            moment,
        }
        .paint(&mut PaintCtx::new(
            self.face.repaint(self.rect),
            Affine::IDENTITY,
            text_renderer,
            theme,
        ));
        self.painted_hit = hit.map(|hit| Affine::translate(-self.rect.origin().to_vec2()) * hit);
        self.face.append(out_scene, out_transform, self.rect);
        crate::widget::draw_gate_and_peer_focus(self, out_scene, out_transform, theme, presence);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.motion.moving(crate::time::now())
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
    /// cache's append transform, no taffy involvement). The sentinel is the
    /// laid-out rect, not the paint cache's: a button that is not drawn
    /// (scrolled or panned out of view) has nothing new to measure.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
            || self.layout_dirty
            || self.i18n_revision != crate::i18n::I18nManager::revision()
    }

    fn layout_needs_measure(&self) -> bool {
        // A missing paint cache asks for drawing, not another intrinsic
        // measurement. Geometry is available as soon as layout is applied.
        // A button only its host places has no node for the solver to
        // measure: the host sizes it, however hidden or unplaced it is.
        self.solver_placed
            && (self.rect == Rect::ZERO
                || self.layout_dirty
                || self.i18n_revision != crate::i18n::I18nManager::revision())
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
                let logical_pos = ctx.logical(position);
                let hover = self.takes(logical_pos);
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

    /// This button as a slot in the window's bar, sent back as `id`: its
    /// icon in the bar's row, or an entry of the bar's menu by its label
    /// when it has no icon. `None` for a button with no name to show.
    fn window_slot(&self, id: String) -> Option<crate::window_toolbar::ToolbarAction> {
        let name = self.accessibility()?.name.filter(|name| !name.is_empty())?;
        let icon = self.icon.map_or("", crate::icons::Icon::name);
        let action = crate::window_toolbar::ToolbarAction::new(id, icon, name)
            .enabled(self.is_enabled())
            .on(self.toggled == Some(true));
        Some(if self.icon.is_some() {
            action
        } else {
            action.in_menu()
        })
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
        if self.takes(point) {
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
        // The key the name was resolved from: the app's own copy when its
        // catalogue holds it.
        let mut key = self.label.as_str();
        let name = if label.is_empty() {
            // Icon-only: prefer the explicit accessible name, else fall
            // back to the tooltip text so the glyph is never silent when
            // a tooltip is set.
            key = self
                .accessible_name
                .as_deref()
                .or(self.tooltip.as_deref())
                .unwrap_or_default();
            self.accessible_name
                .as_ref()
                .map(|n| i18n!(n))
                .or_else(|| self.tooltip.as_ref().map(|t| i18n!(t)))
                .filter(|n| !n.is_empty())
        } else {
            Some(label)
        };
        let interface = name.is_some() && crate::i18n::I18nManager::has_message(key);
        let role = if self.toggled.is_some() {
            Role::ToggleButton
        } else {
            Role::Button
        };
        let node = AccessNode::new(role)
            .with_semantic_key(&self.id)
            .with_bounds(self.rect)
            .with_optional_name(name)
            .with_interface_name(interface)
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

impl crate::widget::ResourceOperations for Button {
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
    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
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
                    action.type_mut().label.clone_from(name);
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
mod tests;

mod component;
pub use component::ButtonProps;

#[cfg(test)]
#[path = "button_touch_tests.rs"]
mod touch_tests;

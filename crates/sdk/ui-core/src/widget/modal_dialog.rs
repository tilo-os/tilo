//! The one modal: a sheet over a scrim that covers the whole window.
//!
//! [`ModalDialog`] is the retained widget: a title, any content widget, and
//! up to two footer actions. It is always presented in the window's top
//! layer ([`Widget::draw_overlay`]), after the app and the window's own
//! chrome (the History bar, sidebars), so its scrim covers all of them
//! wherever in the tree the dialog is mounted. Escape or a click on the scrim
//! dismisses; Enter fires the primary action unless the content consumes it.
//!
//! Immediate-mode hosts that own their own state (the identity ceremonies,
//! shell sheets) use the same chrome through [`layout`], [`paint_backdrop`]
//! and [`paint_actions`]:
//!
//! ```ignore
//! let layout = modal_dialog::layout(window, &spec, text_renderer, theme);
//! modal_dialog::paint_backdrop(scene, transform, text_renderer, theme, &layout, &spec);
//! // ... caller draws its own content inside `layout.body` ...
//! modal_dialog::paint_actions(scene, transform, text_renderer, theme, &layout, &spec);
//! ```
//!
//! The sheet is a floating Panel ([`super::floating_surface`]): panel radius,
//! the blurred material, one soft shadow. Its height follows its content;
//! its title is a `Title`; its actions are Regular buttons anchored to the
//! footer in the platform's order. Dialog callbacks are host-supplied
//! closures and do not flow through the document.

use crate::widget::{AppModel, ModelPath};
use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::Icon;
use crate::platform::{DialogActionOrder, PlatformConventions};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::Widget;
use crate::widget::button::{Button, ButtonFace, ButtonStyle};
use crate::widget::control_size::ControlSize;
use crate::widget::{TaffyTreeExt, floating_surface};

/// Widest a modal sheet is drawn, in design pixels (grown with the text
/// scale). Past this a single column of text stops being one column.
pub const MODAL_MAX_WIDTH: f64 = 520.0;
/// Narrowest a modal sheet is drawn, in design pixels, before it elides.
pub const MODAL_MIN_WIDTH: f64 = 320.0;
/// Contrast a modal's own ink must clear against the sheet before it is
/// used as-is. WCAG 2.1 AA for body text.
const MODAL_MIN_INK_CONTRAST: f64 = 4.5;

/// How an action is weighted in the footer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalEmphasis {
    /// The default action. One per modal, and the one Enter fires.
    Primary,
    /// Everything else: Cancel, and any additional verb.
    Secondary,
    /// A primary action that destroys something. Still what Enter fires.
    Danger,
}

/// One button in the footer.
// Four independent interaction axes (enabled, hovered, pressed, focused)
// that can co-occur in any combination.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent interaction-state flags, not related toggles"
)]
#[derive(Clone, Copy, Debug)]
pub struct ModalAction<'a> {
    pub label: &'a str,
    pub emphasis: ModalEmphasis,
    /// A disabled action is painted dimmed and is not the default action.
    pub enabled: bool,
    pub hovered: bool,
    pub pressed: bool,
    /// Keyboard focus is on this action: it draws the focus ring.
    pub focused: bool,
}

impl<'a> ModalAction<'a> {
    pub fn primary(label: &'a str) -> Self {
        Self::of(label, ModalEmphasis::Primary)
    }
    pub fn secondary(label: &'a str) -> Self {
        Self::of(label, ModalEmphasis::Secondary)
    }
    pub fn danger(label: &'a str) -> Self {
        Self::of(label, ModalEmphasis::Danger)
    }
    fn of(label: &'a str, emphasis: ModalEmphasis) -> Self {
        Self {
            label,
            emphasis,
            enabled: true,
            hovered: false,
            pressed: false,
            focused: false,
        }
    }
    #[must_use]
    pub fn enabled(mut self, v: bool) -> Self {
        self.enabled = v;
        self
    }
    #[must_use]
    pub fn hovered(mut self, v: bool) -> Self {
        self.hovered = v;
        self
    }
    #[must_use]
    pub fn pressed(mut self, v: bool) -> Self {
        self.pressed = v;
        self
    }
    #[must_use]
    pub fn focused(mut self, v: bool) -> Self {
        self.focused = v;
        self
    }

    fn is_default(&self) -> bool {
        matches!(
            self.emphasis,
            ModalEmphasis::Primary | ModalEmphasis::Danger
        )
    }
}

/// What a modal is being asked to frame.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent spec flags set by unrelated callers, not related toggles"
)]
#[derive(Clone, Debug)]
pub struct ModalSpec<'a> {
    /// The sheet's title, set in the `Title` role.
    pub title: &'a str,
    /// Draw a close control. Escape always dismisses; this is its visible
    /// half, for a sheet with no Cancel action.
    pub closable: bool,
    /// Actions in semantic authoring order. Geometry follows
    /// [`Self::platform`], so callers do not branch on native button order.
    pub actions: &'a [ModalAction<'a>],
    /// Native shortcut and dialog presentation conventions.
    pub platform: PlatformConventions,
    /// Height the caller's body content wants, in the same logical pixels
    /// as the rest of the layout (already grown by the text scale).
    pub body_height: f64,
    /// Widest the sheet may be drawn, in design pixels. Defaults to
    /// [`MODAL_MAX_WIDTH`].
    pub max_width: f64,
    pub close_hovered: bool,
    pub close_focused: bool,
    /// Whether a scrim click dismisses. The scrim swallows the click either
    /// way: a surface that lets clicks through is not modal.
    pub dismiss_on_scrim: bool,
}

impl Default for ModalSpec<'_> {
    fn default() -> Self {
        Self {
            title: "",
            closable: true,
            actions: &[],
            platform: PlatformConventions::default(),
            body_height: 0.0,
            max_width: MODAL_MAX_WIDTH,
            close_hovered: false,
            close_focused: false,
            dismiss_on_scrim: true,
        }
    }
}

impl<'a> ModalSpec<'a> {
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            ..Self::default()
        }
    }
    #[must_use]
    pub fn actions(mut self, actions: &'a [ModalAction<'a>]) -> Self {
        self.actions = actions;
        self
    }
    #[must_use]
    pub fn platform_conventions(mut self, platform: PlatformConventions) -> Self {
        self.platform = platform;
        self
    }
    #[must_use]
    pub fn body_height(mut self, h: f64) -> Self {
        self.body_height = h;
        self
    }
    #[must_use]
    pub fn max_width(mut self, w: f64) -> Self {
        self.max_width = w;
        self
    }
    #[must_use]
    pub fn closable(mut self, v: bool) -> Self {
        self.closable = v;
        self
    }
    #[must_use]
    pub fn dismiss_on_scrim(mut self, v: bool) -> Self {
        self.dismiss_on_scrim = v;
        self
    }
    #[must_use]
    pub fn close_hovered(mut self, v: bool) -> Self {
        self.close_hovered = v;
        self
    }
    #[must_use]
    pub fn close_focused(mut self, v: bool) -> Self {
        self.close_focused = v;
        self
    }
}

/// Where every part of the modal landed. Pure geometry, computed without a
/// GPU, so the layout rules are tested directly.
#[derive(Clone, Debug, PartialEq)]
pub struct ModalLayout {
    /// The whole window. The scrim covers this.
    pub scrim: Rect,
    /// The sheet.
    pub card: Rect,
    /// The title's line, from the sheet's leading padding to the close
    /// control (or the trailing padding).
    pub title: Rect,
    /// The close control's hit target, when the spec asked for one.
    pub close: Option<Rect>,
    /// The region the caller owns. Content taller than this scrolls or
    /// elides inside it; see [`Self::body_overflows`].
    pub body: Rect,
    /// One rect per entry in `spec.actions`, in the same order.
    pub actions: Vec<Rect>,
    /// The body wanted more height than a sheet inside the window can give.
    pub body_overflows: bool,
    /// Index of the action Enter fires, if any.
    pub primary: Option<usize>,
    /// Mirrors [`ModalSpec::dismiss_on_scrim`].
    pub dismiss_on_scrim: bool,
}

/// What a point landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalHit {
    Close,
    /// An action, by index into `spec.actions`.
    Action(usize),
    /// Inside the sheet but not on a control. Swallow it.
    Card,
    /// The scrim. Dismiss when [`ModalLayout::dismiss_on_scrim`], otherwise
    /// swallow.
    Scrim,
}

impl ModalLayout {
    /// Resolve a point to what it hit, controls first. `None` only for a
    /// point outside the window entirely.
    pub fn hit(&self, p: Point) -> Option<ModalHit> {
        if let Some(c) = self.close
            && c.contains(p)
        {
            return Some(ModalHit::Close);
        }
        for (i, r) in self.actions.iter().enumerate() {
            if r.area() > 0.0 && r.contains(p) {
                return Some(ModalHit::Action(i));
            }
        }
        if self.card.contains(p) {
            return Some(ModalHit::Card);
        }
        if self.scrim.contains(p) {
            return Some(ModalHit::Scrim);
        }
        None
    }

    /// True for the close control, and for the scrim when the spec allows
    /// it. Everything else is swallowed.
    pub fn dismisses(&self, p: Point) -> bool {
        match self.hit(p) {
            Some(ModalHit::Close) => true,
            Some(ModalHit::Scrim) => self.dismiss_on_scrim,
            _ => false,
        }
    }
}

/// Ink that is legible on the sheet, as `(primary, dim)`: the theme's own
/// when it clears AA against the panel ground, else the contrast-guaranteed
/// on-content ramp.
pub fn ink(theme: &Theme) -> (Color, Color) {
    let ground = crate::chrome::panel_ground(theme);
    if crate::a11y::contrast_ratio(theme.text_primary, ground) >= MODAL_MIN_INK_CONTRAST {
        (theme.text_primary, theme.text_dim)
    } else {
        let on = theme.on_content();
        (on.ink, on.ink_dim)
    }
}

/// The sheet's padding on every side: the content gutter.
fn pad(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s6)
}

/// Width of the sheet for `max_width` (design pixels) inside `window`,
/// knowable before any body row is measured. Widths grow with the text
/// scale, because what fills a sheet is text; the sheet keeps the content
/// gutter clear of every window edge.
pub fn card_width(window: Rect, max_width: f64, theme: &Theme) -> f64 {
    let scale = f64::from(theme.text_scale);
    let avail = (window.width() - pad(theme) * 2.0).max(0.0);
    (max_width * scale)
        .min(avail)
        .max((MODAL_MIN_WIDTH * scale).min(avail))
}

/// Width the caller's body is laid into: the sheet minus its padding.
/// Equal to `layout(..).body.width()` for the same inputs.
pub fn body_width(window: Rect, max_width: f64, theme: &Theme) -> f64 {
    (card_width(window, max_width, theme) - pad(theme) * 2.0).max(0.0)
}

/// Height of one footer action: a Regular control.
pub fn action_height(theme: &Theme) -> f64 {
    ControlSize::Regular.height(theme)
}

/// Height of the title line (and of the close control beside it).
fn title_row_height(theme: &Theme) -> f64 {
    let line = f64::from(theme.text(TextRole::Title).line_height_px());
    line.max(f64::from(theme.tokens.metrics.min_pointer_target))
}

/// Compute the geometry for `spec` inside `window`.
pub fn layout(
    window: Rect,
    spec: &ModalSpec<'_>,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) -> ModalLayout {
    let spacing = &theme.tokens.spacing;
    let pad = pad(theme);
    let w = card_width(window, spec.max_width, theme);
    let has_title = !spec.title.is_empty() || spec.closable;
    let title_h = if has_title {
        title_row_height(theme)
    } else {
        0.0
    };
    let title_gap = if has_title {
        f64::from(spacing.s4)
    } else {
        0.0
    };
    let button_h = action_height(theme);
    let footer_h = if spec.actions.is_empty() {
        0.0
    } else {
        f64::from(spacing.s6) + button_h
    };
    let chrome_h = pad * 2.0 + title_h + title_gap + footer_h;
    let wanted_h = chrome_h + spec.body_height.max(0.0);
    let avail_h = (window.height() - pad * 2.0).max(0.0);
    let h = wanted_h.min(avail_h);
    // The sheet never grows past the window: a footer nobody can reach is
    // worse than a body that scrolls.
    let body_overflows = wanted_h > avail_h + 0.5;

    let c = window.center();
    let card = Rect::new(c.x - w / 2.0, c.y - h / 2.0, c.x + w / 2.0, c.y + h / 2.0);
    let title_top = card.y0 + pad;

    let close = spec.closable.then(|| {
        let side = f64::from(theme.tokens.metrics.min_pointer_target);
        // The control's glyph lines up with the content's trailing edge.
        let x1 = card.x1 - pad + (side - f64::from(theme.tokens.metrics.icon_md)) / 2.0;
        let mid = title_top + title_h / 2.0;
        Rect::new(x1 - side, mid - side / 2.0, x1, mid + side / 2.0)
    });
    // The title stops where the close control starts, so a long title
    // elides instead of running under it.
    let title_x1 = close.map_or(card.x1 - pad, |c| c.x0 - f64::from(spacing.s2));
    let title = Rect::new(
        card.x0 + pad,
        title_top,
        title_x1.max(card.x0 + pad),
        title_top + title_h,
    );

    let body_top = title.y1 + title_gap;
    let body = Rect::new(
        card.x0 + pad,
        body_top,
        card.x1 - pad,
        (card.y1 - pad - footer_h).max(body_top),
    );

    // Geometry stays indexed by the authored action while the visual order
    // follows emphasis and the platform's convention.
    let mut actions = vec![Rect::ZERO; spec.actions.len()];
    if !spec.actions.is_empty() {
        let label = ControlSize::Regular.label_style(theme);
        let label_pad = f64::from(spacing.s4);
        let min_w = f64::from(spacing.s12) * 2.0;
        let by = card.y1 - pad - button_h;
        let mut bx = card.x1 - pad;
        let mut visual_order: Vec<usize> = (0..spec.actions.len()).collect();
        visual_order.sort_by_key(|&index| {
            let primary = spec.actions.get(index).is_some_and(ModalAction::is_default);
            match spec.platform.dialog_action_order() {
                DialogActionOrder::PrimaryFirst => !primary,
                DialogActionOrder::PrimaryLast => primary,
            }
        });
        for &index in visual_order.iter().rev() {
            let Some(a) = spec.actions.get(index) else {
                continue;
            };
            let (tw, _) = text_renderer.measure_style(a.label, None, &label);
            let bw = (f64::from(tw) + label_pad * 2.0).max(min_w);
            let r = Rect::new(bx - bw, by, bx, by + button_h);
            if let Some(slot) = actions.get_mut(index) {
                *slot = r;
            }
            bx = r.x0 - f64::from(spacing.s2);
        }
    }

    let primary = spec
        .actions
        .iter()
        .position(|a| a.is_default() && a.enabled);

    ModalLayout {
        scrim: window,
        card,
        title,
        close,
        body,
        actions,
        body_overflows,
        primary,
        dismiss_on_scrim: spec.dismiss_on_scrim,
    }
}

/// Paint the scrim, the sheet, the title and the close control. Call
/// before drawing the body.
pub fn paint_backdrop(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    layout: &ModalLayout,
    spec: &ModalSpec<'_>,
) {
    crate::overlay::scrim::modal(scene, transform, layout.scrim, 1.0, theme);
    floating_surface::paint(scene, transform, layout.card, theme);

    if !spec.title.is_empty() {
        let (ink, _) = ink(theme);
        let style = theme.text(TextRole::Title);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded screen-space width; f32 precision is sufficient for text layout"
        )]
        let title_w = layout.title.width() as f32;
        let shown = text_renderer.elide_to_width_style(spec.title, title_w, &style);
        let (_, th) = text_renderer.measure_style(&shown, None, &style);
        let ty = crate::text_metrics::centered_text_top(
            layout.title.y0,
            layout.title.height(),
            f64::from(th),
        );
        text_renderer.draw_style(
            scene,
            &shown,
            ink,
            transform.pre_translate(Vec2::new(layout.title.x0, ty)),
            None,
            &style,
        );
    }

    if let Some(c) = layout.close {
        let face = ButtonFace::new(c, "")
            .style(ButtonStyle::Ghost)
            .icon(Icon::X)
            .hovered(spec.close_hovered)
            .focused(spec.close_focused)
            .accessible_name("Close");
        Button::paint(scene, transform, text_renderer, theme, &face);
    }
}

/// Paint the footer actions. Call after drawing the body.
pub fn paint_actions(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    layout: &ModalLayout,
    spec: &ModalSpec<'_>,
) {
    for (i, a) in spec.actions.iter().enumerate() {
        let Some(rect) = layout.actions.get(i).copied() else {
            continue;
        };
        let style = match a.emphasis {
            ModalEmphasis::Primary => ButtonStyle::Primary,
            ModalEmphasis::Secondary => ButtonStyle::Neutral,
            ModalEmphasis::Danger => ButtonStyle::Danger,
        };
        let face = ButtonFace::new(rect, a.label)
            .style(style)
            .control_size(ControlSize::Regular)
            .hovered(a.hovered && a.enabled)
            .pressed(a.pressed && a.enabled)
            .focused(a.focused && a.enabled)
            .disabled(!a.enabled);
        Button::paint(scene, transform, text_renderer, theme, &face);
    }
}

/// One footer action of a [`ModalDialog`]; boxed `FnMut` so the dialog can
/// invoke it from `handle_event` without taking ownership.
pub struct DialogAction {
    pub label: String,
    pub on_click: Box<dyn FnMut()>,
}

impl std::fmt::Debug for DialogAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DialogAction")
            .field("label", &self.label)
            .field("on_click", &"<callback>")
            .finish()
    }
}

/// Which footer slot an index of the resolved spec is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Secondary,
    Primary,
}

/// What a layout was resolved for, so a paint with a different window or
/// scale re-resolves it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LayoutKey {
    window: Rect,
    text_scale: f32,
    ui_scale: f32,
    platform: PlatformConventions,
}

/// A modal dialog: a title, any content widget, and up to two footer
/// actions, over a scrim that covers the window. Generic over the content
/// type so callers pass any `Widget` without boxing.
pub struct ModalDialog<C: Widget> {
    action_identity: uuid::Uuid,
    test_id: Option<String>,
    title: String,
    content: C,
    primary_action: Option<DialogAction>,
    secondary_action: Option<DialogAction>,
    on_dismiss: Option<Box<dyn FnMut()>>,
    open: bool,
    destructive: bool,
    /// Widest the sheet is drawn, in design pixels.
    max_width: f64,
    /// The window the sheet is centred in: the layout node's rect until the
    /// first paint, then the window the top layer was painted over.
    window: Rect,
    /// `window` came from the host's top layer rather than the layout node.
    presented: bool,
    layout: Option<ModalLayout>,
    laid_out_for: Option<LayoutKey>,
    slots: Vec<Slot>,
    /// Tracked here so `MouseInput` (which carries no position) can map to
    /// a control.
    last_cursor: Point,
    /// Whether `last_cursor` is current: cleared by `CursorLeft` and on open,
    /// so a hover face never outlives the pointer that caused it.
    pointer_inside: bool,
    /// Isolated taffy tree for the content, kept alive so `handle_event`
    /// can forward events into the content with a valid node.
    content_taffy: Option<TaffyTree<()>>,
    content_node: Option<NodeId>,
    platform: PlatformConventions,
    /// The theme from the latest layout build, for the content layout
    /// `apply_layout` runs without one.
    layout_theme: Theme,
}

impl<C: Widget> ModalDialog<C> {
    pub fn new(title: impl Into<String>, content: C) -> Self {
        Self {
            action_identity: uuid::Uuid::new_v4(),
            test_id: None,
            title: title.into(),
            content,
            primary_action: None,
            secondary_action: None,
            on_dismiss: None,
            open: false,
            destructive: false,
            max_width: MODAL_MAX_WIDTH,
            window: Rect::ZERO,
            presented: false,
            layout: None,
            laid_out_for: None,
            slots: Vec::new(),
            last_cursor: Point::ZERO,
            pointer_inside: false,
            content_taffy: None,
            content_node: None,
            platform: PlatformConventions::default(),
            layout_theme: Theme::dark(),
        }
    }

    /// A stable address for inspection, headless scenarios and `tilo-control`.
    #[must_use]
    pub fn with_test_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self
    }

    /// When closed, the dialog paints nothing and ignores input.
    #[must_use]
    pub fn open(mut self, open: bool) -> Self {
        self.set_open(open);
        self
    }

    /// Show or hide after construction; also toggles whether the dialog
    /// [traps focus](Widget::traps_focus).
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
        // A closed dialog receives no moves, so whatever position it last
        // saw is stale by the time it reopens.
        self.pointer_inside = false;
    }

    /// The default action: Enter fires it, and it is the inverted button.
    #[must_use]
    pub fn primary_action(
        mut self,
        label: impl Into<String>,
        on_click: impl FnMut() + 'static,
    ) -> Self {
        self.primary_action = Some(DialogAction {
            label: label.into(),
            on_click: Box::new(on_click),
        });
        self.laid_out_for = None;
        self
    }

    /// The other action, usually Cancel.
    #[must_use]
    pub fn secondary_action(
        mut self,
        label: impl Into<String>,
        on_click: impl FnMut() + 'static,
    ) -> Self {
        self.secondary_action = Some(DialogAction {
            label: label.into(),
            on_click: Box::new(on_click),
        });
        self.laid_out_for = None;
        self
    }

    /// Fires on Escape, on a scrim click and on the close control. A dialog
    /// with a dismiss handler and no secondary action shows the close
    /// control.
    #[must_use]
    pub fn on_dismiss<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_dismiss = Some(Box::new(f));
        self.laid_out_for = None;
        self
    }

    /// The primary action deletes, revokes or discards: it is a destructive
    /// button.
    #[must_use]
    pub fn with_destructive(mut self, on: bool) -> Self {
        self.destructive = on;
        self
    }

    /// Widest the sheet is drawn, in design pixels (default
    /// [`MODAL_MAX_WIDTH`]). Its height always follows its content.
    #[must_use]
    pub fn with_max_width(mut self, max_width: f64) -> Self {
        self.set_max_width(max_width);
        self
    }

    /// [`Self::with_max_width`] after construction.
    pub fn set_max_width(&mut self, max_width: f64) {
        self.max_width = max_width.max(1.0);
        self.laid_out_for = None;
    }

    pub fn content(&self) -> &C {
        &self.content
    }

    pub fn content_mut(&mut self) -> &mut C {
        &mut self.content
    }

    pub fn dialog_rect(&self) -> Rect {
        self.layout.as_ref().map_or(Rect::ZERO, |l| l.card)
    }
    pub fn primary_rect(&self) -> Rect {
        self.slot_rect(Slot::Primary)
    }
    pub fn secondary_rect(&self) -> Rect {
        self.slot_rect(Slot::Secondary)
    }
    /// The close control, when the dialog shows one.
    pub fn close_rect(&self) -> Option<Rect> {
        self.layout.as_ref().and_then(|l| l.close)
    }
    pub fn is_open(&self) -> bool {
        self.open
    }

    fn slot_rect(&self, slot: Slot) -> Rect {
        let Some(layout) = &self.layout else {
            return Rect::ZERO;
        };
        self.slots
            .iter()
            .position(|s| *s == slot)
            .and_then(|i| layout.actions.get(i).copied())
            .unwrap_or(Rect::ZERO)
    }

    fn closable(&self) -> bool {
        self.on_dismiss.is_some() && self.secondary_action.is_none()
    }

    fn hovered(&self, rect: Rect) -> bool {
        self.pointer_inside && rect.contains(self.last_cursor)
    }

    /// The footer as the shared chrome sees it, with the slot of each entry.
    fn actions(&self) -> (Vec<ModalAction<'_>>, Vec<Slot>) {
        let mut actions = Vec::new();
        let mut slots = Vec::new();
        if let Some(a) = &self.secondary_action {
            actions.push(ModalAction::secondary(&a.label));
            slots.push(Slot::Secondary);
        }
        if let Some(a) = &self.primary_action {
            actions.push(if self.destructive {
                ModalAction::danger(&a.label)
            } else {
                ModalAction::primary(&a.label)
            });
            slots.push(Slot::Primary);
        }
        (actions, slots)
    }

    fn key(&self, theme: &Theme) -> LayoutKey {
        LayoutKey {
            window: self.window,
            text_scale: theme.text_scale,
            ui_scale: theme.ui_scale,
            platform: self.platform,
        }
    }

    /// Resolve the sheet in `self.window`: measure the content at the body
    /// width, size the sheet to it, and place the content in the body.
    fn relayout(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        if self.window.width() <= 0.0 || self.window.height() <= 0.0 {
            return;
        }
        let body_w = body_width(self.window, self.max_width, theme);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a sheet's body width is a bounded screen-pixel size"
        )]
        let available = Size {
            width: taffy::AvailableSpace::Definite(body_w as f32),
            height: taffy::AvailableSpace::MaxContent,
        };
        let content = floating_surface::DetachedContent::lay_out(
            &mut self.content,
            theme,
            available,
            text_renderer,
        );
        let (_, body_h) = content.size();
        let (actions, slots) = self.actions();
        let spec = ModalSpec::new(&self.title)
            .actions(&actions)
            .platform_conventions(self.platform)
            .body_height(body_h)
            .max_width(self.max_width)
            .closable(self.closable());
        let layout = layout(self.window, &spec, text_renderer, theme);
        drop(actions);
        self.content
            .apply_layout(&content.taffy, content.node, layout.body.origin());
        self.slots = slots;
        self.layout = Some(layout);
        self.content_node = Some(content.node);
        self.content_taffy = Some(content.taffy);
        self.laid_out_for = Some(self.key(theme));
    }

    /// No-op until the first content layout has run. Returns whether the
    /// content consumed the event.
    fn forward_to_content(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
    ) -> bool {
        let (Some(taffy), Some(node)) = (self.content_taffy.as_ref(), self.content_node) else {
            return false;
        };
        let mut inner_handled = false;
        self.content.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor,
                modifiers: &modifiers,
                state,
                path,
                handled: &mut inner_handled,
                focus_target,
                node_id: node,
                taffy,
            },
        );
        inner_handled
    }

    fn dismiss(&mut self) {
        if let Some(cb) = self.on_dismiss.as_mut() {
            cb();
        }
    }

    fn activate(&mut self, slot: Slot) {
        let action = match slot {
            Slot::Primary => self.primary_action.as_mut(),
            Slot::Secondary => self.secondary_action.as_mut(),
        };
        if let Some(action) = action {
            (action.on_click)();
        }
    }
}

impl<C: Widget + 'static> ModalDialog<C> {
    /// Route an event to a dialog that a painted host owns outside its
    /// retained layout tree (it still reaches the dialog's top-layer paint
    /// through [`Widget::children_mut`]).
    pub fn handle_detached(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        if !self.open {
            return false;
        }
        let mut taffy = TaffyTree::new();
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return true;
        };
        let mut handled = false;
        let mut focus_target = None;
        self.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor,
                modifiers: &modifiers,
                state,
                path,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }
}

impl<C: Widget + 'static> Widget for ModalDialog<C> {
    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        self.content.set_platform_conventions(conventions);
        self.laid_out_for = None;
        let theme = self.layout_theme.clone();
        crate::text_render::with_layout_renderer(theme.text_scale, |tr| self.relayout(&theme, tr));
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        // The dialog holds no document state itself; its content does.
        self.content.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        // The content is registered as a child node so its focusable
        // widgets join the host's keyboard focus order (and the focus
        // trap), even though `relayout` places it separately.
        let content_node = self.content.build_layout(taffy, theme, node_map);
        let node = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &[content_node],
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // Until the top layer has been painted over the window once, the
        // node's own rect stands in for it.
        if !self.presented {
            let l = taffy.layout(node).copied().unwrap_or_default();
            self.window = Rect::from_origin_size(
                (
                    origin.x + f64::from(l.location.x),
                    origin.y + f64::from(l.location.y),
                ),
                (f64::from(l.size.width), f64::from(l.size.height)),
            );
        }
        let theme = self.layout_theme.clone();
        crate::text_render::with_layout_renderer(theme.text_scale, |tr| self.relayout(&theme, tr));
    }

    /// Nothing: a modal is presented in the window's top layer
    /// ([`Self::draw_overlay`]), so it covers chrome drawn after the app.
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }
        // The host pushes the window as the presence viewport for its
        // top-layer pass: the scrim covers all of it.
        if let Some(window) = presence.viewport.get() {
            self.window = window;
            self.presented = true;
        }
        if self.laid_out_for != Some(self.key(theme)) {
            self.relayout(theme, text_renderer);
        }
        let Some(layout) = self.layout.clone() else {
            return;
        };
        let (mut actions, _) = self.actions();
        for (action, rect) in actions.iter_mut().zip(&layout.actions) {
            *action = action.hovered(self.pointer_inside && rect.contains(self.last_cursor));
        }
        let close_hovered = layout.close.is_some_and(|c| self.hovered(c));
        let spec = ModalSpec::new(&self.title)
            .actions(&actions)
            .platform_conventions(self.platform)
            .closable(layout.close.is_some())
            .close_hovered(close_hovered);
        paint_backdrop(scene, transform, text_renderer, theme, &layout, &spec);
        paint_actions(scene, transform, text_renderer, theme, &layout, &spec);
        drop(actions);

        if layout.body_overflows {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                transform,
                &layout.body,
            );
        }
        self.content
            .draw(scene, transform, text_renderer, theme, presence);
        self.content
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        if layout.body_overflows {
            scene.pop_layer();
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.open {
            return;
        }
        // Blocking: while open, every event is marked handled so the dialog
        // stays modal even hosted directly (tests, headless).
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.pointer_inside = true;
                // A TextInput only takes a click while hovered, and this
                // CursorMoved is what arms it.
                self.forward_to_content(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                );
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer_inside = false;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.dismiss();
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Enter),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                let content_handled = self.forward_to_content(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                );
                if !content_handled {
                    self.activate(Slot::Primary);
                }
                *ctx.handled = true;
            }
            // Text arrives from an input method, not only as key presses.
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => {
                self.forward_to_content(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                );
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_cursor;
                let hit = self.layout.as_ref().and_then(|l| l.hit(p));
                match hit {
                    Some(ModalHit::Close) => self.dismiss(),
                    Some(ModalHit::Action(i)) => {
                        if let Some(slot) = self.slots.get(i).copied() {
                            self.activate(slot);
                        }
                    }
                    Some(ModalHit::Card) => {
                        self.forward_to_content(
                            event,
                            ctx.scale_factor,
                            *ctx.modifiers,
                            ctx.state,
                            ctx.path,
                            ctx.focus_target,
                        );
                    }
                    Some(ModalHit::Scrim) | None => self.dismiss(),
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput { .. } => {
                // In-sheet presses still reach the content, so a right-click
                // affordance in it can react.
                if self.dialog_rect().contains(self.last_cursor) {
                    self.forward_to_content(
                        event,
                        ctx.scale_factor,
                        *ctx.modifiers,
                        ctx.state,
                        ctx.path,
                        ctx.focus_target,
                    );
                }
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ModalDialog"
    }

    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.open.then(|| self.dialog_rect())
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.open {
            vec![&self.content]
        } else {
            Vec::new()
        }
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if self.open {
            visitor(&self.content);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.open {
            vec![&mut self.content]
        } else {
            Vec::new()
        }
    }

    fn get_rect(&self) -> Rect {
        self.dialog_rect()
    }

    fn traps_focus(&self) -> bool {
        self.open
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // The title is the accessible name (FTL-resolved). A closed dialog
        // is hidden from the tree.
        let title = crate::i18n!(&self.title);
        let name = (!title.is_empty()).then_some(title);
        let mut node = AccessNode::new(Role::Dialog)
            .with_optional_name(name)
            .with_state(AccessState {
                modal: true,
                hidden: !self.open,
                ..Default::default()
            });
        if self.open {
            for (key, index, action, bounds) in [
                ("primary", 1, &self.primary_action, self.primary_rect()),
                (
                    "secondary",
                    2,
                    &self.secondary_action,
                    self.secondary_rect(),
                ),
            ] {
                if let Some(action) = action {
                    node.children.push(
                        AccessNode::new(Role::Button)
                            .with_name(crate::i18n!(&action.label))
                            .with_key(index)
                            .with_state(AccessState::new().focusable(true))
                            .with_bounds(bounds)
                            .with_action_target(format!("dialog:{}:{key}", self.action_identity)),
                    );
                }
            }
            if self.closable() {
                node.children.push(
                    AccessNode::new(Role::Button)
                        .with_name(crate::i18n!("Close"))
                        .with_key(3)
                        .with_state(AccessState::new().focusable(true))
                        .with_bounds(self.close_rect().unwrap_or(Rect::ZERO))
                        .with_action_target(format!("dialog:{}:close", self.action_identity)),
                );
            }
        }
        Some(node)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _: &mut AppModel,
        _: &ModelPath,
    ) -> bool {
        if !self.open || !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let id = self.action_identity;
        if (target == "modal.primary" || target == format!("dialog:{id}:primary"))
            && self.primary_action.is_some()
        {
            self.activate(Slot::Primary);
            true
        } else if (target == "modal.secondary" || target == format!("dialog:{id}:secondary"))
            && self.secondary_action.is_some()
        {
            self.activate(Slot::Secondary);
            true
        } else if (target == "modal.close" || target == format!("dialog:{id}:close"))
            && self.closable()
        {
            self.dismiss();
            true
        } else {
            false
        }
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else { return None };
        self.accessibility()?
            .children
            .get(*index)?
            .bounds
            .map(super::super::a11y::AccessBounds::to_rect)
    }
}

#[cfg(test)]
#[path = "modal_dialog_tests.rs"]
mod tests;

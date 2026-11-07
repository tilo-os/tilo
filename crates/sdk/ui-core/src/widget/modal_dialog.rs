//! The one modal: a sheet over a scrim that covers the whole window.
//!
//! [`ModalDialog`] is the retained widget: a title, any content widget, and up to
//! three footer actions, always presented in the window's top layer
//! ([`Widget::draw_overlay`]) after the app and the window's chrome, so its scrim
//! covers all of them. Escape or a click on the scrim dismisses; Enter fires the
//! default action unless the content consumes it (the primary action, except in a
//! destructive confirmation, which takes the safe answer, as on macOS). Content
//! taller than the window scrolls inside the body.
//!
//! A painted host that asks a question without a retained tree uses [`ModalSheet`],
//! which paints the sheet, keeps its layout for hit-testing and turns input into a
//! [`SheetResponse`]. Hosts that need only the chrome (the identity ceremonies, shell
//! sheets) use [`layout`], [`paint_backdrop`] and [`paint_actions`]:
//!
//! ```ignore
//! let layout = modal_dialog::layout(window, &spec, text_renderer, theme);
//! modal_dialog::paint_backdrop(scene, transform, text_renderer, theme, &layout, &spec);
//! // ... caller draws its own content inside `layout.body` ...
//! modal_dialog::paint_actions(scene, transform, text_renderer, theme, &layout, &spec);
//! ```
//!
//! On a roomy viewport the sheet is a floating Panel ([`super::floating_surface`]);
//! on a compact one an opaque full-window page. Dismissal ([`ModalDialog::on_dismiss`])
//! is the dialog's close control, published as a view action so agents and scripts
//! close the dialog as a person does.

use crate::widget::{AppModel, ModelPath};
#[cfg(test)]
use automerge::AutoCommit;
use automerge::ObjId;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use taffy::{NodeId, Size, Style, TaffyTree};
use tilo_sort::CompactSort as _;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
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
use crate::widget::button::{Button, ButtonFace, ButtonRole};
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
    /// The main action. One per modal, and the one Enter fires.
    Primary,
    /// Everything else: Cancel, and any additional verb.
    Secondary,
    /// A primary action that destroys something. Enter never fires it: in
    /// a sheet with a way out, Enter is that way out (Cancel).
    Danger,
    /// A third answer that is neither the default nor the way out ("Don't save"), standing at the
    /// footer's leading edge; Enter never fires it.
    Alternate,
    /// An [`Self::Alternate`] answer that discards something: danger ink on
    /// a neutral control, never filled, never what Enter fires.
    AlternateDanger,
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
    /// The third answer of a three-way question.
    pub fn alternate(label: &'a str) -> Self {
        Self::of(label, ModalEmphasis::Alternate)
    }
    /// A third answer that discards something ("Don't save", "Overwrite").
    pub fn alternate_danger(label: &'a str) -> Self {
        Self::of(label, ModalEmphasis::AlternateDanger)
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

    fn is_alternate(&self) -> bool {
        matches!(
            self.emphasis,
            ModalEmphasis::Alternate | ModalEmphasis::AlternateDanger
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
    /// An icon leading the title that names who asks (a lock for a
    /// password, the network for a Wi-Fi join).
    pub icon: Option<Icon>,
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
    /// Height the sheet's header wants, below the title: the body scrolls
    /// under it, so a strip of tabs or a search field stays in view.
    pub header_height: f64,
    /// Widest the sheet may be drawn, in design pixels. Defaults to
    /// [`MODAL_MAX_WIDTH`].
    pub max_width: f64,
    pub close_hovered: bool,
    pub close_focused: bool,
    /// Whether a scrim click dismisses. The scrim swallows the click either
    /// way: a surface that lets clicks through is not modal.
    pub dismiss_on_scrim: bool,
    /// The control the sheet asks about: it stands beside it as a popover,
    /// the window undimmed. `None` centres it over the window.
    pub anchor: Option<Rect>,
}

impl Default for ModalSpec<'_> {
    fn default() -> Self {
        Self {
            title: "",
            icon: None,
            closable: true,
            actions: &[],
            platform: PlatformConventions::default(),
            body_height: 0.0,
            header_height: 0.0,
            max_width: MODAL_MAX_WIDTH,
            close_hovered: false,
            close_focused: false,
            dismiss_on_scrim: true,
            anchor: None,
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
    pub fn icon(mut self, icon: Option<Icon>) -> Self {
        self.icon = icon;
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
    pub fn header_height(mut self, h: f64) -> Self {
        self.header_height = h;
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
    #[must_use]
    pub fn anchor(mut self, anchor: Option<Rect>) -> Self {
        self.anchor = anchor;
        self
    }
}

/// Where every part of the modal landed. Pure geometry, computed without a
/// GPU, so the layout rules are tested directly.
#[derive(Clone, Debug, PartialEq)]
pub struct ModalLayout {
    /// The whole window. The scrim covers this.
    pub scrim: Rect,
    pub card: Rect,
    /// The title's line, from the sheet's leading padding to the close
    /// control (or the trailing padding).
    pub title: Rect,
    /// The close control's hit target, when the spec asked for one.
    pub close: Option<Rect>,
    /// The sheet's own header, below the title and above the body: the
    /// body scrolls under it, it stays put.
    pub header: Option<Rect>,
    /// The region the caller owns. Content taller than this scrolls or
    /// elides inside it; see [`Self::body_overflows`].
    pub body: Rect,
    /// One rect per entry in `spec.actions`, in the same order.
    pub actions: Vec<Rect>,
    /// The body wanted more height than a sheet inside the window can give.
    pub body_overflows: bool,
    /// Index of the action Enter fires, if any: the primary action, or
    /// Cancel when the primary action is destructive.
    pub primary: Option<usize>,
    /// Mirrors [`ModalSpec::dismiss_on_scrim`].
    pub dismiss_on_scrim: bool,
    /// The sheet stands beside a control ([`ModalSpec::anchor`]): the
    /// window behind it is not dimmed.
    pub anchored: bool,
    /// The sheet rises from a compact window's foot, as tall as what it holds ([`rise_from_foot`]):
    /// a [`super::BottomSheet`] over the dimmed page.
    pub foot: bool,
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

/// Ink legible on the sheet, as `(primary, dim)`: the theme's own when it clears AA against the
/// panel ground, else the contrast-guaranteed on-content ramp.
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

/// A small viewport presents a modal as a page instead of a floating card, so phone landscape and
/// short viewports stay usable.
pub fn uses_full_window(window: Rect, theme: &Theme) -> bool {
    window.width().min(window.height()) < f64::from(theme.tokens.metrics.panel_width_sheet)
}

/// Width of the sheet for `max_width` (design pixels) inside `window`, knowable before any body row
/// is measured: widths grow with the text scale, since what fills a sheet is text.
pub fn card_width(window: Rect, max_width: f64, theme: &Theme) -> f64 {
    if uses_full_window(window, theme) {
        return window.width().max(0.0);
    }
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

/// Height of one title line, and of the close control beside it.
fn title_row_height(theme: &Theme) -> f64 {
    let line = f64::from(theme.text(TextRole::Title).line_height_px());
    line.max(f64::from(theme.tokens.metrics.min_pointer_target))
}

/// The side of a title icon: the large icon size.
fn title_icon_side(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.icon_lg)
}

/// Where a title icon is drawn: before the title, centred on its first row.
pub fn title_icon_rect(layout: &ModalLayout, theme: &Theme) -> Rect {
    let side = title_icon_side(theme);
    let x1 = layout.title.x0 - f64::from(theme.tokens.spacing.s3);
    let mid = layout.title.y0 + title_row_height(theme).min(layout.title.height()) / 2.0;
    Rect::new(x1 - side, mid - side / 2.0, x1, mid + side / 2.0)
}

/// The title wrapped to `width`: one or two lines, the second elided.
fn title_lines(
    text_renderer: &mut TextRenderer,
    title: &str,
    width: f64,
    theme: &Theme,
) -> Vec<String> {
    if title.is_empty() {
        return Vec::new();
    }
    let style = theme.text(TextRole::Title);
    text_renderer.clamp_to_lines(title, &style, crate::num::f32_of_f64(width), 2)
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
    paint_scrim(scene, transform, theme, layout, layout.card);
    paint_sheet(scene, transform, text_renderer, theme, layout, spec);
}

/// Paint the scrim behind a floating sheet drawn in `drawn_card`, which differs from `layout.card`
/// while it arrives or leaves; a full-window page has none.
pub fn paint_scrim(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    layout: &ModalLayout,
    drawn_card: Rect,
) {
    // Neither a full-window page nor a popover by its control dims the window.
    if layout.card == layout.scrim || layout.anchored {
        return;
    }
    let scale = if layout.card.width() > 0.0 {
        drawn_card.width() / layout.card.width()
    } else {
        1.0
    };
    crate::overlay::scrim::behind_panel(
        scene,
        transform,
        layout.scrim,
        RoundedRect::from_rect(drawn_card, floating_surface::radius(theme) * scale),
        theme,
    );
}

/// Paint the sheet, its title and its close control, without the scrim.
pub fn paint_sheet(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    layout: &ModalLayout,
    spec: &ModalSpec<'_>,
) {
    if layout.foot {
        // The caller painted the sheet from the foot (`BottomSheet::paint`).
    } else if layout.card == layout.scrim {
        // A full-window modal is the foreground page. It does not sample or
        // dim the displaced page and has no floating shadow or rounded edge.
        crate::floating_surface::paint_material(
            scene,
            transform,
            &layout.card,
            crate::floating_surface::Material::Opaque,
            theme,
        );
    } else {
        floating_surface::paint(scene, transform, layout.card, theme);
    }

    if let Some(icon) = spec.icon.filter(|_| layout.title.height() > 0.0) {
        let (ink, _) = ink(theme);
        let rect = title_icon_rect(layout, theme);
        crate::icons::draw_icon(
            scene,
            transform,
            rect,
            icon,
            ink,
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }
    if !spec.title.is_empty() {
        let (ink, _) = ink(theme);
        let style = theme.text(TextRole::Title);
        let lines = title_lines(text_renderer, spec.title, layout.title.width(), theme);
        let line_h = f64::from(style.line_height_px());
        // One line centres on the close control's row; two run from the top.
        let mut y = if lines.len() > 1 {
            layout.title.y0
        } else {
            crate::text_metrics::centered_text_top(
                layout.title.y0,
                title_row_height(theme).min(layout.title.height()),
                line_h,
            )
        };
        for line in &lines {
            text_renderer.draw_style(
                scene,
                line,
                ink,
                transform.pre_translate(Vec2::new(layout.title.x0, y)),
                None,
                &style,
            );
            y += line_h;
        }
    }

    if let Some(c) = layout.close {
        let face = ButtonFace::icon_only(c, Icon::X, "Close")
            .hovered(spec.close_hovered)
            .focused(spec.close_focused);
        Button::paint(scene, transform, text_renderer, &on_sheet(theme), &face);
    }
}

/// `theme` as seen by what stands on the sheet: controls lift off the
/// panel ground rather than the window's (`Theme::on_ground`).
fn on_sheet(theme: &Theme) -> Theme {
    theme.on_ground(crate::chrome::panel_ground(theme))
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
    let theme = &on_sheet(theme);
    for (i, a) in spec.actions.iter().enumerate() {
        let Some(rect) = layout.actions.get(i).copied() else {
            continue;
        };
        let face = ButtonFace::new(rect, a.label);
        // A modal's danger action is the confirmation step, the one place a
        // destructive button is filled.
        let face = match a.emphasis {
            ModalEmphasis::Primary => face.role(ButtonRole::Primary),
            ModalEmphasis::Secondary | ModalEmphasis::Alternate => face.role(ButtonRole::Secondary),
            ModalEmphasis::Danger => face.role(ButtonRole::Destructive).confirming(),
            ModalEmphasis::AlternateDanger => face.role(ButtonRole::Destructive),
        }
        .control_size(ControlSize::Regular)
        .hovered(a.hovered && a.enabled)
        .pressed(a.pressed && a.enabled)
        .focused(a.focused && a.enabled)
        .disabled(!a.enabled);
        Button::paint(scene, transform, text_renderer, theme, &face);
    }
}

/// What an immediate [`ModalSheet`] was answered with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetResponse {
    /// An action, by index into the spec's `actions`.
    Action(usize),
    /// Escape, the close control, or the scrim where the spec allows it.
    Dismiss,
}

/// The immediate-mode sheet for a painted host that keeps its own state (a question
/// with three answers, a confirmation over a canvas): it lays the sheet out, paints it
/// with hover, press and keyboard focus on its actions, keeps that layout for
/// hit-testing and turns input into a [`SheetResponse`]; the host paints only its body.
///
/// ```ignore
/// // draw_overlay:
/// let window = presence.viewport.get().unwrap_or(self.rect);
/// let spec = ModalSpec::new("Save changes to notes.md?").actions(&[
///     ModalAction::primary("Save"),
///     ModalAction::alternate_danger("Don't save"),
///     ModalAction::secondary("Cancel"),
/// ]);
/// self.sheet.paint_message(scene, transform, text, theme, window, &spec, "Your changes will be lost.");
/// // handle_event:
/// match self.sheet.handle(event, ctx.scale_factor) {
///     Some(SheetResponse::Action(0)) => self.save(),
///     Some(SheetResponse::Action(1)) => self.discard(),
///     Some(SheetResponse::Action(_) | SheetResponse::Dismiss) => self.cancel(),
///     None => {}
/// }
/// *ctx.handled = true;
/// ```
#[derive(Clone, Debug, Default)]
pub struct ModalSheet {
    layout: Option<ModalLayout>,
    cursor: Option<Point>,
    pressed: Option<usize>,
    focus: Option<usize>,
    enabled: Vec<bool>,
    /// Action indices in the order they stand, leading to trailing.
    visual: Vec<usize>,
    /// Its arrival (`motion::Appear`, from the sheet's centre), started by the first paint after
    /// [`Self::new`] or [`Self::reset`].
    appear: crate::motion::Appearing,
}

impl ModalSheet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the last paint and any focus: call when the sheet closes, so
    /// the next question starts fresh.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The geometry of the last paint, for hit-testing without a renderer.
    #[must_use]
    pub fn layout(&self) -> Option<&ModalLayout> {
        self.layout.as_ref()
    }

    /// What `point` lands on in the last paint.
    #[must_use]
    pub fn hit(&self, point: Point) -> Option<ModalHit> {
        self.layout.as_ref()?.hit(point)
    }

    /// Height a plain message takes as the body of a sheet in `window`.
    pub fn message_height(
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
        max_width: f64,
        message: &str,
    ) -> f64 {
        if message.is_empty() {
            return 0.0;
        }
        let width = crate::num::f32_of_f64(body_width(window, max_width, theme));
        let (_, h) = text_renderer.measure_style(message, Some(width), &theme.text(TextRole::Body));
        f64::from(h)
    }

    /// Paint the sheet for `spec` over `window` with `message` as its body,
    /// in secondary ink. The body height is measured here.
    pub fn paint_message(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
        spec: &ModalSpec<'_>,
        message: &str,
    ) {
        let height = Self::message_height(text_renderer, theme, window, spec.max_width, message);
        let spec = spec.clone().body_height(height);
        self.paint(
            scene,
            transform,
            text_renderer,
            theme,
            window,
            &spec,
            |scene, transform, text_renderer, theme, body| {
                let (_, dim) = ink(theme);
                text_renderer.draw_style(
                    scene,
                    message,
                    dim,
                    transform.pre_translate(Vec2::new(body.x0, body.y0)),
                    Some(crate::num::f32_of_f64(body.width())),
                    &theme.text(TextRole::Body),
                );
            },
        );
    }

    /// Paint the sheet for `spec` over `window`, calling `body` to draw the host's content inside
    /// the body rect (`spec.body_height` tall).
    pub fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
        spec: &ModalSpec<'_>,
        body: impl FnOnce(&mut Scene, Affine, &mut TextRenderer, &Theme, Rect),
    ) {
        let layout = layout(window, spec, text_renderer, theme);
        self.enabled = spec.actions.iter().map(|a| a.enabled).collect();
        let mut visual: Vec<usize> = (0..spec.actions.len()).collect();
        visual.compact_sort_by(|&a, &b| {
            let x = |i: usize| layout.actions.get(i).map_or(0.0, |r| r.x0);
            x(a).total_cmp(&x(b))
        });
        self.visual = visual;
        if self
            .focus
            .is_some_and(|i| !self.enabled.get(i).copied().unwrap_or(false))
        {
            self.focus = None;
        }
        let actions: Vec<ModalAction<'_>> = spec
            .actions
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let over = self
                    .cursor
                    .zip(layout.actions.get(i))
                    .is_some_and(|(p, r)| r.contains(p));
                a.hovered(a.hovered || over)
                    .pressed(a.pressed || (over && self.pressed == Some(i)))
                    .focused(a.focused || self.focus == Some(i))
            })
            .collect();
        let close_hovered = self
            .cursor
            .zip(layout.close)
            .is_some_and(|(p, r)| r.contains(p));
        let spec = spec
            .clone()
            .actions(&actions)
            .close_hovered(spec.close_hovered || close_hovered);
        // It arrives from its centre; the scrim fades with it, cut out where
        // the sheet is drawn. A full-window page only fades.
        self.appear.follow(true, theme.motion());
        let mut pose = self.appear.pose(layout.card, layout.card.center());
        if layout.card == layout.scrim {
            pose.transform = Affine::IDENTITY;
        }
        let scrim = crate::motion::Pose {
            opacity: pose.opacity,
            transform: Affine::IDENTITY,
        };
        if let Some(at) = scrim.begin(scene, transform, layout.scrim) {
            paint_scrim(scene, at, theme, &layout, pose.drawn_rect(layout.card));
            scrim.end(scene);
        }
        let Some(transform) = pose.begin(scene, transform, layout.scrim) else {
            self.layout = Some(layout);
            return;
        };
        paint_sheet(scene, transform, text_renderer, theme, &layout, &spec);
        let on_panel = on_sheet(theme);
        if layout.body_overflows {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                transform,
                &layout.body,
            );
        }
        body(scene, transform, text_renderer, &on_panel, layout.body);
        if layout.body_overflows {
            scene.pop_layer();
        }
        paint_actions(scene, transform, text_renderer, theme, &layout, &spec);
        pose.end(scene);
        self.layout = Some(layout);
    }

    /// Route `event` to the sheet. Returns what it was answered with; the
    /// host consumes every event while the sheet is up either way.
    pub fn handle(&mut self, event: &WindowEvent, scale_factor: f64) -> Option<SheetResponse> {
        let layout = self.layout.as_ref()?;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Some(Point::new(
                    position.x / scale_factor,
                    position.y / scale_factor,
                ));
                None
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor = None;
                self.pressed = None;
                None
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.cursor?;
                match layout.hit(p) {
                    Some(ModalHit::Action(i)) => {
                        self.pressed = self.enabled.get(i).copied().unwrap_or(false).then_some(i);
                        None
                    }
                    Some(ModalHit::Close) => Some(SheetResponse::Dismiss),
                    Some(ModalHit::Scrim) if layout.dismiss_on_scrim => {
                        Some(SheetResponse::Dismiss)
                    }
                    _ => None,
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let pressed = self.pressed.take()?;
                let p = self.cursor?;
                (layout.hit(p) == Some(ModalHit::Action(pressed)))
                    .then_some(SheetResponse::Action(pressed))
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(key),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => match key {
                NamedKey::Escape => Some(SheetResponse::Dismiss),
                NamedKey::Enter => self.focus.or(layout.primary).map(SheetResponse::Action),
                NamedKey::Space => self.focus.map(SheetResponse::Action),
                NamedKey::Tab | NamedKey::ArrowLeft | NamedKey::ArrowRight => {
                    let back = matches!(key, NamedKey::ArrowLeft);
                    self.move_focus(back);
                    None
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Move keyboard focus to the next enabled action in visual order.
    fn move_focus(&mut self, back: bool) {
        let order: Vec<usize> = self
            .visual
            .iter()
            .copied()
            .filter(|&i| self.enabled.get(i).copied().unwrap_or(false))
            .collect();
        if order.is_empty() {
            self.focus = None;
            return;
        }
        let at = self.focus.and_then(|f| order.iter().position(|&i| i == f));
        let next = match (at, back) {
            (None, false) => 0,
            (None | Some(0), true) => order.len() - 1,
            (Some(i), true) => i - 1,
            (Some(i), false) => (i + 1) % order.len(),
        };
        self.focus = order.get(next).copied();
    }

    /// The action keyboard focus is on, if any.
    #[must_use]
    pub fn focused_action(&self) -> Option<usize> {
        self.focus
    }
}

/// One footer action of a [`ModalDialog`]: the dialog runs it from its own events, the action's
/// focus node when operated through the accessibility tree.
pub struct DialogAction {
    pub label: String,
    on_click: Rc<RefCell<dyn FnMut()>>,
}

impl DialogAction {
    pub fn new(label: impl Into<String>, on_click: impl FnMut() + 'static) -> Self {
        Self {
            label: label.into(),
            on_click: Rc::new(RefCell::new(on_click)),
        }
    }

    fn run(&self) {
        (self.on_click.borrow_mut())();
    }
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
    Tertiary,
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
    content_h: f64,
    header_h: f64,
}

/// A modal dialog: a title, any content widget, and up to three footer
/// actions, over a scrim that covers the window. Generic over the content
/// type so callers pass any `Widget` without boxing.
///
/// Mounted in a retained tree, the content is laid out in the host's own
/// tree: a click on a field focuses it (and blurs the previous one), Tab
/// runs through the content and then the footer actions inside the focus
/// trap, and opening the dialog focuses its first control. A painted host
/// that keeps the dialog outside its tree routes events with
/// [`Self::handle_detached`]; the dialog then lays its content out itself.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent open, presentation, hosting and invalidation flags"
)]
#[crate::component::not_a_component(
    part,
    "the dialog `ui.ModalDialog` is built from, around the view's content"
)]
pub struct ModalDialog<C: Widget + ?Sized> {
    action_identity: uuid::Uuid,
    test_id: Option<String>,
    title: String,
    /// Leads the title ([`ModalSpec::icon`]).
    icon: Option<Icon>,
    /// What assistive technology says about the dialog after its name.
    description: Option<String>,
    primary_action: Option<DialogAction>,
    secondary_action: Option<DialogAction>,
    tertiary_action: Option<DialogAction>,
    tertiary_destructive: bool,
    primary_enabled: bool,
    /// Reads the content, which is the `C` the dialog was built with
    /// ([`Self::primary_enabled_if`]).
    primary_gate: Option<Box<dyn Fn(&dyn Widget) -> bool>>,
    /// Dismissal, published as the dialog's close control
    /// ([`Self::on_dismiss`]); every way of dismissing emits it.
    dismiss: Option<super::dismissal::Dismissal>,
    destructive: bool,
    /// Widest the sheet is drawn, in design pixels.
    max_width: f64,
    /// The window the sheet is centred in: the layout node's rect until the
    /// first paint, then the window the top layer was painted over.
    window: Rect,
    /// `window` came from the host's top layer rather than the layout node.
    presented: bool,
    /// The layout node of the window the dialog stands in, when a host shares one tree among
    /// several windows ([`within_window`]).
    window_node: Option<NodeId>,
    layout: Option<ModalLayout>,
    laid_out_for: Option<LayoutKey>,
    slots: Vec<Slot>,
    /// Focus nodes for the footer actions, in the order they stand.
    keys: Vec<FooterKey>,
    /// Tracked here so `MouseInput` (which carries no position) can map to
    /// a control.
    last_cursor: Point,
    /// Whether `last_cursor` is current: cleared by `CursorLeft` and on open,
    /// so a hover face never outlives the pointer that caused it.
    pointer_inside: bool,
    /// The content's own tree, when no host lays it out (a painted host).
    detached: Option<floating_surface::DetachedContent>,
    /// The content's node in the host's tree, once a host has built it.
    host_content: Option<NodeId>,
    /// A strip of the sheet's own that stands below the title, the body
    /// scrolling under it ([`Self::with_header`]).
    header: Option<Box<dyn Widget>>,
    /// The header's node in the host's tree (or its own, at a painted host).
    header_node: Option<NodeId>,
    /// The header's measured size.
    header_size: (f64, f64),
    /// Where the header's top-left was last placed.
    header_origin: Point,
    /// The header's tree, when no host lays it out.
    header_detached: Option<floating_surface::DetachedContent>,
    /// The content's measured size.
    content_size: (f64, f64),
    /// The body width the hosted content was last laid out in: a draw finding the body wider or
    /// narrower asks for a new layout pass.
    content_laid_out_in: f64,
    /// Where the content's top-left was last placed.
    content_origin: Point,
    /// How far the body is scrolled when the content is taller than it.
    scroll: f64,
    /// How much the body was shortened to end on a content boundary.
    trim: f64,
    /// The host must place the content again (scroll, window, trim).
    stale: bool,
    /// The host must measure the content again (width, content change).
    remeasure: bool,
    /// The footer's focus nodes changed: the host rebuilds its tree.
    structure_dirty: bool,
    platform: PlatformConventions,
    /// The theme from the latest layout build, for the content layout
    /// `apply_layout` runs without one.
    layout_theme: Theme,
    /// Its arriving and leaving (`motion::Appear`), kept as a handle so every rebuild of the host's
    /// tree is handed the same one ([`Self::appearing`]).
    appear: crate::motion::Appearance,
    /// The control the sheet asks about: it stands beside it, the window
    /// undimmed ([`Self::set_anchor`]).
    anchor: Option<Rect>,
    /// On a compact window the sheet rises from the foot when what it holds
    /// fits ([`Self::with_foot_sheet`]).
    foot: Option<super::BottomSheet>,
    /// Last, so a `ModalDialog<C>` coerces to `ModalDialog<dyn Widget>`, the
    /// one type the dialog's behaviour is compiled for.
    content: C,
}

impl<C: Widget + 'static> ModalDialog<C> {
    pub fn new(title: impl Into<String>, content: C) -> Self {
        Self {
            action_identity: uuid::Uuid::new_v4(),
            test_id: None,
            title: title.into(),
            icon: None,
            description: None,
            content,
            primary_action: None,
            secondary_action: None,
            tertiary_action: None,
            tertiary_destructive: false,
            primary_enabled: true,
            primary_gate: None,
            dismiss: None,
            destructive: false,
            max_width: MODAL_MAX_WIDTH,
            window: Rect::ZERO,
            presented: false,
            window_node: None,
            layout: None,
            laid_out_for: None,
            slots: Vec::new(),
            keys: Vec::new(),
            last_cursor: Point::ZERO,
            pointer_inside: false,
            detached: None,
            host_content: None,
            header: None,
            header_node: None,
            header_size: (0.0, 0.0),
            header_origin: Point::ZERO,
            header_detached: None,
            content_size: (0.0, 0.0),
            content_laid_out_in: 0.0,
            content_origin: Point::ZERO,
            scroll: 0.0,
            trim: 0.0,
            stale: false,
            remeasure: false,
            structure_dirty: false,
            platform: PlatformConventions::default(),
            layout_theme: Theme::dark(),
            appear: crate::motion::Appearance::default(),
            anchor: None,
            foot: None,
        }
    }

    /// Stand beside the control at `anchor` as a popover, the window undimmed, rather than over the
    /// window's centre; `None` centres it.
    pub fn set_anchor(&mut self, anchor: Option<Rect>) {
        if self.anchor != anchor {
            self.anchor = anchor;
            self.invalidate_layout();
        }
    }

    /// On a compact window, rise from its foot as a [`super::BottomSheet`] when that fits; else
    /// stay the whole page.
    #[must_use]
    pub fn with_foot_sheet(mut self) -> Self {
        let mut sheet = super::BottomSheet::new();
        sheet.snap_to(super::SheetDetent::Full);
        self.foot = Some(sheet);
        self
    }

    /// An icon before the title naming who asks.
    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.set_icon(Some(icon));
        self
    }

    /// [`Self::with_icon`] after construction; `None` takes it away.
    pub fn set_icon(&mut self, icon: Option<Icon>) {
        if self.icon != icon {
            self.icon = icon;
            self.invalidate_layout();
        }
    }

    /// A strip of the sheet's own below the title, staying put while the body scrolls under it, so
    /// the tabs or the path a person is deep in stay in view.
    #[must_use]
    pub fn with_header(mut self, header: impl Widget + 'static) -> Self {
        self.set_header(Some(Box::new(header)));
        self
    }

    /// [`Self::with_header`] after construction; `None` removes it.
    pub fn set_header(&mut self, header: Option<Box<dyn Widget>>) {
        if header.is_none() {
            self.header_detached = None;
            self.header_size = (0.0, 0.0);
        }
        self.header = header;
        self.invalidate_layout();
    }

    /// What assistive technology says about the dialog after its title:
    /// what is going on in it, and how its keys work.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.set_description(Some(description.into()));
        self
    }

    /// [`Self::with_description`] after construction; `None` takes it away.
    pub fn set_description(&mut self, description: Option<String>) {
        if self.description != description {
            self.description = description;
            self.invalidate_layout();
        }
    }

    /// What the dialog is, as its title: the question it asks.
    pub fn set_title(&mut self, title: impl Into<String>) {
        let title = title.into();
        if self.title != title {
            self.title = title;
            self.invalidate_layout();
        }
    }

    /// A stable address for inspection, headless scenarios and `tilo-control`.
    #[must_use]
    pub fn with_test_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        if let Some(dismiss) = self.dismiss.take() {
            self.dismiss = Some(dismiss.renamed(self.surface_id()));
        }
        self
    }

    /// When closed, the dialog paints nothing and ignores input.
    #[must_use]
    pub fn open(mut self, open: bool) -> Self {
        self.set_open(open);
        // Built open: shown at rest from its first paint, so a host that rebuilds its tree never
        // replays the entrance.
        if self.appear.is_owned() {
            self.appear.snap(open);
        }
        self
    }

    /// Draw this dialog on `appearance`, which the host keeps across rebuilds: built open it
    /// arrives once, and a dialog its host closes leaves until it is gone.
    #[must_use]
    pub fn appearing(mut self, appearance: crate::motion::Appearance) -> Self {
        self.appear = appearance;
        self
    }

    /// Show or hide after construction, which also toggles whether the dialog [traps
    /// focus](Widget::traps_focus); opening scrolls the body to its top and lets the host focus its
    /// first control.
    pub fn set_open(&mut self, open: bool) {
        if !open {
            self.shut(crate::motion::input_source());
            return;
        }
        if !self.up() {
            self.scroll = 0.0;
            self.stale = true;
            self.remeasure = true;
        }
        self.appear.show(true);
        // A closed dialog receives no moves, so whatever position it last
        // saw is stale by the time it reopens.
        self.pointer_inside = false;
    }

    /// The main action, the inverted button. Enter fires it unless it is
    /// [destructive](Self::with_destructive), when Enter is Cancel.
    #[must_use]
    pub fn primary_action(
        mut self,
        label: impl Into<String>,
        on_click: impl FnMut() + 'static,
    ) -> Self {
        self.set_primary_action(Some(DialogAction::new(label, on_click)));
        self
    }

    /// [`Self::primary_action`] after construction; `None` takes it away.
    pub fn set_primary_action(&mut self, action: Option<DialogAction>) {
        self.primary_action = action;
        self.as_dyn_mut().footer_changed();
    }

    /// The other action, usually Cancel. Escape fires it when the dialog
    /// has no [`Self::on_dismiss`].
    #[must_use]
    pub fn secondary_action(
        mut self,
        label: impl Into<String>,
        on_click: impl FnMut() + 'static,
    ) -> Self {
        self.set_secondary_action(Some(DialogAction::new(label, on_click)));
        self
    }

    /// [`Self::secondary_action`] after construction; `None` takes it away.
    pub fn set_secondary_action(&mut self, action: Option<DialogAction>) {
        self.secondary_action = action;
        self.as_dyn_mut().footer_changed();
    }

    /// A third answer for a three-way question (Save / Don't save / Cancel), standing apart from
    /// the other two; Enter never fires it.
    #[must_use]
    pub fn tertiary_action(
        mut self,
        label: impl Into<String>,
        on_click: impl FnMut() + 'static,
    ) -> Self {
        self.set_tertiary_action(Some(DialogAction::new(label, on_click)));
        self
    }

    /// [`Self::tertiary_action`] after construction; `None` takes it away.
    pub fn set_tertiary_action(&mut self, action: Option<DialogAction>) {
        self.tertiary_action = action;
        self.as_dyn_mut().footer_changed();
    }

    /// The tertiary action discards something ("Don't save"): danger ink on
    /// a neutral control.
    #[must_use]
    pub fn with_destructive_tertiary(mut self, on: bool) -> Self {
        self.tertiary_destructive = on;
        self
    }

    /// The primary action deletes, revokes or discards: as on macOS, Enter is then the safe answer
    /// (Cancel) and the sheet opens with the keyboard there.
    #[must_use]
    pub fn with_destructive(mut self, on: bool) -> Self {
        self.set_destructive(on);
        self
    }

    /// [`Self::with_destructive`] after construction.
    pub fn set_destructive(&mut self, on: bool) {
        self.destructive = on;
    }

    /// Whether the primary action can be taken: dimmed, announced as disabled and out of the focus
    /// order when not, and Enter does not fire it.
    pub fn set_primary_enabled(&mut self, enabled: bool) {
        self.primary_enabled = enabled;
    }

    /// [`Self::set_primary_enabled`] on construction.
    #[must_use]
    pub fn with_primary_enabled(mut self, enabled: bool) -> Self {
        self.primary_enabled = enabled;
        self
    }

    /// Replace the gate [`Self::primary_enabled_if`] installed, or take it away.
    pub fn set_primary_gate(&mut self, gate: Option<Box<dyn Fn(&C) -> bool + 'static>>) {
        self.primary_gate = gate.map(|gate| {
            let gate: Box<dyn Fn(&dyn Widget) -> bool> = Box::new(move |content: &dyn Widget| {
                // SAFETY: the gate is only ever called with this dialog's content
                // (`primary_enabled_now`), and the content of a `ModalDialog<C>` is a `C`.
                gate(unsafe { &*std::ptr::from_ref(content).cast::<C>() })
            });
            gate
        });
    }

    /// Enable the primary action only while `gate` holds, read every time the sheet is painted,
    /// announced or fired: a confirmation waiting for a typed phrase, a form waiting for a valid
    /// value.
    #[must_use]
    pub fn primary_enabled_if(mut self, gate: impl Fn(&C) -> bool + 'static) -> Self {
        self.primary_gate = Some(Box::new(move |content: &dyn Widget| {
            // SAFETY: the gate is only ever called with this dialog's content
            // (`primary_enabled_now`), and the content of a `ModalDialog<C>` is
            // a `C`: erasing its type to `dyn Widget` keeps the data pointer.
            gate(unsafe { &*std::ptr::from_ref(content).cast::<C>() })
        }));
        self
    }

    /// The primary action's label, for an action whose name follows the
    /// content ("Delete 3 photos").
    pub fn set_primary_label(&mut self, label: impl Into<String>) {
        let label = label.into();
        if let Some(action) = &mut self.primary_action
            && action.label != label
        {
            action.label = label;
            self.laid_out_for = None;
            self.stale = true;
        }
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
        self.invalidate_layout();
    }

    pub fn content(&self) -> &C {
        &self.content
    }

    pub fn content_mut(&mut self) -> &mut C {
        &mut self.content
    }

    /// Whether the primary action can be taken now.
    pub fn primary_enabled(&self) -> bool {
        self.as_dyn().primary_enabled_now()
    }

    /// This dialog with its content's type erased: the one type its behaviour is compiled for, so
    /// an app links that code from the shared runtime instead of compiling it again.
    fn as_dyn(&self) -> &ModalDialog<dyn Widget> {
        self
    }

    fn as_dyn_mut(&mut self) -> &mut ModalDialog<dyn Widget> {
        self
    }
}

impl<C: Widget + ?Sized> ModalDialog<C> {
    /// Close it as its own way out does, no action running, so it leaves the screen rather than
    /// staying drawn and taking no input; `true` when it was open.
    fn shut(&mut self, source: crate::tokens::MotionSource) -> bool {
        if !self.up() {
            return false;
        }
        self.appear.show_from(false, source);
        self.pointer_inside = false;
        true
    }

    /// Whether it is up: where its appearance is going, which for a dialog on its host's appearance
    /// is the host's own answer.
    fn up(&self) -> bool {
        self.appear.is_open()
    }

    /// Measure and place the content again: call after a change that alters its size without
    /// changing its structure.
    pub fn invalidate_layout(&mut self) {
        self.laid_out_for = None;
        self.stale = true;
        self.remeasure = true;
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
    pub fn tertiary_rect(&self) -> Rect {
        self.slot_rect(Slot::Tertiary)
    }
    /// The close control, when the dialog shows one.
    pub fn close_rect(&self) -> Option<Rect> {
        self.layout.as_ref().and_then(|l| l.close)
    }
    /// The body: where the content shows, scrolled when it is taller.
    pub fn body_rect(&self) -> Rect {
        self.layout.as_ref().map_or(Rect::ZERO, |l| l.body)
    }
    /// The sheet's own header, when it has one.
    pub fn header_rect(&self) -> Option<Rect> {
        self.layout.as_ref().and_then(|l| l.header)
    }
    /// How far the body is scrolled.
    pub fn scroll_offset(&self) -> f64 {
        self.scroll
    }
    pub fn is_open(&self) -> bool {
        self.up()
    }

    /// Whether anything of it is drawn now: open, arriving, or closed and still leaving.
    #[must_use]
    pub fn is_drawn(&self) -> bool {
        self.appear.is_drawn()
    }

    /// How far it has arrived now, `0..=1` (`motion::Appear`).
    #[must_use]
    pub fn appear_progress(&self) -> f64 {
        self.appear.progress()
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

    fn footer_changed(&mut self) {
        self.laid_out_for = None;
        self.structure_dirty = true;
    }
}

impl ModalDialog<dyn Widget> {
    /// Whether the primary action can be taken now.
    fn primary_enabled_now(&self) -> bool {
        self.primary_enabled && self.primary_gate.as_ref().is_none_or(|g| g(&self.content))
    }

    fn closable(&self) -> bool {
        self.dismiss.is_some() && self.secondary_action.is_none()
    }

    /// The close control's address in the accessibility tree.
    fn close_target(&self) -> String {
        format!("dialog:{}:close", self.action_identity)
    }

    fn hovered(&self, rect: Rect) -> bool {
        self.pointer_inside && rect.contains(self.last_cursor)
    }

    fn slot_action(&self, slot: Slot) -> Option<&DialogAction> {
        match slot {
            Slot::Primary => self.primary_action.as_ref(),
            Slot::Secondary => self.secondary_action.as_ref(),
            Slot::Tertiary => self.tertiary_action.as_ref(),
        }
    }

    /// The semantic address of a footer action.
    fn slot_target(&self, slot: Slot) -> String {
        let name = match slot {
            Slot::Primary => "primary",
            Slot::Secondary => "secondary",
            Slot::Tertiary => "tertiary",
        };
        format!("dialog:{}:{name}", self.action_identity)
    }

    fn slot_enabled(&self, slot: Slot) -> bool {
        match slot {
            Slot::Primary => self.primary_enabled_now(),
            Slot::Secondary | Slot::Tertiary => true,
        }
    }

    /// The footer as the shared chrome sees it, with the slot of each entry.
    fn actions(&self) -> (Vec<ModalAction<'_>>, Vec<Slot>) {
        let mut actions = Vec::new();
        let mut slots = Vec::new();
        if let Some(a) = &self.secondary_action {
            actions.push(ModalAction::secondary(&a.label));
            slots.push(Slot::Secondary);
        }
        if let Some(a) = &self.tertiary_action {
            actions.push(if self.tertiary_destructive {
                ModalAction::alternate_danger(&a.label)
            } else {
                ModalAction::alternate(&a.label)
            });
            slots.push(Slot::Tertiary);
        }
        if let Some(a) = &self.primary_action {
            actions.push(
                if self.destructive {
                    ModalAction::danger(&a.label)
                } else {
                    ModalAction::primary(&a.label)
                }
                .enabled(self.primary_enabled_now()),
            );
            slots.push(Slot::Primary);
        }
        (actions, slots)
    }

    /// The footer's slots in Tab order: as they stand, except that the alternate answer comes last
    /// and a destructive confirmation starts on Cancel, never on the answer that destroys
    /// something.
    fn slots_in_focus_order(&self) -> Vec<Slot> {
        let has = |slot: Slot| match slot {
            Slot::Primary => self.primary_action.is_some(),
            Slot::Secondary => self.secondary_action.is_some(),
            Slot::Tertiary => self.tertiary_action.is_some(),
        };
        let order = match self.platform.dialog_action_order() {
            DialogActionOrder::PrimaryFirst if !self.destructive => {
                [Slot::Primary, Slot::Secondary, Slot::Tertiary]
            }
            _ => [Slot::Secondary, Slot::Primary, Slot::Tertiary],
        };
        order.into_iter().filter(|&s| has(s)).collect()
    }

    fn key(&self, theme: &Theme) -> LayoutKey {
        LayoutKey {
            window: self.window,
            text_scale: theme.text_scale,
            ui_scale: theme.ui_scale,
            platform: self.platform,
            content_h: self.content_size.1,
            header_h: self.header_size.1,
        }
    }

    /// Resolve the sheet's geometry in `self.window` around the content's
    /// measured height, keeping the body's trim and scroll inside range.
    fn resolve(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let (actions, slots) = self.actions();
        let spec = ModalSpec::new(&self.title)
            .icon(self.icon)
            .actions(&actions)
            .platform_conventions(self.platform)
            .body_height(self.content_size.1)
            .header_height(self.header_size.1)
            .max_width(self.max_width)
            .closable(self.closable())
            .anchor(self.anchor);
        let mut layout = layout(self.window, &spec, text_renderer, theme);
        drop(actions);
        if self.foot.is_some() {
            rise_from_foot(
                &mut layout,
                self.content_size.1,
                f64::from(theme.tokens.metrics.min_pointer_target),
            );
        }
        if layout.body_overflows && self.trim > 0.0 {
            shorten(&mut layout, self.trim);
        }
        onto_device_pixels(&mut layout);
        let max_scroll = (self.content_size.1 - layout.body.height()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        self.slots = slots;
        let primary_enabled = self.primary_enabled_now();
        let actions: Vec<_> = self
            .keys
            .iter()
            .map(|key| {
                self.slot_action(key.slot)
                    .map(|a| (a.label.clone(), Rc::clone(&a.on_click)))
            })
            .collect();
        for (key, action) in self.keys.iter_mut().zip(actions) {
            key.label = action.as_ref().map(|a| a.0.clone()).unwrap_or_default();
            key.run = action.map(|a| a.1);
            key.enabled = key.slot != Slot::Primary || primary_enabled;
            key.rect = self
                .slots
                .iter()
                .position(|s| *s == key.slot)
                .and_then(|i| layout.actions.get(i).copied())
                .unwrap_or(Rect::ZERO);
        }
        self.layout = Some(layout);
        self.laid_out_for = Some(self.key(theme));
    }

    /// Where the content's top-left belongs now: the body's origin, moved
    /// up by the scroll.
    fn content_target(&self) -> Point {
        let body = self.body_rect();
        Point::new(body.x0, body.y0 - self.scroll)
    }

    /// After the content is placed at the top of an overflowing body, end it on the last whole row,
    /// so the sheet never cuts a row through the middle.
    fn trim_to_content_boundary(&mut self) {
        let Some(layout) = self.layout.as_mut() else {
            return;
        };
        if !layout.body_overflows || self.scroll > 0.0 {
            return;
        }
        let body = layout.body;
        let Some(boundary) = whole_row_top(&self.content, body) else {
            return;
        };
        let trim = body.y1 - boundary;
        shorten(layout, trim);
        self.trim += trim;
        let max_scroll = (self.content_size.1 - layout.body.height()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        for key in &mut self.keys {
            key.rect = key.rect - Vec2::new(0.0, trim);
        }
    }

    /// Lay the content out in its own tree (a painted host keeping the dialog outside its tree):
    /// measure it at the body width, size the sheet to it, place it in the body.
    fn relayout_detached(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        if self.window.width() <= 0.0 || self.window.height() <= 0.0 {
            return;
        }
        let body_w = body_width(self.window, self.max_width, theme);
        let available = Size {
            width: taffy::AvailableSpace::Definite(crate::num::f32_of_f64(body_w)),
            height: taffy::AvailableSpace::MaxContent,
        };
        let content = floating_surface::DetachedContent::lay_out(
            &mut self.content,
            theme,
            available,
            text_renderer,
        );
        self.content_size = content.size();
        self.header_size = (0.0, 0.0);
        if let Some(header) = self.header.as_mut() {
            let laid_out = floating_surface::DetachedContent::lay_out_across(
                header.as_mut(),
                theme,
                body_w,
                text_renderer,
            );
            self.header_size = laid_out.size();
            self.header_detached = Some(laid_out);
        }
        self.trim = 0.0;
        self.resolve(theme, text_renderer);
        self.detached = Some(content);
        self.place_detached();
        self.trim_to_content_boundary();
        self.place_detached();
        self.stale = false;
        self.remeasure = false;
        self.laid_out_for = Some(self.key(theme));
    }

    /// Place the detached content at its target.
    fn place_detached(&mut self) {
        let target = self.content_target();
        if let Some(content) = &self.detached {
            self.content
                .apply_layout(&content.taffy, content.node, target);
            self.content_origin = target;
        }
        self.place_header_detached();
    }

    /// Place a painted host's header at the rect the layout gave it.
    fn place_header_detached(&mut self) {
        let Some(rect) = self.layout.as_ref().and_then(|l| l.header) else {
            return;
        };
        let target = Point::new(rect.x0, rect.y0);
        if let (Some(header), Some(detached)) = (self.header.as_mut(), &self.header_detached) {
            header.apply_layout(&detached.taffy, detached.node, target);
            self.header_origin = target;
        }
    }

    /// Forward `event` to the header. Returns whether the header consumed it.
    fn forward_to_header(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        let Some(header) = self.header.as_mut() else {
            return false;
        };
        floating_surface::DetachedContent::forward(
            header.as_mut(),
            self.header_node,
            self.header_detached.as_ref(),
            event,
            ctx,
        )
    }

    /// Whether `p` is over the header.
    fn over_header(&self, p: Point) -> bool {
        self.header_rect().is_some_and(|header| header.contains(p))
    }

    /// Place the header at the rect the layout gave it, in the host's tree.
    fn place_header_hosted(&mut self, taffy: &TaffyTree<()>) {
        let (Some(rect), Some(header_node)) = (self.header_rect(), self.header_node) else {
            return;
        };
        let cl = taffy.layout(header_node).copied().unwrap_or_default();
        let Some(header) = self.header.as_mut() else {
            return;
        };
        let target = Point::new(rect.x0, rect.y0);
        header.as_mut().apply_layout(
            taffy,
            header_node,
            Point::new(
                target.x - f64::from(cl.location.x),
                target.y - f64::from(cl.location.y),
            ),
        );
        self.header_origin = target;
    }

    /// Forward `event` to the content, returning whether it consumed it; a hosted dialog's content
    /// is addressed by its node in the host's tree.
    fn forward_to_content(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        floating_surface::DetachedContent::forward(
            &mut self.content,
            self.host_content,
            self.detached.as_ref(),
            event,
            ctx,
        )
    }

    fn dismiss(&mut self) {
        if let Some(dismiss) = self.dismiss.as_mut() {
            dismiss.emit();
        } else if let Some(cancel) = self.secondary_action.as_mut() {
            // Escape is Cancel when the sheet names its way out.
            cancel.run();
        }
    }

    fn activate(&mut self, slot: Slot) {
        if !self.slot_enabled(slot) {
            return;
        }
        let action = match slot {
            Slot::Primary => self.primary_action.as_mut(),
            Slot::Secondary => self.secondary_action.as_mut(),
            Slot::Tertiary => self.tertiary_action.as_mut(),
        };
        if let Some(action) = action {
            action.run();
        }
    }

    /// The action Enter takes: the primary action, or Cancel when the primary
    /// action is destructive.
    fn default_slot(&self) -> Option<Slot> {
        if self.destructive {
            self.secondary_action.is_some().then_some(Slot::Secondary)
        } else {
            Some(Slot::Primary)
        }
    }

    /// The footer action keyboard focus is on.
    fn focused_slot(&self) -> Option<Slot> {
        self.keys.iter().find(|k| k.focused).map(|k| k.slot)
    }

    /// Scroll the body by `dy` (positive moves the content up). Returns
    /// whether it moved.
    fn scroll_by(&mut self, dy: f64) -> bool {
        let Some(layout) = &self.layout else {
            return false;
        };
        if !layout.body_overflows {
            return false;
        }
        let max_scroll = (self.content_size.1 - layout.body.height()).max(0.0);
        let next = (self.scroll + dy).clamp(0.0, max_scroll);
        if (next - self.scroll).abs() < 0.01 {
            return false;
        }
        self.scroll = next;
        if self.host_content.is_some() {
            self.stale = true;
        } else {
            self.place_detached();
        }
        true
    }

    /// Paint the hairlines at the body's scrolled edges and the scroll
    /// thumb, when the body scrolls.
    fn paint_scroll_edges(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let Some(layout) = &self.layout else { return };
        if !layout.body_overflows {
            return;
        }
        let body = layout.body;
        let card = layout.card;
        let rule = f64::from(theme.tokens.metrics.chrome_rule_px).max(1.0);
        let hairline = crate::chrome::chrome_hairline(theme);
        let max_scroll = (self.content_size.1 - body.height()).max(0.0);
        if self.scroll > 0.5 {
            scene.fill(
                Fill::NonZero,
                transform,
                hairline,
                None,
                &Rect::new(card.x0, body.y0 - rule, card.x1, body.y0),
            );
        }
        if self.scroll < max_scroll - 0.5 {
            scene.fill(
                Fill::NonZero,
                transform,
                hairline,
                None,
                &Rect::new(card.x0, body.y1, card.x1, body.y1 + rule),
            );
        }
        let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        let track = Rect::new(card.x1 - gutter, body.y0, card.x1, body.y1);
        crate::widget::scrollbar::Scrollbar::vertical().draw(
            scene,
            transform,
            track,
            crate::widget::scrollbar::ScrollMetrics::new(
                self.content_size.1,
                body.height(),
                self.scroll,
            ),
            theme,
            false,
        );
    }

    /// Whether an overlay the content opened (a dropdown's list hanging
    /// below the sheet) is up: it gets the first claim on presses.
    fn content_overlay_open(&self) -> bool {
        self.content.wants_pointer_capture() || self.content.modal_capture().is_some()
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
        state: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        if !self.up() {
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
                state: state.reborrow(),
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

/// The dialog's behaviour, compiled once for every content type: a
/// `ModalDialog<C>` forwards each method to its type-erased form.
impl Widget for ModalDialog<dyn Widget> {
    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        let reorders = self.platform.dialog_action_order() != conventions.dialog_action_order();
        self.platform = conventions;
        self.content.set_platform_conventions(conventions);
        if let Some(header) = self.header.as_deref_mut() {
            header.set_platform_conventions(conventions);
        }
        if reorders && self.host_content.is_some() {
            self.structure_dirty = true;
        }
        self.laid_out_for = None;
        let theme = self.layout_theme.clone();
        crate::text_render::with_layout_renderer(theme.text_scale, |tr| {
            if self.host_content.is_some() {
                self.resolve(&theme, tr);
            } else {
                self.relayout_detached(&theme, tr);
            }
        });
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        // The dialog holds no document state itself; its content does.
        self.content.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        // A move the top layer found (a new window, a scroll) is laid out.
        if self.up() && (self.stale || self.remeasure) {
            crate::widget::note_layout_moved();
        }
        super::defaults::update(self.children_mut(), state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        self.structure_dirty = false;
        self.detached = None;
        // The content is laid out in the host's tree, so its nodes are the
        // window's: a field's focus request and the focus order name the
        // same nodes. The dialog's node covers its parent out of flow;
        // `adjust_layout_style` narrows its content box to the body.
        let content_node = self.content.build_layout(taffy, theme, node_map);
        self.host_content = Some(content_node);
        self.header_detached = None;
        let header_node = self
            .header
            .as_mut()
            .map(|header| header.as_mut().build_layout(taffy, theme, node_map));
        self.header_node = header_node;
        let dialog = self.test_id.clone();
        self.keys = self
            .slots_in_focus_order()
            .into_iter()
            .map(|slot| {
                FooterKey::new(
                    slot,
                    self.slot_action(slot),
                    dialog.as_deref(),
                    self.slot_target(slot),
                )
            })
            .collect();

        let mut children = vec![content_node];
        children.extend(header_node);
        for key in &mut self.keys {
            children.push(key.build_layout(taffy, theme, node_map));
        }
        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Block,
                position: taffy::Position::Absolute,
                inset: taffy::Rect {
                    left: taffy::LengthPercentageAuto::length(0.0),
                    top: taffy::LengthPercentageAuto::length(0.0),
                    right: taffy::LengthPercentageAuto::auto(),
                    bottom: taffy::LengthPercentageAuto::auto(),
                },
                size: Size::from_percent(1.0, 1.0),
                ..Default::default()
            },
            &children,
        );
        self.remeasure = true;
        node
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut TextRenderer,
    ) -> bool {
        let Ok(l) = taffy.layout(node).copied() else {
            return false;
        };
        let node_w = f64::from(l.size.width);
        // Compact presentation depends on both viewport axes. A synthetic
        // one-pixel height would classify every desktop sheet as a page and
        // measure its body at the wrong width, leaving it perpetually dirty.
        // A presented dialog's last painted viewport can precede a resize.
        // Measure against the current host tree before the next paint, so
        // its body and wrapped text use the width they will actually occupy.
        let window = self
            .window_node
            .and_then(|window| window_node_rect(taffy, node, window))
            .or_else(|| tree_window(taffy, node))
            .unwrap_or_else(|| {
                if self.presented {
                    self.window
                } else {
                    Rect::new(0.0, 0.0, node_w, f64::from(l.size.height))
                }
            });
        let body_w = body_width(window, self.max_width, &self.layout_theme);
        let pad = crate::num::f32_of_f64((node_w - body_w).max(0.0));
        let Ok(mut style) = taffy.style(node).cloned() else {
            return false;
        };
        // Whole pixels, so the comparison settles instead of chasing a
        // rounding difference between passes.
        let wanted = taffy::LengthPercentage::length(pad.floor());
        if style.padding.right == wanted {
            return false;
        }
        style.padding.right = wanted;
        taffy.set_style(node, style).is_ok()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // Until the top layer has been painted over the window once, the
        // root of the tree the dialog stands in is the window: the sheet is
        // centred where the first paint will put it, so the content is
        // placed where it is drawn and hit from the first frame.
        let host_window = current_window();
        self.window_node = host_window.map(|(window, _)| window);
        if !self.presented {
            self.window = host_window.map(|(_, rect)| rect).unwrap_or_else(|| {
                tree_window(taffy, node)
                    .unwrap_or_else(|| crate::widget::layout_rect(taffy, node, origin))
            });
        }
        let Some(content_node) = taffy
            .children(node)
            .ok()
            .and_then(|c| c.first().copied())
            .filter(|&c| Some(c) == self.host_content)
        else {
            return;
        };
        let cl = taffy.layout(content_node).copied().unwrap_or_default();
        let size = (f64::from(cl.size.width), f64::from(cl.size.height));
        if (size.1 - self.content_size.1).abs() > 0.5 {
            self.trim = 0.0;
        }
        self.content_size = size;
        self.header_size = self
            .header_node
            .and_then(|header| taffy.layout(header).ok())
            .map_or((0.0, 0.0), |l| {
                (f64::from(l.size.width), f64::from(l.size.height))
            });
        let theme = self.layout_theme.clone();
        crate::text_render::with_layout_renderer(theme.text_scale, |tr| self.resolve(&theme, tr));
        self.content_laid_out_in = self.body_rect().width();
        let place = |dialog: &mut Self| {
            let target = dialog.content_target();
            dialog.content.apply_layout(
                taffy,
                content_node,
                Point::new(
                    target.x - f64::from(cl.location.x),
                    target.y - f64::from(cl.location.y),
                ),
            );
            dialog.content_origin = target;
        };
        place(self);
        self.place_header_hosted(taffy);
        if self.scroll <= 0.0 && self.trim == 0.0 {
            self.trim_to_content_boundary();
        }
        self.stale = false;
        self.remeasure = false;
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The sheet arrives and leaves (`motion::Appear`); a leaving sheet is drawn until it is
        // gone but takes no input.
        if !self.appear.is_drawn() {
            return;
        }
        // Each change starts on the motion the person allows, which only the theme says.
        self.appear.follow(self.up(), theme.motion());
        // The host pushes the window as the presence viewport for its
        // top-layer pass: the scrim covers all of it.
        if let Some(window) = presence.viewport.get() {
            self.window = window;
            self.presented = true;
        }
        // A leaving sheet is drawn as it was laid out: it asks the host for
        // nothing more.
        if self.up() && self.host_content.is_some() {
            if self.laid_out_for != Some(self.key(theme)) {
                self.resolve(theme, text_renderer);
                // The content follows on the host's next layout pass; until
                // then it is drawn moved to where it belongs.
                self.stale = true;
            }
            let body_w = self.body_rect().width();
            if (self.content_laid_out_in - body_w).abs() > 1.0 {
                self.remeasure = true;
                self.stale = true;
            }
        } else if self.up()
            && (self.laid_out_for != Some(self.key(theme)) || self.detached.is_none())
        {
            self.relayout_detached(theme, text_renderer);
        }
        let Some(layout) = self.layout.clone() else {
            return;
        };
        // A gate can change with any keystroke: the footer's buttons are
        // announced with the state the sheet is painted with.
        let primary_enabled = self.primary_enabled_now();
        for key in &mut self.keys {
            key.enabled = key.slot != Slot::Primary || primary_enabled;
        }
        // The sheet a compact window raises from its foot, painted under the title.
        let mut foot = self.foot.clone().filter(|_| layout.foot);
        let focused = self.focused_slot();
        let (mut actions, slots) = self.actions();
        for ((action, rect), slot) in actions.iter_mut().zip(&layout.actions).zip(&slots) {
            *action = action
                .hovered(self.pointer_inside && rect.contains(self.last_cursor))
                .focused(focused == Some(*slot));
        }
        let close_hovered = layout.close.is_some_and(|c| self.hovered(c));
        let spec = ModalSpec::new(&self.title)
            .icon(self.icon)
            .actions(&actions)
            .platform_conventions(self.platform)
            .closable(layout.close.is_some())
            .close_hovered(close_hovered);
        // A floating sheet scales about its centre; a full-window page only fades, and the scrim
        // fades with it, cut out where the sheet is drawn.
        let mut pose = self.appear.pose(layout.card, layout.card.center());
        // A sheet from the foot stands on the window's edge: it fades, as a
        // page does, rather than growing from its centre.
        if layout.card == layout.scrim || layout.foot {
            pose.transform = Affine::IDENTITY;
        }
        let scrim = crate::motion::Pose {
            opacity: pose.opacity,
            transform: Affine::IDENTITY,
        };
        if let Some(at) = scrim.begin(scene, transform, layout.scrim) {
            paint_scrim(scene, at, theme, &layout, pose.drawn_rect(layout.card));
            scrim.end(scene);
        }
        let Some(transform) = pose.begin(scene, transform, layout.scrim) else {
            return;
        };
        if let Some(sheet) = &mut foot {
            sheet.place(
                super::SheetFrame {
                    area: layout.card,
                    peek: 0.0,
                    grip: f64::from(theme.tokens.metrics.min_pointer_target),
                    least_half: 0.0,
                },
                1.0,
                true,
            );
            sheet.paint(scene, transform, theme);
        }
        paint_sheet(scene, transform, text_renderer, theme, &layout, &spec);
        paint_actions(scene, transform, text_renderer, theme, &layout, &spec);
        drop(actions);

        // The header stands on the sheet above the body, which scrolls
        // under it: it is drawn where it is, never moved by the scroll.
        let on_panel = on_sheet(theme);
        if let (Some(header), Some(rect)) = (self.header.as_mut(), layout.header) {
            let shift = Point::new(rect.x0, rect.y0) - self.header_origin;
            header.draw(
                scene,
                transform * Affine::translate(shift),
                text_renderer,
                &on_panel,
                presence,
            );
        }
        if layout.body_overflows {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                transform,
                &layout.body,
            );
        }
        // The body stands on the sheet: its controls lift off the panel.
        let shift = self.content_target() - self.content_origin;
        let content_transform = transform * Affine::translate(shift);
        self.content
            .draw(scene, content_transform, text_renderer, &on_panel, presence);
        if layout.body_overflows {
            scene.pop_layer();
        }
        self.paint_scroll_edges(scene, transform, theme);
        // An overlay the content opens (a dropdown's list) may hang past
        // the body, so it is drawn after the clip.
        self.content
            .draw_overlay(scene, content_transform, text_renderer, &on_panel, presence);
        pose.end(scene);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.up() {
            return;
        }
        // Blocking: while open, every event is marked handled so the dialog
        // stays modal even hosted directly (tests, headless).
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                self.pointer_inside = true;
                // A TextInput only takes a click while hovered, and this
                // CursorMoved is what arms it.
                self.forward_to_header(event, ctx);
                self.forward_to_content(event, ctx);
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer_inside = false;
                self.forward_to_header(event, ctx);
                self.forward_to_content(event, ctx);
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
                // An open list in the content closes first.
                if !(self.content_overlay_open() && self.forward_to_content(event, ctx)) {
                    self.dismiss();
                }
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(key @ (NamedKey::Enter | NamedKey::Space)),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if let Some(slot) = self.focused_slot() {
                    // Focus on a footer action: Enter and Space take it.
                    self.activate(slot);
                } else {
                    // A control that reads the key itself keeps it (a multiline field, an open
                    // list); Space then operates the focused control, and Enter the focused button
                    // or else the sheet's default answer.
                    let enter = *key == NamedKey::Enter;
                    let content_handled = self.forward_to_header(event, ctx)
                        || self.forward_to_content(event, ctx)
                        || activate_focused(&mut self.content, &mut ctx.state, ctx.path, enter);
                    if !content_handled
                        && enter
                        && let Some(slot) = self.default_slot()
                    {
                        self.activate(slot);
                    }
                }
                *ctx.handled = true;
            }
            // Text arrives from an input method, not only as key presses.
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => {
                self.forward_to_header(event, ctx);
                self.forward_to_content(event, ctx);
                *ctx.handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // The body scrolls under the header: the wheel works over
                // either, and the header keeps no list of its own.
                let over_body = self.body_rect().contains(self.last_cursor)
                    || self.over_header(self.last_cursor);
                let consumed = (over_body || self.content_overlay_open())
                    && self.forward_to_content(event, ctx);
                if !consumed && over_body {
                    let dy = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => {
                            -f64::from(*y) * f64::from(theme_line(&self.layout_theme))
                        }
                        winit::event::MouseScrollDelta::PixelDelta(p) => -p.y / ctx.scale_factor,
                    };
                    self.scroll_by(dy);
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // An overlay the content opened claims the press before the sheet does: a pick in a
                // dropdown's list, or the press that closes it.
                if self.content_overlay_open() && self.forward_to_content(event, ctx) {
                    *ctx.handled = true;
                    return;
                }
                let p = self.last_cursor;
                let hit = self.layout.as_ref().and_then(|l| l.hit(p));
                match hit {
                    Some(ModalHit::Close | ModalHit::Scrim) | None => self.dismiss(),
                    Some(ModalHit::Action(i)) => {
                        if let Some(slot) = self.slots.get(i).copied() {
                            self.activate(slot);
                        }
                    }
                    Some(ModalHit::Card) => {
                        if self.over_header(p) {
                            self.forward_to_header(event, ctx);
                        } else if self.body_rect().contains(p) {
                            self.forward_to_content(event, ctx);
                        }
                    }
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput { .. } => {
                // In-sheet presses still reach the content, so a right-click
                // affordance in it can react, and a drag ends where it began.
                if self.over_header(self.last_cursor) {
                    self.forward_to_header(event, ctx);
                } else if self.dialog_rect().contains(self.last_cursor)
                    || self.content_overlay_open()
                {
                    self.forward_to_content(event, ctx);
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
        self.up().then(|| self.dialog_rect())
    }

    /// The shell took the sheet down: close it as its own way out does (no answer runs), so it
    /// leaves the screen.
    fn dismiss_presented_modal(&mut self) -> bool {
        // The shell took the surface down; no shortcut of the app's asked.
        self.shut(crate::tokens::MotionSource::Pointer)
    }

    fn presents_modal(&self) -> bool {
        self.is_drawn()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        // While it leaves, its card is still drawn: its content stays with
        // it until nothing of it is left.
        if !self.is_drawn() {
            return Vec::new();
        }
        let mut children: Vec<&dyn Widget> = Vec::new();
        children.extend(self.header.as_deref().map(|h| h as &dyn Widget));
        children.push(&self.content);
        children.extend(self.keys.iter().map(|k| k as &dyn Widget));
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if self.is_drawn() {
            if let Some(header) = self.header.as_deref() {
                visitor(header);
            }
            visitor(&self.content);
            for key in &self.keys {
                visitor(key);
            }
        }
    }

    /// Laid out also while closed.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        if let Some(header) = self.header.as_deref() {
            visitor(header);
        }
        visitor(&self.content);
        for key in &self.keys {
            visitor(key);
        }
        true
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if !self.is_drawn() {
            return Vec::new();
        }
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::new();
        if let Some(header) = self.header.as_deref_mut() {
            children.push(header as &mut dyn Widget);
        }
        children.push(&mut self.content);
        children.extend(self.keys.iter_mut().map(|k| k as &mut (dyn Widget + '_)));
        children
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let drawn = self.is_drawn();
        let content = drawn && self.content.take_structure_dirty();
        let header = drawn
            && self
                .header
                .as_deref_mut()
                .is_some_and(Widget::take_structure_dirty);
        if content || header {
            self.invalidate_layout();
        }
        if self.host_content.is_some() {
            own || content || header
        } else {
            // Nobody else lays a painted host's dialog out: it re-measures
            // on its next paint.
            false
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.stale
            || self.remeasure
            || (self.is_drawn()
                && (self.content.is_layout_dirty()
                    || self.header.as_deref().is_some_and(Widget::is_layout_dirty)))
    }

    fn layout_needs_measure(&self) -> bool {
        self.remeasure
            || (self.is_drawn()
                && (self.content.layout_needs_measure()
                    || self
                        .header
                        .as_deref()
                        .is_some_and(Widget::layout_needs_measure)))
    }

    fn needs_redraw(&self) -> bool {
        // Opening, closing and every move are marked stale; an open sheet
        // otherwise changes only through its content or an event.
        self.stale
            || self.appear.is_moving()
            || (self.is_drawn()
                && (self.content.needs_redraw()
                    || self.content.needs_animation_frame()
                    || self
                        .header
                        .as_deref()
                        .is_some_and(|h| h.needs_redraw() || h.needs_animation_frame())))
    }

    fn needs_animation_frame(&self) -> bool {
        // A move the top layer found (a new window, a scroll) lands on the
        // host's next layout pass.
        self.appear.is_moving()
            || (self.is_drawn()
                && (self.stale
                    || self.content.needs_animation_frame()
                    || self
                        .header
                        .as_deref()
                        .is_some_and(Widget::needs_animation_frame)))
    }

    fn get_rect(&self) -> Rect {
        self.dialog_rect()
    }

    fn traps_focus(&self) -> bool {
        self.up()
    }

    // A closed dialog is hidden from the tree.
    fn accessibility_hidden(&self) -> bool {
        !self.is_drawn()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // The title is the accessible name (FTL-resolved).
        let title = crate::i18n!(&self.title);
        let name = (!title.is_empty()).then_some(title);
        let mut node = AccessNode::new(Role::Dialog)
            .with_optional_name(name)
            .with_state(AccessState {
                modal: true,
                hidden: self.accessibility_hidden(),
                ..Default::default()
            });
        node.description.clone_from(&self.description);
        // Footer actions a host laid out are buttons of their own focus
        // nodes; a dialog no host has built names them itself.
        if self.up() && self.keys.is_empty() {
            for (key, index, action, bounds, enabled) in [
                (
                    "primary",
                    1,
                    &self.primary_action,
                    self.primary_rect(),
                    self.primary_enabled_now(),
                ),
                (
                    "secondary",
                    2,
                    &self.secondary_action,
                    self.secondary_rect(),
                    true,
                ),
                (
                    "tertiary",
                    4,
                    &self.tertiary_action,
                    self.tertiary_rect(),
                    true,
                ),
            ] {
                if let Some(action) = action {
                    let mut state = AccessState::new().focusable(enabled);
                    state.disabled = !enabled;
                    node.children.push(
                        AccessNode::new(Role::Button)
                            .with_name(crate::i18n!(&action.label))
                            .with_key(index)
                            .with_state(state)
                            .with_bounds(bounds)
                            .with_action_target(format!("dialog:{}:{key}", self.action_identity)),
                    );
                }
            }
        }
        if self.up() && self.closable() {
            node.children.push(
                AccessNode::new(Role::Button)
                    .with_name(crate::i18n!("Close"))
                    .with_key(3)
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(self.close_rect().unwrap_or(Rect::ZERO))
                    .with_action_target(self.close_target()),
            );
        }
        Some(node)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _: &mut crate::widget::Edits<'_>,
        _: &ModelPath,
    ) -> bool {
        if !self.up() || !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let id = self.action_identity;
        let is = |name: &str| {
            target == format!("modal.{name}") || target == format!("dialog:{id}:{name}")
        };
        if is("primary") && self.primary_action.is_some() && self.primary_enabled_now() {
            self.activate(Slot::Primary);
            true
        } else if is("secondary") && self.secondary_action.is_some() {
            self.activate(Slot::Secondary);
            true
        } else if is("tertiary") && self.tertiary_action.is_some() {
            self.activate(Slot::Tertiary);
            true
        } else if is("close") && self.closable() {
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

impl<C: Widget + 'static> Widget for ModalDialog<C> {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::widget::ResourceOperations
    );

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.as_dyn_mut().set_platform_conventions(conventions);
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.as_dyn_mut().sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.as_dyn_mut().update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.as_dyn_mut().build_layout(taffy, theme, node_map);
        // The dialog's node is the dialog: a host reaching it through the
        // map meets this widget, not its type-erased form.
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        self.as_dyn_mut().adjust_layout_style(taffy, node, text)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.as_dyn_mut().apply_layout(taffy, node, origin);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.as_dyn_mut()
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.as_dyn_mut().handle_event(event, ctx);
    }

    fn name(&self) -> &'static str {
        "ModalDialog"
    }

    fn test_id(&self) -> Option<&str> {
        self.as_dyn().test_id()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.as_dyn().modal_capture()
    }

    fn dismiss_presented_modal(&mut self) -> bool {
        self.as_dyn_mut().dismiss_presented_modal()
    }

    fn presents_modal(&self) -> bool {
        self.as_dyn().presents_modal()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.as_dyn().children()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        self.as_dyn().for_each_child(visitor);
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        self.as_dyn().for_each_layout_child(visitor)
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.as_dyn_mut().children_mut()
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.as_dyn_mut().take_structure_dirty()
    }

    fn is_layout_dirty(&self) -> bool {
        self.as_dyn().is_layout_dirty()
    }

    fn layout_needs_measure(&self) -> bool {
        self.as_dyn().layout_needs_measure()
    }

    fn needs_redraw(&self) -> bool {
        self.as_dyn().needs_redraw() || crate::widget::Widget::needs_animation_frame(self)
    }

    fn needs_animation_frame(&self) -> bool {
        self.as_dyn().needs_animation_frame()
    }

    fn get_rect(&self) -> Rect {
        self.as_dyn().get_rect()
    }

    fn traps_focus(&self) -> bool {
        self.as_dyn().traps_focus()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.as_dyn().accessibility()
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        arg2: &mut crate::widget::Edits<'_>,
        arg3: &ModelPath,
    ) -> bool {
        self.as_dyn_mut()
            .perform_semantic_action(target, action, arg2, arg3)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.as_dyn().synthetic_accessibility_rect(path)
    }
}

thread_local! {
    /// The window whose content is being placed ([`within_window`]).
    static LAYOUT_WINDOW: std::cell::Cell<Option<(NodeId, Rect)>> =
        const { std::cell::Cell::new(None) };
}

/// Place a window's content (`place` runs its `apply_layout`) knowing which window it is: a host
/// laying several windows out in one tree (the desktop's strip) calls this so a dialog centres on
/// its own window rather than the shared tree's root.
pub fn within_window<R>(window: NodeId, rect: Rect, place: impl FnOnce() -> R) -> R {
    /// Restores the outer window, also when a contained app's layout
    /// panics and the host recovers.
    struct Restore(Option<(NodeId, Rect)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LAYOUT_WINDOW.with(|cell| cell.set(self.0));
        }
    }
    let _restore = Restore(LAYOUT_WINDOW.with(|cell| cell.replace(Some((window, rect)))));
    place()
}

/// The window being placed, if a host named one ([`within_window`]).
fn current_window() -> Option<(NodeId, Rect)> {
    LAYOUT_WINDOW.with(std::cell::Cell::get)
}

/// The current size of `window`, at the origin; `None` when the tree was rebuilt and `window` is no
/// longer an ancestor of `node`.
fn window_node_rect(taffy: &TaffyTree<()>, node: NodeId, window: NodeId) -> Option<Rect> {
    let mut at = node;
    while at != window {
        at = taffy.parent(at)?;
    }
    let l = taffy.layout(window).ok()?;
    let rect = Rect::new(0.0, 0.0, f64::from(l.size.width), f64::from(l.size.height));
    (rect.width() > 0.0 && rect.height() > 0.0).then_some(rect)
}

/// The window a node stands in: the root of its layout tree.
fn tree_window(taffy: &TaffyTree<()>, node: NodeId) -> Option<Rect> {
    let mut root = node;
    while let Some(parent) = taffy.parent(root) {
        root = parent;
    }
    let l = taffy.layout(root).ok()?;
    let window = Rect::from_origin_size(
        (f64::from(l.location.x), f64::from(l.location.y)),
        (f64::from(l.size.width), f64::from(l.size.height)),
    );
    (window.width() > 0.0 && window.height() > 0.0).then_some(window)
}

/// Offer the focused control under `widget` the activation the window gives on Space, or on Enter
/// (`buttons_only`) only when it is a button; whether it took it.
fn activate_focused(
    widget: &mut dyn Widget,
    state: &mut crate::widget::Edits<'_>,
    path: &ModelPath,
    buttons_only: bool,
) -> bool {
    if widget.is_focused() && widget.is_focusable() {
        let button = widget
            .accessibility()
            .is_some_and(|node| node.role == crate::a11y::Role::Button);
        return (button || !buttons_only)
            && widget.perform_action(&crate::a11y::Action::Activate, &[], state, path);
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| activate_focused(child, state, path, buttons_only))
}

/// How far one wheel notch scrolls a sheet's body: three lines of body text.
fn theme_line(theme: &Theme) -> f32 {
    theme.text(TextRole::Body).line_height_px() * 3.0
}
mod body;
mod close_control;
mod footer_key;
mod geometry;
use body::{shorten, whole_row_top};
use footer_key::FooterKey;
use geometry::onto_device_pixels;
pub use geometry::{layout, rise_from_foot};

#[cfg(test)]
#[path = "modal_dialog_tests.rs"]
mod tests;

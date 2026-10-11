//! The one modal: a sheet over a scrim that covers the whole window.
//!
//! [`ModalDialog`] is the retained widget: a title, any content widget, and
//! up to three footer actions. It is always presented in the window's top
//! layer ([`Widget::draw_overlay`]), after the app and the window's own
//! chrome (the History bar, sidebars), so its scrim covers all of them
//! wherever in the tree the dialog is mounted. Escape or a click on the scrim
//! dismisses; Enter fires the default action unless the content consumes it.
//! The default is the primary action, except in a destructive confirmation,
//! where it is the safe answer (Cancel), as on macOS.
//! Content taller than the window allows scrolls inside the body.
//!
//! A painted host that asks a question without a retained tree uses
//! [`ModalSheet`]: it paints the sheet, keeps its layout for hit-testing and
//! turns input into a [`SheetResponse`]. Hosts that need only the chrome
//! (the identity ceremonies, shell sheets) use [`layout`],
//! [`paint_backdrop`] and [`paint_actions`]:
//!
//! ```ignore
//! let layout = modal_dialog::layout(window, &spec, text_renderer, theme);
//! modal_dialog::paint_backdrop(scene, transform, text_renderer, theme, &layout, &spec);
//! // ... caller draws its own content inside `layout.body` ...
//! modal_dialog::paint_actions(scene, transform, text_renderer, theme, &layout, &spec);
//! ```
//!
//! On a roomy viewport the sheet is a floating Panel ([`super::floating_surface`]):
//! panel radius, the blurred material, one soft shadow. On a compact viewport
//! it is an opaque full-window page with no floating edge. A floating sheet
//! follows its content height;
//! its title is a `Title`; its actions are Regular buttons anchored to the
//! footer in the platform's order. Dialog callbacks are host-supplied
//! closures and do not flow through the document.

use crate::widget::{AppModel, ModelPath};
#[cfg(test)]
use automerge::AutoCommit;
use automerge::ObjId;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
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
    /// A third answer that is neither the default nor the way out ("Don't
    /// save"). It stands apart from the other two at the footer's leading
    /// edge where the platform puts the primary action last.
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
    /// Index of the action Enter fires, if any: the primary action, or
    /// Cancel when the primary action is destructive.
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

/// A small viewport presents a modal as a page instead of a floating card.
/// The sheet-width role grows with the theme's UI and text scale; checking
/// both axes also keeps phone landscape and short viewports usable.
pub fn uses_full_window(window: Rect, theme: &Theme) -> bool {
    window.width().min(window.height()) < f64::from(theme.tokens.metrics.panel_width_sheet)
}

/// Width of the sheet for `max_width` (design pixels) inside `window`,
/// knowable before any body row is measured. Widths grow with the text
/// scale, because what fills a sheet is text. A floating sheet keeps the
/// content gutter clear of every window edge; a compact page fills the window
/// and keeps that gutter inside its body.
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

/// The part of `window` a sheet `wanted_h` tall may cover. A sheet that
/// fits below the window's header row (and clear of the bottom edge by the
/// content gutter) stays below it, so the row's controls stay readable. A
/// taller one takes the whole window inside the gutter: its own content
/// and footer matter more than the header row it covers.
fn sheet_room(window: Rect, wanted_h: f64, theme: &Theme) -> Rect {
    if uses_full_window(window, theme) {
        return window;
    }
    let pad = pad(theme);
    let header = f64::from(theme.tokens.metrics.header_height);
    let bottom = (window.y1 - pad).max(window.y0);
    let below_header = (window.y0 + header).min(bottom);
    let top = if wanted_h <= bottom - below_header {
        below_header
    } else {
        (window.y0 + pad).min(bottom)
    };
    Rect::new(window.x0, top, window.x1, bottom)
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
    let row_h = title_row_height(theme);
    let close_side = f64::from(theme.tokens.metrics.min_pointer_target);
    // The close control's glyph lines up with the content's trailing edge,
    // so its hit target reaches a little into the padding.
    let close_overhang = (close_side - f64::from(theme.tokens.metrics.icon_md)) / 2.0;
    // The title stops where the close control starts, so a long title wraps
    // (and then elides) instead of running under it.
    let title_w = if spec.closable {
        w - pad * 2.0 + close_overhang - close_side - f64::from(spacing.s2)
    } else {
        w - pad * 2.0
    }
    .max(0.0);
    let lines = title_lines(text_renderer, spec.title, title_w, theme).len();
    let line_h = f64::from(theme.text(TextRole::Title).line_height_px());
    let title_h = if has_title {
        (line_h * lines.max(1) as f64).max(row_h)
    } else {
        0.0
    };
    let title_gap = if has_title {
        f64::from(spacing.s4)
    } else {
        0.0
    };
    let footer = footer_geometry(spec, (w - pad * 2.0).max(0.0), text_renderer, theme);
    let footer_h = footer.height;
    let chrome_h = pad * 2.0 + title_h + title_gap + footer_h;
    let wanted_h = chrome_h + spec.body_height.max(0.0);
    let room = sheet_room(window, wanted_h, theme);
    let avail_h = room.height();
    let h = if uses_full_window(window, theme) {
        avail_h
    } else {
        wanted_h.min(avail_h)
    };
    // The sheet never grows past the window: a footer nobody can reach is
    // worse than a body that scrolls.
    let body_overflows = wanted_h > avail_h + 0.5;

    let c = window.center();
    let top = (c.y - h / 2.0).max(room.y0);
    let card = if uses_full_window(window, theme) {
        window
    } else {
        Rect::new(c.x - w / 2.0, top, c.x + w / 2.0, top + h)
    };
    let title_top = card.y0 + pad;

    let close = spec.closable.then(|| {
        let x1 = card.x1 - pad + close_overhang;
        let mid = title_top + row_h / 2.0;
        Rect::new(
            x1 - close_side,
            mid - close_side / 2.0,
            x1,
            mid + close_side / 2.0,
        )
    });
    let title = Rect::new(
        card.x0 + pad,
        title_top,
        card.x0 + pad + title_w,
        title_top + title_h,
    );

    let body_top = title.y1 + title_gap;
    let body = Rect::new(
        card.x0 + pad,
        body_top,
        card.x1 - pad,
        (card.y1 - pad - footer_h).max(body_top),
    );

    let actions = place_actions(spec, card, &footer, theme);
    let destructive = spec
        .actions
        .iter()
        .any(|a| a.emphasis == ModalEmphasis::Danger);
    let primary = if destructive {
        spec.actions
            .iter()
            .position(|a| a.emphasis == ModalEmphasis::Secondary && a.enabled)
    } else {
        spec.actions
            .iter()
            .position(|a| a.is_default() && a.enabled)
    };

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

struct FooterGeometry {
    widths: Vec<f64>,
    stacked: bool,
    height: f64,
}

/// Reserve the footer before allocating body space. Labels are measured in
/// the real control typography, so larger text and translations can wrap
/// the action group into separate rows without covering the body.
fn footer_geometry(
    spec: &ModalSpec<'_>,
    available_width: f64,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) -> FooterGeometry {
    let spacing = &theme.tokens.spacing;
    let label = ControlSize::Regular.label_style(theme);
    let label_pad = f64::from(spacing.s4);
    let min_w = f64::from(spacing.s12) * 2.0;
    let widths: Vec<_> = spec
        .actions
        .iter()
        .map(|action| {
            let (width, _) = text_renderer.measure_style(action.label, None, &label);
            (f64::from(width) + label_pad * 2.0).max(min_w)
        })
        .collect();
    let gap = f64::from(spacing.s2);
    let gaps = widths.len().saturating_sub(1) as f64;
    let stacked = widths.iter().sum::<f64>() + gaps * gap > available_width;
    let height = if widths.is_empty() {
        0.0
    } else {
        f64::from(spacing.s6)
            + if stacked {
                widths.len() as f64 * action_height(theme) + gaps * gap
            } else {
                action_height(theme)
            }
    };
    FooterGeometry {
        widths,
        stacked,
        height,
    }
}

/// The footer's action rects, indexed by the authored action while the
/// visual order follows emphasis and the platform's convention. Where the
/// primary action stands last, an alternate answer stands apart at the
/// leading edge (Don't save … Cancel, Save); where it stands first, the
/// alternate follows it (Save, Don't save, Cancel).
fn place_actions(
    spec: &ModalSpec<'_>,
    card: Rect,
    footer: &FooterGeometry,
    theme: &Theme,
) -> Vec<Rect> {
    let mut actions = vec![Rect::ZERO; spec.actions.len()];
    if spec.actions.is_empty() {
        return actions;
    }
    let spacing = &theme.tokens.spacing;
    let pad = pad(theme);
    let button_h = action_height(theme);
    let gap = f64::from(spacing.s2);
    let by = card.y1 - pad - button_h;
    let order = spec.platform.dialog_action_order();
    let (leading, mut trailing): (Vec<usize>, Vec<usize>) =
        (0..spec.actions.len()).partition(|&i| {
            order == DialogActionOrder::PrimaryLast
                && spec.actions.get(i).is_some_and(ModalAction::is_alternate)
        });
    trailing.sort_by_key(|&index| {
        let Some(a) = spec.actions.get(index) else {
            return 3;
        };
        match (order, a.is_default(), a.is_alternate()) {
            (DialogActionOrder::PrimaryFirst, true, _) => 0,
            (DialogActionOrder::PrimaryFirst, _, true) => 1,
            (DialogActionOrder::PrimaryFirst, _, _)
            | (DialogActionOrder::PrimaryLast, false, _) => 2,
            (DialogActionOrder::PrimaryLast, true, _) => 3,
        }
    });
    if footer.stacked {
        let rows_height = footer.height - f64::from(spacing.s6);
        let mut y = card.y1 - pad - rows_height;
        for index in leading.iter().chain(&trailing) {
            actions[*index] = Rect::new(card.x0 + pad, y, card.x1 - pad, y + button_h);
            y += button_h + gap;
        }
        return actions;
    }
    let mut bx = card.x1 - pad;
    for &index in trailing.iter().rev() {
        let bw = footer.widths[index];
        if let Some(slot) = actions.get_mut(index) {
            *slot = Rect::new(bx - bw, by, bx, by + button_h);
        }
        bx -= bw + gap;
    }
    let mut lx = card.x0 + pad;
    for &index in &leading {
        let bw = footer.widths[index];
        if let Some(slot) = actions.get_mut(index) {
            *slot = Rect::new(lx, by, lx + bw, by + button_h);
        }
        lx += bw + gap;
    }
    actions
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
    if layout.card == layout.scrim {
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
        crate::overlay::scrim::behind_panel(
            scene,
            transform,
            layout.scrim,
            RoundedRect::from_rect(layout.card, floating_surface::radius(theme)),
            theme,
        );
        floating_surface::paint(scene, transform, layout.card, theme);
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

/// The immediate-mode sheet for a painted host that keeps its own state (a
/// question with three answers, a confirmation over a canvas): it lays the
/// sheet out, paints it with hover, press and keyboard focus on its
/// actions, keeps the layout of its last paint for hit-testing, and turns
/// input into a [`SheetResponse`]. The host paints only its body.
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

    /// Paint the sheet for `spec` over `window`, calling `body` to draw the
    /// host's content inside the body rect (`spec.body_height` tall). The
    /// actions show this sheet's hover, press and focus.
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
        visual.sort_by(|&a, &b| {
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
        paint_backdrop(scene, transform, text_renderer, theme, &layout, &spec);
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

/// One footer action of a [`ModalDialog`]. The dialog runs it from its own
/// events, and the action's focus node runs it when it is operated through
/// the accessibility tree, so both hold the callback.
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
}

/// A footer action's place in the window's keyboard focus order and its
/// button in the accessibility tree. The dialog paints the action and runs
/// it; this node holds focus, so Tab moves from the content to the footer and
/// on around inside the trap, and a reader lands on a named button. An
/// activation it is sent lands where the action stands, so the dialog runs it
/// as it would a press.
struct FooterKey {
    slot: Slot,
    label: String,
    /// The action's callback, shared with the dialog.
    run: Option<Rc<RefCell<dyn FnMut()>>>,
    /// The semantic address a reader or agent operates the action by.
    action_target: String,
    rect: Rect,
    focused: bool,
    enabled: bool,
    /// The address a harness or `tilo-control` operates the action by:
    /// `modal.primary`, or `<dialog test id>.primary`.
    test_id: String,
}

impl FooterKey {
    fn new(
        slot: Slot,
        action: Option<&DialogAction>,
        dialog: Option<&str>,
        target: String,
    ) -> Self {
        let name = match slot {
            Slot::Primary => "primary",
            Slot::Secondary => "secondary",
            Slot::Tertiary => "tertiary",
        };
        Self {
            slot,
            label: action.map(|a| a.label.clone()).unwrap_or_default(),
            run: action.map(|a| Rc::clone(&a.on_click)),
            action_target: target,
            rect: Rect::ZERO,
            focused: false,
            enabled: true,
            test_id: format!("{}.{name}", dialog.unwrap_or("modal")),
        }
    }
}

impl Widget for FooterKey {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Out of flow and empty: the dialog places the action itself.
        let style = Style {
            position: taffy::Position::Absolute,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn name(&self) -> &'static str {
        "ModalAction"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let mut state = AccessState::new().focusable(self.enabled);
        state.focused = self.focused;
        state.disabled = !self.enabled;
        Some(
            AccessNode::new(Role::Button)
                .with_name(crate::i18n!(&self.label))
                .with_semantic_key(&self.test_id)
                .with_state(state)
                .with_action_target(self.action_target.clone()),
        )
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _: &mut crate::widget::Edits<'_>,
        _: &ModelPath,
    ) -> bool {
        if !self.enabled || !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(run) = &self.run else { return false };
        (run.borrow_mut())();
        true
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        target == self.action_target && self.perform_action(action, state, path)
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.test_id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn needs_redraw(&self) -> bool {
        false
    }
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
pub struct ModalDialog<C: Widget> {
    action_identity: uuid::Uuid,
    test_id: Option<String>,
    title: String,
    content: C,
    primary_action: Option<DialogAction>,
    secondary_action: Option<DialogAction>,
    tertiary_action: Option<DialogAction>,
    tertiary_destructive: bool,
    primary_enabled: bool,
    primary_gate: Option<Box<dyn Fn(&C) -> bool>>,
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
    /// The content's measured size.
    content_size: (f64, f64),
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
            tertiary_action: None,
            tertiary_destructive: false,
            primary_enabled: true,
            primary_gate: None,
            on_dismiss: None,
            open: false,
            destructive: false,
            max_width: MODAL_MAX_WIDTH,
            window: Rect::ZERO,
            presented: false,
            layout: None,
            laid_out_for: None,
            slots: Vec::new(),
            keys: Vec::new(),
            last_cursor: Point::ZERO,
            pointer_inside: false,
            detached: None,
            host_content: None,
            content_size: (0.0, 0.0),
            content_origin: Point::ZERO,
            scroll: 0.0,
            trim: 0.0,
            stale: false,
            remeasure: false,
            structure_dirty: false,
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
    /// [traps focus](Widget::traps_focus). Opening scrolls the body back
    /// to its top and lets the host focus the first control in it.
    pub fn set_open(&mut self, open: bool) {
        if open && !self.open {
            self.scroll = 0.0;
            self.stale = true;
            self.remeasure = true;
        }
        self.open = open;
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
        self.primary_action = Some(DialogAction::new(label, on_click));
        self.footer_changed();
        self
    }

    /// The other action, usually Cancel. Escape fires it when the dialog
    /// has no [`Self::on_dismiss`].
    #[must_use]
    pub fn secondary_action(
        mut self,
        label: impl Into<String>,
        on_click: impl FnMut() + 'static,
    ) -> Self {
        self.secondary_action = Some(DialogAction::new(label, on_click));
        self.footer_changed();
        self
    }

    /// A third answer for a three-way question: Save / Don't save / Cancel
    /// is `primary_action("Save", ..)`, `tertiary_action("Don't save", ..)`
    /// and `secondary_action("Cancel", ..)`. It stands apart from the other
    /// two, and Enter never fires it.
    #[must_use]
    pub fn tertiary_action(
        mut self,
        label: impl Into<String>,
        on_click: impl FnMut() + 'static,
    ) -> Self {
        self.tertiary_action = Some(DialogAction::new(label, on_click));
        self.footer_changed();
        self
    }

    /// The tertiary action discards something ("Don't save"): danger ink on
    /// a neutral control.
    #[must_use]
    pub fn with_destructive_tertiary(mut self, on: bool) -> Self {
        self.tertiary_destructive = on;
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
    /// button. As on macOS, Enter is then the safe answer (the secondary
    /// action, Cancel) and the sheet opens with the keyboard on it; the
    /// destructive action takes a click, or Tab and Space.
    #[must_use]
    pub fn with_destructive(mut self, on: bool) -> Self {
        self.destructive = on;
        self
    }

    /// Whether the primary action can be taken. A disabled primary action
    /// is painted dimmed, is announced as disabled, leaves the focus order,
    /// and Enter does not fire it.
    pub fn set_primary_enabled(&mut self, enabled: bool) {
        self.primary_enabled = enabled;
    }

    /// [`Self::set_primary_enabled`] on construction.
    #[must_use]
    pub fn with_primary_enabled(mut self, enabled: bool) -> Self {
        self.primary_enabled = enabled;
        self
    }

    /// Enable the primary action only while `gate` holds, read every time
    /// the sheet is painted, announced or fired: a confirmation that waits
    /// for a typed phrase, a form that waits for a valid value. The gate gets
    /// the content, and may as well read the state the content's fields
    /// write elsewhere (an app's shared core it captures), so a form whose
    /// values live outside its widgets is gated the same way.
    #[must_use]
    pub fn primary_enabled_if(mut self, gate: impl Fn(&C) -> bool + 'static) -> Self {
        self.primary_gate = Some(Box::new(gate));
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

    /// Measure and place the content again: call after changing the
    /// content in a way that changes its size without changing its
    /// structure. A structure change the content reports
    /// ([`Widget::take_structure_dirty`]) does this by itself.
    pub fn invalidate_layout(&mut self) {
        self.laid_out_for = None;
        self.stale = true;
        self.remeasure = true;
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
    /// How far the body is scrolled.
    pub fn scroll_offset(&self) -> f64 {
        self.scroll
    }
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether the primary action can be taken now.
    pub fn primary_enabled(&self) -> bool {
        self.primary_enabled && self.primary_gate.as_ref().is_none_or(|g| g(&self.content))
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

    fn closable(&self) -> bool {
        self.on_dismiss.is_some() && self.secondary_action.is_none()
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
            Slot::Primary => self.primary_enabled(),
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
                .enabled(self.primary_enabled()),
            );
            slots.push(Slot::Primary);
        }
        (actions, slots)
    }

    /// The footer's slots in the order Tab visits them: the order they
    /// stand, leading to trailing, except that the alternate answer ("Don't
    /// save") comes last, and a destructive confirmation starts on Cancel
    /// wherever the platform stands it. A dialog with nothing to type in
    /// opens with focus on its first footer action, and that is never the
    /// answer that destroys or discards anything.
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
        }
    }

    /// Resolve the sheet's geometry in `self.window` around the content's
    /// measured height, keeping the body's trim and scroll inside range.
    fn resolve(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let (actions, slots) = self.actions();
        let spec = ModalSpec::new(&self.title)
            .actions(&actions)
            .platform_conventions(self.platform)
            .body_height(self.content_size.1)
            .max_width(self.max_width)
            .closable(self.closable());
        let mut layout = layout(self.window, &spec, text_renderer, theme);
        drop(actions);
        if layout.body_overflows && self.trim > 0.0 {
            shorten(&mut layout, self.trim);
        }
        let max_scroll = (self.content_size.1 - layout.body.height()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        self.slots = slots;
        let primary_enabled = self.primary_enabled();
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

    /// After the content is placed at the top of an overflowing body, end
    /// the body on the last row that shows whole, so the sheet never cuts
    /// a row through the middle.
    fn trim_to_content_boundary(&mut self) {
        let Some(layout) = self.layout.as_mut() else {
            return;
        };
        if !layout.body_overflows || self.scroll > 0.0 {
            return;
        }
        // The rows of the content: the first level with more than one child.
        let mut level: &dyn Widget = &self.content;
        let mut rows = level.children();
        while rows.len() == 1 {
            let Some(&only) = rows.first() else { break };
            level = only;
            rows = level.children();
        }
        let body = layout.body;
        let boundary = rows
            .iter()
            .map(|row| row.get_rect().y1)
            .filter(|&y| y > body.y0 + 0.5 && y <= body.y1 + 0.5)
            .fold(None, |best: Option<f64>, y| {
                Some(best.map_or(y, |b| b.max(y)))
            });
        let Some(boundary) = boundary else { return };
        let trim = body.y1 - boundary;
        // A cut that would take more than a third of the body away is a
        // single tall row: let it scroll instead.
        if trim > 0.5 && trim < body.height() / 3.0 {
            shorten(layout, trim);
            self.trim += trim;
            let max_scroll = (self.content_size.1 - layout.body.height()).max(0.0);
            self.scroll = self.scroll.clamp(0.0, max_scroll);
            for key in &mut self.keys {
                key.rect = key.rect - Vec2::new(0.0, trim);
            }
        }
    }

    /// Lay the content out in its own tree (a painted host that keeps the
    /// dialog outside its tree): measure it at the body width, size the
    /// sheet to it, and place it in the body.
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
    }

    /// Forward `event` to the content. Returns whether the content consumed
    /// it. A hosted dialog's content is addressed by its node in the host's
    /// tree, so a focus request lands on the host's node for the field.
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
        if let Some(cb) = self.on_dismiss.as_mut() {
            cb();
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

/// Shorten an overflowing sheet's body by `by` from the bottom, moving the
/// footer up with it.
fn shorten(layout: &mut ModalLayout, by: f64) {
    let by = by.min(layout.body.height()).max(0.0);
    layout.card.y1 -= by;
    layout.body.y1 -= by;
    for action in &mut layout.actions {
        if action.area() > 0.0 {
            *action = *action - Vec2::new(0.0, by);
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
        state: &mut crate::widget::Edits<'_>,
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

impl<C: Widget + 'static> Widget for ModalDialog<C> {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        let reorders = self.platform.dialog_action_order() != conventions.dialog_action_order();
        self.platform = conventions;
        self.content.set_platform_conventions(conventions);
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
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &children,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
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
        let window = tree_window(taffy, node).unwrap_or_else(|| {
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
        if !self.presented {
            self.window = tree_window(taffy, node)
                .unwrap_or_else(|| crate::widget::layout_rect(taffy, node, origin));
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
        let theme = self.layout_theme.clone();
        crate::text_render::with_layout_renderer(theme.text_scale, |tr| self.resolve(&theme, tr));
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
        if self.scroll <= 0.0 && self.trim == 0.0 {
            self.trim_to_content_boundary();
        }
        self.stale = false;
        self.remeasure = false;
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
        if self.host_content.is_some() {
            if self.laid_out_for != Some(self.key(theme)) {
                self.resolve(theme, text_renderer);
                // The content follows on the host's next layout pass; until
                // then it is drawn moved to where it belongs.
                self.stale = true;
            }
            let body_w = self.body_rect().width();
            if (self.content_size.0 - body_w).abs() > 1.0 {
                self.remeasure = true;
                self.stale = true;
            }
        } else if self.laid_out_for != Some(self.key(theme)) || self.detached.is_none() {
            self.relayout_detached(theme, text_renderer);
        }
        let Some(layout) = self.layout.clone() else {
            return;
        };
        // A gate can change with any keystroke: the footer's buttons are
        // announced with the state the sheet is painted with.
        let primary_enabled = self.primary_enabled();
        for key in &mut self.keys {
            key.enabled = key.slot != Slot::Primary || primary_enabled;
        }
        let focused = self.focused_slot();
        let (mut actions, slots) = self.actions();
        for ((action, rect), slot) in actions.iter_mut().zip(&layout.actions).zip(&slots) {
            *action = action
                .hovered(self.pointer_inside && rect.contains(self.last_cursor))
                .focused(focused == Some(*slot));
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
        // The body stands on the sheet: its controls lift off the panel.
        let on_panel = on_sheet(theme);
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
                self.forward_to_content(event, ctx);
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer_inside = false;
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
                    // A control that reads the key itself keeps it (a
                    // multiline field, an open list). Space then operates
                    // the focused control (a switch, a check box); Enter
                    // operates only a focused button, and is otherwise the
                    // sheet's default answer, as Return is on macOS.
                    let enter = *key == NamedKey::Enter;
                    let content_handled = self.forward_to_content(event, ctx)
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
                self.forward_to_content(event, ctx);
                *ctx.handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let over_body = self.body_rect().contains(self.last_cursor);
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
                // An overlay the content opened claims the press before the
                // sheet does: a pick in a dropdown's list that hangs below
                // the sheet, or the press that closes it.
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
                        if self.body_rect().contains(p) {
                            self.forward_to_content(event, ctx);
                        }
                    }
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput { .. } => {
                // In-sheet presses still reach the content, so a right-click
                // affordance in it can react, and a drag ends where it began.
                if self.dialog_rect().contains(self.last_cursor) || self.content_overlay_open() {
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
        self.open.then(|| self.dialog_rect())
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if !self.open {
            return Vec::new();
        }
        let mut children: Vec<&dyn Widget> = vec![&self.content];
        children.extend(self.keys.iter().map(|k| k as &dyn Widget));
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if self.open {
            visitor(&self.content);
            for key in &self.keys {
                visitor(key);
            }
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if !self.open {
            return Vec::new();
        }
        let mut children: Vec<&mut (dyn Widget + '_)> = vec![&mut self.content];
        children.extend(self.keys.iter_mut().map(|k| k as &mut (dyn Widget + '_)));
        children
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let content = self.open && self.content.take_structure_dirty();
        if content {
            self.invalidate_layout();
        }
        if self.host_content.is_some() {
            own || content
        } else {
            // Nobody else lays a painted host's dialog out: it re-measures
            // on its next paint.
            false
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.stale || self.remeasure || (self.open && self.content.is_layout_dirty())
    }

    fn layout_needs_measure(&self) -> bool {
        self.remeasure || (self.open && self.content.layout_needs_measure())
    }

    fn needs_redraw(&self) -> bool {
        // Opening, closing and every move are marked stale; an open sheet
        // otherwise changes only through its content or an event.
        self.stale || (self.open && self.content.needs_redraw())
    }

    fn needs_animation_frame(&self) -> bool {
        // A move the top layer found (a new window, a scroll) lands on the
        // host's next layout pass.
        self.open && (self.stale || self.content.needs_animation_frame())
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
        // Footer actions a host laid out are buttons of their own focus
        // nodes; a dialog no host has built names them itself.
        if self.open && self.keys.is_empty() {
            for (key, index, action, bounds, enabled) in [
                (
                    "primary",
                    1,
                    &self.primary_action,
                    self.primary_rect(),
                    self.primary_enabled(),
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
        if self.open && self.closable() {
            node.children.push(
                AccessNode::new(Role::Button)
                    .with_name(crate::i18n!("Close"))
                    .with_key(3)
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(self.close_rect().unwrap_or(Rect::ZERO))
                    .with_action_target(format!("dialog:{}:close", self.action_identity)),
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
        if !self.open || !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let id = self.action_identity;
        let is = |name: &str| {
            target == format!("modal.{name}") || target == format!("dialog:{id}:{name}")
        };
        if is("primary") && self.primary_action.is_some() && self.primary_enabled() {
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

/// Offer the focused control under `widget` the activation the window gives a
/// focused control on Space, or on Enter (`buttons_only`) only when it is a
/// button. Returns whether it took it.
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
            && widget.perform_action(&crate::a11y::Action::Activate, state, path);
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
#[cfg(test)]
#[path = "modal_dialog_tests.rs"]
mod tests;

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

use crate::i18n::widget_text;
use crate::widget::DrawChild;
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
use crate::interaction::InteractionEnvironment;
use crate::platform::{DialogActionOrder, PlatformConventions};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::Widget;
use crate::widget::button::{Button, ButtonFace, ButtonRole};
use crate::widget::control_size::ControlSize;
use crate::widget::{TaffyTreeExt, floating_surface};

pub const MODAL_MAX_WIDTH: f64 = 520.0;
/// Narrowest a modal sheet is drawn, in design pixels, before it elides.
pub const MODAL_MIN_WIDTH: f64 = 320.0;
/// Contrast a modal's own ink must clear against the sheet before it is
/// used as-is. WCAG 2.1 AA for body text.
const MODAL_MIN_INK_CONTRAST: f64 = 4.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalEmphasis {
    Primary,
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent interaction-state flags, not related toggles"
)]
#[derive(Clone, Copy, Debug)]
pub struct ModalAction<'a> {
    pub label: &'a str,
    pub emphasis: ModalEmphasis,
    pub enabled: bool,
    pub hovered: bool,
    pub pressed: bool,
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
    pub fn alternate(label: &'a str) -> Self {
        Self::of(label, ModalEmphasis::Alternate)
    }
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent spec flags set by unrelated callers, not related toggles"
)]
#[derive(Clone, Debug)]
pub struct ModalSpec<'a> {
    pub title: &'a str,
    pub icon: Option<Icon>,
    pub closable: bool,
    pub actions: &'a [ModalAction<'a>],
    pub platform: PlatformConventions,
    pub body_height: f64,
    pub header_height: f64,
    pub max_width: f64,
    pub close_hovered: bool,
    pub close_focused: bool,
    pub dismiss_on_scrim: bool,
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

#[derive(Clone, Debug, PartialEq)]
pub struct ModalLayout {
    pub scrim: Rect,
    pub card: Rect,
    pub title: Rect,
    pub close: Option<Rect>,
    pub header: Option<Rect>,
    pub body: Rect,
    pub actions: Vec<Rect>,
    pub body_overflows: bool,
    pub primary: Option<usize>,
    pub dismiss_on_scrim: bool,
    pub anchored: bool,
    pub foot: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalHit {
    Close,
    Action(usize),
    Card,
    /// The scrim. Dismiss when [`ModalLayout::dismiss_on_scrim`], otherwise
    /// swallow.
    Scrim,
}

pub fn ink(theme: &Theme) -> (Color, Color) {
    let ground = crate::chrome::panel_ground(theme);
    if crate::a11y::contrast_ratio(theme.text_primary, ground) >= MODAL_MIN_INK_CONTRAST {
        (theme.text_primary, theme.text_dim)
    } else {
        let on = theme.on_content();
        (on.ink, on.ink_dim)
    }
}

fn pad(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s6)
}

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

pub fn action_height(theme: &Theme) -> f64 {
    ControlSize::Regular.height(theme)
}

fn title_row_height(theme: &Theme) -> f64 {
    let line = f64::from(theme.text(TextRole::Title).line_height_px());
    line.max(f64::from(theme.tokens.metrics.min_pointer_target))
}

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

pub fn paint_scrim(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    layout: &ModalLayout,
    drawn_card: Rect,
) {
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

pub fn paint_sheet(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    layout: &ModalLayout,
    spec: &ModalSpec<'_>,
) {
    if layout.foot {
    } else if layout.card == layout.scrim {
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
        let name = crate::i18n::widget_format("widget-dialog-close", &[]);
        let face = ButtonFace::icon_only(c, Icon::X, &name)
            .hovered(spec.close_hovered)
            .focused(spec.close_focused);
        Button::paint(scene, transform, text_renderer, &on_sheet(theme), &face);
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetResponse {
    Action(usize),
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
    visual: Vec<usize>,
    appear: crate::motion::Appearing,
}

impl ModalSheet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    #[must_use]
    pub fn layout(&self) -> Option<&ModalLayout> {
        self.layout.as_ref()
    }

    #[must_use]
    pub fn hit(&self, point: Point) -> Option<ModalHit> {
        self.layout.as_ref()?.hit(point)
    }

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

    #[must_use]
    pub fn focused_action(&self) -> Option<usize> {
        self.focus
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Secondary,
    Tertiary,
    Primary,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LayoutKey {
    window: Rect,
    text_scale: f32,
    ui_scale: f32,
    platform: PlatformConventions,
    content_h: f64,
    header_h: f64,
    min_target: f64,
}

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
    icon: Option<Icon>,
    description: Option<String>,
    primary_action: Option<DialogAction>,
    secondary_action: Option<DialogAction>,
    tertiary_action: Option<DialogAction>,
    tertiary_destructive: bool,
    primary_enabled: bool,
    primary_gate: Option<Box<dyn Fn(&dyn Widget) -> bool>>,
    dismiss: Option<super::dismissal::Dismissal>,
    destructive: bool,
    max_width: f64,
    window: Rect,
    presented: bool,
    window_node: Option<NodeId>,
    layout: Option<ModalLayout>,
    laid_out_for: Option<LayoutKey>,
    slots: Vec<Slot>,
    keys: Vec<FooterKey>,
    last_cursor: Point,
    /// Whether `last_cursor` is current: cleared by `CursorLeft` and on open,
    /// so a hover face never outlives the pointer that caused it.
    pointer_inside: bool,
    dismiss_on_scrim: bool,
    detached: Option<floating_surface::DetachedContent>,
    host_content: Option<NodeId>,
    header: Option<Box<dyn Widget>>,
    header_node: Option<NodeId>,
    header_size: (f64, f64),
    header_origin: Point,
    header_detached: Option<floating_surface::DetachedContent>,
    content_size: (f64, f64),
    content_laid_out_in: f64,
    content_origin: Point,
    scroll: f64,
    trim: f64,
    /// The host must place the content again (scroll, window, trim).
    stale: bool,
    /// The host must measure the content again (width, content change).
    remeasure: bool,
    structure_dirty: bool,
    platform: PlatformConventions,
    layout_theme: Theme,
    appear: crate::motion::Appearance,
    anchor: Option<Rect>,
    foot: Option<super::BottomSheet>,
    environment: InteractionEnvironment,
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
            environment: InteractionEnvironment::default(),
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
            dismiss_on_scrim: true,
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

    pub fn set_anchor(&mut self, anchor: Option<Rect>) {
        if self.anchor != anchor {
            self.anchor = anchor;
            self.invalidate_layout();
        }
    }

    #[must_use]
    pub fn with_foot_sheet(mut self) -> Self {
        let mut sheet = super::BottomSheet::new();
        sheet.snap_to(super::SheetDetent::Full);
        self.foot = Some(sheet);
        self
    }

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

    pub fn set_title(&mut self, title: impl Into<String>) {
        let title = title.into();
        if self.title != title {
            self.title = title;
            self.invalidate_layout();
        }
    }

    #[must_use]
    pub fn with_test_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        if let Some(dismiss) = self.dismiss.take() {
            self.dismiss = Some(dismiss.renamed(self.surface_id()));
        }
        self
    }

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

    #[must_use]
    pub fn dismiss_on_scrim(mut self, dismisses: bool) -> Self {
        self.dismiss_on_scrim = dismisses;
        self
    }

    #[must_use]
    pub fn with_destructive_tertiary(mut self, on: bool) -> Self {
        self.tertiary_destructive = on;
        self
    }

    #[must_use]
    pub fn with_destructive(mut self, on: bool) -> Self {
        self.set_destructive(on);
        self
    }

    /// [`Self::with_destructive`] after construction.
    pub fn set_destructive(&mut self, on: bool) {
        self.destructive = on;
    }

    pub fn set_primary_enabled(&mut self, enabled: bool) {
        self.primary_enabled = enabled;
    }

    #[must_use]
    pub fn with_primary_enabled(mut self, enabled: bool) -> Self {
        self.primary_enabled = enabled;
        self
    }

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

    pub fn primary_enabled(&self) -> bool {
        self.as_dyn().primary_enabled_now()
    }

    fn as_dyn(&self) -> &ModalDialog<dyn Widget> {
        self
    }

    fn as_dyn_mut(&mut self) -> &mut ModalDialog<dyn Widget> {
        self
    }
}

impl<C: Widget + ?Sized> ModalDialog<C> {
    fn shut(&mut self, source: crate::tokens::MotionSource) -> bool {
        if !self.up() {
            return false;
        }
        self.appear.show_from(false, source);
        self.pointer_inside = false;
        true
    }

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
    pub fn close_rect(&self) -> Option<Rect> {
        self.layout.as_ref().and_then(|l| l.close)
    }
    pub fn body_rect(&self) -> Rect {
        self.layout.as_ref().map_or(Rect::ZERO, |l| l.body)
    }
    pub fn header_rect(&self) -> Option<Rect> {
        self.layout.as_ref().and_then(|l| l.header)
    }
    pub fn scroll_offset(&self) -> f64 {
        self.scroll
    }
    pub fn is_open(&self) -> bool {
        self.up()
    }

    #[must_use]
    pub fn is_drawn(&self) -> bool {
        self.appear.is_drawn()
    }

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
    fn primary_enabled_now(&self) -> bool {
        self.primary_enabled && self.primary_gate.as_ref().is_none_or(|g| g(&self.content))
    }

    fn closable(&self) -> bool {
        self.dismiss.is_some() && self.secondary_action.is_none()
    }

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
            min_target: target(theme, self.environment),
        }
    }

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
            .dismiss_on_scrim(self.dismiss_on_scrim)
            .anchor(self.anchor);
        let mut layout = layout_for(self.window, &spec, text_renderer, theme, self.environment);
        drop(actions);
        if self.foot.is_some() {
            rise_from_foot(
                &mut layout,
                self.content_size.1,
                target(theme, self.environment),
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

    fn place_detached(&mut self) {
        let target = self.content_target();
        if let Some(content) = &self.detached {
            self.content
                .apply_layout(&content.taffy, content.node, target);
            self.content_origin = target;
        }
        self.place_header_detached();
    }

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

    fn over_header(&self, p: Point) -> bool {
        self.header_rect().is_some_and(|header| header.contains(p))
    }

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

    fn default_slot(&self) -> Option<Slot> {
        if self.destructive {
            self.secondary_action.is_some().then_some(Slot::Secondary)
        } else {
            Some(Slot::Primary)
        }
    }

    fn focused_slot(&self) -> Option<Slot> {
        self.keys.iter().find(|k| k.focused).map(|k| k.slot)
    }

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

    fn content_overlay_open(&self) -> bool {
        self.content.wants_pointer_capture() || self.content.modal_capture().is_some()
    }
}

impl<C: Widget + 'static> ModalDialog<C> {
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

impl Widget for ModalDialog<dyn Widget> {
    fn html(&self, cx: &mut crate::composition::HtmlCx<'_>) {
        cx.element("section");
        cx.children();
    }

    fn set_interaction_environment(&mut self, environment: InteractionEnvironment) {
        if self.environment != environment {
            self.environment = environment;
            self.laid_out_for = None;
        }
        for child in self.children_mut() {
            child.set_interaction_environment(environment);
        }
    }

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
        crate::text_render::with_layout_renderer(&theme, |tr| {
            if self.host_content.is_some() {
                self.resolve(&theme, tr);
            } else {
                self.relayout_detached(&theme, tr);
            }
        });
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.content.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
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
        let wanted = taffy::LengthPercentage::length(pad.floor());
        if style.padding.right == wanted {
            return false;
        }
        style.padding.right = wanted;
        taffy.set_style(node, style).is_ok()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
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
        crate::text_render::with_layout_renderer(&theme, |tr| self.resolve(&theme, tr));
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
        if !self.appear.is_drawn() {
            return;
        }
        // Each change starts on the motion the person allows, which only the theme says.
        self.appear.follow(self.up(), theme.motion());
        if let Some(window) = presence.viewport.get() {
            self.window = window;
            self.presented = true;
        }
        if self.up() && self.host_content.is_some() {
            if self.laid_out_for != Some(self.key(theme)) {
                self.resolve(theme, text_renderer);
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
        let primary_enabled = self.primary_enabled_now();
        for key in &mut self.keys {
            key.enabled = key.slot != Slot::Primary || primary_enabled;
        }
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
            header.draw_child(
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
        let shift = self.content_target() - self.content_origin;
        let content_transform = transform * Affine::translate(shift);
        self.content
            .draw_child(scene, content_transform, text_renderer, &on_panel, presence);
        if layout.body_overflows {
            scene.pop_layer();
        }
        self.paint_scroll_edges(scene, transform, theme);
        self.content
            .draw_overlay(scene, content_transform, text_renderer, &on_panel, presence);
        pose.end(scene);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.up() {
            return;
        }
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
                    self.activate(slot);
                } else {
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
                if self.content_overlay_open() && self.forward_to_content(event, ctx) {
                    *ctx.handled = true;
                    return;
                }
                let p = self.last_cursor;
                let hit = self
                    .pointer_inside
                    .then(|| self.layout.as_ref().and_then(|l| l.hit(p)))
                    .flatten();
                match hit {
                    Some(ModalHit::Close) => self.dismiss(),
                    Some(ModalHit::Scrim) if self.dismiss_on_scrim => self.dismiss(),
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
                    Some(ModalHit::Scrim) | None => {}
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput { .. } => {
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

    fn dismiss_presented_modal(&mut self) -> bool {
        self.shut(crate::tokens::MotionSource::Pointer)
    }

    fn presents_modal(&self) -> bool {
        self.is_drawn()
    }

    fn children(&self) -> Vec<&dyn Widget> {
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

    fn accessibility_hidden(&self) -> bool {
        !self.is_drawn()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let title = widget_text(&self.title);
        let name = (!title.is_empty()).then_some(title);
        let mut node = AccessNode::new(Role::Dialog)
            .with_optional_name(name)
            .with_state(AccessState {
                modal: true,
                hidden: self.accessibility_hidden(),
                ..Default::default()
            });
        node.description.clone_from(&self.description);
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
                            .with_name(widget_text(&action.label))
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
                    .with_name(crate::i18n::widget_format("widget-dialog-close", &[]))
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

    fn set_interaction_environment(&mut self, environment: InteractionEnvironment) {
        self.as_dyn_mut().set_interaction_environment(environment);
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
    static LAYOUT_WINDOW: std::cell::Cell<Option<(NodeId, Rect)>> =
        const { std::cell::Cell::new(None) };
}

/// Place a window's content (`place` runs its `apply_layout`) knowing which window it is: a host
/// laying several windows out in one tree (the desktop's strip) calls this so a dialog centres on
/// its own window rather than the shared tree's root.
pub fn within_window<R>(window: NodeId, rect: Rect, place: impl FnOnce() -> R) -> R {
    struct Restore(Option<(NodeId, Rect)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LAYOUT_WINDOW.with(|cell| cell.set(self.0));
        }
    }
    let _restore = Restore(LAYOUT_WINDOW.with(|cell| cell.replace(Some((window, rect)))));
    place()
}

fn current_window() -> Option<(NodeId, Rect)> {
    LAYOUT_WINDOW.with(std::cell::Cell::get)
}

fn window_node_rect(taffy: &TaffyTree<()>, node: NodeId, window: NodeId) -> Option<Rect> {
    let mut at = node;
    while at != window {
        at = taffy.parent(at)?;
    }
    let l = taffy.layout(window).ok()?;
    let rect = Rect::new(0.0, 0.0, f64::from(l.size.width), f64::from(l.size.height));
    (rect.width() > 0.0 && rect.height() > 0.0).then_some(rect)
}

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

fn theme_line(theme: &Theme) -> f32 {
    theme.text(TextRole::Body).line_height_px() * 3.0
}
mod body;
mod close_control;
mod footer_key;
mod geometry;
use body::{shorten, whole_row_top};
use footer_key::FooterKey;
pub use geometry::{layout, layout_for, rise_from_foot};
use geometry::{onto_device_pixels, target};

#[cfg(test)]
#[path = "modal_dialog_tests.rs"]
mod tests;

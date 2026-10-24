//! A notice's own strip under the bar's row, where the window is compact:
//! the notice's whole text, on up to two lines, and its action as a button
//! that names itself, on the bar's own surface. The row stands as it does
//! without a notice; the bar grows by the strip, so the window's content
//! moves down once as the notice comes and back once it has gone, while
//! the strip itself comes and goes on the standard motion.

use super::paint::{labelled_face, layered};
use super::{Affine, CONTROL_PX, LOCATION_ICON_PX, Point, Rect, Scene, TextRenderer, Theme};
use crate::motion::Appearing;
use crate::tokens::{EffectiveMotion, TextRole};
use crate::window_toolbar::{ToolbarAction, ToolbarNotice};

/// The most lines of a notice's text the strip holds; past them it is cut
/// short.
const MAX_LINES: usize = 2;

/// The strip, and the notice it says.
#[derive(Clone, Debug, Default)]
pub(super) struct NoticeStrip {
    /// The notice it says, kept while it leaves.
    notice: Option<ToolbarNotice>,
    /// A notice is up for it: what input acts on at once.
    open: bool,
    appearing: Appearing,
    motion: EffectiveMotion,
    measured: Option<Measured>,
    /// Where it stands (`Rect::ZERO`: not placed).
    rect: Rect,
}

/// The strip as the theme draws its notice, `width` wide.
#[derive(Clone, Debug, PartialEq)]
struct Measured {
    text: String,
    action: String,
    width: f64,
    text_scale: u32,
    lines: Vec<String>,
    line_height: f64,
    /// The action's button width (`0.0`: none).
    button: f64,
    /// Between the icon and the text, and the text and the button.
    gap: f64,
    height: f64,
}

impl NoticeStrip {
    /// Say `notice` (none: take the strip down, on the motion).
    pub(super) fn offer(&mut self, notice: Option<&ToolbarNotice>, motion: EffectiveMotion) {
        self.motion = motion;
        match notice {
            Some(notice) => {
                if self.notice.as_ref() != Some(notice) {
                    self.notice = Some(notice.clone());
                }
                self.open = true;
                if self.rect.area() > 0.0 {
                    self.appearing.follow(true, motion);
                }
            }
            None if self.open => {
                self.open = false;
                self.appearing.follow(false, motion);
            }
            None => {}
        }
    }

    /// Measure the notice as `theme` draws it across `width`. Returns
    /// whether the strip's height changed. A strip that has gone lets its
    /// notice go here.
    pub(super) fn measure(&mut self, theme: &Theme, width: f64) -> bool {
        let before = self.height();
        if !self.open && !self.appearing.is_drawn() {
            self.notice = None;
            self.measured = None;
            self.rect = Rect::ZERO;
            return before > 0.0;
        }
        let Some(notice) = self.notice.as_ref() else {
            return false;
        };
        let action = notice
            .action
            .as_ref()
            .map(|action| action.name.clone())
            .unwrap_or_default();
        let text_scale = theme.text_scale.to_bits();
        if width <= 0.0
            || self.measured.as_ref().is_some_and(|m| {
                m.text == notice.text
                    && m.action == action
                    && (m.width - width).abs() < 0.5
                    && m.text_scale == text_scale
            })
        {
            return false;
        }
        let gap = f64::from(theme.tokens.spacing.s2);
        let pad = f64::from(theme.tokens.spacing.s2);
        let measured = crate::text_render::with_layout_renderer(theme.text_scale, |text| {
            let button = if action.is_empty() {
                0.0
            } else {
                labelled_face(Rect::ZERO, &action).intrinsic_width(text, theme)
            };
            let style = theme.text(TextRole::Body);
            let beside = if button > 0.0 { gap + button } else { 0.0 };
            let room = (width - LOCATION_ICON_PX - gap - beside).max(0.0);
            let lines = text.clamp_to_lines(
                &notice.text,
                &style,
                crate::num::f32_of_f64(room),
                MAX_LINES,
            );
            let line_height = f64::from(style.line_height_px());
            let rows = f64::from(u32::try_from(lines.len()).unwrap_or(1).max(1));
            Measured {
                text: notice.text.clone(),
                action: action.clone(),
                width,
                text_scale,
                lines,
                line_height,
                button,
                gap,
                height: (rows * line_height).max(CONTROL_PX) + 2.0 * pad,
            }
        });
        self.measured = Some(measured);
        (self.height() - before).abs() > 0.5
    }

    /// How tall the strip stands in the bar: its measured height while a
    /// notice is up or still leaving, else none.
    pub(super) fn height(&self) -> f64 {
        match (&self.notice, &self.measured) {
            (Some(_), Some(measured)) if self.open || self.appearing.is_drawn() => measured.height,
            _ => 0.0,
        }
    }

    /// Stand at `rect`, the bar's room for it; a notice up comes in once it
    /// has that room.
    pub(super) fn place(&mut self, rect: Rect) {
        self.rect = rect;
        if self.open && rect.area() > 0.0 {
            self.appearing.follow(true, self.motion);
        }
    }

    pub(super) fn rect(&self) -> Rect {
        self.rect
    }

    /// Whether it still moves (its owner asks for frames), or waits for the
    /// bar to make room for it.
    pub(super) fn needs_frame(&self) -> bool {
        self.appearing.is_moving()
            || (self.open && self.measured.is_some() && self.rect.area() <= 0.0)
            || (!self.open && self.notice.is_some() && !self.appearing.is_drawn())
    }

    /// The notice up in it, while it takes input.
    pub(super) fn notice(&self) -> Option<&ToolbarNotice> {
        self.notice
            .as_ref()
            .filter(|_| self.open && self.rect.area() > 0.0)
    }

    /// The action of the notice up in it.
    pub(super) fn action(&self) -> Option<&ToolbarAction> {
        self.notice()?.action.as_ref()
    }

    /// Where its action's button stands (`Rect::ZERO`: none).
    pub(super) fn button(&self) -> Rect {
        match (&self.measured, self.action()) {
            (Some(measured), Some(_)) if measured.button > 0.0 => {
                let y = self.rect.center().y;
                Rect::new(
                    self.rect.x1 - measured.button,
                    y - CONTROL_PX / 2.0,
                    self.rect.x1,
                    y + CONTROL_PX / 2.0,
                )
            }
            _ => Rect::ZERO,
        }
    }

    /// Where its text stands, its kind's icon first.
    pub(super) fn text_rect(&self) -> Rect {
        let Some(measured) = self.measured.as_ref() else {
            return Rect::ZERO;
        };
        let beside = if measured.button > 0.0 {
            measured.gap + measured.button
        } else {
            0.0
        };
        Rect::new(
            self.rect.x0,
            self.rect.y0,
            (self.rect.x1 - beside).max(self.rect.x0),
            self.rect.y1,
        )
    }

    /// Whether its text stands whole, not cut short.
    pub(super) fn whole(&self) -> bool {
        let letters =
            |text: &str| -> String { text.chars().filter(|c| !c.is_whitespace()).collect() };
        self.measured
            .as_ref()
            .is_some_and(|measured| letters(&measured.lines.concat()) == letters(&measured.text))
    }

    /// Paint it, its button lifted as `(hovered, pressed)`.
    pub(super) fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        (tr, theme): (&mut TextRenderer, &Theme),
        (hovered, pressed): (bool, bool),
    ) {
        let (Some(notice), Some(measured)) = (self.notice.as_ref(), self.measured.as_ref()) else {
            return;
        };
        if self.rect.area() <= 0.0 || !self.appearing.is_drawn() {
            return;
        }
        let opacity = crate::num::f32_of_f64(self.appearing.progress().clamp(0.0, 1.0));
        let text_rect = self.text_rect();
        let button = self.button();
        layered(scene, transform, self.rect, opacity, |scene| {
            let rows = f64::from(u32::try_from(measured.lines.len()).unwrap_or(1).max(1));
            let y0 = self.rect.center().y - rows * measured.line_height / 2.0;
            let icon = Point::new(
                text_rect.x0 + LOCATION_ICON_PX / 2.0,
                y0 + measured.line_height / 2.0,
            );
            crate::icons::draw_icon(
                scene,
                transform,
                Rect::from_center_size(icon, (LOCATION_ICON_PX, LOCATION_ICON_PX)),
                crate::widget::toast::kind_icon(notice.kind),
                crate::widget::toast::kind_color(notice.kind, theme),
                crate::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            let style = theme.text(TextRole::Body);
            let x = text_rect.x0 + LOCATION_ICON_PX + measured.gap;
            let mut y = y0;
            for line in &measured.lines {
                let origin = transform * Affine::translate((x, y));
                tr.draw_style(
                    scene,
                    line,
                    theme.tokens.colors.text_primary,
                    origin,
                    None,
                    &style,
                );
                y += measured.line_height;
            }
            if let Some(action) = notice.action.as_ref()
                && button.area() > 0.0
            {
                let face = labelled_face(button, &action.name)
                    .hovered(hovered && action.enabled)
                    .pressed(pressed && action.enabled)
                    .disabled(!action.enabled);
                crate::widget::Button::paint(scene, transform, tr, theme, &face);
            }
        });
    }
}

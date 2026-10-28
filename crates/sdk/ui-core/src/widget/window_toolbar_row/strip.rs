//! A notice's own strip under the bar's row, where the window is compact:
//! the notice's whole text, on up to two lines, and its actions as buttons
//! that name themselves, on the bar's own surface. The row stands as it does
//! without a notice; the bar grows by the strip, so the window's content
//! moves down once as the notice comes and back once it has gone, while
//! the strip itself comes and goes on the standard motion.

use super::paint::{labelled_face, layered};
use super::{Affine, CONTROL_PX, LOCATION_ICON_PX, Point, Rect, Scene, TextRenderer, Theme};
use crate::motion::Appearing;
use crate::tokens::{EffectiveMotion, TextRole};
use crate::window_toolbar::ToolbarNotice;

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
    actions: Vec<String>,
    width: f64,
    text_scale: u32,
    lines: Vec<String>,
    line_height: f64,
    /// Between the icon and the text, the text and the buttons, and the
    /// buttons; and around them all.
    gap: f64,
    /// Where the text block stands, from the strip's origin: its icon,
    /// then its lines.
    text_rect: Rect,
    /// Where each button stands, from the strip's origin.
    buttons: Vec<Rect>,
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
    ///
    /// The buttons stand after the text where the whole text stands beside
    /// them; where it would be cut short there they go on rows of their own
    /// under it, as many to a row as fit, so the text keeps the whole
    /// width. Every button names itself.
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
        let actions: Vec<String> = notice
            .actions
            .iter()
            .map(|action| action.name.clone())
            .collect();
        let text_scale = theme.text_scale.to_bits();
        if width <= 0.0
            || self.measured.as_ref().is_some_and(|m| {
                m.text == notice.text
                    && m.actions == actions
                    && (m.width - width).abs() < 0.5
                    && m.text_scale == text_scale
            })
        {
            return false;
        }
        let gap = f64::from(theme.tokens.spacing.s2);
        let measured = crate::text_render::with_layout_renderer(theme.text_scale, |text| {
            let widths: Vec<f64> = actions
                .iter()
                .map(|name| labelled_face(Rect::ZERO, name).intrinsic_width(text, theme))
                .collect();
            let style = theme.text(TextRole::Body);
            let line_height = f64::from(style.line_height_px());
            let mut clamp = |room: f64| {
                text.clamp_to_lines(
                    &notice.text,
                    &style,
                    crate::num::f32_of_f64(room.max(0.0)),
                    MAX_LINES,
                )
            };
            let lead = LOCATION_ICON_PX + gap;
            let span = widths.iter().sum::<f64>()
                + gap * f64::from(u32::try_from(widths.len().saturating_sub(1)).unwrap_or(0));
            let beside_room = width - lead - if widths.is_empty() { 0.0 } else { gap + span };
            let beside = clamp(beside_room);
            let rows_of = |lines: &[String]| {
                f64::from(u32::try_from(lines.len()).unwrap_or(1).max(1)) * line_height
            };
            if widths.is_empty() || whole_text(&beside, &notice.text) {
                let block = rows_of(&beside).max(CONTROL_PX);
                let height = block + 2.0 * gap;
                let y = height / 2.0;
                let mut x = width - span;
                let buttons = widths
                    .iter()
                    .map(|w| {
                        let rect = Rect::new(x, y - CONTROL_PX / 2.0, x + w, y + CONTROL_PX / 2.0);
                        x = rect.x1 + gap;
                        rect
                    })
                    .collect();
                let text_h = rows_of(&beside);
                return (
                    beside,
                    Rect::new(0.0, y - text_h / 2.0, beside_room + lead, y + text_h / 2.0),
                    buttons,
                    height,
                    line_height,
                );
            }
            // The buttons on rows of their own under the text, right
            // aligned, as many to a row as fit.
            let lines = clamp(width - lead);
            let text_h = rows_of(&lines);
            let mut rows: Vec<Vec<f64>> = Vec::new();
            for w in &widths {
                match rows.last_mut() {
                    Some(row)
                        if row.iter().sum::<f64>()
                            + gap * f64::from(u32::try_from(row.len()).unwrap_or(0))
                            + w
                            <= width =>
                    {
                        row.push(*w);
                    }
                    _ => rows.push(vec![*w]),
                }
            }
            let mut buttons = Vec::with_capacity(widths.len());
            let mut y = gap + text_h + gap;
            for row in &rows {
                let row_span = row.iter().sum::<f64>()
                    + gap * f64::from(u32::try_from(row.len().saturating_sub(1)).unwrap_or(0));
                let mut x = width - row_span;
                for w in row {
                    buttons.push(Rect::new(x, y, x + w, y + CONTROL_PX));
                    x += w + gap;
                }
                y += CONTROL_PX + gap;
            }
            (
                lines,
                Rect::new(0.0, gap, width, gap + text_h),
                buttons,
                y,
                line_height,
            )
        });
        let (lines, text_rect, buttons, height, line_height) = measured;
        self.measured = Some(Measured {
            text: notice.text.clone(),
            actions,
            width,
            text_scale,
            lines,
            line_height,
            gap,
            text_rect,
            buttons,
            height,
        });
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

    /// Where its buttons stand, in order (none: no notice up, or it has
    /// none).
    pub(super) fn buttons(&self) -> Vec<Rect> {
        match (&self.measured, self.notice()) {
            (Some(measured), Some(_)) => measured
                .buttons
                .iter()
                .map(|rect| *rect + self.rect.origin().to_vec2())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Where its text stands, its kind's icon first.
    pub(super) fn text_rect(&self) -> Rect {
        self.measured.as_ref().map_or(Rect::ZERO, |measured| {
            measured.text_rect + self.rect.origin().to_vec2()
        })
    }

    /// Whether its text stands whole, not cut short.
    pub(super) fn whole(&self) -> bool {
        self.measured
            .as_ref()
            .is_some_and(|measured| whole_text(&measured.lines, &measured.text))
    }

    /// Paint it, button `hovered` and button `pressed` lifted.
    pub(super) fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        (tr, theme): (&mut TextRenderer, &Theme),
        (hovered, pressed): (Option<usize>, Option<usize>),
    ) {
        let (Some(notice), Some(measured)) = (self.notice.as_ref(), self.measured.as_ref()) else {
            return;
        };
        if self.rect.area() <= 0.0 || !self.appearing.is_drawn() {
            return;
        }
        let opacity = crate::num::f32_of_f64(self.appearing.progress().clamp(0.0, 1.0));
        let text_rect = self.text_rect();
        let origin = self.rect.origin().to_vec2();
        layered(scene, transform, self.rect, opacity, |scene| {
            let icon = Point::new(
                text_rect.x0 + LOCATION_ICON_PX / 2.0,
                text_rect.y0 + measured.line_height / 2.0,
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
            let mut y = text_rect.y0;
            for line in &measured.lines {
                let at = transform * Affine::translate((x, y));
                tr.draw_style(
                    scene,
                    line,
                    theme.tokens.colors.text_primary,
                    at,
                    None,
                    &style,
                );
                y += measured.line_height;
            }
            for (i, (action, rect)) in notice.actions.iter().zip(&measured.buttons).enumerate() {
                let face = labelled_face(*rect + origin, &action.name)
                    .hovered(hovered == Some(i) && action.enabled)
                    .pressed(pressed == Some(i) && action.enabled)
                    .disabled(!action.enabled);
                crate::widget::Button::paint(scene, transform, tr, theme, &face);
            }
        });
    }
}

/// Whether `lines` hold all of `text`, not cut short.
fn whole_text(lines: &[String], text: &str) -> bool {
    let letters = |text: &str| -> String { text.chars().filter(|c| !c.is_whitespace()).collect() };
    letters(&lines.concat()) == letters(text)
}

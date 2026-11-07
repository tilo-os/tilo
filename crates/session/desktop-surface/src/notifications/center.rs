//! The notification centre: retained notifications grouped by verified app,
//! one list that scrolls, a keyboard selection, per-row controls, Clear all
//! with Undo. Paint, pointer hit-testing, keyboard and accessibility all read
//! the rows and the one [`CenterLayout`] defined here.
//!
//! The centre holds only viewer-local state (selection, folded groups, the
//! scroll position). The host places it in a panel and owns everything it
//! shows through [`NotificationHost`].

use std::collections::BTreeSet;
use ui::num::{f32_of_f64, f64_of_usize};

use ui::a11y::{AccessNode, AccessState, Role};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::time::Instant;
use ui::widget::{Button, ButtonFace, ButtonRole, ControlSize};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, NamedKey};

use super::NotificationHost;
use super::history::{
    HistoryActivation, HistoryRow, RowIdentity, accessible_description, relative_time,
};
use super::model::Notification;
use super::reader::NotificationReader;
use crate::drill_in::{self as qs, DrillInList};
use crate::i18n::{t, t_args};
use ui::TextRole;

/// Rows the panel asks room for; the rest scroll rather than page.
const PREFERRED_ROWS: usize = 7;
/// Lines of body a row shows before the rest is left to the reader: enough
/// for a message or a failure's reason, short enough that a list scans.
const BODY_LINES: usize = 2;

/// `text` wrapped at `width` into at most `lines` lines, the last elided
/// when the text runs on. Whitespace is normalised as in [`preview_line`].
fn preview_lines(
    renderer: &mut TextRenderer,
    text: &str,
    width: f32,
    style: &ui::text_render::TextStyle,
    lines: usize,
) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let one = renderer.measure_style("Ag", None, style).1;
    let limit = one * (f32::from(u8::try_from(lines).unwrap_or(u8::MAX)) + 0.5);
    let fits = |renderer: &mut TextRenderer, candidate: &str| {
        renderer.measure_style(candidate, Some(width), style).1 <= limit
    };
    if lines <= 1 || fits(renderer, &line) {
        return if lines <= 1 {
            renderer.elide_to_width_style(&line, width, style)
        } else {
            line
        };
    }
    // The longest prefix, in whole characters, that still fits with its
    // ellipsis.
    let chars: Vec<char> = line.chars().collect();
    let with_ellipsis = |count: usize| {
        let prefix: String = chars.iter().take(count).collect();
        format!("{}…", prefix.trim_end())
    };
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = lo.saturating_add(hi.saturating_sub(lo).div_ceil(2));
        if fits(renderer, &with_ellipsis(mid)) {
            lo = mid;
        } else {
            hi = mid.saturating_sub(1);
        }
    }
    with_ellipsis(lo)
}

/// Rows promise one line per field; the complete text remains in their
/// accessible description. Use toolkit shaping/elision, including graphemes.
fn preview_line(
    renderer: &mut TextRenderer,
    text: &str,
    width: f32,
    style: &ui::text_render::TextStyle,
) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    renderer.elide_to_width_style(&line, width, style)
}

/// Measured shared action faces and typography, cached by the host for
/// drawing, pointer hit-testing and accessibility between frames.
#[derive(Clone, Debug)]
pub struct CenterMetrics {
    lines: (f64, f64),
    title_width: f64,
    cleared_width: f64,
    back_width: f64,
    /// The reader's way back, which names the list it returns to.
    reader_back_width: f64,
    clear_width: f64,
    undo_width: f64,
    height: f64,
    /// A notification's own choices: compact, as on its banner, a small
    /// step under its text.
    choice_height: f64,
    choice_gap: f64,
    choice_lead: f64,
    choice_foot: f64,
    /// The theme rows are measured in, so a row is as tall as its body
    /// wraps to and its choices as wide as their labels. Without one (bare
    /// line boxes) every row keeps room for the full preview.
    theme: Option<std::sync::Arc<Theme>>,
}

impl From<(f64, f64)> for CenterMetrics {
    fn from(lines: (f64, f64)) -> Self {
        Self {
            lines,
            title_width: 0.0,
            cleared_width: 0.0,
            back_width: 2.0 * qs::TARGET,
            reader_back_width: 3.0 * qs::TARGET,
            clear_width: 2.0 * qs::TARGET,
            undo_width: 2.0 * qs::TARGET,
            height: qs::TARGET,
            choice_height: qs::TARGET,
            choice_gap: qs::RELATED_GAP,
            choice_lead: qs::RELATED_GAP,
            choice_foot: qs::CONTROL_PAD,
            theme: None,
        }
    }
}

impl From<&CenterMetrics> for CenterMetrics {
    fn from(metrics: &CenterMetrics) -> Self {
        metrics.clone()
    }
}

/// What sizes one row: the lines its body wraps to and the widths its own
/// choices ask for, secondary then primary. A row is as tall as what it
/// holds, so a one-line message is not followed by an empty line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowShape {
    body_lines: usize,
    choices: usize,
    widths: (f64, f64),
    /// Whether the row leads with its source line. Rows inside a group do
    /// not: their header already names the app.
    source: bool,
    /// Whether the row is a member under a group header, which sits one
    /// mark column in from the header's own mark.
    grouped: bool,
}

impl Default for RowShape {
    /// A row with the full preview and no choices.
    fn default() -> Self {
        Self {
            body_lines: BODY_LINES,
            choices: 0,
            widths: (f64::INFINITY, f64::INFINITY),
            source: true,
            grouped: false,
        }
    }
}

impl RowShape {
    /// A row whose body wraps to `body_lines` (at most `BODY_LINES`).
    #[must_use]
    pub fn new(body_lines: usize) -> Self {
        Self {
            body_lines: body_lines.min(BODY_LINES),
            ..Self::default()
        }
    }
    /// The same row without the source line above its title.
    #[must_use]
    pub fn without_source(mut self) -> Self {
        self.source = false;
        self
    }
    /// Whether the row shows its source line.
    pub fn has_source(&self) -> bool {
        self.source
    }
    /// The same row as a member under a group header: no source line (the
    /// header names the app) and one mark column further in.
    #[must_use]
    pub fn grouped(mut self) -> Self {
        self.source = false;
        self.grouped = true;
        self
    }
    /// Whether the row sits under a group header.
    pub fn is_grouped(&self) -> bool {
        self.grouped
    }
    /// The same row carrying choices of these widths, secondary first.
    #[must_use]
    pub fn with_choices(mut self, widths: &[f64]) -> Self {
        self.choices = widths.len().min(2);
        self.widths = match widths {
            [] => (f64::INFINITY, f64::INFINITY),
            [primary] => (0.0, *primary),
            [secondary, primary, ..] => (*secondary, *primary),
        };
        self
    }
    pub fn body_lines(&self) -> usize {
        self.body_lines
    }
}

/// The centre's geometry for one panel rect and type scale, so paint,
/// hit-testing, the visible-row count and the preferred height read one
/// oracle (HIG §7b: one rhythm). Every step is the drill-in panel's shared
/// rhythm ([`crate::drill_in`]).
#[derive(Clone, Debug)]
pub struct CenterLayout {
    panel: Rect,
    /// Line boxes of a caption and a label at the text scale.
    caption_lh: f64,
    label_lh: f64,
    actions: CenterMetrics,
    /// The centre is a level of a drill-in panel (the native quick
    /// settings): its leading control goes back there. Otherwise the centre
    /// stands alone and the control closes it.
    drill_in: bool,
    /// Fresh banners are being held (Do Not Disturb, presenting, a focus
    /// schedule): the header carries a status line saying so, where the
    /// person looks for what they missed, instead of a card over their
    /// work.
    quiet: bool,
}

impl CenterLayout {
    /// Space the cleared band claims under the last row: a related step, the
    /// band, the panel padding.
    fn cleared_reserve(&self) -> f64 {
        qs::RELATED_GAP + self.actions.height + qs::PANEL_PAD
    }

    /// The header's controls and title, without the quiet status line.
    fn controls_height(&self) -> f64 {
        self.actions.height
            + 2.0 * qs::RELATED_GAP
            + if self.wrapped_header() {
                self.label_lh + qs::RELATED_GAP
            } else {
                0.0
            }
    }

    fn header_height(&self) -> f64 {
        self.controls_height()
            + if self.quiet {
                self.caption_lh + qs::RELATED_GAP
            } else {
                0.0
            }
    }

    /// The quiet status line under the header's controls; zero height when
    /// banners show normally.
    pub fn quiet_rect(&self) -> Rect {
        let y0 = self.panel.y0 + self.controls_height();
        let height = if self.quiet { self.caption_lh } else { 0.0 };
        Rect::new(
            self.panel.x0 + qs::PANEL_PAD,
            y0,
            self.panel.x1 - qs::PANEL_PAD,
            y0 + height,
        )
    }

    fn wrapped_header(&self) -> bool {
        self.panel.width() > 0.0
            && self.panel.width() - 2.0 * qs::PANEL_PAD
                < self.leading_width()
                    + self.actions.clear_width
                    + self.actions.title_width
                    + 2.0 * qs::RELATED_GAP
    }

    fn title_rect(&self) -> Rect {
        if self.wrapped_header() {
            let y0 = self.back().y1 + qs::RELATED_GAP;
            Rect::new(
                self.panel.x0 + qs::PANEL_PAD,
                y0,
                self.panel.x1 - qs::PANEL_PAD,
                y0 + self.label_lh,
            )
        } else {
            let x0 = if self.drill_in {
                self.back().x1 + qs::RELATED_GAP
            } else {
                self.panel.x0 + qs::PANEL_PAD
            };
            Rect::new(
                x0,
                self.panel.y0,
                self.clear().x0 - qs::RELATED_GAP,
                self.panel.y0 + self.controls_height(),
            )
        }
    }

    /// Whether the title starts at its band's leading edge: a standalone
    /// centre reads "Notifications" then its controls, as every panel with
    /// a trailing close does; a drill-in level centres it between Back and
    /// Clear all.
    fn title_leads(&self) -> bool {
        !self.drill_in
    }

    /// Resolve the same widths and height the shared button painter needs.
    pub fn metrics(theme: &Theme, text: &mut TextRenderer) -> CenterMetrics {
        let width = |key: &str, role, text: &mut TextRenderer| {
            ButtonFace::new(Rect::ZERO, &t(key))
                .role(role)
                .intrinsic_width(text, theme)
                .max(2.0 * qs::TARGET)
        };
        let back = |key: &str, text: &mut TextRenderer| {
            ButtonFace::new(Rect::ZERO, &t(key))
                .role(ButtonRole::Plain)
                .icon(ui::icons::Icon::ChevronLeft)
                .intrinsic_width(text, theme)
                .max(2.0 * qs::TARGET)
        };
        CenterMetrics {
            lines: Self::type_lines(theme),
            title_width: f64::from(
                text.measure_style(
                    &t("notification-center-title"),
                    None,
                    &theme
                        .text(TextRole::Label)
                        .with_weight(TextWeight::Semibold),
                )
                .0,
            ),
            cleared_width: f64::from(
                text.measure_style(
                    &t("notification-center-cleared"),
                    None,
                    &theme.text(TextRole::Caption),
                )
                .0,
            ),
            back_width: back("drill-in-back", text),
            reader_back_width: back("notification-center-title", text),
            clear_width: width("notification-center-clear-all", ButtonRole::Plain, text),
            undo_width: width("notification-center-undo", ButtonRole::Secondary, text),
            height: ButtonFace::height(ControlSize::Regular, theme),
            choice_height: ui::widget::toast::choice_height(theme),
            choice_gap: ui::widget::toast::choice_gap(theme),
            choice_lead: ui::widget::toast::choice_lead(theme),
            choice_foot: ui::widget::toast::choice_foot(theme),
            theme: Some(std::sync::Arc::new(theme.clone())),
        }
    }

    /// The line boxes of the caption and label roles the centre paints
    /// with, in `theme`: what [`Self::new`] lays rows out from.
    pub fn type_lines(theme: &ui::Theme) -> (f64, f64) {
        let line = |role| f64::from(theme.text(role).line_height_px());
        (line(TextRole::Caption), line(TextRole::Label))
    }

    /// Use [`Self::metrics`] for measured shared actions. Legacy line-box tuples
    /// from [`Self::type_lines`] remain accepted.
    pub fn new(panel: Rect, metrics: impl Into<CenterMetrics>) -> Self {
        let actions = metrics.into();
        Self {
            panel,
            caption_lh: actions.lines.0,
            label_lh: actions.lines.1,
            actions,
            drill_in: false,
            quiet: false,
        }
    }
    /// Fresh banners are being held: the header says so on a status line
    /// ([`NotificationHost::quiet_status`]).
    #[must_use]
    pub fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }
    /// This centre is a level of a drill-in panel: its leading control
    /// reads "Back" and returns there, instead of closing the centre.
    #[must_use]
    pub fn drill_in(mut self) -> Self {
        self.drill_in = true;
        self
    }
    /// Whether the leading control goes back to a drill-in panel.
    pub fn is_drill_in(&self) -> bool {
        self.drill_in
    }
    fn leading_width(&self) -> f64 {
        if self.drill_in {
            self.actions.back_width
        } else {
            self.actions.height
        }
    }
    pub fn panel(&self) -> Rect {
        self.panel
    }
    /// The header's way out, centred in the title band: "‹ Back" on the
    /// leading padding in a drill-in panel (a textual Back survives
    /// enlarged text where a small icon would not), and a square close
    /// button on the trailing padding where the centre stands alone, where
    /// every other panel's close is.
    pub fn back(&self) -> Rect {
        let p = self.panel;
        let x0 = if self.drill_in {
            p.x0 + qs::PANEL_PAD
        } else {
            p.x1 - qs::PANEL_PAD - self.leading_width()
        };
        Rect::new(
            x0,
            p.y0 + qs::RELATED_GAP,
            x0 + self.leading_width(),
            p.y0 + qs::RELATED_GAP + self.actions.height,
        )
    }
    /// The reader's way back to the list: "‹ Notifications", naming where
    /// it goes.
    pub fn reader_back(&self) -> Rect {
        let p = self.panel;
        let width = self
            .actions
            .reader_back_width
            .min(p.width() - 2.0 * qs::PANEL_PAD)
            .max(0.0);
        Rect::new(
            p.x0 + qs::PANEL_PAD,
            p.y0 + qs::RELATED_GAP,
            p.x0 + qs::PANEL_PAD + width,
            p.y0 + qs::RELATED_GAP + self.actions.height,
        )
    }
    /// The "Clear all" chip: on the trailing padding, mirroring Back in a
    /// drill-in panel, and just before the close button where the centre
    /// stands alone.
    pub fn clear(&self) -> Rect {
        let p = self.panel;
        let x1 = if self.drill_in {
            p.x1 - qs::PANEL_PAD
        } else {
            self.back().x0 - qs::RELATED_GAP
        };
        Rect::new(
            x1 - self.actions.clear_width,
            p.y0 + qs::RELATED_GAP,
            x1,
            p.y0 + qs::RELATED_GAP + self.actions.height,
        )
    }
    /// The Undo button on the "cleared" status band.
    pub fn undo(&self) -> Rect {
        let r = self.footer();
        Rect::new(r.x1 - self.actions.undo_width, r.y0, r.x1, r.y1)
    }
    /// The "cleared" status band at the panel's foot.
    fn cleared_band(&self) -> Rect {
        let mut band = self.footer();
        band.y0 -= self.cleared_extra_height();
        band.inflate(qs::CONTROL_PAD, qs::CONTROL_PAD)
    }

    fn cleared_extra_height(&self) -> f64 {
        if self.panel.width() > 0.0 && self.actions.cleared_width > self.cleared_inline_width() {
            self.caption_lh + qs::RELATED_GAP
        } else {
            0.0
        }
    }

    fn cleared_inline_width(&self) -> f64 {
        self.footer().width() - self.actions.undo_width - 2.0 * qs::RELATED_GAP
    }

    fn cleared_label_rect(&self) -> Rect {
        let band = self.cleared_band();
        let x0 = band.x0 + qs::CONTROL_INSET;
        let y0 = band.y0
            + if self.cleared_extra_height() > 0.0 {
                qs::CONTROL_PAD
            } else {
                qs::CONTROL_INSET
            };
        let x1 = if self.cleared_extra_height() > 0.0 {
            band.x1 - qs::CONTROL_INSET
        } else {
            self.undo().x0 - qs::CONTROL_INSET
        };
        Rect::new(x0, y0, x1, y0 + self.caption_lh)
    }
    /// Top of the first row: one related step under the header. The list
    /// carries no "2 notifications" line: what fits is plain to see, and
    /// the rest scrolls.
    fn rows_top(&self) -> f64 {
        self.panel.y0 + self.header_height() + qs::RELATED_GAP
    }
    /// One row with the full preview: source/time, title and
    /// `BODY_LINES` lines of body on the control padding.
    pub fn row_h(&self) -> f64 {
        self.text_extent(&RowShape::default())
    }
    /// A row's text on the control padding: source/time when it has a
    /// source line, title and the lines its body wraps to.
    fn text_extent(&self, shape: &RowShape) -> f64 {
        2.0 * qs::CONTROL_PAD
            + if shape.has_source() {
                self.caption_lh
            } else {
                0.0
            }
            + self.label_lh
            + f64_of_usize(shape.body_lines) * self.caption_lh
    }
    /// A row's height: its text, then its own choices padded as on its
    /// banner, a related step under the text and the card's padding under
    /// them.
    fn row_extent(&self, shape: &RowShape) -> f64 {
        self.text_extent(shape)
            + if shape.choices > 0 {
                self.actions.choice_lead + self.actions.choice_height + self.actions.choice_foot
                    - qs::CONTROL_PAD
            } else {
                0.0
            }
    }
    /// The width a row's text wraps to: the panel less the padding, the
    /// sender's mark before it (one column further in for a group's member
    /// rows), and the controls it keeps clear of.
    fn text_width(&self, grouped: bool) -> f64 {
        self.panel.width()
            - 2.0 * (qs::PANEL_PAD + qs::CONTROL_INSET)
            - Self::mark_column()
            - if grouped { Self::mark_column() } else { 0.0 }
    }
    /// The width a sender's mark takes before a row's text: one glyph and
    /// the related gap after it.
    fn mark_column() -> f64 {
        qs::GLYPH + qs::RELATED_GAP
    }
    /// The box a row's sender mark fills: a glyph in from the row's leading
    /// edge on the title's line, a group's member rows one mark column
    /// further in so they sit under their header's mark. Every row has one,
    /// including one from no app (the system's bell).
    pub fn row_mark(&self, row: Rect, shape: &RowShape) -> Rect {
        let first_y = row.y0 + qs::CONTROL_PAD;
        let title_y = if shape.has_source() {
            first_y + self.caption_lh
        } else {
            first_y
        };
        let x0 = row.x0
            + qs::CONTROL_INSET
            + if shape.is_grouped() {
                Self::mark_column()
            } else {
                0.0
            };
        Rect::from_center_size(
            Point::new(x0 + qs::GLYPH / 2.0, title_y + self.label_lh / 2.0),
            (qs::GLYPH, qs::GLYPH),
        )
    }
    /// The box a group header's sender mark fills: after the fold chevron
    /// that opens the header.
    pub fn group_mark(&self, row: Rect, shape: &RowShape) -> Rect {
        let mark = self.row_mark(row, shape);
        Rect::new(
            mark.x0 + Self::mark_column(),
            mark.y0,
            mark.x1 + Self::mark_column(),
            mark.y1,
        )
    }
    /// Measure with the theme the rows are laid out in, when there is one.
    fn measure<R>(&self, f: impl FnOnce(&Theme, &mut TextRenderer) -> R) -> Option<R> {
        let theme = self.actions.theme.as_deref()?;
        Some(ui::text_render::with_layout_renderer(
            theme.text_scale,
            |text| f(theme, text),
        ))
    }
    /// The lines `body` wraps to in a row, at most `BODY_LINES`. Before
    /// the panel has a width, or without a theme, the full preview.
    pub fn body_lines(&self, body: &str, grouped: bool) -> usize {
        let line = body.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            return 0;
        }
        let width = self.text_width(grouped);
        if width <= 0.0 {
            return BODY_LINES;
        }
        self.measure(|theme, text| {
            let style = theme.text(TextRole::Caption);
            let one = text.measure_style("Ag", None, &style).1;
            let height = text.measure_style(&line, Some(f32_of_f64(width)), &style).1;
            if one <= 0.0 {
                return BODY_LINES;
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a positive count of wrapped lines, clamped to the preview"
            )]
            let lines = (height / one).round().max(1.0) as usize;
            lines.min(BODY_LINES)
        })
        .unwrap_or(BODY_LINES)
    }
    /// The widths a row's choices ask for, as the banner sizes them.
    pub fn choice_widths(&self, labels: &[(String, String)]) -> Vec<f64> {
        labels
            .iter()
            .map(|(_, label)| {
                self.measure(|theme, text| {
                    ui::widget::toast::choice_width(text, theme, label, None)
                })
                .unwrap_or(f64::INFINITY)
            })
            .collect()
    }
    /// The `index`th visible row of a list of full-preview rows.
    pub fn row(&self, index: usize) -> Rect {
        self.row_in(index, &[])
    }
    /// The `index`th visible row, where `shapes` sizes the visible rows
    /// from the first; rows past its end have the full preview and no
    /// choices.
    pub fn row_in(&self, index: usize, shapes: &[RowShape]) -> Rect {
        let shape = |at: usize| shapes.get(at).copied().unwrap_or_default();
        let y0 = (0..index).fold(self.rows_top(), |y, at| {
            y + self.row_extent(&shape(at)) + qs::RELATED_GAP
        });
        Rect::new(
            self.panel.x0 + qs::PANEL_PAD,
            y0,
            self.panel.x1 - qs::PANEL_PAD,
            y0 + self.row_extent(&shape(index)),
        )
    }
    /// How many of `shapes`' rows fit, in order from the first row's
    /// place, above `bottom_reserve` at the panel's foot.
    fn fitting(&self, shapes: &[RowShape], bottom_reserve: f64) -> usize {
        let limit = self.panel.y1 - bottom_reserve + 1e-6;
        let mut y = self.rows_top();
        let mut count = 0usize;
        for shape in shapes {
            let bottom = y + self.row_extent(shape);
            if bottom > limit {
                break;
            }
            count = count.saturating_add(1);
            y = bottom + qs::RELATED_GAP;
        }
        count
    }
    /// The band at the panel's foot, on the panel padding: where the
    /// cleared status and its Undo stand.
    pub fn footer(&self) -> Rect {
        let p = self.panel;
        Rect::new(
            p.x0 + qs::PANEL_PAD,
            p.y1 - qs::PANEL_PAD - self.actions.height,
            p.x1 - qs::PANEL_PAD,
            p.y1 - qs::PANEL_PAD,
        )
    }
    /// Complete rows the panel shows of `total` full-preview rows.
    pub fn visible_rows(&self, total: usize) -> usize {
        self.visible_rows_in(&vec![RowShape::default(); total], 0)
    }
    /// Complete rows the panel shows from `start`, where `shapes` sizes
    /// every row. The rest scrolls: a row is never cut at the panel's foot.
    pub fn visible_rows_in(&self, shapes: &[RowShape], start: usize) -> usize {
        self.fitting(shapes.get(start..).unwrap_or_default(), qs::PANEL_PAD)
            .max(1)
    }
    /// The last first row worth scrolling to: from it, the rest of the list
    /// fills the panel.
    fn max_start(&self, shapes: &[RowShape]) -> usize {
        let total = shapes.len();
        if self.fitting(shapes, qs::PANEL_PAD) >= total {
            return 0;
        }
        let mut start = total.saturating_sub(1);
        while let Some(earlier) = start.checked_sub(1)
            && self.fitting(shapes.get(earlier..).unwrap_or_default(), qs::PANEL_PAD)
                >= total.saturating_sub(earlier)
        {
            start = earlier;
        }
        start
    }
    /// The first row from `start` on that keeps row `index` in view.
    fn start_showing(&self, shapes: &[RowShape], start: usize, index: usize) -> usize {
        if index < start {
            return index;
        }
        let mut first = start;
        while first < index && first.saturating_add(self.visible_rows_in(shapes, first)) <= index {
            first = first.saturating_add(1);
        }
        first
    }
    /// The height the centre wants to show `rows`. An empty centre keeps the
    /// drill-in's one-row band for its message.
    pub fn preferred_height(rows: usize, metrics: impl Into<CenterMetrics>) -> f64 {
        Self::new(Rect::ZERO, metrics).height_showing(&vec![RowShape::default(); rows])
    }
    /// The height that shows `shown` from the panel's top, ending on the
    /// panel padding under the last row.
    fn height_showing(&self, shown: &[RowShape]) -> f64 {
        let Some(last) = shown.len().checked_sub(1) else {
            return DrillInList::list_height(self.header_height(), 0)
                .max(self.rows_top() - self.panel.y0 + self.caption_lh + qs::PANEL_PAD);
        };
        self.row_in(last, shown).y1 - self.panel.y0 + qs::PANEL_PAD
    }
    /// Where the reader's page scrolls: under the header, on the padding.
    pub(super) fn reading_viewport(&self) -> Rect {
        Rect::new(
            self.panel.x0 + qs::PANEL_PAD,
            self.panel.y0 + self.header_height() + qs::RELATED_GAP,
            self.panel.x1 - qs::PANEL_PAD,
            self.panel.y1 - qs::PANEL_PAD,
        )
    }

    /// A row's dismiss button, in the trailing corner, centred on the first
    /// line it takes the place of (the source/time line, or the title of a
    /// row without one).
    pub fn row_controls(&self, row: Rect, shape: &RowShape) -> Rect {
        let size = self.actions.height.min(row.height());
        let line = if shape.has_source() {
            self.caption_lh
        } else {
            self.label_lh
        };
        let middle = row.y0 + qs::CONTROL_PAD + line / 2.0;
        let y0 = (middle - size / 2.0).max(row.y0);
        let x1 = row.x1 - qs::RELATED_GAP;
        Rect::new(x1 - size, y0, x1, y0 + size)
    }

    /// The notification's own choices under a row's text, laid out as on
    /// its banner: compact, at the trailing edge at their own width,
    /// secondary then primary, sharing the band in halves only when their
    /// labels do not fit.
    pub fn row_actions(&self, row: Rect, shape: &RowShape) -> Vec<Rect> {
        let y1 = row.y1 - self.actions.choice_foot;
        let band = Rect::new(
            row.x0 + qs::CONTROL_INSET,
            y1 - self.actions.choice_height,
            row.x1 - qs::CONTROL_INSET,
            y1,
        );
        let (secondary, primary) = ui::widget::toast::choice_rects(
            band,
            self.actions.choice_gap,
            shape.widths.0,
            shape.widths.1,
        );
        match shape.choices {
            0 => Vec::new(),
            1 => {
                let width = shape.widths.1.min(band.width());
                vec![Rect::new(band.x1 - width, band.y0, band.x1, band.y1)]
            }
            _ => vec![secondary, primary],
        }
    }
}

/// The leading header control's name: where it goes.
fn leading_label(layout: &CenterLayout) -> String {
    if layout.is_drill_in() {
        t("drill-in-back")
    } else {
        t("notification-center-close")
    }
}

/// The tone a notification's severity marks, for the rail a warning or a
/// failure keeps; plain information carries no rail.
fn severity_tone(severity: super::model::NotificationSeverity) -> Option<ui::widget::Tone> {
    match severity {
        super::model::NotificationSeverity::Info => None,
        super::model::NotificationSeverity::Warning => Some(ui::widget::Tone::Warning),
        super::model::NotificationSeverity::Error => Some(ui::widget::Tone::Danger),
    }
}

/// A control on a row: its icon buttons, or one of the notification's own
/// actions by position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowControl {
    Dismiss,
    Action(usize),
}

/// The host's choices for `notification`, at most two as on the banner.
fn row_actions_of(
    host: &dyn NotificationHost,
    notification: &Notification,
) -> Vec<(String, String)> {
    let mut actions = host.actions_for(notification);
    actions.truncate(2);
    actions
}

/// What an input asks of the host beyond what the centre did itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CenterOutcome {
    /// The input was not the centre's.
    Ignored,
    /// The centre acted on it.
    Handled,
    /// The person pressed Back.
    Back,
    /// A retained notification with no live sender asks to open its app.
    OpenApp { app_id: String, label: String },
}

/// Keyboard focus on one of the centre's own actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActionFocus {
    Clear,
    Undo,
}

/// Viewer-local state of one notification centre.
#[derive(Debug, Default)]
pub struct NotificationCenter {
    /// First visible row, newest first.
    start: usize,
    /// Touchpad movement not yet amounting to a whole row.
    wheel_remainder: f64,
    /// Keyboard focus on Clear all or Undo.
    action_focus: Option<ActionFocus>,
    back_focused: bool,
    /// The keyboard-selected row, by identity so it survives re-derivation.
    selected: Option<RowIdentity>,
    /// The selected row's focused trailing control.
    row_control: Option<RowControl>,
    /// App groups the viewer folded.
    collapsed: BTreeSet<String>,
    reader: Option<NotificationReader>,
}

impl NotificationCenter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return from reading without invoking or dismissing the notification.
    pub fn close_reader(&mut self) -> bool {
        self.reader.take().is_some()
    }

    /// Read retained notification `id` in full, as choosing it does when it
    /// opens nothing: a live banner the person asked to read lands here.
    pub fn open_reader(&mut self, id: u64) {
        self.selected = Some(RowIdentity::Item(id));
        self.row_control = None;
        self.back_focused = false;
        self.action_focus = None;
        self.reader = Some(NotificationReader::new(id, false));
    }

    pub fn is_reading(&self) -> bool {
        self.reader.is_some()
    }

    fn reading_notification(&self, host: &dyn NotificationHost) -> Option<Notification> {
        let id = self.reader.as_ref()?.id;
        host.log().history().iter().find(|n| n.id == id).cloned()
    }

    /// Shared scrollbar pointer, wheel and keyboard handling.
    pub fn reader_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        self.reader
            .as_mut()
            .is_some_and(|reader| reader.event(event, ctx))
    }

    /// Whether the reading page needs frames: its bar fading, a glide.
    pub fn reading_needs_frame(&self) -> bool {
        self.reader
            .as_ref()
            .is_some_and(NotificationReader::needs_animation_frame)
    }

    /// Step the reading page's glide; returns whether it moved.
    pub fn advance_reading(&mut self) -> bool {
        self.reader
            .as_mut()
            .is_some_and(NotificationReader::advance)
    }

    pub fn reading_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        self.reader.as_ref()?.scroll()
    }

    pub fn apply_reading_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        self.reader
            .as_mut()
            .map_or(ui::widget::PrimaryScrollResult::Unsupported, |reader| {
                reader.apply_scroll(action)
            })
    }

    /// The centre's rows, newest first, honouring the viewer's folded groups.
    pub fn rows(&self, host: &dyn NotificationHost) -> Vec<HistoryRow> {
        host.log().rows(&self.collapsed)
    }

    /// What sizes each of `rows` in `layout`: the lines its body wraps to
    /// and its own choices. A group header shows its newest title while
    /// folded and nothing under its label while open.
    fn shapes(
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        rows: &[HistoryRow],
    ) -> Vec<RowShape> {
        let log = host.log();
        let history = log.history();
        rows.iter()
            .map(|row| match row {
                HistoryRow::Item { index, grouped } => {
                    history
                        .get(*index)
                        .map_or_else(RowShape::default, |notification| {
                            let body = if notification.show_preview {
                                notification.body.as_str()
                            } else {
                                ""
                            };
                            let shape = RowShape::new(layout.body_lines(body, *grouped))
                                .with_choices(
                                    &layout.choice_widths(&row_actions_of(host, notification)),
                                );
                            // Inside a group the header names the app; the
                            // rows beneath it are their own title and body,
                            // one mark column in from the header.
                            if *grouped { shape.grouped() } else { shape }
                        })
                }
                HistoryRow::Group { expanded, .. } => {
                    RowShape::new(usize::from(!*expanded)).without_source()
                }
            })
            .collect()
    }

    /// The panel height the centre asks for over `host`'s log in `layout`:
    /// up to [`PREFERRED_ROWS`] rows, each as tall as it holds.
    fn preferred_height_in(&self, host: &dyn NotificationHost, layout: &CenterLayout) -> f64 {
        let height = if self.is_reading() {
            layout.height_showing(&[RowShape::default(); PREFERRED_ROWS])
        } else {
            let shapes = Self::shapes(host, layout, &self.rows(host));
            layout.height_showing(shapes.get(..PREFERRED_ROWS).unwrap_or(&shapes))
        };
        if host.log().can_undo_clear() {
            height + layout.cleared_reserve()
        } else {
            height
        }
    }

    /// The panel height the centre asks for over `host`'s log at
    /// `metrics`, before the host knows its width: every row keeps room for
    /// the full preview.
    pub fn preferred_height(
        &self,
        host: &dyn NotificationHost,
        metrics: impl Into<CenterMetrics>,
    ) -> f64 {
        self.preferred_height_in(
            host,
            &CenterLayout::new(Rect::ZERO, metrics).quiet(host.quiet_status().is_some()),
        )
    }

    /// The panel height the centre asks for at the host's actual width,
    /// with rows sized to what they hold and room for Undo.
    pub fn preferred_height_for_width(
        &self,
        host: &dyn NotificationHost,
        metrics: impl Into<CenterMetrics>,
        width: f64,
    ) -> f64 {
        let layout = CenterLayout::new(Rect::new(0.0, 0.0, width, 0.0), metrics)
            .quiet(host.quiet_status().is_some());
        self.preferred_height_in(host, &layout)
            + if host.log().can_undo_clear() {
                layout.cleared_extra_height()
            } else {
                0.0
            }
    }

    /// [`Self::preferred_height_for_width`] within `max`. A panel too short
    /// for every row it asks for ends under the last whole row that fits,
    /// rather than leaving a blank band where a taller row did not fit.
    pub fn fitted_height(
        &self,
        host: &dyn NotificationHost,
        metrics: impl Into<CenterMetrics>,
        width: f64,
        max: f64,
    ) -> f64 {
        let metrics = metrics.into();
        let wanted = self.preferred_height_for_width(host, metrics.clone(), width);
        if wanted <= max || self.is_reading() || host.log().can_undo_clear() {
            return wanted.min(max);
        }
        let layout = CenterLayout::new(Rect::new(0.0, 0.0, width, max), metrics)
            .quiet(host.quiet_status().is_some());
        let (_, shapes, start, shown) = self.window(host, &layout);
        let Some(last) = shown.checked_sub(1) else {
            return max;
        };
        let bottom = layout
            .row_in(last, shapes.get(start..).unwrap_or_default())
            .y1;
        (bottom + qs::PANEL_PAD).min(max)
    }

    /// The first visible row, newest first.
    pub fn first_visible_row(&self) -> usize {
        self.start
    }

    /// Complete rows the panel shows now.
    pub fn visible_rows(&self, host: &dyn NotificationHost, layout: &CenterLayout) -> usize {
        let shapes = Self::shapes(host, layout, &self.rows(host));
        layout.visible_rows_in(&shapes, self.start.min(layout.max_start(&shapes)))
    }

    fn selected_row(&self, rows: &[HistoryRow], history: &[Notification]) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        rows.iter()
            .position(|row| row.identity(history).as_ref() == Some(selected))
    }

    /// Keep the window valid when records expire, are cleared or the panel
    /// shrinks. Returns whether the position moved.
    fn clamp_start(&mut self, layout: &CenterLayout, shapes: &[RowShape]) -> bool {
        let next = self.start.min(layout.max_start(shapes));
        let changed = next != self.start;
        self.start = next;
        changed
    }

    /// Move through the rows newest first. Positive movement reveals older
    /// rows. Row-based, so wheels and keys land identically.
    fn scroll(&mut self, host: &dyn NotificationHost, layout: &CenterLayout, movement: isize) {
        let max_start = layout.max_start(&Self::shapes(host, layout, &self.rows(host)));
        let current = self.start.min(max_start);
        let next = if movement.is_negative() {
            current.saturating_sub(movement.unsigned_abs())
        } else {
            current.saturating_add(movement.unsigned_abs())
        }
        .min(max_start);
        if next != self.start {
            self.start = next;
            host.changed();
        }
    }

    /// Scroll from a pointer wheel over the panel. Pixel deltas accumulate
    /// into 48px row steps; line deltas name rows and apply immediately. A
    /// small touchpad movement stays the centre's until it makes a whole
    /// row, rather than scrolling whatever is under the panel.
    pub fn wheel(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        delta: MouseScrollDelta,
        scale_factor: f64,
    ) {
        if self.is_reading() {
            return;
        }
        let rows = match delta {
            MouseScrollDelta::LineDelta(_, y) => -f64::from(y),
            MouseScrollDelta::PixelDelta(position) => -position.y / (48.0 * scale_factor.max(0.1)),
        };
        if rows.abs() <= f64::EPSILON {
            return;
        }
        self.wheel_remainder += rows;
        // `trunc` preserves direction.
        let whole = self.wheel_remainder.trunc();
        if whole.abs() >= 1.0 {
            self.wheel_remainder -= whole;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a wheel delta is bounded by input hardware; this only converts complete row movement"
            )]
            let movement = whole as isize;
            self.scroll(host, layout, movement);
        }
    }

    /// Select row `index` (clamped) and scroll it into view.
    fn select_row(&mut self, host: &dyn NotificationHost, layout: &CenterLayout, index: usize) {
        let rows = self.rows(host);
        let Some(last) = rows.len().checked_sub(1) else {
            self.selected = None;
            return;
        };
        let index = index.min(last);
        let identity = rows
            .get(index)
            .and_then(|row| row.identity(host.log().history()));
        if identity != self.selected {
            self.row_control = None;
        }
        self.selected = identity;
        self.back_focused = false;
        self.action_focus = None;
        let shapes = Self::shapes(host, layout, &rows);
        self.start = layout
            .start_showing(&shapes, self.start.min(layout.max_start(&shapes)), index)
            .min(layout.max_start(&shapes));
        host.changed();
    }

    /// Move the selection by `delta` rows. With nothing selected the first
    /// press selects the first visible row.
    fn move_selection(&mut self, host: &dyn NotificationHost, layout: &CenterLayout, delta: isize) {
        let rows = self.rows(host);
        let current = self.selected_row(&rows, host.log().history());
        let next = match current {
            None => self.start,
            Some(index) if delta.is_negative() => index.saturating_sub(delta.unsigned_abs()),
            Some(index) => index.saturating_add(delta.unsigned_abs()),
        };
        self.select_row(host, layout, next);
    }

    fn row_actions(host: &dyn NotificationHost, row: &HistoryRow) -> Vec<(String, String)> {
        match row {
            HistoryRow::Item { index, .. } => {
                let notification = host.log().history().get(*index).cloned();
                notification
                    .map_or_else(Vec::new, |notification| row_actions_of(host, &notification))
            }
            HistoryRow::Group { .. } => Vec::new(),
        }
    }

    /// The rows from the first visible one, the bands they carry, and how
    /// many show: the window paint, hit-testing and accessibility share.
    fn window(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
    ) -> (Vec<HistoryRow>, Vec<RowShape>, usize, usize) {
        let rows = self.rows(host);
        let shapes = Self::shapes(host, layout, &rows);
        let start = self.start.min(layout.max_start(&shapes));
        let shown = layout
            .visible_rows_in(&shapes, start)
            .min(rows.len().saturating_sub(start));
        (rows, shapes, start, shown)
    }

    /// The row (and control) under `p` among the visible rows. A row's icon
    /// buttons only answer while they are shown: when the row is hovered,
    /// which a press inside it always is.
    fn row_hit(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        p: Point,
    ) -> Option<(usize, Option<RowControl>)> {
        let (rows, shapes, start, shown) = self.window(host, layout);
        let bands = shapes.get(start..).unwrap_or_default();
        for offset in (start..rows.len()).take(shown) {
            let position = offset.saturating_sub(start);
            let rect = layout.row_in(position, bands);
            if !rect.contains(p) {
                continue;
            }
            let shape = bands.get(position).copied().unwrap_or_default();
            let dismiss = layout.row_controls(rect, &shape);
            let control = if dismiss.contains(p) {
                Some(RowControl::Dismiss)
            } else {
                layout
                    .row_actions(rect, &shape)
                    .iter()
                    .position(|action| action.contains(p))
                    .map(RowControl::Action)
            };
            return Some((offset, control));
        }
        None
    }

    /// Act on row `index`: a group header folds or unfolds; a notification
    /// goes where it leads, or the given control. A notification with
    /// nowhere to go opens in the reader instead, its focus shown only when
    /// the keyboard opened it.
    fn activate_row(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        index: usize,
        (control, by_keyboard): (Option<RowControl>, bool),
    ) -> CenterOutcome {
        let rows = self.rows(host);
        let Some(row) = rows.get(index).cloned() else {
            return CenterOutcome::Handled;
        };
        match (row, control) {
            (_, Some(RowControl::Dismiss)) => self.dismiss_row(host, layout, index),
            (HistoryRow::Group { key, .. }, _) => {
                if !self.collapsed.remove(&key) {
                    self.collapsed.insert(key.clone());
                }
                self.selected = Some(RowIdentity::Group(key));
                host.changed();
            }
            (HistoryRow::Item { index: at, .. }, Some(RowControl::Action(choice))) => {
                let notification = host.log().history().get(at).cloned();
                if let Some(notification) = notification
                    && let Some((key, _)) = row_actions_of(host, &notification).get(choice)
                {
                    host.invoke_action(notification.id, key);
                    self.reselect_after_removal(host, layout, index);
                }
            }
            (HistoryRow::Item { index: at, .. }, None) => {
                let id = host.log().history().get(at).map(|n| n.id);
                let Some(id) = id else {
                    return CenterOutcome::Handled;
                };
                let outcome = host.activate_history(id);
                if matches!(
                    outcome,
                    HistoryActivation::Invoked | HistoryActivation::OpenApp { .. }
                ) {
                    self.reselect_after_removal(host, layout, index);
                }
                match outcome {
                    HistoryActivation::OpenApp { app_id, label } => {
                        return CenterOutcome::OpenApp { app_id, label };
                    }
                    HistoryActivation::Unavailable => {
                        // Keyboard focus returns to the row it came from;
                        // a pointer leaves no ring behind.
                        self.selected = by_keyboard.then_some(RowIdentity::Item(id));
                        self.row_control = None;
                        self.back_focused = false;
                        self.action_focus = None;
                        self.reader = Some(NotificationReader::new(id, by_keyboard));
                        host.changed();
                    }
                    HistoryActivation::Invoked | HistoryActivation::Missing => {}
                }
            }
        }
        CenterOutcome::Handled
    }

    /// Dismiss row `index`: one notification, or every notification of a
    /// group header.
    fn dismiss_row(&mut self, host: &dyn NotificationHost, layout: &CenterLayout, index: usize) {
        let rows = self.rows(host);
        match rows.get(index) {
            Some(HistoryRow::Group { key, .. }) => {
                host.dismiss_history_group(key);
                self.collapsed.remove(key);
            }
            Some(HistoryRow::Item { index: at, .. }) => {
                let id = host.log().history().get(*at).map(|n| n.id);
                if let Some(id) = id {
                    host.dismiss_history(id);
                }
            }
            None => return,
        }
        self.reselect_after_removal(host, layout, index);
    }

    /// Keep a keyboard selection on the row that slid into the removed
    /// row's place, so repeated Delete walks down the list.
    fn reselect_after_removal(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        index: usize,
    ) {
        if self.selected.is_none() {
            return;
        }
        if self.rows(host).is_empty() {
            self.selected = None;
            self.row_control = None;
            return;
        }
        self.select_row(host, layout, index);
    }

    fn clear(&mut self, host: &dyn NotificationHost) {
        host.clear_history();
        self.start = 0;
        self.wheel_remainder = 0.0;
    }

    /// A press at `p` inside the panel.
    pub fn click(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        p: Point,
    ) -> CenterOutcome {
        if self.is_reading() {
            if self.reading_notification(host).is_none() || layout.reader_back().contains(p) {
                self.close_reader();
                host.changed();
            }
            return CenterOutcome::Handled;
        }
        if layout.back().contains(p) {
            return CenterOutcome::Back;
        }
        let (history_empty, can_undo) = {
            let log = host.log();
            (log.history().is_empty(), log.can_undo_clear())
        };
        if layout.undo().contains(p) && can_undo {
            host.undo_clear();
            self.back_focused = false;
            self.action_focus = None;
            self.selected = None;
            self.row_control = None;
        } else if layout.clear().contains(p) && !history_empty {
            self.clear(host);
            self.back_focused = false;
            self.action_focus = Some(ActionFocus::Undo);
            self.selected = None;
            self.row_control = None;
        } else if let Some((row, control)) = self.row_hit(host, layout, p) {
            // A pointer press acts on the row without leaving a keyboard
            // selection ring behind it.
            self.selected = None;
            self.row_control = None;
            self.back_focused = false;
            self.action_focus = None;
            return self.activate_row(host, layout, row, (control, false));
        } else {
            return CenterOutcome::Ignored;
        }
        CenterOutcome::Handled
    }

    /// Keyboard navigation, available without pointer hover so the retained
    /// history stays usable by keyboard and assistive-technology users.
    /// Consumes at either edge too, so arrows never start moving whatever is
    /// under the panel once the list has reached its beginning or end.
    fn reading_key(&mut self, host: &dyn NotificationHost, logical_key: &Key) -> bool {
        if let Some(reader) = self.reader.as_mut() {
            if matches!(logical_key, Key::Named(NamedKey::Tab)) {
                reader.toggle_focus();
                host.changed();
                return true;
            }
            if reader.key(logical_key) {
                host.changed();
            }
            let back = matches!(logical_key, Key::Named(NamedKey::Escape))
                || (matches!(logical_key, Key::Named(NamedKey::Enter | NamedKey::Space))
                    && reader.activate_back());
            if back || self.reading_notification(host).is_none() {
                self.close_reader();
                host.changed();
            }
            return true;
        }
        false
    }

    /// The row, then its controls in reading order: the icon buttons on its
    /// first line, then its own actions under the text. The same order as
    /// the row's accessibility children.
    fn row_focus_order(host: &dyn NotificationHost, row: &HistoryRow) -> Vec<Option<RowControl>> {
        let mut order = vec![None, Some(RowControl::Dismiss)];
        let actions = Self::row_actions(host, row).len();
        order.extend((0..actions).map(|at| Some(RowControl::Action(at))));
        order
    }

    fn cycle_row_focus(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        backwards: bool,
    ) {
        let rows = self.rows(host);
        let Some(index) = self.selected_row(&rows, host.log().history()) else {
            self.select_row(host, layout, self.start);
            return;
        };
        let Some(row) = rows.get(index) else {
            return;
        };
        let order = Self::row_focus_order(host, row);
        let current = order
            .iter()
            .position(|control| *control == self.row_control)
            .unwrap_or(0);
        let next = if backwards {
            current.checked_sub(1)
        } else {
            current.checked_add(1).filter(|next| *next < order.len())
        };
        if let Some(next) = next {
            self.row_control = order.get(next).copied().flatten();
        } else {
            let row = if backwards {
                index
                    .checked_sub(1)
                    .unwrap_or_else(|| rows.len().saturating_sub(1))
            } else {
                index.saturating_add(1).checked_rem(rows.len()).unwrap_or(0)
            };
            self.select_row(host, layout, row);
            if backwards {
                self.row_control = rows
                    .get(row)
                    .and_then(|row| Self::row_focus_order(host, row).last().copied())
                    .flatten();
            }
        }
        self.back_focused = false;
        self.action_focus = None;
        host.changed();
    }

    pub fn key(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        logical_key: &Key,
    ) -> CenterOutcome {
        self.key_with_shift(host, layout, logical_key, false)
    }

    pub fn key_with_shift(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        logical_key: &Key,
        backwards: bool,
    ) -> CenterOutcome {
        if self.reading_key(host, logical_key) {
            return CenterOutcome::Handled;
        }
        let rows = self.rows(host);
        let total = rows.len();
        let selected = self.selected_row(&rows, host.log().history());
        let can_undo = host.log().can_undo_clear();
        let page = isize::try_from(self.visible_rows(host, layout)).unwrap_or(isize::MAX);
        let mut outcome = CenterOutcome::Handled;
        match logical_key {
            Key::Named(NamedKey::Escape) => return CenterOutcome::Back,
            Key::Named(NamedKey::Enter | NamedKey::Space) if self.back_focused => {
                return CenterOutcome::Back;
            }
            Key::Named(NamedKey::Enter | NamedKey::Space)
                if self.action_focus == Some(ActionFocus::Clear) && total > 0 =>
            {
                host.clear_history();
                self.back_focused = false;
                self.action_focus = Some(ActionFocus::Undo);
                self.start = 0;
                self.selected = None;
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) if selected.is_some() => {
                if let Some(index) = selected {
                    let control = self.row_control;
                    outcome = self.activate_row(host, layout, index, (control, true));
                }
            }
            Key::Named(NamedKey::Delete) if selected.is_some() => {
                if let Some(index) = selected {
                    self.dismiss_row(host, layout, index);
                }
            }
            Key::Named(direction @ (NamedKey::ArrowRight | NamedKey::ArrowLeft))
                if selected.is_some_and(|index| {
                    matches!(rows.get(index), Some(HistoryRow::Group { expanded, .. })
                        if *expanded == (*direction == NamedKey::ArrowLeft))
                }) =>
            {
                if let Some(index) = selected {
                    outcome = self.activate_row(host, layout, index, (None, true));
                }
            }
            Key::Named(NamedKey::ArrowRight) if selected.is_some() => {
                // Into the row's controls, from its first.
                if let Some(row) = selected.and_then(|index| rows.get(index))
                    && self.row_control.is_none()
                {
                    self.row_control = Self::row_focus_order(host, row).get(1).copied().flatten();
                    host.changed();
                }
            }
            Key::Named(NamedKey::ArrowLeft) if selected.is_some() => {
                self.row_control = None;
                host.changed();
            }
            Key::Named(NamedKey::Tab) => self.cycle_row_focus(host, layout, backwards),
            Key::Named(NamedKey::ArrowDown) => self.move_selection(host, layout, 1),
            Key::Named(NamedKey::ArrowUp) => self.move_selection(host, layout, -1),
            Key::Named(NamedKey::PageDown) if selected.is_some() => {
                self.move_selection(host, layout, page);
            }
            Key::Named(NamedKey::PageUp) if selected.is_some() => {
                self.move_selection(host, layout, page.saturating_neg());
            }
            Key::Named(NamedKey::Home) if selected.is_some() => self.select_row(host, layout, 0),
            Key::Named(NamedKey::End) if selected.is_some() => {
                self.select_row(host, layout, usize::MAX);
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) if can_undo => {
                host.undo_clear();
                self.back_focused = false;
                self.action_focus = None;
            }
            Key::Named(NamedKey::Delete) if total > 0 => self.clear(host),
            Key::Named(NamedKey::PageDown) => self.scroll(host, layout, page),
            Key::Named(NamedKey::PageUp) => self.scroll(host, layout, page.saturating_neg()),
            Key::Named(NamedKey::Home) => self.scroll(host, layout, -isize::MAX),
            Key::Named(NamedKey::End) => self.scroll(host, layout, isize::MAX),
            _ => return CenterOutcome::Ignored,
        }
        let shapes = Self::shapes(host, layout, &self.rows(host));
        let _ = self.clamp_start(layout, &shapes);
        outcome
    }

    /// `tilo-control list-targets` names for the centre's header chips, rows
    /// and their trailing controls, under `prefix`.
    pub fn semantic_rects(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        prefix: &str,
    ) -> Vec<(String, Rect)> {
        if self.is_reading() {
            return vec![
                (format!("{prefix}notifications_back"), layout.reader_back()),
                (
                    format!("{prefix}notification_reading"),
                    layout.reading_viewport(),
                ),
            ];
        }
        let (rows, shapes, start, shown) = self.window(host, layout);
        let bands = shapes.get(start..).unwrap_or_default();
        let mut out = vec![(format!("{prefix}notifications_back"), layout.back())];
        if !host.log().history().is_empty() {
            out.push((format!("{prefix}notifications_clear"), layout.clear()));
        }
        for offset in (start..rows.len()).take(shown) {
            let position = offset.saturating_sub(start);
            let rect = layout.row_in(position, bands);
            let shape = bands.get(position).copied().unwrap_or_default();
            let dismiss = layout.row_controls(rect, &shape);
            out.push((format!("{prefix}notification_row[{offset}]"), rect));
            out.push((format!("{prefix}notification_dismiss[{offset}]"), dismiss));
            for (at, action) in layout.row_actions(rect, &shape).iter().enumerate() {
                out.push((
                    format!("{prefix}notification_action[{offset}][{at}]"),
                    *action,
                ));
            }
        }
        out
    }

    /// What the centre says about itself: why nothing pops up (the
    /// header's quiet status line, read first), then how many rows it
    /// keeps.
    fn description(host: &dyn NotificationHost, total: usize) -> String {
        let rows = if total == 0 {
            t("a11y-notifications-none")
        } else {
            t_args("a11y-notifications-description", &[("count", total.into())])
        };
        match host.quiet_status() {
            Some(status) => format!("{status}. {rows}"),
            None => rows,
        }
    }

    /// The centre as one modal dialog: the row list, Clear all, and while
    /// it can still be undone, the cleared status and Undo. `now` is one
    /// clock edge for every row, so adjacent rows cannot label their ages
    /// across a relative-time boundary.
    pub fn accessibility(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        now: Instant,
    ) -> AccessNode {
        if let (Some(reader), Some(notification)) =
            (self.reader.as_ref(), self.reading_notification(host))
        {
            return reader.accessibility(&notification, layout);
        }
        let (rows, ..) = self.window(host, layout);
        let total = rows.len();
        let description = Self::description(host, total);
        let history = AccessNode::new(Role::List)
            .with_name(t("a11y-notifications-list"))
            .with_children(self.accessibility_rows(host, layout, now));
        let mut children = vec![history];
        if total > 0 {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(t("a11y-notifications-clear"))
                    .with_description(t("a11y-notifications-clear-description"))
                    .with_bounds(layout.clear())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.action_focus == Some(ActionFocus::Clear)),
                    ),
            );
        }
        if host.log().can_undo_clear() {
            children.push(
                AccessNode::new(Role::Group)
                    .with_name(t("a11y-notifications-cleared"))
                    .with_live(ui::a11y::Politeness::Polite),
            );
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(t("a11y-notifications-undo"))
                    .with_description(t("a11y-notifications-undo-description"))
                    .with_bounds(layout.undo())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.action_focus == Some(ActionFocus::Undo)),
                    ),
            );
        }
        children.push(
            AccessNode::new(Role::Button)
                .with_name(leading_label(layout))
                .with_bounds(layout.back())
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.back_focused),
                ),
        );
        AccessNode::new(Role::Dialog)
            .with_name(t("a11y-notifications"))
            .with_description(description)
            .with_state(AccessState::new().modal(true))
            .with_children(children)
    }

    /// Appended after existing children so row, Clear and Undo paths stay stable.
    fn back_path_index(host: &dyn NotificationHost) -> usize {
        1usize
            .saturating_add(usize::from(!host.log().history().is_empty()))
            .saturating_add(if host.log().can_undo_clear() { 2 } else { 0 })
    }

    /// The accessibility path index of Undo inside the dialog.
    fn undo_path_index(host: &dyn NotificationHost) -> usize {
        if host.log().history().is_empty() {
            2
        } else {
            3
        }
    }

    /// Bounds of the dialog node at `path` (relative to the dialog).
    pub fn accessibility_rect_for_path(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        path: &[usize],
    ) -> Option<Rect> {
        if self.is_reading() {
            return match path {
                [] => Some(layout.panel()),
                [0] => Some(layout.reader_back()),
                [1] => Some(layout.reading_viewport()),
                _ => None,
            };
        }
        let (history_empty, can_undo) = {
            let log = host.log();
            (log.history().is_empty(), log.can_undo_clear())
        };
        match path {
            [] => Some(layout.panel()),
            [index] if *index == Self::back_path_index(host) => Some(layout.back()),
            [0, row, rest @ ..] => {
                let items = self.accessibility_rows(host, layout, ui::time::now());
                let item = items.get(*row)?;
                let node = match rest {
                    [] => item,
                    [control] => item.children.get(*control)?,
                    _ => return None,
                };
                node.bounds.map(ui::a11y::AccessBounds::to_rect)
            }
            [1] if !history_empty => Some(layout.clear()),
            [index] if can_undo && *index == Self::undo_path_index(host) => Some(layout.undo()),
            _ => None,
        }
    }

    /// Move keyboard focus to the dialog node at `path` (relative to the
    /// dialog). Returns whether the path named something focusable.
    pub fn focus_accessibility_path(
        &mut self,
        host: &dyn NotificationHost,
        _layout: &CenterLayout,
        path: &[usize],
    ) -> bool {
        if let Some(reader) = self.reader.as_mut() {
            let focused = reader.focus(path);
            if focused {
                host.changed();
            }
            return focused;
        }
        if path == [Self::back_path_index(host)] {
            self.action_focus = None;
            self.selected = None;
            self.row_control = None;
            self.back_focused = true;
            host.changed();
            return true;
        }
        if let [0, row, rest @ ..] = path {
            let rows = self.rows(host);
            let index = self.start.saturating_add(*row);
            let identity = rows
                .get(index)
                .and_then(|row| row.identity(host.log().history()));
            let Some(row) = rows.get(index) else {
                return false;
            };
            // Children follow the focus order, without the row itself.
            let control = match rest {
                [] => None,
                [at] => match Self::row_focus_order(host, row).get(at.saturating_add(1)) {
                    Some(control @ Some(_)) => *control,
                    _ => return false,
                },
                _ => return false,
            };
            let Some(identity) = identity else {
                return false;
            };
            self.selected = Some(identity);
            self.row_control = control;
            self.back_focused = false;
            self.action_focus = None;
            host.changed();
            return true;
        }
        let (history_empty, can_undo) = {
            let log = host.log();
            (log.history().is_empty(), log.can_undo_clear())
        };
        let focus = match path {
            [1] if !history_empty => ActionFocus::Clear,
            [index] if can_undo && *index == Self::undo_path_index(host) => ActionFocus::Undo,
            _ => return false,
        };
        self.back_focused = false;
        self.action_focus = Some(focus);
        self.selected = None;
        host.changed();
        true
    }

    /// One accessibility node per visible row, matching the painted rows
    /// one-to-one so a focus path names the row it lights. Group headers are
    /// level-one items with an expanded state; their notifications are
    /// level two.
    fn accessibility_rows(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        now: Instant,
    ) -> Vec<AccessNode> {
        let (rows, shapes, start, shown) = self.window(host, layout);
        let bands = shapes.get(start..).unwrap_or_default();
        let log = host.log();
        let history = log.history();
        let selected = self.selected_row(&rows, history);
        rows.iter()
            .enumerate()
            .skip(start)
            .take(shown)
            .map(|(offset, row)| {
                let position = offset.saturating_sub(start);
                let rect = layout.row_in(position, bands);
                let is_selected = selected == Some(offset);
                let row_focused = is_selected && self.row_control.is_none();
                let node = match row {
                    HistoryRow::Group {
                        key,
                        label,
                        count,
                        expanded,
                        ..
                    } => {
                        let dismiss = layout
                            .row_controls(rect, &bands.get(position).copied().unwrap_or_default());
                        AccessNode::new(Role::ListItem)
                            .with_semantic_key(format!("notification-group:{key}"))
                            .with_name(label.clone())
                            .with_description(t_args(
                                "notification-center-group-description",
                                &[
                                    ("count", (*count).into()),
                                    ("expanded", if *expanded { "yes" } else { "no" }.into()),
                                ],
                            ))
                            .with_level(1)
                            .with_state(
                                AccessState::new()
                                    .expanded(*expanded)
                                    .focusable(true)
                                    .focused(row_focused)
                                    .selected(is_selected),
                            )
                            .with_children(vec![
                                AccessNode::new(Role::Button)
                                    .with_name(t_args(
                                        "notification-center-dismiss-all",
                                        &[("app", label.as_str().into())],
                                    ))
                                    .with_bounds(dismiss)
                                    .with_state(AccessState::new().focusable(true).focused(
                                        is_selected
                                            && self.row_control == Some(RowControl::Dismiss),
                                    )),
                            ])
                    }
                    HistoryRow::Item { index, grouped } => match history.get(*index) {
                        Some(notification) => self.item_node(
                            host,
                            layout,
                            (rect, bands.get(position).copied().unwrap_or_default()),
                            notification,
                            (*grouped, is_selected),
                            now,
                        ),
                        None => AccessNode::new(Role::ListItem),
                    },
                };
                node.with_position(position.saturating_add(1), shown)
                    .with_bounds(rect)
            })
            .collect()
    }

    /// The accessibility node of one retained notification row and its
    /// trailing controls.
    fn item_node(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        (rect, shape): (Rect, RowShape),
        notification: &Notification,
        (grouped, is_selected): (bool, bool),
        now: Instant,
    ) -> AccessNode {
        let row_focused = is_selected && self.row_control.is_none();
        let dismiss = layout.row_controls(rect, &shape);
        let control_focused = |control| is_selected && self.row_control == Some(control);
        let mut controls = vec![
            AccessNode::new(Role::Button)
                .with_name(t("notification-center-dismiss"))
                .with_bounds(dismiss)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(control_focused(RowControl::Dismiss)),
                ),
        ];
        let actions = row_actions_of(host, notification);
        for (at, ((_, label), bounds)) in actions
            .iter()
            .zip(layout.row_actions(rect, &shape))
            .enumerate()
        {
            controls.push(
                AccessNode::new(Role::Button)
                    .with_name(label.clone())
                    .with_bounds(bounds)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(control_focused(RowControl::Action(at))),
                    ),
            );
        }
        AccessNode::new(Role::ListItem)
            .with_semantic_key(format!("notification:{}", notification.id))
            .with_name(notification.title.clone())
            .with_description(accessible_description(notification, now))
            .with_level(if grouped { 2 } else { 1 })
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(row_focused)
                    .selected(is_selected),
            )
            .with_children(controls)
    }

    /// Paint the centre into `layout`'s panel. The host paints the panel
    /// surface itself; `cursor` lights the hovered controls.
    #[allow(
        clippy::too_many_lines,
        reason = "the render handles, the host, the layout and the pointer are each independent inputs; paint and hit rectangles share one layout pass so they cannot drift"
    )]
    pub fn draw(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        cursor: Option<Point>,
    ) {
        if let Some(notification) = self.reading_notification(host)
            && let Some(reader) = self.reader.as_mut()
        {
            reader.draw(
                &notification,
                layout,
                scene,
                transform,
                text_renderer,
                theme,
                cursor,
            );
            return;
        }
        self.reader = None;
        let rect = layout.panel();
        let colors = &theme.tokens.colors;
        let caption_style = theme.text(TextRole::Caption);
        let label_style = theme.text(TextRole::Label);
        let back = layout.back();
        let label = leading_label(layout);
        let face = if layout.is_drill_in() {
            ButtonFace::new(back, &label)
                .role(ButtonRole::Plain)
                .icon(ui::icons::Icon::ChevronLeft)
        } else {
            ButtonFace::icon_only(back, ui::icons::Icon::X, &label)
        };
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &face
                .hovered(cursor.is_some_and(|point| back.contains(point)))
                .focused(self.back_focused),
        );
        let title_style = label_style.clone().with_weight(TextWeight::Semibold);
        let title_rect = layout.title_rect();
        let title_width = f32_of_f64(title_rect.width().max(1.0));
        let title = preview_line(
            text_renderer,
            &t("notification-center-title"),
            title_width,
            &title_style,
        );
        let title_line = ui::widget::TextLineFace::styled(&title, &title_style, TextRole::Label);
        let placed = if layout.title_leads() {
            title_rect
        } else {
            let (w, _) = title_line.measure(text_renderer);
            Rect::new(
                title_rect.center().x - w / 2.0,
                title_rect.y0,
                title_rect.center().x + w / 2.0,
                title_rect.y1,
            )
        };
        title_line.paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            placed,
            colors.text_primary,
        );
        let quiet = layout.quiet_rect();
        if quiet.height() > 0.0
            && let Some(status) = host.quiet_status()
        {
            // The quiet line is one centred caption, from the shared text-line
            // painter.
            let line = ui::widget::TextLineFace::styled(&status, &caption_style, TextRole::Caption);
            line.paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                quiet,
                colors.text_dim,
            );
        }
        let rows = self.rows(host);
        let log = host.log();
        let history = log.history();
        let can_undo = log.can_undo_clear();
        if history.is_empty() {
            self.start = 0;
            DrillInList::draw_empty_from(
                scene,
                transform,
                text_renderer,
                theme,
                rect,
                &t("notification-center-empty"),
                layout.rows_top() - rect.y0,
            );
            if can_undo {
                Self::draw_cleared_band(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    layout,
                    (cursor, self.action_focus == Some(ActionFocus::Undo)),
                );
            }
            return;
        }

        let shapes = Self::shapes(host, layout, &rows);
        let _ = self.clamp_start(layout, &shapes);
        let selected = self.selected_row(&rows, history);
        // Resizing or growing text must keep the focused row/control visible.
        // Retain the same identity and control, and only move the viewport.
        if let Some(index) = selected {
            self.start = layout.start_showing(&shapes, self.start, index);
        }
        let start = self.start;
        let visible_rows = layout.visible_rows_in(&shapes, start);
        let shown = visible_rows.min(rows.len().saturating_sub(start));
        let bands = shapes.get(start..).unwrap_or_default();

        Self::chip(
            scene,
            transform,
            text_renderer,
            theme,
            (
                layout.clear(),
                cursor,
                self.action_focus == Some(ActionFocus::Clear),
            ),
            &t("notification-center-clear-all"),
            ButtonRole::Plain,
        );

        let now = ui::time::now();
        let hovered = |r: Rect| cursor.is_some_and(|p| r.contains(p));
        for (offset, history_row) in rows.iter().enumerate().skip(start).take(shown) {
            let shape = bands
                .get(offset.saturating_sub(start))
                .copied()
                .unwrap_or_default();
            let position = offset.saturating_sub(start);
            let row = layout.row_in(position, bands);
            let row_selected = selected == Some(offset);
            let focused_control = self.row_control.filter(|_| row_selected);
            // At rest a row is only its content; its icon buttons show,
            // over its time, while it is pointed at or keyboard-focused.
            let reveal = hovered(row) || row_selected;
            let notification = match history_row {
                HistoryRow::Group {
                    label,
                    count,
                    expanded,
                    newest,
                    ..
                } => {
                    let Some(newest) = history.get(*newest) else {
                        continue;
                    };
                    // A group is one card around its header and the members
                    // it shows: the members are its children, not rows of
                    // their own.
                    let members = if *expanded { *count } else { 0 };
                    let last = position
                        .saturating_add(members)
                        .min(shown.saturating_sub(1));
                    let container = if last > position {
                        row.union(layout.row_in(last, bands))
                    } else {
                        row
                    };
                    ui::widget::paint_control_row(
                        scene,
                        transform,
                        theme,
                        container,
                        hovered(container),
                        false,
                    );
                    if hovered(row) {
                        ui::widget::paint_control_row(scene, transform, theme, row, true, false);
                    }
                    Self::draw_group_row(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        (layout, row, reveal, &shape),
                        (label, *count, *expanded, newest),
                        now,
                    );
                    if reveal {
                        Self::draw_row_controls(
                            scene,
                            transform,
                            text_renderer,
                            theme,
                            layout.row_controls(row, &shape),
                            (focused_control, cursor),
                        );
                    }
                    if row_selected && self.row_control.is_none() {
                        Self::draw_selection(scene, transform, theme, row);
                    }
                    continue;
                }
                HistoryRow::Item { index, .. } => {
                    let Some(notification) = history.get(*index) else {
                        continue;
                    };
                    notification
                }
            };
            let controls = layout.row_controls(row, &shape);
            let leading_control = controls;
            // A row is a resting Control surface (HIG §9.5): the lift floor
            // with its derived edge, never a ramp-slot wash. A group's
            // member has no surface of its own: it sits in its header's
            // card, and only lifts inside it while pointed at or selected.
            if !shape.is_grouped() {
                ui::widget::paint_control_row(scene, transform, theme, row, hovered(row), false);
            } else if hovered(row) || position == 0 {
                // Inside its group's card, a member lights only while
                // pointed at. A member whose header scrolled out of the
                // window keeps a surface of its own.
                ui::widget::paint_control_row(scene, transform, theme, row, hovered(row), false);
            }
            // A warning or failure keeps a rail beside its text, scannable
            // without taking another line; plain information carries no
            // colour, so the rail means something wherever it appears. The
            // accessibility description names the severity too, so colour
            // is never the only signal.
            if let Some(tone) = severity_tone(notification.severity) {
                // A warning or failure keeps its rail beside its text, from
                // the shared painter; plain information carries none.
                ui::widget::paint_severity_rail(
                    scene,
                    transform,
                    theme,
                    row,
                    tone,
                    qs::CONTROL_PAD,
                );
            }
            // Who sent it, the source/time, title and body: measured lines on
            // the control padding, the sender's mark a glyph in like every
            // row glyph in the panel, its text after it. A grouped
            // notification sits one mark column further in, under its
            // header, and drops the source line: the header names the app.
            let mark = layout.row_mark(row, &shape);
            notification
                .mark()
                .paint(scene, transform, mark, colors.text_primary);
            let x = mark.x1 + qs::RELATED_GAP;
            let text_right = row.x1 - qs::CONTROL_INSET;
            let first_y = row.y0 + qs::CONTROL_PAD;
            let source_y = shape.has_source().then_some(first_y);
            let title_y = match source_y {
                Some(source_y) => source_y + layout.caption_lh,
                None => first_y,
            };
            let body_y = title_y + layout.label_lh;
            // The icon buttons stand where the time is; a line they reach
            // into stops short of them, shown or not, so nothing reflows
            // under the pointer.
            let clear_of = |y: f64| {
                if leading_control.y1 > y {
                    leading_control.x0 - qs::RELATED_GAP
                } else {
                    text_right
                }
            };
            let age = relative_time(notification, now);
            let (time_width, _) = text_renderer.measure_style(&age, None, &caption_style);
            let source_right = if reveal {
                leading_control.x0 - qs::RELATED_GAP
            } else {
                text_right - f64::from(time_width) - qs::RELATED_GAP
            };
            let caption_line_h = layout.caption_lh;
            // The age takes the row's first line: the source line when
            // the row has one, else the title line beside the title.
            let time_y = source_y.unwrap_or(title_y);
            let time_h = if shape.has_source() {
                caption_line_h
            } else {
                layout.label_lh
            };
            if let Some(source_y) = source_y {
                let sender = notification.sender_name();
                let source_width = (source_right - x).max(0.0);
                let source = preview_line(
                    text_renderer,
                    &sender,
                    f32_of_f64(source_width),
                    &caption_style,
                );
                // The source line is one shared caption, starting at the
                // row's mark and elided to the room beside the controls.
                ui::widget::TextLineFace::styled(&source, &caption_style, TextRole::Caption)
                    .start()
                    .paint(
                        &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                        Rect::new(x, source_y, x + source_width, source_y + caption_line_h),
                        colors.text_dim,
                    );
            }
            if !reveal {
                ui::widget::TextLineFace::styled(&age, &caption_style, TextRole::Caption)
                    .start()
                    .paint(
                        &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                        Rect::new(
                            text_right - f64::from(time_width),
                            time_y,
                            text_right,
                            time_y + time_h,
                        ),
                        colors.text_dim,
                    );
            }
            let title_style = label_style.clone().with_weight(TextWeight::Semibold);
            let title_right = if shape.has_source() {
                clear_of(title_y)
            } else {
                source_right
            };
            let title_width = (title_right - x).max(0.0);
            let title = preview_line(
                text_renderer,
                &notification.title,
                f32_of_f64(title_width),
                &title_style,
            );
            ui::widget::TextLineFace::styled(&title, &title_style, TextRole::Label)
                .start()
                .paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    Rect::new(x, title_y, x + title_width, title_y + layout.label_lh),
                    colors.text_primary,
                );
            if notification.show_preview && !notification.body.is_empty() {
                let body_width = (clear_of(body_y) - x).max(0.0);
                let body = preview_lines(
                    text_renderer,
                    &notification.body,
                    f32_of_f64(body_width),
                    &caption_style,
                    shape.body_lines().max(1),
                );
                let body_h = caption_line_h * ui::num::f64_of_usize(shape.body_lines().max(1));
                ui::widget::TextBlockFace::new(&body, &caption_style).paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    Rect::new(x, body_y, x + body_width, body_y + body_h),
                    colors.text_dim,
                );
            }
            // The notification's own choices, as on its banner: always
            // shown, since they are what the notification asks.
            let actions = row_actions_of(host, notification);
            let last = actions.len().saturating_sub(1);
            for (at, ((_, label), rect)) in actions
                .iter()
                .zip(layout.row_actions(row, &shape))
                .enumerate()
            {
                Button::paint(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    &ButtonFace::new(rect, label)
                        .control_size(ControlSize::Small)
                        .role(if at == last {
                            ButtonRole::Primary
                        } else {
                            ButtonRole::Secondary
                        })
                        .hovered(hovered(rect))
                        .focused(focused_control == Some(RowControl::Action(at))),
                );
            }
            if reveal {
                Self::draw_row_controls(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    controls,
                    (focused_control, cursor),
                );
            }

            if row_selected && self.row_control.is_none() {
                Self::draw_selection(scene, transform, theme, row);
            }
        }

        if can_undo {
            Self::draw_cleared_band(
                scene,
                transform,
                text_renderer,
                theme,
                layout,
                (cursor, self.action_focus == Some(ActionFocus::Undo)),
            );
        }
    }

    /// The toolkit's button face, with the same role, hover and focus
    /// treatment as controls in retained widgets.
    fn chip(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        (rect, cursor, focused): (Rect, Option<Point>, bool),
        label: &str,
        role: ButtonRole,
    ) {
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(rect, label)
                .role(role)
                .hovered(cursor.is_some_and(|point| rect.contains(point)))
                .focused(focused),
        );
    }

    /// The "Notifications cleared" band with its Undo button.
    fn draw_cleared_band(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &CenterLayout,
        (cursor, focused): (Option<Point>, bool),
    ) {
        let colors = &theme.tokens.colors;
        let style = theme.text(TextRole::Caption);
        let status = layout.cleared_band();
        ui::widget::paint_control_row(scene, transform, theme, status, false, false);
        let label = layout.cleared_label_rect();
        let cleared = t("notification-center-cleared");
        ui::widget::TextLineFace::styled(&cleared, &style, TextRole::Caption)
            .start()
            .paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                label,
                colors.text_primary,
            );
        Self::chip(
            scene,
            transform,
            text_renderer,
            theme,
            (layout.undo(), cursor, focused),
            &t("notification-center-undo"),
            ButtonRole::Secondary,
        );
    }

    /// One group header row: fold chevron, the sender's mark and name, the
    /// count as a badge after the name, the newest member's age and (while
    /// folded) its title.
    fn draw_group_row(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        (layout, row, reveal, shape): (&CenterLayout, Rect, bool, &RowShape),
        (label, count, expanded, newest): (&str, usize, bool, &Notification),
        now: Instant,
    ) {
        let colors = &theme.tokens.colors;
        let caption_style = theme.text(TextRole::Caption);
        let label_style = theme.text(TextRole::Label);
        let disclosure = qs::leading_glyph(row, qs::CONTROL_INSET, qs::GLYPH);
        ui::icons::draw_icon(
            scene,
            transform,
            disclosure,
            if expanded {
                ui::icons::Icon::ChevronDown
            } else {
                ui::icons::Icon::ChevronRight
            },
            colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        // The header leads with the same mark as the banners from this app.
        let mark = layout.group_mark(row, shape);
        newest
            .mark()
            .paint(scene, transform, mark, colors.text_primary);
        let dismiss = layout.row_controls(row, shape);
        let x = mark.x1 + qs::RELATED_GAP;
        let title_y = row.y0 + qs::CONTROL_PAD;
        let age = relative_time(newest, now);
        let (time_width, _) = text_renderer.measure_style(&age, None, &caption_style);
        let title_style = label_style.with_weight(TextWeight::Semibold);
        let count = count.to_string();
        let badge_width = ui::widget::BadgeFace::intrinsic_width(&count, text_renderer, theme);
        // The name stops short of the count badge and whatever holds the
        // trailing corner: the dismiss-all button while it shows, else the
        // newest member's age.
        let right = if reveal {
            dismiss.x0 - qs::RELATED_GAP
        } else {
            row.x1 - qs::CONTROL_INSET - f64::from(time_width) - qs::RELATED_GAP
        };
        let label_room = (right - badge_width - qs::RELATED_GAP - x).max(0.0);
        let label = preview_line(text_renderer, label, f32_of_f64(label_room), &title_style);
        let (label_width, _) =
            ui::widget::TextLineFace::styled(&label, &title_style, TextRole::Label)
                .measure(text_renderer);
        ui::widget::TextLineFace::styled(&label, &title_style, TextRole::Label)
            .start()
            .paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                Rect::new(x, title_y, x + label_width, title_y + layout.label_lh),
                colors.text_primary,
            );
        let badge_x = x + label_width + qs::RELATED_GAP;
        ui::widget::BadgeFace::new(
            Rect::new(
                badge_x,
                title_y,
                badge_x + badge_width,
                title_y + layout.label_lh,
            ),
            &count,
        )
        .paint(scene, transform, text_renderer, theme);
        // Dismiss-all stands where the time is while it is shown.
        if !reveal {
            ui::widget::TextLineFace::styled(&age, &caption_style, TextRole::Caption)
                .start()
                .paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    Rect::new(
                        row.x1 - qs::CONTROL_INSET - f64::from(time_width),
                        title_y,
                        row.x1 - qs::CONTROL_INSET,
                        title_y + layout.label_lh,
                    ),
                    colors.text_dim,
                );
        }
        if !expanded {
            let text_w = f32_of_f64((dismiss.x0 - qs::RELATED_GAP - x).max(0.0));
            let title = preview_line(text_renderer, &newest.title, text_w, &caption_style);
            let top = title_y + layout.label_lh;
            ui::widget::TextLineFace::styled(&title, &caption_style, TextRole::Caption)
                .start()
                .paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    Rect::new(x, top, x + f64::from(text_w), top + layout.caption_lh),
                    colors.text_dim,
                );
        }
    }

    /// A row's trailing icon buttons as Controls, the hovered one on the
    /// hover tier and the keyboard-focused one ringed.
    fn draw_row_controls(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        dismiss: Rect,
        (focused, cursor): (Option<RowControl>, Option<Point>),
    ) {
        let mut paint = |rect: Rect, icon: ui::icons::Icon, control, name: &str| {
            Self::quiet_icon_button(
                scene,
                transform,
                text_renderer,
                theme,
                (rect, icon, name),
                (
                    cursor.is_some_and(|point| rect.contains(point)),
                    focused == Some(control),
                ),
            );
        };
        paint(
            dismiss,
            ui::icons::Icon::X,
            RowControl::Dismiss,
            &t("notification-center-dismiss"),
        );
    }

    /// A row's icon button, shown while its row is pointed at or focused:
    /// then only its glyph, in the dim ink of the row's secondary text, so
    /// no control outweighs the title beside it. Itself hovered or focused,
    /// it is the toolkit's full button.
    fn quiet_icon_button(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        (rect, icon, name): (Rect, ui::icons::Icon, &str),
        (hovered, focused): (bool, bool),
    ) {
        if hovered || focused {
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::icon_only(rect, icon, name)
                    .hovered(hovered)
                    .focused(focused),
            );
            return;
        }
        let size = f64::from(theme.tokens.metrics.icon_md).min(rect.width());
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(rect.center(), (size, size)),
            icon,
            theme.tokens.colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }

    /// Ring the keyboard-selected row.
    fn draw_selection(scene: &mut Scene, transform: Affine, theme: &Theme, row: Rect) {
        // The keyboard's selection is the shared focus ring.
        ui::widget::paint_focus_ring(scene, transform, row, qs::control_radius(theme, row), theme);
    }
}

#[cfg(test)]
mod tests {
    use super::super::stack::tests::TestHost;
    use super::*;

    fn from_app(app: &str, title: &str) -> Notification {
        let mut notification = Notification::new(title, "");
        notification.app_id = Some(app.to_string());
        notification.app_name = Some(app.to_uppercase());
        notification
    }

    fn layout() -> CenterLayout {
        CenterLayout::new(Rect::new(0.0, 0.0, 360.0, 560.0), (12.0, 14.0))
    }

    #[test]
    fn keyboard_walks_folds_and_dismisses_rows() {
        let host = TestHost::default();
        for n in [
            from_app("mail", "m1"),
            from_app("mail", "m2"),
            Notification::new("sys", ""),
        ] {
            host.log.borrow_mut().retain(n);
        }
        let layout = layout();
        let mut center = NotificationCenter::new();
        let down = Key::Named(NamedKey::ArrowDown);
        assert_eq!(center.key(&host, &layout, &down), CenterOutcome::Handled);
        assert_eq!(
            center.selected,
            Some(RowIdentity::Item(host.log().history()[2].id))
        );
        center.key(&host, &layout, &down);
        assert!(matches!(center.selected, Some(RowIdentity::Group(_))));
        // Left folds the expanded group; its members leave the rows.
        center.key(&host, &layout, &Key::Named(NamedKey::ArrowLeft));
        assert_eq!(center.rows(&host).len(), 2);
        center.key(&host, &layout, &Key::Named(NamedKey::Delete));
        assert_eq!(host.log().history().len(), 1);
        assert_eq!(host.log().history()[0].title, "sys");
    }

    #[test]
    fn clear_all_is_undoable_from_the_keyboard_and_pointer() {
        let host = TestHost::default();
        host.log.borrow_mut().retain(Notification::new("one", ""));
        let layout = layout();
        let mut center = NotificationCenter::new();
        let clear = layout.clear().center();
        assert_eq!(center.click(&host, &layout, clear), CenterOutcome::Handled);
        assert!(host.log().history().is_empty());
        assert!(host.log().can_undo_clear());
        let enter = Key::Named(NamedKey::Enter);
        assert_eq!(center.key(&host, &layout, &enter), CenterOutcome::Handled);
        assert_eq!(host.log().history().len(), 1);
        assert_eq!(
            center.click(&host, &layout, layout.back().center()),
            CenterOutcome::Back
        );
    }

    #[test]
    fn resize_and_large_text_keep_the_focused_dismiss_action_visible() {
        let host = TestHost::default();
        for index in 0..10 {
            host.log
                .borrow_mut()
                .retain(Notification::new(format!("notice {index}"), "body"));
        }
        let mut center = NotificationCenter::new();
        assert!(center.focus_accessibility_path(&host, &layout(), &[0, 6, 0]));
        let selected = center.selected.clone();
        let theme = Theme::dark().with_text_scale(2.0);
        let mut text = TextRenderer::new();
        let panel = Rect::new(0.0, 0.0, 560.0, 300.0);
        let layout = CenterLayout::new(panel, CenterLayout::metrics(&theme, &mut text));
        center.draw(
            &host,
            &layout,
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            None,
        );
        assert_eq!(center.selected, selected);
        assert_eq!(center.row_control, Some(RowControl::Dismiss));
        assert!(center.first_visible_row() > 0);
        let dialog = center.accessibility(&host, &layout, ui::time::now());
        let dismiss = &dialog.children[0].children[0].children[0];
        let bounds = dismiss.bounds.map(ui::a11y::AccessBounds::to_rect).unwrap();
        assert!(bounds.y0 >= panel.y0 && bounds.y1 <= panel.y1);
        assert!(bounds.height() >= ButtonFace::height(ControlSize::Regular, &theme));
        assert_eq!(
            center.key(&host, &layout, &Key::Named(NamedKey::Delete)),
            CenterOutcome::Handled
        );
        assert_eq!(host.log().history().len(), 9);
    }

    #[test]
    fn pointer_group_activation_releases_clear_all_focus() {
        let host = TestHost::default();
        host.log.borrow_mut().retain(from_app("mail", "one"));
        host.log.borrow_mut().retain(from_app("mail", "two"));
        let layout = layout();
        let mut center = NotificationCenter::new();
        assert!(center.focus_accessibility_path(&host, &layout, &[1]));
        assert_eq!(center.action_focus, Some(ActionFocus::Clear));
        // Fold the group with the pointer after focusing Clear all. Enter
        // must not invoke that old header focus and erase the history.
        let rows = center.rows(&host);
        let shapes = NotificationCenter::shapes(&host, &layout, &rows);
        center.click(&host, &layout, layout.row_in(0, &shapes).center());
        assert_eq!(center.action_focus, None);
        assert_eq!(center.rows(&host).len(), 1);
        assert_eq!(
            center.key(&host, &layout, &Key::Named(NamedKey::Enter)),
            CenterOutcome::Handled
        );
        assert_eq!(center.rows(&host).len(), 3);
        assert_eq!(host.log().history().len(), 2);
    }

    #[test]
    fn escape_leaves_clear_undo_available_and_repeated_clear_restores_history() {
        let host = TestHost::default();
        host.log.borrow_mut().retain(Notification::new("one", ""));
        let layout = layout();
        let mut center = NotificationCenter::new();
        for _ in 0..2 {
            center.click(&host, &layout, layout.clear().center());
            assert!(host.log().history().is_empty());
            assert_eq!(center.action_focus, Some(ActionFocus::Undo));
            assert_eq!(
                center.key(&host, &layout, &Key::Named(NamedKey::Escape)),
                CenterOutcome::Back
            );
            assert!(host.log().can_undo_clear());
            let dialog = center.accessibility(&host, &layout, ui::time::now());
            assert_eq!(
                dialog.children[2]
                    .bounds
                    .map(ui::a11y::AccessBounds::to_rect),
                Some(layout.undo())
            );
            center.click(&host, &layout, layout.undo().center());
            assert_eq!(host.log().history().len(), 1);
            assert_eq!(center.action_focus, None);
            assert_eq!(center.row_control, None);
        }
    }

    #[test]
    fn opening_a_retained_notification_without_a_sender_opens_its_app() {
        let host = TestHost::default();
        host.log.borrow_mut().retain(from_app("mail", "m1"));
        let layout = layout();
        let mut center = NotificationCenter::new();
        let outcome = center.click(&host, &layout, layout.row(0).center());
        assert_eq!(
            outcome,
            CenterOutcome::OpenApp {
                app_id: "mail".into(),
                label: "MAIL".into()
            }
        );
        assert!(host.log().history().is_empty());
    }

    #[test]
    fn accessibility_rows_match_the_painted_rows_and_focus_by_path() {
        let host = TestHost::default();
        host.log
            .borrow_mut()
            .retain(Notification::new("one", "body"));
        host.log.borrow_mut().retain(Notification::new("two", ""));
        let layout = layout();
        let mut center = NotificationCenter::new();
        let dialog = center.accessibility(&host, &layout, ui::time::now());
        assert_eq!(dialog.role, Role::Dialog);
        let list = &dialog.children[0];
        assert_eq!(list.children.len(), 2);
        assert_eq!(list.children[0].name.as_deref(), Some("two"));
        assert_eq!(
            center.accessibility_rect_for_path(&host, &layout, &[0, 1]),
            Some(layout.row_in(1, &[RowShape::new(0)]))
        );
        assert!(center.focus_accessibility_path(&host, &layout, &[0, 1, 0]));
        assert_eq!(center.row_control, Some(RowControl::Dismiss));
        assert!(center.focus_accessibility_path(&host, &layout, &[1]));
        assert_eq!(center.action_focus, Some(ActionFocus::Clear));
    }

    #[test]
    fn a_history_that_overflows_scrolls_by_rows() {
        let host = TestHost::default();
        for i in 0..20 {
            host.log
                .borrow_mut()
                .retain(Notification::new(format!("n{i}"), ""));
        }
        let layout = layout();
        let mut center = NotificationCenter::new();
        let visible = center.visible_rows(&host, &layout);
        assert!(visible < 20);
        center.wheel(&host, &layout, MouseScrollDelta::LineDelta(0.0, -2.0), 1.0);
        assert_eq!(center.start, 2);
        center.key(&host, &layout, &Key::Named(NamedKey::End));
        assert_eq!(center.start, 20 - visible);
    }

    #[test]
    fn a_group_header_and_its_rows_never_repeat_the_source() {
        let host = TestHost::default();
        host.log.borrow_mut().retain(from_app("mail", "one"));
        host.log.borrow_mut().retain(from_app("mail", "two"));
        host.log
            .borrow_mut()
            .retain(Notification::new("Build finished", ""));
        let layout = layout();
        let rows = NotificationCenter::new().rows(&host);
        let shapes = NotificationCenter::shapes(&host, &layout, &rows);
        let grouped = rows
            .iter()
            .zip(&shapes)
            .filter(|(row, _)| matches!(row, HistoryRow::Item { grouped: true, .. }))
            .count();
        assert!(grouped > 0, "the app's members sit under its header");
        for (row, shape) in rows.iter().zip(&shapes) {
            // Only a standalone row names its sender above the title: a
            // header names it once, and its members sit under that name.
            assert_eq!(
                shape.has_source(),
                !matches!(
                    row,
                    HistoryRow::Group { .. } | HistoryRow::Item { grouped: true, .. }
                ),
            );
            assert_eq!(
                shape.is_grouped(),
                matches!(row, HistoryRow::Item { grouped: true, .. })
            );
        }
        let ungrouped = layout.row_extent(&RowShape::default());
        let member = layout.row_extent(&RowShape::default().grouped());
        assert_eq!(member, ungrouped - layout.caption_lh, "one line less");
        let header = layout.row_extent(&RowShape::new(1).without_source());
        assert_eq!(
            header,
            ungrouped - 2.0 * layout.caption_lh,
            "the header trades its source line for one fold preview line"
        );
    }

    #[test]
    fn a_row_opens_where_it_leads_and_only_reads_when_there_is_nowhere() {
        let host = TestHost::default();
        host.log.borrow_mut().retain(Notification::new(
            "Couldn't open Notes",
            "The app stopped before opening. Try again. Your document is safe.",
        ));
        let layout = layout();
        let mut center = NotificationCenter::new();
        assert!(
            !center
                .semantic_rects(&host, &layout, "")
                .iter()
                .any(|(name, _)| name.contains("read")),
            "the row itself is the way in"
        );
        assert_eq!(
            center.click(&host, &layout, layout.row(0).center()),
            CenterOutcome::Handled
        );
        assert!(center.is_reading());
        assert!(
            !center.reader.as_ref().unwrap().focus_visible(),
            "a page the pointer opened draws no focus ring"
        );
        assert_eq!(center.selected, None);
        center.key(&host, &layout, &Key::Named(NamedKey::Tab));
        assert!(center.reader.as_ref().unwrap().focus_visible());
    }

    #[test]
    fn a_rows_own_actions_are_shown_reachable_and_invoked_through_the_host() {
        let host = TestHost::default();
        host.actions.replace(vec![
            ("not-now".into(), "Not now".into()),
            ("keep".into(), "Keep with passkey".into()),
        ]);
        let ask = Notification::new("Keep this identity?", "A passkey brings you back.");
        let id = ask.id;
        host.log.borrow_mut().retain(ask);
        let layout = layout();
        let mut center = NotificationCenter::new();
        let row = layout.row_in(
            0,
            &[RowShape::default().with_choices(&[f64::INFINITY, f64::INFINITY])],
        );
        assert!(
            row.height() > layout.row_h(),
            "the actions get their own band"
        );
        let plain = CenterLayout::preferred_height(1, layout.actions.clone());
        assert!(
            (center.preferred_height(&host, layout.actions.clone())
                - plain
                - (row.height() - layout.row_h()))
            .abs()
                < 1e-6
        );
        let targets = center.semantic_rects(&host, &layout, "");
        let keep = targets
            .iter()
            .find(|(name, _)| name == "notification_action[0][1]")
            .expect("the primary action is a target")
            .1;
        assert!(row.contains(keep.center()));
        let dialog = center.accessibility(&host, &layout, ui::time::now());
        let names: Vec<_> = dialog.children[0].children[0]
            .children
            .iter()
            .filter_map(|node| node.name.clone())
            .collect();
        assert_eq!(
            names,
            [
                t("notification-center-dismiss"),
                "Not now".to_string(),
                "Keep with passkey".to_string()
            ]
        );
        let tab = Key::Named(NamedKey::Tab);
        for expected in [
            None,
            Some(RowControl::Dismiss),
            Some(RowControl::Action(0)),
            Some(RowControl::Action(1)),
        ] {
            center.key(&host, &layout, &tab);
            assert_eq!(center.row_control, expected);
        }
        center.key(&host, &layout, &Key::Named(NamedKey::Enter));
        assert_eq!(*host.invoked.borrow(), [(id, "keep".to_string())]);
        center.click(&host, &layout, keep.center());
        assert_eq!(host.invoked.borrow().len(), 2);
        assert!(
            !center.is_reading(),
            "an action is not the row's activation"
        );
    }

    #[test]
    fn reading_preserves_text_identity_and_never_invokes_or_dismisses() {
        let host = TestHost::default();
        let body = "The app stopped before opening. Try again. Your document is safe.";
        host.log
            .borrow_mut()
            .retain(Notification::new("Couldn't open Notes", body));
        let layout = layout();
        let mut center = NotificationCenter::new();
        center.key(&host, &layout, &Key::Named(NamedKey::ArrowDown));
        center.key(&host, &layout, &Key::Named(NamedKey::Enter));
        assert!(center.is_reading());
        assert!(center.reader.as_ref().unwrap().focus_visible());
        let tree = center.accessibility(&host, &layout, ui::time::now());
        assert_eq!(tree.children[1].description.as_deref(), Some(body));
        assert!(tree.children[1].state.focused);
        for key in [NamedKey::Enter, NamedKey::Space, NamedKey::Delete] {
            assert_eq!(
                center.key(&host, &layout, &Key::Named(key)),
                CenterOutcome::Handled
            );
            assert!(center.is_reading());
            assert_eq!(host.log().history().len(), 1);
        }
        assert_eq!(
            center.key(&host, &layout, &Key::Named(NamedKey::Escape)),
            CenterOutcome::Handled
        );
        assert!(!center.is_reading());
        assert!(center.selected.is_some(), "focus returns to the row");
        assert_eq!(center.row_control, None);
        assert_eq!(
            center.key(&host, &layout, &Key::Named(NamedKey::Escape)),
            CenterOutcome::Back
        );
        center.key(&host, &layout, &Key::Named(NamedKey::Enter));
        assert!(center.is_reading());
        assert!(center.focus_accessibility_path(&host, &layout, &[0]));
        assert_eq!(
            center.key(&host, &layout, &Key::Named(NamedKey::Enter)),
            CenterOutcome::Handled
        );
        assert!(!center.is_reading());
        assert_eq!(host.log().history().len(), 1);
    }

    #[test]
    fn reading_pointer_back_removed_notifications_and_hidden_previews_are_safe() {
        let host = TestHost::default();
        let mut notification = Notification::new("Private message", "Do not reveal this body");
        notification.show_preview = false;
        let id = notification.id;
        host.log.borrow_mut().retain(notification);
        let layout = layout();
        let mut center = NotificationCenter::new();
        let read = layout.row(0);
        assert_eq!(
            center.click(&host, &layout, read.center()),
            CenterOutcome::Handled
        );
        assert!(center.is_reading());
        let tree = center.accessibility(&host, &layout, ui::time::now());
        assert_ne!(
            tree.children[1].description.as_deref(),
            Some("Do not reveal this body")
        );
        assert_eq!(
            center.click(&host, &layout, layout.clear().center()),
            CenterOutcome::Handled
        );
        assert_eq!(host.log().history().len(), 1);
        assert_eq!(
            center.click(&host, &layout, layout.reader_back().center()),
            CenterOutcome::Handled
        );
        assert!(!center.is_reading());
        center.click(&host, &layout, read.center());
        host.log.borrow_mut().take_history(id);
        assert_eq!(
            center.key(&host, &layout, &Key::Named(NamedKey::Enter)),
            CenterOutcome::Handled
        );
        assert!(!center.is_reading());
        assert!(host.log().history().is_empty());
        assert!(!center.focus_accessibility_path(&host, &layout, &[0, 0, 1]));
    }
    #[test]
    fn row_control_tab_order_reverses_and_moves_between_notifications() {
        let host = TestHost::default();
        for title in ["first", "second"] {
            host.log
                .borrow_mut()
                .retain(Notification::new(title, "body"));
        }
        let layout = layout();
        let mut center = NotificationCenter::new();
        let tab = Key::Named(NamedKey::Tab);
        center.key_with_shift(&host, &layout, &tab, false);
        let selected = center.selected.clone();
        assert_eq!(center.row_control, None);
        center.key_with_shift(&host, &layout, &tab, false);
        assert_eq!(center.row_control, Some(RowControl::Dismiss));
        center.key_with_shift(&host, &layout, &tab, true);
        assert_eq!(center.row_control, None);
        assert_eq!(center.selected, selected);
        center.key_with_shift(&host, &layout, &tab, true);
        assert_eq!(center.row_control, Some(RowControl::Dismiss));
        assert_ne!(center.selected, selected);
        center.key_with_shift(&host, &layout, &tab, false);
        assert_eq!(center.row_control, None);
        assert_eq!(center.selected, selected);
        center.key(&host, &layout, &Key::Named(NamedKey::Enter));
        assert!(center.is_reading());
    }

    /// Every row and group header leads with a mark for whoever sent it: an
    /// app's own glyph or artwork, and the system's bell for a notification
    /// from no app at all. The mark gets its own box inside the row and the
    /// text starts after it, so no row has an empty gutter.
    #[test]
    fn every_row_and_group_header_leads_with_its_senders_mark() {
        let host = TestHost::default();
        for n in [
            from_app("mail", "one"),
            from_app("mail", "two"),
            from_app("chat", "hello"),
            Notification::new("Battery low", "20% left"),
        ] {
            host.log.borrow_mut().retain(n);
        }
        let layout = layout();
        let center = NotificationCenter::new();
        let rows = center.rows(&host);
        let shapes = NotificationCenter::shapes(&host, &layout, &rows);
        assert!(
            rows.iter()
                .any(|row| matches!(row, HistoryRow::Group { .. })),
            "the fixture has a group to check its header"
        );
        let mut system = None;
        for (at, (row, shape)) in rows.iter().zip(&shapes).enumerate() {
            let rect = layout.row_in(at, &shapes);
            let (mark, grouped) = match row {
                HistoryRow::Group { .. } => (layout.group_mark(rect, shape), false),
                HistoryRow::Item { index, grouped } => {
                    system = system.or_else(|| {
                        let history = host.log();
                        let notification = history.history().get(*index)?;
                        (!grouped && notification.app_id.is_none()).then(|| notification.clone())
                    });
                    (layout.row_mark(rect, shape), *grouped)
                }
            };
            assert!(
                mark.width() >= qs::GLYPH - 1e-6 && mark.height() >= qs::GLYPH - 1e-6,
                "{row:?} leads with no mark"
            );
            assert!(
                mark.x0 >= rect.x0 + qs::CONTROL_INSET - 1e-6
                    && mark.x1 <= rect.x1 - qs::CONTROL_INSET + 1e-6
                    && mark.y0 >= rect.y0
                    && mark.y1 <= rect.y1,
                "{row:?}'s mark at {mark:?} leaves its row {rect:?}"
            );
            let text_x = mark.x1 + qs::RELATED_GAP;
            match row {
                HistoryRow::Group { .. } => {
                    let disclosure = qs::leading_glyph(rect, qs::CONTROL_INSET, qs::GLYPH);
                    assert!(
                        disclosure.x1 + qs::RELATED_GAP <= mark.x0,
                        "{row:?}'s mark sits on its fold chevron"
                    );
                    assert!(text_x < rect.x1 - qs::CONTROL_INSET);
                }
                HistoryRow::Item { .. } => assert!(
                    (text_x + layout.text_width(grouped) - (rect.x1 - qs::CONTROL_INSET)).abs()
                        < 1e-6,
                    "{row:?}'s text runs under its mark or controls"
                ),
            }
        }
        let notification = system.expect("the fixture has a notification from no app");
        assert_eq!(
            notification.sender_name(),
            t("notification-group-system"),
            "a notification from no app is still attributed"
        );
        assert_eq!(
            notification.mark().icon,
            Some(ui::icons::Icon::Bell),
            "a notification from no app leads with the system's bell"
        );
        let mark_of = |app: &str| {
            host.log()
                .history()
                .iter()
                .find(|n| n.app_id.as_deref() == Some(app))
                .and_then(|n| n.mark().icon)
        };
        assert_eq!(mark_of("mail"), Some(ui::icons::Icon::Mail));
        assert_eq!(mark_of("chat"), Some(ui::icons::Icon::MessageSquare));
        assert_ne!(
            mark_of("mail"),
            mark_of("chat"),
            "two apps never share one mark"
        );
    }

    #[test]
    fn each_sender_known_only_by_its_executable_gets_its_own_mark() {
        let posting = |app: &str| {
            let mut notification = Notification::new("posted", "");
            notification.app_id = Some("/usr/bin/notify-send".to_string());
            notification.app_name = Some(app.to_string());
            notification
        };
        let marks: Vec<_> = ["Notes", "Chat", "Terminal"]
            .into_iter()
            .map(|app| (app, posting(app).mark().icon))
            .collect();
        assert_eq!(
            marks,
            vec![
                ("Notes", Some(ui::icons::Icon::FileText)),
                ("Chat", Some(ui::icons::Icon::MessageSquare)),
                ("Terminal", Some(ui::icons::Icon::SquareTerminal)),
            ],
            "a tool posting for an app is drawn as that app"
        );
        // The launcher-verified app keeps its own glyph whichever name the
        // sender used.
        let mut chat = from_app("chat", "hello");
        chat.app_name = Some("Something else".to_string());
        assert_eq!(chat.mark().icon, Some(ui::icons::Icon::MessageSquare));
    }

    /// The mark is painted from the notification: two rows that differ only
    /// in who sent them draw different glyphs.
    #[test]
    fn a_rows_mark_is_painted_from_its_sender() {
        let paint = |notification: Notification| {
            let host = TestHost::default();
            host.log.borrow_mut().retain(notification);
            let layout = layout();
            let mut center = NotificationCenter::new();
            let mut scene = Scene::new();
            center.draw(
                &host,
                &layout,
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                None,
            );
            scene.encoding().path_data.clone()
        };
        let system = paint(Notification::new("Battery low", "20% left"));
        let app = paint(from_app("mail", "Battery low"));
        assert!(!system.is_empty() && !app.is_empty());
        assert_ne!(
            system, app,
            "an app's row and the system's draw their own marks"
        );
    }
}

#[cfg(test)]
#[path = "center_header_tests.rs"]
mod header_tests;

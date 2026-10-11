//! The window's header row as a face: one geometry for `NavShell` and for
//! every painted app.
//!
//! Windows have no title bar; the top band of the window is the header row
//! (HIG §1, "The header row"). [`crate::widget::NavShell`] draws it for
//! widget apps. A painted app that owns its whole surface builds a
//! [`HeaderRowFace`] for the same band and gets the same rects the shell
//! uses:
//!
//! - the row itself, `metrics.header_height` tall (grown with the text the
//!   way a one-line control grows);
//! - the leading shell buttons: the sidebar toggle and Back, Plain icon
//!   buttons of `min_pointer_target` in the `s3` inset;
//! - the title band: `Heading` in sentence case, starting at the content
//!   gutter, or just after the leading buttons;
//! - the actions band: from the end of the title to the trailing buttons;
//! - the trailing buttons: the window's History control in the top-trailing
//!   corner ([`HeaderRowFace::history_rect`]), any other trailing chip
//!   before it;
//! - the rule: one hairline under a content column's row.
//!
//! ```ignore
//! let face = HeaderRowFace::new(area, theme).with_toggle().with_history();
//! face.paint_rule(scene, transform, theme);
//! face.paint_title(scene, transform, text, theme, "Report.pdf", None);
//! chrome::paint_history_control(scene, transform, text, theme, face.history_rect(), control, hovered);
//! let body = face.body();
//! ```

use crate::num::f32_of_f64;
use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::window_header::HistoryControl;

/// The header row's sizes, resolved against one theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeaderRowMetrics {
    /// Height of the row: `metrics.header_height`, never less than a shell
    /// button in its inset.
    pub height: f64,
    /// Side of a shell button (`min_pointer_target`, grown with the text).
    pub chip: f64,
    /// Inset of the shell buttons from the row's edges (`s3`).
    pub inset: f64,
    /// Gap between neighbours in the row (`s2`).
    pub gap: f64,
    /// Inset of a title with no leading button: the content gutter
    /// (`rhythm.group`), so the title lines up with the content under it.
    pub gutter: f64,
}

impl HeaderRowMetrics {
    #[must_use]
    pub fn resolve(theme: &Theme) -> Self {
        let chip = crate::widget::text_control_height(
            f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
            theme,
        );
        let inset = f64::from(theme.tokens.spacing.s3);
        Self {
            height: f64::from(theme.tokens.metrics.header_height).max(chip + 2.0 * inset),
            chip,
            inset,
            gap: f64::from(theme.tokens.spacing.s2),
            gutter: crate::chrome::column_gutter(theme),
        }
    }

    /// A shell button's square, `index` places from the leading edge of
    /// `row` (`0` is the corner).
    #[must_use]
    pub fn leading_chip(&self, row: Rect, index: usize) -> Rect {
        let y = row.y0 + (row.height() - self.chip) / 2.0;
        let x = row.x0 + self.inset + steps(index) * (self.chip + self.gap);
        Rect::new(x, y, x + self.chip, y + self.chip)
    }

    /// A shell button's square, `index` places from the trailing edge of
    /// `row` (`0` is the corner).
    #[must_use]
    pub fn trailing_chip(&self, row: Rect, index: usize) -> Rect {
        let y = row.y0 + (row.height() - self.chip) / 2.0;
        let x1 = row.x1 - self.inset - steps(index) * (self.chip + self.gap);
        Rect::new(x1 - self.chip, y, x1, y + self.chip)
    }
}

impl Default for HeaderRowMetrics {
    fn default() -> Self {
        Self::resolve(&Theme::dark())
    }
}

fn steps(index: usize) -> f64 {
    f64::from(u32::try_from(index).unwrap_or(u32::MAX))
}

/// The geometry of one header row. See the module docs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeaderRowFace {
    /// The whole row.
    pub row: Rect,
    /// The sidebar toggle, or `Rect::ZERO`.
    pub toggle: Rect,
    /// The Back button, or `Rect::ZERO`.
    pub back: Rect,
    /// Where the title may stand: from the title's start to the trailing
    /// buttons. The title takes its measured width from the leading end.
    pub title: Rect,
    /// Where the column's actions may stand, trailing-aligned: the same run
    /// as the title until [`Self::split`] gives the title its width.
    pub actions: Rect,
    /// The window's History control, or `Rect::ZERO`.
    pub history: Rect,
    /// A trailing shell button before History (the inspector toggle), or
    /// `Rect::ZERO`.
    pub trailing: Rect,
    /// The hairline under the row, or `Rect::ZERO` for a row without one (a
    /// sidebar, a drawer).
    pub rule: Rect,
    metrics: HeaderRowMetrics,
    leading: usize,
    trailing_count: usize,
    gutter_title: bool,
}

impl HeaderRowFace {
    /// The header row across the top of `area`, with a rule under it and no
    /// shell buttons yet.
    #[must_use]
    pub fn new(area: Rect, theme: &Theme) -> Self {
        let metrics = HeaderRowMetrics::resolve(theme);
        let row = Rect::new(
            area.x0,
            area.y0,
            area.x1,
            (area.y0 + metrics.height).min(area.y1.max(area.y0)),
        );
        Self::in_row(row, metrics)
    }

    /// The face of an exact row rect (the shell computes its rows itself).
    #[must_use]
    pub(crate) fn in_row(row: Rect, metrics: HeaderRowMetrics) -> Self {
        let mut face = Self {
            row,
            toggle: Rect::ZERO,
            back: Rect::ZERO,
            title: Rect::ZERO,
            actions: Rect::ZERO,
            history: Rect::ZERO,
            trailing: Rect::ZERO,
            rule: Rect::ZERO,
            metrics,
            leading: 0,
            trailing_count: 0,
            gutter_title: true,
        };
        face.rule = rule_under(row);
        face.refresh();
        face
    }

    /// The row's height for `theme`.
    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        HeaderRowMetrics::resolve(theme).height
    }

    #[must_use]
    pub fn metrics(&self) -> HeaderRowMetrics {
        self.metrics
    }

    /// Put the sidebar toggle at the row's leading corner.
    #[must_use]
    pub fn with_toggle(mut self) -> Self {
        self.toggle = self.next_leading();
        self
    }

    /// Put a Back button at the leading edge, after the toggle.
    #[must_use]
    pub fn with_back(mut self) -> Self {
        self.back = self.next_leading();
        self
    }

    /// Put the window's History control in the row's trailing corner (a
    /// trailing chip placed before it moves one place in).
    #[must_use]
    pub fn with_history(mut self) -> Self {
        if self.trailing.width() > 0.0 {
            self.history = self.metrics.trailing_chip(self.row, 0);
            self.trailing = self.metrics.trailing_chip(self.row, 1);
            self.trailing_count = self.trailing_count.saturating_add(1);
            self.refresh();
        } else {
            self.history = self.next_trailing();
        }
        self
    }

    /// [`Self::with_history`] where the host offered the window's History
    /// control (`shown`: [`crate::widget::HistoryOpener::is_shown`]).
    #[must_use]
    pub fn with_history_if(self, shown: bool) -> Self {
        if shown { self.with_history() } else { self }
    }

    /// A trailing shell button before History (an inspector toggle).
    #[must_use]
    pub fn with_trailing(mut self) -> Self {
        self.trailing = self.next_trailing();
        self
    }

    /// Start the title at the shell buttons' inset instead of the content
    /// gutter (a sidebar's title lines up with its rows, not with content).
    #[must_use]
    pub fn with_inset_title(mut self) -> Self {
        self.gutter_title = false;
        self.refresh();
        self
    }

    fn next_leading(&mut self) -> Rect {
        let rect = self.metrics.leading_chip(self.row, self.leading);
        self.leading = self.leading.saturating_add(1);
        self.refresh();
        rect
    }

    fn next_trailing(&mut self) -> Rect {
        let rect = self.metrics.trailing_chip(self.row, self.trailing_count);
        self.trailing_count = self.trailing_count.saturating_add(1);
        self.refresh();
        rect
    }

    fn refresh(&mut self) {
        let m = self.metrics;
        let x0 = if self.leading > 0 {
            self.row.x0 + m.inset + steps(self.leading) * (m.chip + m.gap)
        } else if self.gutter_title {
            self.row.x0 + m.gutter
        } else {
            self.row.x0 + m.inset
        };
        let x1 = if self.trailing_count > 0 {
            self.row.x1 - m.inset - steps(self.trailing_count) * (m.chip + m.gap) + m.gap
        } else {
            self.row.x1 - m.inset
        };
        let band = Rect::new(x0, self.row.y0, x1.max(x0), self.row.y1);
        self.title = band;
        self.actions = band;
    }

    /// The area under the row: what a painted app lays its content in.
    #[must_use]
    pub fn body_of(&self, area: Rect) -> Rect {
        Rect::new(area.x0, self.row.y1.min(area.y1), area.x1, area.y1)
    }

    /// The History control's rect: where a painted app paints
    /// [`paint_history_control`] and routes its presses.
    #[must_use]
    pub fn history_rect(&self) -> Rect {
        self.history
    }

    /// Give the title `width` (its measured width) and the actions what is
    /// left after it: `(title, actions)`.
    #[must_use]
    pub fn split(&self, width: f64) -> (Rect, Rect) {
        let band = self.title;
        let title_x1 = (band.x0 + width.max(0.0)).min(band.x1);
        let title = Rect::new(band.x0, band.y0, title_x1, band.y1);
        let start = if width > 0.0 {
            title_x1 + self.metrics.gap
        } else {
            band.x0
        };
        let actions = Rect::new(start.min(band.x1), band.y0, band.x1, band.y1);
        (title, actions)
    }

    /// Paint the rule under the row on `theme`'s ground.
    pub fn paint_rule(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if self.rule.width() <= 0.0 {
            return;
        }
        crate::surface::separator(
            scene,
            transform,
            self.rule.x0,
            self.rule.x1,
            self.rule.y0,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
    }

    /// Paint `title` (and a quiet `count` after it) in the title band,
    /// elided to fit before `actions_x0`, the leading edge of what the
    /// actions took (`None`: the whole band).
    pub fn paint_title(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        title: &str,
        count: Option<usize>,
        actions_x0: Option<f64>,
    ) {
        let x1 = actions_x0.map_or(self.title.x1, |x| (x - self.metrics.gap).min(self.title.x1));
        let band = Rect::new(self.title.x0, self.title.y0, x1, self.title.y1);
        paint_title_in(scene, transform, text, theme, band, title, count);
    }
}

/// The hairline rect under a row.
fn rule_under(row: Rect) -> Rect {
    if row.width() <= 0.0 || row.height() <= 0.0 {
        return Rect::ZERO;
    }
    Rect::new(row.x0, row.y1, row.x1, row.y1)
}

/// The style of a header row's title.
pub(crate) fn title_style(theme: &Theme) -> crate::text_render::TextStyle {
    theme.text(TextRole::Heading)
}

fn count_style(theme: &Theme) -> crate::text_render::TextStyle {
    theme.text(TextRole::Caption).numeric()
}

/// The width a title and its count take in the row.
pub(crate) fn measure_title(
    text: &mut TextRenderer,
    theme: &Theme,
    title: &str,
    count: Option<usize>,
) -> (f64, f64) {
    let (w, h) = text.measure_style(title, None, &title_style(theme));
    let mut width = f64::from(w);
    if let Some(count) = count {
        let (pill_w, _) = count_pill_size(text, theme, count);
        width += f64::from(theme.tokens.spacing.rhythm.related) + pill_w;
    }
    (width, f64::from(h))
}

fn count_pill_size(text: &mut TextRenderer, theme: &Theme, count: usize) -> (f64, f64) {
    let pad = f64::from(theme.tokens.spacing.s2);
    let (w, h) = text.measure_style(&count.to_string(), None, &count_style(theme));
    (f64::from(w) + 2.0 * pad, f64::from(h) + pad / 2.0)
}

/// Paint a header title in `band`: `Heading`, primary ink, vertically
/// centred and elided to the band; the count follows as a quiet pill in
/// `Caption` tabular figures (the `SectionHeader::count` look).
pub(crate) fn paint_title_in(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    band: Rect,
    title: &str,
    count: Option<usize>,
) {
    if band.width() <= 0.0 || band.height() <= 0.0 {
        return;
    }
    let related = f64::from(theme.tokens.spacing.rhythm.related);
    let pill = count.map(|count| (count, count_pill_size(text, theme, count)));
    let pill_room = pill.map_or(0.0, |(_, (w, _))| w + related);
    let style = title_style(theme);
    let room = (band.width() - pill_room).max(0.0);
    let shown = text.elide_to_width_style(title, f32_of_f64(room), &style);
    let (w, line_h) = text.measure_style(&shown, None, &style);
    let y = crate::text_metrics::centered_text_top(band.y0, band.height(), f64::from(line_h));
    text.draw_style(
        scene,
        &shown,
        theme.tokens.colors.text_primary,
        transform.pre_translate(Vec2::new(band.x0, y)),
        None,
        &style,
    );
    let Some((count, (pill_w, pill_h))) = pill else {
        return;
    };
    let x0 = band.x0 + f64::from(w) + related;
    if x0 + pill_w > band.x1 + 0.5 {
        return;
    }
    let rect = Rect::new(
        x0,
        band.center().y - pill_h / 2.0,
        x0 + pill_w,
        band.center().y + pill_h / 2.0,
    );
    crate::surface::card(
        scene,
        transform,
        rect,
        rect.height() / 2.0,
        theme.tokens.colors.surfaces.raised,
    );
    let label = count.to_string();
    let style = count_style(theme);
    let (lw, lh) = text.measure_style(&label, None, &style);
    text.draw_style(
        scene,
        &label,
        theme.text_dim,
        transform.pre_translate(Vec2::new(
            rect.center().x - f64::from(lw) / 2.0,
            rect.center().y - f64::from(lh) / 2.0,
        )),
        None,
        &style,
    );
}

/// Paint the window's History control in `rect`: the History glyph as a
/// Plain icon button, on (the selection lift) while History shows. The same
/// face `NavShell` and the window's own bar draw; a painted app routes its
/// presses through a [`crate::widget::HistoryOpener`].
pub fn paint_history_control(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    control: HistoryControl,
    hovered: bool,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let name = crate::i18n::I18nManager::lookup_plain("standalone-history-title");
    let face = crate::widget::ButtonFace::icon_only(rect, crate::icons::Icon::History, &name)
        .toggled(control.is_open())
        .hovered(hovered);
    crate::widget::Button::paint(scene, transform, text, theme, &face);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_face_is_the_shell_metrics() {
        let theme = Theme::dark();
        let area = Rect::new(0.0, 0.0, 1000.0, 700.0);
        let face = HeaderRowFace::new(area, &theme)
            .with_toggle()
            .with_back()
            .with_history();
        let m = face.metrics();
        assert_eq!(face.row.height(), m.height);
        assert!(m.height >= f64::from(theme.tokens.metrics.header_height));
        assert_eq!(face.toggle.x0, m.inset);
        assert_eq!(face.back.x0, face.toggle.x1 + m.gap);
        assert_eq!(face.history.x1, 1000.0 - m.inset);
        assert!(face.title.x0 > face.back.x1);
        assert!(face.title.x1 <= face.history.x0);
        assert_eq!(face.body_of(area).y0, m.height);
        assert_eq!(face.rule.y0, m.height);
        let plain = HeaderRowFace::new(area, &theme);
        assert_eq!(
            plain.title.x0, m.gutter,
            "a title with no leading button starts at the content gutter"
        );
        let (title, actions) = plain.split(200.0);
        assert_eq!(title.width(), 200.0);
        assert_eq!(actions.x0, title.x1 + m.gap);
    }

    #[test]
    fn titles_and_the_history_control_paint_from_tokens() {
        let theme = Theme::light();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let face = HeaderRowFace::new(Rect::new(0.0, 0.0, 600.0, 400.0), &theme).with_history();
        face.paint_rule(&mut scene, Affine::IDENTITY, &theme);
        face.paint_title(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            "Quarterly report",
            Some(12),
            None,
        );
        paint_history_control(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            face.history_rect(),
            HistoryControl::Open,
            false,
        );
        assert!(scene.encoding().draw_tags.len() >= 4);
        let (w, _) = measure_title(&mut text, &theme, "Quarterly report", Some(12));
        let (bare, _) = measure_title(&mut text, &theme, "Quarterly report", None);
        assert!(w > bare, "the count takes room after the title");
    }
}

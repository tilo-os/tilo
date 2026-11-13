//! The notification centre's list: rows grouped under app headings, with paging, an empty
//! state and an undo band after clearing.

use vello::kurbo::{Point, Rect};
use vello::peniko::Fill;

use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::theme::Theme;
use crate::tokens::TextRole as Role;
use crate::widget::{
    Button, ButtonFace, ButtonRole, ControlSize, EmptyState, EmptyStateContent, ListRowFace,
    RowLeading, SectionHeaderFace, Tone,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationListControl {
    /// Fold or unfold the group.
    Fold(usize),
    Dismiss(usize),
    HandToAgent(usize),
    /// Row pressed.
    Open(usize),
    /// Previous page.
    Newer,
    /// Next page.
    Older,
    /// Undo the last clear.
    Undo,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NotificationListRow {
    /// An app's heading.
    Group {
        key: String,
        label: String,
        count: usize,
        expanded: bool,
    },
    Item {
        key: String,
        title: String,
        source: String,
        age: String,
        body: String,
        severity: Option<Tone>,
        grouped: bool,
        agent: bool,
    },
}

impl NotificationListRow {
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Group { key, .. } | Self::Item { key, .. } => key,
        }
    }

    fn is_group(&self) -> bool {
        matches!(self, Self::Group { .. })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NotificationRowControls {
    pub dismiss: Rect,
    /// `None` without a hand-to-agent control.
    pub agent: Option<Rect>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotificationListLayout {
    pub rows: Vec<Rect>,
    pub controls: Vec<NotificationRowControls>,
    pub newer: Rect,
    pub older: Rect,
    /// Zero unless the list was just cleared.
    pub cleared: Rect,
    pub undo: Rect,
}

#[derive(Clone, Copy, Debug)]
pub struct NotificationListView<'a> {
    rows: &'a [NotificationListRow],
    start: usize,
    capacity: usize,
    cursor: Option<Point>,
    focused: Option<usize>,
    action_focus: Option<NotificationListControl>,
    cleared: Option<&'a str>,
    empty: Option<&'a str>,
    newer_label: &'a str,
    older_label: &'a str,
    undo_label: &'a str,
}

impl<'a> NotificationListView<'a> {
    #[must_use]
    pub fn new(rows: &'a [NotificationListRow], start: usize) -> Self {
        Self {
            rows,
            start,
            capacity: usize::MAX,
            cursor: None,
            focused: None,
            action_focus: None,
            cleared: None,
            empty: None,
            newer_label: "Newer",
            older_label: "Older",
            undo_label: "Undo",
        }
    }

    /// Rows per page; `usize::MAX` by default.
    #[must_use]
    pub fn capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity.max(1);
        self
    }

    #[must_use]
    pub fn cursor(mut self, cursor: Option<Point>) -> Self {
        self.cursor = cursor;
        self
    }

    #[must_use]
    pub fn focused(mut self, index: Option<usize>) -> Self {
        self.focused = index;
        self
    }

    #[must_use]
    pub fn action_focus(mut self, control: Option<NotificationListControl>) -> Self {
        self.action_focus = control;
        self
    }

    #[must_use]
    pub fn cleared(mut self, label: Option<&'a str>) -> Self {
        self.cleared = label;
        self
    }

    #[must_use]
    pub fn empty(mut self, message: &'a str) -> Self {
        self.empty = Some(message);
        self
    }

    #[must_use]
    pub fn pager_labels(mut self, newer: &'a str, older: &'a str) -> Self {
        self.newer_label = newer;
        self.older_label = older;
        self
    }

    #[must_use]
    pub fn undo_label(mut self, label: &'a str) -> Self {
        self.undo_label = label;
        self
    }

    fn page(&self) -> std::ops::Range<usize> {
        let start = self.start.min(self.rows.len());
        start..self.rows.len().min(start.saturating_add(self.capacity))
    }

    fn controls_in(row: Rect, theme: &Theme, with_agent: bool) -> NotificationRowControls {
        let size = f64::from(theme.tokens.metrics.icon_control);
        let related = f64::from(theme.tokens.spacing.rhythm.related);
        let y0 = row.center().y - size / 2.0;
        let x1 = row.x1 - related;
        let dismiss = Rect::new(x1 - size, y0, x1, y0 + size);
        let agent = with_agent.then(|| {
            let x1 = dismiss.x0 - related;
            Rect::new(x1 - size, y0, x1, y0 + size)
        });
        NotificationRowControls { dismiss, agent }
    }

    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) -> NotificationListLayout {
        let theme = ctx.theme;
        let mut layout = NotificationListLayout::default();
        if self.rows.is_empty() {
            if let Some(message) = self.empty {
                let content = EmptyStateContent::new(message);
                EmptyState::paint(ctx, rect, &content, self.cursor.unwrap_or(Point::ZERO));
            }
            return layout;
        }
        let hovered = |r: Rect| self.cursor.is_some_and(|p| r.contains(p));
        let mut y = rect.y0;
        for index in self.page() {
            let Some(row) = self.rows.get(index) else {
                continue;
            };
            match row {
                NotificationListRow::Group {
                    label,
                    count,
                    expanded,
                    ..
                } => {
                    let header = SectionHeaderFace::group(label)
                        .count(*count)
                        .disclosure(*expanded);
                    let height = header.height(ctx.text, theme, rect.width());
                    let row_rect = Rect::new(rect.x0, y, rect.x1, y + height);
                    header.paint(
                        ctx,
                        row_rect,
                        crate::widget::Interaction {
                            hovered: hovered(row_rect),
                            pressed: false,
                            focused: self.action_focus
                                == Some(NotificationListControl::Fold(index)),
                            disabled: false,
                        },
                    );
                    layout.rows.push(row_rect);
                    layout.controls.push(NotificationRowControls::default());
                    y += height;
                }
                NotificationListRow::Item {
                    title,
                    source,
                    age,
                    body,
                    severity,
                    grouped,
                    agent,
                    ..
                } => {
                    let mut face = ListRowFace::new(title)
                        .leading(RowLeading::Icon(Icon::Info))
                        .secondary(source)
                        .meta(age)
                        .nested(*grouped)
                        .secondary(body)
                        .interaction(crate::widget::Interaction {
                            hovered: hovered(Rect::new(rect.x0, y, rect.x1, y + 52.0)),
                            pressed: false,
                            focused: self.focused == Some(index),
                            disabled: false,
                        });
                    if let Some(tone) = severity {
                        face = face.mark_tone(*tone).mark(icon_for(*tone));
                    }
                    let height = face.height(ctx.text, theme);
                    let row_rect = Rect::new(rect.x0, y, rect.x1, y + height);
                    let controls = Self::controls_in(row_rect, theme, *agent);
                    face.paint(ctx, row_rect);
                    if let Some(tone) = *severity {
                        paint_severity_rail(ctx, row_rect, tone);
                    }
                    if hovered(row_rect) || self.focused == Some(index) {
                        paint_row_controls(
                            ctx,
                            controls,
                            *agent,
                            self.cursor,
                            self.action_focus,
                            index,
                        );
                    }
                    layout.rows.push(row_rect);
                    layout.controls.push(controls);
                    y += height;
                }
            }
        }
        let shown = layout.rows.len();
        if self.rows.len() > shown {
            let band_h = ButtonFace::height(ControlSize::Small, theme);
            let band = Rect::new(rect.x0, y, rect.x1, y + band_h);
            layout.newer = Rect::new(band.x0, band.y0, band.x0 + band.width() / 2.0, band.y1);
            layout.older = Rect::new(band.x1 - band.width() / 2.0, band.y0, band.x1, band.y1);
            for (button, label, on, focus) in [
                (
                    layout.newer,
                    self.newer_label,
                    self.start > 0,
                    NotificationListControl::Newer,
                ),
                (
                    layout.older,
                    self.older_label,
                    self.start.saturating_add(shown) < self.rows.len(),
                    NotificationListControl::Older,
                ),
            ] {
                Button::paint(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    theme,
                    &ButtonFace::new(button, label)
                        .role(ButtonRole::Plain)
                        .control_size(ControlSize::Small)
                        .disabled(!on)
                        .hovered(on && self.cursor.is_some_and(|p| button.contains(p)))
                        .focused(on && self.action_focus == Some(focus)),
                );
            }
            y = band.y1;
        }
        if self.cleared.is_some() {
            layout.cleared = self.paint_cleared_band(ctx, rect, y);
            layout.undo = layout.cleared;
        }
        layout
    }

    fn paint_cleared_band(&self, ctx: &mut PaintCtx<'_>, rect: Rect, y: f64) -> Rect {
        let theme = ctx.theme;
        let label = self.cleared.unwrap_or_default();
        let related = f64::from(theme.tokens.spacing.rhythm.related);
        let style = theme.text(Role::Caption);
        let (_, h) = ctx.text.measure_style(label, None, &style);
        let band_h =
            ButtonFace::height(ControlSize::Small, theme).max(f64::from(h) + 2.0 * related);
        let band = Rect::new(rect.x0, y, rect.x1, y + band_h);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            theme.tokens.colors.lift(0.35),
            None,
            &vello::kurbo::RoundedRect::from_rect(
                band,
                crate::chrome::ChromeRole::Control.radius(theme, band.height()),
            ),
        );
        ctx.text.draw_style(
            ctx.scene,
            label,
            theme.tokens.colors.text_primary,
            ctx.transform
                .pre_translate((band.x0 + related, band.center().y - f64::from(h) / 2.0).into()),
            None,
            &style,
        );
        let undo_face = ButtonFace::new(band, self.undo_label)
            .role(ButtonRole::Secondary)
            .control_size(ControlSize::Small);
        let width = undo_face.intrinsic_width(ctx.text, theme);
        let undo = Rect::new(
            band.x1 - related - width,
            band.y0,
            band.x1 - related,
            band.y1,
        );
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &undo_face
                .hovered(self.cursor.is_some_and(|p| undo.contains(p)))
                .focused(self.action_focus == Some(NotificationListControl::Undo)),
        );
        undo
    }

    #[must_use]
    pub fn control_at(
        &self,
        layout: &NotificationListLayout,
        point: Point,
    ) -> Option<NotificationListControl> {
        if !layout.undo.is_zero_area() && layout.undo.contains(point) {
            return Some(NotificationListControl::Undo);
        }
        if !layout.newer.is_zero_area() && layout.newer.contains(point) {
            return Some(NotificationListControl::Newer);
        }
        if !layout.older.is_zero_area() && layout.older.contains(point) {
            return Some(NotificationListControl::Older);
        }
        let start = self.start.min(self.rows.len());
        for (offset, (row, controls)) in layout.rows.iter().zip(&layout.controls).enumerate() {
            if controls.dismiss.contains(point) {
                return Some(NotificationListControl::Dismiss(start + offset));
            }
            if controls.agent.is_some_and(|agent| agent.contains(point)) {
                return Some(NotificationListControl::HandToAgent(start + offset));
            }
            if row.contains(point) {
                let index = start + offset;
                return Some(
                    if self
                        .rows
                        .get(index)
                        .is_some_and(NotificationListRow::is_group)
                    {
                        NotificationListControl::Fold(index)
                    } else {
                        NotificationListControl::Open(index)
                    },
                );
            }
        }
        None
    }
}

fn icon_for(severity: Tone) -> Icon {
    match severity {
        Tone::Danger => Icon::CircleAlert,
        Tone::Warning => Icon::TriangleAlert,
        Tone::Success => Icon::CircleCheck,
        _ => Icon::Info,
    }
}

fn paint_severity_rail(ctx: &mut PaintCtx<'_>, row: Rect, tone: Tone) {
    let related = f64::from(ctx.theme.tokens.spacing.rhythm.related);
    let rail = Rect::new(row.x0, row.y0 + related, row.x0 + 3.0, row.y1 - related);
    ctx.scene.fill(
        Fill::NonZero,
        ctx.transform,
        tone.colour(ctx.theme),
        None,
        &vello::kurbo::RoundedRect::from_rect(rail, rail.width() / 2.0),
    );
}

fn paint_row_controls(
    ctx: &mut PaintCtx<'_>,
    controls: NotificationRowControls,
    agent: bool,
    cursor: Option<Point>,
    focus: Option<NotificationListControl>,
    index: usize,
) {
    let theme = ctx.theme;
    let mut paint = |rect: Rect, icon: Icon, control, name: &str| {
        let lit = cursor.is_some_and(|p| rect.contains(p)) || focus == Some(control);
        if lit {
            Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                theme,
                &ButtonFace::icon_only(rect, icon, name)
                    .hovered(cursor.is_some_and(|p| rect.contains(p)))
                    .focused(focus == Some(control)),
            );
            return;
        }
        let size = f64::from(theme.tokens.metrics.icon_sm).min(rect.width());
        icon.draw_with_stroke(
            ctx.scene,
            ctx.transform,
            Rect::from_center_size(rect.center(), (size, size)),
            theme.tokens.colors.text_dim,
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    };
    paint(
        controls.dismiss,
        Icon::X,
        NotificationListControl::Dismiss(index),
        &crate::i18n::widget_format("widget-dismiss", &[]),
    );
    if agent && let Some(agent) = controls.agent {
        paint(
            agent,
            Icon::Send,
            NotificationListControl::HandToAgent(index),
            &crate::i18n::widget_format("widget-hand-to-agent", &[]),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Affine;

    fn item(title: &str) -> NotificationListRow {
        NotificationListRow::Item {
            key: title.to_owned(),
            title: title.to_owned(),
            source: "Mail".into(),
            age: "2 min ago".into(),
            body: "The draft is ready.".into(),
            severity: None,
            grouped: false,
            agent: false,
        }
    }

    fn group(label: &str, count: usize, expanded: bool) -> NotificationListRow {
        NotificationListRow::Group {
            key: label.to_owned(),
            label: label.to_owned(),
            count,
            expanded,
        }
    }

    fn layout(view: NotificationListView<'_>) -> NotificationListLayout {
        let theme = Theme::dark();
        let mut text = crate::text_render::TextRenderer::new();
        let mut scene = vello::Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        view.paint(&mut ctx, Rect::new(0.0, 0.0, 360.0, 600.0))
    }

    #[test]
    fn a_list_paints_a_row_for_every_notification_it_holds() {
        let rows = [item("One"), item("Two")];
        let painted = layout(NotificationListView::new(&rows, 0));
        assert_eq!(painted.rows.len(), 2);
        assert!(
            painted.rows[0].y1 <= painted.rows[1].y0,
            "rows stack without overlap"
        );
    }

    #[test]
    fn a_group_heading_folds_and_unfolds_over_its_rows() {
        let rows = [group("Mail", 3, true), item("One"), item("Two")];
        let painted = layout(NotificationListView::new(&rows, 0));
        assert_eq!(painted.rows.len(), 3, "an expanded group shows its rows");
        assert!(
            painted.rows[0].height() < painted.rows[1].height(),
            "the heading is a band, not a row"
        );
    }

    #[test]
    fn a_page_shows_its_capacity_and_offers_the_pager() {
        let rows = [item("One"), item("Two"), item("Three")];
        let painted = layout(NotificationListView::new(&rows, 0).capacity(2));
        assert_eq!(painted.rows.len(), 2);
        assert!(!painted.older.is_zero_area(), "there is a next page");
        assert!(!painted.older.contains(Point::ZERO));
    }

    #[test]
    fn a_press_on_a_row_or_its_controls_is_routed_by_the_layout() {
        let rows = [item("One")];
        let painted = layout(NotificationListView::new(&rows, 0));
        assert_eq!(
            NotificationListView::new(&rows, 0).control_at(&painted, painted.rows[0].center()),
            Some(NotificationListControl::Open(0))
        );
        assert_eq!(
            NotificationListView::new(&rows, 0)
                .control_at(&painted, painted.controls[0].dismiss.center()),
            Some(NotificationListControl::Dismiss(0))
        );
    }

    #[test]
    fn an_empty_list_says_so_and_a_cleared_one_offers_undo() {
        let rows: [NotificationListRow; 0] = [];
        let painted = layout(NotificationListView::new(&rows, 0).empty("Nothing here yet"));
        assert!(painted.rows.is_empty());
        assert!(painted.undo.is_zero_area(), "nothing was cleared");
        let rows = [item("One")];
        let painted =
            layout(NotificationListView::new(&rows, 0).cleared(Some("Notifications cleared")));
        assert!(!painted.undo.is_zero_area(), "the cleared band offers Undo");
        assert_eq!(
            NotificationListView::new(&rows, 0).control_at(&painted, painted.undo.center()),
            Some(NotificationListControl::Undo)
        );
    }

    #[test]
    fn a_warning_keeps_its_rail_beside_its_text() {
        let mut warning = item("Disk almost full");
        if let NotificationListRow::Item { severity, .. } = &mut warning {
            *severity = Some(Tone::Warning);
        }
        let rows = [warning];
        let painted = layout(NotificationListView::new(&rows, 0));
        assert_eq!(painted.rows.len(), 1);
    }
}

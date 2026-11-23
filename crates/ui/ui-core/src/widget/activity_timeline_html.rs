//! The history list as a page shows it: its filter chips, then a `<ul>` of days and bands with
//! their headings, and each change under it on one line as the canvas lays it: who made it, its
//! mark and what it did, its time at the trailing edge. A file of its own so the widget's source
//! stays a size a person reads whole.

use super::{ActivityTimeline, row_time};
use crate::TextRole;
use crate::composition::{HtmlCx, TextLook};
use crate::kurbo::Rect;

impl ActivityTimeline {
    /// The history as a page's own list: its filter chips, each day or band a heading, each
    /// change its author, its mark, what it did and its time, where the canvas puts them.
    pub(super) fn compose_html(&self, cx: &mut HtmlCx<'_>) {
        let theme = cx.theme();
        cx.element("ul");
        // The raised card the canvas draws the strip and the rows on, unless it stands on one.
        if self.draws_own_surface {
            let raised = theme.tokens.colors.surfaces.raised;
            cx.ground(crate::composition::Ground {
                fill: Some(crate::composition::rgba8(raised)),
                edge: Some(crate::composition::rgba8(crate::surface::edge_on(raised))),
                radius: f64::from(theme.tokens.radii.control),
                inset: [0.0; 2],
            });
        }
        let now = self.now();
        let caption = theme.text(TextRole::Caption);
        // The strip the canvas draws over the rows: each chip as drawn, pressed where it stands.
        let pill = (!self.author_pill_painted.is_zero_area()).then(|| {
            (
                self.author_pill_label(),
                self.author_pill_painted,
                !self.author_query.is_empty(),
            )
        });
        let chips = self
            .strip_chips
            .iter()
            .map(|chip| (chip.label.clone(), chip.painted, chip.selected))
            .chain(pill);
        for (at, (label, painted, selected)) in chips.enumerate() {
            let part = cx.part(&format!("filter:{at}"), "Chip", painted);
            part.tag = Some("button");
            part.focusable = true;
            part.painted_of(&(&label, selected));
            part.semantics = Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Button)
                    .with_name(label)
                    .with_state(crate::a11y::AccessState::new().selected(selected)),
            );
        }
        let heading = theme.text(TextRole::Subheading).strong();
        let viewport = self.rect;
        for index in 0..self.rows.len() {
            let Some(rect) = self.row_rect_unclipped(index) else {
                continue;
            };
            if rect.y1 <= viewport.y0 || rect.y0 >= viewport.y1 {
                continue;
            }
            let item = cx.part(&format!("row:{index}"), "ActivityRow", rect);
            item.tag = Some("li");
            if let Some(title) = self.group_title_at_row(index) {
                let line = f64::from(heading.line_height_px());
                let look = TextLook::new(title.to_owned(), &heading, Some(theme.text_primary))
                    .with_max_lines(Some(1));
                let part = item.part(
                    "title",
                    "Text",
                    Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + line),
                );
                part.tag = Some("h3");
                part.own = Some(crate::composition::Own::Text(look));
                if let Some(subtitle) = self.group_subtitle_at_row(index)
                    && !subtitle.is_empty()
                {
                    let look = TextLook::new(subtitle, &caption, Some(theme.text_dim))
                        .with_max_lines(Some(1));
                    let part = item.part(
                        "subtitle",
                        "Text",
                        Rect::new(rect.x0, rect.y0 + line, rect.x1, rect.y1),
                    );
                    part.tag = Some("p");
                    part.own = Some(crate::composition::Own::Text(look));
                }
                continue;
            }
            let Some(entry) = self.row_entry(index) else {
                continue;
            };
            let row = self.indented_row(index, rect);
            let cy = f64::midpoint(row.y0, row.y1);
            let (cluster, body, time) = crate::text_render::with_layout_renderer(theme, |text| {
                self.entry_row_columns(entry, text, theme, row, 0.0)
            });
            let line_h = f64::from(caption.line_height_px());
            let line =
                |x0: f64, x1: f64| Rect::new(x0, cy - line_h / 2.0, x1.max(x0), cy + line_h / 2.0);
            // Who made it: their face and the chip naming them, as drawn.
            if !entry.is_genesis() && !entry.author.name.is_empty() {
                let end = cluster.as_ref().map_or(body.x0, |cluster| cluster.chip.x1);
                let part = item.part(
                    "author",
                    "ActivityAuthor",
                    Rect::new(row.x0, row.y0, end, row.y1),
                );
                part.painted_of(&entry.author.name);
                part.semantics = Some(
                    crate::a11y::AccessNode::new(crate::a11y::Role::StaticText)
                        .with_name(entry.author.name.clone()),
                );
            }
            let (sentence, redacted) = self.row_sentence(entry, now);
            let icon = f64::from(theme.tokens.metrics.icon_sm);
            if icon <= body.width() {
                item.icon(
                    "mark",
                    Rect::new(body.x0, cy - icon / 2.0, body.x0 + icon, cy + icon / 2.0),
                    entry.entry_kind.icon(),
                    theme.tokens.colors.text_dim,
                );
            }
            let ink = if redacted {
                theme.tokens.colors.text_dim
            } else {
                theme.tokens.colors.text_primary
            };
            let look = TextLook::new(sentence, &caption, Some(ink)).with_max_lines(Some(1));
            let part = item.part(
                "body",
                "Text",
                line(body.x0 + f64::from(theme.tokens.spacing.s6), body.x1),
            );
            part.tag = Some("p");
            part.own = Some(crate::composition::Own::Text(look));
            let look = TextLook::new(row_time(entry, now), &caption, Some(theme.text_dim))
                .with_max_lines(Some(1))
                .aligned("right");
            let part = item.part("time", "Text", line(time.x0, time.x1));
            part.tag = Some("p");
            part.own = Some(crate::composition::Own::Text(look));
        }
    }
}

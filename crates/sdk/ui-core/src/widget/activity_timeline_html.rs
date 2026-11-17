//! The history list as a page shows it: a `<ul>` of days and bands with their headings, and each
//! change under it with its author, its time and what it did. A file of its own so the widget's
//! source stays a size a person reads whole.

use super::{ActivityTimeline, row_time};
use crate::TextRole;
use crate::composition::{HtmlCx, TextLook};

impl ActivityTimeline {
    /// The history as a page's own list: each day or band a heading, each change its author,
    /// its time and what it did.
    pub(super) fn compose_html(&self, cx: &mut HtmlCx<'_>) {
        let theme = cx.theme();
        cx.element("ul");
        let now = self.now();
        let body = theme.text(TextRole::Body);
        let caption = theme.text(TextRole::Caption);
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
                    crate::kurbo::Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + line),
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
                        crate::kurbo::Rect::new(rect.x0, rect.y0 + line, rect.x1, rect.y1),
                    );
                    part.tag = Some("p");
                    part.own = Some(crate::composition::Own::Text(look));
                }
                continue;
            }
            let Some(entry) = self.row_entry(index) else {
                continue;
            };
            let summary = self
                .descriptions
                .get(&entry.id)
                .cloned()
                .unwrap_or_else(|| entry.summary.clone());
            let who = entry.author.name.clone();
            let when = row_time(entry, now);
            let line = if who.is_empty() {
                when.clone()
            } else {
                format!("{who}, {when}")
            };
            let look = TextLook::new(line, &caption, Some(theme.text_dim)).with_max_lines(Some(1));
            let line_h = f64::from(caption.line_height_px());
            let part = item.part(
                "time",
                "Text",
                crate::kurbo::Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + line_h),
            );
            part.tag = Some("p");
            part.own = Some(crate::composition::Own::Text(look));
            let look = TextLook::new(summary, &body, Some(theme.text_primary));
            let part = item.part(
                "body",
                "Text",
                crate::kurbo::Rect::new(rect.x0, rect.y0 + line_h, rect.x1, rect.y1),
            );
            part.tag = Some("p");
            part.own = Some(crate::composition::Own::Text(look));
        }
    }
}

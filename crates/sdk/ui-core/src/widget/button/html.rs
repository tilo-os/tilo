//! A button's face as a page shows it ([`crate::composition::Face`]): what the canvas paints it with,
//! said as an icon, words and a ground.

use super::{Button, FaceMetrics, FacePaint, hint_ink};
use crate::theme::Theme;

impl FacePaint<'_> {
    pub(super) fn composed(&self, theme: &Theme) -> Option<crate::composition::Face> {
        use crate::composition::{Face, TextLook, rgba8};
        let content = &self.content;
        if content.image.is_some()
            || !content.faces.is_empty()
            || content.avatar.is_some()
            || content.badge.is_some()
        {
            return None;
        }
        let ((fill, edge, ink), _) = self.colors(theme);
        let ink = content.tone.map_or(ink, |tone| tone.colour(theme));
        let quiet = hint_ink(theme, self.role, ink, fill);
        let metrics = self.metrics;
        let icon_px = metrics.icon.min(self.rect.height().max(1.0));
        let label = content.label;
        let icon = match content.icon {
            Some(icon) => Some(icon.glyph(icon_px, ink, content.filled_icon)?),
            None if label.is_empty() => return None,
            None => None,
        };
        let words = |text: &str, color| {
            TextLook::new(text.to_owned(), &metrics.label_style, Some(color))
                .with_max_lines(Some(1))
        };
        let detail = content.detail.filter(|detail| !detail.is_empty());
        let visible =
            |color: vello::peniko::Color| (color.components[3] > 0.0).then(|| rgba8(color));
        Some(Face {
            icon,
            detail: detail
                .or(content.hint.as_deref())
                .filter(|_| !label.is_empty())
                .map(|text| words(text, quiet)),
            label: (!label.is_empty())
                .then(|| words(label, if content.placeholder { quiet } else { ink })),
            trailing: (content.menu && !label.is_empty())
                .then(|| crate::icons::Icon::ChevronDown.glyph(icon_px, ink, false))
                .flatten(),
            fill: visible(fill),
            edge: edge.and_then(visible),
            radius: f64::from(theme.tokens.radii.control),
            pad: if content.flush { 0.0 } else { metrics.pad_x },
            gap: metrics.icon_gap,
            spread: detail.is_some() && !label.is_empty(),
        })
    }
}

impl Button {
    pub(crate) fn composed_face(&self, theme: &Theme) -> Option<crate::composition::Face> {
        if self.transport.is_some() || self.count.is_some() {
            return None;
        }
        let metrics = FaceMetrics::of_role(self.control_size, self.role, theme);
        let (label, hint) = self.content();
        let moment = crate::part_paint::Moment::settled(crate::part_paint::States::REST);
        self.face_paint(&metrics, &label, hint, moment)
            .composed(theme)
    }
}

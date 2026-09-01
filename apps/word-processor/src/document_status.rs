//! Floating document statistics and file-status surface.

use std::time::Instant;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};

use ui::text_render::TextRenderer;
use ui::theme::Theme;

use crate::i18n::{arg, t, t_args};
use crate::num;

/// Document facts needed to compose the transient readout.
pub(crate) struct DocumentStatusView<'a> {
    pub(crate) words: usize,
    pub(crate) chars: usize,
    pub(crate) modified: bool,
    pub(crate) file_name: Option<&'a str>,
    pub(crate) format: Option<&'a str>,
}

/// Owns the floating readout's fade lifecycle, geometry, and drawing.
pub(crate) struct DocumentStatus {
    bubble: ui::StatusBubble,
    last_label: String,
    pinned: bool,
}

impl Default for DocumentStatus {
    fn default() -> Self {
        Self {
            bubble: ui::StatusBubble::new(),
            last_label: String::new(),
            pinned: false,
        }
    }
}

impl DocumentStatus {
    pub(crate) fn needs_frame(&self, now: Instant) -> bool {
        self.bubble.needs_frame(now)
    }

    pub(crate) fn pin_for_shot(&mut self, now: Instant) {
        self.pinned = true;
        self.bubble.poke(now);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
        bottom_inset: f64,
        pointer: Point,
        view: &DocumentStatusView<'_>,
    ) {
        let now = Instant::now();
        let label = label(view);
        if label != self.last_label {
            self.bubble.poke(now);
            self.last_label.clone_from(&label);
        }

        let text_size = 12.0;
        let (width, height) = ui::status_bubble::pill_size(&label, text, text_size, 0.0);
        let margin = 14.0;
        let anchor_bottom = window.y1 - bottom_inset - margin;
        let rect = Rect::new(
            window.x0 + margin,
            anchor_bottom - height,
            window.x0 + margin + width,
            anchor_bottom,
        );
        let reveal_zone = Rect::new(
            window.x0,
            rect.y0 - 24.0,
            rect.x1 + 40.0,
            window.y1 - bottom_inset,
        );
        self.bubble
            .set_hover(self.pinned || reveal_zone.contains(pointer), now);
        let alpha = self.bubble.alpha(now);
        if alpha <= 0.0 {
            return;
        }
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &rect.inflate(12.0, 14.0),
        );
        ui::status_bubble::draw_pill(scene, transform, text, theme, rect, &label, text_size);
        scene.pop_layer();
    }
}

fn label(view: &DocumentStatusView<'_>) -> String {
    let count = t_args(
        "status-count",
        &[
            arg("words", num::i64_of_usize(view.words)),
            arg("chars", num::i64_of_usize(view.chars)),
        ],
    );
    let untitled = t("status-untitled");
    let file_name = view.file_name.unwrap_or(&untitled);
    let modified = if view.modified { "●  " } else { "" };
    match view.format.filter(|format| !format.is_empty()) {
        Some(format) => format!("{count}    ·    {modified}{file_name}    ·    {format}"),
        None => format!("{count}    ·    {modified}{file_name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_distinguishes_saved_modified_and_untitled_documents() {
        let saved = label(&DocumentStatusView {
            words: 7,
            chars: 42,
            modified: false,
            file_name: Some("notes.odt"),
            format: Some("ODT"),
        });
        assert!(saved.contains("notes.odt"));
        assert!(saved.contains("ODT"));
        assert!(!saved.contains('●'));

        let modified = label(&DocumentStatusView {
            words: 0,
            chars: 0,
            modified: true,
            file_name: None,
            format: None,
        });
        assert!(modified.contains('●'));
        assert!(modified.contains(&t("status-untitled")));
    }
}

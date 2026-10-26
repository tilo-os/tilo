//! A location's label in the window bar, and the word that follows it: an
//! edited document's, or one its person may only look at.

use vello::Scene;
use vello::kurbo::Affine;

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;

/// What a location's label says, elided to its room, and the word an edited
/// document's label is followed by when it fits after it.
pub(super) struct LocationLabel {
    pub(super) line: String,
    pub(super) width: f64,
    pub(super) height: f64,
    /// The edited word and where it starts after the label's start.
    pub(super) edited: Option<(String, f64)>,
}

impl LocationLabel {
    /// `shown` in `room`: the edited word keeps its place and the label
    /// elides before it; a room too narrow for both shows the label alone.
    pub(super) fn measure(
        tr: &mut TextRenderer,
        theme: &Theme,
        shown: &str,
        word: Option<String>,
        room: f64,
    ) -> Self {
        let style = theme.text(TextRole::Body);
        let gap = f64::from(theme.tokens.spacing.s2);
        let mut edited = word.map(|word| {
            let (w, _) = tr.measure_style(&word, None, &style);
            (word, f64::from(w))
        });
        let label_room = match &edited {
            Some((_, w)) if room - w - gap >= *w => room - w - gap,
            _ => {
                edited = None;
                room
            }
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a location's width is a bounded on-screen size, far inside f32"
        )]
        let line = tr.elide_to_width_style(shown, label_room.max(0.0) as f32, &style);
        let (w, h) = tr.measure_style(&line, None, &style);
        let width = f64::from(w).min(label_room.max(0.0));
        let edited = edited.map(|(word, w)| (word, width + gap, w));
        Self {
            line,
            width: edited.as_ref().map_or(width, |(_, x, w)| x + w),
            height: f64::from(h),
            edited: edited.map(|(word, x, _)| (word, x)),
        }
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine transform composition over f64 screen coordinates"
    )]
    pub(super) fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        (tr, theme): (&mut TextRenderer, &Theme),
        (x, y): (f64, f64),
        ink: vello::peniko::Color,
    ) {
        let style = &theme.text(TextRole::Body);
        if !self.line.is_empty() {
            let origin = transform * Affine::translate((x, y));
            tr.draw_style(scene, &self.line, ink, origin, None, style);
        }
        if let Some((word, dx)) = &self.edited {
            let origin = transform * Affine::translate((x + dx, y));
            tr.draw_style(
                scene,
                word,
                theme.tokens.colors.text_tertiary,
                origin,
                None,
                style,
            );
        }
    }
}

/// The word that follows an edited document's label.
pub(super) fn edited_word() -> String {
    crate::i18n::I18nManager::lookup_plain("window-toolbar-edited")
}

/// The word that follows `location`'s label: "View only" where its person
/// may only look, which no edit of theirs can follow; else "Edited" for a
/// document with unsaved changes; else none.
pub(crate) fn trailing_word(location: &crate::window_toolbar::ToolbarLocation) -> Option<String> {
    if location.view_only {
        Some(crate::i18n::I18nManager::lookup_plain(
            "window-toolbar-view-only",
        ))
    } else {
        location.edited.then(edited_word)
    }
}

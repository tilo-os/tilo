//! A compact rich-text composer with the canonical editor and scroll viewport.
//!
//! Sources retain their typed child resource actions and input-method handling.
//! The measured editor, rather than a second plain-text estimate, determines
//! wrapping and growth. Short input is centred; long input remains scrollable.

use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::vello::kurbo::Rect;
use ui::widget::{Embedded, Scrollable, Widget};

use crate::{RichTextSource, RichTextSurface};

/// Shared composer slot geometry, independent of a message's resource type.
/// Actions retain their canonical Button bindings in the owning view. Slots
/// accept measured control widths, so desktop/mobile extras remain optional.
#[derive(Clone, Debug)]
pub struct ComposerFrame {
    pub editor: Rect,
    pub leading: Vec<Rect>,
    /// Ordered from the trailing edge inwards (Send before optional extras).
    pub trailing: Vec<Rect>,
}

impl ComposerFrame {
    pub fn layout(rect: Rect, theme: &Theme, leading: &[f64], trailing: &[f64]) -> Self {
        use ui::widget::{ButtonFace, ControlSize};
        let gap = f64::from(theme.tokens.spacing.s1);
        let height = ButtonFace::height(ControlSize::Regular, theme);
        let bottom = rect.y1 - gap;
        let top = bottom - height;
        let mut start = rect.x0 + gap;
        let leading: Vec<_> = leading
            .iter()
            .map(|width| {
                let slot = Rect::new(start, top, start + width, bottom);
                start = slot.x1 + gap;
                slot
            })
            .collect();
        if leading.is_empty() {
            start = rect.x0 + f64::from(theme.tokens.spacing.s3);
        }
        let mut end = rect.x1 - gap;
        let trailing = trailing
            .iter()
            .map(|width| {
                let slot = Rect::new(end - width, top, end, bottom);
                end = slot.x0 - gap;
                slot
            })
            .collect();
        Self {
            editor: Rect::new(start, rect.y0 + gap, end.max(start), rect.y1 - gap),
            leading,
            trailing,
        }
    }
}

/// A reusable embedded message, reply or annotation editor.
/// Configure placeholder, submit key and document source on the surface first.
/// The caller supplies the available rectangle and its surrounding actions.
pub struct RichTextComposer<S: RichTextSource + 'static> {
    host: Embedded<Scrollable>,
    source: PhantomData<S>,
    measured_height: f64,
    preserved_offset: Option<f32>,
    caret_in_content: Option<Rect>,
}

impl<S: RichTextSource + 'static> RichTextComposer<S> {
    pub fn new(surface: RichTextSurface<S>) -> Self {
        Self {
            host: Embedded::new(
                Scrollable::new_vertical()
                    .fit_content_height()
                    .with_focus_reveal()
                    .with_child(Box::new(surface)),
            ),
            source: PhantomData,
            measured_height: 0.0,
            preserved_offset: None,
            caret_in_content: None,
        }
    }

    /// The same typed editor source, retained when the viewport changes.
    ///
    /// # Panics
    /// Panics if the private viewport no longer retains its constructor-installed surface.
    #[expect(
        clippy::expect_used,
        reason = "the private viewport has exactly the typed surface installed by this constructor"
    )]
    pub fn widget(&self) -> &RichTextSurface<S> {
        self.host
            .widget()
            .children()
            .into_iter()
            .next()
            .and_then(|child| child.as_any())
            .and_then(|child| child.downcast_ref::<RichTextSurface<S>>())
            .expect("composer surface is retained")
    }

    /// Configure the source, rich-text marks, focus or submission semantics.
    ///
    /// # Panics
    /// Panics if the private viewport no longer retains its constructor-installed surface.
    #[expect(
        clippy::expect_used,
        reason = "the private viewport has exactly the typed surface installed by this constructor"
    )]
    pub fn widget_mut(&mut self) -> &mut RichTextSurface<S> {
        self.host
            .widget_mut()
            .children_mut()
            .into_iter()
            .next()
            .and_then(|child| child.as_any_mut())
            .and_then(|child| child.downcast_mut::<RichTextSurface<S>>())
            .expect("composer surface is retained")
    }

    /// Measure actual editor paragraphs at this width, capped to the available
    /// height. A caller adds its own control insets to this content height.
    pub fn measure(
        &mut self,
        width: f64,
        max_height: f32,
        theme: &Theme,
        text: &mut TextRenderer,
    ) -> f64 {
        self.preserved_offset
            .get_or_insert_with(|| self.host.widget().offset_y());
        self.host.widget_mut().set_max_height(max_height);
        self.measured_height =
            self.host
                .layout(Rect::new(0.0, 0.0, width.max(1.0), 0.0), theme, text);
        self.measured_height
    }

    /// Last measured editor height; no second text shaping is needed by chrome.
    pub fn measured_height(&self) -> f64 {
        self.measured_height
    }

    /// Centre a short editor in the available rectangle. Longer paragraphs
    /// fill it and use the shared viewport's scrolling and focus reveal.
    pub fn layout(&mut self, rect: Rect, theme: &Theme, text: &mut TextRenderer) -> f64 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "bounded logical viewport height narrows to Taffy's f32"
        )]
        let cap = rect.height().max(0.0) as f32;
        let height = self
            .measure(rect.width(), cap, theme, text)
            .min(rect.height());
        let top = rect.y0 + (rect.height() - height).max(0.0) / 2.0;
        let viewport = Rect::new(rect.x0, top, rect.x1, top + height);
        self.host.layout(viewport, theme, text);
        if let Some(offset) = self.preserved_offset.take() {
            let current = self.host.widget().offset_y();
            self.host.widget_mut().scroll_by(0.0, offset - current);
            self.host.layout(viewport, theme, text);
        }
        self.reveal_moved_caret(viewport, theme, text);
        height
    }

    /// Input methods use the editor's actual laid-out caret inside the viewport.
    pub fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(self.widget())
    }

    fn reveal_moved_caret(&mut self, viewport: Rect, theme: &Theme, text: &mut TextRenderer) {
        let caret = self.ime_cursor_rect().map(|caret| {
            let scroll = f64::from(self.host.widget().offset_y());
            Rect::new(
                caret.x0 - viewport.x0,
                caret.y0 - viewport.y0 + scroll,
                caret.x1 - viewport.x0,
                caret.y1 - viewport.y0 + scroll,
            )
        });
        if caret != self.caret_in_content
            && let Some(caret) = caret
        {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "measured logical caret coordinates narrow to the shared f32 scroll viewport"
            )]
            self.host.widget_mut().scroll_into_view(
                caret.x0 as f32,
                caret.y0 as f32,
                caret.width() as f32,
                caret.height() as f32,
            );
            self.host.layout(viewport, theme, text);
        }
        self.caret_in_content = caret;
    }
}

impl<S: RichTextSource + 'static> Deref for RichTextComposer<S> {
    type Target = Embedded<Scrollable>;
    fn deref(&self) -> &Self::Target {
        &self.host
    }
}

impl<S: RichTextSource + 'static> DerefMut for RichTextComposer<S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.host
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Draft, SubmitKey};
    use ui::widget::{BlockContent, CursorPlacement};

    fn composer(text: &str) -> RichTextComposer<Draft> {
        let mut surface = RichTextSurface::over(Draft::with_blocks(&[BlockContent::paragraph(
            "draft", text,
        )]))
        .with_editor(crate::compact_editor())
        .with_submit(SubmitKey::Enter);
        surface.present_now();
        RichTextComposer::new(surface)
    }

    #[test]
    fn one_line_composer_centres_actual_editor_at_both_text_scales() {
        let mut text = TextRenderer::new();
        for theme in [Theme::light(), Theme::dark().with_text_scale(2.0)] {
            let rect = Rect::new(20.0, 100.0, 340.0, 176.0);
            let mut composer = composer("A short reply");
            composer.layout(rect, &theme, &mut text);
            assert!(
                !composer.host.widget().vertical_overflows(),
                "a fully visible one-line draft must not advertise hidden vertical content: viewport {:?}, editor {:?}, measured {}, excess {}",
                composer.host.widget().content_viewport_rect(),
                composer.widget().editor().get_rect(),
                composer.measured_height(),
                composer.host.widget().max_offset_y()
            );
            let editor = composer.widget().editor().get_rect();
            assert!(
                (editor.center().y - rect.center().y).abs() < 0.5,
                "actual editor {editor:?}, available {rect:?}"
            );
            assert!(editor.y0 >= rect.y0 && editor.y1 <= rect.y1);
            composer
                .widget_mut()
                .editor_mut()
                .focus_block(0, CursorPlacement::End);
            assert!(
                composer
                    .ime_cursor_rect()
                    .is_some_and(|caret| rect.contains(caret.center())),
                "actual input-method caret stays in the composer"
            );
        }
    }

    #[test]
    fn multiline_composer_wraps_at_actual_width_and_clamps_its_viewport() {
        let mut text = TextRenderer::new();
        let theme = Theme::light();
        let mut composer = composer(&"A longer reply with spaces to wrap naturally. ".repeat(25));
        let wide = composer.measure(640.0, f32::INFINITY, &theme, &mut text);
        let narrow = composer.measure(220.0, f32::INFINITY, &theme, &mut text);
        assert!(
            narrow > wide,
            "actual narrow editor {narrow} must wrap more than {wide}"
        );
        let viewport = Rect::new(20.0, 100.0, 240.0, 180.0);
        composer.layout(viewport, &theme, &mut text);
        assert!(composer.host.widget().vertical_overflows());
        let shown = composer.host.widget().content_viewport_rect();
        assert!(
            shown.y0 >= viewport.y0 && shown.y1 <= viewport.y1,
            "scroll viewport {shown:?} stays within {viewport:?}"
        );
        let value = composer.widget().blocks()[0].text.clone();
        composer.host.widget_mut().scroll_by(0.0, 80.0);
        assert!(composer.host.widget().offset_y() > 0.0);
        composer.layout(viewport, &theme, &mut text);
        assert!(
            composer.host.widget().offset_y() > 0.0,
            "remeasuring an unchanged composer preserves deliberate manual scrolling"
        );
        assert_eq!(
            composer.widget().blocks()[0].text,
            value,
            "scrolling never edits the canonical rich-text source"
        );
    }
}

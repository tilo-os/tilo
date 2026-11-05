//! An activity's card on the floating layer: what the activity is doing, its
//! reading, its progress and the controls it offers, drawn in the floating
//! frame.
//!
//! The layer places the cards ([`crate::floating_layer`]) and the activity
//! describes itself ([`crate::live_activity::Summary`]); this is the one face
//! they are all drawn as, so every card in the layer looks and reads the same
//! wherever its app put it.

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

use crate::TextWeight;
use crate::floating_layer::{Parts, card_parts};
use crate::live_activity::{ControlKind, Summary, Tone};
use crate::num::f32_of_f64;
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{Button, ButtonFace, ButtonRole, ProgressBarFace};

/// An activity's card, ready to paint: what it says, what its own controls
/// are called out loud, and what the person is doing to it.
#[derive(Clone, Copy, Debug)]
pub struct ActivityCardFace<'a> {
    pub summary: &'a Summary,
    /// The collapse control's name, localised by the session that shows it.
    pub collapse_label: &'a str,
    /// The close control's name.
    pub close_label: &'a str,
    /// The card holds keyboard focus.
    pub focused: bool,
    /// One of the activity's offered controls is held down, by id.
    pub held: Option<&'a str>,
}

impl ActivityCardFace<'_> {
    /// Where the parts of the card stand in `rect`.
    #[must_use]
    pub fn parts(&self, rect: Rect) -> Parts {
        card_parts(rect, self.summary)
    }

    /// Paint the card in `rect`, returning where its parts landed.
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) -> Parts {
        let parts = card_parts(rect, self.summary);
        let summary = self.summary;
        let tone = tone_ink(summary.tone, theme);
        // The tone mark: a dot of the activity's own colour beside its name.
        scene.fill(
            Fill::NonZero,
            transform,
            tone,
            None,
            &RoundedRect::from_rect(parts.mark, parts.mark.height() / 2.0),
        );
        let title_style = theme.text(TextRole::Label);
        let title = text.elide_to_width_style(&summary.title, width(parts.title), &title_style);
        text.draw_style(
            scene,
            &title,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((parts.title.x0, parts.title.y0)),
            None,
            &title_style,
        );
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::icon_only(
                parts.collapse,
                crate::icons::Icon::ChevronDown,
                self.collapse_label,
            )
            .focused(self.focused),
        );
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::icon_only(parts.close, crate::icons::Icon::X, self.close_label),
        );
        if let Some(row) = parts.value {
            let style = theme.text(TextRole::Heading).with_weight(TextWeight::Bold);
            let value = text.elide_to_width_style(&summary.value, width(row), &style);
            text.draw_style(
                scene,
                &value,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((row.x0, row.y0)),
                None,
                &style,
            );
        }
        if let (Some(row), Some(progress)) = (parts.progress, summary.progress) {
            let mut ctx = PaintCtx::new(scene, transform, text, theme);
            ProgressBarFace::new(progress).paint(&mut ctx, row);
        }
        for (id, rect) in &parts.controls {
            let Some(control) = summary
                .action_controls()
                .into_iter()
                .find(|control| control.id == *id)
            else {
                continue;
            };
            let role = match control.kind {
                ControlKind::Primary => ButtonRole::Primary,
                ControlKind::Secondary | ControlKind::Close => ButtonRole::Secondary,
            };
            let style = theme.text(TextRole::Label);
            let label = text.elide_to_width_style(&control.label, width(*rect), &style);
            let held = self.held == Some(id.as_str());
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(*rect, &label).role(role).pressed(held),
            );
        }
        parts
    }
}

/// The ink an activity's tone reads in on a card: its own colour, or the
/// theme's warning when it asks for attention.
fn tone_ink(tone: Tone, theme: &Theme) -> Color {
    match tone {
        Tone::Neutral | Tone::Accent => theme.tokens.colors.intent.accent,
        Tone::Alert => theme.tokens.colors.semantic.danger,
    }
}

fn width(rect: Rect) -> f32 {
    f32_of_f64(rect.width().max(0.0))
}

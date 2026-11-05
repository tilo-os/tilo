//! An activity's card on the floating layer: which app it belongs to, what the
//! activity is doing, its reading, its progress and the controls it offers,
//! drawn in the floating frame.
//!
//! The layer places the cards ([`crate::floating_layer`]) and the activity
//! describes itself ([`crate::live_activity::Summary`]); this is the one face
//! they are all drawn as, so every card in the layer looks and reads the same
//! wherever its app put it.

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

use crate::floating_layer::{Parts, card_parts};
use crate::icons::Icon;
use crate::live_activity::{ControlKind, Summary, Tone};
use crate::num::f32_of_f64;
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{Button, ButtonFace, ButtonRole, ControlSize, ProgressBarFace};

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
        // The app's own glyph, in the activity's tone: a glance says whose
        // card this is.
        let glyph = Icon::from_name(summary.glyph())
            .or_else(|| Icon::from_name(summary.kind.glyph()))
            .unwrap_or(Icon::Layers);
        glyph.draw(scene, transform, parts.icon, tone_ink(summary.tone, theme));
        if let Some(line) = parts.title {
            let style = theme.text(TextRole::Label);
            let title = text.elide_to_width_style(&summary.title, width(line), &style);
            text.draw_style(
                scene,
                &title,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((line.x0, line.y0)),
                None,
                &style,
            );
        }
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::icon_only(parts.collapse, Icon::ChevronDown, self.collapse_label)
                .focused(self.focused),
        );
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::icon_only(parts.close, Icon::X, self.close_label),
        );
        // The hero: the countdown or reading, large and in tabular figures so
        // a ticking number keeps its width; a card with no reading leads with
        // its title instead.
        let hero = theme.text(TextRole::LargeTitle).numeric();
        let reading = if summary.value.is_empty() {
            &summary.title
        } else {
            &summary.value
        };
        let reading = text.elide_to_width_style(reading, width(parts.hero), &hero);
        text.draw_style(
            scene,
            &reading,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((parts.hero.x0, parts.hero.y0)),
            None,
            &hero,
        );
        if let Some(line) = parts.subtitle {
            let style = theme.text(TextRole::Body);
            let subtitle = text.elide_to_width_style(&summary.subtitle, width(line), &style);
            text.draw_style(
                scene,
                &subtitle,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((line.x0, line.y0)),
                None,
                &style,
            );
        }
        if let (Some(row), Some(progress)) = (parts.progress, summary.progress) {
            let mut ctx = PaintCtx::new(scene, transform, text, theme);
            ProgressBarFace::new(progress).paint(&mut ctx, row);
        }
        let offered = summary.action_controls();
        // The controls stand in a well of the card's content ground: a
        // secondary button is a step up from the content, and on the card's
        // own ground it would be no button at all.
        if let Some(row) = parts.controls_row {
            let radius = f64::from(theme.tokens.radii.control);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.content,
                None,
                &RoundedRect::from_rect(row, radius),
            );
        }
        for (id, rect) in &parts.controls {
            let Some(control) = offered.iter().find(|control| control.id == *id) else {
                continue;
            };
            let role = match control.kind {
                ControlKind::Primary => ButtonRole::Primary,
                ControlKind::Secondary | ControlKind::Close => ButtonRole::Secondary,
            };
            let held = self.held == Some(id.as_str());
            if let Some(icon) = Icon::from_name(&control.icon) {
                // A media control is its glyph; its label is still what it is
                // called out loud.
                Button::paint(
                    scene,
                    transform,
                    text,
                    theme,
                    &ButtonFace::new(*rect, "")
                        .icon(icon)
                        .accessible_name(&control.label)
                        .role(role)
                        .control_size(ControlSize::Small)
                        .pressed(held),
                );
            } else {
                let style = theme.text(TextRole::Label);
                let label = text.elide_to_width_style(&control.label, width(*rect), &style);
                Button::paint(
                    scene,
                    transform,
                    text,
                    theme,
                    &ButtonFace::new(*rect, &label)
                        .role(role)
                        .control_size(ControlSize::Small)
                        .pressed(held),
                );
            }
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

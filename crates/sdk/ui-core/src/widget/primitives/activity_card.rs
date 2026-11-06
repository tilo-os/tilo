//! An activity's card on the floating layer: which app it belongs to, what the
//! activity is doing, its reading, its progress and the controls it offers,
//! drawn in the floating frame.
//!
//! The layer places the cards ([`crate::floating_layer`]) and the activity
//! describes itself ([`crate::live_activity::Summary`]); this is the one face
//! they are all drawn as, so every card in the layer looks and reads the same
//! wherever its app put it.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::floating_layer::{Parts, card_parts};
use crate::icons::Icon;
use crate::live_activity::{Art, ControlKind, Summary};
use crate::num::f32_of_f64;
use crate::paint::PaintCtx;
use crate::pictures::Pictures;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::media_card::{MediaArt, paint_art_tile};
use crate::widget::{
    Button, ButtonFace, ButtonRole, ControlSize, PictureSource, ProgressBarFace, TransportFace,
};

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
    /// The person is dragging the song's progress bar: how far into it they
    /// point, from 0 to 1, until they let go.
    pub scrub: Option<f32>,
}

impl ActivityCardFace<'_> {
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
        glyph.draw(scene, transform, parts.icon, summary.tone.ink(theme));
        if let Some(line) = parts.title {
            let style = theme.text(TextRole::Label);
            let header = text.elide_to_width_style(summary.header_line(), width(line), &style);
            text.draw_style(
                scene,
                &header,
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
        let hero = if summary.art == Art::Hero {
            // The picture is what the card is for: its title stands under it,
            // the size of a heading rather than of a reading.
            theme.text(TextRole::Heading)
        } else {
            theme.text(TextRole::LargeTitle).numeric()
        };
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
        // The artwork a song heads its card with: its cover, or the kind's
        // glyph while the picture loads or when there is none to load.
        if let Some(rect) = parts.artwork {
            let radius = f64::from(theme.tokens.radii.control);
            let picture = PictureSource {
                src: summary.artwork.clone(),
            };
            if let Some(image) = Pictures::window().get(&picture) {
                crate::image_store::paint_image_cover(scene, transform, rect, &image, radius);
            } else {
                let kind = Icon::from_name(summary.glyph())
                    .or_else(|| Icon::from_name(summary.kind.glyph()))
                    .unwrap_or(Icon::Layers);
                let mut ctx = PaintCtx::new(scene, transform, text, theme);
                paint_art_tile(&mut ctx, rect, MediaArt::Kind(kind), &summary.title, radius);
            }
        }
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
            ProgressBarFace::new(self.scrub.unwrap_or(progress)).paint(&mut ctx, row);
        }
        // Where the song is and what is left of it, at the bar's ends.
        if let (Some(elapsed), Some(remaining)) = (parts.elapsed, parts.remaining) {
            let style = theme.text(TextRole::Caption);
            for (rect, seconds, align_end) in [
                (
                    elapsed,
                    summary.elapsed_seconds().unwrap_or_default(),
                    false,
                ),
                (
                    remaining,
                    summary.remaining_seconds().unwrap_or_default(),
                    true,
                ),
            ] {
                let label = crate::format::timecode(seconds);
                let (width, _) = text.measure_style(&label, None, &style);
                let x = if align_end {
                    rect.x1 - f64::from(width)
                } else {
                    rect.x0
                };
                text.draw_style(
                    scene,
                    &label,
                    theme.tokens.colors.text_dim,
                    transform * Affine::translate((x, rect.y0)),
                    None,
                    &style,
                );
            }
        }
        let offered = summary.action_controls();
        // A transport stands the way every media surface paints one: a round
        // play/pause in the accent, plain previous and next beside it, on the
        // card's own ground instead of in a well of their own.
        if let (Some(_), Some(steps)) = (parts.controls_row, summary.transport()) {
            for (id, step) in steps {
                let Some(rect) = parts
                    .controls
                    .iter()
                    .find(|(fitted, _)| fitted == id)
                    .map(|(_, rect)| *rect)
                else {
                    continue;
                };
                let face = TransportFace::new(rect, step).pressed(self.held == Some(id));
                TransportFace::paint(scene, transform, theme, &face);
            }
            return parts;
        }
        for (id, rect) in &parts.controls {
            let Some(control) = offered.iter().find(|control| control.id == *id) else {
                continue;
            };
            let role = match control.kind {
                ControlKind::Primary => ButtonRole::Primary,
                ControlKind::Secondary | ControlKind::Close => ButtonRole::Secondary,
                ControlKind::Destructive => ButtonRole::Destructive,
            };
            let held = self.held == Some(id.as_str());
            if let Some(icon) = Icon::from_name(&control.icon) {
                // A glyph leads its label where the slot is wide enough for
                // both, so a row of three says what each one does; a narrower
                // slot keeps the glyph, and its label is still what it is
                // called out loud.
                let style = theme.text(TextRole::Label);
                let fits = f64::from(text.measure_style(&control.label, None, &style).0) + 24.0
                    <= rect.width();
                let label = if fits { control.label.as_str() } else { "" };
                Button::paint(
                    scene,
                    transform,
                    text,
                    theme,
                    &ButtonFace::new(*rect, label)
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

fn width(rect: Rect) -> f32 {
    f32_of_f64(rect.width().max(0.0))
}

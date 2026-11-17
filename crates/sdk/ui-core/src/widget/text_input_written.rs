//! What someone else wrote into a field the person has not seen, marked in their colour where it
//! stands in the text, and their caret where it ends while they are still at work: the marks a
//! block of text shows of an agent's write ([`crate::widget::BlockEditor`]), in a field.
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::TextInput;
use crate::permissions::MemberId;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use automerge::{AutoCommit, ObjId};

impl TextInput {
    #[must_use]
    pub fn bound_at(mut self, locate: impl Fn(&AutoCommit) -> Option<ObjId> + 'static) -> Self {
        self.locate_text = Some(Box::new(locate));
        self
    }

    /// The bytes of what the field holds that differ from what it held
    /// before the change, as the text's own offsets: what the change wrote.
    /// An empty range is where it removed text. With nothing known of before,
    /// it wrote all of it.
    pub(super) fn written_span(&self) -> Option<(usize, usize)> {
        self.change.as_ref()?;
        let now = self.text.as_str();
        let Some(before) = self.change_before.as_deref() else {
            return Some((0, now.len()));
        };
        let start = now
            .char_indices()
            .zip(before.chars())
            .find(|((_, held), was)| held != was)
            .map_or_else(|| now.len().min(before.len()), |((at, _), _)| at);
        // What stands after the change on both sides, never reaching back
        // past where it starts in either text.
        let tail = now
            .get(start..)
            .unwrap_or_default()
            .chars()
            .rev()
            .zip(before.get(start..).unwrap_or_default().chars().rev())
            .take_while(|(held, was)| held == was)
            .map(|(held, _)| held.len_utf8())
            .sum::<usize>();
        Some((start, now.len().saturating_sub(tail)))
    }

    pub(super) fn draw_written(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        if self.holds_secret() {
            return;
        }
        let viewport = transform.transform_rect_bbox(self.rect);
        self.draw_invocations_written(scene, transform, text_renderer, theme, presence);
        let (Some((by, _)), Some((start, end))) = (&self.change, self.written_span()) else {
            return;
        };
        let Some(user) = by.identity.member_id() else {
            return;
        };
        if start < end {
            self.draw_authorship_bands(
                scene,
                transform,
                theme,
                presence,
                user,
                (start, end),
                viewport,
            );
        }
        let now = crate::time::now();
        let Some(acting) = presence
            .visible_interactions(now)
            .into_iter()
            .find(|interaction| interaction.actor.by == by.identity)
        else {
            return;
        };
        presence.note_shown(acting.invocation);
        presence.note_drawn(acting.invocation, now);
        let words = if self.accessible_name.trim().is_empty() {
            presence.interaction_status_label(&acting)
        } else {
            presence.interaction_status_label_doing(
                &acting,
                &crate::i18n::widget_format(
                    "interaction-editing-field",
                    &[("field", self.accessible_name.clone().into())],
                ),
            )
        };
        presence.draw_action_caret_labeled_in(
            scene,
            theme,
            text_renderer,
            transform,
            user,
            &words,
            crate::presence::LabeledCaret::within(self.caret_rect(theme, end), self.rect.y0).among(
                self.selection_rects(theme, 0, self.text.len()),
                self.rect.x1,
            ),
        );
    }
}

impl TextInput {
    /// Mark what a person just wrote in a field in their colour, inside what
    /// the field shows: a scrolled document's spans reach past the field, over
    /// the chrome below it.
    fn draw_authorship_bands(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
        author: &MemberId,
        (start, end): (usize, usize),
        viewport: Rect,
    ) {
        let bands: Vec<(Rect, f32)> = self
            .selection_rects(theme, start, end)
            .into_iter()
            .map(|band| transform.transform_rect_bbox(band))
            .filter(|band| band.overlaps(viewport))
            .map(|band| (band, 1.0))
            .collect();
        if bands.is_empty() {
            return;
        }
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            Affine::IDENTITY,
            &viewport,
        );
        presence.draw_peer_typing(scene, theme, author, &bands);
        scene.pop_layer();
    }

    fn draw_invocations_written(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        let Some((region, block)) = &self.edits else {
            return;
        };
        presence.draw_writes_in(
            &mut crate::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            (Some(region), block, &self.text),
            None,
            &mut |_, _, start, end| {
                (
                    self.selection_rects(theme, start, end),
                    Some(
                        crate::presence::LabeledCaret::within(
                            self.caret_rect(theme, end),
                            self.rect.y0,
                        )
                        .among(
                            self.selection_rects(theme, 0, self.text.len()),
                            self.rect.x1,
                        ),
                    ),
                )
            },
        );
    }
}

/// Where byte `at` of `old` stands in `new`, the one span where the two differ being someone
/// else's edit: before it, it stays; after it, it moves with the text; inside it, it lands at
/// the end of what they wrote.
pub(super) fn through_change(old: &str, new: &str, at: usize) -> usize {
    if old == new {
        return at;
    }
    let prefix = old
        .char_indices()
        .zip(new.chars())
        .find(|((_, a), b)| a != b)
        .map_or_else(|| old.len().min(new.len()), |((index, _), _)| index);
    let (old_rest, new_rest) = (
        old.get(prefix..).unwrap_or_default(),
        new.get(prefix..).unwrap_or_default(),
    );
    let suffix = old_rest
        .chars()
        .rev()
        .zip(new_rest.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum::<usize>();
    let (old_end, new_end) = (
        old.len().saturating_sub(suffix),
        new.len().saturating_sub(suffix),
    );
    if at <= prefix {
        at
    } else if at >= old_end {
        new_end.saturating_add(at.saturating_sub(old_end))
    } else {
        new_end
    }
}

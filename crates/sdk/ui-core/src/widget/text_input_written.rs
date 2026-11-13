//! What someone else wrote into a field the person has not seen, marked in
//! their colour where it stands in the text, and their caret where it ends
//! while they are still at work: the marks a block of text shows of an
//! agent's write ([`crate::widget::BlockEditor`]), in a field.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::TextInput;
use crate::permissions::UserId;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

impl TextInput {
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

    /// The change's characters in its author's colour, and, while that author
    /// is at work here (a visible action of theirs), their caret where the
    /// write ends, labelled with what they are doing. The caret is then the
    /// action's mark: the activity rail does not name it a second time.
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
        // The runs the text itself records as someone's, in that person's colour: the marks a
        // body's paragraphs carry, in a field.
        for (range, author) in &self.authored {
            if presence.is_own_identity(author) {
                continue;
            }
            let bands: Vec<Rect> = self
                .selection_rects(theme, range.start, range.end)
                .into_iter()
                .map(|band| transform.transform_rect_bbox(band))
                .collect();
            presence.draw_peer_authorship(scene, theme, &UserId(author.clone()), &bands);
        }
        let (Some((identity, _)), Some((start, end))) = (&self.change, self.written_span()) else {
            return;
        };
        let user = UserId(identity.clone());
        if start < end {
            let bands: Vec<Rect> = self
                .selection_rects(theme, start, end)
                .into_iter()
                .map(|band| transform.transform_rect_bbox(band))
                .collect();
            presence.draw_peer_authorship(scene, theme, &user, &bands);
        }
        let now = crate::time::now();
        let Some(acting) = presence
            .visible_interactions(now)
            .into_iter()
            .find(|interaction| interaction.actor.id == *identity)
        else {
            return;
        };
        presence.note_shown(acting.invocation);
        presence.note_drawn(acting.invocation, now);
        // What it is doing here is editing this field, whatever its action
        // is called: the field's own name says which.
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
            &user,
            &words,
            crate::presence::LabeledCaret::within(self.caret_rect(theme, end), self.rect.y0).among(
                self.selection_rects(theme, 0, self.text.len()),
                self.rect.x1,
            ),
        );
    }
}

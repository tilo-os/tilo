//! What an action wrote into a field, drawn where it landed: its characters in the writer's
//! colour, and the writer's caret at their end wearing what it did while it arrives
//! ([`crate::presence::PresenceCtx::draw_writes_in`]), as a block of text shows an agent's write
//! ([`crate::widget::BlockEditor`]).
use vello::Scene;
use vello::kurbo::Affine;

use super::TextInput;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use automerge::{AutoCommit, ObjId};

impl TextInput {
    #[must_use]
    pub fn bound_at(mut self, locate: impl Fn(&AutoCommit) -> Option<ObjId> + 'static) -> Self {
        self.locate_text = Some(Box::new(locate));
        self
    }

    pub(super) fn draw_written(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        let Some((region, block)) = self.edits.as_ref().filter(|_| !self.holds_secret()) else {
            return;
        };
        presence.draw_writes_in(
            &mut crate::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            (Some(region), block, &self.text),
            (
                self.typing.last_edit(),
                self.is_focused
                    .then(|| self.selection_anchor_focus_or_caret()),
            ),
            Some(self.accessible_name.as_str()),
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

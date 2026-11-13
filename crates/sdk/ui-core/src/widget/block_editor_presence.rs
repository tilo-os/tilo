//! What peers and agents do inside a block's text: the runs each author
//! wrote, a live typing glow, and an agent's write as the marks a person's
//! typing leaves.

use vello::kurbo::{Point, Rect};

use super::{Block, BlockDrawCtx, BlockEditor, transformed};
use crate::widget::Widget;

impl BlockEditor {
    /// Peer authorship runs, live-typing glows, what an invocation wrote and
    /// its caret, then carets and selection bands for one block: drawn after
    /// every block's own content, so they land on top.
    pub(super) fn draw_block_presence(
        &self,
        i: usize,
        b: &Block,
        document_selections: &[(
            usize,
            usize,
            &crate::permissions::UserId,
            crate::presence::SelectionRange,
        )],
        ctx: &mut BlockDrawCtx<'_>,
    ) {
        let key = self.presence_key_for(&b.id);
        let line = &b.line;
        let brect = line.get_rect();
        // A caret's name chip ends before the text's own right edge.
        let text_right = brect.x1 - line.text_inset(ctx.theme);
        // Drawn first, under caret/selection: the runs each foreign
        // author wrote, marker-style in their color, band by band as the
        // run wraps.
        for (bs, be, author) in line.author_runs() {
            if self.local_author.as_deref() == Some(author.as_str()) {
                continue;
            }
            let bands: Vec<Rect> = line
                .mark_rects(ctx.theme, *bs..*be)
                .into_iter()
                .map(|band| ctx.transform.transform_rect_bbox(band))
                .collect();
            let user = crate::permissions::UserId(author.clone());
            ctx.presence
                .draw_peer_authorship(ctx.scene, ctx.theme, &user, &bands);
        }
        // A peer's recently-inserted run glows and decays over
        // TYPING_FADE. Drawn over authorship, under caret/selection.
        let now = crate::time::now();
        for (user, range, alpha) in ctx.presence.typing_marks_for_key(&key, now) {
            if self.local_author.as_deref() == Some(user.0.as_str()) {
                continue;
            }
            let rect = line.selection_rect(ctx.theme, range.start, range.end);
            let p0 = transformed(ctx.transform, Point::new(rect.x0, rect.y0));
            let p1 = transformed(ctx.transform, Point::new(rect.x1, rect.y1));
            ctx.presence.draw_peer_typing(
                ctx.scene,
                ctx.theme,
                &user,
                &[(Rect::new(p0.x, p0.y, p1.x, p1.y), alpha)],
            );
        }
        // What an invocation writes here glows in its actor's colour, exactly
        // the characters it wrote, with its caret where they end: the marks a
        // person's typing leaves, from an action rather than from keys.
        // Whose action caret stands on this line: the one tag it wears says who and what, so their
        // own caret here draws no second one.
        let mut acting: Vec<crate::permissions::UserId> = Vec::new();
        for interaction in ctx.presence.visible_interactions(now) {
            let Some((start, end)) = written_in(&interaction, &b.id, line.value()) else {
                continue;
            };
            let Some(alpha) = ctx.presence.interaction_fade(interaction.invocation, now) else {
                continue;
            };
            let user = crate::permissions::UserId(interaction.actor.id.clone());
            acting.push(user.clone());
            let rect = line.selection_rect(ctx.theme, start, end);
            let p0 = transformed(ctx.transform, Point::new(rect.x0, rect.y0));
            let p1 = transformed(ctx.transform, Point::new(rect.x1, rect.y1));
            ctx.presence.draw_peer_typing(
                ctx.scene,
                ctx.theme,
                &user,
                &[(Rect::new(p0.x, p0.y, p1.x, p1.y), alpha)],
            );
            // The characters are the mark: the activity rail leaves this
            // invocation out rather than naming it a second time.
            ctx.presence.note_shown(interaction.invocation);
            ctx.presence.note_drawn(interaction.invocation, now);
            let name = ctx.presence.interaction_status_label_written(&interaction);
            ctx.presence.draw_action_caret_labeled_in(
                ctx.scene,
                ctx.theme,
                ctx.text_renderer,
                ctx.transform,
                &user,
                &name,
                crate::presence::LabeledCaret::within(
                    line.caret_rect(ctx.theme, end),
                    self.blocks
                        .first()
                        .map_or(brect.y0, |first| first.line.get_rect().y0),
                )
                .among(self.ink_around(i, ctx.theme), text_right),
            );
        }
        // A peer's caret arrives under the block's key, or under its digest when the key was too
        // long to travel whole ([`crate::presence::PresenceKey::digest_form`]).
        let digest = key.digest_form();
        let carets = ctx.presence.carets.get(&key).into_iter().chain(
            digest
                .as_ref()
                .and_then(|digest| ctx.presence.carets.get(digest)),
        );
        for list in carets {
            // What sits above the editor (a note's title and dates) is not
            // the editor's to cover: a chip on the first line hangs below.
            let top = self
                .blocks
                .first()
                .map_or(brect.y0, |first| first.line.get_rect().y0);
            for (user, byte_pos) in list.iter().filter(|(user, _)| !acting.contains(user)) {
                let name = ctx.presence.display_name(user);
                ctx.presence.draw_peer_caret_labeled_in(
                    ctx.scene,
                    ctx.theme,
                    ctx.text_renderer,
                    ctx.transform,
                    user,
                    &name,
                    crate::presence::LabeledCaret::within(
                        line.caret_rect(ctx.theme, *byte_pos),
                        top,
                    )
                    .among(self.ink_around(i, ctx.theme), text_right),
                );
            }
        }
        let selections = ctx.presence.selections.get(&key).into_iter().chain(
            digest
                .as_ref()
                .and_then(|digest| ctx.presence.selections.get(digest)),
        );
        for list in selections {
            for (user, range) in list {
                ctx.presence.draw_peer_selection(
                    ctx.scene,
                    ctx.theme,
                    ctx.transform,
                    user,
                    line.selection_rect(ctx.theme, range.start, range.end),
                );
            }
        }
        // Cross-block selection presence is sent once under a document
        // key, then expanded into the same per-line geometry locally.
        for &(start_block, end_block, user, range) in document_selections {
            if i < start_block || i > end_block {
                continue;
            }
            let start = if i == start_block { range.start } else { 0 };
            let end = if i == end_block {
                range.end
            } else {
                line.value().len()
            };
            ctx.presence.draw_peer_selection(
                ctx.scene,
                ctx.theme,
                ctx.transform,
                user,
                line.selection_rect(ctx.theme, start, end),
            );
        }
    }
}

impl BlockEditor {
    /// Where block `i` and the blocks either side of it have glyphs, one rect
    /// per visual line: what a peer's caret chip in block `i` must not cover.
    fn ink_around(&self, i: usize, theme: &crate::theme::Theme) -> Vec<Rect> {
        let last = self.blocks.len().saturating_sub(1);
        (i.saturating_sub(1)..=(i + 1).min(last))
            .filter_map(|j| self.blocks.get(j))
            .flat_map(|block| {
                let line = &block.line;
                line.mark_rects(theme, 0..line.value().len())
            })
            .filter(|glyphs| glyphs.width() > 0.0 && glyphs.height() > 0.0)
            .collect()
    }
}

/// The characters of the block `block` an invocation writes, as byte offsets
/// in `text`: the `Text` range its anchor names for that block, counted in
/// characters because a place counts them that way.
fn written_in(
    interaction: &tilo_actions::SemanticInteraction,
    block: &str,
    text: &str,
) -> Option<(usize, usize)> {
    let tilo_actions::InteractionAnchor::Text { ranges, .. } = &interaction.anchor else {
        return None;
    };
    let range = ranges.iter().find(|range| range.block == block)?;
    let chars = text.chars().count();
    let start = usize::try_from(range.start).ok()?.min(chars);
    let end = usize::try_from(range.end).ok()?.min(chars).max(start);
    let byte = |index: usize| {
        text.char_indices()
            .nth(index)
            .map_or(text.len(), |(byte, _)| byte)
    };
    Some((byte(start), byte(end)))
}

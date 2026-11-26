//! What peers and agents do inside a block's text: a live typing glow, and an agent's write as the
//! marks a person's typing leaves.

use vello::kurbo::{Point, Rect};

use super::{Block, BlockDrawCtx, BlockEditor, transformed};
use tilo_ui_base::widget::Widget;

impl BlockEditor {
    /// Live-typing glows, what an invocation wrote and its caret, then carets
    /// and selection bands for one block: drawn after every block's own
    /// content, so they land on top.
    pub(super) fn draw_block_presence(
        &self,
        i: usize,
        b: &Block,
        document_selections: &[(
            usize,
            usize,
            &tilo_ui_base::permissions::MemberId,
            tilo_ui_base::presence::SelectionRange,
        )],
        ctx: &mut BlockDrawCtx<'_>,
    ) {
        let key = self.presence_key_for(&b.id);
        let line = &b.line;
        let brect = line.get_rect();
        let text_right = brect.x1 - line.text_inset(ctx.theme);
        let now = ui::time::now();
        for (user, range, alpha) in ctx.presence.typing_marks_for_key(&key, now) {
            if self.local_author.as_ref() == Some(&user) {
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
        // person's typing leaves, from an action rather than from keys. An
        // agent's last write keeps a quiet wash until the person edits this
        // block or reaches into it.
        // Whose action caret stands on this line: the one tag it wears says who and what, so their
        // own caret here draws no second one.
        let top = self
            .blocks
            .first()
            .map_or(brect.y0, |first| first.line.get_rect().y0);
        let edited = self
            .edited_at
            .as_ref()
            .filter(|(edited, _)| *edited == b.id)
            .map(|(_, at)| *at);
        // Where the person's own caret or selection stands in this block: reaching into what an
        // agent wrote puts its mark away.
        let selected = self
            .document_selection()
            .filter(|_| self.is_focused)
            .map(|selection| selection.ordered())
            .filter(|(from, to)| (from.block..=to.block).contains(&i))
            .map(|(from, to)| {
                (
                    if from.block == i { from.byte } else { 0 },
                    if to.block == i {
                        to.byte
                    } else {
                        line.value().len()
                    },
                )
            })
            .or_else(|| {
                line.is_focused().then(|| {
                    line.selection_range()
                        .unwrap_or((line.cursor_pos(), line.cursor_pos()))
                })
            });
        let acting = ctx.presence.draw_writes_in(
            &mut tilo_ui_base::paint::PaintCtx::new(
                ctx.scene,
                ctx.transform,
                ctx.text_renderer,
                ctx.theme,
            ),
            (None, &b.id, line.value()),
            (edited, selected),
            None,
            &mut |_, interaction, start, end| {
                // One caret, where the write ends: a write over several paragraphs puts it on
                // the last of them.
                let later = self.blocks.get(i + 1..).is_some_and(|later| {
                    later.iter().any(|later| {
                        interaction
                            .written_in(None, &later.id, later.line.value())
                            .is_some()
                    })
                });
                (
                    vec![line.selection_rect(ctx.theme, start, end)],
                    (!later).then(|| {
                        tilo_ui_base::presence::LabeledCaret::within(
                            line.caret_rect(ctx.theme, end),
                            top,
                        )
                        .among(self.ink_around(i, ctx.theme), text_right)
                    }),
                )
            },
        );
        let digest = key.digest_form();
        let carets = ctx.presence.carets.get(&key).into_iter().chain(
            digest
                .as_ref()
                .and_then(|digest| ctx.presence.carets.get(digest)),
        );
        for list in carets {
            for (user, byte_pos) in list.iter().filter(|(user, _)| !acting.contains(user)) {
                let name = ctx.presence.display_name(user);
                ctx.presence.draw_peer_caret_labeled_in(
                    ctx.scene,
                    ctx.theme,
                    ctx.text_renderer,
                    ctx.transform,
                    user,
                    &name,
                    tilo_ui_base::presence::LabeledCaret::within(
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
    fn ink_around(&self, i: usize, theme: &tilo_ui_base::theme::Theme) -> Vec<Rect> {
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

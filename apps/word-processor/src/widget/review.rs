//! Review and collaboration: the comment column and its threads, and the
//! carets and selections of the other people in the document.

use crate::i18n::t;
use crate::num;
use crate::review_overlay::{ReviewAction, ReviewOverlay, ReviewPlacement};
use tilo_rich_text::{FieldOutcome, RichTextView};
use ui::PresenceCtx;
use ui::permissions::UserId;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::Widget;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::ToastKind;
use ui::widget::{Button, ButtonFace, ButtonRole, CardFace, ControlSize, Tone};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::{
    CARET_WIDTH, CtxAction, EditorEventCtx, FrameGeometry, Pass2Markers, RemoteCaretOverlay,
    RemoteSelectionOverlay, WordProcessorWidget, block_presence_key, is_scope_digest_key,
};

/// A comment's own text, its paragraphs joined.
fn thread_root_text(comment: &tilo_doc::snapshot::CommentView) -> String {
    comment
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Lay `view` out in `x0..x1` from `top` so its text, not the editor's
/// padding around it, starts at `x0` (the edge the author's name above it
/// starts at). Returns its height.
fn lay_out_flush(
    view: &mut RichTextView,
    (x0, x1, top): (f64, f64, f64),
    tr: &mut TextRenderer,
    theme: &Theme,
) -> f64 {
    let height = view.layout(Rect::new(x0, top, x1, top), theme, tr);
    let inset = view
        .editor_mut()
        .blocks()
        .first()
        .map_or(0.0, |block| block.line.caret_rect(theme, 0).x0 - x0);
    if inset > 0.5 {
        view.layout(Rect::new(x0 - inset, top, x1 + inset, top), theme, tr)
    } else {
        height
    }
}

impl WordProcessorWidget {
    /// The margin column beside the page: a card per comment and tracked
    /// change, and the open thread expanded in place of its comment's card.
    pub(super) fn draw_review_column(
        &mut self,
        scene: &mut Scene,
        (transform, fg): (Affine, &FrameGeometry),
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        markers: &Pass2Markers,
    ) {
        let to_window = self.zoom.transform(Affine::IDENTITY);
        let placement = ReviewPlacement {
            canvas: fg.canvas,
            page_x1: (to_window * Point::new(fg.page_x + fg.page_w, 0.0)).x,
            to_window,
        };
        let (_, width) = ReviewOverlay::column(theme, &placement);
        let thread = self
            .thread_comment
            .clone()
            .zip(self.comment_thread_height(width, tr, theme));
        self.review.draw_cards(
            &mut ui::paint::PaintCtx::new(scene, transform, tr, theme),
            &placement,
            (&markers.comment_markers, &markers.change_markers),
            thread.as_ref().map(|(id, height)| (id.as_str(), *height)),
        );
        if let Some(slot) = self.review.expanded_slot() {
            self.draw_comment_thread(scene, transform, tr, theme, presence, slot);
        } else {
            self.thread_panel = Rect::ZERO;
            self.thread_submit = Rect::ZERO;
            self.thread_cancel = Rect::ZERO;
        }
    }

    /// The comment whose thread is open, its replies in reading order.
    fn open_thread(&self) -> Option<tilo_doc::snapshot::CommentView> {
        let id = self.thread_comment.as_ref()?;
        let mut comment = self
            .review
            .comments()
            .iter()
            .find(|c| &c.id == id)
            .cloned()
            .or_else(|| self.thread_snapshot.clone())?;
        crate::review_overlay::sort_replies(&mut comment);
        Some(comment)
    }

    /// The open thread's card: its comment's card, expanded in place.
    fn thread_face(comment: &tilo_doc::snapshot::CommentView) -> CardFace<'_> {
        CardFace::new()
            .title(&comment.author)
            .leading_stripe(Tone::of_identity(&comment.author))
    }

    /// Lay out the open thread's card body from `top`, `x0..x1`: the
    /// comment, every reply shown (up to three) and the composer with its
    /// actions. Returns the body's height; positions the reply views and
    /// the composer.
    fn lay_out_thread_body(
        &mut self,
        comment: &tilo_doc::snapshot::CommentView,
        (x0, x1, top): (f64, f64, f64),
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let sp = &theme.tokens.spacing;
        let related = f64::from(sp.rhythm.related);
        let tight = f64::from(sp.rhythm.tight);
        let wrap = num::f32_of_f64((x1 - x0).max(1.0));
        let root = thread_root_text(comment);
        let mut y = top;
        y += f64::from(
            tr.measure_style(&root, Some(wrap), &theme.text(TextRole::Body))
                .1,
        );
        let replies: Vec<_> = comment.replies.iter().take(3).collect();
        self.thread_reply_views
            .resize_with(replies.len(), RichTextView::new);
        let who_style = theme.text(TextRole::Caption).strong();
        for (reply, view) in replies.iter().zip(&mut self.thread_reply_views) {
            y += related;
            y += f64::from(tr.measure_style(&reply.author, Some(wrap), &who_style).1) + tight;
            view.show_views(&reply.blocks);
            y += lay_out_flush(view, (x0, x1, y), tr, theme);
        }
        y += f64::from(sp.s3);
        // The composer stands in a field well, its actions under it.
        let field_h = ButtonFace::height(ControlSize::Regular, theme);
        y += field_h + f64::from(sp.s2) + ButtonFace::height(ControlSize::Small, theme);
        y - top
    }

    /// The open thread's height at `width`, the slot its comment's card
    /// gives up for it.
    fn comment_thread_height(
        &mut self,
        width: f64,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<f64> {
        let comment = self.open_thread()?;
        let face = Self::thread_face(&comment);
        let body = face.body_rect(tr, theme, Rect::new(0.0, 0.0, width, 1.0e4));
        let content = self.lay_out_thread_body(&comment, (body.x0, body.x1, body.y0), tr, theme);
        Some(face.height_for_body(tr, theme, width, content))
    }

    /// Draw the open thread in `slot`, where its comment's card stands in
    /// the margin: the same card, expanded with every reply and the reply
    /// composer, so the thread and the card are one object.
    fn draw_comment_thread(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        slot: Rect,
    ) {
        let Some(comment) = self.open_thread() else {
            self.close_comment_thread();
            return;
        };
        self.thread_panel = slot;
        let face = Self::thread_face(&comment);
        face.paint(
            &mut ui::paint::PaintCtx::new(scene, transform, tr, theme),
            slot,
        );
        let body = face.body_rect(tr, theme, slot);
        self.lay_out_thread_body(&comment, (body.x0, body.x1, body.y0), tr, theme);
        let sp = &theme.tokens.spacing;
        let tight = f64::from(sp.rhythm.tight);
        let colors = &theme.tokens.colors;
        let wrap = num::f32_of_f64(body.width().max(1.0));
        let body_style = theme.text(TextRole::Body);
        let who_style = theme.text(TextRole::Caption).strong();
        let root = thread_root_text(&comment);
        tr.draw_style(
            scene,
            &root,
            colors.text_primary,
            transform * Affine::translate((body.x0, body.y0)),
            Some(wrap),
            &body_style,
        );
        // Each reply: its author over its text, both from the body's edge.
        for (reply, view) in comment
            .replies
            .iter()
            .take(3)
            .zip(&mut self.thread_reply_views)
        {
            let who_h = f64::from(tr.measure_style(&reply.author, Some(wrap), &who_style).1);
            tr.draw_style(
                scene,
                &reply.author,
                colors.text_primary,
                transform * Affine::translate((body.x0, view.rect().y0 - tight - who_h)),
                Some(wrap),
                &who_style,
            );
            view.draw(scene, transform, tr, theme);
        }

        // The composer in its field well, then Cancel and Send (the card's
        // one primary action) along the card's foot.
        let btn_h = ButtonFace::height(ControlSize::Small, theme);
        let field_h = ButtonFace::height(ControlSize::Regular, theme);
        let btn_top = body.y1 - btn_h;
        let field_top = btn_top - f64::from(sp.s2) - field_h;
        let field = Rect::new(body.x0, field_top, body.x1, field_top + field_h);
        ui::surface::card(
            scene,
            transform,
            field,
            f64::from(theme.tokens.radii.control),
            colors.surfaces.raised,
        );
        let line = f64::from(tr.measure_style("Ag", None, &body_style).1);
        let editor_y = field.center().y - line / 2.0;
        self.thread_reply.layout(
            Rect::new(field.x0, editor_y, field.x1, editor_y + line),
            theme,
            tr,
        );
        self.thread_reply
            .draw(scene, transform, tr, theme, presence);
        let cancel_label = t("review-cancel-reply");
        let submit_label = t("review-send-reply");
        let submit_w = ButtonFace::new(Rect::ZERO, &submit_label)
            .role(ButtonRole::Primary)
            .control_size(ControlSize::Small)
            .intrinsic_width(tr, theme);
        let cancel_w = ButtonFace::new(Rect::ZERO, &cancel_label)
            .control_size(ControlSize::Small)
            .intrinsic_width(tr, theme);
        self.thread_submit = Rect::new(body.x1 - submit_w, btn_top, body.x1, btn_top + btn_h);
        let cancel_x1 = self.thread_submit.x0 - f64::from(sp.s2);
        self.thread_cancel = Rect::new(cancel_x1 - cancel_w, btn_top, cancel_x1, btn_top + btn_h);
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::new(self.thread_cancel, &cancel_label)
                .control_size(ControlSize::Small)
                .hovered(self.thread_cancel.contains(self.pointer)),
        );
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::new(self.thread_submit, &submit_label)
                .role(ButtonRole::Primary)
                .control_size(ControlSize::Small)
                .hovered(self.thread_submit.contains(self.pointer)),
        );
    }

    /// Resolve every remote collaborator's caret + selection against this
    /// frame's laid-out blocks, into content-space rectangles ready to paint
    /// through the page content transform.
    ///
    /// Factored out of [`Widget::draw`] so it's directly assertable (a populated
    /// `PresenceCtx` yields the expected rect; an empty one yields nothing). Looks
    /// up presence under the same [`block_presence_key`] the local caret is
    /// broadcast under, then maps offsets through the same `caret_geometry`/
    /// `selection_rects` the local caret uses — so remote and local are
    /// pixel-identical, just recoloured.
    ///
    /// Meaningful only after `self.laid`/`self.layout_cache` are populated by a
    /// `draw` pass.
    pub(crate) fn remote_presence_overlays(
        &self,
        presence: &PresenceCtx,
    ) -> (Vec<RemoteCaretOverlay>, Vec<RemoteSelectionOverlay>) {
        let mut carets = Vec::new();
        let mut selections = Vec::new();
        // LIVE_V1 files carets in blocks whose scoped anchor exceeds the wire
        // bound (e.g. 130-byte worker-minted ids) under a digest stand-in key;
        // resolve those against this widget's own blocks. Only hashed when
        // such a key is present.
        let digests = presence
            .carets
            .keys()
            .chain(presence.selections.keys())
            .any(is_scope_digest_key);
        for l in &self.laid {
            let Some((_, layout)) = self.layout_cache.get(&l.id) else {
                continue;
            };
            let exact = block_presence_key(&l.id);
            let digest = digests.then(|| exact.digest_form()).flatten();
            for key in std::iter::once(exact).chain(digest) {
                // Remote selections (drawn under the text, like the local one).
                if let Some(list) = presence.selections.get(&key) {
                    for (user, range) in list {
                        if range.start >= range.end {
                            continue;
                        }
                        let rects: Vec<Rect> = layout
                            .selection_rects(l.shown(range.start), l.shown(range.end))
                            .into_iter()
                            .map(|(rx0, ry0, rx1, ry1)| {
                                Rect::new(
                                    l.text_x + f64::from(rx0),
                                    l.y_top + f64::from(ry0),
                                    l.text_x + f64::from(rx1),
                                    l.y_top + f64::from(ry1),
                                )
                            })
                            .collect();
                        if !rects.is_empty() {
                            selections.push(RemoteSelectionOverlay {
                                user: user.clone(),
                                rects,
                            });
                        }
                    }
                }

                // Remote carets (drawn over the text, like the local one).
                if let Some(list) = presence.carets.get(&key) {
                    for (user, byte_pos) in list {
                        let (cx, cy, ch) = layout.caret_geometry(l.shown(*byte_pos));
                        let rect = Rect::new(
                            l.text_x + f64::from(cx),
                            l.y_top + f64::from(cy),
                            l.text_x + f64::from(cx) + CARET_WIDTH,
                            l.y_top + f64::from(cy) + f64::from(ch),
                        );
                        let label = presence.display_name(user);
                        carets.push(RemoteCaretOverlay {
                            user: user.clone(),
                            rect,
                            label,
                        });
                    }
                }
            }
        }
        (carets, selections)
    }

    /// A peer's just-typed runs, mapped onto this frame's laid-out blocks.
    ///
    /// `PresencePayload::Typing` names the span a peer just inserted; it decays
    /// over `TYPING_FADE` so the glow follows them through the document instead
    /// of accumulating. Resolved from the same `self.laid` + `self.layout_cache`
    /// the caret/selection overlays use, so all three can't drift apart.
    ///
    /// This is the *ephemeral* half of authorship — the durable half ("this
    /// paragraph is hers") is `tilo_doc::author_runs`, already fed to
    /// [`ui::presence::PresenceCtx::draw_peer_authorship`] by
    /// [`ui::widget::block_editor`]; this widget doesn't call it yet.
    pub(crate) fn remote_typing_overlays(
        &self,
        presence: &PresenceCtx,
        now: ui::time::Instant,
    ) -> Vec<(UserId, Vec<(Rect, f32)>)> {
        let mut out = Vec::new();
        for l in &self.laid {
            let Some((_, layout)) = self.layout_cache.get(&l.id) else {
                continue;
            };
            let key = block_presence_key(&l.id);
            for (user, range, alpha) in presence.typing_marks_for_key(&key, now) {
                if range.start >= range.end {
                    continue;
                }
                let spans: Vec<(Rect, f32)> = layout
                    .selection_rects(l.shown(range.start), l.shown(range.end))
                    .into_iter()
                    .map(|(rx0, ry0, rx1, ry1)| {
                        (
                            Rect::new(
                                l.text_x + f64::from(rx0),
                                l.y_top + f64::from(ry0),
                                l.text_x + f64::from(rx1),
                                l.y_top + f64::from(ry1),
                            ),
                            alpha,
                        )
                    })
                    .collect();
                if !spans.is_empty() {
                    out.push((user, spans));
                }
            }
        }
        out
    }

    /// Context-menu rows offering Accept/Reject for a tracked-change range or
    /// Resolve for a comment range under the click, plus Accept All / Reject
    /// All whenever the document has any tracked change. The clicked word
    /// span `[wstart, wend)` (falling back to `raw_click_off` for a
    /// whitespace click) is matched against `block`'s `ins`/`del`/`comment`
    /// marks. A no-op unless the review overlay is visible.
    pub(super) fn push_review_rows(
        &self,
        menu: &mut ContextMenu,
        actions: &mut Vec<Option<CtxAction>>,
        block: &str,
        wstart: usize,
        wend: usize,
        raw_click_off: usize,
    ) {
        if !self.review.is_visible() {
            return;
        }
        let mut review_rows = false;
        if let Some(b) = self.blocks.iter().find(|b| b.id == block) {
            for m in &b.marks {
                // The clicked word span `[wstart, wend)` must overlap the mark
                // (covers a click anywhere in the range, robust to the
                // word-snap). Fall back to the raw click point for whitespace.
                let overlaps = if wstart < wend {
                    wstart < m.end && m.start < wend
                } else {
                    raw_click_off >= m.start && raw_click_off < m.end
                };
                if !overlaps {
                    continue;
                }
                let Some(id) = m.value.as_str().map(std::string::ToString::to_string) else {
                    continue;
                };
                if m.name == tilo_doc::schema::MARK_INS || m.name == tilo_doc::schema::MARK_DEL {
                    let is_ins = m.name == tilo_doc::schema::MARK_INS;
                    let accept_key = if is_ins {
                        "review-accept-insertion"
                    } else {
                        "review-accept-deletion"
                    };
                    let reject_key = if is_ins {
                        "review-reject-insertion"
                    } else {
                        "review-reject-deletion"
                    };
                    menu.push(MenuItem::action(t(accept_key), || {}));
                    actions.push(Some(CtxAction::Review(ReviewAction::AcceptChange(
                        id.clone(),
                    ))));
                    menu.push(MenuItem::action(t(reject_key), || {}));
                    actions.push(Some(CtxAction::Review(ReviewAction::RejectChange(
                        id.clone(),
                    ))));
                    review_rows = true;
                } else if m.name == tilo_doc::schema::MARK_COMMENT {
                    menu.push(MenuItem::action(t("review-reply"), || {}));
                    actions.push(Some(CtxAction::Review(ReviewAction::OpenCommentThread(
                        id.clone(),
                    ))));
                    menu.push(MenuItem::action(t("review-resolve-comment"), || {}));
                    actions.push(Some(CtxAction::Review(ReviewAction::ResolveComment(
                        id.clone(),
                    ))));
                    review_rows = true;
                }
            }
        }
        if !self.review.changes().is_empty() {
            menu.push(MenuItem::action(t("review-accept-all"), || {}));
            actions.push(Some(CtxAction::Review(ReviewAction::AcceptAllChanges)));
            menu.push(MenuItem::action(t("review-reject-all"), || {}));
            actions.push(Some(CtxAction::Review(ReviewAction::RejectAllChanges)));
            review_rows = true;
        }
        if review_rows {
            menu.push(MenuItem::separator());
            actions.push(None);
        }
    }

    fn open_comment_thread(&mut self, comment: &str) {
        if !self.review.comments().iter().any(|item| item.id == comment) {
            self.toast
                .show(t("review-thread-unavailable"), ToastKind::Error);
            return;
        }
        self.thread_comment = Some(comment.to_string());
        self.thread_snapshot = None;
        self.thread_reply.widget_mut().source_mut().clear();
        self.thread_reply.widget_mut().present_now();
        self.focus_thread_reply(true);
        self.lifecycle.dirty = true;
    }

    pub(super) fn close_comment_thread(&mut self) {
        self.thread_comment = None;
        self.thread_snapshot = None;
        self.thread_panel = Rect::ZERO;
        self.thread_submit = Rect::ZERO;
        self.thread_cancel = Rect::ZERO;
        self.focus_thread_reply(false);
        self.lifecycle.dirty = true;
    }

    /// Put the caret in the reply field, or take it out.
    pub(super) fn focus_thread_reply(&mut self, focused: bool) {
        let surface = self.thread_reply.widget_mut();
        surface.set_focus(focused);
        if focused {
            surface
                .editor_mut()
                .focus_block(usize::MAX, ui::widget::CursorPlacement::End);
        }
    }

    fn submit_comment_reply(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some(comment) = self.thread_comment.clone() else {
            return;
        };
        let draft = self.thread_reply.widget().source();
        if draft.is_empty() {
            return;
        }
        self.perform_verb(
            state,
            crate::model::DocumentAction::ReplyToComment(crate::model::document::ReplyToComment {
                comment: ui::tilo_resource::Id::from_raw(comment),
                text: draft.plain_text().trim().to_owned(),
                body: Some(draft.clip_blocks()),
            }),
        );
        self.thread_reply.widget_mut().source_mut().clear();
        self.thread_reply.widget_mut().present_now();
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    pub(super) fn handle_comment_thread_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EditorEventCtx<'_>,
    ) -> bool {
        if self.thread_comment.is_none() {
            return false;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            self.route_thread_reply(event, ctx);
            return false;
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) {
            if self.thread_submit.contains(self.pointer) {
                self.submit_comment_reply(&mut ctx.state);
                *ctx.handled = true;
                return true;
            }
            if self.thread_cancel.contains(self.pointer) {
                self.close_comment_thread();
                *ctx.handled = true;
                return true;
            }
            if !self.thread_panel.contains(self.pointer) {
                self.close_comment_thread();
                *ctx.handled = true;
                return true;
            }
            if self.thread_reply.rect().contains(self.pointer) {
                self.focus_thread_reply(true);
            }
        }
        self.route_thread_reply(event, ctx);
        match self.thread_reply.widget_mut().take_outcome() {
            Some(FieldOutcome::Submit) => self.submit_comment_reply(&mut ctx.state),
            Some(FieldOutcome::Cancel) => self.close_comment_thread(),
            None => {}
        }
        *ctx.handled = true;
        true
    }

    /// Give the reply field one event and bring it up to date.
    fn route_thread_reply(&mut self, event: &WindowEvent, ctx: &mut EditorEventCtx<'_>) {
        let mut handled = *ctx.handled;
        self.thread_reply.handle_event(
            event,
            ctx.scale_factor,
            &ctx.modifiers,
            &mut ctx.state,
            ctx.path,
            &mut handled,
        );
        self.thread_reply.sync(ctx.state.doc(), ctx.path);
        self.lifecycle.dirty = true;
    }

    /// Act on a tracked-change / comment review affordance (from a margin-card
    /// button or a context-menu row): accept/reject a change, accept/reject all,
    /// or resolve (delete) a comment. Refreshes the materialized blocks +
    /// comments/changes so the acted-on item disappears, and flags an undo point.
    pub(super) fn dispatch_review_action(
        &mut self,
        action: ReviewAction,
        state: &mut ui::widget::Edits<'_>,
    ) {
        use crate::model::{DocumentAction, document};
        let verb = match action {
            ReviewAction::AcceptChange(change) => {
                DocumentAction::AcceptChange(document::AcceptChange { change })
            }
            ReviewAction::RejectChange(change) => {
                DocumentAction::RejectChange(document::RejectChange { change })
            }
            ReviewAction::AcceptAllChanges => DocumentAction::AcceptAllChanges(().into()),
            ReviewAction::RejectAllChanges => DocumentAction::RejectAllChanges(().into()),
            ReviewAction::ResolveComment(comment) => {
                DocumentAction::ResolveComment(document::ResolveComment {
                    comment: ui::tilo_resource::Id::from_raw(comment),
                })
            }
            ReviewAction::OpenCommentThread(id) => {
                self.open_comment_thread(&id);
                return;
            }
        };
        self.perform_verb(state, verb);
        self.refresh_blocks(state);
        // Accepting/rejecting can change body text (an accepted deletion removes
        // glyphs, a rejected insertion removes them); the spell cache is keyed by
        // text so affected blocks re-check on the next draw — clear it wholesale
        // (cheap, and these are infrequent review actions).
        self.spell_cache.clear();
        self.lifecycle.dirty = true;
    }
}

//! Editing through the document's actions: every change the editor makes is
//! an action on the shared document, answered against the current state and
//! mirrored into the local view.

use super::participants::note_participant_outcome;
use crate::WpFormat;
use crate::effects::WordProcessorEffect;
use crate::find_bar::Blocks;
use crate::i18n::t;
use crate::num;
use crate::word_processor::Caret;
use automerge::AutoCommit;
use std::collections::HashMap;
use std::path::PathBuf;
use tilo_doc::snapshot::{AttrVal, BlockView};
use ui::ParticipantPresence;
use ui::tilo_resource::Effect as _;
use ui::widget::toast::ToastKind;

use super::{
    BoundRows, WordProcessorWidget, block_presence_key, inline_mark, local_version, open_request,
    participant_presence_payload, save_request, to_spans,
};

impl WordProcessorWidget {
    /// Resolve the Find results of this window's view and of each
    /// participant's against `doc` as it is now.
    pub(super) fn resolve_bound_edits(&mut self, doc: &AutoCommit) {
        if self.read_only() {
            self.bound = BoundRows::default();
            return;
        }
        let local = self
            .wp_resource_ctx(doc)
            .map(|ctx| crate::actions::search_rows(&ctx, doc))
            .unwrap_or_default();
        let mut participants = HashMap::new();
        let known: Vec<_> = self.participants.participants().cloned().collect();
        for participant in known {
            let Some(ctx) = self
                .participant_view_or_default(&participant, doc)
                .and_then(|view| view.ctx(doc))
            else {
                continue;
            };
            participants.insert(participant, crate::actions::search_rows(&ctx, doc));
        }
        self.bound = BoundRows {
            local,
            participants,
        };
    }

    /// Adopt the view an edit effect left this window's person with, once
    /// the edit applied.
    pub(super) fn adopt_local_outcome(&mut self, state: &AutoCommit) {
        let Some((version, _)) = &self.pending_local else {
            return;
        };
        if *version == local_version(state) {
            return;
        }
        if let Some((_, next)) = self.pending_local.take() {
            let body = self.kept_body();
            let next = next.settle_in(state, body.as_deref());
            self.adopt_local_view(&next, state);
        }
    }

    /// Make the edit an edit effect names at the invoker's caret: a
    /// participant's own view when one invoked it, else this window's. The
    /// edit goes through the door as the invoker's, one undo step.
    fn answer_edit_effect(
        &mut self,
        effect: &WordProcessorEffect,
        cx: &mut ui::framework::EffectCx<crate::framework::Document, Self>,
    ) -> Option<ui::framework::EffectAnswer> {
        let doc = match cx.document() {
            Some(doc) => doc.clone(),
            None => return Some(Err("the document is not at hand".into())),
        };
        let participant = ui::invocation_scope::current_participant();
        // An image told the heading it goes after goes at that paragraph's
        // end, found by its text.
        let after = match effect {
            WordProcessorEffect::PlaceImage {
                after: Some(after), ..
            } if cx.place().is_some() => {
                return Some(Err(format!(
                    "give `at` or `after`, not both (`after` is `{after}`)"
                )));
            }
            WordProcessorEffect::PlaceImage {
                after: Some(after), ..
            } => match crate::actions::image_place::after_paragraph(&doc, after) {
                Ok(place) => Some(place),
                Err(error) => return Some(Err(error)),
            },
            _ => None,
        };
        // Told where to act, it acts there; else at the invoker's caret.
        let view = match (after.as_ref().or_else(|| cx.place()), &participant) {
            (Some(place), _) => {
                match crate::actions::participant::ParticipantView::at_place(&doc, place) {
                    Ok(view) => view,
                    Err(error) => return Some(Err(error)),
                }
            }
            (None, Some(participant)) => {
                match self.participant_view_or_default(participant, &doc) {
                    Some(view) => view,
                    None => {
                        return Some(Err("the document has no body to place a caret in".into()));
                    }
                }
            }
            (None, None) => self.local_edit_view(),
        };
        let dispatch = cx.dispatcher();
        if let WordProcessorEffect::CommentSelection { text, .. } = effect {
            return Some(
                crate::actions::editing::comment_range(&view, &doc)
                    .map(|range| {
                        dispatch.submit(crate::model::Document::ADD_COMMENT.with(
                            crate::model::document::AddComment {
                                block: range.block,
                                start: range.start,
                                end: range.end,
                                text: text.clone(),
                            },
                        ));
                        ui::framework::Effected::done()
                    })
                    .map_err(|error| error.to_string()),
            );
        }
        let built = effect.edit(&view, &doc, self.inner.content_store())?;
        let (envelopes, next) = match built {
            Ok(outcome) => outcome,
            Err(error) => return Some(Err(error.to_string())),
        };
        if envelopes.is_empty() {
            return Some(Err("there is nothing to change here".into()));
        }
        dispatch.group(|| {
            for op in &envelopes {
                dispatch.submit(crate::model::edit(op));
            }
        });
        match participant {
            Some(participant) => {
                self.participants.note_resolution(&participant, &doc);
                note_participant_outcome(&next);
            }
            None => self.pending_local = Some((local_version(&doc), next)),
        }
        self.lifecycle.dirty = true;
        // One that answers its caller does so with the blocks its edit
        // changed, once the door applied it.
        if answers_later(effect) {
            return Some(cx.when_landed(|_, landed, _| {
                // The blocks it changed; the ids it minted are among them.
                let answer = match landed.answer {
                    Some(serde_json::Value::Object(mut fields)) => {
                        fields.remove("changed").unwrap_or_default()
                    }
                    other => other.unwrap_or_default(),
                };
                Ok(ui::framework::Effected::value(answer))
            }));
        }
        Some(Ok(ui::framework::Effected::done()))
    }

    /// Bring the kept state up to date with `state`. Returns whether it
    /// changed.
    pub(super) fn refresh_live(&self, state: &AutoCommit) -> bool {
        self.source.as_ref().is_some_and(|live| {
            live.sync(state).unwrap_or_else(|error| {
                eprintln!("word-processor: document unreadable: {error}");
                false
            })
        })
    }

    pub(super) fn with_live<T>(
        &self,
        read: impl FnOnce(&tilo_document::RichTextState) -> T,
    ) -> Option<T> {
        self.source
            .as_ref()
            .and_then(|live| live.with(|document| document.text().map(read)))
    }

    /// Show the paragraphs `demand` names.
    pub(super) fn demand_live(&self, state: &AutoCommit, demand: ui::tilo_resource::Demand) {
        if let Some(live) = &self.source
            && let Err(error) = live.demand(state, demand)
        {
            eprintln!("word-processor: paragraphs unreadable: {error}");
        }
    }

    /// Dispatch one [`RichTextAction`](crate::actions::RichTextAction) —
    /// the single CRDT-mutation path shared with the Ctrl+K palette — through the
    /// Tilo action executor. Document heads before the event's first action are
    /// stashed so the host records **one** undo boundary bracketing the whole event.
    pub(super) fn perform_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        action: &crate::actions::RichTextAction,
    ) {
        let _ = self.try_perform_action(state, action);
    }

    pub(super) fn perform_edits(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        action: &crate::actions::RichTextAction,
    ) {
        let _ = self.try_perform_edits(state, action);
    }

    /// Submit one document action. `false` when the host refuses it, so a
    /// caller can leave the document and caret as they were.
    fn try_perform_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        action: &crate::actions::RichTextAction,
    ) -> bool {
        self.try_perform_edits(state, action)
    }

    fn try_perform_edits(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        action: &crate::actions::RichTextAction,
    ) -> bool {
        let done = self.try_perform_document(state, &crate::model::action(action));
        #[cfg(test)]
        if done {
            self.last_dispatched = Some(action.clone());
        }
        done
    }

    /// Take the document verb `operation` now, as this window's person.
    pub(super) fn perform_verb(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        operation: crate::model::DocumentAction,
    ) -> bool {
        self.perform_verb_as(state, operation, None)
    }

    /// Take the document verb `operation` now, as `author` when one is
    /// named (a scenario staging a collaborator's comment), else as this
    /// window's person.
    pub(super) fn perform_verb_as(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        operation: crate::model::DocumentAction,
        author: Option<&str>,
    ) -> bool {
        // A staged collaborator's verb is theirs in history.
        let _author = author.map(|author| {
            ui::invocation_scope::InvokerScope::enter(tilo_actions::ActionInvoker::actor(author))
        });
        let action = ui::tilo_resource::Stamped {
            operation,
            now_ms: ui::tilo_resource::origin_now_ms(),
        };
        self.try_perform_document(state, &action)
    }

    /// Submit one action of the document now. `false` when the host
    /// refuses it, so a caller can leave the document and caret as they
    /// were.
    fn try_perform_document(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        action: &<crate::model::Document as ui::tilo_resource::Resource>::Action,
    ) -> bool {
        let Some(dispatch) = self.dispatch.clone() else {
            return false;
        };
        match dispatch.apply_now(state, action) {
            Ok(()) => true,
            Err(error) => {
                if self.read_only() {
                    self.toast.show(t("toast-view-only"), ToastKind::Error);
                    self.lifecycle.dirty = true;
                }
                eprintln!("word-processor: edit refused: {error}");
                false
            }
        }
    }

    /// Persist only the changed fields of named styles through stable typed
    /// actions. This avoids replaying a stale whole-sheet snapshot over a
    /// collaborator's unrelated style edit and gives custom-style deletion an
    /// explicit tombstone plus absolute instance fallback set.
    pub(super) fn persist_stylesheet_via_actions(&mut self, state: &mut ui::widget::Edits<'_>) {
        let before = self.inner.stylesheet(state);
        let after = self.sheet.clone();
        for (style, properties) in &after.styles {
            let old = before.styles.get(style);
            for (key, value) in properties.to_attrs() {
                if old.is_some_and(|entry| entry.to_attrs().get(&key) == Some(&value)) {
                    continue;
                }
                let value = match value {
                    tilo_doc::AttrVal::Bool(value) => crate::actions::AttrVal::Bool(value),
                    tilo_doc::AttrVal::Int(value) => crate::actions::AttrVal::Int(value),
                    tilo_doc::AttrVal::Float(value) => crate::actions::AttrVal::Float(value),
                    tilo_doc::AttrVal::Str(value) => crate::actions::AttrVal::Str(value),
                };
                self.perform_action(
                    state,
                    &crate::actions::set_style_property_action(style, &key, value),
                );
            }
        }
        let removed: Vec<String> = before
            .styles
            .keys()
            .filter(|style| !after.styles.contains_key(*style))
            .cloned()
            .collect();
        if !removed.is_empty()
            && let Ok(containers) = crate::model::open(state)
        {
            let snapshot = tilo_doc::model::materialize(state, &containers);
            for style in removed {
                let instances = snapshot
                    .blocks
                    .iter()
                    .filter(|block| {
                        matches!(block.attrs.get("style"), Some(AttrVal::Str(id)) if id == &style)
                    })
                    .map(|block| block.id.clone())
                    .collect();
                self.perform_action(
                    state,
                    &crate::actions::delete_style_action(&style, instances),
                );
            }
        }
    }

    /// Replace one exact, already-resolved text range through the canonical
    /// action executor.  Find/replace, autocorrection, and ordinary typing all
    /// use this rather than opening a second CRDT write path in the widget.
    pub(super) fn splice_via_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        region: crate::actions::RegionKind,
        block: &str,
        start: usize,
        end: usize,
        text: &str,
    ) {
        self.perform_action(
            state,
            &crate::actions::splice_text_action(region, block, start, end, text),
        );
    }

    /// This window's view for an edit: [`Self::local_participant_view`]
    /// without the Find hits, which no editing primitive reads (keeps a
    /// keystroke from cloning them).
    pub(super) fn local_edit_view(&self) -> crate::actions::participant::ParticipantView {
        use crate::actions::participant::{ParticipantView, ViewPoint};
        let caret = self.inner.caret();
        ParticipantView {
            caret: ViewPoint::new(caret.block.clone(), caret.offset),
            anchor: self
                .inner
                .selection_anchor
                .as_ref()
                .map(|anchor| ViewPoint::new(anchor.block.clone(), anchor.offset)),
            region: self.inner.region_kind(),
            find: crate::actions::participant::FindState::default(),
            pending: None,
        }
    }

    /// Run one editor input through its pure `editing::*` primitive — the
    /// function Ctrl+K and agents invoke as a published operation — against
    /// this window's view, dispatch its envelopes and adopt its next view.
    ///
    /// A refusal (no caret, needs a selection, stale arguments) is a no-op.
    fn edit_via_primitive(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        primitive: impl FnOnce(
            &crate::actions::participant::ParticipantView,
            &AutoCommit,
        ) -> crate::actions::editing::EditResult,
    ) {
        let view = self.local_edit_view();
        let Ok((envelopes, next)) = primitive(&view, state) else {
            return;
        };
        for action in &envelopes {
            self.perform_edits(state, action);
        }
        // A caret waiting on a block the edit created settles on the
        // kept order, brought up to date, not on a listed body.
        let next = if next.pending.is_some() {
            self.sync_source(state);
            let body = self.kept_body();
            next.settle_in(state, body.as_deref())
        } else {
            next
        };
        self.adopt_local_view(&next, state);
    }

    /// Bring the kept state up to date with `state` in the middle of an edit;
    /// the next refresh still copies what changed into the views.
    pub(super) fn sync_source(&mut self, state: &AutoCommit) {
        self.source_unseen |= self.refresh_live(state);
    }

    /// Run a caret motion with the body order brought up to date with
    /// `state`, so it finds the caret's neighbours without listing the body.
    pub(super) fn with_kept_body<T>(
        &mut self,
        state: &AutoCommit,
        motion: impl FnOnce(&mut crate::word_processor::WordProcessor) -> T,
    ) -> T {
        self.sync_source(state);
        let body = self.kept_body();
        self.inner.with_kept_body(body, motion)
    }

    /// The kept body order, once there is kept state.
    pub(super) fn kept_body(&self) -> Option<std::rc::Rc<tilo_document::BodyOrder>> {
        self.with_live(|s| std::rc::Rc::clone(s.body()))
    }

    /// Make `next` (an editing primitive's result, settled against the
    /// document its envelopes produced) this window's caret and selection.
    /// The Find bar keeps its own state.
    pub(super) fn adopt_local_view(
        &mut self,
        next: &crate::actions::participant::ParticipantView,
        state: &AutoCommit,
    ) {
        let next = next.settle(state);
        if next.caret.block.is_empty() {
            return;
        }
        self.inner.set_caret(Caret {
            block: next.caret.block,
            offset: next.caret.offset,
        });
        self.inner.selection_anchor = next.anchor.map(|anchor| Caret {
            block: anchor.block,
            offset: anchor.offset,
        });
    }

    /// Apply a published view operation (`move-caret`, `select-*`,
    /// `find-text`) to this window: the same pure function a participant's
    /// invocation runs, adopted as the local caret/selection. `find-text`
    /// opens the Find bar on the query, as typing it there would.
    pub(crate) fn apply_local_view_operation(
        &mut self,
        state: &AutoCommit,
        movement: &crate::effects::ViewMove,
    ) -> Result<(), String> {
        let operation = match movement {
            crate::effects::ViewMove::Operation(operation) => operation,
            crate::effects::ViewMove::NavigateTo { block, start, end } => {
                self.navigate_to_range(state, block, *start, *end)?;
                self.resolve_bound_edits(state);
                return Ok(());
            }
        };
        if let crate::actions::view_ops::ViewOperation::FindText { query } = operation {
            if query.len() > crate::actions::editing::MAX_FIND_QUERY_BYTES {
                return Err("the Find query is too long".into());
            }
            let blocks = self.search_blocks(state);
            let effect = self.find.open(Some(query), &mut Blocks(&blocks));
            self.apply_find_effect(state, effect);
            return Ok(());
        }
        let view = self.local_edit_view();
        let (_, next) = crate::actions::view_ops::apply(&view, state, operation)
            .map_err(|error| error.to_string())?;
        self.adopt_local_view(&next, state);
        self.resolve_bound_edits(state);
        Ok(())
    }

    /// Move `participant`'s own view (its caret, selection or Find) as
    /// `movement` says. Nothing is written into the document.
    pub(crate) fn move_participant_view(
        &mut self,
        participant: &tilo_actions::ParticipantId,
        movement: &crate::effects::ViewMove,
        doc: &AutoCommit,
    ) -> Result<(), String> {
        let participant = participant.clone();
        let view = self
            .participant_view_or_default(&participant, doc)
            .ok_or("the document has no body to place a caret in")?;
        let outcome = match movement {
            crate::effects::ViewMove::Operation(operation) => {
                crate::actions::view_ops::apply(&view, doc, operation)
            }
            crate::effects::ViewMove::NavigateTo { block, start, end } => {
                crate::actions::view_ops::navigate_to(&view, doc, block, *start, *end)
            }
        };
        let (_, next) = outcome.map_err(|error| error.to_string())?;
        self.set_participant_view(&participant, &next, doc);
        Ok(())
    }

    /// Select the whole active region (Ctrl+A, context menu).
    pub(super) fn select_all_via_primitive(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.edit_via_primitive(state, crate::actions::editing::select_all);
    }

    /// Insert ordinary typed text through the same serializable actions exposed
    /// to agents and Ctrl+K.  A multi-block selection is a structural merge,
    /// represented by its own single envelope rather than a hidden UI-only
    /// sequence of splice/delete writes.
    pub(super) fn insert_text_via_action(&mut self, state: &mut ui::widget::Edits<'_>, text: &str) {
        self.insert_text_edits(state, text);
    }

    pub(super) fn insert_text_edits(&mut self, state: &mut ui::widget::Edits<'_>, text: &str) {
        self.edit_via_primitive(state, |view, doc| {
            crate::actions::editing::type_text(view, doc, text)
        });
    }

    /// Enter is a structural document edit, so split the addressed block through
    /// the same typed action stream as typing and formatting. The newly-created
    /// stable id is discovered from the synchronous before/after set solely to
    /// update the local caret; it is minted by the action's pinned actor.
    pub(super) fn enter_via_action(&mut self, state: &mut ui::widget::Edits<'_>) {
        // The paragraph that follows the caret is read off the kept order.
        self.sync_source(state);
        let body = self.kept_body();
        self.edit_via_primitive(state, |view, doc| {
            crate::actions::editing::new_paragraph_in(view, doc, body.as_deref())
        });
    }

    pub(super) fn page_break_via_action(&mut self, state: &mut ui::widget::Edits<'_>) {
        if self.inner.has_selection() {
            self.insert_text_via_action(state, "");
        }
        let caret = self.inner.caret().clone();
        let region = self.inner.region_kind();
        let before: std::collections::HashSet<String> =
            self.inner.ordered_blocks(state).into_iter().collect();
        self.perform_action(
            state,
            &crate::actions::insert_page_break_in_action(region, &caret.block, caret.offset),
        );
        if let Some(new_block) = self
            .inner
            .ordered_blocks(state)
            .into_iter()
            .find(|id| !before.contains(id))
        {
            self.inner.set_caret(Caret {
                block: new_block,
                offset: 0,
            });
        }
        self.inner.clear_anchor();
    }

    pub(super) fn replace_body_via_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        markdown: &str,
    ) {
        self.perform_action(state, &crate::actions::replace_body_action(markdown));
        if let Some(first) = self.inner.ordered_blocks(state).into_iter().next() {
            self.inner.set_caret(Caret {
                block: first,
                offset: 0,
            });
        }
        self.inner.clear_anchor();
    }

    pub(super) fn insert_page_number_via_action(&mut self, state: &mut ui::widget::Edits<'_>) {
        if self.inner.has_selection() {
            self.insert_text_via_action(state, "");
        }
        let caret = self.inner.caret().clone();
        self.perform_action(
            state,
            &crate::actions::insert_page_number_action(
                self.inner.region_kind(),
                &caret.block,
                caret.offset,
            ),
        );
        self.inner.set_caret(Caret {
            block: caret.block,
            offset: caret.offset.saturating_add(1),
        });
        self.inner.clear_anchor();
    }

    pub(super) fn paste_fragment_via_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        blocks: Vec<crate::word_processor::ClipBlock>,
    ) {
        let caret = self.inner.caret().clone();
        let (start, end) = self
            .inner
            .selection_span(state)
            .unwrap_or_else(|| (caret.clone(), caret));
        let start_prefix_empty = start.offset == 0;
        let end_len = self
            .blocks
            .iter()
            .find(|block| block.id == end.block)
            .map_or(end.offset, |block| block.text.chars().count());
        let adopt_first_style = start_prefix_empty && end.offset == end_len;
        let before: std::collections::HashSet<String> =
            self.inner.ordered_blocks(state).into_iter().collect();
        let final_offset = blocks.last().map_or(0, |block| block.text.chars().count());
        let one_block = blocks.len() == 1;
        self.perform_action(
            state,
            &crate::actions::paste_fragment_action(
                self.inner.region_kind(),
                crate::actions::SelectionTarget {
                    start_block: start.block.clone(),
                    start: start.offset,
                    end_block: end.block.clone(),
                    end: end.offset,
                },
                blocks,
                adopt_first_style,
            ),
        );
        let new_last = self
            .inner
            .ordered_blocks(state)
            .into_iter()
            .rfind(|id| !before.contains(id));
        self.inner.set_caret(Caret {
            block: if one_block {
                start.block
            } else {
                new_last.unwrap_or(start.block)
            },
            offset: if one_block {
                start.offset.saturating_add(final_offset)
            } else {
                final_offset
            },
        });
        self.inner.clear_anchor();
    }

    /// Backspace/Delete run the published delete operation.
    pub(super) fn delete_via_action(&mut self, state: &mut ui::widget::Edits<'_>, backward: bool) {
        let direction = if backward {
            crate::actions::editing::DeleteDirection::Backward
        } else {
            crate::actions::editing::DeleteDirection::Forward
        };
        self.edit_via_primitive(state, |view, doc| {
            crate::actions::editing::delete(view, doc, direction, 1)
        });
    }

    /// The context Ctrl+K needs to build the document's resource tree: the caret
    /// anchor for structural inserts and the blocks a block-level format targets.
    /// `None` until the document is bound (a fresh, unsynced widget).
    pub(crate) fn wp_resource_ctx(
        &self,
        doc: &AutoCommit,
    ) -> Option<crate::actions::WpResourceCtx> {
        let caret = self.inner.caret();
        if caret.block.is_empty() {
            return None;
        }
        // The caret block is the alignment target for a palette-initiated block
        // format (the palette runs between frames with no live selection).
        let target = if self.blocks.iter().any(|b| b.id == caret.block) {
            vec![caret.block.clone()]
        } else {
            Vec::new()
        };
        Some(crate::actions::WpResourceCtx {
            caret_block: caret.block.clone(),
            caret_offset: caret.offset,
            target_blocks: if target.is_empty() {
                vec![caret.block.clone()]
            } else {
                target
            },
            format_spans: to_spans(
                self.selection_blocks_cached()
                    .into_iter()
                    .filter(|(_, start, end)| start < end)
                    .collect(),
            ),
            selection: self.inner.selection_span(doc).map(|(start, end)| {
                crate::actions::SelectionTarget {
                    start_block: start.block,
                    start: start.offset,
                    end_block: end.block,
                    end: end.offset,
                }
            }),
            region: self.inner.region_kind(),
            search_hits: self
                .find
                .matches()
                .iter()
                .take(crate::actions::MAX_RESOURCE_SEARCH_HITS)
                .map(|(block, start, end)| crate::actions::SearchTarget {
                    block: block.clone(),
                    start: *start,
                    end: *end,
                })
                .collect(),
            content: self.inner.content_store().clone(),
        })
    }

    /// This window's own view as a participant view: the caret, the
    /// selection anchor, the active region and the Find bar's query and hits.
    /// `Self::wp_resource_ctx` is its projection (computed from the render
    /// cache so it stays cheap per frame); `view.ctx(doc)` yields the same
    /// context.
    pub fn local_participant_view(&self) -> crate::actions::participant::ParticipantView {
        use crate::actions::participant::{FindState, ParticipantView, ViewPoint};
        let caret = self.inner.caret();
        ParticipantView {
            caret: ViewPoint::new(caret.block.clone(), caret.offset),
            anchor: self
                .inner
                .selection_anchor
                .as_ref()
                .map(|anchor| ViewPoint::new(anchor.block.clone(), anchor.offset)),
            region: self.inner.region_kind(),
            find: FindState::new(
                self.find.query(),
                self.find.matches().iter().map(|(block, start, end)| {
                    crate::actions::SearchTarget {
                        block: block.clone(),
                        start: *start,
                        end: *end,
                    }
                }),
            ),
            pending: None,
        }
    }

    /// `participant`'s current view, rebased onto `doc`. `None` for a
    /// participant this widget holds no view for.
    pub fn participant_view(
        &self,
        participant: &tilo_actions::ParticipantId,
        doc: &AutoCommit,
    ) -> Option<crate::actions::participant::ParticipantView> {
        self.participants.view(participant, doc)
    }

    /// The view an invocation by `participant` resolves against: its stored
    /// view, or — for a participant without one yet — a collapsed caret at the
    /// end of the body (an agent's spawn position). `None` only for a document
    /// without body blocks.
    pub fn participant_view_or_default(
        &self,
        participant: &tilo_actions::ParticipantId,
        doc: &AutoCommit,
    ) -> Option<crate::actions::participant::ParticipantView> {
        self.participants
            .view(participant, doc)
            .or_else(|| crate::actions::participant::ParticipantView::at_document_end(doc))
    }

    /// Store `participant`'s view, valid against `doc` as it is now (after
    /// its envelopes were applied). Presentation state only: nothing is
    /// written into the document. The map stays bounded (LRU, idle TTL).
    pub fn set_participant_view(
        &mut self,
        participant: &tilo_actions::ParticipantId,
        view: &crate::actions::participant::ParticipantView,
        doc: &AutoCommit,
    ) {
        self.participants
            .set(participant, view, doc, ui::time::now());
        self.resolve_bound_edits(doc);
        self.lifecycle.dirty = true;
    }

    /// The name painted next to `participant`'s caret (e.g. "Agent for Ada").
    pub fn set_participant_label(
        &mut self,
        participant: &tilo_actions::ParticipantId,
        label: &str,
    ) {
        self.participants.set_label(participant, label);
    }

    /// The Find results `participant` sees, as resources: this window's
    /// for `None`, else built from that participant's own view only, so no
    /// other participant's hits leak into it.
    #[cfg(test)]
    pub(crate) fn participant_resources(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::Resource> {
        let ctx = match participant {
            None => self.wp_resource_ctx(doc),
            Some(participant) => self
                .participant_view_or_default(participant, doc)
                .and_then(|view| view.ctx(doc)),
        };
        ctx.map(|ctx| {
            crate::actions::search_rows(&ctx, doc)
                .into_iter()
                .map(ui::framework::LiveRow::into_resource)
                .collect()
        })
        .unwrap_or_default()
    }

    /// Apply one participant's edit — the envelopes and next view an
    /// `editing::*` primitive returned for that participant's view — and
    /// store the view. Each envelope goes through the canonical executor with
    /// the invoker in its context (durable attribution), on a dispatcher
    /// separate from this window's, so the edit never joins the local user's
    /// undo group.
    ///
    /// # Errors
    /// Refuses an invoker without a participant, and returns the first
    /// executor refusal; envelopes before it stay applied, the view is then
    /// left as it was (rebased).
    #[cfg(test)]
    pub(crate) fn apply_participant_edit(
        &mut self,
        invoker: &tilo_actions::ActionInvoker,
        state: &mut ui::widget::Edits<'_>,
        outcome: crate::actions::editing::EditOutcome,
    ) -> Result<(), String> {
        let Some(participant) = invoker.participant.clone() else {
            return Err("a participant edit needs the invoking participant".into());
        };
        let (envelopes, view) = outcome;
        let Some(dispatch) = self.dispatch.clone() else {
            return Err("this view edits nothing".into());
        };
        // Each edit is the participant's in history.
        let _invoker = ui::invocation_scope::InvokerScope::enter(invoker.clone());
        for action in &envelopes {
            dispatch
                .apply_now(state, &crate::model::action(action))
                .map_err(|error| error.to_string())?;
        }
        self.set_participant_view(&participant, &view, state);
        Ok(())
    }

    /// Every other participant's caret or selection as collaboration
    /// presence, under the same `wp_block` key and byte offsets the local
    /// window publishes, so peers render an agent exactly like a person.
    /// The host publishes each entry under that participant's identity.
    pub fn participant_presence(&self, doc: &AutoCommit) -> Vec<ParticipantPresence> {
        let mut out = Vec::new();
        for participant in self.participants.participants() {
            let Some(view) = self.participants.view(participant, doc) else {
                continue;
            };
            let Some(payload) = participant_presence_payload(&view, doc) else {
                continue;
            };
            out.push(ParticipantPresence {
                participant: participant.clone(),
                label: self.participants.label(participant).map(str::to_string),
                key: block_presence_key(&view.caret.block),
                payload,
            });
        }
        out.sort_by(|a, b| a.participant.as_str().cmp(b.participant.as_str()));
        out
    }

    // ---- formatting affordances → actions -----------------------------------

    /// Toggle an inline mark (bold/italic/underline/strike) over the selection,
    /// as one replayable action. A toggle with no selection is a no-op.
    pub(super) fn dispatch_toggle_mark(&mut self, state: &mut ui::widget::Edits<'_>, name: &str) {
        if let Some(mark) = inline_mark(name) {
            self.edit_via_primitive(state, |view, doc| {
                crate::actions::editing::toggle_mark(view, doc, mark)
            });
            return;
        }
        let Some((spans, active)) = self.inner.plan_toggle_mark(state, name) else {
            return;
        };
        let region = self.inner.region_kind();
        let value = if active {
            None
        } else {
            Some(crate::actions::MarkVal::Bool(true))
        };
        let action = crate::actions::set_mark_action(region, to_spans(spans), name, value, vec![]);
        self.perform_action(state, &action);
    }

    /// Toggle a mutually-exclusive mark (super/sub) over the selection / caret
    /// block, clearing its sibling, as one replayable action.
    pub(super) fn dispatch_exclusive_mark(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        name: &str,
        sibling: &str,
    ) {
        if let Some(mark) = inline_mark(name) {
            self.edit_via_primitive(state, |view, doc| {
                crate::actions::editing::toggle_mark(view, doc, mark)
            });
            return;
        }
        let (spans, active) = self.inner.plan_exclusive_mark(state, name);
        if spans.iter().all(|(_, s, e)| s >= e) {
            return;
        }
        let region = self.inner.region_kind();
        let value = if active {
            None
        } else {
            Some(crate::actions::MarkVal::Bool(true))
        };
        // When turning on, also clear the sibling over the same spans.
        let also_clear = if active {
            vec![]
        } else {
            vec![sibling.to_string()]
        };
        let action =
            crate::actions::set_mark_action(region, to_spans(spans), name, value, also_clear);
        self.perform_action(state, &action);
    }

    /// Set a valued inline mark (font/size/colour/highlight) over the selection
    /// or caret block, as one replayable action (replacing any prior value).
    pub(super) fn dispatch_set_mark_value(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        name: &str,
        value: crate::actions::MarkVal,
    ) {
        self.edit_via_primitive(state, |view, doc| {
            crate::actions::editing::set_mark(view, doc, name, value)
        });
    }

    /// Clear a valued inline mark over the selection or caret block.
    pub(super) fn dispatch_clear_mark(&mut self, state: &mut ui::widget::Edits<'_>, name: &str) {
        self.edit_via_primitive(state, |view, doc| {
            crate::actions::editing::clear_mark(view, doc, name)
        });
    }

    /// Set the paragraph kind/level over the selection, as one replayable action.
    pub(super) fn dispatch_paragraph_style(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        kind: &str,
        level: Option<i64>,
        ordered: Option<bool>,
    ) {
        let style = crate::actions::editing::ParagraphStyle {
            kind: kind.to_string(),
            level,
            ordered,
        };
        self.edit_via_primitive(state, |view, doc| {
            crate::actions::editing::set_paragraph_style(view, doc, &style)
        });
    }

    /// Apply a named paragraph style (a style-picker pick) over the current
    /// paragraph targets as one replayable action: the style attr plus, for a
    /// built-in, the structural kind / level / ordered.
    pub(super) fn dispatch_apply_named_style(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        id: &str,
    ) {
        self.edit_via_primitive(state, |view, doc| {
            crate::actions::editing::apply_named_style(view, doc, id)
        });
    }

    /// Set a block attribute (alignment / line-spacing / style) over the current
    /// target blocks, as one replayable action.
    pub(super) fn dispatch_block_attr(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        key: &str,
        value: crate::actions::AttrVal,
    ) {
        let blocks = self.inner.target_block_ids(state);
        if blocks.is_empty() {
            return;
        }
        let region = self.inner.region_kind();
        let action = crate::actions::set_block_attr_action(region, blocks, key, value);
        self.perform_action(state, &action);
    }

    /// Make the target paragraphs a list (`ordered` numbered, else
    /// bulleted), or plain paragraphs again when every one already is that
    /// list, as one replayable action. Table cells and other structural
    /// blocks keep their kind.
    pub(super) fn dispatch_toggle_list(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        ordered: bool,
    ) {
        let targets = self.inner.target_block_ids(state);
        let textual: Vec<&BlockView> = targets
            .iter()
            .filter_map(|id| self.blocks.iter().find(|block| &block.id == id))
            .filter(|block| matches!(block.kind.as_str(), "paragraph" | "heading" | "list_item"))
            .collect();
        if textual.is_empty() {
            return;
        }
        let already = textual
            .iter()
            .all(|block| block.kind == "list_item" && block.flag("ordered") == ordered);
        let blocks: Vec<String> = textual.iter().map(|block| block.id.clone()).collect();
        let region = self.inner.region_kind();
        let action = if already {
            crate::actions::set_paragraph_style_action(region, blocks, "paragraph", None, None)
        } else {
            crate::actions::set_paragraph_style_action(
                region,
                blocks,
                "list_item",
                None,
                Some(ordered),
            )
        };
        self.perform_action(state, &action);
    }

    /// Insert a `rows`×`cols` table after the caret as one replayable action,
    /// then re-seat the caret into the new table's first cell (presentation) —
    /// the cell ids are minted inside `apply`, so they're discovered from the
    /// document afterwards rather than known up front.
    pub(super) fn insert_table_via_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        rows: usize,
        cols: usize,
    ) {
        let after = self.inner.caret().block.clone();
        let before: std::collections::HashSet<String> = self.body_cell_ids(state);
        self.perform_action(
            state,
            &crate::actions::insert_table_action(&after, rows, cols),
        );
        // The first cell of the freshly-created table = the new `cell` block at
        // (row 0, col 0). Seat the caret there so typing continues in the table.
        if let Some(c) = self.inner.containers.clone() {
            let snap = tilo_doc::model::materialize(state, &c);
            let first = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "cell" && !before.contains(&b.id))
                .find(|b| {
                    matches!(b.attrs.get("row"), Some(AttrVal::Int(0)))
                        && matches!(b.attrs.get("col"), Some(AttrVal::Int(0)))
                })
                .map(|b| b.id.clone());
            if let Some(fc) = first {
                self.inner.set_caret(Caret {
                    block: fc,
                    offset: 0,
                });
                self.inner.clear_anchor();
            }
        }
    }

    pub(super) fn caret_table_target(&self) -> Option<(String, usize, usize, usize, usize)> {
        let current = self
            .blocks
            .iter()
            .find(|block| block.id == self.inner.caret.block)?;
        let table = match current.attrs.get("table") {
            Some(AttrVal::Str(table)) => table.clone(),
            _ => return None,
        };
        let coordinate = |block: &BlockView, key: &str| match block.attrs.get(key) {
            Some(AttrVal::Int(value)) if *value >= 0 => num::usize_of_i64(*value),
            _ => 0,
        };
        let row = coordinate(current, "row");
        let column = coordinate(current, "col");
        let table_blocks: Vec<_> = self
            .blocks
            .iter()
            .filter(|block| {
                block.kind == "cell"
                    && matches!(block.attrs.get("table"), Some(AttrVal::Str(id)) if id == &table)
            })
            .collect();
        let rows = table_blocks
            .iter()
            .map(|block| coordinate(block, "row"))
            .max()?
            .saturating_add(1);
        let columns = table_blocks
            .iter()
            .map(|block| coordinate(block, "col"))
            .max()?
            .saturating_add(1);
        Some((table, row, column, rows, columns))
    }

    pub(super) fn edit_table_via_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        edit: crate::actions::TableEdit,
    ) {
        let Some((table, row, column, rows, columns)) = self.caret_table_target() else {
            return;
        };
        let order = self.inner.ordered_blocks(state);
        let before_table = self
            .blocks
            .iter()
            .filter(|block| {
                block.kind == "cell"
                    && matches!(block.attrs.get("table"), Some(AttrVal::Str(id)) if id == &table)
            })
            .filter_map(|block| order.iter().position(|id| id == &block.id))
            .min()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| order.get(index).cloned());
        self.perform_action(state, &crate::actions::edit_table_action(&table, edit));
        self.refresh_blocks(state);
        let desired = match edit {
            crate::actions::TableEdit::InsertRow { at } => Some((at, 0)),
            crate::actions::TableEdit::InsertColumn { at } => Some((row, at)),
            crate::actions::TableEdit::DeleteRow { row: removed } => {
                (rows > 1).then_some((removed.min(rows.saturating_sub(2)), column))
            }
            crate::actions::TableEdit::DeleteColumn { column: removed } => {
                (columns > 1).then_some((row, removed.min(columns.saturating_sub(2))))
            }
            crate::actions::TableEdit::Delete => None,
        };
        let next = desired.and_then(|(target_row, target_column)| {
            self.blocks
                .iter()
                .find(|block| {
                    matches!(block.attrs.get("table"), Some(AttrVal::Str(id)) if id == &table)
                        && matches!(block.attrs.get("row"), Some(AttrVal::Int(value)) if *value == num::i64_of_usize(target_row))
                        && matches!(block.attrs.get("col"), Some(AttrVal::Int(value)) if *value == num::i64_of_usize(target_column))
                })
                .map(|block| block.id.clone())
        });
        if let Some(block) = next
            .or(before_table)
            .or_else(|| self.body_order.id_at(0).map(str::to_owned))
        {
            self.inner.set_caret(Caret { block, offset: 0 });
        }
        self.inner.clear_anchor();
    }

    /// The ids of every `cell` block currently in the body.
    fn body_cell_ids(&self, state: &AutoCommit) -> std::collections::HashSet<String> {
        match self.inner.containers.clone() {
            Some(c) => tilo_doc::model::materialize(state, &c)
                .blocks
                .into_iter()
                .filter(|b| b.kind == "cell")
                .map(|b| b.id)
                .collect(),
            None => std::collections::HashSet::new(),
        }
    }
}

/// Whether `effect` answers its caller once its edit applied.
fn answers_later(effect: &WordProcessorEffect) -> bool {
    use ui::tilo_resource::Effect as _;
    let id = effect.id();
    WordProcessorEffect::declarations()
        .iter()
        .any(|declaration| declaration.id == id && declaration.is_job())
}

impl WordProcessorWidget {
    /// Apply a synchronous command or view operation. Returns `Ok(false)`
    /// when `type_id` is not this widget's.
    /// Invoke `effect` through the document's door, as the palette or an
    /// agent does: checked, and answered by [`Self::answer_effect`].
    pub(super) fn invoke_effect(&mut self, effect: &WordProcessorEffect) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.effect(effect);
        }
        self.lifecycle.dirty = true;
    }

    /// Answer `effect`, however it was invoked.
    pub(crate) fn answer_effect(
        &mut self,
        effect: WordProcessorEffect,
        cx: &mut ui::framework::EffectCx<crate::framework::Document, Self>,
    ) -> ui::framework::EffectAnswer {
        use ui::framework::Effected;
        if let Some(answer) = self.answer_edit_effect(&effect, cx) {
            return answer;
        }
        if let Some(movement) = effect.view_operation()? {
            let doc = cx.document().ok_or("the document is not at hand")?.clone();
            // A delivered invocation by a participant (an agent task) moves
            // only that participant's view; without one it is this window's.
            match ui::invocation_scope::current_participant() {
                Some(participant) => self.move_participant_view(&participant, &movement, &doc)?,
                None => self.apply_local_view_operation(&doc, &movement)?,
            }
            self.lifecycle.dirty = true;
            return Ok(Effected::done());
        }
        match effect {
            WordProcessorEffect::OpenDocument { path: Some(path) } => {
                self.open_in_place(cx, std::path::Path::new(&path))?;
            }
            WordProcessorEffect::OpenDocument { path: None } => {
                // Where files have paths, the document is worked on where it
                // is; elsewhere (a browser page) its contents are brought in
                // and Save asks where they go.
                if tilo_files::available(tilo_files::Feature::NativePaths) {
                    return Ok(
                        cx.choose_path(open_request(), |editor, path, cx| match path {
                            Some(path) => {
                                editor.open_in_place(cx, &path)?;
                                Ok(Effected::done())
                            }
                            None => Err("the file picker was cancelled".into()),
                        }),
                    );
                }
                return Ok(
                    cx.choose_file(open_request(), |editor, file, cx| match file {
                        Some(file) => {
                            editor.open_contents(cx, &file.name, &file.bytes)?;
                            Ok(Effected::done())
                        }
                        None => Err("the file picker was cancelled".into()),
                    }),
                );
            }
            WordProcessorEffect::Save { path } => {
                match path.map(PathBuf::from).or_else(|| self.path.clone()) {
                    Some(path) => self.save_now(cx, &path)?,
                    None => match self.kept.clone() {
                        Some((kept, format)) => return self.write_kept(cx, &kept, format),
                        None => return Ok(self.choose_where_to_save(cx)),
                    },
                }
            }
            WordProcessorEffect::SaveAs { path: Some(path) } => {
                self.save_now(cx, std::path::Path::new(&path))?;
            }
            WordProcessorEffect::SaveAs { path: None } => {
                return Ok(self.choose_where_to_save(cx));
            }
            WordProcessorEffect::Find => {
                let doc = cx.document().ok_or("the document is not at hand")?.clone();
                self.open_find(&doc);
            }
            WordProcessorEffect::ZoomIn => {
                self.zoom_by(0.1);
            }
            WordProcessorEffect::ZoomOut => {
                self.zoom_by(-0.1);
            }
            WordProcessorEffect::ZoomReset => {
                self.set_zoom(1.0);
            }
            WordProcessorEffect::Spellcheck => self.toggle_spellcheck(),
            WordProcessorEffect::Markup => self.toggle_markup(),
            WordProcessorEffect::Styles => {
                self.view.styles_panel_visible = !self.view.styles_panel_visible;
            }
            WordProcessorEffect::Ruler => self.view.ruler_visible = !self.view.ruler_visible,
            WordProcessorEffect::Outline => self.outline.toggle(),
            WordProcessorEffect::ImportDocxContent { content, name } => {
                return self.import_content(cx, &content, name.as_deref());
            }
            WordProcessorEffect::ExportContent { format } => {
                return self.export_content(cx, format.as_deref());
            }
            other => return Err(format!("`{}` is not handled here", other.id())),
        }
        self.lifecycle.dirty = true;
        Ok(Effected::done())
    }

    /// Replace the document with the file at `path` (its format by
    /// extension), as one action Undo takes back; the file becomes where
    /// Save writes.
    fn open_in_place(
        &mut self,
        cx: &ui::framework::EffectCx<crate::framework::Document, Self>,
        path: &std::path::Path,
    ) -> Result<(), String> {
        let bytes = tilo_files::file_at(path)
            .and_then(|(dir, name)| tilo_files::blocking::read(&dir, &name))
            .map_err(|error| format!("could not open {}: {error}", path.display()))?;
        let format = WpFormat::from_path(path);
        self.replace_with(cx, &bytes, format)?;
        self.set_file(path.to_path_buf(), format);
        Ok(())
    }

    /// Replace the document with the file `name` whose contents are `bytes`
    /// (its format by extension), as one action Undo takes back. It has no
    /// path here, so Save asks where it goes.
    fn open_contents(
        &mut self,
        cx: &ui::framework::EffectCx<crate::framework::Document, Self>,
        name: &str,
        bytes: &[u8],
    ) -> Result<(), String> {
        let format = WpFormat::from_path(std::path::Path::new(name));
        self.replace_with(cx, bytes, format)?;
        self.path = None;
        self.kept = None;
        self.format = Some(format);
        Ok(())
    }

    /// Replace the body of this document, while it is empty, with the file
    /// `name` (Word when it has none) kept as `content`, a
    /// `tilo.content:<hash>` address. A document with words, pictures or
    /// tables in it is someone's writing, which an import never replaces.
    fn import_content(
        &mut self,
        cx: &ui::framework::EffectCx<crate::framework::Document, Self>,
        content: &str,
        name: Option<&str>,
    ) -> ui::framework::EffectAnswer {
        let hash = ui::tilo_actions::content_hash(content.trim())
            // A plan that carried the content gives its bare hash.
            .or_else(|| {
                let bare = content.trim();
                (bare.len() == 64 && bare.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .then_some(bare)
            })
            .ok_or_else(|| format!("`{content}` names no content: give `tilo.content:<hash>`"))?
            .to_ascii_lowercase();
        let doc = cx.document().ok_or("the document is not at hand")?.clone();
        let containers = crate::model::open(&doc)?;
        let written =
            tilo_doc::model::materialize_region(&doc, &containers, tilo_doc::model::Region::Body)
                .iter()
                .any(|block| block.kind != "paragraph" || !block.text.trim().is_empty());
        if written {
            return Err(
                "this document already has writing in it, which an import would replace: open a \
                 new document and import there"
                    .into(),
            );
        }
        let name = name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("document.docx");
        let bytes = self
            .inner
            .content_store()
            .read(&hash)
            .ok_or_else(|| format!("{name} is not on this device"))?;
        let format = WpFormat::from_path(std::path::Path::new(name));
        self.replace_with(cx, &bytes, format)?;
        self.path = None;
        self.kept = None;
        self.format = Some(format);
        self.lifecycle.dirty = true;
        Ok(ui::framework::Effected::value(serde_json::json!({
            "imported": name,
            "content": format!("tilo.content:{hash}"),
        })))
    }

    /// Keep the document as a file in `format` (Word when none) in the
    /// content store, and answer its address.
    fn export_content(
        &mut self,
        cx: &ui::framework::EffectCx<crate::framework::Document, Self>,
        format: Option<&str>,
    ) -> ui::framework::EffectAnswer {
        let extension = format
            .map(|format| format.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|format| !format.is_empty())
            .unwrap_or_else(|| "docx".to_owned());
        let format = match extension.as_str() {
            "docx" | "word" => WpFormat::Docx,
            "odt" => WpFormat::Odt,
            "md" | "markdown" => WpFormat::Markdown,
            "html" | "htm" => WpFormat::Html,
            "rtf" => WpFormat::Rtf,
            "txt" | "text" => WpFormat::Text,
            "pdf" => WpFormat::Pdf,
            other => {
                return Err(format!(
                    "`{other}` is no format: docx, odt, md, html, rtf, txt or pdf"
                ));
            }
        };
        let doc = cx.document().ok_or("the document is not at hand")?.clone();
        let bytes = self.export_bytes(&doc, format)?;
        let hash = self
            .inner
            .content_store()
            .put(&bytes)
            .map_err(|error| format!("the document could not be kept: {error}"))?;
        ui::framework::Effected::content(&hash)
    }

    /// Replace the document's body with `bytes` read as `format`.
    fn replace_with(
        &mut self,
        cx: &ui::framework::EffectCx<crate::framework::Document, Self>,
        bytes: &[u8],
        format: WpFormat,
    ) -> Result<(), String> {
        let action = crate::import_action(bytes, format, self.inner.content_store())?;
        let dispatch = cx.dispatcher();
        dispatch.group(|| {
            dispatch.submit(crate::model::edit(&action));
            dispatch.submit(crate::model::Document::ADOPT_COMMENTS.invoke());
        });
        self.scroll_y = 0.0;
        // The whole body changes; per-block caches keyed by id/text go stale.
        self.layout_cache.clear();
        self.recent_layout_cache.clear();
        self.spell_cache.clear();
        Ok(())
    }

    /// Write the document to `path` now, from the document lent to the
    /// effect.
    fn save_now(
        &mut self,
        cx: &ui::framework::EffectCx<crate::framework::Document, Self>,
        path: &std::path::Path,
    ) -> Result<(), String> {
        let doc = cx.document().ok_or("the document is not at hand")?.clone();
        // Saving where the document lives says nothing; only a failure is
        // news.
        self.try_write_to_path(&doc, path, WpFormat::from_path(path))?;
        Ok(())
    }

    /// Write the document to `destination`, a place with no path here, as
    /// `format`, without asking.
    fn write_kept(
        &mut self,
        cx: &mut ui::framework::EffectCx<crate::framework::Document, Self>,
        destination: &ui::portal::SaveDestination,
        format: WpFormat,
    ) -> ui::framework::EffectAnswer {
        let doc = cx.document().ok_or("the document is not at hand")?.clone();
        let bytes = self.export_bytes(&doc, format)?;
        Ok(cx.write_destination(destination, bytes, |_, written, _| {
            written.map(|()| ui::framework::Effected::done())
        }))
    }

    /// Ask the person where to save with the host's chooser; the chosen file
    /// is written on the next sync, which answers the caller.
    fn choose_where_to_save(
        &self,
        cx: &mut ui::framework::EffectCx<crate::framework::Document, Self>,
    ) -> ui::framework::Effected {
        let suggested = self.path.as_ref().and_then(|p| p.file_name()).map_or_else(
            || "Untitled.md".to_string(),
            |f| f.to_string_lossy().into_owned(),
        );
        if !tilo_files::available(tilo_files::Feature::NativePaths) {
            // No path here (a browser page): the document goes where the
            // person chooses, as the chosen name's format, and that place is
            // kept for the next Save.
            return cx.choose_destination(save_request(&suggested), |editor, destination, cx| {
                let destination =
                    destination.ok_or_else(|| "the file picker was cancelled".to_owned())?;
                let format = WpFormat::from_path(std::path::Path::new(destination.name()));
                // An export-only format is a copy, not the document's place.
                if !matches!(format, WpFormat::Pdf | WpFormat::Epub) {
                    editor.kept = Some((destination.clone(), format));
                }
                editor.write_kept(cx, &destination, format)
            });
        }
        cx.choose_path(save_request(&suggested), |editor, path, cx| match path {
            Some(path) => {
                editor.pending_save = Some((path, cx.reply()));
                editor.lifecycle.dirty = true;
                Ok(ui::framework::Effected::Pending)
            }
            None => Err("the file picker was cancelled".into()),
        })
    }
}

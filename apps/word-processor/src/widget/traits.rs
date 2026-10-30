//! The editor's part of the toolkit contract, which its window answers
//! with: how it syncs from its document, draws, answers actions and
//! publishes presence, and the capabilities it has as a participant.

use super::toolbar::{HIGHLIGHT_PALETTE, STYLE_PALETTE};
use crate::WpFormat;
use crate::i18n::t;
use crate::text_offsets::char_to_byte;
use automerge::{AutoCommit, ObjId};
use tilo_doc::model::Region;
use ui::paint::clipped;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::paint_damage::PaintParts;
use ui::widget::{AppModel, ModelPath};
use ui::{PresenceCtx, PresenceKey};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use super::accessibility::review_action_target;
use super::{
    ChromeColors, Edges, Editor, LayoutMode, PageContentInput, PaginationResult, PanelHit,
    block_presence_key,
};

/// The editor's part of the widget contract, which the window
/// ([`WordProcessorWidget`](super::WordProcessorWidget)) answers with.
impl Editor {
    pub(crate) fn primary_vertical_scroll(&self) -> ui::widget::PrimaryScrollRegion {
        if self.styles_panel_full_window() {
            let viewport = self.panel_body.height();
            return ui::widget::PrimaryScrollRegion {
                key: "styles",
                owner_generation: None,
                metrics: ui::widget::ScrollMetrics::new(
                    self.panel_max_scroll + viewport,
                    viewport,
                    self.panel_scroll,
                ),
            };
        }
        ui::widget::PrimaryScrollRegion {
            key: "primary",
            owner_generation: None,
            metrics: self.canvas_scroll_metrics(),
        }
    }

    pub(crate) fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        action.apply_to(
            self,
            |editor| Some(editor.primary_vertical_scroll()),
            |doc, _, next| {
                if doc.styles_panel_full_window() {
                    doc.panel_scroll = next;
                } else {
                    doc.scroll_y = next - doc.find_inset;
                }
                doc.lifecycle.dirty = true;
            },
        )
    }

    pub(crate) fn native_input_snapshot(
        &self,
        state: &AppModel,
        path: &ModelPath,
    ) -> serde_json::Value {
        let (state, _) = ui::doc::document_ref(state, path);
        let caret = self.inner.caret();
        let region = self.inner.region();
        // Body text reads only the caret block. Running-region text would
        // materialize an entire region, so omit it from this diagnostic path.
        let caret_text = matches!(region, Region::Body).then(|| self.inner.current_text(state));
        serde_json::json!({
            "offset_y": self.scroll_y,
            "max_scroll": self.max_scroll,
            "body_window_start": self.body_window_start,
            "body_window_len": self.blocks.len(),
            "body_block_count": self.body_order.len(),
            "focused": self.lifecycle.focused,
            "caret_block": caret.block,
            "caret_offset": caret.offset,
            "caret_region": match region {
                Region::Body => "body",
                Region::Header => "header",
                Region::Footer => "footer",
            },
            "has_selection": self.inner.has_selection(),
            "caret_text": caret_text,
        })
    }

    pub(crate) fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document_ref(state, root);
        // A transient cue says so in the window's bar while it lasts.
        self.toast.expire();
        self.toast.publish();
        #[cfg(test)]
        if self.source.is_none() && self.dispatch.is_some() {
            self.source = ui::framework::testing::detached_live(state).ok();
        }
        // A participant who may only view (or History mode's past version)
        // keeps its caret on what is there.
        if self.read_only() {
            self.inner.bind_view_only(state, root);
        } else {
            self.inner.bind(state, root);
        }
        if let Some((path, reply)) = self.pending_save.take() {
            let format = WpFormat::from_path(&path);
            reply.finish(
                self.try_write_to_path(state, &path, format)
                    .map(|()| tilo_actions::ActionOutput::default()),
            );
        }
        self.refresh_blocks(state);
        // Other participants' views follow local and remote edits alike; a
        // participant's own applied edit hands over its next view.
        let now = ui::time::now();
        self.participants.adopt_outcomes(state, now);
        self.adopt_local_outcome(state);
        self.participants.rebase(state);
        self.participants.expire(now);
        self.resolve_bound_edits(state);
        self.follow_settings();
        // Reload the stylesheet from the doc unless a live, not-yet-persisted
        // local edit is in progress (a guide drag, a numeric scrub, or typing
        // into a panel field) — reloading would discard the live preview.
        if self.guide_drag.is_none() && self.editing_field.is_none() {
            self.sheet = self.inner.stylesheet(state);
        }
        // Reload the persisted page margins (document-level page setup) unless a
        // guide drag is mid-flight (which would clobber the live preview). Absent
        // from the document → keep the current/default margins.
        if self.guide_drag.is_none()
            && let Some([t, r, b, l]) = self.inner.page_margins(state)
        {
            self.margins = Edges {
                top: t,
                right: r,
                bottom: b,
                left: l,
            };
        }
        self.lifecycle.dirty = true;
    }

    pub(crate) fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, _path: &ObjId) {
        self.drain_image_choice(edits);
        if let Some(scenario) = self.staged_scenario.take() {
            self.run_staged_scenario(edits, &scenario);
        }
        self.refresh_blocks(edits.doc());
        self.apply_find_replacements(edits);
        // The editor's commands are the window bar's slots: what the person
        // did there runs with the document in hand, then the slots follow
        // the selection.
        self.apply_window_toolbar(edits);
    }

    /// Where the window's layout placed the editor.
    pub(crate) fn place(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub(crate) fn get_rect(&self) -> Rect {
        self.rect
    }

    pub(crate) fn needs_animation_frame(&self) -> bool {
        // Only the outline and inspector drawers move, while they open or
        // close.
        self.outline_drawer.is_animating() || self.inspector_drawer.is_animating()
    }

    pub(crate) fn modal_capture(&self) -> Option<Rect> {
        if self.context_menu.is_open()
            || self.style_menu.is_open()
            || self.file_menu.is_open()
            || self.line_spacing.is_open()
            || self.style_pick.is_open()
        {
            Some(self.rect)
        } else {
            None
        }
    }

    pub(crate) fn traps_focus(&self) -> bool {
        self.modal_capture().is_some()
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.ime_rect = None;
        // Resolve the OS theme into this frame's chrome palette once; every
        // chrome helper below reads `self.chrome` so the whole editor follows
        // light/dark mode + the OS palette. (`draw_frame_chrome` resolves the
        // spacing tokens and the measured panel metrics alongside it.)
        self.chrome = ChromeColors::for_theme(theme);
        self.paper_theme = theme.on_ground(self.chrome.paper);
        self.desk_theme = theme.clone();
        // Document sizes are authored points, not theme text roles: apply
        // both the text size and the UI scale before fitting the sheet, or a
        // larger UI grows the controls while the writing stays small.
        self.text_scale = f64::from(theme.text_scale.clamp(0.75, 2.0) * theme.ui_scale);
        let full = self.rect;
        // Each region is a part of its own, so the frame reports as damage
        // the regions whose drawing changed: a typed character repaints its
        // paragraph, not the window. What the canvas clips (scrolled content
        // never paints over the bars) is clipped in each part.
        let mut parts = PaintParts::new(scene);
        let fg = parts.draw_bounded(scene, "word-bars", |scene| {
            let fg = self.draw_frame_chrome(scene, transform, text_renderer, theme, full);
            let bars = Rect::new(full.x0, full.y0, full.x1, fg.canvas.y0);
            let reach = ui::widget::focus_ring_reach(theme);
            (bars.inflate(reach, reach), fg)
        });
        let canvas = fg.canvas;

        // Pass 1: shape + paginate every block, then resolve scroll against the
        // resulting extent and shift `laid` into screen space.
        let PaginationResult {
            mut laid,
            cache: new_cache,
            cur_page,
            y,
            blocks,
        } = self.paginate_blocks(text_renderer, &fg);
        let (num_pages, sy) = self.finish_scroll(&fg, &mut laid, cur_page, y);

        let sheets = parts.draw(scene, "word-sheets", canvas, |scene| {
            clipped(scene, transform, canvas, |scene| {
                self.draw_page_sheets(scene, text_renderer, &fg, num_pages, sy)
            })
        });

        let (multi_sel, note_nums, spell_by_block) = self.prepare_pass2_inputs(blocks, &laid, &fg);

        let markers = self.draw_page_content(
            &mut parts,
            scene,
            (transform, &fg),
            text_renderer,
            &PageContentInput {
                laid: &laid,
                cache: &new_cache,
                multi_selection: multi_sel,
                spell_by_block: &spell_by_block,
                note_numbers: &note_nums,
            },
        );
        // The page-space passes over the content, each clipped to the canvas.
        parts.draw(scene, "word-notes", canvas, |scene| {
            clipped(scene, transform, canvas, |scene| {
                self.review.draw_pips(
                    &mut ui::paint::PaintCtx::new(scene, fg.ctf, text_renderer, theme),
                    self.chrome.paper,
                    &markers.comment_markers,
                );
                self.draw_footnotes(
                    scene,
                    &fg,
                    text_renderer,
                    &markers.page_notes,
                    &note_nums,
                    sy,
                );
                self.draw_empty_placeholder(scene, fg.ctf, text_renderer, laid.first());
                // Layout-edit overlay: draggable guides + live value labels
                // for the active mode, over the content.
                if self.layout_mode != LayoutMode::None {
                    self.draw_layout_overlay(scene, fg.ctf, text_renderer, sheets);
                }
                // Header/footer edit mode: veil the body + draw the band caret
                // (checks `self.inner.in_running_region()` itself). Drawn last
                // so the veil covers the body but the band + its caret stay
                // crisp on top.
                self.draw_running_region_veil(scene, &fg, num_pages, sy);
            });
        });
        parts.draw(scene, "word-review", canvas, |scene| {
            clipped(scene, transform, canvas, |scene| {
                self.draw_review_column(
                    scene,
                    (transform, &fg),
                    text_renderer,
                    theme,
                    presence,
                    &markers,
                );
            });
        });

        self.laid = laid;
        // Adopt this frame's cache (dropping the pruned entries for blocks that
        // no longer exist) so the next frame can reuse it.
        self.layout_cache = new_cache;
        self.draw_outside_page(
            &mut parts,
            scene,
            (transform, &fg),
            text_renderer,
            theme,
            presence,
        );
        self.paint_damage
            .record(full, parts.finish(scene, vec![full], transform, theme));
    }

    // Where the caret and selection end up is what the edits bound to them
    // are built from.
    pub(crate) fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.route_event(event, ctx);
        self.resolve_bound_edits(ctx.state.doc());
    }

    /// Top-layer pass: paint the right-click context menu (when open) so it
    /// floats above the page + chrome. Delegates to the shared [`ContextMenu`](ui::widget::context_menu::ContextMenu).
    pub(crate) fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let viewport = self.floating_viewport();
        for menu in [
            &mut self.context_menu,
            self.file_menu.menu_mut(),
            self.style_menu.menu_mut(),
        ] {
            if menu.is_open() {
                menu.constrain_to(viewport);
            }
        }
        if self.context_menu.is_open() {
            Widget::draw(
                &mut self.context_menu,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
        if self.styles_panel_visible() {
            let local = if self.styles_panel_overlay() {
                theme.on_ground(ui::chrome::panel_ground(theme))
            } else {
                theme.in_region(ui::surface::Region::Inspector)
            };
            let overlay_presence = PresenceCtx::empty();
            overlay_presence
                .viewport
                .set(Some(self.floating_viewport()));
            for menu in [&mut self.line_spacing, &mut self.style_pick] {
                if menu.is_open() {
                    Widget::draw_overlay(
                        menu,
                        scene,
                        transform,
                        text_renderer,
                        &local,
                        &overlay_presence,
                    );
                }
            }
        }
        self.compact_swatch_palette
            .draw(scene, transform, text_renderer, theme, presence);
        self.file_menu
            .draw(scene, transform, text_renderer, theme, presence);
        self.style_menu
            .draw(scene, transform, text_renderer, theme, presence);
        self.color_picker.draw(
            crate::style_color_picker::DrawArgs {
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            },
            self.rect,
            self.panel_rect,
        );
    }

    /// The word processor's local accessibility semantics: the editable
    /// `Role::Document` root. Blocks render directly, not as child widgets, so
    /// the full subtree comes from [`Editor::accessibility_tree`]
    /// rather than `children()`; convert the carrier into real synthetic
    /// children so AccessKit, AT-SPI and the built-in reader all receive it.
    pub(crate) fn accessibility(&self) -> ui::a11y::AccessNode {
        let mut tree = self.accessibility_tree();
        if !self.styles_panel_full_window() {
            tree.node = tree.node.with_name(t("a11y-document-editor"));
        }
        tree.into_access_node()
    }

    pub(crate) fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        fn published(node: &ui::a11y::AccessNode, target: &str) -> bool {
            node.action_target.as_deref() == Some(target)
                || node.children.iter().any(|child| published(child, target))
        }
        if !published(&self.accessibility(), target) {
            return false;
        }
        self.perform_access_action(action, &[ui::a11y::hash_key(&target)], state, path)
    }

    pub(crate) fn perform_access_action(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        let (state, path) = ui::doc::document(state, path);
        if let Some(result) = self.perform_styles_page_action(action, key_path, state) {
            return result;
        }
        if matches!(action, ui::a11y::Action::Focus) {
            if let Some(target) = key_path.last().copied() {
                let palette_target = if ui::a11y::hash_key(&"compact-swatch:clear") == target {
                    Some("compact-swatch:clear".to_string())
                } else {
                    (0..STYLE_PALETTE.len().max(HIGHLIGHT_PALETTE.len()))
                        .map(|index| format!("swatch:{index}"))
                        .find(|id| ui::a11y::hash_key(id) == target)
                };
                if let Some(id) = palette_target
                    && self.compact_swatch_palette.focus_target(&id, state, path)
                {
                    self.lifecycle.dirty = true;
                    return true;
                }
            }
            self.set_focus(true);
            return true;
        }
        if !matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        let Some(target) = key_path.last().copied() else {
            return false;
        };
        let compact_palette_target = if ui::a11y::hash_key(&"compact-swatch:clear") == target {
            Some("compact-swatch:clear".to_string())
        } else {
            (0..STYLE_PALETTE.len().max(HIGHLIGHT_PALETTE.len()))
                .map(|index| format!("swatch:{index}"))
                .find(|id| ui::a11y::hash_key(id) == target)
        };
        if let Some(id) = compact_palette_target
            && let Some((kind, choice)) = self
                .compact_swatch_palette
                .semantic_choice(&id, action, state, path)
        {
            self.apply_compact_palette_choice(state, kind, choice);
            return true;
        }
        if let Some(review_action) = self.review.hits().iter().find_map(|(_, review_action)| {
            (ui::a11y::hash_key(&review_action_target(review_action)) == target)
                .then(|| review_action.clone())
        }) {
            self.dispatch_review_action(review_action, state);
            return true;
        }
        if let Some(panel_hit) = self.panel_hits.iter().find_map(|(_, panel_hit)| {
            let id = match panel_hit {
                PanelHit::ImageInline => "inline",
                PanelHit::ImageFreePosition => "free",
                PanelHit::ImageWrapLeft => "left",
                PanelHit::ImageWrapRight => "right",
                _ => return None,
            };
            (ui::a11y::hash_key(&format!("word-processor.image-placement:{id}")) == target)
                .then(|| panel_hit.clone())
        }) {
            self.apply_panel_hit(state, panel_hit);
            return true;
        }
        false
    }

    pub(crate) fn claims_raw_touch(&self, point: Point) -> bool {
        self.segmented_rects()
            .iter()
            .flatten()
            .any(|rect| rect.contains(point))
    }

    pub(crate) fn wants_pointer_capture(&self) -> bool {
        self.segmented_touch.is_active()
    }

    pub(crate) fn set_focus(&mut self, focused: bool) {
        self.lifecycle.focused = focused;
        if !focused {
            // Blur any in-progress numeric edit; the next sync reloads the
            // persisted style values.
            self.focus_hosted(None, false);
        }
        self.lifecycle.dirty = true;
    }
    pub(crate) fn is_focused(&self) -> bool {
        self.lifecycle.focused
    }
    pub(crate) fn ime_cursor_rect(&self) -> Option<Rect> {
        if self.styles_panel_full_window() {
            self.num_inputs
                .iter()
                .find(|slot| {
                    Some(slot.field) == self.editing_field
                        && self.visible_styles_rect(slot.rect).is_some()
                })
                .and_then(|slot| Widget::ime_cursor_rect(&slot.input))
        } else {
            self.ime_rect
        }
    }
    pub(crate) fn ime_content(&self) -> Option<ui::ime::InputContent> {
        if self.styles_panel_full_window() {
            self.num_inputs
                .iter()
                .find(|slot| {
                    Some(slot.field) == self.editing_field
                        && self.visible_styles_rect(slot.rect).is_some()
                })
                .and_then(|slot| Widget::ime_content(&slot.input))
        } else {
            Some(ui::ime::InputContent::TEXT)
        }
    }
    pub(crate) fn take_action_palette_request(&mut self) -> Option<tilo_actions::ActionAddress> {
        self.action_palette_request.take()
    }
    pub(crate) fn needs_redraw(&self) -> bool {
        self.lifecycle.dirty
    }
    /// A toast wakes the document to leave.
    pub(crate) fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.toast.deadline()
    }
    pub(crate) fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.needs_redraw() || self.paint_damage.is_recorded() {
            self.paint_damage.collect(self.rect, regions);
        }
    }
    pub(crate) fn damage_covers_own_box(&self) -> bool {
        self.paint_damage.covers_own_bounds()
    }
    pub(crate) fn release_scene_cache(&mut self) {
        self.paint_damage.release_scene_cache();
    }
    pub(crate) fn mark_drawn(&mut self) {
        self.lifecycle.dirty = false;
        self.paint_damage.mark_drawn();
    }
    /// One self-drawn taffy leaf filling its tile: its size follows the
    /// parent alone, so only the first layout is a size change. Pages,
    /// toolbars and panels are positioned inside `self.rect` by hand.
    pub(crate) fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

impl Editor {
    /// Outbound collaboration presence: while focused, broadcast the local caret
    /// (or selection range) keyed to its block. Polled by the App after each event
    /// and fanned out as `CollabEvent::RemotePresenceUpdate`. Offsets are bytes, so
    /// the receiver feeds them straight into `RichTextLayout::caret_geometry`/
    /// `selection_rects`.
    pub(crate) fn presence_payload(&self) -> Option<(PresenceKey, ui::PresencePayload)> {
        if !self.lifecycle.focused {
            return None;
        }
        let caret_block = &self.inner.caret.block;
        if caret_block.is_empty() {
            return None;
        }
        let key = block_presence_key(caret_block);
        // A selection within the caret's block takes priority over the bare
        // caret. (v1 collab presence reports the caret-block portion of a
        // multi-block selection, mirroring how the caret block is what the
        // peer's caret lands in.)
        if self.inner.has_selection()
            && let Some((_, s, e)) = self
                .selection_blocks_cached()
                .into_iter()
                .find(|(b, _, _)| b == caret_block)
            && let Some(block) = self.blocks.iter().find(|b| &b.id == caret_block)
            && s < e
        {
            let start = char_to_byte(&block.text, s);
            let end = char_to_byte(&block.text, e);
            return Some((key, ui::PresencePayload::Selection { start, end }));
        }
        // Bare caret: convert the char offset to a byte offset against the
        // caret block's text so it maps directly through the layout geometry.
        let pos = self
            .blocks
            .iter()
            .find(|b| &b.id == caret_block)
            .map_or(self.inner.caret.offset, |b| {
                char_to_byte(&b.text, self.inner.caret.offset)
            });
        Some((key, ui::PresencePayload::Caret { pos }))
    }

    /// The selection for an agent, in the terms the editor's place-taking
    /// actions read (`at`): the caret's block and character offsets in it.
    /// A selection spanning blocks is shared as its part in the caret's
    /// block, the same part peers see, with every block's part noted beside
    /// it; the excerpt is exactly the caret block's text, so what the agent
    /// reads at the place is what it will act on there.
    pub(crate) fn shared_selection(&self) -> Option<ui::shared_selection::SharedSelection> {
        use ui::shared_selection::SharedSelection;
        if !self.lifecycle.focused {
            return None;
        }
        let caret_block = &self.inner.caret.block;
        let block = self.blocks.iter().find(|b| &b.id == caret_block)?;
        let kind = <tilo_document::RichText as tilo_resource::Resource>::KIND;
        let spans = if self.inner.has_selection() {
            self.selection_blocks_cached()
        } else {
            Vec::new()
        };
        if let Some((_, start, end)) = spans.iter().find(|(b, _, _)| b == caret_block)
            && start < end
        {
            let (start, end) = (*start, *end);
            let text: String = block.text.chars().skip(start).take(end - start).collect();
            let ranges = spans
                .iter()
                .map(|(block, start, end)| tilo_actions::InteractionTextRange {
                    block: block.clone(),
                    start: u64::try_from(*start).unwrap_or(u64::MAX),
                    end: u64::try_from(*end).unwrap_or(u64::MAX),
                })
                .collect();
            let mut selection = SharedSelection::of_text(&text)
                .at(tilo_actions::SelectionPlace::range(
                    caret_block.clone(),
                    start,
                    end,
                ))
                .in_kind(kind);
            selection.anchor = selection.anchor.spanning("body", ranges);
            return Some(selection);
        }
        Some(
            SharedSelection::default()
                .at(tilo_actions::SelectionPlace::caret(
                    caret_block.clone(),
                    self.inner.caret.offset,
                ))
                .in_kind(kind),
        )
    }
}

impl ui::semantic_document_read::SemanticReaders for Editor {
    fn semantic_readers(&self) -> &'static [ui::semantic_document_read::RegisteredSemanticReader] {
        &crate::semantic_read::action::READERS
    }
}

impl ui::undo::LocalUndo for Editor {
    fn handle_local_undo_request(&mut self, _request: ui::undo::UndoRequest) -> bool {
        // The host resolves Undo before dispatching keyboard events to us.
        // An open menu must capture that path as well as ordinary editing keys.
        self.modal_capture().is_some()
    }
}

impl ui::collaboration::participants::Participants for Editor {
    /// An agent task joined: its view is placed at the document end on the
    /// next sync (no document here) and published as presence from then on.
    fn join_participant(&mut self, participant: &tilo_actions::ParticipantId) -> bool {
        let joined = self.participants.join(participant.clone(), ui::time::now());
        self.lifecycle.dirty |= joined;
        joined
    }
    fn leave_participant(&mut self, participant: &tilo_actions::ParticipantId) {
        self.participants.remove(participant);
        self.lifecycle.dirty = true;
    }
    fn set_participant_label(&mut self, participant: &tilo_actions::ParticipantId, label: &str) {
        Editor::set_participant_label(self, participant, label);
    }
    fn participant_presence(&self, doc: &AutoCommit) -> Vec<ui::ParticipantPresence> {
        Editor::participant_presence(self, doc)
    }
}

impl Editor {
    /// The passes outside the page clip, each its own part: the scroll bar,
    /// collaborators' marks (clipped to the canvas themselves), the layout bar
    /// and the drawers.
    fn draw_outside_page(
        &mut self,
        parts: &mut PaintParts,
        scene: &mut Scene,
        (transform, fg): (Affine, &super::FrameGeometry),
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let canvas = fg.canvas;
        let reach = ui::widget::focus_ring_reach(theme);
        let gutter = f64::from(self.desk_theme.tokens.metrics.scrollbar_gutter);
        let track = Rect::new(canvas.x1 - gutter, canvas.y0, canvas.x1, canvas.y1);
        parts.draw(
            scene,
            "word-scrollbar",
            track.inflate(reach, reach),
            |scene| {
                self.draw_scrollbar(scene, transform, canvas);
            },
        );

        parts.draw(scene, "word-peers", canvas, |scene| {
            self.draw_remote_overlays(scene, fg.ctf, fg.canvas_clip, tr, theme, presence);
        });

        let lbar = Rect::new(
            fg.canvas_left,
            fg.layout_bar_top,
            fg.canvas_right,
            fg.content_bottom,
        );
        parts.draw(
            scene,
            "word-layout-bar",
            lbar.inflate(reach, reach),
            |scene| {
                self.draw_layout_bar(scene, transform, tr, theme, lbar);
            },
        );
        parts.draw(scene, "word-drawers", self.rect, |scene| {
            self.draw_drawers(scene, transform, tr, theme);
        });
    }
}

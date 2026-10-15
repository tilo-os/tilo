//! The toolkit traits: how the editor syncs from its document, draws, answers
//! actions and presents itself as an app, a presence publisher and a
//! participant.

use super::toolbar::{HIGHLIGHT_PALETTE, STYLE_PALETTE};
use crate::WpFormat;
use crate::i18n::t;
use crate::text_offsets::char_to_byte;
use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{NodeId, TaffyTree};
use tilo_doc::model::Region;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::TaffyTreeExt;
use ui::widget::Widget;
use ui::widget::{AppModel, ModelPath};
use ui::{PresenceCtx, PresenceKey};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use super::accessibility::{review_action_target, tool_action_id};
use super::{
    ChromeColors, Edges, LayoutMode, PageContentInput, PaginationResult, PanelHit,
    WordProcessorWidget, block_presence_key,
};

impl Widget for WordProcessorWidget {
    ui::provides!(
        dyn ui::collaboration::participants::Participants,
        dyn ui::undo::LocalUndo,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::presence::capability::PublishesPresence
    );

    fn semantic_readers(&self) -> &'static [ui::semantic_document_read::RegisteredSemanticReader] {
        &crate::semantic_read::action::READERS
    }

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        if self.styles_panel_full_window() {
            let viewport = self.panel_body.height();
            return Some(ui::widget::PrimaryScrollRegion {
                key: "styles",
                owner_generation: None,
                metrics: ui::widget::ScrollMetrics::new(
                    self.panel_max_scroll + viewport,
                    viewport,
                    self.panel_scroll,
                ),
            });
        }
        Some(ui::widget::PrimaryScrollRegion {
            key: "primary",
            owner_generation: None,
            metrics: ui::widget::ScrollMetrics::new(
                self.max_scroll + self.scroll_viewport,
                self.scroll_viewport,
                self.scroll_y,
            ),
        })
    }

    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        action.apply_to(self, Self::primary_vertical_scroll, |doc, _, next| {
            if doc.styles_panel_full_window() {
                doc.panel_scroll = next;
            } else {
                doc.scroll_y = next;
            }
            doc.lifecycle.dirty = true;
        })
    }

    fn native_input_snapshot(
        &self,
        state: &AppModel,
        path: &ModelPath,
    ) -> Option<serde_json::Value> {
        let (state, _) = ui::doc::document_ref(state, path);
        let caret = self.inner.caret();
        let region = self.inner.region();
        // Body text reads only the caret block. Running-region text would
        // materialize an entire region, so omit it from this diagnostic path.
        let caret_text = matches!(region, Region::Body).then(|| self.inner.current_text(state));
        Some(serde_json::json!({
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
        }))
    }

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        // The toolbar is painted inside the document's authored chrome rather
        // than exposed as another accessibility child. Its deferred native
        // View requests still need the shared retained lifecycle.
        self.tool_bar.sync(state, root);
        let (state, root) = ui::doc::document_ref(state, root);
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
        self.sync_toolbar();
        self.lifecycle.dirty = true;
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        self.tool_bar.settle(&mut edits.reborrow(), path);
        if let Some(action) = self.tool_fired.take() {
            self.tool_clicked(edits, action);
        }
        self.drain_image_choice(edits);
        if let Some(scenario) = self.staged_scenario.take() {
            self.run_staged_scenario(edits, &scenario);
        }
        self.refresh_blocks(edits.doc());
        self.sync_toolbar();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.window_history.tree_rebuilt();
        taffy.fill_leaf(node_map, self)
    }

    /// Authored chrome keeps its retained toolkit children available to the
    /// host's normal resource, custody and invocation traversal.
    fn children(&self) -> Vec<&dyn Widget> {
        if self.styles_panel_full_window() {
            return Vec::new();
        }
        let mut children: Vec<&dyn Widget> = vec![&self.tool_bar];
        if let Some(button) = self.window_history.button() {
            children.push(button);
        }
        children
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.styles_panel_full_window() {
            return Vec::new();
        }
        let mut children: Vec<&mut (dyn Widget + '_)> = vec![&mut self.tool_bar];
        if let Some(button) = self.window_history.button_mut() {
            children.push(button);
        }
        children
    }

    fn accessibility_represents_child(&self, child_index: usize) -> bool {
        // The formatting toolbar is already authored into the document's
        // synthetic semantics; History remains its own retained control.
        child_index == 0
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut changed = std::mem::take(&mut self.retained_tree_stale);
        for child in self.children_mut() {
            changed |= child.take_structure_dirty();
        }
        changed
    }

    ui::rect_geometry!();

    fn needs_animation_frame(&self) -> bool {
        // Only the outline and inspector drawers move, while they open or
        // close.
        self.outline_drawer.is_animating()
            || self.inspector_drawer.is_animating()
            // A passing "Saved …" holds, fades and goes on its own.
            || self.acknowledger.current(ui::time::now()).is_some()
    }

    fn modal_capture(&self) -> Option<Rect> {
        if self.context_menu.is_open()
            || self.style_menu.is_open()
            || self.template_picker.is_open()
            || self.tool_bar.is_overflow_open()
            || self.line_spacing.is_open()
            || self.style_pick.is_open()
        {
            Some(self.rect)
        } else {
            None
        }
    }

    fn traps_focus(&self) -> bool {
        self.modal_capture().is_some()
    }

    fn draw(
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
        self.text_scale = f64::from(theme.text_scale.clamp(0.75, 2.0));
        let full = self.rect;
        let fg = self.draw_frame_chrome(scene, transform, text_renderer, theme, full);

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

        // `draw_page_sheets` pushes the canvas clip (so scrolled content never
        // paints over the toolbar/status bar) before drawing the sheets; popped
        // once every page-space pass below — sheets, pass 2, review cards,
        // footnotes, placeholder, overlays — has run.
        let page = self.draw_page_sheets(scene, transform, text_renderer, &fg, num_pages, sy);

        let (multi_sel, note_nums, spell_by_block) = self.prepare_pass2_inputs(blocks, &laid, &fg);

        let markers = self.draw_page_content(
            scene,
            &fg,
            text_renderer,
            &PageContentInput {
                laid: &laid,
                cache: &new_cache,
                multi_selection: multi_sel,
                spell_by_block: &spell_by_block,
                note_numbers: &note_nums,
            },
        );
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

        // Layout-edit overlay: draggable guides + live value labels for the
        // active mode (drawn over the content, still inside the page clip).
        if self.layout_mode != LayoutMode::None {
            self.draw_layout_overlay(scene, fg.ctf, text_renderer, page);
        }
        // Header/footer edit mode: veil the body + draw the band caret (checks
        // `self.inner.in_running_region()` itself). Drawn last so the veil
        // covers the body but the band + its caret stay crisp on top.
        self.draw_running_region_veil(scene, &fg, num_pages, sy);

        self.draw_review_column(
            scene,
            (transform, &fg),
            text_renderer,
            theme,
            presence,
            &markers,
        );

        scene.pop_layer();

        self.draw_scrollbar(scene, transform, fg.canvas);

        self.laid = laid;
        // Adopt this frame's cache (dropping the pruned entries for blocks that
        // no longer exist) so the next frame can reuse it.
        self.layout_cache = new_cache;

        self.draw_remote_overlays(
            scene,
            fg.ctf,
            fg.canvas_clip,
            text_renderer,
            theme,
            presence,
        );

        let lbar = Rect::new(
            fg.canvas_left,
            fg.layout_bar_top,
            fg.canvas_right,
            fg.content_bottom,
        );
        self.draw_layout_bar(scene, transform, text_renderer, theme, lbar);
        self.draw_drawers(scene, transform, text_renderer, theme);
    }

    // Where the caret and selection end up is what the edits bound to them
    // are built from.
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.route_event(event, ctx);
        self.resolve_bound_edits(ctx.state.doc());
    }

    /// Top-layer pass: paint the right-click context menu (when open) so it
    /// floats above the page + chrome. Delegates to the shared [`ContextMenu`].
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.staged.template_picker {
            self.staged.template_picker = false;
            self.open_template_picker();
        }
        if self.staged.toolbar_overflow {
            self.staged.toolbar_overflow = false;
            self.open_toolbar_overflow();
        }
        // Toast first, so an open menu stacks above it.
        self.draw_toast(scene, transform, text_renderer, theme);
        let toolbar_presence = PresenceCtx::empty();
        toolbar_presence
            .viewport
            .set(Some(self.floating_viewport()));
        Widget::draw_overlay(
            &mut self.tool_bar,
            scene,
            transform,
            text_renderer,
            theme,
            &toolbar_presence,
        );
        let viewport = self.floating_viewport();
        for menu in [
            &mut self.context_menu,
            self.template_picker.menu_mut(),
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
        self.template_picker
            .draw(scene, transform, text_renderer, theme, presence);
        self.style_menu
            .draw(scene, transform, text_renderer, theme, presence);
        if self.find.is_open() {
            self.find
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
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

    fn name(&self) -> &'static str {
        "WordProcessor"
    }

    // A test reaches the editor its framework app hosts.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    /// The word processor's local accessibility semantics: the editable
    /// `Role::Document` root. Blocks render directly, not as child widgets, so
    /// the full subtree comes from [`WordProcessorWidget::accessibility_tree`]
    /// rather than `children()`; convert the carrier into real synthetic
    /// children so AccessKit, AT-SPI and the built-in reader all receive it.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let mut tree = self.accessibility_tree();
        if !self.styles_panel_full_window() {
            tree.node = tree.node.with_name(t("a11y-document-editor"));
        }
        let mut node = tree.into_access_node();
        node.children.extend(self.toast.access_node());
        node.children.extend(
            self.acknowledger
                .current(ui::time::now())
                .map(|ack| ack.accessibility()),
        );
        Some(node)
    }

    fn perform_semantic_action(
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
        if !self
            .accessibility()
            .is_some_and(|node| published(&node, target))
        {
            return false;
        }
        ui::Widget::perform_action(self, action, &[ui::a11y::hash_key(&target)], state, path)
    }

    fn perform_action(
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
        let Some(tool) = self.toolbar.iter().find_map(|(tool, _)| {
            let id = format!("word-processor.toolbar:{}", tool_action_id(*tool));
            (ui::a11y::hash_key(&id) == target).then_some(*tool)
        }) else {
            return false;
        };

        // The AT route joins the exact same typed document-action/view path as
        // pointer activation. It must also establish the same history/autosave
        // boundary for mutations; otherwise screen-reader edits would silently
        // be second-class collaboration events.
        self.apply_tool(state, tool);
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
        true
    }

    fn test_id(&self) -> Option<&str> {
        Some("word_processor")
    }
    fn claims_raw_touch(&self, point: Point) -> bool {
        self.segmented_rects()
            .iter()
            .flatten()
            .any(|rect| rect.contains(point))
    }

    fn wants_pointer_capture(&self) -> bool {
        self.segmented_touch.is_active()
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.lifecycle.focused = focused;
        self.find.set_host_focused(focused);
        if !focused {
            // Blur any in-progress numeric edit; the next sync reloads the
            // persisted style values.
            self.focus_hosted(None, false);
        }
        self.lifecycle.dirty = true;
    }
    fn is_focused(&self) -> bool {
        self.lifecycle.focused
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
        if self.styles_panel_full_window() {
            self.num_inputs
                .iter()
                .find(|slot| {
                    Some(slot.field) == self.editing_field
                        && self.visible_styles_rect(slot.rect).is_some()
                })
                .and_then(|slot| Widget::ime_cursor_rect(&slot.input))
        } else if self.find.is_open() {
            self.find.ime_cursor_rect()
        } else {
            self.ime_rect
        }
    }
    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        if self.styles_panel_full_window() {
            self.num_inputs
                .iter()
                .find(|slot| {
                    Some(slot.field) == self.editing_field
                        && self.visible_styles_rect(slot.rect).is_some()
                })
                .and_then(|slot| Widget::ime_purpose(&slot.input))
        } else if self.find.is_open() {
            self.find.ime_purpose()
        } else {
            Some(winit::window::ImePurpose::Normal)
        }
    }
    fn take_action_palette_request(&mut self) -> Option<tilo_actions::ActionAddress> {
        self.action_palette_request.take()
    }
    fn needs_redraw(&self) -> bool {
        self.lifecycle.dirty
    }
    fn mark_drawn(&mut self) {
        self.lifecycle.dirty = false;
    }
    /// One self-drawn taffy leaf filling its tile: its size follows the
    /// parent alone, so only the first layout is a size change. Pages,
    /// toolbars and panels are positioned inside `self.rect` by hand.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

impl ui::presence::capability::PublishesPresence for WordProcessorWidget {
    /// Outbound collaboration presence: while focused, broadcast the local caret
    /// (or selection range) keyed to its block. Polled by the App after each event
    /// and fanned out as `CollabEvent::RemotePresenceUpdate`. Offsets are bytes, so
    /// the receiver feeds them straight into `RichTextLayout::caret_geometry`/
    /// `selection_rects`.
    fn presence_payload(&self) -> Option<(PresenceKey, ui::PresencePayload)> {
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
    /// block, the same part peers see; the excerpt is exactly that text, so
    /// what the agent reads is what it will act on.
    fn shared_selection(&self) -> Option<ui::shared_selection::SharedSelection> {
        use ui::shared_selection::SharedSelection;
        if !self.lifecycle.focused {
            return None;
        }
        let caret_block = &self.inner.caret.block;
        let block = self.blocks.iter().find(|b| &b.id == caret_block)?;
        let kind = <tilo_document::RichText as tilo_resource::Resource>::KIND;
        if self.inner.has_selection()
            && let Some((_, start, end)) = self
                .selection_blocks_cached()
                .into_iter()
                .find(|(b, _, _)| b == caret_block)
            && start < end
        {
            let text: String = block.text.chars().skip(start).take(end - start).collect();
            return Some(
                SharedSelection::of_text(&text)
                    .at(tilo_actions::SelectionPlace::range(
                        caret_block.clone(),
                        start,
                        end,
                    ))
                    .in_kind(kind),
            );
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

impl ui::resources::PublishesResources for WordProcessorWidget {
    // The document is the framework's publication; the editor adds only
    // what an edit answers its caller with.
    fn document_action_result(
        &self,
        doc: &AutoCommit,
        _address: &tilo_actions::ActionAddress,
        changes: &[automerge::Patch],
    ) -> Option<serde_json::Value> {
        Some(crate::semantic_read::changes::summary(doc, changes).into())
    }
}

/// The toolbar band is the window's header row: it holds the History control
/// the host offers, so the window draws no History bar above the editor.
impl ui::widget::WindowHeader for WordProcessorWidget {
    fn set_history_control(&mut self, control: Option<ui::widget::HistoryControl>) {
        if self.window_history.control() != control {
            self.window_history.set_control(control);
            self.lifecycle.dirty = true;
        }
        self.retained_tree_stale |= self.window_history.take_rebuilt();
    }
}

impl ui::widget::controls::ActionControls for WordProcessorWidget {
    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        for child in self.children_mut() {
            ui::widget::controls::install_control_custody(child, domain, attachment);
        }
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.children_mut()
            .into_iter()
            .find_map(ui::widget::controls::take_control_invocation)
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        let mut bindings = ui::widget::semantic_action_bindings(self.accessibility());
        self.for_each_child(&mut |child| {
            bindings.extend(ui::widget::controls::control_action_bindings(child));
        });
        bindings
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        for child in self.children_mut() {
            ui::widget::controls::refresh_control_action_availability(child, allowed);
        }
    }
}

impl ui::undo::LocalUndo for WordProcessorWidget {
    fn handle_local_undo_request(&mut self, _request: ui::undo::UndoRequest) -> bool {
        // The host resolves Undo before dispatching keyboard events to us.
        // An open menu must capture that path as well as ordinary editing keys.
        self.modal_capture().is_some()
    }
}

impl ui::collaboration::participants::Participants for WordProcessorWidget {
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
        WordProcessorWidget::set_participant_label(self, participant, label);
    }
    fn participant_presence(&self, doc: &AutoCommit) -> Vec<ui::ParticipantPresence> {
        WordProcessorWidget::participant_presence(self, doc)
    }
}

/// Lets the shell host the word processor in a tab directly, the same way
/// it hosts the terminal and the file explorer, instead of standing a
/// placeholder card in its place.
impl ui::widget::AppWidget for WordProcessorWidget {
    fn app_kind(&self) -> ui::widget::AppKind {
        // The toolkit's app-kind vocabulary has no separate "documents"
        // variant; a word processor is an editor of text, which is what
        // `Editor` names and what its tab glyph depicts.
        ui::widget::AppKind::Editor
    }

    fn app_title(&self) -> &'static str {
        "Document"
    }

    fn app_icon(&self) -> ui::widget::IconDescriptor {
        // Nerd Font document glyph, matching the convention the other
        // hosted apps use for their tab pill.
        ui::widget::IconDescriptor::Glyph("󰈬")
    }

    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        Some(ui::widget::AppKind::Editor)
    }
}

// Included into `app.rs` (shares its module + imports). Holds the input
// layer: pointer hit-testing against the chrome rects computed in
// `app_draw.rs`, drag lifecycle for the highlight / ink tools, and the
// keyboard map (tool hotkeys, zoom, page navigation, incremental search).
//
// All durable mutations go through `PdfDoc` into the shared CRDT so undo
// and live collaboration come for free; view state (zoom, scroll, active
// tool, the search query) stays local because it is per-peer.

impl PdfApp {
    // ---- small view-state helpers -----------------------------------

    /// Re-flow the chrome regions after a layout-affecting toggle and keep
    /// the zoom + scroll consistent with the new canvas size.
    fn reflow(&mut self) {
        self.recompute_regions();
        self.sync_viewport_frame();
        self.dirty = true;
    }

    fn set_tool(&mut self, t: Tool) {
        self.tool = t;
        // Switching tools drops any live text selection so a lingering
        // highlight doesn't sit under the next tool's work.
        if t != Tool::Select {
            self.clear_selection();
        }
        if t == Tool::Sign {
            self.sidebar_tab = SidebarTab::Signatures;
            if !self.show_sidebar {
                self.show_sidebar = true;
                self.reflow();
            }
        }
        self.dirty = true;
    }

    fn toggle_sidebar(&mut self) {
        self.show_sidebar = !self.show_sidebar;
        // On a narrow window the panels are mutually-exclusive overlays.
        if self.show_sidebar && self.overlay_mode {
            self.show_rail = false;
        }
        self.reflow();
    }

    fn toggle_rail(&mut self) {
        self.show_rail = !self.show_rail;
        if self.show_rail && self.overlay_mode {
            self.show_sidebar = false;
        }
        self.reflow();
    }

    fn open_search(&mut self) {
        if !self.search_active {
            self.search_active = true;
            self.reflow();
        }
        ui::widget::Widget::set_focus(&mut self.find_bar, true);
        self.find_bar.focus_query();
        if !self.query().is_empty() {
            self.recompute_matches();
        }
        self.dirty = true;
    }

    /// Close the find band. The query is kept, so opening find again offers
    /// it, selected.
    fn close_search(&mut self) {
        if self.search_active {
            self.search_active = false;
            let query = self.query().to_owned();
            self.find_bar = ui::widget::FindBar::new();
            if !query.is_empty() {
                self.find_bar.set_query(&query);
            }
            self.matches.clear();
            self.raster_matches.clear();
            self.reflow();
        }
    }

    fn fit_page(&mut self) {
        self.fit = FitMode::Page;
        self.recompute_zoom();
        self.clamp_scroll();
        self.dirty = true;
    }

    fn fit_width(&mut self) {
        self.fit = FitMode::Width;
        self.recompute_zoom();
        self.clamp_scroll();
        self.dirty = true;
    }

    fn prev_page(&mut self) {
        let p = self.current_page.saturating_sub(1).max(1);
        self.go_to_page(p);
    }

    fn next_page(&mut self) {
        self.go_to_page(self.current_page + 1);
    }

    /// Map a screen point into the page-point space of `page`, even when
    /// the point lies outside the card (so a highlight drag can run past
    /// the page edge and clamp naturally).
    fn page_point(&self, page: usize, p: Point) -> (f64, f64) {
        let origins = self.page_origins();
        let origin = origins
            .get(page.saturating_sub(1))
            .copied()
            .unwrap_or(Point::ZERO);
        (
            (p.x - origin.x) / self.zoom(),
            (p.y - origin.y) / self.zoom(),
        )
    }

    // ---- pointer hit-testing ----------------------------------------

    fn toolbar_hit(&self, p: Point) -> Option<ToolbarHit> {
        if !self.toolbar_rect.contains(p) {
            return None;
        }
        let lay = self.toolbar_layout();
        let has_doc = self.has_document;
        if lay.open.contains(p) {
            // A pending open owns the document slot; starting a second picker
            // would only create a competing request. Consume the press while
            // the button is visibly disabled.
            if self.pending_open.is_none() {
                return Some(ToolbarHit::Open);
            }
            return Some(ToolbarHit::Inert);
        }
        // Everything else acts on an open document; without one the
        // controls are disabled and a press on them does nothing.
        if !has_doc {
            return Some(ToolbarHit::Inert);
        }
        if lay.save.contains(p) {
            return Some(ToolbarHit::Save);
        }
        for (t, r) in &lay.tools {
            if r.contains(p) {
                return Some(ToolbarHit::Tool(*t));
            }
        }
        if self.tool.annotates() {
            for (s, r) in &lay.swatches {
                if r.contains(p) {
                    return Some(ToolbarHit::Swatch(*s));
                }
            }
        }
        if lay.zoom_out.contains(p) {
            return Some(ToolbarHit::ZoomOut);
        }
        if lay.zoom_in.contains(p) {
            return Some(ToolbarHit::ZoomIn);
        }
        if lay.zoom.contains(p) {
            return Some(ToolbarHit::ZoomValue);
        }
        if lay.fit.contains(p) {
            return Some(ToolbarHit::Fit);
        }
        if lay.search.contains(p) {
            return Some(ToolbarHit::Search);
        }
        if lay.toggle_rail.contains(p) {
            return Some(ToolbarHit::ToggleRail);
        }
        if lay.toggle_sidebar.contains(p) {
            return Some(ToolbarHit::ToggleSidebar);
        }
        // A click anywhere else on the toolbar is still "consumed" so it
        // doesn't fall through to the canvas behind it.
        Some(ToolbarHit::Inert)
    }

    fn apply_toolbar(&mut self, hit: &ToolbarHit, state: &mut AutoCommit) {
        match hit {
            ToolbarHit::Tool(t) => self.set_tool(*t),
            ToolbarHit::Swatch(s) => {
                self.swatch = *s;
                if let Some(id) = self.selected_anno.clone() {
                    self.perform_action(
                        state,
                        &crate::actions::set_annotation_color_action(&id, *s),
                    );
                    self.reload(state);
                }
                self.dirty = true;
            }
            ToolbarHit::ZoomIn => self.zoom_by(1.15),
            ToolbarHit::ZoomOut => self.zoom_by(1.0 / 1.15),
            ToolbarHit::Fit => {
                // Fit page → fit width; fit width or a custom zoom → fit page.
                match self.fit {
                    FitMode::Page => self.fit_width(),
                    _ => self.fit_page(),
                }
            }
            ToolbarHit::Search => {
                if self.search_active {
                    self.close_search();
                } else {
                    self.open_search();
                }
            }
            ToolbarHit::ToggleRail => self.toggle_rail(),
            ToolbarHit::ToggleSidebar => self.toggle_sidebar(),
            ToolbarHit::Open => self.request_open_via_portal(),
            ToolbarHit::Save => self.request_save_via_portal(),
            // The stepper's value is edited in place by the stepper itself.
            ToolbarHit::ZoomValue | ToolbarHit::Inert => {}
        }
    }

    /// Run `f` with an event context for a control the reader positions
    /// itself (the find bar, the zoom stepper, an inline field). A throwaway
    /// leaf satisfies the context's layout node; local controls never write
    /// the passed document.
    fn with_local_ctx<R>(
        scale: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        f: impl FnOnce(&mut ui::widget::EventCtx<'_>) -> R,
    ) -> Option<R> {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).ok()?;
        let mut handled = false;
        let mut focus_target = None;
        Some(f(&mut ui::widget::EventCtx {
            scale_factor: scale,
            modifiers: &modifiers,
            state,
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: node,
            taffy: &taffy,
        }))
    }

    /// Feed one event to the find bar, then carry what it asked for (a new
    /// query, an option, a step, closing) to the document.
    fn route_find_bar(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
    ) -> bool {
        let bar = &mut self.find_bar;
        let took = Self::with_local_ctx(scale, modifiers, state, |ctx| bar.route(event, ctx))
            .unwrap_or(false);
        self.drive_find_bar();
        took
    }

    /// Carry the find bar's pending requests to the document; close the band
    /// when the bar asked to close.
    fn drive_find_bar(&mut self) {
        let mut bar = std::mem::take(&mut self.find_bar);
        bar.drive(self);
        let close = bar.close_requested();
        self.find_bar = bar;
        if close {
            self.close_search();
        }
        self.dirty = true;
    }

    /// Feed one event to the zoom stepper and apply a value the person
    /// committed (typed and confirmed, or stepped).
    fn route_zoom_input(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
    ) {
        let input = &mut self.zoom_input;
        let _ = Self::with_local_ctx(scale, modifiers, state, |ctx| {
            input.handle_event(event, ctx);
        });
        if let Some(percent) = self.zoom_input.take_changed() {
            self.set_custom_zoom(percent / 100.0);
        }
        self.dirty = true;
    }

    /// Whether `p` is over the toolbar's zoom stepper.
    fn zoom_rect_contains(&self, p: Point) -> bool {
        self.has_document && self.toolbar_layout().zoom.contains(p)
    }

    /// Whether `p` is over the page rail's resize grip: its trailing edge.
    fn on_rail_grip(&self, p: Point) -> bool {
        let rail = self.rail_rect;
        let grip = f64::from(self.layout_theme.tokens.spacing.s1);
        self.show_rail && (p.x - rail.x1).abs() <= grip && p.y >= rail.y0 && p.y <= rail.y1
    }

    /// Resolve the most specific captured sidebar hit (smallest rect wins,
    /// so the resolve-chip beats the comment card it sits inside).
    fn apply_sidebar(&mut self, p: Point, state: &mut AutoCommit) -> bool {
        if !self.show_sidebar || !self.sidebar_rect.contains(p) {
            return false;
        }
        // In draw mode, a press inside the signature pad begins a freehand
        // stroke (tracked through CursorMoved / release) rather than hitting a
        // control beneath it.
        if self.sidebar_tab == SidebarTab::Signatures
            && self.sig_mode == SigMode::Draw
            && self.sig_pad_rect != Rect::ZERO
            && self.sig_pad_rect.contains(p)
        {
            self.sig_stroke = Some(vec![self.sig_pad_norm(p)]);
            self.dirty = true;
            return true;
        }
        let mut chosen: Option<SidebarHit> = None;
        let mut best_area = f64::MAX;
        for (hit, r) in &self.sidebar_hits {
            if r.contains(p) {
                let a = r.area();
                if a < best_area {
                    best_area = a;
                    chosen = Some(hit.clone());
                }
            }
        }
        let Some(hit) = chosen else {
            // Empty sidebar space still swallows the click.
            return true;
        };
        match hit {
            SidebarHit::Tab(t) => {
                // Each review panel owns a distinct scroll range. Starting a
                // newly opened panel at its beginning avoids carrying a long
                // comment-thread offset into Contents (or vice versa).
                if self.sidebar_tab != t {
                    self.sidebar_scroll = 0.0;
                    self.sidebar_reveal = None;
                }
                self.sidebar_tab = t;
            }
            SidebarHit::Outline(page) => self.go_to_page(page),
            SidebarHit::Comment(id) => {
                if let Some(c) = self.comments.iter().find(|c| c.id == id) {
                    let page = c.page;
                    self.selected_anno = c.anno_id.clone();
                    self.go_to_page(page);
                }
            }
            SidebarHit::EditComment(id) => {
                if let Some(c) = self.comments.iter().find(|c| c.id == id) {
                    let body = c.body.clone();
                    self.selected_anno = c.anno_id.clone();
                    self.begin_edit(EditTarget::CommentBody(id), &body);
                }
            }
            SidebarHit::ReplyField(id) => {
                self.begin_edit(EditTarget::Reply(id), "");
            }
            SidebarHit::EditSigner => {
                let cur = self.signer_name.clone();
                self.begin_edit(EditTarget::Signer, &cur);
            }
            SidebarHit::Resolve(id) => {
                let cur = self
                    .comments
                    .iter()
                    .find(|c| c.id == id)
                    .is_some_and(|c| c.resolved);
                self.perform_action(
                    state,
                    &crate::actions::set_comment_resolved_action(&id, !cur),
                );
                self.reload(state);
            }
            SidebarHit::DeleteComment(id) => {
                self.delete_target(&DeleteTarget::Comment(id), state);
            }
            SidebarHit::AddSignature => self.set_tool(Tool::Sign),
            SidebarHit::SigMode(mode) => {
                // Switching between typing and drawing keeps whatever the other
                // mode composed; only the in-progress stroke is dropped.
                self.sig_mode = mode;
                self.sig_stroke = None;
            }
            SidebarHit::SigClear => {
                self.sig_strokes.clear();
                self.sig_stroke = None;
            }
            SidebarHit::SigJump(i) => {
                if let Some(s) = self.signatures.get(i) {
                    let page = s.page;
                    self.selected_sig = Some(s.id.clone());
                    self.selected_anno = None;
                    self.go_to_page(page);
                }
            }
            SidebarHit::DeleteSignature(id) => {
                self.delete_target(&DeleteTarget::Signature(id), state);
            }
        }
        self.dirty = true;
        true
    }

    /// A click on the floating page bubble: its chevrons step pages. Hit-tests
    /// the rects the bubble captured this frame, so a click only lands when the
    /// bubble is actually visible.
    fn apply_page_bubble(&mut self, p: Point) -> bool {
        if self.bubble_rect == Rect::ZERO || !self.bubble_rect.contains(p) {
            return false;
        }
        if self.bubble_prev.contains(p) {
            self.prev_page();
        } else if self.bubble_next.contains(p) {
            self.next_page();
        }
        // Keep the indicator up right after a nav click.
        self.page_bubble.poke(ui::time::now());
        self.dirty = true;
        true
    }

    /// Pointer-down inside the page canvas: dispatch by the active tool.
    fn apply_canvas_press(&mut self, p: Point, state: &mut AutoCommit) -> bool {
        if !self.canvas_rect.contains(p) {
            return false;
        }
        // Empty state: the only affordance is the centred "Open a PDF…" button.
        if !self.has_document {
            if self.pending_open.is_none() && self.empty_open_button_rect().contains(p) {
                self.request_open_via_portal();
            }
            return true;
        }
        if let Some((page, pt)) = self.hit_page(p) {
            match self.tool {
                // Select arms one of three gestures. Over real text on a raster
                // page it begins a text selection; otherwise it is a pan-or-click
                // (a press that doesn't move is a selection of markup under it; a
                // press that drags pans the canvas).
                Tool::Select => {
                    if let Some((id, nx, ny)) = self.note_marker_at(page, pt) {
                        // A press on a sticky-note marker begins a reposition drag
                        // (draggable annotation markers), taking priority over text
                        // selection or a pan. A press that doesn't move still just
                        // selects the note (handled on release).
                        self.clear_selection();
                        self.selected_anno = Some(id.clone());
                        self.marker_drag = Some(ui::MarkerDrag::begin(id, pt, (nx, ny)));
                    } else if self.shift_down && self.raster_active() {
                        // Shift + Select drags a rubber-band image-region marquee
                        // to copy; it never pans or selects text.
                        self.clear_selection();
                        self.region = Some(RegionSelect {
                            page,
                            start: pt,
                            cur: pt,
                        });
                    } else if self.raster_active() && self.begin_selection(page, pt, p) {
                        // Text selection armed; motion extends it, release ends it.
                    } else {
                        self.clear_selection();
                        self.pan = Some(PanState {
                            start: p,
                            scroll_x0: self.scroll_x(),
                            scroll_y0: self.scroll_y(),
                            moved: false,
                        });
                    }
                    self.dirty = true;
                }
                Tool::Highlight | Tool::Ink => {
                    self.drag = Some(DragState {
                        tool: self.tool,
                        page,
                        start: pt,
                        cur: pt,
                        points: vec![pt],
                    });
                    self.dirty = true;
                }
                Tool::Note => self.create_note(state, page, pt),
                Tool::Text => {
                    // On an existing box: select it and arm a move (a tap
                    // edits it, on release); elsewhere: place a new box.
                    match self
                        .note_marker_at(page, pt)
                        .filter(|(id, _, _)| self.is_free_text(id))
                    {
                        Some((id, x, y)) => {
                            self.clear_selection();
                            self.selected_anno = Some(id.clone());
                            self.marker_drag = Some(ui::MarkerDrag::begin(id, pt, (x, y)));
                        }
                        None => self.create_free_text(state, page, pt),
                    }
                    self.dirty = true;
                }
                Tool::Sign => self.place_signature(state, page, pt),
            }
        } else {
            // Empty canvas: with Select, start a pan; otherwise clear.
            self.clear_selection();
            if matches!(self.tool, Tool::Select) {
                self.pan = Some(PanState {
                    start: p,
                    scroll_x0: self.scroll_x(),
                    scroll_y0: self.scroll_y(),
                    moved: false,
                });
            } else {
                self.selected_anno = None;
                self.selected_sig = None;
            }
            self.dirty = true;
        }
        true
    }

    /// Resolve a right-click into a context menu over a deletable item.
    fn open_context_menu(&mut self, p: Point) -> bool {
        if let Some((page, pt)) = self.hit_page(p)
            && let Some(target) = self.delete_target_at(page, pt)
        {
            self.context_menu = Some(ContextMenu::delete(target, p, self.rect));
            self.dirty = true;
            return true;
        }
        // A right-click elsewhere just dismisses any open menu.
        if self.context_menu.take().is_some() {
            self.dirty = true;
        }
        false
    }

    // ---- top-level dispatch -----------------------------------------

    fn on_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        self.shift_down = modifiers.state().shift_key();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.on_cursor_moved(event, p, scale_factor, modifiers, state);
                if self.search_active {
                    self.route_find_bar(event, scale_factor, modifiers, state);
                }
                if self.has_document {
                    self.route_zoom_input(event, scale_factor, modifiers, state);
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_press(event, scale_factor, modifiers, state, handled);
            }

            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.search_active {
                    self.route_find_bar(event, scale_factor, modifiers, state);
                }
                if self.zoom_input.is_editing() {
                    self.route_zoom_input(event, scale_factor, modifiers, state);
                }
                self.on_left_release(event, scale_factor, modifiers, state, handled);
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                self.on_right_press(state, handled);
            }

            WindowEvent::MouseWheel { delta, .. } => {
                self.on_mouse_wheel(delta, scale_factor, modifiers, handled);
            }

            // Touchpad pinch: zoom toward the cursor. Upstream winit emits
            // `PinchGesture` only on its macOS/iOS backends; Tilo ships a
            // vendored winit fork whose Wayland backend binds
            // `zwp_pointer_gestures_v1` and delivers the same event, so a bare
            // two-finger pinch reaches this arm on Wayland too (wherever the
            // compositor advertises the protocol — Mutter does; the Tilo
            // compositor advertises it as well). Ctrl + two-finger scroll,
            // which arrives as the `MouseWheel` PixelDelta handled above,
            // remains a fallback for stacks without the gesture protocol.
            WindowEvent::PinchGesture { delta, .. } => {
                if delta.is_finite() && *delta != 0.0 {
                    self.zoom_at(1.0 + *delta, self.last_mouse);
                    *handled = true;
                }
            }

            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                self.on_key(event, key_event, modifiers, scale_factor, state, handled);
            }

            WindowEvent::Ime(_) if self.zoom_input.is_editing() => {
                self.route_zoom_input(event, scale_factor, modifiers, state);
                *handled = true;
            }

            WindowEvent::Ime(_) if self.search_active => {
                self.route_find_bar(event, scale_factor, modifiers, state);
                *handled = true;
            }

            _ => {}
        }
    }

    /// `WindowEvent::CursorMoved`: routes to whichever live drag currently
    /// owns the pointer (signature stroke, inline-editor selection, marker
    /// drag, rail resize, rail reorder, region marquee, text selection,
    /// canvas pan, or an in-progress highlight/ink drag), first match wins.
    fn on_cursor_moved(
        &mut self,
        event: &WindowEvent,
        p: Point,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
    ) {
        self.last_mouse = p;
        if self.on_cursor_moved_chrome_drags(event, p, scale_factor, modifiers, state) {
            return;
        }
        self.on_cursor_moved_page_drags(p);
    }

    /// The signature pad, the inline editor, a sticky-note marker drag, and
    /// the rail resize grip — each owns the pointer exclusively while
    /// active. Returns whether one of them already consumed the move.
    fn on_cursor_moved_chrome_drags(
        &mut self,
        event: &WindowEvent,
        p: Point,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
    ) -> bool {
        // Drawing a signature on the pad: extend the in-progress stroke.
        if self.sig_stroke.is_some() {
            let n = self.sig_pad_norm(p);
            if let Some(st) = self.sig_stroke.as_mut() {
                let far = st
                    .last()
                    .is_none_or(|l| (l.0 - n.0).abs() + (l.1 - n.1).abs() > 0.004);
                if far {
                    st.push(n);
                }
            }
            self.dirty = true;
            return true;
        }

        // Route motion to the inline editor so its hover tracks (for a
        // click-to-caret) and a press-drag extends its selection.
        // Consume the move while it is actively drag-selecting so the
        // page doesn't pan under the selection.
        if self.editing.is_some() {
            self.drive_editor(event, scale_factor, modifiers, state);
            if self.editing.as_ref().is_some_and(Editing::is_dragging) {
                self.dirty = true;
                return true;
            }
        }

        // Dragging a sticky-note marker: move it live (local only; the
        // CRDT is written once on release so a drag is one undo step).
        if let Some(mut mdrag) = self.marker_drag.take() {
            if let Some(page) = self.anno_page(&mdrag.id) {
                let pt = self.page_point(page, p);
                let (nx, ny) = mdrag.update(pt);
                self.move_note_local(&mdrag.id, nx, ny);
            }
            self.marker_drag = Some(mdrag);
            self.dirty = true;
            return true;
        }

        // Rail resize drag takes priority over everything else.
        if let Some(grip_x) = self.rail_drag {
            let _ = grip_x;
            let (lo, hi) = self.rail_width_range();
            let new_w = (p.x - self.rect.x0).clamp(lo, hi);
            self.rail_w = new_w;
            self.reflow();
            self.dirty = true;
            return true;
        }

        false
    }

    /// The page-rail reorder drag, an image-region marquee, a text-selection
    /// drag, canvas pan, and an in-progress highlight/ink drag — the
    /// page-level motion handling once no chrome drag owns the pointer.
    fn on_cursor_moved_page_drags(&mut self, p: Point) {
        // A live page-rail reorder drag (or an open rail menu) owns the
        // pointer while it lasts.
        if self.show_rail && (self.page_sidebar.is_pending_drag() || self.page_sidebar.menu_open())
        {
            if self.page_sidebar.on_motion(p) {
                self.dirty = true;
            }
            return;
        }

        // Image-region marquee drag (Shift + Select).
        if let Some(region) = self.region {
            let pt = self.page_point(region.page, p);
            if let Some(r) = self.region.as_mut() {
                r.cur = pt;
            }
            self.dirty = true;
            return;
        }

        // Text selection drag (Select tool over real text). Map the
        // pointer onto the selection's page even past the card edge so
        // the drag can reach the end of a line.
        if let Some(sel) = self.selection
            && sel.dragging
        {
            let pt = self.page_point(sel.page, p);
            self.update_selection(sel.page, pt);
            return;
        }

        // Drag-to-pan (Select tool).
        if let Some(pan) = self.pan {
            let dx = p.x - pan.start.x;
            let dy = p.y - pan.start.y;
            if dx.hypot(dy) > PAN_SLOP {
                self.viewport
                    .set_scroll(pan.scroll_x0 - dx, pan.scroll_y0 - dy);
                self.current_page = self.nearest_page();
                if let Some(pan) = self.pan.as_mut() {
                    pan.moved = true;
                }
            }
            self.dirty = true;
            return;
        }

        if let Some(page) = self.drag.as_ref().map(|d| d.page) {
            let pt = self.page_point(page, p);
            if let Some(d) = self.drag.as_mut() {
                d.cur = pt;
                if d.tool == Tool::Ink {
                    let push = d
                        .points
                        .last()
                        .is_none_or(|l| (l.0 - pt.0).hypot(l.1 - pt.1) > 1.5);
                    if push {
                        d.points.push(pt);
                    }
                }
            }
        }
        self.dirty = true;
    }

    /// `WindowEvent::MouseInput` (left, pressed): the inline editor / open
    /// menus / chrome first, then the page rail / overlay scrim / selection
    /// HUD / canvas — first match consumes the click.
    fn on_left_press(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        let p = self.last_mouse;
        if self.on_left_press_editor_and_menus(event, p, scale_factor, modifiers, state, handled) {
            return;
        }
        self.on_left_press_rail_and_canvas(p, state, handled);
    }

    /// The inline editor, an open context menu, the toolbar, the find bar,
    /// the page bubble and the sidebar — each intercepts a left press before
    /// it reaches the rail or the canvas. Returns whether one of them already
    /// consumed the click.
    fn on_left_press_editor_and_menus(
        &mut self,
        event: &WindowEvent,
        p: Point,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        // A click inside the active inline editor keeps focus; a click
        // anywhere else commits the edit first, then falls through so
        // the click still performs its normal action.
        if self.editing.is_some() {
            if self.edit_rect.contains(p) {
                // Place the caret (or begin a drag-selection) in the
                // inline editor at the click point.
                self.drive_editor(event, scale_factor, modifiers, state);
                self.dirty = true;
                *handled = true;
                return true;
            }
            self.commit_edit(state);
        }
        // A press anywhere else ends typing a zoom value (committing it).
        let on_zoom = self.zoom_rect_contains(p);
        if self.zoom_input.is_editing() && !on_zoom {
            let input = &mut self.zoom_input;
            let _ = Self::with_local_ctx(scale_factor, modifiers, state, |_| {
                ui::widget::Widget::set_focus(input, false);
            });
            if let Some(percent) = self.zoom_input.take_changed() {
                self.set_custom_zoom(percent / 100.0);
            }
        }
        if let Some(hit) = self.toolbar_hit(p) {
            if matches!(hit, ToolbarHit::ZoomValue) {
                ui::widget::Widget::set_focus(&mut self.zoom_input, true);
                self.route_zoom_input(event, scale_factor, modifiers, state);
            }
            self.apply_toolbar(&hit, state);
            *handled = true;
            return true;
        }
        if self.search_active {
            let inside = self.search_rect.contains(p);
            ui::widget::Widget::set_focus(&mut self.find_bar, inside);
            if inside {
                self.route_find_bar(event, scale_factor, modifiers, state);
                *handled = true;
                return true;
            }
        }
        if self.apply_page_bubble(p) {
            *handled = true;
            return true;
        }
        if self.apply_sidebar(p, state) {
            *handled = true;
            return true;
        }
        false
    }

    /// The page rail (resize grip, then the shared rail widget), the
    /// narrow-window overlay scrim, the floating selection HUD, and finally
    /// the canvas itself — the rest of a left press once the editor/menu/
    /// chrome stage above has passed it through.
    fn on_left_press_rail_and_canvas(
        &mut self,
        p: Point,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        // The page rail: its resize grip (a thin strip on the right
        // edge) first, then the shared rail widget (select, begin a
        // reorder drag, the "Import…" button, or a click that dismisses
        // an open menu).
        if self.show_rail {
            let rail = self.rail_rect;
            if self.on_rail_grip(p) {
                self.rail_drag = Some(p.x);
                self.dirty = true;
                *handled = true;
                return;
            }
            if rail.contains(p) || self.page_sidebar.menu_open() {
                self.page_sidebar.on_press(p);
                self.apply_sidebar_events(state);
                self.dirty = true;
                *handled = true;
                return;
            }
        }
        // On a narrow window a click on the scrim (canvas area not
        // covered by an open overlay panel) dismisses the panels.
        if self.overlay_mode
            && (self.show_rail || self.show_sidebar)
            && self.canvas_rect.contains(p)
        {
            self.show_rail = false;
            self.show_sidebar = false;
            self.reflow();
            *handled = true;
            return;
        }
        // The floating selection HUD (Copy / highlight / Note) intercepts
        // the click before it reaches the page — the same bar the ebook
        // reader draws, via the shared `ui` component.
        if let Some(action) = self.sel_hud.as_ref().and_then(|h| h.hit(p)) {
            self.apply_hud_action(action, state);
            self.dirty = true;
            *handled = true;
            return;
        }
        if self.sel_hud.as_ref().is_some_and(|h| h.contains(p)) {
            // A tap on the bar body (between controls) is swallowed so it
            // doesn't fall through and clear the selection.
            *handled = true;
            return;
        }
        if self.apply_canvas_press(p, state) {
            *handled = true;
        }
    }

    /// `WindowEvent::MouseInput` (left, released): finishes whichever drag or
    /// pending gesture from the press is still live (signature stroke,
    /// inline-editor selection, marker drag, region marquee, rail reorder,
    /// text selection, pan, or a highlight/ink drag).
    fn on_left_release(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        // Finish a signature pad stroke: commit it if it has real ink.
        if let Some(stroke) = self.sig_stroke.take() {
            if stroke.len() >= 2 {
                self.sig_strokes.push(stroke);
            }
            self.dirty = true;
            *handled = true;
            return;
        }
        // End an inline-editor drag-selection.
        if self.editing.as_ref().is_some_and(Editing::is_dragging) {
            self.drive_editor(event, scale_factor, modifiers, state);
            self.dirty = true;
            *handled = true;
            return;
        }
        // End a sticky-note marker drag: persist the new anchor once (a
        // no-move release is a plain select, already applied on press).
        if let Some(mdrag) = self.marker_drag.take() {
            if self.is_free_text(&mdrag.id) {
                // A free text box: a drag moves it, a tap edits its text.
                if mdrag.moved() {
                    self.persist_free_text_box(state, &mdrag.id);
                    self.reload(state);
                } else {
                    self.edit_free_text(&mdrag.id);
                }
            } else if mdrag.moved() {
                if let Some((x, y)) = self.note_pos(&mdrag.id) {
                    self.perform_action(state, &{
                        let (ux, uy) = self
                            .page_map(self.anno_page(&mdrag.id).unwrap_or(1))
                            .card_to_user((x, y));
                        crate::actions::set_annotation_position_user_action(&mdrag.id, ux, uy)
                    });
                    self.reload(state);
                }
            } else {
                // A tap on the marker reveals its comment thread.
                if let Some(c) = self
                    .comments
                    .iter()
                    .find(|c| c.anno_id.as_deref() == Some(mdrag.id.as_str()))
                {
                    let cid = c.id.clone();
                    self.focus_sidebar(SidebarTab::Comments, cid);
                }
            }
            self.dirty = true;
            *handled = true;
            return;
        }
        // End an image-region marquee: crop + copy the region.
        if self.region.is_some() {
            self.copy_region();
            self.region = None;
            *handled = true;
            return;
        }
        // Commit a page-rail reorder drag (or clear its pending press).
        if self.page_sidebar.is_pending_drag() {
            if self.page_sidebar.on_release() {
                self.dirty = true;
            }
            self.apply_sidebar_events(state);
            *handled = true;
            return;
        }
        if self.selection.is_some_and(|s| s.dragging) {
            // End a text-selection drag. A bare click (no drag) selects
            // nothing textual, so also let it pick markup underneath.
            let was_active = self.selection.is_some_and(|s| s.is_active());
            self.finish_selection();
            if !was_active && let Some((page, pt)) = self.hit_page(self.last_mouse) {
                self.click_page(state, page, pt);
            }
            *handled = true;
        } else if self.rail_drag.take().is_some() {
            self.dirty = true;
            *handled = true;
        } else if let Some(pan) = self.pan.take() {
            // A press that never moved is a selection click.
            if !pan.moved {
                if let Some((page, pt)) = self.hit_page(pan.start) {
                    self.click_page(state, page, pt);
                } else {
                    self.selected_anno = None;
                    self.selected_sig = None;
                }
            }
            self.dirty = true;
            *handled = true;
        } else if self.drag.is_some() {
            self.commit_drag(state);
            *handled = true;
        }
    }

    /// `WindowEvent::MouseInput` (right, pressed): the page rail's
    /// duplicate/move/delete menu, else the canvas context menu.
    fn on_right_press(&mut self, state: &mut AutoCommit, handled: &mut bool) {
        let p = self.last_mouse;
        // Right-click a page thumbnail → the shared rail's duplicate /
        // move / delete menu.
        if self.show_rail && self.rail_rect.contains(p) {
            self.page_sidebar.on_right_press(p);
            self.apply_sidebar_events(state);
            self.dirty = true;
            *handled = true;
            return;
        }
        if self.canvas_rect.contains(p) {
            self.open_context_menu(p);
            *handled = true;
        }
    }

    /// `WindowEvent::MouseWheel`: the page rail / sidebar scroll it under the
    /// pointer, Ctrl+wheel zooms, otherwise it pans the canvas.
    fn on_mouse_wheel(
        &mut self,
        delta: &MouseScrollDelta,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        let p = self.last_mouse;
        let (raw_x, raw_y) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * 56.0, f64::from(*y) * 56.0),
            MouseScrollDelta::PixelDelta(pos) => (pos.x / scale_factor, pos.y / scale_factor),
        };
        let mods = modifiers.state();
        let ctrl = mods.control_key() || mods.super_key();
        tracing::debug!(
            kind = match delta {
                MouseScrollDelta::LineDelta(..) => "line",
                MouseScrollDelta::PixelDelta(..) => "pixel",
            },
            raw_x,
            raw_y,
            ctrl,
            shift = mods.shift_key(),
            "pdf MouseWheel"
        );

        // Wheel over the page rail scrolls its thumbnail list.
        if self.show_rail && self.rail_rect.contains(p) {
            if self.page_sidebar.on_wheel(-raw_y) {
                self.dirty = true;
            }
            *handled = true;
            return;
        }

        // Wheel over the sidebar scrolls its list.
        if self.show_sidebar && self.sidebar_rect.contains(p) {
            // (sidebar_max_scroll needs the text renderer, so just
            // nudge and let the next draw clamp it.)
            self.sidebar_scroll = (self.sidebar_scroll - raw_y).max(0.0);
            self.dirty = true;
            *handled = true;
            return;
        }

        if ctrl {
            // Ctrl+wheel zooms, centred on the cursor.
            let factor = if raw_y > 0.0 {
                1.0 + raw_y / 240.0
            } else {
                1.0 / (1.0 - raw_y / 240.0)
            };
            self.zoom_at(factor, p);
            *handled = true;
            return;
        }

        // Shift+wheel (or a horizontal wheel) pans horizontally;
        // otherwise vertical. Either axis only moves if it can. The
        // clamp lives in the shared kernel's `pan_by`.
        if mods.shift_key() && self.is_overflowing_x() {
            self.viewport.pan_by(-raw_y, 0.0);
        } else {
            self.viewport.pan_by(-raw_x, -raw_y);
        }
        self.current_page = self.nearest_page();
        self.dirty = true;
        *handled = true;
    }

    /// Feed one raw event into a hand-positioned, local [`TextInput`]. A
    /// throwaway leaf satisfies the `Widget` signature; local fields never
    /// write the passed document.
    fn drive_local_input(
        input: &mut TextInput,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
    ) {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return;
        };
        input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: scale,
                modifiers: &modifiers,
                state,
                path: &automerge::ROOT,
                handled: &mut false,
                focus_target: &mut None,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    fn drive_editor(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
    ) {
        match self.editing.as_mut().map(|e| &mut e.input) {
            Some(EditInput::Plain(input)) => {
                Self::drive_local_input(input, event, scale, modifiers, state);
            }
            Some(EditInput::Rich(field)) => {
                let mut handled = false;
                field.handle_event(
                    event,
                    scale,
                    &modifiers,
                    state,
                    &automerge::ROOT,
                    &mut handled,
                );
                field.sync(state, &automerge::ROOT);
            }
            None => {}
        }
    }

    fn on_key(
        &mut self,
        event: &WindowEvent,
        key_event: &winit::event::KeyEvent,
        modifiers: winit::event::Modifiers,
        scale: f64,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        if self.on_key_editing(event, key_event, modifiers, scale, state, handled) {
            return;
        }

        let mods = modifiers.state();
        let ctrl = mods.control_key() || mods.super_key();

        if self.on_key_search(event, modifiers, ctrl, scale, state, handled) {
            return;
        }
        if self.on_key_ctrl_shortcuts(key_event, ctrl, handled) {
            return;
        }
        // With a free text box selected, `>` and `<` step its text size.
        if !ctrl
            && let Key::Character(c) = &key_event.logical_key
            && let Some(id) = self
                .selected_anno
                .clone()
                .filter(|id| self.is_free_text(id))
        {
            let delta = match c.as_str() {
                ">" => Some(2.0),
                "<" => Some(-2.0),
                _ => None,
            };
            if let Some(delta) = delta {
                self.resize_free_text(state, &id, delta);
                *handled = true;
                return;
            }
        }
        self.on_key_navigation(key_event, state, handled);
    }

    /// While an inline text field is focused it owns the keyboard, so tool
    /// hotkeys and page navigation don't fire mid-sentence. The field is a
    /// shared `TextInput`: Escape cancels; every other key routes to the
    /// widget — caret motion, selection (shift+arrows), clipboard
    /// (Ctrl+C/V/X), and Enter (which submits → commit). Returns whether it
    /// consumed this key.
    fn on_key_editing(
        &mut self,
        event: &WindowEvent,
        key_event: &winit::event::KeyEvent,
        modifiers: winit::event::Modifiers,
        scale: f64,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        if self.editing.is_none() {
            return false;
        }
        if matches!(&key_event.logical_key, Key::Named(NamedKey::Escape)) {
            self.cancel_edit();
            *handled = true;
            return true;
        }
        self.drive_editor(event, scale, modifiers, state);
        let submitted = self.editing.as_mut().is_some_and(Editing::take_submit);
        if submitted {
            self.commit_edit(state);
        }
        self.dirty = true;
        *handled = true;
        true
    }

    /// While the find bar has focus it owns the keyboard (typing, Enter /
    /// Shift+Enter, F3, Escape); Ctrl+F refocuses its query. While a zoom
    /// value is being typed the stepper owns it. Returns whether either
    /// consumed this key.
    fn on_key_search(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        ctrl: bool,
        scale: f64,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        if self.zoom_input.is_editing() {
            self.route_zoom_input(event, scale, modifiers, state);
            *handled = true;
            return true;
        }
        if !self.search_active {
            return false;
        }
        let WindowEvent::KeyboardInput {
            event: key_event, ..
        } = event
        else {
            return false;
        };
        if ctrl
            && let Key::Character(c) = &key_event.logical_key
            && c.as_str().eq_ignore_ascii_case("f")
        {
            ui::widget::Widget::set_focus(&mut self.find_bar, true);
            self.find_bar.focus_query();
            self.dirty = true;
            *handled = true;
            return true;
        }
        if !ui::widget::Widget::is_focused(&self.find_bar) {
            return false;
        }
        self.route_find_bar(event, scale, modifiers, state);
        *handled = true;
        true
    }

    /// Ctrl/Cmd shortcuts: open, find, zoom (+ / − / 0), copy, save. Handled
    /// up front so the bare-key fallbacks below can keep their own meanings.
    /// Returns whether one of them consumed this key.
    fn on_key_ctrl_shortcuts(
        &mut self,
        key_event: &winit::event::KeyEvent,
        ctrl: bool,
        handled: &mut bool,
    ) -> bool {
        let Key::Character(c) = &key_event.logical_key else {
            return false;
        };
        if !ctrl {
            return false;
        }
        match c.as_str() {
            "o" | "O" => {
                self.request_open_via_portal();
                *handled = true;
                true
            }
            "f" | "F" => {
                self.open_search();
                *handled = true;
                true
            }
            "+" | "=" => {
                self.zoom_by(1.15);
                *handled = true;
                true
            }
            "-" | "_" => {
                self.zoom_by(1.0 / 1.15);
                *handled = true;
                true
            }
            "0" => {
                self.reset_zoom();
                *handled = true;
                true
            }
            "c" | "C" => {
                // Copy the live text selection, if any. Falls through
                // when nothing is selected so the key is a harmless no-op.
                if self.copy_selection() {
                    *handled = true;
                    true
                } else {
                    false
                }
            }
            "s" | "S" => {
                self.request_save_via_portal();
                *handled = true;
                true
            }
            _ => false,
        }
    }

    /// Bare-character tool/zoom/navigation hotkeys (no modifier held).
    /// Returns whether `key` matched one.
    fn on_key_char_shortcut(&mut self, key: &str) -> bool {
        match key {
            "1" => {
                self.set_tool(Tool::Select);
                true
            }
            "2" => {
                self.set_tool(Tool::Highlight);
                true
            }
            "3" => {
                self.set_tool(Tool::Ink);
                true
            }
            "4" => {
                self.set_tool(Tool::Note);
                true
            }
            "5" => {
                self.set_tool(Tool::Sign);
                true
            }
            "6" => {
                self.set_tool(Tool::Text);
                true
            }
            "+" | "=" => {
                self.zoom_by(1.15);
                true
            }
            "-" | "_" => {
                self.zoom_by(1.0 / 1.15);
                true
            }
            "0" => {
                self.reset_zoom();
                true
            }
            "w" | "W" => {
                self.fit_width();
                true
            }
            "b" | "B" => {
                self.toggle_sidebar();
                true
            }
            "[" => {
                self.prev_page();
                true
            }
            "]" | " " => {
                self.next_page();
                true
            }
            _ => false,
        }
    }

    /// Bare-key fallbacks once no modal input, search, or Ctrl shortcut has
    /// claimed the key: tool hotkeys, zoom/fit, page navigation and delete.
    fn on_key_navigation(
        &mut self,
        key_event: &winit::event::KeyEvent,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        match &key_event.logical_key {
            Key::Character(c) => {
                if self.on_key_char_shortcut(c.as_str()) {
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Escape) => {
                self.selected_anno = None;
                self.selected_sig = None;
                self.context_menu = None;
                self.page_sidebar.on_escape();
                self.clear_selection();
                self.status_note = None;
                self.dirty = true;
                *handled = true;
            }
            // Arrows scroll the document smoothly (a small nudge), not a whole
            // page jump. Page Up/Down (and Space) scroll a near-viewport-ful.
            Key::Named(NamedKey::ArrowDown) => {
                self.scroll_by(0.0, KEY_SCROLL_STEP);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.scroll_by(0.0, -KEY_SCROLL_STEP);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.scroll_by(-KEY_SCROLL_STEP, 0.0);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.scroll_by(KEY_SCROLL_STEP, 0.0);
                *handled = true;
            }
            Key::Named(NamedKey::PageDown | NamedKey::Space) => {
                let step = (self.canvas_rect.height() * 0.9).max(KEY_SCROLL_STEP);
                self.scroll_by(0.0, step);
                *handled = true;
            }
            Key::Named(NamedKey::PageUp) => {
                let step = (self.canvas_rect.height() * 0.9).max(KEY_SCROLL_STEP);
                self.scroll_by(0.0, -step);
                *handled = true;
            }
            Key::Named(NamedKey::Home) => {
                self.go_to_page(1);
                *handled = true;
            }
            Key::Named(NamedKey::End) => {
                self.go_to_page(self.doc.page_count());
                *handled = true;
            }
            Key::Named(NamedKey::Delete | NamedKey::Backspace) if self.delete_selected(state) => {
                *handled = true;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod event_tests {
    use super::*;
    use serial_test::serial;
    use winit::dpi::PhysicalPosition;
    use winit::event::{Modifiers, TouchPhase};

    fn sample_app() -> (PdfApp, ui::State) {
        let mut app = PdfApp::sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.measure_toolbar(
            &mut ui::text_render::TextRenderer::new(),
            &ui::theme::Theme::dark(),
        );
        app.recompute_regions();
        app.sync_viewport_frame();
        (app, st)
    }

    /// Synthesise the opaque dummy `DeviceId` winit uses in tests. It has no
    /// public constructor; a zeroed value is the sentinel every backend uses
    /// and the convention across this workspace's widget tests.
    fn dummy_device_id() -> winit::event::DeviceId {
        // SAFETY: DeviceId wraps a platform integer; zero is the "no device"
        // sentinel and carries no validity requirements.
        unsafe { std::mem::zeroed() }
    }

    /// A ctrl-held `Modifiers`, mirroring the workspace convention (see
    /// `ui::widget::command_palette`'s `ctrl_a_selects_query`).
    fn ctrl_mods() -> Modifiers {
        let mut s = winit::keyboard::ModifiersState::empty();
        s.set(winit::keyboard::ModifiersState::CONTROL, true);
        Modifiers::from(s)
    }

    /// Build a real `WindowEvent::KeyboardInput` for `key`. winit's
    /// `KeyEventExtra` is `pub(crate)`, so we zero the struct and set the
    /// public fields — the same trick the terminal + `modal_dialog` tests use.
    fn key_event(key: Key) -> WindowEvent {
        use winit::event::KeyEvent;
        use winit::keyboard::{KeyLocation, NativeKeyCode, PhysicalKey};
        // Real winit reports the committed text alongside a printable key; the
        // shared TextInput inserts from that `text` field, so mirror it (named
        // keys other than Space carry no text).
        let text = match &key {
            Key::Character(s) => Some(s.clone()),
            Key::Named(NamedKey::Space) => Some(winit::keyboard::SmolStr::new(" ")),
            _ => None,
        };
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = key;
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = text;
        WindowEvent::KeyboardInput {
            device_id: dummy_device_id(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn character(ch: &str) -> Key {
        Key::Character(ch.into())
    }

    fn centre(r: Rect) -> Point {
        Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1))
    }

    /// The reader plus the taffy node it is driven through: every event goes
    /// through `PdfApp::handle_event`, the same entry point the live widget
    /// tree drives.
    struct Rig {
        app: PdfApp,
        st: ui::State,
        taffy: TaffyTree<()>,
        node: NodeId,
    }

    impl Rig {
        fn new() -> Self {
            let (app, st) = sample_app();
            Self::with(app, st)
        }

        fn with(app: PdfApp, st: ui::State) -> Self {
            let mut taffy = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            Self {
                app,
                st,
                taffy,
                node,
            }
        }

        /// Feed one real event; returns whether it was handled.
        fn feed(&mut self, mods: Modifiers, ev: &WindowEvent) -> bool {
            let mut handled = false;
            let mut focus = None;
            self.app.handle_event(
                ev,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: &mut self.st.doc,
                    path: &self.st.root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: self.node,
                    taffy: &self.taffy,
                },
            );
            handled
        }

        fn move_to(&mut self, p: (f64, f64)) {
            self.feed(
                Modifiers::default(),
                &WindowEvent::CursorMoved {
                    device_id: dummy_device_id(),
                    position: PhysicalPosition::new(p.0, p.1),
                },
            );
        }

        fn button(&mut self, state: ElementState, button: MouseButton) {
            self.feed(
                Modifiers::default(),
                &WindowEvent::MouseInput {
                    device_id: dummy_device_id(),
                    state,
                    button,
                },
            );
        }

        fn press(&mut self) {
            self.button(ElementState::Pressed, MouseButton::Left);
        }

        fn release(&mut self) {
            self.button(ElementState::Released, MouseButton::Left);
        }

        fn key(&mut self, mods: Modifiers, key: Key) -> bool {
            self.feed(mods, &key_event(key))
        }

        fn wheel(&mut self, mods: Modifiers, delta: MouseScrollDelta) {
            self.feed(
                mods,
                &WindowEvent::MouseWheel {
                    device_id: dummy_device_id(),
                    delta,
                    phase: TouchPhase::Moved,
                },
            );
        }

        /// Arm `tool` by clicking its real toolbar button, and assert the
        /// click landed.
        fn arm_via_toolbar(&mut self, tool: Tool, why: &str) {
            let lay = self.app.toolbar_layout();
            let (_, rect) = lay.tools.iter().find(|(t, _)| *t == tool).unwrap();
            let btn = centre(*rect);
            self.move_to((btn.x, btn.y));
            self.press();
            assert_eq!(self.app.tool, tool, "{why}");
        }

        /// Right-click the seeded sticky note, asserting the context menu
        /// opens targeting it. Returns its id.
        fn right_click_seeded_note(&mut self) -> String {
            let sticky = self
                .app
                .annotations
                .iter()
                .find(|a| matches!(a.kind, AnnoKind::Note { .. }))
                .unwrap()
                .clone();
            let AnnoKind::Note { x: nx, y: ny } = sticky.kind else {
                unreachable!()
            };
            let origin = self.app.page_origins()[0];
            let zoom = self.app.zoom();
            self.move_to((origin.x + nx * zoom, origin.y + ny * zoom));
            self.button(ElementState::Pressed, MouseButton::Right);
            assert!(
                self.app.context_menu.is_some(),
                "right-click over the note opens its delete menu"
            );
            sticky.id
        }

        fn draw_sidebar(
            &mut self,
            tr: &mut ui::text_render::TextRenderer,
        ) -> Vec<(SidebarHit, Rect)> {
            let mut scene = vello::Scene::new();
            let mut er = Rect::ZERO;
            self.app.draw_sidebar(
                &mut scene,
                Affine::IDENTITY,
                tr,
                &ui::theme::Theme::dark(),
                &mut er,
            )
        }
    }

    /// Every toolbar control as `(name, rect)`, hidden (zero) pieces skipped.
    fn toolbar_controls(lay: &ToolbarLayout) -> Vec<(String, Rect)> {
        let mut out = vec![
            ("open".to_string(), lay.open),
            ("save".to_string(), lay.save),
            ("zoom".to_string(), lay.zoom),
            ("fit".to_string(), lay.fit),
            ("search".to_string(), lay.search),
            ("toggle_rail".to_string(), lay.toggle_rail),
            ("toggle_sidebar".to_string(), lay.toggle_sidebar),
        ];
        for (t, r) in &lay.tools {
            out.push((format!("{t:?}"), *r));
        }
        for (sw, r) in &lay.swatches {
            out.push((format!("{sw:?}"), *r));
        }
        out.retain(|(_, r)| r.width() > 0.0 && r.height() > 0.0);
        out
    }

    #[test]
    fn toolbar_arms_tools_recolours_and_lays_out_symmetric_wrapping_rows() {
        let (mut app, mut st) = sample_app();
        let lay = app.toolbar_layout();
        for (tool, r) in lay.tools {
            let hit = app.toolbar_hit(centre(r)).expect("a toolbar hit");
            app.apply_toolbar(&hit, &mut st.doc);
            assert_eq!(app.tool, tool, "clicking {tool:?} should arm it");
        }

        // A swatch recolours the selected (seeded) highlight.
        let (mut app, mut st) = sample_app();
        let hl = app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Markup { .. }))
            .unwrap()
            .clone();
        app.selected_anno = Some(hl.id.clone());
        app.tool = Tool::Highlight;
        app.apply_toolbar(&ToolbarHit::Swatch(Swatch::Green), &mut st.doc);
        let after = app.annotations.iter().find(|a| a.id == hl.id).unwrap();
        assert_eq!(after.color, Swatch::Green);
        assert!(
            ui::undo::take_committed_action(&mut app).is_some(),
            "the recolour dispatched an action and stashed one undo boundary"
        );

        // The first and last toolbar controls sit the same distance from their
        // window edges (the right cluster used to be inset 16 against 12).
        let (app, _st) = sample_app();
        let lay = app.toolbar_layout();
        let left = lay.toggle_rail.x0 - app.toolbar_rect.x0;
        let right = app.toolbar_rect.x1 - lay.toggle_sidebar.x1;
        assert!(
            (left - right).abs() < 0.01,
            "left inset {left} != right inset {right}"
        );

        // At 480px the tool cluster cannot fit between Open/Save and the zoom
        // controls, so it wraps onto a second toolbar row: the bar grows to two
        // rows, the canvas starts beneath it, no two controls overlap and every
        // control stays inside the bar. At desktop width the bar is one row.
        let (mut app, _st) = sample_app();
        app.tool = Tool::Highlight; // swatches visible when there is room
        for (width, rows) in [(1280.0, 1_usize), (480.0, 2)] {
            app.rect = Rect::new(0.0, 0.0, width, 900.0);
            app.recompute_regions();
            let lay = app.toolbar_layout();
            assert_eq!(lay.rows, rows, "rows at {width}px");
            let tb = app.toolbar_rect;
            assert!(
                (tb.height()
                    - f64::from(app.layout_theme.tokens.metrics.header_height) * rows as f64)
                    .abs()
                    < 1e-9,
                "toolbar height {} follows its rows at {width}px",
                tb.height()
            );
            assert!(
                (app.canvas_rect.y0 - tb.y1).abs() < 1e-9,
                "the canvas starts under the whole bar at {width}px"
            );
            let controls = toolbar_controls(&lay);
            assert_eq!(
                lay.tools.len(),
                Tool::ALL.len(),
                "every tool is laid out at {width}px"
            );
            for (name, r) in &controls {
                assert!(
                    r.x0 >= tb.x0 && r.x1 <= tb.x1 && r.y0 >= tb.y0 && r.y1 <= tb.y1,
                    "{name} {r:?} lies inside the toolbar {tb:?} at {width}px"
                );
            }
            for (i, (a_name, a)) in controls.iter().enumerate() {
                for (b_name, b) in controls.iter().skip(i + 1) {
                    let o = a.intersect(*b);
                    assert!(
                        o.width() <= 0.0 || o.height() <= 0.0,
                        "{a_name} {a:?} overlaps {b_name} {b:?} at {width}px"
                    );
                }
            }
        }

        // A freshly synced viewer with the pointer at its default (0,0) — which
        // sits inside the toolbar rect but over no button — must not ask for
        // animation frames, or the loop would spin at ~60Hz while idle.
        let (mut app, _st) = sample_app();
        // resolve_tooltip runs inside the draw; drive it directly.
        app.resolve_tooltip();
        assert!(
            !app.needs_animation_frame(),
            "idle app must settle to zero animation frames"
        );
    }

    /// Scrolling and zooming through the real `handle_event` dispatch, not the
    /// `zoom_at` helper.
    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn wheel_pinch_pixel_scroll_and_arrows_zoom_or_scroll_through_the_event_path() {
        // Pushing far enough crosses into a later page, which the reader tracks.
        let (mut app, _st) = sample_app();
        let p0 = app.current_page;
        let (sx, m) = (app.scroll_x(), app.viewport.max_scroll_y());
        app.viewport.set_scroll(sx, m);
        app.current_page = app.nearest_page();
        assert!(app.current_page >= p0);
        assert_eq!(app.current_page, app.doc.page_count());

        // Ctrl + scroll wheel (the Wayland touchpad pinch surfaces as this): a
        // wheel-up with Ctrl held enlarges the page, a wheel-down shrinks it, and
        // a bare wheel (no Ctrl) scrolls instead of zooming.
        let mut rig = Rig::new();
        // Pointer parked in the canvas so the wheel isn't captured by a panel.
        rig.app.last_mouse = rig.app.canvas_rect.center();
        let z0 = rig.app.zoom();
        rig.wheel(ctrl_mods(), MouseScrollDelta::LineDelta(0.0, 1.0));
        let z_in = rig.app.zoom();
        assert!(
            z_in > z0 + 1e-3,
            "Ctrl+wheel-up zooms in through the event path ({z0} -> {z_in})"
        );
        rig.wheel(ctrl_mods(), MouseScrollDelta::LineDelta(0.0, -1.0));
        let z_out = rig.app.zoom();
        assert!(
            z_out < z_in - 1e-3,
            "Ctrl+wheel-down zooms back out ({z_in} -> {z_out})"
        );
        let z_before_scroll = rig.app.zoom();
        let sy0 = rig.app.scroll_y();
        rig.wheel(Modifiers::default(), MouseScrollDelta::LineDelta(0.0, -1.0));
        assert!(
            (rig.app.zoom() - z_before_scroll).abs() < 1e-6,
            "a bare wheel must not change zoom"
        );
        assert!(
            rig.app.scroll_y() > sy0,
            "a bare wheel scrolls the document instead"
        );

        // A touchpad pinch (`WindowEvent::PinchGesture`) zooms — positive delta
        // enlarges, negative delta shrinks — centred on the cursor.
        let mut rig = Rig::new();
        rig.app.last_mouse = rig.app.canvas_rect.center();
        let pinch = |rig: &mut Rig, delta: f64| {
            rig.feed(
                Modifiers::default(),
                &WindowEvent::PinchGesture {
                    device_id: dummy_device_id(),
                    delta,
                    phase: TouchPhase::Moved,
                },
            );
        };
        let z0 = rig.app.zoom();
        pinch(&mut rig, 0.25);
        let z_in = rig.app.zoom();
        assert!(z_in > z0 + 1e-3, "pinch-open zooms in ({z0} -> {z_in})");
        pinch(&mut rig, -0.25);
        assert!(
            rig.app.zoom() < z_in - 1e-3,
            "pinch-close zooms out ({z_in} -> {})",
            rig.app.zoom()
        );

        // The gesture the user actually gets on Wayland/GNOME: winit's Wayland
        // backend never emits `PinchGesture` (that is macOS/iOS only) and
        // delivers a touchpad two-finger scroll as `MouseWheel` with a
        // **`PixelDelta`** — so Ctrl + two-finger scroll is *the* touchpad zoom.
        // It zooms cursor-centred (the document point under the cursor stays
        // fixed), and the same scroll without Ctrl pans instead of zooming.
        let mut rig = Rig::new();
        // Park the cursor over the page canvas via a real CursorMoved, so the
        // wheel handler's focal point (`self.last_mouse`) is set the live way.
        // Use the canvas horizontal centre: at fit-page the page is narrower
        // than the viewport and stays centred, so only the scrollable (vertical)
        // axis can track the cursor — at the centre both axes hold, giving a
        // clean full cursor-centred check.
        let cursor = Point::new(
            rig.app.canvas_rect.center().x,
            rig.app.canvas_rect.y0 + 220.0,
        );
        rig.move_to((cursor.x, cursor.y));
        let doc_under_cursor_0 = rig.app.viewport.screen_to_content(cursor);
        let z0 = rig.app.zoom();
        rig.wheel(
            ctrl_mods(),
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 30.0)),
        );
        let z_in = rig.app.zoom();
        assert!(
            z_in > z0 + 1e-3,
            "Ctrl + PixelDelta scroll zooms in ({z0} -> {z_in})"
        );
        let drift = (rig.app.viewport.screen_to_content(cursor) - doc_under_cursor_0).hypot();
        assert!(
            drift < 0.5,
            "zoom stays centred on the cursor (doc-point drift {drift:.3}pt)"
        );
        let z_before = rig.app.zoom();
        let sy0 = rig.app.scroll_y();
        rig.wheel(
            Modifiers::default(),
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -40.0)),
        );
        assert!(
            (rig.app.zoom() - z_before).abs() < 1e-6,
            "a bare two-finger scroll must not zoom"
        );
        assert!(
            rig.app.scroll_y() > sy0,
            "a bare two-finger scroll pans the document"
        );

        // Arrow keys scroll the document by a small nudge through the real key
        // path — not a whole-page jump — and `PageDown` scrolls much further.
        let mut rig = Rig::new();
        rig.app.tool = Tool::Select;
        rig.app.go_to_page(1);
        let none = Modifiers::default();
        let sy0 = rig.app.scroll_y();
        rig.key(none, Key::Named(NamedKey::ArrowDown));
        let after_down = rig.app.scroll_y();
        assert!(
            after_down > sy0,
            "ArrowDown scrolls down ({sy0} -> {after_down})"
        );
        assert!(
            (after_down - sy0) <= KEY_SCROLL_STEP + 0.5,
            "ArrowDown is a small nudge (~{KEY_SCROLL_STEP}px), not a page jump: moved {}",
            after_down - sy0
        );
        rig.key(none, Key::Named(NamedKey::ArrowUp));
        assert!(
            (rig.app.scroll_y() - sy0).abs() < 0.5,
            "ArrowUp returns to the start"
        );
        rig.key(none, Key::Named(NamedKey::PageDown));
        assert!(
            rig.app.scroll_y() > sy0 + KEY_SCROLL_STEP,
            "PageDown scrolls a viewport-ful"
        );
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn sidebar_hits_toggle_delete_clip_to_the_body_and_draw_every_tab() {
        let mut tr = ui::text_render::TextRenderer::new();

        // A resolve chip beats its card (smallest rect wins).
        let mut rig = Rig::new();
        rig.app.sidebar_tab = SidebarTab::Comments;
        rig.app.sidebar_hits = rig.draw_sidebar(&mut tr);
        let chip = rig.app.sidebar_hits.iter().find_map(|(h, r)| match h {
            SidebarHit::Resolve(id) => Some((id.clone(), *r)),
            _ => None,
        });
        if let Some((id, r)) = chip {
            let before = rig
                .app
                .comments
                .iter()
                .find(|c| c.id == id)
                .unwrap()
                .resolved;
            assert!(rig.app.apply_sidebar(centre(r), &mut rig.st.doc));
            let after = rig
                .app
                .comments
                .iter()
                .find(|c| c.id == id)
                .unwrap()
                .resolved;
            assert_ne!(
                before, after,
                "resolve chip should toggle, not open the card"
            );
        }

        // Hit rect == painted rect: the sidebar body is painted inside a clip,
        // so a card (and its delete / resolve chips) scrolled under the tab
        // strip must not keep an invisible, clickable rect there.
        // `apply_sidebar` picks the smallest containing rect, which is exactly
        // how a hidden 24px delete button used to beat the "Contents" tab.
        let mut rig = Rig::new();
        // A short window so the seeded thread list overflows its body.
        rig.app.rect = Rect::new(0.0, 0.0, 1280.0, 420.0);
        rig.app.recompute_regions();
        rig.app.sync_viewport_frame();
        rig.app.show_sidebar = true;
        rig.app.sidebar_tab = SidebarTab::Comments;
        // Scroll far enough that the first card sits under the tab strip.
        rig.app.sidebar_scroll = 180.0;
        let hits = rig.draw_sidebar(&mut tr);
        let body = rig.app.sidebar_body();
        let tabs = rig.app.sidebar_tab_rects();
        let body_hits: Vec<&(SidebarHit, Rect)> = hits
            .iter()
            .filter(|(h, _)| !matches!(h, SidebarHit::Tab(_)))
            .collect();
        assert!(
            !body_hits.is_empty(),
            "the scrolled list still exposes the cards that remain visible"
        );
        for (i, (_, r)) in body_hits.iter().enumerate() {
            assert!(
                r.x0 >= body.x0 - 0.01
                    && r.x1 <= body.x1 + 0.01
                    && r.y0 >= body.y0 - 0.01
                    && r.y1 <= body.y1 + 0.01,
                "body hit #{i} {r:?} escapes the painted body {body:?}"
            );
            assert!(
                r.width() > 0.0 && r.height() > 0.0,
                "body hit #{i} {r:?} is empty and should have been dropped"
            );
            for (tab, t) in &tabs {
                let overlap = r.intersect(*t);
                assert!(
                    overlap.width() <= 0.0 || overlap.height() <= 0.0,
                    "body hit #{i} {r:?} reaches into the {tab:?} tab {t:?}"
                );
            }
        }

        // Sidebar delete (×) buttons remove comments, then signatures.
        let mut rig = Rig::new();
        rig.app.sidebar_tab = SidebarTab::Comments;
        rig.app.sidebar_hits = rig.draw_sidebar(&mut tr);
        let del = rig.app.sidebar_hits.iter().find_map(|(h, r)| match h {
            SidebarHit::DeleteComment(id) => Some((id.clone(), *r)),
            _ => None,
        });
        if let Some((id, r)) = del {
            let n = rig.app.comments.len();
            assert!(rig.app.apply_sidebar(centre(r), &mut rig.st.doc));
            assert_eq!(rig.app.comments.len(), n - 1);
            assert!(!rig.app.comments.iter().any(|c| c.id == id));
        }
        rig.app.sidebar_tab = SidebarTab::Signatures;
        rig.app.sidebar_hits = rig.draw_sidebar(&mut tr);
        let dels = rig.app.sidebar_hits.iter().find_map(|(h, r)| match h {
            SidebarHit::DeleteSignature(id) => Some((id.clone(), *r)),
            _ => None,
        });
        if let Some((id, r)) = dels {
            let n = rig.app.signatures.len();
            assert!(rig.app.apply_sidebar(centre(r), &mut rig.st.doc));
            assert_eq!(rig.app.signatures.len(), n - 1);
            assert!(!rig.app.signatures.iter().any(|s| s.id == id));
        }

        // Every sidebar tab and the find bar draw without panicking, and the
        // bar exposes both option toggles.
        let (mut app, _st) = sample_app();
        let theme = ui::theme::Theme::dark();
        app.open_search();
        app.find_bar.set_query("the");
        app.drive_find_bar();
        app.recompute_regions();
        for tab in [
            SidebarTab::Comments,
            SidebarTab::Outline,
            SidebarTab::Signatures,
        ] {
            app.sidebar_tab = tab;
            let mut scene = vello::Scene::new();
            app.prepare_sidebar_scroll(&mut tr);
            let mut er = Rect::ZERO;
            let _ = app.draw_sidebar(&mut scene, Affine::IDENTITY, &mut tr, &theme, &mut er);
            app.draw_find_bar(&mut scene, Affine::IDENTITY, &mut tr, &theme);
            let node = ui::widget::Widget::accessibility(&app.find_bar).unwrap();
            let names: Vec<String> = node
                .children
                .iter()
                .filter_map(|child| child.name.clone())
                .collect();
            assert!(
                names.iter().any(|n| n == "Whole word") && names.iter().any(|n| n == "Match case"),
                "the find bar exposes both option toggles ({tab:?}): {names:?}"
            );
        }
    }

    #[test]
    #[serial]
    fn find_strip_focus_selection_cut_and_ime_go_through_the_shared_input() {
        let none = Modifiers::default();
        let mut rig = Rig::new();
        rig.key(ctrl_mods(), character("f"));
        assert!(rig.app.search_active, "Ctrl+F opens the find bar");
        assert!(
            ui::widget::Widget::is_focused(&rig.app.find_bar),
            "the query takes focus"
        );
        rig.key(none, Key::Named(NamedKey::Escape));
        assert!(!rig.app.search_active, "Escape closes the find bar");
        assert!(
            !ui::widget::Widget::is_focused(&rig.app.find_bar),
            "closing releases focus"
        );

        let mut rig = Rig::new();
        rig.app.open_search();
        for ch in ["s", "a", "n", "d"] {
            rig.key(none, character(ch));
        }
        assert_eq!(rig.app.query(), "sand");
        rig.key(ctrl_mods(), character("a"));
        rig.key(ctrl_mods(), character("x"));
        assert_eq!(rig.app.query(), "", "Ctrl+X cuts the selected query");
        assert_eq!(
            rig.app.match_count(),
            0,
            "derived matches refresh after cut"
        );

        let mut rig = Rig::new();
        rig.app.open_search();
        rig.feed(
            none,
            &WindowEvent::Ime(winit::event::Ime::Commit("東京".into())),
        );
        assert_eq!(rig.app.query(), "東京");
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn canvas_tools_highlight_note_pan_select_and_dismiss_through_the_event_path() {
        // A highlight drag across page one, with the tool armed by its real
        // toolbar button, adds an annotation to the shared document.
        let mut rig = Rig::new();
        rig.arm_via_toolbar(Tool::Highlight, "clicking the toolbar arms the tool");
        let origin = rig.app.page_origins()[0];
        let zoom = rig.app.zoom();
        let before = rig.app.annotations.len();
        rig.move_to((origin.x + 80.0 * zoom, origin.y + 200.0 * zoom));
        rig.press();
        rig.move_to((origin.x + 320.0 * zoom, origin.y + 214.0 * zoom));
        rig.release();
        assert!(
            rig.app.annotations.len() > before,
            "a highlight drag through handle_event adds an annotation"
        );
        assert!(
            ui::undo::take_committed_action(&mut rig.app).is_some(),
            "the new highlight dispatched an action and stashed one undo boundary"
        );
        assert!(
            PdfDoc::list_annotations(&rig.st.doc).len() > before,
            "and it landed in the shared document"
        );

        // The Note tool drops a note whose body is typed and committed through
        // the real keyboard path.
        let mut rig = Rig::new();
        rig.arm_via_toolbar(Tool::Note, "clicking the toolbar arms the note tool");
        let origin = rig.app.page_origins()[0];
        let zoom = rig.app.zoom();
        let comments_before = rig.app.comments.len();
        rig.move_to((origin.x + 200.0 * zoom, origin.y + 260.0 * zoom));
        rig.press();
        assert_eq!(
            rig.app.comments.len(),
            comments_before + 1,
            "placing a note opens a new thread"
        );
        assert!(
            matches!(&rig.app.editing, Some(e) if matches!(e.target, EditTarget::CommentBody(_))),
            "the note's body is focused for typing"
        );
        for ch in "Relay path".chars() {
            let key = if ch == ' ' {
                Key::Named(NamedKey::Space)
            } else {
                Key::Character(ch.to_string().into())
            };
            rig.key(Modifiers::default(), key);
        }
        rig.key(Modifiers::default(), Key::Named(NamedKey::Enter));
        assert!(
            rig.app.editing.is_none(),
            "Enter commits and closes the editor"
        );
        let saved = PdfDoc::list_comments(&rig.st.doc);
        assert!(
            saved.iter().any(|c| c.body == "Relay path"),
            "the typed comment body landed in the shared document: {:?}",
            saved.iter().map(|c| c.body.clone()).collect::<Vec<_>>()
        );

        // A Select-tool press-move-release past `PAN_SLOP` pans the canvas and
        // selects nothing. The sample seeds a pre-selected note; clear it so we
        // can prove a pan never triggers a click-selection of its own.
        let mut rig = Rig::new();
        rig.app.tool = Tool::Select;
        rig.app.go_to_page(1);
        rig.app.selected_anno = None;
        let a = (
            rig.app.canvas_rect.x0 + 200.0,
            rig.app.canvas_rect.y0 + 300.0,
        );
        let sy0 = rig.app.scroll_y();
        rig.move_to(a);
        rig.press();
        rig.move_to((a.0, a.1 - 200.0)); // drag up -> content scrolls down
        rig.release();
        assert!(
            rig.app.scroll_y() > sy0 + 1.0,
            "a Select drag pans the canvas ({sy0} -> {})",
            rig.app.scroll_y()
        );
        assert!(
            rig.app.selected_anno.is_none(),
            "a pan does not select an annotation"
        );

        // A Select-tool press + release without moving, over an annotation,
        // selects it.
        let mut rig = Rig::new();
        rig.app.tool = Tool::Select;
        rig.app.go_to_page(1);
        let hl = rig
            .app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Markup { .. }))
            .unwrap()
            .clone();
        let AnnoKind::Markup { quads, .. } = hl.kind else {
            unreachable!()
        };
        let origin = rig.app.page_origins()[0];
        let (cx, cy) = quads[0].center();
        let zoom = rig.app.zoom();
        rig.move_to((origin.x + cx * zoom, origin.y + cy * zoom));
        rig.press();
        rig.release();
        assert_eq!(
            rig.app.selected_anno.as_deref(),
            Some(hl.id.as_str()),
            "a Select click selects the annotation under it"
        );

        // On a narrow (overlay) window, clicking the canvas scrim beside an
        // open panel dismisses the panels.
        let mut app = PdfApp::sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        // Narrow window -> overlay mode; panels auto-close on the transition.
        app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        app.reconcile_overlay();
        app.recompute_regions();
        app.sync_viewport_frame();
        assert!(app.overlay_mode, "narrow window is in overlay mode");
        // User opens the sidebar as a floating overlay over the canvas.
        app.toggle_sidebar();
        assert!(app.show_sidebar, "sidebar opened as an overlay");
        let mut rig = Rig::with(app, st);
        // Click the scrim: a canvas point on the far left, clear of the panel.
        rig.move_to((rig.app.canvas_rect.x0 + 8.0, rig.app.canvas_rect.y0 + 40.0));
        rig.press();
        assert!(
            !rig.app.show_sidebar && !rig.app.show_rail,
            "clicking the scrim closes the overlay panels"
        );
    }

    /// The inline editor: empty notes are discarded, the focused field's rect
    /// is captured on draw, replies append, and Ctrl+C / Ctrl+X / Ctrl+V are
    /// real clipboard chords, not typed characters.
    ///
    /// `#[serial]` because it *writes* and reads the shared system clipboard,
    /// a shared, process-external resource.
    #[test]
    #[serial]
    fn inline_editor_commits_replies_discards_empty_notes_and_handles_clipboard_chords() {
        // Placing a note then committing without typing must not litter an
        // empty marker + thread.
        let (mut app, mut st) = sample_app();
        let a0 = app.annotations.len();
        let c0 = app.comments.len();
        app.create_note(&mut st.doc, 1, (120.0, 200.0));
        assert_eq!(app.comments.len(), c0 + 1);
        assert!(app.editing.is_some());
        app.commit_edit(&mut st.doc); // committed empty
        assert_eq!(app.comments.len(), c0, "empty note thread removed");
        assert_eq!(app.annotations.len(), a0, "and its page marker removed");

        let (mut app, _st) = sample_app();
        app.sidebar_tab = SidebarTab::Comments;
        let body = app.comments[0].body.clone();
        let cid = app.comments[0].id.clone();
        app.begin_edit(EditTarget::CommentBody(cid), &body);
        let theme = ui::theme::Theme::dark();
        let mut tr = ui::text_render::TextRenderer::new();
        app.prepare_sidebar_scroll(&mut tr);
        let mut scene = vello::Scene::new();
        let mut er = Rect::ZERO;
        let _ = app.draw_sidebar(&mut scene, Affine::IDENTITY, &mut tr, &theme, &mut er);
        assert!(
            er.area() > 1.0,
            "the focused comment field's rect is captured"
        );
        // A click inside that rect must keep the edit alive; outside commits.
        assert!(
            app.edit_rect.area() < 1.0,
            "edit_rect is only set by draw_all, not draw_sidebar"
        );

        // A reply composed into a seeded comment lands in its thread.
        let (mut app, mut st) = sample_app();
        let cid = app.comments[0].id.clone();
        let replies_before = app.comments[0].replies.len();
        app.begin_edit(EditTarget::Reply(cid.clone()), "");
        app.editing.as_mut().unwrap().set_text("Looks good");
        app.commit_edit(&mut st.doc);
        let after = app.comments.iter().find(|c| c.id == cid).unwrap();
        assert_eq!(
            after.replies.len(),
            replies_before + 1,
            "reply landed in the thread"
        );
        assert_eq!(after.replies.last().unwrap().body, "Looks good");

        // Select the whole field first (Ctrl+A) so copy/cut act on the text —
        // the shared TextInput has a real selection model. The cut's
        // field-clear is deterministic regardless of platform-clipboard
        // availability.
        let mut rig = Rig::new();
        let cid = rig.app.comments[0].id.clone();
        rig.app.begin_edit(EditTarget::CommentBody(cid), "hello");
        rig.key(ctrl_mods(), character("a"));
        // Ctrl+C is a clipboard chord, not a keystroke: the field stays "hello".
        assert!(
            rig.key(ctrl_mods(), character("c")),
            "the editor consumes the chord"
        );
        assert_eq!(
            rig.app.editing.as_ref().unwrap().text(),
            "hello",
            "Ctrl+C does not type a literal 'c'"
        );
        // Ctrl+X cuts the selection: the whole field clears.
        rig.key(ctrl_mods(), character("x"));
        assert_eq!(
            rig.app.editing.as_ref().unwrap().text(),
            "",
            "Ctrl+X clears the selected field"
        );

        // Paste inserts the system clipboard's text at the caret via the real
        // editor path. The expectation is derived from the clipboard's *actual*
        // content at paste time (read back with no concurrent writer under
        // `#[serial]`), not from the value we tried to copy, so the assertion
        // is exact whether or not the platform clipboard is available: where it
        // round-trips our write, the buffer becomes "AB" + that text at the
        // caret; where it is unavailable (headless CI), paste is a safe no-op
        // leaving "AB".
        let mut rig = Rig::new();
        let cid = rig.app.comments[0].id.clone();
        rig.app.begin_edit(EditTarget::CommentBody(cid), "AB"); // caret at end
        ui::clipboard_copy("XY");
        rig.key(ctrl_mods(), character("v"));
        let after = rig.app.editing.as_ref().unwrap().text();
        let clip: String = ui::clipboard_paste()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        if clip.is_empty() {
            assert_eq!(after, "AB", "paste with no clipboard is a safe no-op");
        } else {
            assert_eq!(
                after,
                format!("AB{clip}"),
                "Ctrl+V inserts the clipboard text at the caret"
            );
            // Where a working clipboard round-trips our own write, it is "XY".
            if ui::clipboard_paste().as_deref() == Some("XY") {
                assert_eq!(after, "ABXY", "the pasted text is exactly what was copied");
            }
        }
    }

    /// The Open affordance goes to the **host portal**, never an in-app
    /// picker, under a recording host so no real dialog is spawned. A chosen
    /// file loads through the per-frame drain in `sync`; a cancel is a no-op.
    #[test]
    #[serial(pdfium)]
    fn open_goes_through_the_host_portal_and_loads_or_cancels_on_the_frame_drain() {
        let (mut app, mut st) = sample_app();
        let open = app.toolbar_layout().open;
        assert!(
            matches!(app.toolbar_hit(centre(open)), Some(ToolbarHit::Open)),
            "the folder button hit-tests to the Open action"
        );
        let (services, recorder) = ui::host::HostServices::recording();
        ui::host::scoped(&services, || {
            app.apply_toolbar(&ToolbarHit::Open, &mut st.doc);
        });
        assert!(
            app.portal_rx.is_some(),
            "Open routes to the host portal request path (not an in-app dialog)"
        );
        let requests = recorder.pick_requests();
        assert_eq!(requests.len(), 1, "exactly one host chooser request");
        assert_eq!(requests[0].title(), "Open a PDF");

        // A portal-chosen file loads through the per-frame drain.
        let mut app = PdfApp::empty();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();
        assert!(!app.has_document, "empty app starts with no document");
        let path = std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sample.pdf"
        ));
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(path));
        ui::host::scoped(&services, || {
            app.apply_toolbar(&ToolbarHit::Open, &mut st.doc);
        });
        app.sync(&mut st.doc, &st.root);
        assert!(
            app.has_document,
            "a portal-chosen PDF loads through the frame drain"
        );
        assert_eq!(app.doc_name, "sample.pdf");
        assert!(
            app.portal_rx.is_none(),
            "the portal channel clears after delivering"
        );
        assert!(app.doc.page_count() >= 1, "the loaded document has pages");

        // A cancelled portal interaction leaves the current view intact.
        let (mut app, mut st) = sample_app(); // sample document already loaded
        let before = app.has_document;
        let name_before = app.doc_name.clone();
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Cancelled);
        ui::host::scoped(&services, || {
            app.apply_toolbar(&ToolbarHit::Open, &mut st.doc);
        });
        assert!(
            app.portal_rx.is_some(),
            "Open issued a host chooser request"
        );
        app.sync(&mut st.doc, &st.root);
        assert_eq!(
            app.has_document, before,
            "cancel does not change the open state"
        );
        assert_eq!(
            app.doc_name, name_before,
            "cancel keeps the current document"
        );
        assert!(
            app.portal_rx.is_none(),
            "the portal channel clears after a cancel"
        );
    }

    /// The sidebar / context-menu Delete on an annotation dispatches the
    /// byte-identical `EncodedAction` the Ctrl+K palette's own delete spec
    /// carries — one source of truth, proven on the real affordance path —
    /// and the shared context menu deletes by pointer and by keyboard.
    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn delete_affordances_and_the_context_menu_dispatch_the_palette_action() {
        use tilo_actions::ActionInput;
        let (mut app, mut st) = sample_app();
        let anno = app
            .annotations
            .first()
            .expect("sample seeds annotations")
            .clone();
        let resources = ui::resources::resources(&app, &st.doc);
        let want_id = format!("pdf.annotation:{}", anno.id);
        let res = resources[0]
            .children
            .iter()
            .find(|r| r.id.as_str() == want_id.as_str())
            .expect("the annotation is published as a resource");
        let spec = res.actions.iter().find(|a| a.id == "delete").unwrap();
        let spec_encoded = match &spec.input {
            ActionInput::Immediate(e) => e.clone(),
            ActionInput::Prompted { .. } | ActionInput::Deferred { .. } => {
                panic!("delete is an immediate action")
            }
        };
        // The real affordance path (Delete key / rail button / context menu).
        app.delete_target(&DeleteTarget::Annotation(anno.id.clone()), &mut st.doc);
        let dispatched = app
            .actions
            .last_dispatched()
            .cloned()
            .expect("the affordance dispatched an action");
        assert_eq!(dispatched.type_id, spec_encoded.type_id);
        assert_eq!(
            dispatched.payload, spec_encoded.payload,
            "one source of truth"
        );
        assert!(
            app.annotations.iter().all(|a| a.id != anno.id),
            "the annotation was deleted via the dispatched action"
        );

        // The context menu's Delete row geometry comes from the shared widget.
        let (mut app, _st) = sample_app();
        app.context_menu = Some(ContextMenu::delete(
            DeleteTarget::Annotation("anno".into()),
            Point::new(400.0, 400.0),
            app.rect,
        ));
        let menu = app.context_menu.as_ref().unwrap();
        assert!(
            menu.widget
                .item_rect(0)
                .is_some_and(|rect| rect.area() > 1.0),
            "the shared Delete row reports exact hit geometry"
        );

        // Right-click a placed annotation, then click the menu's Delete item.
        let mut rig = Rig::new();
        rig.app.go_to_page(1);
        let note_id = rig.right_click_seeded_note();
        let semantics = Widget::accessibility(&rig.app).expect("PDF semantic root");
        let menu = semantics
            .children
            .iter()
            .find(|node| node.role == ui::a11y::Role::Menu)
            .expect("open shared menu is exposed");
        assert_eq!(menu.children[0].name.as_deref(), Some("Delete"));
        assert!(
            !menu.children[0].state.focused,
            "destructive-only menus require an explicit choice"
        );
        assert!(Widget::traps_focus(&rig.app));
        assert!(Widget::modal_capture(&rig.app).is_some());
        let item = rig
            .app
            .context_menu
            .as_ref()
            .and_then(|menu| menu.widget.item_rect(0))
            .unwrap();
        assert!(item.area() > 1.0, "the shared Delete row has hit bounds");
        let before = rig.app.annotations.len();
        let ic = centre(item);
        rig.move_to((ic.x, ic.y));
        rig.press();
        assert!(
            rig.app.context_menu.is_none(),
            "the menu closes after the delete"
        );
        assert_eq!(
            rig.app.annotations.len(),
            before - 1,
            "the targeted annotation is removed"
        );
        assert!(
            !rig.app.annotations.iter().any(|a| a.id == note_id),
            "specifically the right-clicked note"
        );
        assert!(
            !PdfDoc::list_annotations(&rig.st.doc)
                .iter()
                .any(|a| a.id == note_id),
            "and it is gone from the shared document"
        );

        // ArrowDown + Enter deletes through the shared keyboard route.
        let mut rig = Rig::new();
        rig.app.go_to_page(1);
        let note_id = rig.right_click_seeded_note();
        rig.key(Modifiers::default(), Key::Named(NamedKey::ArrowDown));
        rig.key(Modifiers::default(), Key::Named(NamedKey::Enter));
        assert!(rig.app.context_menu.is_none());
        assert!(
            !rig.app
                .annotations
                .iter()
                .any(|annotation| annotation.id == note_id)
        );
        assert!(
            !PdfDoc::list_annotations(&rig.st.doc)
                .iter()
                .any(|annotation| annotation.id == note_id)
        );
    }
}

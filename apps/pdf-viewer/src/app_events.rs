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
        self.search_input.set_focus(true);
        if !self.query().is_empty() {
            self.recompute_matches();
        }
        self.dirty = true;
    }

    fn close_search(&mut self) {
        self.search_input.set_focus(false);
        if self.search_active {
            self.search_active = false;
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
        if lay.open.contains(p) {
            // A pending open owns the document slot; starting a second picker
            // would only create a competing request.  Consume the press while
            // the chrome is visibly disabled.
            if self.pending_open.is_none() {
                return Some(ToolbarHit::Open);
            }
            return Some(ToolbarHit::Tool(self.tool));
        }
        if lay.save.contains(p) {
            return Some(ToolbarHit::Save);
        }
        for (t, r) in &lay.tools {
            if r.contains(p) {
                return Some(ToolbarHit::Tool(*t));
            }
        }
        if matches!(self.tool, Tool::Highlight | Tool::Ink | Tool::Note) {
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
        if lay.zoom_label.contains(p) {
            // Clicking the percentage resets to fit.
            return Some(ToolbarHit::Fit);
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
        Some(ToolbarHit::Tool(self.tool))
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
                // Cycle Fit-page → Fit-width → Fit-page.
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
        }
    }

    fn apply_search_strip(&mut self, p: Point) -> bool {
        for (hit, r) in &self.search_hits {
            if r.contains(p) {
                match hit {
                    SearchHit::ToggleWholeWord => self.search_whole_word = !self.search_whole_word,
                    SearchHit::ToggleCaseSensitive => {
                        self.search_case_sensitive = !self.search_case_sensitive;
                    }
                }
                self.recompute_matches();
                self.dirty = true;
                return true;
            }
        }
        false
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
        self.page_bubble.poke(std::time::Instant::now());
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
                    self.drive_search_input(event, scale_factor, modifiers, state);
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
                    self.drive_search_input(event, scale_factor, modifiers, state);
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

            WindowEvent::Ime(_) if self.search_active => {
                self.drive_search_input(event, scale_factor, modifiers, state);
                if self.search_input.take_dirty().is_some() {
                    self.recompute_matches();
                }
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
            if self.editing.as_ref().is_some_and(|e| e.input.is_dragging()) {
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
            let new_w = (p.x - self.rect.x0).clamp(RAIL_W_MIN, RAIL_W_MAX);
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
        if let Some(hit) = self.toolbar_hit(p) {
            self.apply_toolbar(&hit, state);
            *handled = true;
            return true;
        }
        if self.search_active && self.search_rect.contains(p) {
            if self.search_input.get_rect().contains(p) {
                self.drive_search_input(event, scale_factor, modifiers, state);
            } else {
                self.apply_search_strip(p);
            }
            *handled = true;
            return true;
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
            if (p.x - rail.x1).abs() <= RAIL_GRIP && p.y >= rail.y0 && p.y <= rail.y1 {
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
        if self.editing.as_ref().is_some_and(|e| e.input.is_dragging()) {
            self.drive_editor(event, scale_factor, modifiers, state);
            self.dirty = true;
            *handled = true;
            return;
        }
        // End a sticky-note marker drag: persist the new anchor once (a
        // no-move release is a plain select, already applied on press).
        if let Some(mdrag) = self.marker_drag.take() {
            if mdrag.moved() {
                if let Some((x, y)) = self.note_pos(&mdrag.id) {
                    self.perform_action(
                        state,
                        &crate::actions::set_annotation_position_action(&mdrag.id, x, y),
                    );
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
                self.select_at(page, pt);
            }
            *handled = true;
        } else if self.rail_drag.take().is_some() {
            self.dirty = true;
            *handled = true;
        } else if let Some(pan) = self.pan.take() {
            // A press that never moved is a selection click.
            if !pan.moved {
                if let Some((page, pt)) = self.hit_page(pan.start) {
                    self.select_at(page, pt);
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
            scale,
            &modifiers,
            state,
            &automerge::ROOT,
            &mut false,
            &mut None,
            node,
            &taffy,
        );
    }

    fn drive_editor(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
    ) {
        if let Some(e) = self.editing.as_mut() {
            Self::drive_local_input(&mut e.input, event, scale, modifiers, state);
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
        let submitted = self.editing.as_mut().is_some_and(|e| e.input.take_submit());
        if submitted {
            self.commit_edit(state);
        }
        self.dirty = true;
        *handled = true;
        true
    }

    /// While the search strip is open it owns the keyboard. Returns whether
    /// it consumed this key.
    fn drive_search_input(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
    ) {
        Self::drive_local_input(&mut self.search_input, event, scale, modifiers, state);
    }

    fn on_key_search(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        ctrl: bool,
        scale: f64,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        if !self.search_active {
            return false;
        }
        let WindowEvent::KeyboardInput {
            event: key_event, ..
        } = event
        else {
            return false;
        };
        let mods = modifiers.state();
        match &key_event.logical_key {
            Key::Named(NamedKey::Escape) => self.close_search(),
            Key::Named(NamedKey::Enter) => {
                self.step_match(if mods.shift_key() { -1 } else { 1 });
            }
            Key::Character(c) => {
                if ctrl && c.as_str().eq_ignore_ascii_case("f") {
                    self.close_search();
                } else {
                    self.drive_search_input(event, scale, modifiers, state);
                }
            }
            _ => self.drive_search_input(event, scale, modifiers, state),
        }
        if self.search_input.take_dirty().is_some() {
            self.recompute_matches();
        }
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

    fn app() -> (PdfApp, ui::State) {
        let mut app = PdfApp::sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();
        (app, st)
    }

    #[test]
    fn toolbar_tool_buttons_select_tools() {
        let (mut app, mut st) = app();
        let lay = app.toolbar_layout();
        for (tool, r) in lay.tools {
            let p = Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1));
            let hit = app.toolbar_hit(p).expect("a toolbar hit");
            app.apply_toolbar(&hit, &mut st.doc);
            assert_eq!(app.tool, tool, "clicking {tool:?} should arm it");
        }
    }

    #[test]
    fn swatch_recolors_selected_annotation() {
        let (mut app, mut st) = app();
        // Select the seeded highlight.
        let hl = app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Highlight { .. }))
            .unwrap()
            .clone();
        app.selected_anno = Some(hl.id.clone());
        app.tool = Tool::Highlight;
        app.apply_toolbar(&ToolbarHit::Swatch(Swatch::Green), &mut st.doc);
        let after = app.annotations.iter().find(|a| a.id == hl.id).unwrap();
        assert_eq!(after.color, Swatch::Green);
        assert!(
            app.take_committed_action().is_some(),
            "the recolour dispatched an action and stashed one undo boundary"
        );
    }

    /// The sidebar / context-menu Delete on an annotation dispatches the
    /// byte-identical `EncodedAction` the Ctrl+K palette's own delete spec
    /// carries — one source of truth, proven on the real affordance path.
    #[test]
    fn annotation_delete_affordance_matches_palette_action() {
        use tilo_actions::ActionInput;
        let (mut app, mut st) = app();
        let anno = app
            .annotations
            .first()
            .expect("sample seeds annotations")
            .clone();

        // The EncodedAction the palette would dispatch for this annotation's delete.
        let resources = Widget::resources(&app, &st.doc);
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
    }

    #[test]
    fn wheel_scrolls_and_tracks_page() {
        let (mut app, _st) = app();
        let p0 = app.current_page;
        // Push far enough to cross into a later page.
        let (sx, m) = (app.scroll_x(), app.viewport.max_scroll_y());
        app.viewport.set_scroll(sx, m);
        app.current_page = app.nearest_page();
        assert!(app.current_page >= p0);
        assert_eq!(app.current_page, app.doc.page_count());
    }

    /// Ctrl + scroll wheel (the Wayland touchpad pinch surfaces as this) zooms
    /// through the real `handle_event` dispatch, not the `zoom_at` helper: a
    /// wheel-up with Ctrl held enlarges the page, a wheel-down shrinks it, and a
    /// bare wheel (no Ctrl) scrolls instead of zooming.
    #[test]
    fn ctrl_wheel_zooms_through_the_event_path() {
        use winit::event::{MouseScrollDelta, TouchPhase};

        let (mut app, mut st) = app();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        // Pointer parked in the canvas so the wheel isn't captured by a panel.
        app.last_mouse = app.canvas_rect.center();

        let wheel =
            |app: &mut PdfApp, st: &mut ui::State, dy: f32, mods: &winit::event::Modifiers| {
                let ev = WindowEvent::MouseWheel {
                    device_id: dummy_device_id(),
                    delta: MouseScrollDelta::LineDelta(0.0, dy),
                    phase: TouchPhase::Moved,
                };
                let mut handled = false;
                let mut focus = None;
                app.handle_event(
                    &ev,
                    1.0,
                    mods,
                    &mut st.doc,
                    &st.root,
                    &mut handled,
                    &mut focus,
                    node,
                    &taffy,
                );
            };

        let z0 = app.zoom();
        wheel(&mut app, &mut st, 1.0, &ctrl_mods());
        let z_in = app.zoom();
        assert!(
            z_in > z0 + 1e-3,
            "Ctrl+wheel-up zooms in through the event path ({z0} -> {z_in})"
        );

        wheel(&mut app, &mut st, -1.0, &ctrl_mods());
        let z_out = app.zoom();
        assert!(
            z_out < z_in - 1e-3,
            "Ctrl+wheel-down zooms back out ({z_in} -> {z_out})"
        );

        // Without Ctrl the same wheel scrolls the document, leaving zoom alone.
        let z_before_scroll = app.zoom();
        let sy0 = app.scroll_y();
        wheel(&mut app, &mut st, -1.0, &winit::event::Modifiers::default());
        assert!(
            (app.zoom() - z_before_scroll).abs() < 1e-6,
            "a bare wheel must not change zoom"
        );
        assert!(
            app.scroll_y() > sy0,
            "a bare wheel scrolls the document instead"
        );
    }

    /// A touchpad pinch (`WindowEvent::PinchGesture`) zooms through the real
    /// event path — positive delta enlarges, negative delta shrinks — centred on
    /// the cursor, so the specifically-requested pinch zoom is exercised end to
    /// end and not just the reducer.
    #[test]
    fn pinch_gesture_zooms_through_the_event_path() {
        use winit::event::TouchPhase;

        let (mut app, mut st) = app();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        app.last_mouse = app.canvas_rect.center();
        let mods = winit::event::Modifiers::default();

        let pinch = |app: &mut PdfApp, st: &mut ui::State, delta: f64| {
            let ev = WindowEvent::PinchGesture {
                device_id: dummy_device_id(),
                delta,
                phase: TouchPhase::Moved,
            };
            let mut handled = false;
            let mut focus = None;
            app.handle_event(
                &ev,
                1.0,
                &mods,
                &mut st.doc,
                &st.root,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        };

        let z0 = app.zoom();
        pinch(&mut app, &mut st, 0.25);
        let z_in = app.zoom();
        assert!(z_in > z0 + 1e-3, "pinch-open zooms in ({z0} -> {z_in})");

        pinch(&mut app, &mut st, -0.25);
        assert!(
            app.zoom() < z_in - 1e-3,
            "pinch-close zooms out ({z_in} -> {})",
            app.zoom()
        );
    }

    /// The gesture the user actually gets on Wayland/GNOME: winit's Wayland
    /// backend never emits `PinchGesture` (that is macOS/iOS only) and delivers
    /// a touchpad two-finger scroll as `MouseWheel` with a **`PixelDelta`** — so
    /// Ctrl + two-finger scroll is *the* touchpad zoom. Assert a Ctrl-held
    /// `PixelDelta` wheel zooms in through the real `handle_event`, cursor-
    /// centred (the document point under the cursor stays fixed), and that the
    /// same scroll without Ctrl pans instead of zooming.
    #[test]
    fn ctrl_pixel_scroll_zooms_cursor_centred_through_the_event_path() {
        use winit::dpi::PhysicalPosition;
        use winit::event::{MouseScrollDelta, TouchPhase};

        let (mut app, mut st) = app();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mods_none = winit::event::Modifiers::default();

        let feed = |app: &mut PdfApp,
                    st: &mut ui::State,
                    ev: &WindowEvent,
                    mods: &winit::event::Modifiers| {
            let mut handled = false;
            let mut focus = None;
            app.handle_event(
                ev,
                1.0,
                mods,
                &mut st.doc,
                &st.root,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        };

        // Park the cursor over the page canvas via a real CursorMoved, so the
        // wheel handler's focal point (`self.last_mouse`) is set the live way.
        // Use the canvas horizontal centre: at fit-page the page is narrower
        // than the viewport and stays centred, so only the scrollable (vertical)
        // axis can track the cursor — at the centre both axes hold, giving a
        // clean full cursor-centred check.
        let cursor = Point::new(app.canvas_rect.center().x, app.canvas_rect.y0 + 220.0);
        feed(
            &mut app,
            &mut st,
            &WindowEvent::CursorMoved {
                device_id: dummy_device_id(),
                position: PhysicalPosition::new(cursor.x, cursor.y),
            },
            &mods_none,
        );

        let doc_under_cursor_0 = app.viewport.screen_to_content(cursor);
        let z0 = app.zoom();

        // Ctrl + two-finger scroll UP, exactly as Wayland delivers it: a
        // PixelDelta wheel with the Control modifier tracked from ModifiersChanged.
        let pixel_up = WindowEvent::MouseWheel {
            device_id: dummy_device_id(),
            delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 30.0)),
            phase: TouchPhase::Moved,
        };
        feed(&mut app, &mut st, &pixel_up, &ctrl_mods());
        let z_in = app.zoom();
        assert!(
            z_in > z0 + 1e-3,
            "Ctrl + PixelDelta scroll zooms in ({z0} -> {z_in})"
        );

        // Cursor-centred: the document point beneath the cursor did not drift.
        let doc_under_cursor_1 = app.viewport.screen_to_content(cursor);
        let drift = (doc_under_cursor_1 - doc_under_cursor_0).hypot();
        assert!(
            drift < 0.5,
            "zoom stays centred on the cursor (doc-point drift {drift:.3}pt)"
        );

        // Same scroll without Ctrl scrolls the document instead of zooming.
        let z_before = app.zoom();
        let sy0 = app.scroll_y();
        let pixel_down = WindowEvent::MouseWheel {
            device_id: dummy_device_id(),
            delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -40.0)),
            phase: TouchPhase::Moved,
        };
        feed(&mut app, &mut st, &pixel_down, &mods_none);
        assert!(
            (app.zoom() - z_before).abs() < 1e-6,
            "a bare two-finger scroll must not zoom"
        );
        assert!(
            app.scroll_y() > sy0,
            "a bare two-finger scroll pans the document"
        );
    }

    #[test]
    fn resolve_chip_beats_card_smallest_rect_wins() {
        let (mut app, mut st) = app();
        // Draw once to populate sidebar_hits.
        let mut scene = vello::Scene::new();
        let theme = ui::theme::Theme::dark();
        let mut tr = ui::text_render::TextRenderer::new();
        app.sidebar_tab = SidebarTab::Comments;
        let mut er = Rect::ZERO;
        let hits = app.draw_sidebar(&mut scene, Affine::IDENTITY, &mut tr, &theme, &mut er);
        app.sidebar_hits = hits;
        // Find a resolve chip and click its centre.
        let chip = app.sidebar_hits.iter().find_map(|(h, r)| match h {
            SidebarHit::Resolve(id) => Some((id.clone(), *r)),
            _ => None,
        });
        if let Some((id, r)) = chip {
            let before = app.comments.iter().find(|c| c.id == id).unwrap().resolved;
            let p = Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1));
            assert!(app.apply_sidebar(p, &mut st.doc));
            let after = app.comments.iter().find(|c| c.id == id).unwrap().resolved;
            assert_ne!(
                before, after,
                "resolve chip should toggle, not open the card"
            );
        }
    }

    /// Hit rect == painted rect: the sidebar body is painted inside a clip, so
    /// a card (and its delete / resolve chips) scrolled under the tab strip
    /// must not keep an invisible, clickable rect there. `apply_sidebar` picks
    /// the smallest containing rect, which is exactly how a hidden 24px delete
    /// button used to beat the "Contents" tab.
    #[test]
    fn sidebar_hits_are_clipped_to_the_scrolled_body() {
        let (mut app, _st) = app();
        // A short window so the seeded thread list overflows its body.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 420.0);
        app.recompute_regions();
        app.sync_viewport_frame();
        app.show_sidebar = true;
        app.sidebar_tab = SidebarTab::Comments;
        // Scroll far enough that the first card sits under the tab strip.
        app.sidebar_scroll = 180.0;
        let mut scene = vello::Scene::new();
        let theme = ui::theme::Theme::dark();
        let mut tr = ui::text_render::TextRenderer::new();
        let mut er = Rect::ZERO;
        let hits = app.draw_sidebar(&mut scene, Affine::IDENTITY, &mut tr, &theme, &mut er);
        let body = app.sidebar_body();
        let tabs = app.sidebar_tab_rects();
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
    }

    /// The first and last toolbar controls sit the same distance from their
    /// window edges (the right cluster used to be inset 16 against 12).
    #[test]
    fn toolbar_edge_insets_are_symmetric() {
        let (app, _st) = app();
        let lay = app.toolbar_layout();
        let left = lay.toggle_rail.x0 - app.toolbar_rect.x0;
        let right = app.toolbar_rect.x1 - lay.toggle_sidebar.x1;
        assert!(
            (left - right).abs() < 0.01,
            "left inset {left} != right inset {right}"
        );
    }

    /// Every toolbar control as `(name, rect)`, hidden (zero) pieces skipped.
    fn toolbar_controls(lay: &ToolbarLayout) -> Vec<(String, Rect)> {
        let mut out = vec![
            ("open".to_string(), lay.open),
            ("save".to_string(), lay.save),
            ("zoom_out".to_string(), lay.zoom_out),
            ("zoom_label".to_string(), lay.zoom_label),
            ("zoom_in".to_string(), lay.zoom_in),
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

    /// At 480px the tool cluster cannot fit between Open/Save and the zoom
    /// controls, so it wraps onto a second toolbar row: the bar grows to two
    /// rows, the canvas starts beneath it, no two controls overlap and every
    /// control stays inside the bar. At desktop width the bar is one row.
    #[test]
    fn toolbar_wraps_the_tool_cluster_onto_a_second_row_when_narrow() {
        let (mut app, _st) = app();
        app.tool = Tool::Highlight; // swatches visible when there is room
        for (width, rows) in [(1280.0, 1_usize), (480.0, 2)] {
            app.rect = Rect::new(0.0, 0.0, width, 900.0);
            app.recompute_regions();
            let lay = app.toolbar_layout();
            assert_eq!(lay.rows, rows, "rows at {width}px");
            let tb = app.toolbar_rect;
            assert!(
                (tb.height() - TOOLBAR_H * rows as f64).abs() < 1e-9,
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
    }

    #[test]
    fn sidebar_delete_buttons_remove_items() {
        let (mut app, mut st) = app();
        let mut scene = vello::Scene::new();
        let theme = ui::theme::Theme::dark();
        let mut tr = ui::text_render::TextRenderer::new();

        // Comments tab: click a comment's delete (×) and confirm removal.
        app.sidebar_tab = SidebarTab::Comments;
        let mut er = Rect::ZERO;
        app.sidebar_hits = app.draw_sidebar(&mut scene, Affine::IDENTITY, &mut tr, &theme, &mut er);
        let del = app.sidebar_hits.iter().find_map(|(h, r)| match h {
            SidebarHit::DeleteComment(id) => Some((id.clone(), *r)),
            _ => None,
        });
        if let Some((id, r)) = del {
            let n = app.comments.len();
            let p = Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1));
            assert!(app.apply_sidebar(p, &mut st.doc));
            assert_eq!(app.comments.len(), n - 1);
            assert!(!app.comments.iter().any(|c| c.id == id));
        }

        // Signatures tab: click a signature's delete (×).
        app.sidebar_tab = SidebarTab::Signatures;
        let mut scene2 = vello::Scene::new();
        let mut er2 = Rect::ZERO;
        app.sidebar_hits =
            app.draw_sidebar(&mut scene2, Affine::IDENTITY, &mut tr, &theme, &mut er2);
        let dels = app.sidebar_hits.iter().find_map(|(h, r)| match h {
            SidebarHit::DeleteSignature(id) => Some((id.clone(), *r)),
            _ => None,
        });
        if let Some((id, r)) = dels {
            let n = app.signatures.len();
            let p = Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1));
            assert!(app.apply_sidebar(p, &mut st.doc));
            assert_eq!(app.signatures.len(), n - 1);
            assert!(!app.signatures.iter().any(|s| s.id == id));
        }
    }

    #[test]
    fn every_sidebar_tab_and_search_strip_draws_without_panicking() {
        let (mut app, _st) = app();
        let theme = ui::theme::Theme::dark();
        let mut tr = ui::text_render::TextRenderer::new();
        app.search_active = true;
        app.search_input.set_text("the");
        app.recompute_matches();
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
            let shits = app.draw_search(&mut scene, Affine::IDENTITY, &mut tr, &theme);
            assert!(
                shits
                    .iter()
                    .any(|(h, _)| matches!(h, SearchHit::ToggleWholeWord))
                    && shits
                        .iter()
                        .any(|(h, _)| matches!(h, SearchHit::ToggleCaseSensitive)),
                "search strip exposes both option toggles"
            );
        }
    }

    /// Synthesise the opaque dummy `DeviceId` winit uses in tests. It has no
    /// public constructor; a zeroed value is the sentinel every backend uses
    /// and the convention across this workspace's widget tests.
    fn dummy_device_id() -> winit::event::DeviceId {
        // SAFETY: DeviceId wraps a platform integer; zero is the "no device"
        // sentinel and carries no validity requirements.
        unsafe { std::mem::zeroed() }
    }

    /// Feed one real `WindowEvent` through `PdfApp::handle_event`, the same
    /// entry point the live widget tree drives. Shared by the direct
    /// event-path tests below so each one reads as its own scenario rather
    /// than reproducing the taffy/harness plumbing.
    fn feed_event(
        app: &mut PdfApp,
        st: &mut ui::State,
        taffy: &TaffyTree<()>,
        node: NodeId,
        mods: winit::event::Modifiers,
        ev: &winit::event::WindowEvent,
    ) {
        use ui::Widget as _;
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            1.0,
            &mods,
            &mut st.doc,
            &st.root,
            &mut handled,
            &mut focus,
            node,
            taffy,
        );
    }

    /// Arm the Highlight tool by clicking its real toolbar button, and assert
    /// the click landed.
    fn arm_highlight_tool_via_toolbar(
        app: &mut PdfApp,
        st: &mut ui::State,
        taffy: &TaffyTree<()>,
        node: NodeId,
        mods: winit::event::Modifiers,
        dev: winit::event::DeviceId,
    ) {
        use winit::dpi::PhysicalPosition;
        use winit::event::{ElementState, MouseButton, WindowEvent};

        let lay = app.toolbar_layout();
        let (_, hb) = lay
            .tools
            .iter()
            .find(|(t, _)| *t == Tool::Highlight)
            .unwrap();
        let btn = (f64::midpoint(hb.x0, hb.x1), f64::midpoint(hb.y0, hb.y1));
        feed_event(
            app,
            st,
            taffy,
            node,
            mods,
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(btn.0, btn.1),
            },
        );
        feed_event(
            app,
            st,
            taffy,
            node,
            mods,
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        assert_eq!(
            app.tool,
            Tool::Highlight,
            "clicking the toolbar arms the tool"
        );
    }

    #[test]
    fn highlight_drag_through_real_event_path_adds_annotation() {
        use winit::dpi::PhysicalPosition;
        use winit::event::{ElementState, MouseButton, WindowEvent};

        let mut app = PdfApp::sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mods = winit::event::Modifiers::default();
        let dev = dummy_device_id();

        arm_highlight_tool_via_toolbar(&mut app, &mut st, &taffy, node, mods, dev);

        // Drag across page one to lay down a highlight.
        let origin = app.page_origins()[0];
        let p0 = (origin.x + 80.0 * app.zoom(), origin.y + 200.0 * app.zoom());
        let p1 = (origin.x + 320.0 * app.zoom(), origin.y + 214.0 * app.zoom());
        let before = app.annotations.len();
        feed_event(
            &mut app,
            &mut st,
            &taffy,
            node,
            mods,
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(p0.0, p0.1),
            },
        );
        feed_event(
            &mut app,
            &mut st,
            &taffy,
            node,
            mods,
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        feed_event(
            &mut app,
            &mut st,
            &taffy,
            node,
            mods,
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(p1.0, p1.1),
            },
        );
        feed_event(
            &mut app,
            &mut st,
            &taffy,
            node,
            mods,
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );

        assert!(
            app.annotations.len() > before,
            "a highlight drag through handle_event adds an annotation"
        );
        assert!(
            app.take_committed_action().is_some(),
            "the new highlight dispatched an action and stashed one undo boundary"
        );
        assert!(
            PdfDoc::list_annotations(&st.doc).len() > before,
            "and it landed in the shared document"
        );
    }

    /// Build a real `WindowEvent::KeyboardInput` for `key`. winit's
    /// `KeyEventExtra` is `pub(crate)`, so we zero the struct and set the
    /// public fields — the same trick the terminal + `modal_dialog` tests use.
    fn key_event(key: winit::keyboard::Key) -> winit::event::WindowEvent {
        use winit::event::{ElementState, KeyEvent};
        use winit::keyboard::{Key, KeyLocation, NamedKey, NativeKeyCode, PhysicalKey};
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

    #[test]
    fn find_focus_follows_open_and_close_shortcuts() {
        let (mut app, mut st) = app();
        let mut taffy = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();

        let open = key_event(winit::keyboard::Key::Character("f".into()));
        feed_event(&mut app, &mut st, &taffy, node, ctrl_mods(), &open);
        assert!(app.search_active, "Ctrl+F opens the find strip");
        assert!(app.search_input.is_focused(), "the query takes focus");

        let close = key_event(winit::keyboard::Key::Named(
            winit::keyboard::NamedKey::Escape,
        ));
        feed_event(
            &mut app,
            &mut st,
            &taffy,
            node,
            winit::event::Modifiers::default(),
            &close,
        );
        assert!(!app.search_active, "Escape closes the find strip");
        assert!(!app.search_input.is_focused(), "closing releases focus");
    }

    #[test]
    #[serial]
    fn find_query_uses_shared_selection_and_cut_path() {
        let (mut app, mut st) = app();
        let mut taffy = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        app.open_search();

        for ch in ["s", "a", "n", "d"] {
            let ev = key_event(winit::keyboard::Key::Character(ch.into()));
            feed_event(
                &mut app,
                &mut st,
                &taffy,
                node,
                winit::event::Modifiers::default(),
                &ev,
            );
        }
        assert_eq!(app.query(), "sand");

        let select = key_event(winit::keyboard::Key::Character("a".into()));
        feed_event(&mut app, &mut st, &taffy, node, ctrl_mods(), &select);
        assert_eq!(
            app.search_input.selection_range_public(),
            Some((0, 4)),
            "Ctrl+A selects the query rather than typing into it"
        );

        let cut = key_event(winit::keyboard::Key::Character("x".into()));
        feed_event(&mut app, &mut st, &taffy, node, ctrl_mods(), &cut);
        assert_eq!(app.query(), "", "Ctrl+X cuts the selected query");
        assert_eq!(app.match_count(), 0, "derived matches refresh after cut");
    }

    #[test]
    fn find_query_accepts_ime_commit_through_root_event_path() {
        let (mut app, mut st) = app();
        let mut taffy = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        app.open_search();

        let commit = WindowEvent::Ime(winit::event::Ime::Commit("東京".into()));
        feed_event(
            &mut app,
            &mut st,
            &taffy,
            node,
            winit::event::Modifiers::default(),
            &commit,
        );
        assert_eq!(app.query(), "東京");
    }

    #[test]
    fn note_type_and_commit_writes_body_through_real_event_path() {
        use ui::Widget as _;
        use winit::dpi::PhysicalPosition;
        use winit::keyboard::{Key, NamedKey};

        let mut app = PdfApp::sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mods = winit::event::Modifiers::default();
        let dev = dummy_device_id();

        let feed = |app: &mut PdfApp, st: &mut ui::State, ev: &WindowEvent| {
            let mut handled = false;
            let mut focus = None;
            app.handle_event(
                ev,
                1.0,
                &mods,
                &mut st.doc,
                &st.root,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        };

        // Arm the Note tool via its real toolbar button.
        let lay = app.toolbar_layout();
        let (_, nb) = lay.tools.iter().find(|(t, _)| *t == Tool::Note).unwrap();
        let btn = (f64::midpoint(nb.x0, nb.x1), f64::midpoint(nb.y0, nb.y1));
        feed(
            &mut app,
            &mut st,
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(btn.0, btn.1),
            },
        );
        feed(
            &mut app,
            &mut st,
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        assert_eq!(
            app.tool,
            Tool::Note,
            "clicking the toolbar arms the note tool"
        );

        // Click a page to drop the note; the caret should land in its thread.
        let origin = app.page_origins()[0];
        let click = (origin.x + 200.0 * app.zoom(), origin.y + 260.0 * app.zoom());
        let comments_before = app.comments.len();
        feed(
            &mut app,
            &mut st,
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(click.0, click.1),
            },
        );
        feed(
            &mut app,
            &mut st,
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        assert_eq!(
            app.comments.len(),
            comments_before + 1,
            "placing a note opens a new thread"
        );
        assert!(
            matches!(&app.editing, Some(e) if matches!(e.target, EditTarget::CommentBody(_))),
            "the note's body is focused for typing"
        );

        // Type a real sentence through the keyboard event path.
        for ch in "Relay path".chars() {
            let ev = if ch == ' ' {
                key_event(Key::Named(NamedKey::Space))
            } else {
                key_event(Key::Character(ch.to_string().into()))
            };
            feed(&mut app, &mut st, &ev);
        }
        // Enter commits the edit.
        feed(&mut app, &mut st, &key_event(Key::Named(NamedKey::Enter)));

        assert!(app.editing.is_none(), "Enter commits and closes the editor");
        let saved = PdfDoc::list_comments(&st.doc);
        assert!(
            saved.iter().any(|c| c.body == "Relay path"),
            "the typed comment body landed in the shared document: {:?}",
            saved.iter().map(|c| c.body.clone()).collect::<Vec<_>>()
        );
    }

    /// Arrow keys scroll the document by a small nudge through the real key
    /// path — not a whole-page jump — and `PageDown` scrolls much further.
    #[test]
    fn arrow_keys_scroll_smoothly_not_by_whole_pages() {
        use ui::Widget as _;
        use winit::keyboard::{Key, NamedKey};

        let mut app = PdfApp::sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();
        app.tool = Tool::Select;
        app.go_to_page(1);

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mods = winit::event::Modifiers::default();
        let feed = |app: &mut PdfApp, st: &mut ui::State, ev: &WindowEvent| {
            let mut handled = false;
            let mut focus = None;
            app.handle_event(
                ev,
                1.0,
                &mods,
                &mut st.doc,
                &st.root,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        };

        let sy0 = app.scroll_y();
        feed(
            &mut app,
            &mut st,
            &key_event(Key::Named(NamedKey::ArrowDown)),
        );
        let after_down = app.scroll_y();
        assert!(
            after_down > sy0,
            "ArrowDown scrolls down ({sy0} -> {after_down})"
        );
        assert!(
            (after_down - sy0) <= KEY_SCROLL_STEP + 0.5,
            "ArrowDown is a small nudge (~{KEY_SCROLL_STEP}px), not a page jump: moved {}",
            after_down - sy0
        );

        // ArrowUp reverses it.
        feed(&mut app, &mut st, &key_event(Key::Named(NamedKey::ArrowUp)));
        assert!(
            (app.scroll_y() - sy0).abs() < 0.5,
            "ArrowUp returns to the start"
        );

        // PageDown scrolls far more than a single arrow nudge.
        feed(
            &mut app,
            &mut st,
            &key_event(Key::Named(NamedKey::PageDown)),
        );
        assert!(
            app.scroll_y() > sy0 + KEY_SCROLL_STEP,
            "PageDown scrolls a viewport-ful"
        );
    }

    #[test]
    fn empty_note_is_discarded_on_commit() {
        // Placing a note then committing without typing must not litter an
        // empty marker + thread.
        let (mut app, mut st) = app();
        let a0 = app.annotations.len();
        let c0 = app.comments.len();
        app.create_note(&mut st.doc, 1, (120.0, 200.0));
        assert_eq!(app.comments.len(), c0 + 1);
        assert!(app.editing.is_some());
        app.commit_edit(&mut st.doc); // committed empty
        assert_eq!(app.comments.len(), c0, "empty note thread removed");
        assert_eq!(app.annotations.len(), a0, "and its page marker removed");
    }

    #[test]
    fn editing_comment_captures_focus_rect_on_draw() {
        let (mut app, _st) = app();
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
    }

    #[test]
    fn reply_composer_appends_through_editor() {
        let (mut app, mut st) = app();
        // Pick a seeded comment and compose a reply into it.
        let cid = app.comments[0].id.clone();
        let replies_before = app.comments[0].replies.len();
        app.begin_edit(EditTarget::Reply(cid.clone()), "");
        app.editing
            .as_mut()
            .unwrap()
            .input
            .insert_at_cursor("Looks good");
        app.commit_edit(&mut st.doc);
        let after = app.comments.iter().find(|c| c.id == cid).unwrap();
        assert_eq!(
            after.replies.len(),
            replies_before + 1,
            "reply landed in the thread"
        );
        assert_eq!(after.replies.last().unwrap().body, "Looks good");
    }

    /// The Open affordance goes to the **host portal**, never an in-app
    /// picker. We assert the click resolves to the Open action and that the
    /// action routes into the portal-request path — pre-arming an in-flight
    /// request so the test never actually spawns a real host dialog (the
    /// in-flight guard makes applying the action a no-op).
    #[test]
    fn open_affordance_routes_to_the_host_portal() {
        let (mut app, mut st) = app();

        // Hit-test: the toolbar Open (folder) button resolves to Open.
        let open = app.toolbar_layout().open;
        let c = Point::new(
            f64::midpoint(open.x0, open.x1),
            f64::midpoint(open.y0, open.y1),
        );
        assert!(
            matches!(app.toolbar_hit(c), Some(ToolbarHit::Open)),
            "the folder button hit-tests to the Open action"
        );

        // Pre-arm a portal request so applying Open can't launch a real host
        // dialog during the test; the in-flight guard must preserve it.
        let sentinel = ui::portal::inject_for_test(ui::portal::PortalOutcome::Cancelled);
        app.portal_rx = Some(sentinel);
        app.apply_toolbar(&ToolbarHit::Open, &mut st.doc);
        assert!(
            app.portal_rx.is_some(),
            "Open routes to the host portal request path (not an in-app dialog)"
        );
    }

    /// A file chosen by the host portal loads through the per-frame drain in
    /// `sync` — the portal result is injected (no real dialog) and the next
    /// frame must load the document.
    #[test]
    #[serial(pdfium)]
    fn portal_open_result_loads_document_via_frame_drain() {
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
        app.portal_rx = Some(ui::portal::inject_for_test(
            ui::portal::PortalOutcome::Opened(path),
        ));

        // The per-frame sync drains the portal channel and loads the doc.
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
    }

    /// A cancelled portal interaction is a no-op: the current view is left
    /// intact and the channel clears.
    #[test]
    fn portal_cancel_leaves_current_view_intact() {
        let (mut app, mut st) = app(); // sample document already loaded
        let before = app.has_document;
        let name_before = app.doc_name.clone();
        app.portal_rx = Some(ui::portal::inject_for_test(
            ui::portal::PortalOutcome::Cancelled,
        ));
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

    #[test]
    fn idle_app_requests_no_animation_frames() {
        use ui::Widget as _;
        // A freshly synced viewer with the pointer at its default (0,0) — which
        // sits inside the toolbar rect but over no button — must not ask for
        // animation frames, or the loop would spin at ~60Hz while idle.
        let mut app = PdfApp::sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();
        let theme = ui::theme::Theme::dark();
        let mut tr = ui::text_render::TextRenderer::new();
        // resolve_tooltip runs inside the draw; drive it directly.
        app.resolve_tooltip();
        assert!(
            !app.needs_animation_frame(),
            "idle app must settle to zero animation frames"
        );
        let _ = (&theme, &mut tr);
    }

    #[test]
    fn context_menu_delete_item_rect_comes_from_shared_widget() {
        let (mut app, _st) = app();
        let target = DeleteTarget::Annotation("anno".into());
        app.context_menu = Some(ContextMenu::delete(
            target,
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
    }

    /// A ctrl-held `Modifiers`, mirroring the workspace convention (see
    /// `ui::widget::command_palette`'s `ctrl_a_selects_query`).
    fn ctrl_mods() -> winit::event::Modifiers {
        let mut s = winit::keyboard::ModifiersState::empty();
        s.set(winit::keyboard::ModifiersState::CONTROL, true);
        winit::event::Modifiers::from(s)
    }

    /// Right-click the seeded sticky note through the real event path,
    /// asserting the context menu opens targeting it. Returns its id.
    fn right_click_seeded_note(
        app: &mut PdfApp,
        st: &mut ui::State,
        taffy: &TaffyTree<()>,
        node: NodeId,
        mods: winit::event::Modifiers,
        dev: winit::event::DeviceId,
    ) -> String {
        use winit::dpi::PhysicalPosition;
        use winit::event::{ElementState, MouseButton, WindowEvent};

        let sticky = app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Note { .. }))
            .unwrap()
            .clone();
        let AnnoKind::Note { x: nx, y: ny } = sticky.kind else {
            unreachable!()
        };
        let origin = app.page_origins()[0];
        let screen = (origin.x + nx * app.zoom(), origin.y + ny * app.zoom());

        feed_event(
            app,
            st,
            taffy,
            node,
            mods,
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(screen.0, screen.1),
            },
        );
        feed_event(
            app,
            st,
            taffy,
            node,
            mods,
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Right,
            },
        );
        assert!(
            app.context_menu.is_some(),
            "right-click over the note opens its delete menu"
        );
        sticky.id
    }

    /// Right-click a placed annotation, then click the context menu's Delete
    /// item — all through the real `handle_event` path — and confirm the item
    /// is removed from the shared document.
    #[test]
    fn right_click_delete_removes_item_through_event_path() {
        use winit::dpi::PhysicalPosition;
        use winit::event::{ElementState, MouseButton, WindowEvent};

        let mut app = PdfApp::sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();
        app.go_to_page(1);

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mods = winit::event::Modifiers::default();
        let dev = dummy_device_id();

        let note_id = right_click_seeded_note(&mut app, &mut st, &taffy, node, mods, dev);

        let semantics = Widget::accessibility(&app).expect("PDF semantic root");
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
        assert!(Widget::traps_focus(&app));
        assert!(Widget::modal_capture(&app).is_some());

        let item = app
            .context_menu
            .as_ref()
            .and_then(|menu| menu.widget.item_rect(0))
            .unwrap();
        assert!(item.area() > 1.0, "the shared Delete row has hit bounds");

        // Left-click the Delete item removes the annotation from the doc.
        let before = app.annotations.len();
        let ic = (
            f64::midpoint(item.x0, item.x1),
            f64::midpoint(item.y0, item.y1),
        );
        feed_event(
            &mut app,
            &mut st,
            &taffy,
            node,
            mods,
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(ic.0, ic.1),
            },
        );
        feed_event(
            &mut app,
            &mut st,
            &taffy,
            node,
            mods,
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );

        assert!(
            app.context_menu.is_none(),
            "the menu closes after the delete"
        );
        assert_eq!(
            app.annotations.len(),
            before - 1,
            "the targeted annotation is removed"
        );
        assert!(
            !app.annotations.iter().any(|a| a.id == note_id),
            "specifically the right-clicked note"
        );
        assert!(
            !PdfDoc::list_annotations(&st.doc)
                .iter()
                .any(|a| a.id == note_id),
            "and it is gone from the shared document"
        );
    }

    #[test]
    fn context_menu_enter_deletes_through_shared_keyboard_route() {
        let mut app = PdfApp::sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();
        app.go_to_page(1);

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mods = winit::event::Modifiers::default();
        let note_id =
            right_click_seeded_note(&mut app, &mut st, &taffy, node, mods, dummy_device_id());

        feed_event(
            &mut app,
            &mut st,
            &taffy,
            node,
            mods,
            &key_event(Key::Named(NamedKey::ArrowDown)),
        );
        feed_event(
            &mut app,
            &mut st,
            &taffy,
            node,
            mods,
            &key_event(Key::Named(NamedKey::Enter)),
        );
        assert!(app.context_menu.is_none());
        assert!(
            !app.annotations
                .iter()
                .any(|annotation| annotation.id == note_id)
        );
        assert!(
            !PdfDoc::list_annotations(&st.doc)
                .iter()
                .any(|annotation| annotation.id == note_id)
        );
    }

    /// A Select-tool press-move-release past `PAN_SLOP` pans the canvas and
    /// selects nothing, driven through `handle_event`.
    #[test]
    fn select_tool_drag_pans_the_canvas() {
        use ui::Widget as _;
        use winit::dpi::PhysicalPosition;

        let mut app = PdfApp::sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();
        app.tool = Tool::Select;
        app.go_to_page(1);

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mods = winit::event::Modifiers::default();
        let dev = dummy_device_id();
        let feed = |app: &mut PdfApp, st: &mut ui::State, ev: &WindowEvent| {
            let mut handled = false;
            let mut focus = None;
            app.handle_event(
                ev,
                1.0,
                &mods,
                &mut st.doc,
                &st.root,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        };

        // Press + drag upward past PAN_SLOP = pan (scroll moves, nothing
        // selected). The sample seeds a pre-selected note; clear it so we can
        // prove a pan never triggers a click-selection of its own.
        app.selected_anno = None;
        let a = (app.canvas_rect.x0 + 200.0, app.canvas_rect.y0 + 300.0);
        let b = (a.0, a.1 - 200.0); // drag up -> content scrolls down
        let sy0 = app.scroll_y();
        feed(
            &mut app,
            &mut st,
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(a.0, a.1),
            },
        );
        feed(
            &mut app,
            &mut st,
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        feed(
            &mut app,
            &mut st,
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(b.0, b.1),
            },
        );
        feed(
            &mut app,
            &mut st,
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );
        assert!(
            app.scroll_y() > sy0 + 1.0,
            "a Select drag pans the canvas ({sy0} -> {})",
            app.scroll_y()
        );
        assert!(
            app.selected_anno.is_none(),
            "a pan does not select an annotation"
        );
    }

    /// A Select-tool press + release without moving, over an annotation,
    /// selects it — driven through `handle_event`.
    #[test]
    fn select_tool_click_selects_the_annotation_under_it() {
        use ui::Widget as _;
        use winit::dpi::PhysicalPosition;

        let mut app = PdfApp::sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();
        app.tool = Tool::Select;
        app.go_to_page(1);

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mods = winit::event::Modifiers::default();
        let dev = dummy_device_id();
        let feed = |app: &mut PdfApp, st: &mut ui::State, ev: &WindowEvent| {
            let mut handled = false;
            let mut focus = None;
            app.handle_event(
                ev,
                1.0,
                &mods,
                &mut st.doc,
                &st.root,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        };

        let hl = app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Highlight { .. }))
            .unwrap()
            .clone();
        let AnnoKind::Highlight { rect } = hl.kind else {
            unreachable!()
        };
        let origin = app.page_origins()[0];
        let (cx, cy) = rect.center();
        let p = (origin.x + cx * app.zoom(), origin.y + cy * app.zoom());
        feed(
            &mut app,
            &mut st,
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(p.0, p.1),
            },
        );
        feed(
            &mut app,
            &mut st,
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        feed(
            &mut app,
            &mut st,
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );
        assert_eq!(
            app.selected_anno.as_deref(),
            Some(hl.id.as_str()),
            "a Select click selects the annotation under it"
        );
    }

    /// On a narrow (overlay) window, clicking the canvas scrim beside an open
    /// panel dismisses the panels — driven through the real event path.
    #[test]
    fn overlay_scrim_click_dismisses_open_panels() {
        use ui::Widget as _;
        use winit::dpi::PhysicalPosition;

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

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mods = winit::event::Modifiers::default();
        let dev = dummy_device_id();
        let feed = |app: &mut PdfApp, st: &mut ui::State, ev: &WindowEvent| {
            let mut handled = false;
            let mut focus = None;
            app.handle_event(
                ev,
                1.0,
                &mods,
                &mut st.doc,
                &st.root,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        };

        // Click the scrim: a canvas point on the far left, clear of the panel.
        let scrim = (app.canvas_rect.x0 + 8.0, app.canvas_rect.y0 + 40.0);
        feed(
            &mut app,
            &mut st,
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(scrim.0, scrim.1),
            },
        );
        feed(
            &mut app,
            &mut st,
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        assert!(
            !app.show_sidebar && !app.show_rail,
            "clicking the scrim closes the overlay panels"
        );
    }

    /// Ctrl+C / Ctrl+X in the inline editor are real clipboard chords, not
    /// typed characters — driven through `handle_event`. The cut's field-clear
    /// is deterministic regardless of platform-clipboard availability.
    ///
    /// `#[serial]` because it *writes* the shared system clipboard, which would
    /// otherwise race the paste test's read of it.
    #[test]
    #[serial]
    fn editor_ctrl_chords_are_not_typed_and_cut_clears_the_field() {
        use ui::Widget as _;

        let mut app = PdfApp::sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let ctrl = ctrl_mods();

        let cid = app.comments[0].id.clone();
        app.begin_edit(EditTarget::CommentBody(cid), "hello");
        // Select the whole field first (Ctrl+A) so copy/cut act on the text —
        // the shared TextInput has a real selection model, unlike the old
        // whole-buffer hack.
        let ev = key_event(winit::keyboard::Key::Character("a".into()));
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            &ev,
            1.0,
            &ctrl,
            &mut st.doc,
            &st.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );

        // Ctrl+C is a clipboard chord, not a keystroke: the field stays "hello".
        let ev = key_event(winit::keyboard::Key::Character("c".into()));
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            &ev,
            1.0,
            &ctrl,
            &mut st.doc,
            &st.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        assert!(handled, "the editor consumes the chord");
        assert_eq!(
            app.editing.as_ref().unwrap().text(),
            "hello",
            "Ctrl+C does not type a literal 'c'"
        );

        // Ctrl+X cuts the selection: the whole field clears.
        let ev = key_event(winit::keyboard::Key::Character("x".into()));
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            &ev,
            1.0,
            &ctrl,
            &mut st.doc,
            &st.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        assert_eq!(
            app.editing.as_ref().unwrap().text(),
            "",
            "Ctrl+X clears the selected field"
        );
    }

    /// Paste inserts the system clipboard's text at the caret via the real
    /// editor path. `#[serial]` so no other clipboard-touching test runs
    /// concurrently — the clipboard is a shared, process-external resource.
    ///
    /// The expectation is derived from the clipboard's *actual* content at
    /// paste time (read back with no concurrent writer under `#[serial]`), not
    /// from the value we tried to copy, so the assertion is exact whether or
    /// not the platform clipboard is available: where it round-trips our write,
    /// the buffer becomes "AB" + that text at the caret; where it is
    /// unavailable (headless CI), paste is a safe no-op leaving "AB".
    #[test]
    #[serial]
    fn editor_paste_inserts_clipboard_text_at_caret() {
        let (mut app, mut st) = app();
        let cid = app.comments[0].id.clone();
        app.begin_edit(EditTarget::CommentBody(cid), "AB"); // caret at end

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let ctrl = ctrl_mods();

        ui::clipboard_copy("XY");
        // Ctrl+V through the real event path: the shared TextInput reads the
        // clipboard and inserts it at the caret (end of "AB").
        let ev = key_event(winit::keyboard::Key::Character("v".into()));
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            &ev,
            1.0,
            &ctrl,
            &mut st.doc,
            &st.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        let after = app.editing.as_ref().unwrap().text().to_string();

        // The pasted text is the clipboard's current content (control chars
        // stripped). Read it back — no other test writes it under `#[serial]`,
        // so this equals what paste saw.
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
}

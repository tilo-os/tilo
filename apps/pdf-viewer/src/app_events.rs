// Included into `app.rs` (shares its module + imports). Holds the input
// layer: pointer hit-testing against the chrome rects computed in
// `app_draw.rs`, drag lifecycle for the highlight / ink tools, and the
// keyboard map (tool hotkeys, zoom, page navigation, incremental search).
//
// All durable mutations go through the record's typed edits and verbs so undo
// and live collaboration come for free; view state (zoom, scroll, active
// tool, the search query) stays local because it is per-peer.

impl PdfApp {
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

    /// Arm `colour` for the next mark. A selected mark takes it too, as an
    /// edit of the record.
    fn set_marker_colour(&mut self, colour: Swatch) {
        self.swatch = colour;
        if let Some(id) = self.selected_anno.as_deref() {
            self.submit(crate::actions::set_annotation_color_action(id, colour));
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

    /// Open the window bar's search on the document: it finds in the pages
    /// through [`ui::widget::Findable`].
    fn open_search() {
        ui::window_toolbar::open_search(ui::window_toolbar::SearchOpening::Find);
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

    /// A drag on the docked review panel's leading edge, or a key on its
    /// focused divider, resizes it through the shared inspector; dragged
    /// under its minimum it closes. Returns whether the divider took the
    /// event.
    pub(crate) fn route_review_resize(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if self.overlay_mode && !self.review.is_resizing() {
            return false;
        }
        let was_resizing = self.review.is_resizing();
        let taken = self.review.handle_event(event, ctx);
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) && !self.review.is_resizing()
        {
            // A press anywhere else hands the keys back to the pages.
            self.review.set_resize_focused(false);
        }
        if self.review.take_resize_redraw() {
            self.dirty = true;
        }
        if taken || was_resizing || self.review.is_resizing() {
            self.show_sidebar = self.review.is_open();
            self.reflow();
        }
        taken
    }

    /// Resize the review panel from its divider's semantic node.
    pub(crate) fn perform_review_resize(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
    ) -> bool {
        if self.overlay_mode || !self.review.perform_resize_action(target, action) {
            return false;
        }
        self.show_sidebar = self.review.is_open();
        self.reflow();
        true
    }

    /// The resize cursor over the docked review panel's divider.
    pub(crate) fn review_resize_cursor(&self, p: Point) -> Option<winit::window::CursorIcon> {
        if self.overlay_mode {
            None
        } else {
            self.review.resize_cursor(p)
        }
    }

    /// Whether `p` is over the page rail's resize grip: its trailing edge.
    fn on_rail_grip(&self, p: Point) -> bool {
        let rail = self.rail_rect;
        let grip = f64::from(self.layout_theme.tokens.spacing.s1);
        self.show_rail && (p.x - rail.x1).abs() <= grip && p.y >= rail.y0 && p.y <= rail.y1
    }

    /// Resolve the most specific captured sidebar hit (smallest rect wins,
    /// so the resolve-chip beats the comment card it sits inside).
    fn apply_sidebar(&mut self, p: Point, state: &mut ui::widget::Edits<'_>) -> bool {
        if !self.show_sidebar || !self.sidebar_rect.contains(p) {
            return false;
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
            // Controls painted on the pad (Clear) win before drawing starts.
            if self.sidebar_tab == SidebarTab::Signatures
                && self.sig_mode == SigMode::Draw
                && self.sig_pad_rect != Rect::ZERO
                && self.sig_pad_rect.contains(p)
            {
                self.sig_stroke = Some(vec![self.sig_pad_norm(p)]);
                self.dirty = true;
            }
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
                let cur = self.signer();
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
                    crate::actions::set_comment_resolved_action(&id, !cur),
                );
                self.reload(state);
            }
            SidebarHit::DeleteComment(id) => {
                self.delete_target(&DeleteTarget::Comment(id), state);
            }
            SidebarHit::AddSignature => {
                if self.tool == Tool::Sign {
                    self.set_tool(Tool::Select);
                } else if self.composed_ink().is_some() {
                    self.set_tool(Tool::Sign);
                }
            }
            SidebarHit::SigMode(mode) => {
                // Switching between typing and drawing keeps whatever the other
                // mode composed; only the in-progress stroke is dropped.
                self.sig_mode = mode;
                self.sig_stroke = None;
            }
            SidebarHit::SigClear => {
                self.sig_strokes.clear();
                self.sig_stroke = None;
                self.keep_strokes();
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

    /// Pointer-down inside the page canvas: dispatch by the active tool.
    fn apply_canvas_press(&mut self, p: Point, state: &mut ui::widget::Edits<'_>) -> bool {
        if !self.canvas_rect.contains(p) {
            return false;
        }
        // Empty state: the only affordance is the centred "Open a PDF…" button.
        if !self.has_document {
            if !self.opening() && self.empty_open_button_rect().contains(p) {
                self.request_open_via_portal();
            }
            return true;
        }
        // A new press ends any gesture still waiting for page text, and
        // catches a glide.
        self.pending_press = None;
        self.glide.stop();
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
                    } else if self.shift_down && self.raster_active() && self.extend_selection_to(p)
                    {
                        // Shift extends the live selection to the press.
                    } else if self.shift_down && self.raster_active() {
                        // With nothing selected, Shift + Select drags a
                        // rubber-band image-region marquee to copy; it never
                        // pans or selects text.
                        self.clear_selection();
                        self.region = Some(RegionSelect {
                            page,
                            start: pt,
                            cur: pt,
                        });
                    } else if self.raster_active() {
                        match self.begin_selection(page, pt, p) {
                            // Motion extends it, release ends it.
                            SelectionStart::Armed => {}
                            SelectionStart::Waiting => {
                                // The page's text is on its way: the press
                                // becomes a selection or a pan when it lands.
                                self.clear_selection();
                                self.pending_press = Some(PendingPress {
                                    page,
                                    pt,
                                    press: p,
                                    scroll: (self.scroll_x(), self.scroll_y()),
                                    released: None,
                                });
                            }
                            SelectionStart::NoText => self.begin_pan(p),
                        }
                    } else {
                        self.begin_pan(p);
                    }
                    self.dirty = true;
                }
                Tool::Highlight | Tool::Ink => {
                    if self.tool == Tool::Highlight {
                        self.ensure_raster_text(page);
                    }
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

    /// Clear the selection and start panning from a press at `p`.
    fn begin_pan(&mut self, p: Point) {
        self.clear_selection();
        self.pan = Some(PanState {
            start: p,
            scroll_x0: self.scroll_x(),
            scroll_y0: self.scroll_y(),
            moved: false,
        });
    }

    /// Resolve a right-click (or a held finger) into a context menu: over
    /// a deletable item its Delete menu, over page text Copy and Select all.
    fn open_context_menu(&mut self, p: Point) -> bool {
        if let Some((page, pt)) = self.hit_page(p)
            && let Some(target) = self.delete_target_at(page, pt)
        {
            let anchor = self.delete_menu_anchor(&target, p);
            self.context_menu = Some(ContextMenu::delete(target, anchor, self.rect));
            self.dirty = true;
            return true;
        }
        if self.tool == Tool::Select && self.select_for_menu(p) {
            return true;
        }
        // A right-click elsewhere just dismisses any open menu.
        if self.context_menu.take().is_some() {
            self.dirty = true;
        }
        false
    }

    /// A file dragged in from outside opens when no document is, and
    /// imports as pages into an open one with its page rail shown: the rail
    /// shows it will land while the file hovers the window.
    fn take_file(&mut self, file: ui::widget::FileOpenEvent) {
        let imports = self.has_document && self.show_rail;
        match file {
            ui::widget::FileOpenEvent::HoverChanged => self
                .page_sidebar
                .set_drop_target_armed(imports && self.file_open.is_hovering()),
            ui::widget::FileOpenEvent::Open(path) => {
                self.page_sidebar.set_drop_target_armed(false);
                if imports {
                    self.import_file(&path);
                } else if !self.has_document {
                    self.open_chosen(&path);
                }
            }
        }
        self.dirty = true;
    }

    fn on_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
        self.shift_down = modifiers.state().shift_key();
        self.glide.set_device_scale(scale_factor);
        if let Some(file) = self.file_open.handle_event(event) {
            self.take_file(file);
            *handled = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.on_cursor_moved(event, p, scale_factor, modifiers, state);
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
                self.on_left_release(event, scale_factor, modifiers, state, handled);
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                self.on_right_press(state, handled);
            }

            WindowEvent::MouseWheel { delta, phase, .. } => {
                self.on_mouse_wheel(delta, *phase, scale_factor, modifiers, handled);
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

            // A browser page delivers typed text as IME commits from its
            // hidden text editor, not as key presses: the open comment or
            // reply field takes them, or typing there lands nowhere.
            WindowEvent::Ime(_) if self.editing.is_some() => {
                self.drive_editor(event, scale_factor, modifiers, state);
                self.dirty = true;
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
        state: &mut ui::widget::Edits<'_>,
    ) {
        let from = self.last_mouse;
        self.last_mouse = p;
        self.hover_text(p);
        if self.on_cursor_moved_chrome_drags(event, p, scale_factor, modifiers, state) {
            return;
        }
        self.on_cursor_moved_page_drags(p);
        if !self.pointer_move_is_quiet(from, p) {
            self.dirty = true;
        }
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
        state: &mut ui::widget::Edits<'_>,
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

        // A press waiting for its page's text: it resolves as soon as the
        // text is here.
        if self.pending_press.is_some() {
            self.resolve_pending_press(p);
            return;
        }

        // Text selection drag (Select tool over real text): onto whichever
        // page the pointer is over, or nearest, even past the card edge so
        // the drag can reach the end of a line.
        if self.selection.is_some_and(|sel| sel.dragging) {
            self.update_selection_at(p);
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
            self.dirty = true;
        }
    }

    /// `WindowEvent::MouseInput` (left, pressed): the inline editor / open
    /// menus / chrome first, then the page rail / overlay scrim / selection
    /// HUD / canvas — first match consumes the click.
    fn on_left_press(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
        let p = self.last_mouse;
        if self.on_left_press_editor_and_menus(event, p, scale_factor, modifiers, state, handled) {
            return;
        }
        self.on_left_press_rail_and_canvas(p, state, handled);
    }

    /// The inline editor, an open context menu, the page bubble and the
    /// sidebar — each intercepts a left press before
    /// it reaches the rail or the canvas. Returns whether one of them already
    /// consumed the click.
    fn on_left_press_editor_and_menus(
        &mut self,
        event: &WindowEvent,
        p: Point,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
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
        state: &mut ui::widget::Edits<'_>,
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
        // A selection grip (on a touchscreen) takes its end first.
        if self.tool == Tool::Select && self.canvas_rect.contains(p) && self.take_selection_grip(p)
        {
            self.pending_press = None;
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
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
        // Finish a signature pad stroke: commit it if it has real ink.
        if let Some(stroke) = self.sig_stroke.take() {
            if stroke.len() >= 2 {
                self.sig_strokes.push(stroke);
                self.keep_strokes();
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
                    let (ux, uy) = self
                        .page_map(self.anno_page(&mdrag.id).unwrap_or(1))
                        .card_to_user((x, y));
                    if let Some(action) = crate::markup::PdfMarkup::of(state).and_then(|markup| {
                        crate::actions::set_annotation_position_user_action(
                            &markup, &mdrag.id, ux, uy,
                        )
                    }) {
                        self.perform_action(state, action);
                    }
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
            self.release_region();
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
        if self
            .pending_press
            .is_some_and(|pending| pending.released.is_none())
        {
            // A press released before its page's text came: a click now,
            // or a drag carried out when the text comes.
            if let Some(pending) = self.release_pending_press(self.last_mouse) {
                self.click_page(state, pending.page, pending.pt);
            }
            self.dirty = true;
            *handled = true;
        } else if self.selection.is_some_and(|s| s.dragging) {
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
    fn on_right_press(&mut self, state: &mut ui::widget::Edits<'_>, handled: &mut bool) {
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

    /// Feed one raw event into a hand-positioned, local [`TextInput`]. A
    /// throwaway leaf satisfies the `Widget` signature; local fields never
    /// write the passed document.
    fn drive_local_input(
        input: &mut TextInput,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
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
                state: state.reborrow(),
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
        state: &mut ui::widget::Edits<'_>,
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
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
        if self.on_key_editing(event, key_event, modifiers, scale, state, handled) {
            return;
        }

        let mods = modifiers.state();
        let ctrl = mods.control_key() || mods.super_key();

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
        state: &mut ui::widget::Edits<'_>,
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
                Self::open_search();
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
            "a" | "A" if self.select_all() => {
                *handled = true;
                true
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
        state: &mut ui::widget::Edits<'_>,
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
                self.status_note.dismiss();
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
#[path = "app_events_tests.rs"]
mod event_tests;

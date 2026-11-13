impl PdfApp {
    fn reflow(&mut self) {
        self.recompute_regions();
        self.sync_viewport_frame();
        self.dirty = true;
    }

    fn set_tool(&mut self, t: Tool) {
        self.tool = t;
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
    }

    fn set_marker_colour(&mut self, colour: HighlightColor) {
        self.swatch = colour;
        if let Some(id) = self.selected_anno.as_deref() {
            self.submit(crate::actions::set_annotation_color_action(id, colour));
        }
    }

    fn toggle_sidebar(&mut self) {
        self.show_sidebar = !self.show_sidebar;
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

    pub(crate) fn review_resize_cursor(&self, p: Point) -> Option<winit::window::CursorIcon> {
        if self.overlay_mode {
            None
        } else {
            self.review.resize_cursor(p)
        }
    }

    fn on_rail_grip(&self, p: Point) -> bool {
        let rail = self.rail_rect;
        let grip = f64::from(self.layout_theme.tokens.spacing.s1);
        self.show_rail && (p.x - rail.x1).abs() <= grip && p.y >= rail.y0 && p.y <= rail.y1
    }

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
            if self.sidebar_tab == SidebarTab::Signatures
                && self.sig_mode == SigMode::Draw
                && self.sig_pad_rect != Rect::ZERO
                && self.sig_pad_rect.contains(p)
            {
                self.sig_stroke = Some(vec![self.sig_pad_norm(p)]);
                self.dirty = true;
            }
            return true;
        };
        match hit {
            SidebarHit::Tab(t) => {
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
                    self.selected_anno = c.pinned().map(str::to_owned);
                    self.go_to_page(page);
                }
            }
            SidebarHit::EditComment(id) => {
                if let Some(c) = self.comments.iter().find(|c| c.id == id) {
                    let body = c.comment.text.clone();
                    self.selected_anno = c.pinned().map(str::to_owned);
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
                    .is_some_and(|c| c.comment.resolved);
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

    fn apply_canvas_press(&mut self, p: Point, state: &mut ui::widget::Edits<'_>) -> bool {
        if !self.canvas_rect.contains(p) {
            return false;
        }
        if !self.has_document {
            if !self.opening() {
                if let Some(document) = self.workspace_document_at(p) {
                    let target = document.target.clone();
                    self.open_workspace_document(&target);
                } else if self.empty_open_button_rect().contains(p) {
                    self.request_open_via_portal();
                }
            }
            return true;
        }
        self.pending_press = None;
        self.glide.stop();
        if let Some((page, pt)) = self.hit_page(p) {
            match self.tool {
                Tool::Select => {
                    if let Some((id, nx, ny)) = self.note_marker_at(page, pt) {
                        self.clear_selection();
                        self.selected_anno = Some(id.clone());
                        self.marker_drag = Some(ui::MarkerDrag::begin(id, pt, (nx, ny)));
                    } else if self.shift_down && self.raster_active() && self.extend_selection_to(p)
                    {
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
                            SelectionStart::Armed => {}
                            SelectionStart::Waiting => {
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

    fn begin_pan(&mut self, p: Point) {
        self.clear_selection();
        self.pan = Some(PanState {
            start: p,
            scroll_x0: self.scroll_x(),
            scroll_y0: self.scroll_y(),
            moved: false,
        });
    }

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
        if self.context_menu.take().is_some() {
            self.dirty = true;
        }
        false
    }

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

            WindowEvent::Ime(_) if self.editing.is_some() => {
                self.drive_editor(event, scale_factor, modifiers, state);
                self.dirty = true;
                *handled = true;
            }

            _ => {}
        }
    }

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

    fn on_cursor_moved_chrome_drags(
        &mut self,
        event: &WindowEvent,
        p: Point,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
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

        if self.editing.is_some() {
            self.drive_editor(event, scale_factor, modifiers, state);
            if self.editing.as_ref().is_some_and(Editing::is_dragging) {
                self.dirty = true;
                return true;
            }
        }

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

    fn on_cursor_moved_page_drags(&mut self, p: Point) {
        if self.show_rail && (self.page_sidebar.is_pending_drag() || self.page_sidebar.menu_open())
        {
            if self.page_sidebar.on_motion(p) {
                self.dirty = true;
            }
            return;
        }

        if let Some(region) = self.region {
            let pt = self.page_point(region.page, p);
            if let Some(r) = self.region.as_mut() {
                r.cur = pt;
            }
            self.dirty = true;
            return;
        }

        if self.pending_press.is_some() {
            self.resolve_pending_press(p);
            return;
        }

        if self.selection.is_some_and(|sel| sel.dragging) {
            self.update_selection_at(p);
            return;
        }

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

    fn on_left_press_editor_and_menus(
        &mut self,
        event: &WindowEvent,
        p: Point,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        if self.editing.is_some() {
            if self.edit_rect.contains(p) {
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

    fn on_left_press_rail_and_canvas(
        &mut self,
        p: Point,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
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
        if self.tool == Tool::Select && self.canvas_rect.contains(p) && self.take_selection_grip(p)
        {
            self.pending_press = None;
            *handled = true;
            return;
        }
        if let Some(action) = self.sel_hud.as_ref().and_then(|h| h.hit(p)) {
            self.apply_hud_action(action, state);
            self.dirty = true;
            *handled = true;
            return;
        }
        if self.sel_hud.as_ref().is_some_and(|h| h.contains(p)) {
            *handled = true;
            return;
        }
        if self.apply_canvas_press(p, state) {
            *handled = true;
        }
    }

    fn on_left_release(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
        if let Some(stroke) = self.sig_stroke.take() {
            if stroke.len() >= 2 {
                self.sig_strokes.push(stroke);
                self.keep_strokes();
            }
            self.dirty = true;
            *handled = true;
            return;
        }
        if self.editing.as_ref().is_some_and(Editing::is_dragging) {
            self.drive_editor(event, scale_factor, modifiers, state);
            self.dirty = true;
            *handled = true;
            return;
        }
        if let Some(mdrag) = self.marker_drag.take() {
            if self.is_free_text(&mdrag.id) {
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
                    if let Some(action) = crate::actions::set_annotation_position_user_action(
                        &crate::markup::PdfMarkup::of(state),
                        &mdrag.id,
                        ux,
                        uy,
                    ) {
                        self.perform_action(state, action);
                    }
                    self.reload(state);
                }
            } else {
                if let Some(c) = self
                    .comments
                    .iter()
                    .find(|c| c.pinned() == Some(mdrag.id.as_str()))
                {
                    let cid = c.id.clone();
                    self.focus_sidebar(SidebarTab::Comments, cid);
                }
            }
            self.dirty = true;
            *handled = true;
            return;
        }
        if self.region.is_some() {
            self.release_region();
            *handled = true;
            return;
        }
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
            if let Some(pending) = self.release_pending_press(self.last_mouse) {
                self.click_page(state, pending.page, pending.pt);
            }
            self.dirty = true;
            *handled = true;
        } else if self.selection.is_some_and(|s| s.dragging) {
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

    fn on_right_press(&mut self, state: &mut ui::widget::Edits<'_>, handled: &mut bool) {
        let p = self.last_mouse;
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

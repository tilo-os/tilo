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
        self.recompute_zoom();
        self.clamp_scroll();
        self.dirty = true;
    }

    fn set_tool(&mut self, t: Tool) {
        self.tool = t;
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
        self.reflow();
    }

    fn open_search(&mut self) {
        if !self.search_active {
            self.search_active = true;
            self.reflow();
        }
        if !self.query.is_empty() {
            self.recompute_matches();
        }
        self.dirty = true;
    }

    fn close_search(&mut self) {
        if self.search_active {
            self.search_active = false;
            self.matches.clear();
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
        let origin = origins.get(page.saturating_sub(1)).copied().unwrap_or(Point::ZERO);
        ((p.x - origin.x) / self.zoom, (p.y - origin.y) / self.zoom)
    }

    // ---- pointer hit-testing ----------------------------------------

    fn toolbar_hit(&self, p: Point) -> Option<ToolbarHit> {
        if !self.toolbar_rect.contains(p) {
            return None;
        }
        let lay = self.toolbar_layout();
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
        if lay.fit.contains(p) {
            return Some(ToolbarHit::Fit);
        }
        if lay.search.contains(p) {
            return Some(ToolbarHit::Search);
        }
        if lay.toggle_sidebar.contains(p) {
            return Some(ToolbarHit::ToggleSidebar);
        }
        // A click anywhere else on the toolbar is still "consumed" so it
        // doesn't fall through to the canvas behind it.
        Some(ToolbarHit::Tool(self.tool))
    }

    fn apply_toolbar(&mut self, hit: ToolbarHit, state: &mut AutoCommit) {
        match hit {
            ToolbarHit::Tool(t) => self.set_tool(t),
            ToolbarHit::Swatch(s) => {
                self.swatch = s;
                if let Some(id) = self.selected_anno.clone() {
                    PdfDoc::set_annotation_color(state, &id, s);
                    self.commit_pending = true;
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
            ToolbarHit::ToggleSidebar => self.toggle_sidebar(),
        }
    }

    /// Resolve the most specific captured sidebar hit (smallest rect wins,
    /// so the resolve-chip beats the comment card it sits inside).
    fn apply_sidebar(&mut self, p: Point, state: &mut AutoCommit) -> bool {
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
            // Empty sidebar space still swallows the click.
            return true;
        };
        match hit {
            SidebarHit::Tab(t) => self.sidebar_tab = t,
            SidebarHit::Outline(page) => self.go_to_page(page),
            SidebarHit::Comment(id) => {
                if let Some(c) = self.comments.iter().find(|c| c.id == id) {
                    let page = c.page;
                    self.selected_anno = c.anno_id.clone();
                    self.go_to_page(page);
                }
            }
            SidebarHit::Resolve(id) => {
                let cur = self
                    .comments
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| c.resolved)
                    .unwrap_or(false);
                PdfDoc::set_comment_resolved(state, &id, !cur);
                self.commit_pending = true;
                self.reload(state);
            }
            SidebarHit::AddSignature => self.set_tool(Tool::Sign),
            SidebarHit::SigJump(i) => {
                if let Some(s) = self.signatures.get(i) {
                    let page = s.page;
                    self.go_to_page(page);
                }
            }
        }
        self.dirty = true;
        true
    }

    fn apply_rail(&mut self, p: Point) -> bool {
        if !self.rail_rect.contains(p) {
            return false;
        }
        for (num, r) in self.thumbnail_rects() {
            if r.contains(p) {
                self.go_to_page(num);
                break;
            }
        }
        true
    }

    fn apply_status(&mut self, p: Point) -> bool {
        if !self.status_rect.contains(p) {
            return false;
        }
        let (prev, _label, next) = self.status_layout();
        if prev.contains(p) {
            self.prev_page();
        } else if next.contains(p) {
            self.next_page();
        }
        true
    }

    /// Pointer-down inside the page canvas: dispatch by the active tool.
    fn apply_canvas_press(&mut self, p: Point, state: &mut AutoCommit) -> bool {
        if !self.canvas_rect.contains(p) {
            return false;
        }
        let Some((page, pt)) = self.hit_page(p) else {
            // Click on empty canvas clears any selection.
            self.selected_anno = None;
            self.dirty = true;
            return true;
        };
        match self.tool {
            Tool::Select => self.select_at(page, pt),
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
        true
    }

    // ---- top-level dispatch -----------------------------------------

    fn on_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                if let Some(page) = self.drag.as_ref().map(|d| d.page) {
                    let pt = self.page_point(page, p);
                    if let Some(d) = self.drag.as_mut() {
                        d.cur = pt;
                        if d.tool == Tool::Ink {
                            let push = d
                                .points
                                .last()
                                .map(|l| (l.0 - pt.0).hypot(l.1 - pt.1) > 1.5)
                                .unwrap_or(true);
                            if push {
                                d.points.push(pt);
                            }
                        }
                    }
                }
                self.dirty = true;
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_mouse;
                if let Some(hit) = self.toolbar_hit(p) {
                    self.apply_toolbar(hit, state);
                    *handled = true;
                    return;
                }
                if self.search_active && self.search_rect.contains(p) {
                    *handled = true;
                    return;
                }
                if self.apply_status(p) {
                    *handled = true;
                    return;
                }
                if self.apply_sidebar(p, state) {
                    *handled = true;
                    return;
                }
                if self.apply_rail(p) {
                    *handled = true;
                    return;
                }
                if self.apply_canvas_press(p, state) {
                    *handled = true;
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.drag.is_some() {
                    self.commit_drag(state);
                    *handled = true;
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64 * 56.0,
                    MouseScrollDelta::PixelDelta(pos) => pos.y / scale_factor,
                };
                self.scroll_y -= dy;
                self.clamp_scroll();
                self.current_page = self.nearest_page();
                self.dirty = true;
                *handled = true;
            }

            WindowEvent::KeyboardInput { event: key_event, .. }
                if key_event.state == ElementState::Pressed =>
            {
                self.on_key(key_event, modifiers, state, handled);
            }

            _ => {}
        }
    }

    fn on_key(
        &mut self,
        key_event: &winit::event::KeyEvent,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        let mods = modifiers.state();
        let ctrl = mods.control_key() || mods.super_key();

        // While the search strip is open it owns the keyboard.
        if self.search_active {
            match &key_event.logical_key {
                Key::Named(NamedKey::Escape) => self.close_search(),
                Key::Named(NamedKey::Enter) => {
                    self.step_match(if mods.shift_key() { -1 } else { 1 })
                }
                Key::Named(NamedKey::Backspace) => {
                    self.query.pop();
                    self.recompute_matches();
                }
                Key::Named(NamedKey::Space) => {
                    self.query.push(' ');
                    self.recompute_matches();
                }
                Key::Character(c) => {
                    if ctrl && c.as_str().eq_ignore_ascii_case("f") {
                        self.close_search();
                    } else if !ctrl {
                        for ch in c.chars() {
                            if !ch.is_control() {
                                self.query.push(ch);
                            }
                        }
                        self.recompute_matches();
                    }
                }
                _ => {}
            }
            *handled = true;
            return;
        }

        // Ctrl/Cmd-F opens search from anywhere.
        if ctrl {
            if let Key::Character(c) = &key_event.logical_key {
                if c.as_str().eq_ignore_ascii_case("f") {
                    self.open_search();
                    *handled = true;
                    return;
                }
            }
        }

        match &key_event.logical_key {
            Key::Character(c) => {
                let consumed = match c.as_str() {
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
                        self.fit_page();
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
                    "]" => {
                        self.next_page();
                        true
                    }
                    " " => {
                        self.next_page();
                        true
                    }
                    _ => false,
                };
                if consumed {
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Escape) => {
                self.selected_anno = None;
                self.dirty = true;
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown)
            | Key::Named(NamedKey::PageDown)
            | Key::Named(NamedKey::Space) => {
                self.next_page();
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::PageUp) => {
                self.prev_page();
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
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                if let Some(id) = self.selected_anno.take() {
                    PdfDoc::delete_annotation(state, &id);
                    self.commit_pending = true;
                    self.reload(state);
                    self.dirty = true;
                    *handled = true;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod event_tests {
    use super::*;

    fn app() -> (PdfApp, ui::State) {
        let mut app = PdfApp::new();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.recompute_zoom();
        app.clamp_scroll();
        (app, st)
    }

    #[test]
    fn toolbar_tool_buttons_select_tools() {
        let (mut app, mut st) = app();
        let lay = app.toolbar_layout();
        for (tool, r) in lay.tools.clone() {
            let p = Point::new((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
            let hit = app.toolbar_hit(p).expect("a toolbar hit");
            app.apply_toolbar(hit, &mut st.doc);
            assert_eq!(app.tool, tool, "clicking {:?} should arm it", tool);
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
        app.apply_toolbar(ToolbarHit::Swatch(Swatch::Green), &mut st.doc);
        let after = app.annotations.iter().find(|a| a.id == hl.id).unwrap();
        assert_eq!(after.color, Swatch::Green);
        assert!(app.commit_pending);
    }

    #[test]
    fn wheel_scrolls_and_tracks_page() {
        let (mut app, _st) = app();
        let p0 = app.current_page;
        app.scroll_y = 0.0;
        // Push far enough to cross into a later page.
        app.scroll_y = app.max_scroll();
        app.current_page = app.nearest_page();
        assert!(app.current_page >= p0);
        assert_eq!(app.current_page, app.doc.page_count());
    }

    #[test]
    fn resolve_chip_beats_card_smallest_rect_wins() {
        let (mut app, mut st) = app();
        // Draw once to populate sidebar_hits.
        let mut scene = vello::Scene::new();
        let theme = ui::theme::Theme::dark();
        let mut tr = ui::text_render::TextRenderer::new();
        app.sidebar_tab = SidebarTab::Comments;
        let hits = app.draw_sidebar(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        app.sidebar_hits = hits;
        // Find a resolve chip and click its centre.
        let chip = app
            .sidebar_hits
            .iter()
            .find_map(|(h, r)| match h {
                SidebarHit::Resolve(id) => Some((id.clone(), *r)),
                _ => None,
            });
        if let Some((id, r)) = chip {
            let before = app.comments.iter().find(|c| c.id == id).unwrap().resolved;
            let p = Point::new((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
            assert!(app.apply_sidebar(p, &mut st.doc));
            let after = app.comments.iter().find(|c| c.id == id).unwrap().resolved;
            assert_ne!(before, after, "resolve chip should toggle, not open the card");
        }
    }
}

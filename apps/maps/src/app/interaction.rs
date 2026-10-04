use super::{
    AutoCommit, Hit, Key, MapInfo, MapsApp, NamedKey, Panel, SearchHit, SearchKeyEffect,
    WindowEvent,
};

impl MapsApp {
    /// Route a keystroke into the focused text field (search or an endpoint).
    /// Enter runs the corresponding search; any text edit is mirrored into the
    /// model (`query` / `*_query`) so the existing geocode / dropdown logic runs
    /// unchanged. Returns whether the key was consumed.
    pub(super) fn route_field_key(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
    ) -> bool {
        if self.endpoints.is_focused() {
            if let Some(effect) = self.endpoints.handle_key(event, scale, modifiers, doc) {
                self.apply_endpoint_effect(effect);
            }
            return true;
        }
        if self.search.is_focused() {
            match self.search.handle_key(event, scale, modifiers, doc) {
                SearchKeyEffect::None | SearchKeyEffect::Dismissed => {}
                SearchKeyEffect::Changed(text) => {
                    self.model.query = text;
                    self.on_search_input();
                }
                SearchKeyEffect::Submit => self.run_search(true),
            }
            return true;
        }
        false
    }

    /// Drive one raw event into the ⋯ overflow [`ContextMenu`] (hand-laid: the
    /// menu hit-tests against its own `open_at` rect). Poll `take_selection`
    /// afterwards.
    pub(super) fn drive_overflow(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
    ) {
        crate::search_box::drive_hand_laid(&mut self.overflow, event, scale, modifiers, doc);
    }

    /// Map an overflow-menu row selection to app state. Index 3 hands the live
    /// route surface to the same shell host used by calls and browser `PiP`.
    pub(super) fn overflow_select(&mut self, idx: usize) {
        match idx {
            0 => self.info_sheet.open(MapInfo::Attribution),
            1 => self.offline_panel.open(self.tiles.cache_bytes()),
            2 => self.info_sheet.open(MapInfo::Licenses),
            3 if self.model.route.is_some() => {
                let rect = self.navigation_rect();
                self.navigation_placement.detach_to_desktop(rect);
                self.pending_floating = self.active_navigation_request();
            }
            _ => {}
        }
    }

    /// Forward a `CursorMoved` to all field inputs so their hover state tracks
    /// (a click then lands a caret; a press-drag extends a selection).
    pub(super) fn forward_move_to_field_inputs(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
    ) {
        self.search.forward_pointer(event, scale, modifiers, doc);
        self.endpoints.forward_pointer(event, scale, modifiers, doc);
    }

    /// Forward a press to the field input `origin`/dest/search just focused so
    /// it places its caret at the click point.
    pub(super) fn forward_press_to_field(
        &mut self,
        which: &Hit,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
    ) {
        if matches!(which, Hit::Search(SearchHit::Field)) {
            self.search.forward_press(event, scale, modifiers, doc);
        }
    }

    pub(super) fn handle_key(&mut self, key: &Key, modifiers: winit::event::Modifiers) -> bool {
        // A Directions endpoint field (From / To) has keyboard focus: typed
        // characters geocode that endpoint, mirroring the top search field.
        // Focused text fields (search / endpoints) are handled by
        // `route_field_key`; `handle_key` runs only when no field has keyboard
        // focus, so it dispatches the map-level shortcuts.
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        match key {
            Key::Character(s) if s.as_str() == "/" => {
                self.search.set_focused(true);
                true
            }
            Key::Character(s) if s.as_str() == "+" || s.as_str() == "=" => {
                self.zoom_by(1.0, self.view_center());
                true
            }
            Key::Character(s) if s.as_str() == "-" || s.as_str() == "_" => {
                self.zoom_by(-1.0, self.view_center());
                true
            }
            Key::Character(s) if s.eq_ignore_ascii_case("d") => {
                self.enter_directions();
                true
            }
            Key::Character(s) if s.eq_ignore_ascii_case("t") => {
                self.enter_transit();
                true
            }
            Key::Character(s) if s.eq_ignore_ascii_case("f") => {
                // Fit whatever is shown: a road route, or — in Transit mode
                // — the selected itinerary's geometry, or the endpoints.
                self.fit_current();
                true
            }
            Key::Character(s) if s.eq_ignore_ascii_case("s") && !ctrl => {
                self.toggle_save_selected();
                true
            }
            Key::Character(s) if s.as_str() == "1" => {
                self.set_panel(Panel::Explore);
                true
            }
            Key::Character(s) if s.as_str() == "2" => {
                self.set_panel(Panel::Directions);
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.pan_by(-80.0, 0.0);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.pan_by(80.0, 0.0);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.pan_by(0.0, -80.0);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.pan_by(0.0, 80.0);
                true
            }
            Key::Named(NamedKey::Escape) => {
                if self.overflow.is_open() {
                    self.overflow.close();
                } else if self.offline_panel.is_open() {
                    self.offline_panel.close();
                } else if self.selected_poi.is_some() {
                    self.selected_poi = None;
                } else if self.overlay_mode() && self.nav_open {
                    self.nav_open = false;
                } else {
                    self.model.selected = None;
                }
                true
            }
            Key::Character(s) if s.eq_ignore_ascii_case("m") => {
                // Only reachable while the dialog is closed (once open, its city
                // field owns the keyboard), so this opens it — focusing the
                // search field. Escape / ✕ / the scrim close it.
                if !self.offline_panel.is_open() {
                    self.offline_panel.open(self.tiles.cache_bytes());
                }
                true
            }
            _ => false,
        }
    }
}

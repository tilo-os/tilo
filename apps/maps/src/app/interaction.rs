use super::{Key, MapInfo, MapsApp, NamedKey, Panel, WindowEvent};

impl MapsApp {
    pub(super) fn route_field_key(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if self.endpoints.is_focused() {
            if let Some(effect) = self.endpoints.handle_key(event, scale, modifiers, doc) {
                self.apply_endpoint_effect(effect);
            }
            return true;
        }
        false
    }

    pub(super) fn drive_overflow(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        crate::hand_laid::drive(&mut self.overflow, event, scale, modifiers, doc);
    }

    pub(super) fn overflow_select(&mut self, idx: usize) {
        if self.menu_for == super::MapMenu::Layers {
            if idx == 0 {
                self.set_traffic(!self.map_controls.traffic_enabled());
            }
            return;
        }
        match idx {
            0 => self.info_sheet.open(MapInfo::Attribution),
            1 => self.offline_panel.open(self.tiles.cache_bytes()),
            2 => self.info_sheet.open(MapInfo::Licenses),
            _ => {}
        }
    }

    pub(super) fn forward_move_to_field_inputs(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        self.endpoints.forward_pointer(event, scale, modifiers, doc);
    }

    pub(super) fn handle_key(&mut self, key: &Key, modifiers: winit::event::Modifiers) -> bool {
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        match key {
            Key::Character(s) if s.as_str() == "+" || s.as_str() == "=" => {
                self.zoom_by(1.0, self.visible_map_area().center());
                true
            }
            Key::Character(s) if s.as_str() == "-" || s.as_str() == "_" => {
                self.zoom_by(-1.0, self.visible_map_area().center());
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
                if !self.offline_panel.is_open() {
                    self.offline_panel.open(self.tiles.cache_bytes());
                }
                true
            }
            _ => false,
        }
    }
}

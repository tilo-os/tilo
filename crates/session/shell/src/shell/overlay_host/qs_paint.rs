//! Quick settings painting: transitions, pages, drill-ins and notification history.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Prepare the mutable Quick Settings state before either renderer decides
    /// whether the strip ribbon is visible for this frame.
    pub(super) fn prepare_quick_settings_transition(
        &mut self,
        theme: &Theme,
        flags: crate::shell::chrome_state::OverlayFlags,
    ) {
        self.reconcile_quick_settings_open_edge(flags.quick_settings);
        self.apply_quick_settings_readings();
        self.refresh_now_playing();
        let stack = self
            .chrome
            .as_ref()
            .map(|chrome| chrome.borrow().quick_settings.nav_stack.clone())
            .unwrap_or_default();
        let now = ui::time::now();
        let was_visible = self.qs_motion.is_visible(now);
        if let Some(chrome) = self.chrome.clone() {
            let mut st = chrome.borrow_mut();
            if !flags.quick_settings && !was_visible && st.quick_settings.section.is_some() {
                // A panel that has gone opens next time with its sections
                // closed.
                st.quick_settings.section = None;
                st.quick_settings.wifi_password = None;
            }
            if was_visible {
                // An open section grows the panel on the same disclosure
                // motion the panel's height follows below.
                self.qs_root.follow_sections(
                    &st.quick_settings,
                    theme.motion(),
                    self.input_source,
                    now,
                );
            } else {
                self.qs_root.settle_sections(&st.quick_settings);
            }
        }
        let height = self.quick_settings_rect().height();
        self.qs_motion.follow(&qs_motion::QsFrame {
            open: flags.quick_settings,
            stack: &stack,
            height,
            motion: theme.motion(),
            source: self.input_source,
            now,
        });
    }

    /// Reconcile Quick Settings' retained widget with the authoritative
    /// chrome flag before another input edge can arrive. Open requests can
    /// originate in top-bar pointer/accessibility controls, media chrome,
    /// search actions, or configured shortcuts; none of those callers should
    /// need to know that the popover owns a retained keyboard cursor.
    pub(super) fn reconcile_quick_settings_open_edge(&mut self, open: bool) {
        if open && !self.qs_was_open {
            self.qs_refresh.kick_fast();
            self.qs_refresh.kick_firmware();
            // The Wi-Fi and Bluetooth tiles name what is connected, which
            // only these lists know; without them both read "Not connected"
            // until a drill-in.
            self.qs_refresh.kick_wifi_networks();
            self.qs_refresh.kick_bt_devices();
            self.qs_root.reset_focus();
            self.refresh_quick_settings_summaries();
        }
        if open != self.qs_was_open {
            self.reconcile_now_playing_open_edge(open);
        }
        self.qs_was_open = open;
    }

    /// Counts the root page shows on its Devices and Clipboard rows. Read
    /// once per opening: both panes re-read their full lists on entry.
    pub(super) fn refresh_quick_settings_summaries(&self) {
        let fabric = self.device_fabric_call(DeviceFabricUiAction::Snapshot).ok();
        let personal_devices = fabric.as_ref().map(|view| view.devices.len());
        let sound_outputs = fabric.as_ref().map(sound_outputs_of);
        let clipboard_items = self
            .clipboard_manager_call(ClipboardManagerAction::Snapshot)
            .len();
        if let Some(chrome) = self.chrome.as_ref() {
            let mut st = chrome.borrow_mut();
            st.quick_settings.personal_devices = personal_devices;
            st.quick_settings.clipboard_items = clipboard_items;
            st.quick_settings.notification_count = st.notification_history().len();
            // Without a device fabric (a capture, a test) the outputs stay
            // as they are.
            if let Some(outputs) = sound_outputs {
                st.quick_settings.sound_outputs = outputs;
            }
        }
    }

    /// The quick-settings dropdown: no scrim, so it reads as a dropdown
    /// rather than a modal.
    pub(super) fn draw_quick_settings_dropdown(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<Rect> {
        let now = ui::time::now();
        if self.qs_motion.is_visible(now) {
            // The panel appears from the tray and goes back to it
            // (`ui::motion::Appear`); the same pose covers both directions.
            let pose = self.quick_settings_pose(now);
            let in_flight = !pose.is_rest();
            let qs_rect = self.qs_motion.drawn_rect(self.quick_settings_rect(), now);
            let card_rect = self
                .now_playing_view()
                .and_then(|view| self.now_playing_rect(&view));
            let clip = Rect::new(
                qs_rect.x0 - 8.0,
                qs_rect.y0 - 16.0,
                qs_rect.x1 + 8.0,
                qs_rect.y1 + 8.0,
            );
            let clip = card_rect.map_or(clip, |card| clip.union(card.inflate(8.0, 8.0)));
            let mut card_rect = None;
            pose.paint(scene, transform, clip, |scene, qs_xform| {
                // A Panel casts `e4` (HIG §9.1) along the card's own outline
                // (`ChromeRole::Panel.radius`, the radius
                // `draw_quick_settings_card_background` paints); the pose's
                // opacity fades it with the panel.
                ui::surface::shadow(
                    scene,
                    qs_xform,
                    qs_rect,
                    ui::chrome::ChromeRole::Panel.radius(theme, qs_rect.height()),
                    &theme.tokens.elevation.floating,
                );
                self.draw_quick_settings(scene, qs_xform, text_renderer, theme);
                card_rect = self.draw_now_playing(scene, qs_xform, text_renderer, theme);
            });
            // The in-flight layer clips every QS command to `clip`. Once
            // settled, the damage bounds cover the `e4` shadow's reach.
            return Some(if in_flight {
                transform.transform_rect_bbox(clip)
            } else {
                let settled = quick_settings_settled_bounds(qs_rect, theme, transform);
                card_rect.map_or(settled, |card| {
                    settled.union(quick_settings_settled_bounds(card, theme, transform))
                })
            });
        }
        None
    }

    pub(super) fn draw_quick_settings(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = ui::time::now();
        let settled = self.quick_settings_rect();
        // The card's height follows the page on the expand/collapse motion;
        // each page is laid out at its own height and clipped by the card.
        let rect = self.qs_motion.drawn_rect(settled, now);
        Self::draw_quick_settings_card_background(scene, transform, theme, rect);

        // Every page paints inside the Panel: a discovery list taller than
        // the popover's height budget ends at the panel's edge instead of
        // running on over the desktop inset below it.
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &RoundedRect::from_rect(
                rect,
                ui::chrome::ChromeRole::Panel.radius(theme, rect.height()),
            ),
        );
        let motion = self.motion.get();
        // A drill in or back: the page being left is drawn first, at its
        // own depth (`ui::motion::Drill`); the page on top is drawn last, so
        // the hit targets it records are the ones input reaches.
        if let Some((leaving, pose)) = self.qs_motion.leaving(now, settled.width(), motion) {
            let leaving = leaving.to_vec();
            let shown = self.swap_quick_settings_stack(leaving);
            let leaving_rect = self.quick_settings_rect();
            pose.paint(scene, transform, leaving_rect, |scene, xform| {
                self.draw_quick_settings_page(scene, xform, text_renderer, theme, leaving_rect);
            });
            self.swap_quick_settings_stack(shown);
        }
        let pose = self.qs_motion.current(now, settled.width(), motion);
        pose.paint(scene, transform, settled, |scene, xform| {
            self.draw_quick_settings_page(scene, xform, text_renderer, theme, settled);
        });
        scene.pop_layer();
    }

    /// One Quick Settings page: the one on top of the chrome state's stack.
    pub(super) fn draw_quick_settings_page(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        let qs = self
            .chrome
            .as_ref()
            .map(|c| c.borrow().quick_settings.clone())
            .unwrap_or_default();
        // Root level renders the action row + sliders + toggles; Wifi/
        // Bluetooth levels replace the body with a header + list.
        if !self.draw_quick_settings_drill_in(scene, transform, text_renderer, theme, &qs) {
            self.sync_wifi_password_input();
            let ready = qs
                .wifi_password
                .as_ref()
                .is_some_and(|prompt| prompt.can_submit(self.wifi_password_input.value()));
            self.qs_root.set_wifi_password_ready(ready);
            self.qs_root.draw(
                scene,
                transform,
                text_renderer,
                theme,
                rect,
                self.last_cursor,
                &qs,
            );
            // The inline password's field is the host's secret input, drawn
            // where the Wi-Fi section leaves room for it.
            if let Some(prompt) = qs.wifi_password.as_ref()
                && let Some(field) = self.qs_root.wifi_password_field_rect(&qs)
            {
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    1.0,
                    transform,
                    &self.qs_root.shown_rect(),
                );
                self.draw_qs_wifi_password_field(scene, transform, text_renderer, theme, field);
                if prompt.error.is_some() {
                    // A rejected password outlines its field in the danger
                    // colour, over the field's own edge.
                    ui::widget::paint_invalid_edge(scene, transform, field, theme);
                }
                scene.pop_layer();
            }
        }
    }

    /// Put `stack` on the chrome state for drawing the page being left, and
    /// return the stack it replaced. Input never sees the swap: it happens
    /// and is undone within one draw.
    pub(super) fn swap_quick_settings_stack(
        &self,
        stack: Vec<crate::shell::quick_settings::QsLevel>,
    ) -> Vec<crate::shell::quick_settings::QsLevel> {
        self.chrome.as_ref().map_or_else(Vec::new, |chrome| {
            std::mem::replace(&mut chrome.borrow_mut().quick_settings.nav_stack, stack)
        })
    }

    /// The quick-settings popover's elevated card: shadow, fill and
    /// border. Panels are elevated (HIG §9.1); a flat tint only reads as
    /// deliberate on dark, where the overlay tier is much darker than
    /// its ground — light and high-contrast had nothing lifting it.
    pub(super) fn draw_quick_settings_card_background(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
    ) {
        let radius = ui::chrome::ChromeRole::Panel.radius(theme, rect.height());
        let card = RoundedRect::from_rect(rect, radius);
        ui::chrome::shell::panel_shadow(scene, transform, rect, radius, theme, 1.0);
        ui::floating_surface::paint_material(
            scene,
            transform,
            &card,
            ui::floating_surface::Material::Floating,
            theme,
        );
        // The Panel edge is derived (HIG §9.5).
        ui::chrome::shell::paint_panel_edge(scene, transform, rect, radius, theme);
        ui::surface::floating_edge(scene, transform, rect, radius, theme);
    }

    /// Root level renders the action row + sliders + toggles below;
    /// Wifi/Bluetooth/VPN/Notifications levels replace the whole body
    /// with a header + list and return `true` so the caller stops.
    pub(super) fn draw_quick_settings_drill_in(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) -> bool {
        match qs.current_level() {
            crate::shell::quick_settings::QsLevel::BluetoothDetails(address) => {
                self.draw_qs_bluetooth_details(scene, transform, text_renderer, theme, qs, address);
                true
            }
            crate::shell::quick_settings::QsLevel::Notifications => {
                self.draw_qs_notification_history(scene, transform, text_renderer, theme);
                true
            }
            crate::shell::quick_settings::QsLevel::Root => false,
        }
    }

    /// Draw the `QuickSettings` drill-in header — back-chevron on the left
    /// and a centred title. Used for both `WiFi` and Bluetooth panes; the
    /// back-chevron's hit rect is exposed via [`Self::qs_back_chevron_rect`].
    pub(super) fn draw_qs_drill_in_header(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        title: &str,
    ) {
        let panel = self.quick_settings_rect();
        self.qs_aux_drill_in.layout(panel, 0);
        self.qs_aux_drill_in.draw_header(
            scene,
            transform,
            text_renderer,
            theme,
            panel,
            self.last_cursor,
            title,
        );
    }

    /// Centred dim empty-state message for a drill-in pane with no rows —
    /// without it the pane read as broken rather than "nothing here yet".
    pub(super) fn draw_qs_drill_in_empty(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        message: &str,
    ) {
        DrillInList::draw_empty(
            scene,
            transform,
            text_renderer,
            theme,
            self.quick_settings_rect(),
            message,
        );
    }

    #[cfg(test)]
    pub(super) fn qs_notification_clear_rect(&self) -> Rect {
        self.qs_notification_layout().clear()
    }

    #[cfg(test)]
    pub(super) fn qs_notification_undo_rect(&self) -> Rect {
        self.qs_notification_layout().undo()
    }

    pub(super) fn qs_notification_back_rect(&self) -> Rect {
        self.qs_notification_layout().back()
    }

    /// Paint the notification centre. History lives in chrome rather than
    /// Settings: Settings owns delivery policy, while the shell is the only
    /// authority that actually received and retired these notifications.
    pub(super) fn draw_qs_notification_history(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        let layout = self.qs_notification_layout();
        self.notification_center.draw(
            &ChromeNotifications::new(chrome).with_agents(&self.room_activity),
            &layout,
            scene,
            transform,
            text_renderer,
            theme,
            self.last_cursor,
        );
    }

    pub(super) fn qs_device_row_rect(&self, index: usize) -> Rect {
        let panel = self.quick_settings_rect();
        let y0 = panel.y0 + 78.0 + f64_of_usize(index) * 58.0;
        Rect::new(panel.x0 + 12.0, y0, panel.x1 - 12.0, y0 + 52.0)
    }

    /// Row `index` of the Devices section, where the page shows it while
    /// that section is open.
    pub(super) fn qs_devices_row_rect(&self, index: usize) -> Rect {
        self.chrome
            .as_ref()
            .and_then(|chrome| {
                self.qs_root.hosted_row_rect(
                    &chrome.borrow().quick_settings,
                    crate::shell::quick_settings::QsSection::Devices,
                    index,
                )
            })
            .unwrap_or(Rect::ZERO)
    }

    /// Read the open section's rows and lay the page out settled, as the
    /// next frame would.
    #[cfg(test)]
    pub(super) fn lay_out_quick_settings_for_tests(&mut self) {
        self.refresh_qs_hosted_rows();
        let panel = self.quick_settings_rect();
        if let Some(chrome) = self.chrome.clone() {
            let st = chrome.borrow();
            self.qs_root
                .lay_out_settled_for_tests(panel, &st.quick_settings);
        }
    }

    /// Open the Devices section in place and lay it out.
    #[cfg(test)]
    pub(super) fn open_devices_for_tests(&mut self) {
        if let Some(chrome) = self.chrome.clone() {
            let mut st = chrome.borrow_mut();
            if st.quick_settings.section != Some(crate::shell::quick_settings::QsSection::Devices)
                || !st.overlays.quick_settings
            {
                st.open_quick_settings_section(crate::shell::quick_settings::QsSection::Devices);
            }
        }
        self.lay_out_quick_settings_for_tests();
    }
}

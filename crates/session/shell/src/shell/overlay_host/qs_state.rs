//! Quick settings readings, geometry and list scrolling.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Open the session menu and optionally arm Shut down, then pin its
    /// entrance animation so deterministic screenshots capture the resting
    /// production surface rather than a wall-clock-dependent intermediate.
    pub fn force_power_menu_for_visual_test(&mut self, select_shutdown: bool) {
        self.layers.power_menu.open();
        if select_shutdown {
            use winit::keyboard::{Key, NamedKey};
            for _ in 0..5 {
                let _ = self
                    .layers
                    .power_menu
                    .key(&Key::Named(NamedKey::ArrowDown), false);
            }
        }
        self.layers.power_menu.force_settled_for_visual_test();
    }

    /// Folds every reading the quick-settings workers delivered into
    /// chrome state. Never blocks — the reads happen on a worker, since
    /// a shell-out on the paint path froze the desktop. Each field
    /// applies only if actually read, so a missing daemon leaves the
    /// previous value rather than blanking it.
    /// Bind the owning native session's existing event-loop wake.
    pub fn set_quick_settings_wake(&mut self, wake: crate::session_runtime::Wake) {
        self.qs_action_slot.set_radio_wake(Arc::clone(&wake));
        self.qs_refresh.set_host_wake(wake);
    }

    pub(super) fn apply_quick_settings_readings(&mut self) -> bool {
        let readings = self.qs_refresh.drain();
        if readings.is_empty() {
            return false;
        }
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        for reading in readings {
            let mut st = chrome.borrow_mut();
            self.apply_one_qs_reading(&mut st, reading);
        }
        true
    }

    /// Applies a single quick-settings worker reading to chrome state and
    /// bumps the revision — the per-iteration body of
    /// [`Self::apply_quick_settings_readings`].
    pub(super) fn apply_one_qs_reading(
        &mut self,
        st: &mut crate::shell::chrome_state::ChromeState,
        reading: crate::shell::quick_settings::refresh::QsReading,
    ) {
        use crate::shell::quick_settings::refresh::QsReading;
        match reading {
            QsReading::Fast(reading) => self.apply_fast_qs_reading(st, *reading),
            QsReading::Field(reading) => self.apply_qs_field_reading(st, reading),
            QsReading::WifiNetworks(result) => {
                st.quick_settings.wifi_scanning = false;
                match result {
                    Ok(nets) => {
                        st.quick_settings.wifi_scan_error = None;
                        Self::apply_qs_wifi_networks(st, nets);
                    }
                    Err(error) => {
                        tracing::warn!(target: "tilo::os_actions", op = "list_wifi_networks", %error, "Wi-Fi scan failed");
                        st.quick_settings.wifi_scan_error = Some(error);
                    }
                }
            }
            QsReading::BtDevices(Some(devs)) => {
                use crate::shell::quick_settings::BtDevice;
                st.quick_settings.bt_devices = devs
                    .into_iter()
                    .map(|d| BtDevice {
                        name: d.name,
                        address: d.mac,
                        battery: d.battery,
                        connected: d.connected,
                    })
                    .collect();
            }
            QsReading::BtDevices(None) => {}
            QsReading::Firmware(updates) => {
                st.quick_settings.firmware_updates = updates.map(|v| v.len());
            }
            QsReading::WifiJoin(join) => {
                self.apply_wifi_join_result(st, join);
                return;
            }
        }
        st.bump();
    }

    /// Everything the panel shows, read on the open edge. Each field applies
    /// only if actually read.
    pub(super) fn apply_fast_qs_reading(
        &self,
        st: &mut crate::shell::chrome_state::ChromeState,
        reading: crate::shell::quick_settings::refresh::FastReading,
    ) {
        use crate::shell::quick_settings::refresh::FastReading;
        let FastReading {
            brightness,
            displays,
            volume_available,
            volume,
            bluetooth,
            bluetooth_available,
            power,
            power_profiles,
            wired,
            battery,
            battery_time,
            radios,
            privacy,
            vpns,
        } = reading;
        if let Some(profiles) = power_profiles {
            st.quick_settings.power_profiles = profiles.into_iter().map(ui_power_profile).collect();
        }
        let focused = self.focused_connector_of(st);
        Self::apply_qs_brightness(st, brightness, displays, focused.as_deref());
        if let Some(available) = volume_available {
            st.quick_settings.volume_available = available;
        }
        self.apply_qs_volume(st, volume);
        if let Some(radios) = radios {
            self.apply_qs_radios(st, radios);
        }
        if let Some(vpns) = vpns {
            st.quick_settings.vpns = vpns;
        }
        st.quick_settings.bluetooth_available = bluetooth_available;
        if let Some(on) = bluetooth {
            st.quick_settings.bluetooth = on;
        }
        if let Some(wired) = wired {
            st.quick_settings.wired = Some(wired);
        }
        if let Some(profile) = power {
            st.quick_settings.power_profile = ui_power_profile(profile);
        }
        st.quick_settings.battery = battery;
        st.quick_settings.battery_time = battery_time;
        st.quick_settings.privacy = privacy;
    }

    /// The read-back of the one control the panel just wrote.
    pub(super) fn apply_qs_field_reading(
        &self,
        st: &mut crate::shell::chrome_state::ChromeState,
        reading: crate::shell::quick_settings::refresh::FieldReading,
    ) {
        use crate::shell::quick_settings::refresh::FieldReading;
        match reading {
            FieldReading::Radios(Some(radios)) => self.apply_qs_radios(st, radios),
            FieldReading::Bluetooth(Some(on)) => st.quick_settings.bluetooth = on,
            FieldReading::Wired(Some(wired)) => st.quick_settings.wired = Some(wired),
            FieldReading::Power(Some(profile)) => {
                st.quick_settings.power_profile = ui_power_profile(profile);
            }
            FieldReading::Vpns(Some(vpns)) => st.quick_settings.vpns = vpns,
            FieldReading::Brightness(brightness, displays) => {
                let focused = self.focused_connector_of(st);
                Self::apply_qs_brightness(st, brightness, Some(displays), focused.as_deref());
            }
            FieldReading::Volume(volume) => self.apply_qs_volume(st, volume),
            // Not read: leave the control as it is.
            FieldReading::Radios(None)
            | FieldReading::Bluetooth(None)
            | FieldReading::Wired(None)
            | FieldReading::Power(None)
            | FieldReading::Vpns(None) => {}
        }
    }

    /// Wi-Fi on/off is derived from the radio snapshot rather than read
    /// again; a machine with no adapter leaves the switch alone.
    pub(super) fn apply_qs_radios(
        &self,
        st: &mut crate::shell::chrome_state::ChromeState,
        radios: os_actions::RadioState,
    ) {
        self.qs_action_slot.observe_radio_availability(&radios);
        if let Ok(on) = os_actions::wifi_from_radios(&radios) {
            st.quick_settings.wifi = on;
            if on {
                // Whatever kept it off has let go.
                st.quick_settings.wifi_problem = None;
            }
        }
        st.quick_settings.radios = radios;
    }

    /// [`Self::focused_connector`] for a caller holding the chrome state
    /// mutably, which that method would borrow again.
    pub(super) fn focused_connector_of(
        &self,
        st: &crate::shell::chrome_state::ChromeState,
    ) -> Option<String> {
        let (partitioned, focused) = {
            let tree = st.tree();
            (
                tree.desktop.outputs.displays.len(),
                tree.desktop.outputs.focused_display(),
            )
        };
        displays::focused_connector_in(&self.outputs, partitioned, focused)
    }

    /// `displays: None` means the monitor list was not read this time, so
    /// neither the list nor the slider's availability changes. The display
    /// the person works on (`focused`, its connector) gets the first
    /// slider.
    pub(super) fn apply_qs_brightness(
        st: &mut crate::shell::chrome_state::ChromeState,
        brightness: Option<f32>,
        displays: Option<Vec<crate::shell::chrome_state::DisplayBrightness>>,
        focused: Option<&str>,
    ) {
        let brightness = match displays {
            Some(mut displays) => {
                crate::shell::chrome_state::focused_display_first(&mut displays, focused);
                st.quick_settings.brightness_available =
                    brightness.is_some() || !displays.is_empty();
                let brightness =
                    brightness.or_else(|| displays.first().map(|display| display.level));
                if st.quick_settings.displays != displays {
                    st.quick_settings.displays = displays;
                }
                brightness
            }
            None => brightness,
        };
        if let Some(level) = brightness {
            st.quick_settings.brightness = level;
        }
    }

    /// The tray's [`crate::shell::status::StatusBridge`] is the shell's one
    /// volume: a reading goes there first, and the panel's slider mirrors it
    /// (see [`Self::sync_status_from_bridge`]). Without a bridge — shots,
    /// unit tests — the slider takes the reading directly.
    pub(super) fn apply_qs_volume(
        &self,
        st: &mut crate::shell::chrome_state::ChromeState,
        volume: Option<os_actions::VolumeState>,
    ) {
        let Some(volume) = volume else {
            return;
        };
        if let Some(bridge) = self.status_bridge.as_ref() {
            bridge.set_volume(volume);
            st.status.volume = Some(volume);
        }
        st.quick_settings.volume = volume.level;
    }

    pub(super) fn apply_qs_wifi_networks(
        st: &mut crate::shell::chrome_state::ChromeState,
        nets: Vec<os_actions::WifiNetworkInfo>,
    ) {
        use crate::shell::quick_settings::WifiNetwork;
        st.quick_settings.wifi_networks = nets
            .into_iter()
            .map(|n| WifiNetwork {
                ssid: n.ssid,
                signal: n.signal,
                secure: n.secure,
                security: n.security,
                connected: n.connected,
                known: n.known,
            })
            .collect();
    }

    pub(super) fn apply_wifi_join_result(
        &mut self,
        st: &mut crate::shell::chrome_state::ChromeState,
        join: crate::shell::quick_settings::refresh::WifiJoinResult,
    ) {
        let crate::shell::quick_settings::refresh::WifiJoinResult {
            ssid,
            password,
            shared_credential,
            result,
        } = join;
        st.quick_settings.wifi_joining = None;
        match result {
            Ok(()) => {
                st.quick_settings.wifi_password = None;
                for network in &mut st.quick_settings.wifi_networks {
                    network.connected = network.ssid == ssid;
                }
                st.quick_settings.wifi = true;
                let security = scanned_wifi_security(&st.quick_settings.wifi_networks, &ssid);
                let share_result = if shared_credential {
                    None
                } else {
                    password.as_ref().and_then(|password| {
                        self.wifi_secret_bridge.as_ref().map(|bridge| {
                            let security = security.ok_or_else(|| {
                                "could not verify the connected Wi-Fi security mode".to_string()
                            })?;
                            bridge(WifiSecretAction::Share {
                                ssid: ssid.clone(),
                                security,
                                password: password.clone(),
                            })
                        })
                    })
                };
                let detail = if shared_credential {
                    t("host-wifi-connected-shared-credential")
                } else if share_result.as_ref().is_some_and(Result::is_err) {
                    t("host-wifi-connected-sharing-unavailable")
                } else if share_result.is_some() {
                    t("host-wifi-connected-shared")
                } else if password.is_some() {
                    t("host-wifi-connected-saved")
                } else {
                    t("host-wifi-connected-open")
                };
                st.push_notification(
                    t_args(
                        "host-wifi-connected-title",
                        &[("ssid", ssid.as_str().into())],
                    ),
                    detail,
                );
            }
            Err(error) => {
                tracing::warn!(target: "tilo::os_actions", op = "connect_wifi", %error, "wifi join failed");
                if let Some(prompt) = st
                    .quick_settings
                    .wifi_password
                    .as_mut()
                    .filter(|prompt| prompt.ssid == ssid)
                {
                    prompt.connecting = false;
                    // Selected, so the next keystroke replaces it.
                    if self.wifi_password_prompt == Some(prompt.id) {
                        self.wifi_password_input.select_all();
                    }
                    prompt.error = Some(if shared_credential {
                        t("host-wifi-saved-password-stale")
                    } else {
                        t("host-wifi-join-check-password")
                    });
                } else if st
                    .quick_settings
                    .wifi_networks
                    .iter()
                    .any(|network| network.ssid == ssid && network.secure)
                {
                    // A protected network that turned its saved password
                    // down asks for it, in place.
                    let mut prompt = crate::shell::quick_settings::WifiPasswordPrompt::new(ssid);
                    prompt.error = Some(t("host-wifi-join-check-password"));
                    st.quick_settings.wifi_password = Some(prompt);
                } else {
                    st.push_notification(
                        t_args(
                            "host-wifi-connect-failed-title",
                            &[("ssid", ssid.as_str().into())],
                        ),
                        t("host-wifi-connect-failed-body"),
                    );
                }
            }
        }
        st.bump();
    }

    /// Give `focus` the keyboard in the agent access view, as Tab would.
    pub fn focus_agent_access_for_shots(
        &mut self,
        focus: tilo_desktop::workspace_settings::agent_access::AccessFocus,
    ) {
        self.workspace_settings_overlay.focus_agent_access(focus);
    }

    /// See [`Self::force_palette_settled`].
    /// Show `agent`'s answers in the Agents tab, with `open`'s expanded:
    /// what pressing its Review access and then an app does.
    pub fn open_agent_review_for_shots(
        &mut self,
        agent: &str,
        open: Option<tilo_action_policy::agent_answers::AnswerScope>,
    ) {
        self.workspace_settings_overlay
            .open_agent_review(agent.to_owned());
        if let Some(scope) = open {
            self.workspace_settings_overlay.toggle_agent_app(scope);
        }
    }

    pub fn force_ws_settings_settled(&mut self) {
        if !self.overlay_flags().ws_settings {
            return;
        }
        self.workspace_settings_overlay.force_settled();
    }

    /// Font size the overview sets the current room's name at. It leads the
    /// panel without becoming a display headline that overwhelms the actual
    /// destinations below. A method, not a constant, so it tracks the active
    /// theme's typography token.
    pub fn overview_header_font_size(&self, theme: &Theme) -> f32 {
        Self::overview_header_font_size_for(&theme.tokens)
    }

    pub(super) fn overview_header_font_size_for(tokens: &ui::tokens::Tokens) -> f32 {
        tokens.typography.size(TextRole::Display)
    }

    pub(super) fn overlay_flags(&self) -> crate::shell::chrome_state::OverlayFlags {
        self.chrome
            .as_ref()
            .map(|c| c.borrow().overlays)
            .unwrap_or_default()
    }

    /// Mirror of the geometry in `draw_quick_settings`: the utility Panel
    /// width (HIG 9.6), narrowed to the display it opens on so it never runs
    /// off a narrow one.
    pub(super) fn quick_settings_width(&self) -> f64 {
        let area = self.overlay_area();
        let room = (area.width() - 2.0 * self.qs_panel.margin).max(1.0);
        self.qs_panel.width.min(room)
    }

    /// Under the top bar of the display the person is working on (the
    /// tray's, when the tray opened it: the press there focused that
    /// display): the Panel margin in from the display's trailing edge, the
    /// same margin under the bar, and never past the display's bottom
    /// margin. Pinned to the primary's tray, a volume key or shortcut
    /// pressed while working on the other display opened it out of sight.
    ///
    /// Clipboard history opened to paste (Super+V) is the exception: it
    /// opens where the text goes ([`Self::clipboard_paste_rect`]).
    pub(super) fn quick_settings_rect(&self) -> Rect {
        let area = self.overlay_area();
        let margin = self.qs_panel.margin;
        let popover_w = self.quick_settings_width();
        let x1 = area.x1 - margin;
        let y0 = area.y0 + f64::from(crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX) + margin;
        let (desired_h, paste_origin) = self.chrome.as_ref().map_or((360.0, None), |chrome| {
            let state = chrome.borrow();
            let origin = state
                .quick_settings
                .pasting()
                .then_some(state.quick_settings.paste_origin)
                .flatten();
            (self.quick_settings_preferred_height(&state), origin)
        });
        if let Some(origin) = paste_origin {
            return self.clipboard_paste_rect(origin, (popover_w, desired_h));
        }
        let popover_h = desired_h.min((area.y1 - margin - y0).max(1.0));
        Rect::new(x1 - popover_w, y0, x1, y0 + popover_h)
    }

    /// Where clipboard history opened to paste goes: against the caret of
    /// the field it will paste into, placed as the emoji picker is
    /// ([`tilo_desktop::emoji_picker::card_at_caret`]); without a caret,
    /// centred near the top of the focused window; without either, centred
    /// near the top of the display with focus. Never under the tray, across
    /// the screen from the text.
    pub(super) fn clipboard_paste_rect(
        &self,
        origin: crate::shell::chrome_state::PasteOrigin,
        (width, desired_h): (f64, f64),
    ) -> Rect {
        let margin = self.qs_panel.margin;
        let outputs = self.layer_outputs();
        let display = self
            .layer_geometry(&outputs)
            .display_for(origin.caret.or(origin.window));
        let height = desired_h.min((display.height() - 2.0 * margin).max(1.0));
        if let Some(caret) = origin.caret {
            return tilo_desktop::emoji_picker::card_at_caret(
                caret,
                (width, height),
                display,
                margin,
                margin / 2.0,
            );
        }
        let around = origin
            .window
            .map_or(display, |window| window.intersect(display));
        let x0 = (around.center().x - width / 2.0)
            .min(display.x1 - margin - width)
            .max(display.x0 + margin);
        let y0 = (around.y0 + around.height() * 0.12)
            .min(display.y1 - margin - height)
            .max(display.y0 + margin);
        Rect::from_origin_size((x0, y0), (width, height))
    }

    /// Convert a pointer in painted coordinates back into Quick Settings'
    /// stable layout coordinates. The popover appears from the tray, scaled
    /// about that origin; hit-testing the resting rectangles made controls
    /// visibly move out from under the pointer.
    pub(super) fn untransform_quick_settings_pointer(&self, point: Point, now: Instant) -> Point {
        self.quick_settings_pose(now).to_local(point)
    }

    #[cfg(test)]
    pub(super) fn transform_quick_settings_point_for_test(
        &self,
        point: Point,
        now: Instant,
    ) -> Point {
        self.quick_settings_pose(now).to_drawn(point)
    }

    /// Quick Settings' pose at `now`, appearing from the tray. Painting and
    /// hit testing both go through it, so a control is hit where it is
    /// drawn. A page drilling in is hit at its resting place: its shift is
    /// small and short, and input acts on the new page at once.
    pub(super) fn quick_settings_pose(&self, now: Instant) -> ui::motion::Pose {
        let rect = self.quick_settings_rect();
        self.qs_motion.pose(
            now,
            qs_motion::QsMotion::origin(rect, self.quick_settings_at_caret()),
            self.motion.get(),
        )
    }

    /// Whether Quick Settings stands at a caret (clipboard history opened to
    /// paste) rather than under the tray.
    pub(super) fn quick_settings_at_caret(&self) -> bool {
        self.chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow().quick_settings.pasting())
    }

    /// A drill-in is a compact continuation of Quick Settings, not a blank
    /// fixed-height sheet. Its height follows the actual rows; the root
    /// page's follows its open section. The one cap on
    /// every page is the popover's available height in
    /// [`Self::quick_settings_rect`] — the bar above and the desktop inset
    /// below — the same budget the root page is fitted to; rows past it are
    /// clipped by the panel, never painted over the desktop.
    pub(super) fn quick_settings_preferred_height(
        &self,
        state: &crate::shell::chrome_state::ChromeState,
    ) -> f64 {
        use crate::shell::quick_settings::QsLevel;
        match state.quick_settings.current_level() {
            QsLevel::Root => self.qs_root.preferred_height(&state.quick_settings),
            QsLevel::BluetoothDetails(_) => self
                .device_fabric_call(DeviceFabricUiAction::Snapshot)
                .map_or(216.0, |view| {
                    let rows = view.devices.len().saturating_add(1);
                    (78.0 + 58.0 * f64::from(u32::try_from(rows).unwrap_or(u32::MAX)))
                        .clamp(216.0, 552.0)
                }),
            // `state` is the caller's shared borrow of this same chrome; the
            // host reads it again for the rows' own actions.
            QsLevel::Notifications => self.chrome.as_ref().map_or(0.0, |chrome| {
                self.notification_center.preferred_height_for_width(
                    &ChromeNotifications::new(chrome),
                    &self.qs_type_lines,
                    self.quick_settings_width(),
                )
            }),
        }
    }

    /// Number of complete notification rows the current compact pane shows.
    /// When the history overflows, the pager band's room is reserved so its
    /// position and keyboard hint never overlap the final row.
    #[cfg(test)]
    pub(super) fn qs_notification_visible_rows(&self) -> usize {
        self.chrome.as_ref().map_or(1, |chrome| {
            self.notification_center.visible_rows(
                &ChromeNotifications::new(chrome),
                &self.qs_notification_layout(),
            )
        })
    }

    /// The notification centre's geometry for the current panel and text
    /// scale — the one oracle paint, hit-testing and sizing read.
    pub(super) fn qs_notification_layout(&self) -> CenterLayout {
        let quiet = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| ChromeNotifications::new(chrome).quiet_status().is_some());
        CenterLayout::new(self.quick_settings_rect(), &self.qs_type_lines)
            .drill_in()
            .quiet(quiet)
    }

    /// Scroll the notification centre from a pointer wheel over the open
    /// panel.
    pub(super) fn handle_qs_notification_wheel(
        &mut self,
        delta: MouseScrollDelta,
        scale_factor: f64,
    ) -> bool {
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        let notifications_open = {
            let state = chrome.borrow();
            state.overlays.quick_settings
                && matches!(
                    state.quick_settings.current_level(),
                    crate::shell::quick_settings::QsLevel::Notifications
                )
        };
        if !notifications_open
            || !self
                .last_cursor
                .is_some_and(|point| self.quick_settings_rect().contains(point))
        {
            return false;
        }
        let layout = self.qs_notification_layout();
        self.notification_center.wheel(
            &ChromeNotifications::new(chrome).with_agents(&self.room_activity),
            &layout,
            delta,
            scale_factor,
        );
        true
    }

    /// Scrolling for a Quick Settings root page taller than its popover:
    /// wheel, touchpad, a finger's drag (which the compositor emulates as a
    /// wheel) and the page's scrollbar, through the toolkit's
    /// [`ui::widget::ScrollRegion`]. Anything the region does not take
    /// continues to the ordinary click and hover routing.
    pub(super) fn route_qs_root_scroll(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let on_root = {
            let state = chrome.borrow();
            state.overlays.quick_settings
                && matches!(
                    state.quick_settings.current_level(),
                    crate::shell::quick_settings::QsLevel::Root
                )
        };
        if !on_root && !self.qs_root.scroll_dragging() {
            return false;
        }
        let mut taken = false;
        let changed = self.qs_root.handle_scroll_event(
            event,
            scale_factor,
            modifiers.state(),
            &mut taken,
            &self.overview_layout_theme,
        );
        if changed {
            chrome.borrow_mut().bump();
        }
        if taken {
            *handled = true;
        }
        taken
    }

    /// The root page's scrollbar is fading on an open popover.
    pub(super) fn qs_root_scroll_fading(&self) -> bool {
        self.overlay_flags().quick_settings
            && self.qs_root.scroll_needs_frame(&self.overview_layout_theme)
    }

    /// Keyboard navigation for the notification centre, available without
    /// pointer hover. Returns whether the centre consumed the key.
    #[cfg(test)]
    pub(super) fn handle_qs_notification_keyboard(&mut self, logical_key: &Key) -> bool {
        self.handle_qs_notification_keyboard_with_shift(logical_key, false)
    }

    pub(super) fn handle_qs_notification_keyboard_with_shift(
        &mut self,
        logical_key: &Key,
        backwards: bool,
    ) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let layout = self.qs_notification_layout();
        let outcome = self.notification_center.key_with_shift(
            &ChromeNotifications::new(&chrome).with_agents(&self.room_activity),
            &layout,
            logical_key,
            backwards,
        );
        self.apply_notification_center_outcome(outcome)
    }

    /// Carry out what the notification centre asked of the shell. Returns
    /// whether the input was the centre's.
    pub(super) fn apply_notification_center_outcome(&self, outcome: CenterOutcome) -> bool {
        match outcome {
            CenterOutcome::Ignored => return false,
            CenterOutcome::Handled => {}
            CenterOutcome::Back => self.pop_quick_settings_level(),
            CenterOutcome::OpenApp { app_id, label } => self.launch_app(&app_id, &label),
        }
        true
    }

    /// How many copies the Clipboard section shows at once; typing
    /// searches the rest, and the arrows page through them.
    pub(super) fn qs_clipboard_visible_rows(&self) -> usize {
        QS_CLIPBOARD_ROWS
    }

    pub(super) fn clamp_qs_clipboard_history_start(&mut self, history_len: usize) -> bool {
        let max_start = history_len.saturating_sub(self.qs_clipboard_visible_rows());
        let next = self.qs_clipboard_history_start.min(max_start);
        let changed = next != self.qs_clipboard_history_start;
        self.qs_clipboard_history_start = next;
        changed
    }

    pub(super) fn select_qs_clipboard_item(
        &mut self,
        history: &[crate::compositor::selection::ClipboardHistorySnapshot],
        index: usize,
    ) {
        let Some(item) = history.get(index) else {
            self.qs_selected_clipboard = None;
            return;
        };
        self.qs_selected_clipboard = Some(item.id);
        let visible = self.qs_clipboard_visible_rows();
        if index < self.qs_clipboard_history_start {
            self.qs_clipboard_history_start = index;
        } else if index >= self.qs_clipboard_history_start.saturating_add(visible) {
            self.qs_clipboard_history_start = index.saturating_add(1).saturating_sub(visible);
        }
        self.bump_chrome();
    }

    pub(super) fn scroll_qs_clipboard_history(&mut self, movement: isize) -> bool {
        let history_len = self.filtered_clipboard_history().len();
        let max_start = history_len.saturating_sub(self.qs_clipboard_visible_rows());
        let current = self.qs_clipboard_history_start.min(max_start);
        let next = if movement.is_negative() {
            current.saturating_sub(movement.unsigned_abs())
        } else {
            current.saturating_add(usize::try_from(movement).unwrap_or(usize::MAX))
        }
        .min(max_start);
        if next == self.qs_clipboard_history_start {
            return false;
        }
        self.qs_clipboard_history_start = next;
        self.bump_chrome();
        true
    }

    pub(super) fn handle_qs_clipboard_wheel(
        &mut self,
        delta: MouseScrollDelta,
        scale_factor: f64,
    ) -> bool {
        // Only the picker opened to paste pages its copies under the wheel;
        // in Quick Settings the wheel scrolls the page.
        let clipboard_open = self.chrome.as_ref().is_some_and(|chrome| {
            let state = chrome.borrow();
            state.overlays.quick_settings && state.quick_settings.pasting()
        });
        if !clipboard_open
            || !self
                .last_cursor
                .is_some_and(|point| self.quick_settings_rect().contains(point))
        {
            return false;
        }
        let rows = match delta {
            MouseScrollDelta::LineDelta(_, y) => -f64::from(y),
            MouseScrollDelta::PixelDelta(position) => -position.y / (48.0 * scale_factor.max(0.1)),
        };
        if rows.abs() <= f64::EPSILON {
            return true;
        }
        self.qs_clipboard_wheel_remainder += rows;
        let whole = self.qs_clipboard_wheel_remainder.trunc();
        if whole.abs() >= 1.0 {
            self.qs_clipboard_wheel_remainder -= whole;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a wheel delta is bounded by input hardware; this only converts complete row movement"
            )]
            let movement = whole as isize;
            let _ = self.scroll_qs_clipboard_history(movement);
        }
        true
    }

    /// Whether the open Clipboard section takes `logical_key`: typing
    /// searches its copies; the arrows and Enter choose among them while no
    /// control holds keyboard focus (as when Super+V opened it to paste).
    pub(super) fn qs_clipboard_takes_key(
        &self,
        chrome: &Chrome,
        logical_key: &Key,
        control_down: bool,
    ) -> bool {
        let state = chrome.borrow();
        let qs = &state.quick_settings;
        if !state.overlays.quick_settings
            || qs.current_level() != &crate::shell::quick_settings::QsLevel::Root
            || qs.section != Some(crate::shell::quick_settings::QsSection::Clipboard)
        {
            return false;
        }
        match logical_key {
            Key::Character(_) => !control_down,
            Key::Named(NamedKey::Backspace) => true,
            Key::Named(NamedKey::Escape) => !self.qs_clipboard_query.is_empty(),
            Key::Named(
                NamedKey::ArrowDown
                | NamedKey::ArrowUp
                | NamedKey::PageDown
                | NamedKey::PageUp
                | NamedKey::Home
                | NamedKey::End
                | NamedKey::Enter
                | NamedKey::Space,
            ) => !self.qs_root.has_keyboard_focus(),
            _ => false,
        }
    }

    pub(super) fn handle_qs_clipboard_keyboard(
        &mut self,
        logical_key: &Key,
        control_down: bool,
    ) -> bool {
        let query_input = match logical_key {
            Key::Character(value) if !control_down => {
                let before = self.qs_clipboard_query.len();
                let remaining = 80usize.saturating_sub(self.qs_clipboard_query.chars().count());
                self.qs_clipboard_query.extend(
                    value
                        .chars()
                        .filter(|character| !character.is_control())
                        .take(remaining),
                );
                Some(self.qs_clipboard_query.len() != before)
            }
            Key::Named(NamedKey::Backspace) => {
                Some(ui::delete_last_grapheme(&mut self.qs_clipboard_query))
            }
            Key::Named(NamedKey::Escape) if !self.qs_clipboard_query.is_empty() => {
                self.qs_clipboard_query.clear();
                Some(true)
            }
            _ => None,
        };
        if let Some(changed) = query_input {
            if changed {
                self.qs_clipboard_history_start = 0;
                let history = self.filtered_clipboard_history();
                self.qs_selected_clipboard = history.first().map(|item| item.id);
                self.bump_chrome();
            }
            return true;
        }
        let history = self.filtered_clipboard_history();
        if history.is_empty() {
            self.qs_selected_clipboard = None;
            return matches!(
                logical_key,
                Key::Named(
                    NamedKey::ArrowDown
                        | NamedKey::ArrowUp
                        | NamedKey::PageDown
                        | NamedKey::PageUp
                        | NamedKey::Home
                        | NamedKey::End
                )
            );
        }
        let current = self
            .qs_selected_clipboard
            .and_then(|id| history.iter().position(|item| item.id == id))
            .unwrap_or(0);
        let page = self.qs_clipboard_visible_rows().max(1);
        let target = match logical_key {
            Key::Named(NamedKey::ArrowDown) => current.saturating_add(1),
            Key::Named(NamedKey::ArrowUp) => current.saturating_sub(1),
            Key::Named(NamedKey::PageDown) => current.saturating_add(page),
            Key::Named(NamedKey::PageUp) => current.saturating_sub(page),
            Key::Named(NamedKey::Home) => 0,
            Key::Named(NamedKey::End) => history.len().saturating_sub(1),
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let Some(id) = history.get(current).map(|item| item.id) else {
                    return true;
                };
                self.choose_clipboard_item(id);
                return true;
            }
            _ => return false,
        }
        .min(history.len().saturating_sub(1));
        self.select_qs_clipboard_item(&history, target);
        true
    }

    pub(super) fn qs_device_fabric_visible_rows(&self) -> usize {
        usize_of_f64((self.quick_settings_rect().height() - 78.0) / 58.0).clamp(1, 7)
    }

    pub(super) fn clamp_qs_device_fabric_start(&mut self, row_count: usize) -> bool {
        let max_start = row_count.saturating_sub(self.qs_device_fabric_visible_rows());
        let next = self.qs_device_fabric_start.min(max_start);
        let changed = next != self.qs_device_fabric_start;
        self.qs_device_fabric_start = next;
        changed
    }

    pub(super) fn scroll_qs_device_fabric(&mut self, movement: isize) -> bool {
        let row_count = self
            .device_fabric_call(DeviceFabricUiAction::Snapshot)
            .map_or(0, |view| {
                let bluetooth_details = self.chrome.as_ref().is_some_and(|chrome| {
                    matches!(
                        chrome.borrow().quick_settings.current_level(),
                        crate::shell::quick_settings::QsLevel::BluetoothDetails(_)
                    )
                });
                if bluetooth_details {
                    view.devices.len().saturating_add(1)
                } else {
                    let realtime = self
                        .realtime_continuity_call(RealtimeContinuityUiAction::Snapshot)
                        .unwrap_or_default();
                    device_fabric_pane_rows(&view, &realtime).len()
                }
            });
        let max_start = row_count.saturating_sub(self.qs_device_fabric_visible_rows());
        let current = self.qs_device_fabric_start.min(max_start);
        let next = if movement.is_negative() {
            current.saturating_sub(movement.unsigned_abs())
        } else {
            current.saturating_add(usize::try_from(movement).unwrap_or(usize::MAX))
        }
        .min(max_start);
        if next == self.qs_device_fabric_start {
            return false;
        }
        self.qs_device_fabric_start = next;
        self.bump_chrome();
        true
    }

    pub(super) fn handle_qs_device_fabric_wheel(
        &mut self,
        delta: MouseScrollDelta,
        scale_factor: f64,
    ) -> bool {
        let devices_open = self.chrome.as_ref().is_some_and(|chrome| {
            let state = chrome.borrow();
            state.overlays.quick_settings
                && matches!(
                    state.quick_settings.current_level(),
                    crate::shell::quick_settings::QsLevel::BluetoothDetails(_)
                )
        });
        if !devices_open
            || !self
                .last_cursor
                .is_some_and(|point| self.quick_settings_rect().contains(point))
        {
            return false;
        }
        let rows = match delta {
            MouseScrollDelta::LineDelta(_, y) => -f64::from(y),
            MouseScrollDelta::PixelDelta(position) => -position.y / (48.0 * scale_factor.max(0.1)),
        };
        if rows.abs() <= f64::EPSILON {
            return true;
        }
        self.qs_device_fabric_wheel_remainder += rows;
        let whole = self.qs_device_fabric_wheel_remainder.trunc();
        if whole.abs() >= 1.0 {
            self.qs_device_fabric_wheel_remainder -= whole;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a wheel delta is bounded by input hardware; this only converts complete row movement"
            )]
            let movement = whole as isize;
            let _ = self.scroll_qs_device_fabric(movement);
        }
        true
    }

    pub(super) fn handle_qs_device_fabric_keyboard(&mut self, logical_key: &Key) -> bool {
        let page = isize::try_from(self.qs_device_fabric_visible_rows()).unwrap_or(isize::MAX);
        match logical_key {
            Key::Named(NamedKey::Enter | NamedKey::Space | NamedKey::Delete) => {
                let Ok(view) = self.device_fabric_call(DeviceFabricUiAction::Snapshot) else {
                    return true;
                };
                let realtime = self
                    .realtime_continuity_call(RealtimeContinuityUiAction::Snapshot)
                    .unwrap_or_default();
                let rows = device_fabric_pane_rows(&view, &realtime);
                let consent = matches!(
                    rows.get(self.qs_device_fabric_start),
                    Some(
                        DeviceFabricPaneRow::RemoteAudioConsent(_)
                            | DeviceFabricPaneRow::SpeakerConsent(_)
                            | DeviceFabricPaneRow::RemoteCaptureConsent(_)
                            | DeviceFabricPaneRow::RealtimeConsent(_)
                            | DeviceFabricPaneRow::GamepadAuthorization(_)
                    )
                ) || matches!(
                    rows.get(self.qs_device_fabric_start),
                    Some(DeviceFabricPaneRow::Tethering(status))
                        if status.phase == crate::shell::personal_tethering_runtime::TetheringPhase::ConsentRequired
                );
                if consent {
                    let (allow, decline) =
                        Self::qs_realtime_consent_actions(self.qs_device_row_rect(0));
                    self.handle_qs_device_fabric_click(
                        if matches!(logical_key, Key::Named(NamedKey::Delete)) {
                            decline.center()
                        } else {
                            allow.center()
                        },
                    );
                } else if !matches!(logical_key, Key::Named(NamedKey::Delete)) {
                    self.handle_qs_device_fabric_click(self.qs_device_row_rect(0).center());
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                let _ = self.scroll_qs_device_fabric(1);
            }
            Key::Named(NamedKey::ArrowUp) => {
                let _ = self.scroll_qs_device_fabric(-1);
            }
            Key::Named(NamedKey::PageDown) => {
                let _ = self.scroll_qs_device_fabric(page);
            }
            Key::Named(NamedKey::PageUp) => {
                let _ = self.scroll_qs_device_fabric(page.saturating_neg());
            }
            Key::Named(NamedKey::Home) => {
                let _ = self.scroll_qs_device_fabric(-isize::MAX);
            }
            Key::Named(NamedKey::End) => {
                let _ = self.scroll_qs_device_fabric(isize::MAX);
            }
            _ => return false,
        }
        true
    }

    pub fn quick_settings_semantic_rects(&self) -> Vec<(String, Rect)> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let state = chrome.borrow();
        if !state.overlays.quick_settings {
            return Vec::new();
        }
        match state.quick_settings.current_level() {
            crate::shell::quick_settings::QsLevel::Root => {
                let mut rects = self.qs_root.semantic_rects(&state.quick_settings);
                drop(state);
                rects.extend(self.now_playing_semantic_rects());
                rects
            }
            crate::shell::quick_settings::QsLevel::Notifications => {
                drop(state);
                self.chrome.as_ref().map_or_else(Vec::new, |chrome| {
                    self.notification_center.semantic_rects(
                        &ChromeNotifications::new(chrome).with_agents(&self.room_activity),
                        &self.qs_notification_layout(),
                        "quick_settings_",
                    )
                })
            }
            _ => Vec::new(),
        }
    }

    pub fn power_menu_semantic_rects(&self) -> Vec<(&'static str, Rect)> {
        self.layers.power_menu.action_rects(self.overlay_area())
    }

    /// May the local user invite into the room they're looking at? Asked
    /// through the one invitation policy the issuer and the palette's
    /// `peer.invite` rows answer to, not re-decided here — a second opinion
    /// is a second security boundary. Offered only when submitting it can
    /// produce an invitation: a member who may invite into a room another
    /// person governs is not shown a composer the issuer would refuse.
    /// Fail-closed at every missing piece (no chrome, no workspace,
    /// peer-sync off).
    pub(super) fn may_invite_here(&self) -> bool {
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        let st = chrome.borrow();
        st.peer_sync_enabled
            && crate::shell::invite_policy::active_invite_route(&st)
                .is_some_and(crate::shell::invite_policy::InviteRoute::issues_here)
    }
}

/// The authentication mode the last Wi-Fi scan listed for `ssid`.
pub(super) fn scanned_wifi_security(
    networks: &[crate::shell::quick_settings::WifiNetwork],
    ssid: &str,
) -> Option<os_actions::WifiSecurity> {
    networks
        .iter()
        .find(|network| network.ssid == ssid)
        .map(|network| network.security)
}

/// How often the open Wi-Fi section scans for networks again: often enough
/// that walking into range shows up while the person looks, rarely enough
/// that the radio is not kept busy.
pub(super) const QS_WIFI_RESCAN: Duration = Duration::from_secs(10);

/// Copies the Clipboard section shows at once.
pub(super) const QS_CLIPBOARD_ROWS: usize = 6;

/// The one local source a Quick Settings device click routes to another device.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum QsDeviceSource {
    Media(String),
    Realtime(String),
    Gamepad(u64),
    RemoteAudio(u64),
    RemoteCapture(crate::shell::remote_capture::device::RemoteCaptureKind),
}

/// This machine's sound outputs as Quick Settings lists them.
pub(super) fn sound_outputs_of(
    view: &DeviceFabricView,
) -> Vec<crate::shell::quick_settings::SoundOutput> {
    view.local_outputs
        .iter()
        .map(|output| crate::shell::quick_settings::SoundOutput {
            node: output.node_id,
            name: output.label.clone(),
            current: output.is_default,
        })
        .collect()
}

/// The control a radio change writes.
pub(super) const fn radio_field(change: os_actions::RadioChange) -> QsField {
    match change {
        os_actions::RadioChange::Wifi(_) => QsField::Wifi,
        os_actions::RadioChange::Airplane(_) => QsField::Airplane,
    }
}

/// The panel's three-way profile, from the daemon's.
pub(super) const fn ui_power_profile(
    profile: crate::shell::quick_settings::actions::PowerProfile,
) -> crate::shell::quick_settings::PowerProfile {
    use crate::shell::quick_settings::PowerProfile as UiPP;
    use crate::shell::quick_settings::actions::PowerProfile as ActPP;
    match profile {
        ActPP::Saver => UiPP::Saver,
        ActPP::Balanced => UiPP::Balanced,
        ActPP::Performance => UiPP::Performance,
    }
}

pub(super) fn focus_quick_settings_accessibility(
    host: &mut ShellOverlayHost,
    child_path: &[usize],
) -> bool {
    let Some(chrome) = host.chrome.clone() else {
        return false;
    };
    let notifications_open = matches!(
        chrome.borrow().quick_settings.current_level(),
        crate::shell::quick_settings::QsLevel::Notifications
    );
    if notifications_open
        && let [0, dialog_path @ ..] = child_path
        && host.notification_center.focus_accessibility_path(
            &ChromeNotifications::new(&chrome).with_agents(&host.room_activity),
            &host.qs_notification_layout(),
            dialog_path,
        )
    {
        return true;
    }
    host.qs_root
        .focus_accessibility_path(child_path, &chrome.borrow().quick_settings)
}

/// Exact conservative extent of Quick Settings after its animation layer has settled. This must
/// track both shadows that `draw_quick_settings` emits, rather than a theme-independent margin.
/// Damage bounds of the settled quick-settings card: the card plus the
/// full reach of its `e4` shadow (3σ of the blur, the downward offset, and
/// the one-pixel card stroke centred on its edge). The shadow is the
/// token's own — the same `ui::surface::shadow` call paints it — so this
/// is derived from the token rather than restating a sprite's spread.
pub(super) fn quick_settings_settled_bounds(rect: Rect, theme: &Theme, transform: Affine) -> Rect {
    let panel_reach = ui::surface::std_dev_for(theme.tokens.elevation.floating.blur) * 3.0
        + f64::from(theme.tokens.elevation.floating.offset_y)
        + 0.5;
    transform.transform_rect_bbox(rect.inflate(panel_reach, panel_reach))
}

/// The quick settings panel's theme-resolved size: the list Panel width
/// grown with the text, and the floating Panel margin it keeps inside its
/// display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct QsPanelMetrics {
    pub(super) width: f64,
    pub(super) margin: f64,
}

impl QsPanelMetrics {
    pub(super) fn for_theme(theme: &Theme) -> Self {
        Self {
            // A list Panel's width: two tiles side by side keep their
            // names whole.
            width: f64::from(theme.tokens.metrics.panel_width_list * theme.text_growth()),
            margin: ui::widget::floating_surface::viewport_margin(theme),
        }
    }
}

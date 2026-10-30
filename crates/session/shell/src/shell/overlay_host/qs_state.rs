//! Quick settings readings and the popover's geometry.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Folds every reading the quick-settings workers delivered into
    /// chrome state. Never blocks — the reads happen on a worker, since
    /// a shell-out on the paint path froze the desktop. Each field
    /// applies only if actually read, so a missing daemon leaves the
    /// previous value rather than blanking it.
    /// Bind the owning native session's existing event-loop wake.
    pub fn set_quick_settings_wake(&mut self, wake: crate::session_runtime::Wake) {
        self.qs_action_slot.set_radio_wake(Arc::clone(&wake));
        self.quick_settings.set_wake(Arc::clone(&wake));
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
        &self,
        st: &mut crate::shell::chrome_state::ChromeState,
        reading: crate::shell::quick_settings_refresh::QsReading,
    ) {
        use crate::shell::quick_settings_refresh::QsReading;
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
                use tilo_quick_settings::BtDevice;
                st.quick_settings.bt_devices = devs
                    .into_iter()
                    .map(|d| BtDevice {
                        name: d.name,
                        address: d.address,
                        battery: d.battery.unwrap_or(0),
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
        reading: crate::shell::quick_settings_refresh::FastReading,
    ) {
        use crate::shell::quick_settings_refresh::FastReading;
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
        // The bar's battery is the panel's: a reading taken on open goes to
        // the one status both draw.
        if let Some(bridge) = self.status_bridge.as_ref() {
            bridge.set_battery(battery, battery_time);
        }
        st.quick_settings.status.battery = Some(battery);
        st.quick_settings.status.battery_time = battery_time;
        st.quick_settings.privacy = privacy;
    }

    /// The read-back of the one control the panel just wrote.
    pub(super) fn apply_qs_field_reading(
        &self,
        st: &mut crate::shell::chrome_state::ChromeState,
        reading: crate::shell::quick_settings_refresh::FieldReading,
    ) {
        use crate::shell::quick_settings_refresh::FieldReading;
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
        displays: Option<Vec<tilo_quick_settings::DisplayBrightness>>,
        focused: Option<&str>,
    ) {
        let brightness = match displays {
            Some(mut displays) => {
                tilo_quick_settings::focused_display_first(&mut displays, focused);
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

    /// The [`crate::shell::status::StatusBridge`] is the shell's one
    /// volume: a reading goes there and to the status the bar and the panel
    /// draw (see [`Self::sync_status_from_bridge`]). Without a bridge — shots,
    /// unit tests — only the latter.
    pub(super) fn apply_qs_volume(
        &self,
        st: &mut crate::shell::chrome_state::ChromeState,
        volume: Option<media::audio::default_node::VolumeState>,
    ) {
        let Some(volume) = volume else {
            return;
        };
        if let Some(bridge) = self.status_bridge.as_ref() {
            bridge.set_volume(volume);
        }
        st.quick_settings.status.volume = Some(volume);
    }

    pub(super) fn apply_qs_wifi_networks(
        st: &mut crate::shell::chrome_state::ChromeState,
        nets: Vec<os_actions::WifiNetworkInfo>,
    ) {
        use tilo_quick_settings::WifiNetwork;
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
        &self,
        st: &mut crate::shell::chrome_state::ChromeState,
        join: crate::shell::quick_settings_refresh::WifiJoinResult,
    ) {
        let crate::shell::quick_settings_refresh::WifiJoinResult {
            ssid,
            password,
            shared_credential,
            result,
        } = join;
        st.quick_settings.wifi_joining = None;
        match result {
            Ok(()) => {
                st.quick_settings.wifi_password = None;
                st.quick_settings.wifi_hidden = None;
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
                    let error = if shared_credential {
                        t("host-wifi-saved-password-stale")
                    } else {
                        t("host-wifi-join-check-password")
                    };
                    // Said in the prompt, under its field.
                    prompt.error = Some(error);
                } else if let Some(prompt) = st
                    .quick_settings
                    .wifi_hidden
                    .as_mut()
                    .filter(|prompt| prompt.ssid == ssid)
                {
                    prompt.connecting = false;
                    // Said in the form, under its fields.
                    prompt.error = Some(t("host-wifi-join-check-password"));
                } else if st
                    .quick_settings
                    .wifi_networks
                    .iter()
                    .any(|network| network.ssid == ssid && network.secure)
                {
                    // A protected network that turned its saved password
                    // down asks for it, in place.
                    let mut prompt = tilo_quick_settings::WifiPasswordPrompt::new(ssid);
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
        origin: tilo_quick_settings::PasteOrigin,
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

    /// The popover is as tall as the panel's content lays out; the one
    /// cap is the room under the bar ([`Self::quick_settings_rect`]), past
    /// which the panel scrolls.
    pub(super) fn quick_settings_preferred_height(
        &self,
        _state: &crate::shell::chrome_state::ChromeState,
    ) -> f64 {
        self.quick_settings_content_height()
    }

    /// The panel's named targets where they are on screen now (`qs-tile-wifi`,
    /// `qs-row-battery`, …), and the now-playing card's, while it is open.
    pub fn quick_settings_semantic_rects(&self) -> Vec<(String, Rect)> {
        if !self.overlay_flags().quick_settings {
            return Vec::new();
        }
        let mut rects: Vec<(String, Rect)> = self
            .quick_settings
            .app()
            .map(ui::widget::Widget::test_rects)
            .unwrap_or_default()
            .into_iter()
            .filter(|(name, _)| name.starts_with("qs-"))
            .collect();
        rects.extend(self.now_playing_semantic_rects());
        rects
    }

    /// The Quick Settings target named `name` where it is on screen now.
    pub fn quick_settings_target(&self, name: &str) -> Option<Rect> {
        self.quick_settings_semantic_rects()
            .into_iter()
            .find(|(target, _)| target == name)
            .map(|(_, rect)| rect)
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
        crate::shell::invite_policy::may_invite_here(&chrome.borrow())
    }
}

/// The authentication mode the last Wi-Fi scan listed for `ssid`.
pub(super) fn scanned_wifi_security(
    networks: &[tilo_quick_settings::WifiNetwork],
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

/// The one local source a Quick Settings device click routes to another device.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum QsDeviceSource {
    Media(String),
    Realtime(String),
    Gamepad(u64),
    RemoteAudio(u64),
    RemoteCapture(tilo_device_fabric::remote_capture::RemoteCaptureKind),
}

/// This machine's sound outputs as Quick Settings lists them.
pub(super) fn sound_outputs_of(view: &DeviceFabricView) -> Vec<tilo_quick_settings::SoundOutput> {
    view.local_outputs
        .iter()
        .map(|output| tilo_quick_settings::SoundOutput {
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
    profile: tilo_quick_settings::actions::PowerProfile,
) -> tilo_quick_settings::PowerProfile {
    use tilo_quick_settings::PowerProfile as UiPP;
    use tilo_quick_settings::actions::PowerProfile as ActPP;
    match profile {
        ActPP::Saver => UiPP::Saver,
        ActPP::Balanced => UiPP::Balanced,
        ActPP::Performance => UiPP::Performance,
    }
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

//! What Quick Settings' effects do to the device: radios, levels, VPNs and
//! accessories, each through the OS dispatcher, read back afterwards.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Connect or disconnect an accessory, then read the accessories back.
    pub(super) fn apply_qs_bluetooth_intent(
        &mut self,
        chrome: &Chrome,
        address: &str,
        connect: bool,
    ) {
        let actions = crate::shell::quick_settings_dispatch::ValidatedQsActions::new(
            self.qs_actions.as_ref(),
        );
        let address = address.to_owned();
        let result = if connect {
            actions.connect_bluetooth(&address)
        } else {
            actions.disconnect_bluetooth(&address)
        };
        {
            let mut state = chrome.borrow_mut();
            let name = state
                .quick_settings
                .bt_devices
                .iter()
                .find(|device| device.address == address)
                .map_or_else(|| address.clone(), |device| device.name.clone());
            match result {
                Ok(()) => {
                    if let Some(device) = state
                        .quick_settings
                        .bt_devices
                        .iter_mut()
                        .find(|device| device.address == address)
                    {
                        device.connected = connect;
                    }
                }
                Err(error) => {
                    tracing::warn!(target: "tilo::os_actions", %error, connect, "Bluetooth accessory change failed");
                    state.push_notification(
                        t_args(
                            if connect {
                                "qs-bluetooth-connect-failed"
                            } else {
                                "qs-bluetooth-disconnect-failed"
                            },
                            &[("device", name.into())],
                        ),
                        t_args("qs-action-failed-refreshing", &[("error", error.into())]),
                    );
                }
            }
            state.bump();
        }
        // The accessory list is BlueZ's: read it back either way.
        self.qs_refresh.kick_bt_devices();
    }

    /// Turn a VPN profile on or off, then read the profiles back.
    pub(super) fn apply_qs_vpn_change(&mut self, chrome: &Chrome, name: &str, enabled: bool) {
        #[cfg(target_os = "linux")]
        let device_route = chrome
            .borrow()
            .quick_settings
            .vpns
            .iter()
            .find(|profile| profile.name == name)
            .and_then(|profile| crate::shell::device_vpn::entry_device(&profile.uuid))
            .map(str::to_owned);
        #[cfg(not(target_os = "linux"))]
        let device_route: Option<String> = None;
        let result = match device_route {
            #[cfg(target_os = "linux")]
            Some(device) => crate::shell::device_vpn::choose(&device, enabled),
            _ => crate::shell::quick_settings_dispatch::ValidatedQsActions::new(
                self.qs_actions.as_ref(),
            )
            .set_vpn_connection(name, enabled),
        };
        {
            let mut state = chrome.borrow_mut();
            match result {
                Ok(()) => {
                    if let Some(profile) = state
                        .quick_settings
                        .vpns
                        .iter_mut()
                        .find(|profile| profile.name == name)
                    {
                        profile.active = enabled;
                    }
                }
                Err(error) => {
                    tracing::warn!(target: "tilo::os_actions", %error, "VPN change failed");
                    state.push_notification(
                        t_args("qs-vpn-change-failed", &[("profile", name.into())]),
                        t_args("qs-action-failed-refreshing", &[("error", error.into())]),
                    );
                }
            }
            state.bump();
        }
        self.qs_refresh.kick_field(QsField::Vpn);
    }

    /// The open Wi-Fi section scans for networks when it opens and again
    /// every [`QS_WIFI_RESCAN`] while it stays open with the radio on.
    pub(super) fn rescan_open_wifi_section(&mut self, now: Instant) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let wanted = {
            let state = chrome.borrow();
            state.overlays.quick_settings
                && state.quick_settings.current_level() == &tilo_quick_settings::QsLevel::Root
                && state.quick_settings.section == Some(tilo_quick_settings::QsSection::Wifi)
                && state.quick_settings.wifi
                && !state.quick_settings.radio_change_pending
        };
        if !wanted {
            self.qs_wifi_scanned_at = None;
            return;
        }
        if self
            .qs_wifi_scanned_at
            .is_some_and(|at| now.saturating_duration_since(at) < QS_WIFI_RESCAN)
        {
            return;
        }
        self.qs_wifi_scanned_at = Some(now);
        {
            let mut state = chrome.borrow_mut();
            state.quick_settings.wifi_scanning = true;
            state.quick_settings.wifi_scan_error = None;
            state.bump();
        }
        self.qs_refresh.kick_wifi_rescan();
    }

    /// When the open Wi-Fi section scans next.
    pub(super) fn qs_wifi_rescan_deadline(&self) -> Option<Instant> {
        self.qs_wifi_scanned_at
            .and_then(|at| at.checked_add(QS_WIFI_RESCAN))
    }

    /// The slider shows where it was let go until the device answers.
    fn show_released_level(&self, change: &super::quick_settings::QsLevelChange) {
        use super::quick_settings::QsLevelChange;
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        let mut state = chrome.borrow_mut();
        let qs = &mut state.quick_settings;
        match change {
            QsLevelChange::Brightness(level) => qs.brightness = level_of(*level),
            QsLevelChange::Display(connector, level) => {
                if let Some(display) = qs
                    .displays
                    .iter_mut()
                    .find(|display| display.connector == *connector)
                {
                    display.level = level_of(*level);
                }
            }
            QsLevelChange::Volume(level) => qs.set_volume_level(level_of(*level)),
        }
    }

    /// Commit one level change through the same truthful feedback path as a
    /// hardware key. A positive volume adjustment intentionally unmutes an
    /// already-muted sink; otherwise the slider moves while sound stays off.
    pub(super) fn commit_quick_settings_level(
        &mut self,
        change: super::quick_settings::QsLevelChange,
    ) {
        use super::quick_settings::QsLevelChange;
        let actions = crate::shell::quick_settings_dispatch::ValidatedQsActions::new(
            self.qs_actions.as_ref(),
        );
        self.show_released_level(&change);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a level in 0..=1 as a percentage the OS takes in f32"
        )]
        let percent = |level: f64| (level.clamp(0.0, 1.0) * 100.0) as f32;
        let (field, result) = match change {
            QsLevelChange::Brightness(level) => {
                let value_pct = percent(level);
                (
                    QsField::Brightness,
                    actions
                        .set_brightness(value_pct)
                        .map(|()| crate::shell::osd::Osd::brightness(value_pct / 100.0)),
                )
            }
            QsLevelChange::Display(connector, level) => {
                let value_pct = percent(level);
                // The readout shows on the display that changed as well as
                // on the one the panel is open on, and names the display
                // whenever more than one can be dimmed or it is not the one
                // in use.
                let target = self.chrome.as_ref().and_then(|chrome| {
                    let state = chrome.borrow();
                    let focused = self.focused_connector_of(&state);
                    let displays = &state.quick_settings.displays;
                    displays
                        .iter()
                        .find(|display| display.connector == connector)
                        .map(|display| {
                            let named = crate::shell::media_keys::brightness_names_display(
                                displays.len(),
                                focused.as_deref(),
                                &display.connector,
                            );
                            (
                                display.connector.clone(),
                                named.then(|| display.name.clone()),
                            )
                        })
                });
                let result = target.map_or_else(
                    || Err("that monitor is no longer connected".to_owned()),
                    |(connector, name)| {
                        actions
                            .set_display_brightness(&connector, value_pct)
                            .map(|()| (connector, name))
                    },
                );
                (
                    QsField::Brightness,
                    result.map(|(connector, name)| {
                        crate::shell::osd::Osd::display_brightness(
                            value_pct / 100.0,
                            name.as_deref(),
                        )
                        .on_displays(crate::shell::osd::OsdDisplays::of([connector.as_str()]))
                    }),
                )
            }
            QsLevelChange::Volume(level) => {
                let value_pct = percent(level);
                let bridge = self.status_bridge.as_ref();
                let previous = bridge.and_then(|bridge| bridge.snapshot().volume);
                (
                    QsField::Volume,
                    crate::shell::media_keys::commit_volume(
                        &crate::shell::media_keys::DispatcherAudio(&actions),
                        bridge,
                        previous,
                        value_pct / 100.0,
                    )
                    .map(|(_, osd)| osd),
                )
            }
        };
        match result {
            Ok(osd) => {
                if let Some(chrome) = self.chrome.as_ref() {
                    chrome.borrow_mut().show_osd(osd);
                }
            }
            Err(error) => {
                let volume = field == QsField::Volume;
                let label = if volume { "volume" } else { "brightness" };
                tracing::warn!(target: "tilo::os_actions", op = label, %error, "quick setting failed");
                if let Some(chrome) = self.chrome.as_ref() {
                    let title = if volume {
                        t("host-qs-change-failed-volume")
                    } else {
                        t("host-qs-change-failed-brightness")
                    };
                    chrome
                        .borrow_mut()
                        .push_notification(title, t("host-qs-change-failed-value-restored"));
                }
            }
        }
        // Reconcile the optimistic slider with the device's own level; a
        // read sampled before this commit must not snap the slider back.
        // Worker completion wakes the idle compositor.
        self.qs_refresh.kick_field(field);
    }

    /// The volume icon: mute or unmute the sink the panel shows, at its
    /// level. The bar and the panel read the same status, so both answer at
    /// once.
    pub(super) fn set_quick_settings_muted(&mut self, chrome: &Chrome, muted: bool) {
        let Some(previous) = chrome.borrow().quick_settings.status.volume else {
            return;
        };
        let actions = crate::shell::quick_settings_dispatch::ValidatedQsActions::new(
            self.qs_actions.as_ref(),
        );
        let result = crate::shell::media_keys::commit_mute(
            &crate::shell::media_keys::DispatcherAudio(&actions),
            self.status_bridge.as_ref(),
            previous,
            muted,
        );
        {
            let mut st = chrome.borrow_mut();
            match result {
                Ok((committed, osd)) => {
                    st.quick_settings.status.volume = Some(committed);
                    st.show_osd(osd);
                }
                Err(error) => {
                    tracing::warn!(target: "tilo::os_actions", op = "mute", %error, "quick setting failed");
                    st.push_notification(
                        t("host-qs-change-failed-volume"),
                        t("host-qs-change-failed-value-restored"),
                    );
                }
            }
            st.bump();
        }
        self.qs_refresh.kick_field(QsField::Volume);
    }

    /// Close quick settings and open Settings, at `link` when it names a
    /// page or a setting (`settings://speakers`). A link goes through the
    /// same door as a palette hit on that page, so placement, duplicate
    /// focusing and an already-open Settings window behave the same; a bare
    /// open uses the app launcher, which reports a missing package. Either
    /// way this is the device's settings, never the workspace's.
    pub(super) fn open_settings(&self, chrome: &Chrome, link: Option<&str>) {
        {
            let mut state = chrome.borrow_mut();
            state.overlays.quick_settings = false;
            state.bump();
        }
        if let Some(link) = link {
            match crate::shell::uri_open::open_in_workspace(link, chrome) {
                Ok(_) => return,
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        link,
                        "could not open the Settings link; opening Settings"
                    );
                }
            }
        }
        (self.app_launcher)(chrome, settings_sections::APP_ID, "Settings");
    }

    /// Persist the rotation lock; the compositor's accelerometer worker
    /// reads it from the same settings document.
    pub(super) fn save_rotation_lock(locked: bool) {
        let origin = settings_store::ChangeOrigin::local("qs.rotation-lock");
        if let Err(e) = settings_store::hardware::ROTATION_LOCK.set_by(locked, &origin) {
            tracing::warn!(error = %e, "rotation lock preference save failed");
        }
    }

    pub(super) fn dispatch_quick_settings_radio(&mut self, change: os_actions::RadioChange) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        match self.qs_action_slot.start_radio(change, &chrome) {
            Ok(Some(_)) => {}
            Ok(None) => self.reconcile_quick_settings_radio(change),
            Err(error) => {
                self.finish_quick_settings_action(
                    radio_field(change),
                    match change {
                        os_actions::RadioChange::Wifi(_) => "set_wifi",
                        os_actions::RadioChange::Airplane(_) => "set_airplane_mode",
                    },
                    t(match change {
                        os_actions::RadioChange::Wifi(_) => "host-qs-change-failed-wifi",
                        os_actions::RadioChange::Airplane(_) => "host-qs-change-failed-airplane",
                    }),
                    Err(error),
                );
            }
        }
    }

    pub(super) fn reconcile_quick_settings_radio(&mut self, change: os_actions::RadioChange) {
        self.qs_refresh.kick_field(radio_field(change));
        if change == os_actions::RadioChange::Wifi(true) && self.qs_refresh.os_reads_enabled() {
            if let Some(chrome) = &self.chrome {
                let mut state = chrome.borrow_mut();
                state.quick_settings.wifi_scanning = true;
                state.quick_settings.wifi_scan_error = None;
                state.bump();
            }
            self.qs_refresh.kick_wifi_networks();
        }
    }

    pub(super) fn poll_quick_settings_radio(&mut self) -> bool {
        let Some(job) = self.qs_action_slot.take_radio_completion() else {
            return false;
        };
        if let Some(chrome) = &self.chrome {
            crate::shell::chrome_state::ChromeState::apply_radio_ticket(chrome, &job);
        }
        if job.result().is_some_and(|result| result.is_ok()) {
            self.reconcile_quick_settings_radio(job.change);
        } else {
            self.qs_refresh.kick_field(radio_field(job.change));
        }
        true
    }

    /// Reconcile every optimistic device toggle after the OS accepts or
    /// rejects it. Failures must not leave a switch claiming a state the
    /// machine never entered.
    pub(super) fn finish_quick_settings_action(
        &mut self,
        field: QsField,
        operation: &'static str,
        failure_title: String,
        result: Result<(), String>,
    ) {
        if let Err(error) = result {
            tracing::warn!(target: "tilo::os_actions", op = operation, %error, "quick setting failed");
            if let Some(chrome) = self.chrome.as_ref() {
                chrome
                    .borrow_mut()
                    .push_notification(failure_title, t("host-qs-change-failed-state-restored"));
            }
        }
        // Even a successful daemon request can be normalized by hardware or
        // policy. Read back the authoritative state of what was written —
        // only that; completion wakes paint.
        self.qs_refresh.kick_field(field);
    }
}

/// A level as chrome state keeps it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a level in 0..=1 kept at f32 precision"
)]
fn level_of(level: f64) -> f32 {
    level.clamp(0.0, 1.0) as f32
}

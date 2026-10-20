//! Quick settings clicks, keyboard, sliders and the actions they dispatch.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Clicks inside the open quick-settings popover: dismiss outside
    /// the panel, or route to the drill-in sub-page vs. the root page.
    /// Reached only once `flags.quick_settings` is true, at which point
    /// every path absorbs the click.
    pub(super) fn handle_shell_quick_settings_click(&mut self, p: Point, handled: &mut bool) {
        let p = self.untransform_quick_settings_pointer(p, Instant::now());
        let qs_rect = self.quick_settings_rect();
        if self.handle_now_playing_click(p) {
            *handled = true;
            return;
        }
        if !qs_rect.contains(p) {
            if let Some(chrome) = self.chrome.clone() {
                let mut st = chrome.borrow_mut();
                st.overlays.quick_settings = false;
                // Next open starts at root, not the last drill-in.
                st.quick_settings.nav_stack = vec![crate::shell::quick_settings::QsLevel::Root];
                st.bump();
            }
            *handled = true;
            return;
        }
        let current_level = self
            .chrome
            .as_ref()
            .map_or(crate::shell::quick_settings::QsLevel::Root, |c| {
                c.borrow().quick_settings.current_level().clone()
            });
        if matches!(current_level, crate::shell::quick_settings::QsLevel::Root) {
            self.handle_shell_quick_settings_root_click(p);
        } else {
            self.handle_shell_quick_settings_drill_in_click(p, &current_level);
        }
        *handled = true;
    }

    /// Clicks on a quick-settings drill-in sub-page (Wi-Fi, VPN,
    /// Bluetooth, notifications): back, the passphrase prompt, or a
    /// row. Split out of `handle_shell_quick_settings_click`, which
    /// always treats the click as absorbed regardless of what this
    /// does.
    pub(super) fn handle_shell_quick_settings_drill_in_click(
        &mut self,
        p: Point,
        current_level: &crate::shell::quick_settings::QsLevel,
    ) {
        let back = if matches!(
            current_level,
            crate::shell::quick_settings::QsLevel::Notifications
        ) {
            self.qs_notification_back_rect()
        } else {
            self.qs_back_chevron_rect()
        };
        match current_level {
            crate::shell::quick_settings::QsLevel::BluetoothDetails(address) => {
                self.handle_qs_bluetooth_details_click(p, address);
            }
            crate::shell::quick_settings::QsLevel::Notifications => {
                self.handle_qs_notifications_click(p);
            }
            crate::shell::quick_settings::QsLevel::Root if back.contains(p) => {
                self.pop_quick_settings_level();
            }
            crate::shell::quick_settings::QsLevel::Root => {}
        }
    }

    /// Carry out a press in the Wi-Fi section.
    pub(super) fn apply_qs_wifi_intent(&mut self, chrome: &Chrome, intent: WifiIntent) {
        match intent {
            WifiIntent::Join(ssid) => {
                {
                    let mut state = chrome.borrow_mut();
                    state.quick_settings.wifi_password = None;
                    state.quick_settings.wifi_joining = Some(ssid.clone());
                    state.bump();
                }
                self.sync_wifi_password_input();
                self.qs_refresh.kick_wifi_join(ssid, None, false);
            }
            WifiIntent::AskPassword(ssid) => {
                self.begin_wifi_password(&ssid);
                // A password another of the person's devices shares joins
                // without asking.
                let _ = self.try_connect_with_shared_wifi_password();
            }
            WifiIntent::SubmitPassword => self.submit_wifi_password(),
            WifiIntent::CancelPassword => self.cancel_wifi_password(),
        }
    }

    /// Carry out a press in the Bluetooth section: connect or disconnect an
    /// accessory, or open its use on the person's other devices.
    pub(super) fn apply_qs_bluetooth_intent(&mut self, chrome: &Chrome, intent: BluetoothIntent) {
        let actions = crate::shell::search::providers::actions::ValidatedQsActions::new(
            self.qs_actions.as_ref(),
        );
        let (address, connect) = match intent {
            BluetoothIntent::Details(address) => {
                let mut state = chrome.borrow_mut();
                state.quick_settings.push_level(
                    crate::shell::quick_settings::QsLevel::BluetoothDetails(address),
                );
                self.qs_device_fabric_start = 0;
                self.qs_device_fabric_wheel_remainder = 0.0;
                state.bump();
                return;
            }
            BluetoothIntent::Connect(address) => (address, true),
            BluetoothIntent::Disconnect(address) => (address, false),
        };
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
        let result = crate::shell::search::providers::actions::ValidatedQsActions::new(
            self.qs_actions.as_ref(),
        )
        .set_vpn_connection(name, enabled);
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
                && state.quick_settings.current_level()
                    == &crate::shell::quick_settings::QsLevel::Root
                && state.quick_settings.section
                    == Some(crate::shell::quick_settings::QsSection::Wifi)
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

    pub(super) fn handle_qs_notifications_click(&mut self, p: Point) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let layout = self.qs_notification_layout();
        let outcome = self.notification_center.click(
            &ChromeNotifications::new(&chrome).with_agents(&self.room_activity),
            &layout,
            p,
        );
        self.apply_notification_center_outcome(outcome);
    }

    pub(super) fn pop_quick_settings_level(&self) {
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().quick_settings_back();
        }
    }

    /// Routes a click through the root-page component, then applies the
    /// resulting shell-wide navigation or side effect after hit-testing.
    pub(super) fn handle_shell_quick_settings_root_click(&mut self, p: Point) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let panel = self.quick_settings_rect();
        let action = {
            let mut st = chrome.borrow_mut();
            self.qs_root.handle_click(p, panel, &mut st.quick_settings)
        };
        // A press that changes what is under the pointer (a page, a section
        // opening) owns its release.
        self.qs_navigation_release_pending |=
            matches!(action, RootAction::Enter(_) | RootAction::SectionOpened(_));
        self.apply_quick_settings_root_action(action, &chrome);
    }

    /// Keyboard parity for the immediate-mode quick-settings root. Tab uses
    /// paint order, Enter/Space route through the same action dispatcher as a
    /// pointer click, and arrows adjust the two range controls in small steps.
    #[allow(
        clippy::too_many_lines,
        reason = "Keyboard focus and control adjustments intentionally use the same immediate-mode action path as pointer input."
    )]
    pub(super) fn handle_quick_settings_keyboard(
        &mut self,
        logical_key: &Key,
        backwards: bool,
        control_down: bool,
        handled: &mut bool,
    ) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let history_level = {
            let state = chrome.borrow();
            state
                .overlays
                .quick_settings
                .then(|| state.quick_settings.current_level().clone())
        };
        if matches!(
            history_level,
            Some(crate::shell::quick_settings::QsLevel::Notifications)
        ) {
            let consumed = self.handle_qs_notification_keyboard_with_shift(logical_key, backwards);
            *handled = consumed;
            return consumed;
        }
        if self.qs_clipboard_takes_key(&chrome, logical_key, control_down) {
            let consumed = self.handle_qs_clipboard_keyboard(logical_key, control_down);
            *handled = consumed;
            return consumed;
        }
        if matches!(
            history_level,
            Some(crate::shell::quick_settings::QsLevel::BluetoothDetails(_))
        ) {
            let consumed = self.handle_qs_device_fabric_keyboard(logical_key);
            *handled = consumed;
            return consumed;
        }
        let at_root = {
            let state = chrome.borrow();
            state.overlays.quick_settings
                && matches!(
                    state.quick_settings.current_level(),
                    crate::shell::quick_settings::QsLevel::Root
                )
        };
        if !at_root {
            return false;
        }

        // Edit mode shares the same keyboard contract as every reorderable
        // Tilo list: Ctrl+Up/Down moves the focused control, while ordinary
        // arrows retain their range-adjustment meaning outside edit mode.
        if control_down
            && matches!(
                logical_key,
                Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown)
            )
        {
            let direction = if matches!(logical_key, Key::Named(NamedKey::ArrowUp)) {
                -1
            } else {
                1
            };
            let changed = {
                let state = chrome.borrow();
                self.qs_root
                    .reorder_focused(&state.quick_settings, direction)
            };
            if changed {
                chrome.borrow_mut().bump();
            }
            *handled = changed;
            return changed;
        }

        if matches!(logical_key, Key::Named(NamedKey::Tab)) {
            if self.handle_now_playing_tab(backwards) {
                *handled = true;
                return true;
            }
            let mut state = chrome.borrow_mut();
            self.qs_root.focus_next(&state.quick_settings, backwards);
            state.bump();
            *handled = true;
            return true;
        }

        let card_key = match logical_key {
            Key::Named(NamedKey::Enter | NamedKey::Space) => Some((true, true)),
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowUp) => Some((false, true)),
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowDown) => Some((false, false)),
            _ => None,
        };
        if let Some((activate, forward)) = card_key
            && self.handle_now_playing_key(activate, forward)
        {
            *handled = true;
            return true;
        }

        if matches!(logical_key, Key::Named(NamedKey::Enter | NamedKey::Space)) {
            let panel = self.quick_settings_rect();
            let action = {
                let mut state = chrome.borrow_mut();
                self.qs_root
                    .activate_focused(panel, &mut state.quick_settings)
            };
            self.apply_quick_settings_root_action(action, &chrome);
            *handled = true;
            return true;
        }

        let direction = if matches!(
            logical_key,
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowDown)
        ) {
            -1.0
        } else if matches!(
            logical_key,
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowUp)
        ) {
            1.0
        } else {
            return false;
        };
        let commit = {
            let mut state = chrome.borrow_mut();
            let commit = self
                .qs_root
                .adjust_focused_slider(&mut state.quick_settings, direction);
            if commit.is_some() {
                state.bump();
            }
            commit
        };
        let Some(commit) = commit else {
            return false;
        };
        self.commit_quick_settings_slider(commit);
        *handled = true;
        true
    }

    /// Commit one level change through the same truthful feedback path as a
    /// hardware key. A positive volume adjustment intentionally unmutes an
    /// already-muted sink; otherwise the slider moves while sound stays off.
    pub(super) fn commit_quick_settings_slider(&mut self, commit: SliderCommit) {
        let actions = crate::shell::search::providers::actions::ValidatedQsActions::new(
            self.qs_actions.as_ref(),
        );
        let (field, result) = match commit {
            SliderCommit::Brightness(value_pct) => (
                QsField::Brightness,
                actions
                    .set_brightness(value_pct)
                    .map(|()| crate::shell::osd::Osd::brightness(value_pct / 100.0)),
            ),
            SliderCommit::DisplayBrightness(index, value_pct) => {
                // The readout names the display whenever more than one can
                // be dimmed.
                let target = self.chrome.as_ref().and_then(|chrome| {
                    let state = chrome.borrow();
                    let displays = &state.quick_settings.displays;
                    displays.get(index).map(|display| {
                        (
                            display.connector.clone(),
                            (displays.len() > 1).then(|| display.name.clone()),
                        )
                    })
                });
                let result = target.map_or_else(
                    || Err("that monitor is no longer connected".to_owned()),
                    |(connector, name)| {
                        actions
                            .set_display_brightness(&connector, value_pct)
                            .map(|()| name)
                    },
                );
                (
                    QsField::Brightness,
                    result.map(|name| {
                        crate::shell::osd::Osd::display_brightness(
                            value_pct / 100.0,
                            name.as_deref(),
                        )
                    }),
                )
            }
            SliderCommit::Volume(value_pct) => {
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
        (self.app_launcher)(chrome, "so.tilo.settings", "Settings");
    }

    /// A section opened in place on the root page: what it lists is read as
    /// it opens.
    fn open_qs_section(
        &mut self,
        section: crate::shell::quick_settings::QsSection,
        chrome: &Chrome,
    ) {
        match section {
            crate::shell::quick_settings::QsSection::Wifi => {
                // The first scan starts now; the section keeps
                // scanning while it is open.
                self.qs_wifi_scanned_at = None;
                self.rescan_open_wifi_section(Instant::now());
            }
            crate::shell::quick_settings::QsSection::Bluetooth => {
                self.qs_refresh.kick_bt_devices();
            }
            crate::shell::quick_settings::QsSection::Vpn => {
                self.qs_refresh.kick_field(QsField::Vpn);
            }
            crate::shell::quick_settings::QsSection::Battery => {
                self.qs_refresh.kick_field(QsField::Power);
            }
            crate::shell::quick_settings::QsSection::Sound => {
                if let Ok(view) = self.device_fabric_call(DeviceFabricUiAction::Snapshot) {
                    chrome.borrow_mut().quick_settings.sound_outputs = sound_outputs_of(&view);
                }
            }
            crate::shell::quick_settings::QsSection::Clipboard
            | crate::shell::quick_settings::QsSection::Devices => {
                self.refresh_qs_hosted_rows();
            }
        }
        chrome.borrow_mut().bump();
    }

    pub(super) fn apply_quick_settings_root_action(&mut self, action: RootAction, chrome: &Chrome) {
        match action {
            RootAction::None => {}
            RootAction::Enter(level) => {
                let mut st = chrome.borrow_mut();
                st.quick_settings.push_level(level);
                st.bump();
            }
            RootAction::SectionOpened(section) => self.open_qs_section(section, chrome),
            RootAction::Wifi(intent) => self.apply_qs_wifi_intent(chrome, intent),
            RootAction::Bluetooth(intent) => self.apply_qs_bluetooth_intent(chrome, intent),
            RootAction::Vpn { name, enabled } => self.apply_qs_vpn_change(chrome, &name, enabled),
            RootAction::Hosted {
                section,
                index,
                point,
            } => match section {
                crate::shell::quick_settings::QsSection::Devices => {
                    self.press_qs_device_row(index, point);
                }
                crate::shell::quick_settings::QsSection::Clipboard => {
                    self.press_qs_clipboard_row(index, point);
                }
                _ => {}
            },
            RootAction::ClearClipboard => self.clear_qs_clipboard(),
            RootAction::SoundOutput(node_id) => {
                // The same route the Devices page takes; its answer is the
                // outputs as they stand now.
                match self.device_fabric_call(DeviceFabricUiAction::RouteLocalAudio { node_id }) {
                    Ok(view) => {
                        let mut st = chrome.borrow_mut();
                        st.quick_settings.sound_outputs = sound_outputs_of(&view);
                        st.bump();
                    }
                    Err(error) => {
                        let mut st = chrome.borrow_mut();
                        st.push_notification(t("host-fabric-action-failed"), error);
                        st.bump();
                    }
                }
            }
            RootAction::Changed(pending) => {
                {
                    let mut st = chrome.borrow_mut();
                    if let Some(on) = pending.do_not_disturb {
                        st.set_dnd(on);
                    }
                    st.bump();
                }
                self.dispatch_quick_settings_toggle_actions(pending);
                if let Some(locked) = pending.rotation_lock {
                    Self::save_rotation_lock(locked);
                }
            }
            RootAction::LayoutChanged => {
                chrome.borrow_mut().bump();
            }
            RootAction::OpenSettings(link) => {
                // This is the device settings affordance, not workspace
                // sharing settings. Without a link Settings opens on the
                // page the person used last.
                self.open_settings(chrome, link);
            }
            RootAction::Lock => {
                {
                    let mut st = chrome.borrow_mut();
                    st.overlays.quick_settings = false;
                    st.quick_settings.nav_stack = vec![crate::shell::quick_settings::QsLevel::Root];
                    st.bump();
                }
                self.execute_session_action(os_actions::SessionAction::Lock);
            }
            RootAction::OpenPowerMenu => {
                let mut st = chrome.borrow_mut();
                self.layers.power_menu.open();
                st.overlays.quick_settings = false;
                st.quick_settings.nav_stack = vec![crate::shell::quick_settings::QsLevel::Root];
                st.bump();
            }
        }
    }

    /// Persist the rotation lock; the compositor's accelerometer worker
    /// reads it from the same settings document.
    pub(super) fn save_rotation_lock(locked: bool) {
        let origin = settings_store::ChangeOrigin::local("qs.rotation-lock");
        if let Err(e) = settings_store::hardware::ROTATION_LOCK.set_by(locked, &origin) {
            tracing::warn!(error = %e, "rotation lock preference save failed");
        }
    }

    /// Shells out to the OS for changes returned by the root component.
    /// Do-not-disturb and rotation lock are applied by the caller.
    pub(super) fn dispatch_quick_settings_toggle_actions(
        &mut self,
        pending: crate::shell::quick_settings::root::PendingRootActions,
    ) {
        if let Some(v) = pending.wifi {
            self.dispatch_quick_settings_radio(os_actions::RadioChange::Wifi(v));
        }
        if let Some(v) = pending.bluetooth {
            let result = crate::shell::search::providers::actions::ValidatedQsActions::new(
                self.qs_actions.as_ref(),
            )
            .set_bluetooth(v);
            self.finish_quick_settings_action(
                QsField::Bluetooth,
                "set_bluetooth",
                t("host-qs-change-failed-bluetooth"),
                result,
            );
        }
        if let Some(v) = pending.wired {
            let result = crate::shell::search::providers::actions::ValidatedQsActions::new(
                self.qs_actions.as_ref(),
            )
            .set_wired(v);
            self.finish_quick_settings_action(
                QsField::Wired,
                "set_wired",
                t("host-qs-change-failed-wired"),
                result,
            );
        }
        if let Some(v) = pending.airplane {
            self.dispatch_quick_settings_radio(os_actions::RadioChange::Airplane(v));
        }
        if let Some(v) = pending.hotspot {
            let result = crate::shell::search::providers::actions::ValidatedQsActions::new(
                self.qs_actions.as_ref(),
            )
            .set_hotspot(v);
            self.finish_quick_settings_action(
                QsField::Hotspot,
                "set_hotspot",
                t("host-qs-change-failed-hotspot"),
                result,
            );
        }
        if let Some(v) = pending.mobile {
            let result = crate::shell::search::providers::actions::ValidatedQsActions::new(
                self.qs_actions.as_ref(),
            )
            .set_mobile_broadband(v);
            self.finish_quick_settings_action(
                QsField::Mobile,
                "set_mobile_broadband",
                t("host-qs-change-failed-mobile"),
                result,
            );
        }
        if let Some(v) = pending.night_light {
            let origin = settings_store::ChangeOrigin::local("qs.night-light");
            if let Err(e) = settings_store::hardware::NIGHT_LIGHT.set_by(v, &origin) {
                tracing::warn!(error = %e, "night light preference save failed");
            }
        }
        if let Some(p) = pending.power {
            use crate::shell::quick_settings::PowerProfile as UiPP;
            use crate::shell::quick_settings::actions::PowerProfile as ActPP;
            let mapped = match p {
                UiPP::Saver => ActPP::Saver,
                UiPP::Balanced => ActPP::Balanced,
                UiPP::Performance => ActPP::Performance,
            };
            let result = crate::shell::search::providers::actions::ValidatedQsActions::new(
                self.qs_actions.as_ref(),
            )
            .set_power_profile(mapped);
            self.finish_quick_settings_action(
                QsField::Power,
                "set_power_profile",
                t("host-qs-change-failed-power"),
                result,
            );
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
            job.apply(chrome);
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

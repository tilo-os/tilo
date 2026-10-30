//! Quick Settings, hosted: the panel is the `tilo-quick-settings` app,
//! mounted in this process as the feedback composer is, drawn in the
//! popover under the tray and given the popover's input while it is open.
//!
//! The app owns what the panel shows and how a person moves through it; the
//! shell owns the device. It lends the app a [`PanelHost`]: each frame the
//! host feeds the app the device's readings from chrome state, and every
//! effect the app publishes (a tile, a slider, the palette, a script, an
//! agent on this desktop) comes back here as a [`QsEffect`] and is carried
//! out through the same OS dispatcher, device fabric, clipboard manager
//! and notification log as before. The app's own actions (the section
//! open, the page shown) are published to this device's catalogue as a
//! private window's are.
//!
//! The app's effect handler runs inside the popover's input, while the
//! host is busy routing it, so effects are queued and carried out on the
//! next pass of [`ShellOverlayHost::pump_quick_settings`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tilo_quick_settings::QsEffect;
use tilo_quick_settings::host::{Feed, PanelHost, Showing};

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

/// The tab id the panel's window is published under in this device's
/// catalogue of private windows: no tab has it.
pub(crate) const QUICK_SETTINGS_WINDOW: u64 = u64::MAX - 1;

/// What the app hands the host, kept until the host's next pass.
#[derive(Default)]
struct Link {
    feed: RefCell<Option<Feed>>,
    showing: RefCell<Option<Showing>>,
    effects: RefCell<Vec<QsEffect>>,
    height: Cell<f64>,
    /// The session's wake, which the app's own background work calls too.
    wake: std::sync::Arc<std::sync::Mutex<Option<crate::session_runtime::Wake>>>,
}

impl Link {
    fn wake(&self) {
        wake_through(&self.wake);
    }
}

fn wake_through(wake: &std::sync::Mutex<Option<crate::session_runtime::Wake>>) {
    let wake = wake
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(wake) = wake {
        wake();
    }
}

struct LinkHost(Rc<Link>);

impl PanelHost for LinkHost {
    fn attach(&self, feed: Feed) {
        *self.0.feed.borrow_mut() = Some(feed);
    }

    fn showing(&self, showing: &Showing) {
        *self.0.showing.borrow_mut() = Some(showing.clone());
        self.0.wake();
    }

    fn run(&self, effect: QsEffect) -> Result<(), String> {
        self.0.effects.borrow_mut().push(effect);
        self.0.wake();
        Ok(())
    }

    fn laid_out(&self, height: f64) {
        self.0.height.set(height);
    }
}

/// The hosted panel.
#[derive(Default)]
pub(super) struct QuickSettingsPanel {
    link: Rc<Link>,
    app: Option<Box<dyn ui::widget::AppWidget>>,
    /// What the panel shows, as it last said.
    showing: Showing,
    /// The page and section chrome state named when the host last matched
    /// the panel to it: a caller that opens Quick Settings somewhere (a
    /// shortcut, a search result, a notification) changes them.
    requested: Option<Requested>,
    /// A notification to open in full once the panel shows the
    /// notifications (a banner read from its stack).
    reading: Option<u64>,
    /// Where the panel was laid out, in the overlay's coordinates.
    rect: Rect,
    /// The readings last fed, so an unchanged frame feeds nothing.
    fed: Option<tilo_quick_settings::QuickSettingsState>,
    /// The arrangement of controls, as the person left it.
    arrangement: settings_store::hardware::QuickSettingsConfig,
    /// Whether arranging writes the person's settings; a test or a capture
    /// keeps its arrangement to itself.
    persists: bool,
}

/// Where chrome state asks the panel to be.
#[derive(Clone, Debug, PartialEq)]
struct Requested {
    level: tilo_quick_settings::QsLevel,
    section: Option<tilo_quick_settings::QsSection>,
    pasting: bool,
}

impl Requested {
    fn of(state: &tilo_quick_settings::QuickSettingsState) -> Self {
        Self {
            level: state.current_level().clone(),
            section: state.section,
            pasting: state.paste_origin.is_some(),
        }
    }

    fn of_showing(showing: &Showing) -> Self {
        use tilo_quick_settings::{Page, QsLevel};
        Self {
            level: match showing.page {
                Page::Root => QsLevel::Root,
                Page::Notifications | Page::Notification => QsLevel::Notifications,
                Page::Accessory => QsLevel::BluetoothDetails(showing.accessory.clone()),
            },
            section: showing.section,
            pasting: showing.pasting,
        }
    }
}

impl QuickSettingsPanel {
    pub(super) fn persisted() -> Self {
        Self {
            arrangement: settings_store::hardware::QuickSettingsConfig::load(),
            persists: true,
            ..Self::default()
        }
    }

    /// The panel with `arrangement`, which arranging changes but never
    /// writes to the person's settings.
    pub(super) fn with_arrangement(
        arrangement: settings_store::hardware::QuickSettingsConfig,
    ) -> Self {
        Self {
            arrangement: arrangement.normalised(),
            ..Self::default()
        }
    }

    pub(super) fn set_wake(&self, wake: crate::session_runtime::Wake) {
        *self
            .link
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(wake);
    }

    pub(super) fn app_mut(&mut self) -> Option<&mut Box<dyn ui::widget::AppWidget>> {
        self.app.as_mut()
    }

    /// Give the panel the keyboard from the card beside it: on its last
    /// control coming back with Shift+Tab, else on its first.
    pub(super) fn enter_focus(&mut self, backwards: bool) {
        let Some(app) = self.app.as_mut() else {
            return;
        };
        match app
            .as_any_mut()
            .and_then(|app| app.downcast_mut::<ui::app::EmbeddedApp>())
        {
            Some(embedded) => embedded.enter_focus(backwards),
            None => app.set_focus(true),
        }
    }

    pub(super) fn app(&self) -> Option<&dyn ui::widget::AppWidget> {
        self.app.as_deref()
    }

    pub(super) fn showing(&self) -> &Showing {
        &self.showing
    }

    pub(super) fn rect(&self) -> Rect {
        self.rect
    }

    /// How tall the panel's content lays out, once it has.
    pub(super) fn content_height(&self) -> Option<f64> {
        let height = self.link.height.get();
        (height > 0.0).then_some(height)
    }

    /// Mount the app, once.
    pub(super) fn mount(&mut self) {
        if self.app.is_some() {
            return;
        }
        let host: Rc<dyn PanelHost> = Rc::new(LinkHost(Rc::clone(&self.link)));
        // The panel's record lives in memory with the panel (`prepare`):
        // these name nothing beyond it.
        let component =
            match tilo_quick_settings::prepare(host, "this-device", "owner", QUICK_SETTINGS_WINDOW)
            {
                Ok(component) => component,
                Err(error) => {
                    tracing::warn!(%error, "Quick Settings could not be mounted");
                    return;
                }
            };
        let wake = std::sync::Arc::clone(&self.link.wake);
        match tilo_desktop::resolver::app_tile(component, None, move |_| wake_through(&wake)) {
            Ok(app) => self.app = Some(app),
            Err(error) => tracing::warn!(%error, "Quick Settings could not be mounted"),
        }
    }

    /// Invoke the panel's own action `action` with `arguments`, through its
    /// door, as the catalogue does.
    fn invoke(&mut self, action: &str, arguments: serde_json::Value) {
        let Some(app) = self.app.as_mut() else {
            return;
        };
        let resources = app.device_local_resources().unwrap_or_default();
        let Some(address) = tilo_actions::door::survey(&resources)
            .into_iter()
            .find(|descriptor| {
                descriptor.resource_kind == "quick-settings.panel"
                    && descriptor.address.action == action
            })
            .map(|descriptor| descriptor.address)
        else {
            tracing::warn!(action, "Quick Settings does not publish this action");
            return;
        };
        let arguments = match arguments {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        if let Err(error) = app.invoke_device_local_action(&address, &arguments) {
            tracing::warn!(%error, action, "Quick Settings refused its own action");
        }
    }

    /// Open notification `id` in full, once the panel shows the
    /// notifications.
    pub(super) fn read(&mut self, id: u64) {
        self.reading = Some(id);
    }

    /// Start arranging the panel's controls.
    pub(super) fn edit(&mut self) {
        self.invoke("edit", serde_json::json!({ "editing": true }));
    }

    /// Go back inside the panel: whether there was somewhere to go.
    pub(super) fn back(&mut self) -> bool {
        if !self.showing.goes_back() {
            return false;
        }
        self.invoke("back", serde_json::Value::Null);
        true
    }

    /// Bring the panel to where chrome state asks for it.
    fn follow_request(&mut self, requested: &Requested) {
        use tilo_quick_settings::QsLevel;
        if requested.pasting {
            self.invoke("paste", serde_json::Value::Null);
            return;
        }
        match &requested.level {
            QsLevel::Notifications => self.invoke("show-notifications", serde_json::Value::Null),
            QsLevel::BluetoothDetails(address) => {
                self.invoke("show-accessory", serde_json::json!({ "address": address }));
            }
            QsLevel::Root => match requested.section {
                Some(section) => {
                    self.invoke("open-section", serde_json::json!({ "section": section }));
                }
                None => self.invoke("show-controls", serde_json::Value::Null),
            },
        }
    }

    /// Lay the app out at `rect`.
    pub(super) fn place(&mut self, rect: Rect, theme: &Theme) {
        self.rect = rect;
        let Some(app) = self.app.as_mut() else {
            return;
        };
        let mut taffy = taffy::TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        let child = app.build_layout(&mut taffy, theme, &mut nodes);
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::style_helpers::length(ui::num::f32_of_f64(rect.width())),
                height: taffy::style_helpers::length(ui::num::f32_of_f64(rect.height())),
            },
            ..taffy::Style::default()
        };
        let Ok(root) = taffy.new_with_children(style, &[child]) else {
            return;
        };
        if taffy
            .compute_layout(root, taffy::prelude::TaffyMaxContent::MAX_CONTENT)
            .is_ok()
        {
            app.apply_layout(&taffy, child, rect.origin());
        }
    }
}

impl ShellOverlayHost {
    /// One pass of the hosted panel: mount it, match it to chrome state,
    /// feed it the device's readings, carry out what it asked, and read
    /// what its open section lists. Returns whether anything changed.
    pub(super) fn pump_quick_settings(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        self.quick_settings.mount();
        let mut changed = false;
        // What the person did in the panel comes first: chrome state follows
        // the panel.
        let said = self.quick_settings.link.showing.borrow_mut().take();
        if let Some(showing) = said
            && showing != self.quick_settings.showing
        {
            let requested = Requested::of_showing(&showing);
            {
                let mut state = chrome.borrow_mut();
                let qs = &mut state.quick_settings;
                qs.nav_stack = vec![tilo_quick_settings::QsLevel::Root];
                if requested.level != tilo_quick_settings::QsLevel::Root {
                    qs.nav_stack.push(requested.level.clone());
                }
                qs.section = requested.section;
                if !requested.pasting {
                    qs.paste_origin = None;
                }
                state.bump();
            }
            self.quick_settings.requested = Some(requested);
            self.quick_settings.showing = showing;
            self.follow_quick_settings_showing();
            changed = true;
        }
        // Then where a shortcut, a search result or a notification asks it
        // to be.
        let requested = Requested::of(&chrome.borrow().quick_settings);
        if self.quick_settings.requested.as_ref() != Some(&requested) {
            self.quick_settings.follow_request(&requested);
            self.quick_settings.requested = Some(requested);
            changed = true;
        }
        if let Some(notification) = self.quick_settings.reading.take() {
            self.quick_settings.invoke(
                "read-notification",
                serde_json::json!({ "notification": notification }),
            );
            changed = true;
        }
        changed |= self.run_queued_quick_settings_effects();
        self.feed_quick_settings();
        if let Some(app) = self.quick_settings.app_mut() {
            changed |= app.update(doc, path);
        }
        changed
    }

    /// Carry out what the panel asked since the last pass. Whether it asked
    /// anything.
    fn run_queued_quick_settings_effects(&mut self) -> bool {
        let effects = std::mem::take(&mut *self.quick_settings.link.effects.borrow_mut());
        let ran = !effects.is_empty();
        for effect in effects {
            self.run_quick_settings_effect(effect);
        }
        ran
    }

    /// Feed the panel the device as chrome state reads it now.
    fn feed_quick_settings(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut readings = chrome.borrow().quick_settings.clone();
        readings.arrangement = self.quick_settings.arrangement.clone();
        readings.notifications = self.quick_settings_notifications(&chrome);
        readings.available = self.qs_action_slot.availability();
        if self.quick_settings.fed.as_ref() == Some(&readings) {
            return;
        }
        if let Some(feed) = self.quick_settings.link.feed.borrow().as_ref() {
            feed.set(&readings);
        }
        self.quick_settings.fed = Some(readings);
    }

    /// The notification log as the panel lists it.
    fn quick_settings_notifications(
        &self,
        chrome: &Chrome,
    ) -> tilo_quick_settings::NotificationsView {
        use tilo_desktop::notifications::NotificationHost as _;
        let host = ChromeNotifications::new(chrome).with_agents(&self.room_activity);
        let now = ui::time::now();
        let state = chrome.borrow();
        let log = state.notification_log();
        let rows = log
            .history()
            .iter()
            .rev()
            .map(|notification| tilo_quick_settings::NotificationRow {
                id: notification.id,
                group: tilo_desktop::notifications::group_key(notification)
                    .unwrap_or_else(|| format!("notification-{}", notification.id)),
                app: tilo_desktop::notifications::group_label(notification),
                title: notification.title.clone(),
                body: notification.body.clone(),
                age: tilo_desktop::notifications::relative_time(notification, now),
                actions: host.actions_for(notification).into_iter().take(2).collect(),
                agent: host.agent_for(notification),
                opens: state.history_notification_opens(notification.id),
            })
            .collect();
        tilo_quick_settings::NotificationsView {
            rows,
            quiet: host.quiet_status(),
            can_undo: log.can_undo_clear(),
        }
    }

    /// Read what the page or section the panel shows lists.
    fn follow_quick_settings_showing(&mut self) {
        use tilo_quick_settings::{Page, QsSection};
        let showing = self.quick_settings.showing.clone();
        if showing.page == Page::Accessory {
            let targets = self
                .device_fabric_call(DeviceFabricUiAction::Snapshot)
                .ok()
                .map(|view| {
                    view.devices
                        .into_iter()
                        .map(|device| tilo_quick_settings::HandoffTarget {
                            id: device.id.as_str().to_owned(),
                            label: device.label,
                            online: device.online,
                            local: device.is_local,
                        })
                        .collect()
                });
            if let Some(chrome) = self.chrome.as_ref() {
                chrome.borrow_mut().quick_settings.handoff_targets = targets;
            }
        }
        match showing.section {
            Some(QsSection::Wifi) => {
                // The first scan starts now; the section keeps scanning
                // while it is open.
                self.qs_wifi_scanned_at = None;
                self.rescan_open_wifi_section(Instant::now());
            }
            Some(QsSection::Bluetooth) => self.qs_refresh.kick_bt_devices(),
            Some(QsSection::Vpn) => self.qs_refresh.kick_field(QsField::Vpn),
            Some(QsSection::Battery | QsSection::PowerMode) => {
                self.qs_refresh.kick_field(QsField::Power);
            }
            Some(QsSection::Sound) => {
                if let Ok(view) = self.device_fabric_call(DeviceFabricUiAction::Snapshot)
                    && let Some(chrome) = self.chrome.as_ref()
                {
                    chrome.borrow_mut().quick_settings.sound_outputs = sound_outputs_of(&view);
                }
            }
            Some(QsSection::Clipboard | QsSection::Devices) => self.refresh_qs_hosted_rows(),
            None => {}
        }
        self.bump_chrome();
    }

    /// Carry out one of the panel's effects.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per effect, each handing it to what owns it"
    )]
    fn run_quick_settings_effect(&mut self, effect: QsEffect) {
        use tilo_quick_settings::actions::QsActions as _;
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        match effect {
            QsEffect::SetWifi { on } => {
                self.dispatch_quick_settings_radio(os_actions::RadioChange::Wifi(on));
            }
            QsEffect::ToggleWifi => {
                let on = !chrome.borrow().quick_settings.wifi;
                self.dispatch_quick_settings_radio(os_actions::RadioChange::Wifi(on));
            }
            QsEffect::SetAirplaneMode { on } => {
                self.dispatch_quick_settings_radio(os_actions::RadioChange::Airplane(on));
            }
            QsEffect::ToggleAirplaneMode => {
                let on = !chrome.borrow().quick_settings.radios.airplane_mode;
                self.dispatch_quick_settings_radio(os_actions::RadioChange::Airplane(on));
            }
            QsEffect::SetWired { on } => {
                if let Some(wired) = chrome.borrow_mut().quick_settings.wired.as_mut() {
                    wired.connected = on;
                }
                let result = self.validated_qs_actions().set_wired(on);
                self.finish_quick_settings_action(
                    QsField::Wired,
                    "set_wired",
                    t("host-qs-change-failed-wired"),
                    result,
                );
            }
            QsEffect::ToggleBluetooth => {
                let on = !chrome.borrow().quick_settings.bluetooth;
                self.run_quick_settings_effect(QsEffect::SetBluetooth { on });
            }
            QsEffect::SetBluetooth { on } => {
                chrome.borrow_mut().quick_settings.bluetooth = on;
                let result = self.validated_qs_actions().set_bluetooth(on);
                self.finish_quick_settings_action(
                    QsField::Bluetooth,
                    "set_bluetooth",
                    t("host-qs-change-failed-bluetooth"),
                    result,
                );
            }
            QsEffect::SetMobileData { on } => {
                chrome.borrow_mut().quick_settings.radios.wwan_enabled = on;
                let result = self.validated_qs_actions().set_mobile_broadband(on);
                self.finish_quick_settings_action(
                    QsField::Mobile,
                    "set_mobile_broadband",
                    t("host-qs-change-failed-mobile"),
                    result,
                );
            }
            QsEffect::SetNightLight { on } => {
                chrome.borrow_mut().quick_settings.night_light = on;
                let origin = settings_store::ChangeOrigin::local("qs.night-light");
                if let Err(error) = settings_store::hardware::NIGHT_LIGHT.set_by(on, &origin) {
                    tracing::warn!(%error, "night light preference save failed");
                }
            }
            QsEffect::SetDoNotDisturb { on } => chrome.borrow_mut().set_dnd(on),
            QsEffect::SetRotationLock { locked } => {
                chrome.borrow_mut().quick_settings.rotation_lock = locked;
                Self::save_rotation_lock(locked);
            }
            QsEffect::SetPowerMode { mode } => {
                use tilo_quick_settings::PowerProfile as Ui;
                use tilo_quick_settings::actions::PowerProfile as Os;
                chrome.borrow_mut().quick_settings.power_profile = mode;
                let result = self.validated_qs_actions().set_power_profile(match mode {
                    Ui::Saver => Os::Saver,
                    Ui::Balanced => Os::Balanced,
                    Ui::Performance => Os::Performance,
                });
                self.finish_quick_settings_action(
                    QsField::Power,
                    "set_power_profile",
                    t("host-qs-change-failed-power"),
                    result,
                );
            }
            QsEffect::SetVpn { profile, on } => self.apply_qs_vpn_change(&chrome, &profile, on),
            QsEffect::SetBrightness { level } => {
                self.commit_quick_settings_level(QsLevelChange::Brightness(level));
            }
            QsEffect::SetDisplayBrightness { display, level } => {
                self.commit_quick_settings_level(QsLevelChange::Display(display, level));
            }
            QsEffect::BrightnessUp => {
                let change = brightness_step(&chrome.borrow().quick_settings, BRIGHTNESS_STEP);
                self.commit_quick_settings_level(change);
            }
            QsEffect::BrightnessDown => {
                let change = brightness_step(&chrome.borrow().quick_settings, -BRIGHTNESS_STEP);
                self.commit_quick_settings_level(change);
            }
            QsEffect::SetVolume { level } => {
                self.commit_quick_settings_level(QsLevelChange::Volume(level));
            }
            QsEffect::VolumeUp => {
                let change = volume_step(&chrome.borrow().quick_settings, VOLUME_STEP);
                self.commit_quick_settings_level(change);
            }
            QsEffect::VolumeDown => {
                let change = volume_step(&chrome.borrow().quick_settings, -VOLUME_STEP);
                self.commit_quick_settings_level(change);
            }
            QsEffect::SetMuted { muted } => self.set_quick_settings_muted(&chrome, muted),
            QsEffect::ToggleMuted => {
                let muted = !chrome.borrow().quick_settings.volume_muted();
                self.set_quick_settings_muted(&chrome, muted);
            }
            QsEffect::RouteSound { node } => {
                // The same route the Devices page takes; its answer is the
                // outputs as they stand now.
                let routed = self
                    .device_fabric_call(DeviceFabricUiAction::RouteLocalAudio { node_id: node });
                let mut state = chrome.borrow_mut();
                match routed {
                    Ok(view) => state.quick_settings.sound_outputs = sound_outputs_of(&view),
                    Err(error) => {
                        state.push_notification(t("host-fabric-action-failed"), error);
                    }
                }
            }
            QsEffect::JoinWifi { network } => {
                {
                    let mut state = chrome.borrow_mut();
                    state.quick_settings.wifi_password = None;
                    state.quick_settings.wifi_joining = Some(network.clone());
                }
                self.qs_refresh.kick_wifi_join(network, None, false);
            }
            QsEffect::AskWifiPassword { network } => {
                chrome.borrow_mut().quick_settings.wifi_password =
                    Some(tilo_quick_settings::WifiPasswordPrompt::new(network));
                // A password another of the person's devices shares joins
                // without asking.
                let _ = self.try_connect_with_shared_wifi_password();
            }
            QsEffect::ConnectWifi { network, password } => {
                if password.is_empty() {
                    return;
                }
                {
                    let mut state = chrome.borrow_mut();
                    let prompt = state.quick_settings.wifi_password.get_or_insert_with(|| {
                        tilo_quick_settings::WifiPasswordPrompt::new(network.clone())
                    });
                    prompt.connecting = true;
                    prompt.error = None;
                    state.quick_settings.wifi_joining = Some(network.clone());
                }
                self.qs_refresh
                    .kick_wifi_join(network, Some(password), false);
            }
            QsEffect::CancelWifiPassword => {
                chrome.borrow_mut().quick_settings.wifi_password = None;
            }
            QsEffect::ConnectBluetooth { address } => {
                self.apply_qs_bluetooth_intent(&chrome, &address, true);
            }
            QsEffect::DisconnectBluetooth { address } => {
                let leaving = matches!(
                    self.quick_settings.showing().page,
                    tilo_quick_settings::Page::Accessory
                );
                self.apply_qs_bluetooth_intent(&chrome, &address, false);
                if leaving {
                    self.quick_settings.back();
                }
            }
            QsEffect::HandOffBluetooth { address, device } => {
                let (name, label) = {
                    let state = chrome.borrow();
                    let name = state
                        .quick_settings
                        .bt_devices
                        .iter()
                        .find(|accessory| accessory.address.eq_ignore_ascii_case(&address))
                        .map_or_else(|| address.clone(), |accessory| accessory.name.clone());
                    let label = state
                        .quick_settings
                        .handoff_targets
                        .iter()
                        .flatten()
                        .find(|target| target.id == device)
                        .map_or_else(|| device.clone(), |target| target.label.clone());
                    (name, label)
                };
                let Ok(target) = tilo_network_actor::fabric::DeviceId::new(device) else {
                    return;
                };
                self.finish_qs_device_action(
                    DeviceFabricUiAction::HandoffBluetooth {
                        address,
                        name: name.clone(),
                        target,
                    },
                    t_args(
                        "host-fabric-bluetooth-moving",
                        &[("accessory", name.into()), ("device", label.into())],
                    ),
                );
            }
            QsEffect::PressDevice { row, part } => {
                // optional-decode: device rows are keyed by their index; a press on any other key names no device
                if let Ok(index) = row.parse::<usize>() {
                    self.handle_qs_device_fabric_press(index, part);
                    self.refresh_qs_hosted_rows();
                }
            }
            QsEffect::PasteCopy { copy } => {
                if let Some(id) = clipboard_id(&copy) {
                    self.choose_clipboard_item(id);
                }
            }
            QsEffect::PinCopy { copy, pinned } => {
                if let Some(id) = clipboard_id(&copy) {
                    let _ = self
                        .clipboard_manager_call(ClipboardManagerAction::SetPinned { id, pinned });
                    self.refresh_qs_hosted_rows();
                }
            }
            QsEffect::RemoveCopy { copy } => {
                if let Some(id) = clipboard_id(&copy) {
                    let _ = self.clipboard_manager_call(ClipboardManagerAction::Remove(id));
                    self.refresh_qs_hosted_rows();
                }
            }
            QsEffect::ClearClipboard => {
                let _ = self.clipboard_manager_call(ClipboardManagerAction::ClearUnpinned);
                self.refresh_qs_hosted_rows();
            }
            QsEffect::OpenNotification { notification } => {
                use tilo_desktop::notifications::NotificationHost as _;
                let host = ChromeNotifications::new(&chrome).with_agents(&self.room_activity);
                if let tilo_desktop::notifications::HistoryActivation::OpenApp { app_id, label } =
                    host.activate_history(notification)
                {
                    self.launch_app(&app_id, &label);
                }
            }
            QsEffect::DismissNotification { notification } => {
                use tilo_desktop::notifications::NotificationHost as _;
                ChromeNotifications::new(&chrome).dismiss_history(notification);
            }
            QsEffect::DismissNotificationGroup { group } => {
                use tilo_desktop::notifications::NotificationHost as _;
                ChromeNotifications::new(&chrome).dismiss_history_group(&group);
            }
            QsEffect::RunNotificationAction {
                notification,
                action,
            } => {
                use tilo_desktop::notifications::NotificationHost as _;
                ChromeNotifications::new(&chrome).invoke_action(notification, &action);
            }
            QsEffect::HandNotificationToAgent { notification } => {
                use tilo_desktop::notifications::NotificationHost as _;
                let host = ChromeNotifications::new(&chrome).with_agents(&self.room_activity);
                let kept = chrome
                    .borrow()
                    .notification_log()
                    .history_entry(notification)
                    .cloned();
                if let Some(kept) = kept {
                    host.hand_to_agent(&kept);
                }
            }
            QsEffect::ClearNotifications => {
                use tilo_desktop::notifications::NotificationHost as _;
                ChromeNotifications::new(&chrome).clear_history();
            }
            QsEffect::UndoClearNotifications => {
                use tilo_desktop::notifications::NotificationHost as _;
                ChromeNotifications::new(&chrome).undo_clear();
            }
            effect @ (QsEffect::ShowControl { .. }
            | QsEffect::HideControl { .. }
            | QsEffect::MoveControl { .. }
            | QsEffect::ResetControls) => {
                let readings = {
                    let state = chrome.borrow();
                    let mut readings = state.quick_settings.clone();
                    readings.arrangement = self.quick_settings.arrangement.clone();
                    readings
                };
                let visible =
                    tilo_quick_settings::host::visible_controls(&readings.arrangement, &readings);
                if tilo_quick_settings::host::arrange(
                    &mut self.quick_settings.arrangement,
                    &visible,
                    &effect,
                ) && self.quick_settings.persists
                    && let Err(error) = self.quick_settings.arrangement.save()
                {
                    tracing::warn!(%error, "could not keep the Quick Settings arrangement");
                }
            }
            QsEffect::OpenSettings { link } => {
                self.open_settings(&chrome, (!link.is_empty()).then_some(link.as_str()));
            }
            QsEffect::LockScreen => {
                self.close_quick_settings();
                self.execute_session_action(os_actions::SessionAction::Lock);
            }
            QsEffect::PowerMenu => {
                self.open_power_menu();
                self.close_quick_settings();
            }
        }
        chrome.borrow_mut().bump();
    }

    /// Close the popover; it opens at its controls next time.
    pub(super) fn close_quick_settings(&self) {
        if let Some(chrome) = self.chrome.as_ref() {
            let mut state = chrome.borrow_mut();
            state.overlays.quick_settings = false;
            state.quick_settings.paste_origin = None;
            state.quick_settings.nav_stack = vec![tilo_quick_settings::QsLevel::Root];
            state.quick_settings.section = None;
            state.bump();
        }
    }

    fn validated_qs_actions(
        &self,
    ) -> crate::shell::quick_settings_dispatch::ValidatedQsActions<'_> {
        crate::shell::quick_settings_dispatch::ValidatedQsActions::new(self.qs_actions.as_ref())
    }
}

/// How far Louder and Quieter move the volume.
const VOLUME_STEP: f64 = 0.05;
/// How far Brighter and Dimmer move the brightness.
const BRIGHTNESS_STEP: f64 = 0.1;

/// The volume `step` from the level it is at, as Louder and Quieter ask.
fn volume_step(state: &tilo_quick_settings::QuickSettingsState, step: f64) -> QsLevelChange {
    QsLevelChange::Volume((f64::from(state.volume_level()) + step).clamp(0.0, 1.0))
}

/// The brightness `step` away, as Brighter and Dimmer ask: from the level
/// of the display the person works on, when the desktop spans several that
/// can be dimmed, else from the one brightness.
fn brightness_step(state: &tilo_quick_settings::QuickSettingsState, step: f64) -> QsLevelChange {
    let moved = |level: f32| (f64::from(level) + step).clamp(0.0, 1.0);
    let focused = state.focused_display.as_deref().and_then(|focused| {
        state
            .displays
            .iter()
            .find(|display| display.connector == focused)
    });
    match focused {
        Some(display) => QsLevelChange::Display(display.connector.clone(), moved(display.level)),
        None => QsLevelChange::Brightness(moved(state.brightness)),
    }
}

/// A clipboard copy's id, as its row's key names it.
fn clipboard_id(key: &str) -> Option<[u8; 32]> {
    // optional-decode: clipboard rows are keyed by the copy's hex id; any other key names no copy
    let bytes = hex::decode(key).ok()?;
    bytes.try_into().ok()
}

/// A level the panel set.
pub(super) enum QsLevelChange {
    /// The one brightness, `0..=1`.
    Brightness(f64),
    /// One display's brightness, by connector.
    Display(String, f64),
    Volume(f64),
}

impl ShellOverlayHost {
    /// Follow chrome state's open flag before painting: the popover's
    /// motion, the panel's height as its content lays out, and the
    /// now-playing card beside it.
    pub(super) fn prepare_quick_settings_transition(
        &mut self,
        theme: &Theme,
        flags: crate::shell::chrome_state::OverlayFlags,
    ) {
        self.reconcile_quick_settings_open_edge(flags.quick_settings);
        self.apply_quick_settings_readings();
        self.refresh_now_playing();
        let now = ui::time::now();
        if !flags.quick_settings && !self.qs_motion.is_visible(now) {
            // A panel that has gone opens next time at its controls, with
            // no password half asked for.
            if let Some(chrome) = self.chrome.as_ref() {
                let mut state = chrome.borrow_mut();
                if state.quick_settings.section.is_some()
                    || state.quick_settings.wifi_password.is_some()
                    || state.quick_settings.nav_stack.len() > 1
                {
                    state.quick_settings.section = None;
                    state.quick_settings.wifi_password = None;
                    state.quick_settings.nav_stack = vec![tilo_quick_settings::QsLevel::Root];
                }
            }
        }
        let rect = self.quick_settings_rect();
        self.quick_settings.place(rect, theme);
        self.qs_motion.follow(&qs_motion::QsFrame {
            open: flags.quick_settings,
            height: rect.height(),
            motion: theme.motion(),
            source: self.input_source,
            now,
        });
        self.follow_now_playing_side(flags.quick_settings, theme);
    }

    /// The opening edge: read what the panel shows afresh, give it the
    /// keyboard and start it at its first control. Callers anywhere (the
    /// tray, a shortcut, search, media keys) only flip the chrome flag.
    pub(super) fn reconcile_quick_settings_open_edge(&mut self, open: bool) {
        if open && !self.qs_was_open {
            self.qs_refresh.kick_fast();
            self.qs_refresh.kick_firmware();
            // The Wi-Fi and Bluetooth tiles name what is connected, which
            // only these lists know.
            self.qs_refresh.kick_wifi_networks();
            self.qs_refresh.kick_bt_devices();
            self.refresh_quick_settings_summaries();
        }
        if open != self.qs_was_open {
            // The edge can come before the first update has mounted the
            // panel; mount it now so it takes the keyboard.
            if open {
                self.quick_settings.mount();
            }
            if let Some(app) = self.quick_settings.app_mut() {
                app.set_focus(open);
            }
            self.reconcile_now_playing_open_edge(open);
        }
        self.qs_was_open = open;
    }

    /// Counts the controls page shows on its Devices and Clipboard rows,
    /// read once per opening; while a section is open it follows its list.
    pub(super) fn refresh_quick_settings_summaries(&self) {
        let fabric = self.device_fabric_call(DeviceFabricUiAction::Snapshot).ok();
        let personal_devices = fabric.as_ref().map(|view| view.devices.len());
        let sound_outputs = fabric.as_ref().map(sound_outputs_of);
        let clipboard = self
            .clipboard_manager_bridge
            .is_some()
            .then(|| self.qs_clipboard_view());
        if let Some(chrome) = self.chrome.as_ref() {
            let mut st = chrome.borrow_mut();
            st.quick_settings.personal_devices = personal_devices;
            if let Some(clipboard) = clipboard {
                st.quick_settings.clipboard = clipboard;
            }
            st.quick_settings.notification_count = st.notification_history().len();
            // Without a device fabric (a capture, a test) the outputs stay
            // as they are.
            if let Some(outputs) = sound_outputs {
                st.quick_settings.sound_outputs = outputs;
            }
        }
    }

    /// The popover: no scrim, so it reads as a dropdown rather than a
    /// modal. It appears from the tray (`ui::motion::Appear`) and the
    /// now-playing card comes with it. Returns the bounds it painted.
    pub(super) fn draw_quick_settings_dropdown(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<Rect> {
        let now = ui::time::now();
        if !self.qs_motion.is_visible(now) {
            return None;
        }
        let pose = self.quick_settings_pose(now);
        let in_flight = !pose.is_rest();
        let qs_rect = self.qs_motion.drawn_rect(self.quick_settings_rect(), now);
        // Everything the panel paints, its shadow's reach included, and the
        // now-playing card on each side it stands on while it changes
        // sides: the pose's opacity layer clips to this.
        let panel_reach = quick_settings_settled_bounds(qs_rect, theme, Affine::IDENTITY);
        let reach = self
            .now_playing_view()
            .map(|view| self.now_playing_drawn(&view))
            .unwrap_or_default()
            .into_iter()
            .fold(panel_reach, |reach, (card, _)| {
                reach.union(quick_settings_settled_bounds(card, theme, Affine::IDENTITY))
            });
        let mut card_rect = None;
        pose.paint(scene, transform, reach, |scene, qs_xform| {
            ui::surface::shadow(
                scene,
                qs_xform,
                qs_rect,
                ui::chrome::ChromeRole::Panel.radius(theme, qs_rect.height()),
                &theme.tokens.elevation.floating,
            );
            self.draw_quick_settings(scene, qs_xform, text_renderer, theme, qs_rect);
            card_rect = self.draw_now_playing(scene, qs_xform, text_renderer, theme);
        });
        Some(if in_flight {
            pose.applied_to(transform).transform_rect_bbox(reach)
        } else {
            let settled = quick_settings_settled_bounds(qs_rect, theme, transform);
            card_rect.map_or(settled, |card| {
                settled.union(quick_settings_settled_bounds(card, theme, transform))
            })
        })
    }

    /// The Panel and the app in it, clipped to the Panel as drawn.
    fn draw_quick_settings(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        Self::draw_quick_settings_card_background(scene, transform, theme, rect);
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
        if let Some(app) = self.quick_settings.app_mut() {
            app.draw(
                scene,
                transform,
                text_renderer,
                theme,
                &ui::presence::PresenceCtx::default(),
            );
        }
        scene.pop_layer();
    }

    /// The popover's elevated Panel: shadow, material and edge (HIG §9.1).
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

    /// Input while the popover is open: the popover is modal. A press on
    /// the now-playing card is the card's; anything over the panel is the
    /// app's; a press anywhere else closes it. Escape goes back inside the
    /// panel, and closes it from its controls. Returns whether it took the
    /// event.
    pub(super) fn route_quick_settings_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.overlay_flags().quick_settings {
            return false;
        }
        match event {
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key, state, ..
                },
                ..
            } => {
                // F1 still opens the keyboard guide over the panel.
                if matches!(logical_key, Key::Named(NamedKey::F1)) {
                    return false;
                }
                if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                    if *state == ElementState::Pressed && !self.quick_settings.back() {
                        self.dismiss_quick_settings();
                    }
                    *ctx.handled = true;
                    return true;
                }
                if *state == ElementState::Pressed
                    && self.route_now_playing_key(logical_key, ctx.modifiers.state().shift_key())
                {
                    *ctx.handled = true;
                    return true;
                }
                if let Some(app) = self.quick_settings.app_mut() {
                    app.handle_event(event, ctx);
                }
                // Tab past the panel's last control goes on to the card
                // beside it or around to its first: the popover is modal.
                if *state == ElementState::Pressed
                    && matches!(logical_key, Key::Named(NamedKey::Tab))
                    && !*ctx.handled
                {
                    self.tab_out_of_quick_settings(ctx.modifiers.state().shift_key());
                }
                self.run_queued_quick_settings_effects();
                *ctx.handled = true;
                true
            }
            WindowEvent::Ime(_) | WindowEvent::ModifiersChanged(_) => {
                if let Some(app) = self.quick_settings.app_mut() {
                    app.handle_event(event, ctx);
                }
                matches!(event, WindowEvent::Ime(_))
            }
            WindowEvent::CursorMoved { .. }
            | WindowEvent::CursorLeft { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Touch(_) => self.route_quick_settings_pointer(event, ctx),
            _ => false,
        }
    }

    fn route_quick_settings_pointer(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if self.route_now_playing_scrub(event, ctx.scale_factor) {
            *ctx.handled = true;
            return true;
        }
        let now = Instant::now();
        let at = match event {
            WindowEvent::Touch(touch) => Some(Point::new(
                touch.location.x / ctx.scale_factor,
                touch.location.y / ctx.scale_factor,
            )),
            _ => self.last_cursor,
        }
        .map(|point| self.untransform_quick_settings_pointer(point, now));
        let pressed = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) || matches!(event, WindowEvent::Touch(touch) if touch.phase == winit::event::TouchPhase::Started);
        if pressed
            && let Some(point) = at
            && self.handle_now_playing_click(point)
        {
            *ctx.handled = true;
            return true;
        }
        let over = at.is_some_and(|point| self.quick_settings.rect().contains(point));
        if pressed && !over {
            self.dismiss_quick_settings();
            *ctx.handled = true;
            return true;
        }
        // The panel is hit where it is drawn: while it appears, its pose
        // moves it, and the app's own events are in its resting place.
        let local = match event {
            WindowEvent::CursorMoved { device_id, .. } => {
                at.map(|point| WindowEvent::CursorMoved {
                    device_id: *device_id,
                    position: winit::dpi::PhysicalPosition::new(
                        point.x * ctx.scale_factor,
                        point.y * ctx.scale_factor,
                    ),
                })
            }
            WindowEvent::Touch(touch) => at.map(|point| {
                let mut touch = *touch;
                touch.location = winit::dpi::PhysicalPosition::new(
                    point.x * ctx.scale_factor,
                    point.y * ctx.scale_factor,
                );
                WindowEvent::Touch(touch)
            }),
            other => Some(other.clone()),
        };
        if let (Some(local), Some(app)) = (local, self.quick_settings.app_mut()) {
            app.handle_event(&local, ctx);
        }
        self.run_queued_quick_settings_effects();
        // Nothing over the panel reaches what is under it.
        if over {
            *ctx.handled = true;
        }
        over
    }

    /// Keys for the now-playing card: F6 moves the keyboard to it and back,
    /// and while it holds the keyboard Tab, Enter, Space and the arrows are
    /// its.
    fn route_now_playing_key(&mut self, key: &Key, backwards: bool) -> bool {
        if matches!(key, Key::Named(NamedKey::F6)) {
            return self.switch_now_playing_focus();
        }
        match key {
            Key::Named(NamedKey::Tab) => self.handle_now_playing_tab(backwards),
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                self.handle_now_playing_key(true, true)
            }
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowUp) => {
                self.handle_now_playing_key(false, true)
            }
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowDown) => {
                self.handle_now_playing_key(false, false)
            }
            _ => false,
        }
    }

    /// Close the popover as a press outside it or Escape does. Clipboard
    /// history opened to paste closes with nothing pasted.
    fn dismiss_quick_settings(&self) {
        self.close_quick_settings();
    }

    /// Use a credential from another certified device without ever putting it
    /// into the visible password editor. A rejected saved secret leaves the
    /// ordinary prompt in place so the person can correct it.
    pub(super) fn try_connect_with_shared_wifi_password(&mut self) -> bool {
        let (Some(chrome), Some(bridge)) = (self.chrome.clone(), self.wifi_secret_bridge.as_ref())
        else {
            return false;
        };
        let scanned = {
            let state = chrome.borrow();
            state
                .quick_settings
                .wifi_password
                .as_ref()
                .and_then(|prompt| {
                    scanned_wifi_security(&state.quick_settings.wifi_networks, &prompt.ssid)
                        .map(|security| (prompt.ssid.clone(), security))
                })
        };
        // A network the last scan did not list has no known mode to match a
        // shared secret against.
        let Some((ssid, security)) = scanned else {
            return false;
        };
        let Ok(Some(password)) = bridge(WifiSecretAction::Lookup {
            ssid: ssid.clone(),
            security,
        }) else {
            return false;
        };
        if let Some(prompt) = chrome.borrow_mut().quick_settings.wifi_password.as_mut() {
            prompt.connecting = true;
            prompt.error = None;
        }
        {
            let mut state = chrome.borrow_mut();
            state.quick_settings.wifi_joining = Some(ssid.clone());
            state.bump();
        }
        self.qs_refresh.kick_wifi_join(ssid, Some(password), true);
        true
    }

    /// Bring the panel to rest at once, as it would be a few frames on: its
    /// readings fed, laid out at the height its content takes, and drawn,
    /// so a capture or a test finds it where a person sees it.
    pub(super) fn settle_quick_settings(&mut self) {
        let doc = automerge::AutoCommit::new();
        let theme = self.layout_theme.clone();
        let mut text = TextRenderer::new();
        for _ in 0..4 {
            self.pump_quick_settings(&doc, &automerge::ROOT);
            let rect = self.quick_settings_rect();
            self.quick_settings.place(rect, &theme);
            if let Some(app) = self.quick_settings.app_mut() {
                app.draw(
                    &mut Scene::new(),
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &ui::presence::PresenceCtx::default(),
                );
            }
        }
    }

    /// Start arranging the panel's controls, as its Edit button does.
    pub(super) fn quick_settings_edit(&mut self) {
        self.quick_settings.mount();
        self.quick_settings.edit();
        self.settle_quick_settings();
    }

    /// Press the pointer on the second arranged control and carry it most
    /// of the way past the third, holding the button there: the panel's own
    /// drag as a hand leaves it part-way, for a capture.
    pub(super) fn drag_quick_settings_control_for_capture(&mut self) {
        let mut rows = Vec::new();
        if let Some(root) = self.quick_settings_accessibility() {
            arranged_rows(&root, &mut rows);
        }
        let Some(row) = rows.get(1).copied() else {
            return;
        };
        let from = Point::new(row.x0 + 20.0, row.center().y);
        let travel = row.height() * 1.6;
        let mut events = vec![
            ui::synthetic_input::cursor_moved(from.x, from.y),
            ui::synthetic_input::mouse_press(),
        ];
        events.extend((1..=4_u8).map(|step| {
            ui::synthetic_input::cursor_moved(from.x, from.y + travel * f64::from(step) / 4.0)
        }));
        let Some(app) = self.quick_settings.app_mut() else {
            return;
        };
        let mut taffy = taffy::TaffyTree::<()>::new();
        let Ok(node) = taffy.new_leaf(taffy::Style::default()) else {
            return;
        };
        let mut document = automerge::AutoCommit::new();
        let modifiers = winit::event::Modifiers::default();
        for event in &events {
            let mut handled = false;
            let mut focus = None;
            app.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &modifiers,
                    state: ui::widget::Edits::lend(&mut document),
                    path: &automerge::ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
        }
        self.settle_quick_settings();
    }

    /// The panel's accessibility tree, in the overlay's coordinates.
    pub(super) fn quick_settings_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.quick_settings.app()?.accessibility()
    }

    /// Whether the open panel names one of its controls `target`.
    pub(super) fn quick_settings_publishes(&self, target: &str) -> bool {
        fn has(node: &ui::a11y::AccessNode, target: &str) -> bool {
            node.action_target.as_deref() == Some(target)
                || node.children.iter().any(|child| has(child, target))
        }
        self.overlay_flags().quick_settings
            && self
                .quick_settings_accessibility()
                .is_some_and(|panel| has(&panel, target))
    }

    /// Where the panel's accessible node at `path` is, for activation by a
    /// press there.
    pub(super) fn quick_settings_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let mut node = self.quick_settings_accessibility()?;
        for index in path {
            node = node.children.get(*index)?.clone();
        }
        node.bounds.map(ui::a11y::AccessBounds::to_rect)
    }

    /// How tall the popover is: its content's height, once laid out.
    pub(super) fn quick_settings_content_height(&self) -> f64 {
        self.quick_settings.content_height().unwrap_or(360.0)
    }

    /// The panel's actions, for this device's catalogue of private windows.
    pub(super) fn quick_settings_device_local_resources(
        &self,
    ) -> Option<Vec<tilo_actions::ResourceSpec>> {
        self.quick_settings.app()?.device_local_resources()
    }

    /// Run one of the panel's actions for this device's catalogue.
    pub(super) fn invoke_quick_settings_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        match self.quick_settings.app_mut() {
            Some(app) => app.invoke_device_local_action(address, arguments),
            None => Err(tilo_actions::DoorError::Unknown(address.clone())),
        }
    }

    /// Press row `row` of the Devices section, at `part`.
    #[cfg(test)]
    pub(super) fn press_device_row_for_tests(
        &mut self,
        row: usize,
        part: tilo_quick_settings::RowPart,
    ) {
        self.handle_qs_device_fabric_press(row, part);
        self.refresh_qs_hosted_rows();
    }

    /// Open Quick Settings at its Devices section and read what it lists.
    #[cfg(test)]
    pub(super) fn open_devices_for_tests(&mut self) {
        if let Some(chrome) = self.chrome.clone() {
            chrome
                .borrow_mut()
                .open_quick_settings_section(tilo_quick_settings::QsSection::Devices);
        }
        self.refresh_qs_hosted_rows();
    }

    /// Run `effect` as the panel would, and the pass that follows it.
    #[cfg(test)]
    pub(super) fn run_quick_settings_effect_for_test(&mut self, effect: QsEffect) {
        self.run_quick_settings_effect(effect);
        let _ = self.apply_quick_settings_readings();
    }
}

/// Where the arranging page's rows of controls shown stand, in order.
fn arranged_rows(node: &ui::a11y::AccessNode, rows: &mut Vec<Rect>) {
    if node.role == ui::a11y::Role::ListItem
        && node
            .action_target
            .as_deref()
            .is_some_and(|target| target.starts_with("qs-edit-"))
        && let Some(bounds) = node.bounds
    {
        rows.push(bounds.to_rect());
    }
    for child in &node.children {
        arranged_rows(child, rows);
    }
}

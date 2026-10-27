//! Quick Settings as actions: the dispatcher the panel and the palette
//! share, the typed parameters of each control, and the adapter that
//! sends the panel's own changes through the same validated path.

use super::QuickSettingsKey;
use crate::shell::chrome_state::Chrome;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use std::rc::Rc;

pub(crate) struct SharedQsDispatcher {
    pub(super) actions:
        std::cell::RefCell<Rc<dyn crate::shell::quick_settings::actions::QsActions>>,
    pub(super) availability:
        std::cell::Cell<crate::shell::quick_settings::actions::QsActionAvailability>,
    pub(super) radio_job:
        std::cell::RefCell<Option<crate::shell::quick_settings::radio::RadioTicket>>,
    pub(super) radio_wake: std::cell::RefCell<Option<crate::session_runtime::Wake>>,
}

impl SharedQsDispatcher {
    pub(crate) fn new(
        actions: Rc<dyn crate::shell::quick_settings::actions::QsActions>,
        availability: crate::shell::quick_settings::actions::QsActionAvailability,
    ) -> Self {
        Self {
            actions: std::cell::RefCell::new(actions),
            availability: std::cell::Cell::new(availability),
            radio_job: std::cell::RefCell::new(None),
            radio_wake: std::cell::RefCell::new(None),
        }
    }

    pub(crate) fn replace(
        &self,
        actions: Rc<dyn crate::shell::quick_settings::actions::QsActions>,
        availability: crate::shell::quick_settings::actions::QsActionAvailability,
    ) {
        if let Some(job) = self.radio_job.borrow().as_ref() {
            job.retire();
        }
        *self.actions.borrow_mut() = actions;
        self.availability.set(availability);
    }

    pub(crate) fn set_radio_wake(&self, wake: crate::session_runtime::Wake) {
        *self.radio_wake.borrow_mut() = Some(wake);
    }

    pub(crate) fn observe_radio_availability(&self, radios: &os_actions::RadioState) {
        let mut availability = self.availability.get();
        availability.wifi = radios.wifi_available;
        availability.airplane_mode = radios.wifi_available || radios.wwan_available;
        self.availability.set(availability);
    }

    pub(crate) fn start_radio(
        &self,
        change: os_actions::RadioChange,
        chrome: &Chrome,
    ) -> Result<Option<crate::shell::quick_settings::radio::RadioTicket>, String> {
        let id = match change {
            os_actions::RadioChange::Wifi(_) => QS_WIFI_ID,
            os_actions::RadioChange::Airplane(_) => QS_AIRPLANE_ID,
        };
        if !self.action_available(id) {
            return Err("radio action is unavailable".into());
        }
        if self.radio_job.borrow().is_some() {
            return Err("a radio change is already pending".into());
        }
        let actions = self.actions.borrow().clone();
        if let Some(work) = actions.prepare_radio_change(change) {
            let job = crate::shell::quick_settings::radio::RadioTicket::start(
                change,
                work,
                self.radio_wake.borrow().clone(),
            )?;
            *self.radio_job.borrow_mut() = Some(job.clone());
            let mut state = chrome.borrow_mut();
            state.quick_settings.radio_change_pending = true;
            state.quick_settings.radio_change = Some(change);
            state.bump();
            Ok(Some(job))
        } else {
            let radios = change.run(actions.as_ref())?;
            crate::shell::quick_settings::radio::apply(change, Ok(radios), chrome);
            Ok(None)
        }
    }

    pub(crate) fn take_radio_completion(
        &self,
    ) -> Option<crate::shell::quick_settings::radio::RadioTicket> {
        let mut pending = self.radio_job.borrow_mut();
        if !pending.as_ref()?.finished() {
            return None;
        }
        let _ = pending.as_ref()?.result()?;
        pending.take()
    }

    /// What the Quick Settings rows are built from; see [`QuickSettingsKey`].
    pub(super) fn catalogue_key(&self) -> QuickSettingsKey {
        QuickSettingsKey {
            backend: Rc::as_ptr(&*self.actions.borrow()).cast::<()>().addr(),
            availability: self.availability.get(),
            radio_pending: self.radio_job.borrow().is_some(),
        }
    }

    pub(super) fn action_available(&self, id: &str) -> bool {
        let available = self.availability.get();
        match id {
            QS_BRIGHTNESS_ID => available.brightness,
            QS_VOLUME_ID => available.volume,
            QS_WIFI_ID => available.wifi,
            QS_POWER_ID => available.power_profile,
            QS_VPN_ID => available.vpn,
            QS_BLUETOOTH_ID => available.bluetooth,
            QS_AIRPLANE_ID => available.airplane_mode,
            _ => false,
        }
    }
}

pub(crate) type SharedQsActions = Rc<SharedQsDispatcher>;

pub(crate) const QS_BRIGHTNESS_ID: &str = "quick-settings.brightness.set";
pub(crate) const QS_VOLUME_ID: &str = "quick-settings.volume.set";
pub(crate) const QS_WIFI_ID: &str = "quick-settings.wifi.set-enabled";
pub(crate) const QS_POWER_ID: &str = "quick-settings.power-profile.set";
pub(crate) const QS_VPN_ID: &str = "quick-settings.vpn.set-enabled";
pub(crate) const QS_BLUETOOTH_ID: &str = "quick-settings.bluetooth.set-enabled";
pub(crate) const QS_AIRPLANE_ID: &str = "quick-settings.airplane-mode.set-enabled";

pub(super) fn quick_settings_params(id: &str) -> Vec<ActionParam> {
    let percent = || {
        ActionParam::required_owned(
            "percent",
            ParamType::Number,
            "Exact percentage from 0 through 100.",
        )
        .with_schema(serde_json::json!({"minimum": 0, "maximum": 100}))
    };
    match id {
        QS_BRIGHTNESS_ID => vec![
            percent(),
            ActionParam::optional_owned(
                "display",
                ParamType::Text,
                "Connector of the monitor to set, e.g. eDP-1 or DP-2, as wl_output names it. \
                 Omitted: the built-in panel, else the first monitor.",
            )
            .with_schema(serde_json::json!({"minLength": 1, "maxLength": 64})),
        ],
        QS_VOLUME_ID => vec![percent()],
        QS_WIFI_ID => vec![ActionParam::required_owned(
            "enabled",
            ParamType::Boolean,
            "Whether the Wi-Fi radio should be enabled.",
        )],
        QS_BLUETOOTH_ID => vec![ActionParam::required_owned(
            "enabled",
            ParamType::Boolean,
            "Whether the Bluetooth radio should be powered.",
        )],
        QS_AIRPLANE_ID => vec![ActionParam::required_owned(
            "enabled",
            ParamType::Boolean,
            "Whether airplane mode should turn the wireless radios off.",
        )],
        QS_POWER_ID => vec![
            ActionParam::required_owned("profile", ParamType::Text, "The host power profile.")
                .with_choices(["saver", "balanced", "performance"]),
        ],
        QS_VPN_ID => vec![
            ActionParam::required_owned(
                "name",
                ParamType::Text,
                "Exact configured VPN connection name.",
            )
            .with_schema(serde_json::json!({"minLength": 1, "maxLength": 255})),
            ActionParam::required_owned(
                "enabled",
                ParamType::Boolean,
                "Whether the named VPN connection should be enabled.",
            ),
        ],
        _ => Vec::new(),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "validated hardware percentages are bounded to 0..=100 before this f64-to-f32 provider conversion"
)]
pub(super) fn validated_percent(args: &ActionArgs) -> f32 {
    args.number("percent").unwrap_or_default() as f32
}

pub(super) fn apply_quick_settings_effect(
    id: &str,
    actions: &dyn crate::shell::quick_settings::actions::QsActions,
    args: &ActionArgs,
) -> Result<(), ActionError> {
    let result = match id {
        QS_BRIGHTNESS_ID => match args.text("display") {
            Some(display) => actions.set_display_brightness(display, validated_percent(args)),
            None => actions.set_brightness(validated_percent(args)),
        },
        QS_VOLUME_ID => actions.set_volume(validated_percent(args)),
        QS_WIFI_ID => actions.set_wifi(args.boolean("enabled").unwrap_or(false)),
        QS_BLUETOOTH_ID => actions.set_bluetooth(args.boolean("enabled").unwrap_or(false)),
        QS_AIRPLANE_ID => actions.set_airplane_mode(args.boolean("enabled").unwrap_or(false)),
        QS_POWER_ID => {
            let profile = match args.text("profile") {
                Some("saver") => crate::shell::quick_settings::actions::PowerProfile::Saver,
                Some("balanced") => crate::shell::quick_settings::actions::PowerProfile::Balanced,
                Some("performance") => {
                    crate::shell::quick_settings::actions::PowerProfile::Performance
                }
                _ => return Err(ActionError::Refused("invalid power profile".into())),
            };
            actions.set_power_profile(profile)
        }
        QS_VPN_ID => actions.set_vpn_connection(
            args.text("name").unwrap_or_default(),
            args.boolean("enabled").unwrap_or(false),
        ),
        _ => return Err(ActionError::Refused("unknown Quick Settings effect".into())),
    };
    result.map_err(ActionError::Refused)
}

/// UI adapter for the callable Quick Settings slice. The root panel and its
/// drill-ins still own presentation state, while these effects bind the
/// same typed parameters and call the same provider path as external action
/// invocation. Operations outside this slice pass through unchanged.
pub(crate) struct ValidatedQsActions<'a> {
    inner: &'a dyn crate::shell::quick_settings::actions::QsActions,
}

impl<'a> ValidatedQsActions<'a> {
    pub(crate) fn new(inner: &'a dyn crate::shell::quick_settings::actions::QsActions) -> Self {
        Self { inner }
    }

    fn dispatch(
        &self,
        id: &str,
        values: impl IntoIterator<Item = (String, crate::shell::search::action_api::ArgValue)>,
    ) -> Result<(), String> {
        let args = ActionArgs::bind(&quick_settings_params(id), values)
            .map_err(|error| error.to_string())?;
        apply_quick_settings_effect(id, self.inner, &args).map_err(|error| error.to_string())
    }
}

impl crate::shell::quick_settings::actions::QsActions for ValidatedQsActions<'_> {
    fn prepare_radio_change(
        &self,
        change: os_actions::RadioChange,
    ) -> Option<os_actions::RadioWork> {
        self.inner.prepare_radio_change(change)
    }

    fn apply_radio_change(
        &self,
        change: os_actions::RadioChange,
    ) -> Result<Option<os_actions::RadioState>, String> {
        self.inner.apply_radio_change(change)
    }

    fn set_brightness(&self, percent: f32) -> Result<(), String> {
        self.dispatch(
            QS_BRIGHTNESS_ID,
            [(
                "percent".into(),
                crate::shell::search::action_api::ArgValue::Number(f64::from(percent)),
            )],
        )
    }

    fn set_display_brightness(&self, connector: &str, percent: f32) -> Result<(), String> {
        self.dispatch(
            QS_BRIGHTNESS_ID,
            [
                (
                    "percent".into(),
                    crate::shell::search::action_api::ArgValue::Number(f64::from(percent)),
                ),
                (
                    "display".into(),
                    crate::shell::search::action_api::ArgValue::Text(connector.to_owned()),
                ),
            ],
        )
    }

    fn set_volume(&self, percent: f32) -> Result<(), String> {
        self.dispatch(
            QS_VOLUME_ID,
            [(
                "percent".into(),
                crate::shell::search::action_api::ArgValue::Number(f64::from(percent)),
            )],
        )
    }

    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        self.dispatch(
            QS_WIFI_ID,
            [(
                "enabled".into(),
                crate::shell::search::action_api::ArgValue::Boolean(enabled),
            )],
        )
    }

    fn set_power_profile(
        &self,
        profile: crate::shell::quick_settings::actions::PowerProfile,
    ) -> Result<(), String> {
        let profile = match profile {
            crate::shell::quick_settings::actions::PowerProfile::Saver => "saver",
            crate::shell::quick_settings::actions::PowerProfile::Balanced => "balanced",
            crate::shell::quick_settings::actions::PowerProfile::Performance => "performance",
        };
        self.dispatch(
            QS_POWER_ID,
            [(
                "profile".into(),
                crate::shell::search::action_api::ArgValue::Text(profile.into()),
            )],
        )
    }

    fn set_vpn_connection(&self, name: &str, enabled: bool) -> Result<(), String> {
        self.dispatch(
            QS_VPN_ID,
            [
                (
                    "name".into(),
                    crate::shell::search::action_api::ArgValue::Text(name.into()),
                ),
                (
                    "enabled".into(),
                    crate::shell::search::action_api::ArgValue::Boolean(enabled),
                ),
            ],
        )
    }

    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        self.inner.connect_wifi(ssid, password)
    }

    fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
        self.dispatch(
            QS_BLUETOOTH_ID,
            [(
                "enabled".into(),
                crate::shell::search::action_api::ArgValue::Boolean(enabled),
            )],
        )
    }

    fn connect_bluetooth(&self, address: &str) -> Result<(), String> {
        self.inner.connect_bluetooth(address)
    }

    fn disconnect_bluetooth(&self, address: &str) -> Result<(), String> {
        self.inner.disconnect_bluetooth(address)
    }

    fn set_wired(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_wired(enabled)
    }

    fn set_airplane_mode(&self, enabled: bool) -> Result<(), String> {
        self.dispatch(
            QS_AIRPLANE_ID,
            [(
                "enabled".into(),
                crate::shell::search::action_api::ArgValue::Boolean(enabled),
            )],
        )
    }

    fn set_hotspot(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_hotspot(enabled)
    }

    fn set_mobile_broadband(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_mobile_broadband(enabled)
    }

    fn toggle_mute(&self) -> Result<(), String> {
        self.inner.toggle_mute()
    }

    fn set_mute(&self, muted: bool) -> Result<(), String> {
        self.inner.set_mute(muted)
    }

    fn brightness(&self) -> Result<f32, String> {
        self.inner.brightness()
    }

    fn keyboard_backlight(&self) -> Result<os_actions::KeyboardBacklight, String> {
        self.inner.keyboard_backlight()
    }

    fn set_keyboard_backlight(&self, level: u32) -> Result<(), String> {
        self.inner.set_keyboard_backlight(level)
    }
}

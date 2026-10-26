//! Power pane — battery status, the active power profile, and session-idle
//! preferences.
//!
//! Every surface here is real or honestly absent:
//! - **Battery** is read from `/sys/class/power_supply` (`capacity`,
//!   `status`, `cycle_count`) and the AC adapter's `online` flag, so the card
//!   reflects the machine's real charge. A device with no battery shows an
//!   honest "running on AC power" state rather than a fabricated level.
//! - **Power profile** (Saver / Balanced / Performance) and the **charge
//!   cap** are this device's, changed by [`PowerEffect`] jobs through
//!   `powerprofilesctl` and the battery driver. Unavailable profiles are
//!   never presented as working.
//! - **Automatic suspend**, the idle timeouts and the lid, power-key and
//!   critical-battery actions are the device's `power` settings, which the
//!   compositor's idle layer reads. There is one suspend timer per power
//!   source (on battery, plugged in); only where the stored profile timers
//!   truly differ is the selected profile's timer the one, shown with the
//!   profile. While "Suspend when idle" is off, every timer that depends on
//!   it is disabled and says why.
//! - **Password after sleep** lives beside the lock timer because it is the
//!   other way the lock screen appears: off, a machine that went to sleep
//!   unlocked comes back to what its owner left on screen, while a lock from
//!   the idle timer or the lock command is kept as it was.

mod history_chart;

use std::sync::{Arc, Mutex};

use os_actions::{BatteryInfo, ChargeCapBackend, ChargeCapCapability, PowerProfile};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::tilo_resource;
use ui::widget::{AppModel, ModelPath, Widget};

use history_chart::{HistoryChart, Span};

use crate::device_read::Reading;
use crate::effects::effect;
use crate::host::{Followed, PaneCx};
use crate::i18n::{english, t, t_args};
use crate::prefs::{PowerActionPref, PowerPrefs};
use crate::resource::Settings;
use crate::row::Row;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Item, SettingsSection, block, error_caption, note, row, rows, section, value_row,
};
use ui::i18n::ArgValue;
use ui::lock::MutexRecover as _;

crate::effects::effect_family!(PowerEffect => answer);

crate::area::pane_area! {
    PowerArea;
    fn warm(&self) {
        PowerArea::warm(self);
    }
}

/// The window's [`PowerArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&PowerArea> {
    app.areas().find()
}

/// Idle-suspend timeout options, `(label, minutes)`. `0` = never.
const SUSPEND_OPTIONS: &[(&str, u32)] = &[
    ("5 minutes", 5),
    ("10 minutes", 10),
    ("15 minutes", 15),
    ("30 minutes", 30),
    ("45 minutes", 45),
    ("1 hour", 60),
    ("Never", 0),
];

fn duration_ftl(english: &str) -> &'static str {
    match english {
        "1 minute" => "power-1-minute",
        "5 minutes" => "power-5-minutes",
        "10 minutes" => "power-10-minutes",
        "30 minutes" => "power-30-minutes",
        "45 minutes" => "power-45-minutes",
        "1 hour" => "power-1-hour",
        "Never" => "power-never",
        _ => "power-15-minutes",
    }
}

fn duration_shown(english: &str) -> String {
    t(duration_ftl(english))
}

/// The minutes a suspend option shown as `label` stands for.
fn suspend_minutes_of(label: &str) -> u32 {
    SUSPEND_OPTIONS
        .iter()
        .find(|(english, _)| *english == label || duration_shown(english) == label)
        .map_or(
            settings_store::power::DEFAULT_SUSPEND_MINUTES,
            |(_, minutes)| *minutes,
        )
}

fn suspend_label(minutes: u32) -> String {
    let english = SUSPEND_OPTIONS
        .iter()
        .find(|(_, m)| *m == minutes)
        .map_or("15 minutes", |(l, _)| *l);
    duration_shown(english)
}

/// The field holding `profile`'s own suspend timer.
fn profile_suspend_field(profile: PowerProfile) -> tilo_resource::Path<PowerPrefs, Option<u32>> {
    let paths = PowerPrefs::paths();
    match profile {
        PowerProfile::Saver => paths.saver_suspend_minutes,
        PowerProfile::Balanced => paths.balanced_suspend_minutes,
        PowerProfile::Performance => paths.performance_suspend_minutes,
    }
}

/// The action options offered for the lid / power-key / critical-battery
/// dropdowns, in display order.
const ACTION_OPTIONS: &[PowerActionPref] = &[
    PowerActionPref::Suspend,
    PowerActionPref::Lock,
    PowerActionPref::Ignore,
];

fn action_ftl(pref: PowerActionPref) -> &'static str {
    match pref {
        PowerActionPref::Suspend => "power-action-suspend",
        PowerActionPref::Lock => "power-action-lock",
        PowerActionPref::Ignore => "power-action-ignore",
    }
}

fn action_shown(pref: PowerActionPref) -> String {
    t(action_ftl(pref))
}

/// The power-profile options, as `(display label, profile)`. The label is what
/// the dropdown shows and stores; the profile is what the dispatcher applies.
const PROFILES: &[(&str, PowerProfile)] = &[
    ("Power Saver", PowerProfile::Saver),
    ("Balanced", PowerProfile::Balanced),
    ("Performance", PowerProfile::Performance),
];

fn profile_ftl(english: &str) -> &'static str {
    match english {
        "Power Saver" => "power-profile-saver",
        "Performance" => "power-profile-performance",
        _ => "power-profile-balanced",
    }
}

fn profile_shown(english: &str) -> String {
    t(profile_ftl(english))
}

fn profile_label(p: PowerProfile) -> String {
    let english = PROFILES
        .iter()
        .find(|(_, pp)| *pp == p)
        .map_or("Balanced", |(l, _)| *l);
    profile_shown(english)
}

pub(crate) fn profile_from_label(label: &str) -> Option<PowerProfile> {
    PROFILES
        .iter()
        .find(|(english, _)| *english == label || profile_shown(english) == label)
        .map(|(_, p)| *p)
}

/// The screen-blank timeout options, as `(display label, minutes)`. `0`
/// minutes means "Never".
const BLANK_OPTIONS: &[(&str, u32)] = &[
    ("1 minute", 1),
    ("5 minutes", 5),
    ("10 minutes", 10),
    ("15 minutes", 15),
    ("30 minutes", 30),
    ("Never", 0),
];

fn blank_label(minutes: u32) -> String {
    let english = BLANK_OPTIONS
        .iter()
        .find(|(_, m)| *m == minutes)
        .map_or("10 minutes", |(l, _)| *l);
    duration_shown(english)
}

/// Idle-lock timeout options. `0` is the explicit "Never" value persisted
/// by Settings; manual locking and pre-suspend locking are unaffected.
const LOCK_OPTIONS: &[(&str, u32)] = &[
    ("1 minute", 1),
    ("5 minutes", 5),
    ("10 minutes", 10),
    ("15 minutes", 15),
    ("30 minutes", 30),
    ("Never", 0),
];

fn lock_label(minutes: u32) -> String {
    let english = LOCK_OPTIONS
        .iter()
        .find(|(_, value)| *value == minutes)
        .map_or("5 minutes", |(label, _)| *label);
    duration_shown(english)
}

/// What the battery is doing, and how long that lasts: "On battery, 3 hr
/// 20 min left", "Charging, full in 45 min", "Fully charged".
fn battery_state_shown(battery: &BatteryInfo) -> String {
    if let Some(left) = battery.time_to_empty() {
        return t_args(
            "power-state-left",
            &[("time", ArgValue::from(ui::format::duration(left)))],
        );
    }
    if let Some(to_full) = battery.time_to_full() {
        return t_args(
            "power-state-to-full",
            &[("time", ArgValue::from(ui::format::duration(to_full)))],
        );
    }
    t(match battery.status.as_str() {
        "Full" => "power-status-full",
        "Charging" => "power-state-charging",
        "Not charging" => "power-state-plugged",
        _ if battery.on_ac => "power-state-plugged",
        _ => "power-state-battery",
    })
}

fn health_explanation_shown(battery: &BatteryInfo) -> String {
    match battery.health_percent() {
        None => t("power-health-none"),
        Some(90..=100) => t("power-health-good"),
        Some(80..=89) => t("power-health-aged"),
        Some(60..=79) => t("power-health-reduced"),
        Some(_) => t("power-health-severe"),
    }
}

/// "44.6 Wh of 70 Wh": what the battery holds when full against when new.
fn capacity_shown(battery: &BatteryInfo) -> Option<String> {
    let (full, design) = battery.full_and_design_wh()?;
    Some(t_args(
        "power-capacity-of",
        &[
            ("full", ArgValue::from(watt_hours_shown(full))),
            ("design", ArgValue::from(watt_hours_shown(design))),
        ],
    ))
}

/// "44.6 Wh", "70 Wh": a tenth where there is one.
fn watt_hours_shown(wh: f64) -> String {
    let tenths = (wh * 10.0).round();
    let digits = usize::from(tenths % 10.0 != 0.0);
    t_args(
        "power-watt-hours",
        &[(
            "amount",
            ArgValue::from(ui::format::decimal(tenths / 10.0, digits)),
        )],
    )
}

/// This device's power hardware, as the pane and the effects reach it.
pub trait PowerControl: Send + Sync {
    /// The battery now.
    fn battery(&self) -> BatteryInfo;
    /// Every power supply, for a device with more than one.
    fn devices(&self) -> Vec<os_actions::PowerDevice>;
    /// Recent charge, written by the shell's monitor.
    fn history(&self) -> os_actions::BatteryHistory;
    /// Now, in minutes since the Unix epoch: where the history chart ends.
    fn now_minute(&self) -> u64 {
        tilo_clock::unix_now() / 60
    }
    /// The active profile and the ones offered, or why they are unknown.
    fn profiles(
        &self,
    ) -> (
        Result<PowerProfile, String>,
        Result<Vec<PowerProfile>, String>,
    );
    /// # Errors
    ///
    /// Why the profile was not changed.
    fn set_profile(&self, profile: PowerProfile) -> Result<(), String>;
    /// The battery's charge cap, where the driver offers one.
    fn charge_cap(&self) -> Option<ChargeCapCapability>;
    /// # Errors
    ///
    /// Why the cap was not applied.
    fn set_charge_limit(&self, percent: u32) -> Result<ChargeCapCapability, String>;
}

/// The device's power hardware.
pub struct SystemPower;

impl PowerControl for SystemPower {
    fn battery(&self) -> BatteryInfo {
        crate::device_read::off_ui_thread();
        BatteryInfo::from_system()
    }

    fn devices(&self) -> Vec<os_actions::PowerDevice> {
        crate::device_read::off_ui_thread();
        os_actions::power_devices()
    }

    fn history(&self) -> os_actions::BatteryHistory {
        crate::device_read::off_ui_thread();
        // The shell-owned UPower monitor is the single history writer.
        os_actions::load_battery_history()
    }

    fn profiles(
        &self,
    ) -> (
        Result<PowerProfile, String>,
        Result<Vec<PowerProfile>, String>,
    ) {
        crate::device_read::off_ui_thread();
        (
            os_actions::get_power_profile(),
            os_actions::available_power_profiles(),
        )
    }

    fn set_profile(&self, profile: PowerProfile) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::set_power_profile(profile)
    }

    fn charge_cap(&self) -> Option<ChargeCapCapability> {
        crate::device_read::off_ui_thread();
        os_actions::charge_capability()
    }

    fn set_charge_limit(&self, percent: u32) -> Result<ChargeCapCapability, String> {
        crate::device_read::off_ui_thread();
        os_actions::set_charge_limit_checked(percent)
    }
}

/// The power hardware a capture shows: a laptop battery on Balanced, which
/// the effects change in memory only.
pub struct SamplePower {
    profile: Mutex<PowerProfile>,
    cap: Mutex<ChargeCapCapability>,
}

impl Default for SamplePower {
    fn default() -> Self {
        Self {
            profile: Mutex::new(PowerProfile::Balanced),
            cap: Mutex::new(ChargeCapCapability {
                backend: ChargeCapBackend::PowerSupply,
                current: 80,
                choices: vec![60, 70, 80, 90, 100],
                supports_start_threshold: false,
            }),
        }
    }
}

impl PowerControl for SamplePower {
    fn battery(&self) -> BatteryInfo {
        crate::device_read::off_ui_thread();
        BatteryInfo {
            present: true,
            capacity: 72,
            status: "Discharging".into(),
            cycle_count: Some(148),
            on_ac: false,
            full_capacity: Some(40_000_000),
            design_capacity: Some(50_000_000),
            unit: os_actions::CapacityUnit::Energy,
            charge_limit: Some(self.cap.lock_recover().current),
            remaining: Some(28_800_000),
            rate: Some(8_640_000),
        }
    }

    fn devices(&self) -> Vec<os_actions::PowerDevice> {
        crate::device_read::off_ui_thread();
        Vec::new()
    }

    fn history(&self) -> os_actions::BatteryHistory {
        crate::device_read::off_ui_thread();
        sample_history(self.now_minute())
    }

    fn now_minute(&self) -> u64 {
        // 2023-03-15 13:05 UTC, the captures' clock.
        1_678_885_500 / 60
    }

    fn profiles(
        &self,
    ) -> (
        Result<PowerProfile, String>,
        Result<Vec<PowerProfile>, String>,
    ) {
        crate::device_read::off_ui_thread();
        (
            Ok(*self.profile.lock_recover()),
            Ok(PROFILES.iter().map(|(_, profile)| *profile).collect()),
        )
    }

    fn set_profile(&self, profile: PowerProfile) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        *self.profile.lock_recover() = profile;
        Ok(())
    }

    fn charge_cap(&self) -> Option<ChargeCapCapability> {
        crate::device_read::off_ui_thread();
        Some(self.cap.lock_recover().clone())
    }

    fn set_charge_limit(&self, percent: u32) -> Result<ChargeCapCapability, String> {
        crate::device_read::off_ui_thread();
        let mut cap = self.cap.lock_recover();
        cap.current = percent;
        Ok(cap.clone())
    }
}

/// A week of a laptop's charge for a capture: working days on battery,
/// charged to its 80 % limit at lunch every other day and in the evening,
/// asleep overnight (no points), and at 72 % now, as the capture's battery
/// reads.
fn sample_history(now: u64) -> os_actions::BatteryHistory {
    const LIMIT_PERCENT: u8 = 80;
    let start = now - 7 * 24 * 60;
    let mut points = Vec::new();
    let mut capacity: f64 = 70.0;
    // On the quarter hour, from the start of the week on.
    let mut minute = start.div_ceil(15) * 15;
    while minute <= now {
        // The captures' clock is an hour ahead of UTC.
        let hour = (minute / 60 + 1) % 24;
        let day = (minute - start) / (24 * 60);
        if hour < 8 {
            capacity = (capacity - 0.06).max(3.0);
        } else {
            let on_ac = (18..20).contains(&hour) || (day.is_multiple_of(2) && hour == 12);
            capacity = if on_ac {
                (capacity + 9.0).min(f64::from(LIMIT_PERCENT))
            } else {
                (capacity - 1.25).max(3.0)
            };
            points.push((minute, capacity, on_ac));
        }
        minute += 15;
    }
    // Meet the battery's reading now.
    let shift = points
        .last()
        .map_or(0.0, |(_, last, _)| 72.0 - last.round());
    let mut history = os_actions::BatteryHistory::default();
    for (minute, capacity, on_ac) in points {
        history.record(os_actions::BatteryHistoryPoint::sampled(
            minute,
            (capacity + shift)
                .round()
                .clamp(3.0, f64::from(LIMIT_PERCENT)) as u32,
            Some(80),
            on_ac,
        ));
    }
    history
}

/// What the device's power hardware reports. `powerprofilesctl` asks
/// power-profiles-daemon over the system bus, so this is read off the UI
/// thread ([`Reading`]) when the pane first opens.
#[derive(Clone)]
struct PowerState {
    battery: BatteryInfo,
    devices: Vec<os_actions::PowerDevice>,
    history: os_actions::BatteryHistory,
    /// When it was read, in minutes since the Unix epoch.
    now_minute: u64,
    profile: PowerProfile,
    available_profiles: Vec<PowerProfile>,
    /// Why the profiles could not be read.
    profiles_unavailable: Option<String>,
    /// The active profile could not be read; `profile` is then only the
    /// first choice offered, never shown as the active one.
    profile_unread: bool,
    charge_cap: Option<ChargeCapCapability>,
}

impl PowerState {
    fn read(control: &dyn PowerControl) -> Self {
        let (profile_result, available_result) = control.profiles();
        let mut available_profiles = available_result.clone().unwrap_or_default();
        let profile = profile_result.clone().unwrap_or_else(|_| {
            available_profiles
                .first()
                .copied()
                .unwrap_or(PowerProfile::Balanced)
        });
        if profile_result.is_ok() && !available_profiles.contains(&profile) {
            available_profiles.push(profile);
        }
        let profile_unread = profile_result.is_err();
        let profiles_unavailable = match (profile_result.err(), available_result.err()) {
            (None, None) => None,
            (profile_error, capability_error) => {
                let mut details = Vec::new();
                if let Some(error) = profile_error {
                    details.push(error);
                }
                if let Some(error) = capability_error {
                    details.push(format!("capabilities: {error}"));
                }
                // What failed is for the log; the person learns only that
                // profiles cannot be chosen here.
                tracing::warn!(details = %details.join("; "), "power profiles are unavailable");
                Some(t("power-profile-unavailable"))
            }
        };
        Self {
            battery: control.battery(),
            devices: control.devices(),
            history: control.history(),
            now_minute: control.now_minute(),
            profile,
            available_profiles,
            profiles_unavailable,
            profile_unread,
            charge_cap: control.charge_cap(),
        }
    }
}

/// What the last profile and charge-cap changes said.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Notices {
    profile: Option<String>,
    charge: Option<String>,
}

/// What the power area keeps for the window: the hardware, what it last
/// reported, and what the last changes said.
pub struct PowerArea {
    control: Arc<dyn PowerControl>,
    /// What the hardware last reported, and what the changes it took made
    /// of that.
    state: Reading<PowerState>,
    notices: LocalState<Notices>,
    /// Which span the charge chart shows, kept while the window is open.
    span: LocalState<usize>,
}

impl PowerArea {
    /// Read the hardware in the background unless it has been, so the
    /// page opens on what it last reported.
    pub(crate) fn warm(&self) {
        if self.state.get().is_none() {
            self.state.refresh();
        }
    }

    /// The area over this device's power hardware, or a stand-in for a
    /// capture. Nothing is read until the pane or an effect needs it.
    pub fn new(cx: &PaneCx) -> Self {
        let control: Arc<dyn PowerControl> = if cx.reaches_devices() {
            Arc::new(SystemPower)
        } else {
            Arc::new(SamplePower::default())
        };
        Self::over(control)
    }

    fn over(control: Arc<dyn PowerControl>) -> Self {
        let read = Arc::clone(&control);
        Self {
            control,
            state: Reading::new(move || PowerState::read(read.as_ref())),
            notices: LocalState::new(Notices::default()),
            span: LocalState::new(0),
        }
    }

    /// Take in what a change the hardware took made of it. Before the
    /// first reading lands, the hardware is read again instead, so a read
    /// that started before the change cannot show what it replaced.
    fn changed(&self, change: impl FnOnce(&mut PowerState)) {
        if self.state.get().is_some() {
            self.state.update(change);
        } else {
            self.state.refresh();
        }
    }
}

/// The power profile and charge cap of this device, and its idle and button
/// preferences.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum PowerEffect {
    /// Set this device's power profile.
    #[effect(job, idempotent)]
    SetPowerProfile {
        #[param("The profile", choices = english(PROFILES))]
        profile: String,
    },
    /// Set how full this device's battery charges.
    #[effect(job, idempotent)]
    SetChargeLimit {
        #[param("The cap, in percent")]
        percent: u32,
    },
}

/// Answer `effect`: hardware changes run off the UI thread. The idle and
/// button preferences are field edits of the `power` section.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: PowerEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let not = |value: &str| format!("`{value}` is not offered");
    match effect {
        PowerEffect::SetPowerProfile { profile } => {
            let profile = profile_from_label(&profile).ok_or_else(|| not(&profile))?;
            let area = crate::panes::power::area(app).ok_or_else(crate::area::missing)?;
            let unsupported =
                move || format!("{} is not supported by this device", profile_label(profile));
            // What the hardware offers, as last read; the job asks it when
            // the pane has not read it yet.
            let offered = area
                .state
                .get()
                .map(|state| state.available_profiles.contains(&profile));
            if offered == Some(false) {
                return Err(unsupported());
            }
            let control = Arc::clone(&area.control);
            Ok(crate::device_read::job(
                cx,
                move || {
                    if offered.is_none()
                        && !control
                            .profiles()
                            .1
                            .is_ok_and(|offered| offered.contains(&profile))
                    {
                        return Err(unsupported());
                    }
                    control.set_profile(profile).map(|()| profile)
                },
                |app: &mut SettingsApp, outcome: Result<PowerProfile, String>, _| {
                    let area = crate::panes::power::area(app).ok_or_else(crate::area::missing)?;
                    let mut notices = area.notices.get();
                    let answer = match outcome {
                        Ok(profile) => {
                            area.changed(|state| {
                                state.profile = profile;
                                state.profile_unread = false;
                            });
                            notices.profile =
                                Some(format!("{} is now active.", profile_label(profile)));
                            Ok(Effected::done())
                        }
                        Err(error) => {
                            let error = format!("Could not change the power profile: {error}");
                            notices.profile = Some(error.clone());
                            Err(error)
                        }
                    };
                    area.notices.set(notices);
                    answer
                },
            ))
        }
        PowerEffect::SetChargeLimit { percent } => {
            let area = crate::panes::power::area(app).ok_or_else(crate::area::missing)?;
            let offers = move |cap: Option<&ChargeCapCapability>| {
                cap.is_some_and(|cap| cap.choices.contains(&percent))
            };
            let not_offered = move || format!("this battery offers no {percent}% charge cap");
            // What the battery offers, as last read; the job asks it when
            // the pane has not read it yet.
            let offered = area
                .state
                .get()
                .map(|state| offers(state.charge_cap.as_ref()));
            if offered == Some(false) {
                return Err(not_offered());
            }
            let control = Arc::clone(&area.control);
            Ok(crate::device_read::job(
                cx,
                move || {
                    if offered.is_none() && !offers(control.charge_cap().as_ref()) {
                        return Err(not_offered());
                    }
                    control.set_charge_limit(percent).map(|cap| (percent, cap))
                },
                |app: &mut SettingsApp, outcome: Result<(u32, ChargeCapCapability), String>, _| {
                    let area = crate::panes::power::area(app).ok_or_else(crate::area::missing)?;
                    let mut notices = area.notices.get();
                    let answer = match outcome {
                        Ok((percent, cap)) => {
                            area.changed(|state| {
                                state.battery.charge_limit = Some(percent);
                                state.charge_cap = Some(cap);
                            });
                            notices.charge =
                                Some(format!("Hardware confirmed a {percent}% charge cap."));
                            Ok(Effected::done())
                        }
                        Err(error) => {
                            let error = format!("Charge cap was not applied: {error}");
                            notices.charge = Some(error.clone());
                            Err(error)
                        }
                    };
                    area.notices.set(notices);
                    answer
                },
            ))
        }
    }
}

/// Power pane widget.
pub struct PowerPane {
    cx: PaneCx,
    prefs: Followed<PowerPrefs>,
    /// What the hardware reports, kept current by the area.
    hardware: Reading<PowerState>,
    /// The revision of `hardware` shown.
    hardware_seen: u64,
    /// What the hardware reported; `None` while it is first read.
    state: Option<PowerState>,
    notices_state: LocalState<Notices>,
    notices: Notices,
    /// Which span the charge chart shows: an index into [`Span::ALL`].
    span: LocalState<usize>,
    span_shown: usize,
    composed: ui::widget::Composed,
}

impl PowerPane {
    /// The pane over `area`'s hardware and `cx`'s power settings. The
    /// hardware is read off the UI thread the first time; until then its
    /// cards say it is being read.
    pub fn new(cx: PaneCx, area: &PowerArea) -> Self {
        let hardware = area.state.clone();
        if hardware.get().is_none() {
            hardware.refresh();
        }
        Self {
            prefs: cx.follow::<PowerPrefs>(),
            cx,
            hardware_seen: hardware.revision(),
            state: hardware.get(),
            hardware,
            notices: area.notices.get(),
            notices_state: area.notices.clone(),
            span_shown: area.span.get(),
            span: area.span.clone(),
            composed: ui::widget::Composed::default(),
        }
    }

    /// The battery as last read; `None` while the hardware is first read.
    pub fn battery(&self) -> Option<&BatteryInfo> {
        self.state.as_ref().map(|state| &state.battery)
    }

    pub fn prefs(&self) -> &PowerPrefs {
        self.prefs.get()
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut cards = match &self.state {
            Some(state) => {
                let mut cards = self.battery_card(state);
                cards.extend(Self::devices_card(state));
                cards.push(self.profile_card(state));
                cards
            }
            None => vec![section(
                &t("power-battery-title"),
                rows(vec![note(&t("power-reading"))]),
            )],
        };
        cards.push(self.idle_card());
        cards.push(self.actions_card());
        crate::widgets::page(cards)
    }

    fn battery_card(&self, state: &PowerState) -> Vec<SettingsSection> {
        if !state.battery.present {
            return vec![section(
                &t("power-source-title"),
                rows(vec![
                    value_row(&t("power-source"), &t("power-source-ac")),
                    note(&t("power-source-caption")),
                ]),
            )];
        }
        let mut battery_rows: Vec<Item> = vec![
            block(self.battery_head(&state.battery)),
            block(Box::new(HistoryChart::new(
                &state.history.points,
                Span::ALL.get(self.span_shown).copied().unwrap_or(Span::Day),
                state.now_minute,
                i64::from(ui::clock::local_now().offset().local_minus_utc()),
            ))),
        ];
        if let Some(cycles) = state.battery.cycle_count {
            battery_rows.push(value_row(&t("power-cycles"), &cycles.to_string()));
        }
        if let Some(health) = state.battery.health_percent() {
            battery_rows.push(value_row(&t("power-health"), &format!("{health}%")));
            if let Some(capacity) = capacity_shown(&state.battery) {
                battery_rows.push(value_row(&t("power-capacity-full"), &capacity));
            }
            battery_rows.push(note(&health_explanation_shown(&state.battery)));
        } else {
            battery_rows.push(value_row(&t("power-health"), &t("power-health-unknown")));
        }
        if let Some(capability) = &state.charge_cap {
            let choices: Vec<String> = capability
                .choices
                .iter()
                .map(|limit| format!("{limit}%"))
                .collect();
            let current = format!("{}%", capability.current);
            battery_rows.push(
                self.cx
                    .row(t("power-charge-cap"))
                    .id("power.charge_limit")
                    .choice_effect(
                        choices,
                        &LocalState::new(current),
                        effect(PowerEffect::SET_CHARGE_LIMIT).map(|shown: String| {
                            shown
                                .trim_end_matches('%')
                                .parse::<u32>()
                                .unwrap_or_default()
                        }),
                    ),
            );
            let detail = match capability.backend {
                ChargeCapBackend::ConservationMode => t("power-charge-cap-conservation"),
                ChargeCapBackend::HuaweiWmi => t("power-charge-cap-huawei"),
                ChargeCapBackend::PowerSupply if capability.supports_start_threshold => {
                    t("power-charge-cap-start-stop")
                }
                ChargeCapBackend::PowerSupply => t("power-charge-cap-stop"),
            };
            battery_rows.push(note(&detail));
        } else {
            battery_rows.push(value_row(
                &t("power-charge-cap"),
                &t("power-charge-cap-unsupported"),
            ));
        }
        if let Some(notice) = &self.notices.charge {
            battery_rows.push(note(notice));
        }
        vec![section(&t("power-battery-title"), rows(battery_rows))]
    }

    /// The charge in large type, what the battery is doing and for how long,
    /// and the chart's span on the trailing edge.
    fn battery_head(&self, battery: &BatteryInfo) -> Box<dyn Widget> {
        let charge = ui::widget::Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .push(Box::new(
                ui::widget::Text::new(&ui::format::percent(f64::from(battery.capacity) / 100.0))
                    .with_role(ui::TextRole::LargeTitle),
            ))
            .push(Box::new(
                ui::widget::Text::new(&battery_state_shown(battery)).dimmed(),
            ));
        let span = ui::widget::ToggleGroup::new(
            Span::ALL
                .iter()
                .map(|span| ui::widget::Segment::new(span.label()))
                .collect(),
            self.span_shown,
        )
        .with_id("power.history.span")
        .with_accessible_name(t("power-history-span"))
        .bind_selection(&self.span);
        Box::new(
            ui::widget::Row::new()
                .no_wrap()
                .with_align_items(taffy::AlignItems::End)
                .push(Box::new(
                    ui::widget::Container::new()
                        .with_no_padding()
                        .with_flex_grow()
                        .with_child(Box::new(charge)),
                ))
                .push(Box::new(span)),
        )
    }

    fn profile_card(&self, state: &PowerState) -> SettingsSection {
        let profile_opts: Vec<String> = state
            .available_profiles
            .iter()
            .map(|profile| profile_label(*profile))
            .collect();
        let current = if state.profile_unread {
            t("power-profile-unread")
        } else {
            profile_label(state.profile)
        };
        let mut profile_rows = if profile_opts.is_empty() {
            vec![value_row(
                &t("power-profile"),
                &t("power-profile-unavailable"),
            )]
        } else {
            vec![
                self.cx
                    .row(t("power-profile"))
                    .id("power.profile")
                    .choice_effect(
                        profile_opts,
                        &LocalState::new(current),
                        effect(PowerEffect::SET_POWER_PROFILE).map(|shown: String| {
                            profile_from_label(&shown)
                                .and_then(|profile| {
                                    PROFILES.iter().find(|(_, known)| *known == profile)
                                })
                                .map_or(shown, |(english, _)| (*english).to_owned())
                        }),
                    ),
            ]
        };
        // Truly different profile timers: the selected profile's is the one,
        // and it is shown here, with the profile.
        if self.prefs().timers().profiles_differ() && !state.profile_unread {
            let field = profile_suspend_field(state.profile);
            profile_rows.push(
                self.suspend_row(
                    self.cx
                        .row(t("power-suspend-after"))
                        .id("power.profile_suspend"),
                    |row| {
                        row.of(&self.prefs, field).among(
                            SUSPEND_OPTIONS.iter().map(|(_, minutes)| Some(*minutes)),
                            |minutes: &Option<u32>| suspend_label(minutes.unwrap_or_default()),
                        )
                    },
                ),
            );
        }
        profile_rows.push(note(&t("power-profile-caption")));
        if let Some(notice) = &self.notices.profile {
            profile_rows.push(note(notice));
        } else if let Some(unavailable) = &state.profiles_unavailable {
            profile_rows.push(if state.profile_unread {
                row(error_caption(unavailable))
            } else {
                note(unavailable)
            });
        }
        section(&t("power-profile-title"), rows(profile_rows))
    }

    /// `row` choosing the timeout at `field` among `options`, each named
    /// as `label` names it.
    fn timeout<'a>(
        &'a self,
        row: Row<'a>,
        options: &'static [(&'static str, u32)],
        label: fn(u32) -> String,
        field: tilo_resource::Path<PowerPrefs, u32>,
    ) -> Item {
        row.of(&self.prefs, field).among(
            options.iter().map(|(_, minutes)| *minutes),
            move |minutes: &u32| label(*minutes),
        )
    }

    /// `row`, a suspend timer: disabled, saying why, while "Suspend when
    /// idle" is off; `build` makes its control.
    fn suspend_row<'a>(&'a self, row: Row<'a>, build: impl FnOnce(Row<'a>) -> Item) -> Item {
        let on = self.prefs().auto_suspend;
        let row = row.enabled(on);
        build(if on {
            row
        } else {
            row.described(t("power-suspend-needs-idle"))
        })
    }

    /// `row` choosing a suspend timer through `verb`, now at `minutes`.
    fn suspend_timer<'a>(
        &'a self,
        row: Row<'a>,
        minutes: u32,
        verb: impl ui::tilo_resource::Binds<PowerPrefs, String>,
    ) -> Item {
        let options: Vec<String> = SUSPEND_OPTIONS
            .iter()
            .map(|(_, minutes)| suspend_label(*minutes))
            .collect();
        self.suspend_row(row, |row| {
            row.choice_verb(options, &LocalState::new(suspend_label(minutes)), verb)
        })
    }

    /// The suspend timers: one per power source this device has, unless the
    /// profile timers truly differ (the selected profile's is then shown
    /// with the profile).
    fn suspend_timers(&self) -> Vec<Item> {
        let timers = self.prefs().timers();
        if timers.profiles_differ() {
            return vec![note(&t("power-suspend-in-profile"))];
        }
        let has_battery = self
            .state
            .as_ref()
            .is_some_and(|state| state.battery.present);
        if !has_battery {
            return vec![self.suspend_timer(
                self.cx.row(t("power-suspend-after")).id("power.suspend"),
                timers.plugged_in(),
                PowerPrefs::SUSPEND_PLUGGED_IN.map(|label: String| suspend_minutes_of(&label)),
            )];
        }
        vec![
            self.suspend_timer(
                self.cx
                    .row(t("power-suspend-battery"))
                    .id("power.suspend_battery"),
                timers.on_battery(),
                PowerPrefs::SUSPEND_ON_BATTERY.map(|label: String| suspend_minutes_of(&label)),
            ),
            self.suspend_timer(
                self.cx.row(t("power-suspend-plugged")).id("power.suspend"),
                timers.plugged_in(),
                PowerPrefs::SUSPEND_PLUGGED_IN.map(|label: String| suspend_minutes_of(&label)),
            ),
        ]
    }

    fn idle_card(&self) -> SettingsSection {
        let paths = PowerPrefs::paths();
        let mut body = vec![auto_suspend(&self.cx, &self.prefs)];
        body.extend(self.suspend_timers());
        body.extend([
            self.timeout(
                self.cx.row(t("power-blank")).id("power.blank"),
                BLANK_OPTIONS,
                blank_label,
                paths.blank_minutes,
            ),
            self.timeout(
                self.cx.row(t("power-lock-after")).id("power.lock"),
                LOCK_OPTIONS,
                lock_label,
                paths.lock_minutes,
            ),
            self.cx
                .row(t("power-lock-on-sleep"))
                .id("power.lock_on_sleep")
                .described(t("power-lock-on-sleep-caption"))
                .of(&self.prefs, paths.lock_on_sleep)
                .toggle(),
            note(&t("power-idle-caption")),
        ]);
        section(&t("power-idle-title"), rows(body))
    }

    /// `row` choosing what a button or a critical battery does.
    fn action_choice<'a>(
        &'a self,
        row: Row<'a>,
        field: tilo_resource::Path<PowerPrefs, PowerActionPref>,
    ) -> Item {
        row.of(&self.prefs, field).among(
            ACTION_OPTIONS.iter().copied(),
            |action: &PowerActionPref| action_shown(*action),
        )
    }

    fn actions_card(&self) -> SettingsSection {
        let prefs = *self.prefs();
        let warn = t_args(
            "power-warn-caption",
            &[
                ("warn", ArgValue::from(i64::from(prefs.low_battery_percent))),
                (
                    "critical",
                    ArgValue::from(i64::from(prefs.critical_battery_percent)),
                ),
            ],
        );
        section(
            &t("power-behaviour-title"),
            rows(vec![
                self.action_choice(
                    self.cx.row(t("power-lid")).id("power.lid"),
                    PowerPrefs::paths().lid_close_action,
                ),
                self.action_choice(
                    self.cx.row(t("power-power-key")).id("power.power_key"),
                    PowerPrefs::paths().power_key_action,
                ),
                self.action_choice(
                    self.cx.row(t("power-critical")).id("power.critical"),
                    PowerPrefs::paths().critical_action,
                ),
                note(&warn),
            ]),
        )
    }

    fn devices_card(state: &PowerState) -> Option<SettingsSection> {
        if state.devices.len() <= 1 {
            return None;
        }
        let mut entries = Vec::new();
        for d in &state.devices {
            let kind = t(match d.kind {
                os_actions::PowerSupplyKind::Battery => "power-device-battery",
                os_actions::PowerSupplyKind::Ups => "power-device-ups",
            });
            let line = t_args(
                "power-device-line",
                &[
                    ("kind", ArgValue::from(kind.as_str())),
                    ("capacity", ArgValue::from(i64::from(d.capacity))),
                    ("status", ArgValue::from(d.status.as_str())),
                ],
            );
            entries.push(value_row(d.model.as_deref().unwrap_or(&d.id), &line));
        }
        Some(section(&t("power-devices-title"), rows(entries)))
    }
}

pub(crate) mod values;

pub(crate) fn auto_suspend(cx: &PaneCx, prefs: &Followed<PowerPrefs>) -> Item {
    cx.row(t("power-suspend-idle"))
        .id("power.auto_suspend")
        .of(prefs, PowerPrefs::paths().auto_suspend)
        .toggle()
}

/// Whether waking from sleep asks for the password, as this pane's own row
/// (and Settings' search) shows it.
pub(crate) fn lock_on_sleep(cx: &PaneCx, prefs: &Followed<PowerPrefs>) -> Item {
    cx.row(t("power-lock-on-sleep"))
        .id("power.lock_on_sleep")
        .described(t("power-lock-on-sleep-caption"))
        .of(prefs, PowerPrefs::paths().lock_on_sleep)
        .toggle()
}

impl Widget for PowerPane {
    fn sync(&mut self, shared: &AppModel, root: &ModelPath) {
        // The controls are built from the values: a change rebuilds them.
        if self.prefs.refresh() {
            self.composed.invalidate();
        }
        let revision = self.hardware.revision();
        if revision != self.hardware_seen {
            self.hardware_seen = revision;
            self.state = self.hardware.get();
            self.composed.invalidate();
        }
        let notices = self.notices_state.get();
        if notices != self.notices {
            self.notices = notices;
            self.composed.invalidate();
        }
        let span = self.span.get();
        if span != self.span_shown {
            self.span_shown = span;
            self.composed.invalidate();
        }
        self.composed.sync(shared, root);
    }

    crate::panes::composed_pane!();

    fn has_pending_visual_work(&self) -> bool {
        // The hardware's first reading replaces "Reading…".
        self.state.is_none()
            || self
                .composed
                .element()
                .is_some_and(Widget::has_pending_visual_work)
    }

    fn name(&self) -> &'static str {
        "PowerPane"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::prelude::TestApp;
    use ui::theme::Theme;

    /// A capture's week of charge ends at the battery's own reading, keeps
    /// to its charge limit and breaks overnight.
    #[test]
    fn the_sample_week_ends_at_the_batterys_reading() {
        let power = SamplePower::default();
        let now = power.now_minute();
        let history = sample_history(now);
        let last = history.points.last().expect("readings");
        assert_eq!(last.capacity, power.battery().capacity);
        assert!(last.unix_minute <= now && now - last.unix_minute < 15);
        assert!(history.points.iter().all(|point| point.capacity <= 80));
        assert!(
            history
                .points
                .windows(2)
                .any(|pair| pair[1].unix_minute - pair[0].unix_minute > 60),
            "asleep overnight"
        );
    }

    /// The battery shows its charge large, how long it lasts at the rate
    /// it drains, and its charge over the last day (or week) drawn from the
    /// history, not a bar.
    #[test]
    fn the_battery_shows_time_left_and_its_charge_history() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let area = PowerArea::new(&cx);
        let mut pane = PowerPane::new(cx, &area);
        let state = tilo_test_env::wait_for("the hardware is read", || area.state.get());
        assert!(
            state.history.points.len() > 100,
            "a capture shows a week of readings"
        );
        pane.sync(&ui::automerge::AutoCommit::new(), &ui::automerge::ROOT);
        let text = crate::widgets::announced_text(pane.build_composition().as_ref());
        let says = |needle: &str| text.iter().any(|line| line.contains(needle));
        assert!(says("72"), "{text:#?}");
        assert!(says("left"), "time left at the rate it drains: {text:#?}");
        assert!(says(&t("power-history-day")), "{text:#?}");
        assert!(
            says(&t_args(
                "power-history-name",
                &[("span", ArgValue::from(t("power-history-day")))]
            )),
            "{text:#?}"
        );
        area.span.set(1);
        pane.sync(&ui::automerge::AutoCommit::new(), &ui::automerge::ROOT);
        let text = crate::widgets::announced_text(pane.build_composition().as_ref());
        assert!(
            text.iter().any(|line| line.contains(&t_args(
                "power-history-name",
                &[("span", ArgValue::from(t("power-history-week")))]
            ))),
            "{text:#?}"
        );
    }

    /// The pane and the session's system-settings actions offer the same
    /// timeouts, so a value one accepts is never refused by the other.
    #[test]
    fn the_pane_offers_the_timeouts_the_system_actions_validate() {
        let minutes = |options: &[(&str, u32)]| options.iter().map(|(_, m)| *m).collect::<Vec<_>>();
        let sorted = |values: &[u32]| {
            let mut values = values.to_vec();
            values.sort_unstable();
            values
        };
        let mut blank = minutes(BLANK_OPTIONS);
        let mut lock = minutes(LOCK_OPTIONS);
        let mut suspend = minutes(SUSPEND_OPTIONS);
        blank.sort_unstable();
        lock.sort_unstable();
        suspend.sort_unstable();
        assert_eq!(blank, sorted(settings_store::power::BLANK_CHOICES));
        assert_eq!(lock, sorted(settings_store::power::LOCK_CHOICES));
        assert_eq!(suspend, sorted(settings_store::power::SUSPEND_CHOICES));
        let defaults = PowerPrefs::default();
        assert_eq!(
            defaults.lock_minutes,
            settings_store::power::DEFAULT_LOCK_MINUTES
        );
        assert_eq!(
            defaults.blank_minutes,
            settings_store::power::DEFAULT_BLANK_MINUTES
        );
        assert_eq!(
            defaults.suspend_minutes,
            settings_store::power::DEFAULT_SUSPEND_MINUTES
        );
    }

    fn sample_battery() -> BatteryInfo {
        BatteryInfo {
            present: true,
            capacity: 72,
            status: "Discharging".into(),
            cycle_count: Some(148),
            on_ac: false,
            full_capacity: Some(40_000_000),
            design_capacity: Some(50_000_000),
            unit: os_actions::CapacityUnit::Energy,
            charge_limit: Some(80),
            remaining: Some(28_800_000),
            rate: Some(8_640_000),
        }
    }

    #[test]
    fn a_worn_battery_says_what_it_holds_against_new() {
        let worn = BatteryInfo {
            full_capacity: Some(44_600_000),
            design_capacity: Some(70_000_000),
            ..sample_battery()
        };
        assert_eq!(worn.health_percent(), Some(64));
        assert_eq!(capacity_shown(&worn).as_deref(), Some("44.6 Wh of 70 Wh"));
        assert_eq!(
            health_explanation_shown(&worn),
            "Capacity is noticeably reduced; shorter runtime is expected."
        );
        let unknown = BatteryInfo {
            design_capacity: None,
            ..sample_battery()
        };
        assert_eq!(capacity_shown(&unknown), None);
    }

    #[test]
    fn profile_and_blank_labels_round_trip() {
        assert_eq!(profile_label(PowerProfile::Saver), "Power Saver");
        assert_eq!(
            profile_from_label("Performance"),
            Some(PowerProfile::Performance)
        );
        assert_eq!(profile_from_label("Nonsense"), None);
        for (minutes, label) in [(0, "Never"), (10, "10 minutes")] {
            assert_eq!(blank_label(minutes), label);
        }
    }

    /// Battery facts from sysfs, honest degradation without a battery, and the pane layout.
    #[test]
    fn battery_reads_sysfs_and_the_pane_lays_out() {
        {
            let base = std::env::temp_dir().join(format!("tilo-power-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let bat = base.join("BAT0");
            let ac = base.join("ADP1");
            std::fs::create_dir_all(&bat).unwrap();
            std::fs::create_dir_all(&ac).unwrap();
            std::fs::write(bat.join("type"), "Battery\n").unwrap();
            std::fs::write(bat.join("capacity"), "84\n").unwrap();
            std::fs::write(bat.join("status"), "Charging\n").unwrap();
            std::fs::write(bat.join("cycle_count"), "37\n").unwrap();
            std::fs::write(ac.join("type"), "Mains\n").unwrap();
            std::fs::write(ac.join("online"), "1\n").unwrap();

            let info = BatteryInfo::from_root(&base);
            assert!(info.present);
            assert_eq!(info.capacity, 84);
            assert_eq!(info.status, "Charging");
            assert_eq!(info.cycle_count, Some(37));
            assert!(info.on_ac);
            assert_eq!(info.summary(), "84% · Charging");
            let _ = std::fs::remove_dir_all(&base);
        }

        {
            let base =
                std::env::temp_dir().join(format!("tilo-power-empty-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).unwrap();
            let info = BatteryInfo::from_root(&base);
            assert!(!info.present);
            assert!(info.on_ac, "no battery reads as mains-powered");
            assert_eq!(info.summary(), "Running on AC power");
            let _ = std::fs::remove_dir_all(&base);
        }

        {
            let profile = tempfile::tempdir().unwrap();
            let cx = PaneCx::in_file(profile.path().join("settings.json"));
            let area = PowerArea::new(&cx);
            let mut pane = PowerPane::new(cx.clone(), &area);
            let deadline = tilo_test_env::hang_deadline();
            while area.state.get().is_none() {
                assert!(std::time::Instant::now() < deadline, "the hardware is read");
                std::thread::yield_now();
            }
            pane.sync(&ui::automerge::AutoCommit::new(), &ui::automerge::ROOT);
            assert_eq!(pane.battery(), Some(&sample_battery()));
            // A fresh profile shows "Suspend when idle" off: the same value
            // the compositor's policy and the `power.auto-suspend` action
            // read from the untouched document.
            assert!(!pane.prefs().auto_suspend);
            assert_eq!(
                pane.prefs().auto_suspend,
                settings_store::power::AUTO_SUSPEND.get_at(&profile.path().join("settings.json"))
            );
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);

            cx.settings::<PowerPrefs>()
                .update(|prefs| prefs.auto_suspend = !prefs.auto_suspend)
                .unwrap();
            pane.sync(&ui::automerge::AutoCommit::new(), &ui::automerge::ROOT);
            assert!(
                pane.take_structure_dirty(),
                "a preference change rebuilds the controls"
            );
        }
    }

    /// The profile and the charge cap reach the hardware off the UI thread;
    /// an unsupported cap is refused before it does.
    #[test]
    fn hardware_effects_reach_the_device() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.effect(&SettingsEffect::from(PowerEffect::SetPowerProfile {
            profile: "Performance".into(),
        }))
        .expect("the profile is offered");
        app.effect(&SettingsEffect::from(PowerEffect::SetChargeLimit {
            percent: 60,
        }))
        .expect("the cap is offered");
        assert!(
            app.effect(&SettingsEffect::from(PowerEffect::SetChargeLimit {
                percent: 65
            }))
            .is_err()
        );
        app.effect(&SettingsEffect::from(
            crate::effects::NavigationEffect::OpenPane {
                pane: "power".into(),
            },
        ))
        .unwrap();
        // The hardware is read off the UI thread: its cards, with what the
        // changes said, are shown once the reading lands.
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains("Performance is now active.")
                        && frame.contains("Hardware confirmed a 60% charge cap."))),
            "{}",
            app.harness().inspection_json().unwrap()
        );
    }

    /// Power hardware that does not answer until it is let go, as
    /// `powerprofilesctl` does when power-profiles-daemon is slow to start
    /// on the system bus.
    struct StalledPower(Arc<crate::stall::Stall>);

    impl StalledPower {
        fn wait(&self) {
            self.0.wait();
        }
    }

    impl PowerControl for StalledPower {
        fn battery(&self) -> BatteryInfo {
            self.wait();
            SamplePower::default().battery()
        }
        fn devices(&self) -> Vec<os_actions::PowerDevice> {
            SamplePower::default().devices()
        }
        fn history(&self) -> os_actions::BatteryHistory {
            SamplePower::default().history()
        }
        fn profiles(
            &self,
        ) -> (
            Result<PowerProfile, String>,
            Result<Vec<PowerProfile>, String>,
        ) {
            self.wait();
            SamplePower::default().profiles()
        }
        fn set_profile(&self, _: PowerProfile) -> Result<(), String> {
            self.wait();
            Ok(())
        }
        fn charge_cap(&self) -> Option<ChargeCapCapability> {
            SamplePower::default().charge_cap()
        }
        fn set_charge_limit(&self, percent: u32) -> Result<ChargeCapCapability, String> {
            self.wait();
            SamplePower::default().set_charge_limit(percent)
        }
    }

    /// Opening Power while power-profiles-daemon takes its time is
    /// answered and drawn at once, its preferences already changeable: the
    /// hardware cards say they are being read, and fill in once it answers.
    #[test]
    fn a_stalled_power_daemon_never_holds_the_window() {
        ui::i18n::I18nManager::init("en-US");
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let gate = crate::stall::Stall::new();
        let stalled = Arc::clone(&gate);
        app.with_widget::<SettingsApp, _>(move |settings| {
            settings.replace_area(PowerArea::over(Arc::new(StalledPower(stalled))));
        })
        .expect("the Settings window is mounted");

        crate::effects::show_pane(&mut app, crate::categories::Category::Power);
        app.harness()
            .poll_frame()
            .expect("the window draws its next frame");
        gate.assert_held("opening the pane");
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains(&t("power-reading")), "{frame}");
        crate::host::testing::edit_field(
            &mut app,
            crate::host::testing::POWER,
            "auto_suspend",
            false,
        )
        .expect("a preference changes while the hardware is read");
        assert!(!app.settings::<PowerPrefs>().get().auto_suspend);

        gate.release();
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| !frame.contains(&t("power-reading"))
                        && frame.contains(&profile_label(PowerProfile::Balanced)))),
            "the hardware's cards are shown once it answers"
        );
    }

    /// The idle and button preferences are the device's `power` section,
    /// which the compositor's idle layer reads.
    #[test]
    fn idle_and_button_preferences_write_the_power_section() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        crate::host::testing::edit_field(
            &mut app,
            crate::host::testing::POWER,
            "auto_suspend",
            true,
        )
        .unwrap();
        let edit = |app: &mut TestApp<SettingsShell>, field: &str, value: serde_json::Value| {
            crate::host::testing::edit_field(app, crate::host::testing::POWER, field, value)
        };
        edit(&mut app, "blank_minutes", 30.into()).unwrap();
        edit(&mut app, "lock_minutes", 1.into()).expect("locking sooner asks nothing");
        for weaker in [10, 0] {
            assert!(
                edit(&mut app, "lock_minutes", weaker.into())
                    .is_err_and(|error| error.contains("Confirmation")),
                "locking later or never asks first"
            );
        }
        assert_eq!(app.settings::<PowerPrefs>().get().lock_minutes, 1);
        app.call_confirmed_on(
            &format!("settings:{}", crate::host::testing::POWER),
            "field-edit",
            serde_json::json!({
                "edit": { "path": [{ "field": "lock_minutes" }], "op": "set", "value": 0 }
            }),
        )
        .unwrap();
        edit(&mut app, "saver_suspend_minutes", 5.into()).unwrap();
        edit(&mut app, "lid_close_action", "lock".into()).unwrap();
        let power = app.settings::<PowerPrefs>().get();
        assert!(power.auto_suspend);
        assert_eq!(power.blank_minutes, 30);
        assert_eq!(power.lock_minutes, 0);
        assert_eq!(power.saver_suspend_minutes, Some(5));
        assert_eq!(power.lid_close_action, PowerActionPref::Lock);
        assert!(
            edit(&mut app, "blank_minutes", (25 * 60).into()).is_err(),
            "a delay past a day is refused"
        );
    }

    /// The control `id` the open Power pane shows, and whether it takes
    /// input; `None` when the pane does not show it.
    fn shown(app: &mut TestApp<SettingsShell>, id: &str) -> Option<bool> {
        ui::widget::find_test_id(app.harness().root_widget(), id).map(|control| {
            !control
                .accessibility()
                .is_some_and(|node| node.state.disabled)
        })
    }

    fn open_power(app: &mut TestApp<SettingsShell>) {
        crate::effects::show_pane(app, crate::categories::Category::Power);
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains(&t("power-profile-title")))),
            "the hardware is read"
        );
        for _ in 0..5 {
            app.settle();
            let _ = app.harness().poll_frame();
        }
    }

    #[test]
    fn one_suspend_timer_per_power_source_disabled_and_explained_while_idle_suspend_is_off() {
        ui::i18n::I18nManager::init("en-US");
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.harness().resize((1200, 2400)).unwrap();
        open_power(&mut app);
        // A fresh machine: idle-suspend off, so both timers are shown,
        // disabled, and say why.
        assert_eq!(shown(&mut app, "power.suspend_battery"), Some(false));
        assert_eq!(shown(&mut app, "power.suspend"), Some(false));
        assert_eq!(shown(&mut app, "power.profile_suspend"), None);
        for gone in [
            "Power Saver suspend",
            "Balanced suspend",
            "Performance suspend",
        ] {
            assert!(
                !app.harness().inspection_json().unwrap().contains(gone),
                "no per-profile timer list: {gone}"
            );
        }
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains(&t("power-suspend-needs-idle"))
        );

        // On: both take input; each writes its own source's timer.
        crate::host::testing::edit_field(
            &mut app,
            crate::host::testing::POWER,
            "auto_suspend",
            true,
        )
        .unwrap();
        for _ in 0..5 {
            app.settle();
            let _ = app.harness().poll_frame();
        }
        assert_eq!(shown(&mut app, "power.suspend_battery"), Some(true));
        assert_eq!(shown(&mut app, "power.suspend"), Some(true));
        assert!(
            !app.harness()
                .inspection_json()
                .unwrap()
                .contains(&t("power-suspend-needs-idle"))
        );
        crate::host::testing::invoke_section(
            &mut app,
            crate::host::testing::POWER,
            "set-suspend-on-battery",
            serde_json::json!({ "minutes": 5 }),
        )
        .unwrap();
        crate::host::testing::invoke_section(
            &mut app,
            crate::host::testing::POWER,
            "set-suspend-plugged-in",
            serde_json::json!({ "minutes": 60 }),
        )
        .unwrap();
        let timers = app.settings::<PowerPrefs>().get().timers();
        assert_eq!((timers.on_battery(), timers.plugged_in()), (5, 60));
        assert!(
            crate::host::testing::invoke_section(
                &mut app,
                crate::host::testing::POWER,
                "set-suspend-on-battery",
                serde_json::json!({ "minutes": 7 }),
            )
            .is_err(),
            "only the offered timers"
        );

        // Profile timers that truly differ: the selected profile's timer is
        // the one, shown with the profile, and the source timers go.
        crate::host::testing::edit_field(
            &mut app,
            crate::host::testing::POWER,
            "saver_suspend_minutes",
            10,
        )
        .unwrap();
        for _ in 0..5 {
            app.settle();
            let _ = app.harness().poll_frame();
        }
        assert_eq!(shown(&mut app, "power.profile_suspend"), Some(true));
        assert_eq!(shown(&mut app, "power.suspend_battery"), None);
        assert_eq!(shown(&mut app, "power.suspend"), None);
    }
}

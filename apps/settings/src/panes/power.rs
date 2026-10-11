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
//!   compositor's idle layer reads.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use os_actions::{BatteryInfo, ChargeCapBackend, ChargeCapCapability, PowerProfile};
use taffy::{NodeId, TaffyTree};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::theme::Theme;
use ui::tilo_resource::{self, ActionRef};
use ui::widget::{AppModel, Dropdown, ModelPath, ProgressBar, Sparkline, Toggle, Widget};

use crate::effects::effect;
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::prefs::{PowerActionPref, PowerPrefs};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    El, Item, SettingsSection, block, card, choice_row, error_caption, note, row, rows,
    setting_row, untitled, value_row,
};
use ui::i18n::ArgValue;

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

fn duration_from_options(options: &[(&str, u32)], label: &str) -> Option<u32> {
    options
        .iter()
        .find(|(english, _)| *english == label || duration_shown(english) == label)
        .map(|(_, minutes)| *minutes)
}

fn duration_options(options: &[(&str, u32)]) -> Vec<String> {
    options
        .iter()
        .map(|(english, _)| duration_shown(english))
        .collect()
}

fn suspend_label(minutes: u32) -> String {
    let english = SUSPEND_OPTIONS
        .iter()
        .find(|(_, m)| *m == minutes)
        .map_or("15 minutes", |(l, _)| *l);
    duration_shown(english)
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

pub(crate) fn action_from_label(label: &str) -> Option<PowerActionPref> {
    ACTION_OPTIONS
        .iter()
        .copied()
        .find(|pref| pref.display() == label || action_shown(*pref) == label)
}

fn action_options() -> Vec<String> {
    ACTION_OPTIONS.iter().copied().map(action_shown).collect()
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

fn battery_status_shown(battery: &BatteryInfo) -> String {
    let capacity = ArgValue::from(i64::from(battery.capacity));
    match battery.status.as_str() {
        "Full" => t("power-status-full"),
        "Charging" => t_args("power-status-charging", &[("capacity", capacity)]),
        "Not charging" => t_args("power-status-plugged", &[("capacity", capacity)]),
        _ => t_args("power-status-battery", &[("capacity", capacity)]),
    }
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

/// The English names of a `(label, value)` table, as an effect's choices.
fn english<T>(table: &'static [(&'static str, T)]) -> impl Iterator<Item = &'static str> {
    table.iter().map(|(label, _)| *label)
}

/// The idle-suspend timeouts: the default one, and one per profile.
const SUSPEND_PROFILES: [&str; 4] = ["default", "saver", "balanced", "performance"];

/// This device's power hardware, as the pane and the effects reach it.
pub trait PowerControl: Send + Sync {
    /// The battery now.
    fn battery(&self) -> BatteryInfo;
    /// Every power supply, for a device with more than one.
    fn devices(&self) -> Vec<os_actions::PowerDevice>;
    /// Recent charge, written by the shell's monitor.
    fn history(&self) -> os_actions::BatteryHistory;
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
        BatteryInfo::from_system()
    }

    fn devices(&self) -> Vec<os_actions::PowerDevice> {
        os_actions::power_devices()
    }

    fn history(&self) -> os_actions::BatteryHistory {
        // The shell-owned UPower monitor is the single history writer.
        os_actions::load_battery_history()
    }

    fn profiles(
        &self,
    ) -> (
        Result<PowerProfile, String>,
        Result<Vec<PowerProfile>, String>,
    ) {
        (
            os_actions::get_power_profile(),
            os_actions::available_power_profiles(),
        )
    }

    fn set_profile(&self, profile: PowerProfile) -> Result<(), String> {
        os_actions::set_power_profile(profile)
    }

    fn charge_cap(&self) -> Option<ChargeCapCapability> {
        os_actions::charge_capability()
    }

    fn set_charge_limit(&self, percent: u32) -> Result<ChargeCapCapability, String> {
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
        BatteryInfo {
            present: true,
            capacity: 72,
            status: "Discharging".into(),
            cycle_count: Some(148),
            on_ac: false,
            full_capacity: Some(40_000_000),
            design_capacity: Some(50_000_000),
            charge_limit: Some(
                self.cap
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .current,
            ),
        }
    }

    fn devices(&self) -> Vec<os_actions::PowerDevice> {
        Vec::new()
    }

    fn history(&self) -> os_actions::BatteryHistory {
        os_actions::BatteryHistory::default()
    }

    fn profiles(
        &self,
    ) -> (
        Result<PowerProfile, String>,
        Result<Vec<PowerProfile>, String>,
    ) {
        (
            Ok(*self.profile.lock().unwrap_or_else(PoisonError::into_inner)),
            Ok(PROFILES.iter().map(|(_, profile)| *profile).collect()),
        )
    }

    fn set_profile(&self, profile: PowerProfile) -> Result<(), String> {
        *self.profile.lock().unwrap_or_else(PoisonError::into_inner) = profile;
        Ok(())
    }

    fn charge_cap(&self) -> Option<ChargeCapCapability> {
        Some(
            self.cap
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        )
    }

    fn set_charge_limit(&self, percent: u32) -> Result<ChargeCapCapability, String> {
        let mut cap = self.cap.lock().unwrap_or_else(PoisonError::into_inner);
        cap.current = percent;
        Ok(cap.clone())
    }
}

/// What the device's power hardware reports, read once when first needed.
#[derive(Clone)]
struct PowerState {
    battery: BatteryInfo,
    devices: Vec<os_actions::PowerDevice>,
    history: os_actions::BatteryHistory,
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
                Some(t_args(
                    "power-profile-service-unavailable",
                    &[("details", ArgValue::from(details.join("; ")))],
                ))
            }
        };
        Self {
            battery: control.battery(),
            devices: control.devices(),
            history: control.history(),
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
    state: OnceCell<LocalState<PowerState>>,
    /// Bumped by every change the hardware took, so the pane reads it again.
    applied: LocalState<u64>,
    notices: LocalState<Notices>,
}

impl PowerArea {
    /// The area over this device's power hardware, or a stand-in for a
    /// capture. Nothing is read until the pane or an effect needs it.
    pub fn new(cx: &PaneCx) -> Self {
        let control: Arc<dyn PowerControl> = if cx.reaches_devices() {
            Arc::new(SystemPower)
        } else {
            Arc::new(SamplePower::default())
        };
        Self {
            control,
            state: OnceCell::new(),
            applied: LocalState::new(0),
            notices: LocalState::new(Notices::default()),
        }
    }

    fn state(&self) -> &LocalState<PowerState> {
        self.state
            .get_or_init(|| LocalState::new(PowerState::read(self.control.as_ref())))
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
    /// Suspend when idle, or never.
    #[effect(idempotent)]
    SetAutoSuspend { on: bool },
    /// Set how long the device waits before suspending.
    #[effect(idempotent)]
    SetSuspendAfter {
        #[param("Which timeout: the default, or a profile's", choices = SUSPEND_PROFILES)]
        profile: String,
        #[param("How long", choices = english(SUSPEND_OPTIONS))]
        after: String,
    },
    /// Set how long the screen stays on while idle.
    #[effect(idempotent)]
    SetBlankAfter {
        #[param("How long", choices = english(BLANK_OPTIONS))]
        after: String,
    },
    /// Set how long before an idle session locks.
    #[effect(idempotent)]
    SetLockAfter {
        #[param("How long", choices = english(LOCK_OPTIONS))]
        after: String,
    },
    /// Set what closing the lid does.
    #[effect(idempotent)]
    SetLidAction {
        #[param("What it does", choices = ACTION_OPTIONS.iter().map(|pref| pref.display()))]
        action: String,
    },
    /// Set what the power key does.
    #[effect(idempotent)]
    SetPowerKeyAction {
        #[param("What it does", choices = ACTION_OPTIONS.iter().map(|pref| pref.display()))]
        action: String,
    },
    /// Set what happens when the battery is critically low.
    #[effect(idempotent)]
    SetCriticalAction {
        #[param("What it does", choices = ACTION_OPTIONS.iter().map(|pref| pref.display()))]
        action: String,
    },
}

/// Answer `effect`: hardware changes run off the UI thread; the idle and
/// button preferences are the device's `power` settings.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per power effect, each a short hardware or preference change"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: PowerEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let host = app.host();
    let not = |value: &str| format!("`{value}` is not offered");
    let duration = |options: &[(&str, u32)], after: &str| {
        duration_from_options(options, after).ok_or_else(|| not(after))
    };
    let action = |label: &str| action_from_label(label).ok_or_else(|| not(label));
    match effect {
        PowerEffect::SetAutoSuspend { on } => {
            host.update(|power: &mut PowerPrefs| power.auto_suspend = on)
        }
        PowerEffect::SetSuspendAfter { profile, after } => {
            let minutes = duration(SUSPEND_OPTIONS, &after)?;
            host.update(|power: &mut PowerPrefs| match profile.as_str() {
                "saver" => power.saver_suspend_minutes = minutes,
                "balanced" => power.balanced_suspend_minutes = minutes,
                "performance" => power.performance_suspend_minutes = minutes,
                _ => power.suspend_minutes = minutes,
            })
        }
        PowerEffect::SetBlankAfter { after } => {
            let minutes = duration(BLANK_OPTIONS, &after)?;
            host.update(|power: &mut PowerPrefs| power.blank_minutes = minutes)
        }
        PowerEffect::SetLockAfter { after } => {
            let minutes = duration(LOCK_OPTIONS, &after)?;
            host.update(|power: &mut PowerPrefs| power.lock_minutes = minutes)
        }
        PowerEffect::SetLidAction { action: label } => {
            let pref = action(&label)?;
            host.update(|power: &mut PowerPrefs| power.lid_close_action = pref)
        }
        PowerEffect::SetPowerKeyAction { action: label } => {
            let pref = action(&label)?;
            host.update(|power: &mut PowerPrefs| power.power_key_action = pref)
        }
        PowerEffect::SetCriticalAction { action: label } => {
            let pref = action(&label)?;
            host.update(|power: &mut PowerPrefs| power.critical_action = pref)
        }
        PowerEffect::SetPowerProfile { profile } => {
            let profile = profile_from_label(&profile).ok_or_else(|| not(&profile))?;
            let area = app.power();
            if !area
                .state()
                .with(|state| state.available_profiles.contains(&profile))
            {
                return Err(format!(
                    "{} is not supported by this device",
                    profile_label(profile)
                ));
            }
            let control = Arc::clone(&area.control);
            Ok(cx.thread(
                move |_| control.set_profile(profile).map(|()| profile),
                |app: &mut SettingsApp, outcome: Result<PowerProfile, String>, _| {
                    let area = app.power();
                    let mut notices = area.notices.get();
                    let answer = match outcome {
                        Ok(profile) => {
                            area.state().update(|state| {
                                state.profile = profile;
                                state.profile_unread = false;
                            });
                            area.applied
                                .update(|applied| *applied = applied.wrapping_add(1));
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
            let area = app.power();
            let offered = area.state().with(|state| {
                state
                    .charge_cap
                    .as_ref()
                    .is_some_and(|cap| cap.choices.contains(&percent))
            });
            if !offered {
                return Err(format!("this battery offers no {percent}% charge cap"));
            }
            let control = Arc::clone(&area.control);
            Ok(cx.thread(
                move |_| control.set_charge_limit(percent).map(|cap| (percent, cap)),
                |app: &mut SettingsApp, outcome: Result<(u32, ChargeCapCapability), String>, _| {
                    let area = app.power();
                    let mut notices = area.notices.get();
                    let answer = match outcome {
                        Ok((percent, cap)) => {
                            area.state().update(|state| {
                                state.battery.charge_limit = Some(percent);
                                state.charge_cap = Some(cap);
                            });
                            area.applied
                                .update(|applied| *applied = applied.wrapping_add(1));
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
    hardware: LocalState<PowerState>,
    state: PowerState,
    applied: LocalState<u64>,
    applied_seen: u64,
    notices_state: LocalState<Notices>,
    notices: Notices,
    composed: ui::widget::Composed,
}

impl PowerPane {
    /// The pane over `area`'s hardware and `cx`'s power settings.
    pub fn new(cx: PaneCx, area: &PowerArea) -> Self {
        let hardware = area.state().clone();
        Self {
            prefs: cx.follow::<PowerPrefs>(),
            cx,
            state: hardware.get(),
            hardware,
            applied_seen: area.applied.get(),
            applied: area.applied.clone(),
            notices: area.notices.get(),
            notices_state: area.notices.clone(),
            composed: ui::widget::Composed::default(),
        }
    }

    pub fn battery(&self) -> &BatteryInfo {
        &self.state.battery
    }

    pub fn profile(&self) -> PowerProfile {
        self.state.profile
    }

    pub fn prefs(&self) -> &PowerPrefs {
        self.prefs.get()
    }

    /// A dropdown over `options`, showing `current`, bound to `set`.
    fn choice(
        &self,
        id: Option<&str>,
        options: Vec<String>,
        current: &str,
        set: impl tilo_resource::Binds<crate::effects::SettingsEffect, String>,
    ) -> El {
        let mut dropdown = Dropdown::from_state(options, &LocalState::new(current.to_owned()))
            .with_placeholder(current);
        if let Some(id) = id {
            dropdown = dropdown.with_id(id);
        }
        Box::new(self.cx.bind(dropdown, set))
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut cards = self.battery_card();
        cards.extend(self.devices_card());
        cards.push(self.profile_card());
        cards.extend(self.idle_card());
        cards.push(self.actions_card());
        crate::widgets::cards(cards)
    }

    fn battery_card(&self) -> Vec<SettingsSection> {
        if !self.state.battery.present {
            return vec![card(
                &t("power-source-title"),
                rows(vec![
                    value_row(&t("power-source"), &t("power-source-ac")),
                    note(&t("power-source-caption")),
                ]),
            )];
        }
        let charge = format!("{}%", self.state.battery.capacity);
        let status = battery_status_shown(&self.state.battery);
        let mut battery_rows: Vec<Item> = vec![
            block(Box::new(ProgressBar::new(
                self.state.battery.capacity as f32 / 100.0,
            ))),
            value_row(&t("power-charge"), &charge),
            value_row(&t("power-status"), &status),
        ];
        if let Some(cycles) = self.state.battery.cycle_count {
            battery_rows.push(value_row(&t("power-cycles"), &cycles.to_string()));
        }
        if let Some(health) = self.state.battery.health_percent() {
            battery_rows.push(value_row(&t("power-health"), &format!("{health}%")));
            battery_rows.push(note(&health_explanation_shown(&self.state.battery)));
        } else {
            battery_rows.push(value_row(&t("power-health"), &t("power-health-unknown")));
        }
        let trend: Vec<f32> = self
            .state
            .history
            .points
            .iter()
            .map(|p| p.capacity as f32)
            .collect();
        if let Some(capability) = &self.state.charge_cap {
            let choices: Vec<String> = capability
                .choices
                .iter()
                .map(|limit| format!("{limit}%"))
                .collect();
            let current = format!("{}%", capability.current);
            battery_rows.push(choice_row(
                &t("power-charge-cap"),
                Box::new(
                    self.cx.bind(
                        Dropdown::from_state(choices, &LocalState::new(current.clone()))
                            .with_id("power.charge_limit")
                            .with_placeholder(&current),
                        effect(PowerEffect::SET_CHARGE_LIMIT).map(|shown: String| {
                            shown
                                .trim_end_matches('%')
                                .parse::<u32>()
                                .unwrap_or_default()
                        }),
                    ),
                ),
            ));
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
        let mut sections = vec![card(&t("power-battery-title"), rows(battery_rows))];
        if trend.len() >= 2 {
            sections.push(untitled(rows(vec![
                // A chart is not a row.
                block(Box::new(
                    Sparkline::new(trend, Theme::dark().tokens.colors.text_primary)
                        .with_max(100.0)
                        .with_size(320.0, 56.0),
                )),
                note(&t("power-history-caption")),
            ])));
        }
        sections
    }

    fn profile_card(&self) -> SettingsSection {
        let profile_opts: Vec<String> = self
            .state
            .available_profiles
            .iter()
            .map(|profile| profile_label(*profile))
            .collect();
        let current = if self.state.profile_unread {
            t("power-profile-unread")
        } else {
            profile_label(self.state.profile)
        };
        let mut profile_rows = if profile_opts.is_empty() {
            vec![value_row(
                &t("power-profile"),
                &t("power-profile-unavailable"),
            )]
        } else {
            vec![choice_row(
                &t("power-profile"),
                self.choice(
                    Some("power.profile"),
                    profile_opts,
                    &current,
                    effect(PowerEffect::SET_POWER_PROFILE).map(|shown: String| {
                        profile_from_label(&shown)
                            .and_then(|profile| {
                                PROFILES.iter().find(|(_, known)| *known == profile)
                            })
                            .map_or(shown, |(english, _)| (*english).to_owned())
                    }),
                ),
            )]
        };
        profile_rows.push(note(&t("power-profile-caption")));
        if let Some(notice) = &self.notices.profile {
            profile_rows.push(note(notice));
        } else if let Some(unavailable) = &self.state.profiles_unavailable {
            profile_rows.push(if self.state.profile_unread {
                row(error_caption(unavailable))
            } else {
                note(unavailable)
            });
        }
        card(&t("power-profile-title"), rows(profile_rows))
    }

    /// A timeout chooser: the shown label becomes the English one the effect
    /// takes.
    fn timeout(
        &self,
        id: Option<&str>,
        options: &'static [(&'static str, u32)],
        current: &str,
        set: ActionRef<PowerEffect, String>,
    ) -> El {
        self.choice(
            id,
            duration_options(options),
            current,
            effect(set).map(move |shown: String| duration_english(options, &shown)),
        )
    }

    /// A per-profile suspend timeout chooser.
    fn suspend_timeout(&self, profile: &'static str, current: &str) -> El {
        self.choice(
            (profile == "default").then_some("power.suspend"),
            duration_options(SUSPEND_OPTIONS),
            current,
            effect(PowerEffect::SET_SUSPEND_AFTER).map(move |shown: String| {
                (
                    profile.to_owned(),
                    duration_english(SUSPEND_OPTIONS, &shown),
                )
            }),
        )
    }

    fn idle_card(&self) -> [SettingsSection; 2] {
        let prefs = *self.prefs();
        [
            card(
                &t("power-idle-title"),
                rows(vec![
                    setting_row(
                        &t("power-suspend-idle"),
                        Box::new(
                            self.cx.bind(
                                Toggle::from_state(&LocalState::new(prefs.auto_suspend))
                                    .with_id("power.auto_suspend"),
                                effect(PowerEffect::SET_AUTO_SUSPEND),
                            ),
                        ),
                    ),
                    choice_row(
                        &t("power-suspend-after"),
                        self.suspend_timeout("default", &suspend_label(prefs.suspend_minutes)),
                    ),
                    choice_row(
                        &t("power-blank"),
                        self.timeout(
                            Some("power.blank"),
                            BLANK_OPTIONS,
                            &blank_label(prefs.blank_minutes),
                            PowerEffect::SET_BLANK_AFTER,
                        ),
                    ),
                    choice_row(
                        &t("power-lock-after"),
                        self.timeout(
                            Some("power.lock"),
                            LOCK_OPTIONS,
                            &lock_label(prefs.lock_minutes),
                            PowerEffect::SET_LOCK_AFTER,
                        ),
                    ),
                    note(&t("power-idle-caption")),
                ]),
            ),
            untitled(rows(vec![
                choice_row(
                    &t("power-saver-suspend"),
                    self.suspend_timeout("saver", &suspend_label(prefs.saver_suspend_minutes)),
                ),
                choice_row(
                    &t("power-balanced-suspend"),
                    self.suspend_timeout(
                        "balanced",
                        &suspend_label(prefs.balanced_suspend_minutes),
                    ),
                ),
                choice_row(
                    &t("power-performance-suspend"),
                    self.suspend_timeout(
                        "performance",
                        &suspend_label(prefs.performance_suspend_minutes),
                    ),
                ),
                note(&t("power-profile-idle-caption")),
            ])),
        ]
    }

    /// A button-action chooser.
    fn action_choice(
        &self,
        id: &str,
        current: PowerActionPref,
        set: ActionRef<PowerEffect, String>,
    ) -> El {
        self.choice(
            Some(id),
            action_options(),
            &action_shown(current),
            effect(set).map(|shown: String| {
                action_from_label(&shown).map_or(shown, |pref| pref.display().to_owned())
            }),
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
        card(
            &t("power-behaviour-title"),
            rows(vec![
                choice_row(
                    &t("power-lid"),
                    self.action_choice(
                        "power.lid",
                        prefs.lid_close_action,
                        PowerEffect::SET_LID_ACTION,
                    ),
                ),
                choice_row(
                    &t("power-power-key"),
                    self.action_choice(
                        "power.power_key",
                        prefs.power_key_action,
                        PowerEffect::SET_POWER_KEY_ACTION,
                    ),
                ),
                choice_row(
                    &t("power-critical"),
                    self.action_choice(
                        "power.critical",
                        prefs.critical_action,
                        PowerEffect::SET_CRITICAL_ACTION,
                    ),
                ),
                note(&warn),
            ]),
        )
    }

    fn devices_card(&self) -> Option<SettingsSection> {
        if self.state.devices.len() <= 1 {
            return None;
        }
        let mut entries = Vec::new();
        for d in &self.state.devices {
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
        Some(card(&t("power-devices-title"), rows(entries)))
    }
}

/// The English name of a duration shown in either language.
fn duration_english(options: &[(&'static str, u32)], shown: &str) -> String {
    options
        .iter()
        .find(|(english, _)| *english == shown || duration_shown(english) == shown)
        .map_or_else(|| shown.to_owned(), |(english, _)| (*english).to_owned())
}

impl Widget for PowerPane {
    fn sync(&mut self, shared: &AppModel, root: &ModelPath) {
        // The controls are built from the values: a change rebuilds them.
        if self.prefs.refresh() {
            self.composed.invalidate();
        }
        let applied = self.applied.get();
        if applied != self.applied_seen {
            self.applied_seen = applied;
            self.state = self.hardware.get();
            self.composed.invalidate();
        }
        let notices = self.notices_state.get();
        if notices != self.notices {
            self.notices = notices;
            self.composed.invalidate();
        }
        self.composed.sync(shared, root);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let composition = self.build_composition();
        let child = self
            .composed
            .build_layout(composition, taffy, theme, node_map);
        // `child` was just created in this same `taffy` tree, so wrapping it
        // cannot fail in practice; falling back to the child node itself
        // (skipping the wrapper style) is a real degraded behaviour rather
        // than a panic if that invariant is ever broken.
        crate::panes::pane_node(taffy, child, node_map, self)
    }

    ui::forward_to_composed!(composed);

    fn name(&self) -> &'static str {
        "PowerPane"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    fn sample_battery() -> BatteryInfo {
        BatteryInfo {
            present: true,
            capacity: 72,
            status: "Discharging".into(),
            cycle_count: Some(148),
            on_ac: false,
            full_capacity: Some(40_000_000),
            design_capacity: Some(50_000_000),
            charge_limit: Some(80),
        }
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
        // Case `reads_battery_from_sysfs_tree`.
        {
            // Build a throwaway /sys/class/power_supply-like tree.
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

        // Case `missing_battery_degrades_honestly`.
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

        // Case `builds_layout`.
        {
            let profile = tempfile::tempdir().unwrap();
            let cx = PaneCx::in_file(profile.path().join("settings.json"));
            let mut pane = PowerPane::new(cx.clone(), &PowerArea::new(&cx));
            assert_eq!(pane.battery(), &sample_battery());
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
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("Performance is now active."), "{frame}");
        assert!(
            frame.contains("Hardware confirmed a 60% charge cap."),
            "{frame}"
        );
    }

    /// The idle and button preferences are the device's `power` section,
    /// which the compositor's idle layer reads.
    #[test]
    fn idle_and_button_preferences_write_the_power_section() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        for effect in [
            PowerEffect::SetAutoSuspend { on: false },
            PowerEffect::SetBlankAfter {
                after: "30 minutes".into(),
            },
            PowerEffect::SetLockAfter {
                after: "Never".into(),
            },
            PowerEffect::SetSuspendAfter {
                profile: "saver".into(),
                after: "5 minutes".into(),
            },
            PowerEffect::SetLidAction {
                action: PowerActionPref::Lock.display().into(),
            },
        ] {
            app.effect(&SettingsEffect::from(effect)).unwrap();
        }
        let power = app.settings::<PowerPrefs>().get();
        assert!(!power.auto_suspend);
        assert_eq!(power.blank_minutes, 30);
        assert_eq!(power.lock_minutes, 0);
        assert_eq!(power.saver_suspend_minutes, 5);
        assert_eq!(power.lid_close_action, PowerActionPref::Lock);
        assert!(
            app.effect(&SettingsEffect::from(PowerEffect::SetBlankAfter {
                after: "7 minutes".into(),
            }))
            .is_err()
        );
    }
}

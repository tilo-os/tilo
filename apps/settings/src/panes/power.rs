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

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use os_actions::{BatteryInfo, ChargeCapBackend, ChargeCapCapability, PowerProfile};
use taffy::{NodeId, TaffyTree};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::theme::Theme;
use ui::tilo_resource::{self, ActionRef};
use ui::widget::{AppModel, Dropdown, ModelPath, ProgressBar, Sparkline, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::effects::effect;
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::prefs::{PowerActionPref, PowerPrefs};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{El, caption, card, choice_row, error_caption, rows, setting_row, value_row};
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
                Some(details.join("; "))
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
struct PendingPowerRead {
    revision: u64,
    receiver: std::sync::mpsc::Receiver<PowerState>,
}

#[derive(Clone)]
pub struct PowerArea {
    control: Arc<dyn PowerControl>,
    state: LocalState<Option<PowerState>>,
    reading: Rc<RefCell<Option<PendingPowerRead>>>,
    read_error: LocalState<Option<String>>,
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
            state: LocalState::new(None),
            reading: Rc::default(),
            read_error: LocalState::new(None),
            applied: LocalState::new(0),
            notices: LocalState::new(Notices::default()),
        }
    }

    fn ensure_read(&self) {
        self.poll_read();
        if self.state.get().is_some()
            || self.reading.borrow().is_some()
            || self.read_error.get().is_some()
        {
            return;
        }
        let revision = self.applied.get();
        let control = Arc::clone(&self.control);
        let (sender, receiver) = std::sync::mpsc::channel();
        let wake = ui::WakeHandle::new();
        match ui::background::Background::current().spawn("power_snapshot", move || {
            let snapshot = PowerState::read(control.as_ref());
            if sender.send(snapshot).is_ok() {
                wake.wake();
            }
        }) {
            Ok(()) => *self.reading.borrow_mut() = Some(PendingPowerRead { revision, receiver }),
            Err(error) => {
                self.read_error.set(Some(error.to_string()));
                self.applied
                    .update(|revision| *revision = revision.wrapping_add(1));
            }
        }
    }

    fn poll_read(&self) {
        let ready =
            self.reading
                .borrow()
                .as_ref()
                .and_then(|pending| match pending.receiver.try_recv() {
                    Ok(snapshot) => Some((pending.revision, Ok(snapshot))),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Some((pending.revision, Err(t("power-read-disconnected"))))
                    }
                });
        if let Some((revision, snapshot)) = ready {
            self.reading.borrow_mut().take();
            if revision != self.applied.get() {
                return;
            }
            match snapshot {
                Ok(snapshot) => self.state.set(Some(snapshot)),
                Err(error) => self.read_error.set(Some(error)),
            }
            self.applied
                .update(|revision| *revision = revision.wrapping_add(1));
        }
    }

    fn adopt(&self, snapshot: PowerState) {
        self.reading.borrow_mut().take();
        self.state.set(Some(snapshot));
        self.read_error.set(None);
        self.applied
            .update(|revision| *revision = revision.wrapping_add(1));
    }

    pub(crate) fn statuses(&self) -> Vec<ui::framework::EffectStatus> {
        self.poll_read();
        if self.reading.borrow().is_none() {
            return Vec::new();
        }
        [
            PowerEffect::SET_POWER_PROFILE.id,
            PowerEffect::SET_CHARGE_LIMIT.id,
        ]
        .into_iter()
        .map(|id| {
            ui::framework::EffectStatus::unavailable(
                ActionRef::<crate::effects::SettingsEffect, ()>::new(id, &[]),
                "pending_power_read",
                t("power-reading"),
            )
        })
        .collect()
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
            hardware_job(app, HardwareChange::Profile(profile), cx)
        }
        PowerEffect::SetChargeLimit { percent } => {
            hardware_job(app, HardwareChange::Charge(percent), cx)
        }
    }
}

#[derive(Clone, Copy)]
enum HardwareChange {
    Profile(PowerProfile),
    Charge(u32),
}

struct PowerJobRead {
    snapshot: PowerState,
    result: Result<ConfirmedChange, String>,
}

enum ConfirmedChange {
    Profile(PowerProfile),
    Charge {
        percent: u32,
        cap: ChargeCapCapability,
    },
}

fn prepare_change(
    control: &dyn PowerControl,
    cached: Option<PowerState>,
    change: HardwareChange,
) -> PowerJobRead {
    let snapshot = cached.unwrap_or_else(|| PowerState::read(control));
    let result = match change {
        HardwareChange::Profile(profile) => {
            if snapshot.available_profiles.contains(&profile) {
                control
                    .set_profile(profile)
                    .map(|()| ConfirmedChange::Profile(profile))
            } else {
                Err(format!(
                    "{} is not supported by this device",
                    profile_label(profile)
                ))
            }
        }
        HardwareChange::Charge(percent) => {
            if snapshot
                .charge_cap
                .as_ref()
                .is_some_and(|cap| cap.choices.contains(&percent))
            {
                control
                    .set_charge_limit(percent)
                    .map(|cap| ConfirmedChange::Charge { percent, cap })
            } else {
                Err(format!("this battery offers no {percent}% charge cap"))
            }
        }
    };
    PowerJobRead { snapshot, result }
}

fn hardware_job(
    app: &SettingsApp,
    change: HardwareChange,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let area = app.power();
    area.poll_read();
    if area.reading.borrow().is_some() {
        return Err(t("power-reading"));
    }
    let cached = area.state.get();
    if let Some(snapshot) = &cached {
        match change {
            HardwareChange::Profile(profile) if !snapshot.available_profiles.contains(&profile) => {
                return Err(format!(
                    "{} is not supported by this device",
                    profile_label(profile)
                ));
            }
            HardwareChange::Charge(percent)
                if !snapshot
                    .charge_cap
                    .as_ref()
                    .is_some_and(|cap| cap.choices.contains(&percent)) =>
            {
                return Err(format!("this battery offers no {percent}% charge cap"));
            }
            _ => {}
        }
    }
    let control = Arc::clone(&area.control);
    Ok(cx.thread(
        move |_| prepare_change(control.as_ref(), cached, change),
        move |app: &mut SettingsApp, read: PowerJobRead, _| {
            let area = app.power();
            // A concurrent job may already have changed another hardware field.
            // Merge only this operation's acknowledged field into that snapshot.
            let mut snapshot = area.state.get().unwrap_or(read.snapshot);
            let mut notices = area.notices.get();
            let answer = match read.result {
                Ok(ConfirmedChange::Profile(profile)) => {
                    snapshot.profile = profile;
                    snapshot.profile_unread = false;
                    notices.profile = Some(format!("{} is now active.", profile_label(profile)));
                    Ok(Effected::done())
                }
                Ok(ConfirmedChange::Charge { percent, cap }) => {
                    snapshot.battery.charge_limit = Some(percent);
                    snapshot.charge_cap = Some(cap);
                    notices.charge = Some(format!("Hardware confirmed a {percent}% charge cap."));
                    Ok(Effected::done())
                }
                Err(error) => {
                    let message = match change {
                        HardwareChange::Profile(_) => {
                            format!("Could not change the power profile: {error}")
                        }
                        HardwareChange::Charge(_) => {
                            format!("Could not change the charge cap: {error}")
                        }
                    };
                    match change {
                        HardwareChange::Profile(_) => notices.profile = Some(message.clone()),
                        HardwareChange::Charge(_) => notices.charge = Some(message.clone()),
                    }
                    Err(message)
                }
            };
            area.adopt(snapshot);
            area.notices.set(notices);
            answer
        },
    ))
}

/// Power pane widget.
pub struct PowerPane {
    cx: PaneCx,
    prefs: Followed<PowerPrefs>,
    /// What the hardware reports, kept current by the area.
    area: PowerArea,
    state: Option<PowerState>,
    read_error: Option<String>,
    applied_seen: u64,
    notices_state: LocalState<Notices>,
    notices: Notices,
    composition_dirty: bool,
    catalog_revision: u64,
    structure_dirty: bool,
    composed: ui::widget::Composed,
}

impl PowerPane {
    /// The pane over `area`'s hardware and `cx`'s power settings.
    pub fn new(cx: PaneCx, area: &PowerArea) -> Self {
        area.ensure_read();
        Self {
            prefs: cx.follow::<PowerPrefs>(),
            cx,
            state: area.state.get(),
            read_error: area.read_error.get(),
            area: area.clone(),
            applied_seen: area.applied.get(),
            notices: area.notices.get(),
            notices_state: area.notices.clone(),
            composition_dirty: true,
            catalog_revision: ui::i18n::I18nManager::revision(),
            structure_dirty: false,
            composed: ui::widget::Composed::default(),
        }
    }

    pub fn battery(&self) -> Option<&BatteryInfo> {
        self.state.as_ref().map(|state| &state.battery)
    }

    pub fn profile(&self) -> Option<PowerProfile> {
        self.state
            .as_ref()
            .filter(|state| !state.profile_unread)
            .map(|state| state.profile)
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
        let mut cards = if let Some(state) = &self.state {
            let mut cards = vec![self.battery_card(state), self.profile_card(state)];
            if let Some(devices) = Self::devices_card(state) {
                cards.insert(1, devices);
            }
            cards
        } else {
            let notice = self.read_error.as_ref().map_or_else(
                || t("power-reading"),
                |error| t_args("power-read-error", &[("error", error.as_str().into())]),
            );
            vec![card(
                &t("power-profile-title"),
                rows(vec![caption(&notice)]),
            )]
        };
        cards.extend([self.idle_card(), self.actions_card()]);
        crate::widgets::cards(cards)
    }

    fn battery_card(&self, state: &PowerState) -> El {
        if !state.battery.present {
            return card(
                &t("power-source-title"),
                rows(vec![
                    value_row(&t("power-source"), &t("power-source-ac")),
                    caption(&t("power-source-caption")),
                ]),
            );
        }
        let charge = format!("{}%", state.battery.capacity);
        let status = battery_status_shown(&state.battery);
        let mut battery_rows: Vec<El> = vec![
            Box::new(ProgressBar::new(state.battery.capacity as f32 / 100.0)),
            value_row(&t("power-charge"), &charge),
            value_row(&t("power-status"), &status),
        ];
        if let Some(cycles) = state.battery.cycle_count {
            battery_rows.push(value_row(&t("power-cycles"), &cycles.to_string()));
        }
        if let Some(health) = state.battery.health_percent() {
            battery_rows.push(value_row(&t("power-health"), &format!("{health}%")));
            battery_rows.push(caption(&health_explanation_shown(&state.battery)));
        } else {
            battery_rows.push(value_row(&t("power-health"), &t("power-health-unknown")));
        }
        let trend: Vec<f32> = state
            .history
            .points
            .iter()
            .map(|p| p.capacity as f32)
            .collect();
        if trend.len() >= 2 {
            battery_rows.push(Box::new(
                Sparkline::new(trend, Theme::dark().tokens.colors.text_primary)
                    .with_max(100.0)
                    .with_size(320.0, 56.0),
            ));
            battery_rows.push(caption(&t("power-history-caption")));
        }
        if let Some(capability) = &state.charge_cap {
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
            battery_rows.push(caption(&detail));
        } else {
            battery_rows.push(value_row(
                &t("power-charge-cap"),
                &t("power-charge-cap-unsupported"),
            ));
        }
        if let Some(notice) = &self.notices.charge {
            battery_rows.push(caption(notice));
        }
        card(&t("power-battery-title"), rows(battery_rows))
    }

    fn profile_card(&self, state: &PowerState) -> El {
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
        profile_rows.push(caption(&t("power-profile-caption")));
        if let Some(notice) = &self.notices.profile {
            profile_rows.push(caption(notice));
        } else if let Some(unavailable) = &state.profiles_unavailable {
            let unavailable = t_args(
                "power-profile-service-unavailable",
                &[("details", unavailable.as_str().into())],
            );
            profile_rows.push(if state.profile_unread {
                error_caption(&unavailable)
            } else {
                caption(&unavailable)
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

    fn idle_card(&self) -> El {
        let prefs = *self.prefs();
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
                caption(&t("power-idle-caption")),
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
                caption(&t("power-profile-idle-caption")),
            ]),
        )
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

    fn actions_card(&self) -> El {
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
                caption(&warn),
            ]),
        )
    }

    fn devices_card(state: &PowerState) -> Option<El> {
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
            self.composition_dirty = true;
            self.structure_dirty = true;
        }
        self.area.poll_read();
        let applied = self.area.applied.get();
        if applied != self.applied_seen {
            self.applied_seen = applied;
            self.state = self.area.state.get();
            self.read_error = self.area.read_error.get();
            self.composition_dirty = true;
            self.structure_dirty = true;
        }
        let notices = self.notices_state.get();
        if notices != self.notices {
            self.notices = notices;
            self.composition_dirty = true;
            self.structure_dirty = true;
        }
        self.composed.sync(shared, root);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if self.composition_dirty
            || self.catalog_revision != ui::i18n::I18nManager::revision()
            || self.composed.element().is_none()
        {
            let composition = self.build_composition();
            self.composed.replace(composition);
        }
        self.composition_dirty = false;
        self.catalog_revision = ui::i18n::I18nManager::revision();
        let child = if let Some(composition) = self.composed.element_mut() {
            composition.build_layout(taffy, theme, node_map)
        } else {
            // A missing retained child needs a real composition and layout,
            // rather than an empty node or a guessed geometry.
            let composition = self.build_composition();
            self.composed
                .build_layout(composition, taffy, theme, node_map)
        };
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        // `child` was just created in this same `taffy` tree, so wrapping it
        // cannot fail in practice; falling back to the child node itself
        // (skipping the wrapper style) is a real degraded behaviour rather
        // than a panic if that invariant is ever broken.
        let node = taffy.new_with_children(style, &[child]).unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.composed.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.composed
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.composed.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed.children()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed.children_mut()
    }

    fn has_pending_visual_work(&self) -> bool {
        self.area.reading.borrow().is_some()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self.composed.take_structure_dirty();
        own || child
    }

    fn name(&self) -> &'static str {
        "PowerPane"
    }

    fn get_rect(&self) -> Rect {
        self.composed.rect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    struct HeldPower {
        sample: SamplePower,
        entry: std::sync::mpsc::Sender<std::thread::ThreadId>,
        released: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        profile_reads: std::sync::atomic::AtomicUsize,
    }

    impl PowerControl for HeldPower {
        fn battery(&self) -> BatteryInfo {
            self.sample.battery()
        }
        fn devices(&self) -> Vec<os_actions::PowerDevice> {
            self.sample.devices()
        }
        fn history(&self) -> os_actions::BatteryHistory {
            self.sample.history()
        }
        fn profiles(
            &self,
        ) -> (
            Result<PowerProfile, String>,
            Result<Vec<PowerProfile>, String>,
        ) {
            self.profile_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let gate = self.released.lock().unwrap().take();
            if let Some(released) = gate {
                self.entry.send(std::thread::current().id()).unwrap();
                if released
                    .recv_timeout(std::time::Duration::from_secs(3))
                    .is_err()
                {
                    return (
                        Err("held power service timed out".into()),
                        Err("held power service timed out".into()),
                    );
                }
            }
            self.sample.profiles()
        }
        fn set_profile(&self, profile: PowerProfile) -> Result<(), String> {
            self.sample.set_profile(profile)
        }
        fn charge_cap(&self) -> Option<ChargeCapCapability> {
            self.sample.charge_cap()
        }
        fn set_charge_limit(&self, percent: u32) -> Result<ChargeCapCapability, String> {
            self.sample.set_charge_limit(percent)
        }
    }

    fn held_power() -> (
        Arc<HeldPower>,
        std::thread::JoinHandle<std::thread::ThreadId>,
    ) {
        let (entry, entered) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let control = Arc::new(HeldPower {
            sample: SamplePower::default(),
            entry,
            released: Mutex::new(Some(released)),
            profile_reads: std::sync::atomic::AtomicUsize::new(0),
        });
        let reader_task = std::thread::spawn(move || {
            let reader = entered
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            release.send(()).unwrap();
            reader
        });
        (control, reader_task)
    }

    #[test]
    fn opening_power_does_not_wait_for_the_actual_profile_read() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let (control, releaser) = held_power();
        let mut area = PowerArea::new(&cx);
        area.control = control;
        let started = std::time::Instant::now();
        let pane = PowerPane::new(cx, &area);
        let elapsed = started.elapsed();
        assert!(
            pane.battery().is_none(),
            "no battery facts exist before the actual read"
        );
        assert!(
            pane.profile().is_none(),
            "no active power profile can be guessed"
        );
        assert!(pane.has_pending_visual_work());
        assert!(
            !pane.needs_animation_frame(),
            "waiting for a service is not an animation"
        );
        assert!(pane.next_wake_deadline().is_none());
        let reader = releaser.join().unwrap();
        assert!(
            elapsed < std::time::Duration::from_millis(150),
            "opening the production Power pane waited for its service: {elapsed:?}, reader={reader:?}"
        );
        assert_ne!(reader, std::thread::current().id());
    }

    #[test]
    fn first_published_power_job_prepares_its_capability_off_the_ui_thread() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let (control, releaser) = held_power();
        app.surface::<SettingsApp, _>(|app| app.power_mut().control = control)
            .unwrap();
        let started = std::time::Instant::now();
        let invocation = app
            .start_effect(&SettingsEffect::from(PowerEffect::SetPowerProfile {
                profile: "Performance".into(),
            }))
            .unwrap();
        let elapsed = started.elapsed();
        assert!(
            !app.harness()
                .job_updates(invocation)
                .iter()
                .any(tilo_actions::InvocationUpdate::is_terminal),
            "the held backend has not acknowledged the real job"
        );
        let reader = releaser.join().unwrap();
        assert!(
            elapsed < std::time::Duration::from_millis(150),
            "the actual published job blocked its mounted UI during preparation: {elapsed:?}, reader={reader:?}"
        );
        assert_ne!(reader, std::thread::current().id());
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(3), |h| {
                    h.job_updates(invocation)
                        .iter()
                        .any(tilo_actions::InvocationUpdate::is_terminal)
                })
        );
        assert!(matches!(app.harness().job_updates(invocation).last(),
            Some(tilo_actions::InvocationUpdate::Succeeded { invocation: id, .. }) if *id == invocation));
    }

    #[test]
    fn actual_power_read_wakes_and_adopts_once_without_animation_or_layout_reads() {
        let mut scope = ui::WakeScope::new();
        let (woke, wakes) = std::sync::mpsc::channel();
        scope
            .bind(move |_| {
                let _ = woke.send(());
            })
            .unwrap();
        let _scope = scope.enter();
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let (control, releaser) = held_power();
        let mut area = PowerArea::new(&cx);
        area.control = control.clone();
        let pane = PowerPane::new(cx, &area);
        assert!(pane.battery().is_none());
        assert!(pane.profile().is_none());
        assert_eq!(area.statuses().len(), 2);
        assert_ne!(releaser.join().unwrap(), std::thread::current().id());
        wakes
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        let mut harness = ui::headless::FrameHarness::for_root(
            ui::State::new(),
            pane,
            (1200, 1000),
            Theme::dark(),
        );
        assert!(harness.wait_until(ui::time::Duration::from_secs(3), |h| {
            !h.root::<PowerPane>().has_pending_visual_work()
        }));
        assert_eq!(
            harness.root::<PowerPane>().battery(),
            Some(&sample_battery())
        );
        assert_eq!(
            harness.root::<PowerPane>().profile(),
            Some(PowerProfile::Balanced)
        );
        assert!(area.statuses().is_empty());
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            harness.relayout_themed((1160, 1000), theme).unwrap();
            assert_eq!(
                harness.root::<PowerPane>().battery(),
                Some(&sample_battery())
            );
        }
        assert_eq!(
            control
                .profile_reads
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the mounted adoption and retained layout cannot reread the external service"
        );
    }

    struct UnreadableProfiles {
        sample: SamplePower,
        changed: std::sync::atomic::AtomicUsize,
    }

    impl PowerControl for UnreadableProfiles {
        fn battery(&self) -> BatteryInfo {
            self.sample.battery()
        }
        fn devices(&self) -> Vec<os_actions::PowerDevice> {
            self.sample.devices()
        }
        fn history(&self) -> os_actions::BatteryHistory {
            self.sample.history()
        }
        fn profiles(
            &self,
        ) -> (
            Result<PowerProfile, String>,
            Result<Vec<PowerProfile>, String>,
        ) {
            (
                Err("profile service unavailable".into()),
                Err("profile service unavailable".into()),
            )
        }
        fn set_profile(&self, profile: PowerProfile) -> Result<(), String> {
            self.changed
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.sample.set_profile(profile)
        }
        fn charge_cap(&self) -> Option<ChargeCapCapability> {
            self.sample.charge_cap()
        }
        fn set_charge_limit(&self, percent: u32) -> Result<ChargeCapCapability, String> {
            self.sample.set_charge_limit(percent)
        }
    }

    #[test]
    fn unreadable_power_capability_fails_the_actual_job_without_guessing_a_profile() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let control = Arc::new(UnreadableProfiles {
            sample: SamplePower::default(),
            changed: std::sync::atomic::AtomicUsize::new(0),
        });
        app.surface::<SettingsApp, _>(|app| app.power_mut().control = control.clone())
            .unwrap();
        let invocation = app
            .start_effect(&SettingsEffect::from(PowerEffect::SetPowerProfile {
                profile: "Performance".into(),
            }))
            .unwrap();
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(3), |h| {
                    h.job_updates(invocation)
                        .iter()
                        .any(tilo_actions::InvocationUpdate::is_terminal)
                })
        );
        assert!(matches!(app.harness().job_updates(invocation).last(),
            Some(tilo_actions::InvocationUpdate::Failed { invocation: id, .. }) if *id == invocation));
        assert_eq!(control.changed.load(std::sync::atomic::Ordering::SeqCst), 0);
        app.effect(&SettingsEffect::from(
            crate::effects::NavigationEffect::OpenPane {
                pane: "power".into(),
            },
        ))
        .unwrap();
        assert!(!app.harness().targets().contains_key("power.profile"));
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains("profile service unavailable")
        );
        app.surface::<SettingsApp, _>(|app| {
            let snapshot = app.power().state.get().unwrap();
            assert!(snapshot.profile_unread);
            assert!(snapshot.available_profiles.is_empty());
            assert_eq!(snapshot.battery, sample_battery());
        })
        .unwrap();
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
            let pane = PowerPane::new(cx.clone(), &PowerArea::new(&cx));
            let mut harness = ui::headless::FrameHarness::for_root(
                ui::State::new(),
                pane,
                (1200, 700),
                Theme::dark(),
            );
            assert!(harness.wait_until(ui::time::Duration::from_secs(3), |h| {
                !h.root::<PowerPane>().has_pending_visual_work()
            }));
            assert_eq!(
                harness.root::<PowerPane>().battery(),
                Some(&sample_battery())
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

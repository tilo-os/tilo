//! Power pane — battery status, the active power profile, and session-idle
//! preferences. Every surface here is real or honestly absent.

mod history_chart;

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use os_actions::{BatteryInfo, ChargeCapBackend, ChargeCapCapability, PowerProfile};
use provider_settings_ui::bound::{Setting, SettingRows as _};
use ui::LocalState;
use ui::framework::{Actions, EffectAnswer, EffectCx, Effected, ViewCx};
use ui::tilo_resource;
use ui::widget::{Element, Widget};

use history_chart::{HistoryChart, Span};

use crate::area::AreaContext;
use crate::device_read::Reading;
use crate::effects::effect;
use crate::i18n::{english, t, t_args};
use crate::prefs::{PowerActionPref, PowerPrefs};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    Item, SettingsSection, block, error_caption, note, row, rows, section, thing_row, value_row,
};
use ui::i18n::ArgValue;
use ui::lock::MutexRecover as _;

crate::effects::effect_family!(PowerEffect => answer);

crate::area::pane_area! {
    PowerArea;
    fn warm(&self) {
        PowerArea::warm(self);
    }
    fn poll(&self) {
        PowerArea::poll(self);
    }
    fn first_read(&self) -> bool {
        PowerArea::first_read(self)
    }
}

pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&PowerArea> {
    app.areas().find()
}

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

fn profile_suspend_field(profile: PowerProfile) -> tilo_resource::Path<PowerPrefs, Option<u32>> {
    let paths = PowerPrefs::paths();
    match profile {
        PowerProfile::Saver => paths.saver_suspend_minutes,
        PowerProfile::Balanced => paths.balanced_suspend_minutes,
        PowerProfile::Performance => paths.performance_suspend_minutes,
    }
}

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

pub trait PowerControl: Send + Sync {
    fn battery(&self) -> BatteryInfo;
    fn devices(&self) -> Vec<os_actions::PowerDevice>;
    fn history(&self) -> os_actions::BatteryHistory;
    fn now_minute(&self) -> u64 {
        tilo_clock::unix_now() / 60
    }
    fn profiles(
        &self,
    ) -> (
        Result<PowerProfile, String>,
        Result<Vec<PowerProfile>, String>,
    );
    fn set_profile(&self, profile: PowerProfile) -> Result<(), String>;
    fn charge_cap(&self) -> Option<ChargeCapCapability>;
    fn set_charge_limit(&self, percent: u32) -> Result<ChargeCapCapability, String>;
    fn follow(&self, _changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any>> {
        None
    }
}

pub struct SystemPower;

impl SystemPower {
    fn battery() -> Result<ui::collaboration::SessionResource, String> {
        crate::device_read::off_ui_thread();
        ui::collaboration::SessionResource::open(tilo_session_power::BATTERY)
    }

    fn reading(resource: &ui::collaboration::SessionResource) -> Option<BatteryReading> {
        let Some(Ok(reading)) = resource.latest() else {
            return None;
        };
        serde_json::from_str(&reading).ok()
    }
}

#[derive(serde::Deserialize)]
struct BatteryReading {
    #[serde(default)]
    charge_limit: Option<ChargeCapCapability>,
}

const POWER_SERVICES: &[&str] = &[
    "org.freedesktop.UPower",
    "org.freedesktop.UPower.PowerProfiles",
];

impl PowerControl for SystemPower {
    fn follow(&self, changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any>> {
        match os_actions::bus_follow::follow_system_services(POWER_SERVICES, changed) {
            Ok(follow) => Some(Box::new(follow)),
            Err(error) => {
                tracing::warn!(target: "tilo::settings", %error, "the power services' changes are not followed");
                None
            }
        }
    }

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
        let resource = Self::battery().ok()?;
        Self::reading(&resource)?.charge_limit
    }

    fn set_charge_limit(&self, percent: u32) -> Result<ChargeCapCapability, String> {
        let resource = Self::battery()?;
        let arguments = serde_json::json!({ "percent": percent });
        let serde_json::Value::Object(arguments) = arguments else {
            return Err("the action's arguments are not an object".to_owned());
        };
        resource.invoke("set-charge-limit", &arguments)?;
        Self::reading(&Self::battery()?)
            .and_then(|reading| reading.charge_limit)
            .ok_or_else(|| "the pack no longer reports a charge limit".to_owned())
    }
}

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

fn sample_history(now: u64) -> os_actions::BatteryHistory {
    const LIMIT_PERCENT: u8 = 80;
    let start = now - 7 * 24 * 60;
    let mut points = Vec::new();
    let mut capacity: f64 = 70.0;
    let mut minute = start.div_ceil(15) * 15;
    while minute <= now {
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

#[derive(Clone)]
struct PowerState {
    battery: BatteryInfo,
    devices: Vec<os_actions::PowerDevice>,
    history: os_actions::BatteryHistory,
    now_minute: u64,
    profile: PowerProfile,
    available_profiles: Vec<PowerProfile>,
    profiles_unavailable: Option<String>,
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Notices {
    profile: Option<String>,
    charge: Option<String>,
}

pub struct PowerArea {
    control: Arc<dyn PowerControl>,
    state: Reading<PowerState>,
    notices: LocalState<Notices>,
    span: LocalState<usize>,
    following: std::cell::RefCell<std::rc::Weak<crate::watch::Counted>>,
    changes_seen: std::cell::Cell<Option<u64>>,
    shown: LocalState<u64>,
    seen: std::cell::RefCell<Option<(u64, Notices, usize)>>,
}

impl PowerArea {
    pub(crate) fn warm(&self) {
        if self.state.get().is_none() {
            self.state.refresh();
        }
    }

    pub fn new(cx: &AreaContext) -> Self {
        let control: Arc<dyn PowerControl> = if cx.devices {
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
            following: std::cell::RefCell::default(),
            changes_seen: std::cell::Cell::new(None),
            shown: LocalState::new(0),
            seen: std::cell::RefCell::new(None),
        }
    }

    pub(crate) fn poll(&self) {
        if let Some(following) = self.following.borrow().upgrade() {
            let changes = following.changes();
            if self.changes_seen.replace(changes) != changes {
                self.state.refresh();
            }
        }
        let now = (self.state.revision(), self.notices.get(), self.span.get());
        let mut seen = self.seen.borrow_mut();
        if seen.as_ref() != Some(&now) {
            *seen = Some(now);
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    pub(crate) fn first_read(&self) -> bool {
        self.following.borrow().upgrade().is_some()
            && (self.state.get().is_none()
                || self.seen.borrow().as_ref().map(|seen| seen.0) != Some(self.state.revision()))
    }

    fn follow(&self) -> (std::rc::Rc<crate::watch::Counted>, bool) {
        if let Some(held) = self.following.borrow().upgrade() {
            return (held, false);
        }
        let control = Arc::clone(&self.control);
        let started = crate::watch::shared(&mut self.following.borrow_mut(), move || {
            crate::watch::Counted::start(|changed| control.follow(changed))
        });
        (started, true)
    }

    fn changed(&self, change: impl FnOnce(&mut PowerState)) {
        if self.state.get().is_some() {
            self.state.update(change);
        } else {
            self.state.refresh();
        }
    }
}

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

pub(crate) fn answer(
    app: &SettingsApp,
    effect: PowerEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let not = |value: &str| format!("`{value}` is not offered");
    match effect {
        PowerEffect::SetPowerProfile { profile } => {
            let profile = profile_from_label(&profile).ok_or_else(|| not(&profile))?;
            let area = crate::panes::power::area(app).ok_or_else(crate::area::missing)?;
            let unsupported =
                move || format!("{} is not supported by this device", profile_label(profile));
            let offered = area
                .state
                .get()
                .map(|state| state.available_profiles.contains(&profile));
            if offered == Some(false) {
                return Err(unsupported());
            }
            let control = Arc::clone(&area.control);
            Ok(app.job(
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
                |app: &SettingsApp, outcome: Result<PowerProfile, String>, _| {
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
            let offered = area
                .state
                .get()
                .map(|state| offers(state.charge_cap.as_ref()));
            if offered == Some(false) {
                return Err(not_offered());
            }
            let control = Arc::clone(&area.control);
            Ok(app.job(
                cx,
                move || {
                    if offered.is_none() && !offers(control.charge_cap().as_ref()) {
                        return Err(not_offered());
                    }
                    control.set_charge_limit(percent).map(|cap| (percent, cap))
                },
                |app: &SettingsApp, outcome: Result<(u32, ChargeCapCapability), String>, _| {
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

pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    let (following, started) = held.follow();
    if started || held.state.get().is_none() {
        held.state.refresh();
    }
    held.changes_seen.set(following.changes());
    let shown = held.shown.clone();
    let actions = cx.actions();
    let app = Rc::clone(app);
    let prefs = cx.settings::<PowerPrefs>();
    cx.record(&prefs, move |cx| {
        let (app, actions, shown, following) = (
            Rc::clone(&app),
            actions.clone(),
            shown.clone(),
            Rc::clone(&following),
        );
        cx.switch(&cx.value(|prefs: &PowerPrefs| *prefs), move |cx, prefs| {
            let (app, actions, prefs) = (Rc::clone(&app), actions.clone(), *prefs);
            let following = Rc::clone(&following);
            cx.switch(&shown, move |cx, _| {
                let _followed = &following;
                match area(&app) {
                    Some(area) => page(cx, &actions, area, &prefs),
                    None => provider_settings_ui::widgets::page(Vec::new()),
                }
            })
        })
    })
}

fn page(
    cx: &ViewCx<PowerPrefs>,
    actions: &Actions<Settings>,
    area: &PowerArea,
    prefs: &PowerPrefs,
) -> Element {
    let state = area.state.get();
    let notices = area.notices.get();
    let mut cards = match &state {
        Some(state) => {
            let mut cards = battery_card(cx, actions, area, state, &notices);
            cards.extend(devices_card(state));
            cards.push(profile_card(cx, actions, state, &notices, prefs));
            cards
        }
        None => vec![section(
            &t("power-battery-title"),
            rows(vec![note(&t("power-reading"))]),
        )],
    };
    let has_battery = state.as_ref().is_some_and(|state| state.battery.present);
    cards.push(idle_card(cx, prefs, has_battery));
    cards.push(actions_card(cx, prefs));
    provider_settings_ui::widgets::page(cards)
}

fn battery_card(
    cx: &ViewCx<PowerPrefs>,
    actions: &Actions<Settings>,
    area: &PowerArea,
    state: &PowerState,
    notices: &Notices,
) -> Vec<SettingsSection> {
    if !state.battery.present {
        return vec![section(
            &t("power-source-title"),
            rows(vec![
                value_row(&t("power-source"), &t("power-source-ac")),
                note(&t("power-source-caption")),
            ]),
        )];
    }
    let span = area.span.get();
    let mut battery_rows: Vec<Item> = vec![
        block(battery_head(&state.battery, &area.span, span)),
        block(Box::new(HistoryChart::new(
            &state.history.points,
            Span::ALL.get(span).copied().unwrap_or(Span::Day),
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
            cx.setting(t("power-charge-cap"))
                .id("power.charge_limit")
                .choice_effect(
                    choices,
                    &LocalState::new(current),
                    actions,
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
    if let Some(notice) = &notices.charge {
        battery_rows.push(note(notice));
    }
    vec![section(&t("power-battery-title"), rows(battery_rows))]
}

fn battery_head(battery: &BatteryInfo, spans: &LocalState<usize>, span: usize) -> Box<dyn Widget> {
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
        span,
    )
    .with_id("power.history.span")
    .with_accessible_name(t("power-history-span"))
    .bind_selection(spans);
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

fn profile_card(
    cx: &ViewCx<PowerPrefs>,
    actions: &Actions<Settings>,
    state: &PowerState,
    notices: &Notices,
    prefs: &PowerPrefs,
) -> SettingsSection {
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
            cx.setting(t("power-profile"))
                .id("power.profile")
                .choice_effect(
                    profile_opts,
                    &LocalState::new(current),
                    actions,
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
    if prefs.timers().profiles_differ() && !state.profile_unread {
        let field = profile_suspend_field(state.profile);
        profile_rows.push(
            suspend_row(
                cx.setting(t("power-suspend-after"))
                    .id("power.profile_suspend"),
                prefs,
            )
            .among(
                field,
                SUSPEND_OPTIONS.iter().map(|(_, minutes)| Some(*minutes)),
                |minutes: &Option<u32>| suspend_label(minutes.unwrap_or_default()),
            ),
        );
    }
    if let Some(notice) = &notices.profile {
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

fn timeout(
    row: Setting<'_, PowerPrefs>,
    options: &'static [(&'static str, u32)],
    label: fn(u32) -> String,
    field: tilo_resource::Path<PowerPrefs, u32>,
) -> Item {
    row.among(
        field,
        options.iter().map(|(_, minutes)| *minutes),
        move |minutes: &u32| label(*minutes),
    )
}

fn suspend_row<'a>(row: Setting<'a, PowerPrefs>, prefs: &PowerPrefs) -> Setting<'a, PowerPrefs> {
    let on = prefs.auto_suspend;
    let row = row.enabled(on);
    if on {
        row
    } else {
        row.described(t("power-suspend-needs-idle"))
    }
}

fn suspend_timer(
    row: Setting<'_, PowerPrefs>,
    prefs: &PowerPrefs,
    minutes: u32,
    verb: impl ui::tilo_resource::Binds<PowerPrefs, String>,
) -> Item {
    let options: Vec<String> = SUSPEND_OPTIONS
        .iter()
        .map(|(_, minutes)| suspend_label(*minutes))
        .collect();
    let shown = suspend_label(minutes);
    suspend_row(row, prefs).choice_verb(options, move |_| shown.clone(), verb)
}

fn suspend_timers(cx: &ViewCx<PowerPrefs>, prefs: &PowerPrefs, has_battery: bool) -> Vec<Item> {
    let timers = prefs.timers();
    if timers.profiles_differ() {
        return vec![note(&t("power-suspend-in-profile"))];
    }
    if !has_battery {
        return vec![suspend_timer(
            cx.setting(t("power-suspend-after")).id("power.suspend"),
            prefs,
            timers.plugged_in(),
            PowerPrefs::SUSPEND_PLUGGED_IN.map(|label: String| suspend_minutes_of(&label)),
        )];
    }
    vec![
        suspend_timer(
            cx.setting(t("power-suspend-battery"))
                .id("power.suspend_battery"),
            prefs,
            timers.on_battery(),
            PowerPrefs::SUSPEND_ON_BATTERY.map(|label: String| suspend_minutes_of(&label)),
        ),
        suspend_timer(
            cx.setting(t("power-suspend-plugged")).id("power.suspend"),
            prefs,
            timers.plugged_in(),
            PowerPrefs::SUSPEND_PLUGGED_IN.map(|label: String| suspend_minutes_of(&label)),
        ),
    ]
}

fn idle_card(cx: &ViewCx<PowerPrefs>, prefs: &PowerPrefs, has_battery: bool) -> SettingsSection {
    let paths = PowerPrefs::paths();
    let mut body = vec![auto_suspend(cx)];
    body.extend(suspend_timers(cx, prefs, has_battery));
    body.extend([
        timeout(
            cx.setting(t("power-blank")).id("power.blank"),
            BLANK_OPTIONS,
            blank_label,
            paths.blank_minutes,
        ),
        timeout(
            cx.setting(t("power-lock-after")).id("power.lock"),
            LOCK_OPTIONS,
            lock_label,
            paths.lock_minutes,
        ),
        lock_on_sleep(cx),
    ]);
    section(&t("power-idle-title"), rows(body))
}

fn action_choice(
    row: Setting<'_, PowerPrefs>,
    field: tilo_resource::Path<PowerPrefs, PowerActionPref>,
) -> Item {
    row.among(
        field,
        ACTION_OPTIONS.iter().copied(),
        |action: &PowerActionPref| action_shown(*action),
    )
}

fn actions_card(cx: &ViewCx<PowerPrefs>, prefs: &PowerPrefs) -> SettingsSection {
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
    let paths = PowerPrefs::paths();
    section(
        &t("power-behaviour-title"),
        rows(vec![
            action_choice(
                cx.setting(t("power-lid")).id("power.lid"),
                paths.lid_close_action,
            ),
            action_choice(
                cx.setting(t("power-power-key")).id("power.power_key"),
                paths.power_key_action,
            ),
            action_choice(
                cx.setting(t("power-critical")).id("power.critical"),
                paths.critical_action,
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
        let name = d.model.as_deref().unwrap_or(&d.id);
        let details = vec![
            kind,
            t_args(
                "power-device-charge",
                &[("capacity", ArgValue::from(i64::from(d.capacity)))],
            ),
            d.status.clone(),
        ];
        entries.push(row(Box::new(thing_row(
            &format!("power.device.{name}"),
            name,
            &details,
            None,
        ))));
    }
    Some(section(&t("power-devices-title"), rows(entries)))
}

pub(crate) mod values;

pub(crate) fn auto_suspend(cx: &ViewCx<PowerPrefs>) -> Item {
    cx.setting(t("power-suspend-idle"))
        .id("power.auto_suspend")
        .toggle(PowerPrefs::paths().auto_suspend)
}

pub(crate) fn lock_on_sleep(cx: &ViewCx<PowerPrefs>) -> Item {
    cx.setting(t("power-lock-on-sleep"))
        .id("power.lock_on_sleep")
        .described(t("power-lock-on-sleep-caption"))
        .toggle(PowerPrefs::paths().lock_on_sleep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

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

    #[derive(Default)]
    struct FollowedPower {
        sample: SamplePower,
        reads: std::sync::atomic::AtomicUsize,
        follower: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    }

    impl PowerControl for FollowedPower {
        fn battery(&self) -> BatteryInfo {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        fn follow(&self, changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any>> {
            *self.follower.lock_recover() = Some(Arc::from(changed));
            Some(Box::new(()))
        }
    }

    #[test]
    fn the_shown_page_reads_the_hardware_again_when_a_service_reports_a_change() {
        let control = Arc::new(FollowedPower::default());
        let held = Arc::clone(&control);
        crate::area::replace_in_next_window(move |_| {
            PowerArea::over(held as Arc<dyn PowerControl>)
        });
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::Power);
        let reads = || control.reads.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| !frame.contains(&t("power-reading")))),
            "the hardware is read"
        );
        assert_eq!(reads(), 1);
        let changed = control
            .follower
            .lock_recover()
            .clone()
            .expect("the power services are followed while the page shows");

        *control.sample.profile.lock_recover() = PowerProfile::Performance;
        changed();
        let performance = profile_label(PowerProfile::Performance);
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains(&performance))),
            "the hardware is read again"
        );
        assert_eq!(reads(), 2, "one change, one read");
        app.settle();
        assert_eq!(reads(), 2, "nothing reads while nothing changes");
    }

    #[test]
    fn the_battery_shows_time_left_and_its_charge_history() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.harness().resize((1400, 4000)).unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::Power);
        let day = t_args(
            "power-history-name",
            &[("span", ArgValue::from(t("power-history-day")))],
        );
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains(&day))),
            "the day's charge is drawn"
        );
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("72"), "{frame}");
        assert!(
            frame.contains("left"),
            "time left at the rate it drains: {frame}"
        );
        app.with_widget::<crate::window::Inspected, _>(|settings| {
            area(settings).expect("the power area").span.set(1);
        })
        .expect("the Settings window is mounted");
        app.settle();
        let week = t_args(
            "power-history-name",
            &[("span", ArgValue::from(t("power-history-week")))],
        );
        assert!(
            app.harness().inspection_json().unwrap().contains(&week),
            "the chart shows the week once chosen"
        );
    }

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
            let cx = crate::area::AreaContext::in_profile(profile.path().to_path_buf());
            let area = PowerArea::new(&cx);
            area.warm();
            let state = tilo_test_env::wait_for("the hardware is read", || area.state.get());
            assert_eq!(state.battery, sample_battery());
            let prefs = cx.storage.settings::<PowerPrefs>().get();
            assert!(!prefs.auto_suspend);
            assert_eq!(
                prefs.auto_suspend,
                settings_store::power::AUTO_SUSPEND
                    .get_in(&settings_store::profile(profile.path()))
            );
        }
    }

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

    #[test]
    fn a_stalled_power_daemon_never_holds_the_window() {
        ui::i18n::I18nManager::init("en-US");
        let gate = crate::stall::Stall::new();
        let stalled = Arc::clone(&gate);
        crate::area::replace_in_next_window(move |_| {
            PowerArea::over(Arc::new(StalledPower(stalled)))
        });
        let mut app = TestApp::<SettingsShell>::new().unwrap();

        crate::effects::show_pane(&mut app, crate::categories::Category::Power);
        app.harness()
            .poll_frame()
            .expect("the window draws its next frame");
        gate.assert_held("opening the pane");
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains(&t("power-reading")), "{frame}");
        crate::testing::edit_field(&mut app, crate::testing::POWER, "auto_suspend", false)
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

    #[test]
    fn idle_and_button_preferences_write_the_power_section() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        crate::testing::edit_field(&mut app, crate::testing::POWER, "auto_suspend", true).unwrap();
        let edit = |app: &mut TestApp<SettingsShell>, field: &str, value: serde_json::Value| {
            crate::testing::edit_field(app, crate::testing::POWER, field, value)
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
            &format!("settings:{}", crate::testing::POWER),
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

        crate::testing::edit_field(&mut app, crate::testing::POWER, "auto_suspend", true).unwrap();
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
        crate::testing::invoke_section(
            &mut app,
            crate::testing::POWER,
            "set-suspend-on-battery",
            serde_json::json!({ "minutes": 5 }),
        )
        .unwrap();
        crate::testing::invoke_section(
            &mut app,
            crate::testing::POWER,
            "set-suspend-plugged-in",
            serde_json::json!({ "minutes": 60 }),
        )
        .unwrap();
        let timers = app.settings::<PowerPrefs>().get().timers();
        assert_eq!((timers.on_battery(), timers.plugged_in()), (5, 60));
        assert!(
            crate::testing::invoke_section(
                &mut app,
                crate::testing::POWER,
                "set-suspend-on-battery",
                serde_json::json!({ "minutes": 7 }),
            )
            .is_err(),
            "only the offered timers"
        );

        crate::testing::edit_field(&mut app, crate::testing::POWER, "saver_suspend_minutes", 10)
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

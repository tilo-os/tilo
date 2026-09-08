//! Power pane — battery status, the active power profile, and session-idle
//! preferences.
//!
//! Every surface here is real or honestly absent:
//! - **Battery** is read from `/sys/class/power_supply` (`capacity`,
//!   `status`, `cycle_count`) and the AC adapter's `online` flag, so the card
//!   reflects the machine's real charge. A device with no battery (a desktop
//!   under `--shots`) shows an honest "running on AC power" state rather than
//!   a fabricated level.
//! - **Power profile** (Saver / Balanced / Performance) drives
//!   `powerprofilesctl` through the shared `os_actions::Dispatcher`
//!   (`set_power_profile`), the same binding the compositor's quick-settings
//!   uses. The initial selection and the offered choices are sampled from
//!   `powerprofilesctl`; unavailable profiles are never presented as working.
//! - **Automatic suspend** and **screen-blank timeout** are durable
//!   preferences persisted to the shared store ([`crate::prefs::SystemPrefs`]);
//!   the compositor's idle layer reads them.
//
// CRDT keys this pane reads / writes:
//   `power.profile`     — string, power-profile dropdown
//   `power.auto_suspend`— bool, suspend-when-idle toggle
//   `power.blank`       — string, screen-blank timeout dropdown

use std::collections::HashMap;
use std::sync::Arc;

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use os_actions::{BatteryInfo, ChargeCapBackend, ChargeCapCapability, Dispatcher, PowerProfile};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Dropdown, ProgressBar, Sparkline, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::dispatcher::dispatcher_or_real;
use crate::prefs::{PowerActionPref, PowerPrefs, SystemPrefs};
use crate::widgets::{El, caption, card, choice_row, rows, setting_row, value_row};

pub const PROFILE_KEY: &str = "power.profile";
pub const AUTO_SUSPEND_KEY: &str = "power.auto_suspend";
pub const BLANK_KEY: &str = "power.blank";
pub const SUSPEND_KEY: &str = "power.suspend";
pub const LID_KEY: &str = "power.lid";
pub const POWER_KEY_KEY: &str = "power.power_key";
pub const CRITICAL_KEY: &str = "power.critical";
pub const CHARGE_LIMIT_KEY: &str = "power.charge_limit";
pub const SAVER_SUSPEND_KEY: &str = "power.profile_idle.saver";
pub const BALANCED_SUSPEND_KEY: &str = "power.profile_idle.balanced";
pub const PERFORMANCE_SUSPEND_KEY: &str = "power.profile_idle.performance";

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

fn suspend_label(minutes: u32) -> &'static str {
    SUSPEND_OPTIONS
        .iter()
        .find(|(_, m)| *m == minutes)
        .map_or("15 minutes", |(l, _)| *l)
}

fn suspend_from_label(label: &str) -> Option<u32> {
    SUSPEND_OPTIONS
        .iter()
        .find(|(l, _)| *l == label)
        .map(|(_, m)| *m)
}

/// The action options offered for the lid / power-key / critical-battery
/// dropdowns, in display order.
const ACTION_OPTIONS: &[PowerActionPref] = &[
    PowerActionPref::Suspend,
    PowerActionPref::Lock,
    PowerActionPref::Ignore,
];

fn action_options() -> Vec<String> {
    ACTION_OPTIONS
        .iter()
        .map(|a| a.display().to_string())
        .collect()
}

/// The power-profile options, as `(display label, profile)`. The label is what
/// the dropdown shows and stores; the profile is what the dispatcher applies.
const PROFILES: &[(&str, PowerProfile)] = &[
    ("Power Saver", PowerProfile::Saver),
    ("Balanced", PowerProfile::Balanced),
    ("Performance", PowerProfile::Performance),
];

fn profile_label(p: PowerProfile) -> &'static str {
    PROFILES
        .iter()
        .find(|(_, pp)| *pp == p)
        .map_or("Balanced", |(l, _)| *l)
}

fn profile_from_label(label: &str) -> Option<PowerProfile> {
    PROFILES.iter().find(|(l, _)| *l == label).map(|(_, p)| *p)
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

fn blank_label(minutes: u32) -> &'static str {
    BLANK_OPTIONS
        .iter()
        .find(|(_, m)| *m == minutes)
        .map_or("10 minutes", |(l, _)| *l)
}

fn blank_from_label(label: &str) -> Option<u32> {
    BLANK_OPTIONS
        .iter()
        .find(|(l, _)| *l == label)
        .map(|(_, m)| *m)
}

/// Power pane widget.
pub struct PowerPane {
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    battery: BatteryInfo,
    devices: Vec<os_actions::PowerDevice>,
    history: os_actions::BatteryHistory,
    charge_cap: Option<ChargeCapCapability>,
    charge_notice: Option<String>,
    operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    available_profiles: Vec<PowerProfile>,
    profile_notice: Option<String>,
    profile: PowerProfile,
    last_profile: PowerProfile,
    prefs: PowerPrefs,
    last_prefs: PowerPrefs,
    needs_seed: bool,
    commit_context_pending: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for PowerPane {
    fn default() -> Self {
        Self::new()
    }
}

impl PowerPane {
    #[must_use]
    pub fn with_operation_status(
        mut self,
        status: &ui::LocalState<crate::actions::LocalOperationStatus>,
    ) -> Self {
        self.operation_status = status.clone();
        self
    }

    fn operation_finished(&self, percent: u32, result: Result<String, String>) {
        let token = crate::actions::local_operation_token_for(
            crate::actions::POWER_OPERATION_TYPE,
            &crate::actions::PowerOperation::SetChargeLimit { percent },
        );
        if !matches!(
            self.operation_status.get(),
            crate::actions::LocalOperationStatus::Running { token: active } if active == token
        ) {
            return;
        }
        self.operation_status.set(match result {
            Ok(message) => crate::actions::LocalOperationStatus::Succeeded { token, message },
            Err(message) => crate::actions::LocalOperationStatus::Failed { token, message },
        });
    }

    pub fn new() -> Self {
        let profile_result = os_actions::get_power_profile();
        let available_result = os_actions::available_power_profiles();
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
        let profile_notice = match (profile_result.err(), available_result.err()) {
            (None, None) => None,
            (profile_error, capability_error) => {
                let mut details = Vec::new();
                if let Some(error) = profile_error {
                    details.push(error);
                }
                if let Some(error) = capability_error {
                    details.push(format!("capabilities: {error}"));
                }
                Some(format!(
                    "Power-profile service is unavailable: {}.",
                    details.join("; ")
                ))
            }
        };
        let battery = BatteryInfo::from_system();
        // The shell-owned UPower monitor is the single history writer.
        // Settings only reads the canonical atomic file.
        let history = os_actions::load_battery_history();
        Self {
            dispatcher: dispatcher_or_real(None),
            battery,
            devices: os_actions::power_devices(),
            history,
            charge_cap: os_actions::charge_capability(),
            charge_notice: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            available_profiles,
            profile_notice,
            profile,
            last_profile: profile,
            prefs: SystemPrefs::load().power,
            last_prefs: PowerPrefs::default(),
            needs_seed: true,
            commit_context_pending: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test / `--shots` constructor: inject the dispatcher, a deterministic
    /// battery snapshot, the starting profile, and prefs.
    pub fn with_parts(
        dispatcher: Arc<dyn Dispatcher + Send + Sync>,
        battery: BatteryInfo,
        profile: PowerProfile,
        prefs: PowerPrefs,
    ) -> Self {
        let charge_cap = battery.charge_limit.map(|current| ChargeCapCapability {
            backend: ChargeCapBackend::PowerSupply,
            current,
            choices: vec![60, 70, 80, 90, 100],
            supports_start_threshold: false,
        });
        Self {
            dispatcher,
            battery,
            devices: Vec::new(),
            history: os_actions::BatteryHistory::default(),
            charge_cap,
            charge_notice: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            available_profiles: PROFILES.iter().map(|(_, profile)| *profile).collect(),
            profile_notice: None,
            profile,
            last_profile: profile,
            prefs,
            last_prefs: prefs,
            needs_seed: true,
            commit_context_pending: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn battery(&self) -> &BatteryInfo {
        &self.battery
    }

    pub fn profile(&self) -> PowerProfile {
        self.profile
    }

    pub fn prefs(&self) -> &PowerPrefs {
        &self.prefs
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut cards = vec![
            self.battery_card(),
            self.profile_card(),
            self.idle_card(),
            self.actions_card(),
        ];
        if let Some(device_card) = self.devices_card() {
            cards.insert(1, device_card);
        }
        crate::widgets::cards(cards)
    }

    fn battery_card(&self) -> El {
        if !self.battery.present {
            return card(
                "Power source",
                rows(vec![
                    value_row("Source", "AC power"),
                    caption(
                        "This device has no battery — it runs on mains power, so \
                         charge and battery-saver settings don't apply.",
                    ),
                ]),
            );
        }
        let mut battery_rows: Vec<El> = vec![
            Box::new(ProgressBar::new(self.battery.capacity as f32 / 100.0)),
            value_row("Charge", &format!("{}%", self.battery.capacity)),
            value_row("Status", &self.battery.summary()),
        ];
        if let Some(cycles) = self.battery.cycle_count {
            battery_rows.push(value_row("Charge cycles", &cycles.to_string()));
        }
        if let Some(health) = self.battery.health_percent() {
            battery_rows.push(value_row("Battery health", &format!("{health}%")));
            battery_rows.push(caption(self.battery.health_explanation()));
        } else {
            battery_rows.push(value_row("Battery health", "Not reported by this device"));
        }
        let trend: Vec<f32> = self
            .history
            .points
            .iter()
            .map(|p| p.capacity as f32)
            .collect();
        if trend.len() >= 2 {
            battery_rows.push(Box::new(
                Sparkline::new(trend, Theme::dark().accent_primary)
                    .with_max(100.0)
                    .with_size(320.0, 56.0),
            ));
            battery_rows.push(caption("Charge history · last 30 days · sampled at most every 15 minutes. Device names and serial numbers are never stored."));
        }
        if let Some(capability) = &self.charge_cap {
            let choices: Vec<String> = capability
                .choices
                .iter()
                .map(|limit| format!("{limit}%"))
                .collect();
            battery_rows.push(choice_row(
                "Charge cap",
                Box::new(
                    Dropdown::new(choices, CHARGE_LIMIT_KEY)
                        .with_id("power.charge_limit")
                        .with_placeholder(&format!("{}%", capability.current)),
                ),
            ));
            let detail = match capability.backend {
                ChargeCapBackend::ConservationMode => {
                    "This laptop exposes a discrete conservation mode, so only its real hardware choices are offered."
                }
                ChargeCapBackend::HuaweiWmi => {
                    "The platform uses paired start/end thresholds; qOS preserves a safe charging window."
                }
                ChargeCapBackend::PowerSupply if capability.supports_start_threshold => {
                    "The kernel exposes start and stop thresholds; qOS adjusts the start only when required by a lower cap."
                }
                ChargeCapBackend::PowerSupply => {
                    "The kernel exposes a hardware stop threshold. A lower cap can reduce long-term battery wear."
                }
            };
            battery_rows.push(caption(detail));
        } else {
            battery_rows.push(value_row("Charge cap", "Not supported by this hardware"));
        }
        if let Some(notice) = &self.charge_notice {
            battery_rows.push(caption(notice));
        }
        card("Battery", rows(battery_rows))
    }

    fn profile_card(&self) -> El {
        let profile_opts: Vec<String> = self
            .available_profiles
            .iter()
            .map(|profile| profile_label(*profile).to_string())
            .collect();
        let mut profile_rows = if profile_opts.is_empty() {
            vec![value_row("Profile", "Power-profile service unavailable")]
        } else {
            vec![choice_row(
                "Profile",
                Box::new(
                    Dropdown::new(profile_opts, PROFILE_KEY)
                        .with_id("power.profile")
                        .with_placeholder(profile_label(self.profile)),
                ),
            )]
        };
        profile_rows.push(caption(
            "Only modes supported by this device are shown. Balanced trades performance for battery life; Power Saver favours runtime; Performance favours speed.",
        ));
        if let Some(notice) = &self.profile_notice {
            profile_rows.push(caption(notice));
        }
        card("Power profile", rows(profile_rows))
    }

    fn idle_card(&self) -> El {
        let blank_opts: Vec<String> = BLANK_OPTIONS.iter().map(|(l, _)| l.to_string()).collect();
        let suspend_opts: Vec<String> =
            SUSPEND_OPTIONS.iter().map(|(l, _)| l.to_string()).collect();
        card(
            "Automatic power saving",
            rows(vec![
                setting_row(
                    "Suspend when idle",
                    Box::new(
                        Toggle::new("settings.power.auto_suspend", AUTO_SUSPEND_KEY)
                            .with_id("power.auto_suspend"),
                    ),
                ),
                choice_row(
                    "Suspend after",
                    Box::new(
                        Dropdown::new(suspend_opts.clone(), SUSPEND_KEY)
                            .with_id("power.suspend")
                            .with_placeholder(suspend_label(self.prefs.suspend_minutes)),
                    ),
                ),
                choice_row(
                    "Blank screen when idle",
                    Box::new(
                        Dropdown::new(blank_opts, BLANK_KEY)
                            .with_id("power.blank")
                            .with_placeholder(blank_label(self.prefs.blank_minutes)),
                    ),
                ),
                caption("Idle timeouts are applied by the compositor's power layer."),
                choice_row(
                    "Power Saver suspend",
                    Box::new(
                        Dropdown::new(suspend_opts.clone(), SAVER_SUSPEND_KEY)
                            .with_placeholder(suspend_label(self.prefs.saver_suspend_minutes)),
                    ),
                ),
                choice_row(
                    "Balanced suspend",
                    Box::new(
                        Dropdown::new(suspend_opts.clone(), BALANCED_SUSPEND_KEY)
                            .with_placeholder(suspend_label(self.prefs.balanced_suspend_minutes)),
                    ),
                ),
                choice_row(
                    "Performance suspend",
                    Box::new(
                        Dropdown::new(suspend_opts, PERFORMANCE_SUSPEND_KEY).with_placeholder(
                            suspend_label(self.prefs.performance_suspend_minutes),
                        ),
                    ),
                ),
                caption(
                    "The active power profile selects its own idle timeout in the compositor; changing profile never creates a second policy owner.",
                ),
            ]),
        )
    }

    fn actions_card(&self) -> El {
        card(
            "Power behaviour",
            rows(vec![
                choice_row(
                    "When the lid is closed",
                    Box::new(
                        Dropdown::new(action_options(), LID_KEY)
                            .with_id("power.lid")
                            .with_placeholder(self.prefs.lid_close_action.display()),
                    ),
                ),
                choice_row(
                    "When the power key is pressed",
                    Box::new(
                        Dropdown::new(action_options(), POWER_KEY_KEY)
                            .with_id("power.power_key")
                            .with_placeholder(self.prefs.power_key_action.display()),
                    ),
                ),
                choice_row(
                    "When the battery is critically low",
                    Box::new(
                        Dropdown::new(action_options(), CRITICAL_KEY)
                            .with_id("power.critical")
                            .with_placeholder(self.prefs.critical_action.display()),
                    ),
                ),
                caption(&format!(
                    "You'll be warned at {}% and this action runs at {}%.",
                    self.prefs.low_battery_percent, self.prefs.critical_battery_percent
                )),
            ]),
        )
    }

    fn devices_card(&self) -> Option<El> {
        if self.devices.len() <= 1 {
            return None;
        }
        let mut entries = Vec::new();
        for d in &self.devices {
            let kind = match d.kind {
                os_actions::PowerSupplyKind::Battery => "Battery",
                os_actions::PowerSupplyKind::Ups => "UPS",
            };
            entries.push(value_row(
                d.model.as_deref().unwrap_or(&d.id),
                &format!("{kind} · {}% · {}", d.capacity, d.status),
            ));
        }
        Some(card("Power devices", rows(entries)))
    }

    fn persist(&self) {
        let mut p = SystemPrefs::load();
        p.power = self.prefs;
        if let Err(e) = p.save() {
            warn!(target: "qos::settings", op = "power_prefs_save", error = %e, "failed to persist power prefs");
        }
    }

    fn seed(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !self.needs_seed {
            return;
        }
        let _ = state.put(root, PROFILE_KEY, profile_label(self.profile));
        let _ = state.put(
            root,
            AUTO_SUSPEND_KEY,
            automerge::ScalarValue::Boolean(self.prefs.auto_suspend),
        );
        let _ = state.put(root, SUSPEND_KEY, suspend_label(self.prefs.suspend_minutes));
        let _ = state.put(root, BLANK_KEY, blank_label(self.prefs.blank_minutes));
        let _ = state.put(
            root,
            SAVER_SUSPEND_KEY,
            suspend_label(self.prefs.saver_suspend_minutes),
        );
        let _ = state.put(
            root,
            BALANCED_SUSPEND_KEY,
            suspend_label(self.prefs.balanced_suspend_minutes),
        );
        let _ = state.put(
            root,
            PERFORMANCE_SUSPEND_KEY,
            suspend_label(self.prefs.performance_suspend_minutes),
        );
        let _ = state.put(root, LID_KEY, self.prefs.lid_close_action.display());
        let _ = state.put(root, POWER_KEY_KEY, self.prefs.power_key_action.display());
        let _ = state.put(root, CRITICAL_KEY, self.prefs.critical_action.display());
        if let Some(capability) = &self.charge_cap {
            let _ = state.put(root, CHARGE_LIMIT_KEY, format!("{}%", capability.current));
        }
        self.last_prefs = self.prefs;
        self.needs_seed = false;
    }

    /// Power profile → dispatch on change, guarded so it doesn't re-fire.
    fn handle_profile_change(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let Some(label) = read_str(state, root, PROFILE_KEY) else {
            return;
        };
        let Some(p) = profile_from_label(&label) else {
            return;
        };
        if p == self.last_profile {
            return;
        }
        if self.available_profiles.contains(&p) {
            match self.dispatcher.set_power_profile(p) {
                Ok(()) => {
                    self.last_profile = p;
                    self.profile = p;
                    self.profile_notice = Some(format!("{} is now active.", profile_label(p)));
                }
                Err(e) => {
                    let _ = state.put(root, PROFILE_KEY, profile_label(self.last_profile));
                    self.profile_notice = Some(format!("Could not change the power profile: {e}"));
                    warn!(target: "qos::settings", op = "set_power_profile", error = %e, "power pane dispatch failed");
                }
            }
        } else {
            let _ = state.put(root, PROFILE_KEY, profile_label(self.last_profile));
            self.profile_notice = Some(format!(
                "{} is not supported by this device.",
                profile_label(p)
            ));
        }
    }

    fn handle_charge_limit(&mut self, state: &AutoCommit, root: &ObjId) {
        let Some(label) = read_str(state, root, CHARGE_LIMIT_KEY) else {
            return;
        };
        let Some(limit) = label.strip_suffix('%').and_then(|s| s.parse::<u32>().ok()) else {
            return;
        };
        if !self
            .charge_cap
            .as_ref()
            .is_some_and(|cap| cap.current != limit && cap.choices.contains(&limit))
        {
            return;
        }
        match os_actions::set_charge_limit_checked(limit) {
            Ok(actual) => {
                self.battery.charge_limit = Some(limit);
                self.charge_cap = Some(actual);
                self.charge_notice = Some(format!("Hardware confirmed a {limit}% charge cap."));
                self.operation_finished(
                    limit,
                    Ok(format!("hardware confirmed a {limit}% charge cap")),
                );
            }
            Err(e) => {
                self.charge_notice = Some(format!("Charge cap was not applied: {e}"));
                self.operation_finished(limit, Err(format!("Charge cap was not applied: {e}")));
                warn!(target: "qos::settings", op = "set_charge_limit", error = %e, "battery cap failed");
            }
        }
    }

    /// Auto-suspend, screen-blank and the three per-profile idle-suspend
    /// timeouts → persisted on change.
    fn handle_idle_prefs(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Some(b) = read_bool(state, root, AUTO_SUSPEND_KEY)
            && b != self.last_prefs.auto_suspend
        {
            self.last_prefs.auto_suspend = b;
            self.prefs.auto_suspend = b;
            self.persist();
        }

        if let Some(label) = read_str(state, root, BLANK_KEY)
            && let Some(m) = blank_from_label(&label)
            && m != self.last_prefs.blank_minutes
        {
            self.last_prefs.blank_minutes = m;
            self.prefs.blank_minutes = m;
            self.persist();
        }

        if let Some(label) = read_str(state, root, SUSPEND_KEY)
            && let Some(m) = suspend_from_label(&label)
            && m != self.last_prefs.suspend_minutes
        {
            self.last_prefs.suspend_minutes = m;
            self.prefs.suspend_minutes = m;
            self.persist();
        }
        for (key, current, set) in [
            (
                SAVER_SUSPEND_KEY,
                self.last_prefs.saver_suspend_minutes,
                0u8,
            ),
            (
                BALANCED_SUSPEND_KEY,
                self.last_prefs.balanced_suspend_minutes,
                1u8,
            ),
            (
                PERFORMANCE_SUSPEND_KEY,
                self.last_prefs.performance_suspend_minutes,
                2u8,
            ),
        ] {
            if let Some(label) = read_str(state, root, key)
                && let Some(minutes) = suspend_from_label(&label)
                && minutes != current
            {
                match set {
                    0 => {
                        self.last_prefs.saver_suspend_minutes = minutes;
                        self.prefs.saver_suspend_minutes = minutes;
                    }
                    1 => {
                        self.last_prefs.balanced_suspend_minutes = minutes;
                        self.prefs.balanced_suspend_minutes = minutes;
                    }
                    _ => {
                        self.last_prefs.performance_suspend_minutes = minutes;
                        self.prefs.performance_suspend_minutes = minutes;
                    }
                }
                self.persist();
            }
        }
    }

    /// Lid-close, power-key and critical-battery actions → persisted on
    /// change.
    fn handle_action_prefs(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Some(label) = read_str(state, root, LID_KEY)
            && let Some(a) = PowerActionPref::from_display(&label)
            && a != self.last_prefs.lid_close_action
        {
            self.last_prefs.lid_close_action = a;
            self.prefs.lid_close_action = a;
            self.persist();
        }

        if let Some(label) = read_str(state, root, POWER_KEY_KEY)
            && let Some(a) = PowerActionPref::from_display(&label)
            && a != self.last_prefs.power_key_action
        {
            self.last_prefs.power_key_action = a;
            self.prefs.power_key_action = a;
            self.persist();
        }

        if let Some(label) = read_str(state, root, CRITICAL_KEY)
            && let Some(a) = PowerActionPref::from_display(&label)
            && a != self.last_prefs.critical_action
        {
            self.last_prefs.critical_action = a;
            self.prefs.critical_action = a;
            self.persist();
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(PowerPane::new())
}

impl Widget for PowerPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.seed(state, root);
        self.handle_profile_change(state, root);
        self.handle_charge_limit(state, root);
        self.handle_idle_prefs(state, root);
        self.handle_action_prefs(state, root);

        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
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
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let was_handled = *handled;
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cn,
                taffy,
            );
        }
        self.commit_context_pending |= !was_handled && *handled;
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn take_commit_context(&mut self) -> Option<ui::qos_actions::ActionContext> {
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| content.needs_commit());
        crate::panes::preference_commit_context(
            pending,
            "power",
            "Power",
            "Change power preferences",
        )
    }

    fn name(&self) -> &'static str {
        "PowerPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Boolean(b) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

fn read_str(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => state.text(&obj).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;

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
    fn profile_label_and_from_label_round_trip() {
        assert_eq!(profile_label(PowerProfile::Saver), "Power Saver");
        assert_eq!(
            profile_from_label("Performance"),
            Some(PowerProfile::Performance)
        );
        assert_eq!(profile_from_label("Nonsense"), None);
    }

    #[test]
    fn blank_label_and_from_label_round_trip() {
        assert_eq!(blank_label(0), "Never");
        assert_eq!(blank_label(10), "10 minutes");
        assert_eq!(blank_from_label("30 minutes"), Some(30));
        assert_eq!(blank_from_label("Never"), Some(0));
    }

    #[test]
    fn reads_battery_from_sysfs_tree() {
        // Build a throwaway /sys/class/power_supply-like tree.
        let base = std::env::temp_dir().join(format!("qos-power-test-{}", std::process::id()));
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

    #[test]
    fn missing_battery_degrades_honestly() {
        let base = std::env::temp_dir().join(format!("qos-power-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let info = BatteryInfo::from_root(&base);
        assert!(!info.present);
        assert!(info.on_ac, "no battery reads as mains-powered");
        assert_eq!(info.summary(), "Running on AC power");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Selecting a profile dispatches `set_power_profile` once with the chosen
    /// profile, and not again while it rests — the real dispatch path.
    #[test]
    fn selecting_profile_dispatches_once() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = PowerPane::with_parts(
            rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
            sample_battery(),
            PowerProfile::Balanced,
            PowerPrefs::default(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert!(rec.calls().is_empty(), "seed must not dispatch");
        let _ = doc.put(&root, PROFILE_KEY, "Performance");
        pane.sync(&mut doc, &root);
        pane.sync(&mut doc, &root); // idempotent at rest
        assert_eq!(rec.calls(), vec!["set_power_profile(performance)"]);
        assert_eq!(pane.profile(), PowerProfile::Performance);
    }

    #[test]
    fn failed_profile_change_reverts_instead_of_lying() {
        let rec = Arc::new(RecordingDispatcher::default());
        rec.set_fail_power_profile(true);
        let mut pane = PowerPane::with_parts(
            rec as Arc<dyn Dispatcher + Send + Sync>,
            sample_battery(),
            PowerProfile::Balanced,
            PowerPrefs::default(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, PROFILE_KEY, "Performance");
        pane.sync(&mut doc, &root);

        assert_eq!(pane.profile(), PowerProfile::Balanced);
        assert_eq!(
            read_str(&doc, &root, PROFILE_KEY).as_deref(),
            Some("Balanced")
        );
        assert!(
            pane.profile_notice
                .as_deref()
                .is_some_and(|notice| notice.contains("simulated"))
        );
    }

    /// The idle preferences are durable: toggling auto-suspend and choosing a
    /// blank timeout are adopted into the pane's tracked prefs (persisted via
    /// the shared store, asserted against disk in the persistence test).
    #[test]
    fn idle_prefs_are_adopted_on_change() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("power_idle");
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = PowerPane::with_parts(
            rec as Arc<dyn Dispatcher + Send + Sync>,
            sample_battery(),
            PowerProfile::Balanced,
            PowerPrefs::default(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(
            &root,
            AUTO_SUSPEND_KEY,
            automerge::ScalarValue::Boolean(false),
        );
        let _ = doc.put(&root, BLANK_KEY, "30 minutes");
        pane.sync(&mut doc, &root);
        assert!(!pane.prefs().auto_suspend);
        assert_eq!(pane.prefs().blank_minutes, 30);
    }

    /// The new power-behaviour controls (suspend timeout, lid, power key,
    /// critical action) are adopted on change and land on disk in the shared
    /// settings file — the real persist path the compositor reads.
    #[test]
    fn power_behaviour_prefs_persist_to_disk() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("power_behaviour");
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = PowerPane::with_parts(
            rec as Arc<dyn Dispatcher + Send + Sync>,
            sample_battery(),
            PowerProfile::Balanced,
            PowerPrefs::default(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);

        // Drive each new control through the CRDT the way the dropdowns do.
        let _ = doc.put(&root, SUSPEND_KEY, "30 minutes");
        let _ = doc.put(&root, LID_KEY, PowerActionPref::Lock.display());
        let _ = doc.put(&root, POWER_KEY_KEY, PowerActionPref::Ignore.display());
        let _ = doc.put(&root, CRITICAL_KEY, PowerActionPref::Suspend.display());
        pane.sync(&mut doc, &root);

        assert_eq!(pane.prefs().suspend_minutes, 30);
        assert_eq!(pane.prefs().lid_close_action, PowerActionPref::Lock);
        assert_eq!(pane.prefs().power_key_action, PowerActionPref::Ignore);
        assert_eq!(pane.prefs().critical_action, PowerActionPref::Suspend);

        // Reload from the real settings file: the compositor sees exactly this.
        let reloaded = SystemPrefs::load().power;
        assert_eq!(reloaded.suspend_minutes, 30);
        assert_eq!(reloaded.lid_close_action, PowerActionPref::Lock);
        assert_eq!(reloaded.power_key_action, PowerActionPref::Ignore);
        assert_eq!(reloaded.critical_action, PowerActionPref::Suspend);
    }

    #[test]
    fn builds_layout() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = PowerPane::with_parts(
            rec as Arc<dyn Dispatcher + Send + Sync>,
            sample_battery(),
            PowerProfile::Balanced,
            PowerPrefs::default(),
        );
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }
}

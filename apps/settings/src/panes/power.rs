#![allow(
    clippy::expect_used,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    reason = "power uses a closed static resource contract and keeps the coupled idle-policy transaction auditable"
)]

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
//   `power.lock`        — string, session-lock timeout dropdown

use std::collections::HashMap;
use std::sync::Arc;
use ui::doc::{read_bool, read_str_or_text};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, transaction::Transactable};
use os_actions::{BatteryInfo, ChargeCapBackend, ChargeCapCapability, Dispatcher, PowerProfile};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Dropdown, ProgressBar, Sparkline, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::dispatcher::dispatcher_or_real;
use crate::i18n::{t, t_args};
use crate::prefs::{PowerActionPref, PowerPrefs, SystemPrefs};
use crate::widgets::{El, caption, card, choice_row, rows, setting_row, value_row};
use ui::i18n::ArgValue;

pub const PROFILE_KEY: &str = "power.profile";
pub const AUTO_SUSPEND_KEY: &str = "power.auto_suspend";
pub const BLANK_KEY: &str = "power.blank";
pub const LOCK_KEY: &str = "power.lock";
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

pub(crate) fn suspend_from_label(label: &str) -> Option<u32> {
    duration_from_options(SUSPEND_OPTIONS, label)
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

pub(crate) fn blank_from_label(label: &str) -> Option<u32> {
    duration_from_options(BLANK_OPTIONS, label)
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

pub(crate) fn lock_from_label(label: &str) -> Option<u32> {
    duration_from_options(LOCK_OPTIONS, label)
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

/// Power pane widget.
pub struct PowerPane {
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    battery: BatteryInfo,
    devices: Vec<os_actions::PowerDevice>,
    history: os_actions::BatteryHistory,
    charge_cap: Option<ChargeCapCapability>,
    charge_notice: Option<String>,
    operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    command_state: ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    local_doc: AutoCommit,
    isolate_document: bool,
    require_operation_token: bool,
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
    pub fn with_command_state(
        mut self,
        commands: &ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    ) -> Self {
        self.command_state = commands.clone();
        self.isolate_document = true;
        self
    }
    #[must_use]
    pub fn with_operation_status(
        mut self,
        status: &ui::LocalState<crate::actions::LocalOperationStatus>,
    ) -> Self {
        self.operation_status = status.clone();
        self.require_operation_token = true;
        self
    }

    fn operation_finished(
        &self,
        operation: &crate::actions::PowerOperation,
        result: Result<String, String>,
    ) {
        let token = crate::actions::local_operation_token_for(
            crate::actions::POWER_OPERATION_TYPE,
            operation,
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

    fn operation_is_running(&self, operation: &crate::actions::PowerOperation) -> bool {
        if !self.require_operation_token {
            return true;
        }
        let token = crate::actions::local_operation_token_for(
            crate::actions::POWER_OPERATION_TYPE,
            operation,
        );
        matches!(self.operation_status.get(), crate::actions::LocalOperationStatus::Running { token: active } if active == token)
    }

    fn restore_authoritative_controls(&self, state: &mut AutoCommit, root: &ObjId) {
        fn restore_text(state: &mut AutoCommit, root: &ObjId, key: &str, value: impl AsRef<str>) {
            let value = value.as_ref();
            if read_str_or_text(state, root, key).as_deref() != Some(value) {
                let _ = state.put(root, key, value);
            }
        }
        fn restore_bool(state: &mut AutoCommit, root: &ObjId, key: &str, value: bool) {
            if read_bool(state, root, key) != Some(value) {
                let _ = state.put(root, key, value);
            }
        }
        restore_text(state, root, PROFILE_KEY, profile_label(self.last_profile));
        restore_bool(state, root, AUTO_SUSPEND_KEY, self.last_prefs.auto_suspend);
        restore_text(
            state,
            root,
            BLANK_KEY,
            blank_label(self.last_prefs.blank_minutes),
        );
        restore_text(
            state,
            root,
            SUSPEND_KEY,
            suspend_label(self.last_prefs.suspend_minutes),
        );
        restore_text(
            state,
            root,
            SAVER_SUSPEND_KEY,
            suspend_label(self.last_prefs.saver_suspend_minutes),
        );
        restore_text(
            state,
            root,
            BALANCED_SUSPEND_KEY,
            suspend_label(self.last_prefs.balanced_suspend_minutes),
        );
        restore_text(
            state,
            root,
            PERFORMANCE_SUSPEND_KEY,
            suspend_label(self.last_prefs.performance_suspend_minutes),
        );
        restore_text(
            state,
            root,
            LID_KEY,
            action_shown(self.last_prefs.lid_close_action),
        );
        restore_text(
            state,
            root,
            POWER_KEY_KEY,
            action_shown(self.last_prefs.power_key_action),
        );
        restore_text(
            state,
            root,
            CRITICAL_KEY,
            action_shown(self.last_prefs.critical_action),
        );
        if let Some(capability) = &self.charge_cap {
            restore_text(
                state,
                root,
                CHARGE_LIMIT_KEY,
                format!("{}%", capability.current),
            );
        }
    }

    fn execute_local_command(
        &mut self,
        local: &mut AutoCommit,
        operation: crate::actions::PowerOperation,
    ) {
        let root = automerge::ROOT;
        use crate::actions::PowerOperation as Op;
        match &operation {
            Op::SetChargeLimit { percent } => {
                let _ = local.put(&root, CHARGE_LIMIT_KEY, format!("{percent}%"));
            }
            Op::SetProfile { profile } => {
                let _ = local.put(&root, PROFILE_KEY, profile.as_str());
            }
            Op::SetAutoSuspend { enabled } => {
                let _ = local.put(&root, AUTO_SUSPEND_KEY, *enabled);
            }
            Op::SetBlankTimeout { label } => {
                let _ = local.put(&root, BLANK_KEY, label.as_str());
            }
            Op::SetLockTimeout { label } => {
                let _ = local.put(&root, LOCK_KEY, label.as_str());
            }
            Op::SetSuspendTimeout { profile, label } => {
                let key = match profile.as_str() {
                    "default" => SUSPEND_KEY,
                    "saver" => SAVER_SUSPEND_KEY,
                    "balanced" => BALANCED_SUSPEND_KEY,
                    _ => PERFORMANCE_SUSPEND_KEY,
                };
                let _ = local.put(&root, key, label.as_str());
            }
            Op::SetLidAction { action } => {
                let _ = local.put(&root, LID_KEY, action.as_str());
            }
            Op::SetPowerKeyAction { action } => {
                let _ = local.put(&root, POWER_KEY_KEY, action.as_str());
            }
            Op::SetCriticalAction { action } => {
                let _ = local.put(&root, CRITICAL_KEY, action.as_str());
            }
        }
        self.handle_profile_change(local, &root);
        self.handle_charge_limit(local, &root);
        self.handle_idle_prefs(local, &root);
        self.handle_action_prefs(local, &root);
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
                Some(t_args(
                    "power-profile-service-unavailable",
                    &[("details", ArgValue::from(details.join("; ")))],
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
            command_state: ui::LocalState::new(None),
            local_doc: AutoCommit::new(),
            isolate_document: false,
            require_operation_token: false,
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
            command_state: ui::LocalState::new(None),
            local_doc: AutoCommit::new(),
            isolate_document: false,
            require_operation_token: false,
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
                &t("power-source-title"),
                rows(vec![
                    value_row(&t("power-source"), &t("power-source-ac")),
                    caption(&t("power-source-caption")),
                ]),
            );
        }
        let charge = format!("{}%", self.battery.capacity);
        let status = battery_status_shown(&self.battery);
        let mut battery_rows: Vec<El> = vec![
            Box::new(ProgressBar::new(self.battery.capacity as f32 / 100.0)),
            value_row(&t("power-charge"), &charge),
            value_row(&t("power-status"), &status),
        ];
        if let Some(cycles) = self.battery.cycle_count {
            battery_rows.push(value_row(&t("power-cycles"), &cycles.to_string()));
        }
        if let Some(health) = self.battery.health_percent() {
            battery_rows.push(value_row(&t("power-health"), &format!("{health}%")));
            battery_rows.push(caption(&health_explanation_shown(&self.battery)));
        } else {
            battery_rows.push(value_row(&t("power-health"), &t("power-health-unknown")));
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
            battery_rows.push(caption(&t("power-history-caption")));
        }
        if let Some(capability) = &self.charge_cap {
            let choices: Vec<String> = capability
                .choices
                .iter()
                .map(|limit| format!("{limit}%"))
                .collect();
            battery_rows.push(choice_row(
                &t("power-charge-cap"),
                Box::new(
                    Dropdown::new(choices, CHARGE_LIMIT_KEY)
                        .with_id("power.charge_limit")
                        .on_action(
                            crate::actions::power_charge_binding(
                                &capability.choices,
                                capability.current,
                            )
                            .expect(
                                "charge-cap resource exists when the hardware control is visible",
                            ),
                        )
                        .with_placeholder(&format!("{}%", capability.current)),
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
        if let Some(notice) = &self.charge_notice {
            battery_rows.push(caption(notice));
        }
        card(&t("power-battery-title"), rows(battery_rows))
    }

    fn profile_card(&self) -> El {
        let profile_opts: Vec<String> = self
            .available_profiles
            .iter()
            .map(|profile| profile_label(*profile))
            .collect();
        let current = profile_label(self.profile);
        let mut profile_rows =
            if profile_opts.is_empty() {
                vec![value_row(
                    &t("power-profile"),
                    &t("power-profile-unavailable"),
                )]
            } else {
                vec![choice_row(
                    &t("power-profile"),
                    Box::new(
                        Dropdown::new(profile_opts, PROFILE_KEY)
                            .with_id("power.profile")
                            .on_action(crate::actions::power_text_binding("profile").expect(
                                "power profile action is part of the static resource contract",
                            ))
                            .with_placeholder(&current),
                    ),
                )]
            };
        profile_rows.push(caption(&t("power-profile-caption")));
        if let Some(notice) = &self.profile_notice {
            profile_rows.push(caption(notice));
        }
        card(&t("power-profile-title"), rows(profile_rows))
    }

    fn idle_card(&self) -> El {
        let blank_opts = duration_options(BLANK_OPTIONS);
        let lock_opts = duration_options(LOCK_OPTIONS);
        let suspend_opts = duration_options(SUSPEND_OPTIONS);
        let suspend_now = suspend_label(self.prefs.suspend_minutes);
        let blank_now = blank_label(self.prefs.blank_minutes);
        let lock_now = lock_label(self.prefs.lock_minutes);
        let saver_now = suspend_label(self.prefs.saver_suspend_minutes);
        let balanced_now = suspend_label(self.prefs.balanced_suspend_minutes);
        let performance_now = suspend_label(self.prefs.performance_suspend_minutes);
        card(
            &t("power-idle-title"),
            rows(vec![
                setting_row(
                    &t("power-suspend-idle"),
                    Box::new(
                        Toggle::new("settings.power.auto_suspend", AUTO_SUSPEND_KEY)
                            .with_id("power.auto_suspend")
                            .on_action(
                                crate::actions::power_bool_binding("auto_suspend")
                                    .expect("automatic-suspend action is part of the static resource contract"),
                            ),
                    ),
                ),
                choice_row(
                    &t("power-suspend-after"),
                    Box::new(
                        Dropdown::new(suspend_opts.clone(), SUSPEND_KEY)
                            .with_id("power.suspend")
                            .on_action(
                                crate::actions::power_text_binding("suspend-after")
                                    .expect("suspend action is part of the static resource contract"),
                            )
                            .with_placeholder(&suspend_now),
                    ),
                ),
                choice_row(
                    &t("power-blank"),
                    Box::new(
                        Dropdown::new(blank_opts, BLANK_KEY)
                            .with_id("power.blank")
                            .on_action(
                                crate::actions::power_text_binding("blank-after")
                                    .expect("blank action is part of the static resource contract"),
                            )
                            .with_placeholder(&blank_now),
                    ),
                ),
                choice_row(
                    &t("power-lock-after"),
                    Box::new(
                        Dropdown::new(lock_opts, LOCK_KEY)
                            .with_id("power.lock")
                            .on_action(
                                crate::actions::power_text_binding("lock-after")
                                    .expect("lock action is part of the static resource contract"),
                            )
                            .with_placeholder(&lock_now),
                    ),
                ),
                caption(&t("power-idle-caption")),
                choice_row(
                    &t("power-saver-suspend"),
                    Box::new(
                        Dropdown::new(suspend_opts.clone(), SAVER_SUSPEND_KEY)
                            .on_action(
                                crate::actions::power_text_binding("saver-suspend")
                                    .expect("saver suspend action is part of the static resource contract"),
                            )
                            .with_placeholder(&saver_now),
                    ),
                ),
                choice_row(
                    &t("power-balanced-suspend"),
                    Box::new(
                        Dropdown::new(suspend_opts.clone(), BALANCED_SUSPEND_KEY)
                            .on_action(
                                crate::actions::power_text_binding("balanced-suspend")
                                    .expect("balanced suspend action is part of the static resource contract"),
                            )
                            .with_placeholder(&balanced_now),
                    ),
                ),
                choice_row(
                    &t("power-performance-suspend"),
                    Box::new(
                        Dropdown::new(suspend_opts, PERFORMANCE_SUSPEND_KEY)
                            .on_action(
                                crate::actions::power_text_binding("performance-suspend")
                                    .expect("performance suspend action is part of the static resource contract"),
                            )
                            .with_placeholder(&performance_now),
                    ),
                ),
                caption(&t("power-profile-idle-caption")),
            ]),
        )
    }

    fn actions_card(&self) -> El {
        let lid = action_shown(self.prefs.lid_close_action);
        let power_key = action_shown(self.prefs.power_key_action);
        let critical = action_shown(self.prefs.critical_action);
        let warn = t_args(
            "power-warn-caption",
            &[
                (
                    "warn",
                    ArgValue::from(i64::from(self.prefs.low_battery_percent)),
                ),
                (
                    "critical",
                    ArgValue::from(i64::from(self.prefs.critical_battery_percent)),
                ),
            ],
        );
        card(
            &t("power-behaviour-title"),
            rows(vec![
                choice_row(
                    &t("power-lid"),
                    Box::new(
                        Dropdown::new(action_options(), LID_KEY)
                            .with_id("power.lid")
                            .on_action(
                                crate::actions::power_text_binding("lid")
                                    .expect("lid action is part of the static resource contract"),
                            )
                            .with_placeholder(&lid),
                    ),
                ),
                choice_row(
                    &t("power-power-key"),
                    Box::new(
                        Dropdown::new(action_options(), POWER_KEY_KEY)
                            .with_id("power.power_key")
                            .on_action(
                                crate::actions::power_text_binding("power_key").expect(
                                    "power-key action is part of the static resource contract",
                                ),
                            )
                            .with_placeholder(&power_key),
                    ),
                ),
                choice_row(
                    &t("power-critical"),
                    Box::new(
                        Dropdown::new(action_options(), CRITICAL_KEY)
                            .with_id("power.critical")
                            .on_action(
                                crate::actions::power_text_binding("critical").expect(
                                    "critical action is part of the static resource contract",
                                ),
                            )
                            .with_placeholder(&critical),
                    ),
                ),
                caption(&warn),
            ]),
        )
    }

    fn devices_card(&self) -> Option<El> {
        if self.devices.len() <= 1 {
            return None;
        }
        let mut entries = Vec::new();
        for d in &self.devices {
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

    fn persist(&self) -> Result<(), String> {
        let mut p = SystemPrefs::load();
        p.power = self.prefs;
        p.save().map(|_| ()).map_err(|error| {
            warn!(target: "tilo::settings", op = "power_prefs_save", %error, "failed to persist power prefs");
            t_args(
                "power-save-failed",
                &[("error", ArgValue::from(error.to_string()))],
            )
        })
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
        let _ = state.put(root, LOCK_KEY, lock_label(self.prefs.lock_minutes));
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
        let _ = state.put(root, LID_KEY, action_shown(self.prefs.lid_close_action));
        let _ = state.put(
            root,
            POWER_KEY_KEY,
            action_shown(self.prefs.power_key_action),
        );
        let _ = state.put(root, CRITICAL_KEY, action_shown(self.prefs.critical_action));
        if let Some(capability) = &self.charge_cap {
            let _ = state.put(root, CHARGE_LIMIT_KEY, format!("{}%", capability.current));
        }
        self.last_prefs = self.prefs;
        self.needs_seed = false;
    }

    /// Power profile → dispatch on change, guarded so it doesn't re-fire.
    fn handle_profile_change(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let Some(label) = read_str_or_text(state, root, PROFILE_KEY) else {
            return;
        };
        let Some(p) = profile_from_label(&label) else {
            return;
        };
        if p == self.last_profile {
            return;
        }
        let operation = crate::actions::PowerOperation::SetProfile {
            profile: label.clone(),
        };
        if !self.operation_is_running(&operation) {
            let _ = state.put(root, PROFILE_KEY, profile_label(self.last_profile));
            return;
        }
        if self.available_profiles.contains(&p) {
            match self.dispatcher.set_power_profile(p) {
                Ok(()) => {
                    self.last_profile = p;
                    self.profile = p;
                    self.profile_notice = Some(format!("{} is now active.", profile_label(p)));
                    self.operation_finished(
                        &operation,
                        Ok(format!("{} is now active.", profile_label(p))),
                    );
                }
                Err(e) => {
                    let _ = state.put(root, PROFILE_KEY, profile_label(self.last_profile));
                    self.profile_notice = Some(format!("Could not change the power profile: {e}"));
                    self.operation_finished(
                        &operation,
                        Err(format!("Could not change the power profile: {e}")),
                    );
                    warn!(target: "tilo::settings", op = "set_power_profile", error = %e, "power pane dispatch failed");
                }
            }
        } else {
            let _ = state.put(root, PROFILE_KEY, profile_label(self.last_profile));
            self.profile_notice = Some(format!(
                "{} is not supported by this device.",
                profile_label(p)
            ));
            self.operation_finished(
                &crate::actions::PowerOperation::SetProfile { profile: label },
                Err("Power profile is not supported by this device.".into()),
            );
        }
    }

    fn handle_charge_limit(&mut self, state: &AutoCommit, root: &ObjId) {
        let Some(label) = read_str_or_text(state, root, CHARGE_LIMIT_KEY) else {
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
        let operation = crate::actions::PowerOperation::SetChargeLimit { percent: limit };
        if !self.operation_is_running(&operation) {
            return;
        }
        match os_actions::set_charge_limit_checked(limit) {
            Ok(actual) => {
                self.battery.charge_limit = Some(limit);
                self.charge_cap = Some(actual);
                self.charge_notice = Some(format!("Hardware confirmed a {limit}% charge cap."));
                self.operation_finished(
                    &operation,
                    Ok(format!("hardware confirmed a {limit}% charge cap")),
                );
            }
            Err(e) => {
                self.charge_notice = Some(format!("Charge cap was not applied: {e}"));
                self.operation_finished(
                    &operation,
                    Err(format!("Charge cap was not applied: {e}")),
                );
                warn!(target: "tilo::settings", op = "set_charge_limit", error = %e, "battery cap failed");
            }
        }
    }

    /// Auto-suspend, screen-blank and the three per-profile idle-suspend
    /// timeouts → persisted on change.
    fn handle_idle_prefs(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Some(b) = read_bool(state, root, AUTO_SUSPEND_KEY)
            && b != self.last_prefs.auto_suspend
            && self.operation_is_running(&crate::actions::PowerOperation::SetAutoSuspend {
                enabled: b,
            })
        {
            self.last_prefs.auto_suspend = b;
            self.prefs.auto_suspend = b;
            let result = self
                .persist()
                .map(|()| "Automatic suspend preference saved.".into());
            self.operation_finished(
                &crate::actions::PowerOperation::SetAutoSuspend { enabled: b },
                result,
            );
        }

        if let Some(label) = read_str_or_text(state, root, BLANK_KEY)
            && let Some(m) = blank_from_label(&label)
            && m != self.last_prefs.blank_minutes
            && self.operation_is_running(&crate::actions::PowerOperation::SetBlankTimeout {
                label: label.clone(),
            })
        {
            self.last_prefs.blank_minutes = m;
            self.prefs.blank_minutes = m;
            let result = self
                .persist()
                .map(|()| "Screen blank timeout saved.".into());
            self.operation_finished(
                &crate::actions::PowerOperation::SetBlankTimeout { label },
                result,
            );
        }

        if let Some(label) = read_str_or_text(state, root, LOCK_KEY)
            && let Some(minutes) = lock_from_label(&label)
            && minutes != self.last_prefs.lock_minutes
            && self.operation_is_running(&crate::actions::PowerOperation::SetLockTimeout {
                label: label.clone(),
            })
        {
            self.last_prefs.lock_minutes = minutes;
            self.prefs.lock_minutes = minutes;
            let result = self
                .persist()
                .map(|()| "Automatic lock timeout saved.".into());
            self.operation_finished(
                &crate::actions::PowerOperation::SetLockTimeout { label },
                result,
            );
        }

        if let Some(label) = read_str_or_text(state, root, SUSPEND_KEY)
            && let Some(m) = suspend_from_label(&label)
            && m != self.last_prefs.suspend_minutes
            && self.operation_is_running(&crate::actions::PowerOperation::SetSuspendTimeout {
                profile: "default".into(),
                label: label.clone(),
            })
        {
            self.last_prefs.suspend_minutes = m;
            self.prefs.suspend_minutes = m;
            let result = self.persist().map(|()| "Suspend timeout saved.".into());
            self.operation_finished(
                &crate::actions::PowerOperation::SetSuspendTimeout {
                    profile: "default".into(),
                    label,
                },
                result,
            );
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
            if let Some(label) = read_str_or_text(state, root, key)
                && let Some(minutes) = suspend_from_label(&label)
                && minutes != current
            {
                let profile = match set {
                    0 => "saver",
                    1 => "balanced",
                    _ => "performance",
                };
                let operation = crate::actions::PowerOperation::SetSuspendTimeout {
                    profile: profile.into(),
                    label: label.clone(),
                };
                if !self.operation_is_running(&operation) {
                    continue;
                }
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
                let result = self
                    .persist()
                    .map(|()| format!("{profile} suspend timeout saved."));
                self.operation_finished(&operation, result);
            }
        }
    }

    /// Lid-close, power-key and critical-battery actions → persisted on
    /// change.
    fn handle_action_prefs(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Some(label) = read_str_or_text(state, root, LID_KEY)
            && let Some(a) = action_from_label(&label)
            && a != self.last_prefs.lid_close_action
            && self.operation_is_running(&crate::actions::PowerOperation::SetLidAction {
                action: label.clone(),
            })
        {
            self.last_prefs.lid_close_action = a;
            self.prefs.lid_close_action = a;
            let result = self.persist().map(|()| "Lid-close action saved.".into());
            self.operation_finished(
                &crate::actions::PowerOperation::SetLidAction { action: label },
                result,
            );
        }

        if let Some(label) = read_str_or_text(state, root, POWER_KEY_KEY)
            && let Some(a) = action_from_label(&label)
            && a != self.last_prefs.power_key_action
            && self.operation_is_running(&crate::actions::PowerOperation::SetPowerKeyAction {
                action: label.clone(),
            })
        {
            self.last_prefs.power_key_action = a;
            self.prefs.power_key_action = a;
            let result = self.persist().map(|()| "Power-key action saved.".into());
            self.operation_finished(
                &crate::actions::PowerOperation::SetPowerKeyAction { action: label },
                result,
            );
        }

        if let Some(label) = read_str_or_text(state, root, CRITICAL_KEY)
            && let Some(a) = action_from_label(&label)
            && a != self.last_prefs.critical_action
            && self.operation_is_running(&crate::actions::PowerOperation::SetCriticalAction {
                action: label.clone(),
            })
        {
            self.last_prefs.critical_action = a;
            self.prefs.critical_action = a;
            let result = self
                .persist()
                .map(|()| "Critical-battery action saved.".into());
            self.operation_finished(
                &crate::actions::PowerOperation::SetCriticalAction { action: label },
                result,
            );
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(PowerPane::new())
}

impl Widget for PowerPane {
    ui::provides!(dyn ui::undo::UndoPoints);

    fn sync(&mut self, shared: &mut AppModel, root: &ModelPath) {
        let (shared, _) = ui::doc::document(shared, root);
        let isolated = self.isolate_document;
        let mut state = if isolated {
            std::mem::take(&mut self.local_doc)
        } else {
            std::mem::take(shared)
        };
        let root = automerge::ROOT;
        self.seed(&mut state, &root);
        if let Some(crate::actions::LocalPaneCommand::Power(operation)) = self.command_state.get() {
            self.command_state.set(None);
            self.execute_local_command(&mut state, operation);
        }
        if self.require_operation_token
            && !matches!(
                self.operation_status.get(),
                crate::actions::LocalOperationStatus::Running { .. }
            )
        {
            self.restore_authoritative_controls(&mut state, &root);
        } else {
            self.handle_profile_change(&mut state, &root);
            self.handle_charge_limit(&state, &root);
            self.handle_idle_prefs(&state, &root);
            self.handle_action_prefs(&state, &root);
        }

        if let Some(c) = &mut self.composed {
            c.sync(&mut state, &root);
        }
        if isolated {
            self.local_doc = state;
        } else {
            *shared = state;
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let was_handled = *ctx.handled;
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
        self.commit_context_pending |= !was_handled && *ctx.handled;
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

    fn name(&self) -> &'static str {
        "PowerPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::undo::UndoPoints for PowerPane {
    fn take_commit_context(&mut self) -> Option<ui::tilo_actions::ActionContext> {
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| ui::undo::needs_commit(content.as_ref()));
        crate::panes::preference_commit_context(
            pending,
            "power",
            "Power",
            "Change power preferences",
        )
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
        for (label, minutes) in [("30 minutes", 30), ("Never", 0)] {
            assert_eq!(blank_from_label(label), Some(minutes), "{label}");
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

    /// Profile selection dispatches once, reverts on failure, and replicated latches never reach the device.
    #[test]
    fn profile_selection_dispatches_once_and_never_lies() {
        // Case `selecting_profile_dispatches_once`.
        // Selecting a profile dispatches `set_power_profile` once with the chosen
        // profile, and not again while it rests — the real dispatch path.
        {
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

        // Case `failed_profile_change_reverts_instead_of_lying`.
        {
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
                read_str_or_text(&doc, &root, PROFILE_KEY).as_deref(),
                Some("Balanced")
            );
            assert!(
                pane.profile_notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("simulated"))
            );
        }

        // Case `replicated_power_latches_never_reach_device_or_persistence`.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let rec = Arc::new(RecordingDispatcher::default());
            let status = ui::LocalState::new(crate::actions::LocalOperationStatus::Running {
                token: crate::actions::local_operation_token_for(
                    crate::actions::POWER_OPERATION_TYPE,
                    &crate::actions::PowerOperation::SetBlankTimeout {
                        label: "30 minutes".into(),
                    },
                ),
            });
            let commands = ui::LocalState::new(None);
            let defaults = PowerPrefs::default();
            let mut pane = PowerPane::with_parts(
                rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
                sample_battery(),
                PowerProfile::Balanced,
                defaults,
            )
            .with_command_state(&commands)
            .with_operation_status(&status);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let initial_heads = doc.get_heads();
            pane.sync(&mut doc, &root);
            assert_eq!(doc.get_heads(), initial_heads);

            // Only the exact blank-timeout command is admitted. Concurrent remote
            // mutations of every other hardware latch remain inert.
            let _ = doc.put(&root, PROFILE_KEY, "Performance");
            let _ = doc.put(&root, AUTO_SUSPEND_KEY, false);
            let _ = doc.put(&root, LID_KEY, PowerActionPref::Ignore.display());
            pane.sync(&mut doc, &root);

            assert!(rec.calls().is_empty());
            assert_eq!(pane.profile(), PowerProfile::Balanced);
            assert_eq!(pane.prefs().auto_suspend, defaults.auto_suspend);
            assert_eq!(pane.prefs().lid_close_action, defaults.lid_close_action);
        }
    }

    /// Idle and behaviour preferences are adopted and persist to disk.
    #[test]
    fn power_prefs_are_adopted_and_persist() {
        // Case `idle_prefs_are_adopted_on_change`.
        // The idle preferences are durable: toggling auto-suspend and choosing a
        // blank timeout are adopted into the pane's tracked prefs (persisted via
        // the shared store, asserted against disk in the persistence test).
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
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
            let _ = doc.put(&root, LOCK_KEY, "Never");
            pane.sync(&mut doc, &root);
            assert!(!pane.prefs().auto_suspend);
            assert_eq!(pane.prefs().blank_minutes, 30);
            assert_eq!(pane.prefs().lock_minutes, 0);
            assert_eq!(SystemPrefs::load().power.lock_minutes, 0);
        }

        // Case `power_behaviour_prefs_persist_to_disk`.
        // The new power-behaviour controls (suspend timeout, lid, power key,
        // critical action) are adopted on change and land on disk in the shared
        // settings file — the real persist path the compositor reads.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
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
    }
}

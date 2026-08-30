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
//!   uses; the initial selection is sampled from `powerprofilesctl get`. When
//!   `powerprofilesctl` is absent the selector still records the choice but the
//!   dispatch is a logged no-op.
//! - **Automatic suspend** and **screen-blank timeout** are durable
//!   preferences persisted to the shared store ([`crate::prefs::SystemPrefs`]);
//!   the compositor's idle layer reads them.
//
// CRDT keys this pane reads / writes:
//   `power.profile`     — string, power-profile dropdown
//   `power.auto_suspend`— bool, suspend-when-idle toggle
//   `power.blank`       — string, screen-blank timeout dropdown

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use os_actions::{Dispatcher, PowerProfile};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Dropdown, ProgressBar, Toggle, Widget};
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

/// A snapshot of the machine's power supply, read from
/// `/sys/class/power_supply`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatteryInfo {
    /// Whether a battery is present at all.
    pub present: bool,
    /// Charge percentage, 0..=100.
    pub capacity: u32,
    /// Raw status string (`Charging`, `Discharging`, `Full`, `Not charging`).
    pub status: String,
    /// Charge cycle count, when the kernel exposes it.
    pub cycle_count: Option<u32>,
    /// Whether the AC adapter is plugged in.
    pub on_ac: bool,
}

impl BatteryInfo {
    /// Scan `/sys/class/power_supply` for the first battery and a mains
    /// adapter. Best-effort — a machine with no battery yields
    /// `present: false` so the pane degrades honestly.
    pub fn from_system() -> Self {
        Self::from_root(Path::new("/sys/class/power_supply"))
    }

    /// Read from an explicit power-supply root (tests inject a temp tree).
    pub fn from_root(root: &Path) -> Self {
        let mut battery_dir: Option<PathBuf> = None;
        let mut on_ac = false;
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                let dir = e.path();
                let ty = read_trim(&dir.join("type")).unwrap_or_default();
                match ty.as_str() {
                    "Battery" if battery_dir.is_none() => battery_dir = Some(dir),
                    "Mains" if read_trim(&dir.join("online")).as_deref() == Some("1") => {
                        on_ac = true;
                    }
                    _ => {}
                }
            }
        }
        match battery_dir {
            Some(dir) => Self::from_battery_dir(&dir, on_ac),
            None => Self {
                present: false,
                capacity: 0,
                status: "No battery".to_string(),
                cycle_count: None,
                on_ac: true, // no battery ⇒ mains-powered
            },
        }
    }

    /// Read a single battery directory's fields.
    pub fn from_battery_dir(dir: &Path, on_ac: bool) -> Self {
        let capacity = read_trim(&dir.join("capacity"))
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0)
            .min(100);
        let status = read_trim(&dir.join("status")).unwrap_or_else(|| "Unknown".to_string());
        let cycle_count = read_trim(&dir.join("cycle_count")).and_then(|s| s.parse::<u32>().ok());
        Self {
            present: true,
            capacity,
            status,
            cycle_count,
            on_ac,
        }
    }

    /// A friendly one-line summary of the charge state.
    pub fn summary(&self) -> String {
        if !self.present {
            return "Running on AC power".to_string();
        }
        match self.status.as_str() {
            "Full" => "Fully charged".to_string(),
            "Charging" => format!("{}% · Charging", self.capacity),
            "Not charging" => format!("{}% · Plugged in, not charging", self.capacity),
            _ => format!("{}% · On battery", self.capacity),
        }
    }
}

fn read_trim(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Power pane widget.
pub struct PowerPane {
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    battery: BatteryInfo,
    profile: PowerProfile,
    last_profile: PowerProfile,
    prefs: PowerPrefs,
    last_prefs: PowerPrefs,
    needs_seed: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for PowerPane {
    fn default() -> Self {
        Self::new()
    }
}

impl PowerPane {
    pub fn new() -> Self {
        let profile = os_actions::get_power_profile().unwrap_or(PowerProfile::Balanced);
        Self {
            dispatcher: dispatcher_or_real(None),
            battery: BatteryInfo::from_system(),
            profile,
            last_profile: profile,
            prefs: SystemPrefs::load().power,
            last_prefs: PowerPrefs::default(),
            needs_seed: true,
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
        Self {
            dispatcher,
            battery,
            profile,
            last_profile: profile,
            prefs,
            last_prefs: prefs,
            needs_seed: true,
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
        // --- Battery card.
        let battery_card = if self.battery.present {
            let mut battery_rows: Vec<El> = vec![
                Box::new(ProgressBar::new(self.battery.capacity as f32 / 100.0)),
                value_row("Charge", &format!("{}%", self.battery.capacity)),
                value_row("Status", &self.battery.summary()),
            ];
            if let Some(cycles) = self.battery.cycle_count {
                battery_rows.push(value_row("Charge cycles", &cycles.to_string()));
            }
            card("Battery", rows(battery_rows))
        } else {
            card(
                "Power source",
                rows(vec![
                    value_row("Source", "AC power"),
                    caption(
                        "This device has no battery — it runs on mains power, so \
                         charge and battery-saver settings don't apply.",
                    ),
                ]),
            )
        };

        // --- Power profile card.
        let profile_opts: Vec<String> = PROFILES.iter().map(|(l, _)| l.to_string()).collect();
        let profile = card(
            "Power profile",
            rows(vec![
                choice_row(
                    "Profile",
                    Box::new(
                        Dropdown::new(profile_opts, PROFILE_KEY)
                            .with_id("power.profile")
                            .with_placeholder(profile_label(self.profile)),
                    ),
                ),
                caption(
                    "Balanced trades performance for battery life; Power Saver \
                     favours runtime; Performance favours speed.",
                ),
            ]),
        );

        // --- Session-idle card (durable preferences).
        let blank_opts: Vec<String> = BLANK_OPTIONS.iter().map(|(l, _)| l.to_string()).collect();
        let suspend_opts: Vec<String> =
            SUSPEND_OPTIONS.iter().map(|(l, _)| l.to_string()).collect();
        let idle = card(
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
                        Dropdown::new(suspend_opts, SUSPEND_KEY)
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
            ]),
        );

        // --- Buttons + battery actions card.
        let actions = card(
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
        );

        crate::widgets::cards(vec![battery_card, profile, idle, actions])
    }

    fn persist(&self) {
        let mut p = SystemPrefs::load();
        p.power = self.prefs;
        if let Err(e) = p.save() {
            warn!(target: "qos::settings", op = "power_prefs_save", error = %e, "failed to persist power prefs");
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(PowerPane::new())
}

impl Widget for PowerPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            let _ = state.put(root, PROFILE_KEY, profile_label(self.profile));
            let _ = state.put(
                root,
                AUTO_SUSPEND_KEY,
                automerge::ScalarValue::Boolean(self.prefs.auto_suspend),
            );
            let _ = state.put(root, SUSPEND_KEY, suspend_label(self.prefs.suspend_minutes));
            let _ = state.put(root, BLANK_KEY, blank_label(self.prefs.blank_minutes));
            let _ = state.put(root, LID_KEY, self.prefs.lid_close_action.display());
            let _ = state.put(root, POWER_KEY_KEY, self.prefs.power_key_action.display());
            let _ = state.put(root, CRITICAL_KEY, self.prefs.critical_action.display());
            self.last_prefs = self.prefs;
            self.needs_seed = false;
        }

        // Power profile → dispatch on change, guarded so it doesn't re-fire.
        if let Some(label) = read_str(state, root, PROFILE_KEY)
            && let Some(p) = profile_from_label(&label)
            && p != self.last_profile
        {
            self.last_profile = p;
            self.profile = p;
            if let Err(e) = self.dispatcher.set_power_profile(p) {
                warn!(target: "qos::settings", op = "set_power_profile", error = %e, "power pane dispatch failed");
            }
        }

        // Auto-suspend → persist on change.
        if let Some(b) = read_bool(state, root, AUTO_SUSPEND_KEY)
            && b != self.last_prefs.auto_suspend
        {
            self.last_prefs.auto_suspend = b;
            self.prefs.auto_suspend = b;
            self.persist();
        }

        // Screen-blank timeout → persist on change.
        if let Some(label) = read_str(state, root, BLANK_KEY)
            && let Some(m) = blank_from_label(&label)
            && m != self.last_prefs.blank_minutes
        {
            self.last_prefs.blank_minutes = m;
            self.prefs.blank_minutes = m;
            self.persist();
        }

        // Idle-suspend timeout → persist on change.
        if let Some(label) = read_str(state, root, SUSPEND_KEY)
            && let Some(m) = suspend_from_label(&label)
            && m != self.last_prefs.suspend_minutes
        {
            self.last_prefs.suspend_minutes = m;
            self.prefs.suspend_minutes = m;
            self.persist();
        }

        // Lid-close action → persist on change.
        if let Some(label) = read_str(state, root, LID_KEY)
            && let Some(a) = PowerActionPref::from_display(&label)
            && a != self.last_prefs.lid_close_action
        {
            self.last_prefs.lid_close_action = a;
            self.prefs.lid_close_action = a;
            self.persist();
        }

        // Power-key action → persist on change.
        if let Some(label) = read_str(state, root, POWER_KEY_KEY)
            && let Some(a) = PowerActionPref::from_display(&label)
            && a != self.last_prefs.power_key_action
        {
            self.last_prefs.power_key_action = a;
            self.prefs.power_key_action = a;
            self.persist();
        }

        // Critical-battery action → persist on change.
        if let Some(label) = read_str(state, root, CRITICAL_KEY)
            && let Some(a) = PowerActionPref::from_display(&label)
            && a != self.last_prefs.critical_action
        {
            self.last_prefs.critical_action = a;
            self.prefs.critical_action = a;
            self.persist();
        }

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
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
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

//! Updates pane — the system update manager surface.
//!
//! qOS is an immutable bootc image: updates are atomic image pulls staged
//! for the next boot, not piecemeal package upgrades. This pane shows the
//! current deployment, whether a newer image is *actually* staged (read from
//! `bootc status --format json`), the release channel, and real actions:
//!
//! - **Check for updates** re-queries the system and stamps the last-checked
//!   time (persisted so it survives restarts).
//! - **Restart & update** (shown only when an image is staged) reboots into
//!   the staged deployment via [`Rebooter`] (`systemctl reboot`).
//! - **Automatic updates** enables or disables the update timer itself
//!   ([`UpdateControl`]), and shows the timer's real state rather than a
//!   stored preference, which can drift from what the machine is actually
//!   doing.
//! - **Roll back** makes the previous image the one this machine boots, after
//!   showing what a rollback does not undo.
//! - **Release channel** is still a durable preference in the shared store
//!   ([`crate::prefs::SystemPrefs`]).
//
// CRDT keys this pane reads / writes:
//   `updates.check`             — bool, "Check for updates" button
//   `updates.restart`           — bool, "Restart & update" button
//   `updates.channel`           — string, release-channel dropdown
//   `updates.auto`              — bool, automatic-updates toggle
//   `updates.rollback`          — bool, "Roll back…" (opens the explanation)
//   `updates.rollback.confirm`  — bool, the second click, which acts

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Button, Column, Dropdown, Row, Spacer, Text, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::prefs::{SystemPrefs, UpdatePrefs};
use crate::widgets::{El, caption, card, choice_row, rows, setting_row, value_row};

pub const CHECK_KEY: &str = "updates.check";
pub const RESTART_KEY: &str = "updates.restart";
pub const AUTO_KEY: &str = "updates.auto";
pub const CHANNEL_KEY: &str = "updates.channel";
pub const ROLLBACK_KEY: &str = "updates.rollback";
/// Two clicks to roll back, because the first one is easy to make by accident
/// and the second comes after the pane has said what a rollback does not undo.
pub const ROLLBACK_CONFIRM_KEY: &str = "updates.rollback.confirm";

/// The channels offered by the release-channel dropdown.
pub const CHANNELS: &[&str] = &["Stable", "Testing", "Nightly"];

/// The system-derived part of the update state: the current image and any
/// image staged for the next boot. Preferences (channel, auto, last-checked)
/// live in [`UpdatePrefs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateStatus {
    pub current_version: String,
    pub staged_version: Option<String>,
}

impl UpdateStatus {
    /// Query the real system: pretty name from `/etc/os-release`, staged
    /// deployment from `bootc status`.
    pub fn from_system() -> Self {
        let current = fs::read_to_string(Path::new("/etc/os-release"))
            .ok()
            .and_then(|s| parse_pretty_name(&s))
            .unwrap_or_else(|| "qOS (Fedora bootc)".to_string());
        Self {
            current_version: current,
            staged_version: read_bootc_staged(),
        }
    }
}

fn parse_pretty_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some(rest) = line.trim().strip_prefix("PRETTY_NAME=") {
            let v = rest.trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Ask `bootc` whether an image is staged for the next boot. Returns `None`
/// when `bootc` is missing, fails, or reports nothing staged.
fn read_bootc_staged() -> Option<String> {
    let out = Command::new("bootc")
        .args(["status", "--format", "json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_bootc_staged(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `bootc status --format json` for a staged deployment. Defensive
/// against schema drift: any non-null `status.staged` counts as staged, and
/// we surface the first human-readable label we can find under it.
pub fn parse_bootc_staged(json: &str) -> Option<String> {
    let root: serde_json::Value = serde_json::from_str(json).ok()?;
    let staged = root.get("status")?.get("staged")?;
    if staged.is_null() {
        return None;
    }
    // Prefer an explicit version, then an image reference, else a generic
    // "ready" label so the surface is honest without inventing a version.
    if let Some(v) = staged.get("version").and_then(|v| v.as_str()) {
        return Some(v.to_string());
    }
    if let Some(img) = staged
        .get("image")
        .and_then(|i| i.get("image"))
        .and_then(|i| i.get("image"))
        .and_then(|v| v.as_str())
    {
        return Some(img.to_string());
    }
    Some("Update staged for next boot".to_string())
}

/// Side-effect interface for applying a staged update by rebooting. Tests
/// inject a recorder so the assertion doesn't reboot the machine.
pub trait Rebooter: Send + Sync {
    fn reboot(&self) -> Result<(), String>;
}

/// Drives the update timer and rollback for real. There is no preference to
/// keep in step with the timer: the timer *is* the setting, so this reads and
/// writes it directly and the switch shows what the machine will actually do.
///
/// A trait rather than direct `Command` calls for the reason [`Rebooter`] is
/// one: a test for "the switch turns the timer off" that turns the developer's
/// timer off is not a test anybody runs twice.
pub trait UpdateControl: Send + Sync {
    /// Is the update timer enabled? `None` when that cannot be determined —
    /// no systemd, no such unit, a container. The switch is then shown
    /// unavailable rather than showing a guess, because a switch that reports
    /// a state it did not read is the defect being fixed here.
    fn timer_enabled(&self) -> Option<bool>;
    /// Turn the update timer on or off, now and across reboots.
    fn set_timer(&self, on: bool) -> Result<(), String>;
    /// What this machine can go back to: `(entry label, image reference)`.
    fn rollback_target(&self) -> Option<String>;
    /// Make the next boot the previous deployment.
    fn roll_back(&self) -> Result<(), String>;
}

/// The words on the rollback card. Constants because they are the pane's
/// promise to the user and a test asserts on exactly these, on the production
/// path — a copy in a test would drift the first time somebody reworded one.
const ROLLBACK_CARD_TITLE: &str = "Go back to the previous image";
const NO_ROLLBACK_CAPTION: &str = "One appears after the first update. Until then the only way back is a \
     reinstall, which is why this says so instead of showing a control that \
     would fail.";
const ROLLBACK_INTRO_CAPTION: &str = "Rolling back makes the previous image the one this machine boots. Nothing \
     is deleted \u{2014} the image you are on now stays on the disk and in the \
     boot menu.";
/// What a rollback does not undo. Shown before the button that performs it, in
/// this order, because the second and third are the ones that surprise people.
const ROLLBACK_CAVEATS: &[&str] = &[
    "The system image reverts \u{2014} its programs, the compositor, the apps \
     that ship with it.",
    "Your files and app data do NOT revert. Workspaces, documents, keys and \
     downloads are shared by every image on this disk, so a rollback neither \
     restores them nor loses them.",
    "Apps you installed yourself do NOT revert either, and one built for a \
     newer system can stop launching until you reinstall it or roll forward \
     again.",
];
/// The "How updates work" note. The second line is the part a person cannot
/// discover by looking at the machine, which is the reason it is written down.
const HOW_UPDATES_WORK: &[&str] = &[
    "qOS updates are atomic: a new image is staged and applied on restart, so \
     an update never half-installs.",
    "If a new image fails to reach a login screen three times in a row, this \
     machine goes back to the previous one by itself. If there is nothing to \
     go back to, it starts in Repair Mode instead of leaving you at a black \
     screen \u{2014} choose it from the boot menu at any time.",
];

/// The unit that pulls and applies images. Named once.
const UPDATE_TIMER: &str = "bootc-fetch-apply-updates.timer";
/// The unit that performs a rollback as root. polkit authorises a local active
/// session in `wheel` to start exactly this one — see
/// `os/etc/polkit-1/rules.d/49-qos-updates.rules`.
const ROLLBACK_UNIT: &str = "qos-deployment-rollback.service";

/// The control for a machine that has none: no systemd, no bootc, a test.
///
/// Every answer is "cannot", never "off" or "nothing to roll back to". The
/// difference matters: a switch that reports a state it could not read is
/// exactly the failure this trait exists to remove, and a test that got a
/// silent `false` would be asserting about a machine that does not exist.
pub struct UnavailableControl;

impl UpdateControl for UnavailableControl {
    fn timer_enabled(&self) -> Option<bool> {
        None
    }
    fn set_timer(&self, _on: bool) -> Result<(), String> {
        Err("automatic updates cannot be changed from here on this machine".into())
    }
    fn rollback_target(&self) -> Option<String> {
        None
    }
    fn roll_back(&self) -> Result<(), String> {
        Err("rolling back is not available on this machine".into())
    }
}

/// Production impl: systemd for the timer, `bootc status` for the target, and
/// a one-shot unit for the rollback itself.
pub struct RealUpdateControl;

impl UpdateControl for RealUpdateControl {
    fn timer_enabled(&self) -> Option<bool> {
        let out = Command::new("systemctl")
            .args(["is-enabled", UPDATE_TIMER])
            .output()
            .ok()?;
        // `is-enabled` exits non-zero for "disabled", so the exit status is not
        // the answer — the word on stdout is. An unparseable word is `None`,
        // not `false`: "masked", "static" and "not-found" all mean something
        // this switch cannot represent, and rendering them as "off" would be a
        // switch that lies in the other direction.
        match String::from_utf8_lossy(&out.stdout).trim() {
            "enabled" | "enabled-runtime" => Some(true),
            "disabled" => Some(false),
            _ => None,
        }
    }

    fn set_timer(&self, on: bool) -> Result<(), String> {
        // --now so the change takes effect immediately as well as at boot;
        // without it, switching updates off leaves the timer armed until the
        // next reboot, which is most of the problem being fixed.
        let verb = if on { "enable" } else { "disable" };
        let out = Command::new("systemctl")
            .args([verb, "--now", UPDATE_TIMER])
            .output()
            .map_err(|e| format!("systemctl not available: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            format!("systemctl {verb} {UPDATE_TIMER} failed")
        } else {
            err
        })
    }

    fn rollback_target(&self) -> Option<String> {
        let out = Command::new("bootc")
            .args(["status", "--format", "json"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        parse_bootc_rollback(&String::from_utf8_lossy(&out.stdout))
    }

    fn roll_back(&self) -> Result<(), String> {
        let out = Command::new("systemctl")
            .args(["start", "--wait", ROLLBACK_UNIT])
            .output()
            .map_err(|e| format!("systemctl not available: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            format!("{ROLLBACK_UNIT} failed; see journalctl -u {ROLLBACK_UNIT}")
        } else {
            err
        })
    }
}

/// What `bootc status` says this machine can go back to.
///
/// Same defensive shape as [`parse_bootc_staged`]: any non-null
/// `status.rollback` counts, and the first human-readable label under it wins.
pub fn parse_bootc_rollback(json: &str) -> Option<String> {
    let root: serde_json::Value = serde_json::from_str(json).ok()?;
    let rb = root.get("status")?.get("rollback")?;
    if rb.is_null() {
        return None;
    }
    if let Some(img) = rb
        .get("image")
        .and_then(|i| i.get("image"))
        .and_then(|i| i.get("image"))
        .and_then(|v| v.as_str())
    {
        return Some(img.to_string());
    }
    if let Some(v) = rb
        .get("image")
        .and_then(|i| i.get("version"))
        .and_then(|v| v.as_str())
    {
        return Some(v.to_string());
    }
    Some("the previous image".to_string())
}

/// Production impl: `systemctl reboot`.
pub struct RealRebooter;

impl Rebooter for RealRebooter {
    fn reboot(&self) -> Result<(), String> {
        let out = Command::new("systemctl")
            .arg("reboot")
            .output()
            .map_err(|e| format!("systemctl not available: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

type StatusProvider = Arc<dyn Fn() -> UpdateStatus + Send + Sync>;
type Clock = Arc<dyn Fn() -> String + Send + Sync>;

/// A real wall-clock "last checked" stamp, e.g. `Monday, 4 July 2026 14:05`.
fn now_stamp() -> String {
    let date = os_actions::now_date().unwrap_or_default();
    let time = os_actions::now_clock(true).unwrap_or_default();
    format!("{date} {time}").trim().to_string()
}

pub struct UpdatesPane {
    status: UpdateStatus,
    prefs: UpdatePrefs,
    last_prefs: UpdatePrefs,
    status_provider: StatusProvider,
    clock: Clock,
    rebooter: Arc<dyn Rebooter>,
    control: Arc<dyn UpdateControl>,
    /// The update timer's actual state, as read from the machine. `None` means
    /// it could not be read, and the switch says so instead of guessing.
    timer_enabled: Option<bool>,
    /// What this machine can go back to, if anything.
    rollback_target: Option<String>,
    /// Set once the person has asked to roll back and been shown what it does
    /// not undo; cleared when they leave the pane's structure alone long
    /// enough to rebuild.
    rollback_armed: bool,
    /// The last thing that went wrong, shown in the pane — a failed action
    /// must surface here, not only in the journal.
    notice: Option<String>,
    dispatched_restart: bool,
    needs_seed: bool,
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for UpdatesPane {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdatesPane {
    pub fn new() -> Self {
        let status_provider: StatusProvider = Arc::new(UpdateStatus::from_system);
        let status = status_provider();
        let control: Arc<dyn UpdateControl> = Arc::new(RealUpdateControl);
        let mut prefs = SystemPrefs::load().updates;
        // The timer is the setting; the stored preference exists only so an
        // existing file need not be migrated, and is immediately overwritten
        // by what the machine actually reports.
        let timer_enabled = control.timer_enabled();
        if let Some(on) = timer_enabled {
            prefs.auto = on;
        }
        let rollback_target = control.rollback_target();
        Self {
            status,
            prefs,
            last_prefs: UpdatePrefs::default(),
            status_provider,
            clock: Arc::new(now_stamp),
            rebooter: Arc::new(RealRebooter),
            control,
            timer_enabled,
            rollback_target,
            rollback_armed: false,
            notice: None,
            dispatched_restart: false,
            needs_seed: true,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test constructor: inject the status provider, the clock, the rebooter,
    /// and the starting preferences so the actions are deterministic.
    ///
    /// Leaves the system control as a stub that can do nothing and says so, so
    /// a test written before that existed neither reaches systemd nor gets a
    /// switch that silently reports "off".
    pub fn with_parts(
        status: UpdateStatus,
        prefs: UpdatePrefs,
        status_provider: StatusProvider,
        clock: Clock,
        rebooter: Arc<dyn Rebooter>,
    ) -> Self {
        Self::with_parts_and_control(
            status,
            prefs,
            status_provider,
            clock,
            rebooter,
            Arc::new(UnavailableControl),
        )
    }

    /// [`UpdatesPane::with_parts`], plus the system control — the timer, the
    /// rollback target and the rollback itself.
    pub fn with_parts_and_control(
        status: UpdateStatus,
        prefs: UpdatePrefs,
        status_provider: StatusProvider,
        clock: Clock,
        rebooter: Arc<dyn Rebooter>,
        control: Arc<dyn UpdateControl>,
    ) -> Self {
        let timer_enabled = control.timer_enabled();
        let rollback_target = control.rollback_target();
        let mut prefs = prefs;
        if let Some(on) = timer_enabled {
            prefs.auto = on;
        }
        Self {
            status,
            prefs: prefs.clone(),
            last_prefs: prefs,
            status_provider,
            clock,
            rebooter,
            control,
            timer_enabled,
            rollback_target,
            rollback_armed: false,
            notice: None,
            dispatched_restart: false,
            needs_seed: true,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Is the machine's update timer on, as this pane last read it?
    pub fn timer_enabled(&self) -> Option<bool> {
        self.timer_enabled
    }

    /// The message currently shown in the pane, if any.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn rollback_target(&self) -> Option<&str> {
        self.rollback_target.as_deref()
    }

    /// The rollback card's title and lines, in order.
    ///
    /// Split out of `build_composition` rather than duplicated, so a test can
    /// assert on the words a person actually reads. Asserting on a `Text`
    /// widget's contents is not possible from here — the toolkit does not
    /// expose them — and asserting on a copy of the strings would prove
    /// nothing about what is rendered.
    pub fn rollback_card_lines(&self) -> Vec<String> {
        let mut out = vec![ROLLBACK_CARD_TITLE.to_string()];
        match (&self.rollback_target, self.rollback_armed) {
            (None, _) => {
                out.push("There is no previous image on this machine yet.".into());
                out.push(NO_ROLLBACK_CAPTION.into());
            }
            (Some(target), false) => {
                out.push(format!("Previous image: {target}"));
                out.push(ROLLBACK_INTRO_CAPTION.into());
                out.push("Roll back\u{2026}".into());
            }
            (Some(target), true) => {
                out.push(format!("Roll back to {target}?"));
                out.extend(
                    ROLLBACK_CAVEATS
                        .iter()
                        .map(std::string::ToString::to_string),
                );
                out.push("Roll back and restart later".into());
            }
        }
        out
    }

    /// The "How updates work" captions, in order.
    pub fn help_lines(&self) -> Vec<String> {
        HOW_UPDATES_WORK
            .iter()
            .map(std::string::ToString::to_string)
            .collect()
    }

    pub fn status(&self) -> &UpdateStatus {
        &self.status
    }

    pub fn prefs(&self) -> &UpdatePrefs {
        &self.prefs
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        // Status card — current + staged + the primary action.
        let action: El = match &self.status.staged_version {
            Some(staged) => Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(
                        Text::new(&format!("Update ready: {staged}")).with_size(13.0),
                    ))
                    .push(Box::new(
                        Text::new("Restart to apply the staged image.")
                            .with_size(11.0)
                            .with_color(ui::Theme::dark().text_dim),
                    ))
                    .push(Box::new(
                        Row::new()
                            .push(Box::new(Button::new(
                                "settings.updates.restart",
                                "\u{f0709}  Restart & update",
                                Some(RESTART_KEY),
                            )))
                            .push(Box::new(Spacer::new())),
                    )),
            ),
            None => Box::new(Text::new("Your system is up to date.").with_size(13.0)),
        };
        let status_card = card("Software updates", action);

        let detail = card(
            "Details",
            rows(vec![
                value_row("Current version", &self.status.current_version),
                value_row("Last checked", &self.prefs.last_checked),
                choice_row(
                    "Release channel",
                    Box::new(
                        Dropdown::new(
                            CHANNELS
                                .iter()
                                .map(std::string::ToString::to_string)
                                .collect(),
                            CHANNEL_KEY,
                        )
                        .with_id("updates.channel")
                        .with_placeholder(&self.prefs.channel),
                    ),
                ),
                match self.timer_enabled {
                    Some(_) => setting_row(
                        "Automatic updates",
                        Box::new(
                            Toggle::new("settings.updates.auto", AUTO_KEY).with_id("updates.auto"),
                        ),
                    ),
                    // Not a switch stuck in one position: a switch that cannot
                    // reach the timer would report "off" for a machine that is
                    // updating itself, which is worse than saying so.
                    None => value_row("Automatic updates", "unavailable on this machine"),
                },
                Box::new(
                    Row::new()
                        .push(Box::new(Button::new(
                            "settings.updates.check",
                            "\u{f0450}  Check for updates",
                            Some(CHECK_KEY),
                        )))
                        .push(Box::new(Spacer::new())),
                ),
            ]),
        );

        // The rollback card. Present whether or not there is anything to go
        // back to, because "this machine has nothing to roll back to" is
        // information a person looking for the control needs, and an absent
        // card reads as an absent feature.
        // Built from the same strings `rollback_card_lines` reports, so what a
        // test asserts on and what a person reads cannot come apart.
        let lines = self.rollback_card_lines();
        let rollback_body: El = match (&self.rollback_target, self.rollback_armed) {
            (None, _) => Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(Text::new(&lines[1]).with_size(13.0)))
                    .push(caption(&lines[2])),
            ),
            (Some(_), false) => Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(Text::new(&lines[1]).with_size(13.0)))
                    .push(caption(&lines[2]))
                    .push(Box::new(
                        Row::new()
                            .push(Box::new(Button::new(
                                "settings.updates.rollback",
                                &format!("\u{f0450}  {}", lines[3]),
                                Some(ROLLBACK_KEY),
                            )))
                            .push(Box::new(Spacer::new())),
                    )),
            ),
            // Armed: what it does not undo, in front of the button that does it.
            (Some(_), true) => {
                let mut col = Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(Text::new(&lines[1]).with_size(13.0)));
                for c in &lines[2..2 + ROLLBACK_CAVEATS.len()] {
                    col = col.push(caption(c));
                }
                Box::new(
                    col.push(Box::new(
                        Row::new()
                            .push(Box::new(Button::new(
                                "settings.updates.rollback.confirm",
                                &format!("\u{f0709}  {}", lines[lines.len() - 1]),
                                Some(ROLLBACK_CONFIRM_KEY),
                            )))
                            .push(Box::new(Spacer::new())),
                    )),
                )
            }
        };
        let rollback_card = card(ROLLBACK_CARD_TITLE, rollback_body);

        let note = card(
            "How updates work",
            rows(self.help_lines().iter().map(|l| caption(l)).collect()),
        );

        let mut all = vec![status_card, detail, rollback_card, note];
        if let Some(msg) = &self.notice {
            // In front of everything, because it is about the thing the person
            // just tried to do. A failed switch that only warns into the
            // journal is a switch that failed silently.
            all.insert(
                0,
                card(
                    "That did not work",
                    rows(vec![Box::new(Text::new(msg).with_size(13.0))]),
                ),
            );
        }
        crate::widgets::cards(all)
    }

    fn persist(&self) {
        let mut p = SystemPrefs::load();
        p.updates = self.prefs.clone();
        if let Err(e) = p.save() {
            warn!(target: "qos::settings", op = "update_prefs_save", error = %e, "failed to persist update prefs");
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(UpdatesPane::new())
}

impl Widget for UpdatesPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            let _ = state.put(root, CHANNEL_KEY, self.prefs.channel.as_str());
            let _ = state.put(
                root,
                AUTO_KEY,
                automerge::ScalarValue::Boolean(self.prefs.auto),
            );
            let _ = state.put(root, CHECK_KEY, automerge::ScalarValue::Boolean(false));
            let _ = state.put(root, RESTART_KEY, automerge::ScalarValue::Boolean(false));
            let _ = state.put(root, ROLLBACK_KEY, automerge::ScalarValue::Boolean(false));
            let _ = state.put(
                root,
                ROLLBACK_CONFIRM_KEY,
                automerge::ScalarValue::Boolean(false),
            );
            self.last_prefs = self.prefs.clone();
            self.needs_seed = false;
        }

        // Check for updates → re-query the system + stamp the time + persist.
        if read_bool(state, root, CHECK_KEY) {
            let _ = state.put(root, CHECK_KEY, automerge::ScalarValue::Boolean(false));
            self.status = (self.status_provider)();
            self.prefs.last_checked = (self.clock)();
            self.last_prefs.last_checked = self.prefs.last_checked.clone();
            self.persist();
            self.structure_dirty = true;
        }

        // Restart & update → reboot into the staged image (dedup).
        if read_bool(state, root, RESTART_KEY) {
            let _ = state.put(root, RESTART_KEY, automerge::ScalarValue::Boolean(false));
            if !self.dispatched_restart {
                self.dispatched_restart = true;
                if let Err(e) = self.rebooter.reboot() {
                    warn!(target: "qos::settings", op = "reboot", error = %e, "restart & update failed");
                }
            }
        }

        // Release channel → persist on change.
        if let Some(ch) = read_str(state, root, CHANNEL_KEY)
            && CHANNELS.contains(&ch.as_str())
            && ch != self.last_prefs.channel
        {
            self.last_prefs.channel = ch.clone();
            self.prefs.channel = ch;
            self.persist();
        }

        // Automatic updates → drive the timer, then report what the timer
        // actually says. Persisting the preference happens only once the
        // machine agrees, so the stored value never claims a state the timer
        // isn't actually in.
        if let Some(b) = read_bool_opt(state, root, AUTO_KEY)
            && b != self.last_prefs.auto
        {
            self.last_prefs.auto = b;
            match self.control.set_timer(b) {
                Ok(()) => {
                    // Read it back rather than assuming. `systemctl enable`
                    // can succeed on a unit that is masked or static, and
                    // the switch has to show what the machine will do.
                    self.timer_enabled = self.control.timer_enabled();
                    let effective = self.timer_enabled.unwrap_or(b);
                    self.prefs.auto = effective;
                    self.notice = (effective != b).then(|| {
                        format!(
                            "Automatic updates are still {}. The system did \
                             not accept the change.",
                            if effective { "on" } else { "off" }
                        )
                    });
                    self.persist();
                }
                Err(e) => {
                    warn!(target: "qos::settings", op = "set_update_timer", error = %e, "automatic updates could not be changed");
                    self.notice = Some(format!("Automatic updates could not be changed: {e}"));
                    // Put the switch back where the machine actually is.
                    self.timer_enabled = self.control.timer_enabled();
                    let effective = self.timer_enabled.unwrap_or(self.prefs.auto);
                    self.prefs.auto = effective;
                    self.last_prefs.auto = effective;
                    let _ = state.put(root, AUTO_KEY, automerge::ScalarValue::Boolean(effective));
                }
            }
            self.structure_dirty = true;
        }

        // Roll back, in two steps. The first only opens the explanation.
        if read_bool(state, root, ROLLBACK_KEY) {
            let _ = state.put(root, ROLLBACK_KEY, automerge::ScalarValue::Boolean(false));
            self.rollback_armed = true;
            self.notice = None;
            self.structure_dirty = true;
        }
        if read_bool(state, root, ROLLBACK_CONFIRM_KEY) {
            let _ = state.put(
                root,
                ROLLBACK_CONFIRM_KEY,
                automerge::ScalarValue::Boolean(false),
            );
            match self.control.roll_back() {
                Ok(()) => {
                    self.rollback_armed = false;
                    self.notice = Some(
                        "Done. The previous image is what this machine will boot \
                         next. Restart when you are ready \u{2014} nothing has \
                         been removed, so this is reversible."
                            .to_string(),
                    );
                    // The target moves once the order changes, so re-read it
                    // rather than showing the value from before the action.
                    self.rollback_target = self.control.rollback_target();
                }
                Err(e) => {
                    warn!(target: "qos::settings", op = "rollback", error = %e, "rollback failed");
                    self.notice = Some(format!("Rolling back failed: {e}"));
                }
            }
            self.structure_dirty = true;
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
        "UpdatesPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty());
        own || child
    }
}

/// Read a bool at `key`, treating absent/non-bool as `false`.
fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> bool {
    read_bool_opt(state, root, key).unwrap_or(false)
}

fn read_bool_opt(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
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
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingRebooter {
        calls: Mutex<u32>,
    }
    impl Rebooter for RecordingRebooter {
        fn reboot(&self) -> Result<(), String> {
            *self.calls.lock().unwrap() += 1;
            Ok(())
        }
    }

    fn pane_with(staged: Option<&str>, rebooter: Arc<RecordingRebooter>) -> UpdatesPane {
        let status = UpdateStatus {
            current_version: "qOS 2026.06".into(),
            staged_version: staged.map(std::string::ToString::to_string),
        };
        let requeried = UpdateStatus {
            current_version: "qOS 2026.06".into(),
            staged_version: Some("qOS 2026.07".into()),
        };
        UpdatesPane::with_parts(
            status,
            UpdatePrefs::default(),
            Arc::new(move || requeried.clone()),
            Arc::new(|| "CHECKED-NOW".to_string()),
            rebooter,
        )
    }

    #[test]
    fn parses_pretty_name() {
        assert_eq!(
            parse_pretty_name("PRETTY_NAME=\"qOS 42\"\n").as_deref(),
            Some("qOS 42")
        );
    }

    #[test]
    fn parse_bootc_staged_detects_and_ignores() {
        let staged =
            r#"{"status":{"staged":{"version":"qOS 2026.07"},"booted":{"version":"qOS 2026.06"}}}"#;
        assert_eq!(parse_bootc_staged(staged).as_deref(), Some("qOS 2026.07"));

        let none = r#"{"status":{"staged":null,"booted":{"version":"qOS 2026.06"}}}"#;
        assert_eq!(parse_bootc_staged(none), None);

        let image =
            r#"{"status":{"staged":{"image":{"image":{"image":"quay.io/qos/qos:latest"}}}}}"#;
        assert_eq!(
            parse_bootc_staged(image).as_deref(),
            Some("quay.io/qos/qos:latest")
        );
    }

    /// Check for updates re-queries the system and stamps a real
    /// last-checked time — proving the button acts, not no-ops.
    #[test]
    fn check_requeries_and_stamps_last_checked() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("updates_check");
        let reb = Arc::new(RecordingRebooter::default());
        let mut pane = pane_with(None, reb);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert_eq!(pane.prefs().last_checked, "Never");
        assert_eq!(pane.status().staged_version, None);
        // Press Check.
        let _ = doc.put(&root, CHECK_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        assert_eq!(
            pane.prefs().last_checked,
            "CHECKED-NOW",
            "timestamp updated"
        );
        assert_eq!(
            pane.status().staged_version.as_deref(),
            Some("qOS 2026.07"),
            "the re-query result landed"
        );
        assert!(pane.take_structure_dirty(), "check refreshes the view");
    }

    /// Restart & update reboots exactly once via the injected rebooter.
    #[test]
    fn restart_reboots_once() {
        let reb = Arc::new(RecordingRebooter::default());
        let mut pane = pane_with(Some("qOS 2026.07"), reb.clone());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, RESTART_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        // A second stray press must not reboot again.
        let _ = doc.put(&root, RESTART_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        assert_eq!(*reb.calls.lock().unwrap(), 1);
    }

    /// Changing the release channel is adopted (a live control), and only a
    /// known channel is accepted.
    #[test]
    fn channel_change_is_adopted_and_validated() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("updates_channel");
        let reb = Arc::new(RecordingRebooter::default());
        let mut pane = pane_with(None, reb);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, CHANNEL_KEY, "Nightly");
        pane.sync(&mut doc, &root);
        assert_eq!(pane.prefs().channel, "Nightly");
        // An unknown value is ignored.
        let _ = doc.put(&root, CHANNEL_KEY, "Bogus");
        pane.sync(&mut doc, &root);
        assert_eq!(pane.prefs().channel, "Nightly");
    }

    // ---- timer + rollback control -------------------------------------------

    /// A machine, recorded. `timer` is what `systemctl is-enabled` would say.
    struct FakeControl {
        timer: Mutex<Option<bool>>,
        set_calls: Mutex<Vec<bool>>,
        rollbacks: Mutex<u32>,
        target: Mutex<Option<String>>,
        set_fails: bool,
        rollback_fails: bool,
    }

    impl FakeControl {
        fn new(timer: Option<bool>, target: Option<&str>) -> Self {
            Self {
                timer: Mutex::new(timer),
                set_calls: Mutex::new(Vec::new()),
                rollbacks: Mutex::new(0),
                target: Mutex::new(target.map(str::to_string)),
                set_fails: false,
                rollback_fails: false,
            }
        }
        fn failing_set(mut self) -> Self {
            self.set_fails = true;
            self
        }
        fn failing_rollback(mut self) -> Self {
            self.rollback_fails = true;
            self
        }
    }

    impl UpdateControl for FakeControl {
        fn timer_enabled(&self) -> Option<bool> {
            *self.timer.lock().unwrap()
        }
        fn set_timer(&self, on: bool) -> Result<(), String> {
            self.set_calls.lock().unwrap().push(on);
            if self.set_fails {
                return Err("polkit said no".into());
            }
            *self.timer.lock().unwrap() = Some(on);
            Ok(())
        }
        fn rollback_target(&self) -> Option<String> {
            self.target.lock().unwrap().clone()
        }
        fn roll_back(&self) -> Result<(), String> {
            *self.rollbacks.lock().unwrap() += 1;
            if self.rollback_fails {
                return Err("the boot loader would not take it".into());
            }
            *self.target.lock().unwrap() = None;
            Ok(())
        }
    }

    fn pane_with_control(control: Arc<FakeControl>) -> UpdatesPane {
        let status = UpdateStatus {
            current_version: "qOS 2026.06".into(),
            staged_version: None,
        };
        let requeried = status.clone();
        UpdatesPane::with_parts_and_control(
            status,
            UpdatePrefs::default(),
            Arc::new(move || requeried.clone()),
            Arc::new(|| "CHECKED-NOW".to_string()),
            Arc::new(RecordingRebooter::default()),
            control,
        )
    }

    /// The switch shows the timer, not a stored preference, even when the two
    /// disagree.
    #[test]
    fn the_switch_opens_showing_what_the_timer_actually_is() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("updates_auto_reads");
        // A preferences file that says "off" and a timer that is on.
        let control = Arc::new(FakeControl::new(Some(true), None));
        let pane = UpdatesPane::with_parts_and_control(
            UpdateStatus {
                current_version: "qOS".into(),
                staged_version: None,
            },
            UpdatePrefs {
                auto: false,
                ..UpdatePrefs::default()
            },
            Arc::new(|| UpdateStatus {
                current_version: "qOS".into(),
                staged_version: None,
            }),
            Arc::new(|| "now".to_string()),
            Arc::new(RecordingRebooter::default()),
            control,
        );
        assert!(
            pane.prefs().auto,
            "the machine's timer wins over a stale preference; showing the \
             preference is how a switch comes to lie about the system"
        );
        assert_eq!(pane.timer_enabled(), Some(true));
    }

    /// Turning the switch off turns the timer off.
    #[test]
    fn turning_automatic_updates_off_disables_the_timer() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("updates_auto_off");
        let control = Arc::new(FakeControl::new(Some(true), None));
        let mut pane = pane_with_control(control.clone());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);

        let _ = doc.put(&root, AUTO_KEY, automerge::ScalarValue::Boolean(false));
        pane.sync(&mut doc, &root);

        assert_eq!(
            *control.set_calls.lock().unwrap(),
            vec![false],
            "the switch must reach the update timer exactly once"
        );
        assert_eq!(
            control.timer_enabled(),
            Some(false),
            "and the timer must actually be off afterwards"
        );
        assert_eq!(pane.timer_enabled(), Some(false));
        assert!(
            pane.notice().is_none(),
            "nothing went wrong, so say nothing"
        );
    }

    /// When the system refuses, the switch goes back and the pane says so on
    /// screen. A warning in the journal is, for a settings switch, the same as
    /// failing silently.
    #[test]
    fn a_refused_switch_snaps_back_and_is_reported_in_the_pane() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("updates_auto_refused");
        let control = Arc::new(FakeControl::new(Some(true), None).failing_set());
        let mut pane = pane_with_control(control.clone());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);

        let _ = doc.put(&root, AUTO_KEY, automerge::ScalarValue::Boolean(false));
        pane.sync(&mut doc, &root);

        assert_eq!(
            control.timer_enabled(),
            Some(true),
            "the timer must be untouched when the change was refused"
        );
        assert_eq!(
            read_bool_opt(&doc, &root, AUTO_KEY),
            Some(true),
            "the switch itself must snap back, or it shows a state the machine \
             is not in"
        );
        let notice = pane
            .notice()
            .expect("a refused change has to appear in the pane, not only the journal");
        assert!(
            notice.contains("polkit said no"),
            "the message must carry the reason, got: {notice}"
        );
    }

    /// A machine that cannot be asked gets no switch at all. Rendering "off"
    /// for a timer that could not be read is the same class of lie.
    #[test]
    fn an_unreadable_timer_is_not_rendered_as_off() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("updates_auto_unknown");
        let pane = pane_with_control(Arc::new(FakeControl::new(None, None)));
        assert_eq!(pane.timer_enabled(), None);
        // And the stub used by the older constructor answers the same way, so
        // a test written before this existed cannot get a confident `false`.
        assert_eq!(UnavailableControl.timer_enabled(), None);
        assert!(UnavailableControl.set_timer(true).is_err());
        assert!(UnavailableControl.roll_back().is_err());
    }

    /// Rolling back takes two clicks, and the first one only shows what a
    /// rollback does not undo.
    #[test]
    fn rolling_back_needs_a_confirmation_and_then_acts() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("updates_rollback");
        let control = Arc::new(FakeControl::new(Some(true), Some("qos:gen41")));
        let mut pane = pane_with_control(control.clone());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert_eq!(pane.rollback_target(), Some("qos:gen41"));

        // First click: nothing happens to the machine.
        let _ = doc.put(&root, ROLLBACK_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        assert_eq!(
            *control.rollbacks.lock().unwrap(),
            0,
            "the first click must only ask; a one-click rollback is a rollback \
             by accident"
        );

        // The confirmation view has to actually carry the caveats, or the
        // second click is uninformed.
        let shown = pane.rollback_card_lines().join("\n");
        for phrase in ["do NOT revert", "stop launching"] {
            assert!(
                shown.contains(phrase),
                "the confirmation must say what a rollback does not undo \
                 ({phrase} missing) — shown was:\n{shown}"
            );
        }

        // Second click: it acts.
        let _ = doc.put(
            &root,
            ROLLBACK_CONFIRM_KEY,
            automerge::ScalarValue::Boolean(true),
        );
        pane.sync(&mut doc, &root);
        assert_eq!(*control.rollbacks.lock().unwrap(), 1);
        let notice = pane.notice().expect("the result has to be shown");
        assert!(
            notice.contains("Restart"),
            "the person has to be told a restart is what takes it, got: {notice}"
        );
    }

    /// A failed rollback says so where the person is looking.
    #[test]
    fn a_failed_rollback_is_reported_in_the_pane() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("updates_rollback_fail");
        let control = Arc::new(FakeControl::new(Some(true), Some("qos:gen41")).failing_rollback());
        let mut pane = pane_with_control(control);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(
            &root,
            ROLLBACK_CONFIRM_KEY,
            automerge::ScalarValue::Boolean(true),
        );
        pane.sync(&mut doc, &root);
        let notice = pane.notice().expect("a failed rollback must be visible");
        assert!(
            notice.contains("the boot loader would not take it"),
            "the reason has to survive to the screen, got: {notice}"
        );
    }

    /// The help text must never promise a capability the pane does not offer
    /// — assert on what the pane renders, not on a comment.
    #[test]
    fn the_help_text_does_not_promise_what_is_not_offered() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("updates_help_text");
        let pane = pane_with_control(Arc::new(FakeControl::new(Some(true), Some("qos:gen41"))));
        let help = pane.help_lines().join("\n");
        assert!(
            !help.contains("you can roll back to the previous image at boot"),
            "the old sentence described a feature that did not exist; if it is \
             back, so is the lie. Help text was:\n{help}"
        );
        assert_eq!(
            pane.rollback_card_lines()[0],
            ROLLBACK_CARD_TITLE,
            "the rollback control has to be on the pane, since the pane talks \
             about rolling back"
        );
        assert!(
            help.contains("three times"),
            "the automatic half is the part a person cannot discover by \
             looking, so the pane has to state it. Help text was:\n{help}"
        );
    }

    /// A machine with nothing to go back to says so, rather than offering a
    /// button that would fail.
    #[test]
    fn no_previous_image_is_stated_rather_than_offered() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("updates_no_rollback");
        let pane = pane_with_control(Arc::new(FakeControl::new(Some(true), None)));
        let shown = pane.rollback_card_lines().join("\n");
        assert!(
            shown.contains("no previous image on this machine yet"),
            "the absence has to be stated; an absent card reads as an absent \
             feature. Shown was:\n{shown}"
        );
    }

    #[test]
    fn parse_bootc_rollback_detects_and_ignores() {
        let has = r#"{"status":{"rollback":{"image":{"image":{"image":"qos:gen41"}}}}}"#;
        assert_eq!(parse_bootc_rollback(has).as_deref(), Some("qos:gen41"));
        let none = r#"{"status":{"rollback":null}}"#;
        assert_eq!(parse_bootc_rollback(none), None);
        let bare = r#"{"status":{"rollback":{}}}"#;
        assert_eq!(
            parse_bootc_rollback(bare).as_deref(),
            Some("the previous image"),
            "a rollback deployment bootc describes in a shape we do not know \
             still exists, and hiding it would hide the way back"
        );
    }
}

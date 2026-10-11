//! Updates pane — the system image, firmware, the media codec pack, and
//! the automatic-update policy of this device.
//!
//! Every change is an [`UpdatesEffect`], answered by the [`UpdatesModel`]
//! the app keeps for the window: the model runs the system's update tools
//! off the UI thread, and the pane shows where each request stands.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use tracing::warn;
use ui::framework::{EffectReply, Effected};
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Column, Dropdown, ProgressBar, Row, Spacer, Text, Toggle, Widget};
use ui::{LocalState, WakeHandle};

use crate::codec_pack::{CodecAction, CodecPackStatus, MesaState, PackState};
use crate::effects::effect;
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::prefs::UpdatePrefs;
use crate::widgets::{
    El, caption, card, choice_row, error_caption, error_row, rows, setting_row, value_row,
};

const UPDATE_SCHEDULES: &[&str] = &["System schedule", "Daily", "Weekly"];
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
const LONG_UPDATE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_HELPER_OUTPUT: usize = 1024 * 1024;

fn run_bounded(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<bounded_command::Output, String> {
    bounded_command::run(program, args, None, MAX_HELPER_OUTPUT, timeout)
        .map_err(|error| format!("{program} command failed: {error}"))
}

/// The rollback target when one was read.
fn target_of(target: &Result<Option<String>, String>) -> Option<&str> {
    target.as_ref().ok().and_then(Option::as_deref)
}

fn schedule_shown(id: &str) -> String {
    match id {
        "Daily" => t("upd-schedule-daily"),
        "Weekly" => t("upd-schedule-weekly"),
        _ => t("upd-schedule-system"),
    }
}

fn schedule_from_label(label: &str) -> String {
    if label == "Daily" || label == t("upd-schedule-daily") {
        "Daily".into()
    } else if label == "Weekly" || label == t("upd-schedule-weekly") {
        "Weekly".into()
    } else if label == "System schedule" || label == t("upd-schedule-system") {
        "System schedule".into()
    } else {
        label.to_string()
    }
}

/// The system-derived part of the update state: the current image and any
/// image staged for the next boot. Preferences (channel, auto, last-checked)
/// live in [`UpdatePrefs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateStatus {
    pub current_version: String,
    pub staged_version: Option<String>,
    /// Why the staged image could not be read. While set, `staged_version`
    /// says nothing about the machine and the pane shows this instead.
    pub staged_error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateDetails {
    /// Recent real bootc unit journal lines, newest last.
    pub history: Vec<String>,
    /// OCI documentation/source URL declared by the current or staged image.
    pub release_notes: Option<String>,
    /// Whether Tilo's delayed boot confirmation/rollback floor is armed.
    pub rollback_health: Option<String>,
}

impl UpdateDetails {
    fn from_system(rollback_target: Option<&str>) -> Self {
        let bootc_json = run_bounded("bootc", &["status", "--format", "json"], STATUS_TIMEOUT)
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).to_string());
        let release_notes = bootc_json.as_deref().and_then(parse_bootc_release_notes);
        let history = run_bounded(
            "journalctl",
            &[
                "-u",
                "bootc-fetch-apply-updates.service",
                "-n",
                "6",
                "--output=short-iso",
                "--no-pager",
            ],
            STATUS_TIMEOUT,
        )
        .ok()
        .filter(|output| output.status.success())
        .map(|output| parse_update_history(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default();
        let protection_enabled = run_bounded(
            "systemctl",
            &["is-enabled", "tilo-rollback-confirm.timer"],
            STATUS_TIMEOUT,
        )
        .ok()
        .is_some_and(|output| {
            matches!(
                String::from_utf8_lossy(&output.stdout).trim(),
                "enabled" | "enabled-runtime"
            )
        });
        let rollback_health = Some(match (protection_enabled, rollback_target) {
            (true, Some(target)) => t_args("upd-health-protected", &[("target", target.into())]),
            (true, None) => t("upd-health-confirm"),
            (false, Some(target)) => t_args("upd-health-fallback", &[("target", target.into())]),
            (false, None) => t("upd-health-unknown"),
        });
        Self {
            history,
            release_notes,
            rollback_health,
        }
    }
}

pub fn parse_update_history(output: &str) -> Vec<String> {
    output
        .lines()
        .map(|line| {
            line.chars()
                .filter(|character| !character.is_control())
                .take(220)
                .collect::<String>()
        })
        .filter(|line| !line.trim().is_empty() && !line.contains("-- No entries --"))
        .collect()
}

pub fn parse_bootc_release_notes(json: &str) -> Option<String> {
    fn visit(value: &serde_json::Value) -> Option<String> {
        match value {
            serde_json::Value::Object(map) => {
                for key in [
                    "org.opencontainers.image.documentation",
                    "org.opencontainers.image.url",
                    "org.opencontainers.image.source",
                    "documentation",
                    "releaseNotes",
                ] {
                    if let Some(url) = map.get(key).and_then(serde_json::Value::as_str)
                        && matches!(url.split_once("://"), Some(("https", _)))
                        && url.len() <= 512
                    {
                        return Some(url.to_string());
                    }
                }
                map.values().find_map(visit)
            }
            serde_json::Value::Array(values) => values.iter().find_map(visit),
            _ => None,
        }
    }
    serde_json::from_str(json)
        .ok()
        .and_then(|root| visit(&root))
}

impl UpdateStatus {
    /// Query the real system: pretty name from `/etc/os-release`, staged
    /// deployment from `bootc status`.
    pub fn from_system() -> Self {
        let current = fs::read_to_string(Path::new("/etc/os-release"))
            .ok()
            .and_then(|s| parse_pretty_name(&s))
            .unwrap_or_else(|| "Tilo (Fedora bootc)".to_string());
        let (staged_version, staged_error) = match read_bootc_staged() {
            Ok(staged) => (staged, None),
            Err(error) => (None, Some(error)),
        };
        Self {
            current_version: current,
            staged_version,
            staged_error,
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

/// `bootc status --format json`, or why it could not be read.
fn read_bootc_status() -> Result<String, String> {
    let out = run_bounded("bootc", &["status", "--format", "json"], STATUS_TIMEOUT)?;
    if !out.status.success() {
        let diagnostic = out.stderr_diagnostic();
        return Err(if diagnostic.is_empty() {
            format!("bootc status exited with {}", out.status)
        } else {
            diagnostic
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Ask `bootc` whether an image is staged for the next boot: `Ok(None)` when
/// it reports nothing staged, `Err` when it could not be asked.
fn read_bootc_staged() -> Result<Option<String>, String> {
    read_bootc_status().map(|json| parse_bootc_staged(&json))
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
    Some(t("upd-staged-generic"))
}

/// Side-effect interface for applying a staged update by rebooting. Tests
/// inject a recorder so the assertion doesn't reboot the machine.
pub trait Rebooter: Send + Sync {
    /// # Errors
    /// Returns an error if the reboot could not be issued — e.g. the
    /// implementation's underlying reboot command is unavailable or reports
    /// failure, with that failure's reason.
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
    ///
    /// # Errors
    /// Returns an error if the timer could not be changed — e.g. the
    /// underlying `systemctl` call is unavailable, refused by polkit, or
    /// reports failure — with that failure's reason.
    fn set_timer(&self, on: bool) -> Result<(), String>;
    /// What this machine can go back to: `(entry label, image reference)`.
    ///
    /// # Errors
    /// Returns why the deployments could not be read.
    fn rollback_target(&self) -> Result<Option<String>, String>;
    /// Make the next boot the previous deployment.
    ///
    /// # Errors
    /// Returns an error if the rollback could not be started or reports
    /// failure, with that failure's reason.
    fn roll_back(&self) -> Result<(), String>;
    fn schedule(&self) -> Option<String> {
        None
    }
    /// # Errors
    /// Returns an error if this machine has no download-schedule policy
    /// service to ask (the default), or if that service refuses the
    /// requested schedule.
    fn set_schedule(&self, _schedule: &str) -> Result<(), String> {
        Err("update scheduling is unavailable on this machine".into())
    }
    fn allow_metered(&self) -> Option<bool> {
        None
    }
    /// # Errors
    /// Returns an error if this machine has no metered-connection policy
    /// service to ask (the default), or if that service refuses the request.
    fn set_allow_metered(&self, _allow: bool) -> Result<(), String> {
        Err("metered update policy is unavailable on this machine".into())
    }
    /// # Errors
    /// Returns an error if the update check could not be started or reports
    /// failure (the default: this machine cannot check for updates from
    /// here).
    fn check_now(&self) -> Result<(), String> {
        Err("checking for updates is unavailable on this machine".into())
    }
    /// # Errors
    /// Returns an error when firmware metadata or history cannot be refreshed.
    fn refresh_firmware(
        &self,
    ) -> Result<
        (
            Vec<os_actions::FirmwareUpdate>,
            Vec<os_actions::FirmwareHistoryEntry>,
        ),
        String,
    > {
        Err("firmware refresh is unavailable on this machine".into())
    }
}

/// The words on the rollback card. Constants because they are the pane's
/// promise to the user and a test asserts on exactly these, on the production
/// path — a copy in a test would drift the first time somebody reworded one.
fn rollback_card_title() -> String {
    t("upd-rollback-title")
}

fn no_rollback_caption() -> String {
    t("upd-no-rollback-caption")
}

fn rollback_intro_caption() -> String {
    t("upd-rollback-intro")
}

/// What a rollback does not undo. Shown before the button that performs it, in
/// this order, because the second and third are the ones that surprise people.
fn rollback_caveats() -> [String; 3] {
    [
        t("upd-caveat-image"),
        t("upd-caveat-files"),
        t("upd-caveat-apps"),
    ]
}

/// The unit that pulls and applies images. Named once.
const UPDATE_TIMER: &str = "bootc-fetch-apply-updates.timer";
/// The unit that performs a rollback as root. polkit authorises a local active
/// session in `wheel` to start exactly this one — see
/// `os/etc/polkit-1/rules.d/49-tilo-updates.rules`.
const ROLLBACK_UNIT: &str = "tilo-deployment-rollback.service";
const UPDATE_POLICY_PATH: &str = "/var/lib/tilo/update-policy";

fn read_update_policy() -> std::collections::BTreeMap<String, String> {
    fs::read_to_string(UPDATE_POLICY_PATH)
        .ok()
        .map(|contents| {
            contents
                .lines()
                .filter_map(|line| line.split_once('='))
                .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn start_update_policy(action: &str) -> Result<(), String> {
    if !matches!(
        action,
        "schedule-system"
            | "schedule-daily"
            | "schedule-weekly"
            | "metered-allow"
            | "metered-avoid"
    ) {
        return Err("invalid update policy action".into());
    }
    let unit = format!("tilo-update-policy@{action}.service");
    let output = run_bounded("systemctl", &["start", "--wait", &unit], CONTROL_TIMEOUT)?;
    if output.status.success() {
        Ok(())
    } else {
        let error = output.stderr_diagnostic();
        Err(if error.is_empty() {
            format!("{unit} failed")
        } else {
            error
        })
    }
}

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
    fn rollback_target(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn roll_back(&self) -> Result<(), String> {
        Err("rolling back is not available on this machine".into())
    }

    // Hermetic/test control: the injected StatusProvider is the check source.
    fn check_now(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Production impl: systemd for the timer, `bootc status` for the target, and
/// a one-shot unit for the rollback itself.
pub struct RealUpdateControl;

impl UpdateControl for RealUpdateControl {
    fn timer_enabled(&self) -> Option<bool> {
        let out = run_bounded("systemctl", &["is-enabled", UPDATE_TIMER], STATUS_TIMEOUT).ok()?;
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
        let out = run_bounded("systemctl", &[verb, "--now", UPDATE_TIMER], CONTROL_TIMEOUT)?;
        if out.status.success() {
            return Ok(());
        }
        let err = out.stderr_diagnostic();
        Err(if err.is_empty() {
            format!("systemctl {verb} {UPDATE_TIMER} failed")
        } else {
            err
        })
    }

    fn rollback_target(&self) -> Result<Option<String>, String> {
        read_bootc_status().map(|json| parse_bootc_rollback(&json))
    }

    fn roll_back(&self) -> Result<(), String> {
        let out = run_bounded(
            "systemctl",
            &["start", "--wait", ROLLBACK_UNIT],
            Duration::from_secs(5 * 60),
        )?;
        if out.status.success() {
            return Ok(());
        }
        let err = out.stderr_diagnostic();
        Err(if err.is_empty() {
            format!("{ROLLBACK_UNIT} failed; see journalctl -u {ROLLBACK_UNIT}")
        } else {
            err
        })
    }

    fn schedule(&self) -> Option<String> {
        Some(
            match read_update_policy().get("schedule").map(String::as_str) {
                Some("daily") => "Daily",
                Some("weekly") => "Weekly",
                _ => "System schedule",
            }
            .to_string(),
        )
    }

    fn set_schedule(&self, schedule: &str) -> Result<(), String> {
        start_update_policy(match schedule {
            "System schedule" => "schedule-system",
            "Daily" => "schedule-daily",
            "Weekly" => "schedule-weekly",
            _ => return Err("unknown update schedule".into()),
        })
    }

    fn allow_metered(&self) -> Option<bool> {
        Some(
            read_update_policy()
                .get("metered")
                .is_some_and(|value| value == "allow"),
        )
    }

    fn set_allow_metered(&self, allow: bool) -> Result<(), String> {
        start_update_policy(if allow {
            "metered-allow"
        } else {
            "metered-avoid"
        })
    }

    fn check_now(&self) -> Result<(), String> {
        let output = run_bounded(
            "systemctl",
            &["start", "--wait", "tilo-deployment-update.service"],
            LONG_UPDATE_TIMEOUT,
        )?;
        if output.status.success() {
            Ok(())
        } else {
            let error = output.stderr_diagnostic();
            Err(if error.is_empty() {
                "update service failed; see its system journal".into()
            } else {
                error
            })
        }
    }

    fn refresh_firmware(
        &self,
    ) -> Result<
        (
            Vec<os_actions::FirmwareUpdate>,
            Vec<os_actions::FirmwareHistoryEntry>,
        ),
        String,
    > {
        os_actions::refresh_firmware_metadata()
            .and_then(|()| os_actions::get_firmware_updates())
            .and_then(|updates| {
                os_actions::get_firmware_history().map(|history| (updates, history))
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
/// The rebooter of a capture, which restarts nothing.
struct NoRestart;

impl Rebooter for NoRestart {
    fn reboot(&self) -> Result<(), String> {
        Err("a screenshot restarts nothing".into())
    }
}

pub struct RealRebooter;

impl Rebooter for RealRebooter {
    fn reboot(&self) -> Result<(), String> {
        let out = run_bounded("systemctl", &["reboot"], CONTROL_TIMEOUT)?;
        if out.status.success() {
            Ok(())
        } else {
            Err(out.stderr_diagnostic())
        }
    }
}

type StatusProvider = Arc<dyn Fn() -> UpdateStatus + Send + Sync>;
type Clock = Arc<dyn Fn() -> String + Send + Sync>;
type DetailsProvider = Arc<dyn Fn(Option<&str>) -> UpdateDetails + Send + Sync>;
/// Runs one codec-pack unit to completion (a stub in tests).
pub type CodecRunner = Arc<dyn Fn(CodecAction) -> Result<(), String> + Send + Sync>;
/// Reads the codec-pack helper's status file (a fixture in tests).
pub type CodecStatusProvider = Arc<dyn Fn() -> CodecPackStatus + Send + Sync>;

fn codecs_card_title() -> String {
    t("upd-codecs")
}

fn codecs_explanation() -> String {
    t("upd-codecs-explain")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum FirmwareUiState {
    #[default]
    Idle,
    Installing {
        percent: u8,
        message: String,
    },
    Complete {
        needs_reboot: bool,
    },
}

type CheckEvent = Result<(UpdateStatus, Result<Option<String>, String>, UpdateDetails), String>;
type FirmwareRefreshEvent = Result<
    (
        Vec<os_actions::FirmwareUpdate>,
        Vec<os_actions::FirmwareHistoryEntry>,
    ),
    String,
>;

/// A real wall-clock "last checked" stamp, e.g. `Monday, 4 July 2026 14:05`.
fn now_stamp() -> String {
    let now = ui::clock::local_now();
    format!(
        "{} {}",
        now.format("%A, %-d %B %Y"),
        ui::clock::time_of_day(&now)
    )
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent internal state flag on private pane \
              state, not a call-site boolean-blindness hazard: none are \
              constructor parameters"
)]
pub struct UpdatesModel {
    status: UpdateStatus,
    details: UpdateDetails,
    prefs: UpdatePrefs,
    last_prefs: UpdatePrefs,
    status_provider: StatusProvider,
    details_provider: DetailsProvider,
    clock: Clock,
    rebooter: Arc<dyn Rebooter>,
    control: Arc<dyn UpdateControl>,
    /// The update timer's actual state, as read from the machine. `None` means
    /// it could not be read, and the switch says so instead of guessing.
    timer_enabled: Option<bool>,
    schedule: Option<String>,
    allow_metered: Option<bool>,
    /// What this machine can go back to, if anything, or why that could not
    /// be read.
    rollback_target: Result<Option<String>, String>,
    /// `None` means fwupd is unavailable; `Some([])` means it answered and
    /// there are no applicable upgrades.
    firmware_updates: Result<Vec<os_actions::FirmwareUpdate>, String>,
    firmware_history: Result<Vec<os_actions::FirmwareHistoryEntry>, String>,
    firmware_state: FirmwareUiState,
    firmware_events: Option<std::sync::mpsc::Receiver<os_actions::FirmwareEvent>>,
    check_events: Option<Receiver<CheckEvent>>,
    firmware_refresh_events: Option<Receiver<FirmwareRefreshEvent>>,
    check_pending: bool,
    firmware_refresh_pending: bool,
    codec_status: CodecPackStatus,
    codec_status_provider: CodecStatusProvider,
    codec_runner: CodecRunner,
    /// The codec-pack unit currently running, and its completion.
    codec_pending: Option<CodecAction>,
    codec_events: Option<Receiver<Result<(), String>>>,
    /// Raised by the status-file watcher; the next sync re-reads the status,
    /// so progress written by the helper appears while it runs.
    codec_status_changed: Arc<AtomicBool>,
    wake: WakeHandle,
    /// Set once the person has asked to roll back and been shown what it does
    /// not undo; cleared when they leave the pane's structure alone long
    /// enough to rebuild.
    rollback_armed: bool,
    /// The last thing that went wrong, shown in the pane — a failed action
    /// must surface here, not only in the journal.
    notice: Option<String>,
    dispatched_restart: bool,
    /// The settings document the update preferences are kept in, and the
    /// door the pane's controls go through.
    cx: PaneCx,
    /// The callers waiting for a request to finish.
    replies: crate::panes::model::Replies,
    /// What the pane shows changed since it last looked.
    changed: bool,
}

impl UpdatesModel {
    /// The model of this device's updates, read from the system now.
    pub fn new(cx: PaneCx) -> Self {
        let status_provider: StatusProvider = Arc::new(UpdateStatus::from_system);
        let status = status_provider();
        let control: Arc<dyn UpdateControl> = Arc::new(RealUpdateControl);
        let mut prefs = cx.settings::<UpdatePrefs>().get();
        // The timer is the setting; the stored preference exists only so an
        // existing file need not be migrated, and is immediately overwritten
        // by what the machine actually reports.
        let timer_enabled = control.timer_enabled();
        if let Some(on) = timer_enabled {
            prefs.auto = on;
        }
        let rollback_target = control.rollback_target();
        let details = UpdateDetails::from_system(target_of(&rollback_target));
        let schedule = control.schedule();
        let allow_metered = control.allow_metered();
        let firmware_updates = os_actions::get_firmware_updates();
        let firmware_history = os_actions::get_firmware_history();
        let pane = Self {
            status,
            details,
            prefs,
            last_prefs: UpdatePrefs::default(),
            status_provider,
            details_provider: Arc::new(UpdateDetails::from_system),
            clock: Arc::new(now_stamp),
            rebooter: Arc::new(RealRebooter),
            control,
            timer_enabled,
            schedule,
            allow_metered,
            rollback_target,
            firmware_updates,
            firmware_history,
            firmware_state: FirmwareUiState::Idle,
            firmware_events: None,
            check_events: None,
            firmware_refresh_events: None,
            check_pending: false,
            firmware_refresh_pending: false,
            codec_status: CodecPackStatus::read(),
            codec_status_provider: Arc::new(CodecPackStatus::read),
            codec_runner: Arc::new(crate::codec_pack::run),
            codec_pending: None,
            codec_events: None,
            codec_status_changed: Arc::new(AtomicBool::new(false)),
            wake: WakeHandle::new(),
            rollback_armed: false,
            notice: None,
            dispatched_restart: false,
            cx,
            replies: crate::panes::model::Replies::default(),
            changed: false,
        };
        pane.watch_codec_status();
        pane
    }

    /// Test constructor: inject the status provider, the clock, the rebooter,
    /// and the starting preferences so the actions are deterministic.
    ///
    /// Leaves the system control as a stub that can do nothing and says so, so
    /// a test written before that existed neither reaches systemd nor gets a
    /// switch that silently reports "off".
    pub fn with_parts(
        cx: PaneCx,
        status: UpdateStatus,
        prefs: UpdatePrefs,
        status_provider: StatusProvider,
        clock: Clock,
        rebooter: Arc<dyn Rebooter>,
    ) -> Self {
        Self::with_parts_and_control(
            cx,
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
        cx: PaneCx,
        status: UpdateStatus,
        prefs: UpdatePrefs,
        status_provider: StatusProvider,
        clock: Clock,
        rebooter: Arc<dyn Rebooter>,
        control: Arc<dyn UpdateControl>,
    ) -> Self {
        let timer_enabled = control.timer_enabled();
        let rollback_target = control.rollback_target();
        let schedule = control.schedule();
        let allow_metered = control.allow_metered();
        let mut prefs = prefs;
        if let Some(on) = timer_enabled {
            prefs.auto = on;
        }
        Self {
            status,
            details: UpdateDetails::default(),
            prefs: prefs.clone(),
            last_prefs: prefs,
            status_provider,
            details_provider: Arc::new(|_| UpdateDetails::default()),
            clock,
            rebooter,
            control,
            timer_enabled,
            schedule,
            allow_metered,
            rollback_target,
            firmware_updates: Err("fwupd is unavailable in this test".into()),
            firmware_history: Err("fwupd is unavailable in this test".into()),
            firmware_state: FirmwareUiState::Idle,
            firmware_events: None,
            check_events: None,
            firmware_refresh_events: None,
            check_pending: false,
            firmware_refresh_pending: false,
            codec_status: CodecPackStatus::default(),
            codec_status_provider: Arc::new(CodecPackStatus::default),
            codec_runner: Arc::new(|_| Err("the codec pack is unavailable in this test".into())),
            codec_pending: None,
            codec_events: None,
            codec_status_changed: Arc::new(AtomicBool::new(false)),
            wake: WakeHandle::new(),
            rollback_armed: false,
            notice: None,
            dispatched_restart: false,
            cx,
            replies: crate::panes::model::Replies::default(),
            changed: false,
        }
    }

    /// What a capture shows: a staged image waiting for a restart, and
    /// nothing that reaches the system.
    pub fn sample(cx: PaneCx) -> Self {
        let status = UpdateStatus {
            current_version: "Tilo 2026.06".into(),
            staged_version: Some("Tilo 2026.07".into()),
            staged_error: None,
        };
        let staged = status.clone();
        Self::with_parts_and_control(
            cx,
            status,
            UpdatePrefs::default(),
            Arc::new(move || staged.clone()),
            Arc::new(|| "Never".to_string()),
            Arc::new(NoRestart),
            Arc::new(UnavailableControl),
        )
    }

    /// Replace the codec-pack status source and unit runner (tests and captures).
    #[must_use]
    pub fn with_codec_pack(mut self, status: CodecStatusProvider, runner: CodecRunner) -> Self {
        self.codec_status = status();
        self.codec_status_provider = status;
        self.codec_runner = runner;
        self
    }

    /// Follow the helper's status file, so the card shows its progress and
    /// background results (an install that finished once the device came
    /// online) while the pane is open. Stops when the pane is dropped.
    fn watch_codec_status(&self) {
        let changed = Arc::downgrade(&self.codec_status_changed);
        let wake = self.wake.clone();
        if let Err(error) = crate::codec_pack::watch(move || {
            let Some(changed) = changed.upgrade() else {
                return false;
            };
            changed.store(true, Ordering::Release);
            wake.wake();
            true
        }) {
            tracing::debug!(%error, "codec pack status is not watched");
        }
    }

    /// The status-file watcher saw a change (tests call this in place of the
    /// inotify thread).
    pub fn codec_status_file_changed(&self) {
        self.codec_status_changed.store(true, Ordering::Release);
    }

    fn refresh_codec_status_if_changed(&mut self) {
        if self.codec_status_changed.swap(false, Ordering::AcqRel) {
            let status = (self.codec_status_provider)();
            if status != self.codec_status {
                self.codec_status = status;
                self.changed = true;
            }
        }
    }

    pub fn codec_status(&self) -> &CodecPackStatus {
        &self.codec_status
    }

    /// The codec card's title and lines, in order, as the person reads them.
    pub fn codecs_card_lines(&self) -> Vec<String> {
        let mut out = vec![
            codecs_card_title(),
            codecs_explanation(),
            format!("{}: {}", t("upd-status"), self.codec_status_line()),
        ];
        if let Some(fraction) = self.codec_progress() {
            out.push(t_args(
                "upd-progress",
                &[("percent", format!("{:.0}", fraction * 100.0).into())],
            ));
        }
        if let Some(problem) = self.codec_status.refresh_problem() {
            out.push(problem);
        }
        if let Some(applies) = self.codec_status.when_it_applies() {
            out.push(applies.to_string());
        }
        for (label, _, _) in self.codec_buttons() {
            out.push(label);
        }
        out
    }

    /// Whether the helper has picked up the request this pane made: until it
    /// writes its first status, the card says the request is starting.
    fn codec_request_acknowledged(&self) -> bool {
        match self.codec_pending {
            Some(CodecAction::Install | CodecAction::Refresh) => matches!(
                self.codec_status.state,
                PackState::Installing | PackState::WaitingForNetwork
            ),
            Some(CodecAction::Remove) => self.codec_status.state == PackState::Removing,
            None => true,
        }
    }

    fn codec_status_line(&self) -> String {
        if self.codec_request_acknowledged() {
            return self.codec_status_shown();
        }
        match self.codec_pending {
            Some(CodecAction::Remove) => t("upd-codecs-removing"),
            // A refresh of a working pack keeps it "installed" throughout.
            Some(CodecAction::Refresh) => t("upd-codecs-checking-driver"),
            _ => t("upd-codecs-starting"),
        }
    }

    fn codec_status_shown(&self) -> String {
        match (self.codec_status.state, self.codec_status.mesa) {
            (PackState::NotInstalled, _) => t("upd-codecs-not-installed"),
            (PackState::WaitingForNetwork, _) => t("upd-codecs-waiting"),
            (PackState::Installing, _) => match &self.codec_status.progress {
                Some(progress) if !progress.label.is_empty() => match progress.megabytes() {
                    Some((done, total)) => t_args(
                        "upd-codecs-step-bytes",
                        &[
                            ("label", progress.label.as_str().into()),
                            ("done", done.to_string().into()),
                            ("total", total.to_string().into()),
                            ("step", progress.step.to_string().into()),
                            ("steps", progress.steps.to_string().into()),
                        ],
                    ),
                    None => t_args(
                        "upd-codecs-step",
                        &[
                            ("label", progress.label.as_str().into()),
                            ("step", progress.step.to_string().into()),
                            ("steps", progress.steps.to_string().into()),
                        ],
                    ),
                },
                _ => t("upd-codecs-downloading"),
            },
            (PackState::Removing, _) => t("upd-codecs-removing"),
            (PackState::Failed, _) if self.codec_status.last_error.is_empty() => {
                t("upd-codecs-failed")
            }
            (PackState::Failed, _) => t_args(
                "upd-codecs-failed-reason",
                &[("error", self.codec_status.last_error.as_str().into())],
            ),
            (PackState::Installed, MesaState::Enabled) => t("upd-codecs-installed-hw"),
            (PackState::Installed, MesaState::Waiting) => t("upd-codecs-installed-mesa"),
            (PackState::Installed, MesaState::Off | MesaState::Unsupported) => {
                t("upd-codecs-installed-sw")
            }
        }
    }

    fn codec_applies_shown(&self) -> Option<String> {
        if self.codec_status.state != PackState::Installed {
            return None;
        }
        Some(match self.codec_status.mesa {
            MesaState::Enabled => t("upd-codecs-applies-hw"),
            MesaState::Waiting | MesaState::Off | MesaState::Unsupported => {
                t("upd-codecs-applies-sw")
            }
        })
    }

    fn codec_refresh_problem_shown(&self) -> Option<String> {
        (self.codec_status.state == PackState::Installed
            && !self.codec_status.last_error.is_empty())
        .then(|| {
            t_args(
                "upd-codecs-refresh-failed",
                &[("error", self.codec_status.last_error.as_str().into())],
            )
        })
    }

    /// The bar's fill while an install runs: the helper's step count, or an
    /// empty bar while the request is starting.
    fn codec_progress(&self) -> Option<f32> {
        if self.codec_status.state == PackState::Installing {
            return Some(
                self.codec_status
                    .progress
                    .as_ref()
                    .map_or(0.0, crate::codec_pack::Progress::fraction),
            );
        }
        let starting =
            self.codec_pending == Some(CodecAction::Install) && !self.codec_request_acknowledged();
        starting.then_some(0.0)
    }

    /// (label, control id, action id) of the buttons the card offers.
    fn codec_buttons(&self) -> Vec<(String, &'static str, UpdatesEffect)> {
        if self.codec_pending.is_some() {
            return Vec::new();
        }
        self.codec_status
            .offered_actions()
            .into_iter()
            .map(|action| {
                let label = match (action, self.codec_status.state) {
                    (CodecAction::Install, PackState::Failed) => t("upd-codecs-try-again"),
                    (CodecAction::Install, _) => t("upd-codecs-install"),
                    (CodecAction::Remove, PackState::WaitingForNetwork) => t("upd-codecs-cancel"),
                    (CodecAction::Remove, _) => t("upd-codecs-remove"),
                    (CodecAction::Refresh, _) => t("upd-codecs-refresh"),
                };
                match action {
                    CodecAction::Install => (
                        label,
                        "settings.updates.codecs.install",
                        UpdatesEffect::InstallCodecs,
                    ),
                    CodecAction::Remove => (
                        label,
                        "settings.updates.codecs.remove",
                        UpdatesEffect::RemoveCodecs,
                    ),
                    CodecAction::Refresh => (
                        label,
                        "settings.updates.codecs.refresh",
                        UpdatesEffect::RefreshCodecs,
                    ),
                }
            })
            .collect()
    }

    fn codecs_card(&self) -> El {
        let mut body = vec![
            caption(&codecs_explanation()),
            value_row(&t("upd-status"), &self.codec_status_line()),
        ];
        if let Some(fraction) = self.codec_progress() {
            body.push(Box::new(
                ProgressBar::new(fraction).with_label(t("upd-installing-codecs")),
            ));
        }
        if let Some(problem) = self.codec_refresh_problem_shown() {
            body.push(caption(&problem));
        }
        if let Some(applies) = self.codec_applies_shown() {
            body.push(caption(&applies));
        }
        let buttons = self.codec_buttons();
        if !buttons.is_empty() {
            let mut row = Row::new();
            for (label, control_id, effect) in buttons {
                let primary = matches!(&effect, UpdatesEffect::InstallCodecs);
                let button = self.button(control_id, &label, effect);
                let button = if primary {
                    button.role(ui::widget::ButtonRole::Primary)
                } else {
                    button
                };
                row = row.push(Box::new(button));
            }
            body.push(Box::new(row.push(Box::new(Spacer::new()))));
        }
        card(&codecs_card_title(), rows(body))
    }

    fn change_codecs(&mut self, action: CodecAction) {
        let operation = Self::codec_operation(action);
        if self.codec_pending.is_some() {
            self.operation_failed(operation, "the codec pack is already being changed");
            self.changed = true;
            return;
        }
        let runner = Arc::clone(&self.codec_runner);
        let wake = self.wake.clone();
        let (tx, rx) = mpsc::channel();
        if std::thread::Builder::new()
            .name("tilo-codec-pack".into())
            .spawn(move || {
                let _ = tx.send(runner(action));
                wake.wake();
            })
            .is_err()
        {
            self.notice = Some(t("upd-notice-codecs-worker"));
            self.operation_failed(operation, "codec pack worker could not start");
        } else {
            self.notice = None;
            self.codec_events = Some(rx);
            self.codec_pending = Some(action);
        }
        self.changed = true;
    }

    const fn codec_operation(action: CodecAction) -> &'static str {
        match action {
            CodecAction::Install => UpdatesEffect::INSTALL_CODECS.id,
            CodecAction::Remove => UpdatesEffect::REMOVE_CODECS.id,
            CodecAction::Refresh => UpdatesEffect::REFRESH_CODECS.id,
        }
    }

    fn drain_codec_events(&mut self) {
        let (Some(events), Some(action)) = (self.codec_events.as_ref(), self.codec_pending) else {
            return;
        };
        let result = match events.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("codec pack worker stopped".into()),
        };
        self.finish_codec_operation(action, result);
    }

    fn finish_codec_operation(&mut self, action: CodecAction, result: Result<(), String>) {
        self.codec_events = None;
        self.codec_pending = None;
        self.codec_status = (self.codec_status_provider)();
        let operation = Self::codec_operation(action);
        match result {
            Ok(()) => {
                self.notice = None;
                self.operation_succeeded(operation, "codec pack changed");
            }
            Err(error) => {
                self.notice = Some(t_args(
                    "upd-notice-codecs",
                    &[("error", error.as_str().into())],
                ));
                self.operation_failed(
                    operation,
                    format!("The codec pack could not be changed: {error}"),
                );
            }
        }
        self.changed = true;
    }

    /// Block until the running codec-pack unit returns, then apply its result
    /// (tests; the pane itself drains without blocking on every sync).
    #[cfg(test)]
    fn wait_for_codec_operation(&mut self) {
        let (Some(events), Some(action)) = (self.codec_events.take(), self.codec_pending) else {
            return;
        };
        let result = events
            .recv()
            .unwrap_or_else(|_| Err("codec pack worker stopped".into()));
        self.finish_codec_operation(action, result);
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
        target_of(&self.rollback_target)
    }

    /// The rollback card's title and lines, in order.
    ///
    /// Split out of `build_composition` rather than duplicated, so a test can
    /// assert on the words a person actually reads. Asserting on a `Text`
    /// widget's contents is not possible from here — the toolkit does not
    /// expose them — and asserting on a copy of the strings would prove
    /// nothing about what is rendered.
    pub fn rollback_card_lines(&self) -> Vec<String> {
        let mut out = vec![rollback_card_title()];
        match (&self.rollback_target, self.rollback_armed) {
            (Err(error), _) => {
                out.push(t_args(
                    "upd-rollback-error",
                    &[("error", error.as_str().into())],
                ));
            }
            (Ok(None), _) => {
                out.push(t("upd-no-previous"));
                out.push(no_rollback_caption());
            }
            (Ok(Some(target)), false) => {
                out.push(t_args(
                    "upd-previous-image",
                    &[("image", target.as_str().into())],
                ));
                out.push(rollback_intro_caption());
                out.push(t("upd-roll-back"));
            }
            (Ok(Some(target)), true) => {
                out.push(t_args(
                    "upd-roll-back-to",
                    &[("image", target.as_str().into())],
                ));
                out.extend(rollback_caveats());
                out.push(t("upd-roll-back-later"));
            }
        }
        out
    }

    /// The "How updates work" captions, in order.
    pub fn help_lines(&self) -> Vec<String> {
        vec![t("upd-help-1"), t("upd-help-2")]
    }

    pub fn status(&self) -> &UpdateStatus {
        &self.status
    }

    pub fn prefs(&self) -> &UpdatePrefs {
        &self.prefs
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut all = vec![
            self.overview_card(),
            self.status_card(),
            self.firmware_card(),
            self.codecs_card(),
            self.detail_card(),
            self.lifecycle_card(),
            self.rollback_card(),
            self.help_card(),
        ];
        // Firmware history is useful only when fwupd answered and there is a
        // transaction to describe. If the service is unavailable, the single
        // firmware recovery card above owns that explanation and retry.
        if matches!(&self.firmware_history, Ok(history) if !history.is_empty()) {
            all.insert(3, self.firmware_history_card());
        }
        if let Some(msg) = &self.notice {
            // In front of everything, because it is about the thing the person
            // just tried to do. A failed switch that only warns into the
            // journal is a switch that failed silently.
            all.insert(
                0,
                card(
                    &t("upd-notice-title"),
                    rows(vec![Box::new(Text::new(msg).with_role(ui::TextRole::Body))]),
                ),
            );
        }
        crate::widgets::cards(all)
    }

    /// Status card — current + staged + the primary action.
    fn status_card(&self) -> El {
        if let Some(error) = &self.status.staged_error {
            return card(
                &t("upd-software"),
                error_caption(&t_args(
                    "upd-status-error",
                    &[("error", error.as_str().into())],
                )),
            );
        }
        let action: El = match &self.status.staged_version {
            Some(staged) => Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(
                        Text::new(&t_args("upd-ready", &[("version", staged.as_str().into())]))
                            .with_role(ui::TextRole::Body),
                    ))
                    .push(Box::new(
                        Text::new(&t("upd-restart-apply"))
                            .with_role(ui::TextRole::Caption)
                            .dimmed(),
                    ))
                    .push(Box::new(
                        Row::new()
                            .push(Box::new(self.button(
                                "settings.updates.restart",
                                &t("upd-restart-update"),
                                UpdatesEffect::RestartToUpdate,
                            )))
                            .push(Box::new(Spacer::new())),
                    )),
            ),
            None => Box::new(Text::new(&t("upd-up-to-date")).with_role(ui::TextRole::Body)),
        };
        card(&t("upd-software"), action)
    }

    fn overview_card(&self) -> El {
        let mut overview = vec![match &self.status.staged_error {
            Some(error) => error_row(
                &t("upd-os"),
                &t_args("upd-status-error", &[("error", error.as_str().into())]),
            ),
            None if self.status.staged_version.is_some() => {
                value_row(&t("upd-os"), &t("upd-os-staged"))
            }
            None => value_row(&t("upd-os"), &t("upd-os-current")),
        }];
        if let Ok(updates) = &self.firmware_updates {
            let fw_value = if updates.is_empty() {
                t("upd-firmware-current")
            } else {
                t("upd-firmware-available")
            };
            overview.push(value_row(&t("upd-firmware"), &fw_value));
        }
        card(&t("upd-overview"), rows(overview))
    }

    fn detail_card(&self) -> El {
        let automatic = t("upd-automatic");
        let unavailable = t("upd-unavailable");
        let schedule_label = t("upd-schedule");
        let check_label = if self.check_pending {
            t("upd-checking")
        } else {
            t("upd-check")
        };
        let schedule_placeholder = self.schedule.as_deref().map(schedule_shown);
        card(
            &t("upd-details"),
            rows(vec![
                value_row(&t("upd-current-version"), &self.status.current_version),
                value_row(&t("upd-last-checked"), &self.prefs.last_checked),
                value_row(&t("upd-release-channel"), &t("upd-release-channel-value")),
                match self.timer_enabled {
                    Some(_) => setting_row(
                        &automatic,
                        Box::new(
                            self.cx.bind(
                                Toggle::from_state(&LocalState::new(self.prefs.auto))
                                    .with_id("updates.auto"),
                                effect(UpdatesEffect::SET_AUTOMATIC_UPDATES),
                            ),
                        ),
                    ),
                    // Not a switch stuck in one position: a switch that cannot
                    // reach the timer would report "off" for a machine that is
                    // updating itself, which is worse than saying so.
                    None => value_row(&automatic, &unavailable),
                },
                match &schedule_placeholder {
                    Some(schedule) => choice_row(
                        &schedule_label,
                        Box::new(
                            self.cx.bind(
                                Dropdown::from_state(
                                    UPDATE_SCHEDULES
                                        .iter()
                                        .map(|id| schedule_shown(id))
                                        .collect(),
                                    &LocalState::new(schedule.clone()),
                                )
                                .with_placeholder(schedule),
                                effect(UpdatesEffect::SET_UPDATE_SCHEDULE)
                                    .map(|shown: String| schedule_from_label(&shown)),
                            ),
                        ),
                    ),
                    None => value_row(&schedule_label, &unavailable),
                },
                match self.allow_metered {
                    Some(_) => setting_row(
                        &t("upd-metered"),
                        Box::new(
                            self.cx.bind(
                                Toggle::from_state(&LocalState::new(
                                    self.allow_metered.unwrap_or(false),
                                ))
                                .with_id("updates.metered"),
                                effect(UpdatesEffect::SET_METERED_UPDATES),
                            ),
                        ),
                    ),
                    None => value_row(&t("upd-metered-short"), &t("upd-metered-unavailable")),
                },
                Box::new(
                    Row::new()
                        .push(Box::new(self.button(
                            "settings.updates.check",
                            &check_label,
                            UpdatesEffect::CheckForUpdates,
                        )))
                        .push(Box::new(Spacer::new())),
                ),
            ]),
        )
    }

    fn lifecycle_card(&self) -> El {
        let not_reported = t("upd-not-reported");
        let no_doc = t("upd-no-doc-url");
        let mut lifecycle_rows = vec![value_row(
            &t("upd-rollback-health"),
            self.details
                .rollback_health
                .as_deref()
                .unwrap_or(&not_reported),
        )];
        lifecycle_rows.push(value_row(
            &t("upd-release-notes"),
            self.details.release_notes.as_deref().unwrap_or(&no_doc),
        ));
        if self.details.history.is_empty() {
            lifecycle_rows.push(caption(&t("upd-no-bootc-history")));
        } else {
            lifecycle_rows.extend(self.details.history.iter().map(|line| caption(line)));
        }
        card(&t("upd-lifecycle"), rows(lifecycle_rows))
    }

    fn firmware_card(&self) -> El {
        let body = match &self.firmware_updates {
            Err(error) => self.firmware_card_error(error),
            Ok(updates) if updates.is_empty() => self.firmware_card_up_to_date(),
            Ok(updates) => self.firmware_card_available(updates),
        };
        card(&t("upd-firmware"), body)
    }

    fn firmware_card_error(&self, error: &str) -> El {
        let refresh_label = if self.firmware_refresh_pending {
            t("upd-refreshing")
        } else {
            t("upd-try-again")
        };
        rows(vec![
            value_row(&t("upd-fw-status"), &t("upd-fw-query-failed")),
            caption(&t_args("upd-fwupd-reported", &[("error", error.into())])),
            Box::new(
                Row::new()
                    .push(Box::new(self.button(
                        "settings.updates.firmware.refresh",
                        &refresh_label,
                        UpdatesEffect::RefreshFirmware,
                    )))
                    .push(Box::new(Spacer::new())),
            ),
        ])
    }

    fn firmware_card_up_to_date(&self) -> El {
        let mut body = vec![value_row(&t("upd-fw-status"), &t("upd-fw-current-status"))];
        if matches!(
            self.firmware_state,
            FirmwareUiState::Complete { needs_reboot: true }
        ) {
            body.push(value_row(&t("upd-activation"), &t("upd-restart-required")));
            body.push(Box::new(
                Row::new()
                    .push(Box::new(self.button(
                        "settings.updates.firmware.restart",
                        &t("upd-restart-now"),
                        UpdatesEffect::RestartToUpdate,
                    )))
                    .push(Box::new(Spacer::new())),
            ));
        } else {
            let check_fw = if self.firmware_refresh_pending {
                t("upd-checking-firmware")
            } else {
                t("upd-check-firmware")
            };
            body.push(Box::new(
                Row::new()
                    .push(Box::new(self.button(
                        "settings.updates.firmware.refresh",
                        &check_fw,
                        UpdatesEffect::RefreshFirmware,
                    )))
                    .push(Box::new(Spacer::new())),
            ));
        }
        rows(body)
    }

    fn firmware_card_available(&self, updates: &[os_actions::FirmwareUpdate]) -> El {
        let mut items: Vec<El> = updates
            .iter()
            .map(|update| {
                let version = match (&update.current_version, &update.new_version) {
                    (Some(from), Some(to)) => format!("{from} → {to}"),
                    (_, Some(to)) => t_args("upd-update-to", &[("version", to.as_str().into())]),
                    _ => t("upd-update-available"),
                };
                value_row(&update.device, &version)
            })
            .collect();
        for update in updates {
            if let Some(description) = &update.description {
                items.push(caption(description));
            }
            if !update.problems.is_empty() {
                items.push(caption(&t_args(
                    "upd-before-installing",
                    &[("problems", update.problems.join(", ").into())],
                )));
            }
        }
        items.push(caption(&t("upd-fw-caption")));
        match &self.firmware_state {
            FirmwareUiState::Idle => items.push(Box::new(
                Row::new()
                    .push(Box::new(self.button(
                        "settings.updates.firmware.install",
                        &t("upd-install-firmware"),
                        UpdatesEffect::InstallFirmware,
                    )))
                    .push(Box::new(Spacer::new())),
            )),
            FirmwareUiState::Installing { percent, message } => {
                items.push(Box::new(ProgressBar::new(f32::from(*percent) / 100.0)));
                items.push(value_row(&t("upd-installing"), message));
            }
            FirmwareUiState::Complete { needs_reboot } => {
                let installed = if *needs_reboot {
                    t("upd-installed-reboot")
                } else {
                    t("upd-installed-ok")
                };
                items.push(value_row(&t("upd-installation"), &installed));
                if *needs_reboot {
                    items.push(Box::new(
                        Row::new()
                            .push(Box::new(self.button(
                                "settings.updates.firmware.restart",
                                &t("upd-restart-now"),
                                UpdatesEffect::RestartToUpdate,
                            )))
                            .push(Box::new(Spacer::new())),
                    ));
                }
            }
        }
        rows(items)
    }

    fn firmware_history_card(&self) -> El {
        match &self.firmware_history {
            Ok(history) if history.is_empty() => {
                card(&t("upd-fw-history"), caption(&t("upd-fw-history-empty")))
            }
            Ok(history) => {
                let mut entries = Vec::new();
                for item in history.iter().take(8) {
                    let versions = match (&item.from_version, &item.to_version) {
                        (Some(from), Some(to)) => format!("{from} → {to}\n{}", item.state),
                        (_, Some(to)) => format!("{to}\n{}", item.state),
                        _ => item.state.clone(),
                    };
                    entries.push(value_row(&item.device, &versions));
                    if let Some(error) = &item.error {
                        entries.push(caption(&t_args(
                            "upd-fwupd-error",
                            &[("error", error.as_str().into())],
                        )));
                    }
                }
                card(&t("upd-fw-history"), rows(entries))
            }
            Err(error) => card(
                &t("upd-fw-history"),
                rows(vec![
                    value_row(&t("upd-fw-status"), &t("upd-history-query-failed")),
                    caption(&t_args(
                        "upd-fwupd-reported",
                        &[("error", error.as_str().into())],
                    )),
                ]),
            ),
        }
    }

    /// The rollback card. Present whether or not there is anything to go
    /// back to, because "this machine has nothing to roll back to" is
    /// information a person looking for the control needs, and an absent
    /// card reads as an absent feature.
    /// Built from the same strings `rollback_card_lines` reports, so what a
    /// test asserts on and what a person reads cannot come apart.
    fn rollback_card(&self) -> El {
        let lines = self.rollback_card_lines();
        let rollback_body: El = match (&self.rollback_target, self.rollback_armed, lines.as_slice())
        {
            (Err(_), _, [_, error, ..]) => error_caption(error),
            (Ok(None), _, [_, no_previous, no_previous_caption, ..]) => Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(
                        Text::new(no_previous).with_role(ui::TextRole::Body),
                    ))
                    .push(caption(no_previous_caption)),
            ),
            (Ok(Some(_)), false, [_, previous, intro, button_label, ..]) => Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(Text::new(previous).with_role(ui::TextRole::Body)))
                    .push(caption(intro))
                    .push(Box::new(
                        Row::new()
                            .push(Box::new(self.button(
                                "settings.updates.rollback",
                                button_label,
                                UpdatesEffect::ReviewRollback,
                            )))
                            .push(Box::new(Spacer::new())),
                    )),
            ),
            // Armed: what it does not undo, in front of the button that does it.
            (Ok(Some(_)), true, [_, prompt, caveats @ .., confirm_label]) => {
                let mut col = Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(Text::new(prompt).with_role(ui::TextRole::Body)));
                for c in caveats {
                    col = col.push(caption(c));
                }
                Box::new(
                    col.push(Box::new(
                        Row::new()
                            .push(Box::new(self.button(
                                "settings.updates.rollback.confirm",
                                confirm_label,
                                UpdatesEffect::RollBack,
                            )))
                            .push(Box::new(Spacer::new())),
                    )),
                )
            }
            // `rollback_card_lines` always returns a shape matching one of
            // the arms above for the same `(rollback_target,
            // rollback_armed)` state it was built from; render nothing
            // sooner than panic if that invariant is ever broken.
            _ => Box::new(Column::new()),
        };
        card(&rollback_card_title(), rollback_body)
    }

    fn help_card(&self) -> El {
        card(
            &t("upd-help-title"),
            rows(self.help_lines().iter().map(|l| caption(l)).collect()),
        )
    }

    fn persist(&self) {
        if let Err(e) = self.cx.settings::<UpdatePrefs>().set(&self.prefs) {
            warn!(target: "tilo::settings", op = "update_prefs_save", error = %e, "failed to persist update prefs");
        }
    }

    /// Apply any pending firmware events from the background install, if one
    /// is running.
    fn drain_firmware_events(&mut self) {
        let mut finished = false;
        let events: Vec<os_actions::FirmwareEvent> = self
            .firmware_events
            .as_ref()
            .map(|events| events.try_iter().collect())
            .unwrap_or_default();
        {
            for event in events {
                match event {
                    os_actions::FirmwareEvent::Started { total } => {
                        self.firmware_state = FirmwareUiState::Installing {
                            percent: 0,
                            message: t_args(
                                "upd-fw-preparing",
                                &[("total", total.to_string().into())],
                            ),
                        };
                    }
                    os_actions::FirmwareEvent::Progress { percent, message } => {
                        let previous = match &self.firmware_state {
                            FirmwareUiState::Installing { percent, .. } => *percent,
                            _ => 0,
                        };
                        // fwupdmgr interleaves status lines without a numeric
                        // percentage. They update the message but must not make
                        // a visible progress bar jump backwards to zero.
                        self.firmware_state = FirmwareUiState::Installing {
                            percent: if percent == 0 {
                                previous
                            } else {
                                percent.max(previous)
                            },
                            message,
                        };
                    }
                    os_actions::FirmwareEvent::Completed(outcome) => {
                        self.firmware_state = FirmwareUiState::Complete {
                            needs_reboot: outcome.needs_reboot,
                        };
                        self.notice = Some(if outcome.needs_reboot {
                            t("upd-notice-fw-reboot")
                        } else {
                            t("upd-notice-fw-ok")
                        });
                        self.firmware_updates = os_actions::get_firmware_updates();
                        self.firmware_history = os_actions::get_firmware_history();
                        self.operation_succeeded(
                            UpdatesEffect::INSTALL_FIRMWARE.id,
                            "firmware installation finished",
                        );
                        finished = true;
                    }
                    os_actions::FirmwareEvent::Failed(error) => {
                        self.firmware_state = FirmwareUiState::Idle;
                        self.notice = Some(t_args(
                            "upd-notice-fw-failed",
                            &[("error", error.as_str().into())],
                        ));
                        self.operation_failed(
                            UpdatesEffect::INSTALL_FIRMWARE.id,
                            format!("Firmware installation failed: {error}"),
                        );
                        finished = true;
                    }
                }
                self.changed = true;
            }
        }
        if finished {
            self.firmware_events = None;
        }
    }

    fn drain_check_events(&mut self) {
        let Some(events) = self.check_events.as_ref() else {
            return;
        };
        let event = match events.try_recv() {
            Ok(event) => event,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.check_events = None;
                self.check_pending = false;
                self.notice = Some(t("upd-notice-check-stopped"));
                self.operation_failed(
                    UpdatesEffect::CHECK_FOR_UPDATES.id,
                    "update check worker stopped",
                );
                self.changed = true;
                return;
            }
        };
        self.check_events = None;
        self.check_pending = false;
        match event {
            Ok((status, rollback_target, details)) => {
                self.status = status;
                self.rollback_target = rollback_target;
                self.details = details;
                self.prefs.last_checked = (self.clock)();
                self.last_prefs.last_checked = self.prefs.last_checked.clone();
                self.notice = None;
                self.persist();
                self.operation_succeeded(
                    UpdatesEffect::CHECK_FOR_UPDATES.id,
                    "update check finished",
                );
            }
            Err(error) => {
                self.notice = Some(t_args(
                    "upd-notice-check",
                    &[("error", error.as_str().into())],
                ));
                self.operation_failed(
                    UpdatesEffect::CHECK_FOR_UPDATES.id,
                    format!("Updates could not be checked: {error}"),
                );
            }
        }
        self.changed = true;
    }

    fn drain_firmware_refresh_events(&mut self) {
        let Some(events) = self.firmware_refresh_events.as_ref() else {
            return;
        };
        let event = match events.try_recv() {
            Ok(event) => event,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.firmware_refresh_events = None;
                self.firmware_refresh_pending = false;
                let error = "firmware refresh worker stopped".to_string();
                self.firmware_updates = Err(error.clone());
                self.firmware_history = Err(error.clone());
                self.notice = Some(t_args(
                    "upd-notice-fw-refresh",
                    &[("error", error.as_str().into())],
                ));
                self.operation_failed(
                    UpdatesEffect::REFRESH_FIRMWARE.id,
                    format!("Firmware metadata could not be refreshed: {error}"),
                );
                self.changed = true;
                return;
            }
        };
        self.firmware_refresh_events = None;
        self.firmware_refresh_pending = false;
        match event {
            Ok((updates, history)) => {
                self.firmware_updates = Ok(updates);
                self.firmware_history = Ok(history);
                self.notice = None;
                self.operation_succeeded(
                    UpdatesEffect::REFRESH_FIRMWARE.id,
                    "firmware metadata refreshed",
                );
            }
            Err(error) => {
                self.firmware_updates = Err(error.clone());
                self.firmware_history = Err(error.clone());
                self.notice = Some(t_args(
                    "upd-notice-fw-refresh",
                    &[("error", error.as_str().into())],
                ));
                self.operation_failed(
                    UpdatesEffect::REFRESH_FIRMWARE.id,
                    format!("Firmware metadata could not be refreshed: {error}"),
                );
            }
        }
        self.changed = true;
    }

    /// Check for updates → re-query the system + stamp the time + persist.
    fn check(&mut self) {
        if self.check_pending {
            self.operation_failed(
                UpdatesEffect::CHECK_FOR_UPDATES.id,
                "an update check is already running",
            );
            self.changed = true;
            return;
        }
        let status_provider = Arc::clone(&self.status_provider);
        let details_provider = Arc::clone(&self.details_provider);
        let control = Arc::clone(&self.control);
        let wake = self.wake.clone();
        let (tx, rx) = mpsc::channel();
        if std::thread::Builder::new()
            .name("tilo-update-check".into())
            .spawn(move || {
                let result = control.check_now().map(|()| {
                    let status = status_provider();
                    let rollback_target = control.rollback_target();
                    let details = details_provider(target_of(&rollback_target));
                    (status, rollback_target, details)
                });
                let _ = tx.send(result);
                wake.wake();
            })
            .is_err()
        {
            self.notice = Some(t("upd-notice-check-worker"));
            self.operation_failed(
                UpdatesEffect::CHECK_FOR_UPDATES.id,
                "update check worker could not start",
            );
        } else {
            self.check_events = Some(rx);
            self.check_pending = true;
        }
        self.changed = true;
    }

    fn refresh_firmware(&mut self) {
        if self.firmware_refresh_pending || self.firmware_events.is_some() {
            self.operation_failed(
                UpdatesEffect::REFRESH_FIRMWARE.id,
                "firmware work is already in progress",
            );
            self.changed = true;
            return;
        }
        let control = Arc::clone(&self.control);
        let wake = self.wake.clone();
        let (tx, rx) = mpsc::channel();
        if std::thread::Builder::new()
            .name("tilo-firmware-refresh".into())
            .spawn(move || {
                let result = control.refresh_firmware();
                let _ = tx.send(result);
                wake.wake();
            })
            .is_err()
        {
            self.notice =
                Some("Firmware metadata could not be refreshed: worker could not start".into());
            self.operation_failed(
                UpdatesEffect::REFRESH_FIRMWARE.id,
                "firmware refresh worker could not start",
            );
        } else {
            self.firmware_refresh_events = Some(rx);
            self.firmware_refresh_pending = true;
        }
        self.changed = true;
    }

    fn install_firmware(&mut self) {
        if self.firmware_refresh_pending || self.firmware_events.is_some() {
            self.notice = Some(t("upd-notice-fw-busy"));
            self.operation_failed(
                UpdatesEffect::INSTALL_FIRMWARE.id,
                "firmware work is already in progress",
            );
            self.changed = true;
            return;
        }
        match self
            .firmware_updates
            .as_deref()
            .map_err(Clone::clone)
            .and_then(os_actions::start_firmware_install)
        {
            Ok(events) => {
                self.firmware_events = Some(events);
                self.firmware_state = FirmwareUiState::Installing {
                    percent: 0,
                    message: t("upd-fw-preparing-tx"),
                };
                self.notice = None;
            }
            Err(e) => {
                self.notice = Some(t_args(
                    "upd-notice-fw-install",
                    &[("error", e.as_str().into())],
                ));
                self.operation_failed(
                    UpdatesEffect::INSTALL_FIRMWARE.id,
                    format!("Firmware upgrades could not be installed: {e}"),
                );
            }
        }
        self.changed = true;
    }

    /// Restart & update → reboot into the staged image (dedup).
    fn restart(&mut self) {
        if self.dispatched_restart {
            self.operation_failed(
                UpdatesEffect::RESTART_TO_UPDATE.id,
                "the restart is already on its way",
            );
        } else {
            self.dispatched_restart = true;
            if let Err(e) = self.rebooter.reboot() {
                warn!(target: "tilo::settings", op = "reboot", error = %e, "restart & update failed");
                self.notice = Some(t_args(
                    "upd-notice-restart",
                    &[("error", e.as_str().into())],
                ));
                self.operation_failed(
                    UpdatesEffect::RESTART_TO_UPDATE.id,
                    format!("Restarting into the staged update failed: {e}"),
                );
                self.changed = true;
            } else {
                self.operation_succeeded(
                    UpdatesEffect::RESTART_TO_UPDATE.id,
                    "restart command accepted by the system",
                );
            }
        }
    }

    /// Automatic updates → drive the timer, then report what the timer
    /// actually says. Persisting the preference happens only once the
    /// machine agrees, so the stored value never claims a state the timer
    /// isn't actually in.
    fn set_automatic(&mut self, b: bool) {
        let operation = UpdatesEffect::SET_AUTOMATIC_UPDATES.id;
        if b == self.last_prefs.auto {
            self.operation_succeeded(operation, "automatic-update timer was already selected");
            return;
        }
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
                if effective == b {
                    self.operation_succeeded(operation, "automatic-update timer changed");
                } else {
                    self.operation_failed(
                        operation,
                        "the system did not accept the automatic-update timer change",
                    );
                }
            }
            Err(e) => {
                warn!(target: "tilo::settings", op = "set_update_timer", error = %e, "automatic updates could not be changed");
                self.notice = Some(t_args("upd-notice-auto", &[("error", e.as_str().into())]));
                // Put the switch back where the machine actually is.
                self.timer_enabled = self.control.timer_enabled();
                let effective = self.timer_enabled.unwrap_or(self.prefs.auto);
                self.prefs.auto = effective;
                self.last_prefs.auto = effective;
                self.operation_failed(
                    operation,
                    format!("Automatic updates could not be changed: {e}"),
                );
            }
        }
        self.changed = true;
    }

    fn set_schedule(&mut self, requested: &str) {
        let operation = UpdatesEffect::SET_UPDATE_SCHEDULE.id;
        let Some(current) = self.schedule.clone() else {
            self.operation_failed(
                operation,
                "the update schedule cannot be read on this device",
            );
            return;
        };
        let requested = schedule_from_label(requested);
        if requested == current || !UPDATE_SCHEDULES.contains(&requested.as_str()) {
            if requested == current {
                self.operation_succeeded(operation, "update schedule was already selected");
            } else {
                self.operation_failed(operation, "unsupported update schedule");
            }
            return;
        }
        match self.control.set_schedule(&requested) {
            Ok(()) => {
                self.schedule = match self.control.schedule() {
                    Some(schedule) => Some(schedule),
                    None => Some(requested),
                };
                self.notice = None;
                self.operation_succeeded(operation, "update schedule changed");
            }
            Err(error) => {
                self.notice = Some(t_args(
                    "upd-notice-schedule",
                    &[("error", error.as_str().into())],
                ));
                self.operation_failed(
                    operation,
                    format!("Download schedule could not be changed: {error}"),
                );
            }
        }
        self.changed = true;
    }

    fn set_metered(&mut self, requested: bool) {
        let Some(current) = self.allow_metered else {
            self.operation_failed(
                UpdatesEffect::SET_METERED_UPDATES.id,
                "the metered-network policy cannot be read on this device",
            );
            return;
        };
        if requested == current {
            self.operation_succeeded(
                UpdatesEffect::SET_METERED_UPDATES.id,
                "metered-network policy was already selected",
            );
            return;
        }
        match self.control.set_allow_metered(requested) {
            Ok(()) => {
                self.allow_metered = self.control.allow_metered().or(Some(requested));
                self.notice = None;
                self.operation_succeeded(
                    UpdatesEffect::SET_METERED_UPDATES.id,
                    "metered-network policy changed",
                );
            }
            Err(error) => {
                self.notice = Some(t_args(
                    "upd-notice-metered",
                    &[("error", error.as_str().into())],
                ));
                self.operation_failed(
                    UpdatesEffect::SET_METERED_UPDATES.id,
                    format!("Metered-network policy could not be changed: {error}"),
                );
            }
        }
        self.changed = true;
    }

    /// Roll back, in two steps. The first only opens the explanation.
    fn review_rollback(&mut self) {
        {
            self.rollback_armed = true;
            self.notice = None;
            self.operation_succeeded(UpdatesEffect::REVIEW_ROLLBACK.id, "rollback details opened");
            self.changed = true;
        }
    }

    fn roll_back(&mut self) {
        {
            match self.control.roll_back() {
                Ok(()) => {
                    self.rollback_armed = false;
                    self.notice = Some(t("upd-notice-rollback-ok"));
                    // The target moves once the order changes, so re-read it
                    // rather than showing the value from before the action.
                    self.rollback_target = self.control.rollback_target();
                    self.operation_succeeded(
                        UpdatesEffect::ROLL_BACK.id,
                        "previous system image selected for next boot",
                    );
                }
                Err(e) => {
                    warn!(target: "tilo::settings", op = "rollback", error = %e, "rollback failed");
                    self.notice = Some(t_args(
                        "upd-notice-rollback",
                        &[("error", e.as_str().into())],
                    ));
                    self.operation_failed(
                        UpdatesEffect::ROLL_BACK.id,
                        format!("Rolling back failed: {e}"),
                    );
                }
            }
            self.changed = true;
        }
    }
}

/// Updates, firmware and the media codec pack of this device.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum UpdatesEffect {
    /// Check for a new system image now.
    #[effect(job)]
    CheckForUpdates,
    /// Restart into the staged system image.
    #[effect(job, destructive)]
    RestartToUpdate,
    /// Show what rolling back to the previous system image does not undo.
    #[effect(job)]
    ReviewRollback,
    /// Boot the previous system image next time.
    #[effect(job, destructive)]
    RollBack,
    /// Ask the firmware service for new firmware.
    #[effect(job)]
    RefreshFirmware,
    /// Install the available firmware upgrades.
    #[effect(job, destructive)]
    InstallFirmware,
    /// Play more video and audio formats: download the media codecs to this device.
    #[effect(job)]
    InstallCodecs,
    /// Remove the downloaded media codecs.
    #[effect(job, destructive)]
    RemoveCodecs,
    /// Check again for hardware H.264/HEVC support.
    #[effect(job)]
    RefreshCodecs,
    /// Update the system automatically, or only when asked.
    #[effect(job, idempotent)]
    SetAutomaticUpdates { on: bool },
    /// Set when updates download.
    #[effect(job, idempotent)]
    SetUpdateSchedule {
        #[param("When", choices = UPDATE_SCHEDULES.iter().copied())]
        schedule: String,
    },
    /// Download updates on metered networks, or wait for another network.
    #[effect(job, idempotent)]
    SetMeteredUpdates { allowed: bool },
}

impl UpdatesModel {
    /// Start `effect`; `reply` is told how it ends.
    pub(crate) fn run(&mut self, effect: UpdatesEffect, reply: Option<EffectReply>) {
        if let Some(reply) = reply {
            self.replies.wait(effect.id(), reply);
        }
        match effect {
            UpdatesEffect::CheckForUpdates => self.check(),
            UpdatesEffect::RestartToUpdate => self.restart(),
            UpdatesEffect::ReviewRollback => self.review_rollback(),
            UpdatesEffect::RollBack => self.roll_back(),
            UpdatesEffect::RefreshFirmware => self.refresh_firmware(),
            UpdatesEffect::InstallFirmware => self.install_firmware(),
            UpdatesEffect::InstallCodecs => self.change_codecs(CodecAction::Install),
            UpdatesEffect::RemoveCodecs => self.change_codecs(CodecAction::Remove),
            UpdatesEffect::RefreshCodecs => self.change_codecs(CodecAction::Refresh),
            UpdatesEffect::SetAutomaticUpdates { on } => self.set_automatic(on),
            UpdatesEffect::SetUpdateSchedule { schedule } => self.set_schedule(&schedule),
            UpdatesEffect::SetMeteredUpdates { allowed } => self.set_metered(allowed),
        }
    }

    /// Tell the caller of `operation` it succeeded.
    fn operation_succeeded(&mut self, operation: &'static str, message: impl Into<String>) {
        self.replies.succeed(operation, message);
    }

    /// Tell the caller of `operation` why it failed.
    fn operation_failed(&mut self, operation: &'static str, message: impl Into<String>) {
        self.replies.fail(operation, message);
    }

    /// A button running `effect`.
    fn button(&self, control_id: &str, label: &str, effect: UpdatesEffect) -> Button {
        self.cx
            .bind_invocation(Button::new(control_id, label), effect)
    }
}

/// What the updates area keeps for the window: the model, read from the
/// system when first needed.
pub struct UpdatesArea {
    cx: PaneCx,
    model: std::cell::OnceCell<crate::panes::model::Shared<UpdatesModel>>,
}

impl UpdatesArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: std::cell::OnceCell::new(),
        }
    }

    /// The model, created on first use: a capture's reaches no system tool.
    pub fn model(&self) -> &crate::panes::model::Shared<UpdatesModel> {
        self.model.get_or_init(|| {
            let model = if self.cx.reaches_devices() {
                UpdatesModel::new(self.cx.clone())
            } else {
                UpdatesModel::sample(self.cx.clone())
            };
            let model = if !self.cx.reaches_devices()
                && let Some(status) = self.cx.scene().and_then(|scene| scene.codec_status.clone())
            {
                model.with_codec_pack(
                    Arc::new(move || status.clone()),
                    Arc::new(|_| Err("a capture cannot change installed formats".into())),
                )
            } else {
                model
            };
            std::rc::Rc::new(std::cell::RefCell::new(model))
        })
    }

    /// Take in finished background work, if the model exists.
    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get() {
            use crate::panes::model::PaneModel;
            let _ = model.borrow_mut().poll();
        }
    }
}

/// Answer `effect` on the area's model.
#[expect(
    clippy::unnecessary_wraps,
    reason = "every area answers through the effect vocabulary's one signature"
)]
pub(crate) fn answer(
    app: &crate::settings_app::SettingsApp,
    effect: UpdatesEffect,
    cx: &mut ui::framework::EffectCx<crate::resource::Settings, crate::settings_app::SettingsApp>,
) -> ui::framework::EffectAnswer {
    let reply = cx.reply();
    app.updates().model().borrow_mut().run(effect, Some(reply));
    Ok(Effected::Pending)
}

impl crate::panes::model::PaneModel for UpdatesModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        self.drain_check_events();
        self.drain_firmware_refresh_events();
        self.drain_firmware_events();
        self.drain_codec_events();
        self.refresh_codec_status_if_changed();
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "UpdatesPane"
    }

    fn busy(&self) -> bool {
        self.check_pending || self.firmware_refresh_pending || self.firmware_events.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panes::model::PaneModel;
    use std::sync::Mutex;

    /// A model's settings document of its own.
    fn test_cx() -> PaneCx {
        let profile = tempfile::tempdir().unwrap().keep();
        PaneCx::in_file(profile.join("settings.json"))
    }

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

    fn pane_with(staged: Option<&str>, rebooter: Arc<RecordingRebooter>) -> UpdatesModel {
        let status = UpdateStatus {
            current_version: "Tilo 2026.06".into(),
            staged_version: staged.map(std::string::ToString::to_string),
            staged_error: None,
        };
        let requeried = UpdateStatus {
            current_version: "Tilo 2026.06".into(),
            staged_version: Some("Tilo 2026.07".into()),
            staged_error: None,
        };
        UpdatesModel::with_parts(
            test_cx(),
            status,
            UpdatePrefs::default(),
            Arc::new(move || requeried.clone()),
            Arc::new(|| "CHECKED-NOW".to_string()),
            rebooter,
        )
    }

    /// A pane whose codec runner rewrites a fake status file the way the
    /// helper does and returns `outcome`; `status` is what the watcher reads.
    fn codec_pane(
        status: Arc<Mutex<String>>,
        runs: Arc<Mutex<Vec<CodecAction>>>,
        outcome: Result<(), String>,
    ) -> UpdatesModel {
        let reader = Arc::clone(&status);
        pane_with(None, Arc::new(RecordingRebooter::default())).with_codec_pack(
            Arc::new(move || CodecPackStatus::parse(&reader.lock().unwrap())),
            Arc::new(move |action| {
                runs.lock().unwrap().push(action);
                *status.lock().unwrap() = match (action, outcome.is_ok()) {
                    (CodecAction::Install | CodecAction::Refresh, true) => {
                        r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"waiting"},"desired":true}"#
                    }
                    (CodecAction::Install | CodecAction::Refresh, false) => {
                        r#"{"state":"failed","last_error":"offline","desired":true}"#
                    }
                    (CodecAction::Remove, _) => r#"{"state":"not-installed","desired":false}"#,
                }
                .to_string();
                outcome.clone()
            }),
        )
    }

    fn shared_status(json: &str) -> Arc<Mutex<String>> {
        Arc::new(Mutex::new(json.to_string()))
    }

    /// Press a codec control, then wait for the unit to return (no polling).
    fn press_and_finish(pane: &mut UpdatesModel, effect: UpdatesEffect) {
        pane.poll();
        pane.run(effect, None);
        pane.poll();
        pane.wait_for_codec_operation();
        pane.poll();
    }

    #[test]
    fn codec_card_is_one_short_line_and_one_button() {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let pane = codec_pane(shared_status(""), runs, Ok(()));
        assert_eq!(
            pane.codecs_card_lines(),
            [
                "Play more video and audio formats",
                "Downloaded to this device from the formats' publishers. You can turn it off here at any time.",
                "Status: Off",
                "Install",
            ]
        );
    }

    #[test]
    fn every_helper_state_has_its_status_and_controls() {
        let cases: [(&str, &str, &[&str]); 7] = [
            (r#"{"state":"not-installed"}"#, "Off", &["Install"]),
            (
                r#"{"state":"waiting-for-network","desired":true}"#,
                "Waiting for an internet connection",
                &["Cancel"],
            ),
            (
                r#"{"state":"installing","desired":true,"progress":{"step":3,"steps":7,"label":"Downloading"}}"#,
                "Downloading (step 3 of 7)",
                &[],
            ),
            (
                r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"enabled"},"desired":true}"#,
                "On, including hardware H.264/HEVC",
                &["Remove"],
            ),
            (
                r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"waiting"},"desired":true}"#,
                "follows once a matching driver is published",
                &["Check for hardware support again", "Remove"],
            ),
            (
                r#"{"state":"failed","last_error":"no route to host","desired":true}"#,
                "Could not install: no route to host",
                &["Try again", "Remove"],
            ),
            (r#"{"state":"removing"}"#, "Removing", &[]),
        ];
        for (json, status_line, controls) in cases {
            let runs = Arc::new(Mutex::new(Vec::new()));
            let pane = codec_pane(shared_status(json), runs, Ok(()));
            let lines = pane.codecs_card_lines();
            assert!(
                lines
                    .iter()
                    .any(|line| line.starts_with("Status: ") && line.contains(status_line)),
                "{json}: {lines:?}"
            );
            let buttons = pane.codec_buttons();
            let offered: Vec<&str> = buttons.iter().map(|b| b.0.as_str()).collect();
            assert_eq!(offered, controls, "{json}");
            // The card builds for every state.
            let _ = pane.codecs_card();
        }
    }

    #[test]
    fn a_running_install_shows_the_helpers_progress() {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let status = shared_status(
            r#"{"state":"installing","desired":true,"progress":{"step":3,"steps":5,"label":"Downloading","bytes":20000000,"total":40000000}}"#,
        );
        let pane = codec_pane(status, runs, Ok(()));
        let lines = pane.codecs_card_lines();
        assert!(lines.contains(&"Progress: 50%".to_string()), "{lines:?}");
        assert!(
            lines.contains(&"Status: Downloading 20 of 40 MB (step 3 of 5)\u{2026}".to_string()),
            "{lines:?}"
        );
    }

    #[test]
    fn the_watcher_updates_the_card_while_the_helper_works() {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let status = shared_status(r#"{"state":"waiting-for-network","desired":true}"#);
        let mut pane = codec_pane(Arc::clone(&status), runs, Ok(()));
        pane.poll();
        // The device came online and the helper installed in the background.
        *status.lock().unwrap() =
            r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"enabled"},"desired":true}"#.to_string();
        pane.poll();
        assert_eq!(pane.codec_status().state, PackState::WaitingForNetwork);
        pane.codec_status_file_changed();
        pane.poll();
        assert_eq!(pane.codec_status().state, PackState::Installed);
        assert!(
            pane.codecs_card_lines()
                .iter()
                .any(|line| line.contains("Sign out and back in"))
        );
    }

    #[test]
    fn install_runs_the_unit_and_shows_the_waiting_mesa_part() {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let mut pane = codec_pane(shared_status(""), Arc::clone(&runs), Ok(()));
        press_and_finish(&mut pane, UpdatesEffect::InstallCodecs);
        assert_eq!(*runs.lock().unwrap(), vec![CodecAction::Install]);
        assert_eq!(pane.codec_status().state, PackState::Installed);
        let lines = pane.codecs_card_lines();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("follows once a matching driver is published"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("No restart is needed"))
        );
        assert_eq!(lines.last().map(String::as_str), Some("Remove"));
        assert_eq!(pane.notice(), None);
    }

    #[test]
    fn a_request_the_helper_has_not_picked_up_reads_as_starting() {
        let (release, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let mut pane = pane_with(None, Arc::new(RecordingRebooter::default())).with_codec_pack(
            Arc::new(CodecPackStatus::default),
            Arc::new(move |_| {
                let _ = gate.lock().unwrap().recv();
                Ok(())
            }),
        );
        pane.poll();
        pane.run(UpdatesEffect::InstallCodecs, None);
        pane.poll();
        let lines = pane.codecs_card_lines();
        assert!(lines.contains(&"Status: Starting\u{2026}".to_string()));
        assert!(lines.contains(&"Progress: 0%".to_string()));
        assert!(
            !lines.iter().any(|line| line == "Install"),
            "no second request while one runs"
        );
        release.send(()).unwrap();
        pane.wait_for_codec_operation();
    }

    #[test]
    fn failed_install_surfaces_the_helper_error_and_offers_retry() {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let mut pane = codec_pane(shared_status(""), Arc::clone(&runs), Err("offline".into()));
        press_and_finish(&mut pane, UpdatesEffect::InstallCodecs);
        assert_eq!(
            pane.notice(),
            Some("The formats could not be changed: offline")
        );
        let lines = pane.codecs_card_lines();
        assert!(lines.contains(&"Status: Could not install: offline".to_string()));
        assert!(lines.contains(&"Try again".to_string()));
        press_and_finish(&mut pane, UpdatesEffect::InstallCodecs);
        assert_eq!(
            *runs.lock().unwrap(),
            vec![CodecAction::Install, CodecAction::Install]
        );
    }

    #[test]
    fn checking_again_for_the_mesa_build_runs_a_refresh() {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let status = shared_status(
            r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"waiting"},"desired":true}"#,
        );
        let mut pane = codec_pane(status, Arc::clone(&runs), Ok(()));
        press_and_finish(&mut pane, UpdatesEffect::RefreshCodecs);
        assert_eq!(*runs.lock().unwrap(), vec![CodecAction::Refresh]);
    }

    #[test]
    fn remove_returns_to_not_installed() {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let status = shared_status(
            r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"enabled"},"desired":true}"#,
        );
        let mut pane = codec_pane(status, Arc::clone(&runs), Ok(()));
        press_and_finish(&mut pane, UpdatesEffect::RemoveCodecs);
        assert_eq!(*runs.lock().unwrap(), vec![CodecAction::Remove]);
        assert_eq!(pane.codec_status().state, PackState::NotInstalled);
        assert_eq!(
            pane.codecs_card_lines().last().map(String::as_str),
            Some("Install")
        );
    }

    /// os-release, bootc staged/rollback, release notes and journal history parsing.
    #[test]
    fn update_status_parsers_accept_only_trusted_shapes() {
        // Case `parses_pretty_name`.
        {
            assert_eq!(
                parse_pretty_name("PRETTY_NAME=\"Tilo 42\"\n").as_deref(),
                Some("Tilo 42")
            );
        }

        // Case `parse_bootc_staged_detects_and_ignores`.
        {
            let staged = r#"{"status":{"staged":{"version":"Tilo 2026.07"},"booted":{"version":"Tilo 2026.06"}}}"#;
            assert_eq!(parse_bootc_staged(staged).as_deref(), Some("Tilo 2026.07"));

            let none = r#"{"status":{"staged":null,"booted":{"version":"Tilo 2026.06"}}}"#;
            assert_eq!(parse_bootc_staged(none), None);

            let image =
                r#"{"status":{"staged":{"image":{"image":{"image":"quay.io/tilo/tilo:latest"}}}}}"#;
            assert_eq!(
                parse_bootc_staged(image).as_deref(),
                Some("quay.io/tilo/tilo:latest")
            );
        }

        // Case `parse_bootc_rollback_detects_and_ignores`.
        {
            let has = r#"{"status":{"rollback":{"image":{"image":{"image":"tilo:gen41"}}}}}"#;
            assert_eq!(parse_bootc_rollback(has).as_deref(), Some("tilo:gen41"));
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

        // Case `release_notes_only_accept_https_image_metadata`.
        {
            let json = r#"{"status":{"staged":{"image":{"annotations":{"org.opencontainers.image.documentation":"https://updates.example.test/releases/42"}}}}}"#;
            assert_eq!(
                parse_bootc_release_notes(json).as_deref(),
                Some("https://updates.example.test/releases/42")
            );
            let unsafe_url = r#"{"releaseNotes":"file:///etc/shadow"}"#;
            assert_eq!(parse_bootc_release_notes(unsafe_url), None);
        }

        // Case `journal_history_is_bounded_and_control_free`.
        {
            let history = parse_update_history("2026-01-01 update complete\nline\u{0007} two\n");
            assert_eq!(history, ["2026-01-01 update complete", "line two"]);
        }
    }

    /// Checks and firmware refresh return promptly, stamp only on success, dedupe, and ignore replicated latches.
    #[test]
    fn update_checks_run_promptly_and_stamp_only_on_completion() {
        // Case `check_requeries_and_stamps_last_checked`.
        // Check for updates re-queries the system and stamps a real
        // last-checked time — proving the button acts, not no-ops.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let reb = Arc::new(RecordingRebooter::default());
            let mut pane = pane_with(None, reb);
            pane.poll();
            assert_eq!(pane.prefs().last_checked, "Never");
            assert_eq!(pane.status().staged_version, None);
            // Press Check.
            pane.run(UpdatesEffect::CheckForUpdates, None);
            pane.poll();
            settle(&mut pane, |pane| pane.check_pending);
            assert_eq!(
                pane.prefs().last_checked,
                "CHECKED-NOW",
                "timestamp updated"
            );
            assert_eq!(
                pane.status().staged_version.as_deref(),
                Some("Tilo 2026.07"),
                "the re-query result landed"
            );
        }

        // Case `blocking_update_check_returns_promptly_and_stamps_only_on_completion`.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let control = Arc::new(FakeControl::new(None, None));
            let release = control.gate_checks();
            let mut pane = pane_with_control(Arc::clone(&control));
            pane.poll();
            pane.run(UpdatesEffect::CheckForUpdates, None);
            // The check is held until released below: were `sync` to wait for
            // it, this call would only return once the gate timed out.
            pane.poll();
            assert!(pane.check_pending, "the check runs off the UI thread");
            assert_eq!(pane.prefs().last_checked, "Never");
            release.send(()).unwrap();
            settle(&mut pane, |pane| pane.check_pending);
            assert!(
                !control.gate_timed_out(),
                "sync blocked on the update check"
            );
            assert_eq!(pane.prefs().last_checked, "CHECKED-NOW");
            assert!(pane.notice().is_none());
        }

        // Case `failed_async_update_check_does_not_stamp_last_checked`.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let control = Arc::new(FakeControl::new(None, None).failing_check());
            let mut pane = pane_with_control(control);
            pane.poll();
            pane.run(UpdatesEffect::CheckForUpdates, None);
            settle(&mut pane, |pane| pane.check_pending);
            assert_eq!(pane.prefs().last_checked, "Never");
            assert!(
                pane.notice()
                    .is_some_and(|message| message.contains("could not be checked"))
            );
        }

        // Case `blocking_firmware_refresh_returns_promptly_and_reports_success_or_failure`.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let control = Arc::new(FakeControl::new(None, None));
            let release = control.gate_refreshes();
            let mut pane = pane_with_control(Arc::clone(&control));
            pane.poll();
            pane.run(UpdatesEffect::RefreshFirmware, None);
            pane.poll();
            assert!(pane.firmware_refresh_pending);
            release.send(()).unwrap();
            settle(&mut pane, |pane| pane.firmware_refresh_pending);
            assert!(
                !control.gate_timed_out(),
                "sync blocked on the firmware refresh"
            );
            assert_eq!(pane.firmware_updates, Ok(Vec::new()));

            control.set_refresh_fails(true);
            pane.run(UpdatesEffect::RefreshFirmware, None);
            pane.poll();
            assert!(pane.firmware_refresh_pending);
            release.send(()).unwrap();
            settle(&mut pane, |pane| pane.firmware_refresh_pending);
            assert!(pane.firmware_updates.is_err());
            assert!(pane.notice().is_some_and(|message| {
                message.contains("Firmware metadata could not be refreshed")
            }));
        }

        // Case `duplicate_update_checks_are_cleared_while_one_is_pending`.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let control = Arc::new(FakeControl::new(None, None));
            let release = control.gate_checks();
            let mut pane = pane_with_control(Arc::clone(&control));
            pane.poll();
            pane.run(UpdatesEffect::CheckForUpdates, None);
            pane.poll();
            assert!(pane.check_pending);
            // A second press while the first check is provably still running.
            pane.run(UpdatesEffect::CheckForUpdates, None);
            pane.poll();
            release.send(()).unwrap();
            settle(&mut pane, |pane| pane.check_pending);
            assert_eq!(*control.check_calls.lock().unwrap(), 1);
        }
    }

    /// Drive the host's polling until `pending` clears. The bound is a hang
    /// guard for a worker that never reports, not a performance budget.
    fn settle(pane: &mut UpdatesModel, pending: impl Fn(&UpdatesModel) -> bool) {
        let deadline = ui::time::now() + GATE_TIMEOUT;
        loop {
            pane.poll();
            if !pending(pane) {
                return;
            }
            assert!(
                ui::time::now() < deadline,
                "background worker never reported"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Restart reboots once; rollback is confirmed, reported, honestly described and absent without an image.
    #[test]
    fn restart_and_rollback_are_confirmed_and_reported() {
        // Case `restart_reboots_once`.
        // Restart & update reboots exactly once via the injected rebooter.
        {
            let reb = Arc::new(RecordingRebooter::default());
            let mut pane = pane_with(Some("Tilo 2026.07"), reb.clone());
            pane.poll();
            pane.run(UpdatesEffect::RestartToUpdate, None);
            pane.poll();
            // A second stray press must not reboot again.
            pane.run(UpdatesEffect::RestartToUpdate, None);
            pane.poll();
            assert_eq!(*reb.calls.lock().unwrap(), 1);
        }

        // Case `rolling_back_needs_a_confirmation_and_then_acts`.
        // Rolling back takes two clicks, and the first one only shows what a
        // rollback does not undo.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let control = Arc::new(FakeControl::new(Some(true), Some("tilo:gen41")));
            let mut pane = pane_with_control(control.clone());
            pane.poll();
            assert_eq!(pane.rollback_target(), Some("tilo:gen41"));

            // First click: nothing happens to the machine.
            pane.run(UpdatesEffect::ReviewRollback, None);
            pane.poll();
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
            pane.run(UpdatesEffect::RollBack, None);
            pane.poll();
            assert_eq!(*control.rollbacks.lock().unwrap(), 1);
            let notice = pane.notice().expect("the result has to be shown");
            assert!(
                notice.contains("Restart"),
                "the person has to be told a restart is what takes it, got: {notice}"
            );
        }

        // Case `a_failed_rollback_is_reported_in_the_pane`.
        // A failed rollback says so where the person is looking.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let control =
                Arc::new(FakeControl::new(Some(true), Some("tilo:gen41")).failing_rollback());
            let mut pane = pane_with_control(control);
            pane.poll();
            pane.run(UpdatesEffect::RollBack, None);
            pane.poll();
            let notice = pane.notice().expect("a failed rollback must be visible");
            assert!(
                notice.contains("the boot loader would not take it"),
                "the reason has to survive to the screen, got: {notice}"
            );
        }

        // Case `the_help_text_does_not_promise_what_is_not_offered`.
        // The help text must never promise a capability the pane does not offer
        // — assert on what the pane renders, not on a comment.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let pane =
                pane_with_control(Arc::new(FakeControl::new(Some(true), Some("tilo:gen41"))));
            let help = pane.help_lines().join("\n");
            assert!(
                !help.contains("you can roll back to the previous image at boot"),
                "the old sentence described a feature that did not exist; if it is \
             back, so is the lie. Help text was:\n{help}"
            );
            assert_eq!(
                pane.rollback_card_lines()[0],
                rollback_card_title(),
                "the rollback control has to be on the pane, since the pane talks \
             about rolling back"
            );
            assert!(
                help.contains("three times"),
                "the automatic half is the part a person cannot discover by \
             looking, so the pane has to state it. Help text was:\n{help}"
            );
        }

        // Case `no_previous_image_is_stated_rather_than_offered`.
        // A machine with nothing to go back to says so, rather than offering a
        // button that would fail.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let pane = pane_with_control(Arc::new(FakeControl::new(Some(true), None)));
            let shown = pane.rollback_card_lines().join("\n");
            assert!(
                shown.contains("no previous image on this machine yet"),
                "the absence has to be stated; an absent card reads as an absent \
             feature. Shown was:\n{shown}"
            );
        }
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
        check_gate: Mutex<Option<mpsc::Receiver<()>>>,
        check_fails: bool,
        refresh_gate: Mutex<Option<mpsc::Receiver<()>>>,
        refresh_fails: Mutex<bool>,
        check_calls: Mutex<u32>,
        gate_timed_out: std::sync::atomic::AtomicBool,
    }

    /// How long a gated fake waits for its release before giving up and
    /// recording that the caller held the UI thread.
    const GATE_TIMEOUT: Duration = Duration::from_secs(30);

    impl FakeControl {
        fn new(timer: Option<bool>, target: Option<&str>) -> Self {
            Self {
                timer: Mutex::new(timer),
                set_calls: Mutex::new(Vec::new()),
                rollbacks: Mutex::new(0),
                target: Mutex::new(target.map(str::to_string)),
                set_fails: false,
                rollback_fails: false,
                check_gate: Mutex::new(None),
                check_fails: false,
                refresh_gate: Mutex::new(None),
                refresh_fails: Mutex::new(false),
                check_calls: Mutex::new(0),
                gate_timed_out: std::sync::atomic::AtomicBool::new(false),
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
        fn failing_check(mut self) -> Self {
            self.check_fails = true;
            self
        }
        /// Hold every update check until the returned sender releases it.
        fn gate_checks(&self) -> mpsc::Sender<()> {
            let (release, gate) = mpsc::channel();
            *self.check_gate.lock().unwrap() = Some(gate);
            release
        }
        /// Hold every firmware refresh until the returned sender releases it.
        fn gate_refreshes(&self) -> mpsc::Sender<()> {
            let (release, gate) = mpsc::channel();
            *self.refresh_gate.lock().unwrap() = Some(gate);
            release
        }
        fn wait_at(&self, gate: &Mutex<Option<mpsc::Receiver<()>>>) {
            if let Some(gate) = gate.lock().unwrap().as_ref()
                && gate.recv_timeout(GATE_TIMEOUT).is_err()
            {
                self.gate_timed_out
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        fn gate_timed_out(&self) -> bool {
            self.gate_timed_out
                .load(std::sync::atomic::Ordering::SeqCst)
        }
        fn set_refresh_fails(&self, fails: bool) {
            *self.refresh_fails.lock().unwrap() = fails;
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
        fn rollback_target(&self) -> Result<Option<String>, String> {
            Ok(self.target.lock().unwrap().clone())
        }
        fn roll_back(&self) -> Result<(), String> {
            *self.rollbacks.lock().unwrap() += 1;
            if self.rollback_fails {
                return Err("the boot loader would not take it".into());
            }
            *self.target.lock().unwrap() = None;
            Ok(())
        }

        fn check_now(&self) -> Result<(), String> {
            *self.check_calls.lock().unwrap() += 1;
            self.wait_at(&self.check_gate);
            if self.check_fails {
                Err("update check failed".into())
            } else {
                Ok(())
            }
        }

        fn refresh_firmware(
            &self,
        ) -> Result<
            (
                Vec<os_actions::FirmwareUpdate>,
                Vec<os_actions::FirmwareHistoryEntry>,
            ),
            String,
        > {
            self.wait_at(&self.refresh_gate);
            if *self.refresh_fails.lock().unwrap() {
                Err("firmware refresh failed".into())
            } else {
                Ok((Vec::new(), Vec::new()))
            }
        }
    }

    /// `bootc` that cannot be asked (not root, absent, refused) is reported
    /// as that failure. "Your system is up to date" is a claim about the
    /// machine and is never made from a failed read.
    #[test]
    fn an_unreadable_bootc_status_is_not_reported_as_up_to_date() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let status = UpdateStatus {
            current_version: "Tilo 2026.06".into(),
            staged_version: None,
            staged_error: Some("error: This command requires root privileges".into()),
        };
        let requeried = status.clone();
        let pane = UpdatesModel::with_parts_and_control(
            test_cx(),
            status,
            UpdatePrefs::default(),
            Arc::new(move || requeried.clone()),
            Arc::new(|| "CHECKED-NOW".to_string()),
            Arc::new(RecordingRebooter::default()),
            Arc::new(FakeControl::new(Some(true), None)),
        );
        let text = crate::widgets::announced_text(pane.build_composition().as_ref());
        assert!(
            text.iter().any(|line| line
                == "Can't check for updates: error: This command requires root privileges"),
            "{text:#?}"
        );
        for claim in [t("upd-up-to-date"), t("upd-os-current")] {
            assert!(!text.contains(&claim), "claims {claim:?}: {text:#?}");
        }
    }

    fn pane_with_control(control: Arc<FakeControl>) -> UpdatesModel {
        let status = UpdateStatus {
            current_version: "Tilo 2026.06".into(),
            staged_version: None,
            staged_error: None,
        };
        let requeried = status.clone();
        UpdatesModel::with_parts_and_control(
            test_cx(),
            status,
            UpdatePrefs::default(),
            Arc::new(move || requeried.clone()),
            Arc::new(|| "CHECKED-NOW".to_string()),
            Arc::new(RecordingRebooter::default()),
            control,
        )
    }

    /// The switch mirrors the timer, disables it, snaps back on refusal and is absent when unreadable.
    #[test]
    fn automatic_update_switch_follows_the_timer() {
        // Case `the_switch_opens_showing_what_the_timer_actually_is`.
        // The switch shows the timer, not a stored preference, even when the two
        // disagree.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            // A preferences file that says "off" and a timer that is on.
            let control = Arc::new(FakeControl::new(Some(true), None));
            let pane = UpdatesModel::with_parts_and_control(
                test_cx(),
                UpdateStatus {
                    current_version: "Tilo".into(),
                    staged_version: None,
                    staged_error: None,
                },
                UpdatePrefs {
                    auto: false,
                    ..UpdatePrefs::default()
                },
                Arc::new(|| UpdateStatus {
                    current_version: "Tilo".into(),
                    staged_version: None,
                    staged_error: None,
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

        // Case `turning_automatic_updates_off_disables_the_timer`.
        // Turning the switch off turns the timer off.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let control = Arc::new(FakeControl::new(Some(true), None));
            let mut pane = pane_with_control(control.clone());
            pane.poll();

            pane.run(UpdatesEffect::SetAutomaticUpdates { on: false }, None);
            pane.poll();

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

        // Case `a_refused_switch_snaps_back_and_is_reported_in_the_pane`.
        // When the system refuses, the switch goes back and the pane says so on
        // screen. A warning in the journal is, for a settings switch, the same as
        // failing silently.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let control = Arc::new(FakeControl::new(Some(true), None).failing_set());
            let mut pane = pane_with_control(control.clone());
            pane.poll();

            pane.run(UpdatesEffect::SetAutomaticUpdates { on: false }, None);
            pane.poll();

            assert_eq!(
                control.timer_enabled(),
                Some(true),
                "the timer must be untouched when the change was refused"
            );
            assert!(
                pane.prefs().auto,
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

        // Case `an_unreadable_timer_is_not_rendered_as_off`.
        // A machine that cannot be asked gets no switch at all. Rendering "off"
        // for a timer that could not be read is the same class of lie.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let pane = pane_with_control(Arc::new(FakeControl::new(None, None)));
            assert_eq!(pane.timer_enabled(), None);
            // And the stub used by the older constructor answers the same way, so
            // a test written before this existed cannot get a confident `false`.
            assert_eq!(UnavailableControl.timer_enabled(), None);
            assert!(UnavailableControl.set_timer(true).is_err());
            assert!(UnavailableControl.roll_back().is_err());
        }
    }

    /// The effects reach the model the app keeps; a capture's reaches no
    /// system tool and says why it did nothing.
    #[test]
    fn effects_reach_the_model_and_report_how_they_end() {
        use crate::effects::SettingsEffect;
        use crate::resource::SettingsShell;
        use ui::prelude::TestApp;

        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.effect(&SettingsEffect::from(UpdatesEffect::CheckForUpdates))
            .expect("the check reads the sample image");
        let refused = app
            .effect_confirmed(&SettingsEffect::from(UpdatesEffect::RollBack))
            .unwrap_err();
        assert!(
            format!("{refused:?}").contains("Rolling back failed"),
            "{refused:?}"
        );
        app.effect(&SettingsEffect::from(
            crate::effects::NavigationEffect::OpenPane {
                pane: "updates".into(),
            },
        ))
        .unwrap();
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("Tilo 2026.07"), "{frame}");
    }
}

//! Updates: the system image, firmware, the media codec pack and Windows
//! support, and the automatic-update policy of this device.
//!
//! The system update is the session's `system-update` resource: the pane shows its reading as it
//! changes and acts through its actions. Each change is an [`UpdatesEffect`] answered by
//! [`UpdatesModel`], which runs the rest of the system's update tools off the UI thread.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::device_read::{Landed, Landing};
use std::time::Duration;

use provider_settings_ui::bound::SettingRows as _;
use tracing::warn;
use ui::framework::{EffectReply, Effected, ViewCx};
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Element, ProgressBar, Row, Text};
use ui::{LocalState, WakeHandle};

use crate::area::AreaContext;
#[cfg(test)]
use crate::channel;
use crate::codec_pack::{CodecAction, CodecPackStatus, MesaState, PackState};
use crate::effects::{SettingsEffect, effect};
use crate::i18n::{t, t_args};
use crate::prefs::UpdatePrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::windows_compat::{Readiness as WindowsReadiness, WindowsSupportAction};
use provider_settings_ui::widgets::{
    Body, El, Item, SettingsSection, block, error_caption, note, row, rows, section,
    setting_row_described, thing_row, value_row,
};

crate::effects::effect_family!(UpdatesEffect => answer);

crate::area::pane_area! {
    UpdatesArea;
    crate::area::serves!(Updates);
    fn poll(&self) {
        UpdatesArea::poll(self);
    }
    fn busy(&self) -> bool {
        UpdatesArea::busy(self)
    }
    fn statuses(&self) -> Vec<ui::framework::EffectStatus> {
        UpdatesArea::statuses(self)
    }
}

pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&UpdatesArea> {
    app.areas().find()
}

const UPDATE_SCHEDULES: &[&str] = &["System schedule", "Daily", "Weekly"];
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HELPER_OUTPUT: usize = 1024 * 1024;

fn command_output(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<bounded_command::Output, String> {
    bounded_command::run(program, args, None, MAX_HELPER_OUTPUT, timeout)
        .map_err(|error| format!("{program} command failed: {error}"))
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

use tilo_system_resources::effects::{PowerAction, UpdateAction};
/// The session's `system-update` resource: what is booted, what is staged and what finishing it
/// needs, as the session's system service reads the machine. The pane shows this reading and
/// acts through the resource's own actions; it reads nothing of the update itself.
pub use tilo_system_resources::update::{Deployment, Update};

/// The resource the pane follows, and its actions.
const SYSTEM_UPDATE: &str = tilo_system_resources::SYSTEM_UPDATE;
/// The power resource, whose restart a firmware update that waits for one takes.
const POWER: &str = tilo_system_resources::POWER;

/// The `system-update` resource's readings as they arrive; the window is woken at each.
pub trait UpdateFeed: Send + Sync {
    /// The newest reading since the last call, as its JSON; `None` when nothing new arrived.
    fn latest(&self) -> Option<Result<String, String>>;
}

impl UpdateFeed for ui::collaboration::SessionResource {
    fn latest(&self) -> Option<Result<String, String>> {
        ui::collaboration::SessionResource::latest(self)
    }
}

/// A reading held here: a capture's, a test's.
#[derive(Default)]
pub struct HeldReading(std::sync::Mutex<Option<Result<String, String>>>);

impl HeldReading {
    /// A feed that has `update` to read.
    #[must_use]
    pub fn of(update: &Update) -> Arc<Self> {
        let held = Arc::new(Self::default());
        held.set(update);
        held
    }

    /// Have `update` read next.
    pub fn set(&self, update: &Update) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(serde_json::to_string(update).map_err(|error| error.to_string()));
    }

    /// Have the session's refusal `error` read next.
    pub fn fail(&self, error: &str) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Err(error.to_owned()));
    }
}

impl UpdateFeed for HeldReading {
    fn latest(&self) -> Option<Result<String, String>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

/// The `system-update` and `power` resources' actions, invoked as the person.
pub trait UpdateActions {
    /// Invoke the action at `address`, whose id is `action`, carrying the person's
    /// confirmation when `confirmed`.
    fn invoke(
        &mut self,
        action: &'static str,
        address: ui::tilo_actions::ActionAddress,
        confirmed: bool,
    );
    /// The invocations the session answered since the last call: taken, or why not.
    fn answered(&mut self) -> Vec<(&'static str, Result<(), String>)>;
}

/// The actions as the session's broker takes them: decided as any caller's, and a confirmation
/// the person gave here carried with the one that asks for it.
pub struct SessionActions {
    remote: ui::RemoteActions,
    calls: Vec<(&'static str, ui::InvocationCall)>,
}

impl SessionActions {
    #[must_use]
    pub fn new(remote: ui::RemoteActions) -> Self {
        Self {
            remote,
            calls: Vec::new(),
        }
    }
}

impl UpdateActions for SessionActions {
    fn invoke(
        &mut self,
        action: &'static str,
        address: ui::tilo_actions::ActionAddress,
        confirmed: bool,
    ) {
        use ui::tilo_actions::remote::{
            Confirmation, InvokeArgument, RemoteActionTarget, session_actions,
        };
        let call = self.remote.invoke(
            RemoteActionTarget {
                provider: session_actions::PROVIDER.to_owned(),
                address,
            },
            InvokeArgument::None,
            confirmed.then_some(Confirmation::Confirmed),
        );
        self.calls.push((action, call));
    }

    fn answered(&mut self) -> Vec<(&'static str, Result<(), String>)> {
        use ui::tilo_actions::InvocationUpdate;
        self.remote.poll();
        let mut answered = Vec::new();
        self.calls.retain(|(action, call)| {
            let answer = match call.get() {
                ui::Arrival::Pending { .. } => return true,
                ui::Arrival::Ready(InvocationUpdate::Failed { error, .. }) => Err(error.message),
                ui::Arrival::Ready(InvocationUpdate::Cancelled { .. }) => {
                    Err("the session gave the action up".to_owned())
                }
                ui::Arrival::Ready(_) => Ok(()),
                ui::Arrival::Failed(error) => Err(error.to_string()),
            };
            answered.push((*action, answer));
            false
        });
        answered
    }
}

/// Actions a capture shows and nobody takes: each is refused.
#[derive(Default)]
struct NoActions(Vec<&'static str>);

impl UpdateActions for NoActions {
    fn invoke(&mut self, action: &'static str, _: ui::tilo_actions::ActionAddress, _: bool) {
        self.0.push(action);
    }

    fn answered(&mut self) -> Vec<(&'static str, Result<(), String>)> {
        self.0
            .drain(..)
            .map(|action| (action, Err("a capture takes no action".to_owned())))
            .collect()
    }
}

/// What the page says of the system update, from the resource's reading: one state, with the
/// action that fits it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateState {
    /// The first reading has not arrived.
    Reading,
    /// The session does not publish the update.
    Unreachable,
    /// Checking for an update, and fetching and installing one there is.
    Checking,
    /// The desktop restarts to finish the update.
    Finishing,
    /// The update could not be applied while the computer ran and was taken back: a restart
    /// completes it.
    Failed,
    /// Installed; only the desktop has to restart, and the apps stay open.
    DesktopRestart,
    /// The computer has to restart: the kernel, firmware or boot loader changed.
    Restart,
    /// The newest update is installed and running.
    Done,
    /// Nothing to do.
    UpToDate,
}

impl UpdateState {
    /// The state `update` is in, with the actions in `asked` that the session has not answered
    /// yet: a check or a desktop restart asked here shows at once.
    #[must_use]
    pub fn of(update: Option<&Result<Update, String>>, asked: &[&str]) -> Self {
        let update = match update {
            None => return Self::Reading,
            Some(Err(_)) => return Self::Unreachable,
            Some(Ok(update)) => update,
        };
        let running = |action: &str| {
            asked.contains(&action) || update.pending.iter().any(|pending| pending == action)
        };
        if checking(update) || running(tilo_system_resources::update::UPDATE_NOW) {
            Self::Checking
        } else if running(tilo_system_resources::update::RESTART_DESKTOP) {
            Self::Finishing
        } else if update.failed.is_some() {
            Self::Failed
        } else if update.desktop_pending && !update.restart_needed {
            Self::DesktopRestart
        } else if update.restart_needed {
            Self::Restart
        } else if update.applied {
            Self::Done
        } else {
            Self::UpToDate
        }
    }
}

/// The session checks for an update, or fetches and applies the one it found.
fn checking(update: &Update) -> bool {
    update.updating
        || update
            .pending
            .iter()
            .any(|pending| pending == tilo_system_resources::update::UPDATE_NOW)
}

/// The pane's effect that asks for the session's `action`.
fn operation_of(action: &str) -> &'static str {
    match action {
        tilo_system_resources::update::UPDATE_NOW => UpdatesEffect::CHECK_FOR_UPDATES.id,
        tilo_system_resources::update::RESTART_DESKTOP => UpdatesEffect::RESTART_DESKTOP.id,
        _ => UpdatesEffect::RESTART_TO_UPDATE.id,
    }
}

/// What a person reads when the session refused `action` or it did not finish. The session's
/// own words go to the log: they name units and tools, not what to do.
fn did_not_finish(action: &str) -> String {
    t(match action {
        tilo_system_resources::update::UPDATE_NOW => "upd-notice-check",
        tilo_system_resources::update::RESTART_DESKTOP => "upd-notice-desktop-failed",
        _ => "upd-notice-restart",
    })
}

/// What a person calls `deployment`: its version or image, and the day it was built.
fn deployment_shown(deployment: &Deployment) -> Option<String> {
    let name = deployment.name()?;
    Some(match deployment.built_on() {
        Some(date) => t_args(
            "upd-image-built",
            &[("image", name.into()), ("date", date.into())],
        ),
        None => name.to_owned(),
    })
}

/// The system's own name for what runs, where the deployments are its admins' alone to see.
fn os_release_name() -> Option<String> {
    fs::read_to_string(Path::new("/etc/os-release"))
        .ok()
        .and_then(|release| crate::panes::about::parse_os_release_pretty_name(&release))
}

/// What the Recovery card shows beside the reading: the update service's last lines, and
/// whether the boot check that takes the machine back from a start that does not come up is on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateDetails {
    pub history: Vec<String>,
    pub protected: bool,
}

impl UpdateDetails {
    fn from_system() -> Self {
        crate::device_read::off_ui_thread();
        let history = command_output(
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
        let protected = command_output(
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
        Self { history, protected }
    }
}

/// What protects this machine from an update that does not start, and what it goes back to.
fn rollback_health(protected: bool, target: Option<&str>) -> String {
    match (protected, target) {
        (true, Some(target)) => t_args("upd-health-protected", &[("target", target.into())]),
        (true, None) => t("upd-health-confirm"),
        (false, Some(target)) => t_args("upd-health-fallback", &[("target", target.into())]),
        (false, None) => t("upd-health-unknown"),
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

/// Drives the update timer and rollback for real. There is no preference to
/// keep in step with the timer: the timer *is* the setting, so this reads and
/// writes it directly and the switch shows what the machine will actually do.
pub trait UpdateControl: Send + Sync {
    /// Is the update timer enabled? `None` when that cannot be determined —
    /// no systemd, no such unit, a container. The switch is then shown
    /// unavailable rather than showing a guess, because a switch that reports
    /// a state it did not read is the defect being fixed here.
    fn timer_enabled(&self) -> Option<bool>;
    fn set_timer(&self, on: bool) -> Result<(), String>;
    fn roll_back(&self) -> Result<(), String>;
    fn schedule(&self) -> Option<String> {
        None
    }
    fn set_schedule(&self, _schedule: &str) -> Result<(), String> {
        Err("update scheduling is unavailable on this machine".into())
    }
    fn allow_metered(&self) -> Option<bool> {
        None
    }
    fn set_allow_metered(&self, _allow: bool) -> Result<(), String> {
        Err("metered update policy is unavailable on this machine".into())
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
        Err("firmware refresh is unavailable on this machine".into())
    }
}

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

const UPDATE_TIMER: &str = "bootc-fetch-apply-updates.timer";
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
    let output = command_output("systemctl", &["start", "--wait", &unit], CONTROL_TIMEOUT)?;
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
        crate::device_read::off_ui_thread();
        Err("automatic updates cannot be changed from here on this machine".into())
    }
    fn roll_back(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Err("rolling back is not available on this machine".into())
    }
}

pub struct RealUpdateControl;

impl UpdateControl for RealUpdateControl {
    fn timer_enabled(&self) -> Option<bool> {
        crate::device_read::off_ui_thread();
        let out =
            command_output("systemctl", &["is-enabled", UPDATE_TIMER], STATUS_TIMEOUT).ok()?;
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
        crate::device_read::off_ui_thread();
        // --now so the change takes effect immediately as well as at boot;
        // without it, switching updates off leaves the timer armed until the
        // next reboot, which is most of the problem being fixed.
        let verb = if on { "enable" } else { "disable" };
        let out = command_output("systemctl", &[verb, "--now", UPDATE_TIMER], CONTROL_TIMEOUT)?;
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

    fn roll_back(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        let out = command_output(
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
        crate::device_read::off_ui_thread();
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
        crate::device_read::off_ui_thread();
        start_update_policy(match schedule {
            "System schedule" => "schedule-system",
            "Daily" => "schedule-daily",
            "Weekly" => "schedule-weekly",
            _ => return Err("unknown update schedule".into()),
        })
    }

    fn allow_metered(&self) -> Option<bool> {
        crate::device_read::off_ui_thread();
        Some(
            read_update_policy()
                .get("metered")
                .is_some_and(|value| value == "allow"),
        )
    }

    fn set_allow_metered(&self, allow: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        start_update_policy(if allow {
            "metered-allow"
        } else {
            "metered-avoid"
        })
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
        crate::device_read::off_ui_thread();
        os_actions::refresh_firmware_metadata()
            .and_then(|()| os_actions::get_firmware_updates())
            .and_then(|updates| {
                os_actions::get_firmware_history().map(|history| (updates, history))
            })
    }
}

type Clock = Arc<dyn Fn() -> String + Send + Sync>;
type DetailsProvider = Arc<dyn Fn() -> UpdateDetails + Send + Sync>;
pub type CodecRunner = Arc<dyn Fn(CodecAction) -> Result<(), String> + Send + Sync>;
pub type CodecStatusProvider = Arc<dyn Fn() -> CodecPackStatus + Send + Sync>;
pub type WindowsSupportRunner =
    Arc<dyn Fn(WindowsSupportAction) -> Result<(), String> + Send + Sync>;
pub type WindowsSupportProvider = Arc<dyn Fn() -> WindowsReadiness + Send + Sync>;

fn codecs_card_title() -> String {
    t("upd-codecs")
}

fn codecs_explanation() -> String {
    t("upd-codecs-explain")
}

pub(super) fn windows_card_title() -> String {
    t("upd-windows")
}

pub(super) fn windows_explanation() -> String {
    t("upd-windows-explain")
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

type FirmwareRefreshEvent = Result<
    (
        Vec<os_actions::FirmwareUpdate>,
        Vec<os_actions::FirmwareHistoryEntry>,
    ),
    String,
>;
type FirmwareReading = (
    Result<Vec<os_actions::FirmwareUpdate>, String>,
    Result<Vec<os_actions::FirmwareHistoryEntry>, String>,
);
type FirmwareReader = Arc<dyn Fn() -> FirmwareReading + Send + Sync>;

const WORKER_STOPPED: &str = "the update worker stopped before it finished";

/// Where the model reads the device and acts on it. Each reaches the
/// system, so the model calls them only off the UI thread.
#[derive(Clone)]
struct DeviceReaders {
    details: DetailsProvider,
    control: Arc<dyn UpdateControl>,
    firmware: FirmwareReader,
    codec_status: CodecStatusProvider,
    windows_support: WindowsSupportProvider,
}

impl DeviceReaders {
    fn system() -> Self {
        Self {
            details: Arc::new(UpdateDetails::from_system),
            control: Arc::new(RealUpdateControl),
            firmware: Arc::new(|| -> FirmwareReading {
                crate::device_read::off_ui_thread();
                (
                    os_actions::get_firmware_updates(),
                    os_actions::get_firmware_history(),
                )
            }),
            codec_status: Arc::new(CodecPackStatus::read),
            windows_support: Arc::new(WindowsReadiness::read),
        }
    }

    /// What the pane shows of the device beside the session's reading, read once: tens of
    /// seconds when systemd or fwupd are slow, so never on the UI thread.
    fn read(&self) -> FirstRead {
        let timer_enabled = self.control.timer_enabled();
        let details = (self.details)();
        let (firmware_updates, firmware_history) = (self.firmware)();
        FirstRead {
            os_release: os_release_name(),
            timer_enabled,
            details,
            schedule: self.control.schedule(),
            allow_metered: self.control.allow_metered(),
            firmware_updates,
            firmware_history,
            codec_status: (self.codec_status)(),
            windows_support: (self.windows_support)(),
        }
    }
}

struct FirstRead {
    os_release: Option<String>,
    timer_enabled: Option<bool>,
    details: UpdateDetails,
    schedule: Option<String>,
    allow_metered: Option<bool>,
    firmware_updates: Result<Vec<os_actions::FirmwareUpdate>, String>,
    firmware_history: Result<Vec<os_actions::FirmwareHistoryEntry>, String>,
    codec_status: CodecPackStatus,
    windows_support: WindowsReadiness,
}

impl FirstRead {
    fn unread(error: &str) -> Self {
        Self {
            os_release: None,
            timer_enabled: None,
            details: UpdateDetails::default(),
            schedule: None,
            allow_metered: None,
            firmware_updates: Err(error.to_string()),
            firmware_history: Err(error.to_string()),
            codec_status: CodecPackStatus::default(),
            windows_support: WindowsReadiness::NotOffered,
        }
    }
}

type Apply = Box<dyn FnOnce(&mut UpdatesModel) + Send>;

struct Job {
    operation: &'static str,
    landing: Landing,
}

fn now_stamp() -> String {
    ui::format::timestamp(&ui::clock::local_now())
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent internal state flag on private pane \
              state, not a call-site boolean-blindness hazard: none are \
              constructor parameters"
)]
pub struct UpdatesModel {
    /// The session's `system-update` resource, as it publishes it.
    feed: Option<Arc<dyn UpdateFeed>>,
    /// Its newest reading: `None` until the first arrives, why not where the session has none.
    update: Option<Result<Update, String>>,
    actions: Box<dyn UpdateActions>,
    /// The actions asked here the session has not answered yet.
    asked: Vec<&'static str>,
    /// The action asked here last, and whether the reading showed it running: once it no
    /// longer runs, the reading's problem is its outcome.
    watching: Option<(&'static str, bool)>,
    /// What the system calls itself, for a member who may not see its deployments.
    os_release: Option<String>,
    details: UpdateDetails,
    prefs: UpdatePrefs,
    last_prefs: UpdatePrefs,
    clock: Clock,
    control: Arc<dyn UpdateControl>,
    firmware_reader: FirmwareReader,
    first_read: Option<Landing>,
    waiting: VecDeque<(UpdatesEffect, Option<EffectReply>)>,
    jobs: Vec<Job>,
    /// The update timer's actual state, as read from the machine. `None` means
    /// it could not be read, and the switch says so instead of guessing.
    timer_enabled: Option<bool>,
    schedule: Option<String>,
    allow_metered: Option<bool>,
    firmware_updates: Result<Vec<os_actions::FirmwareUpdate>, String>,
    firmware_history: Result<Vec<os_actions::FirmwareHistoryEntry>, String>,
    firmware_state: FirmwareUiState,
    firmware_events: Option<std::sync::mpsc::Receiver<os_actions::FirmwareEvent>>,
    firmware_refresh_events: Option<Landing>,
    firmware_refresh_pending: bool,
    codec_status: CodecPackStatus,
    codec_status_provider: CodecStatusProvider,
    codec_runner: CodecRunner,
    pub(super) windows_support: WindowsReadiness,
    windows_support_provider: WindowsSupportProvider,
    windows_support_runner: WindowsSupportRunner,
    pub(super) windows_pending: Option<WindowsSupportAction>,
    pub(super) windows_events: Option<Landing>,
    codec_pending: Option<CodecAction>,
    codec_events: Option<Landing>,
    /// Raised by the status-file watcher; the next sync re-reads the status,
    /// so progress written by the helper appears while it runs.
    codec_status_changed: Arc<AtomicBool>,
    codec_status_watch: Option<tilo_files::native::ChangeWatch>,
    wake: WakeHandle,
    rollback_armed: bool,
    notice: Option<String>,
    sections: crate::sections::Sections,
    replies: crate::replies::Replies,
    changed: bool,
}

impl UpdatesModel {
    pub fn new(storage: &ui::storage::AppStorage, sections: crate::sections::Sections) -> Self {
        let mut model = Self::reading(storage, sections, DeviceReaders::system());
        model.actions = Box::new(SessionActions::new(ui::RemoteActions::current()));
        match ui::collaboration::SessionResource::follow(SYSTEM_UPDATE, model.wake.clone()) {
            Ok(feed) => model.feed = Some(Arc::new(feed)),
            Err(error) => {
                warn!(target: "tilo::settings", %error, "the system update is not followed");
                model.update = Some(Err(error));
            }
        }
        model.codec_runner = Arc::new(crate::codec_pack::run);
        model.windows_support_runner = Arc::new(crate::windows_compat::run);
        model.watch_codec_status();
        model
    }

    fn reading(
        storage: &ui::storage::AppStorage,
        sections: crate::sections::Sections,
        readers: DeviceReaders,
    ) -> Self {
        let prefs = storage.settings::<UpdatePrefs>().get();
        let mut model = Self::assemble(sections, readers.clone(), Arc::new(now_stamp), prefs);
        model.first_read = Some(Landing::start(move || readers.read()));
        model
    }

    fn assemble(
        sections: crate::sections::Sections,
        readers: DeviceReaders,
        clock: Clock,
        prefs: UpdatePrefs,
    ) -> Self {
        Self {
            feed: None,
            update: None,
            actions: Box::new(NoActions::default()),
            asked: Vec::new(),
            watching: None,
            os_release: None,
            details: UpdateDetails::default(),
            prefs: prefs.clone(),
            last_prefs: prefs,
            clock,
            control: readers.control,
            firmware_reader: readers.firmware,
            first_read: None,
            waiting: VecDeque::new(),
            jobs: Vec::new(),
            timer_enabled: None,
            schedule: None,
            allow_metered: None,
            firmware_updates: Ok(Vec::new()),
            firmware_history: Ok(Vec::new()),
            firmware_state: FirmwareUiState::Idle,
            firmware_events: None,
            firmware_refresh_events: None,
            firmware_refresh_pending: false,
            codec_status: CodecPackStatus::default(),
            codec_status_provider: readers.codec_status,
            windows_support: WindowsReadiness::NotOffered,
            windows_support_provider: readers.windows_support,
            windows_pending: None,
            windows_events: None,
            codec_runner: Arc::new(|_| Err("the codec pack is unavailable in this test".into())),
            windows_support_runner: Arc::new(|_| {
                Err("Windows support is unavailable in this test".into())
            }),
            codec_pending: None,
            codec_events: None,
            codec_status_changed: Arc::new(AtomicBool::new(false)),
            codec_status_watch: None,
            wake: WakeHandle::new(),
            rollback_armed: false,
            notice: None,
            sections,
            replies: crate::replies::Replies::default(),
            changed: false,
        }
    }

    /// Take in the device's first read. The timer is the setting: the
    /// stored preference exists only so an existing file need not be
    /// migrated, and is overwritten by what the machine reports.
    fn adopt(&mut self, read: FirstRead) {
        self.os_release = read.os_release;
        self.timer_enabled = read.timer_enabled;
        if let Some(on) = read.timer_enabled {
            self.prefs.auto = on;
        }
        self.last_prefs = self.prefs.clone();
        self.details = read.details;
        self.schedule = read.schedule;
        self.allow_metered = read.allow_metered;
        if let Err(error) = &read.firmware_updates {
            warn!(target: "tilo::settings", %error, "the firmware updates are not listed");
        }
        self.firmware_updates = read.firmware_updates;
        self.firmware_history = read.firmware_history;
        self.codec_status = read.codec_status;
        self.windows_support = read.windows_support;
        self.changed = true;
    }

    /// A pane that follows `feed`, asks `actions` and drives `control`, read at once: what a
    /// test and a capture show.
    pub fn with_parts(
        sections: crate::sections::Sections,
        prefs: UpdatePrefs,
        feed: Arc<dyn UpdateFeed>,
        actions: Box<dyn UpdateActions>,
        clock: Clock,
        control: Arc<dyn UpdateControl>,
    ) -> Self {
        let read = FirstRead {
            os_release: Some("Tilo".into()),
            timer_enabled: control.timer_enabled(),
            details: UpdateDetails::default(),
            schedule: control.schedule(),
            allow_metered: control.allow_metered(),
            firmware_updates: Err("fwupd is unavailable in this test".into()),
            firmware_history: Err("fwupd is unavailable in this test".into()),
            codec_status: CodecPackStatus::default(),
            windows_support: WindowsReadiness::NotOffered,
        };
        let readers = DeviceReaders {
            details: Arc::new(UpdateDetails::default),
            control,
            firmware: Arc::new(|| -> FirmwareReading {
                crate::device_read::off_ui_thread();
                let unavailable = String::from("fwupd is unavailable in this test");
                (Err(unavailable.clone()), Err(unavailable))
            }),
            codec_status: Arc::new(CodecPackStatus::default),
            windows_support: Arc::new(|| WindowsReadiness::NotOffered),
        };
        let mut model = Self::assemble(sections, readers, clock, prefs);
        model.adopt(read);
        model.feed = Some(feed);
        model.actions = actions;
        model.drain_update();
        model.changed = false;
        model
    }

    /// What a capture shows: an update staged that the computer restarts to finish.
    pub fn sample(sections: crate::sections::Sections) -> Self {
        let deployment = |version: &str, built: &str| Deployment {
            image: Some("registry.tilo.so/tilo/tilo:stable".into()),
            version: Some(version.into()),
            built: Some(built.into()),
            ..Deployment::default()
        };
        let update = Update {
            booted: Some(deployment("Tilo 2026.06", "2026-06-02T09:00:00Z")),
            staged: Some(deployment("Tilo 2026.07", "2026-07-01T09:00:00Z")),
            rollback: Some(deployment("Tilo 2026.05", "2026-05-04T09:00:00Z")),
            restart_needed: true,
            ..Update::default()
        };
        Self::with_parts(
            sections,
            UpdatePrefs::default(),
            HeldReading::of(&update),
            Box::new(NoActions::default()),
            Arc::new(|| "Never".to_string()),
            Arc::new(UnavailableControl),
        )
    }

    #[must_use]
    pub fn with_codec_pack(mut self, status: CodecStatusProvider, runner: CodecRunner) -> Self {
        self.codec_status = status();
        self.codec_status_provider = status;
        self.codec_runner = runner;
        self
    }

    /// Show `readiness` as a capture's scene staged it, without reading the device.
    #[must_use]
    pub fn with_windows_support(mut self, readiness: WindowsReadiness) -> Self {
        self.windows_support = readiness;
        self
    }

    /// Follow the helper's status file, so the card shows its progress and
    /// background results (an install that finished once the device came
    /// online) while the pane is open. Stops when the pane is dropped.
    fn watch_codec_status(&mut self) {
        let changed = Arc::clone(&self.codec_status_changed);
        let wake = self.wake.clone();
        match crate::codec_pack::watch(move || {
            changed.store(true, Ordering::Release);
            wake.wake();
        }) {
            Ok(watch) => self.codec_status_watch = Some(watch),
            Err(error) => tracing::debug!(%error, "codec pack status is not watched"),
        }
    }

    #[cfg(test)]
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

    #[cfg(test)]
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

    fn codecs_card(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let mut buttons = Row::new()
            .no_wrap()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_align_items(ui::taffy::AlignItems::Center);
        let offered = self.codec_buttons();
        let any = !offered.is_empty();
        for (label, control_id, effect) in offered {
            let primary = matches!(&effect, UpdatesEffect::InstallCodecs);
            let button =
                button(cx, control_id, &label, effect).control_size(ui::widget::ControlSize::Small);
            buttons = buttons.push(Box::new(if primary {
                button.role(ui::widget::ButtonRole::Primary)
            } else {
                button
            }));
        }
        let mut body = vec![
            note(&codecs_explanation()),
            row(Box::new(thing_row(
                "settings.updates.codecs.status",
                &t("upd-status"),
                &[self.codec_status_line()],
                any.then(|| Box::new(buttons) as El),
            ))),
        ];
        if let Some(fraction) = self.codec_progress() {
            body.push(block(Box::new(
                ProgressBar::new(fraction).with_label(t("upd-installing-codecs")),
            )));
        }
        if let Some(problem) = self.codec_refresh_problem_shown() {
            body.push(note(&problem));
        }
        if let Some(applies) = self.codec_applies_shown() {
            body.push(note(&applies));
        }
        section(&codecs_card_title(), rows(body))
    }

    pub fn windows_support(&self) -> &WindowsReadiness {
        &self.windows_support
    }

    fn change_codecs(&mut self, action: CodecAction) {
        let operation = Self::codec_operation(action);
        if self.codec_pending.is_some() {
            self.operation_failed(operation, "the codec pack is already being changed");
            self.changed = true;
            return;
        }
        let runner = Arc::clone(&self.codec_runner);
        self.notice = None;
        self.codec_events = Some(Landing::start(move || runner(action)));
        self.codec_pending = Some(action);
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
        let result = match events.poll::<Result<(), String>>() {
            Landed::Done(result) => result,
            Landed::Pending => return,
            Landed::Lost => Err("codec pack worker stopped".into()),
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
                self.failed(
                    operation,
                    t_args("upd-notice-codecs", &[("error", error.as_str().into())]),
                );
            }
        }
        self.changed = true;
    }

    #[cfg(test)]
    fn wait_for_codec_operation(&mut self) {
        while self.codec_events.is_some() {
            self.drain_codec_events();
            std::thread::yield_now();
        }
    }

    pub fn timer_enabled(&self) -> Option<bool> {
        self.timer_enabled
    }

    pub fn allow_metered(&self) -> Option<bool> {
        self.allow_metered
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// The session's newest reading of the system update, once one arrived.
    pub fn update(&self) -> Option<&Update> {
        self.update.as_ref()?.as_ref().ok()
    }

    /// What the page says of the system update.
    pub fn update_state(&self) -> UpdateState {
        UpdateState::of(self.update.as_ref(), &self.asked)
    }

    /// What this machine can go back to, as the reading names it, or why that is not known.
    fn rollback_target(&self) -> Result<Option<String>, String> {
        match &self.update {
            None => Ok(None),
            Some(Err(_)) => Err(t("upd-unreachable")),
            Some(Ok(update)) if update.booted.is_none() => Err(t("upd-admins-only")),
            Some(Ok(update)) => Ok(update
                .rollback
                .as_ref()
                .map(|rollback| deployment_shown(rollback).unwrap_or_else(|| t("upd-previous")))),
        }
    }

    /// What the computer runs: the booted deployment, else the name the system gives itself.
    /// What runs: the staged update once the live update applied it in full, else what started.
    fn current_version(&self) -> String {
        self.update()
            .and_then(|update| {
                let applied = update.staged.as_ref().filter(|_| update.applied);
                applied
                    .and_then(deployment_shown)
                    .or_else(|| update.booted.as_ref().and_then(deployment_shown))
            })
            .or_else(|| self.os_release.clone())
            .unwrap_or_else(|| t("upd-not-reported"))
    }

    /// The rollback card's title and lines, in order.
    pub fn rollback_card_lines(&self) -> Vec<String> {
        let mut out = vec![rollback_card_title()];
        match (self.rollback_target(), self.rollback_armed) {
            (Err(error), _) => out.push(error),
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

    pub fn help_lines(&self) -> Vec<String> {
        vec![t("upd-help-1"), t("upd-help-2")]
    }

    pub fn prefs(&self) -> &UpdatePrefs {
        &self.prefs
    }

    fn page(&self, cx: &ViewCx<Settings>) -> Element {
        if self.first_read.is_some() {
            return provider_settings_ui::widgets::page(vec![section(
                &t("upd-software"),
                rows(vec![note(&t("upd-reading"))]),
            )]);
        }
        let mut all: Vec<SettingsSection> = vec![
            self.status_card(cx),
            self.firmware_card(cx),
            self.codecs_card(cx),
            self.windows_card(cx),
            self.detail_card(cx),
            self.lifecycle_card(),
        ];
        // What the machine goes back to is the session's to say: until its reading arrives,
        // "nothing to go back to" would be a guess.
        if self.update.is_some() {
            all.push(self.rollback_card(cx));
        }
        all.push(self.help_card());
        // Firmware history is useful only when fwupd answered and there is a
        // transaction to describe. If the service is unavailable, the single
        // firmware recovery card above owns that explanation and retry.
        if matches!(&self.firmware_history, Ok(history) if !history.is_empty()) {
            all.insert(2, self.firmware_history_card());
        }
        if let Some(msg) = &self.notice {
            // In front of everything, because it is about the thing the person
            // just tried to do. A failed switch that only warns into the
            // journal is a switch that failed silently.
            all.insert(
                0,
                section(
                    &t("upd-notice-title"),
                    rows(vec![row(Box::new(
                        Text::new(msg).with_role(ui::TextRole::Body),
                    ))]),
                ),
            );
        }
        provider_settings_ui::widgets::page(all)
    }

    /// The system update: one state, said plainly, with the action that fits it.
    fn status_card(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let restart = |id: &str, label: &str, effect| {
            Box::new(button(cx, id, label, effect).role(ui::widget::ButtonRole::Primary)) as El
        };
        let check = || {
            Some(Box::new(
                button(
                    cx,
                    "settings.updates.check",
                    &t("upd-check"),
                    UpdatesEffect::CheckForUpdates,
                )
                .control_size(ui::widget::ControlSize::Small),
            ) as El)
        };
        let working = || Some(Box::new(ui::widget::Spinner::new()) as El);
        let status = |title: &str, details: &[String], trailing: Option<El>| {
            row(Box::new(thing_row(
                "settings.updates.software.status",
                title,
                details,
                trailing,
            )))
        };
        let staged = self
            .update()
            .and_then(|update| update.staged.as_ref())
            .and_then(Deployment::name);
        let body = match self.update_state() {
            UpdateState::Reading => note(&t("upd-reading")),
            UpdateState::Unreachable => status(&t("upd-unreachable"), &[], None),
            UpdateState::Checking => status(&t("upd-checking"), &[], working()),
            UpdateState::Finishing => status(&t("upd-finishing"), &[], working()),
            UpdateState::Failed => setting_row_described(
                &t("upd-live-failed-title"),
                &t("upd-live-failed"),
                restart(
                    "settings.updates.restart",
                    &t("upd-restart-update"),
                    UpdatesEffect::RestartToUpdate,
                ),
            ),
            UpdateState::DesktopRestart => setting_row_described(
                &t("upd-live-ready"),
                &t("upd-finishes-now"),
                restart(
                    "settings.updates.restart-desktop",
                    &t("upd-finish-now"),
                    UpdatesEffect::RestartDesktop,
                ),
            ),
            UpdateState::Restart => setting_row_described(
                &match staged {
                    Some(version) => t_args("upd-ready", &[("version", version.into())]),
                    None => t("upd-ready-unnamed"),
                },
                &t("upd-restart-apply"),
                restart(
                    "settings.updates.restart",
                    &t("upd-restart-update"),
                    UpdatesEffect::RestartToUpdate,
                ),
            ),
            UpdateState::Done => status(&t("upd-up-to-date"), &[t("upd-done")], check()),
            UpdateState::UpToDate => status(&t("upd-up-to-date"), &[], check()),
        };
        section(&t("upd-software"), rows(vec![body]))
    }

    fn detail_card(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let automatic = t("upd-automatic");
        let unavailable = t("upd-unavailable");
        let schedule_label = t("upd-schedule");
        let schedule_placeholder = self.schedule.as_deref().map(schedule_shown);
        section(
            &t("upd-details"),
            rows(vec![
                value_row(&t("upd-current-version"), &self.current_version()),
                value_row(&t("upd-last-checked"), &self.prefs.last_checked),
                match self.timer_enabled {
                    Some(_) => cx.setting(automatic).id("updates.auto").toggle_effect(
                        &LocalState::new(self.prefs.auto),
                        &cx.actions(),
                        effect(UpdatesEffect::SET_AUTOMATIC_UPDATES),
                    ),
                    // Not a switch stuck in one position: a switch that cannot
                    // reach the timer would report "off" for a machine that is
                    // updating itself, which is worse than saying so.
                    None => value_row(&automatic, &unavailable),
                },
                match &schedule_placeholder {
                    Some(schedule) => cx.setting(schedule_label).choice_effect(
                        UPDATE_SCHEDULES.iter().map(|id| schedule_shown(id)),
                        &LocalState::new(schedule.clone()),
                        &cx.actions(),
                        effect(UpdatesEffect::SET_UPDATE_SCHEDULE)
                            .map(|shown: String| schedule_from_label(&shown)),
                    ),
                    None => value_row(&schedule_label, &unavailable),
                },
                match self.allow_metered {
                    Some(_) => cx
                        .setting(t("upd-metered"))
                        .id("updates.metered")
                        .toggle_effect(
                            &LocalState::new(self.allow_metered.unwrap_or(false)),
                            &cx.actions(),
                            effect(UpdatesEffect::SET_METERED_UPDATES),
                        ),
                    None => value_row(&t("upd-metered-short"), &t("upd-metered-unavailable")),
                },
            ]),
        )
    }

    fn lifecycle_card(&self) -> SettingsSection {
        let target = self.rollback_target().ok().flatten();
        let mut lifecycle_rows = vec![value_row(
            &t("upd-rollback-health"),
            &rollback_health(self.details.protected, target.as_deref()),
        )];
        if self.details.history.is_empty() {
            lifecycle_rows.push(note(&t("upd-no-bootc-history")));
        } else {
            lifecycle_rows.extend(self.details.history.iter().map(|line| note(line)));
        }
        section(&t("upd-lifecycle"), rows(lifecycle_rows))
    }

    fn firmware_card(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let body = match &self.firmware_updates {
            Err(_) => self.firmware_card_error(cx),
            Ok(updates) if updates.is_empty() => self.firmware_card_up_to_date(cx),
            Ok(updates) => self.firmware_card_available(cx, updates),
        };
        section(&t("upd-firmware"), body)
    }

    /// fwupd did not answer. What it said is for the log: a person reads that firmware cannot
    /// be checked here, and can ask again.
    fn firmware_card_error(&self, cx: &ViewCx<Settings>) -> Body {
        let refresh_label = if self.firmware_refresh_pending {
            t("upd-refreshing")
        } else {
            t("upd-try-again")
        };
        rows(vec![status_row(
            cx,
            &t("upd-fw-query-failed"),
            "settings.updates.firmware.refresh",
            &refresh_label,
            UpdatesEffect::RefreshFirmware,
        )])
    }

    fn firmware_card_up_to_date(&self, cx: &ViewCx<Settings>) -> Body {
        if matches!(
            self.firmware_state,
            FirmwareUiState::Complete { needs_reboot: true }
        ) {
            return rows(vec![
                value_row(&t("upd-fw-status"), &t("upd-fw-current-status")),
                status_row(
                    cx,
                    &t("upd-restart-required"),
                    "settings.updates.firmware.restart",
                    &t("upd-restart-now"),
                    UpdatesEffect::RestartToUpdate,
                ),
            ]);
        }
        let check_fw = if self.firmware_refresh_pending {
            t("upd-checking-firmware")
        } else {
            t("upd-check-firmware")
        };
        rows(vec![status_row(
            cx,
            &t("upd-fw-current-status"),
            "settings.updates.firmware.refresh",
            &check_fw,
            UpdatesEffect::RefreshFirmware,
        )])
    }

    fn firmware_card_available(
        &self,
        cx: &ViewCx<Settings>,
        updates: &[os_actions::FirmwareUpdate],
    ) -> Body {
        let mut items: Vec<Item> = updates
            .iter()
            .map(|update| {
                let version = match (&update.current_version, &update.new_version) {
                    (Some(from), Some(to)) => format!("{from} → {to}"),
                    (_, Some(to)) => t_args("upd-update-to", &[("version", to.as_str().into())]),
                    _ => t("upd-update-available"),
                };
                let mut details = Vec::new();
                if let Some(description) = &update.description {
                    details.push(description.clone());
                }
                if !update.problems.is_empty() {
                    details.push(t_args(
                        "upd-before-installing",
                        &[("problems", update.problems.join(", ").into())],
                    ));
                }
                if details.is_empty() {
                    value_row(&update.device, &version)
                } else {
                    setting_row_described(
                        &update.device,
                        &details.join("\n"),
                        Box::new(Text::new(&version).numeric().dimmed()),
                    )
                }
            })
            .collect();
        match &self.firmware_state {
            FirmwareUiState::Idle => items.push(button_row(
                cx,
                "settings.updates.firmware.install",
                &t("upd-install-firmware"),
                UpdatesEffect::InstallFirmware,
            )),
            FirmwareUiState::Installing { percent, message } => {
                items.push(row(Box::new(ProgressBar::new(f32::from(*percent) / 100.0))));
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
                    items.push(button_row(
                        cx,
                        "settings.updates.firmware.restart",
                        &t("upd-restart-now"),
                        UpdatesEffect::RestartToUpdate,
                    ));
                }
            }
        }
        rows(items)
    }

    fn firmware_history_card(&self) -> SettingsSection {
        match &self.firmware_history {
            Ok(history) if history.is_empty() => section(
                &t("upd-fw-history"),
                rows(vec![note(&t("upd-fw-history-empty"))]),
            ),
            Ok(history) => {
                let mut entries = Vec::new();
                for item in history.iter().take(8) {
                    let versions = match (&item.from_version, &item.to_version) {
                        (Some(from), Some(to)) => format!("{from} → {to}\n{}", item.state),
                        (_, Some(to)) => format!("{to}\n{}", item.state),
                        _ => item.state.clone(),
                    };
                    entries.push(match &item.error {
                        Some(error) => setting_row_described(
                            &item.device,
                            &t_args("upd-fwupd-error", &[("error", error.as_str().into())]),
                            Box::new(Text::new(&versions).numeric().dimmed()),
                        ),
                        None => value_row(&item.device, &versions),
                    });
                }
                section(&t("upd-fw-history"), rows(entries))
            }
            Err(_) => section(
                &t("upd-fw-history"),
                rows(vec![value_row(
                    &t("upd-fw-status"),
                    &t("upd-history-query-failed"),
                )]),
            ),
        }
    }

    /// The rollback card. Present whether or not there is anything to go
    /// back to, because "this machine has nothing to roll back to" is
    fn rollback_card(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let lines = self.rollback_card_lines();
        let rollback_body = match (
            self.rollback_target(),
            self.rollback_armed,
            lines.as_slice(),
        ) {
            (Err(_), _, [_, error, ..]) => vec![row(error_caption(error))],
            (Ok(None), _, [_, no_previous, no_previous_caption, ..]) => vec![
                row(Box::new(
                    Text::new(no_previous).with_role(ui::TextRole::Body),
                )),
                note(no_previous_caption),
            ],
            (Ok(Some(_)), false, [_, previous, intro, button_label, ..]) => {
                vec![setting_row_described(
                    previous,
                    intro,
                    Box::new(button(
                        cx,
                        "settings.updates.rollback",
                        button_label,
                        UpdatesEffect::ReviewRollback,
                    )),
                )]
            }
            // Armed: what it does not undo, in front of the button that does it.
            (Ok(Some(_)), true, [_, prompt, caveats @ .., confirm_label]) => {
                vec![setting_row_described(
                    prompt,
                    &caveats.join("\n"),
                    Box::new(button(
                        cx,
                        "settings.updates.rollback.confirm",
                        confirm_label,
                        UpdatesEffect::RollBack,
                    )),
                )]
            }
            // `rollback_card_lines` always returns a shape matching one of
            // the arms above for the same `(rollback_target,
            // rollback_armed)` state it was built from; render nothing
            // sooner than panic if that invariant is ever broken.
            _ => Vec::new(),
        };
        section(&rollback_card_title(), rows(rollback_body))
    }

    fn help_card(&self) -> SettingsSection {
        section(
            &t("upd-help-title"),
            rows(self.help_lines().iter().map(|l| note(l)).collect()),
        )
    }

    fn record(&self, record: impl Into<ui::tilo_resource::Invoke<UpdatePrefs>>) {
        if let Err(e) = self.sections.invoke::<UpdatePrefs>(record) {
            warn!(target: "tilo::settings", op = "update_prefs_save", error = %e, "failed to keep update prefs");
        }
    }

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
                                &[("total", i64::try_from(total).unwrap_or(i64::MAX).into())],
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
                        self.reread_firmware();
                        self.operation_succeeded(
                            UpdatesEffect::INSTALL_FIRMWARE.id,
                            "firmware installation finished",
                        );
                        finished = true;
                    }
                    os_actions::FirmwareEvent::Failed(error) => {
                        self.firmware_state = FirmwareUiState::Idle;
                        self.failed(
                            UpdatesEffect::INSTALL_FIRMWARE.id,
                            t_args("upd-notice-fw-failed", &[("error", error.as_str().into())]),
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

    /// Take in the session's newest reading of the system update, where one arrived.
    fn drain_update(&mut self) {
        let Some(reading) = self.feed.as_ref().and_then(|feed| feed.latest()) else {
            return;
        };
        let update = reading.and_then(|json| {
            serde_json::from_str::<Update>(&json).map_err(|error| error.to_string())
        });
        if let Err(error) = &update {
            warn!(target: "tilo::settings", %error, "the system update does not read");
        }
        let was_checking = self.update().is_some_and(checking);
        self.update = Some(update);
        let now = self.update().cloned();
        if let Some(now) = &now {
            // A check that ended well is the last check, whoever asked for it.
            if was_checking && !checking(now) && now.problem.is_none() {
                self.stamp_check();
            }
            self.follow_asked(now);
        }
        self.changed = true;
    }

    /// Follow the action asked here last through the reading: it runs, then it ends, and the
    /// reading's problem is how.
    fn follow_asked(&mut self, update: &Update) {
        let Some((action, seen)) = self.watching else {
            return;
        };
        if update.pending.iter().any(|pending| pending == action) {
            self.watching = Some((action, true));
            return;
        }
        if !seen {
            return;
        }
        self.watching = None;
        if let Some(problem) = &update.problem {
            warn!(target: "tilo::settings", action, %problem, "the system update did not do what was asked");
            self.notice = Some(did_not_finish(action));
        }
    }

    fn stamp_check(&mut self) {
        self.prefs.last_checked = (self.clock)();
        self.last_prefs.last_checked = self.prefs.last_checked.clone();
        self.record(UpdatePrefs::RECORD_CHECK.with(self.prefs.last_checked.clone()));
    }

    /// Take the session's answers to what was asked here.
    fn drain_answers(&mut self) {
        for (action, answer) in self.actions.answered() {
            self.asked.retain(|asked| *asked != action);
            let operation = operation_of(action);
            match answer {
                Ok(()) => self.operation_succeeded(operation, "the session took it"),
                Err(error) => {
                    warn!(target: "tilo::settings", action, %error, "the session refused the action");
                    self.watching = None;
                    self.failed(operation, did_not_finish(action));
                }
            }
            self.changed = true;
        }
    }

    /// Ask the session for `effect`, an action of its resource `resource`, with the person's
    /// confirmation when it takes one.
    fn ask(&mut self, resource: &str, effect: &impl ui::tilo_resource::Effect, confirmed: bool) {
        let action = effect.id();
        if self.asked.contains(&action) {
            self.operation_failed(operation_of(action), "the session has it already");
            return;
        }
        self.notice = None;
        self.actions
            .invoke(action, effect.address_on(resource), confirmed);
        self.asked.push(action);
        self.watching = Some((action, false));
        self.changed = true;
    }

    /// Restart to finish the update, or for the firmware that waits for a restart: the update's
    /// own restart where the reading says one is needed, the computer's otherwise.
    fn restart(&mut self) {
        if self.update().is_some_and(|update| update.restart_needed) {
            self.ask(SYSTEM_UPDATE, &UpdateAction::RestartToUpdate, true);
        } else {
            self.ask(POWER, &PowerAction::Restart, true);
        }
    }

    fn drain_firmware_refresh_events(&mut self) {
        let Some(events) = self.firmware_refresh_events.as_ref() else {
            return;
        };
        let event = match events.poll::<FirmwareRefreshEvent>() {
            Landed::Done(event) => event,
            Landed::Pending => return,
            Landed::Lost => {
                self.firmware_refresh_events = None;
                self.firmware_refresh_pending = false;
                let error = WORKER_STOPPED.to_owned();
                warn!(target: "tilo::settings", %error, "the firmware updates are not listed");
                self.firmware_updates = Err(error.clone());
                self.firmware_history = Err(error);
                self.failed(
                    UpdatesEffect::REFRESH_FIRMWARE.id,
                    t("upd-notice-fw-refresh"),
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
                warn!(target: "tilo::settings", %error, "the firmware updates are not listed");
                self.firmware_updates = Err(error.clone());
                self.firmware_history = Err(error);
                self.failed(
                    UpdatesEffect::REFRESH_FIRMWARE.id,
                    t("upd-notice-fw-refresh"),
                );
            }
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
        self.firmware_refresh_events = Some(Landing::start(move || -> FirmwareRefreshEvent {
            control.refresh_firmware()
        }));
        self.firmware_refresh_pending = true;
        self.changed = true;
    }

    fn install_firmware(&mut self) {
        if self.firmware_refresh_pending || self.firmware_events.is_some() {
            self.failed(UpdatesEffect::INSTALL_FIRMWARE.id, t("upd-notice-fw-busy"));
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
                self.failed(
                    UpdatesEffect::INSTALL_FIRMWARE.id,
                    t_args("upd-notice-fw-install", &[("error", e.as_str().into())]),
                );
            }
        }
        self.changed = true;
    }

    fn start(&mut self, operation: &'static str, work: impl FnOnce() -> Apply + Send + 'static) {
        self.jobs.push(Job {
            operation,
            landing: Landing::start(work),
        });
    }

    fn drain_jobs(&mut self) {
        let mut index = 0;
        while let Some(job) = self.jobs.get(index) {
            match job.landing.poll::<Apply>() {
                Landed::Pending => index += 1,
                Landed::Done(apply) => {
                    self.jobs.remove(index);
                    apply(self);
                }
                Landed::Lost => {
                    let job = self.jobs.remove(index);
                    warn!(target: "tilo::settings", op = job.operation, "update worker stopped");
                    self.failed(job.operation, WORKER_STOPPED.to_string());
                    self.changed = true;
                }
            }
        }
    }

    fn reread_firmware(&mut self) {
        let reader = Arc::clone(&self.firmware_reader);
        self.start(UpdatesEffect::INSTALL_FIRMWARE.id, move || {
            let (updates, history) = reader();
            Box::new(move |model: &mut Self| {
                model.firmware_updates = updates;
                model.firmware_history = history;
                model.changed = true;
            })
        });
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
        let control = Arc::clone(&self.control);
        self.start(operation, move || {
            let set = control.set_timer(b);
            let timer = control.timer_enabled();
            Box::new(move |model: &mut Self| model.automatic_set(b, set, timer))
        });
    }

    fn automatic_set(&mut self, b: bool, set: Result<(), String>, timer: Option<bool>) {
        let operation = UpdatesEffect::SET_AUTOMATIC_UPDATES.id;
        self.timer_enabled = timer;
        match set {
            Ok(()) => {
                let effective = self.timer_enabled.unwrap_or(b);
                self.prefs.auto = effective;
                self.notice = (effective != b).then(|| {
                    format!(
                        "Automatic updates are still {}. The system did \
                         not accept the change.",
                        if effective { "on" } else { "off" }
                    )
                });
                self.record(UpdatePrefs::RECORD_AUTOMATIC.with(effective));
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
                let effective = self.timer_enabled.unwrap_or(self.prefs.auto);
                self.prefs.auto = effective;
                self.last_prefs.auto = effective;
                self.failed(
                    operation,
                    t_args("upd-notice-auto", &[("error", e.as_str().into())]),
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
        let control = Arc::clone(&self.control);
        self.start(operation, move || {
            let set = control.set_schedule(&requested);
            let schedule = set.is_ok().then(|| control.schedule()).flatten();
            Box::new(move |model: &mut Self| model.schedule_set(requested, set, schedule))
        });
    }

    fn schedule_set(
        &mut self,
        requested: String,
        set: Result<(), String>,
        schedule: Option<String>,
    ) {
        let operation = UpdatesEffect::SET_UPDATE_SCHEDULE.id;
        match set {
            Ok(()) => {
                self.schedule = Some(schedule.unwrap_or(requested));
                self.notice = None;
                self.operation_succeeded(operation, "update schedule changed");
            }
            Err(error) => {
                self.failed(
                    operation,
                    t_args("upd-notice-schedule", &[("error", error.as_str().into())]),
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
        let control = Arc::clone(&self.control);
        self.start(UpdatesEffect::SET_METERED_UPDATES.id, move || {
            let set = control.set_allow_metered(requested);
            let allowed = set.is_ok().then(|| control.allow_metered()).flatten();
            Box::new(move |model: &mut Self| model.metered_set(requested, set, allowed))
        });
    }

    fn metered_set(&mut self, requested: bool, set: Result<(), String>, allowed: Option<bool>) {
        match set {
            Ok(()) => {
                self.allow_metered = allowed.or(Some(requested));
                self.notice = None;
                self.operation_succeeded(
                    UpdatesEffect::SET_METERED_UPDATES.id,
                    "metered-network policy changed",
                );
            }
            Err(error) => {
                self.failed(
                    UpdatesEffect::SET_METERED_UPDATES.id,
                    t_args("upd-notice-metered", &[("error", error.as_str().into())]),
                );
            }
        }
        self.changed = true;
    }

    fn review_rollback(&mut self) {
        {
            self.rollback_armed = true;
            self.notice = None;
            self.operation_succeeded(UpdatesEffect::REVIEW_ROLLBACK.id, "rollback details opened");
            self.changed = true;
        }
    }

    fn roll_back(&mut self) {
        let control = Arc::clone(&self.control);
        self.start(UpdatesEffect::ROLL_BACK.id, move || {
            let result = control.roll_back();
            Box::new(move |model: &mut Self| model.rolled_back(result))
        });
    }

    fn rolled_back(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.rollback_armed = false;
                self.notice = Some(t("upd-notice-rollback-ok"));
                self.operation_succeeded(
                    UpdatesEffect::ROLL_BACK.id,
                    "previous system image selected for next boot",
                );
            }
            Err(e) => {
                warn!(target: "tilo::settings", op = "rollback", error = %e, "rollback failed");
                self.failed(
                    UpdatesEffect::ROLL_BACK.id,
                    t_args("upd-notice-rollback", &[("error", e.as_str().into())]),
                );
            }
        }
        self.changed = true;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum UpdatesEffect {
    /// Check for a new system image now.
    #[effect(kind = "outside")]
    #[effect(job, returns)]
    #[effect(label = "upd-check")]
    CheckForUpdates,
    /// Restart into the staged system image.
    #[effect(kind = "outside")]
    #[effect(job, destructive, returns)]
    RestartToUpdate,
    /// Finish an update that applied live: restart the desktop in place, with
    /// the apps the person has open kept.
    #[effect(kind = "outside")]
    #[effect(job, returns)]
    RestartDesktop,
    /// Show what rolling back to the previous system image does not undo.
    #[effect(kind = "outside")]
    #[effect(job, returns)]
    ReviewRollback,
    /// Boot the previous system image next time.
    #[effect(kind = "outside")]
    #[effect(job, destructive, returns)]
    RollBack,
    /// Ask the firmware service for new firmware.
    #[effect(kind = "outside")]
    #[effect(job, returns)]
    RefreshFirmware,
    /// Install the available firmware upgrades.
    #[effect(kind = "outside")]
    #[effect(job, destructive, returns)]
    InstallFirmware,
    /// Play more video and audio formats: download the media codecs to this device.
    #[effect(kind = "outside")]
    #[effect(job, returns)]
    InstallCodecs,
    /// Remove the downloaded media codecs.
    #[effect(kind = "outside")]
    #[effect(job, destructive, returns)]
    RemoveCodecs,
    /// Check again for hardware H.264/HEVC support.
    #[effect(kind = "outside")]
    #[effect(job, returns)]
    RefreshCodecs,
    /// Run Windows apps: download Windows support to this device.
    #[effect(kind = "outside")]
    #[effect(job, returns)]
    InstallWindowsSupport,
    /// Remove the downloaded Windows support.
    #[effect(kind = "outside")]
    #[effect(job, destructive, returns)]
    RemoveWindowsSupport,
    /// Update the system automatically, or only when asked.
    #[effect(kind = "outside")]
    #[effect(job, idempotent, returns)]
    SetAutomaticUpdates { on: bool },
    /// Set when updates download.
    #[effect(kind = "outside")]
    #[effect(job, idempotent, returns)]
    SetUpdateSchedule {
        #[param("When", choices = UPDATE_SCHEDULES.iter().copied())]
        schedule: String,
    },
    /// Download updates on metered networks, or wait for another network.
    #[effect(kind = "outside")]
    #[effect(job, idempotent, returns)]
    SetMeteredUpdates { allowed: bool },
}

impl UpdatesModel {
    /// Start `effect`; `reply` is told how it ends.
    ///
    /// Until the device's first read lands, and while the same request
    /// still runs, it waits and runs after: it acts on what the device
    /// reports, not on what the pane has yet to read.
    pub(crate) fn run(&mut self, effect: UpdatesEffect, reply: Option<EffectReply>) {
        let operation = effect.id();
        if self.first_read.is_some() || self.jobs.iter().any(|job| job.operation == operation) {
            self.waiting.push_back((effect, reply));
            return;
        }
        if let Some(reply) = reply {
            self.replies.wait(operation, reply);
        }
        match effect {
            UpdatesEffect::CheckForUpdates => {
                self.ask(SYSTEM_UPDATE, &UpdateAction::UpdateNow, false);
            }
            UpdatesEffect::RestartToUpdate => self.restart(),
            UpdatesEffect::RestartDesktop => {
                self.ask(SYSTEM_UPDATE, &UpdateAction::RestartDesktop, false);
            }
            UpdatesEffect::ReviewRollback => self.review_rollback(),
            UpdatesEffect::RollBack => self.roll_back(),
            UpdatesEffect::RefreshFirmware => self.refresh_firmware(),
            UpdatesEffect::InstallFirmware => self.install_firmware(),
            UpdatesEffect::InstallCodecs => self.change_codecs(CodecAction::Install),
            UpdatesEffect::RemoveCodecs => self.change_codecs(CodecAction::Remove),
            UpdatesEffect::RefreshCodecs => self.change_codecs(CodecAction::Refresh),
            UpdatesEffect::InstallWindowsSupport => {
                self.change_windows_support(WindowsSupportAction::Install);
            }
            UpdatesEffect::RemoveWindowsSupport => {
                self.change_windows_support(WindowsSupportAction::Remove);
            }
            UpdatesEffect::SetAutomaticUpdates { on } => self.set_automatic(on),
            UpdatesEffect::SetUpdateSchedule { schedule } => self.set_schedule(&schedule),
            UpdatesEffect::SetMeteredUpdates { allowed } => self.set_metered(allowed),
        }
    }

    fn drain_first_read(&mut self) {
        let Some(read) = self.first_read.as_ref() else {
            return;
        };
        let read = match read.poll::<FirstRead>() {
            Landed::Done(read) => read,
            Landed::Pending => return,
            Landed::Lost => FirstRead::unread(WORKER_STOPPED),
        };
        self.first_read = None;
        self.adopt(read);
    }

    fn run_waiting(&mut self) {
        if self.first_read.is_some() {
            return;
        }
        for (effect, reply) in std::mem::take(&mut self.waiting) {
            self.run(effect, reply);
        }
    }

    fn drain(&mut self) {
        self.drain_first_read();
        self.drain_update();
        self.drain_answers();
        self.drain_jobs();
        self.drain_firmware_refresh_events();
        self.drain_firmware_events();
        self.drain_codec_events();
        self.drain_windows_events();
        self.refresh_codec_status_if_changed();
        self.run_waiting();
    }

    #[cfg(test)]
    fn working(&self) -> bool {
        self.first_read.is_some() || !self.jobs.is_empty() || !self.waiting.is_empty()
    }

    fn operation_succeeded(&mut self, operation: &'static str, message: impl Into<String>) {
        self.replies.succeed(operation, message);
    }

    fn operation_failed(&mut self, operation: &'static str, message: impl Into<String>) {
        self.replies.fail(operation, message);
    }

    /// Show the person `notice` and tell the caller of `operation` the same
    /// sentence: one failure has one wording, so nothing can drift apart and
    /// a test that reads the notice reads what the caller was told.
    fn failed(&mut self, operation: &'static str, notice: String) {
        self.notice = Some(notice.clone());
        self.operation_failed(operation, notice);
    }
}

fn button_row(cx: &ViewCx<Settings>, control_id: &str, label: &str, effect: UpdatesEffect) -> Item {
    row(Box::new(
        Row::new().push(Box::new(button(cx, control_id, label, effect))),
    ))
}

fn status_row(
    cx: &ViewCx<Settings>,
    status: &str,
    control_id: &str,
    label: &str,
    effect: UpdatesEffect,
) -> Item {
    row(Box::new(thing_row(
        &format!("{control_id}.status"),
        status,
        &[],
        Some(Box::new(
            button(cx, control_id, label, effect).control_size(ui::widget::ControlSize::Small),
        )),
    )))
}

fn button(cx: &ViewCx<Settings>, control_id: &str, label: &str, effect: UpdatesEffect) -> Button {
    cx.actions().bind_effect_invocation(
        Button::new(control_id, label),
        &SettingsEffect::from(effect),
    )
}

pub struct UpdatesArea {
    storage: ui::storage::AppStorage,
    sections: crate::sections::Sections,
    devices: bool,
    codec_status: Option<CodecPackStatus>,
    windows_support: Option<WindowsReadiness>,
    model: std::cell::OnceCell<Rc<RefCell<UpdatesModel>>>,
    shown: LocalState<u64>,
}

impl UpdatesArea {
    pub fn new(cx: &AreaContext) -> Self {
        Self {
            storage: cx.storage.clone(),
            sections: cx.sections.clone(),
            devices: cx.devices,
            codec_status: cx.scene().and_then(|scene| scene.codec_status.clone()),
            windows_support: cx.scene().and_then(|scene| scene.windows_support.clone()),
            model: std::cell::OnceCell::new(),
            shown: LocalState::new(0),
        }
    }

    /// What this device says about Windows support, for the availability the window publishes.
    /// A capture shows its own state; a live window reads the small status files the extension
    /// helper keeps, so the offer follows an install another view finished without opening this
    /// pane or running a command.
    fn windows_readiness(&self) -> WindowsReadiness {
        if !self.devices {
            return self
                .windows_support
                .clone()
                .unwrap_or(WindowsReadiness::NotOffered);
        }
        WindowsReadiness::read()
    }

    /// Windows support is offered only in the states that can act: the install while it is not
    /// installed, the removal while it is. The rest of the window (and search, which publishes
    /// the same actions) reads the offer from this.
    pub(crate) fn statuses(&self) -> Vec<ui::framework::EffectStatus> {
        windows_card::support_statuses(&self.windows_readiness())
    }

    #[cfg(test)]
    pub(crate) fn set_windows_support(&mut self, readiness: WindowsReadiness) {
        self.windows_support = Some(readiness);
    }

    pub fn model(&self) -> &Rc<RefCell<UpdatesModel>> {
        self.model.get_or_init(|| {
            let model = if self.devices {
                UpdatesModel::new(&self.storage, self.sections.clone())
            } else {
                UpdatesModel::sample(self.sections.clone())
            };
            let model = if !self.devices
                && let Some(status) = self.codec_status.clone()
            {
                model.with_codec_pack(
                    Arc::new(move || status.clone()),
                    Arc::new(|_| Err("a capture cannot change installed formats".into())),
                )
            } else {
                model
            };
            let model = match (&self.devices, self.windows_support.clone()) {
                (false, Some(readiness)) => model.with_windows_support(readiness),
                _ => model,
            };
            Rc::new(RefCell::new(model))
        })
    }

    pub(crate) fn poll(&self) {
        let Some(model) = self.model.get() else {
            return;
        };
        let Ok(mut model) = model.try_borrow_mut() else {
            return;
        };
        if model.poll() {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    fn busy(&self) -> bool {
        self.model
            .get()
            .and_then(|model| model.try_borrow().ok())
            .is_some_and(|model| model.busy())
    }
}

pub(crate) fn answer(
    app: &crate::settings_app::SettingsApp,
    effect: UpdatesEffect,
    cx: &mut ui::framework::EffectCx<crate::resource::Settings>,
) -> ui::framework::EffectAnswer {
    if matches!(
        effect,
        UpdatesEffect::InstallWindowsSupport | UpdatesEffect::RemoveWindowsSupport
    ) {
        // Its card is where the download is shown and where its end is read;
        // a person who asked from search lands there instead of on a window
        // whose change they cannot see.
        app.request_pane(crate::categories::Category::Updates);
    }
    let reply = cx.reply();
    crate::panes::updates::area(app)
        .ok_or_else(crate::area::missing)?
        .model()
        .borrow_mut()
        .run(effect, Some(reply));
    Ok(Effected::Pending)
}

impl UpdatesModel {
    fn poll(&mut self) -> bool {
        self.drain();
        std::mem::take(&mut self.changed)
    }

    fn busy(&self) -> bool {
        !self.asked.is_empty()
            || self.update().is_some_and(checking)
            || self.firmware_refresh_pending
            || self.firmware_events.is_some()
    }
}

pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    let model = Rc::clone(held.model());
    model.borrow_mut().poll();
    Some(cx.switch(&held.shown, move |cx, _| model.borrow().page(cx)))
}

pub(crate) mod values {
    use ui::LocalState;

    use provider_settings_ui::bound::SettingRows as _;
    use ui::framework::ViewCx;

    use super::{UpdatesEffect, UpdatesModel, area};
    use crate::effects::effect;
    use crate::i18n::t;
    use crate::resource::Settings;
    use crate::setting::{Follows, Read};
    use crate::settings_app::SettingsApp;
    use provider_settings_ui::widgets::Item;

    fn read<V>(app: &SettingsApp, read: impl FnOnce(&UpdatesModel) -> Option<V>) -> Option<V> {
        let model = area(app)?.model.get()?;
        let model = model.borrow();
        read(&model)
    }

    pub(crate) fn automatic(app: &SettingsApp, _: &Follows) -> Read {
        read(app, |model| {
            model.timer_enabled.is_some().then_some(model.prefs.auto)
        })
        .map_or(Read::Value(None), Read::Switch)
    }

    pub(crate) fn automatic_switch(cx: &ViewCx<Settings>, on: bool) -> Item {
        cx.setting(t("upd-automatic"))
            .id("updates.auto")
            .toggle_effect(
                &LocalState::new(on),
                &cx.actions(),
                effect(UpdatesEffect::SET_AUTOMATIC_UPDATES),
            )
    }

    pub(crate) fn metered(app: &SettingsApp, _: &Follows) -> Read {
        read(app, |model| model.allow_metered).map_or(Read::Value(None), Read::Switch)
    }

    pub(crate) fn metered_switch(cx: &ViewCx<Settings>, on: bool) -> Item {
        cx.setting(t("upd-metered"))
            .id("updates.metered")
            .toggle_effect(
                &LocalState::new(on),
                &cx.actions(),
                effect(UpdatesEffect::SET_METERED_UPDATES),
            )
    }
}

mod windows_card;

#[cfg(test)]
mod tests;

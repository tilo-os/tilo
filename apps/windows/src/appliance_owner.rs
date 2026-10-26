//! Narrow asynchronous client for the owner-managed Windows appliance.
//!
//! The command names and arguments are fixed. In particular, installation
//! media moves from the XDG portal's already-open file descriptor directly to
//! `pkexec` stdin; no user-controlled path crosses the privileged boundary.
use std::fs::File;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use ui::time::{Duration, Instant};

use serde::Deserialize;

use crate::strings::{self, format_all};

const OWNER: &str = "/usr/libexec/tilo-windows-owner";
const RECOVERY_CONSOLE: &str = "/usr/libexec/tilo-windows-recovery-console";
const STATUS_INTERVAL: Duration = Duration::from_secs(2);
const OWNER_OUTPUT_LIMIT: usize = 1024 * 1024;
const STATUS_TIMEOUT: Duration = Duration::from_secs(3);
const ACTION_TIMEOUT: Duration = Duration::from_secs(10 * 60);

fn run_owner_command(
    command: Command,
    timeout: Duration,
) -> Result<bounded_command::Output, String> {
    bounded_command::run_prepared(command, None, OWNER_OUTPUT_LIMIT, timeout)
        .map_err(|error| format_all("windows-error-command", &[("error", error.to_string())]))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleAction {
    Start,
    Restart,
    Suspend,
    Resume,
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressPurpose {
    Activation,
    WindowsUpdate,
}

impl EgressPurpose {
    #[must_use]
    pub fn label(self) -> String {
        strings::t(match self {
            Self::Activation => "windows-purpose-activation",
            Self::WindowsUpdate => "windows-purpose-update",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporaryEgressLease {
    pub purpose: EgressPurpose,
    pub expires_at_unix: u64,
}

impl TemporaryEgressLease {
    /// How long the connection has left, said in the person's words: the
    /// status the lease is read with follows it, so this counts down.
    #[must_use]
    pub fn expiry_label(&self) -> String {
        let now = ui::time::unix_now();
        let Some(remaining) = self.expires_at_unix.checked_sub(now) else {
            return strings::t("windows-connection-expired");
        };
        let (minutes, seconds) = (remaining / 60, remaining % 60);
        if minutes == 0 {
            return format_all(
                "windows-connection-expires-seconds",
                &[("seconds", seconds.to_string())],
            );
        }
        format_all(
            "windows-connection-expires",
            &[
                ("minutes", minutes.to_string()),
                ("seconds", seconds.to_string()),
            ],
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppliancePowerState {
    Running,
    Suspended,
    Paused,
    ShutDown,
    #[default]
    Unknown,
}

impl AppliancePowerState {
    #[must_use]
    pub fn label(self) -> String {
        strings::t(match self {
            Self::Running => "windows-power-running",
            Self::Suspended => "windows-power-suspended",
            Self::Paused => "windows-power-paused",
            Self::ShutDown => "windows-power-shut-down",
            Self::Unknown => "windows-power-unknown",
        })
    }

    #[must_use]
    pub const fn allows(self, action: LifecycleAction) -> bool {
        matches!(
            (self, action),
            (
                Self::Running,
                LifecycleAction::Restart | LifecycleAction::Suspend | LifecycleAction::Shutdown
            ) | (Self::Suspended | Self::Paused, LifecycleAction::Resume)
                | (Self::ShutDown, LifecycleAction::Start)
        )
    }
}

impl EgressPurpose {
    const fn argument(self) -> &'static str {
        match self {
            Self::Activation => "activation",
            Self::WindowsUpdate => "windows-update",
        }
    }

    fn ready_notice(self) -> String {
        strings::t(match self {
            Self::Activation => "windows-connection-opened-activation",
            Self::WindowsUpdate => "windows-connection-opened-update",
        })
    }
}

impl LifecycleAction {
    const fn argument(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Restart => "restart",
            Self::Suspend => "suspend",
            Self::Resume => "resume",
            Self::Shutdown => "shutdown",
        }
    }

    /// The action's own name, as its button says it.
    fn label(self) -> String {
        strings::t(match self {
            Self::Start => "windows-start-appliance",
            Self::Restart => "windows-restart-appliance",
            Self::Suspend => "windows-suspend-appliance",
            Self::Resume => "windows-resume-appliance",
            Self::Shutdown => "windows-shutdown-appliance",
        })
    }

    fn complete_notice(self) -> String {
        strings::t(match self {
            Self::Start => "windows-start-done",
            Self::Restart => "windows-restart-done",
            Self::Suspend => "windows-suspend-done",
            Self::Resume => "windows-resume-done",
            Self::Shutdown => "windows-shutdown-done",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppliancePhase {
    NotProvisioned,
    Installing,
    AwaitingGuestBridge,
    Ready,
    RepairRequired,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseState {
    Unknown,
    DigitallyActivated,
    ProductKeyActivated,
    OrganisationManaged,
    Evaluation,
    Unlicensed,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplianceStatus {
    pub schema_version: u8,
    pub configured: bool,
    pub phase: AppliancePhase,
    pub license: LicenseState,
    pub catalog_generation: u64,
    pub published_applications: usize,
    pub recovery_available: bool,
    #[serde(default)]
    pub power_state: AppliancePowerState,
    #[serde(default)]
    pub temporary_egress: Option<TemporaryEgressLease>,
    #[serde(default)]
    pub detail: Option<String>,
}

impl ApplianceStatus {
    #[must_use]
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            schema_version: 1,
            configured: false,
            phase: AppliancePhase::Unavailable,
            license: LicenseState::Unknown,
            catalog_generation: 0,
            published_applications: 0,
            recovery_available: false,
            power_state: AppliancePowerState::Unknown,
            temporary_egress: None,
            detail: Some(detail.into()),
        }
    }

    /// A set-up, running, activated appliance with one published app: the
    /// offline catalogue's appliance.
    #[must_use]
    pub fn sample_ready() -> Self {
        Self {
            schema_version: 1,
            configured: true,
            phase: AppliancePhase::Ready,
            license: LicenseState::DigitallyActivated,
            catalog_generation: 1,
            published_applications: 1,
            recovery_available: true,
            power_state: AppliancePowerState::Running,
            temporary_egress: None,
            detail: None,
        }
    }

    #[must_use]
    pub const fn is_provisioned(&self) -> bool {
        self.configured
            && !matches!(
                self.phase,
                AppliancePhase::Unavailable | AppliancePhase::NotProvisioned
            )
    }

    #[must_use]
    pub fn phase_label(&self) -> String {
        strings::t(match self.phase {
            AppliancePhase::NotProvisioned => "windows-phase-not-set-up",
            AppliancePhase::Installing => "windows-phase-installing",
            AppliancePhase::AwaitingGuestBridge => "windows-phase-finishing-setup",
            AppliancePhase::Ready => "windows-phase-ready",
            AppliancePhase::RepairRequired => "windows-phase-needs-recovery",
            AppliancePhase::Unavailable => "windows-phase-unavailable",
        })
    }

    /// The one sentence the appliance page states its status in: how far
    /// setup is and, once Windows is ready, whether it is running.
    #[must_use]
    pub fn status_statement(&self) -> String {
        strings::t(match (&self.phase, self.power_state) {
            (AppliancePhase::Ready, AppliancePowerState::Running) => "windows-status-running",
            (AppliancePhase::Ready, AppliancePowerState::Suspended) => "windows-status-suspended",
            (AppliancePhase::Ready, AppliancePowerState::Paused) => "windows-status-paused",
            (AppliancePhase::Ready, AppliancePowerState::ShutDown) => "windows-status-shut-down",
            (AppliancePhase::Ready, AppliancePowerState::Unknown) => "windows-status-ready",
            (AppliancePhase::NotProvisioned, _) => "windows-status-not-set-up",
            (AppliancePhase::Installing, _) => "windows-status-installing",
            (AppliancePhase::AwaitingGuestBridge, _) => "windows-status-finishing-setup",
            (AppliancePhase::RepairRequired, _) => "windows-status-needs-recovery",
            (AppliancePhase::Unavailable, _) => "windows-status-unavailable",
        })
    }

    /// The appliance's status in a list row: its power state once Windows is
    /// ready, how far setup is before that.
    #[must_use]
    pub fn summary_label(&self) -> String {
        match (&self.phase, self.power_state) {
            (AppliancePhase::Ready, AppliancePowerState::Unknown) => self.phase_label(),
            (AppliancePhase::Ready, power) => power.label(),
            _ => self.phase_label(),
        }
    }

    /// Tilo reports only what the authenticated guest said. It deliberately
    /// does not turn activation into a licensing determination.
    #[must_use]
    pub fn license_label(&self) -> String {
        strings::t(match self.license {
            LicenseState::Unknown => "windows-license-unknown",
            LicenseState::DigitallyActivated => "windows-license-digital",
            LicenseState::ProductKeyActivated => "windows-license-product-key",
            LicenseState::OrganisationManaged => "windows-license-managed",
            LicenseState::Evaluation => "windows-license-evaluation",
            LicenseState::Unlicensed => "windows-license-unlicensed",
        })
    }

    /// Additional context for unusual activation states. This stays in
    /// management UI and is intentionally absent for normal app launches.
    #[must_use]
    pub fn license_detail(&self) -> Option<String> {
        Some(strings::t(match self.license {
            LicenseState::Unknown => "windows-license-unknown-detail",
            LicenseState::Unlicensed => "windows-license-unlicensed-detail",
            LicenseState::Evaluation => "windows-license-evaluation-detail",
            LicenseState::DigitallyActivated
            | LicenseState::ProductKeyActivated
            | LicenseState::OrganisationManaged => return None,
        }))
    }
}

enum OwnerEvent {
    Status(Result<ApplianceStatus, String>),
    Imported(Result<(), String>),
    Recovery(Result<(), String>),
    Lifecycle(LifecycleAction, Result<(), String>),
    Egress(EgressPurpose, Result<(), String>),
    EgressRevoked(Result<(), String>),
}

/// One narrow nonblocking owner-command queue. It never exposes a generic
/// command interface to the UI.
#[allow(
    clippy::struct_excessive_bools,
    reason = "pending status, pending action, checked and offline are independent facts"
)]
pub struct ApplianceOwnerClient {
    events: Receiver<OwnerEvent>,
    sender: Sender<OwnerEvent>,
    status_pending: bool,
    action_pending: bool,
    action_result: Option<Result<String, String>>,
    next_refresh: Instant,
    status: ApplianceStatus,
    /// A status has arrived (or was given): the appliance is not still
    /// being checked.
    checked: bool,
    /// Never asks the owner helper: the status was given and stays.
    offline: bool,
}

impl ApplianceOwnerClient {
    #[must_use]
    pub fn new() -> Self {
        let (sender, events) = mpsc::channel();
        let mut client = Self {
            events,
            sender,
            status_pending: false,
            action_pending: false,
            action_result: None,
            next_refresh: ui::time::now()
                .checked_sub(STATUS_INTERVAL)
                .unwrap_or_else(Instant::now),
            status: ApplianceStatus::unavailable(strings::t("windows-appliance-checking")),
            checked: false,
            offline: false,
        };
        client.refresh();
        client
    }

    /// A client that reports `status` and never runs the owner helper: for
    /// offline catalogues, screenshots and tests.
    #[must_use]
    pub fn offline(status: ApplianceStatus) -> Self {
        let (sender, events) = mpsc::channel();
        Self {
            events,
            sender,
            status_pending: false,
            action_pending: false,
            action_result: None,
            next_refresh: ui::time::now(),
            status,
            checked: true,
            offline: true,
        }
    }

    /// Refuse a new owner action while one runs, or when this client only
    /// reports a given status.
    fn ensure_idle(&self) -> Result<(), String> {
        if self.offline {
            return Err(strings::t("windows-error-unmanageable"));
        }
        if self.action_pending {
            return Err(strings::t("windows-error-busy"));
        }
        Ok(())
    }

    /// Whether a status has arrived since the client started.
    #[must_use]
    pub const fn has_status(&self) -> bool {
        self.checked
    }

    #[must_use]
    pub fn status(&self) -> &ApplianceStatus {
        &self.status
    }

    #[must_use]
    pub const fn busy(&self) -> bool {
        self.action_pending
    }

    pub fn refresh(&mut self) {
        if self.offline
            || self.status_pending
            || self.action_pending
            || ui::time::now() < self.next_refresh
        {
            return;
        }
        self.status_pending = true;
        self.next_refresh = ui::time::now() + STATUS_INTERVAL;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send(OwnerEvent::Status(read_status()));
        });
    }

    pub fn import_iso(&mut self, source: File) -> Result<(), String> {
        self.ensure_idle()?;
        self.action_pending = true;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let mut command = Command::new("pkexec");
            command.arg(OWNER).arg("import-iso");
            // The open XDG portal descriptor is intentionally the only
            // installation-media input handed to the privileged helper.
            let result =
                command_with_stdin(command, source).and_then(|output| success_output(&output));
            let _ = sender.send(OwnerEvent::Imported(result));
        });
        Ok(())
    }

    pub fn open_recovery(&mut self) -> Result<(), String> {
        self.ensure_idle()?;
        if !self.status.recovery_available {
            return Err(strings::t("windows-error-recovery-unavailable"));
        }
        self.start_recovery()
    }

    /// Execute one fixed appliance lifecycle action through the existing
    /// polkit boundary. There is intentionally no generic command facility.
    pub fn lifecycle(&mut self, action: LifecycleAction) -> Result<(), String> {
        self.ensure_idle()?;
        if !self.status.is_provisioned() {
            return Err(strings::t("windows-error-set-up-power"));
        }
        if !self.status.power_state.allows(action) {
            return Err(format_all(
                "windows-error-power-state",
                &[("action", action.label())],
            ));
        }
        self.action_pending = true;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let mut command = Command::new("pkexec");
            command.arg(OWNER).arg("lifecycle").arg(action.argument());
            let result = run_owner_command(command, ACTION_TIMEOUT)
                .and_then(|output| success_bounded_output(&output));
            let _ = sender.send(OwnerEvent::Lifecycle(action, result));
        });
        Ok(())
    }

    /// Materialise a ten-minute, purpose-bound appliance connection and then
    /// take the owner to the already-authorised recovery/update desktop. The
    /// system-owned lease reaper closes it even if this manager exits.
    pub fn open_temporary_egress(&mut self, purpose: EgressPurpose) -> Result<(), String> {
        self.ensure_idle()?;
        if !self.status.is_provisioned() {
            return Err(strings::t("windows-error-set-up-connection"));
        }
        if self.status.power_state != AppliancePowerState::Running {
            return Err(strings::t("windows-error-not-running"));
        }
        self.action_pending = true;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let mut command = Command::new("pkexec");
            command
                .arg(OWNER)
                .arg("egress-open")
                .arg(purpose.argument())
                .arg("600");
            let result = run_owner_command(command, ACTION_TIMEOUT)
                .and_then(|output| success_bounded_output(&output));
            let _ = sender.send(OwnerEvent::Egress(purpose, result));
        });
        Ok(())
    }

    /// Revoke the active temporary egress lease early. The owner helper looks
    /// it up from authoritative state, so no lease identifier reaches UI.
    pub fn revoke_temporary_egress(&mut self) -> Result<(), String> {
        self.ensure_idle()?;
        if self.status.temporary_egress.is_none() {
            return Err(strings::t("windows-error-connection-no-lease"));
        }
        self.action_pending = true;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let mut command = Command::new("pkexec");
            command.arg(OWNER).arg("egress-revoke");
            let result = run_owner_command(command, ACTION_TIMEOUT)
                .and_then(|output| success_bounded_output(&output));
            let _ = sender.send(OwnerEvent::EgressRevoked(result));
        });
        Ok(())
    }

    /// This deliberately bypasses the read-only status precheck only after a
    /// successful `import-iso`: the helper has just prepared the appliance,
    /// while the status cache has not yet had an opportunity to refresh.
    fn start_recovery(&mut self) -> Result<(), String> {
        self.ensure_idle()?;
        self.action_pending = true;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let mut command = Command::new("pkexec");
            command.arg(OWNER).arg("recovery-start");
            let result = run_owner_command(command, ACTION_TIMEOUT)
                .and_then(|output| success_bounded_output(&output))
                .and_then(|()| {
                    Command::new(RECOVERY_CONSOLE)
                        .arg("--owner-authorized")
                        .spawn()
                        .map(|_| ())
                        .map_err(|error| {
                            format_all(
                                "windows-error-recovery-open",
                                &[("error", error.to_string())],
                            )
                        })
                });
            let _ = sender.send(OwnerEvent::Recovery(result));
        });
        Ok(())
    }

    /// Applies completed worker events and returns a human-facing result for
    /// the Windows Apps notice area.
    pub fn drain(&mut self) -> Option<String> {
        let mut notice = None;
        while let Ok(event) = self.events.try_recv() {
            match event {
                OwnerEvent::Status(Ok(status)) if status.schema_version == 1 => {
                    self.status = status;
                    self.status_pending = false;
                    self.checked = true;
                }
                OwnerEvent::Status(Ok(_)) => {
                    self.status =
                        ApplianceStatus::unavailable(strings::t("windows-error-status-schema"));
                    self.status_pending = false;
                    self.checked = true;
                }
                OwnerEvent::Status(Err(error)) => {
                    self.status = ApplianceStatus::unavailable(error);
                    self.status_pending = false;
                    self.checked = true;
                }
                OwnerEvent::Imported(Ok(())) => {
                    self.action_pending = false;
                    notice = Some(strings::t("windows-media-accepted"));
                    // First provisioning intentionally uses the recovery-only
                    // desktop route, then normal apps return to app-focused
                    // guest windows.
                    if let Err(error) = self.start_recovery() {
                        self.action_result = Some(Err(error.clone()));
                        notice = Some(error);
                    }
                }
                OwnerEvent::Recovery(Ok(())) => {
                    self.action_pending = false;
                    let detail = strings::t("windows-recovery-open");
                    self.action_result = Some(Ok(detail.clone()));
                    notice = Some(detail);
                }
                OwnerEvent::Imported(Err(error))
                | OwnerEvent::Recovery(Err(error))
                | OwnerEvent::Lifecycle(_, Err(error))
                | OwnerEvent::Egress(_, Err(error))
                | OwnerEvent::EgressRevoked(Err(error)) => {
                    self.action_pending = false;
                    self.action_result = Some(Err(error.clone()));
                    notice = Some(error);
                }
                OwnerEvent::Lifecycle(action, Ok(())) => {
                    self.action_pending = false;
                    let detail = action.complete_notice();
                    self.action_result = Some(Ok(detail.clone()));
                    notice = Some(detail);
                }
                OwnerEvent::Egress(purpose, Ok(())) => {
                    self.action_pending = false;
                    notice = Some(purpose.ready_notice());
                    if let Err(error) = self.start_recovery() {
                        self.action_result = Some(Err(error.clone()));
                        notice = Some(error);
                    }
                }
                OwnerEvent::EgressRevoked(Ok(())) => {
                    self.action_pending = false;
                    notice = Some(strings::t("windows-connection-revoked"));
                }
            }
        }
        if !self.action_pending {
            self.refresh();
        }
        notice
    }

    /// Take only a completed owner action, excluding periodic status notices.
    pub fn take_action_result(&mut self) -> Option<Result<String, String>> {
        self.action_result.take()
    }
}

fn command_with_stdin(mut command: Command, source: File) -> Result<std::process::Output, String> {
    command
        .stdin(Stdio::from(source))
        .output()
        .map_err(|error| {
            format_all(
                "windows-error-permission-ask",
                &[("error", error.to_string())],
            )
        })
}

fn read_status() -> Result<ApplianceStatus, String> {
    let mut command = Command::new(OWNER);
    command.arg("status");
    let output = run_owner_command(command, STATUS_TIMEOUT)
        .map_err(|error| format_all("windows-error-status-read", &[("error", error)]))?;
    if !output.status.success() {
        return Err(output_error(&output.stderr));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format_all(
            "windows-error-status-invalid",
            &[("error", error.to_string())],
        )
    })
}

fn success_output(output: &std::process::Output) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(output_error(&output.stderr))
    }
}

fn success_bounded_output(output: &bounded_command::Output) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(output_error(&output.stderr))
    }
}

impl Default for ApplianceOwnerClient {
    fn default() -> Self {
        Self::new()
    }
}

fn output_error(stderr: &[u8]) -> String {
    let detail = String::from_utf8_lossy(stderr).trim().to_string();
    if detail.is_empty() {
        strings::t("windows-error-permission")
    } else {
        detail
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn owner_status_parses_only_the_typed_shape() {
        {
            let status: ApplianceStatus = serde_json::from_str(
            r#"{"schema_version":1,"configured":true,"phase":"ready","license":"digitally_activated","catalog_generation":4,"published_applications":2,"recovery_available":true}"#,
        )
        .unwrap();
            assert!(status.is_provisioned());
            assert_eq!(status.phase_label(), "Ready");
            assert_eq!(status.power_state, AppliancePowerState::Unknown);
            assert_eq!(status.temporary_egress, None);
            assert_eq!(status.status_statement(), "Windows is ready for your apps.");
            assert_eq!(status.summary_label(), "Ready");
            assert_eq!(
                status.license_label(),
                "Windows says it's activated with a digital license"
            );
            assert_eq!(status.license_detail(), None);
        }
        {
            let status = ApplianceStatus {
                schema_version: 1,
                configured: true,
                phase: AppliancePhase::Ready,
                license: LicenseState::Unlicensed,
                catalog_generation: 0,
                published_applications: 0,
                recovery_available: true,
                power_state: AppliancePowerState::Running,
                temporary_egress: None,
                detail: None,
            };
            assert_eq!(status.license_label(), "Windows says it isn't activated");
            assert!(
                status
                    .license_detail()
                    .is_some_and(|detail| detail.contains("doesn't keep product keys"))
            );
        }
        {
            let result = serde_json::from_str::<ApplianceStatus>(
                r#"{"schema_version":1,"configured":false,"phase":"not_provisioned","license":"unknown","catalog_generation":0,"published_applications":0,"recovery_available":false,"command":"cmd.exe"}"#,
            );
            assert!(result.is_err());
        }
        {
            let status: ApplianceStatus = serde_json::from_str(
            r#"{"schema_version":1,"configured":true,"phase":"ready","license":"unlicensed","catalog_generation":4,"published_applications":2,"recovery_available":true,"power_state":"suspended","temporary_egress":{"purpose":"activation","expires_at_unix":4102444800}}"#,
        )
        .unwrap();
            assert_eq!(status.power_state, AppliancePowerState::Suspended);
            assert!(status.power_state.allows(LifecycleAction::Resume));
            assert!(!status.power_state.allows(LifecycleAction::Shutdown));
            assert!(AppliancePowerState::ShutDown.allows(LifecycleAction::Start));
            assert!(AppliancePowerState::Paused.allows(LifecycleAction::Resume));
            assert_eq!(
                status.temporary_egress.as_ref().map(|lease| lease.purpose),
                Some(EgressPurpose::Activation)
            );
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let source_path = dir.path().join("selected.iso");
            let captured_path = dir.path().join("captured.iso");
            std::fs::write(&source_path, b"accepted ISO descriptor").unwrap();
            let source = File::open(&source_path).unwrap();
            std::fs::remove_file(&source_path).unwrap();
            std::fs::write(&source_path, b"replacement ISO pathname").unwrap();

            let mut command = Command::new("sh");
            command
                .arg("-c")
                .arg("cat > \"$1\"")
                .arg("tilo-test-capture")
                .arg(&captured_path);
            let output = command_with_stdin(command, source).unwrap();
            assert!(output.status.success());
            let mut captured = String::new();
            File::open(captured_path)
                .unwrap()
                .read_to_string(&mut captured)
                .unwrap();
            assert_eq!(captured, "accepted ISO descriptor");
        }
    }
}

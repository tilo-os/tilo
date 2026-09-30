//! Narrow asynchronous client for the owner-managed Windows appliance.
//!
//! The command names and arguments are fixed. In particular, installation
//! media moves from the XDG portal's already-open file descriptor directly to
//! `pkexec` stdin; no user-controlled path crosses the privileged boundary.
use std::fs::File;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use ui::time::{Duration, Instant, UNIX_EPOCH};

use serde::Deserialize;

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
        .map_err(|error| format!("The Windows appliance command failed: {error}"))
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
    pub const fn label(self) -> &'static str {
        match self {
            Self::Activation => "Windows activation",
            Self::WindowsUpdate => "Windows Update",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporaryEgressLease {
    pub purpose: EgressPurpose,
    pub expires_at_unix: u64,
}

impl TemporaryEgressLease {
    #[must_use]
    pub fn expiry_label(&self) -> String {
        let now = ui::time::wall()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        if self.expires_at_unix <= now {
            "Expired; refreshing status…".into()
        } else {
            let remaining = self.expires_at_unix - now;
            format!("Expires in {}m {}s", remaining / 60, remaining % 60)
        }
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
    pub const fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Suspended => "Suspended",
            Self::Paused => "Paused",
            Self::ShutDown => "Shut down",
            Self::Unknown => "Power state unavailable",
        }
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

    const fn ready_notice(self) -> &'static str {
        match self {
            Self::Activation => {
                "Windows has a temporary activation connection. Opening the authorised recovery desktop…"
            }
            Self::WindowsUpdate => {
                "Windows has a temporary update connection. Opening the authorised recovery desktop…"
            }
        }
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

    const fn complete_notice(self) -> &'static str {
        match self {
            Self::Start => "Windows is starting.",
            Self::Restart => "Windows is restarting.",
            Self::Suspend => "Windows has been suspended.",
            Self::Resume => "Windows is resuming.",
            Self::Shutdown => "Windows is shutting down.",
        }
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

    #[must_use]
    pub const fn is_provisioned(&self) -> bool {
        self.configured
            && !matches!(
                self.phase,
                AppliancePhase::Unavailable | AppliancePhase::NotProvisioned
            )
    }

    #[must_use]
    pub const fn phase_label(&self) -> &'static str {
        match self.phase {
            AppliancePhase::NotProvisioned => "Not set up",
            AppliancePhase::Installing => "Windows is installing",
            AppliancePhase::AwaitingGuestBridge => "Finishing Windows setup",
            AppliancePhase::Ready => "Ready",
            AppliancePhase::RepairRequired => "Needs recovery",
            AppliancePhase::Unavailable => "Status unavailable",
        }
    }

    /// A short, factual step for the appliance-management card. This is not
    /// shown in a published application's normal launch path: activation and
    /// setup are attributes of the appliance, not of an app window.
    #[must_use]
    pub const fn next_step_label(&self) -> &'static str {
        match self.phase {
            AppliancePhase::NotProvisioned => "Choose Windows installation media",
            AppliancePhase::Installing => "Complete Windows Setup in the authorised setup desktop",
            AppliancePhase::AwaitingGuestBridge => {
                "Finish Tilo guest integration in the authorised recovery desktop"
            }
            AppliancePhase::Ready => "Windows is ready for published applications",
            AppliancePhase::RepairRequired => {
                "Open the authorised recovery desktop to repair Windows"
            }
            AppliancePhase::Unavailable => "Check the appliance status before making changes",
        }
    }

    /// Tilo reports only what the authenticated guest said. It deliberately
    /// does not turn activation into a licensing determination.
    #[must_use]
    pub const fn license_label(&self) -> &'static str {
        match self.license {
            LicenseState::Unknown => "Activation status unavailable",
            LicenseState::DigitallyActivated => "Windows reports digitally activated",
            LicenseState::ProductKeyActivated => "Windows reports product-key activation",
            LicenseState::OrganisationManaged => "Activation is managed by your organisation",
            LicenseState::Evaluation => "Windows reports an evaluation licence",
            LicenseState::Unlicensed => "Windows reports that it is not activated",
        }
    }

    /// Additional context for unusual activation states. This stays in
    /// management UI and is intentionally absent for normal app launches.
    #[must_use]
    pub const fn license_detail(&self) -> Option<&'static str> {
        match self.license {
            LicenseState::Unknown => {
                Some("Finish Windows Setup, then return here to check its activation status.")
            }
            LicenseState::Unlicensed => Some(
                "Use the authorised setup or recovery desktop to activate Windows. Tilo does not keep product keys or decide whether a licence is valid.",
            ),
            LicenseState::Evaluation => Some(
                "Evaluation editions have their own terms and expiry. Tilo does not keep product keys or decide whether a licence is valid.",
            ),
            LicenseState::DigitallyActivated
            | LicenseState::ProductKeyActivated
            | LicenseState::OrganisationManaged => None,
        }
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
pub struct ApplianceOwnerClient {
    events: Receiver<OwnerEvent>,
    sender: Sender<OwnerEvent>,
    status_pending: bool,
    action_pending: bool,
    action_result: Option<Result<String, String>>,
    next_refresh: Instant,
    status: ApplianceStatus,
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
            status: ApplianceStatus::unavailable("Checking the Windows appliance…"),
        };
        client.refresh();
        client
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
        if self.status_pending || self.action_pending || ui::time::now() < self.next_refresh {
            return;
        }
        self.status_pending = true;
        self.next_refresh = ui::time::now() + STATUS_INTERVAL;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send(OwnerEvent::Status(read_status()));
        });
    }

    /// # Errors
    ///
    /// Returns an error if an appliance action is already active.
    pub fn import_iso(&mut self, source: File) -> Result<(), String> {
        if self.action_pending {
            return Err("A Windows appliance action is already in progress.".into());
        }
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

    /// # Errors
    ///
    /// Returns an error if the appliance is not provisioned or an action is
    /// already active.
    pub fn open_recovery(&mut self) -> Result<(), String> {
        if self.action_pending {
            return Err("A Windows appliance action is already in progress.".into());
        }
        if !self.status.recovery_available {
            return Err(
                "The Windows recovery desktop is unavailable until the appliance is provisioned."
                    .into(),
            );
        }
        self.start_recovery()
    }

    /// Execute one fixed appliance lifecycle action through the existing
    /// polkit boundary. There is intentionally no generic command facility.
    ///
    /// # Errors
    ///
    /// Returns an error when another owner action is active or the appliance
    /// has not been provisioned.
    pub fn lifecycle(&mut self, action: LifecycleAction) -> Result<(), String> {
        if self.action_pending {
            return Err("A Windows appliance action is already in progress.".into());
        }
        if !self.status.is_provisioned() {
            return Err("Set up the Windows appliance before managing its power state.".into());
        }
        if !self.status.power_state.allows(action) {
            return Err(format!(
                "{} is not available while Windows is {}.",
                action.argument(),
                self.status.power_state.label().to_ascii_lowercase()
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
    ///
    /// # Errors
    ///
    /// Returns an error when another owner action is active or the appliance
    /// has not been provisioned.
    pub fn open_temporary_egress(&mut self, purpose: EgressPurpose) -> Result<(), String> {
        if self.action_pending {
            return Err("A Windows appliance action is already in progress.".into());
        }
        if !self.status.is_provisioned() {
            return Err("Set up the Windows appliance before connecting it temporarily.".into());
        }
        if self.status.power_state != AppliancePowerState::Running {
            return Err("Windows must be running before it can be connected temporarily.".into());
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
    ///
    /// # Errors
    ///
    /// Returns an error when another owner action is pending or no temporary
    /// connection is active.
    pub fn revoke_temporary_egress(&mut self) -> Result<(), String> {
        if self.action_pending {
            return Err("A Windows appliance action is already in progress.".into());
        }
        if self.status.temporary_egress.is_none() {
            return Err("There is no active temporary Windows connection to revoke.".into());
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
        if self.action_pending {
            return Err("A Windows appliance action is already in progress.".into());
        }
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
                            format!(
                                "Could not open the authorised Windows recovery viewer: {error}"
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
                }
                OwnerEvent::Status(Ok(_)) => {
                    self.status = ApplianceStatus::unavailable(
                        "The Windows appliance returned an unsupported status schema.",
                    );
                    self.status_pending = false;
                }
                OwnerEvent::Status(Err(error)) => {
                    self.status = ApplianceStatus::unavailable(error);
                    self.status_pending = false;
                }
                OwnerEvent::Imported(Ok(())) => {
                    self.action_pending = false;
                    notice = Some(
                        "Windows media was accepted. Opening the initial setup desktop…".into(),
                    );
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
                    let detail =
                        "The authorised Windows setup or recovery desktop is open.".to_string();
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
                    let detail = action.complete_notice().to_string();
                    self.action_result = Some(Ok(detail.clone()));
                    notice = Some(detail);
                }
                OwnerEvent::Egress(purpose, Ok(())) => {
                    self.action_pending = false;
                    notice = Some(purpose.ready_notice().into());
                    if let Err(error) = self.start_recovery() {
                        self.action_result = Some(Err(error.clone()));
                        notice = Some(error);
                    }
                }
                OwnerEvent::EgressRevoked(Ok(())) => {
                    self.action_pending = false;
                    notice = Some("The temporary Windows connection was revoked.".into());
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
        .map_err(|error| format!("Could not start the standard authorisation prompt: {error}"))
}

fn read_status() -> Result<ApplianceStatus, String> {
    let mut command = Command::new(OWNER);
    command.arg("status");
    let output = run_owner_command(command, STATUS_TIMEOUT)
        .map_err(|error| format!("Could not read Windows appliance status: {error}"))?;
    if !output.status.success() {
        return Err(output_error(&output.stderr));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Windows appliance returned invalid status: {error}"))
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
        "The authorisation action did not complete.".into()
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
        // Case `parses_only_the_typed_owner_status_shape`.
        {
            let status: ApplianceStatus = serde_json::from_str(
            r#"{"schema_version":1,"configured":true,"phase":"ready","license":"digitally_activated","catalog_generation":4,"published_applications":2,"recovery_available":true}"#,
        )
        .unwrap();
            assert!(status.is_provisioned());
            assert_eq!(status.phase_label(), "Ready");
            assert_eq!(status.power_state, AppliancePowerState::Unknown);
            assert_eq!(status.temporary_egress, None);
            assert_eq!(
                status.next_step_label(),
                "Windows is ready for published applications"
            );
            assert_eq!(
                status.license_label(),
                "Windows reports digitally activated"
            );
            assert_eq!(status.license_detail(), None);
        }
        // Case `unactivated_status_is_management_guidance_not_a_licensing_claim`.
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
            assert_eq!(
                status.license_label(),
                "Windows reports that it is not activated"
            );
            assert!(
                status
                    .license_detail()
                    .is_some_and(|detail| detail.contains("does not keep product keys"))
            );
        }
        // Case `rejects_untyped_owner_status_fields`.
        {
            let result = serde_json::from_str::<ApplianceStatus>(
                r#"{"schema_version":1,"configured":false,"phase":"not_provisioned","license":"unknown","catalog_generation":0,"published_applications":0,"recovery_available":false,"command":"cmd.exe"}"#,
            );
            assert!(result.is_err());
        }
        // Case `parses_temporary_egress_and_power_state_without_exposing_a_lease_id`.
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
        // Case `iso_handoff_streams_the_open_descriptor_not_a_later_path_reopen`.
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

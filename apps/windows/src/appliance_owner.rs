//! Narrow asynchronous client for the owner-managed Windows appliance.
//!
//! The command names and arguments are fixed. In particular, installation
//! media moves from the XDG portal's already-open file descriptor directly to
//! `pkexec` stdin; no user-controlled path crosses the privileged boundary.
use std::fs::File;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use serde::Deserialize;

const OWNER: &str = "/usr/libexec/qos-windows-owner";
const RECOVERY_CONSOLE: &str = "/usr/libexec/qos-windows-recovery-console";
const STATUS_INTERVAL: Duration = Duration::from_secs(2);

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
}

enum OwnerEvent {
    Status(Result<ApplianceStatus, String>),
    Imported(Result<(), String>),
    Recovery(Result<(), String>),
}

/// One narrow nonblocking owner-command queue. It never exposes a generic
/// command interface to the UI.
pub struct ApplianceOwnerClient {
    events: Receiver<OwnerEvent>,
    sender: Sender<OwnerEvent>,
    status_pending: bool,
    action_pending: bool,
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
            next_refresh: Instant::now() - STATUS_INTERVAL,
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
        if self.status_pending || self.action_pending || Instant::now() < self.next_refresh {
            return;
        }
        self.status_pending = true;
        self.next_refresh = Instant::now() + STATUS_INTERVAL;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send(OwnerEvent::Status(read_status()));
        });
    }

    pub fn import_iso(&mut self, source: File) -> Result<(), String> {
        if self.action_pending {
            return Err("A Windows appliance action is already in progress.".into());
        }
        self.action_pending = true;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result = Command::new("pkexec")
                .arg(OWNER)
                .arg("import-iso")
                // The open XDG portal descriptor is intentionally the only
                // installation-media input handed to the privileged helper.
                .stdin(Stdio::from(source))
                .output()
                .map_err(|error| {
                    format!("Could not start the standard authorisation prompt: {error}")
                })
                .and_then(success_output);
            let _ = sender.send(OwnerEvent::Imported(result));
        });
        Ok(())
    }

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
            let result = Command::new("pkexec")
                .arg(OWNER)
                .arg("recovery-start")
                .output()
                .map_err(|error| {
                    format!("Could not start the standard authorisation prompt: {error}")
                })
                .and_then(success_output)
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
                        notice = Some(error);
                    }
                }
                OwnerEvent::Imported(Err(error)) => {
                    self.action_pending = false;
                    notice = Some(error);
                }
                OwnerEvent::Recovery(Ok(())) => {
                    self.action_pending = false;
                    notice =
                        Some("The authorised Windows setup or recovery desktop is open.".into());
                }
                OwnerEvent::Recovery(Err(error)) => {
                    self.action_pending = false;
                    notice = Some(error);
                }
            }
        }
        if !self.action_pending {
            self.refresh();
        }
        notice
    }
}

fn read_status() -> Result<ApplianceStatus, String> {
    let output = Command::new(OWNER)
        .arg("status")
        .output()
        .map_err(|error| format!("Could not read Windows appliance status: {error}"))?;
    if !output.status.success() {
        return Err(output_error(&output.stderr));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Windows appliance returned invalid status: {error}"))
}

fn success_output(output: std::process::Output) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(output_error(&output.stderr))
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

    #[test]
    fn parses_only_the_typed_owner_status_shape() {
        let status: ApplianceStatus = serde_json::from_str(
            r#"{"schema_version":1,"configured":true,"phase":"ready","license":"digitally_activated","catalog_generation":4,"published_applications":2,"recovery_available":true}"#,
        )
        .unwrap();
        assert!(status.is_provisioned());
        assert_eq!(status.phase_label(), "Ready");
    }

    #[test]
    fn rejects_untyped_owner_status_fields() {
        let result = serde_json::from_str::<ApplianceStatus>(
            r#"{"schema_version":1,"configured":false,"phase":"not_provisioned","license":"unknown","catalog_generation":0,"published_applications":0,"recovery_available":false,"command":"cmd.exe"}"#,
        );
        assert!(result.is_err());
    }
}

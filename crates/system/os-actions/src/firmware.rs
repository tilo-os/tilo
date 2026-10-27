//! Firmware inventory and upgrades through fwupd.
//!
//! `fwupdmgr` is only a client for the system fwupd daemon.  The daemon
//! validates signed LVFS metadata and firmware and polkit owns authorization;
//! this module never downloads an arbitrary URL or elevates a shell command.

use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crate::process::{run, run_capture};

const MAX_FIRMWARE_STREAM_BYTES: u64 = 8 * 1024 * 1024;
const MAX_FIRMWARE_LINE_BYTES: usize = 64 * 1024;
const MAX_FIRMWARE_EVENTS: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwareUpdate {
    /// fwupd's stable device identifier, used to scope installation to the
    /// exact inventory shown to the person approving the transaction.
    pub device_id: Option<String>,
    pub device: String,
    pub current_version: Option<String>,
    pub new_version: Option<String>,
    pub description: Option<String>,
    pub urgency: Option<String>,
    pub problems: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwareHistoryEntry {
    pub device: String,
    pub from_version: Option<String>,
    pub to_version: Option<String>,
    pub state: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwareInstallOutcome {
    pub installed: usize,
    pub needs_reboot: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FirmwareEvent {
    Started { total: usize },
    Progress { percent: u8, message: String },
    Completed(FirmwareInstallOutcome),
    Failed(String),
}

/// Query updates already offered by fwupd's configured remotes.
pub fn get_firmware_updates() -> Result<Vec<FirmwareUpdate>, String> {
    let json = run_capture("fwupdmgr", &["get-updates", "--json"])?;
    parse_updates(&json)
}

/// Refresh signed metadata from configured fwupd remotes.
pub fn refresh_firmware_metadata() -> Result<(), String> {
    run("fwupdmgr", &["refresh"])
}

/// Read fwupd's own transaction history. This is authoritative lifecycle
/// state, not a Tilo-maintained success counter.
pub fn get_firmware_history() -> Result<Vec<FirmwareHistoryEntry>, String> {
    let json = run_capture("fwupdmgr", &["get-history", "--json"])?;
    parse_history(&json)
}

/// Start a long-running fwupd transaction without blocking the compositor or
/// Settings event loop. fwupd remains the verifier and policy authority; Tilo
/// only reports its lifecycle and never passes force/downgrade flags.
///
/// # Errors
///
/// Returns an error message if `updates` is empty, the battery is below 20% and not on AC
/// power, an update is missing a device id, or the worker thread fails to start.
pub fn start_firmware_install(
    updates: &[FirmwareUpdate],
) -> Result<Receiver<FirmwareEvent>, String> {
    if updates.is_empty() {
        return Err("there are no firmware updates to install".into());
    }
    let battery = crate::battery::BatteryInfo::from_system();
    if battery.present && !battery.on_ac && battery.capacity < 20 {
        return Err(
            "connect power or charge the system battery above 20% before updating firmware".into(),
        );
    }
    let device_ids: Vec<String> = updates
        .iter()
        .map(|update| {
            update
                .device_id
                .clone()
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| format!("fwupd did not identify {}", update.device))
        })
        .collect::<Result<_, _>>()?;
    let total = device_ids.len();
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("tilo-fwupd".into())
        .spawn(move || {
            let _ = tx.send(FirmwareEvent::Started { total });
            let mut needs_reboot = false;
            let result = device_ids
                .iter()
                .enumerate()
                .try_for_each(|(index, device_id)| {
                    run_fwupd_transaction(device_id, |event| {
                        let event = match event {
                            FirmwareEvent::Progress { percent, message } => {
                                // Blend this device's own progress into the overall total: each
                                // device owns an equal 100/total-percent slice of the run.
                                let overall = index
                                    .saturating_mul(100)
                                    .saturating_add(usize::from(percent))
                                    .checked_div(total)
                                    .unwrap_or(0)
                                    .min(100);
                                FirmwareEvent::Progress {
                                    percent: u8::try_from(overall).unwrap_or(100),
                                    message,
                                }
                            }
                            other => other,
                        };
                        let _ = tx.send(event);
                    })
                    .map(|reboot| needs_reboot |= reboot)
                });
            match result {
                Ok(()) => {
                    let _ = tx.send(FirmwareEvent::Completed(FirmwareInstallOutcome {
                        installed: total,
                        needs_reboot,
                    }));
                }
                Err(error) => {
                    let _ = tx.send(FirmwareEvent::Failed(error));
                }
            }
        })
        .map_err(|e| format!("could not start firmware worker: {e}"))?;
    Ok(rx)
}

fn run_fwupd_transaction(
    device_id: &str,
    mut emit: impl FnMut(FirmwareEvent),
) -> Result<bool, String> {
    let mut child = Command::new("fwupdmgr")
        .args(["update", device_id, "-y", "--no-reboot-check"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("fwupdmgr could not start: {e}"))?;
    let stdout = child.stdout.take().ok_or("fwupdmgr stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("fwupdmgr stderr unavailable")?;
    let (line_tx, line_rx) = mpsc::channel();
    let out_tx = line_tx.clone();
    std::thread::spawn(move || read_firmware_progress(stdout, &out_tx));
    let err_tx = line_tx.clone();
    std::thread::spawn(move || read_firmware_progress(stderr, &err_tx));
    drop(line_tx);

    let started = Instant::now();
    let deadline = Duration::from_secs(2700);
    let mut last_lines = Vec::new();
    loop {
        while let Ok(event) = line_rx.try_recv() {
            let line = match event {
                Ok(line) => line,
                Err(error) => {
                    bounded_command::terminate_tree(&mut child, std::time::Duration::ZERO);
                    return Err(error);
                }
            };
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                last_lines.push(trimmed.to_string());
                if last_lines.len() > 8 {
                    last_lines.remove(0);
                }
                emit(FirmwareEvent::Progress {
                    percent: progress_percent(trimmed).unwrap_or(0),
                    message: clean_progress_message(trimmed),
                });
            }
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("fwupdmgr wait failed: {e}"))?
        {
            if !status.success() {
                return Err(if last_lines.is_empty() {
                    format!("fwupdmgr exited with status {:?}", status.code())
                } else {
                    last_lines.join(" · ")
                });
            }
            break;
        }
        if started.elapsed() > deadline {
            bounded_command::terminate_tree(&mut child, std::time::Duration::ZERO);
            return Err("firmware update timed out after 45 minutes".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(reboot_needed())
}

fn read_firmware_progress(
    stream: impl Read + Send + 'static,
    tx: &mpsc::Sender<Result<String, String>>,
) {
    let mut reader = BufReader::new(stream).take(MAX_FIRMWARE_STREAM_BYTES + 1);
    for _ in 0..MAX_FIRMWARE_EVENTS {
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => return,
            Ok(_) if line.len() > MAX_FIRMWARE_LINE_BYTES => {
                let _ = tx.send(Err(
                    "firmware progress line exceeded the safety limit".into()
                ));
                return;
            }
            Ok(_) => {
                let clean = String::from_utf8_lossy(&line)
                    .chars()
                    .filter(|character| {
                        !character.is_control() || matches!(character, ' ' | '\n' | '\t')
                    })
                    .collect();
                if tx.send(Ok(clean)).is_err() {
                    return;
                }
            }
            Err(_) => {
                let _ = tx.send(Err("firmware progress stream failed".into()));
                return;
            }
        }
    }
    let _ = tx.send(Err(
        "firmware progress event count exceeded the safety limit".into(),
    ));
}

fn progress_percent(line: &str) -> Option<u8> {
    let end = line.find('%')?;
    let prefix = line.get(..end)?;
    let digits: String = prefix
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || c.is_ascii_whitespace())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.trim().parse::<u8>().ok().map(|v| v.min(100))
}

fn clean_progress_message(line: &str) -> String {
    let no_control: String = line.chars().filter(|c| !c.is_control()).collect();
    let trimmed = no_control.trim();
    if trimmed.len() > 160 {
        format!("{}…", trimmed.chars().take(159).collect::<String>())
    } else {
        trimmed.to_string()
    }
}

fn reboot_needed() -> bool {
    run_capture("fwupdmgr", &["check-reboot-needed"]).is_ok_and(|out| {
        let lower = out.to_ascii_lowercase();
        lower.contains("reboot") && !lower.contains("no reboot") && !lower.contains("not required")
    })
}

fn parse_updates(json: &str) -> Result<Vec<FirmwareUpdate>, String> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("invalid fwupd json: {e}"))?;
    let mut updates = Vec::new();
    collect_devices(&root, &mut updates);
    updates.sort_by(|a, b| a.device.cmp(&b.device));
    updates.dedup();
    Ok(updates)
}

fn collect_devices(value: &serde_json::Value, out: &mut Vec<FirmwareUpdate>) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_devices(value, out);
            }
        }
        serde_json::Value::Object(map) => {
            let releases = map
                .get("Releases")
                .or_else(|| map.get("releases"))
                .and_then(serde_json::Value::as_array);
            if let Some(releases) = releases
                && let Some(release) = releases.first().and_then(serde_json::Value::as_object)
            {
                let string = |key: &str| {
                    map.get(key)
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                };
                let new_version = release
                    .get("Version")
                    .or_else(|| release.get("version"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let description = release
                    .get("Description")
                    .or_else(|| release.get("description"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let urgency = release
                    .get("Urgency")
                    .or_else(|| release.get("urgency"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let problems = map
                    .get("Problems")
                    .or_else(|| map.get("problems"))
                    .and_then(serde_json::Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(FirmwareUpdate {
                    device_id: string("DeviceId").or_else(|| string("device_id")),
                    device: string("Name")
                        .or_else(|| string("name"))
                        .unwrap_or_else(|| "Firmware device".into()),
                    current_version: string("Version").or_else(|| string("version")),
                    new_version,
                    description,
                    urgency,
                    problems,
                });
            }
            for child in map.values() {
                collect_devices(child, out);
            }
        }
        _ => {}
    }
}

fn parse_history(json: &str) -> Result<Vec<FirmwareHistoryEntry>, String> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("invalid fwupd history json: {e}"))?;
    let mut history = Vec::new();
    collect_history(&root, &mut history);
    Ok(history)
}

fn collect_history(value: &serde_json::Value, out: &mut Vec<FirmwareHistoryEntry>) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_history(value, out);
            }
        }
        serde_json::Value::Object(map) => {
            let string = |keys: &[&str]| {
                keys.iter().find_map(|key| {
                    map.get(*key)
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
            };
            if let Some(state) = string(&["UpdateState", "update_state"]) {
                out.push(FirmwareHistoryEntry {
                    device: string(&["Name", "name"]).unwrap_or_else(|| "Firmware device".into()),
                    from_version: string(&["VersionOld", "version_old"]),
                    to_version: string(&["Version", "version"]),
                    state,
                    error: string(&["UpdateError", "update_error"]),
                });
            }
            for child in map.values() {
                collect_history(child, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fwupd_devices_history_and_progress_parse_bounded_and_sanitized() {
        {
            let (tx, rx) = mpsc::channel();
            read_firmware_progress(
                std::io::Cursor::new(vec![b'x'; MAX_FIRMWARE_LINE_BYTES + 1]),
                &tx,
            );
            assert!(rx.recv().unwrap().unwrap_err().contains("line"));

            let (tx, rx) = mpsc::channel();
            read_firmware_progress(
                std::io::Cursor::new("x\n".repeat(MAX_FIRMWARE_EVENTS + 1)),
                &tx,
            );
            let events: Vec<_> = rx.try_iter().collect();
            assert_eq!(events.len(), MAX_FIRMWARE_EVENTS + 1);
            assert!(
                events
                    .last()
                    .unwrap()
                    .as_ref()
                    .unwrap_err()
                    .contains("event")
            );
        }
        {
            let updates = parse_updates(
                r#"{"Devices":[{"DeviceId":"device-1","Name":"System Firmware","Version":"1.0","Releases":[{"Version":"1.2","Flags":4}]}]}"#,
            )
            .unwrap();
            assert_eq!(updates.len(), 1);
            assert_eq!(updates[0].device_id.as_deref(), Some("device-1"));
            assert_eq!(updates[0].device, "System Firmware");
            assert_eq!(updates[0].new_version.as_deref(), Some("1.2"));
            assert!(updates[0].problems.is_empty());
        }
        {
            let history = parse_history(
                r#"{"Devices":[{"Name":"System Firmware","VersionOld":"1.0","Version":"1.2","UpdateState":"success"},{"Name":"Dock","Version":"3","UpdateState":"failed","UpdateError":"cable removed"}]}"#,
            )
            .unwrap();
            assert_eq!(history.len(), 2);
            assert_eq!(history[0].from_version.as_deref(), Some("1.0"));
            assert_eq!(history[1].error.as_deref(), Some("cable removed"));
        }
        {
            assert_eq!(progress_percent("Installing… [  42%]"), Some(42));
            assert_eq!(
                clean_progress_message("\u{1b}[2KWriting firmware"),
                "[2KWriting firmware"
            );
        }
    }
}

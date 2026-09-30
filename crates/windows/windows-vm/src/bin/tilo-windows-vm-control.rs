use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::ExitCode;

use std::time::{SystemTime, UNIX_EPOCH};

use windows_vm::{
    ApplianceConfig, DomainState, EgressLease, EgressPurpose, GraphicsAssignment,
    GraphicsCapabilities, LibvirtBackend, StdCommandRunner, VmBackend,
};

const ROOT: &str = "/var/lib/tilo/windows";
const UUID_FILE: &str = "/var/lib/tilo/windows/domain.uuid";
const SYSTEM_DISK: &str = "/var/lib/tilo/windows/system.qcow2";
const NVRAM: &str = "/var/lib/tilo/windows/nvram.fd";
// Keep controller egress state apart from per-session portal-file lease
// projections below `$XDG_RUNTIME_DIR/tilo/windows-vm`.
const EGRESS_RUNTIME_ROOT: &str = "/run/tilo/windows-egress";
const LEASE_ROOT: &str = "/run/tilo/windows-egress/leases";
const EGRESS_SUMMARY: &str = "/run/tilo/windows-egress/summary.json";
const STATUS_SUMMARY: &str = "/run/tilo/windows-egress/status.json";
const CONTROLLER_LOCK: &str = "/run/tilo/windows-egress/controller.lock";
const GRAPHICS_ASSIGNMENT: &str = "/var/lib/tilo/windows/graphics-assignment.json";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("tilo-windows-vm-control: {message}");
            ExitCode::FAILURE
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the root controller keeps its complete typed action dispatch in one auditable function"
)]
fn run() -> Result<(), String> {
    let mut arguments = env::args_os().skip(1);
    let action = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(usage)?;
    let rest = arguments
        .map(|value| value.into_string().map_err(|_| usage()))
        .collect::<Result<Vec<_>, _>>()?;

    // This read-only probe is useful before the appliance is provisioned. It
    // never creates a VF/mdev or unbinds the GPU currently displaying Tilo.
    if action == "graphics-capabilities" {
        if !rest.is_empty() {
            return Err(usage());
        }
        let capabilities = GraphicsCapabilities::probe().map_err(|error| error.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&capabilities)
                .map_err(|error| format!("cannot encode graphics capabilities: {error}"))?
        );
        return Ok(());
    }

    // The reaper also runs before first provisioning. It still takes the
    // controller lock and removes any stale public status left by a failed or
    // replaced appliance, but it has no domain to query yet.
    if action == "egress-reap" && !Path::new(UUID_FILE).exists() {
        let _lock = acquire_controller_lock()?;
        remove_egress_summary();
        let _ = fs::remove_file(STATUS_SUMMARY);
        return Ok(());
    }

    require_regular_file(Path::new(UUID_FILE), "domain identity")?;
    require_regular_file(Path::new(SYSTEM_DISK), "system disk")?;
    let domain_uuid = fs::read_to_string(UUID_FILE)
        .map_err(|error| format!("cannot read the protected domain identity: {error}"))?;
    let config = ApplianceConfig::recommended(domain_uuid.trim(), SYSTEM_DISK, NVRAM);
    let backend = LibvirtBackend::new(StdCommandRunner);

    if !matches!(
        action.as_str(),
        "egress-open" | "egress-close" | "graphics-attach"
    ) && !rest.is_empty()
    {
        return Err(usage());
    }

    // All operations that inspect or mutate the live NIC/lease set share one
    // root-owned lock. This serialises concurrent pkexec requests with the
    // timer reaper, so an old lease can never remove a newer NIC or summary.
    let _controller_lock = matches!(
        action.as_str(),
        "egress-open"
            | "egress-close"
            | "egress-revoke"
            | "egress-reap"
            | "start"
            | "shutdown"
            | "force-shutdown"
            | "restart"
            | "suspend"
            | "resume"
            | "define"
            | "graphics-attach"
            | "graphics-detach"
    )
    .then(acquire_controller_lock)
    .transpose()?;

    // A live-only NIC disappears on a domain restart, so retire its lease
    // records before any power transition. This also makes a later retry
    // unable to resurrect an already-expired capability.
    if matches!(
        action.as_str(),
        "shutdown" | "force-shutdown" | "restart" | "suspend"
    ) {
        close_all_leases(&backend, &config)?;
        write_status_summary(&backend, &config)?;
    }

    match action.as_str() {
        "status" => {
            let state = backend.state(&config).map_err(|error| error.to_string())?;
            let temporary_egress = read_egress_summary()?
                .map(|(purpose, expires_unix_seconds)| {
                    format!(
                        ",\"temporary_egress\":{{\"purpose\":\"{purpose}\",\"expires_at_unix\":{expires_unix_seconds}}}"
                    )
                })
                .unwrap_or_default();
            println!(
                "{{\"schema_version\":1,\"domain\":\"{}\"{temporary_egress}}}",
                domain_state_name(state)
            );
            Ok(())
        }
        "define" => backend.ensure_singleton(&config).map_err(|e| e.to_string()),
        "start" => backend.start(&config).map_err(|e| e.to_string()),
        "shutdown" => backend.shutdown(&config).map_err(|e| e.to_string()),
        "force-shutdown" => backend.force_shutdown(&config).map_err(|e| e.to_string()),
        "restart" => backend.restart(&config).map_err(|e| e.to_string()),
        "suspend" => backend.suspend(&config).map_err(|e| e.to_string()),
        "resume" => backend.resume(&config).map_err(|e| e.to_string()),
        "graphics-attach" => {
            let [kind, identifier] = rest.as_slice() else {
                return Err("usage: tilo-windows-vm-control graphics-attach {sriov-vf|mdev} <canonical-id>".to_string());
            };
            if Path::new(GRAPHICS_ASSIGNMENT).exists() {
                return Err("a Windows graphics partition is already recorded; detach it first".to_string());
            }
            let capabilities = GraphicsCapabilities::probe().map_err(|error| error.to_string())?;
            let assignment = match kind.as_str() {
                "sriov-vf" => capabilities.sriov_assignment(identifier),
                "mdev" => capabilities.mediated_assignment(identifier),
                _ => None,
            }
            .ok_or_else(|| {
                "the selected partition is not an existing VFIO-ready display VF/mdev"
                    .to_string()
            })?;
            write_graphics_assignment(&assignment)?;
            if let Err(error) = backend.attach_graphics(&config, &assignment) {
                let _ = fs::remove_file(GRAPHICS_ASSIGNMENT);
                return Err(error.to_string());
            }
            println!(
                "{}",
                serde_json::to_string(&assignment)
                    .map_err(|error| format!("cannot encode graphics assignment: {error}"))?
            );
            Ok(())
        }
        "graphics-detach" => {
            let assignment = read_graphics_assignment()?;
            backend
                .detach_graphics(&config, &assignment)
                .map_err(|error| error.to_string())?;
            fs::remove_file(GRAPHICS_ASSIGNMENT)
                .map_err(|error| format!("cannot retire graphics assignment: {error}"))?;
            println!("{{\"schema_version\":1,\"result\":\"detached\"}}");
            Ok(())
        }
        "egress-open" => {
            let purpose = match rest.first().map(String::as_str) {
                Some("activation") => EgressPurpose::Activation,
                Some("windows-update") => EgressPurpose::WindowsUpdate,
                _ => return Err("usage: tilo-windows-vm-control egress-open {activation|windows-update} [ttl-seconds]".to_string()),
            };
            let ttl = match rest.get(1) {
                None => 600,
                Some(value) => value.parse::<u64>().map_err(|_| "invalid egress TTL".to_string())?,
            };
            if rest.len() > 2 { return Err(usage()); }
            // The periodic timer is only a backstop. A user asking for a new
            // connection should not be held hostage by an already-expired
            // record for up to one timer interval; reap it transactionally
            // while this controller lock is held.
            reap_leases(&backend, &config)?;
            if active_lease()?.is_some() {
                return Err("an egress lease is already active".to_string());
            }
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "system clock is before the Unix epoch".to_string())?.as_secs();
            let pending_id = format!("egress-pending-{}", std::process::id());
            write_lease(&pending_id, purpose, now.saturating_add(ttl))?;
            let lease = match backend.open_egress(&config, purpose, now, ttl) {
                Ok(lease) => lease,
                Err(error) => { remove_lease(&pending_id); return Err(error.to_string()); }
            };
            let pending_path = lease_path(&pending_id)?;
            let active_path = lease_path(&lease.id)?;
            if active_path.exists() {
                return Err(rollback_attached_egress(
                    &backend,
                    &config,
                    &lease,
                    &pending_id,
                    "an egress lease became active during setup".to_string(),
                ));
            }
            if let Err(error) = fs::rename(pending_path, active_path) {
                return Err(rollback_attached_egress(
                    &backend,
                    &config,
                    &lease,
                    &pending_id,
                    format!("cannot commit the egress lease: {error}"),
                ));
            }
            if let Err(error) = write_egress_summary(lease.purpose, lease.expires_unix_seconds) {
                return Err(rollback_attached_egress(
                    &backend,
                    &config,
                    &lease,
                    &lease.id,
                    error,
                ));
            }
            println!("{{\"schema_version\":1,\"id\":\"{}\",\"purpose\":\"{}\",\"expires_unix_seconds\":{}}}", lease.id, lease.purpose.as_str(), lease.expires_unix_seconds);
            Ok(())
        }
        "egress-close" => {
            let id = rest.first().cloned().ok_or_else(|| "usage: tilo-windows-vm-control egress-close <lease-id>".to_string())?;
            if rest.len() != 1 { return Err(usage()); }
            let lease = EgressLease { id, purpose: EgressPurpose::WindowsUpdate, expires_unix_seconds: 0 };
            backend.close_egress(&config, &lease).map_err(|e| e.to_string())?;
            remove_lease(&lease.id);
            remove_egress_summary();
            Ok(())
        }
        "egress-revoke" => {
            if !rest.is_empty() {
                return Err(usage());
            }
            let Some((id, purpose, expires_unix_seconds)) = active_lease()?
            else {
                return Err("there is no active Windows egress lease".to_string());
            };
            let lease = EgressLease {
                id: id.clone(),
                purpose,
                expires_unix_seconds,
            };
            backend.close_egress(&config, &lease).map_err(|error| error.to_string())?;
            remove_lease(&id);
            remove_egress_summary();
            println!("{{\"schema_version\":1,\"result\":\"revoked\"}}");
            Ok(())
        }
        "egress-reap" => {
            if !rest.is_empty() { return Err(usage()); }
            reap_leases(&backend, &config)?;
            Ok(())
        }
        _ => return Err(usage()),
    }
    .map_err(|error| format!("cannot manage the singleton appliance: {error}"))?;

    if matches!(
        action.as_str(),
        "egress-open"
            | "egress-close"
            | "egress-revoke"
            | "egress-reap"
            | "start"
            | "shutdown"
            | "force-shutdown"
            | "restart"
            | "suspend"
            | "resume"
    ) {
        write_status_summary(&backend, &config)?;
    }

    Ok(())
}

fn usage() -> String {
    "usage: tilo-windows-vm-control {status|graphics-capabilities|graphics-attach|graphics-detach|define|start|shutdown|force-shutdown|restart|suspend|resume|egress-open|egress-close|egress-revoke|egress-reap}".to_string()
}

fn write_graphics_assignment(assignment: &GraphicsAssignment) -> Result<(), String> {
    let bytes = serde_json::to_vec(assignment)
        .map_err(|error| format!("cannot encode graphics assignment: {error}"))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(GRAPHICS_ASSIGNMENT)
        .map_err(|error| format!("cannot record graphics assignment: {error}"))?;
    let result = file
        .write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot commit graphics assignment: {error}"));
    if result.is_err() {
        let _ = fs::remove_file(GRAPHICS_ASSIGNMENT);
    }
    result
}

fn read_graphics_assignment() -> Result<GraphicsAssignment, String> {
    let metadata = fs::symlink_metadata(GRAPHICS_ASSIGNMENT)
        .map_err(|_| "no Tilo-managed Windows graphics partition is attached".to_string())?;
    if !metadata.file_type().is_file()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("the graphics assignment record has unsafe permissions".to_string());
    }
    let bytes = fs::read(GRAPHICS_ASSIGNMENT)
        .map_err(|error| format!("cannot read graphics assignment: {error}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("graphics assignment record is invalid: {error}"))
}

fn lease_path(id: &str) -> Result<std::path::PathBuf, String> {
    if !id.starts_with("egress-")
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("invalid egress lease identifier".to_string());
    }
    Ok(std::path::Path::new(LEASE_ROOT).join(format!("{id}.json")))
}

fn write_lease(id: &str, purpose: EgressPurpose, expires: u64) -> Result<(), String> {
    let path = lease_path(id)?;
    fs::create_dir_all(LEASE_ROOT)
        .map_err(|error| format!("cannot create lease directory: {error}"))?;
    fs::set_permissions(LEASE_ROOT, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure egress lease directory: {error}"))?;
    let bytes = format!("{{\"schema_version\":1,\"id\":\"{id}\",\"purpose\":\"{}\",\"expires_unix_seconds\":{expires}}}\n", purpose.as_str()).into_bytes();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| format!("cannot create egress lease: {error}"))?;
    let result = file
        .write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot commit egress lease: {error}"));
    if result.is_err() {
        let _ = fs::remove_file(&path);
    }
    result
}

/// Publish only the user-safe egress purpose and expiry. The controller's
/// opaque lease ID remains in its root-only record under `LEASE_ROOT`.
fn write_egress_summary(purpose: EgressPurpose, expires: u64) -> Result<(), String> {
    ensure_egress_runtime_root()?;
    let bytes = format!(
        "{{\"schema_version\":1,\"purpose\":\"{}\",\"expires_at_unix\":{expires}}}\n",
        purpose.as_str()
    );
    tilo_files::native::atomic::write_with_mode(Path::new(EGRESS_SUMMARY), bytes.as_bytes(), 0o640)
        .map_err(|error| format!("cannot publish egress summary: {error}"))
}

fn ensure_egress_runtime_root() -> Result<(), String> {
    let root = Path::new(EGRESS_RUNTIME_ROOT);
    fs::create_dir_all(root)
        .map_err(|error| format!("cannot create egress runtime root: {error}"))?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o2750))
        .map_err(|error| format!("cannot secure egress runtime root: {error}"))
}

fn acquire_controller_lock() -> Result<advisory_lock::FileLock, String> {
    ensure_egress_runtime_root()?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(CONTROLLER_LOCK)
        .map_err(|error| format!("cannot open Windows controller lock: {error}"))?;
    advisory_lock::FileLock::exclusive(file)
        .map_err(|error| format!("cannot lock Windows controller: {error}"))
}

fn write_status_summary<R: VmBackend>(backend: &R, config: &ApplianceConfig) -> Result<(), String> {
    ensure_egress_runtime_root()?;
    let state = backend
        .state(config)
        .map_err(|error| format!("cannot read Windows domain state for status summary: {error}"))?;
    let power_state = match state {
        DomainState::Running => "running",
        DomainState::Suspended => "suspended",
        DomainState::Paused => "paused",
        DomainState::ShutOff => "shut_down",
        DomainState::Absent
        | DomainState::Starting
        | DomainState::ShuttingDown
        | DomainState::Crashed
        | DomainState::Other => "unknown",
    };
    let temporary_egress = active_lease()?
        .map(|(_, purpose, expires_at_unix)| {
            format!(
                ",\"temporary_egress\":{{\"purpose\":\"{}\",\"expires_at_unix\":{expires_at_unix}}}",
                purpose.as_str()
            )
        })
        .unwrap_or_default();
    let summary =
        format!("{{\"schema_version\":1,\"power_state\":\"{power_state}\"{temporary_egress}}}\n");
    tilo_files::native::atomic::write_with_mode(
        Path::new(STATUS_SUMMARY),
        summary.as_bytes(),
        0o640,
    )
    .map_err(|error| format!("cannot publish Windows status summary: {error}"))
}

fn remove_egress_summary() {
    let _ = fs::remove_file(EGRESS_SUMMARY);
}

fn read_egress_summary() -> Result<Option<(&'static str, u64)>, String> {
    let metadata = match fs::symlink_metadata(EGRESS_SUMMARY) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect egress summary: {error}")),
    };
    if !metadata.file_type().is_file()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o007 != 0
    {
        return Err("egress summary has unsafe permissions".to_string());
    }
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(EGRESS_SUMMARY)
            .map_err(|error| format!("cannot read egress summary: {error}"))?,
    )
    .map_err(|_| "egress summary is malformed".to_string())?;
    if value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err("egress summary has an unsupported schema".to_string());
    }
    let purpose = match value.get("purpose").and_then(serde_json::Value::as_str) {
        Some("activation") => "activation",
        Some("windows_update") => "windows_update",
        _ => return Err("egress summary has an invalid purpose".to_string()),
    };
    let expires = value
        .get("expires_at_unix")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "egress summary has an invalid expiry".to_string())?;
    Ok(Some((purpose, expires)))
}

fn remove_lease(id: &str) {
    if let Ok(path) = lease_path(id) {
        let _ = fs::remove_file(path);
    }
}

/// Undo an attachment only when libvirt confirms that the live NIC is gone.
/// On a transient detach failure the root-only lease record deliberately
/// remains available to manual revoke and the periodic reaper.
fn rollback_attached_egress<R: VmBackend>(
    backend: &R,
    config: &ApplianceConfig,
    lease: &EgressLease,
    recorded_id: &str,
    cause: String,
) -> String {
    match backend.close_egress(config, lease) {
        Ok(()) => {
            remove_lease(recorded_id);
            remove_egress_summary();
            cause
        }
        Err(detach_error) => {
            let status_detail = write_status_summary(backend, config)
                .err()
                .map(|error| format!("; runtime status refresh also failed: {error}"))
                .unwrap_or_default();
            format!(
                "{cause}; live egress detach failed and its lease was retained for retry: {detach_error}{status_detail}"
            )
        }
    }
}

/// Read the one active egress lease without ever printing its opaque ID in a
/// management status response.  Lease files are controller-owned and their
/// filename still passes through `lease_path` before use.
fn active_lease() -> Result<Option<(String, EgressPurpose, u64)>, String> {
    let entries = match fs::read_dir(LEASE_ROOT) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect egress leases: {error}")),
    };
    let mut active = None;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot inspect egress lease: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read egress lease: {error}"))?;
        let value: serde_json::Value = serde_json::from_str(&source)
            .map_err(|_| "an egress lease record is malformed".to_string())?;
        let id = value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "an egress lease has no id".to_string())?;
        let purpose = match value.get("purpose").and_then(serde_json::Value::as_str) {
            Some("activation") => EgressPurpose::Activation,
            Some("windows_update") => EgressPurpose::WindowsUpdate,
            _ => return Err("an egress lease has an invalid purpose".to_string()),
        };
        let expiry = value
            .get("expires_unix_seconds")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "an egress lease has an invalid expiry".to_string())?;
        let expected_path = lease_path(id)?;
        if expected_path != path {
            return Err("an egress lease is stored under an unexpected name".to_string());
        }
        if active.replace((id.to_string(), purpose, expiry)).is_some() {
            return Err("more than one Windows egress lease is active".to_string());
        }
    }
    Ok(active)
}

fn reap_leases<R: VmBackend>(backend: &R, config: &ApplianceConfig) -> Result<(), String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_string())?
        .as_secs();
    let entries = match fs::read_dir(LEASE_ROOT) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            remove_egress_summary();
            return Ok(());
        }
        Err(error) => return Err(format!("cannot inspect egress leases: {error}")),
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot inspect egress lease: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        let Some(id) = source
            .split("\"id\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
        else {
            continue;
        };
        let Some(purpose) = source
            .split("\"purpose\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
        else {
            continue;
        };
        let Some(expiry) = source
            .split("\"expires_unix_seconds\":")
            .nth(1)
            .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|v| v.parse::<u64>().ok())
        else {
            continue;
        };
        if expiry > now {
            continue;
        }
        let purpose = match purpose {
            "activation" => EgressPurpose::Activation,
            "windows_update" => EgressPurpose::WindowsUpdate,
            _ => continue,
        };
        let lease = EgressLease {
            id: id.to_string(),
            purpose,
            expires_unix_seconds: expiry,
        };
        if backend.close_egress(config, &lease).is_ok() {
            let _ = fs::remove_file(path);
            remove_egress_summary();
        }
    }
    if active_lease()?.is_none() {
        remove_egress_summary();
    }
    Ok(())
}

fn close_all_leases<R: VmBackend>(backend: &R, config: &ApplianceConfig) -> Result<(), String> {
    let Some((id, purpose, expires_unix_seconds)) = active_lease()? else {
        remove_egress_summary();
        return Ok(());
    };
    let lease = EgressLease {
        id: id.clone(),
        purpose,
        expires_unix_seconds,
    };
    // Never discard the controller record until libvirt confirms the live
    // NAT interface is detached. On a transient failure the lifecycle action
    // stops, while the lease remains visible and retryable by UI/reaper.
    backend.close_egress(config, &lease).map_err(|error| {
        format!("cannot revoke Windows egress before power transition: {error}")
    })?;
    remove_lease(&id);
    remove_egress_summary();
    Ok(())
}

fn domain_state_name(state: DomainState) -> &'static str {
    match state {
        DomainState::Absent => "absent",
        DomainState::ShutOff => "shut_off",
        DomainState::Running => "running",
        DomainState::Paused => "paused",
        DomainState::Suspended => "suspended",
        DomainState::Starting => "starting",
        DomainState::ShuttingDown => "shutting_down",
        DomainState::Crashed => "crashed",
        DomainState::Other => "other",
    }
}

fn require_regular_file(path: &Path, label: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("missing or unreadable {label} below {ROOT}: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err(format!("{label} below {ROOT} is not a regular file"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use advisory_lock::{FileLock, LockMode, TryLockError};

    #[test]
    fn controller_lock_contract_is_exclusive_across_concurrent_callers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("controller.lock");
        let first = FileLock::open(&path, LockMode::Exclusive).unwrap();
        assert!(matches!(
            FileLock::try_open(&path, LockMode::Exclusive),
            Err(TryLockError::WouldBlock)
        ));
        drop(first);
        drop(FileLock::try_open(&path, LockMode::Exclusive).unwrap());
    }
}
